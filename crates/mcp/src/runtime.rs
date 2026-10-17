//! The serving loop of the WHIQ contract: nothing hangs, work is never lost.
//!
//! - The main thread reads requests. `ping`, `tools/list`, `server_status`, `job_status` and
//!   `list_jobs` are answered at once from shared state; everything else is queued for one worker
//!   thread that owns the session, in order.
//! - A watchdog answers `busy` (with `retry_after_ms`) for a queued call that waited longer than
//!   `--busy-after-ms`, adds files finished `open` jobs read, and evicts (auto-saves and closes)
//!   the open presentations when the server is idle too long or uses too much memory.
//! - SIGTERM / SIGINT / SIGHUP (Ctrl+C, Ctrl+Break, console close on Windows) and the end of input
//!   auto-save unsaved work to recovered files before the process ends.

use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use deckcraft_engine::file_access::AutomationRoots;
use serde_json::{Value, json};

use crate::contract::{self, ToolError, code};
use crate::server::{Server, status_value};
use crate::state::{Shared, lock};

type Out = Arc<Mutex<Box<dyn Write + Send>>>;

struct Pending {
    msg: Value,
    id: Option<Value>,
    /// The tool of a `tools/call`, for `busy` answers.
    tool: Option<String>,
    at: Instant,
}

struct Queue {
    items: Mutex<VecDeque<Pending>>,
    cv: Condvar,
    closed: AtomicBool,
}

fn write_line(out: &Out, v: &Value) {
    let mut w = lock(out);
    let line = v.to_string();
    if w.write_all(line.as_bytes()).and_then(|()| w.write_all(b"\n")).and_then(|()| w.flush()).is_err() {
        // The client went away; the end of input will follow.
        log::debug!("could not write a reply");
    }
}

/// A `CallToolResult` built outside the worker (status answers and `busy`).
fn tool_result(v: Value) -> Value {
    let text = serde_json::to_string_pretty(&v).unwrap_or_default();
    let sc = if v.is_object() { v } else { json!({"result": v}) };
    json!({"content": [{"type": "text", "text": text}], "structuredContent": sc, "isError": false})
}

fn tool_error(e: &ToolError) -> Value {
    json!({"content": [{"type": "text", "text": e.text()}], "structuredContent": {"error": e.to_value()}, "isError": true})
}

