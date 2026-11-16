//! How the Models page draws: the list, the detail pane and its actions,
//! problems, the models folder, and the overlay while it moves.

use super::stats::{compare, stats};
use super::{Migration, ModelsPage};
use crate::catalog::{self, format_bytes, format_size_mb};
use crate::downloads::{DownloadState, percent};
use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{ModelStatus, ProgressStatus};
use kass_i18n::{parse_markup, t, tr};
use kass_ui::{
    Button, KassTheme, close_dialog, dialog_footer, dialog_header, metrics, open_dialog, theme,
};
use std::path::PathBuf;
use std::time::Duration;

fn spinner(id: impl Into<ElementId>, size: Pixels, color: Hsla) -> impl IntoElement {
    svg()
        .path(IconName::LoaderCircle.path())
        .size(size)
        .flex_none()
        .text_color(color)
        .with_animation(
            id,
            Animation::new(Duration::from_millis(900)).repeat(),
            |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))),
        )
}

/// A thin bar filled to `fraction` (0 to 1).
fn bar(fraction: f32, height: Pixels, t_: &KassTheme) -> Div {
    div()
        .w_full()
        .h(height)
        .rounded_full()
        .bg(t_.muted)
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .w(relative(fraction.clamp(0., 1.)))
                .rounded_full()
                .bg(t_.accent),
        )
}

fn badge(text: String, color: Hsla) -> Div {
    div()
        .flex_none()
        .px(px(8.))
        .py(px(2.))
        .rounded_full()
        .bg(color.opacity(0.12))
        .text_color(color)
        .text_size(metrics::TEXT_CAPTION)
        .font_weight(FontWeight::MEDIUM)
        .child(text)
}

fn mono(t_: &KassTheme) -> Div {
    div()
        .font_family(t_.mono_font.clone())
        .text_size(metrics::TEXT_CAPTION)
}

/// Storage buttons are small and monospace.
fn storage_button(id: &'static str, label: String, t_: &KassTheme) -> Button {
    Button::new(id)
        .outline()
        .small()
        .label(label)
        .h(px(24.))
        .px(px(8.))
        .font_family(t_.mono_font.clone())
        .text_size(metrics::TEXT_CAPTION)
        .font_weight(FontWeight::NORMAL)
}

