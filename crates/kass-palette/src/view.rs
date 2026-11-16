//! The ⌘K palette: search captures and run commands. A leading `>` limits
//! the list to commands. ⏎ runs the highlighted row, and ⌘⏎ on a capture
//! copies its text instead of opening it.

use crate::search::{Results, capture_meta, capture_text, match_ranges, step};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::CaptureResponse;
use kass_i18n::tr;
use kass_ui::{metrics, theme};
use std::rc::Rc;

actions!(kass_palette, [SelectNext, SelectPrevious]);

const CONTEXT: &str = "CommandPalette";

struct KeysBound;
impl Global for KeysBound {}

fn bind_keys(cx: &mut App) {
    if cx.has_global::<KeysBound>() {
        return;
    }
    cx.set_global(KeysBound);
    // The search box takes up and down for its cursor otherwise.
    let input = format!("{CONTEXT} > Input");
    cx.bind_keys([
        KeyBinding::new("down", SelectNext, Some(CONTEXT)),
        KeyBinding::new("up", SelectPrevious, Some(CONTEXT)),
        KeyBinding::new("down", SelectNext, Some(&input)),
        KeyBinding::new("up", SelectPrevious, Some(&input)),
    ]);
}

/// Something the palette can do besides opening a capture.
#[derive(Clone)]
pub struct PaletteCommand {
    pub id: SharedString,
    pub label: SharedString,
    /// A short mono hint on the right of the row.
    pub hint: SharedString,
    pub run: Rc<dyn Fn(&mut App)>,
}

/// What the palette asks of the app around it.
pub trait PaletteHost: 'static {
    /// Put the palette away.
    fn close(&self, cx: &mut App);
    /// Show the capture on the Captures screen.
    fn open_capture(&self, id: &str, cx: &mut App);
    /// Put the capture's text on the clipboard and say so.
    fn copy_capture(&self, text: String, cx: &mut App);
}

/// A row of the list.
enum Item {
    Capture(usize),
    Command(usize),
}

pub struct CommandPalette {
    host: Rc<dyn PaletteHost>,
    commands: Vec<PaletteCommand>,
    captures: Vec<CaptureResponse>,
    search: Entity<InputState>,
    query: String,
    selected: usize,
    scroll: ScrollHandle,
    _search: Subscription,
}

impl CommandPalette {
    pub fn new(
        host: Rc<dyn PaletteHost>,
        commands: Vec<PaletteCommand>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        bind_keys(cx);
        let search = cx
            .new(|cx| InputState::new(window, cx).placeholder(kass_i18n::t("palette-placeholder")));
        let subscription =
            cx.subscribe_in(
                &search,
                window,
                |palette, state, event, _, cx| match event {
                    InputEvent::Change => {
                        palette.query = state.read(cx).value().to_string();
                        palette.selected = 0;
                        palette.scroll.scroll_to_item(0);
                        cx.notify();
                    }
                    InputEvent::PressEnter { secondary, .. } => {
                        palette.choose(None, *secondary, cx)
                    }
                    InputEvent::Focus | InputEvent::Blur => {}
                },
            );
        search.update(cx, |state, cx| state.focus(window, cx));
        Self {
            host,
            commands,
            captures: Vec::new(),
            search,
            query: String::new(),
            selected: 0,
            scroll: ScrollHandle::new(),
            _search: subscription,
        }
    }

    /// The captures to search, once they've loaded.
    pub fn set_captures(&mut self, captures: Vec<CaptureResponse>, cx: &mut Context<Self>) {
        self.captures = captures;
        cx.notify();
    }

    fn results(&self) -> Results {
        let labels: Vec<&str> = self.commands.iter().map(|c| c.label.as_ref()).collect();
        Results::of(&self.query, &self.captures, &labels)
    }

    fn item(results: &Results, index: usize) -> Option<Item> {
        match results.captures.get(index) {
            Some(found) => Some(Item::Capture(found.index)),
            None => results
                .commands
                .get(index - results.captures.len())
                .map(|&command| Item::Command(command)),
        }
    }

