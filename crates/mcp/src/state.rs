//! State shared by the protocol loop, the worker, background jobs and the watchdog (WHIQ
//! contract): configuration, jobs, what is open, what was auto-saved, what was evicted.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use deckcraft_engine::model::Presentation;
use serde_json::{Value, json};

/// Server settings (CLI flags).
#[derive(Clone, Debug)]
pub struct Config {
    /// `--author NAME`: who edits, when a call names no `caller`.
    pub author: String,
    pub read_root: Option<PathBuf>,
    pub write_root: Option<PathBuf>,
    /// Results above this many bytes of JSON are trimmed with `truncated` / `continue_from`.
    pub max_result_bytes: usize,
    /// Rendered images above this many bytes are re-rendered smaller.
    pub max_image_bytes: usize,
    /// Opening a file at least this big runs as a job.
    pub open_job_bytes: u64,
    /// Exporting every slide of a deck with at least this many slides runs as a job.
    pub export_job_slides: usize,
    /// A queued call that waited this long is answered `busy` instead.
    pub busy_after_ms: u64,
    /// Evict (auto-save and close) open documents after this long without a call.
    pub idle_evict_secs: Option<u64>,
    /// Evict open documents when the process uses more memory than this.
    pub max_memory_mb: Option<u64>,
    /// `DECKCRAFT_MCP_TEST_HOOKS=1`: honour `_delay_ms` on calls and jobs (contract tests only).
    pub test_hooks: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            author: String::new(),
            read_root: None,
            write_root: None,
            max_result_bytes: 256 * 1024,
            max_image_bytes: 6 * 1024 * 1024,
            open_job_bytes: 24 * 1024 * 1024,
            export_job_slides: 24,
            busy_after_ms: 8_000,
            idle_evict_secs: None,
            max_memory_mb: None,
            test_hooks: std::env::var("DECKCRAFT_MCP_TEST_HOOKS").is_ok_and(|v| v == "1"),
        }
    }
}

/// A file written to keep work that would otherwise be lost.
#[derive(Clone, Debug)]
pub struct Recovered {
    pub path: String,
    pub title: String,
    /// Where the presentation came from (its own file), if anywhere.
    pub original: Option<String>,
    /// `idle`, `memory`, `evict`, `panic`, `shutdown`, `signal`, `close`, or `found` (a previous run's).
    pub reason: String,
    pub at: String,
}

impl Recovered {
    pub fn to_value(&self) -> Value {
        json!({"path": self.path, "title": self.title, "original": self.original, "reason": self.reason, "at": self.at})
    }
}

/// A presentation closed by the server (idle, memory) rather than by the agent.
#[derive(Clone, Debug)]
pub struct Evicted {
    pub title: String,
    pub original: Option<String>,
    /// The auto-saved copy, when it had unsaved work; else reopen `original`.
    pub recovered: Option<String>,
    pub reason: String,
}

impl Evicted {
    pub fn reopen_path(&self) -> Option<&str> {
        self.recovered.as_deref().or(self.original.as_deref())
    }
    pub fn to_value(&self) -> Value {
        json!({"title": self.title, "original": self.original, "recovered": self.recovered, "reason": self.reason, "reopen": self.reopen_path()})
    }
}

/// One open presentation as last seen by the worker.
#[derive(Clone, Debug)]
pub struct DocSnapshot {
    pub title: String,
    pub path: Option<String>,
    pub dirty: bool,
    pub active: bool,
    pub slides: usize,
    pub doc: Arc<Presentation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Running,
    Done,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn name(self) -> &'static str {
        match self {
            JobState::Running => "running",
            JobState::Done => "done",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
        }
    }
}

