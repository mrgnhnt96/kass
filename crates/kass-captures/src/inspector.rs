//! The details beside an open capture: the recording, the models that made
//! it, its corrections (each can be taken back) and how learning is going.

use crate::detail::learning_phase;
use crate::format::{format_duration, format_time, format_when, language_name, words_per_minute};
use crate::page::{CapturesPage, LearningAction, Load};
use crate::widgets::{disclosure, hunk_list};
use crate::word_diff::{count_words, diff_words};
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{
    CaptureFeedbackResponse, CaptureFeedbackSource, CaptureResponse, CorrectionLearningStatus,
    FeedbackTarget, LearningOutcome, WritingStyleHabit,
};
use kass_i18n::tr;
use kass_ui::{Button, KassTheme, metrics, theme};
use kass_writing_style::habit_key;

fn section(title: String, t: &KassTheme) -> Div {
    div().flex().flex_col().gap(px(10.)).child(
        div()
            .font_family(t.mono_font.clone())
            .text_size(metrics::TEXT_CAPTION)
            .text_color(t.muted_foreground)
            .child(title.to_uppercase()),
    )
}

fn row(label: String, value: String, t: &KassTheme) -> Div {
    div()
        .flex()
        .justify_between()
        .gap(px(8.))
        .text_size(metrics::TEXT_BASE)
        .child(
            div()
                .flex_none()
                .text_color(t.muted_foreground)
                .child(label),
        )
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_right()
                .text_color(t.foreground)
                .child(value),
        )
}

/// Learned punctuation habits, one plain-language line each; `None` when
/// there are none to list.
fn habit_list(habits: &[WritingStyleHabit]) -> Option<Div> {
    let lines: Vec<String> = habits
        .iter()
        .filter_map(|habit| habit_key(*habit))
        .map(|key| tr!(&format!("writing-style-habits-{key}")))
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .children(lines.into_iter().map(|line| {
                div()
                    .flex()
                    .gap(px(6.))
                    .child(div().flex_none().child("•"))
                    .child(div().min_w_0().child(line))
            })),
    )
}

fn aside(t: &KassTheme) -> Stateful<Div> {
    div()
        .id("capture-inspector")
        .w(px(250.))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .gap(px(24.))
        .px(px(20.))
        .py(px(24.))
        .border_l_1()
        .border_color(t.border)
        .bg(t.card)
        .overflow_y_scroll()
}

impl CapturesPage {
    pub(crate) fn render_inspector(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = theme(cx).clone();
        let detail = self.detail.as_ref().expect("a capture is open");
        let capture = &detail.capture;
        let raw_words = count_words(&capture.transcript_raw);
        let refined_words = capture
            .transcript_refined
            .as_deref()
            .filter(|r| !r.is_empty())
            .map(count_words);

        let mut recording = section(tr!("captures-inspector-recording"), &t).child(row(
            tr!("captures-inspector-length"),
            format_duration(capture.duration_ms),
            &t,
        ));
        if let Some(pace) = words_per_minute(raw_words, capture.duration_ms) {
            recording = recording.child(row(
                tr!("captures-inspector-pace"),
                tr!("captures-inspector-wpm", count = pace),
                &t,
            ));
        }
        recording = recording.child(row(
            tr!("captures-inspector-words"),
            match refined_words {
                Some(refined) => format!("{raw_words} → {refined}"),
                None => raw_words.to_string(),
            },
            &t,
        ));
        if let Some(language) = &capture.language {
            recording = recording.child(row(
                tr!("captures-inspector-language"),
                language_name(language),
                &t,
            ));
        }
        if let Some(mac) = self.origin(capture) {
            recording = recording.child(row(tr!("captures-inspector-mac"), mac, &t));
        }

        let mut inspector = aside(&t).child(recording);
        if capture.stt_model.is_some() || capture.llm_model.is_some() {
            let mut models = section(tr!("captures-inspector-models"), &t);
            if let Some(model) = &capture.stt_model {
                models = models.child(row(
                    tr!("captures-inspector-speech"),
                    tr!("captures-inspector-whisper", model = model.clone()),
                    &t,
                ));
            }
            if let Some(model) = &capture.llm_model {
                models = models.child(row(
                    tr!("captures-inspector-refinement"),
                    tr!("captures-inspector-qwen", model = model.clone()),
                    &t,
                ));
            }
            inspector = inspector.child(models);
        }
        inspector.child(self.render_corrections(cx))
    }

