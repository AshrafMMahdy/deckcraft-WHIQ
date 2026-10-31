//! WHIQ: the package a presentation was read from, so that saving an edited customer file keeps
//! everything the edit did not touch exactly as it was (owner's rule: editing a customer's file
//! must not lose anything it holds).
//!
//! The reader records which part each master, theme, layout, slide and notes page came from, the
//! original bytes of those parts (and of their relationships), the document properties and the
//! thumbnail, the CRC-32 and size of every entry, and a copy of the model as read. The writer
//! names the parts as they were named, and writes a part's original bytes when the model is the
//! same for that part as when it was read (`deckcraft-pptx`, `write/keep.rs`).

use std::collections::HashMap;
use std::sync::Arc;

use crate::{Diagram, LayoutId, MasterId, MediaId, Presentation, Shape, SlideId, Xfrm};

/// Where the presentation's parts came from.
#[derive(Debug, Default)]
pub struct PackageNames {
    /// Original part of each slide master, and of its theme.
    pub masters: HashMap<MasterId, String>,
    pub themes: HashMap<MasterId, String>,
    pub layouts: HashMap<LayoutId, String>,
    pub slides: HashMap<SlideId, String>,
    /// Each slide's notes page.
    pub notes: HashMap<SlideId, String>,
    /// The notes master and handout master parts, and their themes.
    pub notes_master: Option<(String, Option<String>)>,
    pub handout_master: Option<(String, Option<String>)>,
    /// The presentation part.
    pub presentation: String,
    /// Original bytes of the parts that can be written back as they were, and of their
    /// relationship parts (`…/_rels/….rels`), by part name.
    pub parts: HashMap<String, Arc<Vec<u8>>>,
    /// CRC-32 and size of every entry of the original package, by lower-case name.
    pub entries: HashMap<String, (u32, u64)>,
    /// The part each media item (picture, video, embedded object…) was read from: a save names
    /// it so again.
    pub media: HashMap<MediaId, String>,
    /// The package's thumbnail: part name, content type, bytes.
    pub thumbnail: Option<(String, String, Arc<Vec<u8>>)>,
    /// `docProps/core.xml` and `docProps/app.xml` as they were (part name, bytes).
    pub core: Option<(String, Arc<Vec<u8>>)>,
    pub app: Option<(String, Arc<Vec<u8>>)>,
}

/// The package a presentation came from and the model as it was read.
#[derive(Debug, Default)]
pub struct PackageSource {
    pub names: Arc<PackageNames>,
    /// The presentation as read (its own `source` has no `pristine`).
    pub pristine: Option<Arc<Presentation>>,
}

/// [`Presentation::source`]: `None` for a presentation that was not read from a package. It never
/// takes part in comparing presentations and is not saved in `.deckcraft` files.
#[derive(Clone, Debug, Default)]
pub struct Source(pub Option<Arc<PackageSource>>);

impl PartialEq for Source {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Source {
    pub fn get(&self) -> Option<&PackageSource> {
        self.0.as_deref()
    }

    /// The source for a new document made from this file (a deck built on a template): its
    /// masters, layouts and themes are still saved as they were, but not its document
    /// properties or thumbnail.
    pub fn for_new_document(&self) -> Source {
        let Some(s) = self.get() else { return Source(None) };
        let strip = |n: &PackageNames| PackageNames {
            masters: n.masters.clone(),
            themes: n.themes.clone(),
            layouts: n.layouts.clone(),
            slides: n.slides.clone(),
            notes: n.notes.clone(),
            notes_master: n.notes_master.clone(),
            handout_master: n.handout_master.clone(),
            presentation: n.presentation.clone(),
            parts: n.parts.clone(),
            entries: n.entries.clone(),
            media: n.media.clone(),
            thumbnail: None,
            core: None,
            app: None,
        };
        let names = Arc::new(strip(&s.names));
        let pristine = s.pristine.as_ref().map(|p| {
            let mut q = (**p).clone();
            q.source = Source(Some(Arc::new(PackageSource { names: names.clone(), pristine: None })));
            Arc::new(q)
        });
        Source(Some(Arc::new(PackageSource { names, pristine })))
    }
}

/// [`Diagram::kept`]: a SmartArt graphic's own parts as the file had them. `None` for a diagram
/// that was not read from a package. It never takes part in comparing diagrams and is not saved
/// in `.deckcraft` files.
#[derive(Clone, Debug, Default)]
pub struct DiagramKept(pub Option<Arc<DiagramSource>>);

impl PartialEq for DiagramKept {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl DiagramKept {
    pub fn get(&self) -> Option<&DiagramSource> {
        self.0.as_deref()
    }
}

/// A SmartArt graphic as it was read: its parts, and the model they were read into. While the
/// diagram is the same as read, its data model, layout, quick style and colour parts are written
/// as they were; while its shapes are too, its drawing is.
#[derive(Debug, Default)]
pub struct DiagramSource {
    /// The diagram as read (its own `kept` empty).
    pub diagram: Diagram,
    /// The graphic's shapes (the drawing), their child space and the frame, as read.
    pub children: Vec<Shape>,
    pub child: Xfrm,
    pub frame: Option<Xfrm>,
    pub data: Option<DiagramPart>,
    pub layout: Option<DiagramPart>,
    pub style: Option<DiagramPart>,
    pub colors: Option<DiagramPart>,
    pub drawing: Option<DiagramPart>,
    /// The relationship id the data model names its drawing by (`dsp:dataModelExt@relId`).
    pub drawing_ref: String,
}

/// One part of a SmartArt graphic: its name, the relationship id the graphic named it by, its
/// bytes, and its own relationships.
#[derive(Clone, Debug, Default)]
pub struct DiagramPart {
    pub name: String,
    pub rel_id: String,
    pub bytes: Arc<Vec<u8>>,
    pub rels: Vec<DiagramRel>,
}

/// A relationship of a SmartArt part: to a picture (the media item read from it) or an external
/// target (a hyperlink).
#[derive(Clone, Debug, Default)]
pub struct DiagramRel {
    pub id: String,
    pub rel_type: String,
    pub media: Option<MediaId>,
    /// The target of an external relationship.
    pub external: Option<String>,
}