impl ModelsPage {
    fn list(&self, cx: &mut Context<Self>) -> AnyElement {
        let t_ = theme(cx).clone();
        let Some(models) = &self.models else {
            return div()
                .flex()
                .justify_center()
                .py(px(32.))
                .child(spinner("models-loading", px(20.), t_.muted_foreground))
                .into_any_element();
        };
        let current = self.current().map(|model| model.model_name.clone());
        let mut rows: Vec<AnyElement> = Vec::new();
        for group in catalog::groups(models) {
            let key = format!("models-groups-{}", group.family);
            let heading = if kass_i18n::has(&key) {
                t(&key)
            } else {
                group.family.clone()
            };
            rows.push(
                mono(&t_)
                    .px_4()
                    .pt(px(14.))
                    .pb(px(6.))
                    .text_color(t_.muted_foreground)
                    .child(heading.to_uppercase())
                    .into_any_element(),
            );
            for model in &group.models {
                let selected = current.as_deref() == Some(model.model_name.as_str());
                rows.push(self.row(model, selected, &t_, cx));
            }
        }
        div()
            .id("models-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pb_2()
            .children(rows)
            .into_any_element()
    }

    fn row(
        &self,
        model: &ModelStatus,
        selected: bool,
        t_: &KassTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = self.downloads.state_of(model);
        let name = model.model_name.clone();
        let dot = div().size(px(8.)).flex_none().rounded_full();
        let dot = if state.error.is_some() {
            dot.bg(t_.destructive)
        } else if state.downloading || model.loaded {
            dot.bg(t_.accent)
        } else if model.downloaded {
            dot.bg(t_.success)
        } else {
            dot.border_1().border_color(t_.input)
        };
        let pct = percent(state.progress.as_ref());

        let (status, color, action) = if state.error.is_some() {
            (t("models-row-retry"), t_.destructive, true)
        } else if state.downloading {
            let text = pct.map_or_else(|| t("models-row-connecting"), |p| format!("{p:.0}%"));
            (text, t_.accent, false)
        } else if model.loaded {
            (t("models-row-loaded"), t_.accent, false)
        } else if model.downloaded {
            (t("models-row-downloaded"), t_.success, false)
        } else {
            (t("models-row-get"), t_.muted_foreground, true)
        };
        let status = mono(t_)
            .id(SharedString::from(format!("model-get-{name}")))
            .flex_none()
            .pr_4()
            .pl_3()
            .text_color(color)
            .child(status)
            .when(action, |status| {
                let name = name.clone();
                status
                    .cursor_pointer()
                    .hover(|s| s.underline())
                    .on_click(cx.listener(move |page, _, window, cx| {
                        cx.stop_propagation();
                        page.download(name.clone(), window, cx);
                    }))
            });

        div()
            .id(SharedString::from(format!("model-row-{name}")))
            .relative()
            .flex()
            .items_center()
            .gap_3()
            .pl_4()
            .py(px(10.))
            .cursor_pointer()
            .when(selected, |row| row.bg(t_.muted))
            .when(!selected, |row| {
                let hover = t_.muted.opacity(0.5);
                row.hover(move |s| s.bg(hover))
            })
            .on_click(cx.listener({
                let name = name.clone();
                move |page, _, _, cx| page.select(name.clone(), cx)
            }))
            .when(selected, |row| {
                row.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(2.))
                        .bg(t_.accent),
                )
            })
            .child(dot)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(5.))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .truncate()
                                    .child(model.display_name.clone()),
                            )
                            .when_some(model.size_mb.filter(|_| model.downloaded), |line, size| {
                                line.child(
                                    mono(t_)
                                        .flex_none()
                                        .text_color(t_.muted_foreground)
                                        .child(format_size_mb(size)),
                                )
                            }),
                    )
                    .when(state.downloading, |col| {
                        col.child(bar(pct.unwrap_or(0.) as f32 / 100., px(3.), t_))
                    }),
            )
            .child(status)
            .into_any_element()
    }

    fn storage(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.host.manages_storage(cx) {
            return None;
        }
        let dir = self.cache_dir.clone()?;
        let t_ = theme(cx).clone();
        let migrating = self.migration.is_some();
        let custom = self.host.custom_models_dir(cx).is_some();
        let move_label = if migrating {
            t("models-storage-migrating")
        } else {
            t("models-storage-move")
        };
        Some(
            mono(&t_)
                .mt(px(8.))
                .flex()
                .items_center()
                .gap_2()
                .text_color(t_.muted_foreground)
                .child(div().flex_1().min_w_0().truncate().child(dir.clone()))
                .child(
                    storage_button("models-storage-open", t("models-storage-open"), &t_).on_click(
                        cx.listener(move |page, _, window, cx| page.open_folder(&dir, window, cx)),
                    ),
                )
                .child(
                    storage_button("models-storage-move", move_label, &t_)
                        .loading(migrating)
                        .on_click(cx.listener(|page, _, window, cx| page.pick_folder(window, cx))),
                )
                .when(custom, |row| {
                    row.child(
                        storage_button("models-storage-reset", t("models-storage-reset"), &t_)
                            .ghost()
                            .disabled(migrating)
                            .on_click(
                                cx.listener(|page, _, window, cx| page.reset_folder(window, cx)),
                            ),
                    )
                })
                .into_any_element(),
        )
    }

    fn problems(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let errors = self.downloads.errors();
        if errors.is_empty() {
            return None;
        }
        let t_ = theme(cx).clone();
        let lines = errors.iter().map(|task| {
            let error = task
                .error
                .clone()
                .unwrap_or_else(|| t("models-problems-no-details"));
            div()
                .child(
                    div()
                        .text_color(t_.foreground)
                        .child(format!("{}: {error}", self.display_name(&task.model_name))),
                )
                .child(div().text_color(t_.muted_foreground).child(tr!(
                    "models-problems-started-at",
                    time = catalog::local_time(&task.started_at)
                )))
        });
        Some(
            div()
                .mx_4()
                .mt_3()
                .mb_4()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .rounded(metrics::RADIUS_MD)
                .border_1()
                .border_color(t_.destructive.opacity(0.3))
                .bg(t_.destructive.opacity(0.06))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(metrics::TEXT_XS)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(t_.destructive)
                                .child(tr!("models-problems-count", count = errors.len())),
                        )
                        .child(
                            Button::new("models-problems-clear")
                                .ghost()
                                .small()
                                .h(px(24.))
                                .px(px(8.))
                                .text_size(metrics::TEXT_CAPTION)
                                .label(t("models-problems-clear-all"))
                                .loading(self.busy.clearing)
                                .on_click(cx.listener(|page, _, window, cx| {
                                    page.clear_problems(window, cx)
                                })),
                        ),
                )
                .child(
                    mono(&t_)
                        .id("models-problems")
                        .max_h(px(160.))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(lines),
                )
                .into_any_element(),
        )
    }

    fn detail(&self, model: &ModelStatus, cx: &mut Context<Self>) -> AnyElement {
        let t_ = theme(cx).clone();
        let state = self.downloads.state_of(model);
        let in_use = self.in_use().get(&model.model_name).copied();
        let family = catalog::family(&model.model_name).to_string();
        let purpose_key = format!("models-purpose-{family}");
        let siblings: Vec<ModelStatus> = self
            .models()
            .iter()
            .filter(|other| catalog::family(&other.model_name) == family)
            .cloned()
            .collect();
        let info = model
            .hf_repo_id
            .as_ref()
            .and_then(|repo| self.info.get(repo));

        let header = div()
            .flex_none()
            .px(px(32.))
            .pt(px(28.))
            .pb(px(22.))
            .border_b_1()
            .border_color(t_.border)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .mr_1()
                            .text_size(px(24.))
                            .line_height(px(32.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(model.display_name.clone()),
                    )
                    .when(model.loaded, |row| {
                        row.child(badge(t("models-status-loaded"), t_.accent))
                    })
                    .when_some(in_use, |row, role| {
                        row.child(badge(
                            tr!("models-tags-in-use", role = role.label()),
                            t_.success,
                        ))
                    })
                    .when(state.error.is_some(), |row| {
                        row.child(badge(t("common-error"), t_.destructive))
                    }),
            )
            .child(
                div()
                    .mt(px(6.))
                    .flex()
                    .flex_wrap()
                    .gap_x_2()
                    .text_size(metrics::TEXT_BASE)
                    .text_color(t_.muted_foreground)
                    .when(kass_i18n::has(&purpose_key), |line| {
                        line.child(t(&purpose_key))
                    })
                    .when_some(model.hf_repo_id.clone(), |line, repo| {
                        let url = format!("https://huggingface.co/{repo}");
                        line.child(
                            div()
                                .id("models-hf-link")
                                .text_color(t_.accent)
                                .cursor_pointer()
                                .hover(|s| s.underline())
                                .child(tr!("models-detail-hf-link", repo = repo))
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                    }),
            );

        let body = div()
            .id(SharedString::from(format!(
                "model-detail-{}",
                model.model_name
            )))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(32.))
            .py(px(24.))
            .flex()
            .flex_col()
            .gap(px(24.))
            .when_some(catalog::description(&model.model_name), |body, text| {
                body.child(
                    div()
                        .max_w(px(640.))
                        .text_size(metrics::TEXT_SM)
                        .line_height(px(22.))
                        .text_color(t_.foreground.opacity(0.85))
                        .child(text),
                )
            })
            .when(state.downloading, |body| {
                body.child(download_progress(&state, &t_))
            })
            .when_some(state.error.as_ref(), |body, error| {
                body.child(
                    mono(&t_)
                        .p_3()
                        .rounded(metrics::RADIUS_MD)
                        .border_1()
                        .border_color(t_.destructive.opacity(0.3))
                        .bg(t_.destructive.opacity(0.06))
                        .text_color(t_.destructive)
                        .child(
                            error
                                .error
                                .clone()
                                .unwrap_or_else(|| t("models-problems-no-details")),
                        ),
                )
            })
            .child(stats(model, info, &t_))
            .when(siblings.len() > 1, |body| {
                body.child(compare(&siblings, &model.model_name, &self.downloads, &t_))
            });

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(header)
            .child(body)
            .child(self.actions(model, &state, in_use.is_some(), &t_, cx))
            .into_any_element()
    }

    fn actions(
        &self,
        model: &ModelStatus,
        state: &DownloadState,
        in_use: bool,
        t_: &KassTheme,
        cx: &mut Context<Self>,
    ) -> Div {
        let name = model.model_name.clone();
        let bar = div()
            .flex_none()
            .h(px(64.))
            .px(px(32.))
            .flex()
            .items_center()
            .gap_2()
            .border_t_1()
            .border_color(t_.border);
        let on = |f: fn(&mut ModelsPage, String, &mut Window, &mut Context<ModelsPage>)| {
            let name = name.clone();
            cx.listener(move |page: &mut ModelsPage, _: &ClickEvent, window, cx| {
                f(page, name.clone(), window, cx)
            })
        };

        if state.error.is_some() {
            return bar
                .child(
                    Button::new("model-retry")
                        .label(t("models-actions-retry"))
                        .on_click(on(ModelsPage::download)),
                )
                .child(
                    Button::new("model-dismiss")
                        .ghost()
                        .label(t("models-actions-dismiss"))
                        .loading(self.busy.cancelling.as_ref() == Some(&name))
                        .on_click(on(ModelsPage::cancel)),
                );
        }
        if state.downloading {
            return bar.child(
                Button::new("model-cancel")
                    .outline()
                    .label(t("models-actions-cancel-download"))
                    .loading(self.busy.cancelling.as_ref() == Some(&name))
                    .on_click(on(ModelsPage::cancel)),
            );
        }
        if !model.downloaded {
            return bar.child(
                Button::new("model-download")
                    .label(t("models-actions-download"))
                    .on_click(on(ModelsPage::download)),
            );
        }

        let role = catalog::role_of(&name);
        let unloading = self.busy.unloading.as_ref() == Some(&name);
        let folder = self
            .cache_dir
            .as_ref()
            .zip(model.hf_repo_id.as_ref())
            .map(|(dir, repo)| catalog::hf_cache_folder(dir, repo));
        bar.when_some(role.filter(|_| !in_use), |bar, role| {
            bar.child(
                Button::new("model-use")
                    .label(tr!(
                        "models-actions-use",
                        role = role.label().to_lowercase()
                    ))
                    .loading(self.busy.using.as_ref() == Some(&name))
                    .on_click(on(ModelsPage::use_model)),
            )
        })
        .when(model.loaded, |bar| {
            bar.child(
                Button::new("model-unload")
                    .secondary()
                    .label(if unloading {
                        t("models-actions-unloading")
                    } else {
                        t("models-actions-unload-from-memory")
                    })
                    .loading(unloading)
                    .on_click(on(ModelsPage::unload)),
            )
        })
        .when_some(folder, |bar, folder| {
            bar.child(
                Button::new("model-finder")
                    .outline()
                    .label(t("models-actions-show-in-finder"))
                    .on_click(cx.listener(move |page, _, window, cx| {
                        page.open_folder(&folder, window, cx)
                    })),
            )
        })
        .child(div().flex_1())
        .child({
            let delete = Button::new("model-delete")
                .ghost()
                .text_color(t_.destructive)
                .label(t("models-actions-delete-model"))
                .disabled(model.loaded);
            if model.loaded {
                delete.tooltip(t("models-actions-unload-first"))
            } else {
                let model = model.clone();
                delete.on_click(cx.listener(move |page, _, window, cx| {
                    page.confirm_delete(model.clone(), window, cx)
                }))
            }
        })
    }

    fn confirm_delete(&mut self, model: ModelStatus, window: &mut Window, cx: &mut Context<Self>) {
        let page = cx.entity().downgrade();
        open_dialog(window, cx, move |_, cx| {
            let t_ = theme(cx).clone();
            let deleting = page.upgrade().is_some_and(|page| {
                page.read(cx).busy.deleting.as_ref() == Some(&model.model_name)
            });
            let body = tr!(
                "models-delete-dialog-body",
                name = model.display_name.clone()
            );
            let mut text = String::new();
            let mut marks = Vec::new();
            for run in parse_markup(&body, &["strong"]) {
                let start = text.len();
                text.push_str(&run.text);
                if run.tag.is_some() {
                    marks.push((
                        start..text.len(),
                        HighlightStyle {
                            font_weight: Some(FontWeight::SEMIBOLD),
                            color: Some(t_.foreground),
                            ..Default::default()
                        },
                    ));
                }
            }
            let (page, name) = (page.clone(), model.model_name.clone());
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(dialog_header(t("models-delete-dialog-title"), None, cx))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .text_size(metrics::TEXT_SM)
                        .line_height(metrics::LINE_SM)
                        .text_color(t_.muted_foreground)
                        .child(StyledText::new(text).with_highlights(marks))
                        .when_some(model.size_mb, |col, size| {
                            col.child(tr!(
                                "models-delete-dialog-size-note",
                                size = format_size_mb(size)
                            ))
                        }),
                )
                .child(
                    dialog_footer()
                        .child(
                            Button::new("model-delete-cancel")
                                .outline()
                                .label(t("common-cancel"))
                                .disabled(deleting)
                                .on_click(|_, window, cx| close_dialog(window, cx)),
                        )
                        .child(
                            Button::new("model-delete-confirm")
                                .destructive()
                                .label(if deleting {
                                    t("models-delete-dialog-deleting")
                                } else {
                                    t("common-delete")
                                })
                                .loading(deleting)
                                .on_click(move |_, window, cx| {
                                    let name = name.clone();
                                    page.update(cx, |page, cx| page.delete(name, window, cx))
                                        .ok();
                                }),
                        ),
                )
                .into_any_element()
        });
    }

    pub(super) fn confirm_move(
        &mut self,
        folder: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let page = cx.entity().downgrade();
        open_dialog(window, cx, move |_, cx| {
            let t_ = theme(cx).clone();
            let (page, folder) = (page.clone(), folder.clone());
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(dialog_header(
                    t("models-migrate-dialog-title"),
                    Some(t("models-migrate-dialog-description").into()),
                    cx,
                ))
                .child(
                    mono(&t_)
                        .px_3()
                        .py_2()
                        .rounded(metrics::RADIUS_MD)
                        .border_1()
                        .border_color(t_.border)
                        .bg(t_.muted)
                        .text_color(t_.muted_foreground)
                        .truncate()
                        .child(folder.to_string_lossy().into_owned()),
                )
                .child(
                    dialog_footer()
                        .child(
                            Button::new("models-move-cancel")
                                .outline()
                                .label(t("common-cancel"))
                                .on_click(|_, window, cx| close_dialog(window, cx)),
                        )
                        .child(
                            Button::new("models-move-confirm")
                                .label(t("models-migrate-dialog-action"))
                                .on_click(move |_, window, cx| {
                                    close_dialog(window, cx);
                                    let folder = folder.clone();
                                    page.update(cx, |page, cx| page.migrate(folder, window, cx))
                                        .ok();
                                }),
                        ),
                )
                .into_any_element()
        });
    }
}

