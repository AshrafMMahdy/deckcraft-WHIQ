//! WHIQ: slides as vector artwork. The renderer draws the slide onto a
//! [`deckcraft_render::Recorder`]; its paths, solid colours and gradients become PDF paths and
//! shadings, pictures become embedded images at print resolution, and text stays text (embedded
//! font subsets, a ToUnicode map, so it can be searched and copied). What PDF cannot express —
//! blur filters (shadows, glow, soft edges), masks, unusual blend modes — is painted to pixels for
//! just that part (an "island") and placed as an image under the vector artwork above it.

use std::collections::HashMap;
use std::sync::Arc;

use craft_pdf::write::{self, backend as krilla};
use deckcraft_render::{Op, TextOp};
use krilla::geom::{Point, Size, Transform};
use krilla::image::Image;
use krilla::num::NormalizedF32;
use krilla::paint::{
    Fill, FillRule, LineCap, LineJoin, LinearGradient, Paint, RadialGradient, SpreadMethod, Stop, Stroke, StrokeDash, SweepGradient,
};
use krilla::surface::Surface;
use krilla::text::{Font, GlyphId, KrillaGlyph};
use kurbo::{Affine, BezPath, Rect, Shape as _};
use vello_cpu::peniko;

use crate::{PdfError, Result, to_path};

fn tf(a: Affine) -> Transform {
    let c = a.as_coeffs();
    Transform::from_row(c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32, c[4] as f32, c[5] as f32)
}

fn norm(v: f32) -> NormalizedF32 {
    NormalizedF32::new(if v.is_finite() { v.clamp(0.0, 1.0) } else { 1.0 }).unwrap_or(NormalizedF32::ONE)
}

/// What a solid colour or gradient becomes in PDF: paint and opacity.
fn paint_of(p: &peniko::Brush<vello_cpu::Image, peniko::Gradient>, paint_transform: Affine) -> Option<(Paint, f32)> {
    match p {
        peniko::Brush::Solid(c) => {
            let [r, g, b, a] = c.to_rgba8().to_u8_array();
            Some((krilla::color::rgb::Color::new(r, g, b).into(), f32::from(a) / 255.0))
        }
        peniko::Brush::Gradient(g) => gradient(g, paint_transform).map(|p| (p, 1.0)),
        peniko::Brush::Image(_) => None,
    }
}

fn gradient(g: &peniko::Gradient, t: Affine) -> Option<Paint> {
    let stops: Vec<Stop> = g
        .stops
        .iter()
        .map(|s| {
            let c = s.color.to_alpha_color::<peniko::color::Srgb>().to_rgba8();
            let [r, gg, b, a] = c.to_u8_array();
            Stop { offset: norm(s.offset), color: krilla::color::rgb::Color::new(r, gg, b).into(), opacity: norm(f32::from(a) / 255.0) }
        })
        .collect();
    if stops.is_empty() {
        return None;
    }
    let spread = match g.extend {
        peniko::Extend::Pad => SpreadMethod::Pad,
        peniko::Extend::Repeat => SpreadMethod::Repeat,
        peniko::Extend::Reflect => SpreadMethod::Reflect,
    };
    let transform = tf(t);
    Some(match g.kind {
        peniko::GradientKind::Linear(l) => LinearGradient {
            x1: l.start.x as f32,
            y1: l.start.y as f32,
            x2: l.end.x as f32,
            y2: l.end.y as f32,
            transform,
            spread_method: spread,
            stops,
            anti_alias: false,
        }
        .into(),
        peniko::GradientKind::Radial(r) => RadialGradient {
            fx: r.start_center.x as f32,
            fy: r.start_center.y as f32,
            fr: r.start_radius,
            cx: r.end_center.x as f32,
            cy: r.end_center.y as f32,
            cr: r.end_radius,
            transform,
            spread_method: spread,
            stops,
            anti_alias: false,
        }
        .into(),
        peniko::GradientKind::Sweep(s) => SweepGradient {
            cx: s.center.x as f32,
            cy: s.center.y as f32,
            start_angle: s.start_angle.to_degrees(),
            end_angle: s.end_angle.to_degrees(),
            transform,
            spread_method: spread,
            stops,
            anti_alias: false,
        }
        .into(),
    })
}

