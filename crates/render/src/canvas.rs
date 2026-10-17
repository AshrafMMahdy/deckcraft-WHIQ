//! WHIQ: what the renderer draws on. [`Canvas`] is the handful of drawing operations every
//! drawing routine of this crate uses; `vello_cpu`'s `RenderContext` is one (pixels), and a
//! [`Recorder`] is another: it keeps the operations as a display list, so a slide can be written
//! as vector artwork (PDF) with its text kept as text, and replayed onto pixels where vector
//! output cannot express something (blur filters, masks, unusual blend modes).

use std::sync::Arc;

use deckcraft_fonts::FontFace;
use kurbo::{Affine, BezPath, Rect, Shape as _};
use vello_common::filter_effects::Filter;
use vello_cpu::peniko::{BlendMode, Fill as FillRule};
use vello_cpu::{Mask, PaintType, RenderContext};

/// The drawing operations of the renderer.
pub trait Canvas {
    fn set_paint_type(&mut self, paint: PaintType);
    fn set_transform(&mut self, transform: Affine);
    fn set_stroke(&mut self, stroke: kurbo::Stroke);
    fn set_fill_rule(&mut self, rule: FillRule);
    fn set_paint_transform(&mut self, transform: Affine);
    fn reset_paint_transform(&mut self);
    fn fill_path(&mut self, path: &BezPath);
    fn stroke_path(&mut self, path: &BezPath);
    fn fill_rect(&mut self, rect: &Rect);
    fn push_layer(&mut self, clip: Option<&BezPath>, blend: Option<BlendMode>, opacity: Option<f32>, mask: Option<Mask>, filter: Option<Filter>);
    fn pop_layer(&mut self);
    fn width(&self) -> u16;
    fn height(&self) -> u16;
    fn push_clip_layer(&mut self, path: &BezPath) {
        self.push_layer(Some(path), None, None, None, None);
    }
    fn push_blend_layer(&mut self, blend: BlendMode) {
        self.push_layer(None, Some(blend), None, None, None);
    }
    /// A run of glyphs placed by `m` (text space → canvas), faded by `opacity`. A canvas that keeps
    /// text as text records it and returns `true`; the default draws nothing and returns `false`,
    /// and the caller fills the glyph outlines.
    fn text_run(&mut self, _run: &deckcraft_text::GlyphRun, _m: Affine, _opacity: f64) -> bool {
        false
    }
}

impl dyn Canvas + '_ {
    /// Set the paint (a colour, gradient or image).
    pub fn set_paint(&mut self, paint: impl Into<PaintType>) {
        self.set_paint_type(paint.into());
    }
}

impl Canvas for RenderContext {
    fn set_paint_type(&mut self, paint: PaintType) {
        RenderContext::set_paint(self, paint);
    }
    fn set_transform(&mut self, transform: Affine) {
        RenderContext::set_transform(self, transform);
    }
    fn set_stroke(&mut self, stroke: kurbo::Stroke) {
        RenderContext::set_stroke(self, stroke);
    }
    fn set_fill_rule(&mut self, rule: FillRule) {
        RenderContext::set_fill_rule(self, rule);
    }
    fn set_paint_transform(&mut self, transform: Affine) {
        RenderContext::set_paint_transform(self, transform);
    }
    fn reset_paint_transform(&mut self) {
        RenderContext::reset_paint_transform(self);
    }
    fn fill_path(&mut self, path: &BezPath) {
        RenderContext::fill_path(self, path);
    }
    fn stroke_path(&mut self, path: &BezPath) {
        RenderContext::stroke_path(self, path);
    }
    fn fill_rect(&mut self, rect: &Rect) {
        RenderContext::fill_rect(self, rect);
    }
    fn push_layer(&mut self, clip: Option<&BezPath>, blend: Option<BlendMode>, opacity: Option<f32>, mask: Option<Mask>, filter: Option<Filter>) {
        RenderContext::push_layer(self, clip, blend, opacity, mask, filter);
    }
    fn pop_layer(&mut self) {
        RenderContext::pop_layer(self);
    }
    fn width(&self) -> u16 {
        RenderContext::width(self)
    }
    fn height(&self) -> u16 {
        RenderContext::height(self)
    }
}

