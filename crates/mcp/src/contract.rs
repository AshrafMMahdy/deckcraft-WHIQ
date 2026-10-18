//! WHIQ "no dead stops" contract helpers: structured errors (`code`, `reason`, `next_step`),
//! forgiving tool names and arguments, and trimming of oversized results.
//!
//! Every limit is a recoverable answer: an error always says what the agent can do next.

use serde_json::{Map, Value, json};

/// The error codes an agent can branch on: the same set in WordCraft, GridCraft and DeckCraft
/// (CONTRACT-PARITY.md). Every error a tool answers carries one of these.
pub mod code {
    pub const BUSY: &str = "busy";
    pub const CANCELLED: &str = "cancelled";
    pub const DISK_FULL: &str = "disk_full";
    pub const EVICTED: &str = "evicted";
    pub const FAILED: &str = "failed";
    pub const INTERNAL: &str = "internal";
    pub const INVALID_ARGUMENT: &str = "invalid_argument";
    pub const INVALID_REQUEST: &str = "invalid_request";
    pub const LOCKED: &str = "locked";
    pub const NO_DOCUMENT: &str = "no_document";
    pub const NOT_AVAILABLE: &str = "not_available";
    pub const NOT_FOUND: &str = "not_found";
    pub const OUT_OF_RANGE: &str = "out_of_range";
    pub const OUTSIDE_ROOT: &str = "outside_root";
    pub const PASSWORD_REQUIRED: &str = "password_required";
    pub const TOO_LARGE: &str = "too_large";
    pub const UNSUPPORTED: &str = "unsupported";
    pub const WRONG_PASSWORD: &str = "wrong_password";
    /// Every code, sorted.
    pub const ALL: &[&str] = &[
        BUSY,
        CANCELLED,
        DISK_FULL,
        EVICTED,
        FAILED,
        INTERNAL,
        INVALID_ARGUMENT,
        INVALID_REQUEST,
        LOCKED,
        NO_DOCUMENT,
        NOT_AVAILABLE,
        NOT_FOUND,
        OUT_OF_RANGE,
        OUTSIDE_ROOT,
        PASSWORD_REQUIRED,
        TOO_LARGE,
        UNSUPPORTED,
        WRONG_PASSWORD,
    ];
}

/// Arguments every tool takes besides its own: the same in every craft app (CONTRACT-PARITY.md).
pub const COMMON_ARGS: &[&str] = &["caller", "async", "overwrite", "continue_from", "max_bytes"];

/// A structured tool error.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolError {
    pub code: String,
    pub reason: String,
    pub next_step: String,
    /// Extra fields (`retry_after_ms`, `closest`, `job_id`, `recovered_path`…).
    pub extra: Map<String, Value>,
}

impl ToolError {
    pub fn new(code: &str, reason: impl Into<String>, next_step: impl Into<String>) -> Self {
        Self { code: code.to_string(), reason: one_line(&reason.into()), next_step: next_step.into(), extra: Map::new() }
    }
    pub fn with(mut self, key: &str, v: Value) -> Self {
        self.extra.insert(key.to_string(), v);
        self
    }
    /// `{"code", "reason", "next_step", ...extra}`.
    pub fn to_value(&self) -> Value {
        let mut o = Map::new();
        o.insert("code".into(), json!(self.code));
        o.insert("reason".into(), json!(self.reason));
        o.insert("next_step".into(), json!(self.next_step));
        for (k, v) in &self.extra {
            o.insert(k.clone(), v.clone());
        }
        Value::Object(o)
    }
    /// The one-line text form shown in `content`.
    pub fn text(&self) -> String {
        format!("[{}] {}\nnext_step: {}", self.code, self.reason, self.next_step)
    }
}

/// The reason on one line (an agent reads the first line).
fn one_line(s: &str) -> String {
    let s = s.trim();
    let flat: String = s.split('\n').map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" | ");
    if flat.chars().count() > 600 { format!("{}…", flat.chars().take(600).collect::<String>()) } else { flat }
}

/// What the classifier knows about the call that failed.
#[derive(Default)]
pub struct ErrorContext<'a> {
    pub tool: &'a str,
    pub read_root: Option<String>,
    pub write_root: Option<String>,
    pub slide_count: Option<usize>,
}

