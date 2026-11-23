//! The "When I say" box of a phrase: typed, or said from its mic or while
//! the dictation shortcut is held. The Dictionary page's add form and the
//! Captures page's phrase dialog both use it.

use crate::dictation::{PhraseDictation, PhraseTake};
use crate::scopes::{MAX_LENGTH, spoken_phrase};
use crate::widgets::with_keys;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_i18n::tr;
use kass_ui::{Button, ButtonSize, metrics, text_input, theme};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

/// The push-to-talk chord when settings haven't loaded.
pub fn default_chord() -> Vec<String> {
    vec!["MetaRight".into(), "AltGr".into()]
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Take {
    #[default]
    Idle,
    Listening {
        from_button: bool,
    },
    Cleaning,
}

/// Which hint sits under the box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SayHint {
    /// Adding a phrase from nothing: type it, click the mic, or hold the keys.
    Add,
    /// The box already has the phrase: say it another way.
    Another,
}

/// A heard phrase just filled the box.
pub struct SayFilled;

pub struct SayBox {
    input: Entity<InputState>,
    dictation: Option<Rc<dyn PhraseDictation>>,
    following: Option<Subscription>,
    /// Whether a shortcut take should fill the box instead of being typed.
    aims_here: Rc<Cell<bool>>,
    chord: Vec<String>,
    take: Take,
    heard: Option<String>,
    filled: bool,
}

impl EventEmitter<SayFilled> for SayBox {}

