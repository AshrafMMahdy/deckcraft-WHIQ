//! WHIQ: what the platform shows and reads of a presentation, drawn and read by the engine.
//!
//! - [`preview`]: the preview page: one self-contained HTML page (no external fetch), one card per
//!   slide in order, each slide drawn as vector SVG by the renderer itself (background, master and
//!   layout graphics, text with its formatting, shapes, connectors, freeforms, tables, charts,
//!   pictures as data URIs, slide numbers), its speaker notes under it. The same kind of page the
//!   platform's own pptx preview made (`shared/render/pptx.ts`), now from DeckCraft's model.
//! - [`markdown`]: the deck as markdown for an agent to read: per slide its title, text (bullet
//!   levels), tables, chart data, pictures' descriptions and notes.
//!
//! Both work on a window of slides (`start`, `count`) so any size of deck can be shown or read in
//! parts; the preview page also stops at a byte budget and says where to go on.

use deckcraft_model::text::{Bullet, Paragraph, TextBody};
use deckcraft_model::{PhType, Presentation, Shape, ShapeKind, Slide};
use deckcraft_render::svg::esc;
use deckcraft_render::{PictureLimits, SvgDoc, SvgStats};
use serde_json::{Value, json};

use crate::{EngineError, Result};

/// A window of slides: 0-based `start`, at most `count` slides (`None`: to the end).
#[derive(Clone, Copy, Debug, Default)]
pub struct Window {
    pub start: usize,
    pub count: Option<usize>,
}

impl Window {
    /// The slide indices `start..end` of a deck of `total` slides; `Err` when `start` is past the
    /// last slide of a deck that has slides.
    pub fn range(&self, total: usize) -> Result<(usize, usize)> {
        if total == 0 {
            return Ok((0, 0));
        }
        if self.start >= total {
            return Err(EngineError::Other(format!(
                "no slide {} (slide_start is 0-based: this deck has {total} slide{}, 0 through {})",
                self.start,
                if total == 1 { "" } else { "s" },
                total - 1
            )));
        }
        let end = match self.count {
            Some(c) => self.start.saturating_add(c.max(1)).min(total),
            None => total,
        };
        Ok((self.start, end))
    }
}

/// How the preview page is made.
#[derive(Clone, Debug)]
pub struct PreviewOptions {
    pub window: Window,
    /// The name in the page's title and heading (the file's name).
    pub name: String,
    /// Speaker notes under each slide (default true).
    pub notes: bool,
    /// Largest page, as the bytes of its JSON string (what an MCP answer carries); slides past it
    /// are left for the next window. `None`: no limit (the page goes to a file).
    pub max_bytes: Option<usize>,
    pub pictures: PictureLimits,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        PreviewOptions { window: Window::default(), name: String::new(), notes: true, max_bytes: None, pictures: PictureLimits::default() }
    }
}

/// A preview page and what it holds.
#[derive(Clone, Debug)]
pub struct PreviewPage {
    pub html: String,
    /// First slide on the page (0-based) and how many follow it.
    pub start: usize,
    pub rendered: usize,
    pub total: usize,
    /// Per slide on the page: index, number, title, hidden, notes.
    pub slides: Vec<Value>,
    pub stats: SvgStats,
    pub warnings: Vec<String>,
}

impl PreviewPage {
    /// The first slide after the page, when the deck goes on.
    pub fn next(&self) -> Option<usize> {
        let n = self.start + self.rendered;
        (n < self.total).then_some(n)
    }
}

