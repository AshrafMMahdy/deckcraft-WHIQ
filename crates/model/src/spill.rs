//! WHIQ: large media kept on disk, and the memory budget the readers size their limits from.
//!
//! A presentation's pictures live in memory ([`crate::MediaItem::data`]); a long video, a sound
//! track or any other large part does not have to. Its bytes are inflated once into a
//! [`SpillFile`] in the spill folder and stream from there into every file saved, so a 600 MB
//! drone video costs disk, not memory, and is never left out of a save (the owner's rule: no tool
//! is limited by file size; protective limits follow the machine's memory and are reported).
//!
//! The spill folder is `DECKCRAFT_SPILL_DIR` when set, else `deckcraft-spill` in the system's
//! temporary folder. A spill file is removed when the last item holding it is dropped; files a
//! killed process left behind are removed by [`sweep_stale`] (the MCP server runs it at start).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Bytes of a media part from which it is kept on disk rather than in memory: sound, video and
/// other binary parts from 8 MiB.
pub const SPILL_MIN: u64 = 8 << 20;
/// Pictures stay in memory (they are decoded to draw) up to 64 MiB; a larger one is kept on disk
/// too and read in only while it is drawn.
pub const SPILL_MIN_PICTURE: u64 = 64 << 20;

/// Is a part of `len` bytes and `content_type` kept on disk? Sound, video and 3-D models from
/// [`SPILL_MIN`]; pictures from [`SPILL_MIN_PICTURE`]. Fonts and embedded documents stay in memory
/// (their readers parse them), within the reader's in-memory limit, which reports what it refuses.
pub fn should_spill(content_type: &str, len: u64) -> bool {
    let ct = content_type.to_ascii_lowercase();
    if ct.starts_with("image/") {
        len >= SPILL_MIN_PICTURE
    } else if ct.starts_with("video/") || ct.starts_with("audio/") || ct.starts_with("model/") {
        len >= SPILL_MIN
    } else {
        false
    }
}

/// The bytes of one media part in a file of the spill folder. The file is removed on drop.
#[derive(Debug)]
pub struct SpillFile {
    path: PathBuf,
    len: u64,
}

impl PartialEq for SpillFile {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

static NEXT: AtomicU64 = AtomicU64::new(0);

/// The spill folder (created on first use).
pub fn spill_dir() -> PathBuf {
    match std::env::var_os("DECKCRAFT_SPILL_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => std::env::temp_dir().join("deckcraft-spill"),
    }
}

impl SpillFile {
    /// Copy `src` into a new spill file, at most `max` bytes: a source that holds more is an
    /// error (`InvalidData`), never a silently shortened copy.
    pub fn from_reader(src: &mut dyn Read, max: u64) -> std::io::Result<SpillFile> {
        let dir = spill_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}-{}.part", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
        // Removed again on any failure below.
        let mut sf = SpillFile { path, len: 0 };
        let mut buf = vec![0u8; 1 << 20];
        let mut limited = src.take(max.saturating_add(1));
        loop {
            let n = limited.read(&mut buf)?;
            if n == 0 {
                break;
            }
            sf.len = sf.len.saturating_add(n as u64);
            if sf.len > max {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("more than the {max} bytes it declared")));
            }
            f.write_all(buf.get(..n).unwrap_or_default())?;
        }
        f.flush()?;
        Ok(sf)
    }
    /// A spill file holding `bytes` (a large part made in memory, e.g. by an import).
    pub fn from_bytes(bytes: &[u8]) -> std::io::Result<SpillFile> {
        let mut r = bytes;
        Self::from_reader(&mut r, bytes.len() as u64)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Read the whole file into memory (a caller that has to decode it, e.g. to draw a picture).
    pub fn read_all(&self) -> std::io::Result<Vec<u8>> {
        std::fs::read(&self.path)
    }
    /// Stream the bytes into `w`; returns the bytes copied.
    pub fn copy_to(&self, w: &mut dyn Write) -> std::io::Result<u64> {
        let mut f = std::fs::File::open(&self.path)?;
        let n = std::io::copy(&mut f, w)?;
        if n != self.len {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, format!("{} holds {n} of {} bytes", self.path.display(), self.len)));
        }
        Ok(n)
    }
    /// The first `n` bytes (format sniffing).
    pub fn head(&self, n: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(n);
        if let Ok(f) = std::fs::File::open(&self.path) {
            let _ = f.take(n as u64).read_to_end(&mut out);
        }
        out
    }
}

