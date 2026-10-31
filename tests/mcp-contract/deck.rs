//! WHIQ deck work over MCP, against the built binary: build_deck's nine layouts with notes, table
//! auto-paging, the overlay vocabulary (with a regression deck rendered to PNG), theme adoption
//! from a company template with its masters and layouts, `.potx`, export, and in-place editing
//! that keeps media, animations, embedded fonts and unmodelled parts.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io::Write as _;
use std::path::{Path, PathBuf};

use common::{Mcp, png_size, scratch, zip_bytes, zip_names, zip_text};
use serde_json::{Value, json};

fn start(dir: &Path) -> Mcp {
    let d = dir.to_str().unwrap();
    Mcp::start(dir, &["--automation-read-root", d, "--automation-write-root", d, "--author", "Deck Test"], &[])
}

/// A plain picture to put on slides: a rendered blank slide.
fn picture(m: &mut Mcp, dir: &Path, name: &str) -> PathBuf {
    m.ok("new_presentation", json!({"blank": false}));
    let p = dir.join(name);
    m.ok("render_slide", json!({"path": p.to_str().unwrap(), "scale": 0.5}));
    m.ok("close_presentation", json!({}));
    p
}

fn slides(m: &mut Mcp) -> Vec<Value> {
    m.ok("inspect_document", json!({}))["slides"].as_array().unwrap().clone()
}

fn shapes(m: &mut Mcp, index: usize) -> Vec<Value> {
    m.ok("inspect_slide", json!({"index": index}))["shapes"].as_array().unwrap().clone()
}

fn named<'a>(shapes: &'a [Value], name: &str) -> Vec<&'a Value> {
    shapes.iter().filter(|s| s["name"].as_str().is_some_and(|n| n.starts_with(name))).collect()
}

fn nine_layouts() -> Value {
    json!({
        "title": "Site Logistics Review",
        "subtitle": "Week 3",
        "notes": "Title notes",
        "slides": [
            {"layout": "bullets", "title": "Summary", "paragraphs": ["Where we are."], "bullets": ["**Scope** agreed", "  phase 2 *next*", "Risks `R-12`"], "notes": "n-bullets"},
            {"layout": "two-column", "title": "Options", "left": {"heading": "A", "bullets": ["cheap", "slow"]}, "right": ["fast", "dear"], "notes": "n-two"},
            {"layout": "table", "title": "Register", "table": [["ID", "Item", "Owner"], ["R-1", "Crane", "Site"], ["R-2", "Gate", "Ops"]], "notes": "n-table"},
            {"layout": "callout", "title": "Key message", "text": "Deliver phase 1 by March.", "bullets": ["because"], "notes": "n-callout"},
            {"layout": "kpi", "title": "Numbers", "bullets": ["Progress: 62%", "Spend: $4.2M", "Days left: 118"], "notes": "n-kpi"},
            {"layout": "section", "title": "Part two", "notes": "n-section"},
            {"layout": "quote", "title": "Client", "quote": "Safety first, then speed.", "attribution": "Site director", "notes": "n-quote"},
            {"layout": "chart", "title": "Output", "table": [["Month", "Plan", "Actual"], ["Jan", "10", "8"], ["Feb", "20", "22"], ["Mar", "30", "27"]], "notes": "n-chart"},
            {"layout": "image", "title": "Plan", "image": {"path": "pic.png", "position": "full", "caption": "North up"}, "notes": "n-image"},
            {"layout": "image", "title": "Left", "image": {"path": "pic.png", "position": "left"}, "bullets": ["beside"], "notes": "n-left"},
            {"layout": "image", "title": "Right", "image": {"path": "pic.png", "position": "right"}, "bullets": ["beside"], "notes": "n-right"},
            {"layout": "bullets", "title": "With logo", "image": {"path": "pic.png", "position": "logo"}, "bullets": ["logo top right"], "notes": "n-logo"},
            {"layout": "Two Columns", "title": "Forgiving name", "bullets": ["a", "b", "c", "d"], "notes": "n-forgiving"}
        ],
        "path": "nine.pptx"
    })
}

#[test]
fn the_nine_layouts_each_with_speaker_notes() {
    let dir = scratch("nine");
    let mut m = start(&dir);
    picture(&mut m, &dir, "pic.png");
    let r = m.ok("build_deck", nine_layouts());
    assert_eq!(r["slides"], 14, "{r}");
    assert!(r["warnings"].as_array().unwrap().is_empty(), "{r}");
    let s = slides(&mut m);
    let notes: Vec<&str> = s.iter().map(|x| x["notes"].as_str().unwrap()).collect();
    assert_eq!(notes[0], "Title notes");
    for (i, want) in [
        "n-bullets",
        "n-two",
        "n-table",
        "n-callout",
        "n-kpi",
        "n-section",
        "n-quote",
        "n-chart",
        "n-image",
        "n-left",
        "n-right",
        "n-logo",
        "n-forgiving",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(notes[i + 1], *want, "slide {}", i + 1);
    }
    let titles: Vec<&str> = s.iter().map(|x| x["title"].as_str().unwrap()).collect();
    assert_eq!(titles[1], "Summary", "titles are title placeholders: {titles:?}");

    // Each layout drew its own shapes, where the platform puts them (points on 960 × 540).
    let sh = shapes(&mut m, 1);
    let bullets = named(&sh, "Bullets")[0];
    assert!(bullets["text"].as_str().unwrap().contains("Scope agreed"), "markdown markers removed: {bullets}");
    assert_eq!(named(&shapes(&mut m, 2), "Left Column").len() + named(&shapes(&mut m, 2), "Right Column").len(), 2);
    assert!(named(&shapes(&mut m, 2), "Left Column")[0]["text"].as_str().unwrap().starts_with('A'));
    assert_eq!(shapes(&mut m, 3).iter().filter(|x| x["kind"] == "table").count(), 1);
    assert!(named(&shapes(&mut m, 4), "Callout").iter().any(|x| x["text"] == "Deliver phase 1 by March."));
    let kpi = shapes(&mut m, 5);
    assert_eq!(named(&kpi, "KPI Card").len(), 3);
    let values: Vec<&str> = named(&kpi, "KPI Value").iter().map(|x| x["text"].as_str().unwrap()).collect();
    assert_eq!(values, ["62%", "$4.2M", "118"]);
    let quote = shapes(&mut m, 7).into_iter().find(|x| x["name"] == "Quote").unwrap();
    assert!(quote["text"].as_str().unwrap().contains("— Site director"), "{quote}");
    let chart = shapes(&mut m, 8).into_iter().find(|x| x["kind"] == "chart").unwrap();
    assert_eq!(chart["chart"]["series"].as_array().unwrap().len(), 2, "{chart}");
    assert_eq!(chart["chart"]["categories"], json!(["Jan", "Feb", "Mar"]));
    let pics = |m: &mut Mcp, i: usize| shapes(m, i).into_iter().filter(|x| x["kind"] == "picture" && x["name"] != "Logo").collect::<Vec<_>>();
    let full = &pics(&mut m, 9)[0]["box"];
    assert!((full[0].as_f64().unwrap() - 43.2).abs() < 0.5 && (full[2].as_f64().unwrap() - 873.6).abs() < 0.5, "{full}");
    let left = &pics(&mut m, 10)[0]["box"];
    let right = &pics(&mut m, 11)[0]["box"];
    assert!(left[0].as_f64().unwrap() < 100.0 && right[0].as_f64().unwrap() > 450.0, "{left} {right}");
    let logo = shapes(&mut m, 12).into_iter().find(|x| x["kind"] == "picture").unwrap();
    assert!(logo["box"][0].as_f64().unwrap() > 800.0 && logo["box"][2].as_f64().unwrap() < 100.0, "{logo}");
    assert_eq!(named(&shapes(&mut m, 13), "Left Column").len(), 1, "`Two Columns` is two-column");

    // In the file: every slide has its notes part and a slide number field.
    let file = dir.join("nine.pptx");
    let names = zip_names(&file);
    for i in 1..=14 {
        assert!(names.contains(&format!("ppt/notesSlides/notesSlide{i}.xml")), "notes part {i}");
    }
    assert!(zip_text(&file, "ppt/slides/slide2.xml").contains("type=\"slidenum\""));
    // It opens again with the same content.
    m.ok("open_presentation", json!({"path": "nine.pptx"}));
    let again = slides(&mut m);
    assert_eq!(again.len(), 14);
    assert_eq!(again[8]["notes"], "n-chart");
    // Arabic slides are right-to-left in the file.
    m.ok("build_deck", json!({"slides": [{"layout": "bullets", "title": "ملخص", "bullets": ["بند أول"]}], "path": "ar.pptx"}));
    let xml = zip_text(&dir.join("ar.pptx"), "ppt/slides/slide1.xml");
    assert!(xml.contains("rtl=\"1\"") && xml.contains("algn=\"r\""), "{xml}");
}

