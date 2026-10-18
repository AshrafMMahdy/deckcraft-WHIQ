//! WHIQ "no dead stops" contract, proven against the built `deckcraft-cli mcp` over stdio:
//! structured errors, busy with retry, jobs, trimmed results, eviction and recovery, `OLD_`
//! renames, path confinement, annotations, caller identity and bidi base direction.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::time::{Duration, Instant};

use common::{Mcp, files_with, scratch, zip_text};
use serde_json::{Value, json};

fn roots(dir: &std::path::Path) -> Vec<String> {
    let d = dir.to_str().unwrap().to_string();
    vec!["--automation-read-root".into(), d.clone(), "--automation-write-root".into(), d]
}

fn start(dir: &std::path::Path, extra: &[&str], env: &[(&str, &str)]) -> Mcp {
    let mut args = roots(dir);
    args.extend(extra.iter().map(|s| s.to_string()));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    Mcp::start(dir, &refs, env)
}

const HOOKS: &[(&str, &str)] = &[("DECKCRAFT_MCP_TEST_HOOKS", "1")];

// §1 ─ every error carries code, reason and next_step; not_found lists the closest matches.
#[test]
fn errors_have_a_code_a_reason_and_a_next_step() {
    let dir = scratch("errors");
    let mut m = start(&dir, &[], &[]);
    let e = m.err("rendr_slid", json!({}));
    assert_eq!(e["code"], "not_found");
    // The text block: `[code] reason` and the next step (the same in every craft app).
    let r = m.call("rendr_slid", json!({}));
    let text = r["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("[not_found] ") && text.contains("\nnext_step: "), "{text}");
    // A protocol error carries the same shape in `data`.
    let v = m.request("nope/nope", json!({}));
    assert!(v["error"]["data"]["next_step"].is_string(), "{v}");
    assert!(e["closest"].as_array().unwrap().iter().any(|c| c == "render_slide"), "{e}");

    m.ok("new_presentation", json!({}));
    let e = m.err("go_to_slide", json!({"index": 9}));
    assert_eq!(e["code"], "out_of_range", "{e}");
    assert!(e["next_step"].as_str().unwrap().contains("0..0"), "{e}");

    let e = m.err("set_text", json!({"id": 999_999, "text": "x"}));
    assert_eq!(e["code"], "not_found", "{e}");
    assert!(!e["closest"].as_array().unwrap().is_empty(), "the shapes that are there: {e}");

    let e = m.err("save_presentation", json!({"path": "/etc/deckcraft-should-not-write.pptx"}));
    assert_eq!(e["code"], "outside_root", "{e}");
    assert!(e["next_step"].as_str().unwrap().contains(dir.to_str().unwrap()), "names the write root: {e}");

    let e = m.err("run_command", json!({"command": "slide.nwe"}));
    assert_eq!(e["code"], "not_found");
    assert!(e["closest"].to_string().contains("slide.new"), "{e}");

    std::fs::write(dir.join("report-2026.pptx"), b"x").unwrap();
    let e = m.err("open_presentation", json!({"path": "report-2025.pptx"}));
    assert_eq!(e["code"], "not_found", "{e}");
    assert!(e["closest"].to_string().contains("report-2026.pptx"), "closest files: {e}");
}

// §5 ─ forgiving names and arguments: either spelling, case, strings for numbers; unknown
// arguments are warnings on a successful call.
#[test]
fn inputs_are_forgiving() {
    let dir = scratch("forgiving");
    let mut m = start(&dir, &[], &[]);
    let r = m.ok("NewPresentation", json!({"Theme": "Harbor", "colour_scheme": "x"}));
    let w = r["warnings"].to_string();
    assert!(w.contains("read as `new_presentation`") && w.contains("colour_scheme"), "{r}");
    let r = m.ok("add-slide", json!({"layout": "titleAndContent", "title": "Two"}));
    assert!(r["warnings"].to_string().contains("add_slide"), "{r}");
    m.ok("go_to_slide", json!({"index": "0"}));
    let r = m.ok("run_command", json!({"cmd": "Slide.New", "params": {"title": "Three"}}));
    assert!(r["warnings"].to_string().contains("slide.new"), "{r}");
    let doc = m.ok("inspect_document", json!({}));
    assert_eq!(doc["slides"].as_array().unwrap().len(), 3);
}

// §2 ─ nothing hangs: a call stuck behind a slow one answers busy with retry_after_ms, and
// server_status answers at once meanwhile.
#[test]
fn busy_comes_with_a_retry_time_and_status_answers_at_once() {
    let dir = scratch("busy");
    let mut m = start(&dir, &["--busy-after-ms", "300"], HOOKS);
    m.ok("new_presentation", json!({}));
    let slow = m.send_call("inspect_document", json!({"_delay_ms": 2500}));
    std::thread::sleep(Duration::from_millis(150));
    let t = Instant::now();
    let st = m.send_call("server_status", json!({}));
    let st = m.recv(st, Duration::from_secs(2));
    assert!(t.elapsed() < Duration::from_millis(1500), "status waited {:?}", t.elapsed());
    assert_eq!(st["result"]["structuredContent"]["current_call"]["tool"], "inspect_document", "{st}");
    let queued = m.send_call("inspect_slide", json!({}));
    let busy = m.recv(queued, Duration::from_secs(2));
    let e = &busy["result"]["structuredContent"]["error"];
    assert_eq!(e["code"], "busy", "{busy}");
    assert!(e["retry_after_ms"].as_u64().unwrap() >= 250, "{busy}");
    let done = m.recv(slow, Duration::from_secs(10));
    assert_eq!(done["result"]["isError"], false, "{done}");
    // After the retry time it is served.
    m.ok("inspect_slide", json!({}));
}

fn wait_job(m: &mut Mcp, id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let j = m.ok("job_status", json!({"job_id": id}));
        match j["status"].as_str().unwrap() {
            "running" => {
                assert!(j["retry_after_ms"].as_u64().is_some(), "{j}");
                assert!(Instant::now() < deadline, "job {id} never finished: {j}");
                std::thread::sleep(Duration::from_millis(100));
            }
            _ => return j,
        }
    }
}

