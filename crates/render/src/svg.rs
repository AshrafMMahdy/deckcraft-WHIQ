//! WHIQ: slides as SVG, for a preview page (the MCP tool `render_preview`). The renderer draws the
//! slide onto a [`crate::Recorder`] exactly as it draws pixels, so the SVG shows what the app shows:
//! background, master and layout graphics, shapes and connectors, tables, charts, pictures, text.
//! Paths, colours and gradients become SVG paths and paint servers; clips and opacity become groups;
//! blur filters (shadows, glow, soft edges) `feGaussianBlur`; the Porter-Duff layers the effects use
//! (a shadow recoloured to its colour, a soft edge, a reflection's fade) become masks; blend modes
//! `mix-blend-mode`. Pictures are `<image>`s with a data URI: the picture's own bytes when a browser
//! shows them as stored, else its pixels at the size drawn. Text is the shaped glyphs of the fonts
//! the app used (so Arabic, vertical text and every font look as in the app), with the characters
//! kept as invisible text for search and copy. Nothing is painted to pixels.
//!
//! Glyph outlines and pictures are written once per page ([`SvgDoc::defs`]) and drawn with `<use>`,
//! so a logo on every slide or a font's letters cost their bytes once.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use kurbo::{Affine, BezPath, PathEl, Rect, Shape as _};
use vello_cpu::peniko;

use crate::canvas::{DrawState, Op, TextOp};

/// How pictures are embedded.
#[derive(Clone, Debug)]
pub struct PictureLimits {
    /// A picture's own bytes are embedded when at most this large; a larger one is embedded as its
    /// pixels at the size it is drawn, re-encoded.
    pub max_original: usize,
    /// Pictures' own bytes embedded per page, after which pictures are re-encoded at the size drawn.
    pub original_budget: usize,
    /// Re-encoded pictures have their sides divided by this (1: as drawn).
    pub reduce: u32,
}

impl Default for PictureLimits {
    fn default() -> Self {
        PictureLimits { max_original: 3 << 20, original_budget: 24 << 20, reduce: 1 }
    }
}

/// What a page holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SvgStats {
    /// Pictures embedded (each once).
    pub pictures: usize,
    /// … of them as their own bytes.
    pub originals: usize,
    /// … of them re-encoded from their pixels.
    pub reencoded: usize,
    /// Bytes of the pictures' data (before base64).
    pub picture_bytes: usize,
    /// Text runs written.
    pub texts: usize,
    /// Distinct glyph outlines.
    pub glyphs: usize,
}

/// The glyph outlines and pictures the slides of one page share, and the counters that keep every
/// id unique on the page.
pub struct SvgDoc {
    prefix: String,
    glyphs: HashMap<(u32, u32), Option<(String, Rect)>>,
    pictures: HashMap<usize, (Arc<vello_cpu::Pixmap>, String)>,
    /// Pictures embedded as their own bytes, by a hash of the bytes and their size, so the same
    /// picture stored twice (a logo on every slide of a file) is embedded once.
    by_content: HashMap<(u64, usize, u16, u16), String>,
    defs: String,
    next: usize,
    originals_used: usize,
    pub limits: PictureLimits,
    pub stats: SvgStats,
    // What the slide being written added, to take back when it does not fit the page.
    mark: Option<Mark>,
}

struct Mark {
    defs: usize,
    stats: SvgStats,
    originals_used: usize,
    glyphs: Vec<(u32, u32)>,
    pictures: Vec<usize>,
    contents: Vec<(u64, usize, u16, u16)>,
}

impl SvgDoc {
    /// `prefix` starts every id the page uses (letters, digits, `-`; it must differ between pages
    /// that could be shown in one document).
    pub fn new(prefix: &str, limits: PictureLimits) -> Self {
        let prefix: String = prefix.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
        SvgDoc {
            prefix: if prefix.is_empty() { "dc".into() } else { prefix },
            glyphs: HashMap::new(),
            pictures: HashMap::new(),
            by_content: HashMap::new(),
            defs: String::new(),
            next: 0,
            originals_used: 0,
            limits,
            stats: SvgStats::default(),
            mark: None,
        }
    }

