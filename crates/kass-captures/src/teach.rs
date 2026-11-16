//! Teaching one transcript of a capture: the draft of what the user meant,
//! the optional note, and which correction a save makes or amends.
//!
//! Corrections stack: each round of edits is saved as its own correction,
//! made from the newest one before it and holding the whole text, so the
//! newest is the text as corrected so far (what learning uses). `reports`
//! is the capture's corrections, newest first.

use kass_api::types::{CaptureFeedbackResponse, FeedbackTarget};

/// A correction to send: the text, its note, the round it amends, and the
/// text as shown before this fix (for fixing it where it was typed too).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submission {
    pub expected_text: String,
    pub notes: String,
    pub replaces: Option<String>,
    pub before: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Teach {
    pub target: FeedbackTarget,
    /// Kass's own text.
    pub original: String,
    /// The text being edited; None while the transcript isn't.
    pub draft: Option<String>,
    pub notes: String,
    /// The round just saved, for the notice under the text.
    pub learned: Option<String>,
    pub saving: bool,
    /// The correction being undone or removed, while it is.
    pub removing: Option<String>,
}

impl Teach {
    pub fn new(target: FeedbackTarget, original: impl Into<String>) -> Self {
        Self {
            target,
            original: original.into(),
            draft: None,
            notes: String::new(),
            learned: None,
            saving: false,
            removing: None,
        }
    }

    /// This transcript's corrections, newest first.
    pub fn rounds<'a>(
        &self,
        reports: &'a [CaptureFeedbackResponse],
    ) -> Vec<&'a CaptureFeedbackResponse> {
        reports.iter().filter(|r| r.target == self.target).collect()
    }

    /// The newest correction: the text as corrected so far.
    pub fn saved<'a>(
        &self,
        reports: &'a [CaptureFeedbackResponse],
    ) -> Option<&'a CaptureFeedbackResponse> {
        reports.iter().find(|r| r.target == self.target)
    }

    /// The text an edit starts from: the newest correction, else Kass's.
    pub fn base(&self, reports: &[CaptureFeedbackResponse]) -> String {
        self.saved(reports)
            .map(|r| r.expected_text.clone())
            .unwrap_or_else(|| self.original.clone())
    }

    /// The text as it reads now: the edit, else the saved correction, else Kass's.
    pub fn current(&self, reports: &[CaptureFeedbackResponse]) -> String {
        self.draft.clone().unwrap_or_else(|| self.base(reports))
    }

    /// The edit differs from where it started and from Kass's own text.
    /// Exact, so adding or removing a line break counts as a fix; back to
    /// Kass's own text isn't one (Undo takes corrections back).
    pub fn changed(&self, reports: &[CaptureFeedbackResponse]) -> bool {
        self.draft
            .as_deref()
            .is_some_and(|d| d != self.base(reports) && d != self.original)
    }

    /// The text before `report`'s round: the round under it, else Kass's.
    pub fn before(&self, reports: &[CaptureFeedbackResponse], report_id: &str) -> String {
        let rounds = self.rounds(reports);
        rounds
            .iter()
            .position(|r| r.id == report_id)
            .and_then(|at| rounds.get(at + 1))
            .map(|r| r.expected_text.clone())
            .unwrap_or_else(|| self.original.clone())
    }

    /// The round just saved, while it's still the newest.
    pub fn learned_round<'a>(
        &self,
        reports: &'a [CaptureFeedbackResponse],
    ) -> Option<&'a CaptureFeedbackResponse> {
        let saved = self.saved(reports)?;
        (self.learned.as_deref() == Some(saved.id.as_str())).then_some(saved)
    }

    fn submission(
        &self,
        text: &str,
        notes: &str,
        replaces: Option<String>,
        reports: &[CaptureFeedbackResponse],
    ) -> Option<Submission> {
        let base = self.base(reports);
        if text == self.original || text == base || self.saving {
            return None;
        }
        Some(Submission {
            expected_text: text.to_string(),
            notes: notes.to_string(),
            replaces,
            before: base,
        })
    }

    /// Starts editing from the current text, so the user fixes it in place.
    pub fn begin(&mut self, reports: &[CaptureFeedbackResponse]) {
        if self.draft.is_none() {
            self.draft = Some(self.base(reports));
        }
    }

    pub fn cancel(&mut self) {
        self.draft = None;
        self.notes.clear();
    }

    /// Drops an untouched draft when focus leaves, back to the plain text.
    pub fn settle(&mut self, reports: &[CaptureFeedbackResponse]) {
        if !self.changed(reports) && self.notes.is_empty() {
            self.draft = None;
        }
    }

    /// The draft as a new round.
    pub fn save(&self, reports: &[CaptureFeedbackResponse]) -> Option<Submission> {
        let draft = self.draft.as_deref()?;
        self.submission(draft, &self.notes, None, reports)
    }

    /// Saves `text` as the fix, showing it in the edit box while it saves.
    /// While editing, it saves that round; otherwise, with a correction
    /// already saved, it amends the newest one, keeping its note.
    pub fn save_text(
        &mut self,
        text: &str,
        reports: &[CaptureFeedbackResponse],
    ) -> Option<Submission> {
        let submission = match (self.draft.is_none(), self.saved(reports)) {
            (true, Some(saved)) => {
                self.submission(text, &saved.notes, Some(saved.id.clone()), reports)
            }
            _ => self.submission(text, &self.notes, None, reports),
        }?;
        self.draft = Some(text.to_string());
        Some(submission)
    }

    /// A save went through: show the round, close the edit.
    pub fn saved_round(&mut self, report_id: String) {
        self.saving = false;
        self.learned = Some(report_id);
        self.draft = None;
        self.notes.clear();
    }
}

