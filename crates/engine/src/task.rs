//! WHIQ: progress and cancellation of long work.
//!
//! A host runs work inside [`scope`] with a [`Task`]; long loops (building a deck slide by slide,
//! exporting pages and frames, reading a presentation's slides) call [`step`] at their safe points.
//! `step` records the progress and, once [`Task::cancel`] was called from another thread, fails
//! with [`cancelled`]: the command fails there, and the session puts the presentation back as it
//! was before the command (a failed command changes nothing), so it stays consistent.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::EngineError;

/// Progress and a cancel flag shared between the thread doing the work and its host.
///
/// Work in several stages names each one with [`phase`] and the share of the whole it covers
/// (`lo..hi`); [`Task::overall`] maps the stage's own steps into that span, so the progress of the
/// whole never goes back when a new stage starts counting from 0.
#[derive(Debug)]
pub struct Task {
    cancelled: AtomicBool,
    done: AtomicU64,
    total: AtomicU64,
    stage: Mutex<String>,
    /// Every stage named so far, in order.
    stages: Mutex<Vec<String>>,
    span: Mutex<(f64, f64)>,
    /// The highest overall progress reported, as `f64` bits (non-negative floats order as their bits).
    peak: AtomicU64,
}

impl Default for Task {
    fn default() -> Self {
        Task {
            cancelled: AtomicBool::new(false),
            done: AtomicU64::new(0),
            total: AtomicU64::new(0),
            stage: Mutex::new(String::new()),
            stages: Mutex::new(Vec::new()),
            span: Mutex::new((0.0, 1.0)),
            peak: AtomicU64::new(0),
        }
    }
}

impl Task {
    pub fn new() -> Arc<Self> {
        Arc::new(Task::default())
    }
    /// Ask the work to stop at its next safe point.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
    /// (done, total) of the current stage; total 0 when unknown.
    pub fn progress(&self) -> (u64, u64) {
        (self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed))
    }
    /// Share done, 0..1 (`None` when unknown).
    pub fn fraction(&self) -> Option<f64> {
        let (d, t) = self.progress();
        (t > 0).then(|| (d as f64 / t as f64).clamp(0.0, 1.0))
    }
    pub fn set(&self, done: u64, total: u64) {
        self.total.store(total, Ordering::Relaxed);
        self.done.store(done.min(total.max(done)), Ordering::Relaxed);
    }
    pub fn set_stage(&self, stage: &str) {
        if let Ok(mut s) = self.stage.lock() {
            *s = stage.to_string();
        }
        if let Ok(mut v) = self.stages.lock()
            && v.last().is_none_or(|l| l != stage)
        {
            v.push(stage.to_string());
        }
    }
    /// The stages named so far, in order (a stage entered twice in a row counts once).
    pub fn stages(&self) -> Vec<String> {
        self.stages.lock().map(|v| v.clone()).unwrap_or_default()
    }
    pub fn stage(&self) -> String {
        self.stage.lock().map(|s| s.clone()).unwrap_or_default()
    }
    /// Start stage `name`, covering `lo..hi` of the whole (0..1); its steps count from 0.
    pub fn set_phase(&self, name: &str, lo: f64, hi: f64) {
        let lo = lo.clamp(0.0, 1.0);
        let hi = hi.clamp(lo, 1.0);
        self.record(lo);
        if let Ok(mut s) = self.span.lock() {
            *s = (lo, hi);
        }
        self.total.store(0, Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
        self.set_stage(name);
    }
    fn record(&self, v: f64) -> f64 {
        let bits = v.clamp(0.0, 1.0).to_bits();
        f64::from_bits(self.peak.fetch_max(bits, Ordering::Relaxed).max(bits))
    }
    /// Share of the whole work done, 0..1: the current stage's steps within its span. Never
    /// decreases.
    pub fn overall(&self) -> f64 {
        let (lo, hi) = self.span.lock().map(|s| *s).unwrap_or((0.0, 1.0));
        self.record(lo + (hi - lo) * self.fraction().unwrap_or(0.0))
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Task>>> = const { RefCell::new(None) };
}

/// Run `f` with `task` as this thread's task.
pub fn scope<T>(task: Arc<Task>, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Arc<Task>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let prev = self.0.take();
            CURRENT.with(|c| *c.borrow_mut() = prev);
        }
    }
    let _restore = Restore(CURRENT.with(|c| c.borrow_mut().replace(task)));
    f()
}

/// This thread's task, if any.
pub fn current() -> Option<Arc<Task>> {
    CURRENT.with(|c| c.borrow().clone())
}

/// The error a cancelled command fails with.
pub fn cancelled() -> EngineError {
    EngineError::Other("cancelled at a safe point: the presentation is as it was before this command".into())
}

/// A safe point: record `done` of `total` and stop when the task was cancelled.
pub fn step(done: usize, total: usize) -> Result<(), EngineError> {
    match current() {
        Some(t) => {
            t.set(done as u64, total as u64);
            if t.is_cancelled() { Err(cancelled()) } else { Ok(()) }
        }
        None => Ok(()),
    }
}

/// A safe point without progress.
pub fn check() -> Result<(), EngineError> {
    if current().is_some_and(|t| t.is_cancelled()) { Err(cancelled()) } else { Ok(()) }
}

/// Name the stage the current task is in (shown with its progress).
pub fn stage(name: &str) {
    if let Some(t) = current() {
        t.set_stage(name);
    }
}

/// Start stage `name` of the current task, covering `lo..hi` of the whole (see [`Task::set_phase`]).
pub fn phase(name: &str, lo: f64, hi: f64) {
    if let Some(t) = current() {
        t.set_phase(name, lo, hi);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn steps_report_progress_and_stop_once_cancelled() {
        let t = Task::new();
        let r = scope(t.clone(), || {
            for i in 0..10 {
                if i == 4 {
                    t.cancel();
                }
                step(i, 10)?;
            }
            Ok::<(), EngineError>(())
        });
        assert!(r.is_err());
        assert_eq!(t.progress(), (4, 10));
        assert!(step(1, 2).is_ok(), "outside the scope nothing is cancelled");
    }

    #[test]
    fn phases_map_their_steps_into_the_whole_and_never_go_back() {
        let t = Task::new();
        scope(t.clone(), || {
            phase("reading", 0.0, 0.2);
            step(5, 10).unwrap();
            assert!((t.overall() - 0.1).abs() < 1e-9);
            phase("parsing", 0.2, 0.9);
            assert!((t.overall() - 0.2).abs() < 1e-9, "a new stage starts where the last one ended");
            step(1, 2).unwrap();
            assert!((t.overall() - 0.55).abs() < 1e-9);
            assert_eq!(t.stage(), "parsing");
            // A stage that restarts its count does not move the whole back.
            step(0, 2).unwrap();
            assert!((t.overall() - 0.55).abs() < 1e-9);
        });
    }
}
