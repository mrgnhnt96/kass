//! The Insights page: the app list, a header with the period switch, and
//! the stats for the app and period picked. Switching keeps the last stats
//! up until the new ones land, and new dictations refresh it as they come.

use crate::host::InsightsHost;
use crate::link::Link;
use crate::widgets::{app_tile, icon, symbol_tile};
use gpui_kit::assets::IconName;
use gpui_kit::component::Sizable;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::{
    CaptureAppCount, CaptureAppFilter, CaptureAppsResponse, UsagePeriod, UsageStatsResponse,
    WritingStylesResponse,
};
use kass_api::{ApiClient, CaptureEvent, SseCancel};
use kass_i18n::tr;
use kass_ui::{metrics, theme};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

const PERIODS: [UsagePeriod; 4] = [
    UsagePeriod::Today,
    UsagePeriod::Days7,
    UsagePeriod::Days30,
    UsagePeriod::All,
];

/// What the pointer is over, for the one hover tip showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hover {
    /// A stat card's note, by card.
    Note(usize),
    /// A point of the words chart.
    Words(usize),
    /// A heat map cell: day (Monday first), hour.
    Heat(usize, usize),
    /// A bin of the length curve.
    Length(usize),
}

pub struct InsightsPage {
    pub(crate) client: ApiClient,
    pub(crate) host: Rc<dyn InsightsHost>,
    pub(crate) apps: Option<CaptureAppsResponse>,
    pub(crate) styles: Option<WritingStylesResponse>,
    pub(crate) filter: CaptureAppFilter,
    pub(crate) period: UsagePeriod,
    pub(crate) stats: Option<UsageStatsResponse>,
    pub(crate) loading: bool,
    pub(crate) apps_open: bool,
    pub(crate) hover: Option<Hover>,
    /// Bumped by every stats request, so an older answer never replaces a
    /// newer one.
    request: u64,
    loading_stats: Option<Task<()>>,
    loading_apps: Option<Task<()>>,
    events: SseCancel,
    _listen: Task<()>,
}

impl Drop for InsightsPage {
    fn drop(&mut self) {
        self.events.cancel();
    }
}

impl InsightsPage {
    pub fn new(client: ApiClient, host: Rc<dyn InsightsHost>, cx: &mut Context<Self>) -> Self {
        let events = SseCancel::new();
        let listen = listen(&client, &events, cx);
        let mut page = Self {
            client,
            host,
            apps: None,
            styles: None,
            filter: CaptureAppFilter::All,
            period: UsagePeriod::Days7,
            stats: None,
            loading: true,
            apps_open: false,
            hover: None,
            request: 0,
            loading_stats: None,
            loading_apps: None,
            events,
            _listen: listen,
        };
        page.refresh(cx);
        page
    }