/// Turn an error message from the engine or a backend into a [`ToolError`].
pub fn classify(msg: &str, cx: &ErrorContext<'_>) -> ToolError {
    let l = msg.to_ascii_lowercase();
    let tool = cx.tool;
    if l.contains("outside the read root") || l.contains("read authority is absent") {
        let root = cx.read_root.clone().unwrap_or_else(|| "(none: reading files is not granted)".into());
        return ToolError::new(
            code::OUTSIDE_ROOT,
            msg,
            format!("use a path inside the read root {root}; relative paths are resolved inside the write root"),
        )
        .with("read_root", json!(cx.read_root))
        .with("write_root", json!(cx.write_root));
    }
    if l.contains("outside the write root") || l.contains("write authority is absent") || l.contains("automation path rejected") {
        let root = cx.write_root.clone().unwrap_or_else(|| "(none: writing files is not granted)".into());
        return ToolError::new(
            code::OUTSIDE_ROOT,
            msg,
            format!("save inside the write root {root}; a bare file name such as `deck.pptx` lands there"),
        )
        .with("read_root", json!(cx.read_root))
        .with("write_root", json!(cx.write_root));
    }
    if l.contains("os error 32")
        || l.contains("os error 33")
        || l.contains("being used by another process")
        || l.contains("permission denied")
        || l.contains("access is denied")
    {
        return ToolError::new(
            code::LOCKED,
            msg,
            "the file is open in another program or read-only: close it there and retry, or save under another name",
        )
        .with("retry_after_ms", json!(5000));
    }
    if l.contains("no space left") || l.contains("os error 28") || l.contains("os error 112") || l.contains("disk full") {
        return ToolError::new(code::DISK_FULL, msg, "free space on the disk and save again; the presentation is still open");
    }
    if l.contains("password") && (l.contains("wrong") || l.contains("incorrect")) {
        return ToolError::new(code::WRONG_PASSWORD, msg, "call open_presentation again with the right `password`");
    }
    if l.contains("password-protected") || l.contains("password protected") || l.contains("encrypted") {
        return ToolError::new(
            code::PASSWORD_REQUIRED,
            msg,
            "the file is password-protected: open it with its password, or ask the owner for an unprotected copy",
        );
    }
    if l.contains("cancelled") {
        return ToolError::new(code::CANCELLED, msg, "the call stopped at a safe point and changed nothing past it; call it again to do the work");
    }
    if l.contains("no presentation open") || l.contains("no document") || l.contains("no active") || l.contains("nothing is open") {
        return ToolError::new(code::NO_DOCUMENT, msg, "call new_presentation, open_presentation or build_deck first");
    }
    if l.contains("unknown command") {
        return ToolError::new(code::NOT_FOUND, msg, "call list_commands with a filter to find the command id");
    }
    if l.contains("not available") || l.contains("is disabled") {
        return ToolError::new(
            code::NOT_AVAILABLE,
            msg,
            "the command can't run in the current state (selection, slide): change that first, or use another command",
        );
    }
    if l.contains("needs the desktop app") || l.contains("is a ui command") || l.contains("unsupported") {
        return ToolError::new(code::UNSUPPORTED, msg, "this cannot be done headless; use render_slide / export / run_command instead");
    }
    if l.contains("no such file") || l.contains("os error 2") || l.contains("cannot find") || l.contains("not found") {
        return ToolError::new(code::NOT_FOUND, msg, "check the path; the closest matches are listed in `closest` when there are any");
    }
    if let Some(n) = slide_number_in(&l) {
        let range = cx
            .slide_count
            .map(|c| if c == 0 { "the presentation has no slides".to_string() } else { format!("valid slide indices are 0..{}", c - 1) });
        return ToolError::new(
            code::OUT_OF_RANGE,
            msg,
            format!("{}; inspect_document lists the slides", range.unwrap_or_else(|| "use an existing slide index".into())),
        )
        .with("index", json!(n));
    }
    if l.contains("no shape") || l.contains("no such shape") || l.contains("unknown shape") || l.contains("is not a shape id") {
        return ToolError::new(code::NOT_FOUND, msg, "call inspect_slide to list the shape ids on the current slide (see `closest`)");
    }
    if l.contains("too large") || l.contains("exceeds") || l.contains("too many") || l.contains("too big") {
        return ToolError::new(code::TOO_LARGE, msg, "ask for less at once: a slide range (slides: \"1-10\"), a smaller scale, or offset/limit");
    }
    if l.contains("missing")
        || l.contains("invalid")
        || l.contains("must be")
        || l.contains("needs ")
        || l.contains("isn't")
        || l.contains("is not")
        || l.contains("unknown")
        || l.contains("expected")
        || l.contains("bad ")
    {
        return ToolError::new(code::INVALID_ARGUMENT, msg, format!("fix the argument named in the reason; tools/list has the schema of `{tool}`"));
    }
    ToolError::new(
        code::FAILED,
        msg,
        format!("check the arguments of `{tool}`; inspect_document shows the current state, server_status the server's"),
    )
}