impl Drop for SpillFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The process id a spill file's name starts with (`<pid>-<n>.part`).
fn owner(path: &Path) -> Option<u32> {
    path.file_name()?.to_str()?.split('-').next()?.parse().ok()
}

/// Remove spill files left by processes that are gone: older than `age`, or (where the system
/// tells it, Linux) whose process no longer runs. Returns how many.
pub fn sweep_stale(age: std::time::Duration) -> usize {
    let Ok(rd) = std::fs::read_dir(spill_dir()) else { return 0 };
    let me = std::process::id();
    let mut n = 0;
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "part") {
            continue;
        }
        let old = e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|d| d > age);
        let gone = owner(&path).is_some_and(|pid| pid != me && Path::new("/proc/self").exists() && !Path::new(&format!("/proc/{pid}")).exists());
        if (old || gone) && std::fs::remove_file(&path).is_ok() {
            n += 1;
        }
    }
    n
}

/// Remove this process's spill files (before an exit that skips destructors: a signal).
pub fn remove_own() {
    let Ok(rd) = std::fs::read_dir(spill_dir()) else { return };
    let me = std::process::id();
    for e in rd.flatten() {
        if owner(&e.path()) == Some(me) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

static BUDGET: AtomicU64 = AtomicU64::new(0);

/// The process's memory budget in bytes: the `--max-memory-mb` the host gave (the platform sizes
/// it from the machine), else half the machine's memory where it can be read, else 4 GiB.
/// Every in-memory limit of the readers is a share of it, so it moves with the machine.
pub fn memory_budget() -> u64 {
    match BUDGET.load(Ordering::Relaxed) {
        0 => machine_memory().map(|m| m / 2).unwrap_or(4 << 30).max(256 << 20),
        b => b,
    }
}

/// Set the memory budget (bytes; 0 goes back to the machine's default).
pub fn set_memory_budget(bytes: u64) {
    BUDGET.store(bytes, Ordering::Relaxed);
}

/// Physical memory of the machine, where the platform tells it without native calls (Linux).
pub fn machine_memory() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    kb.checked_mul(1024)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn a_spill_file_keeps_every_byte_and_goes_away_on_drop() {
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        let f = SpillFile::from_bytes(&data).unwrap();
        assert_eq!(f.len(), data.len() as u64);
        let mut out = Vec::new();
        assert_eq!(f.copy_to(&mut out).unwrap(), data.len() as u64);
        assert_eq!(out, data);
        assert_eq!(f.head(4), data[..4]);
        let p = f.path().to_path_buf();
        assert!(p.exists());
        drop(f);
        assert!(!p.exists(), "the spill file is removed with its last holder");
    }

    #[test]
    fn a_source_longer_than_it_declared_is_an_error_not_a_short_copy() {
        let data = vec![7u8; 1000];
        let mut r = data.as_slice();
        let e = SpillFile::from_reader(&mut r, 999).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn files_of_a_process_that_is_gone_are_swept() {
        if !Path::new("/proc/self").exists() {
            return;
        }
        std::fs::create_dir_all(spill_dir()).unwrap();
        // No process has this id (above the Linux maximum).
        let orphan = spill_dir().join("4194399-0.part");
        std::fs::write(&orphan, b"left by a killed process").unwrap();
        let mine = SpillFile::from_bytes(b"still used").unwrap();
        assert!(sweep_stale(std::time::Duration::from_secs(24 * 3600)) >= 1);
        assert!(!orphan.exists());
        assert!(mine.path().exists(), "this process's files stay");
    }

    #[test]
    fn the_budget_follows_the_host() {
        set_memory_budget(1 << 30);
        assert_eq!(memory_budget(), 1 << 30);
        set_memory_budget(0);
        assert!(memory_budget() >= 256 << 20);
        assert!(should_spill("video/mp4", SPILL_MIN));
        assert!(!should_spill("image/png", SPILL_MIN));
        assert!(should_spill("image/png", SPILL_MIN_PICTURE));
        assert!(!should_spill("application/x-fontdata", SPILL_MIN_PICTURE), "fonts are parsed from memory");
    }
}