/// The drawing state an operation was made with.
#[derive(Clone, Debug)]
pub struct DrawState {
    pub paint: PaintType,
    pub transform: Affine,
    pub paint_transform: Affine,
    pub fill_rule: FillRule,
    pub stroke: kurbo::Stroke,
}

/// A text run kept as text.
#[derive(Clone, Debug)]
pub struct TextOp {
    pub face: Arc<FontFace>,
    /// Points.
    pub size: f64,
    /// Glyph id and where its origin goes (text space → canvas, the slant included).
    pub glyphs: Vec<(u32, Affine)>,
    /// Per glyph: the character it draws (cluster head), for the text the PDF keeps.
    pub chars: Vec<(usize, char)>,
    /// The run's characters in logical order, and their range in the paragraph.
    pub text: String,
    pub range: (usize, usize),
    pub color: deckcraft_color::Rgba,
    /// Fill opacity (0: outline only).
    pub alpha: f64,
    pub outline: Option<(deckcraft_color::Rgba, f64)>,
    pub fake_bold: bool,
    pub link: bool,
}

/// One recorded operation.
#[derive(Clone, Debug)]
pub enum Op {
    Fill {
        path: BezPath,
        state: DrawState,
    },
    Stroke {
        path: BezPath,
        state: DrawState,
    },
    Push {
        clip: Option<BezPath>,
        transform: Affine,
        fill_rule: FillRule,
        blend: Option<BlendMode>,
        opacity: Option<f32>,
        mask: Option<Mask>,
        filter: Option<Filter>,
    },
    Pop,
    Text(TextOp),
}

/// A canvas that keeps what is drawn on it ([`Op`]s) instead of painting pixels.
pub struct Recorder {
    width: u16,
    height: u16,
    state: DrawState,
    pub ops: Vec<Op>,
}

impl Recorder {
    pub fn new(width: u16, height: u16) -> Self {
        Recorder {
            width,
            height,
            state: DrawState {
                paint: PaintType::Solid(vello_cpu::peniko::Color::BLACK),
                transform: Affine::IDENTITY,
                paint_transform: Affine::IDENTITY,
                fill_rule: FillRule::NonZero,
                stroke: kurbo::Stroke::new(1.0),
            },
            ops: Vec::new(),
        }
    }
}

impl Canvas for Recorder {
    fn set_paint_type(&mut self, paint: PaintType) {
        self.state.paint = paint;
    }
    fn set_transform(&mut self, transform: Affine) {
        self.state.transform = transform;
    }
    fn set_stroke(&mut self, stroke: kurbo::Stroke) {
        self.state.stroke = stroke;
    }
    fn set_fill_rule(&mut self, rule: FillRule) {
        self.state.fill_rule = rule;
    }
    fn set_paint_transform(&mut self, transform: Affine) {
        self.state.paint_transform = transform;
    }
    fn reset_paint_transform(&mut self) {
        self.state.paint_transform = Affine::IDENTITY;
    }
    fn fill_path(&mut self, path: &BezPath) {
        self.ops.push(Op::Fill { path: path.clone(), state: self.state.clone() });
    }
    fn stroke_path(&mut self, path: &BezPath) {
        self.ops.push(Op::Stroke { path: path.clone(), state: self.state.clone() });
    }
    fn fill_rect(&mut self, rect: &Rect) {
        self.ops.push(Op::Fill { path: rect.to_path(0.1), state: self.state.clone() });
    }
    fn push_layer(&mut self, clip: Option<&BezPath>, blend: Option<BlendMode>, opacity: Option<f32>, mask: Option<Mask>, filter: Option<Filter>) {
        self.ops.push(Op::Push {
            clip: clip.cloned(),
            transform: self.state.transform,
            fill_rule: self.state.fill_rule,
            blend,
            opacity,
            mask,
            filter,
        });
    }
    fn pop_layer(&mut self) {
        self.ops.push(Op::Pop);
    }
    fn width(&self) -> u16 {
        self.width
    }
    fn height(&self) -> u16 {
        self.height
    }
    fn text_run(&mut self, run: &deckcraft_text::GlyphRun, m: Affine, opacity: f64) -> bool {
        let skew = if run.fake_italic { Affine::new([1.0, 0.0, -0.2, 1.0, 0.0, 0.0]) } else { Affine::IDENTITY };
        let glyphs = run.glyphs.iter().map(|(gid, x, y)| (*gid, m * crate::glyph_place(run, *gid, *x, *y) * skew)).collect();
        self.ops.push(Op::Text(TextOp {
            face: run.face.clone(),
            size: run.size,
            glyphs,
            chars: run.cells.clone(),
            text: run.text.clone(),
            range: run.chars,
            color: run.color,
            alpha: (run.alpha * opacity).clamp(0.0, 1.0),
            outline: run.outline,
            fake_bold: run.fake_bold,
            link: run.link,
        }));
        true
    }
}