fn slide_number_in(l: &str) -> Option<u64> {
    let i = l.find("no slide ")?;
    let rest = l.get(i + "no slide ".len()..)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Lowercase with `_` between words: `renderSlide`, `Render-Slide`, `render slide` → `render_slide`.
pub fn snake(name: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for c in name.trim().chars() {
        if c.is_ascii_uppercase() {
            if prev_lower {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
            prev_lower = false;
        } else if matches!(c, '-' | ' ' | '.' | '/') {
            out.push('_');
            prev_lower = false;
        } else {
            out.push(c);
            prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').to_string()
}

/// A key squashed for loose matching: lowercase, no `_`, `-`, spaces.
pub fn squash(k: &str) -> String {
    k.chars().filter(|c| !matches!(c, '_' | '-' | ' ' | '.')).flat_map(char::to_lowercase).collect()
}

/// Levenshtein distance (bounded inputs: names and ids).
pub fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().take(128).collect();
    let b: Vec<char> = b.chars().take(128).collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev.get(j).copied().unwrap_or(0) + usize::from(ca != cb);
            let del = prev.get(j + 1).copied().unwrap_or(0) + 1;
            let ins = cur.get(j).copied().unwrap_or(0) + 1;
            cur.push(sub.min(del).min(ins));
        }
        prev = cur;
    }
    prev.last().copied().unwrap_or(0)
}

/// The `n` names closest to `want` (substring matches first).
pub fn closest<'a>(want: &str, names: impl IntoIterator<Item = &'a str>, n: usize) -> Vec<String> {
    let w = squash(want);
    let mut scored: Vec<(usize, &str)> = names
        .into_iter()
        .map(|name| {
            let s = squash(name);
            let d = if !w.is_empty() && (s.contains(&w) || w.contains(&s)) { 0 } else { distance(&w, &s) };
            (d, name)
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(b.1)));
    scored.into_iter().take(n).map(|(_, s)| s.to_string()).collect()
}

/// Short names agents use for tools.
const TOOL_ALIASES: &[(&str, &str)] = &[
    ("open", "open_presentation"),
    ("open_deck", "open_presentation"),
    ("save", "save_presentation"),
    ("save_deck", "save_presentation"),
    ("new", "new_presentation"),
    ("new_deck", "new_presentation"),
    ("status", "server_status"),
    ("render", "render_slide"),
    ("inspect", "inspect_document"),
    ("build", "build_deck"),
    ("job", "job_status"),
    ("jobs", "list_jobs"),
    ("overlay", "add_overlays"),
    ("overlays", "add_overlays"),
    ("close", "close_presentation"),
    ("theme_from_template", "adopt_template"),
    ("apply_template", "adopt_template"),
];

/// Resolve a tool name forgivingly. `Err` carries the closest names.
pub fn resolve_tool<'a>(name: &str, names: &[&'a str]) -> Result<&'a str, Vec<String>> {
    if let Some(n) = names.iter().find(|n| **n == name) {
        return Ok(n);
    }
    let s = snake(name);
    if let Some(n) = names.iter().find(|n| **n == s || squash(n) == squash(name)) {
        return Ok(n);
    }
    if let Some((_, to)) = TOOL_ALIASES.iter().find(|(a, _)| *a == s)
        && let Some(n) = names.iter().find(|n| *n == to)
    {
        return Ok(n);
    }
    Err(closest(name, names.iter().copied(), 3))
}

/// Resolve a command id forgivingly (`Slide.New`, `slide_new`, `slideNew` → `slide.new`).
pub fn resolve_command<'a>(id: &str, ids: &[&'a str]) -> Result<&'a str, Vec<String>> {
    if let Some(i) = ids.iter().find(|i| **i == id) {
        return Ok(i);
    }
    if let Some(i) = ids.iter().find(|i| i.eq_ignore_ascii_case(id)) {
        return Ok(i);
    }
    let want = squash(id);
    let hits: Vec<&&str> = ids.iter().filter(|i| squash(i) == want).collect();
    if let [one] = hits.as_slice() {
        return Ok(one);
    }
    Err(closest(id, ids.iter().copied(), 5))
}

