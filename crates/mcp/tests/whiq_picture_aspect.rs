//! WHIQ (owner, 2026-11-02): a picture keeps its aspect ratio as in PowerPoint — one side given, the other follows; both
//! given while locked, the width wins and the answer says the height was not used; unlock_aspect: true sets both freely.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use deckcraft_mcp::{Headless, Server};
use serde_json::{Value, json};

/// A 2x1 PNG (a 2:1 picture).
const PNG: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x7b, 0x40, 0xe8, 0xdd, 0x00, 0x00, 0x00, 0x0f, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x38, 0x91, 0x62, 0xc4, 0x25, 0x22, 0x07, 0x00, 0x07, 0xd7, 0x01, 0x9b, 0x2e, 0xd5, 0x2b, 0x69, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82];

fn call(server: &mut Server, name: &str, args: Value) -> Value {
    let line = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}}).to_string();
    let reply: Value = serde_json::from_str(&server.handle_line(&line).unwrap()).unwrap();
    let result = &reply["result"];
    let text = result["content"].as_array().unwrap().iter().find_map(|c| c["text"].as_str()).unwrap_or_default().to_string();
    assert!(result["isError"] != true, "{name}: {text}");
    serde_json::from_str(&text).unwrap_or(Value::String(text))
}

fn bx(server: &mut Server, id: u64) -> [f64; 4] {
    let s = call(server, "inspect_slide", json!({}));
    let sh = s["shapes"].as_array().unwrap().iter().find(|x| x["id"].as_u64() == Some(id)).unwrap().clone();
    let b = sh["box"].as_array().unwrap();
    [b[0].as_f64().unwrap(), b[1].as_f64().unwrap(), b[2].as_f64().unwrap(), b[3].as_f64().unwrap()]
}

fn setup() -> (Server, String) {
    let dir = std::env::temp_dir().join(format!("dc-whiq-aspect-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pic = dir.join("wide.png");
    std::fs::write(&pic, PNG).unwrap();
    let mut server = Server::new(Box::new(Headless::with_document()));
    call(&mut server, "new_presentation", json!({}));
    (server, pic.to_string_lossy().into_owned())
}

#[test]
fn insert_with_a_width_only_keeps_the_ratio_and_the_position() {
    let (mut s, pic) = setup();
    let v = call(&mut s, "insert_picture", json!({"path": pic, "x": 10, "y": 20, "w": 200}));
    let id = v["id"].as_u64().unwrap();
    let b = bx(&mut s, id);
    assert_eq!([b[0], b[1], b[2]], [10.0, 20.0, 200.0]);
    assert!((b[3] - 100.0).abs() < 0.01, "height follows the 2:1 ratio: {b:?}");
    assert!(v["note"].as_str().unwrap().contains("height followed"));
}

#[test]
fn both_sides_while_locked_the_width_wins_and_the_answer_says_so() {
    let (mut s, pic) = setup();
    let id = call(&mut s, "insert_picture", json!({"path": pic}))["id"].as_u64().unwrap();
    let v = call(&mut s, "set_bounds", json!({"id": id, "w": 300, "h": 300}));
    let b = bx(&mut s, id);
    assert!((b[2] - 300.0).abs() < 0.01 && (b[3] - 150.0).abs() < 0.01, "width wins, height follows: {b:?}");
    let note = v["note"].as_str().unwrap();
    assert!(note.contains("was not used") && note.contains("unlock_aspect"), "{note}");
}

#[test]
fn a_width_only_resize_moves_the_height_too() {
    let (mut s, pic) = setup();
    let id = call(&mut s, "insert_picture", json!({"path": pic}))["id"].as_u64().unwrap();
    let r0 = { let b = bx(&mut s, id); b[2] / b[3] };
    call(&mut s, "set_bounds", json!({"id": id, "w": 100}));
    let b = bx(&mut s, id);
    assert!((b[2] / b[3] - r0).abs() < 1e-6 && (b[2] - 100.0).abs() < 0.01, "{b:?}");
}

#[test]
fn unlocked_both_sides_apply_and_the_answer_warns() {
    let (mut s, pic) = setup();
    let id = call(&mut s, "insert_picture", json!({"path": pic}))["id"].as_u64().unwrap();
    let v = call(&mut s, "set_bounds", json!({"id": id, "w": 200, "h": 200, "unlock_aspect": true}));
    let b = bx(&mut s, id);
    assert_eq!([b[2], b[3]], [200.0, 200.0]);
    assert!(v["note"].as_str().unwrap().contains("squeezed or stretched"));
    let v2 = call(&mut s, "insert_picture", json!({"path": pic, "x": 0, "y": 0, "w": 50, "h": 50, "unlock_aspect": true}));
    let b2 = bx(&mut s, v2["id"].as_u64().unwrap());
    assert_eq!(b2, [0.0, 0.0, 50.0, 50.0]);
}

#[test]
fn a_shape_that_is_not_a_picture_resizes_freely_as_before() {
    let (mut s, _pic) = setup();
    let id = call(&mut s, "add_shape", json!({"preset": "rect", "x": 0, "y": 0, "w": 100, "h": 50}))["id"].as_u64().unwrap();
    call(&mut s, "set_bounds", json!({"id": id, "w": 300}));
    let b = bx(&mut s, id);
    assert_eq!([b[2], b[3]], [300.0, 50.0]);
}
