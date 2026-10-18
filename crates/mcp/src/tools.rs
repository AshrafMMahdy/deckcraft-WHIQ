//! MCP tool definitions (names, descriptions, JSON Schemas) and their implementations on top of a
//! [`Backend`].

use serde_json::{Value, json};

use crate::backend::Backend;
use crate::headless::NEEDS_APP;

/// The result of `tools/call`: MCP content blocks plus the `isError` flag.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    pub content: Vec<Value>,
    pub is_error: bool,
}

impl ToolResult {
    pub fn text(t: impl Into<String>) -> Self {
        Self { content: vec![json!({"type": "text", "text": t.into()})], is_error: false }
    }
    pub fn json(v: &Value) -> Self {
        Self::text(serde_json::to_string_pretty(v).unwrap_or_default())
    }
    pub fn error(t: impl Into<String>) -> Self {
        Self { content: vec![json!({"type": "text", "text": t.into()})], is_error: true }
    }
    pub fn image(png_base64: String, info: &Value) -> Self {
        Self {
            content: vec![json!({"type": "image", "data": png_base64, "mimeType": "image/png"}), json!({"type": "text", "text": info.to_string()})],
            is_error: false,
        }
    }
    pub fn to_value(&self) -> Value {
        json!({"content": self.content, "isError": self.is_error})
    }
}

fn num(desc: &str) -> Value {
    json!({"type": "number", "description": desc})
}
fn int(desc: &str) -> Value {
    json!({"type": "integer", "minimum": 0, "description": desc})
}
fn string(desc: &str) -> Value {
    json!({"type": "string", "description": desc})
}
fn boolean(desc: &str) -> Value {
    json!({"type": "boolean", "description": desc})
}
fn mods_schema() -> Value {
    json!({"type": "object", "additionalProperties": false, "description": "Modifier keys (cmd = Command on macOS / Ctrl elsewhere; alt = Option)", "properties": {"shift": {"type": "boolean"}, "alt": {"type": "boolean"}, "cmd": {"type": "boolean"}}})
}
fn obj(props: Value, required: &[&str]) -> Value {
    let mut o = json!({"type": "object", "properties": props, "additionalProperties": false});
    if !required.is_empty() {
        o["required"] = json!(required);
    }
    o
}
fn tool(name: &str, title: &str, desc: &str, schema: Value, read_only: bool) -> Value {
    let (destructive, idempotent) = hints(name, read_only);
    let mut schema = schema;
    // WHIQ: every tool takes the caller's name (comments and document properties carry it), and
    // the arguments every craft app's tools take (CONTRACT-PARITY.md).
    if let Some(props) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        props.insert("caller".into(), json!({"type": "string", "description": "Who is making this call (author of comments and of the saved file); default: the server's --author"}));
        if !matches!(name, "server_status" | "job_status" | "list_jobs" | "cancel") {
            props
                .entry("async")
                .or_insert(json!({"type": "boolean", "description": "Run as a job: answer at once with a job_id and poll job_status"}));
            if !props.contains_key("offset") {
                props
                    .entry("continue_from")
                    .or_insert(json!({"type": "integer", "minimum": 0, "description": "Where to resume a trimmed result (its continue_from)"}));
            }
            props.entry("max_bytes").or_insert(
                json!({"type": "integer", "minimum": 0, "description": "Largest result to return before trimming (default --max-result-bytes)"}),
            );
        }
    }
    json!({"name": name, "title": title, "description": desc, "inputSchema": schema, "annotations": {
        "title": title,
        "readOnlyHint": read_only,
        "destructiveHint": destructive,
        "idempotentHint": idempotent,
        "openWorldHint": false
    }})
}