/// Argument-name aliases agents commonly use: (alias, canonical).
const ARG_ALIASES: &[(&str, &str)] = &[
    ("file", "path"),
    ("filename", "path"),
    ("filepath", "path"),
    ("file_path", "path"),
    ("output", "path"),
    ("out", "path"),
    ("idx", "index"),
    ("slideindex", "slide"),
    ("page", "slide"),
    ("args", "params"),
    ("arguments", "params"),
    ("parameters", "params"),
    ("cmd", "command"),
    ("id_list", "ids"),
    ("jobid", "job_id"),
    ("job", "job_id"),
    ("background", "async"),
    ("continuefrom", "offset"),
    ("offset", "continue_from"),
];

/// Normalised arguments plus warnings about what was renamed, coerced or dropped.
pub struct Normalized {
    pub args: Value,
    pub warnings: Vec<String>,
}

/// Map `args` onto the tool's schema: keys matched case-insensitively and with either spelling,
/// numbers and booleans given as strings coerced, unknown keys dropped with a warning.
pub fn normalize_args(tool: &str, schema: &Value, args: &Value) -> Normalized {
    let mut warnings = vec![];
    let Some(obj) = args.as_object() else {
        if !args.is_null() {
            warnings.push(format!("{tool}: arguments must be an object; ignored {}", short(args)));
        }
        return Normalized { args: json!({}), warnings };
    };
    let props = schema.get("properties").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut out = Map::new();
    for (k, v) in obj {
        let canonical = if props.contains_key(k) || (COMMON_ARGS.contains(&k.as_str()) && !props.contains_key("offset")) {
            Some(k.clone())
        } else {
            let sq = squash(k);
            props.keys().find(|p| squash(p) == sq).cloned().or_else(|| {
                ARG_ALIASES
                    .iter()
                    .find(|(a, _)| squash(a) == sq)
                    .map(|(_, c)| (*c).to_string())
                    .filter(|c| props.contains_key(c) && !obj.contains_key(c))
            })
        };
        match canonical {
            Some(c) => {
                if &c != k {
                    warnings.push(format!("{tool}: argument `{k}` read as `{c}`"));
                }
                let v = coerce(&c, v, props.get(&c), tool, &mut warnings);
                out.insert(c, v);
            }
            None => {
                let mut names: Vec<&str> = props.keys().map(String::as_str).collect();
                names.sort_unstable();
                let near = closest(k, names.iter().copied(), 2);
                warnings.push(format!(
                    "{tool}: unknown argument `{k}` ignored (expected one of: {}; closest: {})",
                    names.join(", "),
                    near.join(", ")
                ));
            }
        }
    }
    Normalized { args: Value::Object(out), warnings }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 80 { format!("{}…", s.chars().take(80).collect::<String>()) } else { s }
}

/// Coerce a value to the schema type where that is safe.
fn coerce(key: &str, v: &Value, schema: Option<&Value>, tool: &str, warnings: &mut Vec<String>) -> Value {
    // Enumerated values in any case or spelling.
    if let (Some(allowed), Some(given)) = (schema.and_then(|s| s.get("enum")).and_then(Value::as_array), v.as_str())
        && !allowed.iter().any(|a| a.as_str() == Some(given))
        && let Some(hit) = allowed.iter().filter_map(Value::as_str).find(|a| squash(a) == squash(given))
    {
        warnings.push(format!("{tool}: `{key}`: `{given}` read as `{hit}`"));
        return json!(hit);
    }
    let Some(ty) = schema.and_then(|s| s.get("type")).and_then(Value::as_str) else { return v.clone() };
    match (ty, v) {
        ("integer", Value::String(s)) => match s.trim().parse::<i64>() {
            Ok(n) => {
                warnings.push(format!("{tool}: `{key}` given as a string; read as {n}"));
                json!(n)
            }
            Err(_) => v.clone(),
        },
        ("integer", Value::Number(n)) if n.as_i64().is_none() && n.as_u64().is_none() => match n.as_f64() {
            Some(f) if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e15 => json!(f as i64),
            _ => v.clone(),
        },
        ("number", Value::String(s)) => match s.trim().trim_end_matches("pt").trim().parse::<f64>() {
            Ok(n) if n.is_finite() => {
                warnings.push(format!("{tool}: `{key}` given as a string; read as {n}"));
                json!(n)
            }
            _ => v.clone(),
        },
        ("boolean", Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" | "on" => json!(true),
            "false" | "no" | "0" | "off" => json!(false),
            _ => v.clone(),
        },
        ("boolean", Value::Number(n)) => json!(n.as_f64().is_some_and(|f| f != 0.0)),
        ("array", Value::String(s)) if schema.and_then(|s| s.pointer("/items/type")).and_then(Value::as_str) == Some("integer") => {
            let parsed: Vec<i64> = s.split([',', ' ']).filter(|p| !p.trim().is_empty()).filter_map(|p| p.trim().parse().ok()).collect();
            if parsed.is_empty() { v.clone() } else { json!(parsed) }
        }
        ("array", Value::String(s)) if schema.and_then(|s| s.pointer("/items/type")).and_then(Value::as_str).is_none_or(|t| t == "string") => {
            json!(s.split(',').map(str::trim).filter(|p| !p.is_empty()).collect::<Vec<_>>())
        }
        ("array", Value::Number(_)) => json!([v]),
        _ => v.clone(),
    }
}

