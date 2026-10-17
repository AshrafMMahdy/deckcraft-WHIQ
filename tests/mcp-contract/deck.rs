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
