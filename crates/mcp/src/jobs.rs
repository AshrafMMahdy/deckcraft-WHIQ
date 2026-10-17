//! Long work as jobs (WHIQ contract §2): opening a very large file and exporting every slide run
//! on their own thread; the call returns a `job_id` at once and `job_status` reports progress and
//! the result, which stays available however long the client took.

use std::sync::Arc;

use deckcraft_engine::file_access::{self, AutomationRoots};
use deckcraft_engine::model::Presentation;
use serde_json::{Value, json};

use crate::contract::{ErrorContext, classify};
use crate::state::Shared;

fn error_value(shared: &Shared, tool: &str, msg: &str) -> Value {
    let cx = ErrorContext {
        tool,
        read_root: shared.config.read_root.as_ref().map(|p| p.display().to_string()),
        write_root: shared.config.write_root.as_ref().map(|p| p.display().to_string()),
        slide_count: None,
    };
    classify(msg, &cx).to_value()
}

fn test_delay(shared: &Shared, args: &Value) {
    if shared.config.test_hooks
        && let Some(ms) = args.get("_delay_ms").and_then(Value::as_u64)
    {
        std::thread::sleep(std::time::Duration::from_millis(ms.min(60_000)));
    }
}

/// Read and parse `path` on a job thread; the worker adds it to the session when it is ready.
pub fn spawn_open(shared: &Arc<Shared>, roots: Option<Arc<AutomationRoots>>, path: String, args: Value) -> String {
    let id = shared.new_job("open", &path);
    let sh = Arc::clone(shared);
    let jid = id.clone();
    let task = shared.job_task(&id).unwrap_or_default();
    let spawned = std::thread::Builder::new().name(format!("deckcraft-{jid}")).spawn(move || {
        let t = task.clone();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            deckcraft_engine::task::scope(t, || {
                file_access::confine(roots.as_ref(), || {
                    deckcraft_engine::task::phase("reading the file", 0.0, 0.15);
                    test_delay(&sh, &args);
                    deckcraft_engine::cmd::file::read_presentation_file(&path).map_err(|e| e.to_string())
                })
            })
        }));
        if task.is_cancelled() {
            sh.job_cancelled(&jid, json!({"opened": false}));
            return;
        }
        match r {
            Ok(Ok(loaded)) => {
                task.set_phase("adding it to the session", 0.9, 1.0);
                sh.set_pending_open(&jid, loaded)
            }
            Ok(Err(e)) => sh.job_failed(&jid, error_value(&sh, "open_presentation", &e)),
            Err(_) => sh.job_failed(&jid, error_value(&sh, "open_presentation", "the file could not be read (internal error while parsing)")),
        }
    });
    if let Err(e) = spawned {
        shared.job_failed(&id, error_value(shared, "open_presentation", &format!("could not start the job: {e}")));
    }
    id
}

/// Export a snapshot of the presentation on a job thread.
pub fn spawn_export(
    shared: &Arc<Shared>,
    roots: Option<Arc<AutomationRoots>>,
    doc: Arc<Presentation>,
    current: usize,
    args: Value,
    overwrite: bool,
) -> String {
    let path = args.get("path").and_then(Value::as_str).unwrap_or_default().to_string();
    let id = shared.new_job("export", &path);
    let sh = Arc::clone(shared);
    let jid = id.clone();
    let task = shared.job_task(&id).unwrap_or_default();
    let spawned = std::thread::Builder::new().name(format!("deckcraft-{jid}")).spawn(move || {
        let t = task.clone();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            deckcraft_engine::task::scope(t, || {
                file_access::confine(roots.as_ref(), || {
                    test_delay(&sh, &args);
                    let run = || export(&doc, current, &args);
                    if overwrite { (run(), vec![]) } else { deckcraft_engine::io::keep_previous(run) }
                })
            })
        }));
        if task.is_cancelled() {
            let note = match &r {
                Ok((Err(e), _)) => e.clone(),
                _ => String::new(),
            };
            sh.job_cancelled(&jid, json!({"note": note}));
            return;
        }
        match r {
            Ok((Ok(mut v), renamed)) => {
                if !renamed.is_empty() {
                    let abs = |p: &str| std::path::absolute(p).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| p.to_string());
                    v["previous_files_renamed"] =
                        json!(renamed.iter().map(|r| json!({"path": abs(&r.path), "previous": abs(&r.previous)})).collect::<Vec<_>>());
                }
                sh.job_done(&jid, v)
            }
            Ok((Err(e), _)) => sh.job_failed(&jid, error_value(&sh, "export", &e)),
            Err(_) => sh.job_failed(&jid, error_value(&sh, "export", "the export failed (internal error)")),
        }
    });
    if let Err(e) = spawned {
        shared.job_failed(&id, error_value(shared, "export", &format!("could not start the job: {e}")));
    }
    id
}

