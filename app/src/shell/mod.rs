//! The main window in GPUI: the title bar strip, the left rail, the screen
//! showing and the status line. App-wide dialogs and toasts (asks,
//! downloads) draw over every screen.

mod asks;
mod away;
mod captures;
pub mod data;
mod downloads;
mod insights;
mod launch;
pub mod logs;
mod models;
mod reports;
pub(crate) mod status;

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_captures::CapturesPage;
use kass_insights::{InsightsPage, Link};
use kass_models::ModelsPage;
use kass_ui::{metrics, theme, AlertLevel, Sidebar, SidebarFooter, SidebarTab, TabAlert};

use crate::host::{self, AppHandle, Listener, Manager};
use crate::settings::{self, Settings};
use crate::updater::{self, UpdateStatus};
use crate::ServerState;
use asks::Asks;
use data::{Health, ShellData};
use downloads::DownloadToasts;

/// A screen of the main window, like `/models` with `?model=whisper-turbo`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Location {
    pub path: String,
    pub search: String,
}

/// The strip the traffic lights sit in, above everything else.
pub(crate) const TITLEBAR: f32 = 48.;
/// The left rail.
pub(crate) const RAIL: f32 = 68.;

struct ShellGlobal {
    data: Entity<ShellData>,
    view: Option<WeakEntity<MainShell>>,
    /// Where to go once the main window opens.
    pending: Option<Location>,
}

impl Global for ShellGlobal {}

