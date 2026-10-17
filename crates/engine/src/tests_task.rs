//! WHIQ: real commands stop at their safe points when their task is cancelled, and leave the
//! session as it was.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::json;

use crate::Session;
use crate::task::{Task, scope};

fn cancelled_task() -> std::sync::Arc<Task> {
    let t = Task::new();
    t.cancel();
    t
}

#[test]
fn a_cancelled_build_adds_no_deck() {
    let mut s = Session::with_new();
    let slides: Vec<_> = (0..20).map(|i| json!({"layout": "bullets", "title": format!("S{i}")})).collect();
    let r = scope(cancelled_task(), || s.execute("deck.build", &json!({"slides": slides})));
    assert!(r.unwrap_err().to_string().contains("cancelled"));
    assert_eq!(s.documents().len(), 1, "no half-built deck");
    // Uncancelled it builds, reporting progress.
    let t = Task::new();
    scope(t.clone(), || s.execute("deck.build", &json!({"slides": [{"title": "a"}, {"title": "b"}]}))).unwrap();
    assert_eq!(t.progress(), (1, 2));
}

#[test]
fn cancelled_exports_write_nothing_past_their_safe_point() {
    let mut s = Session::new();
    crate::sample::open_sample(&mut s).unwrap();
    let dir = std::env::temp_dir().join(format!("deckcraft-task-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pdf = dir.join("c.pdf");
    let r = scope(cancelled_task(), || s.execute("file.export", &json!({"path": pdf.to_str().unwrap()})));
    assert!(r.unwrap_err().to_string().contains("cancelled"));
    assert!(!pdf.exists());
    let png = dir.join("c.png");
    let r = scope(cancelled_task(), || s.execute("file.export", &json!({"path": png.to_str().unwrap(), "all": true, "scale": 0.1})));
    assert!(r.is_err());
    assert!(!dir.join("c1.png").exists());
    let gif = dir.join("c.gif");
    assert!(scope(cancelled_task(), || s.execute("file.export", &json!({"path": gif.to_str().unwrap(), "width": 64}))).is_err());
    assert!(!gif.exists());
    // Reading a presentation stops between slides.
    let pptx = dir.join("c.pptx");
    s.execute("file.export", &json!({"path": pptx.to_str().unwrap()})).unwrap();
    let before = s.documents().len();
    assert!(scope(cancelled_task(), || s.execute("file.open", &json!({"path": pptx.to_str().unwrap()}))).is_err());
    assert_eq!(s.documents().len(), before);
    // A PDF export reports its pages.
    let t = Task::new();
    scope(t.clone(), || s.execute("file.export", &json!({"path": pdf.to_str().unwrap(), "dpi": 36}))).unwrap();
    let (done, total) = t.progress();
    assert!(total >= 8 && done + 1 == total, "{done}/{total}");
    let _ = std::fs::remove_dir_all(&dir);
}
