//! WHIQ (owner, 2026-11-05, "Undo the call"): one call that fails half way leaves the presentation
//! exactly as it was before it, and the error says nothing of it was applied. A batch keeps the
//! steps before an error (each step whole or nothing) and is undone as a whole only on a cancel.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Value, json};

use crate::state::Config;
use crate::{Headless, Server};

fn call(s: &mut Server, name: &str, args: Value) -> Value {
    let line = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}}).to_string();
    let out: Value = serde_json::from_str(&s.handle_line(&line).expect("a response")).expect("json");
    out["result"].clone()
}

fn ok(s: &mut Server, name: &str, args: Value) -> Value {
    let r = call(s, name, args);
    assert_eq!(r["isError"], false, "{name}: {r}");
    let text = r["content"].as_array().unwrap().iter().find_map(|c| c["text"].as_str()).unwrap_or_default().to_string();
    serde_json::from_str(&text).unwrap_or(Value::String(text))
}

fn err(s: &mut Server, name: &str, args: Value) -> (String, Value) {
    let r = call(s, name, args);
    assert_eq!(r["isError"], true, "{name} should fail: {r}");
    (r["content"][0]["text"].as_str().unwrap().to_string(), r["structuredContent"]["error"].clone())
}

fn slide(s: &mut Server) -> Value {
    ok(s, "inspect_slide", json!({}))
}

fn shape(s: &mut Server, id: u64) -> Value {
    slide(s)["shapes"].as_array().unwrap().iter().find(|x| x["id"].as_u64() == Some(id)).unwrap().clone()
}

fn slides(s: &mut Server) -> usize {
    ok(s, "inspect_document", json!({}))["slides"].as_array().unwrap().len()
}

fn with_shape() -> (Server, u64) {
    let mut s = Server::new(Box::new(Headless::new()));
    ok(&mut s, "new_presentation", json!({}));
    let id = ok(&mut s, "add_shape", json!({"preset": "rect", "x": 100, "y": 100, "w": 200, "h": 100, "text": "Box"}))["id"].as_u64().unwrap();
    (s, id)
}

#[test]
fn format_shape_that_fails_half_way_keeps_nothing_and_says_so() {
    let (mut s, id) = with_shape();
    let before = shape(&mut s, id);
    // The fill and the line go through, then the text colour cannot: the fill used to stay.
    let (text, e) = err(&mut s, "format_shape", json!({"id": id, "fill": "FF0000", "line": "00FF00", "color": "notacolor"}));
    assert!(text.contains("nothing of this call was applied — send it again (fixed)"), "{text}");
    assert_eq!(e["code"], "invalid_argument", "{e}");
    assert_eq!(shape(&mut s, id), before, "the shape is as it was before the call");
    // The same call, fixed, applies whole.
    ok(&mut s, "format_shape", json!({"id": id, "fill": "FF0000", "color": "0000FF"}));
    assert_ne!(shape(&mut s, id), before);
}

#[test]
fn add_text_box_whose_formatting_fails_adds_nothing() {
    let (mut s, _) = with_shape();
    let n = slide(&mut s)["shapes"].as_array().unwrap().len();
    let (text, _) = err(&mut s, "add_text_box", json!({"x": 10, "y": 10, "w": 200, "text": "Hello", "color": "notacolor"}));
    assert!(text.contains("nothing of this call was applied"), "{text}");
    assert_eq!(slide(&mut s)["shapes"].as_array().unwrap().len(), n, "the box the call inserted is gone");
}

#[test]
fn a_batch_error_keeps_the_steps_before_it_and_nothing_of_the_failed_step() {
    let (mut s, id) = with_shape();
    let before = shape(&mut s, id);
    let n = slides(&mut s);
    let (text, e) = err(
        &mut s,
        "batch",
        json!({"commands": [
            {"tool": "add_slide", "args": {"title": "Kept"}},
            {"tool": "format_shape", "args": {"id": id, "fill": "FF0000", "color": "notacolor"}, "slide": 0},
            {"tool": "add_slide", "args": {"title": "Never run"}}
        ]}),
    );
    assert_eq!(slides(&mut s), n + 1, "the step before the error is kept, the one after it never ran");
    ok(&mut s, "go_to_slide", json!({"index": 0}));
    assert_eq!(shape(&mut s, id), before, "nothing of the failed step is applied");
    let reason = e["results"][1]["error"]["reason"].as_str().unwrap();
    assert!(reason.contains("nothing of this step was applied"), "{reason}");
    assert!(text.contains("the steps before it were kept"), "{text}");
    // The description says the same.
    let line = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}).to_string();
    let tools: Value = serde_json::from_str(&s.handle_line(&line).unwrap()).unwrap();
    let batch = tools["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "batch").unwrap()["description"].clone();
    let d = batch.as_str().unwrap();
    assert!(d.contains("the steps before the error are kept") && d.contains("A cancel (stop) undoes the whole batch"), "{d}");
}

fn hooked() -> (Server, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("dc-whiq-undo-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = Config { test_hooks: true, write_root: Some(dir.clone()), ..Config::default() };
    (Server::with_config(Box::new(Headless::new()), config), dir)
}

#[test]
fn a_call_that_panics_after_its_work_is_undone_before_the_work_is_saved() {
    let (mut s, dir) = hooked();
    ok(&mut s, "new_presentation", json!({}));
    ok(&mut s, "add_slide", json!({"title": "Keep me"}));
    // `_panic` (test hooks): the call runs, then fails inside the server.
    let (text, e) = err(&mut s, "add_slide", json!({"title": "Not me", "_panic": true}));
    assert_eq!(e["code"], "internal", "{e}");
    assert!(text.contains("nothing of this call was applied") && !text.contains("retry the call"), "{text}");
    assert_eq!(slides(&mut s), 2, "the slide the call added is gone");
    let saved = e["recovered_path"].as_str().unwrap();
    let reopened = deckcraft_engine::cmd::file::read_presentation_file(saved).unwrap();
    assert_eq!(reopened.doc.slides.len(), 2, "the auto-saved file holds the presentation as it was before the call");
    // A call that opened a presentation and then failed leaves it closed.
    let (text, _) = err(&mut s, "build_deck", json!({"title": "Half", "_panic": true}));
    assert!(text.contains("nothing of this call was applied"), "{text}");
    assert_eq!(ok(&mut s, "server_status", json!({}))["documents"].as_array().unwrap().len(), 1, "no second deck stays open");
    assert_eq!(slides(&mut s), 2);
    let _ = std::fs::remove_dir_all(&dir);
}
