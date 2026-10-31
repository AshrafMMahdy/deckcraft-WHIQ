//! PPTX import and export: Office Open XML PresentationML / DrawingML, written from the public
//! ECMA-376 standard.
//!
//! [`import`] reads a package leniently (unknown parts are skipped, malformed parts are logged and
//! ignored, every size and depth is bounded) into the DeckCraft model; [`export`] writes a valid
//! package that PowerPoint opens without repair.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

mod cfb;
mod custgeom;
mod dgm;
mod maps;
mod opc;
mod png;
pub(crate) mod read;
mod tables;
mod write;
mod xml;

/// The bounded XML reader the importer uses, for callers that read kept fragments (a shape's
/// 3-D settings) instead of scanning strings.
pub use xml::{Doc as XmlDoc, El as XmlEl, parse as parse_xml};

use deckcraft_model::Presentation;

/// Extension URI of the PowerPoint 2010 section list in `presentation.xml`.
pub(crate) const SECTION_EXT_URI: &str = "{521415D9-36F7-43E2-AB2F-B90AF26B5E84}";

/// Extension URI of the modern-comments anchor in a slide (names the threaded comment part).
pub(crate) const COMMENTS_EXT_URI: &str = "{6950BFC3-D8DA-4A85-94F7-54DA5524770B}";

#[derive(Debug, thiserror::Error)]
pub enum PptxError {
    #[error("not a PPTX file: {0}")]
    NotPptx(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("write failed: {0}")]
    Write(String),
    #[error("malformed XML: {0}")]
    Xml(String),
}

/// Does this look like an OOXML package (zip with `[Content_Types].xml`)?
///
/// The part is looked up in the zip's central directory, not in the leading bytes: writers may
/// store it anywhere in the archive (recent PowerPoint builds put it last, megabytes in).
pub fn sniff(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"PK") {
        return false;
    }
    zip::ZipArchive::new(std::io::Cursor::new(bytes)).map(|z| z.index_for_name("[Content_Types].xml").is_some()).unwrap_or(false)
}

/// Is this a password-protected presentation (an encrypted package inside a compound file)?
pub fn is_encrypted(bytes: &[u8]) -> bool {
    cfb::is_encrypted(bytes)
}

/// Read a `.pptx` / `.potx` / `.ppsx` package.
pub fn import(bytes: &[u8]) -> Result<Presentation, PptxError> {
    read::import(bytes)
}

/// WHIQ: [`import`] with a callback before each slide (`false` stops: cancelled).
pub fn import_with(bytes: &[u8], on_slide: &mut dyn FnMut(usize, usize) -> bool) -> Result<Presentation, PptxError> {
    read::import_with(bytes, on_slide)
}

/// WHIQ: what an import could not keep ([`import_reader`]).
pub use read::ImportReport;
/// WHIQ: take the presentation as it is now as the one read from its file (after a repair).
pub use read::source::snapshot as snapshot_source;

/// WHIQ: read a package from `r` (an open file): the file is never read whole into memory, large
/// media is inflated into spill files on disk, and everything that could not be kept is in the
/// report.
pub fn import_reader(r: Box<dyn opc::ReadSeek>, on_slide: &mut dyn FnMut(usize, usize) -> bool) -> Result<(Presentation, ImportReport), PptxError> {
    read::import_reader(r, on_slide)
}

/// WHIQ: [`sniff`] on an open file (the central directory is read, not the file).
pub fn sniff_reader(r: Box<dyn opc::ReadSeek>) -> bool {
    zip::ZipArchive::new(r).map(|z| z.index_for_name("[Content_Types].xml").is_some()).unwrap_or(false)
}

/// Anything a package can be read from (a file, bytes in memory).
pub use opc::ReadSeek;

/// Read the theme of a `.thmx` file, or of a `.pptx` / `.potx`'s first master, with its media.
pub fn import_theme(bytes: &[u8]) -> Result<(deckcraft_model::theme::Theme, Vec<deckcraft_model::MediaItem>), PptxError> {
    read::import_theme(bytes)
}

/// Write a `.pptx` package.
pub fn export(p: &Presentation) -> Result<Vec<u8>, PptxError> {
    write::export(p, write::CT_PRES)
}