    /// Run row `index`, or the highlighted one. `copy` copies a capture.
    fn choose(&mut self, index: Option<usize>, copy: bool, cx: &mut Context<Self>) {
        let results = self.results();
        let index = index.unwrap_or_else(|| results.active(self.selected));
        let Some(item) = Self::item(&results, index) else {
            return;
        };
        let host = self.host.clone();
        host.close(cx);
        match item {
            Item::Capture(i) => {
                let capture = &self.captures[i];
                if copy {
                    host.copy_capture(capture_text(capture).to_string(), cx);
                } else {
                    host.open_capture(&capture.id, cx);
                }
            }
            Item::Command(i) => (self.commands[i].run)(cx),
        }
    }

    fn move_selection(&mut self, down: bool, cx: &mut Context<Self>) {
        let results = self.results();
        if results.is_empty() {
            return;
        }
        self.selected = step(results.active(self.selected), results.len(), down);
        // The list's children include the group headings.
        let headings = usize::from(!results.captures.is_empty())
            + usize::from(self.selected >= results.captures.len() && !results.commands.is_empty());
        self.scroll.scroll_to_item(self.selected + headings);
        cx.notify();
    }

    fn hover(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.selected != index {
            self.selected = index;
            cx.notify();
        }
    }

    fn row(&self, index: usize, active: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = theme(cx);
        let row = div()
            .id(("palette-row", index))
            .relative()
            .w_full()
            .overflow_hidden()
            .rounded(metrics::RADIUS_LG)
            .px(px(12.))
            .text_color(t.foreground)
            .on_mouse_move(cx.listener(move |palette, _, _, cx| palette.hover(index, cx)))
            .on_click(cx.listener(move |palette, _, _, cx| palette.choose(Some(index), false, cx)));
        if active {
            row.bg(t.secondary).child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(2.))
                    .bg(t.accent),
            )
        } else {
            row
        }
    }

    fn heading(&self, label: String, spaced: bool, cx: &App) -> Div {
        let t = theme(cx);
        div()
            .px(px(10.))
            .pt(px(if spaced { 14. } else { 10. }))
            .pb(px(6.))
            .font_family(t.mono_font.clone())
            .text_size(metrics::TEXT_CAPTION)
            .text_color(t.muted_foreground)
            .child(label)
    }

    fn list(
        &self,
        results: &Results,
        max_height: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx).clone();
        let active = results.active(self.selected);
        let mut list = div()
            .id("palette-list")
            .max_h(max_height)
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .p(px(8.));
        if !results.captures.is_empty() {
            list = list.child(self.heading(
                tr!("palette-captures", count = results.captures.len()),
                false,
                cx,
            ));
            for (i, found) in results.captures.iter().enumerate() {
                let capture = &self.captures[found.index];
                let highlight = HighlightStyle {
                    background_color: Some(t.accent.opacity(0.25)),
                    ..Default::default()
                };
                let text = StyledText::new(found.snippet.clone()).with_highlights(
                    match_ranges(&found.snippet, &results.term)
                        .into_iter()
                        .map(|range| (range, highlight)),
                );
                list = list.child(
                    self.row(i, i == active, cx)
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .py(px(10.))
                        .child(
                            div()
                                .text_size(metrics::TEXT_SM)
                                .line_height(px(20.))
                                .line_clamp(2)
                                .child(text),
                        )
                        .child(
                            div()
                                .font_family(t.mono_font.clone())
                                .text_size(metrics::TEXT_CAPTION)
                                .text_color(t.muted_foreground)
                                .child(capture_meta(capture)),
                        ),
                );
            }
        }
        if !results.commands.is_empty() {
            list = list.child(self.heading(
                kass_i18n::t("palette-commands"),
                !results.captures.is_empty(),
                cx,
            ));
            for (j, &command) in results.commands.iter().enumerate() {
                let i = results.captures.len() + j;
                let command = &self.commands[command];
                list = list.child(
                    self.row(i, i == active, cx)
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .h(px(40.))
                        .text_size(metrics::TEXT_SM)
                        .text_color(t.foreground.opacity(0.85))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(command.label.clone()),
                        )
                        .child(
                            div()
                                .font_family(t.mono_font.clone())
                                .text_size(metrics::TEXT_CAPTION)
                                .text_color(t.muted_foreground)
                                .child(command.hint.clone()),
                        ),
                );
            }
        }
        if results.is_empty() {
            list = list.child(
                div()
                    .px(px(12.))
                    .py(px(24.))
                    .text_center()
                    .text_size(metrics::TEXT_BASE)
                    .text_color(t.muted_foreground)
                    .child(kass_i18n::t("palette-empty")),
            );
        }
        list
    }
}

