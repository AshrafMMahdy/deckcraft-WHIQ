//! WHIQ: decks from a JSON spec, in the nine slide layouts of the White Hat IQ platform's own
//! writer (bullets, two-column, table with auto-paging, callout, kpi, section, quote, chart,
//! image at full/left/right/logo), each with speaker notes; the platform's overlay vocabulary
//! on pictures (rectangles, ellipses, lines, freeform paths, dashed and arrowed strokes, text
//! labels, all in percentages of the picture box); and theme adoption from a company template
//! with its masters and layouts.
//!
//! Geometry follows the platform's grid: a 13.333 × 7.5 in (960 × 540 pt) slide, scaled to the
//! deck's slide size.

use std::sync::Arc;

use deckcraft_color::{ColorTransform, Rgba};
use deckcraft_geom::Xfrm;
use deckcraft_model::chart::{ChartType, Series};
use deckcraft_model::style::{Dash, LineEnd, PictureFill};
use deckcraft_model::table::{Cell, Row};
use deckcraft_model::text::{Align, Anchor, AutoFit, Bullet, ParaProps, Paragraph, Run, RunKind, RunProps, TextBody};
use deckcraft_model::{
    Background, Chart, ColorRef, CustomPath, Fill, Geom, LayoutId, LayoutType, Line, PhType, Presentation, Shape, ShapeId, ShapeKind, Slide, Table,
    defaults,
};
use serde_json::{Value, json};

use super::*;
use crate::{DocState, EngineError, Result, Session};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            noundo "deck.build",
            "Build Deck",
            [],
            None,
            "{slides: [{layout: bullets|two-column|table|callout|kpi|section|quote|chart|image, title, notes, bullets, paragraphs, left, right, table, kpis, quote, attribution, text, chart, image, images, overlays}], title?, subtitle?, titleSlide?: bool, theme?: corporate|slate|warm|mono|{colours, fontHead, fontBody}, template?: .pptx/.potx path, logo?: image path, slideNumbers?: bool, path?: save to (.pptx/.potx/.deckcraft)} → {document, slides, warnings, path?}",
            always,
            build
        ),
        cmd!(
            "picture.overlay",
            "Picture Overlays",
            [],
            None,
            "{id?: picture (else the selected one), overlays: [{kind: rect|ellipse|line|text|path, x, y, w, h (percent of the picture box), color, label, points, dashed, arrow: end|both, filled}] | rows: [[shape, x, y, w, h, colour, label, points?, style?]], relativeTo?: box|image} → {ids}",
            has_slide,
            picture_overlay
        ),
        cmd!(
            "design.adoptTemplate",
            "Use Company Template",
            ["Design", "Themes"],
            None,
            "{path} — take the theme (colours, fonts), masters and layouts of a .pptx/.potx; each slide moves to the template layout of the same kind",
            has_doc,
            adopt_template
        ),
    ]
}

// ── theme ────────────────────────────────────────────────────────────────────────────────────

/// The platform's deck palette: hex colours and two font families.
#[derive(Clone, Debug, PartialEq)]
pub struct DeckTheme {
    pub name: String,
    pub bg: Rgba,
    pub text: Rgba,
    pub heading: Rgba,
    pub muted: Rgba,
    pub accent: Rgba,
    pub accent2: Rgba,
    pub table_head_bg: Rgba,
    pub table_head_text: Rgba,
    pub band_bg: Rgba,
    pub band_text: Rgba,
    pub font_head: String,
    pub font_body: String,
}

fn hex(s: &str) -> Rgba {
    Rgba::from_hex(s).unwrap_or(Rgba::BLACK)
}

/// The four presets, with the platform's colours. Fonts are open families (AGENTS.md §1); pass
/// `fontHead` / `fontBody` to name others.
pub fn preset_theme(name: &str) -> Option<DeckTheme> {
    let t = |name: &str, c: [&str; 10], head: &str, body: &str| DeckTheme {
        name: name.into(),
        bg: hex(c[0]),
        text: hex(c[1]),
        heading: hex(c[2]),
        muted: hex(c[3]),
        accent: hex(c[4]),
        accent2: hex(c[5]),
        table_head_bg: hex(c[6]),
        table_head_text: hex(c[7]),
        band_bg: hex(c[8]),
        band_text: hex(c[9]),
        font_head: head.into(),
        font_body: body.into(),
    };
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "" | "corporate" | "default" => {
            t("corporate", ["FFFFFF", "1C1C1C", "0F2E4C", "6B7280", "1F6FB2", "C8842A", "0F2E4C", "FFFFFF", "0F2E4C", "FFFFFF"], "Inter", "Inter")
        }
        "slate" | "dark" => {
            t("slate", ["1B1F24", "E6E6E6", "FFFFFF", "9AA4AF", "4DA3FF", "F0A22E", "2B323A", "FFFFFF", "11151A", "FFFFFF"], "Inter", "Inter")
        }
        "warm" => t(
            "warm",
            ["FCFAF6", "241F1A", "5A3A1E", "857A6B", "B4541F", "3F6B52", "5A3A1E", "FFFFFF", "5A3A1E", "FFFFFF"],
            "Merriweather",
            "Merriweather",
        ),
        "mono" => t(
            "mono",
            ["FFFFFF", "111111", "000000", "777777", "000000", "555555", "EDEDED", "111111", "111111", "FFFFFF"],
            "Liberation Sans",
            "Liberation Sans",
        ),
        _ => return None,
    })
}

fn is_dark(c: Rgba) -> bool {
    (0.2126 * f64::from(c.r) + 0.7152 * f64::from(c.g) + 0.0722 * f64::from(c.b)) / 255.0 < 0.5
}

/// `theme` as a preset name or an object of colours (`bg`, `text`, `heading`, `muted`, `accent`,
/// `accent2`, `tableHeadBg`, `tableHeadText`, `bandBg`, `bandText`) and `fontHead` / `fontBody`,
/// on top of `base` (a preset, or a template's palette).
fn theme_from(v: Option<&Value>, base: DeckTheme, warnings: &mut Vec<String>) -> DeckTheme {
    match v {
        None | Some(Value::Null) => base,
        Some(Value::String(n)) => preset_theme(n).unwrap_or_else(|| {
            warnings.push(format!("theme `{n}` is not a preset (corporate, slate, warm, mono); used {}", base.name));
            base
        }),
        Some(Value::Object(o)) => {
            let mut t = o.get("name").and_then(Value::as_str).and_then(preset_theme).unwrap_or(base);
            let mut set = |keys: &[&str], slot: &mut Rgba| {
                for k in keys {
                    if let Some(c) = o.get(*k).and_then(Value::as_str) {
                        match Rgba::from_hex(c.trim_start_matches('#')) {
                            Some(rgb) => *slot = rgb,
                            None => warnings.push(format!("theme.{k}: `{c}` is not a #RRGGBB colour")),
                        }
                        return;
                    }
                }
            };
            set(&["bg", "background"], &mut t.bg);
            set(&["text"], &mut t.text);
            set(&["heading"], &mut t.heading);
            set(&["muted"], &mut t.muted);
            set(&["accent"], &mut t.accent);
            set(&["accent2"], &mut t.accent2);
            set(&["tableHeadBg", "table_head_bg"], &mut t.table_head_bg);
            set(&["tableHeadText", "table_head_text"], &mut t.table_head_text);
            set(&["bandBg", "band_bg"], &mut t.band_bg);
            set(&["bandText", "band_text"], &mut t.band_text);
            for (keys, slot) in [(["fontHead", "font_head"], &mut t.font_head), (["fontBody", "font_body"], &mut t.font_body)] {
                if let Some(f) = keys.iter().find_map(|k| o.get(*k).and_then(Value::as_str)).filter(|f| !f.trim().is_empty()) {
                    *slot = f.trim().to_string();
                }
            }
            t
        }
        Some(other) => {
            warnings.push(format!("theme must be a preset name or an object, not {other}"));
            base
        }
    }
}

/// A company template's palette, as the platform lifts it: lt1 background, dk1 text, dk2 headings,
/// accent1/accent2, readable contrast forced, major/minor fonts.
pub fn theme_of_template(p: &Presentation) -> DeckTheme {
    use deckcraft_color::SchemeSlot as S;
    let mut t = preset_theme("corporate").unwrap_or_else(|| DeckTheme {
        name: String::new(),
        bg: Rgba::WHITE,
        text: Rgba::BLACK,
        heading: Rgba::BLACK,
        muted: Rgba::rgb(0x6B, 0x72, 0x80),
        accent: Rgba::BLACK,
        accent2: Rgba::BLACK,
        table_head_bg: Rgba::BLACK,
        table_head_text: Rgba::WHITE,
        band_bg: Rgba::BLACK,
        band_text: Rgba::WHITE,
        font_head: String::new(),
        font_body: String::new(),
    });
    let Some(m) = p.masters.first() else { return t };
    let c = &m.theme.colors;
    t.name = format!("template({})", m.theme.name);
    t.bg = c.get(S::Lt1);
    t.text = c.get(S::Dk1);
    t.heading = c.get(S::Dk2);
    t.accent = c.get(S::Accent1);
    t.accent2 = c.get(S::Accent2);
    t.table_head_bg = t.accent;
    t.table_head_text = if is_dark(t.accent) { Rgba::WHITE } else { Rgba::rgb(0x11, 0x11, 0x11) };
    t.band_bg = t.heading;
    t.band_text = if is_dark(t.band_bg) { Rgba::WHITE } else { Rgba::rgb(0x11, 0x11, 0x11) };
    if is_dark(t.bg) == is_dark(t.text) {
        t.text = if is_dark(t.bg) { Rgba::rgb(0xE6, 0xE6, 0xE6) } else { Rgba::rgb(0x1C, 0x1C, 0x1C) };
    }
    if is_dark(t.bg) == is_dark(t.heading) {
        t.heading = if is_dark(t.bg) { Rgba::WHITE } else { Rgba::rgb(0x0F, 0x2E, 0x4C) };
    }
    let major = &m.theme.fonts.major.latin;
    let minor = &m.theme.fonts.minor.latin;
    if !major.is_empty() && !major.starts_with('+') {
        t.font_head = major.clone();
    }
    if !minor.is_empty() && !minor.starts_with('+') {
        t.font_body = minor.clone();
    }
    t
}

/// Put the palette into the master's theme, so theme colours and fonts follow it.
fn apply_theme_to_master(p: &mut Presentation, t: &DeckTheme) {
    use deckcraft_color::SchemeSlot as S;
    for m in &mut p.masters {
        let m = Arc::make_mut(m);
        let c = &mut m.theme.colors;
        c.set(S::Lt1, t.bg);
        c.set(S::Dk1, t.text);
        c.set(S::Dk2, t.heading);
        c.set(S::Accent1, t.accent);
        c.set(S::Accent2, t.accent2);
        c.set(S::Accent3, t.muted);
        c.name = format!("WHIQ {}", t.name);
        m.theme.fonts.major.latin = t.font_head.clone();
        m.theme.fonts.minor.latin = t.font_body.clone();
        m.theme.fonts.name = format!("WHIQ {}", t.name);
        m.theme.name = format!("WHIQ {}", t.name);
        m.background = Some(Background::Fill { fill: Fill::solid(ColorRef::rgb(t.bg)) });
    }
}

