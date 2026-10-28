//! WHIQ: large media kept on disk, and the memory budget the readers size their limits from.
//!
//! A presentation's pictures live in memory ([`crate::MediaItem::data`]); a long video, a sound
//! track or any other large part does not have to. Its bytes are inflated once into a
//! [`SpillFile`] in the spill folder and stream from there into every file saved, so a 600 MB
//! drone video costs disk, not memory, and is never left out of a save (the owner's rule: no tool
//! is limited by file size; protective limits follow the machine's memory and are reported).
//!
//! The spill folder is one per process (`pid-<pid>`) under `DECKCRAFT_SPILL_DIR`, else under
//! `deckcraft/spill` in the user's data folder, with a lock file held for the process's life. A
//! spill file is removed when the last item holding it is dropped; the folders of killed processes
//! (their lock is free) are removed by [`sweep_stale`] (the MCP server runs it at start), never a
//! live process's.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
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

/// The bytes of one media part in a file of the spill folder. The file is removed on drop. It stays
/// OPEN while the part is cold: every read goes through that handle, so a cleaner that deletes the
/// file does not take the part; a read that fails is remembered ([`SpillFile::failure`]) and the
/// callers say so (never a part read as empty).
#[derive(Debug)]
pub struct SpillFile {
    path: PathBuf,
    file: Mutex<std::fs::File>,
    len: u64,
    failure: Mutex<Option<String>>,
}

impl PartialEq for SpillFile {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

static NEXT: AtomicU64 = AtomicU64::new(0);

static BASE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Fix the folder all processes' spill folders are in (before the first part is kept on disk).
pub fn set_spill_base(base: Option<PathBuf>) {
    if let Ok(mut b) = BASE.lock() {
        *b = base;
    }
}

/// The folder all processes' spill folders are in: `DECKCRAFT_SPILL_DIR`, else `deckcraft/spill` in
/// the user's data folder (not the temporary folder, which cleaners empty), else the temporary one.
pub fn spill_base() -> PathBuf {
    if let Some(b) = BASE.lock().ok().and_then(|b| b.clone()) {
        return b;
    }
    match std::env::var_os("DECKCRAFT_SPILL_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => {
            let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
            let data = var("LOCALAPPDATA").or_else(|| var("XDG_DATA_HOME")).or_else(|| var("HOME").map(|h| h.join(".local").join("share")));
            data.unwrap_or_else(std::env::temp_dir).join("deckcraft").join("spill")
        }
    }
}

const LOCK_NAME: &str = "lock";

/// The folder this process claimed and the lock on it, held until it exits.
static CLAIM: std::sync::OnceLock<Option<(PathBuf, std::fs::File)>> = std::sync::OnceLock::new();

/// Waits between tries to take the lock of `pid-<pid>` when someone else holds it for a moment (a
/// sweeper looking at the folder of a dead process that had our PID).
const CLAIM_WAITS_MS: [u64; 7] = [10, 20, 40, 80, 160, 320, 640];

/// Claim a spill folder for process `pid` under `base`: `pid-<pid>` when its lock can be taken
/// (retrying after each of `waits`, the holder being a sweeper that lets go in a moment), else a
/// fresh `pid-<pid>-<random>`. The folder is emptied once locked (what is in it is a crashed
/// process's). A process never runs without the lock of its own folder.
fn claim_folder(base: &Path, pid: u32, waits: &[std::time::Duration]) -> Option<(PathBuf, std::fs::File)> {
    let lock_in = |dir: &Path| -> Option<std::fs::File> {
        std::fs::create_dir_all(dir).ok()?;
        let f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join(LOCK_NAME)).ok()?;
        f.try_lock().ok()?;
        empty_folder(dir);
        Some(f)
    };
    let own = base.join(format!("pid-{pid}"));
    for wait in std::iter::once(&std::time::Duration::ZERO).chain(waits) {
        std::thread::sleep(*wait);
        if let Some(f) = lock_in(&own) {
            return Some((own, f));
        }
    }
    static SALT: AtomicU64 = AtomicU64::new(0);
    for _ in 0..16 {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
        let r = nanos ^ SALT.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed).rotate_left(17);
        let dir = base.join(format!("pid-{pid}-{:08x}", r as u32));
        if let Some(f) = lock_in(&dir) {
            return Some((dir, f));
        }
    }
    None
}

