//! WHIQ: saving a presentation read from a file keeps everything an edit did not touch exactly as
//! it was (owner's rule: editing a customer's file must not lose anything it holds).
//!
//! - Parts keep the names they had ([`Namer`]), so the file's own relationships stay valid.
//! - A part is written as its original bytes, with its original relationships, when the model is
//!   the same for it as when it was read: the writer's output for the part now equals its output
//!   for the model as read (`Source::pristine`), and every part its original relationships point
//!   at is in the package as the same object (a master, layout, slide, notes page or theme, which
//!   keep their names) or with the same bytes ([`pass_through`]).
//! - A notes page whose text was edited keeps its other shapes: only the body placeholder's text
//!   is replaced in the original XML ([`splice_notes`]).
//! - `docProps/core.xml` and `docProps/app.xml` are kept as they were, with the fields the model
//!   changed set, `modified` set to now and `lastModifiedBy` to the caller ([`core_xml`],
//!   [`app_xml`]); the thumbnail is kept.

use std::collections::{HashMap, HashSet};

use deckcraft_model::source::PackageNames;
use deckcraft_model::{Presentation, Properties};

use crate::opc::{self, PackageOut};

/// Part names for a save: the name each master, theme, layout, slide and notes page had in the
/// file it was read from, and fresh names (never one of those) for the rest.
pub(crate) struct Namer {
    reserved: HashSet<String>,
    used: HashSet<String>,
}

impl Namer {
    pub fn new(p: &Presentation) -> Namer {
        let mut reserved = HashSet::new();
        if let Some(n) = p.source.get().map(|s| &s.names) {
            for m in &p.masters {
                reserved.extend(n.masters.get(&m.id).map(|s| s.to_ascii_lowercase()));
                reserved.extend(n.themes.get(&m.id).map(|s| s.to_ascii_lowercase()));
                for l in &m.layouts {
                    reserved.extend(n.layouts.get(&l.id).map(|s| s.to_ascii_lowercase()));
                }
            }
            for s in &p.slides {
                reserved.extend(n.slides.get(&s.id).map(|x| x.to_ascii_lowercase()));
                reserved.extend(n.notes.get(&s.id).map(|x| x.to_ascii_lowercase()));
                deckcraft_model::walk(&s.shapes, &mut |sh, _| {
                    if let deckcraft_model::ShapeKind::Chart(c) = &sh.kind
                        && !c.kept.part.is_empty()
                    {
                        reserved.insert(c.kept.part.to_ascii_lowercase());
                    }
                });
            }
            for (part, theme) in [&n.notes_master, &n.handout_master].into_iter().flatten() {
                reserved.insert(part.to_ascii_lowercase());
                reserved.extend(theme.as_ref().map(|t| t.to_ascii_lowercase()));
            }
        }
        Namer { reserved, used: HashSet::new() }
    }

    /// Take `name` when no part of the file's and no part written so far has it.
    pub fn claim(&mut self, name: &str) -> bool {
        let l = name.to_ascii_lowercase();
        !self.reserved.contains(&l) && self.used.insert(l)
    }

    /// `original` when it is free, else `{prefix}{n}.xml` for the smallest free `n`.
    pub fn name(&mut self, original: Option<&String>, prefix: &str) -> String {
        if let Some(o) = original
            && self.used.insert(o.to_ascii_lowercase())
        {
            return o.clone();
        }
        let mut n = 1u32;
        loop {
            let c = format!("{prefix}{n}.xml");
            let l = c.to_ascii_lowercase();
            if !self.reserved.contains(&l) && self.used.insert(l) {
                return c;
            }
            n += 1;
        }
    }
}

