//! The styles as cards with their apps, the new-style card, and the list of
//! every app with the style it uses.

use super::{WritingStylePage, toast_error};
use crate::model::{
    AppFilter, MovePlan, PREVIEW_APPS, app_label, apps_in, can_add_style, confirmed,
    default_style, filter_apps, moved, plan_move, suggested_style,
};
use crate::widgets::{MenuTrigger, app_tile, icon, menu_item, mono_label};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Escape, InputEvent, InputState};
use gpui_kit::component::popover::Popover;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{AppToConfirm, MovedCorrections, StyledApp, WritingStyle, WritingStylesResponse};
use kass_i18n::tr;
use kass_ui::{
    Button, SettingSection, close_dialog, dialog_footer, dialog_header, metrics, open_dialog, text_input, theme,
};

/// An app being dragged from one style's card to another.
#[derive(Clone)]
struct DraggedApp {
    bundle: String,
    label: String,
}

/// What follows the pointer while an app is dragged.
struct DragPreview(String);

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded(metrics::RADIUS_MD)
            .border_1()
            .border_color(t.accent.opacity(0.5))
            .bg(t.card)
            .text_size(metrics::TEXT_XS)
            .text_color(t.foreground)
            .child(app_tile(&self.0, px(20.), px(10.), px(5.), cx))
            .child(self.0.clone())
    }
}

impl WritingStylePage {
    pub(super) fn render_board(
        &mut self,
        data: &WritingStylesResponse,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx).clone();
        let drafting = self.draft.is_some();
        let full = !can_add_style(data, false);
        let can_add = can_add_style(data, drafting);
        let chosen = self.style_id();

        let header = div()
            .flex()
            .items_center()
            .justify_end()
            .gap_3()
            .mb(px(10.))
            .when(full, |el| {
                el.child(
                    div()
                        .text_size(px(11.5))
                        .text_color(t.muted_foreground)
                        .child(tr!("writing-style-styles-limit", count = data.max_styles)),
                )
            })
            .child(
                div()
                    .id("new-style")
                    .flex()
                    .items_center()
                    .gap_1()
                    .h(px(32.))
                    .px_3()
                    .rounded(metrics::RADIUS_MD)
                    .border_1()
                    .border_dashed()
                    .border_color(t.input)
                    .text_size(px(12.5))
                    .text_color(t.foreground)
                    .when(can_add, |el| {
                        el.cursor_pointer()
                            .hover(|s| s.bg(t.secondary))
                            .on_click(cx.listener(|page, _, window, cx| {
                                page.begin_draft(window, cx)
                            }))
                    })
                    .when(!can_add, |el| el.opacity(0.5))
                    .child(icon(IconName::Plus, px(14.), t.foreground))
                    .child(tr!("writing-style-styles-new")),
            );

        let cards = data
            .styles
            .iter()
            .map(|style| self.render_card(data, style, chosen.as_deref() == Some(&style.id), cx))
            .collect::<Vec<_>>();
        let draft = self.draft.as_ref().map(|(input, _)| input.clone());