/// WHIQ contract §8: `(destructiveHint, idempotentHint)` per tool. Destructive: may delete or
/// overwrite (replacing text, deleting via keys or commands, saving with `overwrite`). Idempotent:
/// calling twice with the same arguments leaves the same state.
fn hints(name: &str, read_only: bool) -> (bool, bool) {
    if read_only {
        return (false, true);
    }
    match name {
        "run_command" | "batch" | "call_app" | "key" | "pointer" | "type_text" => (true, false),
        "save_presentation" | "export" | "build_deck" => (true, false),
        "set_text" | "adopt_template" => (true, true),
        "close_presentation" => (true, false),
        "go_to_slide" | "select" | "select_tool" | "cancel" | "evict" => (false, true),
        _ => (false, false),
    }
}

const APP_ONLY: &str = " Desktop app only (`deckcraft-cli mcp --connect PORT`).";

pub fn tool_definitions() -> Vec<Value> {
    vec![
        tool(
            "list_commands",
            "List commands",
            "Every command (id, label, ribbon/menu place, shortcut, parameter summary, whether it is enabled now). Filter by a substring of id or label.",
            obj(
                json!({"filter": string("Substring of the id or label, e.g. `format.` or `slide`"), "offset": int("Skip this many (continue_from of a truncated answer)"), "limit": int("At most this many")}),
                &[],
            ),
            true,
        ),
        tool(
            "run_command",
            "Run a command",
            "Run any command by id with JSON parameters. This is how everything is done: slide.new, shape.insert, text.set, format.bold, shape.fill, design.theme, transition.set, animation.add, table.merge, edit.undo…",
            obj(
                json!({"command": string("Command id from list_commands"), "params": {"type": "object", "description": "Parameters (see the command's params summary)"}}),
                &["command"],
            ),
            false,
        ),
        tool(
            "batch",
            "Run several commands",
            "Run commands in order; stops at the first error unless keepGoing. Returns each result.",
            obj(
                json!({"commands": {"type": "array", "items": obj(json!({"command": string("id"), "params": {"type": "object"}}), &["command"])}, "keepGoing": boolean("Continue after errors")}),
                &["commands"],
            ),
            false,
        ),
        tool(
            "new_presentation",
            "New presentation",
            "Start a new presentation (one title slide). Optional theme: DeckCraft, Harbor, Ember, Meadow, Nocturne, Paper, Coral Reef, Slate.",
            obj(json!({"theme": string("Theme name"), "blank": boolean("No first slide")}), &[]),
            false,
        ),
        tool(
            "open_presentation",
            "Open presentation",
            "Open a .deckcraft, .pptx or .potx file by path (relative paths are inside the write root). A very large file opens as a job: the answer is a job_id; poll job_status.",
            obj(json!({"path": string("File path"), "async": boolean("Open as a job even when the file is small")}), &["path"]),
            false,
        ),
        tool(
            "save_presentation",
            "Save presentation",
            "Save the active presentation (.deckcraft, .pptx or .potx template by extension). An existing file is never silently replaced: it is renamed OLD_<name> first (the answer says so) unless overwrite is true.",
            obj(
                json!({"path": string("File path (optional when it was opened from a file); relative paths are inside the write root"), "format": {"type": "string", "enum": ["pptx", "potx", "ppsx", "deckcraft"], "description": "Format (default: from the extension)"}, "overwrite": boolean("Replace an existing file instead of renaming it OLD_<name>")}),
                &[],
            ),
            false,
        ),
        tool(
            "export",
            "Export",
            "Export slides: png/jpeg (one slide, or all with all:true), pptx/potx, deckcraft, outline text, pdf (slides, notes pages or handouts), or an animated gif of the deck (one frame per slide). Exporting every slide of a large deck runs as a job (answer: job_id; poll job_status). Existing files are renamed OLD_<name> unless overwrite is true.",
            obj(
                json!({
                    "path": string("Output path"),
                    "format": {"type": "string", "enum": ["png", "jpeg", "pptx", "potx", "deckcraft", "outline", "pdf", "gif"], "description": "Output format (default: from the path's extension)"},
                    "overwrite": boolean("Replace existing files instead of renaming them OLD_<name>"),
                    "async": boolean("Run as a job even for a small deck"),
                    "slide": int("Slide index for images"),
                    "all": boolean("All slides (images)"),
                    "scale": num("Pixels per point for images (default 2)"),
                    "layout": {"type": "string", "enum": ["slides", "notes", "handouts"], "description": "PDF page layout (default slides)"},
                    "perPage": {"type": "integer", "enum": [1, 2, 3, 4, 6, 9], "description": "PDF handouts: slides per page (default 6)"},
                    "dpi": num("PDF: resolution of pictures and effects (and of the whole slide with raster), 36-600 (default 200)"),
                    "raster": boolean("PDF: draw each slide as one picture (with an invisible text layer) instead of vector artwork with real text (default false)"),
                    "slides": {"type": "array", "items": {"type": "integer", "minimum": 0}, "description": "PDF and GIF: slide indices to export, in order (default all)"},
                    "includeHidden": boolean("PDF and GIF: include hidden slides (default false)"),
                    "textLayer": boolean("PDF: invisible text layer for search, select and copy (default true)"),
                    "frame": boolean("PDF notes/handouts: thin frame around each slide (default true)"),
                    "paper": {"type": "string", "enum": ["letter", "a4", "legal", "a3"], "description": "PDF notes/handouts: paper size (default letter)"},
                    "orientation": {"type": "string", "enum": ["portrait", "landscape"], "description": "PDF notes/handouts: page orientation (default portrait)"},
                    "width": {
                        "oneOf": [
                            {"type": "integer", "minimum": 16, "maximum": 3840},
                            {"type": "string", "enum": ["small", "medium", "large", "extraLarge"]}
                        ],
                        "description": "GIF: frame width in pixels, or a size: small 640, medium 960 (default), large 1280, extraLarge 1920; the height follows the slide's shape"
                    },
                    "secondsPerSlide": {"type": "number", "minimum": 0.1, "maximum": 600, "description": "GIF: seconds each slide shows (default 5)"},
                    "loop": boolean("GIF: loop forever (true, default) or play once (false)")
                }),
                &["path"],
            ),
            false,
        ),
        tool(
            "inspect_document",
            "Inspect presentation",
            "Slides (index, id, title, layout, shape count, transition, animations, notes), sections, theme, layouts, media and the selection. Long slide lists are paged with offset/limit.",
            obj(json!({"offset": int("First slide to list (continue_from of a truncated answer)"), "limit": int("At most this many slides")}), &[]),
            true,
        ),
        tool(
            "inspect_slide",
            "Inspect slide",
            "One slide's shapes with ids, kinds, boxes [x,y,w,h], placeholder types, text, fills, tables, charts; plus background, transition, animations, notes.",
            obj(
                json!({"index": int("Slide index (default: current)"), "offset": int("First shape to list"), "limit": int("At most this many shapes")}),
                &[],
            ),
            true,
        ),
        tool(
            "render_slide",
            "Render slide",
            "Render a slide to a PNG image and return it, to check your work visually.",
            obj(
                json!({"slide": int("Slide index (default: current)"), "scale": num("Pixels per point (default 1 = 960×540 for 16:9)"), "edit": boolean("Show placeholder prompts like the editor"), "path": string("Write the PNG to this file instead of returning it")}),
                &[],
            ),
            true,
        ),
        tool(
            "add_slide",
            "Add slide",
            "Insert a slide after the current one with a layout (title, titleAndContent, sectionHeader, twoContent, comparison, titleOnly, blank, contentWithCaption, pictureWithCaption) and optional title/body text (body: one paragraph per line, leading tabs indent).",
            obj(json!({"layout": string("Layout name or kind"), "title": string("Title text"), "body": string("Body text")}), &[]),
            false,
        ),
        tool(
            "go_to_slide",
            "Go to slide",
            "Make a slide current (shape commands act on the current slide).",
            obj(json!({"index": int("Slide index")}), &["index"]),
            false,
        ),
        tool(
            "add_shape",
            "Add shape",
            "Insert a preset shape (rect, roundRect, ellipse, triangle, diamond, rightArrow, chevron, star5, heart, cloud, wedgeRectCallout, line, straightConnector1… see shape.presets) at x,y with size w×h, with optional text and fill colour (#RRGGBB or accent1..6).",
            obj(
                json!({"preset": string("Preset name"), "x": num("Left (pt)"), "y": num("Top (pt)"), "w": num("Width (pt)"), "h": num("Height (pt)"), "text": string("Text inside"), "fill": string("Fill colour"), "line": string("Outline colour or `none`")}),
                &["preset", "x", "y", "w", "h"],
            ),
            false,
        ),
        tool(
            "add_text_box",
            "Add text box",
            "Insert a text box. Height grows to fit the text.",
            obj(
                json!({"x": num("Left (pt)"), "y": num("Top (pt)"), "w": num("Width (pt)"), "h": num("Height (pt)"), "text": string("Text (one paragraph per line)"), "size": num("Font size (pt)"), "color": string("Text colour"), "align": string("left|center|right|justify")}),
                &["x", "y", "w", "text"],
            ),
            false,
        ),
        tool(
            "set_text",
            "Set text",
            "Replace a shape's text (keeps its first run's formatting). Leading tabs set bullet levels.",
            obj(json!({"id": int("Shape id (from inspect_slide)"), "text": string("New text")}), &["id", "text"]),
            false,
        ),
        tool(
            "insert_picture",
            "Insert picture",
            "Insert a picture from a file path or base64 data, optionally at a box.",
            obj(
                json!({"path": string("Image file path"), "data": string("Base64 image data (instead of path)"), "name": string("File name for data"), "x": num("Left"), "y": num("Top"), "w": num("Width"), "h": num("Height")}),
                &[],
            ),
            false,
        ),
        tool(
            "insert_table",
            "Insert table",
            "Insert a table with rows×cols and optional cell text (data: rows of strings).",
            obj(
                json!({"rows": int("Rows"), "cols": int("Columns"), "data": {"type": "array", "items": {"type": "array", "items": {"type": "string"}}}, "x": num("Left"), "y": num("Top"), "w": num("Width"), "h": num("Height")}),
                &["rows", "cols"],
            ),
            false,
        ),
        tool(
            "insert_chart",
            "Insert chart",
            "Insert a chart (column, bar, line, pie, doughnut, area, scatter, stackedColumn…) with categories and series.",
            obj(
                json!({"type": string("Chart type"), "title": string("Chart title"), "categories": {"type": "array", "items": {"type": "string"}}, "series": {"type": "array", "items": obj(json!({"name": string("Series name"), "values": {"type": "array", "items": {"type": "number"}}}), &["values"])}}),
                &[],
            ),
            false,
        ),
        tool(
            "select",
            "Select shapes",
            "Select shapes by id (formatting commands then act on them).",
            obj(json!({"ids": {"type": "array", "items": {"type": "integer"}}}), &["ids"]),
            false,
        ),
        tool(
            "pointer",
            "Pointer gesture",
            "Drive the active tool with pointer events in slide points: down/drag/up/move/doubleclick/contextclick, exactly like the mouse (draws, moves, resizes, rotates, places the caret; contextclick selects like a right click).",
            obj(
                json!({"events": {"type": "array", "items": obj(json!({"kind": string("down|drag|up|move|doubleclick|tripleclick|contextclick"), "x": num("x (pt)"), "y": num("y (pt)"), "mods": mods_schema()}), &["kind", "x", "y"])}, "mods": mods_schema()}),
                &["events"],
            ),
            false,
        ),
        tool(
            "select_tool",
            "Select tool",
            "Pick the pointer tool: select, textBox, shape (with preset), pen, highlighter, eraser — or a preset name.",
            obj(json!({"tool": string("Tool name"), "preset": string("Shape preset for the shape tool")}), &["tool"]),
            false,
        ),
        tool(
            "key",
            "Press key",
            "Press a key as on the keyboard: text editing keys (arrows, Backspace, Enter, Tab), nudges, and shortcuts (cmd+B…).",
            obj(json!({"key": string("Key name or character"), "mods": mods_schema()}), &["key"]),
            false,
        ),
        tool(
            "type_text",
            "Type text",
            "Type text into the text being edited (or into the selected shape).",
            obj(json!({"text": string("Text")}), &["text"]),
            false,
        ),
        tool("screenshot", "Screenshot", &format!("Screenshot of the whole DeckCraft window (UI included).{APP_ONLY}"), obj(json!({}), &[]), true),
        tool(
            "build_deck",
            "Build deck",
            "Build a whole deck from a JSON spec in one call, in the platform's nine layouts: bullets, two-column (left/right or bullets split), table (auto-paged over slides, header repeated), callout, kpi (2-4 {label, value} cards or `Label: Value` bullets), section, quote, chart (chart: {kind: bar|line|pie|…, categories, series:[{name, values}]} or a numeric table), image (image: {path, position: full|left|right|logo, caption}). Every slide takes `notes` (speaker notes) and inline **bold** / *italic* / `code`. An image slide takes `overlays` drawn over the picture in percent of its box: [{kind: rect|ellipse|line|text|path, x, y, w, h, color, label, points: \"5,50 12,48\", dashed, arrow: end|both, filled}] (a line's w/h are its end point). theme: corporate|slate|warm|mono or {bg, text, heading, muted, accent, accent2, tableHeadBg, tableHeadText, bandBg, bandText, fontHead, fontBody}; template: a company .pptx/.potx whose theme, masters and layouts the deck uses. Saves to `path` when given (.pptx, .potx template, .deckcraft); an existing file is renamed OLD_<name> unless overwrite.",
            obj(
                json!({
                    "slides": {"type": "array", "description": "Slide specs in order: {layout, title, notes, bullets, paragraphs, left, right, table, kpis, quote, attribution, text, chart, image, images, overlays}", "items": {"type": "object"}},
                    "title": string("Title slide heading (and the deck's title)"),
                    "subtitle": string("Title slide subtitle"),
                    "titleSlide": boolean("Make the title slide (default true when title is given)"),
                    "theme": {"description": "Preset name or palette object", "oneOf": [{"type": "string"}, {"type": "object"}]},
                    "template": string("Company .pptx/.potx to take theme, masters and layouts from"),
                    "logo": string("Logo image for every slide (bottom right)"),
                    "slideNumbers": boolean("Slide numbers (default true)"),
                    "notes": string("Speaker notes of the title slide"),
                    "path": string("Save the deck here (.pptx, .potx, .deckcraft)"),
                    "overwrite": boolean("Replace an existing file instead of renaming it OLD_<name>")
                }),
                &["slides"],
            ),
            false,
        ),
        tool(
            "add_overlays",
            "Add picture overlays",
            "Draw the overlay vocabulary over a picture on the current slide: rect, ellipse, line, text and path (freeform) marks, solid or dashed, lines and paths with an arrow at the end or both ends, paths closed and filled; x, y, w, h and points in percent of the picture box (relativeTo: image to measure against the picture itself when it is letterboxed). Native, editable shapes.",
            obj(
                json!({
                    "id": int("Picture shape id (default: the selected picture, else the first picture on the slide)"),
                    "overlays": {"type": "array", "items": {"type": "object"}, "description": "[{kind, x, y, w, h, color, label, points, dashed, arrow, filled}]"},
                    "rows": {"type": "array", "items": {"type": "array"}, "description": "The platform's overlay table rows: [shape, x, y, w, h, colour, label, points?, style?] (a `shape` header row places the columns)"},
                    "relativeTo": {"type": "string", "enum": ["box", "image"], "description": "Percentages of the picture box (default) or of the visible picture"}
                }),
                &[],
            ),
            false,
        ),
        tool(
            "adopt_template",
            "Adopt company template",
            "Restyle the open deck with a company .pptx/.potx: its theme (colours, fonts), slide masters and layouts (logo and decorations included); each slide moves to the template layout of the same kind. Save as .potx to write a template.",
            obj(json!({"path": string("The .pptx/.potx template")}), &["path"]),
            false,
        ),
        tool(
            "close_presentation",
            "Close presentation",
            "Close the active presentation. Unsaved work is never discarded: it is auto-saved to a recovered file first and the answer names it.",
            obj(json!({}), &[]),
            false,
        ),
        tool(
            "server_status",
            "Server status",
            "Version, memory, open presentations, running jobs, file roots, recovered files and limits. Always answers at once, even while other work runs.",
            obj(json!({}), &[]),
            true,
        ),
        tool(
            "job_status",
            "Job status",
            "Progress and result of a job (a large open, an export of many slides, any call past --job-after-ms or with async: true): status (running, done, failed, cancelled), progress 0..1, stage, steps and the stages passed, then its result or error. Results stay available after the client timed out. Without job_id: every job, like list_jobs.",
            obj(json!({"job_id": string("The job_id a call returned")}), &[]),
            true,
        ),
        tool(
            "cancel",
            "Cancel",
            "Stop the call that is running now (or the job `job_id`) at its next safe point: between slides of a build, pages of an export, slides of a file being read, commands of a batch. The presentation is left as it was before the stopped command. queued: true also drops the calls waiting behind it (each is answered `cancelled`). Answers at once.",
            obj(
                json!({"job_id": string("A job to cancel instead of the running call"), "queued": boolean("Also drop the calls waiting in the queue")}),
                &[],
            ),
            false,
        ),
        tool(
            "list_jobs",
            "List jobs",
            "Every job of this server (newest 200): job_id, tool, status, progress, stage, elapsed time, result.",
            obj(json!({}), &[]),
            true,
        ),
        tool(
            "roundtrip_check",
            "Round-trip check",
            "Open and save a .pptx/.potx/.ppsx in memory (without touching the open presentations or the file) and report whether anything would be dropped: parts, relationships, content types, binary parts, and the elements of every XML part. Use it on a customer's deck before editing it.",
            obj(json!({"path": string("The .pptx, .potx or .ppsx file")}), &["path"]),
            true,
        ),
        tool(
            "evict",
            "Evict documents",
            "Free memory now: auto-save every open presentation with unsaved work to a recovered file, then close them all (for supervisors). The next call names the files.",
            obj(json!({}), &[]),
            false,
        ),
        tool(
            "call_app",
            "Call app method",
            &format!("Call any control-channel method (ui.click, ui.set, ui.dialog.*, ui.inspect…).{APP_ONLY}"),
            obj(json!({"method": string("Method"), "params": {"type": "object"}}), &["method"]),
            false,
        ),
    ]
}