#[test]
fn a_long_table_pages_over_slides_with_its_header() {
    let dir = scratch("paging");
    let mut m = start(&dir);
    let mut rows = vec![json!(["ID", "Description", "Owner"])];
    rows.extend((0..60).map(|i| json!([format!("R-{i:03}"), format!("Risk item number {i}"), "Site team"])));
    let r = m.ok("build_deck", json!({"slides": [{"layout": "table", "title": "Risk register", "table": rows, "notes": "the register"}]}));
    let n = r["slides"].as_u64().unwrap() as usize;
    assert!(n >= 3, "60 rows need several slides: {r}");
    let s = slides(&mut m);
    assert_eq!(s[0]["title"], format!("Risk register (1/{n})"));
    assert_eq!(s[0]["notes"], "the register");
    let mut seen = 0;
    for i in 0..n {
        let t = shapes(&mut m, i).into_iter().find(|x| x["kind"] == "table").unwrap();
        let cells = t["table"]["cells"].as_array().unwrap();
        assert_eq!(cells[0], json!(["ID", "Description", "Owner"]), "header repeated on page {i}");
        seen += cells.len() - 1;
        let b = &t["box"];
        assert!(b[1].as_f64().unwrap() + b[3].as_f64().unwrap() <= 540.0, "page {i} fits the slide: {b}");
    }
    assert_eq!(seen, 60, "every row once");
    // A fixed page size.
    let r = m.ok("build_deck", json!({"slides": [{"layout": "table", "title": "T", "table": rows, "rowsPerSlide": 20}]}));
    assert_eq!(r["slides"], 3);
}

/// Mean colour of the pixels within `r` of (x, y).
fn around(img: &image::RgbaImage, x: f64, y: f64, r: i64) -> Vec<[u8; 3]> {
    let mut v = vec![];
    for dy in -r..=r {
        for dx in -r..=r {
            let (px, py) = (x as i64 + dx, y as i64 + dy);
            if px >= 0 && py >= 0 && (px as u32) < img.width() && (py as u32) < img.height() {
                let p = img.get_pixel(px as u32, py as u32);
                v.push([p[0], p[1], p[2]]);
            }
        }
    }
    v
}

fn overlay_deck() -> Value {
    json!({"titleSlide": false, "slides": [{
        "layout": "image", "title": "Overlay regression", "image": {"path": "plain.png"}, "notes": "overlays",
        "overlays": [
            {"kind": "rect", "x": 10, "y": 10, "w": 25, "h": 20, "color": "#2B6CB0", "label": "Storage"},
            {"kind": "ellipse", "x": 55, "y": 15, "w": 30, "h": 40, "color": "#C53030", "dashed": true, "label": "Crane radius"},
            {"kind": "line", "x": 5, "y": 90, "w": 95, "h": 60, "color": "#2F855A", "arrow": "both"},
            {"kind": "path", "points": "5,50 20,45 35,55 50,48", "color": "#DD6B20", "arrow": "end", "label": "Haul road"},
            {"kind": "path", "points": [[60, 70], [80, 65], [90, 85], [65, 90]], "color": "#6B46C1", "filled": true, "label": "Compound"},
            {"kind": "text", "x": 40, "y": 92, "w": 30, "h": 6, "color": "#1A202C", "label": "Gate 1"}
        ]
    }], "path": "overlays.pptx"})
}

/// The overlay regression deck: every mark kind, solid and dashed, arrowed, open and filled,
/// labelled, rendered to PNG and probed where each mark must be: in percent of the visible
/// picture (a 16:9 picture letterboxed in the 873.6 × 288 slot: 224, 122.4, 512 × 288).
#[test]
fn overlay_regression_deck_renders_every_mark() {
    let dir = scratch("overlays");
    let mut m = start(&dir);
    picture(&mut m, &dir, "plain.png");
    let r = m.ok("build_deck", overlay_deck());
    assert!(r["warnings"].as_array().unwrap().is_empty(), "{r}");
    let png = dir.join("overlay-regression.png");
    m.ok("render_slide", json!({"slide": 0, "scale": 1, "path": png.to_str().unwrap()}));
    let bytes = std::fs::read(&png).unwrap();
    // Kept for people to look at.
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("overlay-regression.png");
    let _ = std::fs::write(&out, &bytes);
    let _ = std::fs::copy(dir.join("overlays.pptx"), PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("overlay-regression.pptx"));
    let img = image::load_from_memory(&bytes).unwrap().to_rgba8();
    assert_eq!((img.width(), img.height()), (960, 540));
    let (bx, by, bw, bh) = (224.0, 122.4, 512.0, 288.0);
    let at = |px: f64, py: f64| (bx + px / 100.0 * bw, by + py / 100.0 * bh);
    let any = |pts: &[[u8; 3]], f: &dyn Fn([u8; 3]) -> bool| pts.iter().any(|p| f(*p));
    let all = |pts: &[[u8; 3]], f: &dyn Fn([u8; 3]) -> bool| pts.iter().all(|p| f(*p));
    let (r_, g_, b_) = (|p: [u8; 3]| i32::from(p[0]), |p: [u8; 3]| i32::from(p[1]), |p: [u8; 3]| i32::from(p[2]));
    // Filled rectangle: tinted blue inside (away from its label).
    let (x, y) = at(13.0, 12.0);
    assert!(all(&around(&img, x, y, 2), &|p| b_(p) > r_(p) + 25), "rect fill at {x},{y}");
    // Dashed ellipse: an outline, not filled; its left edge is red somewhere along a dash.
    let (x, y) = at(70.0, 20.0);
    assert!(all(&around(&img, x, y, 2), &|p| r_(p) > 200 && g_(p) > 200 && b_(p) > 200), "dashed ellipse is not filled");
    let (x, y) = at(55.0, 35.0);
    assert!(any(&around(&img, x, y, 6), &|p| r_(p) > g_(p) + 60 && r_(p) > b_(p) + 60), "ellipse outline at {x},{y}");
    // Straight line with arrows: green at its middle.
    let (x, y) = at(50.0, 75.0);
    assert!(any(&around(&img, x, y, 3), &|p| g_(p) > r_(p) + 40 && g_(p) > b_(p) + 10), "line at {x},{y}");
    // Open freeform route: orange along its first leg.
    let (x, y) = at(12.5, 47.5);
    assert!(any(&around(&img, x, y, 3), &|p| r_(p) > b_(p) + 80), "route at {x},{y}");
    // Filled freeform compound: tinted purple inside.
    let (x, y) = at(75.0, 75.0);
    assert!(all(&around(&img, x, y, 2), &|p| b_(p) > g_(p) + 20 && r_(p) > g_(p)), "compound fill at {x},{y}");
    // Text label: dark letters in its box.
    let (x, y) = at(45.0, 94.5);
    assert!(any(&around(&img, x, y, 8), &|p| r_(p) + g_(p) + b_(p) < 250), "label at {x},{y}");

    // The shapes are native and editable: geometry and line ends in the file.
    let xml = zip_text(&dir.join("overlays.pptx"), "ppt/slides/slide1.xml");
    for needle in [
        "<a:prstDash val=\"dash\"/>",
        "<a:tailEnd type=\"triangle\"",
        "<a:headEnd type=\"triangle\"",
        "<a:custGeom>",
        "<a:close/>",
        "prst=\"ellipse\"",
        "prst=\"line\"",
        "<a:alpha val=\"35000\"/>",
    ] {
        assert!(xml.contains(needle), "{needle}");
    }
    let sh = shapes(&mut m, 0);
    assert_eq!(sh.iter().filter(|s| s["name"].as_str().unwrap().starts_with("Overlay")).count(), 6 + 4, "6 marks, 4 labels");
}

/// A drawing with the Polaris scan's aspect (4967 × 3509), letterboxed in a full-width slot.
fn drawing(dir: &Path) -> PathBuf {
    let p = dir.join("drawing.png");
    image::RgbImage::from_pixel(1415, 1000, image::Rgb([250, 250, 245])).save(&p).unwrap();
    p
}

fn boxed(shapes: &[Value], name: &str) -> [f64; 4] {
    let s = shapes.iter().find(|s| s["name"] == name).unwrap_or_else(|| panic!("no {name}"));
    let b = s["box"].as_array().unwrap();
    [0, 1, 2, 3].map(|k| b[k].as_f64().unwrap())
}

fn assert_box(got: [f64; 4], want: [f64; 4], what: &str) {
    assert!(got.iter().zip(want).all(|(g, w)| (g - w).abs() < 0.05), "{what}: {got:?}, expected {want:?}");
}

