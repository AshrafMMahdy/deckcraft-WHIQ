//! WHIQ (owner, 2026-11-05, "Undo the call"): a call that fails half way leaves the presentations
//! exactly as they were before it — nothing of the call is applied, and the error says so.

use deckcraft_engine::{DocState, Session};

/// What a call that fails half way says about its work.
pub(crate) const NOTHING_APPLIED: &str = "nothing of this call was applied — send it again (fixed)";

/// The open presentations as a call found them: each one whole (content, selection, undo history,
/// saved state) and which one was active. Cheap: a presentation's content is shared, not copied.
pub(crate) struct Before {
    docs: Vec<DocState>,
    active: Option<u64>,
}

impl Before {
    pub(crate) fn take(s: &Session) -> Self {
        Before { docs: s.documents().to_vec(), active: s.active().map(|d| d.uid) }
    }

    /// Put the session back as the call found it: presentations the call opened are closed, the
    /// ones it changed (or closed) come back as they were, and the active one is active again.
    pub(crate) fn restore(self, s: &mut Session) {
        let kept: Vec<u64> = self.docs.iter().map(|d| d.uid).collect();
        for i in (0..s.documents().len()).rev() {
            if s.documents().get(i).is_some_and(|d| !kept.contains(&d.uid)) {
                s.close_document(i);
            }
        }
        for mut d in self.docs {
            match s.documents().iter().position(|x| x.uid == d.uid) {
                Some(i) => {
                    let live = s.documents().get(i).map_or(d.revision, |x| x.revision);
                    // The edit counter only goes forward (checkpoints compare it): a presentation
                    // the call changed comes back at a newer revision with its old content.
                    if live != d.revision {
                        d.revision = live.max(d.revision) + 1;
                    }
                    s.set_active(i);
                    if let Some(slot) = s.active_mut() {
                        *slot = d;
                    }
                }
                // Closed by the call: open again (handles name it by its uid, as before).
                None => {
                    s.add_document(d);
                }
            }
        }
        if let Some(i) = self.active.and_then(|uid| s.documents().iter().position(|d| d.uid == uid)) {
            s.set_active(i);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_restore_undoes_edits_closes_what_the_call_opened_and_reopens_what_it_closed() {
        let mut s = Session::with_new();
        s.execute("slide.new", &json!({})).unwrap();
        let first = s.active().unwrap().uid;
        s.execute("file.new", &json!({})).unwrap();
        let second = s.active().unwrap().uid;
        s.set_active(0);
        let before = Before::take(&s);
        let (slides, revision) = (s.active().unwrap().doc.slides.len(), s.active().unwrap().revision);
        // The call: an edit on the first, the second closed, a third opened.
        s.execute("slide.new", &json!({})).unwrap();
        s.close_document(1);
        s.execute("file.new", &json!({})).unwrap();
        assert_eq!(s.documents().len(), 2);
        before.restore(&mut s);
        let uids: Vec<u64> = s.documents().iter().map(|d| d.uid).collect();
        assert_eq!(uids.len(), 2);
        assert!(uids.contains(&first) && uids.contains(&second), "{uids:?}");
        let a = s.active().unwrap();
        assert_eq!(a.uid, first, "the presentation active before is active again");
        assert_eq!(a.doc.slides.len(), slides, "the edit is undone");
        assert!(a.revision > revision, "the edit counter only goes forward");
    }

    #[test]
    fn an_untouched_session_keeps_its_revisions() {
        let mut s = Session::with_new();
        let before = Before::take(&s);
        let revision = s.active().unwrap().revision;
        before.restore(&mut s);
        assert_eq!(s.active().unwrap().revision, revision);
        assert_eq!(s.documents().len(), 1);
    }
}