fn exec(b: &mut dyn Backend, cmd: &str, params: Value) -> Result<Value, String> {
    b.call("engine.execute", json!({"command": cmd, "params": params}))
}

fn f(a: &Value, k: &str) -> Option<f64> {
    a.get(k).and_then(Value::as_f64)
}

fn wrap(r: Result<Value, String>) -> ToolResult {
    match r {
        Ok(v) => ToolResult::json(&v),
        Err(e) => ToolResult::error(e),
    }
}

pub fn call_tool(b: &mut dyn Backend, name: &str, a: &Value) -> ToolResult {
    let a = if a.is_object() { a.clone() } else { json!({}) };
    match name {
        "list_commands" => wrap(b.call("engine.commands", json!({})).map(|v| {
            let f = a.get("filter").and_then(Value::as_str).unwrap_or("").to_lowercase();
            Value::Array(
                v.as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| {
                        f.is_empty()
                            || c.get("id").and_then(Value::as_str).is_some_and(|i| i.to_lowercase().contains(&f))
                            || c.get("label").and_then(Value::as_str).is_some_and(|i| i.to_lowercase().contains(&f))
                    })
                    .collect(),
            )
        })),
        "run_command" => {
            let Some(c) = a.get("command").and_then(Value::as_str) else { return ToolResult::error("missing `command`") };
            wrap(exec(b, c, a.get("params").cloned().unwrap_or(json!({}))))
        }
        "batch" => {
            let keep = a.get("keepGoing").and_then(Value::as_bool).unwrap_or(false);
            let mut out = vec![];
            let commands = a.get("commands").and_then(Value::as_array).cloned().unwrap_or_default();
            let n = commands.len();
            deckcraft_engine::task::phase("running commands", 0.0, 1.0);
            // WHIQ parity pass: a cancelled batch is undone as a whole (the presentation as it was
            // before the call), as in every craft app.
            let before = b.session().and_then(|s| s.active_index().zip(s.active().cloned()));
            for (k, c) in commands.into_iter().enumerate() {
                let id = c.get("command").and_then(Value::as_str).unwrap_or("");
                // WHIQ: a safe point between commands, with the progress of the batch.
                if let Err(e) = deckcraft_engine::task::step(k, n) {
                    let mut rolled = false;
                    if let (Some((i, st)), Some(s)) = (before, b.session())
                        && s.active_index() == Some(i)
                        && let Some(a) = s.active_mut()
                        && a.uid == st.uid
                    {
                        *a = st;
                        rolled = true;
                    }
                    let note = if rolled { "; the batch was undone (the presentation is as it was before it)" } else { "" };
                    out.push(json!({"command": id, "ok": false, "error": format!("{e}{note}")}));
                    return ToolResult { content: vec![json!({"type": "text", "text": Value::Array(out).to_string()})], is_error: true };
                }
                match exec(b, id, c.get("params").cloned().unwrap_or(json!({}))) {
                    Ok(v) => out.push(json!({"command": id, "ok": true, "result": v})),
                    Err(e) => {
                        out.push(json!({"command": id, "ok": false, "error": e}));
                        if !keep {
                            return ToolResult { content: vec![json!({"type": "text", "text": Value::Array(out).to_string()})], is_error: true };
                        }
                    }
                }
            }
            ToolResult::json(&Value::Array(out))
        }
        "new_presentation" => wrap(exec(b, "file.new", a)),
        "open_presentation" => wrap(exec(b, "file.open", a)),
        "save_presentation" => wrap(exec(b, "file.save", a)),
        "export" => wrap(exec(b, "file.export", a)),
        "inspect_document" => wrap(b.call("document.inspect", json!({}))),
        "inspect_slide" => wrap(exec(b, "slide.inspect", a)),
        "render_slide" => match b.call("ui.render", a.clone()) {
            Ok(v) => match v.get("png").and_then(Value::as_str) {
                Some(png) => {
                    ToolResult::image(png.to_string(), &json!({"width": v.get("width"), "height": v.get("height"), "slide": v.get("slide")}))
                }
                None => ToolResult::json(&v),
            },
            Err(e) => ToolResult::error(e),
        },
        "add_slide" => wrap(exec(b, "slide.new", a)),
        "go_to_slide" => wrap(exec(b, "slide.go", a)),
        "add_shape" => {
            let r = exec(
                b,
                "shape.insert",
                json!({"preset": a.get("preset"), "rect": [f(&a, "x"), f(&a, "y"), f(&a, "w"), f(&a, "h")], "text": a.get("text")}),
            );
            let Ok(v) = r else { return wrap(r) };
            let id = v.get("id").cloned().unwrap_or_default();
            if let Some(c) = a.get("fill").filter(|c| c.is_string())
                && let Err(e) = exec(b, "shape.fill", json!({"id": id, "color": c}))
            {
                return ToolResult::error(e);
            }
            if let Some(c) = a.get("line").and_then(Value::as_str) {
                let p = if c == "none" { json!({"id": id, "none": true}) } else { json!({"id": id, "color": c}) };
                if let Err(e) = exec(b, "shape.line", p) {
                    return ToolResult::error(e);
                }
            }
            ToolResult::json(&v)
        }
        "add_text_box" => {
            let r =
                exec(b, "insert.textBox", json!({"rect": [f(&a, "x"), f(&a, "y"), f(&a, "w"), f(&a, "h").unwrap_or(30.0)], "text": a.get("text")}));
            let Ok(v) = r else { return wrap(r) };
            let id = v.get("id").cloned().unwrap_or_default();
            let _ = exec(b, "text.exit", json!({}));
            let _ = exec(b, "edit.select", json!({"ids": [id]}));
            if let Some(s) = f(&a, "size") {
                let _ = exec(b, "format.size", json!({"size": s}));
            }
            if let Some(c) = a.get("color").filter(|c| c.is_string()) {
                let _ = exec(b, "format.color", json!({"color": c}));
            }
            if let Some(al) = a.get("align").and_then(Value::as_str) {
                let _ = exec(b, "format.align", json!({"align": al}));
            }
            ToolResult::json(&v)
        }
        "set_text" => wrap(exec(b, "text.set", a)),
        "insert_picture" => {
            let mut p = a.clone();
            if let (Some(x), Some(y), Some(w), Some(h)) = (f(&a, "x"), f(&a, "y"), f(&a, "w"), f(&a, "h")) {
                p["rect"] = json!([x, y, w, h]);
            }
            wrap(exec(b, "insert.picture", p))
        }
        "insert_table" => {
            let mut p = a.clone();
            if let (Some(x), Some(y), Some(w), Some(h)) = (f(&a, "x"), f(&a, "y"), f(&a, "w"), f(&a, "h")) {
                p["rect"] = json!([x, y, w, h]);
            }
            wrap(exec(b, "insert.table", p))
        }
        "insert_chart" => wrap(exec(b, "insert.chart", a)),
        "build_deck" => wrap(exec(b, "deck.build", a)),
        "add_overlays" => wrap(exec(b, "picture.overlay", a)),
        "adopt_template" => wrap(exec(b, "design.adoptTemplate", a)),
        "select" => wrap(exec(b, "edit.select", a)),
        "pointer" => wrap(b.call("ui.pointer", a)),
        "select_tool" => wrap(b.call("ui.tool.select", a)),
        "key" => {
            let mut p = a.get("mods").cloned().filter(Value::is_object).unwrap_or(json!({}));
            p["key"] = a.get("key").cloned().unwrap_or_default();
            wrap(b.call("ui.key", p))
        }
        "type_text" => wrap(b.call("ui.text", a)),
        "screenshot" => {
            if !b.has_ui() {
                return ToolResult::error(format!("screenshot: {NEEDS_APP}. Use render_slide to see a slide headlessly."));
            }
            match b.call("ui.screenshot", json!({"base64": true})) {
                Ok(v) => match v.get("png").and_then(Value::as_str) {
                    Some(png) => ToolResult::image(png.to_string(), &json!({"width": v.get("width"), "height": v.get("height")})),
                    None => ToolResult::json(&v),
                },
                Err(e) => ToolResult::error(e),
            }
        }
        "call_app" => {
            let Some(m) = a.get("method").and_then(Value::as_str) else { return ToolResult::error("missing `method`") };
            wrap(b.call(m, a.get("params").cloned().unwrap_or(json!({}))))
        }
        other => ToolResult::error(format!("unknown tool `{other}`")),
    }
}