// §2 ─ long work runs as a job: the call returns a job_id at once; job_status reports progress and
// keeps the result.
#[test]
fn long_work_runs_as_a_job() {
    let dir = scratch("jobs");
    let mut m = start(&dir, &["--export-job-slides", "10"], HOOKS);
    m.ok("new_presentation", json!({}));
    let cmds: Vec<Value> = (0..12)
        .map(|i| json!({"command": "slide.new", "params": {"layout": "titleAndContent", "title": format!("Slide {i}"), "body": "a\nb"}}))
        .collect();
    m.ok("batch", json!({"commands": cmds}));
    m.ok("save_presentation", json!({"path": "big.pptx"}));
    let t = Instant::now();
    let r = m.ok("export", json!({"path": "big.pdf"}));
    let id = r["job_id"].as_str().unwrap().to_string();
    assert!(t.elapsed() < Duration::from_secs(5), "returned at once");
    let j = wait_job(&mut m, &id);
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["tool"], "export", "{j}");
    assert!(std::fs::read(dir.join("big.pdf")).unwrap().starts_with(b"%PDF"));
    // Every job reports its stages and progress (WHIQ round 2).
    assert_eq!(j["progress"], 1.0, "{j}");
    assert_eq!(j["stages"], json!(["writing PDF pages", "writing the file"]), "{j}");
    // Every slide as PNG, with progress per slide.
    let r = m.ok("export", json!({"path": "slide.png", "all": true, "scale": 0.25}));
    let j = wait_job(&mut m, r["job_id"].as_str().unwrap());
    assert_eq!(j["result"]["files"].as_array().unwrap().len(), 13, "{j}");
    assert_eq!(j["stages"], json!(["rendering slides"]), "{j}");
    assert_eq!(j["steps"], json!({"done": 13, "total": 13}), "{j}");
    // Any export can be asked to run as a job; a .pptx one reports slides, compression, the file.
    let r = m.ok("export", json!({"path": "copy.pptx", "async": true}));
    let j = wait_job(&mut m, r["job_id"].as_str().unwrap());
    assert_eq!(j["stages"], json!(["writing slides", "compressing the package", "writing the file"]), "{j}");
    assert_eq!(j["progress"], 1.0, "{j}");
    // The result survives: asking again later still gives it.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(m.ok("job_status", json!({"job_id": id}))["status"], "done");
    let all = m.ok("list_jobs", json!({}));
    assert!(all["jobs"].as_array().unwrap().len() >= 2, "{all}");
    assert_eq!(m.ok("job_status", json!({}))["jobs"], all["jobs"]);
    drop(m);

    // A (pretend) very large open: a job, the document tools answer busy until it is in.
    let mut m = start(&dir, &[], HOOKS);
    let r = m.ok("open_presentation", json!({"path": "big.pptx", "async": true, "_delay_ms": 1500}));
    let id = r["job_id"].as_str().unwrap().to_string();
    let e = m.err("inspect_document", json!({}));
    assert_eq!(e["code"], "busy", "{e}");
    assert_eq!(e["job_id"], json!(id));
    let running = m.ok("job_status", json!({"job_id": id}));
    assert_eq!(running["stage"], "reading the file", "{running}");
    let j = wait_job(&mut m, &id);
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["progress"], 1.0, "{j}");
    let stages: Vec<&str> = j["stages"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(stages.first(), Some(&"reading the file"), "{j}");
    assert!(stages.contains(&"reading slides") && stages.last() == Some(&"adding it to the session"), "{j}");
    assert_eq!(m.ok("inspect_document", json!({}))["slides"].as_array().unwrap().len(), 13);
    let e = m.err("job_status", json!({"job_id": "job-99"}));
    assert_eq!(e["code"], "not_found");
}