/// Parts that are the same object under the same name in the file and in the save.
fn structural(n: &PackageNames) -> HashSet<String> {
    let mut s: HashSet<String> = HashSet::new();
    s.extend(n.masters.values().map(|x| x.to_ascii_lowercase()));
    s.extend(n.themes.values().map(|x| x.to_ascii_lowercase()));
    s.extend(n.layouts.values().map(|x| x.to_ascii_lowercase()));
    s.extend(n.slides.values().map(|x| x.to_ascii_lowercase()));
    s.extend(n.notes.values().map(|x| x.to_ascii_lowercase()));
    for (part, theme) in [&n.notes_master, &n.handout_master].into_iter().flatten() {
        s.insert(part.to_ascii_lowercase());
        s.extend(theme.as_ref().map(|t| t.to_ascii_lowercase()));
    }
    s.insert(n.presentation.to_ascii_lowercase());
    s
}

/// CRC-32 (IEEE, as zip stores it).
fn crc32(b: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        t
    });
    !b.iter().fold(!0u32, |c, &x| t.get(((c ^ u32::from(x)) & 0xFF) as usize).copied().unwrap_or(0) ^ (c >> 8))
}

/// Write the original bytes of every part the edit did not change (see the module notes).
/// `now` is the package written for the presentation, `then` the one the writer makes for the
/// presentation as read. Returns the parts kept as they were.
pub(crate) fn pass_through(now: &mut PackageOut, then: &PackageOut, n: &PackageNames) -> Vec<String> {
    let same = |name: &str| -> bool {
        let rels = opc::rels_path(name);
        now.bytes_of(name).is_some() && now.bytes_of(name) == then.bytes_of(name) && now.bytes_of(&rels) == then.bytes_of(&rels)
    };
    let mut keep: HashSet<String> =
        n.parts.keys().filter(|k| !k.to_ascii_lowercase().ends_with(".rels")).filter(|k| same(k)).map(|k| k.to_ascii_lowercase()).collect();
    let structural = structural(n);
    let written: HashMap<String, String> = now.names().map(|x| (x.to_ascii_lowercase(), x.to_string())).collect();
    // The original relationships of each candidate, resolved.
    let targets: HashMap<String, Vec<String>> = keep
        .iter()
        .map(|k| {
            let orig = n.parts.keys().find(|x| x.eq_ignore_ascii_case(k)).cloned().unwrap_or_else(|| k.clone());
            let rels = n.parts.get(&opc::rels_path(&orig)).map(|b| opc::rels_from_bytes(b, &orig)).unwrap_or_default();
            (k.clone(), rels.items.into_iter().filter(|r| !r.external).map(|r| opc::norm(&r.target)).collect())
        })
        .collect();
    // A chart's relationships may reach only its own sidecar parts (workbook, style, drawing,
    // picture): one that points elsewhere (at a slide…) is written from the model, which drops it.
    keep.retain(|k| !k.starts_with("ppt/charts/") || targets.get(k).into_iter().flatten().all(|t| opc::sidecar_part(t)));
    loop {
        let before = keep.len();
        let snapshot = keep.clone();
        keep.retain(|k| {
            targets.get(k).into_iter().flatten().all(|t| {
                if !written.contains_key(t) {
                    return false;
                }
                if snapshot.contains(t) || structural.contains(t) {
                    return true;
                }
                // Anything else must be in the save with the bytes it had.
                let Some(name) = written.get(t) else { return false };
                match (now.bytes_of(name), n.entries.get(t)) {
                    (Some(b), Some((crc, len))) => b.len() as u64 == *len && crc32(b) == *crc,
                    _ => false,
                }
            })
        });
        if keep.len() == before {
            break;
        }
    }
    let mut out = vec![];
    for (orig, bytes) in n.parts.iter().filter(|(k, _)| keep.contains(&k.to_ascii_lowercase())) {
        let Some(name) = written.get(&orig.to_ascii_lowercase()).cloned() else { continue };
        now.replace(&name, bytes.clone());
        let rels = opc::rels_path(orig);
        if let Some(rb) = n.parts.get(&rels) {
            now.replace(&opc::rels_path(&name), rb.clone());
        }
        out.push(name);
    }
    out.sort();
    out
}