/// What the signal handler needs: the server (if it can get it) and what to save otherwise.
struct Running {
    server: Weak<Mutex<Server>>,
    shared: Arc<Shared>,
    roots: Option<Arc<AutomationRoots>>,
    out: Out,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

fn install_signal_handler() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let r = ctrlc::set_handler(|| {
            let (server, shared, roots, out) = match lock(&RUNNING).as_ref() {
                Some(r) => (r.server.upgrade(), Arc::clone(&r.shared), r.roots.clone(), Arc::clone(&r.out)),
                None => std::process::exit(0),
            };
            eprintln!("deckcraft-cli: stopping on a signal; saving unsaved work");
            // The worker finishes its call first, for a few seconds at most; else save what it last saw.
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut saved = false;
            if let Some(server) = server {
                while Instant::now() < deadline {
                    if let Ok(mut s) = server.try_lock() {
                        s.shutdown_save("signal");
                        saved = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            if !saved {
                crate::autosave::save_snapshots(&shared, roots.as_ref(), &shared.docs(), "signal");
            }
            let _ = lock(&out).flush();
            std::process::exit(0);
        });
        if let Err(e) = r {
            eprintln!("deckcraft-cli: no signal handler ({e}); unsaved work is still saved at the end of input");
        }
    });
}

/// Answer the requests that never wait. `None`: queue it.
fn fast_answer(msg: &Value, shared: &Arc<Shared>, server: &Arc<Mutex<Server>>, queue: &Queue, out: &Out) -> Option<Value> {
    let id = msg.get("id")?.clone();
    if !(id.is_string() || id.is_number()) {
        return None;
    }
    let method = msg.get("method").and_then(Value::as_str)?;
    let result = match method {
        "ping" => json!({}),
        "tools/list" => json!({"tools": crate::tools::tool_definitions()}),
        "tools/call" => {
            let params = msg.get("params")?;
            let raw = params.get("name").and_then(Value::as_str)?;
            let names = crate::server::tool_names();
            let name = contract::resolve_tool(raw, &names).ok()?;
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            match name {
                "server_status" => tool_result(status_value(shared)),
                "cancel" => {
                    let job = args.get("job_id").or_else(|| args.get("jobId")).map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string()));
                    let mut answer = json!({});
                    if let Some(id) = job {
                        match shared.cancel_job(&id) {
                            Some(state) => answer = json!({"job_id": id, "state": state, "cancelled": state != "done" && state != "failed"}),
                            None => {
                                return Some(
                                    json!({"jsonrpc": "2.0", "id": id_of(msg), "result": tool_error(&ToolError::new(code::NOT_FOUND, format!("no job `{id}`"), "list_jobs shows the jobs of this server"))}),
                                );
                            }
                        }
                    } else {
                        match shared.cancel_current() {
                            Some((tool, ms)) => {
                                answer = json!({"cancelled": true, "running": tool, "elapsed_ms": ms, "note": "it stops at its next safe point; its answer says `cancelled`"})
                            }
                            None => answer = json!({"cancelled": false, "reason": "nothing is running"}),
                        }
                    }
                    if args.get("queued").and_then(Value::as_bool) == Some(true) {
                        let dropped: Vec<Pending> = lock(&queue.items).drain(..).collect();
                        let n = dropped.len();
                        for p in dropped {
                            if let Some(pid) = p.id.clone() {
                                let e = ToolError::new(
                                    code::CANCELLED,
                                    "dropped from the queue by cancel {queued: true}",
                                    "send the call again if it is still wanted",
                                );
                                let reply = if p.tool.is_some() {
                                    json!({"jsonrpc": "2.0", "id": pid, "result": tool_error(&e)})
                                } else {
                                    json!({"jsonrpc": "2.0", "id": pid, "error": {"code": -32800, "message": e.reason, "data": e.to_value()}})
                                };
                                write_line(out, &reply);
                            }
                        }
                        answer["dropped_from_queue"] = json!(n);
                    }
                    tool_result(answer)
                }
                "list_jobs" => {
                    adopt_if_free(shared, server);
                    tool_result(Value::Array(shared.jobs_value()))
                }
                "job_status" => {
                    adopt_if_free(shared, server);
                    let jid = args
                        .get("job_id")
                        .or_else(|| args.get("jobId"))
                        .or_else(|| args.get("id"))
                        .map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string()))
                        .unwrap_or_default();
                    match shared.job_value(&jid) {
                        Some(v) => tool_result(v),
                        None => {
                            let ids = shared.job_ids();
                            tool_error(
                                &ToolError::new(code::NOT_FOUND, format!("no job `{jid}`"), "list_jobs shows the jobs of this server")
                                    .with("candidates", json!(contract::closest(&jid, ids.iter().map(String::as_str), 5))),
                            )
                        }
                    }
                }
                _ => return None,
            }
        }
        _ => return None,
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// Finished `open` jobs join the session when the worker is free.
fn adopt_if_free(shared: &Shared, server: &Arc<Mutex<Server>>) {
    if shared.has_pending_opens()
        && let Ok(mut s) = server.try_lock()
    {
        s.adopt_pending();
    }
}

fn id_of(msg: &Value) -> Value {
    msg.get("id").cloned().unwrap_or(Value::Null)
}

fn busy_reply(p: &Pending, shared: &Shared) -> Option<Value> {
    let id = p.id.clone()?;
    let (running, elapsed) = shared.current().unwrap_or_else(|| ("a previous call".into(), 0));
    let retry = (elapsed / 2).clamp(250, 5_000);
    shared.busy_replies.fetch_add(1, Ordering::Relaxed);
    let e = ToolError::new(
        code::BUSY,
        format!("the server is still working on `{running}` ({elapsed} ms so far); this call was not run"),
        format!("retry this call in {retry} ms; server_status answers at once and shows progress"),
    )
    .with("retry_after_ms", json!(retry))
    .with("running", json!(running));
    if p.tool.is_some() {
        Some(json!({"jsonrpc": "2.0", "id": id, "result": tool_error(&e)}))
    } else {
        Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": e.reason, "data": e.to_value()}}))
    }
}

