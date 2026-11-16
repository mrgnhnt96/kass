//! The always-on status line at the bottom of the main window: server,
//! the shared Mac's sync, microphone, permissions and what Kass has learned.
//! Each item opens where it's configured and says more on hover.

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_api::types::HealthResponse;
use kass_ui::{metrics, theme};

use super::data::{Health, SharedSync, ShellData};

pub const HEIGHT: f32 = 30.;

/// "2h 14m": how long something has been running, to the minute.
pub fn format_duration(seconds: f64) -> String {
    let minutes = (seconds / 60.).floor().max(0.) as u64;
    let days = minutes / 1440;
    let hours = (minutes % 1440) / 60;
    let rest = minutes % 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {rest}m")
    } else if minutes > 0 {
        format!("{rest}m")
    } else {
        "less than a minute".into()
    }
}

pub fn format_memory(mb: u64) -> String {
    if mb >= 1024 {
        format!("{:.1} GB", mb as f64 / 1024.)
    } else {
        format!("{mb} MB")
    }
}

/// What the server reports about itself, in the order the status bar tip
/// and Settings show it. `now` is in Unix seconds, for the running time.
pub fn server_stats(health: &HealthResponse, now: f64) -> Vec<(&'static str, String)> {
    let mut stats = Vec::new();
    if let Some(started) = health.started_at {
        stats.push(("Running for", format_duration(now - started)));
    }
    if let Some(version) = &health.version {
        stats.push(("Version", version.clone()));
    }
    let whisper = if health.model_loaded {
        format!("{} in memory", health.model_size.as_deref().unwrap_or(""))
            .trim()
            .to_string()
    } else {
        "not loaded".into()
    };
    stats.push(("Whisper", whisper));
    if let Some(gpu) = &health.gpu_type {
        stats.push(("Acceleration", gpu.clone()));
    }
    if let Some(mb) = health.peak_memory_mb {
        stats.push(("Peak memory", format_memory(mb)));
    }
    if let Some(pid) = health.pid {
        stats.push(("Process", pid.to_string()));
    }
    stats
}

pub fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.)
}

fn server_details(health: &HealthResponse, now: f64) -> String {
    let mut lines: Vec<String> = server_stats(health, now)
        .into_iter()
        .map(|(label, value)| format!("{label}: {value}"))
        .collect();
    lines.push("Click for server settings.".into());
    lines.join("\n")
}

/// The other Mac's name, like studio.local, from its server URL.
pub fn mac_name(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    let host = match host.rsplit_once(':') {
        Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => name,
        _ => host,
    };
    if host.is_empty() {
        url.to_string()
    } else {
        host.to_string()
    }
}

fn changes(count: u64) -> String {
    format!("{count} {}", if count == 1 { "change" } else { "changes" })
}

/// "on this Mac · 3 to sync" while the other Mac is away, "syncing 3" once it's back.
pub fn sync_label(sync: &SharedSync) -> String {
    if !sync.away {
        format!("syncing {}", sync.pending)
    } else if sync.pending > 0 {
        format!("on this Mac · {} to sync", sync.pending)
    } else {
        "on this Mac".into()
    }
}

pub fn sync_details(sync: &SharedSync) -> String {
    let mac = mac_name(&sync.remote);
    if !sync.away {
        return format!("Sending {} made on this Mac to {mac}.", changes(sync.pending));
    }
    let waiting = if sync.pending > 0 {
        format!(" {} will sync when it's back.", changes(sync.pending))
    } else {
        " What you dictate will sync when it’s back.".into()
    };
    format!("Can't reach {mac}, so Kass is dictating on this Mac.{waiting}")
}

/// Where the status bar's items lead.
#[derive(Clone, Copy)]
pub enum Target {
    Path(&'static str),
    Accessibility,
    InputMonitoring,
}

/// The status line. `go` opens what an item is about.
pub fn bar(
    data: &ShellData,
    go: impl Fn(Target, &mut Window, &mut App) + Clone + 'static,
    cx: &App,
) -> impl IntoElement {
    let t = theme(cx);
    let (state, dot) = match &data.health {
        Health::Online(_) => ("online", t.success),
        Health::Offline => ("offline", t.destructive),
        Health::Connecting => ("connecting", t.accent),
    };
    let server_tip = match &data.health {
        Health::Online(health) => server_details(health, unix_now()),
        _ => format!("Server {state}"),
    };
    let mic = data
        .settings
        .as_ref()
        .and_then(|s| s.input_device_id.as_deref())
        .and_then(|id| data.devices.iter().find(|d| d.id == id))
        .map_or_else(|| "system default".to_string(), |d| d.name.clone());

    let item = |id: &'static str, tip: String, target: Target| {
        let go = go.clone();
        let hover = t.foreground.opacity(0.06);
        let fg = t.foreground;
        div()
            .id(id)
            .flex()
            .items_center()
            .h(px(22.))
            .px(px(6.))
            .rounded(metrics::RADIUS_SM)
            .whitespace_nowrap()
            .cursor_pointer()
            .hover(move |s| s.bg(hover).text_color(fg))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_click(move |_, window, cx| go(target, window, cx))
    };