impl SayBox {
    pub fn new(
        placeholder: String,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(value)
                .validate(|text, _| text.chars().count() <= MAX_LENGTH)
        });
        Self {
            input,
            dictation: None,
            following: None,
            aims_here: Rc::new(Cell::new(false)),
            chord: default_chord(),
            take: Take::Idle,
            heard: None,
            filled: false,
        }
    }

    /// Lets the box be dictated: a mic beside it, and shortcut takes while
    /// [`set_aims`](Self::set_aims) says they're meant for it.
    pub fn with_dictation(
        mut self,
        dictation: Option<Rc<dyn PhraseDictation>>,
        cx: &mut Context<Self>,
    ) -> Self {
        self.set_dictation(dictation, cx);
        self
    }

    pub fn set_dictation(
        &mut self,
        dictation: Option<Rc<dyn PhraseDictation>>,
        cx: &mut Context<Self>,
    ) {
        self.following = dictation.as_ref().map(|dictation| {
            let aims = self.aims_here.clone();
            let this = cx.weak_entity();
            dictation.follow(
                Rc::new(move |_| aims.get()),
                Rc::new(move |take, cx| {
                    this.update(cx, |this, cx| this.on_take(take, cx)).ok();
                }),
                cx,
            )
        });
        self.dictation = dictation;
        cx.notify();
    }

    pub fn input(&self) -> &Entity<InputState> {
        &self.input
    }

    /// What's in the box, trimmed.
    pub fn value(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_string()
    }

    pub fn set_value(&mut self, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value(value.to_string(), window, cx)
        });
    }

    /// Whether a shortcut take that begins now fills the box. The owner
    /// says so while no other field of its own has focus.
    pub fn set_aims(&self, aims: bool) {
        self.aims_here.set(aims);
    }

    pub fn set_chord(&mut self, chord: Vec<String>, cx: &mut Context<Self>) {
        if !chord.is_empty() && chord != self.chord {
            self.chord = chord;
            cx.notify();
        }
    }

    /// No take is under way for the box.
    pub fn idle(&self) -> bool {
        self.take == Take::Idle
    }

    fn on_take(&mut self, take: PhraseTake, cx: &mut Context<Self>) {
        self.take = match take {
            PhraseTake::Listening { from_button } => Take::Listening { from_button },
            PhraseTake::Cleaning => Take::Cleaning,
            PhraseTake::Idle => Take::Idle,
            PhraseTake::Heard(text) => {
                let phrase = spoken_phrase(&text);
                if !phrase.is_empty() {
                    self.heard = Some(phrase);
                }
                Take::Idle
            }
        };
        cx.notify();
    }

    /// Puts a heard phrase in the box, which flashes to show it was filled.
    fn fill_heard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(phrase) = self.heard.take() else {
            return;
        };
        self.input
            .update(cx, |input, cx| input.set_value(phrase, window, cx));
        self.filled = true;
        cx.emit(SayFilled);
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(900))
                .await;
            this.update(cx, |this, cx| {
                this.filled = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn toggle_mic(&mut self, cx: &mut Context<Self>) {
        let Some(dictation) = self.dictation.clone() else {
            return;
        };
        match self.take {
            Take::Listening { from_button: true } => dictation.stop(cx),
            Take::Idle => dictation.start(cx),
            _ => {}
        }
    }

    /// The line under the box: how to fill it, or to let go of the keys.
    pub fn hint(&self, kind: SayHint, cx: &App) -> Div {
        let id = if self.dictation.is_none() {
            "dictionary-phrases-hint-typed"
        } else if self.take == (Take::Listening { from_button: false }) {
            "dictionary-phrases-hint-release"
        } else if kind == SayHint::Another {
            "dictionary-phrases-hint-another"
        } else {
            "dictionary-phrases-hint"
        };
        with_keys(&tr!(id), &self.chord, cx)
            .min_h(px(26.))
            .text_size(metrics::TEXT_XS)
            .line_height(px(26.))
    }
}

impl Render for SayBox {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.fill_heard(window, cx);
        let t = theme(cx).clone();
        let listening = matches!(self.take, Take::Listening { .. });
        let from_button = self.take == (Take::Listening { from_button: true });
        let cleaning = self.take == Take::Cleaning;
        let input: Input = text_input(&self.input, cx)
            .h(px(32.))
            .when(self.filled, |input| input.bg(t.accent.opacity(0.1)));
        let field = div()
            .relative()
            .flex_1()
            .min_w_0()
            .when(cleaning, |f| f.opacity(0.6))
            .child(input)
            .when(listening, |f| f.child(listening_overlay(cx)));

        div()
            .flex()
            .items_center()
            .gap_1p5()
            .child(field)
            .when_some(self.dictation.as_ref(), |row, _| {
                let button = Button::new("phrase-mic")
                    .size(ButtonSize::Icon)
                    .w(px(32.))
                    .h(px(32.))
                    .disabled(cleaning || (listening && !from_button))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_mic(cx)));
                let button = if from_button {
                    button
                        .icon(IconName::Square)
                        .tooltip(tr!("dictionary-phrases-stop"))
                } else {
                    button
                        .outline()
                        .icon(IconName::Mic)
                        .tooltip(tr!("dictionary-phrases-dictate"))
                };
                row.child(button)
            })
    }
}

/// Bars that show the box is listening, over its text.
fn listening_overlay(cx: &App) -> Div {
    let t = theme(cx).clone();
    let accent = t.accent;
    let bars = [6., 14., 10., 16., 8.];
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(10.))
        .rounded(metrics::RADIUS_MD)
        .border_1()
        .border_color(accent)
        .bg(t.card)
        .shadow(vec![BoxShadow {
            color: accent.opacity(0.2),
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(3.),
            inset: false,
        }])
        .child(
            div().flex().items_center().gap(px(2.)).h(px(16.)).children(
                bars.into_iter()
                    .enumerate()
                    .map(move |(i, height): (usize, f32)| {
                        div()
                            .w(px(3.))
                            .h(px(height))
                            .rounded(px(1.5))
                            .bg(accent)
                            .with_animation(
                                ("listening-bar", i),
                                Animation::new(Duration::from_millis(900)).repeat(),
                                move |bar, delta| {
                                    let phase = (delta + i as f32 * 0.18).fract();
                                    let scale = 0.45 + 0.55 * (phase * std::f32::consts::PI).sin();
                                    bar.h(px(height * scale))
                                },
                            )
                    }),
            ),
        )
        .child(
            div()
                .font_family(t.mono_font.clone())
                .text_size(metrics::TEXT_CAPTION)
                .text_color(accent)
                .child(tr!("dictionary-phrases-listening")),
        )
}