const CSS: &str = ":root{color-scheme:light dark}\
body{margin:0;padding:16px;font-family:system-ui,'Segoe UI',Arial,sans-serif;background:#f4f3f0;color:#1c1c1c}\
.head{font:12px Consolas,monospace;color:#666;margin:0 auto 12px;max-width:960px}\
.slide{background:#fff;border:1px solid #d5d2cb;border-radius:8px;padding:10px 12px 12px;margin:0 auto 16px;max-width:960px;box-shadow:0 1px 3px rgba(0,0,0,.06)}\
.bar{display:flex;gap:10px;align-items:baseline;margin:0 0 8px;font:12px Consolas,monospace;color:#999}\
.bar .title{font:13px system-ui,'Segoe UI',Arial,sans-serif;color:#555;flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}\
.tag{font:11px Consolas,monospace;color:#7a5c00;background:#fff8e6;border:1px solid #e8d9a8;border-radius:4px;padding:0 5px}\
svg.art{display:block;width:100%;height:auto;border:1px solid #e3e1dc;border-radius:4px;background:#fff}\
.slide.hidden svg.art{opacity:.55}\
.missing{padding:24px;text-align:center;font:12px Consolas,monospace;color:#7a5c00;background:#fff8e6;border:1px solid #e8d9a8;border-radius:6px}\
details.notes{margin-top:10px;border-top:1px dashed #d5d2cb;padding-top:8px}\
details.notes summary{font-size:12px;color:#777;cursor:pointer}\
details.notes .n{font-size:13px;color:#444;margin-top:6px;white-space:pre-wrap}\
.cap{margin:0 auto 12px;max-width:960px;font:12px Consolas,monospace;color:#a66f00}\
@media (prefers-color-scheme:dark){body{background:#1a1a1a;color:#e8e8e8}.slide{background:#242424;border-color:#3a3a3a;box-shadow:none}\
svg.art{border-color:#3a3a3a}.bar .title{color:#bbb}details.notes .n{color:#bbb}.tag{background:#3a3220;border-color:#5a4a20;color:#e8c860}}";

/// Bytes `s` takes as a JSON string (without its quotes).
fn json_len(s: &str) -> usize {
    serde_json::to_string(s).map(|j| j.len().saturating_sub(2)).unwrap_or(s.len())
}

/// Canvas units per point the slides are recorded at: 2 (a 16:9 slide is 1920 × 1080), smaller
/// for a very large slide so the canvas stays within 4000 units.
fn record_scale(doc: &Presentation) -> f64 {
    let longest = doc.slide_size.width.max(doc.slide_size.height).max(1.0);
    (4000.0 / longest).clamp(0.05, 2.0)
}

/// The notes worth showing: the notes text, unless it is only the slide-number placeholder.
fn notes_of(slide: &Slide) -> String {
    let t = slide.notes_text().replace('\u{b}', "\n");
    let t = t.trim();
    if t.chars().all(|c| c.is_ascii_digit()) { String::new() } else { t.to_string() }
}

fn title_of(slide: &Slide) -> String {
    let t = slide.title();
    let t = t.trim();
    if !t.is_empty() {
        return t.to_string();
    }
    // A deck without title placeholders: the slide's name, else nothing.
    slide.name.trim().to_string()
}

/// One slide's card.
fn card(i: usize, total: usize, slide: &Slide, art: &str, notes: bool) -> String {
    let title = title_of(slide);
    let mut c = format!("<section class=\"slide{}\" id=\"slide-{}\" data-index=\"{i}\">", if slide.hidden { " hidden" } else { "" }, i + 1);
    c.push_str(&format!("<div class=\"bar\"><span class=\"num\">{}/{total}</span>", i + 1));
    c.push_str(&format!("<span class=\"title\" dir=\"auto\">{}</span>", esc(&title)));
    if slide.hidden {
        c.push_str("<span class=\"tag\">hidden in the show</span>");
    }
    c.push_str("</div>");
    c.push_str(art);
    let n = if notes { notes_of(slide) } else { String::new() };
    if !n.is_empty() {
        c.push_str(&format!("<details class=\"notes\"><summary>Speaker notes</summary><div class=\"n\" dir=\"auto\">{}</div></details>", esc(&n)));
    }
    c.push_str("</section>");
    c
}

/// The page around the cards.
fn page(name: &str, total: usize, start: usize, rendered: usize, defs: &str, cards: &str) -> String {
    let mut head = format!("{} — {total} slide{}", esc(name), if total == 1 { "" } else { "s" });
    let partial = rendered < total;
    if partial && rendered > 0 {
        head.push_str(&format!(" · slides {}–{}", start + 1, start + rendered));
    }
    let cap = if total == 0 {
        "<div class=\"cap\">This presentation has no slides.</div>".to_string()
    } else if partial {
        format!("<div class=\"cap\">Showing slides {}–{} of {total}.</div>", start + 1, start + rendered)
    } else {
        String::new()
    };
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>{}</title><style>{CSS}</style></head><body><div class=\"head\">{head}</div>{cap}{defs}{cards}</body></html>",
        esc(name)
    )
}