// ── inline markdown ──────────────────────────────────────────────────────────────────────────

/// `**bold**`, `__bold__`, `*italic*`, `_italic_` and `` `code` `` → (text, bold, italic, code)
/// pieces, so a deck never shows literal markers.
pub fn md_runs(text: &str) -> Vec<(String, bool, bool, bool)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<(String, bool, bool, bool)> = vec![];
    let mut plain = String::new();
    let mut i = 0usize;
    let find = |from: usize, pat: &[char]| -> Option<usize> {
        let n = pat.len();
        (from..chars.len().saturating_sub(n.saturating_sub(1))).find(|&k| chars.get(k..k + n) == Some(pat))
    };
    while i < chars.len() {
        let c = chars.get(i).copied().unwrap_or(' ');
        let two = chars.get(i + 1).copied() == Some(c);
        let (marker, bold, italic, code): (Vec<char>, bool, bool, bool) = match c {
            '*' | '_' if two => (vec![c, c], true, false, false),
            '*' | '_' => (vec![c], false, true, false),
            '`' => (vec!['`'], false, false, true),
            _ => {
                plain.push(c);
                i += 1;
                continue;
            }
        };
        let start = i + marker.len();
        // `_` inside a word (snake_case) is not emphasis.
        let word_inside = c == '_' && i > 0 && chars.get(i - 1).is_some_and(|p| p.is_alphanumeric());
        match find(start, &marker).filter(|&end| end > start && !word_inside) {
            Some(end) => {
                if !plain.is_empty() {
                    out.push((std::mem::take(&mut plain), false, false, false));
                }
                out.push((chars.get(start..end).map(|s| s.iter().collect()).unwrap_or_default(), bold, italic, code));
                i = end + marker.len();
            }
            None => {
                plain.push(c);
                i += 1;
            }
        }
    }
    if !plain.is_empty() || out.is_empty() {
        out.push((plain, false, false, false));
    }
    out
}

/// Plain text with the markers removed.
pub fn md_plain(text: &str) -> String {
    md_runs(text).into_iter().map(|r| r.0).collect()
}

// ── geometry and shapes ─────────────────────────────────────────────────────────────────────

/// The platform's grid, in inches.
const GW: f64 = 13.333;
const GH: f64 = 7.5;

#[derive(Clone, Copy, Debug)]
struct TextStyle {
    size: f64,
    bold: bool,
    italic: bool,
    color: Rgba,
    align: Option<Align>,
    anchor: Anchor,
    shrink: bool,
    head: bool,
}

impl TextStyle {
    fn body(color: Rgba, size: f64) -> Self {
        TextStyle { size, bold: false, italic: false, color, align: None, anchor: Anchor::Top, shrink: false, head: false }
    }
}

/// One paragraph to lay out: text (inline markdown), outline level, bullet.
#[derive(Clone, Debug)]
struct Para {
    text: String,
    level: u8,
    bullet: bool,
}

struct Builder {
    doc: Presentation,
    theme: DeckTheme,
    sx: f64,
    sy: f64,
    template: bool,
    warnings: Vec<String>,
    logo: Option<deckcraft_model::MediaId>,
    slide_numbers: bool,
}

impl Builder {
    /// Inches on the platform grid → a box on this deck's slide.
    fn r(&self, x: f64, y: f64, w: f64, h: f64) -> Xfrm {
        Xfrm::new(x * 72.0 * self.sx, y * 72.0 * self.sy, (w * 72.0 * self.sx).max(0.0), (h * 72.0 * self.sy).max(0.0))
    }
    fn id(&mut self) -> Result<ShapeId> {
        self.doc.alloc_id().map(ShapeId).ok_or_else(|| bad("deck.build", NO_ID_SPACE))
    }
    fn font(&self, head: bool) -> String {
        if head { self.theme.font_head.clone() } else { self.theme.font_body.clone() }
    }
    fn run_props(&self, st: &TextStyle, bold: bool, italic: bool, code: bool) -> RunProps {
        RunProps {
            font: Some(if code { "Liberation Mono".to_string() } else { self.font(st.head) }),
            size: Some(st.size),
            bold: Some(st.bold || bold),
            italic: Some(st.italic || italic),
            fill: Some(Fill::solid(ColorRef::rgb(st.color))),
            ..Default::default()
        }
    }
    fn paragraph(&self, p: &Para, st: &TextStyle) -> Paragraph {
        let runs = md_runs(&p.text).into_iter().map(|(t, b, i, c)| Run::with(t, self.run_props(st, b, i, c))).collect();
        let mut props = ParaProps { align: st.align, ..Default::default() };
        if p.bullet {
            let indent = 18.0 * self.sx;
            props.bullet = Some(Bullet::Char { char: "•".into() });
            props.margin_left = Some(indent * (f64::from(p.level) + 1.0));
            props.indent = Some(-indent);
            props.space_before = Some(deckcraft_model::text::Spacing::Pts(4.0));
        } else {
            props.bullet = Some(Bullet::None);
        }
        // Base direction from the first strong character (WHIQ contract §10).
        if deckcraft_model::bidi::first_strong_is_rtl(&md_plain(&p.text)) {
            props.rtl = Some(true);
            if props.align.is_none() || props.align == Some(Align::Left) {
                props.align = Some(Align::Right);
            }
        }
        Paragraph { level: p.level.min(8), props, runs, end_props: self.run_props(st, false, false, false), ..Default::default() }
    }
    fn body(&self, paras: &[Para], st: &TextStyle) -> TextBody {
        let mut b = TextBody { paragraphs: paras.iter().map(|p| self.paragraph(p, st)).collect(), ..Default::default() };
        if b.paragraphs.is_empty() {
            b.paragraphs.push(Paragraph::default());
        }
        b.body.wrap = Some(true);
        b.body.anchor = Some(st.anchor);
        b.body.autofit = Some(if st.shrink { AutoFit::Shrink { font_scale: None, line_reduction: 0.0 } } else { AutoFit::None });
        let inset = 7.2 * self.sx;
        b.body.inset_l = Some(inset);
        b.body.inset_r = Some(inset);
        b.body.inset_t = Some(3.6 * self.sy);
        b.body.inset_b = Some(3.6 * self.sy);
        b
    }
    fn text_box(&mut self, rect: Xfrm, paras: &[Para], st: &TextStyle, name: &str) -> Result<Shape> {
        Ok(Shape {
            id: self.id()?,
            name: name.into(),
            xfrm: Some(rect),
            text_box: true,
            fill: Some(Fill::None),
            line: Some(Line::none()),
            geom: Geom::preset("rect"),
            text: Some(self.body(paras, st)),
            ..Default::default()
        })
    }
    fn rect_shape(&mut self, rect: Xfrm, fill: Option<Fill>, line: Option<Line>, name: &str) -> Result<Shape> {
        Ok(Shape {
            id: self.id()?,
            name: name.into(),
            xfrm: Some(rect),
            geom: Geom::preset("rect"),
            fill: Some(fill.unwrap_or(Fill::None)),
            line: Some(line.unwrap_or_else(Line::none)),
            ..Default::default()
        })
    }
    fn solid(c: Rgba) -> Fill {
        Fill::solid(ColorRef::rgb(c))
    }

    /// A new slide on the layout of `kind`: its title placeholder kept (and, from a template, its
    /// body placeholders removed so our content takes their place).
    fn slide(&mut self, kind: LayoutType) -> Result<Slide> {
        let layout = self.layout_for(kind);
        let mut s = defaults::new_slide(&mut self.doc, layout).ok_or_else(|| bad("deck.build", NO_ID_SPACE))?;
        s.shapes.retain(|sh| matches!(sh.ph_type(), Some(PhType::Title | PhType::CtrTitle | PhType::SubTitle)));
        Ok(s)
    }
    fn layout_for(&self, kind: LayoutType) -> LayoutId {
        let master = self.doc.masters.first();
        let find = |k: LayoutType| master.and_then(|m| m.layouts.iter().find(|l| l.kind == k)).map(|l| l.id);
        let fallback = match kind {
            LayoutType::Title => [LayoutType::Title, LayoutType::SectionHeader, LayoutType::TitleOnly],
            LayoutType::SectionHeader => [LayoutType::SectionHeader, LayoutType::Title, LayoutType::TitleOnly],
            _ => [LayoutType::TitleOnly, LayoutType::TitleAndContent, LayoutType::Blank],
        };
        fallback.into_iter().find_map(find).or_else(|| master.and_then(|m| m.layouts.first()).map(|l| l.id)).unwrap_or_default()
    }

    /// Put `text` into the slide's title placeholder (`kinds`), styled as the platform does
    /// unless the deck comes from a template (then the template's title style shows).
    fn set_title(&mut self, s: &mut Slide, kinds: &[PhType], text: &str, rect: Xfrm, st: &TextStyle) -> Result<()> {
        let body = self.body(&[Para { text: text.to_string(), level: 0, bullet: false }], st);
        let template = self.template;
        if let Some(ph) = s.shapes.iter_mut().find(|sh| sh.ph_type().is_some_and(|k| kinds.contains(&k))) {
            if template {
                // Keep the layout's position and look; only the words are ours.
                let mut tb = TextBody::from_text(&md_plain(text));
                for p in &mut tb.paragraphs {
                    if deckcraft_model::bidi::first_strong_is_rtl(&p.text()) {
                        p.props.rtl = Some(true);
                    }
                }
                ph.text = Some(tb);
            } else {
                ph.xfrm = Some(rect);
                ph.text = Some(body);
            }
            return Ok(());
        }
        let shape = self.text_box(rect, &[Para { text: text.to_string(), level: 0, bullet: false }], st, "Title")?;
        s.shapes.push(shape);
        Ok(())
    }

    fn heading(&mut self, s: &mut Slide, text: &str) -> Result<()> {
        let st = TextStyle {
            size: 28.0,
            bold: true,
            italic: false,
            color: self.theme.heading,
            align: Some(Align::Left),
            anchor: Anchor::Middle,
            shrink: true,
            head: true,
        };
        let rect = self.r(0.6, 0.45, GW - 2.4, 0.9);
        self.set_title(s, &[PhType::Title, PhType::CtrTitle], text, rect, &st)?;
        if !self.template {
            let rule = self.rect_shape(self.r(0.6, 1.32, 1.6, 0.05), Some(Self::solid(self.theme.accent)), None, "Accent Rule")?;
            s.shapes.push(rule);
        }
        Ok(())
    }

