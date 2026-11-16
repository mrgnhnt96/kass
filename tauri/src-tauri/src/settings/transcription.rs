//! Settings › Transcription & refinement: Kass's shared models, Whisper and
//! its language, voice training, and Qwen3 cleanup. Models themselves are
//! chosen in the Models tab.

use std::time::Duration;

use gpui_kit::component::select::{SelectEvent, SelectState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{
    CaptureSettings, CaptureSettingsUpdate, CorrectionLearningStatus, Qwen3ModelSize, SoundsState,
    VoiceTrainingStatus, WhisperModelSize,
};
use kass_ui::{
    metrics, select, select_state, theme, Button, SelectOption, SelectOptions, SettingRow,
    SettingSection, Toggle,
};

use super::{note, toast_error};
use crate::shell::{self, data::ShellData};

const P: &str = "settings-captures-transcription";
const R: &str = "settings-captures-refinement";
const S: &str = "settings-captures-shared-adapters";
const V: &str = "settings-captures-voice-training";

const LANGUAGES: [&str; 8] = ["auto", "en", "es", "fr", "de", "ja", "zh", "hi"];

/// How often voice training is asked again: often while something moves.
const BUSY_POLL: Duration = Duration::from_secs(2);
const IDLE_POLL: Duration = Duration::from_secs(60);

fn t(id: impl AsRef<str>) -> String {
    kass_i18n::t(id.as_ref())
}

/// "Whisper Turbo · Pruned Large v3 · Near-best, fast".
fn whisper_label(size: WhisperModelSize) -> String {
    let tail = match size {
        WhisperModelSize::Small => "balanced",
        WhisperModelSize::Medium => "higher",
        WhisperModelSize::Large => "best",
        WhisperModelSize::Turbo => "near-best",
        _ => "fast",
    };
    let key = match size {
        WhisperModelSize::Unknown => "turbo",
        size => size.as_str(),
    };
    kass_i18n::tr!(&format!("{P}-model-{key}"), tail = t(format!("{P}-model-tail-{tail}")))
}

/// The Qwen3 label and the Models tab's id for it.
fn qwen(size: Qwen3ModelSize) -> (String, &'static str) {
    let (key, tail, id) = match size {
        Qwen3ModelSize::Size1_7B => ("size17", "fast", "qwen3-1.7b"),
        Qwen3ModelSize::Size4B => ("size40", "full-quality", "qwen3-4b"),
        _ => ("size06", "very-fast", "qwen3-0.6b"),
    };
    let label = kass_i18n::tr!(&format!("{R}-model-{key}"), tail = t(format!("{R}-model-tail-{tail}")));
    (label, id)
}

/// Takes are split 85/15 into training and test takes, by id: how many it
/// takes to have enough of both.
pub fn takes_needed(voice: &VoiceTrainingStatus) -> u64 {
    let train = voice.min_train as f64 / 0.85;
    let test = voice.min_test as f64 / 0.15;
    train.max(test).ceil() as u64
}

fn collecting(voice: &VoiceTrainingStatus) -> bool {
    voice.bank.train < voice.min_train || voice.bank.test < voice.min_test
}

/// "Jan 5, 2026" for the voice model's start, from an ISO timestamp.
fn local_date(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%-m/%-d/%Y")
                .to_string()
        })
        .unwrap_or_default()
}

pub struct TranscriptionPage {
    data: Entity<ShellData>,
    language: Entity<SelectState<SelectOptions<String>>>,
    learning: Option<CorrectionLearningStatus>,
    downloading: bool,
    training: bool,
    undoing: bool,
    _poll: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl TranscriptionPage {
    pub fn new(data: Entity<ShellData>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let current = data
            .read(cx)
            .settings
            .as_ref()
            .map_or_else(|| "auto".to_string(), |s| s.language.clone());
        let options = LANGUAGES
            .iter()
            .map(|code| SelectOption::new(code.to_string(), t(format!("{P}-language-{code}"))))
            .collect();
        let language = cx.new(|cx| select_state(options, Some(&current), window, cx));
        let subscriptions = vec![
            cx.subscribe(&language, |this, _, event: &SelectEvent<_>, cx| {
                if let SelectEvent::Confirm(Some(code)) = event {
                    this.save(
                        CaptureSettingsUpdate {
                            language: Some(code.clone()),
                            ..Default::default()
                        },
                        cx,
                    );
                }
            }),
            cx.observe_in(&data, window, |this, data, window, cx| {
                if let Some(code) = data.read(cx).settings.as_ref().map(|s| s.language.clone()) {
                    this.language.update(cx, |state, cx| {
                        if state.selected_value() != Some(&code) {
                            state.set_selected_value(&code, window, cx);
                        }
                    });
                }
                cx.notify();
            }),
        ];
        Self {
            _poll: Self::poll(cx),
            data,
            language,
            learning: None,
            downloading: false,
            training: false,
            undoing: false,
            _subscriptions: subscriptions,
        }
    }

    fn poll(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| loop {
            let Ok(api) = this.read_with(cx, |this, cx| this.data.read(cx).api.clone()) else {
                return;
            };
            let status = cx
                .background_executor()
                .spawn(async move { api.correction_learning_status().ok() })
                .await;
            let Ok(busy) = this.update(cx, |this, cx| {
                if status.is_some() {
                    this.learning = status;
                }
                cx.notify();
                this.busy()
            }) else {
                return;
            };
            cx.background_executor()
                .timer(if busy { BUSY_POLL } else { IDLE_POLL })
                .await;
        })
    }