    /// The shared definitions as a hidden `<svg>` to put once on the page (before or after the
    /// slides). Empty when no slide needed any.
    pub fn defs(&self) -> String {
        if self.defs.is_empty() {
            return String::new();
        }
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"0\" height=\"0\" style=\"position:absolute;width:0;height:0;overflow:hidden\" aria-hidden=\"true\" focusable=\"false\"><defs>{}</defs></svg>",
            self.defs
        )
    }

    /// Bytes of the shared definitions so far.
    pub fn defs_len(&self) -> usize {
        self.defs.len()
    }

    /// The shared definitions added after [`SvgDoc::defs_len`] was `from`.
    pub fn defs_since(&self, from: usize) -> &str {
        self.defs.get(from..).unwrap_or("")
    }

    /// Start a slide whose additions [`SvgDoc::rollback`] can take back.
    pub fn begin(&mut self) {
        self.mark = Some(Mark {
            defs: self.defs.len(),
            stats: self.stats.clone(),
            originals_used: self.originals_used,
            glyphs: vec![],
            pictures: vec![],
            contents: vec![],
        });
    }

    /// Take back what the slide since [`SvgDoc::begin`] added (it is left off the page).
    pub fn rollback(&mut self) {
        let Some(m) = self.mark.take() else { return };
        self.defs.truncate(m.defs);
        self.stats = m.stats;
        self.originals_used = m.originals_used;
        for g in m.glyphs {
            self.glyphs.remove(&g);
        }
        for p in m.pictures {
            self.pictures.remove(&p);
        }
        for c in m.contents {
            self.by_content.remove(&c);
        }
    }

    /// One slide's drawing operations (recorded on a `w` × `h` canvas) as an `<svg>` element.
    /// `tag` makes the slide's own ids unique on the page; `label` is its accessible name.
    pub fn slide(&mut self, ops: &[Op], w: u16, h: u16, tag: &str, label: &str) -> String {
        let (open, body) = self.slide_parts(ops, w, h, tag, label);
        format!("{open}{body}</svg>")
    }

    fn slide_parts(&mut self, ops: &[Op], w: u16, h: u16, tag: &str, label: &str) -> (String, String) {
        let tag: String = tag.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
        let base = format!("{}{}-", self.prefix, tag);
        let canvas = Rect::new(0.0, 0.0, f64::from(w.max(1)), f64::from(h.max(1)));
        let mut sw = SlideWriter { doc: self, base, n: 0, canvas };
        let body = sw.ops(ops);
        let open = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" class=\"art\" viewBox=\"0 0 {} {}\" role=\"img\" aria-label=\"{}\">",
            w.max(1),
            h.max(1),
            esc(label)
        );
        (open, body)
    }

    fn id(&mut self, kind: char) -> String {
        self.next += 1;
        format!("{}{kind}{}", self.prefix, self.next)
    }

    /// The id of glyph `gid` of `face` in the shared definitions and its box (font units), or
    /// `None` when it has no outline (a space).
    fn glyph(&mut self, face: &deckcraft_fonts::FontFace, gid: u32) -> Option<(String, Rect)> {
        let key = (face.id(), gid);
        if let Some(g) = self.glyphs.get(&key) {
            return g.clone();
        }
        let path = crate::glyph(face, gid);
        let found = if path.elements().is_empty() {
            None
        } else {
            let id = self.id('g');
            let _ = write!(self.defs, "<path id=\"{id}\" d=\"{}\"/>", path_d(&path, 1));
            self.stats.glyphs += 1;
            Some((id, path.bounding_box()))
        };
        self.glyphs.insert(key, found.clone());
        if let Some(m) = &mut self.mark {
            m.glyphs.push(key);
        }
        found
    }

    /// The id of a picture in the shared definitions (an `<image>` of the pixmap's size).
    fn picture(&mut self, pm: &Arc<vello_cpu::Pixmap>) -> Option<String> {
        let key = Arc::as_ptr(pm) as usize;
        if let Some((_, id)) = self.pictures.get(&key) {
            return Some(id.clone());
        }
        let (w, h) = (pm.width(), pm.height());
        if w == 0 || h == 0 {
            return None;
        }
        let original = crate::images::source_of(pm)
            .filter(|b| b.len() <= self.limits.max_original && self.originals_used.saturating_add(b.len()) <= self.limits.original_budget);
        let content_key = original.as_ref().filter(|b| browser_mime(b).is_some()).map(|b| {
            use std::hash::{Hash as _, Hasher as _};
            let mut hs = std::hash::DefaultHasher::new();
            b.as_slice().hash(&mut hs);
            (hs.finish(), b.len(), w, h)
        });
        if let Some(id) = content_key.and_then(|k| self.by_content.get(&k)).cloned() {
            self.pictures.insert(key, (pm.clone(), id.clone()));
            if let Some(m) = &mut self.mark {
                m.pictures.push(key);
            }
            return Some(id);
        }
        let (mime, data, own) = match original.as_ref().and_then(|b| browser_mime(b).map(|m| (m, b))) {
            Some((mime, b)) => (mime, b.as_slice().to_vec(), true),
            None => {
                let (mime, data) = encode_pixmap(pm, self.limits.reduce.max(1))?;
                (mime, data, false)
            }
        };
        let id = self.id('p');
        let _ = write!(
            self.defs,
            "<image id=\"{id}\" width=\"{w}\" height=\"{h}\" preserveAspectRatio=\"none\" href=\"data:{mime};base64,{}\"/>",
            base64(&data)
        );
        self.stats.pictures += 1;
        self.stats.picture_bytes += data.len();
        if own {
            self.stats.originals += 1;
            self.originals_used += data.len();
        } else {
            self.stats.reencoded += 1;
        }
        self.pictures.insert(key, (pm.clone(), id.clone()));
        if let Some(m) = &mut self.mark {
            m.pictures.push(key);
        }
        if let Some(k) = content_key.filter(|_| own) {
            self.by_content.insert(k, id.clone());
            if let Some(m) = &mut self.mark {
                m.contents.push(k);
            }
        }
        Some(id)
    }
}

/// The MIME type of picture bytes a browser shows as the app does, else `None` (re-encode them):
/// PNG, GIF, WebP, and JPEG without an EXIF rotation (the app, like PowerPoint, ignores it; a
/// browser would turn the picture).
fn browser_mime(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if b.starts_with(b"GIF8") {
        Some("image/gif")
    } else if b.len() > 12 && b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP".as_slice()) {
        Some("image/webp")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        (jpeg_orientation(b).unwrap_or(1) == 1).then_some("image/jpeg")
    } else {
        None
    }
}

