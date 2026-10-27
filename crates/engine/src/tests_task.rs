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
    // A cancelled save writes nothing and leaves the document unsaved.
    let saved = dir.join("cancelled.pptx");
    assert!(scope(cancelled_task(), || s.execute("file.save", &json!({"path": saved.to_str().unwrap()}))).is_err());
    assert!(!saved.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Run `cmd` under a fresh task; its stages and final overall progress.
fn staged(s: &mut Session, cmd: &str, p: serde_json::Value) -> (Vec<String>, f64) {
    let t = Task::new();
    scope(t.clone(), || s.execute(cmd, &p)).unwrap();
    (t.stages(), t.overall())
}

#[test]
fn every_job_reports_its_stages_and_progress() {
    let mut s = Session::new();
    crate::sample::open_sample(&mut s).unwrap();
    let dir = std::env::temp_dir().join(format!("deckcraft-progress-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = |n: &str| dir.join(n).to_str().unwrap().to_string();
    let done = |(stages, overall): (Vec<String>, f64), want: &[&str]| {
        for w in want {
            assert!(stages.iter().any(|s| s == w), "stage {w} missing from {stages:?}");
        }
        assert!((overall - 1.0).abs() < 1e-9, "{stages:?} ended at {overall}");
    };
    done(staged(&mut s, "file.export", json!({"path": path("p.pdf"), "dpi": 36})), &["writing PDF pages", "writing the file"]);
    done(staged(&mut s, "file.export", json!({"path": path("p.png"), "all": true, "scale": 0.1})), &["rendering slides"]);
    done(staged(&mut s, "file.export", json!({"path": path("p.gif"), "width": 64})), &["rendering GIF frames", "writing the file"]);
    done(staged(&mut s, "file.export", json!({"path": path("p.pptx")})), &["writing slides", "compressing the package", "writing the file"]);
    done(staged(&mut s, "file.export", json!({"path": path("p.deckcraft")})), &["writing the presentation", "writing the file"]);
    done(staged(&mut s, "file.export", json!({"path": path("p.txt")})), &["writing the file"]);
    done(staged(&mut s, "file.save", json!({"path": path("saved.pptx")})), &["writing slides", "compressing the package", "writing the file"]);
    // Opening: the file by bytes, then its slides; the host adds it to the session last.
    for f in ["p.pptx", "p.deckcraft"] {
        let t = Task::new();
        let loaded = scope(t.clone(), || crate::cmd::file::read_presentation_file(&path(f))).unwrap();
        assert!(!loaded.doc.slides.is_empty());
        let stages = t.stages();
        assert_eq!(stages.first().map(String::as_str), Some("reading the file"), "{stages:?}");
        // A .pptx is read slide by slide from the file; a .deckcraft as one presentation.
        let body = if f == "p.pptx" { "reading slides" } else { "reading the presentation" };
        assert!(stages.iter().any(|s| s == body), "{stages:?}");
        assert_eq!(stages.last().map(String::as_str), Some("adding it to the session"), "{stages:?}");
        assert!((t.overall() - 0.9).abs() < 1e-9);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