fn rule(r: peniko::Fill) -> FillRule {
    match r {
        peniko::Fill::NonZero => FillRule::NonZero,
        peniko::Fill::EvenOdd => FillRule::EvenOdd,
    }
}

fn stroke_of(s: &kurbo::Stroke, paint: Paint, opacity: f32) -> Stroke {
    Stroke {
        paint,
        width: s.width as f32,
        miter_limit: s.miter_limit as f32,
        line_cap: match s.start_cap {
            kurbo::Cap::Butt => LineCap::Butt,
            kurbo::Cap::Round => LineCap::Round,
            kurbo::Cap::Square => LineCap::Square,
        },
        line_join: match s.join {
            kurbo::Join::Miter => LineJoin::Miter,
            kurbo::Join::Round => LineJoin::Round,
            kurbo::Join::Bevel => LineJoin::Bevel,
        },
        opacity: norm(opacity),
        dash: (!s.dash_pattern.is_empty())
            .then(|| StrokeDash { array: s.dash_pattern.iter().map(|d| *d as f32).collect(), offset: s.dash_offset as f32 }),
    }
}

/// A layer the PDF can hold: clip and opacity only.
fn vector_layer(blend: Option<peniko::BlendMode>, mask: &Option<vello_cpu::Mask>, filter: &Option<vello_common::filter_effects::Filter>) -> bool {
    mask.is_none() && filter.is_none() && blend.is_none_or(|b| b == peniko::BlendMode::default())
}

/// The ops from the layer opened at `ops[start]` to its close, inclusive; the index after it.
fn layer_end(ops: &[Op], start: usize) -> usize {
    let mut depth = 0usize;
    for (i, op) in ops.iter().enumerate().skip(start) {
        match op {
            Op::Push { .. } => depth += 1,
            Op::Pop => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
    }
    ops.len()
}

pub(crate) struct VectorWriter<'a> {
    pub fonts: &'a mut HashMap<u32, Font>,
    pub images: HashMap<usize, Image>,
    /// Canvas size the slide was recorded at (pixels of the islands).
    pub canvas: (u16, u16),
    /// Text kept as text (count), for tests and reports.
    pub texts: usize,
    /// Islands painted to pixels.
    pub islands: usize,
}