/// Long work running off the protocol loop.
pub struct Job {
    pub id: String,
    pub kind: String,
    pub detail: String,
    pub state: JobState,
    pub progress: f64,
    pub started: Instant,
    pub finished: Option<Instant>,
    pub result: Option<Value>,
    pub error: Option<Value>,
    /// An `open` job's parsed file, waiting for the worker to add it to the session.
    pub pending_open: Option<deckcraft_engine::cmd::file::LoadedFile>,
    /// Progress and cancellation shared with the job's thread.
    pub task: std::sync::Arc<deckcraft_engine::task::Task>,
}

impl Job {
    /// Progress 0..1: what the job's thread reports at its safe points, at least what was set.
    pub fn effective_progress(&self) -> f64 {
        match self.state {
            JobState::Done => 1.0,
            _ => self.progress.max(self.task.fraction().unwrap_or(0.0) * 0.95),
        }
    }

    pub fn to_value(&self) -> Value {
        let elapsed = self.finished.unwrap_or_else(Instant::now).duration_since(self.started).as_millis() as u64;
        let progress = self.effective_progress();
        let (done, total) = self.task.progress();
        let mut v = json!({
            "job_id": self.id,
            "kind": self.kind,
            "detail": self.detail,
            "state": self.state.name(),
            "progress": (progress * 1000.0).round() / 1000.0,
            "stage": self.task.stage(),
            "steps": {"done": done, "total": total},
            "elapsed_ms": elapsed,
        });
        match self.state {
            JobState::Cancelled => {
                v["result"] = self.result.clone().unwrap_or(Value::Null);
                v["next_step"] = json!("the job was cancelled at a safe point; nothing past it was changed");
            }
            JobState::Running => {
                let retry = retry_hint(elapsed, progress);
                v["retry_after_ms"] = json!(retry);
                v["next_step"] = json!(format!("call job_status {{job_id: \"{}\"}} again in about {retry} ms", self.id));
            }
            JobState::Done => {
                v["result"] = self.result.clone().unwrap_or(Value::Null);
                v["next_step"] = json!("the job is finished; its result is above");
            }
            JobState::Failed => {
                v["error"] = self.error.clone().unwrap_or(Value::Null);
                v["next_step"] = json!("the job failed; see `error.next_step`");
            }
        }
        v
    }
}

/// How long to wait before asking again: a fraction of the time still expected.
pub fn retry_hint(elapsed_ms: u64, progress: f64) -> u64 {
    let left = if progress > 0.02 && progress < 1.0 { (elapsed_ms as f64 * (1.0 - progress) / progress) as u64 } else { elapsed_ms };
    (left / 2).clamp(250, 5_000)
}

/// Everything the threads share.
pub struct Shared {
    pub config: Config,
    pub started: Instant,
    jobs: Mutex<BTreeMap<String, Job>>,
    next_job: AtomicU64,
    docs: Mutex<Vec<DocSnapshot>>,
    recovered: Mutex<Vec<Recovered>>,
    evicted: Mutex<Vec<Evicted>>,
    /// Recovered files not yet named to the agent.
    unannounced: Mutex<Vec<Recovered>>,
    last_activity: Mutex<Instant>,
    /// The call the worker is running: tool and start.
    current: Mutex<Option<(String, Instant)>>,
    pub calls: AtomicU64,
    pub busy_replies: AtomicU64,
    pub backend_description: Mutex<String>,
    /// The task of the call the worker is running (`cancel` stops it at its next safe point).
    current_task: Mutex<Option<std::sync::Arc<deckcraft_engine::task::Task>>>,
}

