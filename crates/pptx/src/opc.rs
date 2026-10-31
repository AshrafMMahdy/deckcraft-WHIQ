//! Open Packaging Conventions: the zip container, part names, relationships and content types.
//!
//! WHIQ: the package is read from its zip lazily. A part is inflated when the importer asks for
//! it, so the file itself is never held in memory and no part is inflated that is not used.
//! Large media (video, sound, other binary parts, very large pictures) is inflated straight into a
//! spill file on disk ([`deckcraft_model::spill`]) and streamed from there into every save. The
//! in-memory limits are shares of the process's memory budget, and every part the reader cannot
//! keep is reported ([`Package::not_kept`]), never skipped in silence.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::io::{Cursor, Read, Seek, Write};
use std::sync::Arc;

use deckcraft_model::spill::{self, SpillFile};

use crate::PptxError;
use crate::xml::{self, A, W};

/// Largest single part inflated into memory at the smallest budget (XML, small media); the
/// working limit is a quarter of the memory budget when that is more ([`Limits::from_budget`]).
pub const MAX_PART: u64 = 96 * 1024 * 1024;
/// Total bytes inflated into memory for one package at the smallest budget; twice the memory
/// budget when that is more.
pub const MAX_TOTAL: u64 = 2 * 1024 * 1024 * 1024;
/// Zip entries at the smallest budget (one entry costs about a kilobyte of index).
pub const MAX_ENTRIES: usize = 50_000;
/// A part inflated to disk that expands more than this many times over its compressed size, and
/// past 256 MiB, is a zip bomb: refused (and reported), not written to disk.
pub const MAX_SPILL_RATIO: u64 = 1_000;

pub const NS_A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
pub const NS_R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
pub const NS_P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
pub const NS_C: &str = "http://schemas.openxmlformats.org/drawingml/2006/chart";
pub const NS_MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";
pub const NS_P14: &str = "http://schemas.microsoft.com/office/powerpoint/2010/main";
pub const NS_P15: &str = "http://schemas.microsoft.com/office/powerpoint/2012/main";
pub const NS_P159: &str = "http://schemas.microsoft.com/office/powerpoint/2015/09/main";
pub const NS_A14: &str = "http://schemas.microsoft.com/office/drawing/2010/main";
/// Modern (threaded) comments, Office 2018/8 schema.
pub const NS_P188: &str = "http://schemas.microsoft.com/office/powerpoint/2018/8/main";

pub const RT_BASE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/";
pub const RT_MEDIA: &str = "http://schemas.microsoft.com/office/2007/relationships/media";
/// A 3-D model part (`.glb`). Office relationship, not an ECMA-376 base type.
pub const RT_MODEL3D: &str = "http://schemas.microsoft.com/office/2017/06/relationships/model3d";
pub const RT_CORE: &str = "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties";

/// Anything a zip can be read from: the file on disk, or bytes in memory.
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// The reader's in-memory limits for one package.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// One part inflated into memory.
    pub part: u64,
    /// Every part inflated into memory, together.
    pub total: u64,
    /// Inflate work into memory, counting parts then discarded.
    pub work: u64,
    pub entries: usize,
}

impl Limits {
    /// Limits for a memory budget in bytes: a part a quarter of it, all parts twice it, never
    /// below the fixed minimums above.
    pub fn from_budget(budget: u64) -> Limits {
        let total = budget.saturating_mul(2).max(MAX_TOTAL);
        Limits {
            part: (budget / 4).max(MAX_PART),
            total,
            work: total.saturating_mul(2),
            entries: usize::try_from(budget / 1024).unwrap_or(usize::MAX).max(MAX_ENTRIES),
        }
    }
}

struct Entry {
    name: String,
    index: usize,
    size: u64,
    compressed: u64,
    /// WHIQ: the entry's CRC-32 from the zip directory (a save compares parts with it).
    crc: u32,
    data: OnceCell<Option<Vec<u8>>>,
    used: Cell<bool>,
}

/// A media part's bytes as the reader hands them over.
pub enum Payload {
    Mem(Vec<u8>),
    Disk(SpillFile),
}