/// D1: build_deck draws overlays in percent of the visible picture, not of its slot, when the
/// picture is letterboxed; `relativeTo: box` at slide, image or overlay level keeps the slot.
#[test]
fn build_deck_overlays_land_on_the_visible_picture_of_a_letterboxed_drawing() {
    let dir = scratch("overlay-letterbox");
    let mut m = start(&dir);
    drawing(&dir);
    let marks = || {
        json!([
            {"kind": "rect", "x": 10, "y": 10, "w": 20, "h": 20, "label": "TP-001"},
            {"kind": "rect", "x": 10, "y": 10, "w": 20, "h": 20, "label": "Slot", "relativeTo": "box"}
        ])
    };
    let r = m.ok("build_deck", json!({"titleSlide": false, "slides": [
        {"layout": "image", "title": "Default", "image": {"path": "drawing.png", "position": "full"}, "overlays": marks()},
        {"layout": "image", "title": "Slide box", "relativeTo": "box", "image": {"path": "drawing.png", "position": "full"}, "overlays": marks()},
        {"layout": "image", "title": "Image box", "image": {"path": "drawing.png", "position": "full", "relativeTo": "box"}, "overlays": marks()},
        {"layout": "image", "title": "Slide box, image image", "relativeTo": "box", "image": {"path": "drawing.png", "relativeTo": "image"}, "overlays": marks()}
    ]}));
    assert!(r["warnings"].as_array().unwrap().is_empty(), "{r}");
    // Slot 43.2, 122.4, 873.6 × 288; the drawing (1.415:1) inside it: 276.24, 122.4, 407.52 × 288.
    let slot = [43.2 + 87.36, 122.4 + 28.8, 174.72, 57.6];
    let visible = [276.24 + 40.752, 122.4 + 28.8, 81.504, 57.6];
    for (k, first) in [(0, visible), (1, slot), (2, slot), (3, visible)] {
        let sh = shapes(&mut m, k);
        assert_box(boxed(&sh, "Picture"), [43.2, 122.4, 873.6, 288.0], "the picture keeps its slot");
        assert_box(boxed(&sh, "Overlay: TP-001"), first, &format!("slide {k}"));
        assert_box(boxed(&sh, "Overlay: Slot"), slot, &format!("slide {k}: the mark's own relativeTo"));
    }
}

/// D2: add_overlays {relativeTo: image} on a letterboxed drawing measures the visible picture:
/// every mark keeps its width (it used to collapse to a point at the picture's left edge).
#[test]
fn add_overlays_relative_to_the_image_keeps_each_mark_on_the_drawing() {
    let dir = scratch("overlay-image");
    let mut m = start(&dir);
    drawing(&dir);
    m.ok("build_deck", json!({"titleSlide": false, "slides": [{"layout": "image", "title": "Plan", "image": {"path": "drawing.png"}}]}));
    let sh = shapes(&mut m, 0);
    let id = sh.iter().find(|s| s["name"] == "Picture").unwrap()["id"].as_u64().unwrap();
    m.ok(
        "add_overlays",
        json!({"id": id, "relativeTo": "image", "overlays": [
            {"kind": "rect", "x": 0, "y": 0, "w": 100, "h": 100, "label": "Whole"},
            {"kind": "rect", "x": 8, "y": 12, "w": 22, "h": 14, "label": "TP-001 Storage compound"},
            {"kind": "line", "x": 5, "y": 85, "w": 92, "h": 85, "label": "Haul road"}
        ]}),
    );
    let sh = shapes(&mut m, 0);
    let (ix, iy, iw, ih) = (276.24, 122.4, 407.52, 288.0);
    assert_box(boxed(&sh, "Overlay: Whole"), [ix, iy, iw, ih], "the whole drawing");
    assert_box(boxed(&sh, "Overlay: TP-001 Storage compound"), [ix + 0.08 * iw, iy + 0.12 * ih, 0.22 * iw, 0.14 * ih], "a compound");
    let line = boxed(&sh, "Overlay Line: Haul road");
    assert_box(line, [ix + 0.05 * iw, iy + 0.85 * ih, 0.87 * iw, 0.0], "a haul road");
    // The default stays the picture box.
    m.ok("add_overlays", json!({"id": id, "overlays": [{"kind": "rect", "x": 0, "y": 0, "w": 100, "h": 100, "label": "Box"}]}));
    assert_box(boxed(&shapes(&mut m, 0), "Overlay: Box"), [43.2, 122.4, 873.6, 288.0], "relativeTo box by default");
}

/// The same vocabulary on a picture already on a slide (in place), from the platform's table rows.
#[test]
fn overlays_go_on_an_existing_picture_from_the_platform_table() {
    let dir = scratch("overlay-existing");
    let mut m = start(&dir);
    let pic = picture(&mut m, &dir, "p.png");
    m.ok("new_presentation", json!({}));
    let id = m.ok("insert_picture", json!({"path": pic.to_str().unwrap(), "x": 100, "y": 100, "w": 400, "h": 225}))["id"].as_u64().unwrap();
    let r = m.ok(
        "add_overlays",
        json!({"id": id, "rows": [
            ["shape", "x", "y", "w", "h", "colour", "label", "points", "style"],
            ["rect", "10", "10", "20", "20", "2b6cb0", "TP-001", "", "dashed"],
            ["line", "0", "0", "100", "100", "C05621", "", "", "arrows"],
            ["path", "", "", "", "", "6B46C1", "Route", "5,50 12,48 20,55", "arrow"],
            ["text", "50", "50", "", "", "000000", "Note", "", ""]
        ]}),
    );
    assert_eq!(r["ids"].as_array().unwrap().len(), 6, "{r}");
    let sh = shapes(&mut m, 0);
    let rect = sh.iter().find(|s| s["name"] == "Overlay: TP-001").unwrap();
    assert_eq!(rect["box"], json!([140.0, 122.5, 80.0, 45.0]), "percent of the picture box");
    let line = sh.iter().find(|s| s["name"] == "Overlay Line").unwrap();
    assert_eq!(line["box"], json!([100.0, 100.0, 400.0, 225.0]));
    // Undo removes them in one step.
    m.ok("run_command", json!({"command": "edit.undo"}));
    assert!(!shapes(&mut m, 0).iter().any(|s| s["name"].as_str().unwrap().starts_with("Overlay")));
    let e = m.err("add_overlays", json!({"id": id, "overlays": []}));
    assert_eq!(e["code"], "invalid_argument", "{e}");
}

