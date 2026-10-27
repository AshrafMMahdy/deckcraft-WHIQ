//! WHIQ: spill folders belong to a process. A second process starting up never deletes the media of
//! one that is alive, a cleaner deleting a file does not take the part, and a part that cannot be
//! read back is an error, never empty bytes. This test sets the spill folder, so it has the binary
//! to itself.
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use deckcraft_model::MediaItem;
use deckcraft_model::spill::{SpillFile, set_spill_base, spill_base, spill_dir, sweep_stale};

#[test]
fn a_live_process_keeps_its_spill_files_and_a_dead_ones_go() {
    let base = std::env::temp_dir().join(format!("dc-spill-folders-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    set_spill_base(Some(base.clone()));
    assert_eq!(spill_base(), base);

    // Another process, alive: a folder with its lock file held, and a file in it.
    let live = base.join("pid-4000001");
    std::fs::create_dir_all(&live).unwrap();
    let live_lock = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(live.join("lock")).unwrap();
    live_lock.try_lock().unwrap();
    std::fs::write(live.join("0.part"), b"a live video").unwrap();
    // Another process, gone: a folder whose lock nobody holds.
    let dead = base.join("pid-4000002");
    std::fs::create_dir_all(&dead).unwrap();
    std::fs::write(dead.join("lock"), b"").unwrap();
    std::fs::write(dead.join("0.part"), b"a dead one's video").unwrap();

    // Our own folder, with a part in it.
    let data: Vec<u8> = (0..500_000u32).map(|i| i as u8).collect();
    let spill = Arc::new(SpillFile::from_bytes(&data).unwrap());
    assert!(spill_dir().starts_with(&base) && spill_dir().ends_with(format!("pid-{}", std::process::id())));
    let item = MediaItem { name: "clip.mp4".into(), content_type: "video/mp4".into(), spill: Some(spill.clone()), ..Default::default() };

    assert_eq!(sweep_stale(Duration::from_secs(0)), 1, "only the dead process's file");
    assert!(live.join("0.part").exists(), "a live process's media is never swept, whatever its age");
    assert!(!dead.exists());
    assert_eq!(item.try_shared_bytes().unwrap().as_slice(), data.as_slice(), "ours is untouched");

    // A cleaner deletes our file behind our back: the handle we hold still reads it.
    #[cfg(unix)]
    {
        std::fs::remove_file(spill.path()).unwrap();
        assert_eq!(item.try_shared_bytes().unwrap().as_slice(), data.as_slice());
        assert!(item.failure().is_none());
    }

    // A file cut short is an error and is said; it is never empty bytes.
    let short = Arc::new(SpillFile::from_bytes(&data).unwrap());
    std::fs::OpenOptions::new().write(true).open(short.path()).unwrap().set_len(10).unwrap();
    let item = MediaItem { name: "cut.mp4".into(), content_type: "video/mp4".into(), spill: Some(short), ..Default::default() };
    assert!(item.try_shared_bytes().is_err() && item.try_bytes().is_err());
    assert!(item.shared_bytes().is_empty(), "the legacy reader gives nothing, and says why:");
    assert!(item.failure().is_some_and(|w| w.contains("cut.mp4")), "{:?}", item.failure());
    let mut sink = Vec::new();
    assert!(item.spill.as_ref().unwrap().copy_to(&mut sink).is_err());
    drop(live_lock);
    std::fs::remove_dir_all(&base).unwrap();
}