    /// Fetches the app list and the stats again.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.load_apps(cx);
        self.load_stats(cx);
    }

    fn load_apps(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        self.loading_apps = Some(cx.spawn(async move |page, cx| {
            let (apps, styles) = cx
                .background_executor()
                .spawn(async move {
                    (
                        client.list_capture_apps().map_err(|e| e.to_string()),
                        client.list_writing_styles().ok(),
                    )
                })
                .await;
            page.update(cx, |page, cx| {
                match apps {
                    Ok(apps) => page.apps = Some(apps),
                    Err(error) => eprintln!("kass-insights: apps failed to load: {error}"),
                }
                if styles.is_some() {
                    page.styles = styles;
                }
                page.drop_missing_app(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn load_stats(&mut self, cx: &mut Context<Self>) {
        self.request += 1;
        let request = self.request;
        let client = self.client.clone();
        let (period, filter) = (self.period, self.filter.clone());
        self.loading = true;
        self.loading_stats = Some(cx.spawn(async move |page, cx| {
            let stats = cx
                .background_executor()
                .spawn(async move { client.capture_stats(period, &filter) })
                .await;
            page.update(cx, |page, cx| {
                if page.request != request {
                    return;
                }
                page.loading = false;
                match stats {
                    Ok(stats) => page.stats = Some(stats),
                    Err(error) => eprintln!("kass-insights: stats failed to load: {error}"),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    pub(crate) fn set_filter(&mut self, filter: CaptureAppFilter, cx: &mut Context<Self>) {
        if filter == self.filter {
            return;
        }
        self.filter = filter;
        self.hover = None;
        self.load_stats(cx);
    }

    /// Shows what a link from another screen asks for (see [`Link`]).
    pub fn open(&mut self, link: Link, cx: &mut Context<Self>) {
        if let Some(period) = link.period {
            self.set_period(period, cx);
        }
        if let Some(filter) = link.filter {
            self.set_filter(filter, cx);
        }
    }

    fn set_period(&mut self, period: UsagePeriod, cx: &mut Context<Self>) {
        if period == self.period {
            return;
        }
        self.period = period;
        self.hover = None;
        self.load_stats(cx);
    }

    /// Back to all apps when the app picked has no captures left.
    fn drop_missing_app(&mut self, cx: &mut Context<Self>) {
        let Some(apps) = &self.apps else {
            return;
        };
        let gone = match &self.filter {
            CaptureAppFilter::All => false,
            CaptureAppFilter::App(bundle) => !apps.apps.iter().any(|a| &a.app_bundle_id == bundle),
            CaptureAppFilter::Unknown => apps.unknown_count == 0,
        };
        if gone {
            self.set_filter(CaptureAppFilter::All, cx);
        }
    }

    /// Shows the tip for `target` while the pointer is over it.
    pub(crate) fn set_hover(&mut self, target: Hover, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            self.hover = Some(target);
        } else if self.hover == Some(target) {
            self.hover = None;
        } else {
            return;
        }
        cx.notify();
    }

    pub(crate) fn app(&self, bundle: &str) -> Option<&CaptureAppCount> {
        self.apps
            .as_ref()?
            .apps
            .iter()
            .find(|a| a.app_bundle_id == bundle)
    }

    pub(crate) fn style_name(&self, id: &str) -> Option<String> {
        self.styles
            .as_ref()?
            .styles
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.clone())
    }

    /// The picked app's name; None for all apps.
    pub(crate) fn app_name(&self) -> Option<String> {
        match &self.filter {
            CaptureAppFilter::All => None,
            CaptureAppFilter::App(bundle) => Some(
                self.app(bundle)
                    .map(display_name)
                    .unwrap_or_else(|| bundle.clone()),
            ),
            CaptureAppFilter::Unknown => Some(tr!("captures-apps-unknown")),
        }
    }

    /// The picked app's tile.
    pub(crate) fn app_tile(&self, cx: &App) -> AnyElement {
        match &self.filter {
            CaptureAppFilter::All => symbol_tile(IconName::LayoutGrid, cx),
            CaptureAppFilter::Unknown => symbol_tile(IconName::CircleQuestionMark, cx),
            CaptureAppFilter::App(bundle) => {
                let name = self.app_name().unwrap_or_default();
                app_tile(self.host.as_ref(), bundle, &name, cx)
            }
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let title = match self.app_name() {
            Some(app) => tr!("insights-title-app", app = app),
            None => tr!("insights-title"),
        };
        let mut switch = div()
            .ml_auto()
            .flex()
            .flex_none()
            .rounded(px(7.))
            .bg(t.muted)
            .p(px(2.));
        for period in PERIODS {
            let active = period == self.period;
            switch = switch.child(
                div()
                    .id(SharedString::from(format!("period-{}", period.as_str())))
                    .rounded(px(5.))
                    .px(px(12.))
                    .py(px(5.))
                    .text_size(metrics::TEXT_XS)
                    .line_height(px(16.))
                    .cursor_pointer()
                    .map(|b| {
                        if active {
                            b.bg(t.card).text_color(t.foreground).shadow_sm()
                        } else {
                            b.text_color(t.muted_foreground)
                                .hover(|s| s.text_color(t.foreground))
                        }
                    })
                    .on_click(cx.listener(move |page, _, _, cx| page.set_period(period, cx)))
                    .child(tr!(period_key(period))),
            );
        }
        div()
            .h(px(64.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(32.))
            .border_b_1()
            .border_color(t.border)
            .child(self.app_tile(cx))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(switch)
    }

    fn render_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        if self.apps.as_ref().is_some_and(|a| a.total == 0) {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .px(px(32.))
                .text_center()
                .child(icon(IconName::ChartColumn, px(24.), t.muted_foreground))
                .child(
                    div()
                        .text_size(metrics::TEXT_SM)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(tr!("insights-empty-title")),
                )
                .child(
                    div()
                        .max_w(px(320.))
                        .text_size(metrics::TEXT_BASE)
                        .text_color(t.muted_foreground)
                        .child(tr!("insights-empty-body")),
                )
                .into_any_element();
        }
        let Some(stats) = &self.stats else {
            return div()
                .py(px(48.))
                .flex()
                .justify_center()
                .when(self.loading, |d| {
                    d.child(Spinner::new().small().color(t.muted_foreground))
                })
                .into_any_element();
        };
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .px(px(32.))
            .py(px(24.))
            .child(self.render_stat_cards(stats, cx))
            .child(self.render_words_chart(stats, cx))
            .child(self.render_where(stats, window, cx))
            .child(
                // React's grid-cols-[3fr_2fr]: both cards as tall as the taller.
                div()
                    .flex()
                    .gap(px(16.))
                    .child(column(3.).child(self.render_heat_map(stats, cx)))
                    .child(column(2.).child(self.render_length_curve(stats, cx))),
            )
            .into_any_element()
    }
}

impl Render for InsightsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        div()
            .size_full()
            .flex()
            .overflow_hidden()
            .bg(t.background)
            .text_color(t.foreground)
            .font_family(t.sans_font())
            .child(self.render_app_list(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(self.render_header(cx))
                    .child(
                        div()
                            .id("insights-body")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .child(self.render_body(window, cx)),
                    ),
            )
    }
}

/// A column taking `weight` shares of a row's width.
fn column(weight: f32) -> Div {
    let mut column = div().min_w_0().flex().flex_col();
    let style = column.style();
    style.flex_grow = Some(weight);
    style.flex_shrink = Some(1.);
    style.flex_basis = Some(relative(0.).into());
    column
}

/// An app's name as the list shows it.
pub(crate) fn display_name(app: &CaptureAppCount) -> String {
    app.app_name
        .clone()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| app.app_bundle_id.clone())
}

fn period_key(period: UsagePeriod) -> &'static str {
    match period {
        UsagePeriod::Today => "insights-period-today",
        UsagePeriod::Days30 => "insights-period-30d",
        UsagePeriod::All => "insights-period-all",
        _ => "insights-period-7d",
    }
}

/// Listens for new and changed captures and refreshes, reconnecting while
/// the page is open.
fn listen(client: &ApiClient, cancel: &SseCancel, cx: &mut Context<InsightsPage>) -> Task<()> {
    let (sender, receiver) = mpsc::channel::<CaptureEvent>();
    let client = client.clone();
    let cancel = cancel.clone();
    std::thread::Builder::new()
        .name("kass-insights-events".into())
        .spawn(move || {
            while !cancel.is_cancelled() {
                if let Ok(stream) = client.capture_events(&cancel) {
                    for event in stream {
                        let Ok(event) = event else { break };
                        if sender.send(CaptureEvent::from(event)).is_err() {
                            return;
                        }
                    }
                }
                for _ in 0..30 {
                    if cancel.is_cancelled() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        })
        .ok();
    cx.spawn(async move |page, cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            let mut changed = false;
            loop {
                match receiver.try_recv() {
                    Ok(CaptureEvent::Changed) => changed = true,
                    Ok(_) => {}
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return,
                }
            }
            if changed && page.update(cx, |page, cx| page.refresh(cx)).is_err() {
                return;
            }
        }
    })
}
