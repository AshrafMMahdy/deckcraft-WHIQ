//! JSON-RPC 2.0 framing and the MCP lifecycle / tools / resources methods, with the WHIQ
//! "no dead stops" contract on every tool call: structured errors, forgiving names and
//! arguments, jobs for long work, auto-save of work that would be lost, trimmed results,
//! `OLD_` renames instead of silent overwrites, and the caller's name on what it changes.

use std::io::{BufRead, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::backend::Backend;
use crate::contract::{self, ErrorContext, ToolError, code};
use crate::state::{Config, DocSnapshot, Evicted, Shared};
use crate::tools::{ToolResult, call_tool, tool_definitions};

/// The MCP revision we implement.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
const SUPPORTED_VERSIONS: &[&str] = &[PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const RESOURCE_NOT_FOUND: i64 = -32002;

const INSTRUCTIONS: &str = "DeckCraft is a presentation app (a PowerPoint clone). Coordinates are points on the \
current slide (y down; the default 16:9 slide is 960×540). Every action is a command: find ids and parameters with \
list_commands and run them with run_command (or several with batch). Typical flow: new_presentation → add_slide \
{layout:\"titleAndContent\", title, body} → add_shape / add_text_box / insert_picture / insert_chart → \
inspect_slide to read back ids, boxes and text → render_slide to look at the result. Shape ids are looked up on the \
current slide. An inserted shape is selected; run_command edit.select {ids:[…]} selects others. format.* acts on \
the selected text or shapes; shape.fill, shape.line and the other shape commands whose parameters list `ids` act on \
those shapes, else on the selected ones. Save with save_presentation (.deckcraft or .pptx).";

const CONTRACT: &str = "Contract: no call is a dead stop. Every error has `code`, `reason` and `next_step` \
(structuredContent.error). `busy` comes with `retry_after_ms`; long work (a very large open, exporting many slides) \
returns a `job_id`: poll job_status. Unsaved work is auto-saved to `<name>.recovered-<stamp>.<ext>` in the write root \
when the server must close it (idle, memory, SIGTERM, a crash) and the next answer names the file (`recovered_files`). \
Big answers come back `truncated` with `continue_from`: call again with `offset`. Saving never silently replaces a \
file: the old one becomes OLD_<name> unless `overwrite: true`. Unknown arguments are ignored with a `warnings` entry. \
Every tool takes `caller` (your name, put on comments and the saved file). build_deck makes a whole deck in one call.";

/// Resource URIs.
pub const DOC_URI: &str = "deckcraft://document";
pub const COMMANDS_URI: &str = "deckcraft://commands";

/// An MCP server bound to one backend.
pub struct Server {
    backend: Box<dyn Backend>,
    initialized: bool,
    shared: Arc<Shared>,
}

fn response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

/// A successful tool answer before it is shaped into a `CallToolResult`.
struct Outcome {
    value: Value,
    /// A rendered PNG (base64) to return as an image block.
    image: Option<String>,
    warnings: Vec<String>,
}

impl Outcome {
    fn new(value: Value) -> Self {
        Outcome { value, image: None, warnings: vec![] }
    }
}

pub(crate) fn tool_names() -> Vec<&'static str> {
    static NAMES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    NAMES
        .get_or_init(|| tool_definitions().iter().filter_map(|t| t.get("name").and_then(Value::as_str).map(String::from)).collect())
        .iter()
        .map(String::as_str)
        .collect()
}

fn tool_def(name: &str) -> Option<Value> {
    static DEFS: std::sync::OnceLock<Vec<Value>> = std::sync::OnceLock::new();
    DEFS.get_or_init(tool_definitions).iter().find(|t| t.get("name").and_then(Value::as_str) == Some(name)).cloned()
}

/// Tools that act on an open presentation.
fn needs_document(name: &str, args: &Value) -> bool {
    match name {
        "list_commands" | "new_presentation" | "open_presentation" | "build_deck" | "server_status" | "job_status" | "list_jobs" | "evict"
        | "cancel" | "select_tool" | "screenshot" | "call_app" => false,
        "run_command" => !args.get("command").and_then(Value::as_str).is_some_and(command_needs_no_document),
        "batch" => !args
            .get("commands")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
            .and_then(|c| c.get("command"))
            .and_then(Value::as_str)
            .is_some_and(command_needs_no_document),
        _ => true,
    }
}

fn command_needs_no_document(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    id.starts_with("file.new")
        || id.starts_with("file.open")
        || id.starts_with("file.adopt")
        || id.starts_with("file.templates")
        || id.starts_with("file.recovery")
        || id.starts_with("deck.build")
        || id.starts_with("app.")
}

/// The array a tool's `offset` / `limit` page through.
fn page_pointer(name: &str) -> Option<&'static str> {
    match name {
        "list_commands" | "batch" | "list_jobs" => Some(""),
        "inspect_document" => Some("/slides"),
        "inspect_slide" => Some("/shapes"),
        _ => None,
    }
}

/// `path` made absolute against the working folder (the write root under the automation roots).
fn absolute(path: &str) -> String {
    std::path::absolute(path).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| path.to_string())
}

