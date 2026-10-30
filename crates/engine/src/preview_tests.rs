//! WHIQ: the preview page and the markdown, on decks made with the engine's own commands.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Value, json};

use super::*;
use crate::Session;

fn deck() -> Session {
    let mut s = Session::new();
    s.execute("file.new", &json!({"blank": true})).unwrap();
    s
}

fn run(s: &mut Session, cmd: &str, p: Value) -> Value {
    s.execute(cmd, &p).unwrap_or_else(|e| panic!("{cmd} {p}: {e}"))
}

fn page_of(s: &Session, window: Window) -> PreviewPage {
    let o = PreviewOptions { window, name: "deck.pptx".into(), ..Default::default() };
    preview(&s.doc().unwrap().doc, &o).unwrap()
}

/// The `<svg class="art">` of the slide whose card has `data-index="i"`.
fn art_of(html: &str, i: usize) -> &str {
    let at = html.find(&format!("data-index=\"{i}\"")).expect("card");
    let rest = &html[at..];
    let a = rest.find("<svg").expect("svg");
    let b = rest[a..].find("</svg></section>").or_else(|| rest[a..].find("</svg><details")).expect("svg end");
    &rest[a..a + b + 6]
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x * 7) as u8, (y * 5) as u8, 90, 255]));
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png).unwrap();
    buf
}

#[test]
fn the_page_is_self_contained_html_with_one_card_per_slide() {
    let mut s = deck();
    run(&mut s, "slide.new", json!({"layout": "titleAndContent", "title": "First", "body": "One\n\tTwo"}));
    run(&mut s, "slide.new", json!({"layout": "titleAndContent", "title": "Second", "body": "Three"}));
    let p = page_of(&s, Window::default());
    assert_eq!((p.total, p.rendered, p.next()), (2, 2, None));
    assert!(p.html.starts_with("<!DOCTYPE html><html><head><meta charset=\"utf-8\">"));
    assert!(p.html.contains("<title>deck.pptx</title>") && p.html.contains("deck.pptx — 2 slides"));
    assert_eq!(p.html.matches("<section class=\"slide").count(), 2);
    assert!(p.html.contains("<span class=\"num\">1/2</span>") && p.html.contains("<span class=\"num\">2/2</span>"));
    // Nothing is fetched: no URL but data URIs and the SVG namespace.
    for bad in ["src=\"http", "href=\"http", "<script", "<link", "@import", "url(http"] {
        assert!(!p.html.contains(bad), "{bad}");
    }
    // Not a picture of the slide: vector paths and text.
    assert!(!p.html.contains("data:image"), "no picture on these slides, so no image");
    let a = art_of(&p.html, 0);
    assert!(a.contains("viewBox=\"0 0 1920 1080\""), "{}", &a[..200]);
    assert!(a.contains("<use href=\"#dc0-g"), "glyphs drawn from the shared outlines");
    assert!(a.contains(">First</text>") && a.contains(">One</text>") && a.contains(">Two</text>"), "text kept for search and copy");
    assert_eq!(p.slides[1]["title"], "Second");
}

#[test]
fn shapes_connectors_arrows_and_freeforms_are_vector_paths() {
    let mut s = deck();
    run(&mut s, "slide.new", json!({"layout": "blank"}));
    let a = run(&mut s, "shape.insert", json!({"preset": "rect", "rect": [60, 60, 160, 90], "text": "Start"}))["id"].clone();
    let b = run(&mut s, "shape.insert", json!({"preset": "ellipse", "rect": [500, 300, 160, 90], "text": "End"}))["id"].clone();
    run(&mut s, "shape.insert", json!({"preset": "rightArrow", "rect": [300, 60, 120, 60]}));
    run(&mut s, "shape.insert", json!({"preset": "star5", "rect": [700, 60, 100, 100]}));
    let c = run(&mut s, "shape.connect", json!({"from": a, "to": b, "preset": "bentConnector3"}))["id"].clone();
    run(&mut s, "shape.line", json!({"ids": [c], "tail": "triangle", "width": 2, "color": "#C00000", "dash": "dash"}));
    run(&mut s, "shape.freeform", json!({"points": [[100, 400], [200, 350], [260, 470], [120, 500]], "closed": true}));
    let p = page_of(&s, Window::default());
    let art = art_of(&p.html, 0);
    // Six shapes' outlines and fills, the arrowhead, and their labels.
    assert!(art.matches("<path d=\"M").count() >= 10, "{}", art.matches("<path d=\"M").count());
    assert!(art.contains("stroke=\"#c00000\""), "the connector's own colour");
    assert!(art.contains("stroke-dasharray="), "its dash");
    assert!(art.contains(">Start</text>") && art.contains(">End</text>"));
    // The bent connector is a path with corners, not a straight line between the boxes.
    let conn = art.split("stroke=\"#c00000\"").next().unwrap();
    let d = &conn[conn.rfind("<path d=\"").unwrap()..];
    assert!(d.matches('L').count() >= 2, "an elbow: {d}");
}