impl VectorWriter<'_> {
    fn font(&mut self, face: &deckcraft_fonts::FontFace) -> Result<Font> {
        if let Some(f) = self.fonts.get(&face.id()) {
            return Ok(f.clone());
        }
        let f = write::font(face.data(), face.index(), None)?;
        self.fonts.insert(face.id(), f.clone());
        Ok(f)
    }

    /// Write `ops` (canvas coordinates; the surface's transform maps canvas → page).
    pub fn ops(&mut self, s: &mut Surface, ops: &[Op]) -> Result<()> {
        // What each open layer pushed onto the surface, to pop on its close.
        let mut pushed: Vec<usize> = Vec::new();
        let mut i = 0;
        while i < ops.len() {
            let Some(op) = ops.get(i) else { break };
            match op {
                Op::Push { clip, transform, fill_rule, blend, opacity, mask, filter } => {
                    if !vector_layer(*blend, mask, filter) {
                        let end = layer_end(ops, i);
                        self.island(s, ops.get(i..end).unwrap_or_default());
                        i = end;
                        continue;
                    }
                    let mut n = 0;
                    if let Some(c) = clip {
                        let mut p = c.clone();
                        p.apply_affine(*transform);
                        if let Some(path) = to_path(&p) {
                            s.push_clip_path(&path, &rule(*fill_rule));
                            n += 1;
                        }
                    }
                    if let Some(o) = opacity.filter(|o| *o < 0.999) {
                        s.push_opacity(norm(o));
                        n += 1;
                    }
                    pushed.push(n);
                }
                Op::Pop => {
                    for _ in 0..pushed.pop().unwrap_or(0) {
                        s.pop();
                    }
                }
                Op::Fill { path, state } => self.fill(s, path, state)?,
                Op::Stroke { path, state } => match paint_of(&state.paint, state.paint_transform) {
                    Some((paint, opacity)) => {
                        let Some(p) = to_path(path) else {
                            i += 1;
                            continue;
                        };
                        s.push_transform(&tf(state.transform));
                        s.set_fill(None);
                        s.set_stroke(Some(stroke_of(&state.stroke, paint, opacity)));
                        s.draw_path(&p);
                        s.set_stroke(None);
                        s.pop();
                    }
                    None => self.island(s, std::slice::from_ref(op)),
                },
                Op::Text(t) => self.text(s, t)?,
            }
            i += 1;
        }
        for n in pushed {
            for _ in 0..n {
                s.pop();
            }
        }
        Ok(())
    }

    fn fill(&mut self, s: &mut Surface, path: &BezPath, state: &deckcraft_render::canvas::DrawState) -> Result<()> {
        let Some(p) = to_path(path) else { return Ok(()) };
        if let peniko::Brush::Image(img) = &state.paint {
            return self.image_fill(s, path, state, img);
        }
        let Some((paint, opacity)) = paint_of(&state.paint, state.paint_transform) else { return Ok(()) };
        s.push_transform(&tf(state.transform));
        s.set_stroke(None);
        s.set_fill(Some(Fill { paint, opacity: norm(opacity), rule: rule(state.fill_rule) }));
        s.draw_path(&p);
        s.set_fill(None);
        s.pop();
        Ok(())
    }

    fn pdf_image(&mut self, pm: &Arc<vello_cpu::Pixmap>) -> Image {
        let key = Arc::as_ptr(pm) as usize;
        if let Some(i) = self.images.get(&key) {
            return i.clone();
        }
        let mut rgba = pm.data_as_u8_slice().to_vec();
        for px in rgba.as_chunks_mut::<4>().0 {
            let a = u32::from(px[3]);
            if a != 0 && a != 255 {
                for c in &mut px[..3] {
                    *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        let img = Image::from_rgba8(rgba, u32::from(pm.width()), u32::from(pm.height()));
        self.images.insert(key, img.clone());
        img
    }

    /// A path filled with a picture: the picture clipped to the path (tiles repeated).
    fn image_fill(&mut self, s: &mut Surface, path: &BezPath, state: &deckcraft_render::canvas::DrawState, img: &vello_cpu::Image) -> Result<()> {
        let vello_cpu::ImageSource::Pixmap(pm) = &img.image else { return Ok(()) };
        let (w, h) = (f64::from(pm.width()), f64::from(pm.height()));
        let (Some(p), Some(size)) = (to_path(path), Size::from_wh(w as f32, h as f32)) else { return Ok(()) };
        let image = self.pdf_image(pm);
        s.push_transform(&tf(state.transform));
        s.push_clip_path(&p, &rule(state.fill_rule));
        let alpha = img.sampler.alpha;
        let faded = alpha < 0.999;
        if faded {
            s.push_opacity(norm(alpha));
        }
        let tiles = img.sampler.x_extend != peniko::Extend::Pad || img.sampler.y_extend != peniko::Extend::Pad;
        if tiles {
            // The tiles that cover the path, in picture space (bounded).
            let inv = state.paint_transform.inverse();
            let b = inv.transform_rect_bbox(path.bounding_box());
            let (c0, c1) = ((b.x0 / w).floor() as i64, (b.x1 / w).ceil() as i64);
            let (r0, r1) = ((b.y0 / h).floor() as i64, (b.y1 / h).ceil() as i64);
            if (c1 - c0).saturating_mul(r1 - r0) <= 4096 {
                for r in r0..r1 {
                    for c in c0..c1 {
                        let fx = img.sampler.x_extend == peniko::Extend::Reflect && c.rem_euclid(2) == 1;
                        let fy = img.sampler.y_extend == peniko::Extend::Reflect && r.rem_euclid(2) == 1;
                        let flip = Affine::new([
                            if fx { -1.0 } else { 1.0 },
                            0.0,
                            0.0,
                            if fy { -1.0 } else { 1.0 },
                            if fx { w } else { 0.0 },
                            if fy { h } else { 0.0 },
                        ]);
                        let at = state.paint_transform * Affine::translate((c as f64 * w, r as f64 * h)) * flip;
                        s.push_transform(&tf(at));
                        s.draw_image(image.clone(), size);
                        s.pop();
                    }
                }
            }
        } else {
            s.push_transform(&tf(state.paint_transform));
            s.draw_image(image, size);
            s.pop();
        }
        if faded {
            s.pop();
        }
        s.pop();
        s.pop();
        Ok(())
    }

    /// Paint `ops` to pixels and place them (the part PDF can't express).
    fn island(&mut self, s: &mut Surface, ops: &[Op]) {
        let (w, h) = self.canvas;
        if w == 0 || h == 0 || ops.is_empty() {
            return;
        }
        let img = deckcraft_render::rasterize(ops, w, h);
        // Only the part that was painted.
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        for (i, px) in img.pixels.as_chunks::<4>().0.iter().enumerate() {
            if px[3] != 0 {
                let (x, y) = ((i as u32) % img.width, (i as u32) / img.width);
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let (cw, ch) = (x1 - x0, y1 - y0);
        let mut rgba = Vec::with_capacity(cw as usize * ch as usize * 4);
        for y in y0..y1 {
            let from = (y * img.width + x0) as usize * 4;
            rgba.extend_from_slice(img.pixels.get(from..from + cw as usize * 4).unwrap_or_default());
        }
        for px in rgba.as_chunks_mut::<4>().0 {
            let a = u32::from(px[3]);
            if a != 0 && a != 255 {
                for c in &mut px[..3] {
                    *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        let Some(size) = Size::from_wh(cw as f32, ch as f32) else { return };
        s.push_transform(&Transform::from_translate(x0 as f32, y0 as f32));
        s.draw_image(Image::from_rgba8(rgba, cw, ch), size);
        s.pop();
        self.islands += 1;
    }

    /// A text run as real text: glyphs of the embedded font, grouped into straight lines.
    fn text(&mut self, s: &mut Surface, t: &TextOp) -> Result<()> {
        if t.glyphs.is_empty() || t.size <= 0.0 || !t.size.is_finite() {
            return Ok(());
        }
        let font = self.font(&t.face).map_err(|e| PdfError::Write(e.to_string()))?;
        // The run's characters in logical order; each glyph maps to its cluster's characters.
        let text = if t.text.is_empty() {
            let mut heads = t.chars.clone();
            heads.sort_by_key(|h| h.0);
            heads.dedup_by_key(|h| h.0);
            heads.iter().map(|h| h.1).collect()
        } else {
            t.text.clone()
        };
        let range_of = crate::glyph_text_ranges(
            &text,
            if t.text.is_empty() {
                (t.chars.iter().map(|c| c.0).min().unwrap_or(0), t.chars.iter().map(|c| c.0 + 1).max().unwrap_or(0))
            } else {
                t.range
            },
            &t.chars,
            t.glyphs.len(),
        );
        let fill = (t.alpha > 0.0 && t.color.a > 0).then(|| Fill {
            paint: krilla::color::rgb::Color::new(t.color.r, t.color.g, t.color.b).into(),
            opacity: norm((f64::from(t.color.a) / 255.0 * t.alpha) as f32),
            rule: FillRule::NonZero,
        });
        let stroke = match (t.outline, t.fake_bold, &fill) {
            (Some((c, w)), _, _) => Some(stroke_of(
                &kurbo::Stroke::new(w).with_join(kurbo::Join::Round),
                krilla::color::rgb::Color::new(c.r, c.g, c.b).into(),
                f32::from(c.a) / 255.0,
            )),
            (None, true, Some(f)) => Some(stroke_of(
                &kurbo::Stroke::new(t.size * 0.025).with_join(kurbo::Join::Round),
                f.paint.clone(),
                (f64::from(t.color.a) / 255.0 * t.alpha) as f32,
            )),
            _ => None,
        };
        if fill.is_none() && stroke.is_none() {
            return Ok(());
        }
        let size = t.size;
        let n = t.glyphs.len();
        let mut i = 0;
        while i < n {
            let Some((_, a0)) = t.glyphs.get(i) else { break };
            let inv = a0.inverse();
            // Glyphs on the same straight baseline as the first: only moved along it.
            let mut j = i + 1;
            while let Some((_, aj)) = t.glyphs.get(j) {
                let rel = (inv * *aj).as_coeffs();
                // Same size and direction; moved along the line (raised or lowered marks too).
                let straight = (rel[0] - 1.0).abs() < 1e-6 && rel[1].abs() < 1e-6 && rel[2].abs() < 1e-6 && (rel[3] - 1.0).abs() < 1e-6;
                if !straight {
                    break;
                }
                j += 1;
            }
            let xs: Vec<f64> = (i..j).filter_map(|k| t.glyphs.get(k)).map(|(_, a)| (inv * *a).as_coeffs()[4]).collect();
            let ys: Vec<f64> = (i..j).filter_map(|k| t.glyphs.get(k)).map(|(_, a)| (inv * *a).as_coeffs()[5]).collect();
            let kg: Vec<KrillaGlyph> = (i..j)
                .enumerate()
                .map(|(gi, k)| {
                    let gid = t.glyphs.get(k).map(|g| g.0).unwrap_or(0);
                    let x = xs.get(gi).copied().unwrap_or(0.0);
                    let next = xs.get(gi + 1).copied().unwrap_or_else(|| x + t.face.advance(gid) * size / t.face.upem.max(1.0));
                    let range = range_of.get(k).cloned().unwrap_or(text.len()..text.len());
                    let dy = ys.get(gi).copied().unwrap_or(0.0);
                    KrillaGlyph::new(GlyphId::new(gid), ((next - x) / size) as f32, 0.0, (-dy / size) as f32, 0.0, range, None)
                })
                .collect();
            s.push_transform(&tf(*a0));
            s.set_fill(fill.clone());
            s.set_stroke(stroke.clone());
            s.draw_glyphs(Point::from_xy(0.0, 0.0), &kg, font.clone(), &text, size as f32, false);
            s.set_fill(None);
            s.set_stroke(None);
            s.pop();
            i = j;
        }
        self.texts += 1;
        Ok(())
    }
}

/// The page box `b` (page points) as a canvas of `canvas` pixels for slide `slide_size`.
pub(crate) fn canvas_to_page(b: Rect, canvas: (u16, u16)) -> Affine {
    let k = b.width() / f64::from(canvas.0.max(1));
    let ky = b.height() / f64::from(canvas.1.max(1));
    Affine::translate((b.x0, b.y0)) * Affine::scale_non_uniform(k, ky)
}

pub(crate) fn clip_rect(w: u16, h: u16) -> Option<krilla::geom::Path> {
    to_path(&Rect::new(0.0, 0.0, f64::from(w), f64::from(h)).to_path(0.1))
}

pub(crate) fn transform_of(a: Affine) -> Transform {
    tf(a)
}