/// `server_status` from the shared state alone, so it answers while the worker is busy.
pub fn status_value(shared: &Shared) -> Value {
    let c = &shared.config;
    let docs: Vec<Value> = shared
        .docs()
        .iter()
        .enumerate()
        .map(|(i, d)| json!({"index": i, "title": d.title, "path": d.path, "slides": d.slides, "dirty": d.dirty, "active": d.active}))
        .collect();
    json!({
        "server": "deckcraft",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": PROTOCOL_VERSION,
        "backend": crate::state::lock(&shared.backend_description).clone(),
        "pid": std::process::id(),
        "uptime_ms": shared.started.elapsed().as_millis() as u64,
        "author": c.author,
        "memory": crate::state::memory_value(c.max_memory_mb),
        "checkpoints": crate::checkpoint::status(shared),
        "roots": {"read": c.read_root.as_ref().map(|p| p.display().to_string()), "write": c.write_root.as_ref().map(|p| p.display().to_string())},
        "documents": docs,
        "jobs": {"running": shared.running_jobs(), "list": shared.jobs_value()},
        "current_call": shared.current().map(|(t, ms)| json!({"tool": t, "elapsed_ms": ms, "progress": shared.current_progress()})),
        "idle_ms": shared.idle_for().as_millis() as u64,
        "calls": shared.calls.load(std::sync::atomic::Ordering::Relaxed),
        "busy_replies": shared.busy_replies.load(std::sync::atomic::Ordering::Relaxed),
        "recovered_files": shared.recovered().iter().map(crate::state::Recovered::to_value).collect::<Vec<_>>(),
        "evicted": shared.evicted().iter().map(Evicted::to_value).collect::<Vec<_>>(),
        "limits": {
            "max_result_bytes": c.max_result_bytes,
            "max_image_bytes": c.max_image_bytes,
            "busy_after_ms": c.busy_after_ms,
            "open_job_bytes": c.open_job_bytes,
            "export_job_slides": c.export_job_slides,
            "idle_evict_secs": c.idle_evict_secs,
        },
    })
}

impl Server {
    pub fn new(backend: Box<dyn Backend>) -> Self {
        Self::with_config(backend, Config::default())
    }

    pub fn with_config(backend: Box<dyn Backend>, config: Config) -> Self {
        let shared = Shared::new(config);
        *crate::state::lock(&shared.backend_description) = backend.describe();
        let mut s = Self { backend, initialized: false, shared };
        s.update_status();
        s
    }

    pub fn backend(&mut self) -> &mut dyn Backend {
        self.backend.as_mut()
    }

    /// The state shared with the runtime's threads.
    pub fn shared(&self) -> Arc<Shared> {
        Arc::clone(&self.shared)
    }

    /// Whether the client has sent `notifications/initialized`.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Serve newline-delimited JSON-RPC until `input` closes, with the contract's runtime: calls run
    /// on a worker, waiting calls are answered `busy`, `server_status` / `job_status` answer at once,
    /// idle or memory eviction, auto-save on SIGTERM and at the end of input. Logs go to stderr only.
    pub fn serve(self, input: impl BufRead + Send + 'static, output: impl Write + Send + 'static) -> std::io::Result<()> {
        crate::runtime::serve(self, input, output)
    }