/// The preview page of a window of `doc`'s slides.
pub fn preview(doc: &Presentation, o: &PreviewOptions) -> Result<PreviewPage> {
    let total = doc.slides.len();
    let (start, end) = o.window.range(total)?;
    let scale = record_scale(doc);
    let mut svg = SvgDoc::new(&format!("dc{start}-"), o.pictures.clone());
    let mut cards = String::new();
    let mut slides = vec![];
    let mut warnings = vec![];
    // The page without cards, and the shared definitions so far, as JSON string bytes.
    let shell = json_len(&page(&o.name, total, start, end.saturating_sub(start), "", ""));
    let mut used = shell + 512;
    let mut rendered = 0usize;
    crate::task::phase("drawing slides", 0.0, 1.0);
    for i in start..end {
        crate::task::step(i - start, end - start)?;
        let Some(slide) = doc.slides.get(i) else { break };
        let mut attempt = 0;
        let (card_html, cost) = loop {
            let defs_before = svg.defs_len();
            svg.begin();
            let art = match deckcraft_render::record_slide(doc, i, scale) {
                Some(rec) => {
                    use deckcraft_render::Canvas as _;
                    svg.slide(&rec.ops, rec.width(), rec.height(), &format!("s{i}"), &title_of(slide))
                }
                None => {
                    warnings.push(format!("slide {i} could not be drawn; its card says so"));
                    "<div class=\"missing\">This slide could not be drawn.</div>".to_string()
                }
            };
            let c = card(i, total, slide, &art, o.notes);
            // The card, what it added to the shared definitions, and its line in the answer's list.
            let cost = json_len(&c) + json_len(svg.defs_since(defs_before)) + 160;
            match o.max_bytes {
                Some(max) if used + cost > max => {
                    svg.rollback();
                    if rendered > 0 {
                        break (None, 0);
                    }
                    // The first slide alone is over the budget: its pictures smaller, then off.
                    attempt += 1;
                    match attempt {
                        1..=3 => {
                            svg.limits.max_original = 0;
                            svg.limits.reduce = svg.limits.reduce.max(1) * 2;
                            continue;
                        }
                        _ => {
                            return Err(EngineError::Other(format!(
                                "slide {i} alone is too large for one answer as a preview ({} bytes, max_bytes {max}): pass `out` (an .html path) to write the page to a file, which has no size limit, or a larger max_bytes",
                                used + cost
                            )));
                        }
                    }
                }
                _ => break (Some(c), cost),
            }
        };
        let Some(card_html) = card_html else { break };
        if attempt > 0 {
            warnings.push(format!("slide {i}: its pictures were made smaller (1/{} of their sides) to fit max_bytes", svg.limits.reduce));
        }
        used += cost;
        cards.push_str(&card_html);
        let notes = notes_of(slide);
        slides.push(json!({
            "index": i,
            "number": i + 1,
            "title": title_of(slide),
            "hidden": slide.hidden,
            "notes": !notes.is_empty(),
        }));
        rendered += 1;
    }
    let html = page(&o.name, total, start, rendered, &svg.defs(), &cards);
    Ok(PreviewPage { html, start, rendered, total, slides, stats: svg.stats.clone(), warnings })
}

/// The slide size as answers give it.
pub fn slide_size_json(doc: &Presentation) -> Value {
    let (w, h) = (doc.slide_size.width, doc.slide_size.height);
    let r = |v: f64| (v * 100.0).round() / 100.0;
    json!({
        "width_pt": r(w),
        "height_pt": r(h),
        "width_in": r(w / 72.0),
        "height_in": r(h / 72.0),
        "width_emu": (w * 12700.0).round() as i64,
        "height_emu": (h * 12700.0).round() as i64,
    })
}

// ------------------------------------------------------------------ markdown

/// The deck as markdown, a window of slides at a time.
#[derive(Clone, Debug)]
pub struct Markdown {
    pub text: String,
    pub start: usize,
    pub count: usize,
    pub total: usize,
    pub warnings: Vec<String>,
}

