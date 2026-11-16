//! The main window's left rail: logo, labeled icon tabs, and the version at
//! the bottom (or, once an update has downloaded, a button to restart into it).

use super::Handler;
use crate::theme::{metrics, theme};
use gpui_kit::assets::IconName;
use gpui_kit::component::ThemeStyled as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::rc::Rc;
use std::time::Duration;

/// How bad a tab's alert is: amber triangle or red circle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertLevel {
    Warning,
    Error,
}

/// A mark in a tab's corner, with what's wrong in its tooltip.
#[derive(Clone, Debug)]
pub struct TabAlert {
    pub level: AlertLevel,
    pub messages: Vec<SharedString>,
}

pub struct SidebarTab {
    id: SharedString,
    label: SharedString,
    icon: IconName,
    alert: Option<TabAlert>,
    on_click: Option<Handler>,
}

impl SidebarTab {
    pub fn new(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        icon: IconName,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon,
            alert: None,
            on_click: None,
        }
    }

    pub fn alert(mut self, alert: Option<TabAlert>) -> Self {
        self.alert = alert;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

/// What sits at the bottom of the rail.
pub enum SidebarFooter {
    /// The running version, e.g. `0.10.0-beta.7`.
    Version(SharedString),
    /// A newer release is ready; clicking restarts into it.
    Update {
        label: SharedString,
        /// The tooltip, e.g. "Restart to update to 0.11.0 (you have 0.10.0)".
        tip: SharedString,
        latest: SharedString,
        restarting: bool,
        on_restart: Handler,
    },
}

#[derive(IntoElement)]
pub struct Sidebar {
    logo: Option<AnyElement>,
    tabs: Vec<SidebarTab>,
    active: Option<SharedString>,
    footer: Option<SidebarFooter>,
}

impl Sidebar {
    pub fn new() -> Self {
        Self {
            logo: None,
            tabs: Vec::new(),
            active: None,
            footer: None,
        }
    }

    /// Drawn 32px square above the tabs.
    pub fn logo(mut self, logo: impl IntoElement) -> Self {
        self.logo = Some(logo.into_any_element());
        self
    }

    pub fn tab(mut self, tab: SidebarTab) -> Self {
        self.tabs.push(tab);
        self
    }

    /// The id of the tab that's showing.
    pub fn active(mut self, id: impl Into<SharedString>) -> Self {
        self.active = Some(id.into());
        self
    }

    pub fn footer(mut self, footer: SidebarFooter) -> Self {
        self.footer = Some(footer);
        self
    }
}

impl Default for Sidebar {
    fn default() -> Self {
        Self::new()
    }
}

/// `v0.10.0` with the prerelease tag (`beta.7`) on its own line, to fit the rail.
pub fn version_lines(version: &str) -> (String, Option<String>) {
    let version = version.trim_start_matches('v');
    match version.split_once('-') {
        Some((release, prerelease)) => (format!("v{release}"), Some(prerelease.to_string())),
        None => (format!("v{version}"), None),
    }
}

fn version_label(version: &str, size: Pixels, mono: SharedString) -> Div {
    let (release, prerelease) = version_lines(version);
    div()
        .flex()
        .flex_col()
        .items_center()
        .font_family(mono)
        .text_size(size)
        .line_height(relative(1.25))
        .child(release)
        .children(prerelease)
}

fn icon(icon: IconName, size: f32, color: Hsla) -> Svg {
    svg()
        .path(icon.path())
        .size(px(size))
        .flex_none()
        .text_color(color)
}

impl RenderOnce for Sidebar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = theme(cx).clone();
        let active = self.active;
        let tabs = self.tabs.into_iter().map(|tab| {
            let is_active = active.as_ref() == Some(&tab.id);
            let fg = if is_active {
                t.foreground
            } else {
                t.muted_foreground
            };
            let hover_bg = t.muted.opacity(0.5);
            let hover_fg = t.foreground;
            let alert = tab.alert.clone();
            let group: SharedString = format!("sidebar-tab-{}", tab.id).into();
            div()
                .id(ElementId::Name(tab.id.clone()))
                .group(group.clone())
                .relative()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_1()
                .w(px(56.))
                .h(px(52.))
                .rounded(metrics::RADIUS_LG)
                .text_size(metrics::TEXT_2XS)
                .text_color(fg)
                .cursor_pointer()
                .when(is_active, |s| s.bg(t.muted))
                .when(!is_active, |s| {
                    s.hover(move |s| s.bg(hover_bg).text_color(hover_fg))
                })
                .when_some(tab.on_click, |s, on_click| {
                    s.on_click(move |_, window, cx| on_click(window, cx))
                })
                .child(icon(tab.icon, 18., fg).when(!is_active, |s| {
                    s.group_hover(group, move |s| s.text_color(hover_fg))
                }))
                .child(tab.label)
                .when_some(alert, |s, alert| {
                    let (mark, color) = match alert.level {
                        AlertLevel::Warning => (IconName::TriangleAlert, t.warning),
                        AlertLevel::Error => (IconName::CircleAlert, t.destructive),
                    };
                    let text: SharedString = alert.messages.join("\n").into();
                    s.child(
                        div()
                            .absolute()
                            .top(px(6.))
                            .right(px(6.))
                            .rounded_full()
                            .bg(t.sidebar)
                            .child(icon(mark, 12., color)),
                    )
                    .tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
                })
        });

        let footer = self.footer.map(|footer| match footer {
            SidebarFooter::Version(version) => {
                version_label(&version, metrics::TEXT_2XS, t.mono_font.clone())
                    .mt_auto()
                    .text_color(t.muted_foreground.opacity(0.6))
                    .into_any_element()
            }
            SidebarFooter::Update {
                label,
                tip,
                latest,
                restarting,
                on_restart,
            } => {
                let focus = window
                    .use_keyed_state("sidebar-update", cx, |_, cx| cx.focus_handle())
                    .read(cx)
                    .clone();
                let focused = focus.is_focused(window);
                let mark = if restarting {
                    icon(IconName::LoaderCircle, 18., t.accent)
                        .with_animation(
                            "sidebar-update-spin",
                            Animation::new(Duration::from_millis(900)).repeat(),
                            |svg, delta| {
                                svg.with_transformation(Transformation::rotate(percentage(delta)))
                            },
                        )
                        .into_any_element()
                } else {
                    icon(IconName::CircleArrowUp, 18., t.accent).into_any_element()
                };
                let hover_bg = t.accent.opacity(0.1);
                div()
                    .id("sidebar-update")
                    .track_focus(&focus)
                    .mt_auto()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(2.))
                    .w(px(56.))
                    .py(px(6.))
                    .rounded(metrics::RADIUS_LG)
                    .text_size(metrics::TEXT_2XS)
                    .text_color(t.accent)
                    .when(!restarting, |s| {
                        s.cursor_pointer()
                            .hover(move |s| s.bg(hover_bg))
                            .on_click(move |_, window, cx| on_restart(window, cx))
                    })
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .when(focused, |s| s.focus_ring_style(window, cx))
                    .child(mark)
                    .child(label)
                    .child(version_label(&latest, px(9.), t.mono_font.clone()).opacity(0.8))
                    .into_any_element()
            }
        });

        div()
            .flex()
            .flex_col()
            .flex_none()
            .items_center()
            .gap_1()
            .w(px(68.))
            .h_full()
            .pt_3()
            .pb_3()
            .bg(t.sidebar)
            .border_r_1()
            .border_color(t.border)
            .when_some(self.logo, |s, logo| {
                s.child(div().mb_4().size(px(32.)).flex_none().child(logo))
            })
            .children(tabs)
            .children(footer)
    }
}

#[cfg(test)]
mod tests {
    use super::version_lines;

    #[test]
    fn prerelease_goes_on_its_own_line() {
        assert_eq!(
            version_lines("0.10.0-beta.7"),
            ("v0.10.0".into(), Some("beta.7".into()))
        );
        assert_eq!(version_lines("v1.2.3"), ("v1.2.3".into(), None));
    }
}