// §2 ─ a call still running after --job-after-ms carries on as a job (its result kept), and so does
// one sent with async: true, or one the client gave up on (notifications/cancelled).
#[test]
fn a_long_call_becomes_a_job_whose_result_survives() {
    let dir = scratch("call-jobs");
    let mut m = start(&dir, &["--job-after-ms", "300"], HOOKS);
    m.ok("new_presentation", json!({}));
    let t = Instant::now();
    let r = m.ok("inspect_document", json!({"_delay_ms": 1500}));
    assert!(t.elapsed() < Duration::from_millis(1200), "the job id came before the work finished");
    assert_eq!(r["status"], "running", "{r}");
    let id = r["job_id"].as_str().unwrap().to_string();
    let running = m.ok("job_status", json!({"job_id": id}));
    for k in ["job_id", "tool", "status", "elapsed_ms", "progress", "stage", "steps", "stages", "retry_after_ms", "next_step"] {
        assert!(running.get(k).is_some(), "job_status lacks {k}: {running}");
    }
    assert_eq!(m.ok("server_status", json!({}))["current_call"]["job_id"], json!(id));
    let j = wait_job(&mut m, &id);
    assert_eq!(j["status"], "done", "{j}");
    assert_eq!(j["tool"], "inspect_document");
    assert_eq!(j["result"]["slides"].as_array().unwrap().len(), 1, "{j}");
    // async: true on any tool.
    let r = m.ok("render_slide", json!({"scale": 0.1, "async": true}));
    let j = wait_job(&mut m, r["job_id"].as_str().unwrap());
    assert_eq!(j["status"], "done", "{j}");
    let pic = m.call("job_status", json!({"job_id": r["job_id"]}));
    assert!(pic["content"].as_array().unwrap().iter().any(|c| c["type"] == "image"), "the picture comes with the job: {pic}");
    // The client giving up (clients also send this on their own timeouts): kept as a job.
    let slow = m.send_call("inspect_slide", json!({"_delay_ms": 800}));
    std::thread::sleep(Duration::from_millis(100));
    m.notify_with("notifications/cancelled", json!({"requestId": slow, "reason": "client timeout"}));
    let st = m.ok("server_status", json!({}));
    let id = st["current_call"]["job_id"].as_str().expect("the call became a job").to_string();
    let j = wait_job(&mut m, &id);
    assert_eq!(j["status"], "done", "{j}");
}

// §4 ─ oversized results are trimmed with truncated / continue_from, and offset continues.
#[test]
fn oversized_results_are_trimmed_and_continue() {
    let dir = scratch("trim");
    let mut m = start(&dir, &["--max-result-bytes", "8192"], &[]);
    let r = m.call("list_commands", json!({}));
    let sc = &r["structuredContent"];
    assert_eq!(sc["truncated"], true, "{}", sc.to_string().len());
    let from = sc["continue_from"].as_u64().unwrap();
    let total = sc["total"].as_u64().unwrap();
    assert!(from > 0 && from < total, "{from} of {total}");
    assert!(r.to_string().len() < 20_000, "never floods: {}", r.to_string().len());
    let next = m.ok("list_commands", json!({"offset": from, "limit": 5}));
    assert_eq!(next["result"].as_array().unwrap().len(), 5);
    assert_eq!(next["total"].as_u64(), Some(total));
    let st = m.ok("server_status", json!({}));
    for k in ["version", "memory", "documents", "jobs", "roots", "limits"] {
        assert!(st.get(k).is_some(), "server_status has {k}: {st}");
    }
    assert_eq!(st["roots"]["write"], json!(dir.to_str().unwrap()));
}

// §3 ─ an idle eviction auto-saves the work; the next call names the file; it reopens.
#[test]
fn eviction_auto_saves_and_the_next_call_names_the_file() {
    let dir = scratch("evict");
    let mut m = start(&dir, &["--idle-evict-secs", "1"], &[]);
    m.ok("new_presentation", json!({}));
    m.ok("add_slide", json!({"layout": "titleAndContent", "title": "Unsaved work"}));
    std::thread::sleep(Duration::from_millis(2500));
    let e = m.err("inspect_document", json!({}));
    assert_eq!(e["code"], "evicted", "{e}");
    let path = e["recovered_path"].as_str().unwrap().to_string();
    assert!(path.contains(".recovered-") && std::path::Path::new(&path).exists(), "{e}");
    assert!(e["next_step"].as_str().unwrap().contains(&path));
    let r = m.ok("open_presentation", json!({"path": path}));
    assert_eq!(r["slides"], 2, "the work is back: {r}");

    // `evict` (for a supervisor) does the same on demand.
    m.ok("add_slide", json!({"title": "More"}));
    let r = m.ok("evict", json!({}));
    let rec = r["evicted"][0]["recovered_path"].as_str().unwrap().to_string();
    assert!(std::path::Path::new(&rec).exists());
    let e = m.err("add_slide", json!({}));
    assert_eq!(e["code"], "evicted");
}