    fn notes(s: &mut Slide, notes: Option<&str>) {
        if let Some(n) = notes.map(str::trim).filter(|n| !n.is_empty()) {
            s.notes = TextBody::from_text(n);
        }
    }

    /// The platform's master decoration (bottom accent rule, logo) and slide number, on each slide.
    fn decorate(&mut self, s: &mut Slide) -> Result<()> {
        if self.template {
            return self.number(s);
        }
        let rule = self.rect_shape(self.r(0.0, GH - 0.32, GW, 0.06), Some(Self::solid(self.theme.accent)), None, "Footer Rule")?;
        s.shapes.insert(0, rule);
        if let Some(media) = self.logo {
            let pic = self.picture(media, self.r(GW - 1.45, GH - 0.95, 1.05, 0.5), "Logo")?;
            s.shapes.insert(1, pic);
        }
        self.number(s)
    }

    fn number(&mut self, s: &mut Slide) -> Result<()> {
        if !self.slide_numbers {
            return Ok(());
        }
        let st = TextStyle::body(self.theme.muted, 10.0);
        let mut shape = self.text_box(self.r(0.5, GH - 0.5, 1.0, 0.3), &[], &st, "Slide Number")?;
        if let Some(p) = shape.text.as_mut().and_then(|t| t.paragraphs.first_mut()) {
            p.runs =
                vec![Run {
                    text: "‹#›".into(), props: self.run_props(&st, false, false, false), kind: RunKind::Field { field: "slidenum".into() }
                }];
        }
        s.shapes.push(shape);
        Ok(())
    }

    fn picture(&mut self, media: deckcraft_model::MediaId, rect: Xfrm, name: &str) -> Result<Shape> {
        // `contain`: the whole picture inside the box, centred; padding through negative crop.
        let (pw, ph) = self.doc.media(media).map(|m| image_px(m.data.as_ref())).unwrap_or((4.0, 3.0));
        let (ia, ba) = (pw / ph.max(1e-9), rect.w / rect.h.max(1e-9));
        let crop = if ia > ba {
            let pad = (ia / ba - 1.0) / 2.0;
            [0.0, -pad, 0.0, -pad]
        } else if ia < ba {
            let pad = (ba / ia - 1.0) / 2.0;
            [-pad, 0.0, -pad, 0.0]
        } else {
            [0.0; 4]
        };
        Ok(Shape {
            id: self.id()?,
            name: name.into(),
            xfrm: Some(rect),
            kind: ShapeKind::Picture { fill: PictureFill { media, crop, ..Default::default() } },
            descr: name.into(),
            ..Default::default()
        })
    }

    fn media_from_path(&mut self, path: &str) -> Result<deckcraft_model::MediaId> {
        let bytes = crate::io::read_file(path).map_err(EngineError::Other)?;
        let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "image.png".into());
        let ct = super::insert::content_type(&name, &bytes);
        if !ct.starts_with("image/") || (ct != "image/svg+xml" && deckcraft_render::decode_image(&bytes).is_none()) {
            return Err(bad("deck.build", format!("{path} is not a picture DeckCraft can read")));
        }
        self.doc.add_media(&name, ct, bytes).ok_or_else(|| bad("deck.build", NO_ID_SPACE))
    }
}

fn image_px(bytes: &[u8]) -> (f64, f64) {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .map(|(w, h)| (f64::from(w), f64::from(h)))
        .filter(|(w, h)| *w > 0.0 && *h > 0.0)
        .unwrap_or((4.0, 3.0))
}

// ── spec reading ────────────────────────────────────────────────────────────────────────────

/// A layout name read forgivingly; unknown names fall back to bullets (with a warning), as the
/// platform's writer does.
pub fn layout_name(raw: &str) -> Option<&'static str> {
    let k: String = raw.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect();
    Some(match k.as_str() {
        "" | "bullets" | "bullet" | "list" | "text" | "content" | "default" => "bullets",
        "twocolumn" | "twocolumns" | "2column" | "2columns" | "2col" | "columns" | "twocol" => "two-column",
        "table" | "tables" | "grid" => "table",
        "callout" | "callouts" | "highlight" | "note" => "callout",
        "kpi" | "kpis" | "metrics" | "stats" | "figures" => "kpi",
        "section" | "sectionheader" | "divider" => "section",
        "quote" | "pullquote" | "quotation" => "quote",
        "chart" | "graph" | "plot" => "chart",
        "image" | "picture" | "photo" | "img" | "figure" => "image",
        _ => return None,
    })
}