#[test]
fn a_chart_is_drawn_and_its_data_read() {
    let mut s = deck();
    run(&mut s, "slide.new", json!({"layout": "titleOnly", "title": "Revenue"}));
    run(
        &mut s,
        "insert.chart",
        json!({"type": "column", "rect": [80, 120, 700, 380], "categories": ["Q1", "Q2", "Q3"], "series": [{"name": "2025", "values": [10, 20, 15]}, {"name": "2026", "values": [12, 26, 18]}]}),
    );
    let p = page_of(&s, Window::default());
    let art = art_of(&p.html, 0);
    for label in ["Q1", "Q2", "Q3", "2025", "2026"] {
        assert!(art.contains(&format!(">{label}</text>")), "{label}");
    }
    assert!(art.matches("<path d=\"M").count() >= 6, "six bars at least");
    let md = markdown(&s.doc().unwrap().doc, Window::default(), "deck", None).unwrap().text;
    assert!(md.contains("**Chart** (column)") || md.contains("**Chart: "), "{md}");
    assert!(md.contains("| Category | 2025 | 2026 |"), "{md}");
    assert!(md.contains("| Q2 | 20 | 26 |"), "{md}");
}

#[test]
fn a_table_is_drawn_and_read_as_a_markdown_table() {
    let mut s = deck();
    run(&mut s, "slide.new", json!({"layout": "titleOnly", "title": "Plan"}));
    run(
        &mut s,
        "insert.table",
        json!({"rows": 3, "cols": 2, "data": [["Task", "Owner"], ["Survey", "Ali"], ["Design | draft", "Mona"]], "rect": [60, 120, 600, 200]}),
    );
    let p = page_of(&s, Window::default());
    let art = art_of(&p.html, 0);
    for t in ["Task", "Owner", "Survey", "Mona"] {
        assert!(art.contains(&format!(">{t}</text>")), "{t}");
    }
    let md = markdown(&s.doc().unwrap().doc, Window::default(), "deck", None).unwrap().text;
    assert!(md.contains("| Task | Owner |\n| --- | --- |\n| Survey | Ali |\n| Design \\| draft | Mona |"), "{md}");
}

#[test]
fn a_picture_is_embedded_as_its_own_bytes() {
    let mut s = deck();
    run(&mut s, "slide.new", json!({"layout": "blank"}));
    let bytes = png(64, 48);
    run(&mut s, "insert.picture", json!({"data": crate::cmd::base64_encode(&bytes), "name": "site.png", "rect": [100, 100, 320, 240]}));
    // The same picture twice (two media items of the same bytes) is embedded once.
    run(&mut s, "insert.picture", json!({"data": crate::cmd::base64_encode(&bytes), "name": "site.png", "rect": [500, 100, 160, 120]}));
    let p = page_of(&s, Window::default());
    let uri = format!("data:image/png;base64,{}", crate::cmd::base64_encode(&bytes));
    assert!(p.html.contains(&uri), "the PNG as stored");
    assert_eq!(p.stats.pictures, 1, "{:?}", p.stats);
    assert_eq!(p.stats.originals, 1);
    assert_eq!(art_of(&p.html, 0).matches("<use href=\"#dc0-p").count(), 2);
    let md = markdown(&s.doc().unwrap().doc, Window::default(), "deck", None).unwrap().text;
    assert!(md.contains("*[Picture"), "{md}");
}

#[test]
fn notes_go_under_the_slide_and_into_the_markdown() {
    let mut s = deck();
    run(&mut s, "slide.new", json!({"layout": "titleAndContent", "title": "Budget", "body": "Total 4.2M"}));
    run(&mut s, "slide.notes", json!({"text": "Say the risk first.\nThen the numbers."}));
    let p = page_of(&s, Window::default());
    assert!(p.html.contains("<details class=\"notes\"><summary>Speaker notes</summary><div class=\"n\" dir=\"auto\">Say the risk first.\nThen the numbers.</div></details>"));
    assert_eq!(p.slides[0]["notes"], true);
    let no = PreviewOptions { notes: false, ..Default::default() };
    assert!(!preview(&s.doc().unwrap().doc, &no).unwrap().html.contains("class=\"notes\""));
    let md = markdown(&s.doc().unwrap().doc, Window::default(), "deck", None).unwrap().text;
    assert!(md.contains("## Slide 1: Budget\n\n- Total 4.2M\n\n**Notes:**\n\n> Say the risk first.\n> Then the numbers.\n"), "{md}");
}

