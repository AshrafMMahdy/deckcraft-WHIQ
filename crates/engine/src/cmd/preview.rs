//! WHIQ: the preview page and the reading markdown (MCP `render_preview`, `export_markdown`); the
//! work is in [`crate::preview`].

use std::sync::Arc;

use deckcraft_model::Presentation;
use serde_json::{Value, json};

use super::*;
use crate::preview::{self, PreviewOptions, Window};
use crate::{EngineError, Result, Session};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "file.renderPreview",
            "Preview Page",
            [],
            None,
            "{path?: a .pptx/.potx/.ppsx/.deckcraft to read (not opened; else the active presentation), slideStart?: 0-based (0), slideCount?: slides (all, or as many as maxBytes holds), out?: .html file to write the page to, notes?: bool (true), maxBytes?: largest page as JSON string bytes when it is returned} → {html | out, bytes, slide_count, slide_size, slide_start, slides_rendered, next_slide_start, complete, slides, pictures, warnings}: one self-contained HTML page, each slide drawn as vector SVG",
            always,
            render_preview
        ),
        cmd!(
            query "file.exportMarkdown",
            "Export Markdown",
            [],
            None,
            "{path?: a presentation file to read (not opened; else the active presentation), slideStart?: 0-based (0), slideCount?: slides (all, or as many as maxBytes holds), out?: .md file to write, maxBytes?: largest text as JSON string bytes when it is returned} → {markdown | out, bytes, slide_count, slide_size, slide_start, slides_exported, next_slide_start, complete}: per slide its title, text, tables, chart data, pictures and notes",
            always,
            export_markdown
        ),
    ]
}

/// A parameter by either spelling (`slide_start`, `slideStart`).
fn param<'a>(p: &'a Value, snake: &str, camel: &str) -> Option<&'a Value> {
    p.get(snake).or_else(|| p.get(camel)).filter(|v| !v.is_null())
}

fn count_param(p: &Value, snake: &str, camel: &str) -> Result<Option<usize>> {
    match param(p, snake, camel) {
        None => Ok(None),
        Some(v) => v
            .as_u64()
            .or_else(|| v.as_f64().filter(|f| f.is_finite() && *f >= 0.0).map(|f| f as u64))
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            .map(|n| Some(n as usize))
            .ok_or_else(|| bad("file.renderPreview", format!("`{snake}` must be a whole number of slides"))),
    }
}

/// The presentation a window is taken from: the file at `path`, else the active one; and its name.
fn source(s: &Session, p: &Value) -> Result<(Arc<Presentation>, String, Vec<String>, Option<String>)> {
    if let Some(path) = str_param(p, "path").filter(|x| !x.trim().is_empty()) {
        let (doc, damaged) = preview::read_for_window(path)?;
        let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
        let abs = std::path::absolute(path).map(|a| a.to_string_lossy().to_string()).unwrap_or_else(|_| path.to_string());
        let warnings = damaged.into_iter().map(|d| format!("not shown (the file's open could not keep it): {d}")).collect();
        return Ok((doc, name, warnings, Some(abs)));
    }
    let st = s.doc()?;
    let name =
        st.path.as_deref().and_then(|p| std::path::Path::new(p).file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| st.title());
    Ok((st.doc.clone(), name, vec![], None))
}

fn window(p: &Value) -> Result<Window> {
    Ok(Window { start: count_param(p, "slide_start", "slideStart")?.unwrap_or(0), count: count_param(p, "slide_count", "slideCount")? })
}

fn write_out(path: &str, bytes: &[u8]) -> Result<String> {
    crate::task::phase("writing the file", 0.95, 1.0);
    crate::io::write_file(path, bytes).map_err(EngineError::Other)?;
    Ok(std::path::absolute(path).map(|a| a.to_string_lossy().to_string()).unwrap_or_else(|_| path.to_string()))
}

fn render_preview(s: &mut Session, p: &Value) -> Result<Value> {
    let (doc, name, mut warnings, src) = source(s, p)?;
    let out = str_param(p, "out").filter(|x| !x.trim().is_empty()).map(String::from);
    let max_bytes = if out.is_some() { None } else { count_param(p, "max_bytes", "maxBytes")?.map(|m| m.max(16 << 10)) };
    let o = PreviewOptions { window: window(p)?, name, notes: bool_or(p, "notes", true), max_bytes, ..Default::default() };
    let page = preview::preview(&doc, &o)?;
    warnings.extend(page.warnings.iter().cloned());
    let next = page.next();
    let mut v = json!({
        "format": "html",
        "bytes": page.html.len(),
        "slide_count": page.total,
        "slide_size": preview::slide_size_json(&doc),
        "slide_start": page.start,
        "slides_rendered": page.rendered,
        "next_slide_start": next,
        "complete": next.is_none(),
        "slides": page.slides,
        "pictures": {
            "embedded": page.stats.pictures,
            "original": page.stats.originals,
            "reencoded": page.stats.reencoded,
            "bytes": page.stats.picture_bytes,
        },
    });
    if let Some(src) = src {
        v["source"] = json!(src);
    }
    match out {
        Some(path) => v["out"] = json!(write_out(&path, page.html.as_bytes())?),
        None => v["html"] = json!(page.html),
    }
    if let Some(n) = next {
        v["next_step"] = json!(format!(
            "slides {}–{} of {} are on this page; call again with slide_start: {n} for the next ones",
            page.start,
            page.start + page.rendered.saturating_sub(1),
            page.total
        ));
    }
    if !warnings.is_empty() {
        v["warnings"] = json!(warnings);
    }
    Ok(v)
}

fn export_markdown(s: &mut Session, p: &Value) -> Result<Value> {
    let (doc, name, mut warnings, src) = source(s, p)?;
    let out = str_param(p, "out").filter(|x| !x.trim().is_empty()).map(String::from);
    let max_bytes = if out.is_some() { None } else { count_param(p, "max_bytes", "maxBytes")?.map(|m| m.max(16 << 10)) };
    let md = preview::markdown(&doc, window(p)?, &name, max_bytes)?;
    warnings.extend(md.warnings.iter().cloned());
    let next = md.next();
    let mut v = json!({
        "format": "markdown",
        "bytes": md.text.len(),
        "slide_count": md.total,
        "slide_size": preview::slide_size_json(&doc),
        "slide_start": md.start,
        "slides_exported": md.count,
        "next_slide_start": next,
        "complete": next.is_none(),
    });
    if let Some(src) = src {
        v["source"] = json!(src);
    }
    match out {
        Some(path) => v["out"] = json!(write_out(&path, md.text.as_bytes())?),
        None => v["markdown"] = json!(md.text),
    }
    if let Some(n) = next {
        v["next_step"] = json!(format!("call again with slide_start: {n} for the next slides"));
    }
    if !warnings.is_empty() {
        v["warnings"] = json!(warnings);
    }
    Ok(v)
}