        SettingSection::new(tr!("writing-style-styles-title"))
            .description(tr!("writing-style-styles-description"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(header)
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(10.))
                            .children(cards)
                            .when_some(draft, |el, input| {
                                el.child(render_draft(&input, window, cx))
                            }),
                    )
                    .when_some(data.cache_mb_per_style, |el, mb| {
                        el.child(
                            div()
                                .mt(px(10.))
                                .text_size(metrics::TEXT_XS)
                                .text_color(t.muted_foreground)
                                .child(tr!("writing-style-styles-memory", mb = mb)),
                        )
                    }),
            )
    }

    fn render_card(
        &self,
        data: &WritingStylesResponse,
        style: &WritingStyle,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx).clone();
        let apps = apps_in(data, &style.id);
        let rest = apps.len().saturating_sub(PREVIEW_APPS);
        let id = style.id.clone();
        let tiles = apps.iter().take(PREVIEW_APPS).map(|app| {
            let label = app_label(app);
            let dragged = DraggedApp {
                bundle: app.bundle_id.clone(),
                label: label.clone(),
            };
            div()
                .id(SharedString::from(format!("tile-{}", app.bundle_id)))
                .relative()
                .cursor_grab()
                .child(app_tile(&label, px(32.), px(13.), metrics::RADIUS_LG, cx))
                .when(!app.confirmed, |el| {
                    el.child(
                        div()
                            .absolute()
                            .top(px(-3.))
                            .right(px(-3.))
                            .size(px(10.))
                            .rounded_full()
                            .bg(t.accent)
                            .border_2()
                            .border_color(t.background),
                    )
                })
                .tooltip({
                    let label = label.clone();
                    move |window, cx| gpui_kit::component::tooltip::Tooltip::new(label.clone()).build(window, cx)
                })
                .on_drag(dragged, |app, _, _, cx| cx.new(|_| DragPreview(app.label.clone())))
        });

        let border = if selected {
            t.accent.opacity(0.55)
        } else {
            t.border
        };
        let bg = if selected {
            t.accent.opacity(0.04)
        } else {
            t.card.opacity(0.4)
        };
        let accent = t.accent;

        div()
            .id(SharedString::from(format!("style-{}", style.id)))
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(200.))
            .gap(px(10.))
            .p(px(12.))
            .rounded(px(10.))
            .border_1()
            .border_color(border)
            .bg(bg)
            .cursor_pointer()
            .drag_over::<DraggedApp>(move |s, _, _, _| {
                s.border_dashed()
                    .border_color(accent)
                    .bg(accent.opacity(0.08))
            })
            .on_drop(cx.listener({
                let id = id.clone();
                move |page, app: &DraggedApp, window, cx| {
                    page.move_app(&app.bundle, &id, window, cx)
                }
            }))
            .on_click(cx.listener({
                let id = id.clone();
                move |page, _, _, cx| {
                    page.filter = match &page.filter {
                        AppFilter::Style(current) if *current == id => AppFilter::All,
                        _ => AppFilter::Style(id.clone()),
                    };
                    page.choose_style(Some(id.clone()), cx);
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(metrics::TEXT_SM)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.foreground)
                            .child(style.name.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.5))
                            .text_color(t.muted_foreground)
                            .child(tr!("writing-style-styles-count", count = apps.len())),
                    )
                    .child(div().flex_1())
                    .when(style.is_default, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .px_2()
                                .rounded_full()
                                .border_1()
                                .border_color(t.input)
                                .text_size(px(10.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(t.muted_foreground)
                                .child(tr!("writing-style-styles-default")),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_h(px(32.))
                    .children(tiles)
                    .when(rest > 0, |el| {
                        el.child(
                            div()
                                .id(SharedString::from(format!("more-{}", style.id)))
                                .flex()
                                .items_center()
                                .h(px(32.))
                                .px_2()
                                .rounded(metrics::RADIUS_LG)
                                .bg(t.secondary)
                                .text_size(metrics::TEXT_XS)
                                .text_color(t.foreground)
                                .hover(|s| s.bg(t.input))
                                .child(tr!("writing-style-styles-more", count = rest))
                                .on_click(cx.listener({
                                    let id = id.clone();
                                    move |page, _, _, cx| {
                                        cx.stop_propagation();
                                        page.filter = AppFilter::Style(id.clone());
                                        page.choose_style(Some(id.clone()), cx);
                                    }
                                })),
                        )
                    })
                    .when(apps.is_empty(), |el| {
                        el.child(
                            div()
                                .text_size(metrics::TEXT_XS)
                                .text_color(t.muted_foreground.opacity(0.7))
                                .child(tr!("writing-style-styles-empty")),
                        )
                    }),
            )
            .into_any_element()
    }

    // Adding a style

    fn begin_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.draft.is_some() {
            return;
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(tr!("writing-style-styles-name-placeholder"))
                .validate(|text, _| text.chars().count() <= crate::model::MAX_NAME)
        });
        let subscription = cx.subscribe_in(&input, window, |page, _, event, window, cx| {
            match event {
                InputEvent::PressEnter { .. } | InputEvent::Blur => page.finish_draft(window, cx),
                _ => {}
            }
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.draft = Some((input, subscription));
        cx.notify();
    }

    fn cancel_draft(&mut self, cx: &mut Context<Self>) {
        self.draft = None;
        cx.notify();
    }

    fn finish_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((input, _)) = self.draft.take() else {
            return;
        };
        cx.notify();
        let name = input.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        self.call(
            window,
            cx,
            move |client| client.create_writing_style(&name),
            |page, result, window, cx| match result {
                Ok(style) => {
                    page.wanted = Some(style.id);
                    page.reload(cx);
                }
                Err(error) => {
                    toast_error("writing-style-styles-create-failed", error, window, cx)
                }
            },
        );
    }

    // The app list

    pub(super) fn render_app_list(
        &mut self,
        data: &WritingStylesResponse,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = theme(cx).clone();
        let fresh: Vec<&StyledApp> = data.apps.iter().filter(|app| !app.confirmed).collect();
        let default_name = default_style(data).map(|s| s.name.clone()).unwrap_or_default();
        let query = self.search.read(cx).value().to_string();
        let apps = filter_apps(data, &self.filter, &query);

        let notice = (!fresh.is_empty()).then(|| {
            let count = fresh.len();
            let to_confirm: Vec<AppToConfirm> = fresh.iter().map(|app| (*app).into()).collect();
            div()
                .flex()
                .items_center()
                .gap_3()
                .mb_3()
                .px_3()
                .py_2()
                .rounded(metrics::RADIUS_LG)
                .border_1()
                .border_color(t.accent.opacity(0.3))
                .bg(t.accent.opacity(0.06))
                .text_size(px(12.5))
                .text_color(t.foreground)
                .child(div().flex_none().size(px(7.)).rounded_full().bg(t.accent))
                .child(div().flex_1().child(tr!(
                    "writing-style-styles-new-notice",
                    count = count,
                    style = default_name.clone()
                )))
                .child(
                    Button::new("show-new")
                        .outline()
                        .small()
                        .label(tr!("writing-style-styles-show-new"))
                        .on_click(cx.listener(|page, _, _, cx| {
                            page.filter = AppFilter::New;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("keep-all")
                        .small()
                        .label(tr!("writing-style-styles-keep-all", style = default_name.clone()))
                        .on_click(cx.listener(move |page, _, window, cx| {
                            page.keep_all(to_confirm.clone(), window, cx)
                        })),
                )
        });

        let filter_chip = match &self.filter {
            AppFilter::All => None,
            AppFilter::New => Some(tr!("writing-style-styles-filter-new")),
            AppFilter::Style(id) => Some(tr!(
                "writing-style-styles-filter-style",
                style = data
                    .styles
                    .iter()
                    .find(|s| &s.id == id)
                    .map(|s| s.name.clone())
                    .unwrap_or_default()
            )),
        };

        let tools = div()
            .flex()
            .items_center()
            .gap_2()
            .mb_3()
            .child(
                div().w(px(260.)).child(
                    text_input(&self.search, cx)
                        .prefix(icon(IconName::Search, px(14.), t.muted_foreground)),
                ),
            )
            .when_some(filter_chip, |el, label| {
                el.child(
                    div()
                        .id("clear-filter")
                        .flex()
                        .items_center()
                        .gap_1()
                        .h(px(28.))
                        .px_2()
                        .rounded(metrics::RADIUS_MD)
                        .border_1()
                        .border_color(t.accent.opacity(0.5))
                        .bg(t.accent.opacity(0.08))
                        .text_size(px(12.5))
                        .text_color(t.foreground)
                        .cursor_pointer()
                        .child(label)
                        .child(icon(IconName::X, px(12.), t.muted_foreground))
                        .on_click(cx.listener(|page, _, _, cx| {
                            page.filter = AppFilter::All;
                            cx.notify();
                        })),
                )
            });

        let head = div()
            .flex()
            .items_center()
            .gap(px(16.))
            .px(px(14.))
            .py_2()
            .bg(t.card)
            .child(div().flex_1().child(mono_label(tr!("writing-style-styles-col-app"), cx)))
            .child(
                div()
                    .w(px(88.))
                    .flex()
                    .justify_end()
                    .child(mono_label(tr!("writing-style-styles-col-count"), cx)),
            )
            .child(div().w(px(180.)).child(mono_label(tr!("writing-style-styles-col-style"), cx)));

        let rows: Vec<AnyElement> = apps
            .iter()
            .map(|app| self.render_app_row(data, app, window, cx))
            .collect();

        SettingSection::new(tr!("writing-style-styles-apps-title"))
            .description(tr!("writing-style-styles-apps-total", count = data.apps.len()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .children(notice)
                    .child(tools)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .rounded(px(10.))
                            .border_1()
                            .border_color(t.border)
                            .overflow_hidden()
                            .child(head)
                            .child(
                                div()
                                    .id("app-rows")
                                    .flex()
                                    .flex_col()
                                    .max_h(px(340.))
                                    .overflow_y_scroll()
                                    .children(rows)
                                    .when(apps.is_empty(), |el| {
                                        el.child(
                                            div()
                                                .px(px(14.))
                                                .py(px(16.))
                                                .border_t_1()
                                                .border_color(t.border.opacity(0.6))
                                                .text_size(px(12.5))
                                                .text_color(t.muted_foreground)
                                                .child(tr!("writing-style-styles-no-match")),
                                        )
                                    }),
                            ),
                    ),
            )
    }

    fn render_app_row(
        &self,
        data: &WritingStylesResponse,
        app: &StyledApp,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx).clone();
        let label = app_label(app);
        let current = if app.confirmed {
            data.styles
                .iter()
                .find(|s| s.id == app.style_id)
                .map(|s| (s.name.clone(), false))
        } else {
            suggested_style(data, app)
                .map(|s| (tr!("writing-style-styles-suggested", style = s.name.clone()), true))
        };
        let (current_label, suggested) = current.unwrap_or_default();

        let styles: Vec<(String, String)> = data
            .styles
            .iter()
            .map(|s| (s.id.clone(), s.name.clone()))
            .collect();
        let bundle = app.bundle_id.clone();
        let chosen = app.confirmed.then(|| app.style_id.clone());
        let page = cx.entity().downgrade();

        let trigger = MenuTrigger::new(
            SharedString::from(format!("app-style-{}", app.bundle_id)),
            div()
                .flex()
                .items_center()
                .gap_1()
                .w_full()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(if suggested { t.accent } else { t.foreground })
                        .child(current_label),
                )
                .child(icon(IconName::ChevronDown, px(14.), t.muted_foreground)),
            move |el, open, cx| {
                let t = theme(cx);
                el.flex()
                    .items_center()
                    .h(px(28.))
                    .w(px(180.))
                    .px_2()
                    .rounded(metrics::RADIUS_MD)
                    .border_1()
                    .border_color(if open { t.ring } else { t.input })
                    .bg(t.card)
                    .text_size(metrics::TEXT_XS)
            },
        );

        let menu = Popover::new(SharedString::from(format!("app-menu-{}", app.bundle_id)))
            .w(px(200.))
            .p_1()
            .trigger(trigger)
            .content(move |_, _, cx| {
                let t = theme(cx).clone();
                let popover = cx.entity();
                div().flex().flex_col().children(styles.iter().map(|(id, name)| {
                    let page = page.clone();
                    let popover = popover.clone();
                    let bundle = bundle.clone();
                    let to = id.clone();
                    menu_item(SharedString::from(format!("to-{id}")), cx)
                        .child(div().w(px(14.)).flex_none().when(
                            chosen.as_deref() == Some(id.as_str()),
                            |el| el.child(icon(IconName::Check, px(14.), t.foreground)),
                        ))
                        .child(div().flex_1().truncate().child(name.clone()))
                        .on_click(move |_, window, cx| {
                            popover.update(cx, |p, cx| p.dismiss(window, cx));
                            page.update(cx, |page, cx| page.move_app(&bundle, &to, window, cx))
                                .ok();
                        })
                }))
            });

        div()
            .id(SharedString::from(format!("app-{}", app.bundle_id)))
            .flex()
            .items_center()
            .gap(px(16.))
            .h(px(44.))
            .px(px(14.))
            .border_t_1()
            .border_color(t.border.opacity(0.6))
            .text_size(px(13.5))
            .text_color(t.foreground)
            .hover(|s| s.bg(t.card))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .child(app_tile(&label, px(20.), px(10.), px(5.), cx))
                    .child(div().min_w_0().truncate().child(label.clone()))
                    .when(!app.confirmed, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .px(px(5.))
                                .rounded(metrics::RADIUS_SM)
                                .border_1()
                                .border_color(t.accent.opacity(0.4))
                                .text_size(px(10.))
                                .font_weight(FontWeight::BOLD)
                                .text_color(t.accent)
                                .child(tr!("captures-apps-new").to_uppercase()),
                        )
                    }),
            )
            .child(
                div()
                    .w(px(88.))
                    .flex()
                    .justify_end()
                    .text_size(px(12.5))
                    .text_color(t.muted_foreground)
                    .child(app.count.to_string()),
            )
            .child(div().w(px(180.)).child(menu))
            .into_any_element()
    }

    fn keep_all(&mut self, apps: Vec<AppToConfirm>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(data) = self.data.as_mut() {
            confirmed(data);
        }
        if self.filter == AppFilter::New {
            self.filter = AppFilter::All;
        }
        cx.notify();
        self.call(
            window,
            cx,
            move |client| client.confirm_apps(&apps),
            |page, result, window, cx| match result {
                Ok(data) => {
                    page.data = Some(data);
                    cx.notify();
                }
                Err(error) => {
                    toast_error("writing-style-styles-assign-failed", error, window, cx);
                    page.reload(cx);
                }
            },
        );
    }

    // Moving an app

    fn move_app(&mut self, bundle: &str, to: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(data) = self.data.as_ref() else {
            return;
        };
        let Some(app) = data.apps.iter().find(|app| app.bundle_id == bundle) else {
            return;
        };
        match plan_move(app, to) {
            MovePlan::Stay => {}
            MovePlan::Assign => self.assign(bundle, to, MovedCorrections::Bring, window, cx),
            MovePlan::Ask => {
                let find = |id: &str| {
                    data.styles
                        .iter()
                        .find(|s| s.id == id)
                        .map(|s| s.name.clone())
                        .unwrap_or_default()
                };
                let ask = MoveAsk {
                    bundle: bundle.to_string(),
                    app: app_label(app),
                    from: find(&app.style_id),
                    to_id: to.to_string(),
                    to: find(to),
                    count: app.corrections,
                };
                self.ask_move(ask, window, cx);
            }
        }
    }

    fn ask_move(&mut self, ask: MoveAsk, window: &mut Window, cx: &mut Context<Self>) {
        let page = cx.entity().downgrade();
        open_dialog(window, cx, move |_, cx| {
            let choose = |corrections: MovedCorrections| {
                let page = page.clone();
                let bundle = ask.bundle.clone();
                let to = ask.to_id.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    close_dialog(window, cx);
                    page.update(cx, |page, cx| page.assign(&bundle, &to, corrections, window, cx))
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

    fn assign(
        &mut self,
        bundle: &str,
        to: &str,
        corrections: MovedCorrections,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(data) = self.data.as_mut() else {
            return;
        };
        let name = data
            .apps
            .iter()
            .find(|app| app.bundle_id == bundle)
            .and_then(|app| app.name.clone());
        moved(data, bundle, to);
        cx.notify();
        let (bundle, to) = (bundle.to_string(), to.to_string());
        self.call(
            window,
            cx,
            move |client| client.assign_app_style(&bundle, &to, name.as_deref(), corrections),
            |page, result, window, cx| match result {
                Ok(data) => {
                    page.data = Some(data);
                    page.loaded_for = None;
                    page.load_style(cx);
                    cx.notify();
                }
                Err(error) => {
                    toast_error("writing-style-styles-assign-failed", error, window, cx);
                    page.reload(cx);
                }
            },
        );
    }
}

/// The question asked before an app's corrections move with it.
struct MoveAsk {
    bundle: String,
    app: String,
    from: String,
    to_id: String,
    to: String,
    count: u64,
}

fn render_draft(
    input: &Entity<InputState>,
    _: &mut Window,
    cx: &mut Context<WritingStylePage>,
) -> AnyElement {
    let t = theme(cx).clone();
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w(px(200.))
        .gap_2()
        .p(px(12.))
        .rounded(px(10.))
        .border_1()
        .border_dashed()
        .border_color(t.accent.opacity(0.6))
        .on_action(cx.listener(|page, _: &Escape, _, cx| page.cancel_draft(cx)))
        .child(text_input(input, cx))
        .child(
            div()
                .text_size(px(11.5))
                .text_color(t.muted_foreground)
                .child(tr!("writing-style-styles-new-hint")),
        )
        .into_any_element()
}