impl Markdown {
    pub fn next(&self) -> Option<usize> {
        let n = self.start + self.count;
        (n < self.total).then_some(n)
    }
}

/// Markdown of the slides in `w`; `name` heads it (with the slide count and size). With
/// `max_bytes` (as JSON string bytes) the window stops at the slide that would pass it (the first
/// slide is cut there instead, and says so).
pub fn markdown(doc: &Presentation, w: Window, name: &str, max_bytes: Option<usize>) -> Result<Markdown> {
    let total = doc.slides.len();
    let (start, end) = w.range(total)?;
    let mut out = String::new();
    let title = if name.trim().is_empty() { doc.props.title.trim().to_string() } else { name.trim().to_string() };
    out.push_str(&format!(
        "# {}\n\n_{total} slide{}, {:.2} × {:.2} in{}_\n\n",
        if title.is_empty() { "Presentation" } else { &title },
        if total == 1 { "" } else { "s" },
        doc.slide_size.width / 72.0,
        doc.slide_size.height / 72.0,
        if end - start < total && total > 0 { format!("; slides {}–{} below", start + 1, end) } else { String::new() }
    ));
    let mut used = json_len(&out) + 1024;
    let mut count = 0usize;
    let mut warnings = vec![];
    for i in start..end {
        crate::task::step(i - start, end - start)?;
        let Some(slide) = doc.slides.get(i) else { break };
        let mut one = String::new();
        slide_markdown(doc, i, slide, &mut one);
        let cost = json_len(&one);
        if let Some(max) = max_bytes
            && used + cost > max
        {
            if count > 0 {
                break;
            }
            // One slide over the budget alone: cut at a character boundary, said in the text too.
            let room = max.saturating_sub(used).saturating_sub(256);
            let mut cut = String::new();
            for ch in one.chars() {
                if cut.len() + ch.len_utf8() * 2 > room {
                    break;
                }
                cut.push(ch);
            }
            cut.push_str(&format!("\n\n_[slide {} cut here: it is longer than max_bytes; pass `out` to write it whole]_\n\n", i + 1));
            warnings.push(format!("slide {i} was cut at max_bytes; pass `out` (a .md path) to write it whole"));
            one = cut;
        }
        used += json_len(&one);
        out.push_str(&one);
        count += 1;
    }
    Ok(Markdown { text: out, start, count, total, warnings })
}

fn slide_markdown(doc: &Presentation, i: usize, slide: &Slide, out: &mut String) {
    let title = title_of(slide);
    out.push_str(&format!(
        "## Slide {}{}{}\n\n",
        i + 1,
        if title.is_empty() { String::new() } else { format!(": {}", one_line(&title)) },
        if slide.hidden { " (hidden)" } else { "" }
    ));
    let title_id = slide.shapes.iter().find(|s| s.ph_type().is_some_and(PhType::is_title)).map(|s| s.id);
    let ctx = deckcraft_model::resolve::Ctx::for_slide(doc, slide);
    let mut body = String::new();
    for sh in &slide.shapes {
        if Some(sh.id) == title_id {
            continue;
        }
        shape_markdown(ctx.as_ref(), sh, &mut body, 0);
    }
    if !body.trim().is_empty() {
        out.push_str(body.trim_end());
        out.push_str("\n\n");
    }
    let notes = notes_of(slide);
    if !notes.is_empty() {
        out.push_str("**Notes:**\n\n");
        for l in notes.lines() {
            out.push_str("> ");
            out.push_str(l);
            out.push('\n');
        }
        out.push('\n');
    }
}

fn one_line(s: &str) -> String {
    s.replace(['\u{b}', '\n', '\r'], " ").trim().to_string()
}

fn cell(s: &str) -> String {
    one_line(s).replace('|', "\\|")
}