/// The EXIF orientation of a JPEG (1 = as stored), if it has one.
fn jpeg_orientation(b: &[u8]) -> Option<u16> {
    let mut i = 2usize;
    // Markers up to the first scan.
    for _ in 0..64 {
        if b.get(i) != Some(&0xFF) {
            return None;
        }
        let marker = *b.get(i + 1)?;
        if marker == 0xDA || marker == 0xD9 {
            return None;
        }
        let len = usize::from(u16::from_be_bytes([*b.get(i + 2)?, *b.get(i + 3)?]));
        let seg = b.get(i + 4..i + 2 + len)?;
        if marker == 0xE1 && seg.starts_with(b"Exif\0\0") {
            let t = seg.get(6..)?;
            let le = t.starts_with(b"II");
            let u16_at = |o: usize| -> Option<u16> {
                let s = [*t.get(o)?, *t.get(o + 1)?];
                Some(if le { u16::from_le_bytes(s) } else { u16::from_be_bytes(s) })
            };
            let u32_at = |o: usize| -> Option<u32> {
                let s = [*t.get(o)?, *t.get(o + 1)?, *t.get(o + 2)?, *t.get(o + 3)?];
                Some(if le { u32::from_le_bytes(s) } else { u32::from_be_bytes(s) })
            };
            let ifd = usize::try_from(u32_at(4)?).ok()?;
            let n = usize::from(u16_at(ifd)?);
            for k in 0..n.min(512) {
                let e = ifd + 2 + k * 12;
                if u16_at(e)? == 0x0112 {
                    return u16_at(e + 8);
                }
            }
            return None;
        }
        i = i.checked_add(2 + len)?;
    }
    None
}

/// A pixmap as JPEG (opaque) or PNG bytes, its sides divided by `reduce`.
fn encode_pixmap(pm: &vello_cpu::Pixmap, reduce: u32) -> Option<(&'static str, Vec<u8>)> {
    let (w, h) = (u32::from(pm.width()), u32::from(pm.height()));
    let straight = crate::Image { width: w, height: h, pixels: pm.data_as_u8_slice().to_vec() }.to_straight();
    let mut img = image::RgbaImage::from_raw(w, h, straight)?;
    if reduce > 1 && w.max(h) / reduce >= 16 {
        img = image::imageops::resize(&img, (w / reduce).max(1), (h / reduce).max(1), image::imageops::FilterType::Triangle);
    }
    let opaque = img.pixels().all(|p| p[3] == 255);
    let mut buf = Vec::new();
    if opaque {
        let rgb = image::DynamicImage::ImageRgba8(img).to_rgb8();
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 85);
        rgb.write_with_encoder(enc).ok()?;
        Some(("image/jpeg", buf))
    } else {
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png).ok()?;
        Some(("image/png", buf))
    }
}

/// An open layer: what is drawn in it so far, where, and how it is put down when it closes.
struct Frame {
    buf: String,
    bbox: Option<Rect>,
    push: Option<Push>,
}

struct Push {
    clip: Option<(BezPath, peniko::Fill)>,
    blend: Option<peniko::BlendMode>,
    opacity: Option<f32>,
    blur: Option<f64>,
}

struct SlideWriter<'a> {
    doc: &'a mut SvgDoc,
    base: String,
    n: usize,
    canvas: Rect,
}