    let mut bar = div()
        .h(px(HEIGHT))
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(8.))
        .border_t_1()
        .border_color(t.border)
        .bg(t.sidebar)
        .font_family(t.mono_font.clone())
        .text_size(metrics::TEXT_CAPTION)
        .text_color(t.muted_foreground)
        .child(
            item("status-server", server_tip, Target::Path("/settings")).child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().size(px(7.)).rounded_full().bg(dot))
                    .child(format!("server {state}")),
            ),
        );
    if let Some(sync) = data.shared_sync().filter(|s| s.away || s.pending > 0) {
        let accent = t.accent;
        bar = bar.child(
            item("status-sync", sync_details(&sync), Target::Path("/settings")).child(
                div()
                    .when(sync.away, |d| d.text_color(accent))
                    .child(sync_label(&sync)),
            ),
        );
    }
    bar = bar.child(
        item(
            "status-mic",
            format!("Microphone: {mic}. Click to choose another."),
            Target::Path("/settings/dictation"),
        )
        .min_w_0()
        .child(div().truncate().child(format!("mic: {mic}"))),
    );
    for (id, label, granted, purpose, target) in [
        (
            "status-accessibility",
            "accessibility",
            data.accessibility,
            "Lets Kass type the text into the app you're using.",
            Target::Accessibility,
        ),
        (
            "status-input-monitoring",
            "input monitoring",
            data.input_monitoring,
            "Lets Kass hear the dictation shortcut in any app.",
            Target::InputMonitoring,
        ),
    ] {
        let tip = format!(
            "{purpose} {} Click to open it in System Settings.",
            if granted { "Granted." } else { "Not granted." }
        );
        let (mark, color) = if granted {
            ("✓", t.success)
        } else {
            ("✗", t.destructive)
        };
        bar = bar.child(
            item(id, tip, target)
                .child(label)
                .child(div().ml(px(8.)).text_color(color).child(mark)),
        );
    }
    bar = bar.child(div().flex_1());
    if data.examples > 0 {
        let noun = if data.examples == 1 { "example" } else { "examples" };
        bar = bar.child(
            item(
                "status-examples",
                "Examples from your corrections that cleanup learns from.".into(),
                Target::Path("/settings/writing-style"),
            )
            .child(format!("{} {noun} learned", data.examples)),
        );
    }
    bar
}

#[cfg(test)]
mod tests {
    use super::{
        format_duration, format_memory, mac_name, server_stats, sync_details, sync_label,
    };
    use crate::shell::data::SharedSync;
    use kass_api::types::HealthResponse;

    #[test]
    fn durations_read_like_the_page() {
        assert_eq!(format_duration(30.), "less than a minute");
        assert_eq!(format_duration(-5.), "less than a minute");
        assert_eq!(format_duration(5. * 60.), "5m");
        assert_eq!(format_duration(2. * 3600. + 14. * 60.), "2h 14m");
        assert_eq!(format_duration(3. * 86400. + 5. * 3600.), "3d 5h");
    }

    #[test]
    fn memory_switches_to_gigabytes() {
        assert_eq!(format_memory(512), "512 MB");
        assert_eq!(format_memory(1536), "1.5 GB");
    }

    #[test]
    fn stats_skip_what_the_server_left_out() {
        let health = HealthResponse {
            status: "healthy".into(),
            model_loaded: true,
            model_downloaded: None,
            model_size: Some("turbo".into()),
            gpu_available: true,
            gpu_type: None,
            backend_type: None,
            version: None,
            started_at: Some(1_000.),
            pid: Some(42),
            peak_memory_mb: None,
        };
        assert_eq!(
            server_stats(&health, 1_000. + 3_600.),
            vec![
                ("Running for", "1h 0m".to_string()),
                ("Whisper", "turbo in memory".to_string()),
                ("Process", "42".to_string()),
            ]
        );
    }

    #[test]
    fn names_the_other_mac_by_host() {
        assert_eq!(mac_name("http://studio.local:17493"), "studio.local");
        assert_eq!(mac_name("http://192.168.1.4:17493/"), "192.168.1.4");
        assert_eq!(mac_name("not a url"), "not a url");
    }

    #[test]
    fn sync_reads_like_the_page() {
        let away = SharedSync {
            remote: "http://studio.local:17493".into(),
            away: true,
            pending: 1,
        };
        assert_eq!(sync_label(&away), "on this Mac · 1 to sync");
        assert_eq!(
            sync_details(&away),
            "Can't reach studio.local, so Kass is dictating on this Mac. 1 change will sync when it's back."
        );
        let back = SharedSync {
            away: false,
            pending: 3,
            ..away
        };
        assert_eq!(sync_label(&back), "syncing 3");
        assert_eq!(
            sync_details(&back),
            "Sending 3 changes made on this Mac to studio.local."
        );
    }
}