// §3 ─ SIGTERM saves unsaved work; the next run names the recovered file.
#[cfg(unix)]
#[test]
fn sigterm_saves_unsaved_work_and_the_next_run_names_it() {
    let dir = scratch("sigterm");
    let mut m = start(&dir, &[], &[]);
    m.ok("new_presentation", json!({}));
    m.ok("add_slide", json!({"title": "Before the signal"}));
    let (_, stderr) = m.terminate();
    let found = files_with(&dir, ".recovered-");
    assert_eq!(found.len(), 1, "{stderr}");
    let mut m = start(&dir, &[], &[]);
    let st = m.ok("server_status", json!({}));
    assert!(st["recovered_files"].to_string().contains(found[0].file_name().unwrap().to_str().unwrap()), "{st}");
    let r = m.ok("open_presentation", json!({"path": found[0].to_str().unwrap()}));
    assert_eq!(r["slides"], 2);
}

// §3 ─ an internal failure is an error that names the auto-saved file; the server keeps serving.
#[test]
fn an_internal_failure_saves_the_work_and_the_server_goes_on() {
    let dir = scratch("panic");
    let mut m = start(&dir, &[], HOOKS);
    m.ok("new_presentation", json!({}));
    m.ok("add_slide", json!({"title": "Keep me"}));
    let e = m.err("inspect_document", json!({"_panic": true}));
    assert_eq!(e["code"], "internal", "{e}");
    let saved = e["recovered_path"].as_str().unwrap();
    assert!(std::path::Path::new(saved).exists() && saved.contains(".recovered-"), "{e}");
    assert_eq!(m.ok("inspect_document", json!({}))["slides"].as_array().unwrap().len(), 2);
}

// §3 ─ the end of input never discards unsaved work.
#[test]
fn end_of_input_keeps_unsaved_work() {
    let dir = scratch("eof");
    let mut m = start(&dir, &[], &[]);
    m.ok("new_presentation", json!({}));
    m.ok("save_presentation", json!({"path": "saved.pptx"}));
    m.ok("add_slide", json!({"title": "after the save"}));
    let (ok, stderr) = m.close();
    assert!(ok, "{stderr}");
    let rec = files_with(&dir, "saved.recovered-");
    assert_eq!(rec.len(), 1, "{stderr}");
}

// §6 ─ saving never overwrites silently: OLD_<name> first, unless overwrite: true.
#[test]
fn saves_keep_the_previous_file_as_old() {
    let dir = scratch("old");
    let mut m = start(&dir, &[], &[]);
    m.ok("new_presentation", json!({}));
    m.ok("save_presentation", json!({"path": "deck.pptx"}));
    let first = std::fs::read(dir.join("deck.pptx")).unwrap();
    m.ok("add_slide", json!({"title": "v2"}));
    let r = m.ok("save_presentation", json!({"path": "deck.pptx"}));
    assert_eq!(r["previous_files_renamed"][0]["previous"], json!(dir.join("OLD_deck.pptx").to_str().unwrap()), "{r}");
    assert!(r["notice"].as_str().unwrap().contains("OLD_deck.pptx"));
    assert_eq!(std::fs::read(dir.join("OLD_deck.pptx")).unwrap(), first, "the old file is kept as it was");
    m.ok("add_slide", json!({"title": "v3"}));
    m.ok("save_presentation", json!({"path": "deck.pptx"}));
    assert!(dir.join("OLD_2_deck.pptx").exists(), "a second save keeps the first OLD_ too");
    let r = m.ok("save_presentation", json!({"path": "deck.pptx", "overwrite": true}));
    assert!(r.get("previous_files_renamed").is_none(), "{r}");
    assert!(!dir.join("OLD_3_deck.pptx").exists());
    // Exports too.
    m.ok("export", json!({"path": "s.png", "scale": 0.2}));
    let r = m.ok("export", json!({"path": "s.png", "scale": 0.2}));
    assert!(dir.join("OLD_s.png").exists(), "{r}");
    // No temporary files are left behind (atomic temp + rename).
    assert!(files_with(&dir, ".saving").is_empty());
}