impl SlideWriter<'_> {
    fn id(&mut self) -> String {
        self.n += 1;
        format!("{}{}", self.base, self.n)
    }

    fn ops(&mut self, ops: &[Op]) -> String {
        let mut stack: Vec<Frame> = vec![Frame { buf: String::new(), bbox: None, push: None }];
        for op in ops {
            let drawn = match op {
                Op::Fill { path, state } => self.fill(path, state),
                Op::Stroke { path, state } => self.stroke(path, state),
                Op::Text(t) => self.text(t),
                Op::Push { clip, transform, fill_rule, blend, opacity, mask: _, filter } => {
                    let clip = clip.as_ref().map(|c| {
                        let mut p = c.clone();
                        p.apply_affine(*transform);
                        (p, *fill_rule)
                    });
                    let blur = filter.as_ref().and_then(blur_of);
                    stack.push(Frame { buf: String::new(), bbox: None, push: Some(Push { clip, blend: *blend, opacity: *opacity, blur }) });
                    None
                }
                Op::Pop => {
                    if stack.len() > 1
                        && let Some(f) = stack.pop()
                    {
                        self.close(f, &mut stack);
                    }
                    None
                }
            };
            if let (Some((s, b)), Some(top)) = (drawn, stack.last_mut()) {
                top.buf.push_str(&s);
                top.bbox = union(top.bbox, Some(b));
            }
        }
        while stack.len() > 1 {
            if let Some(f) = stack.pop() {
                self.close(f, &mut stack);
            }
        }
        stack.pop().map(|f| f.buf).unwrap_or_default()
    }

    /// Put a closed layer down onto the one below it.
    fn close(&mut self, f: Frame, stack: &mut [Frame]) {
        let Some(parent) = stack.last_mut() else { return };
        let Some(push) = f.push else {
            parent.buf.push_str(&f.buf);
            parent.bbox = union(parent.bbox, f.bbox);
            return;
        };
        if f.buf.is_empty() && !matches!(push.blend.map(|b| b.compose), Some(peniko::Compose::SrcIn | peniko::Compose::DestIn)) {
            return;
        }
        let mut bbox = f.bbox;
        let mut pre = String::new();
        let mut attrs = String::new();
        if let Some(s) = push.blur.filter(|s| *s > 0.01) {
            bbox = bbox.map(|b| b.inflate(s * 3.0 + 1.0, s * 3.0 + 1.0));
            let id = self.id();
            let r = bbox.unwrap_or(self.canvas);
            let _ = write!(
                pre,
                "<filter id=\"{id}\" filterUnits=\"userSpaceOnUse\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" color-interpolation-filters=\"sRGB\"><feGaussianBlur stdDeviation=\"{}\"/></filter>",
                n(r.x0),
                n(r.y0),
                n(r.width()),
                n(r.height()),
                n(s)
            );
            let _ = write!(attrs, " filter=\"url(#{id})\"");
        }
        if let Some((clip, rule)) = &push.clip {
            let id = self.id();
            let _ = write!(
                pre,
                "<clipPath id=\"{id}\" clipPathUnits=\"userSpaceOnUse\"><path d=\"{}\"{}/></clipPath>",
                path_d(clip, 2),
                clip_rule(*rule)
            );
            let _ = write!(attrs, " clip-path=\"url(#{id})\"");
            bbox = intersect(bbox, Some(clip.bounding_box()));
        }
        if let Some(o) = push.opacity.filter(|o| *o < 0.999) {
            let _ = write!(attrs, " opacity=\"{}\"", n(f64::from(o.max(0.0))));
        }
        let compose = push.blend.map_or(peniko::Compose::SrcOver, |b| b.compose);
        if let Some(mode) = push.blend.and_then(|b| mix_mode(b.mix)) {
            let _ = write!(attrs, " style=\"mix-blend-mode:{mode}\"");
        }
        let wrapped = if attrs.is_empty() { f.buf } else { format!("{pre}<g{attrs}>{}</g>", f.buf) };
        match compose {
            peniko::Compose::SrcIn => {
                // The layer, kept where what is below it in its parent is (a shadow's colour in
                // the shape's silhouette); what was below goes.
                if parent.buf.is_empty() {
                    return;
                }
                let region = parent.bbox;
                let below = std::mem::take(&mut parent.buf);
                let id = self.id();
                parent.buf = format!("{}<g mask=\"url(#{id})\">{wrapped}</g>", mask(&id, region.unwrap_or(self.canvas), true, &below));
                parent.bbox = intersect(region, bbox);
            }
            peniko::Compose::DestIn => {
                // What is below, kept where the layer is (a soft edge, a fade).
                if parent.buf.is_empty() {
                    return;
                }
                let below = std::mem::take(&mut parent.buf);
                let id = self.id();
                let region = bbox.unwrap_or(self.canvas);
                parent.buf = format!("{}<g mask=\"url(#{id})\">{below}</g>", mask(&id, region, true, &wrapped));
                parent.bbox = intersect(parent.bbox, bbox);
            }
            peniko::Compose::DestOut => {
                // What is below, cut where the layer is.
                if parent.buf.is_empty() {
                    return;
                }
                let below = std::mem::take(&mut parent.buf);
                let region = parent.bbox.unwrap_or(self.canvas);
                let (id, black) = (self.id(), self.id());
                let content = format!(
                    "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"#fff\"/><filter id=\"{black}\" color-interpolation-filters=\"sRGB\"><feColorMatrix type=\"matrix\" values=\"0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0\"/></filter><g filter=\"url(#{black})\">{wrapped}</g>",
                    n(region.x0),
                    n(region.y0),
                    n(region.width()),
                    n(region.height())
                );
                parent.buf = format!("{}<g mask=\"url(#{id})\">{below}</g>", mask(&id, region, false, &content));
            }
            _ => {
                parent.buf.push_str(&wrapped);
                parent.bbox = union(parent.bbox, bbox);
            }
        }
    }

    fn fill(&mut self, path: &BezPath, state: &DrawState) -> Option<(String, Rect)> {
        if path.elements().is_empty() {
            return None;
        }
        let bbox = state.transform.transform_rect_bbox(path.bounding_box());
        let tf = matrix_attr(state.transform);
        let rule = fill_rule(state.fill_rule);
        if let peniko::Brush::Image(img) = &state.paint {
            return self.image_fill(path, state, img).map(|s| (s, bbox));
        }
        let (paint, defs) = self.paint(&state.paint, state.paint_transform, "fill")?;
        Some((format!("{defs}<path d=\"{}\"{paint}{rule}{tf}/>", path_d(path, 2)), bbox))
    }

    fn stroke(&mut self, path: &BezPath, state: &DrawState) -> Option<(String, Rect)> {
        if path.elements().is_empty() {
            return None;
        }
        let s = &state.stroke;
        let half = s.width.abs() / 2.0 + 1.0;
        let bbox = state.transform.transform_rect_bbox(path.bounding_box().inflate(half, half));
        let (paint, defs) = match &state.paint {
            peniko::Brush::Image(img) => {
                let (pat, defs) = self.image_pattern(img, state.paint_transform)?;
                (format!(" stroke=\"url(#{pat})\""), defs)
            }
            p => self.paint(p, state.paint_transform, "stroke")?,
        };
        let mut a = format!(" fill=\"none\"{paint} stroke-width=\"{}\"", n(s.width));
        match s.start_cap {
            kurbo::Cap::Butt => {}
            kurbo::Cap::Round => a.push_str(" stroke-linecap=\"round\""),
            kurbo::Cap::Square => a.push_str(" stroke-linecap=\"square\""),
        }
        match s.join {
            kurbo::Join::Miter => {
                if (s.miter_limit - 4.0).abs() > 1e-6 {
                    let _ = write!(a, " stroke-miterlimit=\"{}\"", n(s.miter_limit.max(1.0)));
                }
            }
            kurbo::Join::Round => a.push_str(" stroke-linejoin=\"round\""),
            kurbo::Join::Bevel => a.push_str(" stroke-linejoin=\"bevel\""),
        }
        if !s.dash_pattern.is_empty() && s.dash_pattern.iter().all(|d| d.is_finite() && *d >= 0.0) && s.dash_pattern.iter().any(|d| *d > 0.0) {
            let dashes: Vec<String> = s.dash_pattern.iter().map(|d| n(*d)).collect();
            let _ = write!(a, " stroke-dasharray=\"{}\"", dashes.join(" "));
            if s.dash_offset.abs() > 1e-9 {
                let _ = write!(a, " stroke-dashoffset=\"{}\"", n(s.dash_offset));
            }
        }
        Some((format!("{defs}<path d=\"{}\"{a}{}/>", path_d(path, 2), matrix_attr(state.transform)), bbox))
    }

    /// ` fill="…"` (or stroke) for a colour or gradient, and the definitions it needs.
    fn paint(&mut self, p: &vello_cpu::PaintType, paint_transform: Affine, what: &str) -> Option<(String, String)> {
        match p {
            peniko::Brush::Solid(c) => {
                let [r, g, b, a] = c.to_rgba8().to_u8_array();
                if a == 0 {
                    return None;
                }
                let mut s = format!(" {what}=\"{}\"", hex(r, g, b));
                if a < 255 {
                    let _ = write!(s, " {what}-opacity=\"{}\"", n(f64::from(a) / 255.0));
                }
                Some((s, String::new()))
            }
            peniko::Brush::Gradient(g) => {
                let (id, defs) = self.gradient(g, paint_transform)?;
                Some((format!(" {what}=\"url(#{id})\""), defs))
            }
            peniko::Brush::Image(_) => None,
        }
    }

    fn gradient(&mut self, g: &peniko::Gradient, t: Affine) -> Option<(String, String)> {
        let stops: Vec<(f32, [u8; 4])> =
            g.stops.iter().map(|s| (s.offset, s.color.to_alpha_color::<peniko::color::Srgb>().to_rgba8().to_u8_array())).collect();
        if stops.is_empty() {
            return None;
        }
        let id = self.id();
        let spread = match g.extend {
            peniko::Extend::Pad => "",
            peniko::Extend::Repeat => " spreadMethod=\"repeat\"",
            peniko::Extend::Reflect => " spreadMethod=\"reflect\"",
        };
        let gt = if t == Affine::IDENTITY { String::new() } else { format!(" gradientTransform=\"{}\"", matrix(t)) };
        let mut stop_xml = String::new();
        for (o, [r, gg, b, a]) in &stops {
            let o = if o.is_finite() { o.clamp(0.0, 1.0) } else { 0.0 };
            let _ = write!(stop_xml, "<stop offset=\"{}\" stop-color=\"{}\"", n(f64::from(o)), hex(*r, *gg, *b));
            if *a < 255 {
                let _ = write!(stop_xml, " stop-opacity=\"{}\"", n(f64::from(*a) / 255.0));
            }
            stop_xml.push_str("/>");
        }
        let defs = match g.kind {
            peniko::GradientKind::Linear(l) => format!(
                "<defs><linearGradient id=\"{id}\" gradientUnits=\"userSpaceOnUse\" x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"{gt}{spread}>{stop_xml}</linearGradient></defs>",
                n(l.start.x),
                n(l.start.y),
                n(l.end.x),
                n(l.end.y)
            ),
            peniko::GradientKind::Radial(r) => format!(
                "<defs><radialGradient id=\"{id}\" gradientUnits=\"userSpaceOnUse\" fx=\"{}\" fy=\"{}\" fr=\"{}\" cx=\"{}\" cy=\"{}\" r=\"{}\"{gt}{spread}>{stop_xml}</radialGradient></defs>",
                n(r.start_center.x),
                n(r.start_center.y),
                n(f64::from(r.start_radius.max(0.0))),
                n(r.end_center.x),
                n(r.end_center.y),
                n(f64::from(r.end_radius.max(0.0)))
            ),
            // SVG has no conic gradient: the sweep's colours as a ring around its centre, the
            // middle colour of the stops (a sweep is rare: angle-style shape gradients).
            peniko::GradientKind::Sweep(s) => {
                let mid = stops.get(stops.len() / 2).map(|s| s.1).unwrap_or([0, 0, 0, 255]);
                let first = stops.first().map(|s| s.1).unwrap_or(mid);
                format!(
                    "<defs><radialGradient id=\"{id}\" gradientUnits=\"userSpaceOnUse\" cx=\"{}\" cy=\"{}\" r=\"{}\"{gt}><stop offset=\"0\" stop-color=\"{}\"/><stop offset=\"1\" stop-color=\"{}\"/></radialGradient></defs>",
                    n(s.center.x),
                    n(s.center.y),
                    n(self.canvas.width().max(self.canvas.height())),
                    hex(first[0], first[1], first[2]),
                    hex(mid[0], mid[1], mid[2])
                )
            }
        };
        Some((id, defs))
    }

    /// A tiled picture as a pattern (id, definitions).
    fn image_pattern(&mut self, img: &vello_cpu::Image, paint_transform: Affine) -> Option<(String, String)> {
        let vello_cpu::ImageSource::Pixmap(pm) = &img.image else { return None };
        let pid = self.doc.picture(pm)?;
        let id = self.id();
        let pt = if paint_transform == Affine::IDENTITY { String::new() } else { format!(" patternTransform=\"{}\"", matrix(paint_transform)) };
        Some((
            id.clone(),
            format!(
                "<defs><pattern id=\"{id}\" patternUnits=\"userSpaceOnUse\" width=\"{}\" height=\"{}\"{pt}><use href=\"#{pid}\"/></pattern></defs>",
                pm.width(),
                pm.height()
            ),
        ))
    }

    /// A path filled with a picture: the picture clipped to the path (tiles repeated).
    fn image_fill(&mut self, path: &BezPath, state: &DrawState, img: &vello_cpu::Image) -> Option<String> {
        let vello_cpu::ImageSource::Pixmap(pm) = &img.image else { return None };
        let alpha = img.sampler.alpha;
        if alpha <= 0.0 {
            return None;
        }
        let op = if alpha < 0.999 { format!(" opacity=\"{}\"", n(f64::from(alpha))) } else { String::new() };
        let tf = matrix_attr(state.transform);
        let tiles = img.sampler.x_extend != peniko::Extend::Pad || img.sampler.y_extend != peniko::Extend::Pad;
        if tiles {
            let (pat, defs) = self.image_pattern(img, state.paint_transform)?;
            return Some(format!("{defs}<path d=\"{}\" fill=\"url(#{pat})\"{}{op}{tf}/>", path_d(path, 2), fill_rule(state.fill_rule)));
        }
        let pid = self.doc.picture(pm)?;
        let whole = Rect::new(0.0, 0.0, f64::from(pm.width()), f64::from(pm.height()));
        let b = path.bounding_box();
        let is_whole = state.paint_transform == Affine::IDENTITY
            && path.elements().len() <= 6
            && (b.x0 - whole.x0).abs() < 0.01
            && (b.y0 - whole.y0).abs() < 0.01
            && (b.x1 - whole.x1).abs() < 0.01
            && (b.y1 - whole.y1).abs() < 0.01;
        if is_whole {
            return Some(format!("<use href=\"#{pid}\"{op}{tf}/>"));
        }
        let id = self.id();
        let pt = matrix_attr(state.paint_transform);
        Some(format!(
            "<g{tf}><clipPath id=\"{id}\" clipPathUnits=\"userSpaceOnUse\"><path d=\"{}\"{}/></clipPath><g clip-path=\"url(#{id})\"{op}><use href=\"#{pid}\"{pt}/></g></g>",
            path_d(path, 2),
            clip_rule(state.fill_rule)
        ))
    }

    /// A text run: its glyphs drawn from the shared outlines, its characters as invisible text.
    fn text(&mut self, t: &TextOp) -> Option<(String, Rect)> {
        if t.glyphs.is_empty() || !(t.size > 0.0 && t.size.is_finite()) {
            return None;
        }
        let upem = t.face.upem.max(1.0);
        let k = t.size / upem;
        let mut paint = String::new();
        let visible = t.alpha > 0.0 && t.color.a > 0;
        if visible {
            let _ = write!(paint, " fill=\"{}\"", hex(t.color.r, t.color.g, t.color.b));
            let o = f64::from(t.color.a) / 255.0 * t.alpha;
            if o < 0.999 {
                let _ = write!(paint, " fill-opacity=\"{}\"", n(o));
            }
        } else {
            paint.push_str(" fill=\"none\"");
        }
        match (t.outline, t.fake_bold && visible) {
            (Some((c, w)), _) => {
                let _ = write!(paint, " stroke=\"{}\" stroke-width=\"{}\" stroke-linejoin=\"round\"", hex(c.r, c.g, c.b), n(w / k.max(1e-9)));
                if c.a < 255 {
                    let _ = write!(paint, " stroke-opacity=\"{}\"", n(f64::from(c.a) / 255.0));
                }
            }
            (None, true) => {
                let _ = write!(
                    paint,
                    " stroke=\"{}\" stroke-width=\"{}\" stroke-linejoin=\"round\"",
                    hex(t.color.r, t.color.g, t.color.b),
                    n(upem * 0.025)
                );
            }
            _ => {
                if !visible {
                    return None;
                }
            }
        }
        // The run's characters in logical order, by their index in the paragraph.
        let text: Vec<char> = if t.text.is_empty() {
            let mut heads = t.chars.clone();
            heads.sort_by_key(|h| h.0);
            heads.dedup_by_key(|h| h.0);
            heads.iter().map(|h| h.1).collect()
        } else {
            t.text.chars().collect()
        };
        let first = if t.text.is_empty() { t.chars.iter().map(|c| c.0).min().unwrap_or(0) } else { t.range.0 };
        let mut glyphs = String::new();
        let mut hidden = String::new();
        let mut bbox: Option<Rect> = None;
        let count = t.glyphs.len();
        let mut i = 0;
        while i < count {
            let Some((_, a0)) = t.glyphs.get(i) else { break };
            let a0 = *a0;
            let inv = a0.inverse();
            // Glyphs on the same straight baseline as the first: only moved along it.
            let mut j = i + 1;
            while let Some((_, aj)) = t.glyphs.get(j) {
                let rel = (inv * *aj).as_coeffs();
                if !((rel[0] - 1.0).abs() < 1e-6 && rel[1].abs() < 1e-6 && rel[2].abs() < 1e-6 && (rel[3] - 1.0).abs() < 1e-6) {
                    break;
                }
                j += 1;
            }
            let _ = write!(glyphs, "<g transform=\"{}\">", matrix(a0 * Affine::scale(k)));
            let (mut x_min, mut x_max) = (f64::INFINITY, f64::NEG_INFINITY);
            let (mut c_min, mut c_max) = (usize::MAX, 0usize);
            for g in i..j {
                let Some((gid, at)) = t.glyphs.get(g) else { continue };
                let rel = (inv * *at).as_coeffs();
                let (dx, dy) = (rel[4], rel[5]);
                let adv = t.face.advance(*gid) * k;
                x_min = x_min.min(dx);
                x_max = x_max.max(dx + adv);
                if let Some((ci, _)) = t.chars.get(g) {
                    c_min = c_min.min(*ci);
                    c_max = c_max.max(*ci);
                }
                let Some((gref, gb)) = self.doc.glyph(&t.face, *gid) else { continue };
                glyphs.push_str("<use href=\"#");
                glyphs.push_str(&gref);
                glyphs.push('"');
                if dx.abs() > 1e-9 {
                    let _ = write!(glyphs, " x=\"{}\"", n1(dx / k));
                }
                if dy.abs() > 1e-9 {
                    let _ = write!(glyphs, " y=\"{}\"", n1(dy / k));
                }
                glyphs.push_str("/>");
                let place = *at * Affine::scale(k);
                bbox = union(bbox, Some(place.transform_rect_bbox(gb)));
            }
            glyphs.push_str("</g>");
            // The characters of these glyphs, invisible, over them: search and copy find the text.
            if c_min <= c_max && x_max > x_min && x_min.is_finite() {
                let from = c_min.saturating_sub(first);
                let to = (c_max.saturating_sub(first) + 1).min(text.len());
                let s: String = text.get(from.min(to)..to).unwrap_or_default().iter().collect();
                if !s.trim().is_empty() {
                    let _ = write!(
                        hidden,
                        "<text transform=\"{}\" x=\"{}\" font-size=\"{}\" textLength=\"{}\" lengthAdjust=\"spacingAndGlyphs\" fill=\"#000\" fill-opacity=\"0\" style=\"white-space:pre\">{}</text>",
                        matrix(a0),
                        n(x_min),
                        n(t.size),
                        n(x_max - x_min),
                        esc(&s)
                    );
                }
            }
            i = j;
        }
        let bbox = bbox?;
        self.doc.stats.texts += 1;
        Some((format!("<g{paint}>{glyphs}</g>{hidden}"), bbox))
    }
}