/// WHIQ: [`export_as`] reporting progress (`on(stage, done, total)`, see `write::export_with`);
/// returning `false` stops it.
pub fn export_as_with(p: &Presentation, ext: &str, on: &mut dyn FnMut(&'static str, usize, usize) -> bool) -> Result<Vec<u8>, PptxError> {
    let ct = match ext {
        "potx" => write::CT_TEMPLATE,
        "ppsx" => write::CT_SHOW,
        _ => write::CT_PRES,
    };
    write::export_with(p, ct, on)
}

/// WHIQ: [`export_as_with`] straight into `out` (an open file): nothing of the package is held in
/// memory, spilled media streams from disk. Returns the bytes written.
pub fn export_into<O: std::io::Write + std::io::Seek>(
    p: &Presentation,
    ext: &str,
    out: &mut O,
    on: &mut dyn FnMut(&'static str, usize, usize) -> bool,
) -> Result<u64, PptxError> {
    let ct = match ext {
        "potx" => write::CT_TEMPLATE,
        "ppsx" => write::CT_SHOW,
        _ => write::CT_PRES,
    };
    write::export_into(p, ct, out, on)
}

/// Write a package for `ext`: `potx` (template) and `ppsx` (slide show) differ from `pptx` only in
/// the main part's content type.
pub fn export_as(p: &Presentation, ext: &str) -> Result<Vec<u8>, PptxError> {
    let ct = match ext {
        "potx" => write::CT_TEMPLATE,
        "ppsx" => write::CT_SHOW,
        _ => write::CT_PRES,
    };
    write::export(p, ct)
}

/// Content type for a file name's extension.
pub(crate) fn mime_for(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" | "jpe" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "emf" => "image/x-emf",
        "wmf" => "image/x-wmf",
        "webp" => "image/webp",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "wmv" => "video/x-ms-wmv",
        "avi" => "video/x-msvideo",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "wma" => "audio/x-ms-wma",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "glb" => "model/gltf-binary",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "fntdata" => "application/x-fontdata",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod import_theme_tests {
    use std::io::{Read, Write};

    use deckcraft_model::Presentation;

    fn custom() -> Presentation {
        let mut p = Presentation::default();
        let m = std::sync::Arc::make_mut(&mut p.masters[0]);
        m.theme.name = "Harbour".into();
        m.theme.fonts.major.latin = "Lato".into();
        m.theme.fonts.minor.latin = "Source Sans 3".into();
        m.theme.colors.set(deckcraft_color::SchemeSlot::Accent1, deckcraft_color::Rgba::rgb(0x12, 0x34, 0x56));
        p
    }

    /// A `.thmx`: the deck's theme part as the package's main document.
    fn thmx(pptx: &[u8]) -> Vec<u8> {
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(pptx)).unwrap();
        let mut theme = String::new();
        z.by_name("ppt/theme/theme1.xml").unwrap().read_to_string(&mut theme).unwrap();
        let mut out = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default();
        out.start_file("[Content_Types].xml", o).unwrap();
        out.write_all(br#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/theme/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/></Types>"#).unwrap();
        out.start_file("_rels/.rels", o).unwrap();
        out.write_all(br#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="theme/theme/theme1.xml"/></Relationships>"#).unwrap();
        out.start_file("theme/theme/theme1.xml", o).unwrap();
        out.write_all(theme.as_bytes()).unwrap();
        out.finish().unwrap().into_inner()
    }

    #[test]
    fn reads_the_theme_of_a_pptx_and_of_a_thmx() {
        let pptx = crate::export(&custom()).unwrap();
        for bytes in [pptx.clone(), thmx(&pptx)] {
            let (t, _) = crate::import_theme(&bytes).unwrap();
            assert_eq!(t.name, "Harbour");
            assert_eq!(t.fonts.major.latin, "Lato");
            assert_eq!(t.fonts.minor.latin, "Source Sans 3");
            assert_eq!(t.colors.get(deckcraft_color::SchemeSlot::Accent1), deckcraft_color::Rgba::rgb(0x12, 0x34, 0x56));
        }
    }

    #[test]
    fn non_theme_packages_are_errors() {
        assert!(crate::import_theme(b"not a zip").is_err());
        let mut out = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        out.start_file("[Content_Types].xml", zip::write::SimpleFileOptions::default()).unwrap();
        out.write_all(b"<Types/>").unwrap();
        assert!(crate::import_theme(&out.finish().unwrap().into_inner()).is_err());
    }
}

#[cfg(test)]
mod whiq_size_tests {
    //! WHIQ: no part is limited by file size. Large media goes through spill files on disk and is
    //! never dropped; what cannot be kept is reported.
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use std::io::{Cursor, Read, Write};

    use deckcraft_model::Presentation;

    fn video_deck(len: usize) -> (Presentation, Vec<u8>) {
        let mut p = Presentation::default();
        let video: Vec<u8> = (0..len).map(|i| (i % 253) as u8).collect();
        let id = p.add_media("clip.mp4", "video/mp4", video.clone()).unwrap();
        let sh = deckcraft_model::Shape {
            id: deckcraft_model::ShapeId(p.alloc_id().unwrap()),
            name: "Video".into(),
            xfrm: Some(deckcraft_geom::Xfrm::new(10.0, 10.0, 200.0, 120.0)),
            kind: deckcraft_model::ShapeKind::Media(deckcraft_model::MediaClip { media: id, video: true, volume: 1.0, ..Default::default() }),
            ..Default::default()
        };
        if p.slides.is_empty() {
            let layout = deckcraft_model::defaults::layout_of_kind(&p, deckcraft_model::LayoutType::Blank).unwrap_or_default();
            let s = deckcraft_model::defaults::new_slide(&mut p, layout).unwrap();
            p.slides.push(std::sync::Arc::new(s));
        }
        std::sync::Arc::make_mut(&mut p.slides[0]).shapes.push(sh);
        (p, video)
    }

    fn part(zip: &[u8], suffix: &str) -> Option<Vec<u8>> {
        let mut z = zip::ZipArchive::new(Cursor::new(zip)).unwrap();
        let name = (0..z.len()).filter_map(|i| z.name_for_index(i).map(String::from)).find(|n| n.ends_with(suffix))?;
        let mut out = Vec::new();
        z.by_name(&name).unwrap().read_to_end(&mut out).unwrap();
        Some(out)
    }

    #[test]
    fn a_large_video_streams_through_a_spill_file_and_is_saved_whole() {
        let len = (deckcraft_model::spill::SPILL_MIN as usize) + 1234;
        let (p, video) = video_deck(len);
        let bytes = crate::export(&p).unwrap();
        let (q, report) = crate::import_reader(Box::new(Cursor::new(bytes)), &mut |_, _| true).unwrap();
        assert!(report.not_kept.is_empty(), "{:?}", report.not_kept);
        let m = q.media.iter().find(|m| m.content_type == "video/mp4").expect("the video is kept");
        assert!(m.data.is_empty() && m.spill.is_some(), "kept on disk, not in memory");
        assert_eq!(m.byte_len(), len as u64);
        // Saved into a file-like sink: the video streams from its spill file, byte for byte.
        let mut out = Cursor::new(Vec::new());
        let n = crate::export_into(&q, "pptx", &mut out, &mut |_, _, _| true).unwrap();
        let out = out.into_inner();
        assert_eq!(n, out.len() as u64);
        assert_eq!(part(&out, ".mp4").expect("the video part is in the saved file"), video);
        // The spill file goes with the last presentation holding it.
        let path = m.spill.as_ref().unwrap().path().to_path_buf();
        drop(q);
        assert!(!path.exists());
    }

    #[test]
    fn a_part_past_the_memory_limit_is_reported_not_skipped_in_silence() {
        let mut zw = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default();
        zw.start_file("big.xml", o).unwrap();
        zw.write_all(&vec![b' '; 4096]).unwrap();
        let z = zw.finish().unwrap().into_inner();
        let lim = crate::opc::Limits { part: 1000, total: 1 << 20, work: 1 << 20, entries: 10 };
        let pkg = crate::opc::Package::open_within(Box::new(Cursor::new(z)), lim).unwrap();
        assert!(pkg.get("big.xml").is_none());
        let why = pkg.not_kept();
        assert!(why.iter().any(|m| m.contains("big.xml") && m.contains("in memory")), "{why:?}");
    }

    #[test]
    fn parts_nothing_refers_to_are_listed() {
        let (p, _) = video_deck(1000);
        let bytes = crate::export(&p).unwrap();
        // Add an orphan part beside the deck's own.
        let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for i in 0..z.len() {
            w.raw_copy_file(z.by_index(i).unwrap()).unwrap();
        }
        w.start_file("ppt/media/orphan.bin", zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(&[9u8; 777]).unwrap();
        let bytes = w.finish().unwrap().into_inner();
        let (_, report) = crate::import_reader(Box::new(Cursor::new(bytes)), &mut |_, _| true).unwrap();
        assert!(report.unused.iter().any(|(n, b, referenced)| n == "ppt/media/orphan.bin" && *b == 777 && !referenced), "{:?}", report.unused);
    }
}