    fn busy(&self) -> bool {
        let model = self.learning.as_ref().and_then(|l| l.model.as_ref());
        model.is_some_and(|m| m.running)
            || model
                .and_then(|m| m.voice.as_ref())
                .is_some_and(|v| v.sounds.state == SoundsState::Downloading)
    }

    /// Like the page, a failed save just leaves what the server kept.
    fn save(&mut self, patch: CaptureSettingsUpdate, cx: &mut Context<Self>) {
        let task = self
            .data
            .update(cx, |data, cx| data.update_settings(patch, cx));
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = task.await {
                    eprintln!("[settings] save failed: {e}");
                }
            })
            .detach();
    }

    fn toggle(
        &self,
        id: &'static str,
        checked: bool,
        disabled: bool,
        patch: fn(bool) -> CaptureSettingsUpdate,
        cx: &mut Context<Self>,
    ) -> Toggle {
        let this = cx.entity().downgrade();
        Toggle::new(id)
            .checked(checked)
            .disabled(disabled)
            .on_change(move |value, _, cx| {
                let value = *value;
                let _ = this.update(cx, |this, cx| this.save(patch(value), cx));
            })
    }

    /// Ask the server for something about voice training and show its answer.
    fn learning_action(
        &mut self,
        flag: fn(&mut Self) -> &mut bool,
        call: fn(&kass_api::ApiClient) -> kass_api::Result<Option<CorrectionLearningStatus>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        *flag(self) = true;
        cx.notify();
        let api = self.data.read(cx).api.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { call(&api).and_then(|status| match status {
                    Some(status) => Ok(Some(status)),
                    None => api.correction_learning_status().map(Some),
                }) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                *flag(this) = false;
                match result {
                    Ok(status) => this.learning = status.or(this.learning.take()),
                    Err(e) => toast_error(t(format!("{V}-title")), &e.to_string(), window, cx),
                }
                cx.notify();
            });
            // Training and downloads move on their own: follow them closely.
            this.update(cx, |this, cx| this._poll = Self::poll(cx)).ok();
        })
        .detach();
    }

    fn voice_section(
        &self,
        voice: &VoiceTrainingStatus,
        stt: WhisperModelSize,
        cx: &mut Context<Self>,
    ) -> SettingSection {
        let theme = theme(cx);
        let model = self.learning.as_ref().and_then(|l| l.model.as_ref());
        let running = model.is_some_and(|m| m.running);
        let sounds = &voice.sounds;

        let sounds_action = match sounds.state {
            SoundsState::Ready => note(t(format!("{V}-sounds-ready")), cx).into_any_element(),
            SoundsState::Downloading => note(
                kass_i18n::tr!(
                    &format!("{V}-sounds-downloading"),
                    percent = (sounds.fraction * 100.).round()
                ),
                cx,
            )
            .into_any_element(),
            state => Button::new("voice-sounds")
                .outline()
                .small()
                .label(t(if state == SoundsState::Failed {
                    format!("{V}-sounds-retry")
                } else {
                    format!("{V}-sounds-download")
                }))
                .disabled(self.downloading)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.learning_action(
                        |this| &mut this.downloading,
                        |api| api.download_voice_sounds().map(|()| None),
                        window,
                        cx,
                    )
                }))
                .into_any_element(),
        };

        let bank = &voice.bank;
        let takes = if collecting(voice) {
            kass_i18n::tr!(
                &format!("{V}-takes-collecting"),
                count = bank.takes,
                needed = takes_needed(voice).max(bank.takes + 1)
            )
        } else {
            kass_i18n::tr!(
                &format!("{V}-takes-kept"),
                count = bank.takes,
                minutes = bank.minutes
            )
        };

        let mut lines = vec![if voice.active.is_some() {
            kass_i18n::tr!(
                &format!("{V}-model-active"),
                date = voice.active_since.as_deref().map(local_date).unwrap_or_default()
            )
        } else {
            t(format!("{V}-model-plain"))
        }];
        if let Some(metrics) = &voice.metrics {
            if let Some(candidate) = &metrics.candidate {
                let key = if metrics.passed { "last-passed" } else { "last-kept" };
                let mut line = kass_i18n::tr!(
                    &format!("{V}-model-{key}"),
                    before = metrics.production.as_ref().map_or(0., |p| p.noisy),
                    after = candidate.noisy
                );
                if let (false, Some(reason)) = (metrics.passed, metrics.reasons.first()) {
                    line.push_str(&format!(" {reason}."));
                }
                lines.push(line);
            }
        }
        let error = model.and_then(|m| m.error.clone());
        let destructive = theme.destructive;
        let description = div()
            .flex()
            .flex_col()
            .children(lines)
            .when_some(error, |d, error| d.child(div().text_color(destructive).child(error)));

        SettingSection::new(t(format!("{V}-title")))
            .description(t(format!("{V}-description")))
            .when(stt != WhisperModelSize::Turbo, |s| {
                s.child(
                    div()
                        .py(px(12.))
                        .text_size(metrics::TEXT_XS)
                        .text_color(theme.muted_foreground)
                        .child(t(format!("{V}-needs-turbo"))),
                )
            })
            .child(
                SettingRow::new(t(format!("{V}-sounds-title")))
                    .description(
                        sounds
                            .error
                            .clone()
                            .unwrap_or_else(|| t(format!("{V}-sounds-description"))),
                    )
                    .action(sounds_action),
            )
            .child(
                SettingRow::new(t(format!("{V}-takes-title")))
                    .description(t(format!("{V}-takes-description")))
                    .action(note(takes, cx)),
            )
            .child(
                SettingRow::new(t(format!("{V}-model-title")))
                    .description(description)
                    .action(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                Button::new("voice-train")
                                    .outline()
                                    .small()
                                    .label(t(if running {
                                        format!("{V}-model-training")
                                    } else {
                                        format!("{V}-model-train")
                                    }))
                                    .disabled(self.training || running)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.learning_action(
                                            |this| &mut this.training,
                                            |api| api.run_correction_learning().map(Some),
                                            window,
                                            cx,
                                        )
                                    })),
                            )
                            .when(voice.can_undo, |d| {
                                d.child(
                                    Button::new("voice-undo")
                                        .ghost()
                                        .small()
                                        .label(t(format!("{V}-model-undo")))
                                        .disabled(self.undoing)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.learning_action(
                                                |this| &mut this.undoing,
                                                |api| api.rollback_correction_learning().map(Some),
                                                window,
                                                cx,
                                            )
                                        })),
                                )
                            }),
                    ),
            )
    }
}

