//! The recording in the detail header: a play button, the waveform (click
//! or drag it to seek) and the time.

use crate::audio::{Player, decode_wav, peaks};
use crate::format::format_duration;
use crate::widgets::icon;
use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::ApiClient;
use kass_ui::{metrics, theme};
use std::cell::Cell;
use std::rc::Rc;

const BARS: usize = 34;
const BAR_WIDTH: f32 = 2.;
const BAR_GAP: f32 = 2.;

enum Load {
    Loading,
    Ready { player: Player, peaks: Vec<f32> },
    Failed,
}

pub struct PlayerView {
    server_ms: Option<i64>,
    load: Load,
    /// The user moved the playhead, so the time shows where it is.
    seeked: bool,
    dragging: bool,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    _fetch: Task<()>,
}

impl PlayerView {
    pub fn new(
        client: ApiClient,
        capture_id: String,
        server_ms: Option<i64>,
        cx: &mut Context<Self>,
    ) -> Self {
        let fetch = cx.spawn(async move |view, cx| {
            let clip = cx
                .background_executor()
                .spawn(async move {
                    let bytes = client
                        .capture_audio(&capture_id)
                        .map_err(|e| e.to_string())?;
                    let clip = decode_wav(&bytes)?;
                    let peaks = peaks(&clip, BARS);
                    Ok::<_, String>((clip, peaks))
                })
                .await;
            view.update(cx, |view, cx| {
                view.load = match clip {
                    Ok((clip, peaks)) => Load::Ready {
                        player: Player::new(clip),
                        peaks,
                    },
                    Err(error) => {
                        eprintln!("kass-captures: recording failed to load: {error}");
                        Load::Failed
                    }
                };
                cx.notify();
            })
            .ok();
        });
        Self {
            server_ms,
            load: Load::Loading,
            seeked: false,
            dragging: false,
            bounds: Rc::new(Cell::new(Bounds::default())),
            _fetch: fetch,
        }
    }

    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        let Load::Ready { player, .. } = &mut self.load else {
            return;
        };
        if player.is_playing() {
            player.pause();
        } else if let Err(error) = player.play() {
            eprintln!("kass-captures: playback failed: {error}");
            self.load = Load::Failed;
        }
        cx.notify();
    }

    fn seek_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Load::Ready { player, .. } = &self.load else {
            return;
        };
        let bounds = self.bounds.get();
        if bounds.size.width <= px(0.) {
            return;
        }
        player.seek((x - bounds.origin.x) / bounds.size.width);
        self.seeked = true;
        cx.notify();
    }

    fn time(&self) -> String {
        match &self.load {
            Load::Failed => "—".into(),
            Load::Ready { player, .. } if player.is_playing() || self.seeked => {
                format_duration(Some(player.position_ms() as i64))
            }
            Load::Ready { player, .. } => format_duration(Some(player.clip().duration_ms() as i64)),
            Load::Loading => format_duration(self.server_ms),
        }
    }
}

impl Drop for PlayerView {
    fn drop(&mut self) {
        if let Load::Ready { player, .. } = &mut self.load {
            player.pause();
        }
    }
}

impl Render for PlayerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let (playing, progress, bars) = match &self.load {
            Load::Ready { player, peaks } => {
                (player.is_playing(), player.progress(), peaks.clone())
            }
            _ => (false, 0., vec![0.; BARS]),
        };
        if playing {
            window.request_animation_frame();
        }
        let button_icon = match self.load {
            Load::Loading => IconName::Loader,
            _ if playing => IconName::Pause,
            _ => IconName::Play,
        };
        let ready = matches!(self.load, Load::Ready { .. });
        let bounds = self.bounds.clone();
        let (played, rest) = (t.accent, t.muted_foreground.opacity(0.35));

        let wave = canvas(
            move |b, _, _| bounds.set(b),
            move |b, _, window, _| {
                let mid = b.origin.y + b.size.height / 2.;
                for (i, peak) in bars.iter().enumerate() {
                    let x = b.origin.x + px(i as f32 * (BAR_WIDTH + BAR_GAP));
                    let h = (b.size.height * peak.max(0.08)).max(px(2.));
                    let at = (i as f32 + 0.5) / BARS as f32;
                    let color = if at <= progress { played } else { rest };
                    window.paint_quad(
                        fill(
                            Bounds::new(point(x, mid - h / 2.), size(px(BAR_WIDTH), h)),
                            color,
                        )
                        .corner_radii(px(1.)),
                    );
                }
            },
        )
        .size_full();

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .w(px(240.))
            .h(px(36.))
            .pl(px(4.))
            .pr(px(12.))
            .rounded_full()
            .border_1()
            .border_color(t.border)
            .bg(t.card)
            .child(
                div()
                    .id("player-play")
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(28.))
                    .rounded_full()
                    .bg(t.accent)
                    .when(ready, |b| b.cursor_pointer())
                    .on_click(cx.listener(|view, _, _, cx| view.toggle(cx)))
                    .child(icon(button_icon, px(14.), t.accent_foreground)),
            )
            .child(
                div()
                    .id("player-wave")
                    .flex_1()
                    .min_w_0()
                    .h(px(18.))
                    .when(ready, |w| w.cursor_pointer())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|view, event: &MouseDownEvent, _, cx| {
                            view.dragging = true;
                            view.seek_to(event.position.x, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, _, cx| {
                        if view.dragging && event.pressed_button == Some(MouseButton::Left) {
                            view.seek_to(event.position.x, cx);
                        } else {
                            view.dragging = false;
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|view, _: &MouseUpEvent, _, _| view.dragging = false),
                    )
                    .child(wave),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(t.mono_font.clone())
                    .text_size(metrics::TEXT_CAPTION)
                    .text_color(t.muted_foreground)
                    .child(self.time()),
            )
    }
}