pub fn serve(server: Server, input: impl BufRead + Send + 'static, output: impl Write + Send + 'static) -> std::io::Result<()> {
    let shared = server.shared();
    let roots = {
        let mut s = server;
        let r = s.backend().automation_roots();
        (Arc::new(Mutex::new(s)), r)
    };
    let (server, roots) = roots;
    let out: Out = Arc::new(Mutex::new(Box::new(output)));
    *lock(&RUNNING) = Some(Running { server: Arc::downgrade(&server), shared: Arc::clone(&shared), roots: roots.clone(), out: Arc::clone(&out) });
    install_signal_handler();

    let queue = Arc::new(Queue { items: Mutex::new(VecDeque::new()), cv: Condvar::new(), closed: AtomicBool::new(false) });

    // The worker: one call at a time, in order.
    let worker = {
        let (queue, server, out, shared) = (Arc::clone(&queue), Arc::clone(&server), Arc::clone(&out), Arc::clone(&shared));
        std::thread::Builder::new().name("deckcraft-mcp-worker".into()).spawn(move || {
            loop {
                let next = {
                    let mut items = lock(&queue.items);
                    loop {
                        if let Some(p) = items.pop_front() {
                            break Some(p);
                        }
                        if queue.closed.load(Ordering::SeqCst) {
                            break None;
                        }
                        items = queue.cv.wait_timeout(items, Duration::from_millis(200)).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
                    }
                };
                let Some(p) = next else { break };
                shared.set_current(Some(p.tool.as_deref().unwrap_or_else(|| p.msg.get("method").and_then(Value::as_str).unwrap_or("request"))));
                let reply = {
                    let mut s = lock(&server);
                    s.handle(p.msg)
                };
                shared.set_current(None);
                if let Some(r) = reply {
                    write_line(&out, &r);
                }
            }
        })?
    };

    // The watchdog.
    let stop = Arc::new(AtomicBool::new(false));
    let dog = {
        let (queue, server, out, shared, stop) = (Arc::clone(&queue), Arc::clone(&server), Arc::clone(&out), Arc::clone(&shared), Arc::clone(&stop));
        std::thread::Builder::new().name("deckcraft-mcp-watchdog".into()).spawn(move || {
            let busy_after = Duration::from_millis(shared.config.busy_after_ms.max(1));
            let mut last_memory_evict: Option<Instant> = None;
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(50));
                // Calls that waited too long: answer busy now.
                let timed_out: Vec<Pending> = {
                    let mut items = lock(&queue.items);
                    let mut keep = VecDeque::with_capacity(items.len());
                    let mut gone = vec![];
                    for p in items.drain(..) {
                        if p.id.is_some() && p.at.elapsed() > busy_after { gone.push(p) } else { keep.push_back(p) }
                    }
                    *items = keep;
                    gone
                };
                for p in &timed_out {
                    if let Some(r) = busy_reply(p, &shared) {
                        write_line(&out, &r);
                    }
                }
                adopt_if_free(&shared, &server);
                let quiet = shared.current().is_none() && lock(&queue.items).is_empty();
                let has_docs = !shared.docs().is_empty();
                if let Some(secs) = shared.config.idle_evict_secs
                    && quiet
                    && has_docs
                    && shared.idle_for() >= Duration::from_secs(secs)
                    && let Ok(mut s) = server.try_lock()
                {
                    let ev = s.evict_all("idle");
                    if !ev.is_empty() {
                        eprintln!("deckcraft-cli: evicted {} idle presentation(s)", ev.len());
                    }
                }
                if let (Some(limit), Some(rss)) = (shared.config.max_memory_mb, crate::state::rss_bytes())
                    && rss / (1024 * 1024) > limit
                    && has_docs
                    && last_memory_evict.is_none_or(|t| t.elapsed() > Duration::from_secs(10))
                    && let Ok(mut s) = server.try_lock()
                {
                    last_memory_evict = Some(Instant::now());
                    let ev = s.evict_all("memory");
                    eprintln!("deckcraft-cli: memory {} MB over {limit} MB: evicted {} presentation(s)", rss / (1024 * 1024), ev.len());
                }
            }
        })?
    };

    // The checkpointer (WHIQ): unsaved work goes to its checkpoint every N edits or seconds, so a
    // hard kill loses little. It works from the worker's snapshots and never waits for a call.
    let checkpointer = {
        let (shared, stop, roots) = (Arc::clone(&shared), Arc::clone(&stop), roots.clone());
        std::thread::Builder::new().name("deckcraft-mcp-checkpoint".into()).spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(200));
                for path in crate::checkpoint::tick(&shared, roots.as_ref(), Instant::now()) {
                    eprintln!("deckcraft-cli: checkpoint written to {path}");
                }
            }
        })?
    };

    // The reader.
    let mut read_error = None;
    for line in input.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                read_error = Some(e);
                break;
            }
        };
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let msg = match serde_json::from_str::<Value>(t) {
            Ok(m) => m,
            Err(e) => {
                write_line(&out, &json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}}));
                continue;
            }
        };
        if let Some(r) = fast_answer(&msg, &shared, &server, &queue, &out) {
            write_line(&out, &r);
            continue;
        }
        let id = msg.get("id").cloned().filter(|i| i.is_string() || i.is_number());
        let tool = (msg.get("method").and_then(Value::as_str) == Some("tools/call"))
            .then(|| msg.pointer("/params/name").and_then(Value::as_str).map(String::from))
            .flatten();
        let mut items = lock(&queue.items);
        items.push_back(Pending { msg, id, tool, at: Instant::now() });
        queue.cv.notify_one();
    }
    queue.closed.store(true, Ordering::SeqCst);
    queue.cv.notify_all();
    let _ = worker.join();
    stop.store(true, Ordering::SeqCst);
    let _ = dog.join();
    let _ = checkpointer.join();
    // The end of input: unsaved work goes to recovered files, never away.
    {
        let mut s = lock(&server);
        s.shutdown_save("shutdown");
    }
    *lock(&RUNNING) = None;
    let _ = lock(&out).flush();
    match read_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