#[test]
fn arabic_text_is_shaped_right_to_left_and_kept_as_text() {
    let mut s = deck();
    let ar = "مراجعة الجدول الزمني";
    run(&mut s, "slide.new", json!({"layout": "titleAndContent", "title": ar, "body": "المرحلة الأولى"}));
    let p = page_of(&s, Window::default());
    let art = art_of(&p.html, 0);
    assert!(art.contains(&format!(">{ar}</text>")), "the Arabic title as text");
    assert!(art.contains(">المرحلة الأولى</text>"));
    assert!(p.html.contains(&format!("<span class=\"title\" dir=\"auto\">{ar}</span>")));
    // Drawn from glyph outlines (the shaped forms), not left to a browser font.
    assert!(p.stats.glyphs > 5, "{:?}", p.stats);
    // Right to left: the body paragraph ends at the right of its box, not the left.
    let doc = &s.doc().unwrap().doc;
    let body = doc.slides[0].shapes.iter().find(|sh| sh.text.as_ref().is_some_and(|t| t.text().contains("المرحلة"))).unwrap();
    let ctx = deckcraft_model::resolve::Ctx::for_slide(doc, &doc.slides[0]).unwrap();
    let bx = deckcraft_model::resolve::xfrm(&ctx, body);
    let hidden = art.split("<text ").find(|t| t.contains("المرحلة")).unwrap();
    let m = hidden.split("transform=\"matrix(").nth(1).unwrap();
    let x: f64 = m.split_whitespace().nth(4).unwrap().parse().unwrap();
    let mid = (bx.x + bx.w / 2.0) * 2.0;
    assert!(x > mid, "starts right of the box middle ({x} > {mid})");
    let md = markdown(doc, Window::default(), "deck", None).unwrap().text;
    assert!(md.contains(&format!("## Slide 1: {ar}")) && md.contains("- المرحلة الأولى"), "{md}");
}

#[test]
fn a_hundred_slide_deck_goes_in_windows() {
    let mut s = deck();
    for k in 0..100 {
        run(&mut s, "slide.new", json!({"layout": "titleAndContent", "title": format!("Slide title {k}"), "body": format!("Point {k}")}));
    }
    // An explicit window.
    let p = page_of(&s, Window { start: 40, count: Some(20) });
    assert_eq!((p.start, p.rendered, p.total, p.next()), (40, 20, 100, Some(60)));
    assert!(p.html.contains("<span class=\"num\">41/100</span>") && p.html.contains("<span class=\"num\">60/100</span>"));
    assert!(!p.html.contains("<span class=\"num\">61/100</span>") && !p.html.contains("<span class=\"num\">40/100</span>"));
    assert!(p.html.contains("Showing slides 41–60 of 100."));
    // Past the end is out of range, by its index.
    let e = preview(&s.doc().unwrap().doc, &PreviewOptions { window: Window { start: 100, count: None }, ..Default::default() }).unwrap_err();
    assert!(e.to_string().contains("no slide 100"), "{e}");
    // A byte budget: windows that follow next() cover every slide once.
    let doc = s.doc().unwrap().doc.clone();
    let (mut start, mut seen, mut calls) = (0usize, vec![], 0);
    loop {
        let o = PreviewOptions { window: Window { start, count: None }, max_bytes: Some(200_000), ..Default::default() };
        let p = preview(&doc, &o).unwrap();
        assert!(p.rendered > 0);
        assert!(serde_json::to_string(&p.html).unwrap().len() <= 200_000, "the page stays within the budget");
        // Ids are unique across windows: each window has its own prefix.
        assert!(p.html.contains(&format!("id=\"dc{start}-g")));
        seen.extend(p.slides.iter().map(|v| v["index"].as_u64().unwrap() as usize));
        calls += 1;
        match p.next() {
            Some(n) => start = n,
            None => break,
        }
    }
    assert_eq!(seen, (0..100).collect::<Vec<_>>());
    assert!(calls > 1, "the budget split the deck");
    // Markdown in windows too.
    let md = markdown(&doc, Window { start: 90, count: Some(20) }, "deck", None).unwrap();
    assert_eq!((md.start, md.count, md.next()), (90, 10, None));
    assert!(md.text.contains("## Slide 91: Slide title 90") && md.text.contains("## Slide 100: Slide title 99"));
    assert!(!md.text.contains("## Slide 90:"));
    let md = markdown(&doc, Window::default(), "deck", Some(1_500)).unwrap();
    assert!(md.count < 100 && md.next() == Some(md.count));
}

#[test]
fn backgrounds_gradients_and_shadows_are_vector() {
    let mut s = deck();
    run(&mut s, "slide.new", json!({"layout": "blank"}));
    let id = run(&mut s, "shape.insert", json!({"preset": "roundRect", "rect": [100, 100, 300, 160]}))["id"].clone();
    run(&mut s, "shape.fill", json!({"ids": [id], "gradient": {"stops": [[0, "#1F4E79"], [1, "#9DC3E6"]], "angle": 90}}));
    run(&mut s, "shape.effects", json!({"ids": [id], "shadow": "outer"}));
    let p = page_of(&s, Window::default());
    let art = art_of(&p.html, 0);
    assert!(art.contains("<linearGradient"), "the gradient as a paint server");
    assert!(art.contains("<feGaussianBlur"), "the shadow's blur");
    assert!(!p.html.contains("data:image"), "nothing painted to pixels");
}
