//! The main window's frame in GPUI: the title bar strip, the left rail, the
//! settings navigation and the status line. The screens not yet rebuilt in
//! GPUI still come from the page, in a web view placed over the content
//! area; the native ones (src/settings) hide it.

pub mod bridge;
pub mod data;
pub mod logs;
pub(crate) mod status;

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_ui::{
    metrics, theme, AlertLevel, Sidebar, SidebarFooter, SidebarTab, TabAlert,
};
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::Rect;

use crate::host::{self, webview, AppHandle, Listener, Manager};
use crate::settings::{self, Settings};
use crate::updater::{self, UpdateStatus};
use crate::{ServerState, MAIN_WINDOW_LABEL};
use bridge::{Location, Synced};
use data::ShellData;

/// The strip the traffic lights sit in, above everything else.
const TITLEBAR: f32 = 48.;
/// The left rail.
const RAIL: f32 = 68.;

struct ShellGlobal {
    data: Entity<ShellData>,
    view: Option<WeakEntity<MainShell>>,
}

impl Global for ShellGlobal {}

/// Start what the shell keeps from launch: the server's log and its data.
pub fn init(cx: &mut App) {
    logs::init(cx);
    let data = cx.new(ShellData::new);
    cx.set_global(ShellGlobal { data, view: None });
    AppHandle.listen("update:status", |_| {
        host::with_app(|cx| {
            let data = shell_data(cx);
            data.update(cx, |data, cx| data.refresh_update(cx));
        });
    });
}

pub fn shell_data(cx: &App) -> Entity<ShellData> {
    cx.global::<ShellGlobal>().data.clone()
}

/// What GPUI draws in the main window.
pub fn root(window: &mut Window, cx: &mut App) -> AnyView {
    let view = cx.new(|cx| MainShell::new(window, cx));
    cx.global_mut::<ShellGlobal>().view = Some(view.downgrade());
    view.into()
}

fn view(cx: &App) -> Option<Entity<MainShell>> {
    cx.try_global::<ShellGlobal>()?.view.as_ref()?.upgrade()
}

/// Show `path`: the page's router follows, and the shell shows it now.
pub fn navigate(path: &str, search: &str, cx: &mut App) {
    if let Some(view) = view(cx) {
        view.update(cx, |shell, cx| {
            shell.location = Location {
                path: path.to_string(),
                search: search.to_string(),
            };
            cx.notify();
        });
    }
    bridge::navigate(path, search);
}

/// The page's router moved (`shell_location`).
pub fn set_location(location: Location) {
    host::with_app(move |cx| {
        if let Some(view) = view(cx) {
            view.update(cx, |shell, cx| {
                if shell.location != location {
                    shell.location = location;
                    cx.notify();
                }
            });
        }
    });
}

/// The page's preferences changed (`shell_sync`).
pub fn set_synced(synced: Synced) {
    host::with_app(move |cx| {
        let data = shell_data(cx);
        data.update(cx, |data, cx| data.set_synced(synced, cx));
    });
}

/// Where the web view goes, for the screen that's showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    /// A native screen: no web view.
    Hidden,
    /// Right of the rail.
    Content,
    /// Right of the settings navigation.
    Settings,
}

impl Placement {
    fn of(path: &str) -> Self {
        if settings::Page::of(path).is_some() {
            Self::Hidden
        } else if settings::is_settings(path) {
            Self::Settings
        } else {
            Self::Content
        }
    }

    fn frame(self, size: Size<Pixels>) -> Option<Rect> {
        let left = match self {
            Self::Hidden => return None,
            Self::Content => RAIL,
            Self::Settings => RAIL + settings::NAV_WIDTH,
        };
        // Below the title strip and the 1px line under it; above the status line.
        let top = TITLEBAR + 1.;
        let width = (f32::from(size.width) - left).max(0.);
        let height = (f32::from(size.height) - top - status::HEIGHT).max(0.);
        Some(Rect {
            position: LogicalPosition::new(f64::from(left), f64::from(top)).into(),
            size: LogicalSize::new(f64::from(width), f64::from(height)).into(),
        })
    }
}

/// The tab of the rail a path belongs to: its first segment.
fn active_tab(path: &str) -> &str {
    path.trim_start_matches('/').split('/').next().unwrap_or("")
}