fn shape_markdown(ctx: Option<&deckcraft_model::resolve::Ctx>, sh: &Shape, out: &mut String, depth: usize) {
    if depth > 32 || sh.ph_type().is_some_and(|k| matches!(k, PhType::Date | PhType::Footer | PhType::SlideNum | PhType::Header)) {
        return;
    }
    match &sh.kind {
        ShapeKind::Group { children, .. } => {
            for c in children {
                shape_markdown(ctx, c, out, depth + 1);
            }
            return;
        }
        ShapeKind::Table(t) => {
            let rows: Vec<Vec<String>> = t
                .rows
                .iter()
                .map(|r| r.cells.iter().map(|c| if c.h_merge || c.v_merge { String::new() } else { cell(&c.text.text()) }).collect())
                .collect();
            let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
            if cols > 0 {
                for (k, r) in rows.iter().enumerate() {
                    let mut line = String::from("|");
                    for c in 0..cols {
                        line.push(' ');
                        line.push_str(r.get(c).map(String::as_str).unwrap_or(""));
                        line.push_str(" |");
                    }
                    out.push_str(&line);
                    out.push('\n');
                    if k == 0 {
                        out.push('|');
                        out.push_str(&" --- |".repeat(cols));
                        out.push('\n');
                    }
                }
                out.push('\n');
            }
            return;
        }
        ShapeKind::Chart(c) => {
            let kind = serde_json::to_value(c.kind).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_else(|| "chart".into());
            let title = c.title.as_deref().map(one_line).filter(|t| !t.is_empty());
            out.push_str(&format!("**Chart{}** ({kind})\n\n", title.map(|t| format!(": {t}")).unwrap_or_default()));
            let n = c.series.iter().map(|s| s.values.len().max(s.x_values.len())).max().unwrap_or(0).max(c.categories.len());
            if !c.series.is_empty() && n > 0 {
                let scatter = c.series.iter().any(|s| !s.x_values.is_empty()) && c.categories.is_empty();
                out.push_str(if scatter { "| x |" } else { "| Category |" });
                for s in &c.series {
                    out.push_str(&format!(" {} |", if s.name.trim().is_empty() { "Series".to_string() } else { cell(&s.name) }));
                }
                out.push('\n');
                out.push('|');
                out.push_str(&" --- |".repeat(c.series.len() + 1));
                out.push('\n');
                let num = |v: Option<f64>| v.filter(|v| v.is_finite()).map(|v| format!("{v}")).unwrap_or_default();
                for k in 0..n.min(10_000) {
                    let label = if scatter {
                        num(c.series.first().and_then(|s| s.x_values.get(k).copied().flatten()))
                    } else {
                        c.categories.get(k).map(|c| cell(c)).unwrap_or_else(|| format!("{}", k + 1))
                    };
                    out.push_str(&format!("| {label} |"));
                    for s in &c.series {
                        out.push_str(&format!(" {} |", num(s.values.get(k).copied().flatten())));
                    }
                    out.push('\n');
                }
                out.push('\n');
            }
            return;
        }
        ShapeKind::Picture { .. } => {
            let d = one_line(if sh.descr.trim().is_empty() { &sh.title } else { &sh.descr });
            out.push_str(&if d.is_empty() { "*[Picture]*\n\n".to_string() } else { format!("*[Picture: {d}]*\n\n") });
        }
        ShapeKind::Media(m) => {
            out.push_str(if m.video { "*[Video]*\n\n" } else { "*[Audio]*\n\n" });
        }
        ShapeKind::Opaque { label, .. } if !label.trim().is_empty() => {
            out.push_str(&format!("*[{}]*\n\n", one_line(label)));
        }
        _ => {}
    }
    if let Some(d) = &sh.diagram {
        for node in &d.nodes {
            let t = one_line(&node.text);
            if !t.is_empty() {
                out.push_str(&format!("{}- {t}\n", "  ".repeat(usize::from(node.level.min(8)))));
            }
        }
        out.push('\n');
        return;
    }
    if let Some(t) = &sh.text {
        text_markdown(ctx, sh, t, out);
    }
}