fn blur_of(f: &vello_common::filter_effects::Filter) -> Option<f64> {
    f.graph.primitives.iter().find_map(|p| match p {
        vello_common::filter_effects::FilterPrimitive::GaussianBlur { std_deviation, .. } => {
            Some(f64::from(*std_deviation)).filter(|s| s.is_finite() && *s >= 0.0)
        }
        _ => None,
    })
}

fn mask(id: &str, r: Rect, alpha: bool, content: &str) -> String {
    format!(
        "<mask id=\"{id}\" maskUnits=\"userSpaceOnUse\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"{}>{content}</mask>",
        n(r.x0 - 1.0),
        n(r.y0 - 1.0),
        n(r.width() + 2.0),
        n(r.height() + 2.0),
        if alpha { " mask-type=\"alpha\" style=\"mask-type:alpha\"" } else { "" }
    )
}

fn mix_mode(m: peniko::Mix) -> Option<&'static str> {
    use peniko::Mix::*;
    Some(match m {
        Normal => return None,
        Multiply => "multiply",
        Screen => "screen",
        Overlay => "overlay",
        Darken => "darken",
        Lighten => "lighten",
        ColorDodge => "color-dodge",
        ColorBurn => "color-burn",
        HardLight => "hard-light",
        SoftLight => "soft-light",
        Difference => "difference",
        Exclusion => "exclusion",
        Hue => "hue",
        Saturation => "saturation",
        Color => "color",
        Luminosity => "luminosity",
    })
}