/// Lock, recovering from a poisoned lock (a panicking thread must not stop the server).
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Shared {
    pub fn new(config: Config) -> Arc<Self> {
        Arc::new(Shared {
            config,
            started: Instant::now(),
            jobs: Mutex::new(BTreeMap::new()),
            next_job: AtomicU64::new(1),
            docs: Mutex::new(vec![]),
            recovered: Mutex::new(vec![]),
            evicted: Mutex::new(vec![]),
            unannounced: Mutex::new(vec![]),
            last_activity: Mutex::new(Instant::now()),
            current: Mutex::new(None),
            calls: AtomicU64::new(0),
            busy_replies: AtomicU64::new(0),
            backend_description: Mutex::new(String::new()),
            current_task: Mutex::new(None),
        })
    }

    // Jobs.
    pub fn new_job(&self, kind: &str, detail: &str) -> String {
        let n = self.next_job.fetch_add(1, Ordering::Relaxed);
        let id = format!("job-{n}");
        let job = Job {
            id: id.clone(),
            kind: kind.into(),
            detail: detail.into(),
            state: JobState::Running,
            progress: 0.0,
            started: Instant::now(),
            finished: None,
            result: None,
            error: None,
            pending_open: None,
            task: deckcraft_engine::task::Task::new(),
        };
        let mut jobs = lock(&self.jobs);
        // Keep the table bounded: drop the oldest finished jobs past 200.
        while jobs.len() >= 200 {
            let Some(old) = jobs.iter().find(|(_, j)| j.state != JobState::Running).map(|(k, _)| k.clone()) else { break };
            jobs.remove(&old);
        }
        jobs.insert(id.clone(), job);
        id
    }
    pub fn with_job<T>(&self, id: &str, f: impl FnOnce(&mut Job) -> T) -> Option<T> {
        lock(&self.jobs).get_mut(id).map(f)
    }
    pub fn job_progress(&self, id: &str, p: f64) {
        self.with_job(id, |j| j.progress = p.clamp(0.0, 1.0));
    }
    pub fn job_done(&self, id: &str, result: Value) {
        self.with_job(id, |j| {
            j.state = JobState::Done;
            j.progress = 1.0;
            j.finished = Some(Instant::now());
            j.result = Some(result);
        });
    }
    pub fn job_cancelled(&self, id: &str, partial: Value) {
        self.with_job(id, |j| {
            j.state = JobState::Cancelled;
            j.finished = Some(Instant::now());
            j.result = Some(partial);
            j.pending_open = None;
        });
    }
    /// The task of job `id` (its thread runs inside it).
    pub fn job_task(&self, id: &str) -> Option<std::sync::Arc<deckcraft_engine::task::Task>> {
        self.with_job(id, |j| j.task.clone())
    }
    /// Ask job `id` to stop at its next safe point. `Some(state)` with its state now.
    pub fn cancel_job(&self, id: &str) -> Option<&'static str> {
        let want = id.trim().to_ascii_lowercase();
        let want = if want.chars().all(|c| c.is_ascii_digit()) { format!("job-{want}") } else { want };
        let mut jobs = lock(&self.jobs);
        let j = jobs.get_mut(&want)?;
        if j.state == JobState::Running {
            j.task.cancel();
            if j.pending_open.take().is_some() {
                // Read but not yet added: dropping it changes nothing.
                j.state = JobState::Cancelled;
                j.finished = Some(Instant::now());
            }
        }
        Some(j.state.name())
    }
    pub fn job_failed(&self, id: &str, error: Value) {
        self.with_job(id, |j| {
            j.state = JobState::Failed;
            j.finished = Some(Instant::now());
            j.error = Some(error);
        });
    }
    /// Look a job up by id (case-insensitively, `7` meaning `job-7`).
    pub fn job_value(&self, id: &str) -> Option<Value> {
        let jobs = lock(&self.jobs);
        let want = id.trim().to_ascii_lowercase();
        let want = if want.chars().all(|c| c.is_ascii_digit()) { format!("job-{want}") } else { want };
        jobs.get(&want).map(Job::to_value)
    }
    pub fn job_ids(&self) -> Vec<String> {
        lock(&self.jobs).keys().cloned().collect()
    }
    pub fn jobs_value(&self) -> Vec<Value> {
        lock(&self.jobs).values().map(Job::to_value).collect()
    }
    pub fn running_jobs(&self) -> usize {
        lock(&self.jobs).values().filter(|j| j.state == JobState::Running).count()
    }
    /// The running `open` job, if any: (id, progress, elapsed ms).
    pub fn running_open(&self) -> Option<(String, f64, u64)> {
        lock(&self.jobs)
            .values()
            .find(|j| j.kind == "open" && j.state == JobState::Running)
            .map(|j| (j.id.clone(), j.effective_progress(), j.started.elapsed().as_millis() as u64))
    }
    /// Parsed files waiting to be added to the session.
    pub fn take_pending_opens(&self) -> Vec<(String, deckcraft_engine::cmd::file::LoadedFile)> {
        let mut jobs = lock(&self.jobs);
        jobs.values_mut().filter_map(|j| j.pending_open.take().map(|f| (j.id.clone(), f))).collect()
    }
    pub fn set_pending_open(&self, id: &str, f: deckcraft_engine::cmd::file::LoadedFile) {
        self.with_job(id, |j| {
            j.progress = 0.95;
            j.pending_open = Some(f);
        });
    }
    pub fn has_pending_opens(&self) -> bool {
        lock(&self.jobs).values().any(|j| j.pending_open.is_some())
    }

    // Documents.
    pub fn set_docs(&self, docs: Vec<DocSnapshot>) {
        *lock(&self.docs) = docs;
    }
    pub fn docs(&self) -> Vec<DocSnapshot> {
        lock(&self.docs).clone()
    }

    // Recovery and eviction.
    pub fn add_recovered(&self, r: Recovered) {
        lock(&self.unannounced).push(r.clone());
        let mut all = lock(&self.recovered);
        all.push(r);
        let n = all.len();
        if n > 500 {
            all.drain(..n - 500);
        }
    }
    pub fn recovered(&self) -> Vec<Recovered> {
        lock(&self.recovered).clone()
    }
    /// Recovered files the agent has not been told about yet (each is named once, then stays in
    /// `server_status`).
    pub fn take_unannounced(&self) -> Vec<Recovered> {
        std::mem::take(&mut *lock(&self.unannounced))
    }
    pub fn add_evicted(&self, e: Evicted) {
        lock(&self.evicted).push(e);
    }
    pub fn evicted(&self) -> Vec<Evicted> {
        lock(&self.evicted).clone()
    }
    pub fn clear_evicted(&self) {
        lock(&self.evicted).clear();
    }

    // Activity.
    pub fn touch(&self) {
        *lock(&self.last_activity) = Instant::now();
    }
    pub fn idle_for(&self) -> Duration {
        lock(&self.last_activity).elapsed()
    }
    pub fn set_current(&self, tool: Option<&str>) {
        *lock(&self.current) = tool.map(|t| (t.to_string(), Instant::now()));
    }
    pub fn set_current_task(&self, t: Option<std::sync::Arc<deckcraft_engine::task::Task>>) {
        *lock(&self.current_task) = t;
    }
    /// Cancel the running call; `Some((tool, elapsed ms))` when one was running.
    pub fn cancel_current(&self) -> Option<(String, u64)> {
        let running = self.current()?;
        if let Some(t) = lock(&self.current_task).as_ref() {
            t.cancel();
        }
        Some(running)
    }
    /// Progress of the running call, when it reports any.
    pub fn current_progress(&self) -> Option<Value> {
        lock(&self.current_task).as_ref().map(|t| {
            let (d, n) = t.progress();
            json!({"done": d, "total": n, "stage": t.stage(), "cancelling": t.is_cancelled()})
        })
    }
    pub fn current(&self) -> Option<(String, u64)> {
        lock(&self.current).as_ref().map(|(t, at)| (t.clone(), at.elapsed().as_millis() as u64))
    }
}

/// Resident memory of this process in bytes, where the platform reports it.
pub fn rss_bytes() -> Option<u64> {
    memory_stats::memory_stats().map(|m| m.physical_mem as u64)
}

/// `20261017-142530` (UTC) for recovered file names.
pub fn stamp() -> String {
    chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string()
}

/// RFC 3339 time for reports.
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
