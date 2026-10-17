//! Reading and writing the files a command names by path.
//!
//! Every such read and write goes through [`read_file`] and [`write_file`], so the automation
//! roots ([`crate::file_access`]) are checked in one place. A command module that called
//! `std::fs` itself would bypass them; a test in `file_access` keeps that from happening.

/// The bytes of the file at `path`, inside the automation read root when there is one.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_file(path: &str) -> Result<Vec<u8>, String> {
    crate::file_access::check_read(path)?;
    std::fs::read(path).map_err(|e| format!("{path}: {e}"))
}

#[cfg(target_arch = "wasm32")]
pub fn read_file(path: &str) -> Result<Vec<u8>, String> {
    Err(format!("{path}: files are opened through the browser on the web"))
}

/// Write `bytes` to the file at `path`, inside the automation write root when there is one.
///
/// The bytes go to a temporary sibling first and are renamed over `path`, so a failed save never
/// truncates the old file. The sibling is in the same folder, so it is inside the root whenever
/// `path` is.
#[cfg(not(target_arch = "wasm32"))]
pub fn write_file(path: &str, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    crate::file_access::check_write(path)?;
    let err = |e: std::io::Error| format!("{path}: {e}");
    // A uniquely named temporary sibling, created exclusively (never following or reusing an
    // existing file or symlink), is renamed over the destination: concurrent saves never share
    // temporary bytes.
    let mut attempts = 0;
    let (tmp, mut file) = loop {
        let tmp = format!("{path}.{}.{}.saving", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed));
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => break (tmp, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempts < 16 => attempts += 1,
            Err(e) => return Err(err(e)),
        }
    };
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    // WHIQ: under `keep_previous`, an existing file is renamed `OLD_<name>` first, never replaced.
    let kept = match written.as_ref() {
        Ok(()) => keep_previous_file(path),
        Err(_) => Ok(None),
    };
    let kept = match kept {
        Ok(k) => k,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    let result = written.and_then(|()| std::fs::rename(&tmp, path));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        // Put the previous file back where it was.
        if let Some(old) = &kept {
            let _ = std::fs::rename(old, path);
        }
        return Err(err(e));
    }
    if let Some(old) = kept {
        KEEP.with(|k| {
            if let Some(list) = k.borrow_mut().as_mut() {
                list.push(Renamed { path: path.to_string(), previous: old });
            }
        });
    }
    sync_parent(path);
    Ok(())
}

/// An existing file a save renamed out of the way ([`keep_previous`]): `path` was written, the file that was
/// there is now at `previous` (`OLD_<name>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Renamed {
    pub path: String,
    pub previous: String,
}

thread_local! {
    /// `Some` inside [`keep_previous`]: the renames made so far.
    static KEEP: std::cell::RefCell<Option<Vec<Renamed>>> = const { std::cell::RefCell::new(None) };
}

/// Run `f` so that no [`write_file`] replaces an existing file: the file already at the path is renamed
/// `OLD_<name>` (`OLD_2_<name>`… when that is taken) first. Returns what was renamed. Nothing is ever deleted.
pub fn keep_previous<T>(f: impl FnOnce() -> T) -> (T, Vec<Renamed>) {
    struct Restore(Option<Vec<Renamed>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prev = self.0.take();
            KEEP.with(|k| *k.borrow_mut() = prev);
        }
    }
    let outer = KEEP.with(|k| k.borrow_mut().replace(Vec::new()));
    let guard = Restore(outer);
    let out = f();
    let renamed = KEEP.with(|k| k.borrow_mut().take()).unwrap_or_default();
    drop(guard);
    (out, renamed)
}

/// The `OLD_` name for `path` that is free: `OLD_<name>`, else `OLD_2_<name>`…
#[cfg(not(target_arch = "wasm32"))]
pub fn old_name_for(path: &str) -> Option<String> {
    let p = std::path::Path::new(path);
    let name = p.file_name()?.to_string_lossy().to_string();
    let dir = p.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    (1..10_000u32).find_map(|n| {
        let candidate = if n == 1 { format!("OLD_{name}") } else { format!("OLD_{n}_{name}") };
        let full = dir.join(&candidate);
        (std::fs::symlink_metadata(&full).is_err()).then(|| full.to_string_lossy().to_string())
    })
}

/// Under [`keep_previous`], move the file at `path` (if any) to its `OLD_` name.
#[cfg(not(target_arch = "wasm32"))]
fn keep_previous_file(path: &str) -> Result<Option<String>, String> {
    if KEEP.with(|k| k.borrow().is_none()) {
        return Ok(None);
    }
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => {}
        _ => return Ok(None),
    }
    let old = old_name_for(path).ok_or_else(|| format!("{path}: no free OLD_ name to keep the previous file"))?;
    crate::file_access::check_write(&old)?;
    std::fs::rename(path, &old).map_err(|e| format!("{path}: can't keep the previous file as {old}: {e}"))?;
    Ok(Some(old))
}

/// Make a finished rename durable (the folder entry), where the platform allows it.
#[cfg(not(target_arch = "wasm32"))]
fn sync_parent(path: &str) {
    #[cfg(unix)]
    if let Some(dir) = std::path::Path::new(path).parent() {
        let dir = if dir.as_os_str().is_empty() { std::path::Path::new(".") } else { dir };
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(target_arch = "wasm32")]
pub fn write_file(path: &str, _bytes: &[u8]) -> Result<(), String> {
    Err(format!("{path}: saving to a path is not available in the browser"))
}

/// The size in bytes of the file at `path`; `None` when it can't be read (or is outside the
/// automation read root, so File › Info doesn't reveal files the agent may not read).
#[cfg(not(target_arch = "wasm32"))]
pub fn file_len(path: &str) -> Option<u64> {
    crate::file_access::check_read(path).ok()?;
    std::fs::metadata(path).ok().map(|m| m.len())
}

#[cfg(target_arch = "wasm32")]
pub fn file_len(_path: &str) -> Option<u64> {
    None
}