fn union(a: Option<Rect>, b: Option<Rect>) -> Option<Rect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.union(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

fn intersect(a: Option<Rect>, b: Option<Rect>) -> Option<Rect> {
    match (a, b) {
        (Some(a), Some(b)) => {
            let r = a.intersect(b);
            Some(if r.width() < 0.0 || r.height() < 0.0 { Rect::new(r.x0, r.y0, r.x0, r.y0) } else { r })
        }
        (a, None) => a,
        (None, b) => b,
    }
}

fn fill_rule(r: peniko::Fill) -> &'static str {
    match r {
        peniko::Fill::NonZero => "",
        peniko::Fill::EvenOdd => " fill-rule=\"evenodd\"",
    }
}

fn clip_rule(r: peniko::Fill) -> &'static str {
    match r {
        peniko::Fill::NonZero => "",
        peniko::Fill::EvenOdd => " clip-rule=\"evenodd\"",
    }
}

fn hex(r: u8, g: u8, b: u8) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// A number with at most two decimals, no trailing zeros (0 for NaN and infinities).
fn n(v: f64) -> String {
    fmt_num(v, 100.0)
}

/// … with at most one decimal.
fn n1(v: f64) -> String {
    fmt_num(v, 10.0)
}

fn fmt_num(v: f64, k: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let r = (v * k).round() / k;
    if r == 0.0 {
        return "0".into();
    }
    let decimals = (k.log10().round().max(0.0) as usize).min(9);
    let mut s = format!("{r:.decimals$}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

fn matrix(a: Affine) -> String {
    let c = a.as_coeffs();
    format!("matrix({} {} {} {} {} {})", fmt_num(c[0], 1e5), fmt_num(c[1], 1e5), fmt_num(c[2], 1e5), fmt_num(c[3], 1e5), n(c[4]), n(c[5]))
}

fn matrix_attr(a: Affine) -> String {
    if a == Affine::IDENTITY {
        return String::new();
    }
    let c = a.as_coeffs();
    if (c[0] - 1.0).abs() < 1e-9 && c[1].abs() < 1e-9 && c[2].abs() < 1e-9 && (c[3] - 1.0).abs() < 1e-9 {
        return format!(" transform=\"translate({} {})\"", n(c[4]), n(c[5]));
    }
    format!(" transform=\"{}\"", matrix(a))
}

/// SVG path data; `decimals` 1 or 2.
fn path_d(p: &BezPath, decimals: u8) -> String {
    let f = |v: f64| if decimals == 1 { n1(v) } else { n(v) };
    let mut s = String::new();
    for el in p.elements() {
        match el {
            PathEl::MoveTo(a) => {
                let _ = write!(s, "M{} {}", f(a.x), f(a.y));
            }
            PathEl::LineTo(a) => {
                let _ = write!(s, "L{} {}", f(a.x), f(a.y));
            }
            PathEl::QuadTo(a, b) => {
                let _ = write!(s, "Q{} {} {} {}", f(a.x), f(a.y), f(b.x), f(b.y));
            }
            PathEl::CurveTo(a, b, c) => {
                let _ = write!(s, "C{} {} {} {} {} {}", f(a.x), f(a.y), f(b.x), f(b.y), f(c.x), f(c.y));
            }
            PathEl::ClosePath => s.push('Z'),
        }
    }
    s
}

/// Text for XML content and attribute values.
pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' | '\u{fffe}' | '\u{ffff}' => o.push(' '),
            c => o.push(c),
        }
    }
    o
}

