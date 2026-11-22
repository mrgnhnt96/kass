//! Adding a word picked from a capture to the dictionary: selecting a word
//! or phrase while correcting offers it, the user types how it is spelled
//! and where it applies, and the correction saves with it spelled that way.

use crate::page::CapturesPage;
use crate::phrases::{Picked, respelled_fix, spelling_entry};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::DictionaryPlaceInput;
use kass_dictionary::scopes::{
    EVERYWHERE_KEY, ScopeOptions, build_scope_options, places_from_keys, toggle_place,
};
use kass_dictionary::{OnToggle, Selected, places_menu};
use kass_i18n::tr;
use kass_ui::{
    Button, Toast, close_dialog, dialog_footer, dialog_header, metrics, open_dialog, show_toast,
    text_input, theme,
};
use std::cell::RefCell;
use std::rc::Rc;

type Submit = Rc<dyn Fn(&mut Window, &mut App)>;

/// The "Applies in" choice in a dialog: everywhere unless the user picks.
#[derive(Clone)]
pub(crate) struct Places {
    keys: Rc<RefCell<Vec<String>>>,
    options: Rc<ScopeOptions>,
}

impl Places {
    pub fn new(page: &CapturesPage) -> Self {
        Self {
            keys: Rc::new(RefCell::new(vec![EVERYWHERE_KEY.to_string()])),
            options: Rc::new(build_scope_options(page.styles.as_ref(), &page.dictionary)),
        }
    }

    /// No place checked, which can't be saved.
    pub fn is_empty(&self) -> bool {
        self.keys.borrow().is_empty()
    }

    pub fn chosen(&self) -> Vec<DictionaryPlaceInput> {
        places_from_keys(&self.keys.borrow(), &self.options)
    }

    /// "Applies in" and the places menu.
    pub fn render(&self, id: &'static str, cx: &App) -> Div {
        let keys = self.keys.clone();
        let selected: Selected = Rc::new(move |_| keys.borrow().clone());
        let keys = self.keys.clone();
        let on_toggle: OnToggle = Rc::new(move |key, window, _| {
            let next = toggle_place(&keys.borrow(), key);
            *keys.borrow_mut() = next;
            window.refresh();
        });
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_size(metrics::TEXT_XS)
                    .text_color(theme(cx).muted_foreground)
                    .child(tr!("dictionary-list-applies-in")),
            )
            .child(places_menu(
                id,
                selected,
                on_toggle,
                self.options.clone(),
                None,
                cx,
            ))
    }
}

impl CapturesPage {
    /// Asks how a picked word is spelled, and where, then adds it.
    pub(crate) fn pick_word(
        &mut self,
        picked: Picked,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(capture_id) = self.detail.as_ref().map(|d| d.capture.id.clone()) else {
            return;
        };
        let spelling =
            cx.new(|cx| InputState::new(window, cx).default_value(picked.phrase.clone()));
        let places = Places::new(self);
        let page = cx.entity().downgrade();
        let submit: Submit = Rc::new({
            let (spelling, places) = (spelling.clone(), places.clone());
            move |window, cx| {
                let written = spelling.read(cx).value().trim().to_string();
                if written.is_empty() || places.is_empty() {
                    return;
                }
                let chosen = places.chosen();
                close_dialog(window, cx);
                let (picked, capture_id) = (picked.clone(), capture_id.clone());
                page.update(cx, |page, cx| {
                    page.add_spelling(capture_id, picked, written, chosen, window, cx)
                })
                .ok();
            }
        });
        cx.subscribe_in(&spelling, window, {
            let submit = submit.clone();
            move |_, _, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    submit(window, cx);
                }
            }
        })
        .detach();

        let field = spelling.clone();
        open_dialog(window, cx, move |_, cx| {
            let t = theme(cx).clone();
            let ready = !spelling.read(cx).value().trim().is_empty() && !places.is_empty();
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(dialog_header(
                    tr!("captures-dictionary-title"),
                    Some(tr!("captures-dictionary-description").into()),
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(
                            div()
                                .text_size(metrics::TEXT_XS)
                                .text_color(t.muted_foreground)
                                .child(tr!("captures-dictionary-spelling")),
                        )
                        .child(text_input(&spelling, cx)),
                )
                .child(places.render("spelling-places", cx))
                .child(
                    dialog_footer()
                        .child(
                            Button::new("spelling-cancel")
                                .outline()
                                .label(tr!("common-cancel"))
                                .on_click(|_, window, cx| close_dialog(window, cx)),
                        )
                        .child(
                            Button::new("spelling-add")
                                .label(tr!("captures-dictionary-submit"))
                                .disabled(!ready)
                                .on_click({
                                    let submit = submit.clone();
                                    move |_, window, cx| submit(window, cx)
                                }),
                        ),
                )
                .into_any_element()
        });
        field.update(cx, |state, cx| {
            state.focus(window, cx);
            state.select_all(window, cx);
        });
    }

    fn add_spelling(
        &mut self,
        capture_id: String,
        picked: Picked,
        written: String,
        places: Vec<DictionaryPlaceInput>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.adding_entry {
            return;
        }
        self.adding_entry = true;
        let body = spelling_entry(&picked.phrase, &written, places);
        let written = body.written.clone();
        cx.notify();
        self.call(
            window,
            cx,
            move |client| client.create_dictionary_entry(&body),
            move |page, result, window, cx| {
                page.adding_entry = false;
                match result {
                    Ok(entry) => {
                        page.dictionary.push(entry);
                        show_toast(
                            window,
                            cx,
                            Toast::new(tr!("captures-dictionary-added", written = written.clone())),
                        );
                        page.save_respelled(&capture_id, &picked, &written, window, cx);
                    }
                    Err(error) => {
                        Self::toast_error(tr!("captures-dictionary-add-failed"), error, window, cx)
                    }
                }
                cx.notify();
            },
        );
    }

    /// Saves the correction with the picked word spelled the way it was
    /// added: the edit when one is open, else amending the saved one.
    fn save_respelled(
        &mut self,
        capture_id: &str,
        picked: &Picked,
        written: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(detail) = &mut self.detail else {
            return;
        };
        if detail.capture.id != capture_id || detail.teach.saving {
            return;
        }
        let current = detail.teach.current(&detail.reports);
        let Some(text) = respelled_fix(&current, picked, written) else {
            return;
        };
        let Some(submission) = detail.teach.save_text(&text, &detail.reports) else {
            return;
        };
        // The edit box shows the fix while it saves.
        detail
            .editor
            .update(cx, |state, cx| state.set_value(text, window, cx));
        self.submit(submission, window, cx);
    }
}
