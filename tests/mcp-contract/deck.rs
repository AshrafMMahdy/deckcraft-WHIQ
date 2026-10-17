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
/// labelled, rendered to PNG and probed where each mark must be (box: 43.2, 122.4, 873.6 × 288).
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
    let (bx, by, bw, bh) = (43.2, 122.4, 873.6, 288.0);
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