fn download_progress(state: &DownloadState, t_: &KassTheme) -> Div {
    let progress = state.progress.as_ref();
    let pct = percent(progress);
    let text = match progress {
        Some(p) if pct.is_some() => format!(
            "{} / {} ({:.1}%)",
            format_bytes(p.current.unwrap_or(0) as f64),
            format_bytes(p.total.unwrap_or(0) as f64),
            pct.unwrap_or(0.)
        ),
        Some(p) if p.filename.is_some() => p.filename.clone().unwrap_or_default(),
        _ => t("models-progress-connecting-hf"),
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(bar(pct.unwrap_or(0.) as f32 / 100., px(4.), t_))
        .child(mono(t_).text_color(t_.muted_foreground).child(text))
}

fn overlay(migration: &Migration, t_: &KassTheme) -> Stateful<Div> {
    let progress = migration.progress.as_ref();
    let restarting =
        migration.restarting || progress.is_some_and(|p| p.status == ProgressStatus::Complete);
    div()
        .id("models-migrating")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(t_.background.opacity(0.95))
        .occlude()
        .child(
            div()
                .w_full()
                .max_w(px(448.))
                .px(px(32.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(8.))
                .text_center()
                .child(spinner("models-migrating-spinner", px(24.), t_.accent))
                .child(
                    div()
                        .text_size(metrics::TEXT_LG)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(t("models-migrate-title")),
                )
                .child(
                    div()
                        .text_size(metrics::TEXT_BASE)
                        .text_color(t_.muted_foreground)
                        .child(if restarting {
                            t("models-migrate-dialog-restarting-server")
                        } else {
                            t("models-migrate-offline")
                        }),
                )
                .when_some(progress.filter(|p| p.total > 0.), |col, p| {
                    col.child(
                        div()
                            .mt(px(16.))
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(bar(p.progress as f32 / 100., px(4.), t_))
                            .child(
                                mono(t_)
                                    .flex()
                                    .justify_between()
                                    .gap_4()
                                    .text_color(t_.muted_foreground)
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .child(p.filename.clone().unwrap_or_default()),
                                    )
                                    .child(div().flex_none().child(format!(
                                        "{} / {}",
                                        format_bytes(p.current),
                                        format_bytes(p.total)
                                    ))),
                            ),
                    )
                }),
        )
}

impl Render for ModelsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_info(cx);
        let t_ = theme(cx).clone();
        let detail = match self.current().cloned() {
            Some(model) => self.detail(&model, cx),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(metrics::TEXT_BASE)
                .text_color(t_.muted_foreground)
                .when(self.models.is_some(), |empty| {
                    empty.child(t("models-empty"))
                })
                .into_any_element(),
        };

        div()
            .id("models-page")
            .relative()
            .size_full()
            .flex()
            .bg(t_.background)
            .text_color(t_.foreground)
            .child(
                div()
                    .w(px(440.))
                    .flex_none()
                    .h_full()
                    .flex()
                    .flex_col()
                    .border_r_1()
                    .border_color(t_.border)
                    .child(
                        div()
                            .flex_none()
                            .px_4()
                            .pt(px(18.))
                            .pb(px(14.))
                            .child(
                                div()
                                    .text_size(metrics::TEXT_LG)
                                    .line_height(metrics::LINE_LG)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(t("models-title")),
                            )
                            .children(self.storage(cx)),
                    )
                    .child(self.list(cx))
                    .children(self.problems(cx)),
            )
            .child(div().flex_1().min_w_0().h_full().child(detail))
            .when_some(self.migration.as_ref(), |page, migration| {
                page.child(overlay(migration, &t_))
            })
    }
}
