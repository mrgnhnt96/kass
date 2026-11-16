//! Settings › Command mode: the shortcut that rewrites the selected text,
//! and the saved transforms you run by name.

use std::rc::Rc;

use gpui_kit::component::input::{InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{CaptureSettingsUpdate, Transform};
use kass_ui::{
    close_dialog, dialog_footer, dialog_header, metrics, open_dialog, show_toast, text_input,
    theme, Button, SettingRow, SettingSection, Toast, ToastKind,
};

use super::chord::{chord_keys, default_chord, open_chord_picker, same_chord};
use super::toast_error;
use crate::shell::data::ShellData;

const P: &str = "settings-command-mode";
const MAX_NAME: usize = 60;
const MAX_INSTRUCTION: usize = 2000;

fn t(id: impl AsRef<str>) -> String {
    kass_i18n::t(id.as_ref())
}

/// Whether `keys` would start dictation instead of Command mode.
pub fn chord_taken(keys: &[String], push_to_talk: &[String], toggle: &[String]) -> bool {
    same_chord(keys, push_to_talk) || same_chord(keys, toggle)
}

/// The transforms with the one at `index` replaced, or `transform` added.
pub fn upsert(transforms: &[Transform], index: Option<usize>, transform: Transform) -> Vec<Transform> {
    let mut next = transforms.to_vec();
    match index.and_then(|i| next.get_mut(i)) {
        Some(slot) => *slot = transform,
        None => next.push(transform),
    }
    next
}

pub struct CommandModePage {
    data: Entity<ShellData>,
    _subscriptions: Vec<Subscription>,
}

impl CommandModePage {
    pub fn new(data: Entity<ShellData>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![cx.observe_in(&data, window, |_, _, _, cx| cx.notify())];
        Self {
            data,
            _subscriptions: subscriptions,
        }
    }

    fn transforms(&self, cx: &App) -> Vec<Transform> {
        self.data
            .read(cx)
            .settings
            .as_ref()
            .map(|s| s.command_transforms.clone())
            .unwrap_or_default()
    }

    /// Save `patch`; `done` runs once it's saved, and a failure says why
    /// under `failed`.
    fn save(
        &mut self,
        patch: CaptureSettingsUpdate,
        failed: Option<&'static str>,
        done: impl FnOnce(&mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self.data.update(cx, |data, cx| data.update_settings(patch, cx));
        cx.spawn_in(window, async move |_, cx| {
            let result = task.await;
            let _ = cx.update(|window, cx| match result {
                Ok(()) => done(window, cx),
                Err(e) => match failed {
                    Some(title) => toast_error(t(title), &e, window, cx),
                    None => eprintln!("[settings] save failed: {e}"),
                },
            });
        })
        .detach();
    }

    fn save_transforms(
        &mut self,
        transforms: Vec<Transform>,
        done: impl FnOnce(&mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.save(
            CaptureSettingsUpdate {
                command_transforms: Some(transforms),
                ..Default::default()
            },
            Some("settings-command-mode-transforms-save-failed"),
            done,
            window,
            cx,
        );
    }

    fn set_chord(&mut self, keys: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let settings = self.data.read(cx).settings.as_ref();
        let (ptt, toggle) = settings.map_or_else(
            || (default_chord(false), default_chord(true)),
            |s| (s.chord_push_to_talk_keys.clone(), s.chord_toggle_to_talk_keys.clone()),
        );
        if !keys.is_empty() && chord_taken(&keys, &ptt, &toggle) {
            show_toast(
                window,
                cx,
                Toast::new(t(format!("{P}-chord-taken"))).kind(ToastKind::Destructive),
            );
            return;
        }
        self.save(
            CaptureSettingsUpdate {
                chord_command_keys: Some(keys),
                ..Default::default()
            },
            None,
            |_, _| {},
            window,
            cx,
        );
    }

    fn change_chord(&mut self, keys: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        open_chord_picker(
            t(format!("{P}-chord-picker-title")),
            t(format!("{P}-chord-picker-description")),
            keys,
            move |keys, window, cx| {
                let _ = this.update(cx, |this, cx| this.set_chord(keys, window, cx));
            },
            window,
            cx,
        );
    }

    /// Open the transform at `index` to edit, or a blank one to add.
    fn edit(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let existing = index.and_then(|i| self.transforms(cx).get(i).cloned());
        let page = cx.entity().downgrade();
        let on_save: OnSave = Rc::new(move |transform, window, cx| {
            let _ = page.update(cx, |page, cx| {
                let next = upsert(&page.transforms(cx), index, transform);
                page.save_transforms(next, close_dialog, window, cx);
            });
        });
        let editor = cx.new(|cx| TransformEditor::new(existing, on_save, window, cx));
        open_dialog(window, cx, move |_, _| editor.clone().into_any_element());
    }

    fn delete(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let mut next = self.transforms(cx);
        if index < next.len() {
            next.remove(index);
            self.save_transforms(next, |_, _| {}, window, cx);
        }
    }

    fn render_chord(&self, cx: &mut Context<Self>) -> SettingRow {
        let (hotkey, keys) = {
            let s = self.data.read(cx).settings.as_ref();
            (
                s.is_some_and(|s| s.hotkey_enabled),
                s.map(|s| s.chord_command_keys.clone()).unwrap_or_default(),
            )
        };
        let t_ = theme(cx);
        let description = if hotkey {
            t(format!("{P}-chord-description"))
        } else {
            t(format!("{P}-chord-needs-shortcut"))
        };
        let set = !keys.is_empty();
        let shown = if set {
            chord_keys(&keys, false, cx)
        } else {
            div()
                .text_size(metrics::TEXT_XS)
                .text_color(t_.muted_foreground)
                .child(t(format!("{P}-chord-off")))
                .into_any_element()
        };
        let turn_off = cx.entity().downgrade();
        let change = cx.entity().downgrade();
        SettingRow::new(t(format!("{P}-chord-title")))
            .description(description)
            .action(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(shown)
                    .when(set, |row| {
                        row.child(
                            Button::new("command-chord-off")
                                .ghost()
                                .small()
                                .label(t(format!("{P}-chord-turn-off")))
                                .on_click(move |_, window, cx| {
                                    let _ = turn_off
                                        .update(cx, |this, cx| this.set_chord(Vec::new(), window, cx));
                                }),
                        )
                    })
                    .child(
                        Button::new("command-chord-change")
                            .outline()
                            .small()
                            .ml(px(6.))
                            .disabled(!hotkey)
                            .label(if set {
                                t(format!("{P}-chord-change"))
                            } else {
                                t(format!("{P}-chord-set"))
                            })
                            .on_click(move |_, window, cx| {
                                let keys = keys.clone();
                                let _ = change
                                    .update(cx, |this, cx| this.change_chord(keys, window, cx));
                            }),
                    ),
            )
    }

    fn render_transform(&self, index: usize, transform: &Transform, cx: &mut Context<Self>) -> Div {
        let t_ = theme(cx);
        let edit = cx.entity().downgrade();
        let delete = cx.entity().downgrade();
        div()
            .flex()
            .items_start()
            .justify_between()
            .gap(px(32.))
            .py(px(14.))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(metrics::TEXT_SM)
                            .line_height(metrics::TEXT_SM)
                            .text_color(t_.foreground)
                            .child(transform.name.clone()),
                    )
                    .child(
                        div()
                            .mt(px(6.))
                            .text_size(metrics::TEXT_XS)
                            .line_height(px(19.5))
                            .line_clamp(2)
                            .text_color(t_.muted_foreground)
                            .child(transform.instruction.clone()),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        Button::new(("transform-edit", index))
                            .outline()
                            .small()
                            .label(t(format!("{P}-transforms-edit")))
                            .on_click(move |_, window, cx| {
                                let _ = edit.update(cx, |this, cx| this.edit(Some(index), window, cx));
                            }),
                    )
                    .child(
                        Button::new(("transform-delete", index))
                            .ghost()
                            .small()
                            .label(t(format!("{P}-transforms-delete")))
                            .on_click(move |_, window, cx| {
                                let _ = delete.update(cx, |this, cx| this.delete(index, window, cx));
                            }),
                    ),
            )
    }
}

impl Render for CommandModePage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let transforms = self.transforms(cx);
        let add = cx.entity().downgrade();
        let shortcut = SettingSection::new(t(format!("{P}-section-shortcut")))
            .description(t(format!("{P}-description")))
            .child(self.render_chord(cx));

        let mut list = SettingSection::new(t(format!("{P}-transforms-title")))
            .description(t(format!("{P}-transforms-description")));
        for (index, transform) in transforms.iter().enumerate() {
            list = list.child(self.render_transform(index, transform, cx));
        }
        let t_ = theme(cx);
        if transforms.is_empty() {
            list = list.child(
                div()
                    .py(px(14.))
                    .text_size(metrics::TEXT_XS)
                    .text_color(t_.muted_foreground)
                    .child(t(format!("{P}-transforms-empty"))),
            );
        }
        let list = list.child(
            div().py(px(14.)).child(
                Button::new("transform-add")
                    .outline()
                    .small()
                    .label(t(format!("{P}-transforms-add")))
                    .on_click(move |_, window, cx| {
                        let _ = add.update(cx, |this, cx| this.edit(None, window, cx));
                    }),
            ),
        );

        div().flex().flex_col().child(shortcut).child(list)
    }
}