/// A notes page whose text changed: the original page with only the body placeholder's text
/// replaced by `fresh`'s (the page the writer made), so its other shapes (the slide image, the
/// slide number…) and everything else stay. `None` when that cannot be done safely.
pub(crate) fn splice_notes(original: &[u8], fresh: &[u8]) -> Option<Vec<u8>> {
    let orig = std::str::from_utf8(original).ok()?;
    let new = std::str::from_utf8(fresh).ok()?;
    let body_new = body_tx(new)?;
    let text_new = new.get(body_new.0..body_new.1)?;
    // The new text must not point at relationships the original page does not have.
    if text_new.contains("r:id=") || text_new.contains("r:embed=") {
        return None;
    }
    let body_old = body_tx(orig)?;
    let mut out = String::with_capacity(orig.len() + text_new.len());
    out.push_str(orig.get(..body_old.0)?);
    out.push_str(text_new);
    out.push_str(orig.get(body_old.1..)?);
    Some(out.into_bytes())
}

/// The byte range of the `<p:txBody>…</p:txBody>` of the shape whose placeholder is the notes
/// body (`<p:ph type="body"…>`).
fn body_tx(xml: &str) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(i) = xml.get(from..)?.find("<p:sp>").or_else(|| xml.get(from..)?.find("<p:sp ")) {
        let start = from + i;
        let end = start + xml.get(start..)?.find("</p:sp>")? + "</p:sp>".len();
        let sp = xml.get(start..end)?;
        let ph_body =
            sp.find("<p:ph").and_then(|k| sp.get(k..).and_then(|r| r.find('>').map(|e| &r[..e]))).is_some_and(|ph| ph.contains("type=\"body\""));
        if ph_body {
            let a = sp.find("<p:txBody")?;
            let b = sp.find("</p:txBody>")? + "</p:txBody>".len();
            return Some((start + a, start + b));
        }
        from = end;
    }
    None
}

// ------------------------------------------------------------------ document properties

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The element `local` (any prefix) in `xml`: (start, end, start of its text, end of its text, the
/// qualified name); the text range is empty for `<x/>`.
fn element(xml: &str, local: &str) -> Option<(usize, usize, usize, usize, String)> {
    let needle = format!(":{local}");
    let mut from = 0;
    while let Some(i) = xml.get(from..)?.find(&needle) {
        let at = from + i;
        from = at + needle.len();
        let lt = xml.get(..at)?.rfind('<')?;
        let prefix = xml.get(lt + 1..at)?;
        if prefix.is_empty() || !prefix.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            continue;
        }
        let after = xml.get(from..)?.chars().next()?;
        if !matches!(after, '>' | '/' | ' ' | '\t' | '\n' | '\r') {
            continue;
        }
        let qname = format!("{prefix}:{local}");
        let gt = from + xml.get(from..)?.find('>')?;
        if xml.get(..gt)?.ends_with('/') {
            return Some((lt, gt + 1, gt + 1, gt + 1, qname));
        }
        let close = format!("</{qname}>");
        let c = gt + 1 + xml.get(gt + 1..)?.find(&close)?;
        return Some((lt, c + close.len(), gt + 1, c, qname));
    }
    None
}

/// `xml` with the text of element `local` set to `value` (the element added before the root's
/// end tag, with its namespace declared, when there is none).
fn set_text(xml: &str, local: &str, ns_prefix: &str, ns: &str, value: &str, attrs: &str) -> String {
    match element(xml, local) {
        Some((s, e, ts, te, q)) if ts == te => {
            // `<x/>` or `<x attr/>`: keep its attributes.
            let open = xml.get(s..e).unwrap_or("").trim_end_matches('>').trim_end_matches('/').to_string();
            format!("{}{open}>{}</{q}>{}", xml.get(..s).unwrap_or(""), esc(value), xml.get(e..).unwrap_or(""))
        }
        Some((_, _, ts, te, _)) => format!("{}{}{}", xml.get(..ts).unwrap_or(""), esc(value), xml.get(te..).unwrap_or("")),
        None => {
            let Some(end) = xml.rfind("</") else { return xml.to_string() };
            format!(
                "{}<{ns_prefix}:{local} xmlns:{ns_prefix}=\"{ns}\"{attrs}>{}</{ns_prefix}:{local}>{}",
                xml.get(..end).unwrap_or(""),
                esc(value),
                xml.get(end..).unwrap_or("")
            )
        }
    }
}