/// Start what the shell keeps from launch: the server's log and its data.
pub fn init(cx: &mut App) {
    logs::init(cx);
    let data = cx.new(ShellData::new);
    cx.set_global(ShellGlobal {
        data: data.clone(),
        view: None,
        pending: None,
    });
    launch::init(&data, cx);
    crate::deep_link::deliver(cx);
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

/// Onboarding closed: the main window keeps the keys again.
pub fn resume_after_onboarding(cx: &mut App) {
    launch::resume(cx);
}

/// [`shell_data`], or `None` before [`init`].
pub fn try_shell_data(cx: &App) -> Option<Entity<ShellData>> {
    Some(cx.try_global::<ShellGlobal>()?.data.clone())
}

/// What GPUI draws in the main window.
pub fn root(window: &mut Window, cx: &mut App) -> AnyView {
    let view = cx.new(|cx| MainShell::new(window, cx));
    let global = cx.global_mut::<ShellGlobal>();
    global.view = Some(view.downgrade());
    if let Some(location) = global.pending.take() {
        view.update(cx, |shell, cx| shell.go(location, cx));
    }
    view.into()
}

fn view(cx: &App) -> Option<Entity<MainShell>> {
    cx.try_global::<ShellGlobal>()?.view.as_ref()?.upgrade()
}

/// Whether the main window shows `path`.
pub fn showing(path: &str, cx: &App) -> bool {
    view(cx).is_some_and(|view| view.read(cx).location.path.trim_end_matches('/') == path)
}

/// Show `path` (with `search`, like `?model=whisper-turbo`).
pub fn navigate(path: &str, search: &str, cx: &mut App) {
    let location = Location {
        path: moved(path).to_string(),
        search: search.to_string(),
    };
    match view(cx) {
        Some(view) => view.update(cx, |shell, cx| shell.go(location, cx)),
        None if cx.has_global::<ShellGlobal>() => {
            cx.global_mut::<ShellGlobal>().pending = Some(location);
        }
        None => {}
    }
}

/// Where a path from an older link or a bare `kass://` now leads.
fn moved(path: &str) -> &str {
    match path.trim_end_matches('/') {
        "" => "/captures",
        "/settings/captures" => "/settings/dictation",
        "/settings/server" => "/settings",
        _ => path,
    }
}

/// The model a `?model=` link asks the Models page to show.
fn model_in(search: &str) -> Option<String> {
    search
        .trim_start_matches('?')
        .split('&')
        .find_map(|pair| pair.strip_prefix("model="))
        .map(|name| name.trim_matches('"'))
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

/// The tab of the rail a path belongs to: its first segment.
fn active_tab(path: &str) -> &str {
    path.trim_start_matches('/').split('/').next().unwrap_or("")
}

pub struct MainShell {
    data: Entity<ShellData>,
    location: Location,
    settings: Settings,
    captures: Entity<CapturesPage>,
    /// The server the Captures page was made for; it starts over on another.
    captures_server: String,
    icons: Rc<insights::Icons>,
    insights: Entity<InsightsPage>,
    window: AnyWindowHandle,
    reports: Entity<reports::ReportsPage>,
    models: Entity<ModelsPage>,
    /// The search Models was last shown with; `None` while it's not showing.
    models_shown: Option<String>,
    logo: Arc<Image>,
    restarting: bool,
    _asks: Entity<Asks>,
    _downloads: Entity<DownloadToasts>,
    _subscriptions: Vec<Subscription>,
}

impl MainShell {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let data = shell_data(cx);
        let subscriptions = vec![
            cx.observe_in(&data, window, |shell: &mut Self, data, window, cx| {
                let server = data.read(cx).api.base_url();
                if server != shell.captures_server {
                    shell.captures = new_captures(&data, &shell.icons, window, cx);
                    shell.captures_server = server;
                }
                cx.notify();
            }),
            cx.observe_window_appearance(window, |_, window, cx| {
                kass_ui::system_appearance_changed(window.appearance(), cx);
            }),
        ];
        kass_ui::system_appearance_changed(window.appearance(), cx);
        let api = data.read(cx).api.clone();
        let icons = Rc::new(insights::Icons::default());
        // The Dictionary and Writing style show apps' icons from here too.
        let lookup = icons.clone();
        kass_ui::set_app_icons(move |bundle_id| lookup.get(bundle_id), cx);
        let asks = cx.new(|cx| Asks::new(data.clone(), window, cx));
        let downloads = cx.new(|cx| DownloadToasts::new(data.clone(), window, cx));
        let host = Rc::new(models::ShellModels { data: data.clone() });
        let models = cx.new(|cx| ModelsPage::new(api.clone(), window, cx).with_host(host));
        Self {
            settings: Settings::new(data.clone(), window, cx),
            captures: new_captures(&data, &icons, window, cx),
            captures_server: api.base_url(),
            insights: cx.new(|cx| InsightsPage::new(api, icons.clone(), cx)),
            icons,
            window: window.window_handle(),
            reports: cx.new(|_| reports::ReportsPage::default()),
            models,
            models_shown: None,
            data,
            location: Location {
                path: "/captures".into(),
                search: String::new(),
            },
            logo: Arc::new(Image::from_bytes(
                ImageFormat::Png,
                include_bytes!("../../assets/kass-logo.png").to_vec(),
            )),
            restarting: false,
            _asks: asks,
            _downloads: downloads,
            _subscriptions: subscriptions,
        }
    }

    fn go(&mut self, location: Location, cx: &mut Context<Self>) {
        self.location = location;
        self.follow_link(cx);
        cx.notify();
    }

    /// A link into a native screen: Insights on an app or period, or a
    /// capture in Captures.
    fn follow_link(&mut self, cx: &mut Context<Self>) {
        match active_tab(&self.location.path) {
            "insights" => {
                let link = Link::parse(&self.location.search);
                if !link.is_empty() {
                    self.insights.update(cx, |page, cx| page.open(link, cx));
                }
            }
            "captures" => {
                let Some(id) = captures::capture_in(&self.location.search) else {
                    return;
                };
                // Dropped once followed, so the same link works again.
                self.location.search.clear();
                let (page, window) = (self.captures.downgrade(), self.window);
                cx.defer(move |cx| {
                    let _ = window.update(cx, |_, window, cx| {
                        let _ = page.update(cx, |page, cx| page.open_capture(id, window, cx));
                    });
                });
            }
            _ => {}
        }
    }

    /// Models fetches and polls only while it's showing, and opens the model
    /// a `?model=` link names.
    fn follow_models(&mut self, cx: &mut Context<Self>) {
        let shown =
            (active_tab(&self.location.path) == "models").then(|| self.location.search.clone());
        if shown == self.models_shown {
            return;
        }
        let was_showing = self.models_shown.is_some();
        self.models_shown = shown.clone();
        self.models.update(cx, |page, cx| match shown {
            Some(search) => {
                page.show(model_in(&search), cx);
                if !was_showing {
                    page.refresh(cx);
                }
            }
            None => page.hide(),
        });
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
            .tab(tab(
                "captures",
                &kass_i18n::t("nav-captures"),
                IconName::Captions,
            ))
            .tab(tab(
                "insights",
                &kass_i18n::t("nav-insights"),
                IconName::ChartColumn,
            ))
            .tab(tab("models", &kass_i18n::t("nav-models"), IconName::Box).alert(alerts))
            .tab(tab(
                "reports",
                &kass_i18n::t("nav-reports"),
                IconName::FileArchive,
            ))
            .tab(tab(
                "settings",
                &kass_i18n::t("nav-settings"),
                IconName::SlidersHorizontal,
            ))
            .active(active_tab(&self.location.path).to_string())
            .footer(footer)
    }
}

/// The Captures page, on the server the shell is using now.
fn new_captures(
    data: &Entity<ShellData>,
    icons: &Rc<insights::Icons>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<CapturesPage> {
    let api = data.read(cx).api.clone();
    let host = Rc::new(captures::Host {
        icons: icons.clone(),
    });
    cx.new(|cx| CapturesPage::new(api, host, window, cx))
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
            kass_i18n::tr!(
                "nav-alerts-download-failed",
                name = download.model_name.clone()
            )
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
        self.follow_models(cx);
        let t = theme(cx);
        let (background, foreground, border, sans) =
            (t.background, t.foreground, t.border, t.sans_font());
        let path = self.location.path.clone();

        let data = self.data.read(cx);
        let failed = match (&data.startup_error, &data.health) {
            (Some(error), Health::Offline) => Some(error.clone()),
            _ => None,
        };
        let content: AnyElement = if let Some(error) = failed {
            launch::failure(&error, cx)
        } else if settings::is_settings(&path) {
            let search = self.location.search.clone();
            self.settings
                .render(&path, &search, window, cx)
                .into_any_element()
        } else if active_tab(&path) == "insights" {
            self.insights.clone().into_any_element()
        } else if active_tab(&path) == "captures" {
            self.captures.clone().into_any_element()
        } else if active_tab(&path) == "reports" {
            self.reports.clone().into_any_element()
        } else if active_tab(&path) == "models" {
            self.models.clone().into_any_element()
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
                            .children(away::banner(&self.data, cx))
                            .child(content),
                    ),
            )
            .child(status)
    }
}

#[cfg(test)]
mod tests {
    use super::{active_tab, model_in, moved};

    #[test]
    fn model_links_name_a_model() {
        assert_eq!(
            model_in("?model=whisper-turbo").as_deref(),
            Some("whisper-turbo")
        );
        assert_eq!(model_in("?x=1&model=qwen3-4b").as_deref(), Some("qwen3-4b"));
        assert_eq!(model_in("?model="), None);
        assert_eq!(model_in(""), None);
    }

    #[test]
    fn old_links_lead_where_the_screens_are_now() {
        assert_eq!(moved("/"), "/captures");
        assert_eq!(moved(""), "/captures");
        assert_eq!(moved("/settings/captures"), "/settings/dictation");
        assert_eq!(moved("/settings/server"), "/settings");
        assert_eq!(moved("/models"), "/models");
    }

    #[test]
    fn the_rail_follows_the_first_segment() {
        assert_eq!(active_tab("/settings/logs"), "settings");
        assert_eq!(active_tab("/models"), "models");
        assert_eq!(active_tab("/"), "");
    }
}