fn strings(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => s.lines().map(str::to_string).filter(|l| !l.trim().is_empty()).collect(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| match x {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => o.get("text").and_then(Value::as_str).map(String::from),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

/// Bullets: strings (leading `-`, `*` and two-space / tab indents set the level) or
/// `{text, level}` objects.
fn bullets(v: Option<&Value>) -> Vec<Para> {
    let item = |s: &str| {
        let indent = s.chars().take_while(|c| *c == ' ' || *c == '\t').map(|c| if c == '\t' { 2 } else { 1 }).sum::<usize>();
        let t = s.trim_start();
        // One list marker (`- `, `* `, `• `) goes; `**bold**` at the start stays.
        let t = ["- ", "* ", "• "].iter().find_map(|m| t.strip_prefix(m)).unwrap_or(t).trim_start();
        Para { text: t.to_string(), level: (indent / 2).min(4) as u8, bullet: true }
    };
    match v {
        Some(Value::String(s)) => s.lines().filter(|l| !l.trim().is_empty()).map(item).collect(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| match x {
                Value::String(s) => Some(item(s)),
                Value::Object(o) => o.get("text").and_then(Value::as_str).map(|t| Para {
                    text: t.to_string(),
                    level: o.get("level").and_then(Value::as_u64).unwrap_or(0).min(4) as u8,
                    bullet: true,
                }),
                Value::Number(n) => Some(Para { text: n.to_string(), level: 0, bullet: true }),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

fn paragraphs(v: Option<&Value>) -> Vec<Para> {
    strings(v).into_iter().map(|t| Para { text: t, level: 0, bullet: false }).collect()
}

/// Table rows from `[[..]]`, `{rows: [[..]]}` or `{header: [..], rows: [[..]]}`.
fn table_rows(v: Option<&Value>) -> Vec<Vec<String>> {
    let cell = |c: &Value| match c {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let rows_of = |a: &Vec<Value>| -> Vec<Vec<String>> { a.iter().filter_map(Value::as_array).map(|r| r.iter().map(cell).collect()).collect() };
    match v {
        Some(Value::Array(a)) => rows_of(a),
        Some(Value::Object(o)) => {
            let mut rows = vec![];
            if let Some(h) = o.get("header").or_else(|| o.get("headers")).or_else(|| o.get("columns")).and_then(Value::as_array) {
                rows.push(h.iter().map(cell).collect());
            }
            if let Some(a) = o.get("rows").and_then(Value::as_array) {
                rows.extend(rows_of(a));
            }
            rows
        }
        _ => vec![],
    }
}

fn number_of(s: &str) -> Option<f64> {
    let t: String = s.trim().chars().filter(|c| !matches!(c, ',' | '%' | '$' | '€' | '£' | ' ')).collect();
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Chart data from a table: column 0 the categories, every numeric column a series (at most 4).
fn chart_from_rows(rows: &[Vec<String>]) -> Option<(Vec<String>, Vec<(String, Vec<f64>)>)> {
    if rows.len() < 3 {
        return None;
    }
    let head = rows.first()?;
    let body = rows.get(1..)?;
    let labels: Vec<String> = body.iter().map(|r| r.first().map(|c| md_plain(c).trim().to_string()).unwrap_or_default()).collect();
    if labels.iter().any(String::is_empty) {
        return None;
    }
    let mut series = vec![];
    for c in 1..head.len() {
        let vals: Option<Vec<f64>> = body.iter().map(|r| r.get(c).and_then(|v| number_of(v))).collect();
        if let Some(v) = vals {
            series.push((head.get(c).map(|h| md_plain(h)).unwrap_or_else(|| format!("Series {c}")), v));
        }
    }
    (!series.is_empty()).then(|| (labels, series.into_iter().take(4).collect()))
}

/// KPI cards: `{label, value}` objects, `"Label: Value"` strings (bullets), at most four.
fn kpis(slide: &Value) -> Vec<(String, String)> {
    let mut out = vec![];
    if let Some(a) = slide.get("kpis").or_else(|| slide.get("metrics")).and_then(Value::as_array) {
        for k in a {
            match k {
                Value::Object(o) => {
                    let label = o.get("label").or_else(|| o.get("name")).and_then(Value::as_str).unwrap_or_default();
                    let value = o.get("value").map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).unwrap_or_default();
                    if !label.is_empty() || !value.is_empty() {
                        out.push((label.to_string(), value));
                    }
                }
                Value::String(s) => out.extend(split_kpi(s)),
                _ => {}
            }
        }
    }
    if out.is_empty() {
        for b in bullets(slide.get("bullets")) {
            out.extend(split_kpi(&b.text));
        }
    }
    if out.len() < 2 && slide.get("kpis").is_none() {
        return vec![];
    }
    out.truncate(4);
    out
}

fn split_kpi(s: &str) -> Option<(String, String)> {
    let i = s.char_indices().take(41).find(|(_, c)| matches!(c, ':' | '—')).map(|(i, _)| i)?;
    let (l, v) = (s.get(..i)?.trim(), s.get(i..)?.trim_start_matches([':', '—']).trim());
    (!l.is_empty() && !v.is_empty()).then(|| (l.to_string(), v.to_string()))
}

#[derive(Clone, Debug)]
struct ImageSpec {
    path: String,
    pos: &'static str,
    caption: String,
}

fn images(slide: &Value) -> Vec<ImageSpec> {
    let one = |v: &Value| -> Option<ImageSpec> {
        match v {
            Value::String(p) => Some(ImageSpec { path: p.clone(), pos: "full", caption: String::new() }),
            Value::Object(o) => {
                let path = o.get("path").or_else(|| o.get("src")).or_else(|| o.get("file")).and_then(Value::as_str)?.to_string();
                let pos =
                    match o.get("position").or_else(|| o.get("pos")).and_then(Value::as_str).unwrap_or("full").trim().to_ascii_lowercase().as_str() {
                        "left" => "left",
                        "right" => "right",
                        "logo" => "logo",
                        _ => "full",
                    };
                let caption = o.get("caption").or_else(|| o.get("alt")).and_then(Value::as_str).unwrap_or_default().to_string();
                Some(ImageSpec { path, pos, caption })
            }
            _ => None,
        }
    };
    let mut out: Vec<ImageSpec> = slide.get("image").and_then(one).into_iter().collect();
    if let Some(a) = slide.get("images").and_then(Value::as_array) {
        out.extend(a.iter().filter_map(one));
    }
    out
}

// ── overlays ────────────────────────────────────────────────────────────────────────────────

/// One overlay mark, in percent of the picture box.
#[derive(Clone, Debug, PartialEq)]
pub struct Overlay {
    pub kind: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub color: Rgba,
    pub label: String,
    pub points: Vec<(f64, f64)>,
    pub dashed: bool,
    /// `None`, `end` or `both`.
    pub arrow: Option<&'static str>,
    pub filled: bool,
}

fn pct(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().filter(|f| f.is_finite()).unwrap_or(0.0),
        Some(Value::String(s)) => s.trim().trim_end_matches('%').trim().parse::<f64>().ok().filter(|f| f.is_finite()).unwrap_or(0.0),
        _ => 0.0,
    }
}

fn parse_points(v: Option<&Value>) -> Vec<(f64, f64)> {
    match v {
        Some(Value::String(s)) => s
            .split([';', ' ', '\t', '\n'])
            .filter_map(|pair| {
                let mut it = pair.split(',');
                let (a, b) = (it.next()?, it.next()?);
                if it.next().is_some() {
                    return None;
                }
                let x = a.trim().trim_end_matches('%').parse::<f64>().ok().filter(|f| f.is_finite())?;
                let y = b.trim().trim_end_matches('%').parse::<f64>().ok().filter(|f| f.is_finite())?;
                Some((x, y))
            })
            .collect(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|p| match p {
                Value::Array(xy) => Some((pct(xy.first()), pct(xy.get(1)))),
                Value::Object(o) => Some((pct(o.get("x")), pct(o.get("y")))),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

fn style_flags(s: &str) -> (bool, Option<&'static str>, bool) {
    let toks: Vec<String> = s.to_ascii_lowercase().split([' ', ',', ';', '\t']).filter(|t| !t.is_empty()).map(String::from).collect();
    let has = |t: &str| toks.iter().any(|x| x == t);
    let arrow = if has("arrows") || has("both") {
        Some("both")
    } else if has("arrow") {
        Some("end")
    } else {
        None
    };
    (has("dashed") || has("dash"), arrow, has("fill") || has("filled"))
}

const OVERLAY_KINDS: [&str; 5] = ["rect", "ellipse", "line", "text", "path"];

fn overlay_kind(k: &str) -> Option<&'static str> {
    Some(match k.trim().to_ascii_lowercase().as_str() {
        "rect" | "rectangle" | "box" | "square" => "rect",
        "ellipse" | "circle" | "oval" => "ellipse",
        "line" | "arrow" | "leader" => "line",
        "text" | "label" => "text",
        "path" | "freeform" | "polyline" | "polygon" | "route" => "path",
        _ => return None,
    })
}

/// Overlays from `[{kind, x, y, w, h, color, label, points, dashed, arrow, filled, style}]`, or
/// from the platform's overlay table rows `[[shape, x, y, w, h, colour, label, points?, style?]]`
/// (a `shape` header row places the `points` and `style` columns).
pub fn parse_overlays(v: &Value, warnings: &mut Vec<String>) -> Vec<Overlay> {
    let rows = match v {
        Value::Object(o) => o.get("rows").cloned().unwrap_or(Value::Null),
        other => other.clone(),
    };
    let Value::Array(items) = rows else { return vec![] };
    let header: Option<Vec<String>> = items.iter().find_map(|r| {
        let a = r.as_array()?;
        let first = a.first()?.as_str()?.trim().to_ascii_lowercase();
        (first == "shape").then(|| a.iter().map(|c| c.as_str().unwrap_or_default().trim().to_ascii_lowercase()).collect())
    });
    let col = |name: &str, fallback: usize| header.as_ref().and_then(|h| h.iter().position(|c| c == name)).unwrap_or(fallback);
    let (points_col, style_col) = (col("points", 7), col("style", 8));
    let mut out = vec![];
    for (i, item) in items.iter().enumerate().take(2000) {
        let (kind_raw, x, y, w, h, color, label, points, style, dashed, arrow, filled) = match item {
            Value::Array(r) => {
                let s = |k: usize| r.get(k).map(|c| c.as_str().map(String::from).unwrap_or_else(|| c.to_string())).unwrap_or_default();
                let kind = s(0);
                if kind.trim().eq_ignore_ascii_case("shape") {
                    continue;
                }
                (
                    kind,
                    pct(r.get(1)),
                    pct(r.get(2)),
                    pct(r.get(3)),
                    pct(r.get(4)),
                    s(5),
                    s(6),
                    parse_points(r.get(points_col)),
                    s(style_col),
                    None,
                    None,
                    None,
                )
            }
            Value::Object(o) => {
                let s = |k: &str| o.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                let kind = o.get("kind").or_else(|| o.get("shape")).or_else(|| o.get("type")).and_then(Value::as_str).unwrap_or_default().to_string();
                let color = o.get("color").or_else(|| o.get("colour")).and_then(Value::as_str).unwrap_or_default().to_string();
                let arrow = match o.get("arrow") {
                    Some(Value::Bool(true)) => Some(Some("end")),
                    Some(Value::String(a)) if a.eq_ignore_ascii_case("both") || a.eq_ignore_ascii_case("arrows") => Some(Some("both")),
                    Some(Value::String(a)) if !a.is_empty() && !a.eq_ignore_ascii_case("none") => Some(Some("end")),
                    Some(_) => Some(None),
                    None => None,
                };
                (
                    kind,
                    pct(o.get("x")),
                    pct(o.get("y")),
                    pct(o.get("w").or_else(|| o.get("x2"))),
                    pct(o.get("h").or_else(|| o.get("y2"))),
                    color,
                    o.get("label").or_else(|| o.get("text")).and_then(Value::as_str).unwrap_or_default().to_string(),
                    parse_points(o.get("points")),
                    s("style"),
                    o.get("dashed").and_then(Value::as_bool),
                    arrow,
                    o.get("filled").or_else(|| o.get("fill")).and_then(Value::as_bool),
                )
            }
            _ => continue,
        };
        let Some(kind) = overlay_kind(&kind_raw) else {
            // The platform skips rows that are not shapes (the header, a stray caption row).
            if matches!(item, Value::Object(_)) {
                warnings.push(format!("overlay {i}: unknown kind `{kind_raw}` (expected one of {})", OVERLAY_KINDS.join(", ")));
            }
            continue;
        };
        let (sd, sa, sf) = style_flags(&style);
        let arrow_word = kind_raw.trim().eq_ignore_ascii_case("arrow").then_some("end");
        let color = Rgba::from_hex(color.trim().trim_start_matches('#')).unwrap_or(Rgba::rgb(0x2B, 0x6C, 0xB0));
        let o = Overlay {
            kind: kind.into(),
            x,
            y,
            w,
            h,
            color,
            label: label.trim().to_string(),
            points,
            dashed: dashed.unwrap_or(sd),
            arrow: arrow.unwrap_or(sa).or(arrow_word),
            filled: filled.unwrap_or(sf),
        };
        if o.kind == "path" && o.points.len() < 2 {
            warnings.push(format!("overlay {i}: a path needs at least two points"));
            continue;
        }
        out.push(o);
    }
    out
}

/// The shapes that draw `marks` over the picture box `bx` (points), as the platform draws them.
fn overlay_shapes(b: &mut Builder, bx: Xfrm, marks: &[Overlay]) -> Result<Vec<Shape>> {
    let clamp = |p: f64| p.clamp(0.0, 100.0);
    let fx = |p: f64| bx.x + clamp(p) / 100.0 * bx.w;
    let fy = |p: f64| bx.y + clamp(p) / 100.0 * bx.h;
    let min_side = 0.02 * 72.0 * b.sx;
    let fw = |p: f64| (clamp(p) / 100.0 * bx.w).max(min_side);
    let fh = |p: f64| (clamp(p) / 100.0 * bx.h).max(min_side);
    let stroke = |m: &Overlay, width: f64| Line {
        fill: Some(Fill::solid(ColorRef::rgb(m.color))),
        width: Some(width),
        dash: m.dashed.then(|| Dash::Preset { name: "dash".into() }),
        tail: m.arrow.map(|_| LineEnd { kind: "triangle".into(), w: "med".into(), len: "med".into() }),
        head: (m.arrow == Some("both")).then(|| LineEnd { kind: "triangle".into(), w: "med".into(), len: "med".into() }),
        ..Default::default()
    };
    let translucent = |c: Rgba| Fill::solid(ColorRef::rgb(c).with(ColorTransform::Alpha(35_000)));
    let label_style = |color: Rgba, size: f64, align: Option<Align>| TextStyle {
        size,
        bold: true,
        italic: false,
        color,
        align,
        anchor: Anchor::Top,
        shrink: false,
        head: false,
    };
    let mut out = vec![];
    for m in marks {
        let label = [Para { text: m.label.clone(), level: 0, bullet: false }];
        match m.kind.as_str() {
            "text" => {
                let w = if m.w > 0.0 { m.w } else { 20.0 };
                let h = if m.h > 0.0 { m.h } else { 5.0 };
                let mut t = b.text_box(Xfrm::new(fx(m.x), fy(m.y), fw(w), fh(h)), &label, &label_style(m.color, 11.0, None), "Overlay Label")?;
                t.descr = m.label.clone();
                out.push(t);
            }
            "path" => {
                let xs: Vec<f64> = m.points.iter().map(|p| fx(p.0)).collect();
                let ys: Vec<f64> = m.points.iter().map(|p| fy(p.1)).collect();
                let x0 = xs.iter().copied().fold(f64::INFINITY, f64::min);
                let y0 = ys.iter().copied().fold(f64::INFINITY, f64::min);
                let bw = (xs.iter().copied().fold(f64::NEG_INFINITY, f64::max) - x0).max(min_side);
                let bh = (ys.iter().copied().fold(f64::NEG_INFINITY, f64::max) - y0).max(min_side);
                let mut d = String::new();
                for (i, (x, y)) in xs.iter().zip(&ys).enumerate() {
                    d.push_str(&format!("{} {:.3} {:.3} ", if i == 0 { "M" } else { "L" }, x - x0, y - y0));
                }
                if m.filled {
                    d.push('Z');
                }
                let shape = Shape {
                    id: b.id()?,
                    name: if m.label.is_empty() { "Overlay Path".into() } else { format!("Overlay Path: {}", m.label) },
                    xfrm: Some(Xfrm::new(x0, y0, bw, bh)),
                    geom: Geom::Custom {
                        paths: vec![CustomPath {
                            w: bw,
                            h: bh,
                            d: d.trim().to_string(),
                            fill: if m.filled { deckcraft_geom::preset::FillMode::Norm } else { deckcraft_geom::preset::FillMode::None },
                            stroke: true,
                        }],
                    },
                    fill: Some(if m.filled { translucent(m.color) } else { Fill::None }),
                    line: Some(stroke(m, 2.0)),
                    descr: m.label.clone(),
                    ..Default::default()
                };
                out.push(shape);
                if !m.label.is_empty() {
                    let r = Xfrm::new(x0, y0 - 0.18 * 72.0 * b.sy, bw.max(1.2 * 72.0 * b.sx), 0.2 * 72.0 * b.sy);
                    out.push(b.text_box(r, &label, &label_style(m.color, 9.0, None), "Overlay Label")?);
                }
            }
            "line" => {
                // w/h are the END point; a leg running right-to-left or upwards is a positive box with
                // the line flipped inside it (a negative extent is invalid OOXML).
                let (x1, y1, x2, y2) = (fx(m.x), fy(m.y), fx(m.w), fy(m.h));
                let mut r = Xfrm::new(x1.min(x2), y1.min(y2), (x2 - x1).abs(), (y2 - y1).abs());
                r.flip_h = x2 < x1;
                r.flip_v = y2 < y1;
                out.push(Shape {
                    id: b.id()?,
                    name: if m.label.is_empty() { "Overlay Line".into() } else { format!("Overlay Line: {}", m.label) },
                    xfrm: Some(r),
                    geom: Geom::preset("line"),
                    line: Some(stroke(m, 2.0)),
                    descr: m.label.clone(),
                    ..Default::default()
                });
            }
            kind => {
                let r = Xfrm::new(fx(m.x), fy(m.y), fw(m.w), fh(m.h));
                // A dashed box or circle is an outline (an exclusion zone, a future compound).
                out.push(Shape {
                    id: b.id()?,
                    name: if m.label.is_empty() { "Overlay Shape".into() } else { format!("Overlay: {}", m.label) },
                    xfrm: Some(r),
                    geom: Geom::preset(if kind == "ellipse" { "ellipse" } else { "rect" }),
                    fill: Some(if m.dashed { Fill::None } else { translucent(m.color) }),
                    line: Some(stroke(m, 1.0)),
                    descr: m.label.clone(),
                    ..Default::default()
                });
                if !m.label.is_empty() {
                    let lr = Xfrm::new(r.x, r.y + r.h / 2.0 - 0.12 * 72.0 * b.sy, r.w, 0.24 * 72.0 * b.sy);
                    out.push(b.text_box(lr, &label, &label_style(Rgba::rgb(0x1A, 0x1A, 0x1A), 9.0, Some(Align::Center)), "Overlay Label")?);
                }
            }
        }
    }
    Ok(out)
}

// ── layouts ─────────────────────────────────────────────────────────────────────────────────

fn column_widths(rows: &[Vec<String>], total: f64) -> Vec<f64> {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return vec![];
    }
    let raw: Vec<f64> =
        (0..cols).map(|c| rows.iter().map(|r| r.get(c).map(|s| s.chars().count()).unwrap_or(0)).max().unwrap_or(0).clamp(6, 60) as f64).collect();
    let sum: f64 = raw.iter().sum::<f64>().max(1.0);
    let min = total * 0.8 / 12.0;
    let scaled: Vec<f64> = raw.iter().map(|w| (w / sum * total).max(min)).collect();
    let over: f64 = scaled.iter().sum::<f64>().max(1e-9);
    scaled.iter().map(|w| w / over * total).collect()
}

fn shade(c: Rgba) -> Rgba {
    let d = if is_dark(c) { 14i16 } else { -10 };
    let f = |v: u8| (i16::from(v) + d).clamp(0, 255) as u8;
    Rgba::rgb(f(c.r), f(c.g), f(c.b))
}

/// Height (points) a table row needs: lines of wrapped text at `size`.
fn row_height(row: &[String], widths: &[f64], size: f64) -> f64 {
    let lines = row
        .iter()
        .zip(widths)
        .map(|(t, w)| {
            let per_line = ((w - 14.4) / (size * 0.52)).max(4.0);
            let chars = md_plain(t).chars().count() as f64;
            (chars / per_line).ceil().max(1.0) + t.matches('\n').count() as f64
        })
        .fold(1.0, f64::max);
    lines * size * 1.2 + 7.2 + 2.0
}

fn table_slides(b: &mut Builder, spec: &Value, title: &str, rows: &[Vec<String>], notes: Option<&str>) -> Result<Vec<Slide>> {
    let Some(head) = rows.first() else { return Ok(vec![]) };
    let cols = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let total_w = (GW - 1.2) * 72.0 * b.sx;
    let widths = column_widths(rows, total_w);
    let size = spec.get("fontSize").and_then(Value::as_f64).filter(|s| s.is_finite()).unwrap_or(12.0).clamp(6.0, 40.0);
    let top = 1.7 * 72.0 * b.sy;
    let avail = (GH - 1.7 - 0.6) * 72.0 * b.sy;
    let head_h = row_height(head, &widths, size);
    let fixed = spec.get("rowsPerSlide").and_then(Value::as_u64).map(|n| n.clamp(1, 500) as usize);
    // Pages of body rows: by the height they need, or a fixed count.
    let body: Vec<&Vec<String>> = rows.iter().skip(1).collect();
    let mut pages: Vec<Vec<&Vec<String>>> = vec![];
    let mut cur: Vec<&Vec<String>> = vec![];
    let mut used = head_h;
    for r in body {
        let h = row_height(r, &widths, size);
        let full = match fixed {
            Some(n) => cur.len() >= n,
            None => !cur.is_empty() && used + h > avail,
        };
        if full {
            pages.push(std::mem::take(&mut cur));
            used = head_h;
        }
        used += h;
        cur.push(r);
    }
    if !cur.is_empty() || pages.is_empty() {
        pages.push(cur);
    }
    if pages.len() > 200 {
        return Err(bad("deck.build", format!("the table needs {} slides; give at most 200 slides of rows at once", pages.len())));
    }
    let n = pages.len();
    let mut out = vec![];
    for (pi, page) in pages.into_iter().enumerate() {
        let mut s = b.slide(LayoutType::TitleOnly)?;
        let t = if n > 1 { format!("{title} ({}/{n})", pi + 1) } else { title.to_string() };
        b.heading(&mut s, &t)?;
        if pi == 0 {
            Builder::notes(&mut s, notes);
        }
        let mut tbl = Table { cols: widths.clone(), rows: vec![], style: "{2D5ABB26-0587-4C30-8999-92F81FD0307C}".into(), ..Default::default() };
        tbl.first_row = true;
        tbl.band_row = false;
        let border = Some(Line::solid(ColorRef::rgb(b.theme.muted), 0.5));
        let mk_cell = |b: &Builder, text: &str, header: bool, fill: Rgba| {
            let st = TextStyle {
                size,
                bold: header,
                italic: false,
                color: if header { b.theme.table_head_text } else { b.theme.text },
                align: None,
                anchor: Anchor::Top,
                shrink: false,
                head: false,
            };
            let mut body = b.body(&[Para { text: if header { md_plain(text) } else { text.to_string() }, level: 0, bullet: false }], &st);
            body.body = Default::default();
            Cell {
                text: body,
                fill: Some(Fill::solid(ColorRef::rgb(fill))),
                borders: [border.clone(), border.clone(), border.clone(), border.clone()],
                grid_span: 1,
                row_span: 1,
                ..Default::default()
            }
        };
        let pad = |r: &Vec<String>| -> Vec<String> { (0..cols).map(|c| r.get(c).cloned().unwrap_or_default()).collect() };
        tbl.rows.push(Row { height: head_h, cells: pad(head).iter().map(|c| mk_cell(b, c, true, b.theme.table_head_bg)).collect() });
        for (ri, r) in page.iter().enumerate() {
            let fill = if ri % 2 == 1 { b.theme.bg } else { shade(b.theme.bg) };
            tbl.rows.push(Row { height: row_height(r, &widths, size), cells: pad(r).iter().map(|c| mk_cell(b, c, false, fill)).collect() });
        }
        let h = tbl.height();
        let shape = Shape {
            id: b.id()?,
            name: "Table".into(),
            xfrm: Some(Xfrm::new(0.6 * 72.0 * b.sx, top, total_w, h)),
            kind: ShapeKind::Table(tbl),
            ..Default::default()
        };
        s.shapes.push(shape);
        out.push(s);
    }
    Ok(out)
}

fn chart_slide(b: &mut Builder, title: &str, spec: &Value, notes: Option<&str>) -> Result<Option<Slide>> {
    let c = spec.get("chart");
    let kind_name = c
        .and_then(|c| c.get("kind").or_else(|| c.get("type")))
        .and_then(Value::as_str)
        .or_else(|| spec.get("chartKind").and_then(Value::as_str))
        .unwrap_or("bar");
    let (cats, series): (Vec<String>, Vec<(String, Vec<f64>)>) = match c.and_then(|c| c.get("series")).and_then(Value::as_array) {
        Some(series) => (
            strings(c.and_then(|c| c.get("categories").or_else(|| c.get("labels")))),
            series
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let name = s.get("name").and_then(Value::as_str).map(String::from).unwrap_or_else(|| format!("Series {}", i + 1));
                    let vals = s
                        .get("values")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(|v| v.as_f64().or_else(|| v.as_str().and_then(number_of))).collect())
                        .unwrap_or_default();
                    (name, vals)
                })
                .collect(),
        ),
        None => match chart_from_rows(&table_rows(spec.get("table").or_else(|| c.and_then(|c| c.get("table"))))) {
            Some(d) => d,
            None => {
                b.warnings
                    .push(format!("chart slide `{title}`: no numeric data (give chart.series or a table with numeric columns); drawn as bullets"));
                return Ok(None);
            }
        },
    };
    let kind = match kind_name.to_ascii_lowercase().as_str() {
        // The platform's `bar` is pptxgenjs's vertical bar: a column chart.
        "bar" | "column" => ChartType::Column,
        "hbar" | "horizontal" | "horizontalbar" => ChartType::Bar,
        other => super::insert::chart_type(other),
    };
    let pie = matches!(kind, ChartType::Pie | ChartType::Doughnut);
    let palette = [b.theme.accent, b.theme.accent2, b.theme.heading, b.theme.muted];
    let mut chart = Chart::sample(kind);
    chart.title = None;
    chart.categories = cats;
    let series: Vec<(String, Vec<f64>)> = if pie { series.into_iter().take(1).collect() } else { series };
    chart.series = series
        .iter()
        .enumerate()
        .map(|(i, (name, vals))| {
            let col = palette.get(i % palette.len()).copied().unwrap_or(b.theme.accent);
            Series {
                name: name.clone(),
                values: vals.iter().map(|v| Some(*v)).collect(),
                fill: (!pie).then(|| Fill::solid(ColorRef::rgb(col))),
                line: matches!(kind, ChartType::Line | ChartType::LineMarkers).then(|| Line::solid(ColorRef::rgb(col), 2.25)),
                point_fills: if pie {
                    (0..vals.len()).map(|k| (k, Fill::solid(ColorRef::rgb(palette.get(k % palette.len()).copied().unwrap_or(col))))).collect()
                } else {
                    vec![]
                },
                ..Default::default()
            }
        })
        .collect();
    chart.legend = (chart.series.len() > 1 || pie).then(|| "b".to_string());
    chart.data_labels = !matches!(kind, ChartType::Line | ChartType::LineMarkers);
    chart.text_color = Some(ColorRef::rgb(b.theme.muted));
    chart.text_size = Some(11.0);
    let mut s = b.slide(LayoutType::TitleOnly)?;
    b.heading(&mut s, title)?;
    Builder::notes(&mut s, notes);
    let shape = Shape {
        id: b.id()?,
        name: "Chart".into(),
        xfrm: Some(b.r(0.6, 1.7, GW - 1.2, 4.9)),
        kind: ShapeKind::Chart(Box::new(chart)),
        ..Default::default()
    };
    s.shapes.push(shape);
    Ok(Some(s))
}

/// The slides one spec item makes (a table may page over several).
fn build_slide(b: &mut Builder, i: usize, spec: &Value) -> Result<Vec<Slide>> {
    let raw_layout = spec.get("layout").and_then(Value::as_str).unwrap_or_default();
    let layout = match layout_name(raw_layout) {
        Some(l) => l,
        None => {
            b.warnings.push(format!("slide {i}: unknown layout `{raw_layout}`, drawn as bullets"));
            "bullets"
        }
    };
    let title = md_plain(spec.get("title").or_else(|| spec.get("heading")).and_then(Value::as_str).unwrap_or_default());
    let notes_owned = strings(spec.get("notes").or_else(|| spec.get("speakerNotes")).or_else(|| spec.get("speaker_notes"))).join("\n");
    let notes = Some(notes_owned.as_str()).filter(|n| !n.trim().is_empty());
    let th = b.theme.clone();

    if layout == "section" {
        let mut s = b.slide(LayoutType::SectionHeader)?;
        if !b.template {
            s.background = Some(Background::Fill { fill: Builder::solid(th.band_bg) });
        }
        let st = TextStyle {
            size: 32.0,
            bold: true,
            italic: false,
            color: th.band_text,
            align: Some(Align::Left),
            anchor: Anchor::Middle,
            shrink: true,
            head: true,
        };
        let rect = b.r(0.8, GH / 2.0 - 0.8, GW - 1.6, 1.2);
        b.set_title(&mut s, &[PhType::Title, PhType::CtrTitle], if title.is_empty() { "Section" } else { &title }, rect, &st)?;
        if let Some(sub) = spec.get("subtitle").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) {
            let st = TextStyle { size: 16.0, ..st };
            s.shapes.push(b.text_box(
                b.r(0.8, GH / 2.0 + 0.5, GW - 1.6, 0.8),
                &[Para { text: sub.into(), level: 0, bullet: false }],
                &st,
                "Subtitle",
            )?);
        }
        Builder::notes(&mut s, notes);
        return Ok(vec![s]);
    }

    let rows = table_rows(spec.get("table"));
    let marks = match spec.get("overlays").or_else(|| spec.get("overlay")).or_else(|| spec.get("markup")) {
        Some(v) => {
            let mut w = vec![];
            let m = parse_overlays(v, &mut w);
            b.warnings.extend(w.into_iter().map(|x| format!("slide {i}: {x}")));
            m
        }
        None => vec![],
    };
    let mut paras =
        paragraphs(spec.get("paragraphs").or_else(|| spec.get("body")).or_else(|| spec.get("text").filter(|_| !matches!(layout, "callout"))));
    let bl = bullets(spec.get("bullets").or_else(|| spec.get("points")).or_else(|| spec.get("items")));
    let quote_text = strings(spec.get("quote").or_else(|| spec.get("quotes"))).join("\n");

    if layout == "chart"
        && let Some(cs) = chart_slide(b, if title.is_empty() { "Chart" } else { &title }, spec, notes)?
    {
        // The chart replaces the table slide for this item, as on the platform.
        return Ok(vec![cs]);
    }

    let table_only = layout == "table" && bl.is_empty() && paras.is_empty() && quote_text.is_empty() && !rows.is_empty();
    let mut out = vec![];
    if !table_only {
        let mut s = b.slide(LayoutType::TitleOnly)?;
        b.heading(&mut s, if title.is_empty() { "Overview" } else { &title })?;
        Builder::notes(&mut s, notes);

        // Images: full (default) | left | right | logo.
        let (mut body_x, mut body_w) = (0.6, GW - 1.2);
        let imgs = images(spec);
        if layout == "image" && imgs.is_empty() {
            b.warnings.push(format!("slide {i}: an image slide with no image"));
        }
        let mut placed = vec![];
        for im in imgs {
            match b.media_from_path(&im.path) {
                Ok(m) => placed.push((im, m)),
                Err(e) => b.warnings.push(format!("slide {i}: image `{}` skipped: {e}", im.path)),
            }
        }
        for (im, m) in placed.iter().filter(|(im, _)| im.pos == "logo") {
            let pic = b.picture(*m, b.r(GW - 1.9, 0.45, 1.3, 0.62), if im.caption.is_empty() { "Logo" } else { &im.caption })?;
            s.shapes.push(pic);
        }
        if let Some((im, m)) = placed.iter().find(|(im, _)| im.pos == "left" || im.pos == "right") {
            let half = (GW - 1.5) / 2.0;
            let ix = if im.pos == "left" { 0.6 } else { 0.9 + half };
            let bx = b.r(ix, 1.7, half, 4.4);
            let pic = b.picture(*m, bx, if im.caption.is_empty() { "Picture" } else { &im.caption })?;
            s.shapes.push(pic);
            if !marks.is_empty() {
                let shapes = overlay_shapes(b, bx, &marks)?;
                s.shapes.extend(shapes);
            }
            if !im.caption.is_empty() {
                let st = TextStyle { align: Some(Align::Center), ..TextStyle::body(th.muted, 11.0) };
                s.shapes.push(b.text_box(
                    b.r(ix, 6.15, half, 0.35),
                    &[Para { text: im.caption.clone(), level: 0, bullet: false }],
                    &st,
                    "Caption",
                )?);
            }
            body_x = if im.pos == "left" { 0.9 + half } else { 0.6 };
            body_w = half;
        } else {
            let full: Vec<&(ImageSpec, deckcraft_model::MediaId)> = placed.iter().filter(|(im, _)| im.pos == "full").take(2).collect();
            let n = full.len().max(1) as f64;
            let gw = (GW - 1.2 - 0.3 * (n - 1.0)) / n;
            for (k, (im, m)) in full.iter().enumerate() {
                let x = 0.6 + k as f64 * (gw + 0.3);
                let bx = b.r(x, 1.7, gw, 4.0);
                let pic = b.picture(*m, bx, if im.caption.is_empty() { "Picture" } else { &im.caption })?;
                s.shapes.push(pic);
                if k == 0 && !marks.is_empty() {
                    let shapes = overlay_shapes(b, bx, &marks)?;
                    s.shapes.extend(shapes);
                }
                if !im.caption.is_empty() {
                    let st = TextStyle { align: Some(Align::Center), ..TextStyle::body(th.muted, 12.0) };
                    s.shapes.push(b.text_box(b.r(x, 5.8, gw, 0.4), &[Para { text: im.caption.clone(), level: 0, bullet: false }], &st, "Caption")?);
                }
            }
            if !full.is_empty() {
                // The body goes under the picture.
                if !bl.is_empty() || !paras.is_empty() {
                    let st = TextStyle::body(th.text, 14.0);
                    let all: Vec<Para> = paras.iter().chain(bl.iter()).cloned().collect();
                    s.shapes.push(b.text_box(b.r(0.6, 6.2, GW - 1.2, 0.6), &all, &st, "Body")?);
                }
                b.decorate(&mut s)?;
                out.push(s);
                if !rows.is_empty() && marks.is_empty() {
                    out.extend(table_slides(b, spec, if title.is_empty() { "Table" } else { &title }, &rows, None)?);
                }
                return Ok(out);
            } else if !marks.is_empty() {
                b.warnings.push(format!("slide {i}: overlays need a full-width image; none was placed"));
            }
        }

        let base = TextStyle::body(th.text, 16.0);
        let kp = if layout == "kpi" { kpis(spec) } else { vec![] };
        if layout == "kpi" && kp.is_empty() {
            b.warnings.push(format!("slide {i}: a kpi slide needs at least two `Label: Value` items; drawn as bullets"));
        }
        if !kp.is_empty() {
            let gap = 0.3;
            let n = kp.len() as f64;
            let cw = (GW - 1.2 - gap * (n - 1.0)) / n;
            for (k, (label, value)) in kp.iter().enumerate() {
                let x = 0.6 + k as f64 * (cw + gap);
                let card =
                    b.rect_shape(b.r(x, 1.9, cw, 2.1), Some(Builder::solid(th.bg)), Some(Line::solid(ColorRef::rgb(th.accent), 1.0)), "KPI Card")?;
                s.shapes.push(card);
                let val = md_plain(value);
                let len = val.chars().count();
                let vsize = if len <= 12 {
                    30.0
                } else if len <= 24 {
                    22.0
                } else if len <= 48 {
                    16.0
                } else {
                    12.0
                };
                let vs = TextStyle {
                    size: vsize,
                    bold: true,
                    italic: false,
                    color: th.accent,
                    align: Some(Align::Center),
                    anchor: Anchor::Middle,
                    shrink: true,
                    head: true,
                };
                s.shapes.push(b.text_box(b.r(x, 2.05, cw, 1.05), &[Para { text: val, level: 0, bullet: false }], &vs, "KPI Value")?);
                let ls = TextStyle {
                    size: 13.0,
                    bold: false,
                    italic: false,
                    color: th.muted,
                    align: Some(Align::Center),
                    anchor: Anchor::Top,
                    shrink: true,
                    head: false,
                };
                s.shapes.push(b.text_box(b.r(x, 3.1, cw, 0.8), &[Para { text: md_plain(label), level: 0, bullet: false }], &ls, "KPI Label")?);
            }
            if !paras.is_empty() {
                s.shapes.push(b.text_box(b.r(0.6, 4.3, GW - 1.2, 1.6), &paras, &base, "Body")?);
            }
        } else if layout == "quote" || layout == "callout" {
            let text = if layout == "callout" {
                let t = strings(spec.get("text").or_else(|| spec.get("callout"))).join("\n");
                if t.is_empty() {
                    if quote_text.is_empty() { paras.iter().map(|p| p.text.clone()).collect::<Vec<_>>().join("\n") } else { quote_text.clone() }
                } else {
                    t
                }
            } else if quote_text.is_empty() {
                paras.iter().map(|p| p.text.clone()).collect::<Vec<_>>().join("\n")
            } else {
                quote_text.clone()
            };
            if layout == "quote" && quote_text.is_empty() {
                paras.clear();
            }
            let band = b.rect_shape(
                b.r(0.6, 1.9, GW - 1.2, 2.6),
                Some(Builder::solid(th.accent2)),
                None,
                if layout == "quote" { "Quote Band" } else { "Callout Band" },
            )?;
            s.shapes.push(band);
            let st = TextStyle {
                size: if layout == "quote" { 22.0 } else { 18.0 },
                bold: false,
                italic: layout == "quote",
                color: Rgba::WHITE,
                align: None,
                anchor: Anchor::Middle,
                shrink: true,
                head: false,
            };
            let mut lines: Vec<Para> = text.lines().map(|l| Para { text: md_plain(l), level: 0, bullet: false }).collect();
            if let Some(a) = spec.get("attribution").or_else(|| spec.get("author")).and_then(Value::as_str).filter(|a| !a.trim().is_empty()) {
                lines.push(Para { text: format!("— {}", a.trim()), level: 0, bullet: false });
            }
            s.shapes.push(b.text_box(b.r(1.0, 2.1, GW - 2.0, 2.2), &lines, &st, if layout == "quote" { "Quote" } else { "Callout" })?);
            if !bl.is_empty() {
                s.shapes.push(b.text_box(b.r(body_x, 4.8, body_w, 2.0), &bl, &base, "Body")?);
            }
        } else if layout == "two-column" && body_w > GW - 2.0 && (!bl.is_empty() || spec.get("left").is_some() || spec.get("right").is_some()) {
            let (left, right) = if spec.get("left").is_some() || spec.get("right").is_some() {
                let col = |k: &str| {
                    let v = spec.get(k);
                    let mut p = bullets(v.and_then(|v| v.get("bullets")).or(v.filter(|v| v.is_array() || v.is_string())));
                    if let Some(h) = v.and_then(|v| v.get("heading").or_else(|| v.get("title"))).and_then(Value::as_str) {
                        p.insert(0, Para { text: format!("**{h}**"), level: 0, bullet: false });
                    }
                    p
                };
                (col("left"), col("right"))
            } else {
                let mid = bl.len().div_ceil(2);
                (bl.get(..mid).unwrap_or_default().to_vec(), bl.get(mid..).unwrap_or_default().to_vec())
            };
            let cw = (GW - 1.5) / 2.0;
            if !paras.is_empty() {
                b.warnings.push(format!("slide {i}: two-column slides show bullets; paragraphs went to the notes"));
                let extra = paras.iter().map(|p| md_plain(&p.text)).collect::<Vec<_>>().join("\n");
                s.notes = TextBody::from_text(
                    &[notes.unwrap_or_default(), extra.as_str()].iter().filter(|t| !t.is_empty()).copied().collect::<Vec<_>>().join("\n"),
                );
            }
            s.shapes.push(b.text_box(b.r(0.6, 1.7, cw, 4.8), &left, &base, "Left Column")?);
            s.shapes.push(b.text_box(b.r(0.75 + cw, 1.7, cw, 4.8), &right, &base, "Right Column")?);
        } else {
            // Bullets (and the fallback for every layout that found nothing of its own).
            let mut y = 1.7;
            if !paras.is_empty() {
                let h = (0.32 * paras.len() as f64 + 0.3).min(2.6);
                s.shapes.push(b.text_box(b.r(body_x, y, body_w, h), &paras, &base, "Body")?);
                y += h + 0.15;
            }
            if !bl.is_empty() {
                s.shapes.push(b.text_box(b.r(body_x, y, body_w, (GH - y - 0.8).max(0.5)), &bl, &base, "Bullets")?);
            }
            if !quote_text.is_empty() {
                let st = TextStyle { italic: true, ..TextStyle::body(th.muted, 15.0) };
                let qs: Vec<Para> = quote_text.lines().map(|l| Para { text: l.to_string(), level: 0, bullet: false }).collect();
                s.shapes.push(b.text_box(b.r(0.6, (y + 0.2).min(GH - 2.0), GW - 1.2, 1.2), &qs, &st, "Quote")?);
            }
            if layout == "table" && rows.is_empty() {
                b.warnings.push(format!("slide {i}: a table slide with no table rows"));
            }
        }
        b.decorate(&mut s)?;
        out.push(s);
    }
    if !rows.is_empty() && !(layout == "image" && !marks.is_empty()) {
        let t = if title.is_empty() { "Table" } else { &title };
        let mut ts = table_slides(b, spec, t, &rows, if table_only { notes } else { None })?;
        for s in &mut ts {
            b.decorate(s)?;
        }
        out.extend(ts);
    }
    Ok(out)
}

/// A presentation to build on: the template's masters and layouts (its slides dropped), or a
/// blank one.
fn base_presentation(template: Option<&str>) -> Result<(Presentation, bool)> {
    let Some(path) = template else {
        return Ok((defaults::blank_presentation(defaults::WIDE, Default::default(), false), false));
    };
    let bytes = crate::io::read_file(path).map_err(EngineError::Other)?;
    if !deckcraft_pptx::sniff(&bytes) {
        return Err(bad("deck.build", format!("{path}: the template must be a .pptx or .potx file")));
    }
    let mut p = super::file::open_presentation(path, &bytes)?;
    p.slides.clear();
    p.sections.clear();
    p.custom_shows.clear();
    Ok((p, true))
}

/// The largest picture a template's masters and layouts show: the brand mark, as the platform
/// lifts it.
fn template_logo(p: &Presentation) -> Option<deckcraft_model::MediaId> {
    let mut best: Option<(usize, deckcraft_model::MediaId)> = None;
    let mut look = |shapes: &[Shape]| {
        deckcraft_model::walk(shapes, &mut |s, _| {
            if let ShapeKind::Picture { fill } = &s.kind
                && let Some(m) = p.media(fill.media)
            {
                let n = m.data.len();
                if (4096..2_000_000).contains(&n) && best.is_none_or(|(b, _)| n > b) {
                    best = Some((n, fill.media));
                }
            }
        });
    };
    for m in &p.masters {
        look(&m.shapes);
        for l in &m.layouts {
            look(&l.shapes);
        }
    }
    best.map(|(_, id)| id)
}

fn build(s: &mut Session, p: &Value) -> Result<Value> {
    let p = if p.is_object() { p.clone() } else { json!({}) };
    let mut warnings: Vec<String> = vec![];
    let template = str_param(&p, "template").filter(|t| !t.trim().is_empty());
    let (mut doc, from_template) = base_presentation(template)?;
    let base_theme = if from_template { theme_of_template(&doc) } else { preset_theme("corporate").unwrap_or_else(|| theme_of_template(&doc)) };
    let theme = theme_from(p.get("theme"), base_theme, &mut warnings);
    if !from_template {
        apply_theme_to_master(&mut doc, &theme);
    }
    let size = doc.slide_size;
    let mut b = Builder {
        doc,
        theme,
        sx: size.width / 960.0,
        sy: size.height / 540.0,
        template: from_template,
        warnings,
        logo: None,
        slide_numbers: bool_or(&p, "slideNumbers", true),
    };
    if let Some(logo) = str_param(&p, "logo").filter(|l| !l.trim().is_empty()) {
        match b.media_from_path(logo) {
            Ok(m) => b.logo = Some(m),
            Err(e) => b.warnings.push(format!("logo skipped: {e}")),
        }
    } else if from_template {
        // The template's masters already show its logo.
        let _ = template_logo(&b.doc);
    }
    let specs: Vec<Value> = match p.get("slides") {
        Some(Value::Array(a)) => a.clone(),
        Some(other) if !other.is_null() => return Err(bad("deck.build", "`slides` must be an array of slide objects")),
        _ => vec![],
    };
    if specs.len() > 2000 {
        return Err(bad("deck.build", format!("{} slides is too many for one call; build at most 2000 at once", specs.len())));
    }
    let mut slides: Vec<Slide> = vec![];
    let title = str_param(&p, "title").map(md_plain).filter(|t| !t.trim().is_empty());
    if bool_or(&p, "titleSlide", true)
        && let Some(t) = &title
    {
        let mut s = b.slide(LayoutType::Title)?;
        let th = b.theme.clone();
        if !b.template {
            s.background = Some(Background::Fill { fill: Builder::solid(th.band_bg) });
        }
        let st = TextStyle {
            size: 40.0,
            bold: true,
            italic: false,
            color: th.band_text,
            align: Some(Align::Left),
            anchor: Anchor::Top,
            shrink: true,
            head: true,
        };
        let rect = b.r(0.8, 2.4, GW - 1.6, 1.6);
        b.set_title(&mut s, &[PhType::CtrTitle, PhType::Title], t, rect, &st)?;
        if let Some(sub) = str_param(&p, "subtitle").filter(|t| !t.trim().is_empty()) {
            let st = TextStyle { size: 16.0, bold: false, ..st };
            let rect = b.r(0.8, 4.0, GW - 1.6, 0.8);
            if b.template && s.shapes.iter().any(|x| x.ph_type() == Some(PhType::SubTitle)) {
                b.set_title(&mut s, &[PhType::SubTitle], sub, rect, &st)?;
            } else {
                s.shapes.retain(|x| x.ph_type() != Some(PhType::SubTitle));
                s.shapes.push(b.text_box(rect, &[Para { text: sub.to_string(), level: 0, bullet: false }], &st, "Subtitle")?);
            }
        } else {
            s.shapes.retain(|x| x.ph_type() != Some(PhType::SubTitle));
        }
        if !b.template {
            s.shapes.push(b.rect_shape(b.r(0.8, 2.1, 2.2, 0.07), Some(Builder::solid(th.accent)), None, "Accent Rule")?);
        }
        Builder::notes(&mut s, str_param(&p, "notes"));
        slides.push(s);
    }
    crate::task::stage("building slides");
    for (i, spec) in specs.iter().enumerate() {
        // A safe point: nothing is added to the session until the whole deck is built.
        crate::task::step(i, specs.len())?;
        if !spec.is_object() {
            b.warnings.push(format!("slide {i}: not an object, skipped"));
            continue;
        }
        let made = build_slide(&mut b, i, spec)?;
        slides.extend(made);
        if slides.len() > 5000 {
            return Err(bad("deck.build", "more than 5000 slides; build the deck in parts"));
        }
    }
    let Builder { mut doc, warnings, .. } = b;
    let n = slides.len();
    doc.slides = slides.into_iter().map(Arc::new).collect();
    doc.props.title = title.clone().unwrap_or_default();
    if !s.prefs.author.is_empty() {
        doc.props.author = s.prefs.author.clone();
        doc.props.last_modified_by = s.prefs.author.clone();
    }
    // A template's own slides went; so do the pictures only they used.
    let used = crate::media_refs::presentation_media(&doc);
    doc.media.retain(|m| used.contains(&m.id));
    doc.fix_next_id();
    let name = title.clone().unwrap_or_else(|| s.next_untitled());
    let i = s.add_document(DocState::new(doc, None, name));
    let mut out = json!({"document": i, "slides": n, "warnings": warnings, "template": from_template});
    if let Some(path) = str_param(&p, "path").filter(|x| !x.trim().is_empty()) {
        let saved = s.execute("file.save", &json!({"path": path}))?;
        out["path"] = saved.get("path").cloned().unwrap_or(Value::Null);
        out["bytes"] = saved.get("bytes").cloned().unwrap_or(Value::Null);
    }
    Ok(out)
}

// ── picture.overlay ─────────────────────────────────────────────────────────────────────────

fn picture_overlay(s: &mut Session, p: &Value) -> Result<Value> {
    let st = s.doc()?;
    let id = match id_param(p, "id", "picture.overlay")? {
        Some(id) => id,
        None => st
            .selected_shapes()
            .into_iter()
            .find(|x| matches!(x.kind, ShapeKind::Picture { .. }))
            .map(|x| x.id)
            .or_else(|| st.shapes().iter().find(|x| matches!(x.kind, ShapeKind::Picture { .. })).map(|x| x.id))
            .ok_or_else(|| bad("picture.overlay", "no picture on this slide; give `id` (see slide.inspect)"))?,
    };
    let pic = st.shape(id).cloned().ok_or_else(|| bad("picture.overlay", format!("no shape {}", id.0)))?;
    let ShapeKind::Picture { fill } = &pic.kind else {
        return Err(bad("picture.overlay", format!("shape {} is not a picture", id.0)));
    };
    let mut bx = xfrm_of(&st.doc, &st.selection, &pic);
    if str_param(p, "relativeTo").is_some_and(|r| r.eq_ignore_ascii_case("image")) {
        // The picture itself inside its box (padding by negative crop excluded).
        let [l, t, r, bt] = fill.crop;
        let (fw, fh) = ((1.0 - l - r).max(1e-6), (1.0 - t - bt).max(1e-6));
        let (ox, oy) = ((-l).max(0.0) / fw * bx.w, (-t).max(0.0) / fh * bx.h);
        let (iw, ih) = (bx.w * (1.0 - (-l).max(0.0) - (-r).max(0.0)) / fw, bx.h * (1.0 - (-t).max(0.0) - (-bt).max(0.0)) / fh);
        bx = Xfrm::new(bx.x + ox, bx.y + oy, iw.max(1.0), ih.max(1.0));
    }
    let mut warnings = vec![];
    let src = p.get("overlays").or_else(|| p.get("rows").map(|_| p)).cloned().unwrap_or(Value::Null);
    let marks = parse_overlays(&src, &mut warnings);
    if marks.is_empty() {
        return Err(bad("picture.overlay", "no overlays given: pass `overlays: [{kind, x, y, w, h, color, label}]` or the overlay table `rows`"));
    }
    let doc_clone = st.doc.as_ref().clone();
    let size = doc_clone.slide_size;
    let theme = theme_of_template(&doc_clone);
    let mut b = Builder {
        doc: doc_clone,
        theme,
        sx: size.width / 960.0,
        sy: size.height / 540.0,
        template: true,
        warnings: vec![],
        logo: None,
        slide_numbers: false,
    };
    let shapes = overlay_shapes(&mut b, bx, &marks)?;
    let next_id = b.doc.next_id;
    let ids: Vec<u32> = shapes.iter().map(|x| x.id.0).collect();
    s.edit(|doc, sel| {
        doc.next_id = doc.next_id.max(next_id);
        let list = crate::shapes_mut(doc, sel).ok_or_else(|| bad("picture.overlay", "no slide"))?;
        let at = list.iter().position(|x| x.id == id).map_or(list.len(), |k| k + 1);
        for (k, sh) in shapes.into_iter().enumerate() {
            list.insert((at + k).min(list.len()), sh);
        }
        Ok(())
    })?;
    Ok(json!({"ids": ids, "picture": id.0, "warnings": warnings}))
}

// ── design.adoptTemplate ────────────────────────────────────────────────────────────────────

fn adopt_template(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_param(p, "path").ok_or_else(|| bad("design.adoptTemplate", "missing `path` (a .pptx or .potx)"))?;
    let bytes = crate::io::read_file(path).map_err(EngineError::Other)?;
    if !deckcraft_pptx::sniff(&bytes) {
        return Err(bad("design.adoptTemplate", format!("{path}: not a .pptx or .potx file")));
    }
    let tpl = super::file::open_presentation(path, &bytes)?;
    if tpl.masters.is_empty() {
        return Err(bad("design.adoptTemplate", format!("{path} has no slide master")));
    }
    let mut moved = 0usize;
    s.edit(|doc, _| {
        // Ids from the template are moved past the deck's so nothing collides.
        let mut tpl = tpl;
        let offset = doc.next_id.max(1);
        renumber(&mut tpl, offset).ok_or_else(|| bad("design.adoptTemplate", NO_ID_SPACE))?;
        // Media the template's masters use come along (its slides' do not).
        tpl.slides.clear();
        let used = crate::media_refs::presentation_media(&tpl);
        let mut media_map: Vec<(deckcraft_model::MediaId, deckcraft_model::MediaId)> = vec![];
        for m in tpl.media.iter().filter(|m| used.contains(&m.id)) {
            if let Some(id) = doc.add_media(&m.name, &m.content_type, m.data.as_ref().clone()) {
                media_map.push((m.id, id));
            }
        }
        for master in &mut tpl.masters {
            crate::media_refs::visit_mut::master(Arc::make_mut(master), &mut |id: &mut deckcraft_model::MediaId| {
                if let Some((_, n)) = media_map.iter().find(|(o, _)| o == id) {
                    *id = *n;
                }
            });
        }
        let new_layouts: Vec<(LayoutId, LayoutType, String)> =
            tpl.masters.iter().flat_map(|m| m.layouts.iter().map(|l| (l.id, l.kind, l.name.clone()))).collect();
        let old_kinds: std::collections::HashMap<LayoutId, (LayoutType, String)> =
            doc.masters.iter().flat_map(|m| m.layouts.iter().map(|l| (l.id, (l.kind, l.name.clone())))).collect();
        let pick = |kind: LayoutType, name: &str| -> LayoutId {
            new_layouts
                .iter()
                .find(|(_, k, _)| *k == kind && kind != LayoutType::Custom)
                .or_else(|| new_layouts.iter().find(|(_, _, n)| n.eq_ignore_ascii_case(name)))
                .or_else(|| new_layouts.iter().find(|(_, k, _)| *k == LayoutType::TitleAndContent))
                .or_else(|| new_layouts.first())
                .map(|(id, _, _)| *id)
                .unwrap_or_default()
        };
        for sl in &mut doc.slides {
            let (kind, name) = old_kinds.get(&sl.layout).cloned().unwrap_or((LayoutType::TitleAndContent, String::new()));
            Arc::make_mut(sl).layout = pick(kind, &name);
            moved += 1;
        }
        doc.masters = tpl.masters;
        doc.next_id = doc.next_id.max(tpl.next_id);
        doc.fix_next_id();
        Ok(())
    })?;
    let st = s.doc()?;
    Ok(json!({
        "slides": moved,
        "theme": st.doc.masters.first().map(|m| m.theme.name.clone()),
        "layouts": st.doc.masters.iter().flat_map(|m| m.layouts.iter().map(|l| l.name.clone())).collect::<Vec<_>>(),
    }))
}

/// Shift every id of `p`'s masters and layouts (and media) by `offset`. `None` when ids overflow.
fn renumber(p: &mut Presentation, offset: u32) -> Option<()> {
    let bump = |v: &mut u32| -> Option<()> {
        *v = v.checked_add(offset)?;
        Some(())
    };
    let mut layout_map = std::collections::HashMap::new();
    for m in &mut p.masters {
        let m = Arc::make_mut(m);
        bump(&mut m.id.0)?;
        for l in &mut m.layouts {
            let old = l.id;
            bump(&mut l.id.0)?;
            layout_map.insert(old, l.id);
            renumber_shapes(&mut l.shapes, offset)?;
        }
        renumber_shapes(&mut m.shapes, offset)?;
    }
    p.next_id = p.next_id.checked_add(offset)?;
    Some(())
}

fn renumber_shapes(shapes: &mut [Shape], offset: u32) -> Option<()> {
    for s in shapes {
        s.id.0 = s.id.0.checked_add(offset)?;
        if let Some(kids) = s.children_mut() {
            renumber_shapes(kids, offset)?;
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn inline_markdown_becomes_runs() {
        let r = md_runs("A **bold** and *it* `code` snake_case");
        let texts: Vec<&str> = r.iter().map(|x| x.0.as_str()).collect();
        assert_eq!(texts, ["A ", "bold", " and ", "it", " ", "code", " snake_case"]);
        assert!(r.get(1).unwrap().1 && r.get(3).unwrap().2 && r.get(5).unwrap().3);
        assert_eq!(md_plain("**x** y"), "x y");
        assert_eq!(md_plain("a * b"), "a * b");
    }

    #[test]
    fn layout_names_are_forgiving() {
        assert_eq!(layout_name("Two Column"), Some("two-column"));
        assert_eq!(layout_name("KPIs"), Some("kpi"));
        assert_eq!(layout_name("whatever"), None);
    }

    #[test]
    fn overlay_table_rows_follow_the_header() {
        let rows = json!([
            ["shape", "x", "y", "w", "h", "colour", "label", "style", "points"],
            ["rect", "12", "40", "22", "8", "#2b6cb0", "TP-001", "dashed", ""],
            ["path", "0", "0", "0", "0", "C05621", "Route", "arrow fill", "5,50 12,48 20,55"],
            ["caption", "x"]
        ]);
        let mut w = vec![];
        let o = parse_overlays(&rows, &mut w);
        assert_eq!(o.len(), 2);
        assert!(o[0].dashed && o[0].arrow.is_none());
        assert_eq!(o[1].points.len(), 3);
        assert_eq!(o[1].arrow, Some("end"));
        assert!(o[1].filled);
    }

    #[test]
    fn kpis_from_bullets() {
        let k = kpis(&json!({"bullets": ["Revenue: $4.2M", "Margin — 31%", "no value"]}));
        assert_eq!(k, vec![("Revenue".to_string(), "$4.2M".to_string()), ("Margin".to_string(), "31%".to_string())]);
    }
}