pub struct MainShell {
    data: Entity<ShellData>,
    location: Location,
    placed: Option<Placement>,
    settings: Settings,
    logo: Arc<Image>,
    restarting: bool,
    _subscriptions: Vec<Subscription>,
}

impl MainShell {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let data = shell_data(cx);
        let subscriptions = vec![
            cx.observe(&data, |_, _, cx| cx.notify()),
            cx.observe_window_appearance(window, |_, window, cx| {
                kass_ui::system_appearance_changed(window.appearance(), cx);
            }),
        ];
        kass_ui::system_appearance_changed(window.appearance(), cx);
        Self {
            settings: Settings::new(data.clone(), window, cx),
            data,
            location: Location {
                path: "/captures".into(),
                search: String::new(),
            },
            placed: None,
            logo: Arc::new(Image::from_bytes(
                ImageFormat::Png,
                include_bytes!("../../../../app/src/assets/kass-logo.png").to_vec(),
            )),
            restarting: false,
            _subscriptions: subscriptions,
        }
    }

    /// Put the web view where the screen showing needs it. It keeps its
    /// margins as the window resizes, so this runs only when they change.
    fn place_webview(&mut self, window: &Window) {
        let placement = Placement::of(&self.location.path);
        if self.placed == Some(placement) {
            return;
        }
        if webview::place(MAIN_WINDOW_LABEL, placement.frame(window.viewport_size())) {
            self.placed = Some(placement);
        }
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> Sidebar {
        let data = self.data.read(cx);
        let alerts = model_alerts(data);
        let tab = |id: &'static str, label: &str, icon: IconName| {
            SidebarTab::new(id, label.to_string(), icon)
                .on_click(move |_, cx| navigate(&format!("/{id}"), "", cx))
        };
        let footer = match &data.update {
            UpdateStatus::Ready { version: latest } => {
                let restarting = self.restarting;
                let entity = cx.entity().downgrade();
                SidebarFooter::Update {
                    label: if restarting {
                        kass_i18n::t("nav-update-restarting-label")
                    } else {
                        kass_i18n::t("nav-update-label")
                    }
                    .into(),
                    tip: if restarting {
                        kass_i18n::t("nav-update-restarting")
                    } else {
                        kass_i18n::tr!(
                            "nav-update-tip",
                            latest = latest.clone(),
                            current = data.version.clone()
                        )
                    }
                    .into(),
                    latest: latest.clone().into(),
                    restarting,
                    on_restart: std::rc::Rc::new(move |_, cx| {
                        let _ = entity.update(cx, |shell, cx| {
                            shell.restarting = true;
                            cx.notify();
                        });
                        let entity = entity.clone();
                        host::async_runtime::spawn(async move {
                            let state = AppHandle.state::<ServerState>();
                            if let Err(e) = updater::restart_to_update(state).await {
                                eprintln!("[update] restart failed: {e}");
                                host::with_app(move |cx| {
                                    let _ = entity.update(cx, |shell, cx| {
                                        shell.restarting = false;
                                        cx.notify();
                                    });
                                });
                            }
                        });
                    }),
                }
            }
            _ => SidebarFooter::Version(data.version.clone().into()),
        };
        Sidebar::new()
            .logo(img(self.logo.clone()).size(px(32.)))
            .tab(tab("captures", &kass_i18n::t("nav-captures"), IconName::Captions))
            .tab(tab("insights", &kass_i18n::t("nav-insights"), IconName::ChartColumn))
            .tab(tab("models", &kass_i18n::t("nav-models"), IconName::Box).alert(alerts))
            .tab(tab("reports", &kass_i18n::t("nav-reports"), IconName::FileArchive))
            .tab(tab(
                "settings",
                &kass_i18n::t("nav-settings"),
                IconName::SlidersHorizontal,
            ))
            .active(active_tab(&self.location.path).to_string())
            .footer(footer)
    }
}