type OnSave = Rc<dyn Fn(Transform, &mut Window, &mut App)>;

/// The add/edit dialog's body: a name and an instruction.
struct TransformEditor {
    id: Option<String>,
    editing: bool,
    name: Entity<InputState>,
    instruction: Entity<TextareaState>,
    on_save: OnSave,
    _subscriptions: Vec<Subscription>,
}

impl TransformEditor {
    fn new(
        existing: Option<Transform>,
        on_save: OnSave,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t(format!("{P}-dialog-name-placeholder")))
                .validate(|text, _| text.chars().count() <= MAX_NAME)
        });
        let instruction = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(5)
                .placeholder(t(format!("{P}-dialog-instruction-placeholder")))
        });
        if let Some(existing) = &existing {
            name.update(cx, |state, cx| state.set_value(existing.name.clone(), window, cx));
            instruction.update(cx, |state, cx| {
                state.set_value(existing.instruction.clone(), window, cx)
            });
        }
        let subscriptions = vec![
            cx.subscribe_in(&name, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe_in(&instruction, window, |_, state, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = state.read(cx).value();
                    if let Some((at, _)) = text.char_indices().nth(MAX_INSTRUCTION) {
                        let clipped = text[..at].to_string();
                        state.update(cx, |state, cx| state.set_value(clipped, window, cx));
                    }
                    cx.notify();
                }
            }),
        ];
        name.update(cx, |state, cx| state.focus(window, cx));
        Self {
            editing: existing.as_ref().is_some_and(|e| !e.name.is_empty()),
            id: existing.and_then(|e| e.id),
            name,
            instruction,
            on_save,
            _subscriptions: subscriptions,
        }
    }

    /// The transform as typed, once both fields have something in them.
    fn transform(&self, cx: &App) -> Option<Transform> {
        let name = self.name.read(cx).value().trim().to_string();
        let instruction = self.instruction.read(cx).value().trim().to_string();
        (!name.is_empty() && !instruction.is_empty()).then(|| Transform {
            id: self.id.clone(),
            name,
            instruction,
        })
    }
}