/// The current time as W3CDTF (`2026-10-31T09:15:00Z`); `None` where there is no system clock
/// (the browser build: `SystemTime::now` is not available there).
pub(crate) fn now_w3cdtf() -> Option<String> {
    if cfg!(target_arch = "wasm32") {
        return None;
    }
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    Some(format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60))
}

const NS_DC: &str = "http://purl.org/dc/elements/1.1/";
const NS_CP: &str = "http://schemas.openxmlformats.org/package/2006/metadata/core-properties";
const NS_DCTERMS: &str = "http://purl.org/dc/terms/";
const NS_XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";

/// `docProps/core.xml` as it was in the file, with the properties the model changed set,
/// `modified` now and `lastModifiedBy` the model's (the caller's, set at save).
pub(crate) fn core_xml(original: &[u8], now: &Properties, then: Option<&Properties>) -> Option<Vec<u8>> {
    let mut x = std::str::from_utf8(original).ok()?.to_string();
    let fields: [(&str, &str, &str, fn(&Properties) -> &String); 7] = [
        ("title", "dc", NS_DC, |p| &p.title),
        ("subject", "dc", NS_DC, |p| &p.subject),
        ("creator", "dc", NS_DC, |p| &p.author),
        ("keywords", "cp", NS_CP, |p| &p.keywords),
        ("description", "dc", NS_DC, |p| &p.comments),
        ("category", "cp", NS_CP, |p| &p.category),
        ("lastModifiedBy", "cp", NS_CP, |p| &p.last_modified_by),
    ];
    for (local, pre, ns, get) in fields {
        let v = get(now);
        let changed = then.is_none_or(|t| get(t) != v);
        if changed && !(v.is_empty() && element(&x, local).is_none()) {
            x = set_text(&x, local, pre, ns, v, "");
        }
    }
    // Modified now (where there is no clock, the model's own date when it has one).
    let modified = now_w3cdtf().or_else(|| (now.modified.len() >= 10).then(|| now.modified.clone()));
    if let Some(m) = modified {
        x = set_text(&x, "modified", "dcterms", NS_DCTERMS, &m, &format!(" xmlns:xsi=\"{NS_XSI}\" xsi:type=\"dcterms:W3CDTF\""));
    }
    Some(x.into_bytes())
}

/// `docProps/app.xml` as it was, with the company set when the model changed it and the slide,
/// notes and hidden-slide counts made true.
pub(crate) fn app_xml(original: &[u8], p: &Presentation, then: Option<&Properties>) -> Option<Vec<u8>> {
    const NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/extended-properties";
    let mut x = std::str::from_utf8(original).ok()?.to_string();
    if then.is_none_or(|t| t.company != p.props.company) {
        x = set_unprefixed(&x, "Company", NS, &p.props.company);
    }
    let counts = [
        ("Slides", p.slides.len()),
        ("Notes", p.slides.iter().filter(|s| !s.notes.is_empty()).count()),
        ("HiddenSlides", p.slides.iter().filter(|s| s.hidden).count()),
    ];
    for (tag, n) in counts {
        if x.contains(&format!("<{tag}>")) || x.contains(&format!("<{tag}/>")) {
            x = set_unprefixed(&x, tag, NS, &n.to_string());
        }
    }
    Some(x.into_bytes())
}

