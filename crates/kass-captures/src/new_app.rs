//! "New app. Use Code for Zed?" under the newest capture of each app whose
//! writing style the user hasn't chosen yet: take the suggested style, pick
//! another, or name a new one in place. Moving an app that has corrections
//! asks whether they come along, wherever the move starts.

use crate::page::CapturesPage;
use crate::widgets::icon;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Escape, InputEvent, InputState};
use gpui_kit::component::popover::Popover;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{
    CaptureResponse, CaptureSource, MovePlan, MovedCorrections, WritingStylesResponse,
};
use kass_i18n::tr;
use kass_ui::{
    Button, MenuTrigger, close_dialog, dialog_footer, dialog_header, menu_item, metrics,
    text_input, theme,
};
use std::collections::HashMap;

/// The longest style name, as the Writing Style page allows.
const MAX_NAME: usize = 40;

/// An app without a chosen style, prompted under one of its captures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NewApp {
    pub bundle: String,
    pub name: String,
    /// The style it's suggested: (id, name).
    pub suggested: (String, String),
}

/// One prompt per app without a confirmed style, keyed by the id of its
/// newest capture in `visible` (newest first). Commands teach no style, so
/// they get none.
pub(crate) fn new_app_prompts(
    visible: &[CaptureResponse],
    styles: &WritingStylesResponse,
) -> HashMap<String, NewApp> {
    let mut prompts = HashMap::new();
    let mut seen = std::collections::HashSet::new();
    for capture in visible {
        let Some(bundle) = capture.app_bundle_id.as_ref().filter(|b| !b.is_empty()) else {
            continue;
        };
        if capture.source == CaptureSource::Command || !seen.insert(bundle.clone()) {
            continue;
        }
        let app = styles.app(bundle);
        if app.is_some_and(|app| app.confirmed) {
            continue;
        }
        let suggested = match app {
            Some(app) => styles.suggested_style(app),
            None => styles.default_style(),
        };
        let Some(suggested) = suggested else {
            continue;
        };
        let name = capture
            .app_name
            .clone()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| bundle.clone());
        prompts.insert(
            capture.id.clone(),
            NewApp {
                bundle: bundle.clone(),
                name,
                suggested: (suggested.id.clone(), suggested.name.clone()),
            },
        );
    }
    prompts
}

/// Naming a new style in place of an app's prompt.
pub(crate) struct Naming {
    pub bundle: String,
    pub name: String,
    pub input: Entity<InputState>,
    _subscription: Subscription,
}

/// Asking whether an app's corrections come along to its new style.
struct MoveAsk {
    bundle: String,
    /// The name to send with the move, when known.
    name: Option<String>,
    /// The name the dialog shows.
    app: String,
    from: String,
    to_id: String,
    to: String,
    count: u64,
}