    /// The Mac a capture came from, when another Mac shares this server.
    fn origin(&self, capture: &CaptureResponse) -> Option<String> {
        let this = self.client.mac()?.id;
        if !self.peers.iter().any(|peer| peer.id != this) {
            return None;
        }
        let mac = capture.mac_id.as_ref()?;
        Some(if *mac == this {
            tr!("captures-inspector-this-mac")
        } else {
            capture
                .mac_name
                .clone()
                .unwrap_or_else(|| tr!("captures-inspector-other-mac"))
        })
    }

    fn render_corrections(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = theme(cx).clone();
        let detail = self.detail.as_ref().expect("a capture is open");
        let reports = &detail.reports;
        let teach = &detail.teach;
        let title = if reports.is_empty() {
            tr!("captures-inspector-corrections")
        } else {
            tr!(
                "captures-inspector-corrections-count",
                count = reports.len()
            )
        };
        let mut corrections = section(title, &t);
        if detail.reports_load == Load::Failed {
            corrections = corrections.child(
                div()
                    .text_size(metrics::TEXT_BASE)
                    .text_color(t.destructive)
                    .child(tr!("captures-feedback-load-failed")),
            );
        } else if detail.reports_load == Load::Ready && reports.is_empty() {
            corrections = corrections.child(
                div()
                    .text_size(metrics::TEXT_BASE)
                    .line_height(px(19.5))
                    .text_color(t.muted_foreground)
                    .child(tr!("captures-inspector-no-corrections")),
            );
        }
        let saved = teach.saved(reports).map(|r| r.id.clone());
        let busy = teach.removing.is_some();
        let mut list = div().flex().flex_col();
        for (i, report) in reports.iter().enumerate() {
            let showing = saved.as_deref() == Some(report.id.as_str());
            let hunks = if report.target == teach.target {
                diff_words(&teach.before(reports, &report.id), &report.expected_text).hunks
            } else {
                Vec::new()
            };
            let target = tr!(match report.target {
                FeedbackTarget::Raw => "captures-transcript-raw",
                _ => "captures-transcript-refined",
            });
            let id = report.id.clone();
            let mut head = div().flex().items_center().gap(px(6.)).child(
                div()
                    .font_family(t.mono_font.clone())
                    .text_size(metrics::TEXT_CAPTION)
                    .text_color(t.muted_foreground)
                    .child(format!("{target} · {}", format_time(&report.created_at))),
            );
            if showing {
                head = head.child(
                    div()
                        .flex()
                        .items_center()
                        .h(px(16.))
                        .px(px(6.))
                        .rounded_full()
                        .bg(t.success.opacity(0.15))
                        .font_family(t.mono_font.clone())
                        .text_size(metrics::TEXT_2XS)
                        .text_color(t.success)
                        .child(tr!("captures-inspector-showing")),
                );
            }
            head = head.child(div().flex_1()).child(
                div()
                    .id(("report-withdraw", i))
                    .px(px(4.))
                    .text_size(metrics::TEXT_XS)
                    .text_color(t.muted_foreground)
                    .when(busy, |b| b.opacity(0.5))
                    .when(!busy, |b| {
                        b.cursor_pointer()
                            .hover(|s| s.underline().text_color(t.foreground))
                    })
                    .child(tr!(if showing {
                        "captures-teach-undo"
                    } else {
                        "captures-teach-remove"
                    }))
                    .on_click(cx.listener(move |page, _, window, cx| {
                        page.withdraw(id.clone(), window, cx)
                    })),
            );
            let mut item = div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .py(px(10.))
                .when(i == 0, |d| d.pt_0())
                .border_b_1()
                .border_color(t.border)
                .child(head);
            item = if hunks.is_empty() {
                item.child(div().text_size(metrics::TEXT_BASE).child(
                    if report.expected_text.is_empty() {
                        "∅".to_string()
                    } else {
                        report.expected_text.clone()
                    },
                ))
            } else {
                item.child(hunk_list(&hunks, cx, |_, _| None))
            };
            if report.source == CaptureFeedbackSource::VoiceFix {
                item = item.child(
                    div()
                        .font_family(t.mono_font.clone())
                        .text_size(metrics::TEXT_2XS)
                        .text_color(t.muted_foreground)
                        .child(tr!("captures-inspector-by-voice-edit").to_uppercase()),
                );
            }
            if !report.notes.is_empty() {
                item = item.child(
                    div()
                        .text_size(metrics::TEXT_XS)
                        .text_color(t.muted_foreground)
                        .child(report.notes.clone()),
                );
            }
            list = list.child(item);
        }
        corrections = corrections.child(list);
        if let Some(learning) = self.render_learning(reports, cx) {
            corrections = corrections.child(learning);
        }
        corrections
    }