/// [`set_text`] for an element in the default namespace (`<Company>`).
fn set_unprefixed(xml: &str, tag: &str, ns: &str, value: &str) -> String {
    let empty = format!("<{tag}/>");
    if let Some(i) = xml.find(&empty) {
        return format!("{}<{tag}>{}</{tag}>{}", &xml[..i], esc(value), &xml[i + empty.len()..]);
    }
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    if let (Some(a), Some(b)) = (xml.find(&open), xml.find(&close))
        && b >= a + open.len()
    {
        return format!("{}{}{}", &xml[..a + open.len()], esc(value), &xml[b..]);
    }
    if value.is_empty() {
        return xml.to_string();
    }
    match xml.rfind("</") {
        Some(end) => format!("{}<{tag} xmlns=\"{ns}\">{}</{tag}>{}", &xml[..end], esc(value), &xml[end..]),
        None => xml.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn core_properties_keep_what_they_had_and_take_the_changes() {
        let orig = br#"<?xml version="1.0"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:title>Board</dc:title><dc:subject/><dc:creator>Ann</dc:creator><cp:keywords>a, b</cp:keywords><cp:lastModifiedBy>Ann</cp:lastModifiedBy><dcterms:modified xsi:type="dcterms:W3CDTF">2013-01-27T09:15:58Z</dcterms:modified><cp:contentStatus>Draft</cp:contentStatus></cp:coreProperties>"#;
        let then =
            Properties { title: "Board".into(), author: "Ann".into(), keywords: "a, b".into(), last_modified_by: "Ann".into(), ..Default::default() };
        let mut now = then.clone();
        now.subject = "Q3 & Q4".into();
        now.last_modified_by = "Agent".into();
        let out = String::from_utf8(core_xml(orig, &now, Some(&then)).unwrap()).unwrap();
        assert!(
            out.contains("<dc:title>Board</dc:title>")
                && out.contains("<dc:creator>Ann</dc:creator>")
                && out.contains("<cp:keywords>a, b</cp:keywords>"),
            "{out}"
        );
        assert!(out.contains("<dc:subject>Q3 &amp; Q4</dc:subject>"), "{out}");
        assert!(out.contains("<cp:lastModifiedBy>Agent</cp:lastModifiedBy>"), "{out}");
        assert!(out.contains("<cp:contentStatus>Draft</cp:contentStatus>"), "an element the model does not know stays: {out}");
        assert!(!out.contains("2013-01-27T09:15:58Z") && out.contains("xsi:type=\"dcterms:W3CDTF\">20"), "{out}");
        crate::xml::parse(out.as_bytes()).unwrap();
    }

    #[test]
    fn a_notes_page_keeps_its_other_shapes_when_its_text_changes() {
        let orig = br#"<p:notes><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="Slide Image"/><p:cNvSpPr/><p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr><p:spPr/></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="Notes"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:r><a:t>old</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:nvSpPr><p:cNvPr id="4" name="Slide Number"/><p:cNvSpPr/><p:nvPr><p:ph type="sldNum" idx="5"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:fld type="slidenum"><a:t>2</a:t></a:fld></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notes>"#;
        let fresh = br#"<p:notes><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="Notes Placeholder"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:p><a:r><a:t>new</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notes>"#;
        let out = String::from_utf8(splice_notes(orig, fresh).unwrap()).unwrap();
        assert!(out.contains("<a:t>new</a:t>") && !out.contains("<a:t>old</a:t>"));
        assert!(out.contains("type=\"sldImg\"") && out.contains("type=\"sldNum\"") && out.contains("name=\"Notes\""), "{out}");
    }

    #[test]
    fn fresh_names_never_take_a_kept_one() {
        let mut n = Namer { reserved: ["ppt/slides/slide1.xml".to_string()].into_iter().collect(), used: HashSet::new() };
        assert_eq!(n.name(None, "ppt/slides/slide"), "ppt/slides/slide2.xml");
        assert_eq!(n.name(Some(&"ppt/slides/slide1.xml".to_string()), "ppt/slides/slide"), "ppt/slides/slide1.xml");
        assert_eq!(n.name(Some(&"ppt/slides/slide1.xml".to_string()), "ppt/slides/slide"), "ppt/slides/slide3.xml", "a name is given once");
        let now = now_w3cdtf().unwrap();
        assert!(now.ends_with('Z') && now.len() == 20, "{now}");
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
