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
pub(crate) fn recovery_dir(shared: &Shared, original: Option<&str>) -> PathBuf {
    if let Some(root) = &shared.config.write_root {
        return root.clone();
    }
    if let Some(dir) = original.and_then(|p| Path::new(p).parent()).filter(|d| !d.as_os_str().is_empty() && d.is_dir()) {
        return dir.to_path_buf();
    }
    std::env::temp_dir()
}

/// File-name-safe stem for `title`.
pub(crate) fn safe_stem(s: &str) -> String {
    let t: String = s.chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.') { c } else { '_' }).collect();
    let t = t.trim().trim_matches('.').to_string();
    if t.is_empty() { "Presentation".into() } else { t.chars().take(80).collect() }
}

/// The format a recovered copy is written in: the original's when it is one we write losslessly.
pub(crate) fn recovered_format(original: Option<&str>) -> &'static str {
    let ext = original.and_then(|p| Path::new(p).extension()).map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "deckcraft" => "deckcraft",
        "potx" => "potx",
        "ppsx" => "ppsx",
        _ => "pptx",
    }
}

/// `<stem>.recovered-<stamp>.<fmt>` in `dir`, with `-2`, `-3`… when that is taken.
pub(crate) fn free_name(dir: &Path, stem: &str, fmt: &str) -> String {
    let base = format!("{stem}.recovered-{}", stamp());
    let mut name = format!("{base}.{fmt}");
    let mut n = 2;
    while dir.join(&name).exists() && n < 1000 {
        name = format!("{base}-{n}.{fmt}");
        n += 1;
    }
    name
}

/// Write one presentation to a recovered file. `Err` is a reason (logged; the server keeps going).
pub fn write_recovered(
    shared: &Shared,
    roots: Option<&Arc<AutomationRoots>>,
    doc: &Presentation,
    title: &str,
    original: Option<&str>,
    reason: &str,
    id: Option<(u64, u64)>,
) -> Result<Recovered, String> {
    let fmt = recovered_format(original);
    let stem = original.and_then(|p| Path::new(p).file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| title.to_string());
    let dir = recovery_dir(shared, original);
    // WHIQ: streamed into the file (spilled media from disk), never whole in memory.
    let write = |path: &str| deckcraft_engine::cmd::file::save_to_path(doc, fmt, path).map(|_| ()).map_err(|e| e.to_string());
    // The same stretch of work already has a checkpoint: it becomes the recovered file (one file,
    // brought up to date, as in every craft app).
    let reuse = id.and_then(|(uid, _)| crate::checkpoint::take_path(shared, uid));
    let path = match &reuse {
        Some(p) => p.clone(),
        None => dir.join(free_name(&dir, &safe_stem(&stem), fmt)).to_string_lossy().to_string(),
    };
    let written = if reuse.is_some() {
        file_access::confine(roots, || write(&path))
    } else {
        file_access::confine(roots, || deckcraft_engine::io::keep_previous(|| write(&path))).0
    };
    written?;
    let r = Recovered { path, title: title.to_string(), original: original.map(String::from), reason: reason.to_string(), at: now_rfc3339() };
    eprintln!("deckcraft-cli: work auto-saved ({reason}) to {}", r.path);
    shared.add_recovered(r.clone());
    // A full copy of the presentation now exists: its periodic checkpoint is superseded.
    if let Some((uid, revision)) = id {
        crate::checkpoint::superseded(shared, uid, revision);
    }
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
        match write_recovered(shared, roots, &st.doc, &st.title(), st.path.as_deref(), reason, Some((st.uid, st.revision))) {
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
        .filter_map(|d| match write_recovered(shared, roots, &d.doc, &d.title, d.path.as_deref(), reason, Some((d.uid, d.revision))) {
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
            // Not an atomic write's temporary file a crash left (`<name>.<pid>.<n>.saving`).
            if !name.contains(".recovered-") || name.starts_with("OLD_") || name.ends_with(".saving") {
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