impl CapturesPage {
    /// Moves an app to a style, asking first when it has corrections
    /// teaching the style it's leaving. Choosing the style it already uses
    /// confirms a new app.
    pub(crate) fn move_app(
        &mut self,
        bundle: String,
        name: Option<String>,
        to: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(styles) = self.styles.as_ref().filter(|_| !self.moving) else {
            return;
        };
        let Some(app) = styles.app(&bundle) else {
            // Not seen by the styles yet: there's nothing to bring.
            self.assign_style(bundle, to, name, MovedCorrections::Bring, window, cx);
            return;
        };
        match app.plan_move(&to) {
            MovePlan::Stay => {}
            MovePlan::Assign => {
                self.assign_style(bundle, to, name, MovedCorrections::Bring, window, cx)
            }
            MovePlan::Ask => {
                let style_name =
                    |id: &str| styles.style(id).map(|s| s.name.clone()).unwrap_or_default();
                let ask = MoveAsk {
                    app: name
                        .clone()
                        .or_else(|| app.name.clone())
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| bundle.clone()),
                    from: style_name(&app.style_id),
                    to: style_name(&to),
                    count: app.corrections,
                    name,
                    bundle,
                    to_id: to,
                };
                self.ask_move(ask, window, cx);
            }
        }
    }

    fn ask_move(&mut self, ask: MoveAsk, window: &mut Window, cx: &mut Context<Self>) {
        // The header's picker already shows the new style; put it back until
        // the move happens, however the dialog closes.
        self.style_select = None;
        self.sync_style_select(window, cx);
        cx.notify();
        let page = cx.entity().downgrade();
        kass_ui::open_dialog(window, cx, move |_, cx| {
            let choose = |corrections: MovedCorrections| {
                let page = page.clone();
                let bundle = ask.bundle.clone();
                let to = ask.to_id.clone();
                let name = ask.name.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    close_dialog(window, cx);
                    page.update(cx, |page, cx| {
                        page.assign_style(
                            bundle.clone(),
                            to.clone(),
                            name.clone(),
                            corrections,
                            window,
                            cx,
                        )
                    })
                    .ok();
                }
            };
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(dialog_header(
                    tr!(
                        "writing-style-styles-move-dialog-title",
                        app = ask.app.clone(),
                        to = ask.to.clone()
                    ),
                    Some(
                        tr!(
                            "writing-style-styles-move-dialog-description",
                            app = ask.app.clone(),
                            from = ask.from.clone(),
                            to = ask.to.clone(),
                            count = ask.count
                        )
                        .into(),
                    ),
                    cx,
                ))
                .child(
                    dialog_footer()
                        .child(
                            Button::new("move-cancel")
                                .ghost()
                                .label(tr!("common-cancel"))
                                .on_click(|_, window, cx| close_dialog(window, cx)),
                        )
                        .child(
                            Button::new("move-leave")
                                .outline()
                                .label(tr!(
                                    "writing-style-styles-move-dialog-leave",
                                    from = ask.from.clone()
                                ))
                                .on_click(choose(MovedCorrections::Leave)),
                        )
                        .child(
                            Button::new("move-bring")
                                .label(tr!(
                                    "writing-style-styles-move-dialog-bring",
                                    count = ask.count,
                                    to = ask.to.clone()
                                ))
                                .on_click(choose(MovedCorrections::Bring)),
                        ),
                )
                .into_any_element()
        });
    }

    // Naming a new style

    fn begin_naming(&mut self, app: &NewApp, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr!("writing-style-styles-name-placeholder"))
                .validate(|text, _| text.chars().count() <= MAX_NAME)
        });
        let subscription =
            cx.subscribe_in(
                &input,
                window,
                |page, input, event, window, cx| match event {
                    InputEvent::PressEnter { .. } => page.finish_naming(window, cx),
                    // Leaving it empty cancels, as Escape does.
                    InputEvent::Blur if input.read(cx).value().trim().is_empty() => {
                        page.cancel_naming(cx)
                    }
                    _ => {}
                },
            );
        input.update(cx, |input, cx| input.focus(window, cx));
        self.naming = Some(Naming {
            bundle: app.bundle.clone(),
            name: app.name.clone(),
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn cancel_naming(&mut self, cx: &mut Context<Self>) {
        if self.naming.take().is_some() {
            cx.notify();
        }
    }

    /// Adds the named style, then moves the app to it.
    fn finish_naming(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(naming) = self.naming.as_ref() else {
            return;
        };
        let style_name = naming.input.read(cx).value().trim().to_string();
        if style_name.is_empty() || self.moving {
            return;
        }
        let (bundle, name) = (naming.bundle.clone(), naming.name.clone());
        self.moving = true;
        cx.notify();
        self.call(
            window,
            cx,
            move |client| client.create_writing_style(&style_name),
            move |page, result, window, cx| {
                page.moving = false;
                match result {
                    Ok(style) => {
                        page.naming = None;
                        let id = style.id.clone();
                        if let Some(styles) = page.styles.as_mut() {
                            styles.styles.push(style);
                        }
                        page.move_app(bundle, Some(name), id, window, cx);
                    }
                    Err(error) => Self::toast_error(
                        tr!("writing-style-styles-create-failed"),
                        error,
                        window,
                        cx,
                    ),
                }
                cx.notify();
            },
        );
    }

    // Drawing

    /// The prompt under an app's newest capture, or the new style's name
    /// field while the user is naming one for it.
    pub(crate) fn render_new_app_prompt(&self, app: &NewApp, cx: &mut Context<Self>) -> AnyElement {
        if let Some(naming) = self.naming.as_ref().filter(|n| n.bundle == app.bundle) {
            return self.render_naming(naming, cx);
        }
        let t = theme(cx).clone();
        let styles = self.styles.as_ref();
        let others: Vec<(String, String)> = styles
            .map(|s| {
                s.styles
                    .iter()
                    .filter(|style| style.id != app.suggested.0)
                    .map(|style| (style.id.clone(), style.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let full = styles.is_some_and(|s| !s.has_room());
        let max = styles.map_or(0, |s| s.max_styles);
        let busy = self.moving;
        let page = cx.entity().downgrade();

        let use_suggested = {
            let (bundle, name, to) = (
                app.bundle.clone(),
                app.name.clone(),
                app.suggested.0.clone(),
            );
            Button::new(SharedString::from(format!("use-{}", app.bundle)))
                .small()
                .h(px(26.))
                .px(px(10.))
                .text_size(metrics::TEXT_XS)
                .label(tr!("captures-apps-use", style = app.suggested.1.clone()))
                .disabled(busy)
                .on_click(cx.listener(move |page, _, window, cx| {
                    page.move_app(bundle.clone(), Some(name.clone()), to.clone(), window, cx)
                }))
        };

        let trigger = MenuTrigger::new(
            SharedString::from(format!("pick-{}", app.bundle)),
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(tr!("captures-apps-pick-another"))
                .child(icon(IconName::ChevronDown, px(12.), t.muted_foreground)),
            move |el, open, cx| {
                let t = theme(cx);
                el.flex()
                    .items_center()
                    .h(px(26.))
                    .px(px(10.))
                    .rounded(metrics::RADIUS_MD)
                    .border_1()
                    .border_color(if open { t.ring } else { t.input })
                    .text_size(metrics::TEXT_XS)
                    .text_color(t.foreground)
                    .hover(|s| s.bg(t.secondary))
            },
        );
        let prompted = app.clone();
        let menu = Popover::new(SharedString::from(format!("pick-menu-{}", app.bundle)))
            .anchor(Anchor::TopRight)
            .w(px(220.))
            .p_1()
            .trigger(trigger)
            .content(move |_, _, cx| {
                let t = theme(cx).clone();
                let popover = cx.entity();
                let pick = |to: String| {
                    let (page, popover, app) = (page.clone(), popover.clone(), prompted.clone());
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        popover.update(cx, |p, cx| p.dismiss(window, cx));
                        page.update(cx, |page, cx| {
                            page.move_app(
                                app.bundle.clone(),
                                Some(app.name.clone()),
                                to.clone(),
                                window,
                                cx,
                            )
                        })
                        .ok();
                    }
                };
                let new_style = {
                    let (page, popover, app) = (page.clone(), popover.clone(), prompted.clone());
                    menu_item("pick-new-style", cx)
                        .when(full, |el| el.opacity(0.5).cursor_default())
                        .child(icon(IconName::Plus, px(14.), t.foreground))
                        .child(tr!("writing-style-styles-new"))
                        .when(!full, |el| {
                            el.on_click(move |_, window, cx| {
                                popover.update(cx, |p, cx| p.dismiss(window, cx));
                                page.update(cx, |page, cx| page.begin_naming(&app, window, cx))
                                    .ok();
                            })
                        })
                };
                div()
                    .flex()
                    .flex_col()
                    .children(others.iter().map(|(id, name)| {
                        menu_item(SharedString::from(format!("pick-{id}")), cx)
                            .child(div().flex_1().truncate().child(name.clone()))
                            .on_click(pick(id.clone()))
                    }))
                    .when(!others.is_empty(), |el| {
                        el.child(div().my_1().h(px(1.)).bg(t.border))
                    })
                    .child(new_style)
                    .when(full, |el| {
                        el.child(
                            div()
                                .px_2()
                                .py(px(6.))
                                .text_size(px(11.5))
                                .text_color(t.muted_foreground)
                                .child(tr!("writing-style-styles-limit", count = max)),
                        )
                    })
                    .into_any_element()
            });

        div()
            .flex()
            .items_center()
            .gap_2()
            .px(px(12.))
            .pb(px(12.))
            .text_size(metrics::TEXT_XS)
            .text_color(t.muted_foreground)
            .child(div().flex_1().min_w_0().child(tr!(
                "captures-apps-prompt",
                style = app.suggested.1.clone(),
                app = app.name.clone()
            )))
            .child(use_suggested)
            .child(
                div()
                    .flex_none()
                    .when(busy, |el| el.opacity(0.5))
                    .child(menu),
            )
            .into_any_element()
    }

    /// Enter adds the style; Escape, Cancel or leaving it empty puts the
    /// prompt back.
    fn render_naming(&self, naming: &Naming, cx: &mut Context<Self>) -> AnyElement {
        let empty = naming.input.read(cx).value().trim().is_empty();
        let busy = self.moving;
        div()
            .flex()
            .items_center()
            .gap_2()
            .px(px(12.))
            .pb(px(12.))
            .on_action(cx.listener(|page, _: &Escape, _, cx| page.cancel_naming(cx)))
            .child(
                div().flex_1().min_w_0().child(
                    text_input(&naming.input, cx)
                        .h(px(26.))
                        .text_size(metrics::TEXT_XS),
                ),
            )
            .child(
                Button::new("naming-add")
                    .small()
                    .h(px(26.))
                    .px(px(10.))
                    .text_size(metrics::TEXT_XS)
                    .label(tr!("writing-style-styles-add"))
                    .disabled(empty || busy)
                    .on_click(cx.listener(|page, _, window, cx| page.finish_naming(window, cx))),
            )
            .child(
                Button::new("naming-cancel")
                    .outline()
                    .small()
                    .h(px(26.))
                    .px(px(10.))
                    .text_size(metrics::TEXT_XS)
                    .label(tr!("common-cancel"))
                    .on_click(cx.listener(|page, _, _, cx| page.cancel_naming(cx))),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's `test` macro would replace the standard one.
    use super::new_app_prompts;
    use kass_api::types::{CaptureResponse, WritingStylesResponse};
    use serde_json::json;

    fn capture(id: &str, bundle: Option<&str>, source: &str) -> CaptureResponse {
        serde_json::from_value(json!({
            "id": id,
            "audio_path": "",
            "source": source,
            "transcript_raw": "raw",
            "created_at": "2026-11-16T10:00:00",
            "app_bundle_id": bundle,
            "app_name": bundle.map(|b| b.trim_start_matches("com.")),
        }))
        .unwrap()
    }

    fn styles() -> WritingStylesResponse {
        serde_json::from_value(json!({
            "styles": [
                {"id": "work", "name": "Work", "position": 0, "is_default": true,
                 "punctuation_style": "learned", "preserve_technical": false},
                {"id": "code", "name": "Code", "position": 1, "is_default": false,
                 "punctuation_style": "learned", "preserve_technical": true},
            ],
            "apps": [
                {"bundle_id": "com.zed", "style_id": "work", "confirmed": false,
                 "suggested_style_id": "code"},
                {"bundle_id": "com.slack", "style_id": "work", "confirmed": true},
            ],
            "max_styles": 3,
        }))
        .unwrap()
    }

    #[test]
    fn prompts_each_unconfirmed_app_once_under_its_newest_capture() {
        let visible = [
            capture("3", Some("com.zed"), "dictation"),
            capture("2", Some("com.slack"), "dictation"),
            capture("1", Some("com.zed"), "dictation"),
        ];
        let prompts = new_app_prompts(&visible, &styles());
        assert_eq!(prompts.len(), 1);
        let zed = &prompts["3"];
        assert_eq!(zed.bundle, "com.zed");
        assert_eq!(zed.name, "zed");
        assert_eq!(zed.suggested, ("code".into(), "Code".into()));
    }

    #[test]
    fn skips_commands_and_captures_without_an_app() {
        let visible = [
            capture("3", Some("com.zed"), "command"),
            capture("2", None, "dictation"),
            capture("1", Some("com.zed"), "dictation"),
        ];
        let prompts = new_app_prompts(&visible, &styles());
        assert_eq!(prompts.keys().collect::<Vec<_>>(), ["1"]);
    }

    #[test]
    fn an_app_the_styles_dont_know_yet_is_suggested_the_default() {
        let visible = [capture("1", Some("com.notes"), "dictation")];
        let prompts = new_app_prompts(&visible, &styles());
        assert_eq!(prompts["1"].suggested, ("work".into(), "Work".into()));
    }
}