/// `inline-flex h-5 min-w-5 rounded border border-input px-1.5 font-mono text-[11px]`.
fn kbd(label: &'static str, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(20.))
        .min_w(px(20.))
        .px(px(6.))
        .rounded(px(4.))
        .border_1()
        .border_color(t.input)
        .font_family(t.mono_font.clone())
        .text_size(metrics::TEXT_CAPTION)
        .text_color(t.muted_foreground)
        .child(label)
}

impl Render for CommandPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let results = self.results();
        let viewport = window.viewport_size();
        let max_height = px(460.).min(viewport.height * 0.6);
        let width = px(620.).min(viewport.width - px(32.));
        let host = self.host.clone();

        let panel = div()
            .id("palette")
            .w(width)
            .overflow_hidden()
            .rounded(metrics::RADIUS_XL)
            .border_1()
            .border_color(t.input)
            .bg(t.popover)
            .shadow(vec![BoxShadow {
                color: black().opacity(0.6),
                offset: point(px(0.), px(25.)),
                blur_radius: px(50.),
                spread_radius: px(-12.),
                inset: false,
            }])
            // A click inside stays inside; the backdrop's closes.
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .h(px(56.))
                    .px(px(18.))
                    .border_b_1()
                    .border_color(t.border)
                    .child(
                        svg()
                            .path(IconName::Search.path())
                            .size(px(17.))
                            .flex_none()
                            .text_color(t.muted_foreground),
                    )
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.search)
                                .appearance(false)
                                .text_size(px(17.)),
                        ),
                    )
                    .child(kbd("esc", cx)),
            )
            .child(self.list(&results, max_height, cx))
            .child(
                div()
                    .flex()
                    .gap(px(18.))
                    .px(px(18.))
                    .py(px(10.))
                    .border_t_1()
                    .border_color(t.border)
                    .font_family(t.mono_font.clone())
                    .text_size(metrics::TEXT_CAPTION)
                    .text_color(t.muted_foreground)
                    .child(kass_i18n::t("palette-hints-move"))
                    .child(kass_i18n::t("palette-hints-open"))
                    .child(kass_i18n::t("palette-hints-copy"))
                    .child(div().flex_1())
                    .child(kass_i18n::t("palette-hints-commands-only")),
            );

        div()
            .id("palette-backdrop")
            .key_context(CONTEXT)
            .on_action(
                cx.listener(|palette, _: &SelectNext, _, cx| palette.move_selection(true, cx)),
            )
            .on_action(
                cx.listener(|palette, _: &SelectPrevious, _, cx| palette.move_selection(false, cx)),
            )
            .on_action(cx.listener(|palette, _: &Escape, _, cx| palette.host.close(cx)))
            .size_full()
            .relative()
            .bg(black().opacity(0.7))
            .font_family(t.sans_font())
            .text_size(metrics::TEXT_BASE)
            .text_color(t.foreground)
            .on_click(move |_, _, cx| host.close(cx))
            .child(
                div()
                    .absolute()
                    .top(viewport.height * 0.14)
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .child(panel),
            )
    }
}