// §6 ─ paths are confined; relative paths land in the write root; links cannot escape.
#[test]
fn paths_are_confined_to_the_roots() {
    let base = scratch("confine");
    let (work, outside) = (base.join("work"), base.join("outside"));
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let mut m = start(&work, &[], &[]);
    m.ok("new_presentation", json!({}));
    let r = m.ok("save_presentation", json!({"path": "rel.pptx"}));
    assert!(work.join("rel.pptx").exists(), "{r}");
    let e = m.err("save_presentation", json!({"path": outside.join("x.pptx").to_str().unwrap()}));
    assert_eq!(e["code"], "outside_root");
    let e = m.err("save_presentation", json!({"path": "../outside/y.pptx"}));
    assert_eq!(e["code"], "outside_root");
    assert!(!outside.join("y.pptx").exists());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, work.join("link")).unwrap();
        let e = m.err("save_presentation", json!({"path": "link/z.pptx"}));
        assert_eq!(e["code"], "outside_root", "a link out of the root is refused: {e}");
        assert!(!outside.join("z.pptx").exists());
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(outside.join("secret.txt"), work.join("secret.txt")).unwrap();
        let e = m.err("open_presentation", json!({"path": "secret.txt"}));
        assert_eq!(e["code"], "outside_root", "{e}");
    }
}

// §8 ─ annotations: readOnlyHint only on reads, destructiveHint on what can delete or overwrite,
// idempotentHint where true; structuredContent on every result (checked by the client).
#[test]
fn annotations_tell_reads_from_writes() {
    let dir = scratch("annotations");
    let mut m = start(&dir, &[], &[]);
    let tools = m.request("tools/list", json!({}))["result"]["tools"].as_array().unwrap().clone();
    let reads = [
        "list_commands",
        "inspect_document",
        "inspect_slide",
        "render_slide",
        "screenshot",
        "server_status",
        "job_status",
        "list_jobs",
        "roundtrip_check",
    ];
    let destructive = ["run_command", "batch", "save_presentation", "export", "set_text", "close_presentation", "key", "build_deck"];
    let idempotent = ["go_to_slide", "select", "set_text", "adopt_template", "cancel", "evict"];
    assert!(tools.len() >= 30);
    for t in &tools {
        let name = t["name"].as_str().unwrap();
        let a = &t["annotations"];
        for k in ["readOnlyHint", "destructiveHint", "idempotentHint", "openWorldHint"] {
            assert!(a[k].is_boolean(), "{name}: {k}");
        }
        assert_eq!(a["readOnlyHint"], reads.contains(&name), "{name}");
        if reads.contains(&name) {
            assert_eq!(a["destructiveHint"], false, "{name}");
            assert_eq!(a["idempotentHint"], true, "{name}");
        }
        if destructive.contains(&name) {
            assert_eq!(a["destructiveHint"], true, "{name}");
        }
        if idempotent.contains(&name) {
            assert_eq!(a["idempotentHint"], true, "{name}");
        }
        assert!(t["inputSchema"]["properties"]["caller"].is_object(), "{name} takes caller");
    }
    // Every answer so far had structuredContent (Mcp::check); one of each kind.
    m.ok("new_presentation", json!({}));
    let r = m.call("render_slide", json!({"scale": 0.1}));
    assert_eq!(r["content"][0]["type"], "image");
    assert!(r["structuredContent"]["width"].as_u64().is_some(), "{}", r["structuredContent"]);
}

// §7 ─ the caller's name: --author at start, `caller` per call; comments and the saved file carry it.
#[test]
fn the_caller_is_named_on_comments_and_files() {
    let dir = scratch("caller");
    let mut m = start(&dir, &["--author", "Server Bot"], &[]);
    m.ok("new_presentation", json!({}));
    m.ok("run_command", json!({"command": "comment.add", "params": {"text": "Check the figures"}, "caller": "Ashraf Mahdy"}));
    m.ok("run_command", json!({"command": "comment.add", "params": {"text": "Default author"}}));
    let c = m.ok("run_command", json!({"command": "comment.list"}));
    let s = c.to_string();
    assert!(s.contains("Ashraf Mahdy") && s.contains("Server Bot"), "{s}");
    m.ok("save_presentation", json!({"path": "by-caller.pptx", "caller": "Ashraf Mahdy"}));
    let core = zip_text(&dir.join("by-caller.pptx"), "docProps/core.xml");
    assert!(core.contains("<cp:lastModifiedBy>Ashraf Mahdy</cp:lastModifiedBy>"), "{core}");
    m.ok("save_presentation", json!({"path": "by-server.pptx"}));
    let core = zip_text(&dir.join("by-server.pptx"), "docProps/core.xml");
    assert!(core.contains("<cp:lastModifiedBy>Server Bot</cp:lastModifiedBy>"), "{core}");
}