    /// How correction learning is going, once one of these corrections was
    /// used, with Run, Roll back and Cancel.
    fn render_learning(
        &self,
        reports: &[CaptureFeedbackResponse],
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let status: &CorrectionLearningStatus = self.learning.as_ref()?;
        if !reports
            .iter()
            .any(|r| status.evaluated_report_ids.contains(&r.id))
        {
            return None;
        }
        let t = theme(cx).clone();
        let open = self.learning_open;
        let mut panel = div()
            .flex()
            .flex_col()
            .rounded(metrics::RADIUS_MD)
            .border_1()
            .border_color(t.border)
            .bg(t.card)
            .p(px(12.))
            .text_size(metrics::TEXT_BASE)
            .child(
                disclosure(
                    "learning",
                    tr!("captures-feedback-learning-title"),
                    open,
                    cx,
                )
                .on_click(cx.listener(|page, _, _, cx| {
                    page.learning_open = !page.learning_open;
                    cx.notify();
                })),
            );
        if !open {
            return Some(panel.into_any_element());
        }
        let muted = |text: String| div().text_color(t.muted_foreground).child(text);
        panel = panel.child(
            muted(tr!("captures-feedback-learning-description"))
                .mt(px(8.))
                .text_size(metrics::TEXT_XS)
                .line_height(px(19.5)),
        );
        if let Some(habits) = habit_list(&self.habits) {
            panel = panel.child(
                div()
                    .mt(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(tr!("writing-style-learning-panel-title")),
                    )
                    .child(habits),
            );
        }
        let outcome = tr!(match status.outcome {
            LearningOutcome::Waiting => "captures-feedback-learning-waiting",
            LearningOutcome::Updated => "captures-feedback-learning-updated",
            LearningOutcome::NoChange => "captures-feedback-learning-no-change",
            LearningOutcome::RolledBack => "captures-feedback-learning-rolled-back",
            _ => "captures-feedback-learning-waiting",
        });
        let mut rules = div()
            .mt(px(8.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(div().child(tr!(
                "captures-feedback-learning-active",
                count = status.active_rules
            )))
            .child(div().child(outcome));
        if let Some(last) = &status.last_run {
            rules = rules.child(muted(tr!(
                "captures-feedback-learning-last-run",
                date = format_when(last)
            )));
        }
        if status.metrics.as_ref().is_some_and(|m| !m.latency_passed) {
            rules = rules.child(div().child(tr!("captures-feedback-learning-too-slow")));
        }
        panel = panel.child(rules);
        if self.learning_failed {
            panel = panel.child(
                div()
                    .mt(px(8.))
                    .text_color(t.destructive)
                    .child(tr!("captures-feedback-learning-failed")),
            );
        }
        let model = status.model.as_ref();
        if let Some(model) = model {
            let count = |key: &str| model.counts.get(key).copied().unwrap_or(0);
            let mut section = div()
                .mt(px(12.))
                .pt(px(12.))
                .border_t_1()
                .border_color(t.border)
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .child(tr!("captures-feedback-learning-model-title")),
                )
                .child(div().child(learning_phase(&model.phase)))
                .child(muted(tr!(
                    "captures-feedback-learning-dataset",
                    train = count("train"),
                    validation = count("validation"),
                    test = count("audio_test")
                )))
                .child(div().child(tr!(if model.active_adapter.is_some() {
                    "captures-feedback-learning-personal-model"
                } else {
                    "captures-feedback-learning-base-model"
                })));
            if let Some(speech) = &model.speech_model {
                section = section.child(div().child(tr!(
                    "captures-feedback-learning-speech-model",
                    model = speech.clone()
                )));
            }
            if let Some(adapter) = model.metrics.as_ref().and_then(|m| m.adapter.as_ref()) {
                section = section.child(div().child(tr!(
                    "captures-feedback-learning-evaluation",
                    before = adapter.baseline_errors,
                    after = adapter.candidate_errors
                )));
            }
            if let Some(error) = &model.error {
                section = section.child(div().text_color(t.destructive).child(error.clone()));
            }
            panel = panel.child(section);
        }

        let busy = self.learning_busy;
        let running = model.is_some_and(|m| m.running);
        let model_rollback = model.is_some_and(|m| m.can_rollback);
        let mut buttons = div()
            .mt(px(12.))
            .flex()
            .flex_wrap()
            .gap(px(8.))
            .child(
                Button::new("learning-run")
                    .small()
                    .outline()
                    .disabled(busy || running)
                    .label(tr!(if busy {
                        "captures-feedback-learning-running"
                    } else {
                        "captures-feedback-learning-run"
                    }))
                    .on_click(cx.listener(|page, _, window, cx| {
                        page.learning_action(LearningAction::Run, window, cx)
                    })),
            )
            .child(
                Button::new("learning-rollback")
                    .small()
                    .ghost()
                    .disabled(busy || !(status.can_rollback || model_rollback))
                    .label(tr!(if model_rollback {
                        "captures-feedback-learning-rollback-model"
                    } else {
                        "captures-feedback-learning-rollback"
                    }))
                    .on_click(cx.listener(|page, _, window, cx| {
                        page.learning_action(LearningAction::Rollback, window, cx)
                    })),
            );
        if running {
            buttons = buttons.child(
                Button::new("learning-cancel")
                    .small()
                    .ghost()
                    .disabled(busy)
                    .label(tr!("captures-feedback-learning-cancel-training"))
                    .on_click(cx.listener(|page, _, window, cx| {
                        page.learning_action(LearningAction::Cancel, window, cx)
                    })),
            );
        }
        Some(panel.child(buttons).into_any_element())
    }

    /// A command's details: how long, and the models.
    pub(crate) fn render_command_inspector(
        &self,
        capture: &CaptureResponse,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let t = theme(cx).clone();
        let mut rows = div().flex().flex_col().gap(px(10.));
        if capture.duration_ms.is_some() {
            rows = rows.child(row(
                tr!("captures-inspector-length"),
                format_duration(capture.duration_ms),
                &t,
            ));
        }
        if let Some(model) = &capture.stt_model {
            rows = rows.child(row(
                tr!("captures-inspector-speech"),
                tr!("captures-inspector-whisper", model = model.clone()),
                &t,
            ));
        }
        if let Some(model) = &capture.llm_model {
            rows = rows.child(row(
                tr!("captures-command-rewriting"),
                tr!("captures-inspector-qwen", model = model.clone()),
                &t,
            ));
        }
        aside(&t).child(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::habit_list;
    use kass_api::types::WritingStyleHabit;

    #[test]
    fn lists_only_habits_it_can_word() {
        assert!(habit_list(&[]).is_none());
        assert!(habit_list(&[WritingStyleHabit::Unknown]).is_none());
        assert!(habit_list(&[WritingStyleHabit::DropFinalPeriod]).is_some());
    }
}