fn text_markdown(ctx: Option<&deckcraft_model::resolve::Ctx>, sh: &Shape, t: &TextBody, out: &mut String) {
    let list_default = sh.ph_type().is_some_and(|k| matches!(k, PhType::Body | PhType::Obj)) && !sh.text_box;
    let mut wrote = false;
    let mut number = 0u32;
    for p in &t.paragraphs {
        if p.is_empty() || p.text().trim().is_empty() {
            continue;
        }
        let text = para_markdown(p);
        let indent = "  ".repeat(usize::from(p.level.min(8)));
        // The bullet as the slide shows it: the paragraph's own, else what its placeholder and
        // master give it.
        let bullet = match ctx {
            Some(c) => deckcraft_model::resolve::para(c, sh, p).bullet,
            None => p.props.bullet.clone(),
        };
        match &bullet {
            Some(Bullet::AutoNum { start_at, .. }) => {
                number = if number == 0 { *start_at } else { number + 1 };
                out.push_str(&format!("{indent}{number}. {text}\n"));
            }
            Some(Bullet::Char { .. }) | Some(Bullet::Picture { .. }) => out.push_str(&format!("{indent}- {text}\n")),
            None if list_default => out.push_str(&format!("{indent}- {text}\n")),
            _ => {
                number = 0;
                out.push_str(&format!("{text}\n\n"));
            }
        }
        wrote = true;
    }
    if wrote && !out.ends_with("\n\n") {
        out.push('\n');
    }
}

/// A paragraph's runs, bold and italic marked.
fn para_markdown(p: &Paragraph) -> String {
    let mut s = String::new();
    for r in &p.runs {
        let c = r.content().replace('\u{b}', "  \n");
        if c.trim().is_empty() {
            s.push_str(&c);
            continue;
        }
        let (lead, core, trail) = split_space(&c);
        let b = r.props.bold == Some(true);
        let it = r.props.italic == Some(true);
        let mark = match (b, it) {
            (true, true) => "***",
            (true, false) => "**",
            (false, true) => "*",
            _ => "",
        };
        s.push_str(lead);
        s.push_str(mark);
        s.push_str(core);
        s.push_str(mark);
        s.push_str(trail);
    }
    s.trim().to_string()
}

fn split_space(s: &str) -> (&str, &str, &str) {
    let core = s.trim();
    let lead = s.len() - s.trim_start().len();
    let trail_at = lead + core.len();
    (s.get(..lead).unwrap_or(""), core, s.get(trail_at..).unwrap_or(""))
}

// ------------------------------------------------------------------ files read by path

/// WHIQ: the last presentation read by path for a preview or markdown window, kept so the next
/// window of the same unchanged file does not read it again (one at a time; released with the
/// other caches under memory pressure).
#[cfg(not(target_arch = "wasm32"))]
mod cache {
    use std::sync::{Arc, Mutex};

    use deckcraft_model::Presentation;

    type Key = (String, u64, Option<std::time::SystemTime>);
    static LAST: Mutex<Option<(Key, Arc<Presentation>)>> = Mutex::new(None);

    /// The file's path, size and time, read through the checked gateway (`io::open_file`).
    fn key(path: &str) -> Option<Key> {
        let m = crate::io::open_file(path).ok()?.metadata().ok()?;
        Some((path.to_string(), m.len(), m.modified().ok()))
    }

    pub fn read(path: &str) -> crate::Result<(Arc<Presentation>, Vec<String>)> {
        crate::file_access::check_read(path).map_err(crate::EngineError::Other)?;
        let k = key(path);
        if let Some(k) = &k
            && let Some((have, doc)) = LAST.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
            && have == k
        {
            return Ok((doc.clone(), vec![]));
        }
        let loaded = crate::cmd::file::read_presentation_file(path)?;
        let doc = Arc::new(loaded.doc);
        if let Some(k) = k {
            *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some((k, doc.clone()));
        }
        Ok((doc, loaded.damaged))
    }

    pub fn release() -> bool {
        LAST.lock().unwrap_or_else(|e| e.into_inner()).take().is_some()
    }
}

/// Read the presentation at `path` for a preview or markdown window (it is not opened as a
/// document); the parts its open could not keep come back too.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_for_window(path: &str) -> Result<(std::sync::Arc<Presentation>, Vec<String>)> {
    cache::read(path)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "preview_tests.rs"]
mod tests;

#[cfg(target_arch = "wasm32")]
pub fn read_for_window(path: &str) -> Result<(std::sync::Arc<Presentation>, Vec<String>)> {
    Err(EngineError::Other(format!("{path}: files are not read by path in the browser")))
}

/// Forget the presentation kept for windows read by path. Returns whether one was kept.
pub fn release_cache() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        cache::release()
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}