    /// Handle one line; returns the reply line (None for notifications and blank lines).
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let reply = match serde_json::from_str::<Value>(line) {
            Ok(Value::Array(batch)) => {
                // Batches were removed in 2025-06-18; still answer older clients sensibly.
                if batch.is_empty() {
                    Some(error(Value::Null, INVALID_REQUEST, "empty batch"))
                } else {
                    let replies: Vec<Value> = batch.into_iter().filter_map(|m| self.handle(m)).collect();
                    (!replies.is_empty()).then_some(Value::Array(replies))
                }
            }
            Ok(msg) => self.handle(msg),
            Err(e) => Some(error(Value::Null, PARSE_ERROR, format!("parse error: {e}"))),
        };
        reply.map(|r| r.to_string())
    }

    /// Handle one JSON-RPC message; `None` for notifications and responses.
    pub fn handle(&mut self, msg: Value) -> Option<Value> {
        let Value::Object(o) = &msg else { return Some(error(Value::Null, INVALID_REQUEST, "message must be an object")) };
        let id = o.get("id").cloned();
        let Some(method) = o.get("method").and_then(Value::as_str) else {
            // A response to a server→client request (we send none): ignore. Anything else is invalid.
            if o.contains_key("result") || o.contains_key("error") {
                return None;
            }
            return Some(error(id.unwrap_or(Value::Null), INVALID_REQUEST, "missing `method`"));
        };
        let params = o.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = id else {
            self.notification(method);
            return None;
        };
        if !(id.is_string() || id.is_number()) {
            return Some(error(Value::Null, INVALID_REQUEST, "`id` must be a string or number"));
        }
        Some(match self.request(method, &params) {
            Ok(r) => response(id, r),
            Err((code, m)) => error(id, code, m),
        })
    }

    fn notification(&mut self, method: &str) {
        match method {
            "notifications/initialized" => self.initialized = true,
            "notifications/cancelled" | "notifications/progress" | "notifications/roots/list_changed" => {}
            other => log::debug!("ignoring notification {other}"),
        }
    }

    fn request(&mut self, method: &str, params: &Value) -> Result<Value, (i64, String)> {
        match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION);
                let version = if SUPPORTED_VERSIONS.contains(&asked) { asked } else { PROTOCOL_VERSION };
                let recovered = self.shared.recovered();
                let found = if recovered.is_empty() {
                    String::new()
                } else {
                    format!(
                        " Recovered files are waiting: {} (open_presentation on one to continue that work).",
                        recovered.iter().take(5).map(|r| r.path.as_str()).collect::<Vec<_>>().join(", ")
                    )
                };
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}, "resources": {}},
                    "serverInfo": {"name": "deckcraft", "title": "DeckCraft", "version": env!("CARGO_PKG_VERSION"), "websiteUrl": deckcraft_engine::links::APP_PAGE},
                    "instructions": format!(
                        "{INSTRUCTIONS} {CONTRACT}{found} Backend: {}. Community: {} (Discord), {} (app page), {} (source). The `app.links` command returns these links.",
                        self.backend.describe(),
                        deckcraft_engine::links::DISCORD,
                        deckcraft_engine::links::APP_PAGE,
                        deckcraft_engine::links::GITHUB
                    ),
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tool_definitions()})),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).ok_or((INVALID_PARAMS, "missing tool `name`".to_string()))?;
                let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                Ok(self.call(name, &args))
            }
            "resources/list" => Ok(json!({"resources": [
                {"uri": DOC_URI, "name": "document", "title": "Active document", "description": "Slides (titles, layouts, shapes), sections, theme, media and selection of the active presentation (document.inspect)", "mimeType": "application/json"},
                {"uri": COMMANDS_URI, "name": "commands", "title": "Commands", "description": "Every command: id, label, menu path, shortcut, params, enabled (engine.commands)", "mimeType": "application/json"},
            ]})),
            "resources/templates/list" => Ok(json!({"resourceTemplates": []})),
            "resources/read" => {
                let uri = params.get("uri").and_then(Value::as_str).ok_or((INVALID_PARAMS, "missing `uri`".to_string()))?;
                let v = match uri {
                    DOC_URI => self.backend.call("document.inspect", json!({})),
                    COMMANDS_URI => self.backend.call("engine.commands", json!({})),
                    _ => return Err((RESOURCE_NOT_FOUND, format!("resource not found: {uri}"))),
                }
                .map_err(|e| (INTERNAL_ERROR, e))?;
                Ok(json!({"contents": [{"uri": uri, "mimeType": "application/json", "text": serde_json::to_string_pretty(&v).unwrap_or_default()}]}))
            }
            other => Err((METHOD_NOT_FOUND, format!("method not found: {other}"))),
        }
    }

    // ── the contract's tool call ─────────────────────────────────────────────────────────────

    fn error_context<'a>(&mut self, tool: &'a str) -> ErrorContext<'a> {
        let c = &self.shared.config;
        ErrorContext {
            tool,
            read_root: c.read_root.as_ref().map(|p| p.display().to_string()),
            write_root: c.write_root.as_ref().map(|p| p.display().to_string()),
            slide_count: self.backend.session().and_then(|s| s.active().map(|d| d.doc.slides.len())),
        }
    }

    fn has_document(&mut self) -> bool {
        match self.backend.session() {
            Some(s) => s.active().is_some(),
            None => true,
        }
    }

    /// One `tools/call`: never panics, never hangs on purpose, always says what to do next.
    pub fn call(&mut self, raw_name: &str, raw_args: &Value) -> Value {
        self.shared.touch();
        self.shared.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.adopt_pending();
        let names = tool_names();
        let name: &'static str = match contract::resolve_tool(raw_name, &names) {
            Ok(n) => n,
            Err(close) => {
                let e = ToolError::new(
                    code::NOT_FOUND,
                    format!("unknown tool `{raw_name}`"),
                    format!("call tools/list for the tool names; closest: {}", close.join(", ")),
                )
                .with("candidates", json!(close));
                return self.finish(raw_name, &json!({}), Err(e), vec![], vec![]);
            }
        };
        let mut warnings = vec![];
        if name != raw_name {
            warnings.push(format!("tool `{raw_name}` read as `{name}`"));
        }
        let schema = tool_def(name).and_then(|d| d.get("inputSchema").cloned()).unwrap_or(json!({}));
        let norm = contract::normalize_args(name, &schema, raw_args);
        warnings.extend(norm.warnings);
        let mut args = norm.args;
        let caller =
            args.as_object_mut().and_then(|o| o.remove("caller")).and_then(|v| v.as_str().map(str::to_string)).filter(|c| !c.trim().is_empty());
        let overwrite = args.get("overwrite").and_then(Value::as_bool).unwrap_or(false);
        let delay_ms = if self.shared.config.test_hooks && !matches!(name, "open_presentation" | "export") {
            raw_args.get("_delay_ms").and_then(Value::as_u64).unwrap_or(0).min(60_000)
        } else {
            0
        };
        // Contract tests only: jobs (open, export) take their delay on the job thread.
        if self.shared.config.test_hooks
            && matches!(name, "open_presentation" | "export")
            && let (Some(d), Some(o)) = (raw_args.get("_delay_ms").cloned(), args.as_object_mut())
        {
            o.insert("_delay_ms".into(), d);
        }

        // Gates: a document the server closed, a document still opening.
        if needs_document(name, &args) && !self.has_document() {
            if let Some(ev) = self.shared.evicted().last() {
                let reopen = ev.reopen_path().unwrap_or_default().to_string();
                let e = ToolError::new(
                    code::EVICTED,
                    format!("the presentation `{}` was closed by the server ({}) to free resources", ev.title, ev.reason),
                    format!("your work is auto-saved at {reopen}; call open_presentation {{path: \"{reopen}\"}} again on it"),
                )
                .with("recovered", json!(ev.recovered))
                .with("reopen", json!(reopen))
                .with("evicted", json!(self.shared.evicted().iter().map(Evicted::to_value).collect::<Vec<_>>()));
                return self.finish(name, &args, Err(e), warnings, vec![]);
            }
            if let Some((job, progress, elapsed)) = self.shared.running_open() {
                let retry = crate::state::retry_hint(elapsed, progress);
                let e = ToolError::new(
                    code::BUSY,
                    format!("a presentation is still opening ({job}, {}%)", (progress * 100.0).round()),
                    format!("retry in {retry} ms, or poll job_status {{job_id: \"{job}\"}}"),
                )
                .with("retry_after_ms", json!(retry))
                .with("job_id", json!(job));
                return self.finish(name, &args, Err(e), warnings, vec![]);
            }
        }

        // Who is calling: comments and the saved file carry the name.
        let author = caller.clone().or_else(|| Some(self.shared.config.author.clone()).filter(|a| !a.is_empty()));
        if let (Some(a), Some(s)) = (&author, self.backend.session()) {
            s.prefs.author = a.clone();
        }

        let crash = self.shared.config.test_hooks && raw_args.get("_panic").and_then(Value::as_bool) == Some(true);
        let run = |me: &mut Self| -> Result<Outcome, ToolError> {
            if crash {
                // Contract tests only (DECKCRAFT_MCP_TEST_HOOKS=1): an internal failure mid-call.
                std::panic::resume_unwind(Box::new(String::from("test hook: _panic")));
            }
            // Contract tests only: slow work with a safe point every 20 ms.
            let steps = delay_ms / 20;
            for k in 0..steps {
                std::thread::sleep(std::time::Duration::from_millis(20));
                if let Err(e) = deckcraft_engine::task::step(k as usize, steps as usize) {
                    let cx = me.error_context(name);
                    return Err(contract::classify(&e.to_string(), &cx));
                }
            }
            me.dispatch(name, &args, author.as_deref(), overwrite)
        };
        // The call's task: `cancel` (answered by the reader) stops it at its next safe point.
        let task = deckcraft_engine::task::Task::new();
        self.shared.set_current_task(Some(task.clone()));
        let caught = catch_unwind(AssertUnwindSafe(|| {
            deckcraft_engine::task::scope(task.clone(), || {
                if overwrite || matches!(name, "server_status" | "job_status" | "list_jobs" | "cancel") {
                    (run(self), vec![])
                } else {
                    deckcraft_engine::io::keep_previous(|| run(self))
                }
            })
        }));
        self.shared.set_current_task(None);
        let (outcome, renamed) = match caught {
            Ok(r) => r,
            Err(payload) => {
                let msg = deckcraft_engine::guard::panic_message(payload.as_ref());
                let roots = self.backend.automation_roots();
                let saved = self
                    .backend
                    .session()
                    .map(|s| crate::autosave::save_session(s, &self.shared, roots.as_ref(), "panic", true, true))
                    .unwrap_or_default();
                let at = saved.first().map(|r| r.path.clone());
                let e = ToolError::new(
                    code::INTERNAL,
                    format!("internal error while running `{name}` ({msg}); the presentation is still open"),
                    match &at {
                        Some(p) => format!("your work is auto-saved at {p}; retry the call, or open that file if the server restarts"),
                        None => "retry the call; server_status shows the server's state".into(),
                    },
                )
                .with("recovered", json!(at));
                (Err(e), vec![])
            }
        };
        let renamed: Vec<Value> = renamed.iter().map(|r| json!({"path": absolute(&r.path), "previous": absolute(&r.previous)})).collect();
        // Paths in answers are absolute, so the agent knows where a relative name landed.
        let outcome = outcome.map(|mut o| {
            if matches!(name, "save_presentation" | "export" | "build_deck" | "render_slide" | "run_command")
                && let Some(p) = o.value.get("path").and_then(Value::as_str).map(absolute)
            {
                o.value["path"] = json!(p);
            }
            if let Some(files) = o.value.get_mut("files").and_then(Value::as_array_mut) {
                for f in files.iter_mut() {
                    if let Some(p) = f.as_str().map(absolute) {
                        *f = json!(p);
                    }
                }
            }
            o
        });
        let out = self.finish(name, &args, outcome, warnings, renamed);
        self.update_status();
        out
    }

    fn dispatch(&mut self, name: &'static str, args: &Value, author: Option<&str>, overwrite: bool) -> Result<Outcome, ToolError> {
        match name {
            "server_status" => Ok(Outcome::new(status_value(&self.shared))),
            "list_jobs" => Ok(Outcome::new(Value::Array(self.shared.jobs_value()))),
            "job_status" => {
                let id = args.get("job_id").map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).unwrap_or_default();
                self.shared.job_value(&id).map(Outcome::new).ok_or_else(|| {
                    let ids = self.shared.job_ids();
                    ToolError::new(code::NOT_FOUND, format!("no job `{id}`"), "list_jobs shows the jobs of this server")
                        .with("candidates", json!(contract::closest(&id, ids.iter().map(String::as_str), 5)))
                })
            }
            "cancel" => {
                // In-process (the reader answers this while a call runs): only jobs can be running.
                match args.get("job_id").map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())) {
                    Some(id) => match self.shared.cancel_job(&id) {
                        Some(state) => Ok(Outcome::new(json!({"job_id": id, "state": state, "cancelled": state != "done" && state != "failed"}))),
                        None => Err(ToolError::new(code::NOT_FOUND, format!("no job `{id}`"), "list_jobs shows the jobs of this server")),
                    },
                    None => Ok(Outcome::new(json!({"cancelled": false, "reason": "nothing else is running"}))),
                }
            }
            "evict" => {
                let ev = self.evict_all("evict");
                Ok(Outcome::new(json!({"evicted": ev.iter().map(Evicted::to_value).collect::<Vec<_>>()})))
            }
            "close_presentation" => self.close_active(),
            "open_presentation" => self.open(args),
            "export" => self.export(args, author, overwrite),
            "save_presentation" => {
                self.stamp_author(author);
                self.tool(name, args)
            }
            "run_command" => {
                let mut a = args.clone();
                let mut warnings = vec![];
                let id = a.get("command").and_then(Value::as_str).unwrap_or_default().to_string();
                let canonical = self.command_id(&id)?;
                if canonical != id {
                    warnings.push(format!("command `{id}` read as `{canonical}`"));
                    a["command"] = json!(canonical);
                }
                if canonical.starts_with("file.save") || canonical == "file.export" {
                    self.stamp_author(author);
                }
                let mut o = self.tool(name, &a)?;
                o.warnings.extend(warnings);
                Ok(o)
            }
            "batch" => {
                let mut a = args.clone();
                let mut warnings = vec![];
                if let Some(cmds) = a.get_mut("commands").and_then(Value::as_array_mut) {
                    for c in cmds.iter_mut() {
                        let id = c.get("command").and_then(Value::as_str).unwrap_or_default().to_string();
                        let canonical = self.command_id(&id)?;
                        if canonical != id {
                            warnings.push(format!("command `{id}` read as `{canonical}`"));
                            c["command"] = json!(canonical);
                        }
                    }
                }
                let mut o = self.tool(name, &a)?;
                o.warnings.extend(warnings);
                Ok(o)
            }
            "render_slide" => self.render(args),
            "build_deck" | "new_presentation" => {
                let o = self.tool(name, args)?;
                self.shared.clear_evicted();
                Ok(o)
            }
            _ => self.tool(name, args),
        }
    }

    /// A command id, forgivingly; `Err` when nothing is close.
    fn command_id(&self, id: &str) -> Result<String, ToolError> {
        if id.is_empty() {
            return Err(ToolError::new(
                code::INVALID_ARGUMENT,
                "missing `command`",
                "pass `command`: a command id from list_commands, e.g. slide.new",
            ));
        }
        let ids: Vec<&str> = deckcraft_engine::command_specs().iter().map(|c| c.id).collect();
        // A connected app may know commands this build does not: pass unknown ids through then.
        if self.backend.has_ui() && !ids.contains(&id) {
            return Ok(id.to_string());
        }
        contract::resolve_command(id, &ids).map(str::to_string).map_err(|close| {
            ToolError::new(
                code::NOT_FOUND,
                format!("unknown command `{id}`"),
                format!("call list_commands with a filter; closest: {}", close.join(", ")),
            )
            .with("candidates", json!(close))
        })
    }

    /// Run a tool on the backend and turn its result into an outcome or a classified error.
    fn tool(&mut self, name: &'static str, args: &Value) -> Result<Outcome, ToolError> {
        let r = call_tool(self.backend.as_mut(), name, args);
        self.outcome(name, r)
    }

    fn outcome(&mut self, name: &'static str, r: ToolResult) -> Result<Outcome, ToolError> {
        let text = r
            .content
            .iter()
            .find(|c| c.get("type").and_then(Value::as_str) == Some("text"))
            .and_then(|c| c.get("text"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if r.is_error {
            // A batch reports every step; classify the one that failed.
            if name == "batch"
                && let Ok(Value::Array(steps)) = serde_json::from_str::<Value>(&text)
            {
                let failed = steps.iter().rev().find(|s| s.get("ok") == Some(&json!(false))).cloned().unwrap_or(Value::Null);
                let msg = failed.get("error").and_then(Value::as_str).unwrap_or("a command failed").to_string();
                let cmd = failed.get("command").and_then(Value::as_str).unwrap_or_default().to_string();
                let cx = self.error_context(name);
                let mut e = contract::classify(&format!("{cmd}: {msg}"), &cx);
                e.next_step = format!("{} (the steps before it ran; pass keepGoing: true to run the rest anyway)", e.next_step);
                return Err(e.with("results", Value::Array(steps)).with("failed_command", json!(cmd)));
            }
            let cx = self.error_context(name);
            let mut e = contract::classify(&text, &cx);
            self.add_candidates(&mut e, &text);
            return Err(e);
        }
        let image = r
            .content
            .iter()
            .find(|c| c.get("type").and_then(Value::as_str) == Some("image"))
            .and_then(|c| c.get("data"))
            .and_then(Value::as_str)
            .map(String::from);
        let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
        Ok(Outcome { value, image, warnings: vec![] })
    }

    /// `not_found` answers list what is there.
    fn add_candidates(&mut self, e: &mut ToolError, msg: &str) {
        if e.code != code::NOT_FOUND || e.extra.contains_key("candidates") {
            return;
        }
        let l = msg.to_ascii_lowercase();
        if l.contains("shape") {
            if let Ok(v) = self.backend.call("engine.execute", json!({"command": "slide.inspect", "params": {}})) {
                let list: Vec<Value> = v
                    .get("shapes")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().take(30).map(|s| json!({"id": s.get("id"), "name": s.get("name"), "kind": s.get("kind")})).collect())
                    .unwrap_or_default();
                e.extra.insert("candidates".into(), Value::Array(list));
            }
            return;
        }
        // A missing file: similar names in its folder (when that folder may be read).
        let path =
            msg.split_whitespace().find(|w| w.contains('/') || w.contains('\\') || w.contains('.')).map(|w| w.trim_end_matches(':').to_string());
        if let Some(p) = path {
            let pth = std::path::Path::new(&p);
            let dir =
                pth.parent().filter(|d| !d.as_os_str().is_empty()).map(std::path::Path::to_path_buf).unwrap_or_else(|| std::path::PathBuf::from("."));
            let want = pth.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
            let roots = self.backend.automation_roots();
            let allowed =
                deckcraft_engine::file_access::confine(roots.as_ref(), || deckcraft_engine::file_access::check_read(&dir.to_string_lossy()).is_ok());
            if allowed && let Ok(rd) = std::fs::read_dir(&dir) {
                let names: Vec<String> = rd.flatten().take(2000).map(|e| e.file_name().to_string_lossy().to_string()).collect();
                let near = contract::closest(&want, names.iter().map(String::as_str), 5);
                let full: Vec<String> = near.iter().map(|n| dir.join(n).to_string_lossy().to_string()).collect();
                e.extra.insert("candidates".into(), json!(full));
            }
        }
    }

    /// The caller's name on the file being saved.
    fn stamp_author(&mut self, author: Option<&str>) {
        let Some(a) = author.filter(|a| !a.trim().is_empty()) else { return };
        if let Some(st) = self.backend.session().and_then(|s| s.active_mut()) {
            let doc = Arc::make_mut(&mut st.doc);
            doc.props.last_modified_by = a.to_string();
            if doc.props.author.is_empty() {
                doc.props.author = a.to_string();
            }
        }
    }

    fn open(&mut self, args: &Value) -> Result<Outcome, ToolError> {
        let path = args.get("path").and_then(Value::as_str).unwrap_or_default().to_string();
        let want_job = args.get("async").and_then(Value::as_bool).unwrap_or(false);
        let roots = self.backend.automation_roots();
        let headless = self.backend.session().is_some();
        if headless && !path.is_empty() {
            let size = deckcraft_engine::file_access::confine(roots.as_ref(), || {
                deckcraft_engine::file_access::check_read(&path).ok().and_then(|()| std::fs::metadata(&path).ok()).map(|m| m.len())
            });
            if let Some(size) = size
                && (want_job || size >= self.shared.config.open_job_bytes)
            {
                let id = crate::jobs::spawn_open(&self.shared, roots, path.clone(), args.clone());
                self.shared.clear_evicted();
                return Ok(Outcome::new(json!({
                    "job_id": id,
                    "state": "running",
                    "bytes": size,
                    "retry_after_ms": 500,
                    "next_step": format!("the file opens in the background; poll job_status {{job_id: \"{id}\"}} until state is done"),
                })));
            }
        }
        let o = self.tool("open_presentation", args)?;
        self.shared.clear_evicted();
        Ok(o)
    }

    fn export(&mut self, args: &Value, author: Option<&str>, overwrite: bool) -> Result<Outcome, ToolError> {
        let path = args.get("path").and_then(Value::as_str).unwrap_or_default().to_string();
        let format = args
            .get("format")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| deckcraft_engine::cmd::file::format_for_path(&path).to_string());
        if matches!(format.as_str(), "pptx" | "potx" | "ppsx") {
            self.stamp_author(author);
        }
        let want_job = args.get("async").and_then(Value::as_bool).unwrap_or(false);
        let threshold = self.shared.config.export_job_slides;
        let roots = self.backend.automation_roots();
        let snapshot = self.backend.session().and_then(|s| s.active().map(|d| (Arc::clone(&d.doc), d.selection.slide)));
        if let Some((doc, current)) = snapshot
            && !path.is_empty()
        {
            let all_images = matches!(format.as_str(), "png" | "jpeg" | "jpg") && args.get("all").and_then(Value::as_bool).unwrap_or(false);
            let bulk = all_images || matches!(format.as_str(), "pdf" | "gif");
            if want_job || (bulk && doc.slides.len() >= threshold) {
                let n = doc.slides.len();
                let id = crate::jobs::spawn_export(&self.shared, roots, doc, current, args.clone(), overwrite);
                return Ok(Outcome::new(json!({
                    "job_id": id,
                    "state": "running",
                    "slides": n,
                    "retry_after_ms": 500,
                    "next_step": format!("the export runs in the background; poll job_status {{job_id: \"{id}\"}} until state is done"),
                })));
            }
        }
        self.tool("export", args)
    }

    /// `render_slide`, re-rendered smaller when the picture would flood the caller.
    fn render(&mut self, args: &Value) -> Result<Outcome, ToolError> {
        let cap = self.shared.config.max_image_bytes;
        let mut a = args.clone();
        let mut o = self.tool("render_slide", &a)?;
        let mut tries = 0;
        while let Some(img) = &o.image {
            if img.len() <= cap || tries >= 4 {
                break;
            }
            let scale = a.get("scale").and_then(Value::as_f64).unwrap_or(1.0);
            let next = scale * ((cap as f64 / img.len() as f64).sqrt() * 0.9).clamp(0.1, 0.95);
            a["scale"] = json!(next);
            let w = format!(
                "the picture at scale {scale:.3} was {} bytes (limit {cap}); returned at scale {next:.3}; pass `path` to write a full-size PNG",
                img.len()
            );
            o = self.tool("render_slide", &a)?;
            o.warnings.push(w);
            tries += 1;
        }
        Ok(o)
    }

    fn close_active(&mut self) -> Result<Outcome, ToolError> {
        let roots = self.backend.automation_roots();
        let shared = Arc::clone(&self.shared);
        let Some(s) = self.backend.session() else {
            return self.tool("run_command", &json!({"command": "file.close"})).map(|_| Outcome::new(json!({"closed": true})));
        };
        let Some(st) = s.active() else {
            return Err(ToolError::new(code::NO_DOCUMENT, "no presentation is open", "nothing to close"));
        };
        let title = st.title();
        let saved = crate::autosave::save_session(s, &shared, roots.as_ref(), "close", true, false);
        if let Some(i) = s.active_index() {
            s.close_document(i);
        }
        Ok(Outcome::new(json!({"closed": title, "recovered": saved.first().map(|r| r.path.clone())})))
    }

    /// Auto-save every open presentation with unsaved work, then close them all.
    pub fn evict_all(&mut self, reason: &str) -> Vec<Evicted> {
        let roots = self.backend.automation_roots();
        let shared = Arc::clone(&self.shared);
        let Some(s) = self.backend.session() else { return vec![] };
        let mut out = vec![];
        let n = s.documents().len();
        for i in (0..n).rev() {
            let Some(st) = s.documents().get(i) else { continue };
            let (title, original) = (st.title(), st.path.clone());
            let unsaved = st.is_dirty() || st.path.is_none();
            let recovered = if unsaved {
                match crate::autosave::write_recovered(
                    &shared,
                    roots.as_ref(),
                    &st.doc,
                    &title,
                    original.as_deref(),
                    reason,
                    Some((st.uid, st.revision)),
                ) {
                    Ok(r) => Some(r.path),
                    Err(e) => {
                        // Never discard: a document that cannot be saved stays open.
                        eprintln!("deckcraft-cli: not evicting {title}: auto-save failed: {e}");
                        continue;
                    }
                }
            } else {
                None
            };
            s.close_document(i);
            let ev = Evicted { title, original, recovered, reason: reason.to_string() };
            shared.add_evicted(ev.clone());
            out.push(ev);
        }
        self.update_status();
        out
    }

    /// Save unsaved work before the process ends (end of input, SIGTERM).
    pub fn shutdown_save(&mut self, reason: &str) -> Vec<crate::state::Recovered> {
        let roots = self.backend.automation_roots();
        let shared = Arc::clone(&self.shared);
        match self.backend.session() {
            Some(s) => crate::autosave::save_session(s, &shared, roots.as_ref(), reason, false, false),
            None => vec![],
        }
    }

    /// Add the files `open` jobs finished reading to the session.
    pub fn adopt_pending(&mut self) {
        if !self.shared.has_pending_opens() {
            return;
        }
        let roots = self.backend.automation_roots();
        let pending = self.shared.take_pending_opens();
        for (jid, loaded) in pending {
            let path = loaded.path.clone();
            let Some(s) = self.backend.session() else { continue };
            let token = s.stage_loaded(loaded);
            let r = deckcraft_engine::file_access::confine(roots.as_ref(), || s.execute("file.adopt", &json!({"path": path, "token": token})));
            s.ui_requests.clear();
            match r {
                Ok(v) => self.shared.job_done(&jid, v),
                Err(e) => {
                    let cx = self.error_context("open_presentation");
                    self.shared.job_failed(&jid, contract::classify(&e.to_string(), &cx).to_value());
                }
            }
        }
        self.update_status();
    }

    /// Tell the shared state what is open (for server_status, eviction and SIGTERM).
    pub fn update_status(&mut self) {
        let docs = match self.backend.session() {
            Some(s) => {
                let active = s.active_index();
                s.documents()
                    .iter()
                    .enumerate()
                    .map(|(i, d)| DocSnapshot {
                        uid: d.uid,
                        revision: d.revision,
                        title: d.title(),
                        path: d.path.clone(),
                        dirty: d.is_dirty(),
                        active: Some(i) == active,
                        slides: d.doc.slides.len(),
                        doc: Arc::clone(&d.doc),
                    })
                    .collect()
            }
            None => vec![],
        };
        self.shared.set_docs(docs);
    }

    /// The `CallToolResult`: content blocks, `structuredContent`, `isError`.
    fn finish(&mut self, name: &str, args: &Value, outcome: Result<Outcome, ToolError>, mut warnings: Vec<String>, renamed: Vec<Value>) -> Value {
        let recovered: Vec<Value> = self.shared.take_unannounced().iter().map(crate::state::Recovered::to_value).collect();
        let cap = self.shared.config.max_result_bytes;
        match outcome {
            Ok(o) => {
                warnings.extend(o.warnings);
                let mut v = o.value;
                // Paging.
                let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
                let limit = args.get("limit").and_then(Value::as_u64).map(|l| l as usize);
                let mut total = None;
                if let Some(ptr) = page_pointer(name)
                    && (offset > 0 || limit.is_some())
                {
                    total = contract::page(&mut v, ptr, offset, limit);
                }
                let trimmed = contract::trim(&mut v, cap.saturating_sub(4096).max(1024));
                let mut sc = if v.is_object() { v.clone() } else { json!({"result": v.clone()}) };
                if let Some(t) = &trimmed {
                    let from = offset + t.kept;
                    sc["truncated"] = json!(true);
                    sc["continue_from"] = json!(from);
                    sc["total"] = json!(total.unwrap_or(offset + t.total));
                    sc["truncated_path"] = json!(t.path);
                    sc["next_step"] = json!(if page_pointer(name).is_some() {
                        format!("the answer was cut at {} items; call {name} again with offset: {from} for the rest", t.kept)
                    } else {
                        "the answer was cut to stay small; ask for less (one slide, a smaller range)".to_string()
                    });
                } else if let Some(t) = total {
                    sc["total"] = json!(t);
                    sc["offset"] = json!(offset);
                }
                if !warnings.is_empty() {
                    let mut all: Vec<Value> = sc.get("warnings").and_then(Value::as_array).cloned().unwrap_or_default();
                    all.extend(warnings.iter().map(|w| json!(w)));
                    sc["warnings"] = Value::Array(all);
                }
                if !renamed.is_empty() {
                    sc["previous_files_renamed"] = Value::Array(renamed.clone());
                    sc["notice"] = json!(format!(
                        "an existing file was kept: {} (pass overwrite: true to replace instead)",
                        renamed.iter().filter_map(|r| r.get("previous").and_then(Value::as_str)).collect::<Vec<_>>().join(", ")
                    ));
                }
                if !recovered.is_empty() {
                    sc["recovered_files"] = Value::Array(recovered);
                }
                let shown = if v.is_object() || sc.as_object().is_some_and(|o| o.len() > 1) { &sc } else { &v };
                let text = if serde_json::to_string(shown).map(|s| s.len()).unwrap_or(0) > cap / 2 {
                    serde_json::to_string(shown).unwrap_or_default()
                } else {
                    serde_json::to_string_pretty(shown).unwrap_or_default()
                };
                let mut content = vec![];
                if let Some(img) = o.image {
                    content.push(json!({"type": "image", "data": img, "mimeType": "image/png"}));
                }
                content.push(json!({"type": "text", "text": text}));
                json!({"content": content, "structuredContent": sc, "isError": false})
            }
            Err(e) => {
                let mut sc = json!({"error": e.to_value()});
                if !warnings.is_empty() {
                    sc["warnings"] = json!(warnings);
                }
                if !recovered.is_empty() {
                    sc["recovered_files"] = Value::Array(recovered);
                }
                let mut text = e.text();
                if !warnings.is_empty() {
                    text.push_str(&format!("\nwarnings: {}", warnings.join("; ")));
                }
                json!({"content": [{"type": "text", "text": text}], "structuredContent": sc, "isError": true})
            }
        }
    }
}