/// The files of a package, keyed by normalised part name (no leading slash, lowercase).
pub struct Package {
    zip: RefCell<zip::ZipArchive<Box<dyn ReadSeek>>>,
    entries: HashMap<String, Entry>,
    limits: Limits,
    in_memory: Cell<u64>,
    work: Cell<u64>,
    not_kept: RefCell<Vec<String>>,
    content_types: OnceCell<Option<xml::Doc>>,
}

impl Package {
    /// A package in memory (tests, data handed over as bytes). The bytes are copied once.
    pub fn open(bytes: &[u8]) -> Result<Self, PptxError> {
        Self::open_reader(Box::new(Cursor::new(bytes.to_vec())))
    }
    /// A package read from `r` (the file on disk), within the process's memory budget.
    pub fn open_reader(r: Box<dyn ReadSeek>) -> Result<Self, PptxError> {
        Self::open_within(r, Limits::from_budget(spill::memory_budget()))
    }
    /// [`Package::open_reader`] with explicit limits.
    pub fn open_within(r: Box<dyn ReadSeek>, limits: Limits) -> Result<Self, PptxError> {
        let mut zip = zip::ZipArchive::new(r).map_err(|e| PptxError::NotPptx(format!("{e}")))?;
        if zip.len() > limits.entries {
            return Err(PptxError::NotPptx(format!(
                "too many entries ({}; this process's memory budget indexes up to {})",
                zip.len(),
                limits.entries
            )));
        }
        let mut entries = HashMap::new();
        let mut not_kept = vec![];
        for i in 0..zip.len() {
            let f = match zip.by_index_raw(i) {
                Ok(f) => f,
                Err(e) => {
                    not_kept.push(format!("zip entry {i} could not be read: {e}"));
                    continue;
                }
            };
            if f.is_dir() {
                continue;
            }
            let name = f.name().trim_start_matches('/').to_string();
            let e = Entry {
                name: name.clone(),
                index: i,
                size: f.size(),
                compressed: f.compressed_size(),
                crc: f.crc32(),
                data: OnceCell::new(),
                used: Cell::new(false),
            };
            entries.insert(name.to_ascii_lowercase(), e);
        }
        Ok(Package {
            zip: RefCell::new(zip),
            entries,
            limits,
            in_memory: Cell::new(0),
            work: Cell::new(0),
            not_kept: RefCell::new(not_kept),
            content_types: OnceCell::new(),
        })
    }
    fn note(&self, msg: String) {
        log::warn!("pptx: {msg}");
        let mut v = self.not_kept.borrow_mut();
        if !v.contains(&msg) {
            v.push(msg);
        }
    }
    /// Every part the reader could not keep, with the reason, in the order met.
    pub fn not_kept(&self) -> Vec<String> {
        self.not_kept.borrow().clone()
    }
    /// Inflate an entry into memory within the limits (`None`, reported, past them).
    fn inflate(&self, e: &Entry) -> Option<Vec<u8>> {
        let (part, total, work) = (self.limits.part, self.limits.total, self.limits.work);
        if e.size > part {
            self.note(format!(
                "part {} ({} MB unpacked) is larger than one part this process holds in memory ({} MB)",
                e.name,
                e.size >> 20,
                part >> 20
            ));
            return None;
        }
        if self.work.get() >= work {
            self.note(format!("part {}: the package inflated past {} MB in memory", e.name, work >> 20));
            return None;
        }
        let cap = part.min(total.saturating_sub(self.in_memory.get())).min(work.saturating_sub(self.work.get()));
        let mut zip = self.zip.borrow_mut();
        let mut f = match zip.by_index(e.index) {
            Ok(f) => f,
            Err(err) => {
                self.note(format!("part {} could not be read: {err}", e.name));
                return None;
            }
        };
        let mut data = Vec::with_capacity(e.size.min(16 * 1024 * 1024) as usize);
        let read = (&mut f).take(cap.saturating_add(1)).read_to_end(&mut data);
        self.work.set(self.work.get().saturating_add(data.len() as u64));
        match read {
            Ok(_) if data.len() as u64 > cap => {
                self.note(format!("part {} inflates past the memory left for this package ({} MB)", e.name, cap >> 20));
                None
            }
            Ok(_) => {
                self.in_memory.set(self.in_memory.get().saturating_add(data.len() as u64));
                Some(data)
            }
            Err(err) => {
                self.note(format!("part {} could not be read: {err}", e.name));
                None
            }
        }
    }
    fn entry(&self, name: &str) -> Option<&Entry> {
        let e = self.entries.get(&norm(name))?;
        e.used.set(true);
        Some(e)
    }
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        let e = self.entry(name)?;
        e.data.get_or_init(|| self.inflate(e)).as_deref()
    }
    /// The unpacked size a part declares, without inflating it.
    pub fn size_of(&self, name: &str) -> Option<u64> {
        self.entries.get(&norm(name)).map(|e| e.size)
    }
    /// A media part's bytes: inflated into a spill file on disk when it is large
    /// ([`spill::should_spill`]), else into memory. Not cached: the caller keeps them.
    pub fn media(&self, name: &str, content_type: &str) -> Option<Payload> {
        let e = self.entry(name)?;
        if let Some(Some(d)) = e.data.get() {
            return Some(Payload::Mem(d.clone()));
        }
        if !spill::should_spill(content_type, e.size) {
            return self.inflate(e).map(Payload::Mem);
        }
        if e.size > 256 << 20 && e.size / e.compressed.max(1) > MAX_SPILL_RATIO {
            self.note(format!("part {} declares {} MB from {} KB compressed: refused as a zip bomb", e.name, e.size >> 20, e.compressed >> 10));
            return None;
        }
        let mut zip = self.zip.borrow_mut();
        let mut f = match zip.by_index(e.index) {
            Ok(f) => f,
            Err(err) => {
                self.note(format!("part {} could not be read: {err}", e.name));
                return None;
            }
        };
        match SpillFile::from_reader(&mut f, e.size) {
            Ok(s) => Some(Payload::Disk(s)),
            Err(err) => {
                self.note(format!("part {} ({} MB) could not be kept on disk in {}: {err}", e.name, e.size >> 20, spill::spill_dir().display()));
                None
            }
        }
    }
    /// An XML part's bytes, when it is within the XML size limit.
    pub fn xml_bytes(&self, name: &str) -> Option<&[u8]> {
        let b = self.get(name)?;
        if b.len() as u64 > self.limits.part {
            return None;
        }
        Some(b)
    }
    /// Parse an XML part.
    pub fn xml(&self, name: &str) -> Option<xml::Doc> {
        let b = self.get(name)?;
        match xml::parse(b) {
            Ok(d) => Some(d),
            Err(e) => {
                self.note(format!("part {name} is not well-formed XML: {e}"));
                None
            }
        }
    }
    /// Parts the reader never asked for, with their unpacked sizes: `(part, bytes, referenced)`,
    /// where `referenced` tells a part some relationship points at from an orphan nothing does.
    /// Relationship parts and the content types are bookkeeping and never listed.
    pub fn unused_parts(&self) -> Vec<(String, u64, bool)> {
        let unused: Vec<(String, u64)> = self
            .entries
            .values()
            .filter(|e| !e.used.get())
            .filter(|e| {
                let n = e.name.to_ascii_lowercase();
                !n.ends_with(".rels") && n != "[content_types].xml"
            })
            .map(|e| (e.name.clone(), e.size))
            .collect();
        if unused.is_empty() {
            return vec![];
        }
        // Every internal target of every relationship part.
        let rels: Vec<String> = self.entries.values().filter(|e| e.name.to_ascii_lowercase().ends_with(".rels")).map(|e| e.name.clone()).collect();
        let mut targets = std::collections::HashSet::new();
        for r in rels {
            let part = rels_source(&r);
            if let Some(doc) = self.get(&r).and_then(|b| xml::parse(b).ok()) {
                for rel in rels_of(&doc, &part).items.into_iter().filter(|x| !x.external) {
                    targets.insert(norm(&rel.target));
                }
            }
        }
        let mut out: Vec<(String, u64, bool)> = unused.into_iter().map(|(n, b)| (n.clone(), b, targets.contains(&norm(&n)))).collect();
        out.sort();
        out
    }
    /// Relationships of a part (`ppt/slides/slide1.xml` → `ppt/slides/_rels/slide1.xml.rels`).
    pub fn rels(&self, part: &str) -> Rels {
        let rp = rels_path(part);
        let Some(doc) = self.xml(&rp) else { return Rels::default() };
        rels_of(&doc, part)
    }
    /// Content types: (extension defaults, part overrides).
    pub fn content_type(&self, part: &str) -> Option<String> {
        let doc = self.content_types.get_or_init(|| self.xml("[Content_Types].xml")).as_ref()?;
        let p = format!("/{}", norm(part));
        for o in doc.root.children_named("Override") {
            if o.attr("PartName").is_some_and(|n| n.to_ascii_lowercase() == p) {
                return o.attr("ContentType").map(String::from);
            }
        }
        let ext = part.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        doc.root
            .children_named("Default")
            .find(|d| d.attr("Extension").is_some_and(|e| e.eq_ignore_ascii_case(&ext)))
            .and_then(|d| d.attr("ContentType").map(String::from))
    }
    pub fn original_name(&self, name: &str) -> Option<&str> {
        self.entries.get(&norm(name)).map(|e| e.name.as_str())
    }
    /// WHIQ: every entry's CRC-32 and unpacked size, by lower-case name (read from the zip
    /// directory: nothing is inflated).
    pub fn digests(&self) -> HashMap<String, (u32, u64)> {
        self.entries.iter().map(|(k, e)| (k.clone(), (e.crc, e.size))).collect()
    }
}

