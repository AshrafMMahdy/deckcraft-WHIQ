//! Work is never lost (WHIQ contract §3): on eviction, memory pressure, a panic, SIGTERM or the
//! end of input, open presentations are written to `<name>.recovered-<stamp>.<ext>` inside the
//! write root, and the next call names the file.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use deckcraft_engine::Session;
use deckcraft_engine::file_access::{self, AutomationRoots};
use deckcraft_engine::model::Presentation;

use crate::state::{DocSnapshot, Recovered, Shared, now_rfc3339, stamp};

/// The folder recovered files go to: the write root, else the presentation's own folder, else the
/// system's temporary folder.
fn recovery_dir(shared: &Shared, original: Option<&str>) -> PathBuf {
    if let Some(root) = &shared.config.write_root {
        return root.clone();
    }
    if let Some(dir) = original.and_then(|p| Path::new(p).parent()).filter(|d| !d.as_os_str().is_empty() && d.is_dir()) {
        return dir.to_path_buf();
    }
    std::env::temp_dir()
}

/// File-name-safe stem for `title`.
fn safe_stem(s: &str) -> String {
    let t: String = s.chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.') { c } else { '_' }).collect();
    let t = t.trim().trim_matches('.').to_string();
    if t.is_empty() { "Presentation".into() } else { t.chars().take(80).collect() }
}

/// The format a recovered copy is written in: the original's when it is one we write losslessly.
fn recovered_format(original: Option<&str>) -> &'static str {
    let ext = original.and_then(|p| Path::new(p).extension()).map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "deckcraft" => "deckcraft",
        "potx" => "potx",
        "ppsx" => "ppsx",
        _ => "pptx",
    }
}

/// Write one presentation to a recovered file. `Err` is a reason (logged; the server keeps going).
pub fn write_recovered(
    shared: &Shared,
    roots: Option<&Arc<AutomationRoots>>,
    doc: &Presentation,
    title: &str,
    original: Option<&str>,
    reason: &str,
) -> Result<Recovered, String> {
    let fmt = recovered_format(original);
    let stem = original.and_then(|p| Path::new(p).file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| title.to_string());
    let name = format!("{}.recovered-{}.{fmt}", safe_stem(&stem), stamp());
    let path = recovery_dir(shared, original).join(name).to_string_lossy().to_string();
    let bytes = deckcraft_engine::cmd::file::save_bytes(doc, fmt).map_err(|e| e.to_string())?;
    let (written, _) = file_access::confine(roots, || deckcraft_engine::io::keep_previous(|| deckcraft_engine::io::write_file(&path, &bytes)));
    written?;
    let r = Recovered { path, title: title.to_string(), original: original.map(String::from), reason: reason.to_string(), at: now_rfc3339() };
    eprintln!("deckcraft-cli: work auto-saved ({reason}) to {}", r.path);
    shared.add_recovered(r.clone());
    Ok(r)
}

/// Auto-save the session's presentations: every one with unsaved work (`all_dirty`), or only the
/// active one. A presentation with no file of its own counts as unsaved.
pub fn save_session(
    session: &Session,
    shared: &Shared,
    roots: Option<&Arc<AutomationRoots>>,
    reason: &str,
    active_only: bool,
    include_clean: bool,
) -> Vec<Recovered> {
    let active = session.active_index();
    let mut out = vec![];
    for (i, st) in session.documents().iter().enumerate() {
        if active_only && Some(i) != active {
            continue;
        }
        let unsaved = st.is_dirty() || st.path.is_none();
        if !unsaved && !include_clean {
            continue;
        }
        match write_recovered(shared, roots, &st.doc, &st.title(), st.path.as_deref(), reason) {
            Ok(r) => out.push(r),
            Err(e) => eprintln!("deckcraft-cli: auto-save of {} failed: {e}", st.title()),
        }
    }
    out
}

/// Auto-save from the last snapshot the worker left (when the worker itself is busy or stuck).
pub fn save_snapshots(shared: &Shared, roots: Option<&Arc<AutomationRoots>>, docs: &[DocSnapshot], reason: &str) -> Vec<Recovered> {
    docs.iter()
        .filter(|d| d.dirty || d.path.is_none())
        .filter_map(|d| match write_recovered(shared, roots, &d.doc, &d.title, d.path.as_deref(), reason) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("deckcraft-cli: auto-save of {} failed: {e}", d.title);
                None
            }
        })
        .collect()
}

/// Recovered files a previous run left in `dir` (newest first, at most 50).
pub fn scan(dir: &Path) -> Vec<Recovered> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut found: Vec<(std::time::SystemTime, Recovered)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.contains(".recovered-") || name.starts_with("OLD_") {
                return None;
            }
            let meta = e.metadata().ok().filter(|m| m.is_file())?;
            let title = name.split(".recovered-").next().unwrap_or_default().to_string();
            Some((
                meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                Recovered { path: e.path().to_string_lossy().to_string(), title, original: None, reason: "found".into(), at: String::new() },
            ))
        })
        .collect();
    found.sort_by_key(|x| std::cmp::Reverse(x.0));
    found.into_iter().take(50).map(|(_, r)| r).collect()
}