/// Dark pixels per half of a region of an RGBA image.
fn ink(png: &[u8], x0: u32, y0: u32, x1: u32, y1: u32) -> (usize, usize) {
    let img = image::load_from_memory(png).unwrap().to_rgba8();
    let mid = (x0 + x1) / 2;
    let (mut l, mut r) = (0, 0);
    for y in y0..y1.min(img.height()) {
        for x in x0..x1.min(img.width()) {
            let p = img.get_pixel(x, y);
            if u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2]) < 300 {
                if x < mid { l += 1 } else { r += 1 }
            }
        }
    }
    (l, r)
}

// §10 ─ a paragraph with no direction set takes it from its first strong character: Arabic is
// right-to-left and right-aligned, in the file and in the rendered slide (shaped with the bundled
// Arabic fallback font when the build has craft-fonts).
#[test]
fn arabic_paragraphs_are_right_to_left_by_default() {
    let dir = scratch("bidi");
    let mut m = start(&dir, &[], &[]);
    m.ok("new_presentation", json!({}));
    m.ok("add_slide", json!({"layout": "titleAndContent", "title": "مراجعة المشروع", "body": "هذا نص عربي قصير"}));
    m.ok("add_slide", json!({"layout": "titleAndContent", "title": "English", "body": "A short English line"}));
    m.ok("save_presentation", json!({"path": "bidi.pptx"}));
    let xml = zip_text(&dir.join("bidi.pptx"), "ppt/slides/slide2.xml");
    assert!(xml.contains("rtl=\"1\"") && xml.contains("algn=\"r\""), "{xml}");
    let en = zip_text(&dir.join("bidi.pptx"), "ppt/slides/slide3.xml");
    assert!(!en.contains("rtl=\"1\""));
    // A text box added by hand, no direction given: right-to-left in the file too.
    m.ok("go_to_slide", json!({"index": 2}));
    m.ok("add_text_box", json!({"x": 60, "y": 460, "w": 840, "text": "سطر مضاف"}));
    m.ok("save_presentation", json!({"path": "bidi2.pptx"}));
    assert!(zip_text(&dir.join("bidi2.pptx"), "ppt/slides/slide3.xml").contains("rtl=\"1\""));
    // Rendered: the Arabic body sits on the right of its box, the English one on the left.
    for (index, arabic) in [(1, true), (2, false)] {
        let sl = m.ok("inspect_slide", json!({"index": index}));
        let body = sl["shapes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["text"].as_str().is_some_and(|t| t.contains("نص") || t.contains("short")))
            .unwrap()
            .clone();
        let b: Vec<f64> = body["box"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
        let png = dir.join(format!("s{index}.png"));
        m.ok("render_slide", json!({"slide": index, "path": png.to_str().unwrap()}));
        let (l, r) = ink(&std::fs::read(&png).unwrap(), b[0] as u32, b[1] as u32, (b[0] + b[2]) as u32, (b[1] + 60.0) as u32);
        if arabic {
            assert!(r > 30 && r > l * 3, "Arabic ink should be on the right: left {l}, right {r}");
        } else {
            assert!(l > 30 && l > r * 3, "English ink should be on the left: left {l}, right {r}");
        }
    }
}

// Round 2 ─ a running call can be cancelled: it stops at its next safe point and the document is
// as it was before it; cancel answers at once; queued calls can be dropped; jobs can be cancelled.
#[test]
fn a_running_call_stops_at_a_safe_point_when_cancelled() {
    let dir = scratch("cancel");
    let mut m = start(&dir, &["--busy-after-ms", "30000"], HOOKS);
    m.ok("new_presentation", json!({}));
    let slow = m.send_call("inspect_document", json!({"_delay_ms": 20000}));
    let queued1 = m.send_call("add_slide", json!({"title": "never"}));
    let queued2 = m.send_call("inspect_slide", json!({}));
    std::thread::sleep(Duration::from_millis(200));
    let t = Instant::now();
    let c = m.send_call("cancel", json!({"queued": true}));
    let c = m.recv(c, Duration::from_secs(2));
    assert!(t.elapsed() < Duration::from_secs(1), "cancel answers at once");
    let sc = &c["result"]["structuredContent"];
    assert_eq!(sc["cancelled"], true, "{c}");
    assert_eq!(sc["status"], "cancelling", "{c}");
    assert_eq!(sc["tool"], "inspect_document");
    assert_eq!(sc["dropped_from_queue"], 2);
    for q in [queued1, queued2] {
        let r = m.recv(q, Duration::from_secs(2));
        assert_eq!(r["result"]["structuredContent"]["error"]["code"], "cancelled", "{r}");
    }
    let r = m.recv(slow, Duration::from_secs(3));
    assert_eq!(r["result"]["structuredContent"]["error"]["code"], "cancelled", "{r}");
    assert!(t.elapsed() < Duration::from_secs(3), "stopped long before its 20 s");
    // Nothing else changed, and the server goes on.
    assert_eq!(m.ok("inspect_document", json!({}))["slides"].as_array().unwrap().len(), 1);
    let c = m.ok("cancel", json!({}));
    assert_eq!((c["cancelled"].as_bool(), c["status"].as_str()), (Some(false), Some("not_running")), "nothing is running: {c}");
    // A batch stopped part way is undone as a whole.
    let cmds: Vec<Value> = (0..30_000).map(|i| json!({"command": "slide.new", "params": {"title": format!("S{i}")}})).collect();
    let b = m.send_call("batch", json!({"commands": cmds}));
    std::thread::sleep(Duration::from_millis(300));
    m.ok("cancel", json!({}));
    let r = m.recv(b, Duration::from_secs(30));
    assert_eq!(r["result"]["structuredContent"]["error"]["code"], "cancelled", "{r}");
    assert_eq!(m.ok("inspect_document", json!({}))["slides"].as_array().unwrap().len(), 1, "nothing of the batch stays");

    // (Real commands stopping between slides, pages and frames: crates/engine/src/tests_task.rs.)

    // A job: the file it was reading is never added.
    m.ok("save_presentation", json!({"path": "c.pptx"}));
    m.ok("close_presentation", json!({}));
    let r = m.ok("open_presentation", json!({"path": "c.pptx", "async": true, "_delay_ms": 1500}));
    let id = r["job_id"].as_str().unwrap().to_string();
    let c = m.ok("cancel", json!({"job_id": id}));
    assert_eq!(c["cancelled"], true, "{c}");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let j = m.ok("job_status", json!({"job_id": id}));
        if j["status"] != "running" {
            assert_eq!(j["status"], "cancelled", "{j}");
            assert_eq!(j["error"]["code"], "cancelled", "{j}");
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(m.err("inspect_document", json!({}))["code"], "no_document");
}

/// Wait (up to 10 s) for a file in `dir` whose name contains `needle`.
fn wait_for_file(dir: &std::path::Path, needle: &str) -> Vec<std::path::PathBuf> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        // Whole files only (an atomic write's temporary file shares the name's start).
        let f: Vec<_> = files_with(dir, needle).into_iter().filter(|p| p.extension().is_some_and(|e| e == "pptx")).collect();
        if !f.is_empty() || Instant::now() > deadline {
            return f;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// §3 (WHIQ round 2) ─ periodic checkpoints: a hard kill (SIGKILL / TerminateProcess) loses at most
// the last few edits, and the next run names the checkpoint.
#[test]
fn a_hard_kill_loses_little_thanks_to_checkpoints() {
    let dir = scratch("checkpoint-edits");
    let mut m = start(&dir, &["--checkpoint-edits", "3", "--checkpoint-secs", "0"], &[]);
    m.ok("new_presentation", json!({}));
    m.ok("add_slide", json!({"title": "one"}));
    m.ok("add_slide", json!({"title": "two"}));
    std::thread::sleep(Duration::from_millis(600));
    assert!(files_with(&dir, ".recovered-").is_empty(), "2 edits: not yet due");
    m.ok("add_slide", json!({"title": "three"}));
    let found = wait_for_file(&dir, ".recovered-");
    assert_eq!(found.len(), 1, "{}", m.stderr());
    // The file is in place a moment before the checkpointer records it.
    let name = found[0].file_name().unwrap().to_str().unwrap().to_string();
    let mut st = m.ok("server_status", json!({}));
    for _ in 0..100 {
        if st["checkpoints"]["files"].to_string().contains(&name) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
        st = m.ok("server_status", json!({}));
    }
    assert_eq!(st["checkpoints"]["every_edits"], 3, "{st}");
    assert!(st["checkpoints"]["files"].to_string().contains(&name), "{st}");
    assert_eq!(st["checkpoints"]["writes"], 1, "{st}");
    assert!(st["recovered_files"].as_array().unwrap().is_empty(), "a live checkpoint is not a recovered file yet: {st}");
    assert!(st["memory"]["rss_bytes"].as_u64().unwrap() > 1024 * 1024, "measured by the OS: {st}");
    assert_eq!(st["memory"]["measured"], true);
    // More edits replace the same checkpoint in place (no pile of files, no OLD_ copies).
    for t in ["four", "five", "six"] {
        m.ok("add_slide", json!({"title": t}));
    }
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(files_with(&dir, ".recovered-").len(), 1);
    assert!(files_with(&dir, "OLD_").is_empty());
    let stderr = m.kill();
    let found = files_with(&dir, ".recovered-");
    assert_eq!(found.len(), 1, "{stderr}");
    // The next run names it; it opens with every checkpointed slide.
    let mut m = start(&dir, &[], &[]);
    let st = m.ok("server_status", json!({}));
    let name = found[0].file_name().unwrap().to_str().unwrap().to_string();
    assert!(st["recovered_files"].to_string().contains(&name), "{st}");
    let r = m.ok("open_presentation", json!({"path": found[0].to_str().unwrap()}));
    assert_eq!(r["slides"], 7, "{r}");
}

// The time trigger, and a save supersedes the checkpoint.
#[test]
fn checkpoints_follow_time_and_go_once_the_work_is_saved() {
    let dir = scratch("checkpoint-secs");
    let mut m = start(&dir, &["--checkpoint-edits", "0", "--checkpoint-secs", "1"], &[]);
    m.ok("new_presentation", json!({}));
    m.ok("add_slide", json!({"title": "one edit"}));
    let t = Instant::now();
    let found = wait_for_file(&dir, ".recovered-");
    assert_eq!(found.len(), 1, "{}", m.stderr());
    assert!(t.elapsed() >= Duration::from_millis(700), "not before the interval");
    m.ok("save_presentation", json!({"path": "kept.pptx"}));
    let deadline = Instant::now() + Duration::from_secs(10);
    while found[0].exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!found[0].exists(), "saved: the checkpoint is superseded");
    // Unsaved again: a new checkpoint, named after the file; closing brings that same file up to
    // date (one recovered file for the stretch of work, as in every craft app).
    m.ok("add_slide", json!({"title": "after the save"}));
    let found = wait_for_file(&dir, "kept.recovered-");
    assert_eq!(found.len(), 1, "{}", m.stderr());
    let (ok, stderr) = m.close();
    assert!(ok, "{stderr}");
    assert_eq!(files_with(&dir, "kept.recovered-"), found, "{stderr}");
}

// The round-trip check (as in WordCraft): open + save in memory, report what would be lost.
#[test]
fn roundtrip_check_reports_what_a_save_would_keep() {
    let dir = scratch("roundtrip");
    let mut m = start(&dir, &[], &[]);
    m.ok("new_presentation", json!({}));
    m.ok("add_slide", json!({"layout": "titleAndContent", "title": "Kept", "body": "a"}));
    m.ok("save_presentation", json!({"path": "deck.pptx"}));
    let r = m.ok("roundtrip_check", json!({"path": "deck.pptx"}));
    assert_eq!(r["lossless"], true, "{r}");
    for k in ["missingParts", "addedParts", "changedBinaryParts", "missingRelationships", "stories", "problems"] {
        assert!(r.get(k).is_some(), "{k}: {r}");
    }
    let e = m.err("roundtrip_check", json!({"path": "missing.pptx"}));
    assert_eq!(e["code"], "not_found", "{e}");
}

// server_status in the shape every craft app reports.
#[test]
fn server_status_has_the_shared_shape() {
    let dir = scratch("status");
    let mut m = start(&dir, &["--author", "Status Bot"], &[]);
    m.ok("new_presentation", json!({}));
    let st = m.ok("server_status", json!({}));
    for k in [
        "server",
        "version",
        "protocol",
        "pid",
        "uptime_ms",
        "author",
        "memory",
        "fonts",
        "roots",
        "documents",
        "current_call",
        "jobs",
        "recovered_files",
        "evicted",
        "checkpoints",
        "limits",
    ] {
        assert!(st.get(k).is_some(), "server_status lacks {k}: {st}");
    }
    for k in ["rss_bytes", "committed_bytes", "limit_bytes", "measured", "source"] {
        assert!(st["memory"].get(k).is_some(), "memory lacks {k}: {st}");
    }
    for k in ["every_edits", "every_secs", "pending_edits", "writes", "files"] {
        assert!(st["checkpoints"].get(k).is_some(), "checkpoints lacks {k}: {st}");
    }
    assert_eq!(st["documents"][0]["active"], true, "{st}");
    assert!(st["fonts"]["faces"].is_number(), "{st}");
}

// Progress notifications for a call that asks for them (`_meta.progressToken`), as in every craft
// app: the value only grows; the message names the stage.
#[test]
fn a_call_with_a_progress_token_gets_progress_notifications() {
    let dir = scratch("progress");
    let mut m = start(&dir, &[], HOOKS);
    m.ok("new_presentation", json!({}));
    let id = m.send("tools/call", json!({"name": "inspect_document", "arguments": {"_delay_ms": 2500}, "_meta": {"progressToken": "tok-1"}}));
    let r = m.recv(id, Duration::from_secs(10));
    assert_eq!(r["result"]["isError"], false, "{r}");
    let notes: Vec<&Value> = m.seen.iter().filter(|n| n["method"] == "notifications/progress" && n["params"]["progressToken"] == "tok-1").collect();
    assert!(notes.len() >= 2, "{:?}", m.seen);
    let values: Vec<f64> = notes.iter().map(|n| n["params"]["progress"].as_f64().unwrap()).collect();
    assert!(values.windows(2).all(|w| w[1] > w[0]), "progress only grows: {values:?}");
    assert!(notes[0]["params"]["message"].as_str().unwrap().contains(" s)"), "{:?}", notes[0]);
}