/// What `file.export` does, on a snapshot, reporting progress slide by slide for images.
fn export(doc: &Presentation, current: usize, p: &Value) -> Result<Value, String> {
    use deckcraft_engine::cmd::file as f;
    let path = p.get("path").and_then(Value::as_str).ok_or("export: missing `path`")?.to_string();
    let format = p.get("format").and_then(Value::as_str).unwrap_or_else(|| f::format_for_path(&path)).to_string();
    let scale = p.get("scale").and_then(Value::as_f64).filter(|s| s.is_finite()).unwrap_or(2.0).clamp(0.01, 16.0);
    let n = doc.slides.len();
    let write = |out: &str, bytes: &[u8]| deckcraft_engine::io::write_file(out, bytes);
    // The last stage of every format but images: the file, with progress by bytes.
    let write_whole = |out: &str, bytes: &[u8]| {
        deckcraft_engine::task::phase("writing the file", 0.9, 1.0);
        deckcraft_engine::io::write_file_reporting(out, bytes)
    };
    match format.as_str() {
        "png" | "jpeg" | "jpg" => {
            let slides: Vec<usize> = match p.get("slide").and_then(Value::as_u64) {
                Some(i) => vec![i as usize],
                None if p.get("all").and_then(Value::as_bool).unwrap_or(false) => (0..n).collect(),
                None => vec![current],
            };
            if let Some(i) = slides.iter().find(|i| **i >= n) {
                return Err(format!("file.export: no slide {i}"));
            }
            let scale = f::image_scale(doc, scale);
            let mut written: Vec<String> = vec![];
            deckcraft_engine::task::phase("rendering slides", 0.0, 1.0);
            for (k, i) in slides.iter().enumerate() {
                // A safe point between slides: the files written so far are whole.
                deckcraft_engine::task::step(k, slides.len()).map_err(|e| format!("{e}; written so far: {}", written.join(", ")))?;
                let img = deckcraft_render::render_slide(doc, *i, &deckcraft_render::RenderOpts { scale, ..Default::default() });
                let dpi = deckcraft_render::dpi_for_scale(scale);
                let bytes = if format == "png" { img.to_png_at(dpi) } else { img.to_jpeg_at(92, Some(dpi)) };
                let out = if slides.len() == 1 {
                    path.clone()
                } else {
                    let pth = std::path::Path::new(&path);
                    let stem = pth.file_stem().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| "Slide".into());
                    let ext = pth.extension().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| format.clone());
                    pth.with_file_name(format!("{stem}{}.{ext}", k + 1)).to_string_lossy().to_string()
                };
                write(&out, &bytes).map_err(|e| if written.is_empty() { e } else { format!("{e} (slides 1–{k} were written)") })?;
                written.push(out);
                deckcraft_engine::task::step(k + 1, slides.len()).map_err(|e| e.to_string())?;
            }
            Ok(json!({"files": written, "slides": slides.len()}))
        }
        "pdf" => {
            let bytes = f::pdf_bytes(doc, p).map_err(|e| e.to_string())?;
            write_whole(&path, &bytes)?;
            Ok(json!({"path": path, "bytes": bytes.len()}))
        }
        "gif" => {
            let mut gp = p.clone();
            if let Some(text) = p.get("slides").and_then(Value::as_str) {
                gp["slides"] = json!(f::parse_slide_range(text, n).map_err(|e| e.to_string())?);
            }
            let bytes = f::gif_bytes(doc, &gp).map_err(|e| e.to_string())?;
            write_whole(&path, &bytes)?;
            Ok(json!({"path": path, "bytes": bytes.len()}))
        }
        "pptx" | "potx" | "ppsx" | "deckcraft" | "outline" | "txt" => {
            let bytes = f::save_bytes(doc, &format).map_err(|e| e.to_string())?;
            write_whole(&path, &bytes)?;
            Ok(json!({"path": path, "bytes": bytes.len()}))
        }
        other => Err(format!("file.export: unknown format `{other}`")),
    }
}