/// A company template: its theme (colours, fonts), masters with their logo and decorations, and
/// its layouts, so a deck built from it looks like it; `.potx` saved with the template type.
#[test]
fn a_deck_built_from_a_company_template_takes_its_theme_masters_and_layouts() {
    let dir = scratch("template");
    let mut m = start(&dir);
    let logo = picture(&mut m, &dir, "logo.png");
    // The company's template: own palette and fonts, a logo on the master, renamed layouts.
    m.ok("build_deck", json!({"titleSlide": false, "theme": {"accent": "#C0392B", "accent2": "#27AE60", "heading": "#2C3E50", "fontHead": "Merriweather", "fontBody": "Lato"}, "slides": [{"layout": "bullets", "title": "x", "bullets": ["y"]}]}));
    m.ok("run_command", json!({"command": "view.slideMaster", "params": {"master": 0}}));
    m.ok("insert_picture", json!({"path": logo.to_str().unwrap(), "x": 840, "y": 20, "w": 100, "h": 56}));
    m.ok("run_command", json!({"command": "master.renameLayout", "params": {"name": "ACME Title", "master": 0, "layout": 0}}));
    m.ok("run_command", json!({"command": "view.closeMaster"}));
    m.ok("save_presentation", json!({"path": "acme.potx"}));
    let potx = dir.join("acme.potx");
    let ct = zip_text(&potx, "[Content_Types].xml");
    assert!(ct.contains("application/vnd.openxmlformats-officedocument.presentationml.template.main+xml"), "{ct}");
    let master_xml = zip_text(&potx, "ppt/slideMasters/slideMaster1.xml");
    assert!(master_xml.contains("<p:pic>"), "the logo is on the template's master");
    let tdoc = m.ok("inspect_document", json!({}));

    // A deck built from it.
    let r = m.ok(
        "build_deck",
        json!({"template": "acme.potx", "title": "Quarterly", "subtitle": "ACME", "slides": [
        {"layout": "bullets", "title": "Agenda", "bullets": ["one", "two"], "notes": "n"},
        {"layout": "section", "title": "Part 2"},
        {"layout": "kpi", "title": "KPIs", "kpis": [{"label": "A", "value": "1"}, {"label": "B", "value": "2"}]}
    ], "path": "quarterly.pptx"}),
    );
    assert_eq!(r["template"], true, "{r}");
    let doc = m.ok("inspect_document", json!({}));
    assert_eq!(doc["theme"], tdoc["theme"]);
    assert_eq!(doc["fonts"]["major"], "Merriweather");
    assert_eq!(doc["fonts"]["minor"], "Lato");
    let lnames = |d: &Value| d["layouts"].as_array().unwrap().iter().map(|l| l["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(lnames(&doc), lnames(&tdoc), "the template's layouts");
    assert!(lnames(&doc).contains(&"ACME Title".to_string()));
    let out = dir.join("quarterly.pptx");
    let theme = zip_text(&out, "ppt/theme/theme1.xml");
    assert!(theme.contains("C0392B") && theme.contains("Merriweather"), "palette and fonts");
    assert!(zip_text(&out, "ppt/slideMasters/slideMaster1.xml").contains("<p:pic>"), "the master's logo comes along");
    // Titles sit in the template's own placeholders (no position of ours), on its title layout.
    let s1 = zip_text(&out, "ppt/slides/slide1.xml");
    assert!(s1.contains("type=\"ctrTitle\"") || s1.contains("type=\"title\""), "{s1}");
    let rels = zip_text(&out, "ppt/slides/_rels/slide1.xml.rels");
    let layout_part = rels.split("slideLayouts/").nth(1).unwrap().split('"').next().unwrap();
    let layout_xml = zip_text(&out, &format!("ppt/slideLayouts/{layout_part}"));
    assert!(layout_xml.contains("ACME Title"), "the title slide uses the template's title layout");

    // adopt_template restyles a deck that is already built.
    m.ok("new_presentation", json!({"theme": "Harbor"}));
    m.ok("add_slide", json!({"layout": "titleAndContent", "title": "Old look", "body": "text"}));
    let r = m.ok("adopt_template", json!({"path": "acme.potx"}));
    assert_eq!(r["slides"], 2, "{r}");
    let doc = m.ok("inspect_document", json!({}));
    assert_eq!(doc["fonts"]["major"], "Merriweather");
    assert_eq!(lnames(&doc), lnames(&tdoc));
    m.ok("save_presentation", json!({"path": "restyled.pptx"}));
    assert!(zip_text(&dir.join("restyled.pptx"), "ppt/theme/theme1.xml").contains("C0392B"));
    let slide = m.ok("inspect_slide", json!({"index": 1}));
    assert_eq!(slide["title"], "Old look", "content kept");
}

#[test]
fn export_png_at_a_scale_pdf_and_outline() {
    let dir = scratch("export");
    let mut m = start(&dir);
    m.ok("build_deck", json!({"title": "Exported", "slides": [{"layout": "bullets", "title": "First point", "bullets": ["a"]}, {"layout": "quote", "title": "Q", "quote": "q"}]}));
    m.ok("export", json!({"path": "half.png", "slide": 0, "scale": 0.5}));
    assert_eq!(png_size(&std::fs::read(dir.join("half.png")).unwrap()), (480, 270));
    m.ok("export", json!({"path": "double.png", "slide": 1, "scale": 2}));
    assert_eq!(png_size(&std::fs::read(dir.join("double.png")).unwrap()), (1920, 1080));
    let r = m.ok("export", json!({"path": "all.png", "all": true, "scale": 0.25}));
    assert_eq!(r["files"].as_array().unwrap().len(), 3);
    m.ok("export", json!({"path": "deck.pdf"}));
    let vector = std::fs::read(dir.join("deck.pdf")).unwrap();
    assert!(vector.starts_with(b"%PDF"));
    // Vector artwork with real text (an embedded font subset), unless raster is asked for.
    let has = |b: &[u8], n: &[u8]| b.windows(n.len()).any(|w| w == n);
    assert!(has(&vector, b"/FontFile2") || has(&vector, b"/FontFile3"), "fonts embedded");
    assert!(!has(&vector, b"/Subtype /Image") && !has(&vector, b"/Subtype/Image"), "no slide pictures in the vector PDF");
    m.ok("export", json!({"path": "raster.pdf", "raster": true}));
    let raster = std::fs::read(dir.join("raster.pdf")).unwrap();
    assert!(has(&raster, b"/Subtype /Image") || has(&raster, b"/Subtype/Image"), "raster: slide pictures");
    assert!(vector.len() * 3 < raster.len(), "vector {} vs raster {}", vector.len(), raster.len());
    m.ok("export", json!({"path": "notes.pdf", "layout": "notes"}));
    m.ok("export", json!({"path": "outline.txt"}));
    let outline = std::fs::read_to_string(dir.join("outline.txt")).unwrap();
    assert!(outline.contains("Exported") && outline.contains("First point"), "{outline}");
    let r = m.call("render_slide", json!({"slide": 0, "scale": 0.5}));
    assert_eq!(r["content"][0]["type"], "image");
    assert_eq!(r["structuredContent"]["width"], 480);
}

fn store(z: &mut zip::ZipWriter<std::fs::File>, name: &str, data: &[u8]) {
    z.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
    z.write_all(data).unwrap();
}

/// Editing a customer's deck in place keeps what DeckCraft does not change: pictures (byte for
/// byte), animations, embedded fonts, extension XML and parts it does not model at all.
#[test]
fn editing_in_place_keeps_media_animations_fonts_and_unknown_parts() {
    let dir = scratch("lossless");
    let mut m = start(&dir);
    let pic = picture(&mut m, &dir, "photo.png");
    m.ok("new_presentation", json!({}));
    m.ok("add_slide", json!({"layout": "titleAndContent", "title": "Customer slide", "body": "keep me"}));
    m.ok("insert_picture", json!({"path": pic.to_str().unwrap(), "x": 500, "y": 200, "w": 300, "h": 170}));
    m.ok("run_command", json!({"command": "animation.add", "params": {"effect": "fade", "class": "entrance"}}));
    m.ok("save_presentation", json!({"path": "base.pptx"}));

    // The customer's copy: an embedded font, an add-in's part on the presentation and on the
    // slide, and an extension element on the slide (none of which DeckCraft models).
    let base = dir.join("base.pptx");
    let font = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/fonts/tests/fixtures/legacy-regular.ttf")).unwrap();
    let customer = dir.join("customer.pptx");
    {
        let mut src = zip::ZipArchive::new(std::fs::File::open(&base).unwrap()).unwrap();
        let mut z = zip::ZipWriter::new(std::fs::File::create(&customer).unwrap());
        for i in 0..src.len() {
            let mut f = src.by_index(i).unwrap();
            let name = f.name().to_string();
            let mut data = vec![];
            std::io::Read::read_to_end(&mut f, &mut data).unwrap();
            let text = String::from_utf8(data.clone()).unwrap_or_default();
            let data = match name.as_str() {
                "[Content_Types].xml" => text
                    .replace("</Types>", "<Default Extension=\"fntdata\" ContentType=\"application/x-fontdata\"/><Override PartName=\"/ppt/addin/state.xml\" ContentType=\"application/vnd.acme.state+xml\"/><Override PartName=\"/ppt/addin/slideNote.xml\" ContentType=\"application/vnd.acme.note+xml\"/></Types>")
                    .into_bytes(),
                "ppt/_rels/presentation.xml.rels" => text
                    .replace("</Relationships>", "<Relationship Id=\"rIdFont1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/font\" Target=\"fonts/font1.fntdata\"/><Relationship Id=\"rIdAcme\" Type=\"urn:acme:addin-state\" Target=\"addin/state.xml\"/><Relationship Id=\"rIdShared\" Type=\"urn:acme:shared\" Target=\"addin/slideNote.xml\"/></Relationships>")
                    .into_bytes(),
                "ppt/presentation.xml" => {
                    let at = text.find("<p:notesSz").unwrap();
                    let end = at + text[at..].find("/>").unwrap() + 2;
                    format!("{}<p:embeddedFontLst><p:embeddedFont><p:font typeface=\"Legacy Test\"/><p:regular r:id=\"rIdFont1\"/></p:embeddedFont></p:embeddedFontLst>{}", &text[..end], &text[end..]).into_bytes()
                }
                // The add-in's part takes `rId1`, an id DeckCraft's writer would give the layout.
                "ppt/slides/_rels/slide2.xml.rels" => text
                    .replace("Id=\"rId1\"", "Id=\"rIdLayout\"")
                    .replace("</Relationships>", "<Relationship Id=\"rId1\" Type=\"urn:acme:slide-note\" Target=\"../addin/slideNote.xml\"/></Relationships>")
                    .into_bytes(),
                "ppt/slides/slide2.xml" => {
                    let t = text.trim_end();
                    assert!(t.ends_with("</p:sld>"));
                    format!("{}<p:extLst><p:ext uri=\"{{ACME-0001}}\"><acme:tag xmlns:acme=\"urn:acme\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" r:id=\"rId1\" note=\"kept\"/></p:ext></p:extLst></p:sld>", &t[..t.len() - "</p:sld>".len()]).into_bytes()
                }
                _ => data,
            };
            store(&mut z, &name, &data);
        }
        store(&mut z, "ppt/fonts/font1.fntdata", &font);
        store(&mut z, "ppt/addin/state.xml", b"<?xml version=\"1.0\"?><state xmlns=\"urn:acme\">deck-level add-in data</state>");
        store(&mut z, "ppt/addin/slideNote.xml", b"<?xml version=\"1.0\"?><note xmlns=\"urn:acme\">slide-level add-in data</note>");
        z.finish().unwrap();
    }

    // Open, edit one title in place, save over the same file.
    m.ok("open_presentation", json!({"path": "customer.pptx"}));
    let title = shapes(&mut m, 1).into_iter().find(|s| s["placeholder"] == "title").unwrap();
    m.ok("go_to_slide", json!({"index": 1}));
    m.ok("set_text", json!({"id": title["id"], "text": "Customer slide (edited)"}));
    let r = m.ok("save_presentation", json!({}));
    assert_eq!(r["previous_files_renamed"][0]["previous"], json!(dir.join("OLD_customer.pptx").to_str().unwrap()), "{r}");

    let (before, after) = (dir.join("OLD_customer.pptx"), dir.join("customer.pptx"));
    let names_after = zip_names(&after);
    // Every part is still there.
    let missing: Vec<String> = zip_names(&before).into_iter().filter(|n| !names_after.contains(n)).collect();
    assert!(missing.is_empty(), "parts lost: {missing:?}");
    // Media byte for byte.
    for n in zip_names(&before).iter().filter(|n| n.starts_with("ppt/media/") || n.starts_with("ppt/fonts/") || n.starts_with("ppt/addin/")) {
        assert_eq!(zip_bytes(&before, n), zip_bytes(&after, n), "{n} changed");
    }
    // The embedded font is still declared; the animation is still there; extension XML kept.
    assert!(zip_text(&after, "ppt/presentation.xml").contains("Legacy Test"));
    let slide = zip_text(&after, "ppt/slides/slide2.xml");
    assert!(slide.contains("<p:timing>") && slide.contains("presetID=\"10\""), "the fade entrance is kept");
    assert!(slide.contains("ACME-0001") && slide.contains("note=\"kept\""), "extension kept");
    assert!(slide.contains("Customer slide (edited)") && slide.contains("keep me"));
    // Unknown parts are linked again, under their ids.
    assert!(zip_text(&after, "ppt/_rels/presentation.xml.rels").contains("Id=\"rIdAcme\" Type=\"urn:acme:addin-state\""));
    // The kept extension points at the add-in's part by id; the id still names that part, and the
    // writer's own relationships took other ids.
    let rels = zip_text(&after, "ppt/slides/_rels/slide2.xml.rels");
    assert!(rels.contains("Id=\"rId1\" Type=\"urn:acme:slide-note\" Target=\"../addin/slideNote.xml\""), "{rels}");
    assert_eq!(rels.matches("Id=\"rId1\"").count(), 1, "{rels}");
    assert!(slide.contains("r:id=\"rId1\""), "the extension still points at it");
    // A part linked from the deck and from a slide is written once and linked from both.
    assert_eq!(names_after.iter().filter(|n| *n == "ppt/addin/slideNote.xml").count(), 1);
    assert!(zip_text(&after, "ppt/_rels/presentation.xml.rels").contains("Id=\"rIdShared\" Type=\"urn:acme:shared\" Target=\"addin/slideNote.xml\""));
    let ct = zip_text(&after, "[Content_Types].xml");
    assert!(ct.contains("/ppt/addin/state.xml") && ct.contains("application/vnd.acme.note+xml"), "{ct}");
}

/// A 50-slide deck in the platform's layouts: charts, tables, pictures, notes, Arabic.
fn fifty_slides() -> Vec<Value> {
    (0..50)
        .map(|k| match k % 5 {
            0 => json!({"layout": "bullets", "title": format!("Point {k}"), "bullets": ["first", "  second"], "notes": format!("notes {k}")}),
            1 => json!({"layout": "table", "title": format!("Register {k}"), "table": [["ID", "Item"], [format!("R-{k}"), "Crane"]]}),
            2 => json!({"layout": "chart", "title": format!("Output {k}"), "chart": {"kind": "bar", "categories": ["Jan", "Feb"], "series": [{"name": "Plan", "values": [k, k + 1]}]}}),
            3 => json!({"layout": "image", "title": format!("Plan {k}"), "image": {"path": "pic.png", "position": "full"}}),
            _ => json!({"layout": "bullets", "title": format!("مراجعة {k}"), "bullets": ["المرحلة الأولى"]}),
        })
        .collect()
}

#[test]
fn render_preview_and_export_markdown_read_a_fifty_slide_deck_in_windows() {
    let dir = scratch("preview");
    let mut m = start(&dir);
    picture(&mut m, &dir, "pic.png");
    // One call builds all 50 slides (and the title slide).
    let built = m.ok("build_deck", json!({"title": "Big deck", "slides": fifty_slides(), "path": "big.pptx"}));
    assert_eq!(built["slides"], 51, "{built}");
    m.ok("close_presentation", json!({"discard": true}));

    // The tool reads the file by path (nothing open), a window at a time within the answer budget.
    let tools = m.request("tools/list", json!({}));
    let def = tools["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "render_preview").unwrap().clone();
    assert_eq!(def["_meta"]["whiq/files"], json!({"read": ["path"], "opens": false}));
    let (mut start_at, mut seen) = (0u64, vec![]);
    let mut first = Value::Null;
    loop {
        let r = m.call("render_preview", json!({"path": "big.pptx", "slide_start": start_at}));
        assert_eq!(r["isError"], false, "{r}");
        let v = r["structuredContent"].clone();
        assert!(v.get("truncated").is_none(), "a window is never trimmed: it stops at the budget");
        let html = v["html"].as_str().unwrap();
        assert!(html.starts_with("<!DOCTYPE html>") && html.ends_with("</body></html>"));
        assert_eq!(v["slide_count"], 51);
        seen.extend(v["slides"].as_array().unwrap().iter().map(|s| s["index"].as_u64().unwrap()));
        if first.is_null() {
            first = v.clone();
        }
        match v["next_slide_start"].as_u64() {
            Some(n) => {
                assert_eq!(v["complete"], false);
                start_at = n;
            }
            None => break,
        }
    }
    assert_eq!(seen, (0..51).collect::<Vec<_>>(), "every slide once, in order");
    assert_eq!(first["slide_size"]["width_emu"], 12_192_000);
    assert_eq!(first["slide_size"]["height_emu"], 6_858_000);
    // The whole deck to a file in one call; pictures as data URIs, charts and text as vectors.
    let v = m.ok("render_preview", json!({"path": "big.pptx", "out": "big.html"}));
    assert_eq!((v["slides_rendered"].clone(), v["complete"].clone()), (json!(51), json!(true)));
    let html = std::fs::read_to_string(dir.join("big.html")).unwrap();
    assert_eq!(html.matches("<section class=\"slide").count(), 51);
    assert!(html.contains("data:image/png;base64,") && v["pictures"]["embedded"].as_u64().unwrap() >= 1);
    assert!(html.contains(">Jan</text>") && html.contains(">R-6</text>") && html.contains(">المرحلة الأولى</text>"));
    assert!(html.contains("<details class=\"notes\"><summary>Speaker notes</summary><div class=\"n\" dir=\"auto\">notes 5</div>"));
    // A window past the end is out_of_range.
    let e = m.err("render_preview", json!({"path": "big.pptx", "slide_start": 51}));
    assert_eq!(e["code"], "out_of_range", "{e}");

    // Markdown, by window.
    let md = m.ok("export_markdown", json!({"path": "big.pptx", "slide_start": 1, "slide_count": 5}));
    assert_eq!((md["slides_exported"].clone(), md["next_slide_start"].clone()), (json!(5), json!(6)));
    let text = md["markdown"].as_str().unwrap();
    assert!(text.contains("## Slide 2: Point 0") && text.contains("- first\n  - second"), "{text}");
    assert!(text.contains("> notes 0"), "{text}");
    assert!(text.contains("| ID | Item |\n| --- | --- |\n| R-1 | Crane |"), "{text}");
    assert!(text.contains("| Category | Plan |") && text.contains("| Feb | 3 |"), "{text}");
    assert!(text.contains("## Slide 6: مراجعة 4"), "{text}");
    assert!(!text.contains("## Slide 7:"));

    // The open presentation, when no path is given.
    m.ok("open_presentation", json!({"path": "big.pptx"}));
    let v = m.ok("render_preview", json!({"slide_start": 10, "slide_count": 3}));
    assert_eq!((v["slide_start"].clone(), v["slides_rendered"].clone(), v["next_slide_start"].clone()), (json!(10), json!(3), json!(13)));
    m.close();
}

#[test]
fn one_batch_applies_hundreds_of_edits() {
    let dir = scratch("bulk");
    let mut m = start(&dir);
    let pic = picture(&mut m, &dir, "pic.png");
    m.ok("new_presentation", json!({"blank": true}));
    let mut items = vec![];
    for k in 0..10 {
        items.push(json!({"tool": "add_slide", "args": {"layout": "titleOnly", "title": format!("Slide {k}")}}));
    }
    for k in 0..10 {
        for j in 0..15 {
            items.push(json!({"slide": k, "tool": "add_shape", "args": {"preset": "roundRect", "x": 20 + j * 40, "y": 120, "w": 36, "h": 30, "text": format!("{k}.{j}"), "fill": "accent2"}}));
            if j % 5 == 0 {
                // set_text on the shape just made (the selection).
                items.push(json!({"tool": "set_text", "args": {"text": format!("Set {k}.{j}")}}));
            }
        }
        items.push(json!({"slide": k, "tool": "insert_table", "args": {"rows": 2, "cols": 2, "data": [["A", "B"], [format!("{k}"), "x"]], "x": 40, "y": 200, "w": 300, "h": 80}}));
        items.push(json!({"slide": k, "command": "shape.insert", "params": {"preset": "rightArrow", "rect": [400, 300, 120, 60]}}));
    }
    items.push(json!({"slide": 0, "tool": "insert_picture", "args": {"path": pic.to_str().unwrap(), "x": 500, "y": 300, "w": 300, "h": 180}}));
    items.push(json!({"slide": 0, "tool": "add_overlays", "args": {"overlays": [{"kind": "rect", "x": 10, "y": 10, "w": 30, "h": 30, "color": "#FF0000"}, {"kind": "line", "x": 5, "y": 90, "w": 90, "h": 10, "arrow": "end"}]}}));
    let n = items.len();
    assert!(n >= 200, "{n}");
    let r = m.call("batch", json!({"commands": items}));
    assert_eq!(r["isError"], false, "{r}");
    let steps = r["structuredContent"]["result"].as_array().cloned().unwrap_or_default();
    assert!(steps.iter().all(|s| s["ok"] == true), "{r}");
    let doc = m.ok("inspect_document", json!({}));
    assert_eq!(doc["slides"].as_array().unwrap().len(), 10);
    for k in 0..10 {
        let shapes = m.ok("inspect_slide", json!({"index": k}))["shapes"].as_array().unwrap().clone();
        assert!(shapes.len() >= 18, "slide {k}: {} shapes", shapes.len());
        let all = serde_json::to_string(&shapes).unwrap();
        assert!(all.contains(&format!("Set {k}.5")) && all.contains(&format!("{k}.14")) && !all.contains(&format!("\"{k}.5\"")), "slide {k}");
    }
    // A batch with an unknown tool is refused before anything runs, with the closest names.
    let e = m.err("batch", json!({"commands": [{"tool": "paint_everything", "args": {}}]}));
    assert_eq!(e["code"], "not_found", "{e}");
    m.close();
}

/// The coverage table of WHIQ-CHANGES.md ("A deck worked like a person in PowerPoint"), row by row,
/// on a real .pptx: built, saved, opened again, then read, created in, edited, deleted from and
/// searched through MCP only.
#[test]
fn an_agent_reads_creates_edits_deletes_and_searches_a_real_deck() {
    let dir = scratch("coverage");
    let mut m = start(&dir);
    let pic = picture(&mut m, &dir, "pic.png");
    m.ok(
        "build_deck",
        json!({"title": "Coverage", "path": "real.pptx", "slides": [
            {"layout": "bullets", "title": "Scope", "bullets": ["Crane", "  Gate"], "notes": "say scope"},
            {"layout": "table", "title": "Register", "table": [["ID", "Item"], ["R-1", "Crane"]]},
            {"layout": "chart", "title": "Output", "chart": {"kind": "column", "categories": ["Jan", "Feb"], "series": [{"name": "Plan", "values": [1, 2]}]}},
            {"layout": "image", "title": "Plan", "image": {"path": "pic.png", "position": "full"}}
        ]}),
    );
    m.ok("close_presentation", json!({"discard": true}));
    m.ok("open_presentation", json!({"path": "real.pptx"}));

    // READ: slides in order, layouts and masters.
    let doc = m.ok("inspect_document", json!({}));
    let titles: Vec<&str> = doc["slides"].as_array().unwrap().iter().map(|s| s["title"].as_str().unwrap_or("")).collect();
    assert_eq!(titles, ["Coverage", "Scope", "Register", "Output", "Plan"]);
    assert!(doc["masters"].as_array().unwrap().first().unwrap()["layouts"].as_array().unwrap().len() >= 5, "{doc}");
    assert!(doc["layouts"].as_array().unwrap().len() >= 5);
    // READ: every shape's text, a table's cells, a chart's data, a picture, the notes.
    let s1 = m.ok("inspect_slide", json!({"index": 1}));
    assert!(serde_json::to_string(&s1["shapes"]).unwrap().contains("Crane\\nGate"), "{s1}");
    assert_eq!(s1["notes"], "say scope", "{s1}");
    let s2 = shapes(&mut m, 2);
    assert!(s2.iter().any(|s| s["table"]["cells"] == json!([["ID", "Item"], ["R-1", "Crane"]])), "{s2:?}");
    let s3 = shapes(&mut m, 3);
    let chart = s3.iter().find(|s| s["chart"].is_object()).unwrap();
    assert_eq!(chart["chart"]["categories"], json!(["Jan", "Feb"]));
    assert_eq!(chart["chart"]["series"][0]["values"], json!([1.0, 2.0]));
    let s4 = shapes(&mut m, 4);
    assert!(s4.iter().any(|s| s["picture"]["type"] == "image/png"), "{s4:?}");
    // READ: a picture of a slide; the whole deck as markdown and as the preview page.
    let png = m.call("render_slide", json!({"slide": 3}));
    assert_eq!(png["content"][0]["type"], "image", "{png}");
    let md = m.ok("export_markdown", json!({}));
    assert!(md["markdown"].as_str().unwrap().contains("| Jan | 1 |"));
    assert_eq!(m.ok("render_preview", json!({}))["slides_rendered"], 5);

    // CREATE: a slide from a layout, shapes, a connector, a text box, a table, a chart, a picture, notes.
    m.ok("go_to_slide", json!({"index": 4}));
    m.ok("add_slide", json!({"layout": "titleOnly", "title": "Created"}));
    let new_slide = 5;
    let a = m.ok("add_shape", json!({"preset": "rect", "x": 40, "y": 120, "w": 120, "h": 60, "text": "Box A"}))["id"].clone();
    let b = m.ok("add_shape", json!({"preset": "ellipse", "x": 400, "y": 300, "w": 120, "h": 60, "text": "Box B"}))["id"].clone();
    let line = m.ok("add_shape", json!({"preset": "line", "x": 40, "y": 450, "w": 300, "h": 0}))["id"].clone();
    let conn = m.ok("run_command", json!({"command": "shape.connect", "params": {"from": a, "to": b, "preset": "straightConnector1"}}))["id"].clone();
    let tb = m.ok("add_text_box", json!({"x": 600, "y": 120, "w": 200, "text": "Free text"}))["id"].clone();
    let tbl =
        m.ok("insert_table", json!({"rows": 2, "cols": 2, "data": [["K", "V"], ["a", "1"]], "x": 40, "y": 300, "w": 300, "h": 80}))["id"].clone();
    let ch = m.ok("insert_chart", json!({"type": "pie", "categories": ["x", "y"], "series": [{"name": "S", "values": [3, 4]}]}))["id"].clone();
    let im = m.ok("insert_picture", json!({"path": pic.to_str().unwrap(), "x": 600, "y": 300, "w": 160, "h": 90}))["id"].clone();
    m.ok("set_notes", json!({"text": "created notes", "slide": new_slide}));
    let made = shapes(&mut m, new_slide);
    for id in [&a, &b, &line, &conn, &tb, &tbl, &ch, &im] {
        assert!(made.iter().any(|s| &s["id"] == id), "{id} in {made:?}");
    }
    // A group of two shapes reads back with its children.
    m.ok("run_command", json!({"command": "arrange.group", "params": {"ids": [a, b]}}));
    let grouped = shapes(&mut m, new_slide);
    let g = grouped.iter().find(|s| s["children"].is_array()).unwrap();
    assert_eq!(g["children"].as_array().unwrap().len(), 2);
    m.ok("run_command", json!({"command": "arrange.ungroup", "params": {"ids": [g["id"]]}}));

    // EDIT: text, position and size, style, a chart's data and type, a table cell, the layout; in one batch.
    let r = m.call(
        "batch",
        json!({"commands": [
            {"tool": "set_text", "args": {"id": tb, "text": "Edited text"}},
            {"tool": "set_bounds", "args": {"id": a, "x": 50, "y": 130, "w": 140, "h": 70}},
            {"tool": "format_shape", "args": {"id": a, "fill": "#00B050", "line": "#C00000", "lineWidth": 3, "bold": true, "size": 20}},
            {"tool": "format_shape", "args": {"id": conn, "tail": "triangle", "dash": "dash"}},
            {"tool": "edit_chart", "args": {"id": ch, "type": "bar", "categories": ["x", "y", "z"], "series": [{"name": "S", "values": [5, 6, 7]}], "title": "New title"}},
            {"command": "text.set", "params": {"id": tbl, "cell": [1, 1], "text": "99"}},
            {"command": "table.insertRowBelow", "params": {"id": tbl, "row": 1}},
            {"slide": 1, "command": "slide.layout", "params": {"layout": "twoContent"}}
        ]}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let edited = shapes(&mut m, new_slide);
    let by = |id: &Value| edited.iter().find(|s| &s["id"] == id).unwrap().clone();
    assert_eq!(by(&tb)["text"], "Edited text");
    assert_eq!(by(&a)["box"], json!([50.0, 130.0, 140.0, 70.0]));
    assert_eq!(by(&a)["fill"], "#00B050");
    assert_eq!(by(&ch)["chart"]["categories"], json!(["x", "y", "z"]));
    assert_eq!(by(&ch)["chart"]["title"], "New title");
    assert!(by(&ch)["chart"]["type"].as_str().unwrap().to_lowercase().contains("bar"), "{}", by(&ch));
    assert_eq!(by(&tbl)["table"]["rows"], 3);
    assert_eq!(by(&tbl)["table"]["cells"][1][1], "99");
    m.ok("go_to_slide", json!({"index": new_slide}));
    let a_full = m.ok("run_command", json!({"command": "shape.inspect", "params": {"id": a}}));
    let a_json = serde_json::to_string(&a_full).unwrap();
    assert!(a_json.contains("\"bold\":true") && a_json.contains("\"size\":20"), "{a_json}");
    let c_full = serde_json::to_string(&m.ok("run_command", json!({"command": "shape.inspect", "params": {"id": conn}}))).unwrap();
    assert!(c_full.contains("triangle") && c_full.to_lowercase().contains("dash"), "{c_full}");
    assert_eq!(m.ok("inspect_document", json!({}))["slides"][1]["layout"], "Two Content");

    // SEARCH: find text across slides (slide + shape), then find and replace.
    let f = m.ok("find_text", json!({"text": "Crane"}));
    assert!(f["count"].as_u64().unwrap() >= 2, "{f}");
    let hits: Vec<(u64, bool)> = f["matches"].as_array().unwrap().iter().map(|h| (h["slide"].as_u64().unwrap(), h["shape"].is_u64())).collect();
    assert!(hits.contains(&(1, true)) && hits.contains(&(2, true)), "{f}");
    assert!(f["matches"].as_array().unwrap().iter().any(|h| h["paragraph"] == "Crane"), "{f}");
    let notes_hit = m.ok("find_text", json!({"text": "created notes"}));
    assert_eq!(notes_hit["matches"][0]["slide"], 5);
    assert!(notes_hit["matches"][0]["shape"].is_null(), "a notes match has no shape");
    let rep = m.ok("replace_text", json!({"text": "Crane", "with": "Tower crane"}));
    assert!(rep["replaced"].as_u64().unwrap() >= 2, "{rep}");
    assert_eq!(m.ok("find_text", json!({"text": "Tower crane"}))["count"], rep["replaced"]);

    // DELETE: shapes (a table, a chart, a picture, a line), notes, slides.
    m.ok("delete_shapes", json!({"ids": [tbl, ch, im, line], "slide": new_slide}));
    let left = shapes(&mut m, new_slide);
    for id in [&tbl, &ch, &im, &line] {
        assert!(!left.iter().any(|s| &s["id"] == id), "{id} deleted");
    }
    m.ok("set_notes", json!({"text": "", "slide": new_slide}));
    assert_eq!(m.ok("inspect_slide", json!({"index": new_slide}))["notes"], "");
    m.ok("delete_slides", json!({"indices": [2, 3]}));
    let after: Vec<String> =
        m.ok("inspect_document", json!({}))["slides"].as_array().unwrap().iter().map(|s| s["title"].as_str().unwrap_or("").to_string()).collect();
    assert_eq!(after, ["Coverage", "Scope", "Plan", "Created"]);

    // And it all saves to a .pptx that opens again with the edits.
    m.ok("save_presentation", json!({"path": "edited.pptx"}));
    m.ok("close_presentation", json!({}));
    m.ok("open_presentation", json!({"path": "edited.pptx"}));
    let md = m.ok("export_markdown", json!({}))["markdown"].as_str().unwrap().to_string();
    assert!(md.contains("Tower crane") && md.contains("Edited text") && !md.contains("| Jan |"), "{md}");
    m.close();
}

/// `xml` with `add` put just before its first `before` (or before its last `</…>` end tag).
fn put_before(xml: &str, before: &str, add: &str) -> String {
    let at = xml.find(before).unwrap_or_else(|| xml.rfind("</").unwrap());
    format!("{}{add}{}", &xml[..at], &xml[at..])
}

/// A customer's deck made by another producer: the parts and elements DeckCraft does not write
/// itself (the document properties a producer fills in, a thumbnail, a notes page with a slide
/// image and a slide-number placeholder, layout paragraph properties and extension lists, a theme
/// colour from the system, presentation and view property extensions). Built from a DeckCraft deck
/// whose parts are rewritten, so nothing in it comes from another application.
fn customer_deck(m: &mut Mcp, dir: &Path) -> (PathBuf, Vec<u8>) {
    m.ok("new_presentation", json!({}));
    m.ok("add_slide", json!({"layout": "titleAndContent", "title": "Board update", "body": "Cost and programme"}));
    m.ok("set_notes", json!({"text": "Say the cost first."}));
    m.ok("add_slide", json!({"layout": "titleOnly", "title": "Approval route"}));
    m.ok("add_text_box", json!({"x": 80, "y": 200, "w": 300, "text": "Package manager"}));
    m.ok("set_notes", json!({"text": "Every change goes past the director."}));
    m.ok("save_presentation", json!({"path": "made.pptx"}));
    m.ok("close_presentation", json!({}));
    let mut thumb = vec![];
    image::RgbImage::from_pixel(8, 6, image::Rgb([30, 90, 160]))
        .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(&mut thumb, 80))
        .unwrap();
    let base = dir.join("made.pptx");
    let customer = dir.join("customer.pptx");
    let mut src = zip::ZipArchive::new(std::fs::File::open(&base).unwrap()).unwrap();
    let mut z = zip::ZipWriter::new(std::fs::File::create(&customer).unwrap());
    let creation = |n: usize| {
        format!(
            "<p:extLst><p:ext uri=\"{{BB962C8B-B14F-4D97-AF65-F5344CB8AC3E}}\"><p14:creationId xmlns:p14=\"http://schemas.microsoft.com/office/powerpoint/2010/main\" val=\"{}\"/></p:ext></p:extLst>",
            1000 + n
        )
    };
    for i in 0..src.len() {
        let mut f = src.by_index(i).unwrap();
        let name = f.name().to_string();
        let mut data = vec![];
        std::io::Read::read_to_end(&mut f, &mut data).unwrap();
        let text = String::from_utf8(data.clone()).unwrap_or_default();
        let data = if name == "[Content_Types].xml" {
            put_before(&text, "</Types>", "<Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>").into_bytes()
        } else if name == "_rels/.rels" {
            put_before(&text, "</Relationships>", "<Relationship Id=\"rIdThumb\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail\" Target=\"docProps/thumbnail.jpeg\"/>").into_bytes()
        } else if name == "docProps/core.xml" {
            br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:dcmitype="http://purl.org/dc/dcmitype/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:title>Aurora Tower board update</dc:title><dc:subject>Period 14</dc:subject><dc:creator>Ann Customer</dc:creator><cp:keywords>cost; programme</cp:keywords><dc:description>Made by the customer's own tool</dc:description><cp:lastModifiedBy>Ann Customer</cp:lastModifiedBy><cp:revision>7</cp:revision><dcterms:created xsi:type="dcterms:W3CDTF">2013-01-27T09:14:16Z</dcterms:created><dcterms:modified xsi:type="dcterms:W3CDTF">2013-01-27T09:15:58Z</dcterms:modified><cp:category>Board</cp:category><cp:contentStatus>Draft</cp:contentStatus></cp:coreProperties>"#.to_vec()
        } else if name == "docProps/app.xml" {
            br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><TotalTime>12</TotalTime><Words>20</Words><Application>Acme Slides</Application><PresentationFormat>Widescreen</PresentationFormat><Paragraphs>6</Paragraphs><Slides>3</Slides><Notes>2</Notes><HiddenSlides>0</HiddenSlides><MMClips>0</MMClips><ScaleCrop>false</ScaleCrop><HeadingPairs><vt:vector size="2" baseType="variant"><vt:variant><vt:lpstr>Slide Titles</vt:lpstr></vt:variant><vt:variant><vt:i4>3</vt:i4></vt:variant></vt:vector></HeadingPairs><TitlesOfParts><vt:vector size="3" baseType="lpstr"><vt:lpstr>Title</vt:lpstr><vt:lpstr>Board update</vt:lpstr><vt:lpstr>Approval route</vt:lpstr></vt:vector></TitlesOfParts><Manager>M. Manager</Manager><Company>Acme Build</Company><LinksUpToDate>false</LinksUpToDate><SharedDoc>false</SharedDoc><HyperlinksChanged>false</HyperlinksChanged><AppVersion>16.0000</AppVersion></Properties>"#.to_vec()
        } else if name.starts_with("ppt/slideLayouts/slideLayout") && !name.contains("_rels") {
            let n: usize = name.trim_start_matches("ppt/slideLayouts/slideLayout").trim_end_matches(".xml").parse().unwrap();
            // A paragraph with properties DeckCraft does not model, and the layout's creation id.
            let t = text.replacen(
                "<a:p>",
                "<a:p><a:pPr marL=\"0\" indent=\"0\" eaLnBrk=\"0\" latinLnBrk=\"1\" hangingPunct=\"0\"><a:buNone/></a:pPr>",
                1,
            );
            let t = if t.contains("<a:p><a:pPr marL=\"0\"") {
                t
            } else {
                t.replacen(
                    "<a:p/>",
                    "<a:p><a:pPr marL=\"0\" indent=\"0\" eaLnBrk=\"0\" latinLnBrk=\"1\" hangingPunct=\"0\"><a:buNone/></a:pPr></a:p>",
                    1,
                )
            };
            if t.contains("<p:extLst>") { t.into_bytes() } else { put_before(&t, "</p:sldLayout>", &creation(n)).into_bytes() }
        } else if name == "ppt/slideMasters/slideMaster1.xml" && !text.contains("<p:extLst>") {
            put_before(&text, "</p:sldMaster>", &creation(0)).into_bytes()
        } else if name.starts_with("ppt/theme/theme") && !name.contains("_rels") {
            let a = text.find("<a:dk1>").unwrap();
            let b = text.find("</a:dk1>").unwrap();
            format!("{}<a:dk1><a:sysClr val=\"windowText\" lastClr=\"000000\"/>{}", &text[..a], &text[b..]).into_bytes()
        } else if name == "ppt/presProps.xml" {
            put_before(&text, "</p:presentationPr>", "<p:extLst><p:ext uri=\"{E76CE94A-603C-4142-B9EB-6D1370010A27}\"><p14:discardImageEditData xmlns:p14=\"http://schemas.microsoft.com/office/powerpoint/2010/main\" val=\"0\"/></p:ext></p:extLst>").into_bytes()
        } else if name.starts_with("ppt/notesSlides/notesSlide") && !name.contains("_rels") {
            // The slide image and the slide number, beside the notes body.
            let img = "<p:sp><p:nvSpPr><p:cNvPr id=\"90\" name=\"Slide Image Placeholder 1\"/><p:cNvSpPr><a:spLocks noGrp=\"1\" noRot=\"1\" noChangeAspect=\"1\"/></p:cNvSpPr><p:nvPr><p:ph type=\"sldImg\"/></p:nvPr></p:nvSpPr><p:spPr/></p:sp>";
            let num = "<p:sp><p:nvSpPr><p:cNvPr id=\"91\" name=\"Slide Number Placeholder 3\"/><p:cNvSpPr><a:spLocks noGrp=\"1\"/></p:cNvSpPr><p:nvPr><p:ph type=\"sldNum\" sz=\"quarter\" idx=\"5\"/></p:nvPr></p:nvSpPr><p:spPr/><p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:fld id=\"{5E1C0B4F-1D2A-4C3B-9E8F-0A1B2C3D4E5F}\" type=\"slidenum\"><a:rPr lang=\"en-US\"/><a:t>2</a:t></a:fld><a:endParaRPr lang=\"en-US\"/></a:p></p:txBody></p:sp>";
            let at = text.find("<p:sp>").unwrap();
            put_before(&format!("{}{img}{}", &text[..at], &text[at..]), "</p:spTree>", num).into_bytes()
        } else {
            data
        };
        store(&mut z, &name, &data);
    }
    store(&mut z, "docProps/thumbnail.jpeg", &thumb);
    z.finish().unwrap();
    (customer, thumb)
}

/// The owner's rule: editing a customer's file must not lose anything it holds. Open a deck made
/// by another producer, edit one text box, save, and compare every part.
#[test]
fn editing_a_customer_deck_keeps_every_part_the_edit_did_not_touch() {
    let dir = scratch("keep-untouched");
    let mut m = start(&dir);
    let (customer, thumb) = customer_deck(&mut m, &dir);

    // Nothing a save would change.
    let rc = m.ok("roundtrip_check", json!({"path": "customer.pptx"}));
    assert_eq!(rc["lossless"], true, "{rc}");

    // One text box edited.
    m.ok("open_presentation", json!({"path": "customer.pptx"}));
    let box_id = shapes(&mut m, 2).into_iter().find(|s| s["text"] == "Package manager").unwrap()["id"].clone();
    m.ok("go_to_slide", json!({"index": 2}));
    m.ok("set_text", json!({"id": box_id, "text": "Package manager (rev B)"}));
    m.ok("save_presentation", json!({"path": "edited.pptx", "caller": "Agent Smith"}));
    let edited = dir.join("edited.pptx");

    let before = zip_names(&customer);
    let after = zip_names(&edited);
    assert_eq!(
        before.iter().collect::<std::collections::BTreeSet<_>>(),
        after.iter().collect::<std::collections::BTreeSet<_>>(),
        "the same parts, under the same names"
    );
    // The edited slide is the only content part that changed.
    let slide = zip_text(&edited, "ppt/_rels/presentation.xml.rels");
    assert!(slide.contains("slides/slide3.xml"));
    let allowed =
        ["[Content_Types].xml", "_rels/.rels", "docProps/core.xml", "docProps/app.xml", "ppt/slides/slide3.xml", "ppt/slides/_rels/slide3.xml.rels"];
    let changed: Vec<&String> = before.iter().filter(|n| zip_bytes(&customer, n) != zip_bytes(&edited, n)).collect();
    for n in &changed {
        assert!(allowed.contains(&n.as_str()), "{n} changed, but the edit did not touch it (changed: {changed:?})");
    }
    assert!(zip_text(&edited, "ppt/slides/slide3.xml").contains("Package manager (rev B)"));
    // The thumbnail, byte for byte, still linked from the package.
    assert_eq!(zip_bytes(&edited, "docProps/thumbnail.jpeg"), thumb);
    assert!(zip_text(&edited, "_rels/.rels").contains("metadata/thumbnail\" Target=\"docProps/thumbnail.jpeg\""));
    // The document properties as they were, with the save's own: modified now, by the caller.
    let core = zip_text(&edited, "docProps/core.xml");
    for kept in [
        "<dc:title>Aurora Tower board update</dc:title>",
        "<dc:subject>Period 14</dc:subject>",
        "<dc:creator>Ann Customer</dc:creator>",
        "<cp:keywords>cost; programme</cp:keywords>",
        "<dc:description>Made by the customer's own tool</dc:description>",
        "<cp:category>Board</cp:category>",
        "<cp:contentStatus>Draft</cp:contentStatus>",
        "<cp:revision>7</cp:revision>",
        "2013-01-27T09:14:16Z",
    ] {
        assert!(core.contains(kept), "{kept} kept: {core}");
    }
    assert!(core.contains("<cp:lastModifiedBy>Agent Smith</cp:lastModifiedBy>"), "{core}");
    assert!(!core.contains("2013-01-27T09:15:58Z"), "modified is now: {core}");
    let app = zip_text(&edited, "docProps/app.xml");
    for kept in [
        "<Company>Acme Build</Company>",
        "<Manager>M. Manager</Manager>",
        "<Application>Acme Slides</Application>",
        "<TitlesOfParts>",
        "<AppVersion>16.0000</AppVersion>",
    ] {
        assert!(app.contains(kept), "{kept} kept: {app}");
    }
    // Every notes page with its slide image and slide number; layouts with their paragraph
    // properties and creation ids; the theme's system colour.
    for n in before.iter().filter(|n| n.starts_with("ppt/notesSlides/notesSlide")) {
        assert_eq!(zip_bytes(&customer, n), zip_bytes(&edited, n), "{n}");
    }
    let layout = zip_text(&edited, "ppt/slideLayouts/slideLayout1.xml");
    assert!(layout.contains("latinLnBrk=\"1\"") && layout.contains("p14:creationId"), "{layout}");
    assert!(zip_text(&edited, "ppt/theme/theme1.xml").contains("<a:sysClr val=\"windowText\""));

    // A notes page whose text is edited keeps its other placeholders.
    m.ok("set_notes", json!({"slide": 1, "text": "Say the risk first."}));
    m.ok("save_presentation", json!({"path": "edited2.pptx"}));
    let e2 = dir.join("edited2.pptx");
    let notes_rel = zip_text(&e2, "ppt/slides/_rels/slide2.xml.rels");
    let notes_part = format!("ppt/notesSlides/{}", notes_rel.split("notesSlides/").nth(1).unwrap().split('"').next().unwrap());
    let notes = zip_text(&e2, &notes_part);
    assert!(notes.contains("Say the risk first.") && !notes.contains("Say the cost first."), "{notes}");
    assert!(notes.contains("type=\"sldImg\"") && notes.contains("type=\"sldNum\"") && notes.contains("Slide Number Placeholder 3"), "{notes}");
    for n in before.iter().filter(|n| n.starts_with("ppt/slideLayouts/") || n.starts_with("ppt/slideMasters/") || n.starts_with("ppt/theme/")) {
        assert_eq!(zip_bytes(&customer, n), zip_bytes(&e2, n), "{n}");
    }
    m.close();
}
