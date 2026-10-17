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
#[derive(Debug, Default)]
pub struct Task {
    cancelled: AtomicBool,
    done: AtomicU64,
    total: AtomicU64,
    stage: Mutex<String>,
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
    }
    pub fn stage(&self) -> String {
        self.stage.lock().map(|s| s.clone()).unwrap_or_default()
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
}