/// What [`trim`] cut.
#[derive(Clone, Debug, PartialEq)]
pub struct Trimmed {
    /// JSON pointer of the array that was cut (`""` = the top level).
    pub path: String,
    pub kept: usize,
    pub total: usize,
}

/// Cut `v` so its JSON is at most `cap` bytes: the largest array loses its tail (the caller adds
/// `truncated` / `continue_from`), and very long strings are shortened. Never floods the caller.
pub fn trim(v: &mut Value, cap: usize) -> Option<Trimmed> {
    let len = |v: &Value| serde_json::to_string(v).map(|s| s.len()).unwrap_or(0);
    if len(v) <= cap {
        return None;
    }
    let mut first: Option<Trimmed> = None;
    for _ in 0..4 {
        let total_len = len(v);
        if total_len <= cap {
            break;
        }
        let Some((path, arr_len)) = largest_array(v, "", 0) else { break };
        let Some(arr) = v.pointer_mut(&path).and_then(Value::as_array_mut) else { break };
        let total = arr.len();
        // Bytes the rest of the document takes, then keep as many items as fit.
        let rest = total_len.saturating_sub(arr_len);
        let budget = (cap.saturating_sub(rest)).saturating_mul(9) / 10;
        let mut used = 2usize;
        let mut kept = 0usize;
        for item in arr.iter() {
            let l = serde_json::to_string(item).map(|s| s.len()).unwrap_or(0) + 1;
            if used + l > budget {
                break;
            }
            used += l;
            kept += 1;
        }
        if kept >= total {
            break;
        }
        arr.truncate(kept);
        if first.is_none() {
            first = Some(Trimmed { path: path.clone(), kept, total });
        }
    }
    if len(v) > cap {
        shorten_strings(v, (cap / 8).max(256), 0);
        if first.is_none() {
            first = Some(Trimmed { path: String::new(), kept: 0, total: 0 });
        }
    }
    first
}

fn largest_array(v: &Value, path: &str, depth: usize) -> Option<(String, usize)> {
    if depth > 6 {
        return None;
    }
    let here = match v {
        Value::Array(a) if a.len() > 1 => Some((path.to_string(), serde_json::to_string(v).map(|s| s.len()).unwrap_or(0), a.len())),
        _ => None,
    };
    let mut best = here.map(|(p, l, _)| (p, l));
    let children: Vec<(String, &Value)> = match v {
        Value::Object(o) => o.iter().map(|(k, c)| (format!("{path}/{}", k.replace('~', "~0").replace('/', "~1")), c)).collect(),
        Value::Array(a) => a.iter().enumerate().take(64).map(|(i, c)| (format!("{path}/{i}"), c)).collect(),
        _ => vec![],
    };
    for (p, c) in children {
        if let Some((cp, cl)) = largest_array(c, &p, depth + 1) {
            // Prefer the inner array only when it holds most of the bytes.
            let replace = match &best {
                None => true,
                Some((_, bl)) => cl * 10 > *bl * 8 && v.is_object(),
            };
            if replace {
                best = Some((cp, cl));
            }
        }
    }
    best
}

