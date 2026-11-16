//! "Applies in": pick one or more places for an entry. Everywhere clears the rest.

use crate::scopes::{ScopeOption, ScopeOptions};
use crate::widgets::{icon, mono_label, scope_icon};
use gpui_kit::assets::IconName;
use gpui_kit::component::Selectable;
use gpui_kit::component::popover::Popover;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_i18n::tr;
use kass_ui::{metrics, theme};
use std::rc::Rc;

/// What a scope is called: its name, or "Everywhere".
pub fn scope_label(option: &ScopeOption) -> String {
    if option.name.is_empty() {
        tr!("dictionary-scope-everywhere")
    } else {
        option.name.clone()
    }
}

pub type Selected = Rc<dyn Fn(&App) -> Vec<String>>;
pub type OnToggle = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// The menu's button and list. `selected` is read on every draw, so the
/// list stays open and current while places are checked.
pub fn places_menu(
    id: impl Into<ElementId>,
    selected: Selected,
    on_toggle: OnToggle,
    options: Rc<ScopeOptions>,
    viewed_from: Option<ScopeOption>,
    cx: &App,
) -> Popover {
    let chosen = selected(cx);
    let label = match chosen.as_slice() {
        [] => TriggerLabel::NoPlaces,
        [key] => match options.find(key) {
            Some(option) => TriggerLabel::One(option.clone()),
            None => TriggerLabel::Count(1),
        },
        keys => TriggerLabel::Count(keys.len()),
    };
    let app_style_key = viewed_from
        .as_ref()
        .filter(|o| matches!(o.scope, crate::scopes::Scope::App(_)))
        .and_then(|o| o.style_id.as_ref())
        .map(|id| format!("style:{id}"));
    let app_name = viewed_from.map(|o| o.name).unwrap_or_default();

    Popover::new(id)
        .w(px(260.))
        .p_1()
        .trigger(PlacesTrigger { label, open: false })
        .content(move |_, _, cx| {
            let t = theme(cx);
            let popover = cx.entity();
            let chosen = selected(cx);
            let item = |option: &ScopeOption, cx: &App| {
                let key = option.key.clone();
                let on_toggle = on_toggle.clone();
                let popover = popover.clone();
                let checked = chosen.contains(&option.key);
                let uses = (Some(&option.key) == app_style_key.as_ref())
                    .then(|| tr!("dictionary-list-app-uses-style", app = app_name.clone()));
                div()
                    .id(SharedString::from(format!("place-{}", option.key)))
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(30.))
                    .px_2()
                    .rounded(metrics::RADIUS_SM)
                    .text_size(metrics::TEXT_BASE)
                    .text_color(t.foreground)
                    .cursor_pointer()
                    .hover(|s| s.bg(t.secondary))
                    .child(div().w(px(14.)).flex_none().when(checked, |d| {
                        d.child(icon(IconName::Check, px(14.), t.foreground))
                    }))
                    .child(scope_icon(&option.scope, &option.name, px(14.), cx))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .child(scope_label(option)),
                    )
                    .when_some(uses, |row, uses| {
                        row.child(
                            div()
                                .flex_none()
                                .pl_3()
                                .text_size(metrics::TEXT_CAPTION)
                                .text_color(t.muted_foreground)
                                .child(uses),
                        )
                    })
                    .on_click(move |_, window, cx| {
                        on_toggle(&key, window, cx);
                        popover.update(cx, |_, cx| cx.notify());
                    })
            };
            let heading =
                |text: String, cx: &App| div().px_2().pt_2().pb_1().child(mono_label(text, cx));

            div()
                .id("places")
                .flex()
                .flex_col()
                .max_h(px(352.))
                .overflow_y_scroll()
                .child(item(&options.everywhere, cx))
                .when(!options.styles.is_empty(), |list| {
                    list.child(heading(tr!("dictionary-scope-styles"), cx))
                })
                .children(options.styles.iter().map(|o| item(o, cx)))
                .when(!options.apps.is_empty(), |list| {
                    list.child(heading(tr!("dictionary-scope-apps"), cx))
                })
                .children(options.apps.iter().map(|o| item(o, cx)))
                .child(div().my_1().h(px(1.)).bg(t.border))
                .child(
                    div()
                        .px_2()
                        .py_1p5()
                        .text_size(metrics::TEXT_CAPTION)
                        .text_color(t.muted_foreground)
                        .child(tr!("dictionary-list-everywhere-clears")),
                )
        })
}

enum TriggerLabel {
    One(ScopeOption),
    NoPlaces,
    Count(usize),
}

#[derive(IntoElement)]
struct PlacesTrigger {
    label: TriggerLabel,
    open: bool,
}

impl Selectable for PlacesTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl RenderOnce for PlacesTrigger {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx);
        let label = match self.label {
            TriggerLabel::One(option) => div()
                .flex()
                .min_w_0()
                .items_center()
                .gap_1p5()
                .child(scope_icon(&option.scope, &option.name, px(14.), cx))
                .child(div().min_w_0().truncate().child(scope_label(&option))),
            TriggerLabel::NoPlaces => div()
                .text_color(t.muted_foreground)
                .child(tr!("dictionary-list-no-places")),
            TriggerLabel::Count(count) => div().child(tr!("dictionary-list-places", count = count)),
        };
        div()
            .id("places-trigger")
            .flex()
            .items_center()
            .gap_1p5()
            .h(px(28.))
            .max_w(px(220.))
            .px_2()
            .rounded(metrics::RADIUS_MD)
            .border_1()
            .border_color(t.input)
            .text_size(metrics::TEXT_BASE)
            .text_color(t.foreground)
            .cursor_pointer()
            .hover(|s| s.bg(t.secondary))
            .when(self.open, |b| b.bg(t.secondary))
            .child(label)
            .child(icon(IconName::ChevronDown, px(14.), t.muted_foreground))
    }
}