/// `reports` with a new round on top and the one it replaced gone.
pub fn add_round(
    reports: &mut Vec<CaptureFeedbackResponse>,
    report: CaptureFeedbackResponse,
    replaced: Option<&str>,
) {
    reports.retain(|r| Some(r.id.as_str()) != replaced && r.id != report.id);
    reports.insert(0, report);
}

#[cfg(test)]
mod tests {
    use super::*;
    use kass_api::types::{CaptureFeedbackSource, CaptureResponse, CaptureSource};

    fn capture() -> CaptureResponse {
        serde_json::from_value(serde_json::json!({
            "id": "c1",
            "audio_path": "",
            "source": "dictation",
            "transcript_raw": "raw",
            "created_at": "2026-11-16T10:00:00",
        }))
        .unwrap()
    }

    fn report(id: &str, target: FeedbackTarget, text: &str) -> CaptureFeedbackResponse {
        CaptureFeedbackResponse {
            id: id.into(),
            capture_id: "c1".into(),
            target,
            expected_text: text.into(),
            notes: format!("note {id}"),
            snapshot: capture(),
            source: CaptureFeedbackSource::Manual,
            replaces: None,
            created_at: "2026-11-16T10:00:00".into(),
        }
    }

    #[test]
    fn the_capture_parses() {
        assert_eq!(capture().source, CaptureSource::Dictation);
    }

    #[test]
    fn edits_start_from_the_newest_round() {
        let reports = vec![
            report("r2", FeedbackTarget::Refined, "second"),
            report("raw", FeedbackTarget::Raw, "raw fix"),
            report("r1", FeedbackTarget::Refined, "first"),
        ];
        let mut teach = Teach::new(FeedbackTarget::Refined, "kass");
        assert_eq!(teach.base(&reports), "second");
        assert_eq!(teach.before(&reports, "r2"), "first");
        assert_eq!(teach.before(&reports, "r1"), "kass");
        teach.begin(&reports);
        assert_eq!(teach.draft.as_deref(), Some("second"));
        assert!(!teach.changed(&reports));
        teach.settle(&reports);
        assert_eq!(teach.draft, None);
    }

    #[test]
    fn saving_kass_own_text_or_the_base_does_nothing() {
        let reports = vec![report("r1", FeedbackTarget::Refined, "first")];
        let mut teach = Teach::new(FeedbackTarget::Refined, "kass");
        teach.draft = Some("kass".into());
        assert!(!teach.changed(&reports));
        assert_eq!(teach.save(&reports), None);
        teach.draft = Some("first".into());
        assert_eq!(teach.save(&reports), None);
        teach.draft = Some("third".into());
        teach.notes = "why".into();
        assert_eq!(
            teach.save(&reports),
            Some(Submission {
                expected_text: "third".into(),
                notes: "why".into(),
                replaces: None,
                before: "first".into(),
            })
        );
        teach.saving = true;
        assert_eq!(teach.save(&reports), None);
    }

    #[test]
    fn save_text_amends_the_newest_round_when_not_editing() {
        let reports = vec![report("r1", FeedbackTarget::Refined, "first")];
        let mut teach = Teach::new(FeedbackTarget::Refined, "kass");
        let submission = teach.save_text("respelled", &reports).unwrap();
        assert_eq!(submission.replaces.as_deref(), Some("r1"));
        assert_eq!(submission.notes, "note r1");
        assert_eq!(teach.draft.as_deref(), Some("respelled"));

        let mut editing = Teach::new(FeedbackTarget::Refined, "kass");
        editing.draft = Some("typing".into());
        let submission = editing.save_text("respelled", &reports).unwrap();
        assert_eq!(submission.replaces, None);
    }

    #[test]
    fn a_saved_round_goes_on_top_and_replaces_the_one_it_amends() {
        let mut reports = vec![
            report("r2", FeedbackTarget::Refined, "second"),
            report("r1", FeedbackTarget::Refined, "first"),
        ];
        add_round(
            &mut reports,
            report("r3", FeedbackTarget::Refined, "third"),
            Some("r2"),
        );
        let ids: Vec<_> = reports.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["r3", "r1"]);
        let mut teach = Teach::new(FeedbackTarget::Refined, "kass");
        teach.saved_round("r3".into());
        assert!(teach.learned_round(&reports).is_some());
        add_round(
            &mut reports,
            report("r4", FeedbackTarget::Refined, "x"),
            None,
        );
        assert!(teach.learned_round(&reports).is_none());
    }
}