impl Render for TransformEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t_ = theme(cx);
        let ready = self.transform(cx);
        let this = cx.entity().downgrade();
        let title = if self.editing {
            t(format!("{P}-dialog-edit-title"))
        } else {
            t(format!("{P}-dialog-add-title"))
        };
        let field = |label: String, control: AnyElement| {
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .text_size(metrics::TEXT_SM)
                .text_color(t_.foreground)
                .child(label)
                .child(control)
        };
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(dialog_header(title, Some(t(format!("{P}-dialog-description")).into()), cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(field(
                        t(format!("{P}-dialog-name")),
                        text_input(&self.name, cx).into_any_element(),
                    ))
                    .child(field(
                        t(format!("{P}-dialog-instruction")),
                        Textarea::new(&self.instruction)
                            .w_full()
                            .px_3()
                            .py(px(10.))
                            .rounded(metrics::RADIUS_LG)
                            .bg(t_.background)
                            .text_size(metrics::TEXT_BASE)
                            .into_any_element(),
                    )),
            )
            .child(
                dialog_footer()
                    .child(
                        Button::new("transform-cancel")
                            .ghost()
                            .label(t("common-cancel"))
                            .on_click(|_, window, cx| close_dialog(window, cx)),
                    )
                    .child(
                        Button::new("transform-save")
                            .label(t("common-save"))
                            .disabled(ready.is_none())
                            .on_click(move |_, window, cx| {
                                let Some(this) = this.upgrade() else { return };
                                let editor = this.read(cx);
                                if let Some(transform) = editor.transform(cx) {
                                    let on_save = editor.on_save.clone();
                                    on_save(transform, window, cx);
                                }
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(names: &[&str]) -> Vec<String> {
        names.iter().map(|k| k.to_string()).collect()
    }

    fn transform(name: &str) -> Transform {
        Transform {
            id: Some(name.to_lowercase()),
            name: name.into(),
            instruction: format!("{name} it"),
        }
    }

    #[test]
    fn a_dictation_chord_is_taken() {
        let ptt = default_chord(false);
        let toggle = default_chord(true);
        assert!(chord_taken(&keys(&["AltGr", "MetaRight"]), &ptt, &toggle));
        assert!(chord_taken(&keys(&["Space", "AltGr", "MetaRight"]), &ptt, &toggle));
        assert!(!chord_taken(&keys(&["ControlRight", "AltGr"]), &ptt, &toggle));
    }

    #[test]
    fn upsert_replaces_or_adds() {
        let list = vec![transform("Shorten"), transform("Polish")];
        let edited = upsert(&list, Some(1), transform("Tighten"));
        assert_eq!(edited[1].name, "Tighten");
        assert_eq!(edited.len(), 2);
        let added = upsert(&list, None, transform("Translate"));
        assert_eq!(added.len(), 3);
        assert_eq!(added[2].name, "Translate");
    }
}