fn shorten_strings(v: &mut Value, max: usize, depth: usize) {
    if depth > 16 {
        return;
    }
    match v {
        Value::String(s) if s.len() > max => {
            let n = s.chars().count();
            let keep: String = s.chars().take(max).collect();
            *s = format!("{keep}…[truncated {} of {n} characters]", n.saturating_sub(max));
        }
        Value::Array(a) => a.iter_mut().for_each(|c| shorten_strings(c, max, depth + 1)),
        Value::Object(o) => o.values_mut().for_each(|c| shorten_strings(c, max, depth + 1)),
        _ => {}
    }
}

/// The largest array of `v` (where [`trim`] would cut): its JSON pointer.
pub fn bulk_pointer(v: &Value) -> Option<String> {
    largest_array(v, "", 0).map(|(p, _)| p)
}

/// `offset` / `limit` applied to the array at `pointer` of `v`; returns the total length.
pub fn page(v: &mut Value, pointer: &str, offset: usize, limit: Option<usize>) -> Option<usize> {
    let arr = v.pointer_mut(pointer)?.as_array_mut()?;
    let total = arr.len();
    let start = offset.min(total);
    let end = limit.map_or(total, |l| start.saturating_add(l).min(total));
    let kept: Vec<Value> = arr.drain(..).skip(start).take(end - start).collect();
    *arr = kept;
    Some(total)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn names_are_forgiving() {
        let names = ["render_slide", "open_presentation", "server_status"];
        assert_eq!(resolve_tool("renderSlide", &names), Ok("render_slide"));
        assert_eq!(resolve_tool("Render-Slide", &names), Ok("render_slide"));
        assert_eq!(resolve_tool("open", &names), Ok("open_presentation"));
        assert_eq!(resolve_tool("STATUS", &names), Ok("server_status"));
        let e = resolve_tool("rendr_slide", &names).unwrap_err();
        assert_eq!(e.first().map(String::as_str), Some("render_slide"));
        let ids = ["slide.new", "slide.notes", "shape.fill"];
        assert_eq!(resolve_command("Slide.New", &ids), Ok("slide.new"));
        assert_eq!(resolve_command("slide_new", &ids), Ok("slide.new"));
        assert!(resolve_command("slide.nwe", &ids).unwrap_err().contains(&"slide.new".to_string()));
    }

    #[test]
    fn args_are_mapped_and_unknown_ones_warned() {
        let schema = json!({"properties": {"path": {"type": "string"}, "keepGoing": {"type": "boolean"}, "slide": {"type": "integer"}}});
        let n = normalize_args("t", &schema, &json!({"File": "a.pptx", "keep_going": "true", "slide": "3", "bogus": 1}));
        assert_eq!(n.args, json!({"path": "a.pptx", "keepGoing": true, "slide": 3}));
        assert!(n.warnings.iter().any(|w| w.contains("bogus")), "{:?}", n.warnings);
    }

    #[test]
    fn oversized_values_are_trimmed_with_a_marker() {
        let mut v = json!({"slides": (0..5000).map(|i| json!({"index": i, "title": "x".repeat(40)})).collect::<Vec<_>>(), "theme": "T"});
        let t = trim(&mut v, 20_000).unwrap();
        assert_eq!(t.path, "/slides");
        assert_eq!(t.total, 5000);
        assert!(t.kept > 10 && t.kept < 5000);
        assert!(serde_json::to_string(&v).unwrap().len() <= 20_000);
        let mut small = json!({"a": 1});
        assert!(trim(&mut small, 100).is_none());
    }

    #[test]
    fn errors_are_classified() {
        let cx = ErrorContext { tool: "save_presentation", write_root: Some("/w".into()), ..Default::default() };
        let e = classify("automation path rejected: outside the write root /w: /etc/x", &cx);
        assert_eq!(e.code, code::OUTSIDE_ROOT);
        assert!(e.next_step.contains("/w"));
        let cx = ErrorContext { tool: "go_to_slide", slide_count: Some(3), ..Default::default() };
        let e = classify("slide.go: no slide 7", &cx);
        assert_eq!(e.code, code::OUT_OF_RANGE);
        assert!(e.next_step.contains("0..2"), "{}", e.next_step);
        let e = classify("deck.pptx: No space left on device (os error 28)", &cx);
        assert_eq!(e.code, code::DISK_FULL);
        assert!(code::ALL.contains(&e.code.as_str()));
    }
}
