//! WHIQ: periodic checkpoints, so a hard kill (Windows `TerminateProcess`, the OOM killer, a
//! power cut) loses little. Every presentation with unsaved work is written to its checkpoint —
//! its recovered file, `<name>.recovered-<stamp>.<ext>` in the write root (the name every craft
//! app uses; a later auto-save of the same stretch of work writes the same file) — once it has
//! `checkpoint_edits` edits that are not in the checkpoint yet, or once the oldest of them is
//! `checkpoint_secs` old. A background thread does it from the worker's last snapshot (the
//! presentations as the last finished call left them), so a long call never delays it and the
//! worker never waits for it.
//!
//! The checkpoint is replaced in place (written to a temporary file, then renamed over it): it is
//! the server's own copy, not the user's file. It is removed once it is superseded — the
//! presentation saved to its file, or a full recovered copy written (close, eviction, SIGTERM) —
//! and kept, and named to the agent, when the presentation goes away with work no other file
//! holds. After a hard kill the next run lists it in `recovered_files` with the other recovered
//! files.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use deckcraft_engine::file_access::{self, AutomationRoots};

use crate::state::{DocSnapshot, Recovered, Shared, lock, now_rfc3339};

/// What the checkpointer knows of one open presentation.
#[derive(Clone, Debug, Default)]
pub struct Checkpoint {
    /// The checkpoint file, while there is one.
    pub path: Option<String>,
    /// The edit counter (revision) the checkpoint — or the saved file — holds.
    pub revision: u64,
    /// When the oldest edit not yet checkpointed was first seen.
    pub pending_since: Option<Instant>,
    /// No new attempt before this (after a failed write).
    pub retry_at: Option<Instant>,
    pub title: String,
    /// The presentation had unsaved work when last seen.
    pub unsaved: bool,
}

/// Fresh documents start at revision 1 (`DocState::new`).
const FIRST_REVISION: u64 = 1;

/// Checkpoints written in this process (server_status).
static WRITES: AtomicU64 = AtomicU64::new(0);

/// A new recovered file name for a presentation's checkpoint (kept for the whole stretch of
/// unsaved work).
fn file_name(d: &DocSnapshot, fmt: &str, dir: &Path) -> String {
    let stem = d.path.as_deref().and_then(|p| Path::new(p).file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| d.title.clone());
    crate::autosave::free_name(dir, &crate::autosave::safe_stem(&stem), fmt)
}

/// The checkpoint file of document `uid`, handed over to an auto-save that writes the same
/// stretch of work there (the file stays; it is no longer a checkpoint).
pub fn take_path(shared: &Shared, uid: u64) -> Option<String> {
    lock(&shared.checkpoints).get_mut(&uid).and_then(|c| c.path.take())
}

fn remove(path: &str) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("deckcraft-cli: could not remove the superseded checkpoint {path}: {e}"),
    }
}

/// A full copy of document `uid` at `revision` was just written (a recovered file or the
/// presentation's own file): its checkpoint is superseded.
pub fn superseded(shared: &Shared, uid: u64, revision: u64) {
    let mut map = lock(&shared.checkpoints);
    let c = map.entry(uid).or_default();
    if let Some(p) = c.path.take() {
        remove(&p);
    }
    c.revision = c.revision.max(revision);
    c.pending_since = None;
}

/// One pass: checkpoint what is due, drop what is superseded. Returns the files written.
pub fn tick(shared: &Shared, roots: Option<&Arc<AutomationRoots>>, now: Instant) -> Vec<String> {
    let (every_edits, every_secs) = (shared.config.checkpoint_edits, shared.config.checkpoint_secs);
    if every_edits == 0 && every_secs == 0 {
        return vec![];
    }
    let docs = shared.docs();
    let mut map = lock(&shared.checkpoints);
    // Presentations that went away.
    let gone: Vec<u64> = map.keys().copied().filter(|uid| !docs.iter().any(|d| d.uid == *uid)).collect();
    for uid in gone {
        let Some(c) = map.remove(&uid) else { continue };
        let Some(path) = c.path else { continue };
        if c.unsaved {
            // Closed with work no other file holds (an auto-save that failed): keep it, and say so.
            let r = Recovered { path, title: c.title, original: None, reason: "checkpoint".into(), at: now_rfc3339() };
            eprintln!("deckcraft-cli: {} closed with unsaved work; its checkpoint {} is kept", r.title, r.path);
            shared.add_recovered(r);
        } else {
            remove(&path);
        }
    }
    let mut written = vec![];
    for d in &docs {
        let c = map.entry(d.uid).or_insert_with(|| Checkpoint { revision: FIRST_REVISION, ..Default::default() });
        c.title = d.title.clone();
        let unsaved = d.dirty || d.path.is_none();
        if !unsaved {
            // Saved: the file holds everything.
            if let Some(p) = c.path.take() {
                remove(&p);
            }
            c.revision = d.revision;
            c.pending_since = None;
            c.unsaved = false;
            continue;
        }
        if d.revision <= c.revision {
            continue;
        }
        c.unsaved = true;
        let since = *c.pending_since.get_or_insert(now);
        let due = (every_edits > 0 && d.revision - c.revision >= every_edits)
            || (every_secs > 0 && now.duration_since(since) >= Duration::from_secs(every_secs));
        if !due || c.retry_at.is_some_and(|t| now < t) {
            continue;
        }
        let fmt = crate::autosave::recovered_format(d.path.as_deref());
        let dir = crate::autosave::recovery_dir(shared, d.path.as_deref());
        let path = c.path.clone().unwrap_or_else(|| dir.join(file_name(d, fmt, &dir)).to_string_lossy().to_string());
        // WHIQ: streamed into the file (spilled media from disk), never whole in memory.
        let result =
            file_access::confine(roots, || deckcraft_engine::cmd::file::save_to_path(&d.doc, fmt, &path).map(|_| ()).map_err(|e| e.to_string()));
        match result {
            Ok(()) => {
                c.path = Some(path.clone());
                c.revision = d.revision;
                c.pending_since = None;
                c.retry_at = None;
                WRITES.fetch_add(1, Ordering::Relaxed);
                written.push(path);
            }
            Err(e) => {
                eprintln!("deckcraft-cli: checkpoint of {} failed: {e}", d.title);
                c.retry_at = Some(now + Duration::from_secs(every_secs.clamp(5, 60)));
            }
        }
    }
    written
}

/// The checkpoints (for server_status), in the shape every craft app reports.
pub fn status(shared: &Shared) -> serde_json::Value {
    let map = lock(&shared.checkpoints);
    let docs = shared.docs();
    let pending: u64 = docs
        .iter()
        .filter(|d| d.dirty || d.path.is_none())
        .map(|d| d.revision.saturating_sub(map.get(&d.uid).map_or(FIRST_REVISION, |c| c.revision)))
        .sum();
    serde_json::json!({
        "every_edits": shared.config.checkpoint_edits,
        "every_secs": shared.config.checkpoint_secs,
        "pending_edits": pending,
        "writes": WRITES.load(Ordering::Relaxed),
        "files": map.iter().filter_map(|(uid, c)| c.path.as_ref().map(|p| serde_json::json!({"path": p, "document": uid, "title": c.title, "revision": c.revision}))).collect::<Vec<_>>(),
    })
}