/// The spill folder of THIS process (created on first use): `pid-<pid>` under [`spill_base`], with
/// a lock file held open and locked for as long as the process lives, which is what tells another
/// process the folder is in use. If another process holds that lock (a sweeper, for a moment) the
/// claim waits, then takes `pid-<pid>-<random>`.
pub fn spill_dir() -> PathBuf {
    let base = spill_base();
    let pid = std::process::id();
    match CLAIM.get_or_init(|| {
        let waits: Vec<_> = CLAIM_WAITS_MS.iter().map(|ms| std::time::Duration::from_millis(*ms)).collect();
        claim_folder(&base, pid, &waits)
    }) {
        Some((dir, _)) => dir.clone(),
        None => base.join(format!("pid-{pid}")),
    }
}

/// Remove everything in `dir` but the lock file: what a crashed process with the same PID left.
fn empty_folder(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if e.file_name() == LOCK_NAME {
            continue;
        }
        let p = e.path();
        let _ = if e.file_type().is_ok_and(|t| t.is_dir()) { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
    }
}

impl SpillFile {
    /// Copy `src` into a new spill file, at most `max` bytes: a source that holds more is an
    /// error (`InvalidData`), never a silently shortened copy.
    pub fn from_reader(src: &mut dyn Read, max: u64) -> std::io::Result<SpillFile> {
        let dir = spill_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.part", NEXT.fetch_add(1, Ordering::Relaxed)));
        let f = std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path)?;
        // Removed again on any failure below (the drop).
        let mut sf = SpillFile { path, file: Mutex::new(f), len: 0, failure: Mutex::new(None) };
        let mut buf = vec![0u8; 1 << 20];
        let mut limited = src.take(max.saturating_add(1));
        let f = sf.file.get_mut().map_err(|_| std::io::Error::other("lock poisoned"))?;
        let mut len = 0u64;
        loop {
            let n = limited.read(&mut buf)?;
            if n == 0 {
                break;
            }
            len = len.saturating_add(n as u64);
            if len > max {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("more than the {max} bytes it declared")));
            }
            f.write_all(buf.get(..n).unwrap_or_default())?;
        }
        f.flush()?;
        sf.len = len;
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
    /// Why a read of this part failed, if one did.
    pub fn failure(&self) -> Option<String> {
        self.failure.lock().ok().and_then(|f| f.clone())
    }
    /// Note that reading this part failed.
    pub fn fail(&self, e: &std::io::Error) {
        if let Ok(mut f) = self.failure.lock() {
            f.get_or_insert(format!("a media part kept on disk could not be read back ({}): {e}", self.path.display()));
        }
    }
    /// The file at its start, under the lock.
    fn rewound(&self) -> std::io::Result<std::sync::MutexGuard<'_, std::fs::File>> {
        let mut g = self.file.lock().map_err(|_| std::io::Error::other("lock poisoned"))?;
        g.seek(SeekFrom::Start(0))?;
        Ok(g)
    }
    /// Read the whole file into memory (a caller that has to decode it, e.g. to draw a picture). A
    /// file that holds fewer bytes than it should is an error.
    pub fn read_all(&self) -> std::io::Result<Vec<u8>> {
        let r = (|| {
            let mut g = self.rewound()?;
            let mut v = Vec::with_capacity(self.len.min(1 << 30) as usize);
            (&mut *g).take(self.len.saturating_add(1)).read_to_end(&mut v)?;
            if v.len() as u64 != self.len {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("{} holds {} of {} bytes", self.path.display(), v.len(), self.len),
                ));
            }
            Ok(v)
        })();
        if let Err(e) = &r {
            self.fail(e);
        }
        r
    }
    /// Stream the bytes into `w`; returns the bytes copied.
    pub fn copy_to(&self, w: &mut dyn Write) -> std::io::Result<u64> {
        let mut g = self.rewound()?;
        let n = std::io::copy(&mut (&mut *g).take(self.len), w)?;
        if n != self.len {
            let e = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, format!("{} holds {n} of {} bytes", self.path.display(), self.len));
            self.fail(&e);
            return Err(e);
        }
        Ok(n)
    }
    /// The first `n` bytes (format sniffing). Short when the file cannot be read (see `failure`).
    pub fn head(&self, n: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(n);
        match self.rewound() {
            Ok(mut g) => {
                if let Err(e) = (&mut *g).take(n as u64).read_to_end(&mut out) {
                    self.fail(&e);
                }
            }
            Err(e) => self.fail(&e),
        }
        out
    }
}