/// The model in use, and a way to change it in the Models tab.
fn model_in_use(id: &'static str, label: String, model: String, cx: &App) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .child(note(label, cx))
        .child(
            Button::new(id)
                .outline()
                .small()
                .label(t("settings-captures-change-model"))
                .on_click(move |_, _, cx| {
                    shell::navigate("/models", &format!("?model={model}"), cx)
                }),
        )
}

impl Render for TranscriptionPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings: Option<CaptureSettings> = self.data.read(cx).settings.clone();
        let loaded = settings.is_some();
        let stt = settings
            .as_ref()
            .map_or(WhisperModelSize::Turbo, |s| s.stt_model);
        let llm = settings
            .as_ref()
            .map_or(Qwen3ModelSize::Size0_6B, |s| s.llm_model);
        let auto_refine = settings.as_ref().is_none_or(|s| s.auto_refine);
        let self_correction = settings.as_ref().is_none_or(|s| s.self_correction);
        let expressive = settings.as_ref().is_none_or(|s| s.expressive);
        let shared = settings.as_ref().is_none_or(|s| s.shared_adapters);
        let stt_id = match stt {
            WhisperModelSize::Unknown => "turbo",
            size => size.as_str(),
        };
        let (qwen_label, qwen_id) = qwen(llm);

        let shared_section = SettingSection::new(t(format!("{S}-title"))).child(
            SettingRow::new(t(format!("{S}-toggle-title")))
                .description(t(format!("{S}-toggle-description")))
                .action(self.toggle("shared-adapters", shared, !loaded, |v| {
                    CaptureSettingsUpdate {
                        shared_adapters: Some(v),
                        ..Default::default()
                    }
                }, cx)),
        );

        let transcription = SettingSection::new(t(format!("{P}-title")))
            .child(
                SettingRow::new(t(format!("{P}-model-title")))
                    .description(t(format!("{P}-model-description")))
                    .action(model_in_use(
                        "change-whisper",
                        whisper_label(stt),
                        format!("whisper-{stt_id}"),
                        cx,
                    )),
            )
            .child(
                SettingRow::new(t(format!("{P}-language-title")))
                    .description(t(format!("{P}-language-description")))
                    .action(div().w(px(200.)).child(select(&self.language, cx))),
            );

        let voice = self
            .learning
            .as_ref()
            .and_then(|l| l.model.as_ref())
            .and_then(|m| m.voice.clone())
            .map(|voice| self.voice_section(&voice, stt, cx));

        let refinement = SettingSection::new(t(format!("{R}-title")))
            .child(
                SettingRow::new(t(format!("{R}-auto-title")))
                    .description(t(format!("{R}-auto-description")))
                    .action(self.toggle("auto-refine", auto_refine, false, |v| {
                        CaptureSettingsUpdate {
                            auto_refine: Some(v),
                            ..Default::default()
                        }
                    }, cx)),
            )
            .child(
                SettingRow::new(t(format!("{R}-model-title")))
                    .description(t(format!("{R}-model-description")))
                    .action(model_in_use(
                        "change-qwen",
                        qwen_label,
                        qwen_id.to_string(),
                        cx,
                    )),
            )
            .child(
                SettingRow::new(t(format!("{R}-self-correction-title")))
                    .description(t(format!("{R}-self-correction-description")))
                    .action(self.toggle("self-correction", self_correction, !auto_refine, |v| {
                        CaptureSettingsUpdate {
                            self_correction: Some(v),
                            ..Default::default()
                        }
                    }, cx)),
            )
            .child(
                SettingRow::new(t(format!("{R}-expressive-title")))
                    .description(t(format!("{R}-expressive-description")))
                    .action(self.toggle("expressive", expressive, !auto_refine, |v| {
                        CaptureSettingsUpdate {
                            expressive: Some(v),
                            ..Default::default()
                        }
                    }, cx)),
            )
            .child(
                SettingRow::new(t(format!("{R}-styles-title")))
                    .description(t(format!("{R}-styles-description")))
                    .action(
                        Button::new("open-styles")
                            .outline()
                            .small()
                            .label(t(format!("{R}-styles-open")))
                            .on_click(|_, _, cx| shell::navigate("/settings/writing-style", "", cx)),
                    ),
            );

        div()
            .flex()
            .flex_col()
            .child(shared_section)
            .child(transcription)
            .children(voice)
            .child(refinement)
    }
}