/// Standard base64 with padding.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk.first().copied().unwrap_or(0), chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
        let v = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let sym = |i: u32| char::from(T.get(((v >> i) & 63) as usize).copied().unwrap_or(b'A'));
        out.push(sym(18));
        out.push(sym(12));
        out.push(if chunk.len() > 1 { sym(6) } else { '=' });
        out.push(if chunk.len() > 2 { sym(0) } else { '=' });
    }
    out
}

/// Slide `index` of `pres` as one standalone `<svg>` (its glyphs and pictures inside it), drawn at
/// `scale` canvas units per point; `None` when there is no such slide.
pub fn slide_svg(pres: &deckcraft_model::Presentation, index: usize, scale: f64) -> Option<String> {
    use crate::Canvas as _;
    let rec = crate::record_slide(pres, index, scale)?;
    let mut doc = SvgDoc::new("s", PictureLimits::default());
    let (open, body) = doc.slide_parts(&rec.ops, rec.width(), rec.height(), &format!("{index}"), &pres.slides.get(index)?.title());
    let defs = if doc.defs.is_empty() { String::new() } else { format!("<defs>{}</defs>", doc.defs) };
    Some(format!("{open}{defs}{body}</svg>"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_short() {
        assert_eq!(n(1.0), "1");
        assert_eq!(n(1.256), "1.26");
        assert_eq!(n(-0.001), "0");
        assert_eq!(n(f64::NAN), "0");
        assert_eq!(n1(12.34), "12.3");
        // A glyph's scale keeps its digits (it was cut to two decimals: chart labels drew too big).
        assert_eq!(matrix(Affine::scale(0.032718)), "matrix(0.03272 0 0 0.03272 0 0)");
        assert_eq!(base64(b"hello"), "aGVsbG8=");
        assert_eq!(base64(b"hi!"), "aGkh");
    }

    #[test]
    fn jpeg_rotation_is_read() {
        // SOI, APP1 Exif with one IFD entry: orientation 6 (big-endian).
        let mut j = vec![0xFF, 0xD8];
        let tiff: Vec<u8> = [b"MM\0\x2a".as_slice(), &[0, 0, 0, 8], &[0, 1], &[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0], &[0, 0, 0, 0]].concat();
        let seg: Vec<u8> = [b"Exif\0\0".as_slice(), &tiff].concat();
        let len = (seg.len() + 2) as u16;
        j.extend_from_slice(&[0xFF, 0xE1]);
        j.extend_from_slice(&len.to_be_bytes());
        j.extend_from_slice(&seg);
        j.extend_from_slice(&[0xFF, 0xDA, 0, 2]);
        assert_eq!(jpeg_orientation(&j), Some(6));
        assert_eq!(browser_mime(&j), None, "a rotated JPEG is re-encoded");
        assert_eq!(browser_mime(&[0xFF, 0xD8, 0xFF, 0xDA, 0, 2]), Some("image/jpeg"));
    }
}