impl Drop for SpillFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Remove the spill folders of processes that are gone: a folder whose lock file nobody holds. A
/// folder of a live process (its lock is held) is never touched, whatever its age; one with no lock
/// file yet is left for a minute (it is being made). Files of the old layout (`<pid>-<n>.part` in the
/// base folder) go when the system tells their process is gone (Linux). Returns how many files.
pub fn sweep_stale(_age: std::time::Duration) -> usize {
    let base = spill_base();
    let Ok(rd) = std::fs::read_dir(&base) else { return 0 };
    let mine = spill_dir();
    let mut n = 0;
    for e in rd.flatten() {
        let path = e.path();
        if path == mine {
            continue;
        }
        if path.is_dir() {
            // `held` keeps the lock while the folder goes; `free` says the folder is nobody's.
            let mut held = None;
            let free = match std::fs::OpenOptions::new().read(true).write(true).open(path.join(LOCK_NAME)) {
                // Free when we can take the lock.
                Ok(f) => {
                    let got = f.try_lock().is_ok();
                    held = got.then_some(f);
                    got
                }
                // No lock file (a folder of an older build, or one still being made): free once it
                // is not young. (A directory cannot be opened as a file on Windows, so no handle.)
                Err(_) => !e
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.elapsed().ok())
                    .is_none_or(|d| d < std::time::Duration::from_secs(60)),
            };
            if free {
                n += std::fs::read_dir(&path).map(|r| r.flatten().filter(|x| x.file_name() != LOCK_NAME).count()).unwrap_or(0);
                drop(held);
                let _ = std::fs::remove_dir_all(&path);
            }
        } else if path.extension().is_some_and(|x| x == "part") {
            let pid: Option<u32> = path.file_name().and_then(|f| f.to_str()).and_then(|f| f.split('-').next()).and_then(|p| p.parse().ok());
            let gone = pid.is_some_and(|pid| Path::new("/proc/self").exists() && !Path::new(&format!("/proc/{pid}")).exists());
            if gone && std::fs::remove_file(&path).is_ok() {
                n += 1;
            }
        }
    }
    n
}

/// Remove this process's spill folder (before an exit that skips destructors: a signal).
pub fn remove_own() {
    let _ = std::fs::remove_dir_all(spill_dir());
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
    fn a_claim_while_another_handle_holds_the_lock_still_gets_a_locked_folder() {
        let base = std::env::temp_dir().join(format!("dc-claim-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let pid = 4_000_010;
        let own = base.join(format!("pid-{pid}"));
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(own.join("junk.part"), b"x").unwrap();
        let hold = || {
            let f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(own.join(LOCK_NAME)).unwrap();
            f.try_lock().unwrap();
            f
        };
        // A sweeper holds the lock for good: no wait helps, so the claim takes a fresh folder, locked.
        let sweeper = hold();
        let (dir, held) = claim_folder(&base, pid, &[std::time::Duration::from_millis(5)]).unwrap();
        assert_ne!(dir, own);
        assert!(dir.file_name().unwrap().to_string_lossy().starts_with("pid-4000010-"));
        let second = std::fs::OpenOptions::new().read(true).write(true).open(dir.join(LOCK_NAME)).unwrap();
        assert!(matches!(second.try_lock(), Err(std::fs::TryLockError::WouldBlock)), "the fresh folder is locked by the claimer");
        assert!(own.join("junk.part").exists(), "the folder the other handle holds is left alone");
        drop((held, second));
        // The sweeper lets go in a moment: the retry gets pid-<pid> itself, emptied.
        let t = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            drop(sweeper);
        });
        let waits = [std::time::Duration::from_millis(50); 40];
        let (dir, held) = claim_folder(&base, pid, &waits).unwrap();
        t.join().unwrap();
        assert_eq!(dir, own);
        assert!(!own.join("junk.part").exists());
        drop(held);
        std::fs::remove_dir_all(&base).unwrap();
    }

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
    fn the_folder_of_a_process_that_is_gone_is_swept() {
        let mine = SpillFile::from_bytes(b"still used").unwrap();
        // A killed process: a folder with a lock nobody holds (the id is above the Linux maximum).
        let orphan = spill_base().join("pid-4194399");
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::write(orphan.join("lock"), b"").unwrap();
        std::fs::write(orphan.join("0.part"), b"left by a killed process").unwrap();
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