/// The relationships in a parsed rels part, with internal targets resolved against `part`'s folder.
fn rels_of(doc: &xml::Doc, part: &str) -> Rels {
    let base = dir_of(part);
    let mut out = Rels::default();
    for r in doc.root.children_named("Relationship") {
        let (Some(id), Some(ty), Some(target)) = (r.attr("Id"), r.attr("Type"), r.attr("Target")) else { continue };
        let external = r.attr("TargetMode").is_some_and(|m| m.eq_ignore_ascii_case("external"));
        let target_raw = target.to_string();
        let target = if external { target_raw.clone() } else { resolve(&base, target) };
        out.items.push(Rel {
            id: id.to_string(),
            kind: ty.rsplit('/').next().unwrap_or(ty).to_string(),
            ty: ty.to_string(),
            target_raw,
            target,
            external,
        });
    }
    out
}

/// The relationships of `part` from the bytes of its rels part (kept parts carried by a chart).
pub fn rels_from_bytes(bytes: &[u8], part: &str) -> Rels {
    match xml::parse(bytes) {
        Ok(doc) => rels_of(&doc, part),
        Err(e) => {
            log::warn!("pptx: relationships of {part}: {e}");
            Rels::default()
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Rel {
    pub id: String,
    /// Last segment of the relationship type (`slide`, `image`, `media`…).
    pub kind: String,
    /// Full relationship type URI.
    pub ty: String,
    /// `Target` as written in the rels part (relative path or URL).
    pub target_raw: String,
    /// Absolute part name for internal targets; the URL for external ones.
    pub target: String,
    pub external: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Rels {
    pub items: Vec<Rel>,
}

impl Rels {
    pub fn get(&self, id: &str) -> Option<&Rel> {
        self.items.iter().find(|r| r.id == id)
    }
    pub fn target(&self, id: &str) -> Option<&str> {
        self.get(id).map(|r| r.target.as_str())
    }
    pub fn first_of(&self, kind: &str) -> Option<&Rel> {
        self.items.iter().find(|r| r.kind == kind)
    }
    pub fn all_of<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Rel> + 'a {
        self.items.iter().filter(move |r| r.kind == kind)
    }
}

pub fn norm(name: &str) -> String {
    name.trim_start_matches('/').replace('\\', "/").to_ascii_lowercase()
}

/// A part a chart may carry along beside its own `chartN.xml` (workbook, style, drawing, picture).
/// Chart documents themselves are rewritten, and nothing outside these folders is copied: a relationship
/// must not be able to replace `ppt/slides/slide1.xml` on save.
pub fn sidecar_part(name: &str) -> bool {
    let n = norm(name);
    let segs: Vec<&str> = n.split('/').filter(|s| !s.is_empty()).collect();
    if segs.iter().any(|s| *s == "." || *s == "..") {
        return false;
    }
    let (Some(root), Some(area)) = (segs.first().copied(), segs.get(1).copied()) else { return false };
    let file = segs.last().copied().unwrap_or("");
    // WHIQ: a chart's theme override (`ppt/theme/themeOverrideN.xml`, and its relationships) is
    // its own part; the themes of masters, notes and handouts beside it never are.
    if root == "ppt" && area == "theme" {
        return segs.len() <= 4 && file.starts_with("themeoverride");
    }
    if root != "ppt" || !matches!(area, "embeddings" | "charts" | "drawings" | "media") {
        return false;
    }
    let stem = file.strip_prefix("chart").unwrap_or("");
    let num = stem.strip_suffix(".xml.rels").or_else(|| stem.strip_suffix(".xml")).unwrap_or("");
    if stem != file && !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    true
}

pub fn dir_of(part: &str) -> String {
    let p = part.trim_start_matches('/');
    match p.rfind('/') {
        Some(i) => p.get(..i).unwrap_or("").to_string(),
        None => String::new(),
    }
}

pub fn file_of(part: &str) -> &str {
    part.rsplit('/').next().unwrap_or(part)
}

/// The part a relationships part belongs to (`ppt/slides/_rels/slide1.xml.rels` →
/// `ppt/slides/slide1.xml`, `_rels/.rels` → `""`).
pub fn rels_source(rels: &str) -> String {
    let rels = rels.trim_start_matches('/');
    let (dir, file) = rels.rsplit_once('/').unwrap_or(("", rels));
    let dir = dir.strip_suffix("_rels").unwrap_or(dir).trim_end_matches('/');
    let file = file.strip_suffix(".rels").unwrap_or(file);
    if dir.is_empty() { file.to_string() } else { format!("{dir}/{file}") }
}

pub fn rels_path(part: &str) -> String {
    let p = part.trim_start_matches('/');
    let dir = dir_of(p);
    let file = file_of(p);
    if dir.is_empty() { format!("_rels/{file}.rels") } else { format!("{dir}/_rels/{file}.rels") }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let c = b.get(i).copied().unwrap_or(0);
        if c == b'%'
            && let Some(h) = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(h);
            i += 3;
            continue;
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Resolve a relative target against a base directory into a part name.
pub fn resolve(base: &str, target: &str) -> String {
    let target = percent_decode(target);
    let target = target.split('#').next().unwrap_or("");
    let mut segs: Vec<&str> = if target.starts_with('/') { vec![] } else { base.split('/').filter(|s| !s.is_empty()).collect() };
    for s in target.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            s => segs.push(s),
        }
    }
    segs.join("/")
}

/// Relative target from the directory of `from` to part `to` (both absolute part names).
pub fn relative(from: &str, to: &str) -> String {
    let fd_s = dir_of(from);
    let fd: Vec<&str> = fd_s.split('/').filter(|s| !s.is_empty()).collect();
    let td_s = dir_of(to);
    let td: Vec<&str> = td_s.split('/').filter(|s| !s.is_empty()).collect();
    let common = fd.iter().zip(td.iter()).take_while(|(a, b)| a == b).count();
    let mut out = String::new();
    for _ in common..fd.len() {
        out.push_str("../");
    }
    for s in td.iter().skip(common) {
        out.push_str(s);
        out.push('/');
    }
    out.push_str(file_of(to));
    out
}

// ---------------------------------------------------------------------------------------------
// Writing

/// Relationships being built for one part.
#[derive(Default)]
pub struct RelsOut {
    pub items: Vec<(String, String, String, bool)>,
}

impl RelsOut {
    /// Add (or reuse) a relationship; `kind` is a full type URI or a suffix of the standard base.
    pub fn add(&mut self, kind: &str, target: &str, external: bool) -> String {
        let ty = if kind.starts_with("http") { kind.to_string() } else { format!("{RT_BASE}{kind}") };
        if let Some((id, ..)) = self.items.iter().find(|(_, t, tg, e)| *t == ty && tg == target && *e == external) {
            return id.clone();
        }
        let id = self.fresh_id();
        self.items.push((id.clone(), ty, target.to_string(), external));
        id
    }
    /// Always add a new relationship even if an identical one exists.
    pub fn add_new(&mut self, kind: &str, target: &str, external: bool) -> String {
        let ty = if kind.starts_with("http") { kind.to_string() } else { format!("{RT_BASE}{kind}") };
        let id = self.fresh_id();
        self.items.push((id.clone(), ty, target.to_string(), external));
        id
    }
    /// WHIQ: add a relationship under `id` when that id is free (kept XML may point at it), else
    /// under a fresh one. Returns the id used.
    pub fn add_with_id(&mut self, id: &str, kind: &str, target: &str) -> String {
        let free = !id.is_empty() && !self.items.iter().any(|(i, ..)| i == id);
        let ty = if kind.starts_with("http") || kind.contains(':') { kind.to_string() } else { format!("{RT_BASE}{kind}") };
        let id = if free { id.to_string() } else { self.fresh_id() };
        self.items.push((id.clone(), ty, target.to_string(), false));
        id
    }
    /// `rId<n>` not yet used (ids kept from a file may sit anywhere in the sequence).
    fn fresh_id(&self) -> String {
        let mut n = self.items.len() + 1;
        while self.items.iter().any(|(i, ..)| *i == format!("rId{n}")) {
            n += 1;
        }
        format!("rId{n}")
    }
    pub fn to_xml(&self) -> Vec<u8> {
        let mut w = W::new();
        w.open("Relationships", A::new().a("xmlns", "http://schemas.openxmlformats.org/package/2006/relationships"));
        for (id, ty, target, ext) in &self.items {
            let mut a = A::new().a("Id", id).a("Type", ty).a("Target", target);
            if *ext {
                a = a.a("TargetMode", "External");
            }
            w.empty("Relationship", a);
        }
        w.close("Relationships");
        w.finish()
    }
}

/// WHIQ: the bytes of a part being written: made for this save, shared with the model, or
/// streamed from a spill file on disk.
pub enum PartData {
    Owned(Vec<u8>),
    Shared(Arc<Vec<u8>>),
    Disk(Arc<SpillFile>),
}

impl PartData {
    fn len(&self) -> u64 {
        match self {
            PartData::Owned(v) => v.len() as u64,
            PartData::Shared(v) => v.len() as u64,
            PartData::Disk(s) => s.len(),
        }
    }
    fn write_to(&self, w: &mut dyn Write) -> std::io::Result<()> {
        match self {
            PartData::Owned(v) => w.write_all(v),
            PartData::Shared(v) => w.write_all(v),
            PartData::Disk(s) => s.copy_to(w).map(|_| ()),
        }
    }
}

impl From<Vec<u8>> for PartData {
    fn from(v: Vec<u8>) -> Self {
        PartData::Owned(v)
    }
}

impl From<Arc<Vec<u8>>> for PartData {
    fn from(v: Arc<Vec<u8>>) -> Self {
        PartData::Shared(v)
    }
}

/// A package being written.
pub struct PackageOut {
    files: Vec<(String, PartData, bool)>,
    defaults: Vec<(String, String)>,
    overrides: Vec<(String, String)>,
}

impl Default for PackageOut {
    fn default() -> Self {
        PackageOut {
            files: vec![],
            defaults: vec![
                ("rels".into(), "application/vnd.openxmlformats-package.relationships+xml".into()),
                ("xml".into(), "application/xml".into()),
            ],
            overrides: vec![],
        }
    }
}

impl PackageOut {
    /// Add an XML part with its content type override.
    /// WHIQ: is there a part named `name` (case-insensitively) already?
    pub fn has(&self, name: &str) -> bool {
        self.files.iter().any(|(n, ..)| n.eq_ignore_ascii_case(name))
    }
    pub fn xml_part(&mut self, name: &str, content_type: &str, data: Vec<u8>) {
        self.overrides.push((format!("/{name}"), content_type.to_string()));
        self.files.push((name.to_string(), data.into(), true));
    }
    pub fn rels(&mut self, part: &str, rels: &RelsOut) {
        if rels.items.is_empty() {
            return;
        }
        self.files.push((rels_path(part), rels.to_xml().into(), true));
    }
    /// WHIQ: the bytes of a part made for this save (`None` for one streamed from disk).
    pub fn bytes_of(&self, name: &str) -> Option<&[u8]> {
        self.files.iter().find(|(n, ..)| n.eq_ignore_ascii_case(name)).and_then(|(_, d, _)| match d {
            PartData::Owned(v) => Some(v.as_slice()),
            PartData::Shared(v) => Some(v.as_slice()),
            PartData::Disk(_) => None,
        })
    }
    /// WHIQ: the names of the parts written so far.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.files.iter().map(|(n, ..)| n.as_str())
    }
    /// WHIQ: put `data` in place of the part `name` (added when there is none).
    pub fn replace(&mut self, name: &str, data: impl Into<PartData>) {
        let data = data.into();
        match self.files.iter_mut().find(|(n, ..)| n.eq_ignore_ascii_case(name)) {
            Some(f) => f.1 = data,
            None => self.files.push((name.to_string(), data, true)),
        }
    }
    /// WHIQ: a part with its own content type override (a part kept from the source file).
    pub fn typed(&mut self, name: &str, content_type: &str, data: impl Into<PartData>, compress: bool) {
        self.overrides.push((format!("/{name}"), content_type.to_string()));
        self.files.push((name.to_string(), data.into(), compress));
    }
    /// Add a part with no content type of its own (a relationships part).
    pub fn raw(&mut self, name: &str, data: Vec<u8>) {
        self.files.push((name.to_string(), data.into(), true));
    }
    /// Add a binary part; its extension gets a Default content type.
    pub fn binary(&mut self, name: &str, content_type: &str, data: impl Into<PartData>) {
        let ext = name.rsplit('.').next().unwrap_or("bin").to_ascii_lowercase();
        if !self.defaults.iter().any(|(e, _)| *e == ext) {
            self.defaults.push((ext, content_type.to_string()));
        } else if !self.defaults.iter().any(|(e, c)| *e == ext && c == content_type) {
            self.overrides.push((format!("/{name}"), content_type.to_string()));
        }
        self.files.push((name.to_string(), data.into(), false));
    }
    /// The package's bytes. WHIQ: `on(stage, done, total)` is told the bytes compressed so far;
    /// `false` stops it.
    pub fn finish_with(self, on: &mut dyn FnMut(&'static str, usize, usize) -> bool) -> Result<Vec<u8>, PptxError> {
        let mut cur = Cursor::new(Vec::new());
        self.finish_into(&mut cur, on)?;
        Ok(cur.into_inner())
    }
    /// WHIQ: write the package into `out` (a file: nothing of it is held in memory beyond the
    /// part being compressed; spilled media streams from disk). Returns the bytes written.
    pub fn finish_into<O: Write + Seek>(self, out: &mut O, on: &mut dyn FnMut(&'static str, usize, usize) -> bool) -> Result<u64, PptxError> {
        let total: u64 = self.files.iter().map(|f| f.1.len()).sum();
        let total_us = usize::try_from(total).unwrap_or(usize::MAX);
        let mut done = 0u64;
        let mut ct = W::new();
        ct.open("Types", A::new().a("xmlns", "http://schemas.openxmlformats.org/package/2006/content-types"));
        for (e, c) in &self.defaults {
            ct.empty("Default", A::new().a("Extension", e).a("ContentType", c));
        }
        for (p, c) in &self.overrides {
            ct.empty("Override", A::new().a("PartName", p).a("ContentType", c));
        }
        ct.close("Types");
        let start = out.stream_position().unwrap_or(0);
        let mut zw = zip::ZipWriter::new(&mut *out);
        let deflate = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        let err = |e: &dyn std::fmt::Display| PptxError::Write(e.to_string());
        zw.start_file("[Content_Types].xml", deflate).map_err(|e| err(&e))?;
        zw.write_all(&ct.finish()).map_err(|e| err(&e))?;
        let mut written = std::collections::HashSet::new();
        for (name, data, compress) in &self.files {
            // Zip entry names must be unique: a second part under a name already written is
            // dropped rather than producing a package readers reject or resolve differently.
            if !written.insert(norm(name)) {
                log::warn!("pptx: part {name} written twice; keeping the first");
                continue;
            }
            if !on("compressing the package", usize::try_from(done).unwrap_or(usize::MAX), total_us) {
                return Err(PptxError::Write("cancelled".into()));
            }
            done = done.saturating_add(data.len());
            let large = data.len() >= u64::from(u32::MAX) / 2;
            let opts = if *compress { deflate } else { stored }.large_file(large);
            zw.start_file(name.as_str(), opts).map_err(|e| err(&e))?;
            data.write_to(&mut zw).map_err(|e| err(&format!("{name}: {e}")))?;
        }
        zw.finish().map_err(|e| err(&e))?;
        let end = out.stream_position().unwrap_or(start);
        Ok(end.saturating_sub(start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zip_of(entries: &[(&str, usize)]) -> Vec<u8> {
        let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, len) in entries {
            zw.start_file(*name, o).unwrap();
            zw.write_all(&vec![b'x'; *len]).unwrap();
        }
        zw.finish().unwrap().into_inner()
    }

    #[test]
    fn discarded_parts_count_against_the_inflate_budget() {
        // Kept bytes cap at 100: b, c and d are inflated past what's left and discarded. That
        // work is charged, so after d the budget (130) is spent and e is not read, although it
        // would have fit.
        let z = zip_of(&[("a", 80), ("b", 80), ("c", 80), ("d", 80), ("e", 5)]);
        let lim = |work| Limits { part: 1000, total: 100, work, entries: 100 };
        let p = Package::open_within(Box::new(Cursor::new(z.clone())), lim(130)).unwrap();
        assert!(p.get("a").is_some());
        assert!(p.get("b").is_none() && p.get("c").is_none() && p.get("d").is_none());
        assert!(p.not_kept().iter().any(|m| m.contains("part b")), "{:?}", p.not_kept());
        assert!(p.get("e").is_none(), "the inflate budget stops reading");
        // With room to spare the same small part is kept.
        let p = Package::open_within(Box::new(Cursor::new(z)), lim(10_000)).unwrap();
        assert!(p.get("e").is_some());
    }

    #[test]
    fn xml_bytes_applies_the_xml_limit() {
        let z = zip_of(&[("small.xml", 10)]);
        let p = Package::open(&z).unwrap();
        assert_eq!(p.xml_bytes("small.xml").map(<[u8]>::len), Some(10));
        assert!(p.xml_bytes("missing.xml").is_none());
    }

    #[test]
    fn path_resolution() {
        assert_eq!(resolve("ppt/slides", "../media/image1.png"), "ppt/media/image1.png");
        assert_eq!(resolve("ppt", "slides/slide1.xml"), "ppt/slides/slide1.xml");
        assert_eq!(resolve("ppt/slides", "/ppt/media/a%20b.png"), "ppt/media/a b.png");
        assert_eq!(resolve("", "../../../x.xml"), "x.xml");
        assert_eq!(rels_path("ppt/presentation.xml"), "ppt/_rels/presentation.xml.rels");
        assert_eq!(rels_path("[Content_Types].xml"), "_rels/[Content_Types].xml.rels");
        assert_eq!(relative("ppt/slides/slide1.xml", "ppt/media/image1.png"), "../media/image1.png");
        assert_eq!(relative("ppt/presentation.xml", "ppt/slides/slide1.xml"), "slides/slide1.xml");
        assert_eq!(relative("ppt/slides/slide1.xml", "ppt/slides/slide2.xml"), "slide2.xml");
    }

    #[test]
    fn sidecar_parts_stay_inside_chart_folders() {
        assert!(sidecar_part("ppt/embeddings/Microsoft_Excel_Worksheet1.xlsx"));
        assert!(sidecar_part("ppt/charts/style1.xml"));
        assert!(sidecar_part("ppt/drawings/chartDrawing1.xml"));
        assert!(sidecar_part("ppt/media/image1.png"));
        assert!(sidecar_part("ppt/theme/themeOverride1.xml"));
        assert!(sidecar_part("ppt/theme/_rels/themeOverride1.xml.rels"));
        assert!(!sidecar_part("ppt/theme/theme1.xml"));
        assert!(!sidecar_part("ppt/theme/_rels/theme1.xml.rels"));
        assert!(!sidecar_part("ppt/charts/chart1.xml"));
        assert!(!sidecar_part("ppt/charts/_rels/chart1.xml.rels"));
        assert!(!sidecar_part("ppt/slides/slide1.xml"));
        assert!(!sidecar_part("ppt/embeddings/../../ppt/slides/slide1.xml"));
        assert!(!sidecar_part("/etc/passwd"));
    }
}