fn apply(c: &mut dyn Canvas, s: &DrawState) {
    c.set_paint_type(s.paint.clone());
    c.set_transform(s.transform);
    c.set_paint_transform(s.paint_transform);
    c.set_fill_rule(s.fill_rule);
    c.set_stroke(s.stroke.clone());
}

/// Draw recorded operations onto another canvas (pixels for the parts vector output can't hold).
/// Text is drawn as glyph outlines.
pub fn replay(ops: &[Op], c: &mut dyn Canvas) {
    for op in ops {
        match op {
            Op::Fill { path, state } => {
                apply(c, state);
                c.fill_path(path);
            }
            Op::Stroke { path, state } => {
                apply(c, state);
                c.stroke_path(path);
            }
            Op::Push { clip, transform, fill_rule, blend, opacity, mask, filter } => {
                c.set_transform(*transform);
                c.set_fill_rule(*fill_rule);
                c.push_layer(clip.as_ref(), *blend, *opacity, mask.clone(), filter.clone());
            }
            Op::Pop => c.pop_layer(),
            Op::Text(t) => replay_text(t, c),
        }
    }
}

fn replay_text(t: &TextOp, c: &mut dyn Canvas) {
    let k = t.size / t.face.upem.max(1.0);
    c.reset_paint_transform();
    for (gid, at) in &t.glyphs {
        let path = crate::glyph(&t.face, *gid);
        if path.elements().is_empty() {
            continue;
        }
        c.set_transform(*at * Affine::scale(k));
        if t.alpha > 0.0 && t.color.a > 0 {
            c.set_paint(crate::color(t.color, t.alpha));
            c.fill_path(&path);
            if t.fake_bold {
                c.set_stroke(kurbo::Stroke::new(t.face.upem * 0.025).with_join(kurbo::Join::Round));
                c.stroke_path(&path);
            }
        }
        if let Some((oc, ow)) = t.outline {
            c.set_paint(crate::color(oc, 1.0));
            c.set_stroke(kurbo::Stroke::new(ow / k.max(1e-9)).with_join(kurbo::Join::Round));
            c.stroke_path(&path);
        }
    }
}

/// Paint recorded operations onto pixels (`w` × `h`, premultiplied RGBA).
pub fn rasterize(ops: &[Op], w: u16, h: u16) -> crate::Image {
    let mut ctx = RenderContext::new_with(w, h, vello_cpu::RenderSettings { num_threads: 0, ..Default::default() });
    replay(ops, &mut ctx);
    ctx.flush();
    let mut pixels = vec![0u8; w as usize * h as usize * 4];
    if let Some(pm) = vello_cpu::PixmapMut::new(w, h, &mut pixels) {
        ctx.render(pm, &mut vello_cpu::Resources::new());
    }
    crate::Image { width: u32::from(w), height: u32::from(h), pixels }
}

/// Record slide `index` drawn at `scale` canvas units per point: background, master and layout
/// graphics, shapes. `None` when there is no such slide (or drawing it failed).
pub fn record_slide(pres: &deckcraft_model::Presentation, index: usize, scale: f64) -> Option<Recorder> {
    let slide = pres.slides.get(index)?;
    let opts = crate::RenderOpts { scale, ..Default::default() };
    let (w, h) = crate::output_size(pres, &opts);
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut rec = Recorder::new(w, h);
        let mut r = crate::Renderer::default();
        r.slide_into(&mut rec, pres, slide, index, &opts);
        rec
    }))
    .ok()
}
