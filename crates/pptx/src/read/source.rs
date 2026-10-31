//! WHIQ: what the reader records of the package it read, for a save that keeps what an edit did
//! not touch (`deckcraft_model::source`, `write/keep.rs`): the part each master, theme, layout,
//! slide and notes page came from, those parts' bytes and relationships, the document properties,
//! the thumbnail, and every entry's CRC-32.

use std::sync::Arc;

use deckcraft_model::source::{PackageNames, PackageSource, Source};
use deckcraft_model::{LayoutId, MasterId, Presentation, SlideId};

use crate::opc::{self, Package};

/// Original bytes kept per package: past this the remaining parts are not kept (they are then
/// always written from the model, as before).
const KEEP_BUDGET: usize = 256 << 20;

pub(crate) struct SourceBuilder {
    pub names: PackageNames,
    kept: usize,
}

impl SourceBuilder {
    pub fn new(pkg: &Package, pres_path: &str) -> Self {
        let mut b = SourceBuilder { names: PackageNames::default(), kept: 0 };
        b.names.presentation = name_of(pkg, pres_path);
        b.names.entries = pkg.digests();
        b.keep(pkg, pres_path);
        b
    }

    /// Keep a part's bytes and its relationships' bytes.
    pub fn keep(&mut self, pkg: &Package, part: &str) -> String {
        let name = name_of(pkg, part);
        for n in [name.clone(), opc::rels_path(&name)] {
            if self.names.parts.contains_key(&n) {
                continue;
            }
            let Some(b) = pkg.get(&n) else { continue };
            if self.kept.saturating_add(b.len()) > KEEP_BUDGET {
                continue;
            }
            self.kept += b.len();
            self.names.parts.insert(n, Arc::new(b.to_vec()));
        }
        name
    }

    pub fn master(&mut self, pkg: &Package, id: MasterId, part: &str) {
        let name = self.keep(pkg, part);
        self.names.masters.insert(id, name);
        if let Some(t) = pkg.rels(part).first_of("theme") {
            let tname = self.keep(pkg, &t.target);
            self.names.themes.insert(id, tname);
        }
    }

    pub fn layout(&mut self, pkg: &Package, id: LayoutId, part: &str) {
        let name = self.keep(pkg, part);
        self.names.layouts.insert(id, name);
    }

    pub fn slide(&mut self, pkg: &Package, id: SlideId, part: &str, notes: Option<&str>) {
        let name = self.keep(pkg, part);
        // Its charts too: an unchanged chart is saved as it was.
        for r in pkg.rels(part).all_of("chart") {
            let t = r.target.clone();
            self.keep(pkg, &t);
        }
        self.names.slides.insert(id, name);
        if let Some(n) = notes {
            let nname = self.keep(pkg, n);
            self.names.notes.insert(id, nname);
        }
    }

    /// A notes or handout master and its theme.
    pub fn other_master(&mut self, pkg: &Package, part: &str) -> (String, Option<String>) {
        let name = self.keep(pkg, part);
        let theme = pkg.rels(part).first_of("theme").map(|t| t.target.clone()).map(|t| self.keep(pkg, &t));
        (name, theme)
    }

    /// The document properties, the thumbnail and the presentation-level property parts; then
    /// the source with the model as it is now as its pristine copy.
    pub fn finish(mut self, pkg: &Package, pres_path: &str, p: &mut Presentation) {
        let root = pkg.rels("");
        for r in root.items.iter().filter(|r| !r.external) {
            let get = |n: &str| pkg.get(n).map(|b| Arc::new(b.to_vec()));
            match r.kind.as_str() {
                "core-properties" => self.names.core = get(&r.target).map(|b| (name_of(pkg, &r.target), b)),
                "extended-properties" => self.names.app = get(&r.target).map(|b| (name_of(pkg, &r.target), b)),
                "thumbnail" => {
                    let ct = pkg.content_type(&r.target).unwrap_or_else(|| "image/jpeg".into());
                    self.names.thumbnail = get(&r.target).map(|b| (name_of(pkg, &r.target), ct, b));
                }
                _ => {}
            }
        }
        let pres_rels = pkg.rels(pres_path);
        for kind in ["presProps", "viewProps", "tableStyles"] {
            if let Some(r) = pres_rels.first_of(kind) {
                let t = r.target.clone();
                self.keep(pkg, &t);
            }
        }
        p.source = Source(Some(Arc::new(PackageSource { names: Arc::new(self.names), pristine: None })));
        snapshot(p);
    }
}

/// The part's name as the package spells it.
fn name_of(pkg: &Package, part: &str) -> String {
    pkg.original_name(part).map(String::from).unwrap_or_else(|| part.trim_start_matches('/').to_string())
}

/// Take the presentation as it is now as the source's model "as read": parts whose model is
/// still the same when saved are written as they were in the file. Called after the reader, and
/// again after a repair of the model, so a repair alone does not count as an edit.
pub fn snapshot(p: &mut Presentation) {
    let Some(names) = p.source.get().map(|s| s.names.clone()) else { return };
    diagrams_as_read(p);
    let mut pristine = p.clone();
    pristine.source = Source(Some(Arc::new(PackageSource { names: names.clone(), pristine: None })));
    p.source = Source(Some(Arc::new(PackageSource { names, pristine: Some(Arc::new(pristine)) })));
}

/// Every SmartArt graphic read from the file takes its shapes as they are now as its shapes "as
/// read" (the reader's later passes and the repair adjust them; neither is an edit).
fn diagrams_as_read(p: &mut Presentation) {
    use deckcraft_model::{Shape, ShapeKind};
    fn stale(shapes: &[Shape], depth: usize) -> bool {
        depth < 32
            && shapes.iter().any(|s| match (&s.kind, &s.diagram) {
                (ShapeKind::Group { children, child }, Some(d)) => d.kept.get().is_some_and(|k| {
                    let mut now = d.clone();
                    now.kept = Default::default();
                    k.diagram != now || k.children != *children || k.child != *child || k.frame != s.xfrm
                }),
                (ShapeKind::Group { children, .. }, None) => stale(children, depth + 1),
                _ => false,
            })
    }
    fn refresh(shapes: &mut [Shape], depth: usize) {
        if depth >= 32 {
            return;
        }
        for s in shapes {
            if let ShapeKind::Group { children, .. } = &mut s.kind
                && s.diagram.is_none()
            {
                refresh(children, depth + 1);
                continue;
            }
            let Some(k) = s.diagram.as_ref().and_then(|d| d.kept.get()) else { continue };
            let mut fresh = deckcraft_model::source::DiagramSource {
                data: k.data.clone(),
                layout: k.layout.clone(),
                style: k.style.clone(),
                colors: k.colors.clone(),
                drawing: k.drawing.clone(),
                drawing_ref: k.drawing_ref.clone(),
                ..Default::default()
            };
            crate::read::shapes::as_read(&mut fresh, s);
            if let Some(d) = &mut s.diagram {
                d.kept = deckcraft_model::source::DiagramKept(Some(Arc::new(fresh)));
            }
        }
    }
    for s in p.slides.iter_mut().filter(|s| stale(&s.shapes, 0)) {
        refresh(&mut Arc::make_mut(s).shapes, 0);
    }
    for m in p.masters.iter_mut() {
        if stale(&m.shapes, 0) || m.layouts.iter().any(|l| stale(&l.shapes, 0)) {
            let m = Arc::make_mut(m);
            refresh(&mut m.shapes, 0);
            for l in &mut m.layouts {
                refresh(&mut l.shapes, 0);
            }
        }
    }
}