/// What's wrong with the models, for the Models tab: a model dictation needs
/// that isn't downloaded is an error; a failed download is a warning.
fn model_alerts(data: &ShellData) -> Option<TabAlert> {
    let mut errors = Vec::new();
    let auto_refine = data.settings.as_ref().is_some_and(|s| s.auto_refine);
    if let Some(readiness) = &data.readiness {
        if !readiness.stt.ready {
            errors.push(kass_i18n::tr!(
                "nav-alerts-missing",
                role = kass_i18n::t("models-roles-transcription"),
                name = readiness.stt.display_name.clone()
            ));
        }
        if auto_refine && !readiness.llm.ready {
            errors.push(kass_i18n::tr!(
                "nav-alerts-missing",
                role = kass_i18n::t("models-roles-refinement"),
                name = readiness.llm.display_name.clone()
            ));
        }
    }
    let warnings: Vec<String> = data
        .downloads
        .iter()
        .filter(|download| download.status == "error")
        .map(|download| {
            kass_i18n::tr!("nav-alerts-download-failed", name = download.model_name.clone())
        })
        .collect();
    let level = if !errors.is_empty() {
        AlertLevel::Error
    } else if !warnings.is_empty() {
        AlertLevel::Warning
    } else {
        return None;
    };
    Some(TabAlert {
        level,
        messages: errors.into_iter().chain(warnings).map(Into::into).collect(),
    })
}

impl Render for MainShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.place_webview(window);
        let t = theme(cx);
        let (background, foreground, border, sans) =
            (t.background, t.foreground, t.border, t.sans_font());
        let path = self.location.path.clone();

        let content: AnyElement = if settings::is_settings(&path) {
            let search = self.location.search.clone();
            self.settings
                .render(&path, &search, window, cx)
                .into_any_element()
        } else {
            div().into_any_element()
        };

        let go = |target: status::Target, _: &mut Window, cx: &mut App| match target {
            status::Target::Path(path) => navigate(path, "", cx),
            status::Target::Accessibility => {
                let _ = crate::open_accessibility_settings(AppHandle);
            }
            status::Target::InputMonitoring => {
                let _ = crate::open_input_monitoring_settings();
            }
        };
        let status = status::bar(self.data.read(cx), go, cx);

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(background)
            .text_color(foreground)
            .font_family(sans)
            .text_size(metrics::TEXT_BASE)
            .child(
                div()
                    .id("titlebar")
                    .h(px(TITLEBAR))
                    .flex_shrink_0()
                    .w_full()
                    .on_mouse_down(MouseButton::Left, |event, window, _| {
                        if event.click_count == 2 {
                            window.titlebar_double_click();
                        } else {
                            window.start_window_move();
                        }
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .border_t_1()
                    .border_color(border)
                    .child(self.sidebar(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .child(content),
                    ),
            )
            .child(status)
    }
}

#[cfg(test)]
mod tests {
    use super::{active_tab, Placement};
    use gpui_kit::{px, size};
    use wry::dpi::{LogicalPosition, LogicalSize};

    #[test]
    fn native_screens_hide_the_web_view() {
        assert_eq!(Placement::of("/settings"), Placement::Hidden);
        assert_eq!(Placement::of("/settings/"), Placement::Hidden);
        assert_eq!(Placement::of("/settings/transcription"), Placement::Hidden);
        assert_eq!(Placement::of("/settings/logs"), Placement::Hidden);
        assert_eq!(Placement::of("/settings/dictation"), Placement::Settings);
        assert_eq!(Placement::of("/captures"), Placement::Content);
        assert_eq!(Placement::of("/models"), Placement::Content);
    }

    #[test]
    fn the_web_view_fills_what_the_frame_leaves() {
        let frame = Placement::Settings
            .frame(size(px(1200.), px(800.)))
            .unwrap();
        let position: LogicalPosition<f64> = frame.position.to_logical(1.);
        let extent: LogicalSize<f64> = frame.size.to_logical(1.);
        assert_eq!((position.x, position.y), (288., 49.));
        assert_eq!((extent.width, extent.height), (912., 721.));
        assert!(Placement::Hidden.frame(size(px(1200.), px(800.))).is_none());
    }

    #[test]
    fn the_rail_follows_the_first_segment() {
        assert_eq!(active_tab("/settings/logs"), "settings");
        assert_eq!(active_tab("/models"), "models");
        assert_eq!(active_tab("/"), "");
    }
}