#[cfg(test)]
mod tests {
    use super::{collecting, qwen, takes_needed, whisper_label};
    use kass_api::types::{Qwen3ModelSize, VoiceTrainingStatus, WhisperModelSize};
    use kass_api::types::{VoiceBank, VoiceSounds};

    fn voice(min_train: u64, min_test: u64, train: u64, test: u64) -> VoiceTrainingStatus {
        VoiceTrainingStatus {
            active: None,
            active_since: None,
            can_undo: false,
            min_train,
            min_test,
            bank: VoiceBank {
                takes: train + test,
                train,
                test,
                minutes: 0.,
                room_minutes: 0.,
            },
            sounds: VoiceSounds::default(),
            metrics: None,
            training: None,
        }
    }

    #[test]
    fn needs_enough_takes_for_both_splits() {
        // 40 / 0.85 = 47.06, 10 / 0.15 = 66.67.
        assert_eq!(takes_needed(&voice(40, 10, 0, 0)), 67);
        assert!(collecting(&voice(40, 10, 40, 9)));
        assert!(!collecting(&voice(40, 10, 40, 10)));
    }

    #[test]
    fn labels_the_models_in_use() {
        assert_eq!(
            whisper_label(WhisperModelSize::Turbo),
            "Whisper Turbo · Pruned Large v3 · Near-best, fast"
        );
        assert_eq!(
            qwen(Qwen3ModelSize::Size1_7B),
            ("Qwen3 · 1.7B · 1.1 GB · Fast".to_string(), "qwen3-1.7b")
        );
    }
}
