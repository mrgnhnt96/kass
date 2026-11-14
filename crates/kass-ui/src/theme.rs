//! Kass's colors and type as GPUI tokens.
//!
//! The palettes are the ones the React app draws with (`app/src/index.css`,
//! "Console": warm paper greys in light, graphite in dark, one mint accent),
//! the same HSL numbers. [`init`] installs them as the global [`KassTheme`]
//! and projects them onto the component library's theme, so its inputs,
//! selects, dialogs and notifications draw in Kass's colors too.

use gpui_kit::component::{Theme as ComponentTheme, ThemeConfig, ThemeMode};
use gpui_kit::*;
use serde_json::json;
use std::ops::Deref;
use std::rc::Rc;

/// Which palette is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
}

/// What the user picked in Settings: a palette, or whatever macOS is using.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
}

/// The app's color tokens. Names follow the React app's CSS variables.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub background: Hsla,
    pub foreground: Hsla,
    pub card: Hsla,
    pub card_foreground: Hsla,
    pub popover: Hsla,
    pub popover_foreground: Hsla,
    pub primary: Hsla,
    pub primary_foreground: Hsla,
    pub secondary: Hsla,
    pub secondary_foreground: Hsla,
    pub muted: Hsla,
    pub muted_foreground: Hsla,
    /// The brand mint: default buttons, switches that are on, links.
    pub accent: Hsla,
    pub accent_foreground: Hsla,
    pub destructive: Hsla,
    pub destructive_foreground: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub border: Hsla,
    /// Borders of inputs, outline buttons, and the track of a switch that's off.
    pub input: Hsla,
    pub ring: Hsla,
    pub sidebar: Hsla,
    pub chart: [Hsla; 5],
}

/// A CSS `hsl(h s% l%)`.
fn hsl(h: f32, s: f32, l: f32) -> Hsla {
    hsla(h / 360., s / 100., l / 100., 1.)
}

impl Palette {
    pub fn light() -> Self {
        Self {
            background: hsl(40., 10., 96.),
            foreground: hsl(220., 10., 10.),
            card: hsl(0., 0., 100.),
            card_foreground: hsl(220., 10., 10.),
            popover: hsl(0., 0., 100.),
            popover_foreground: hsl(220., 10., 10.),
            primary: hsl(154., 70., 42.),
            primary_foreground: hsl(220., 15., 7.),
            secondary: hsl(40., 8., 90.),
            secondary_foreground: hsl(220., 10., 10.),
            muted: hsl(40., 8., 91.),
            muted_foreground: hsl(220., 6., 38.),
            accent: hsl(158., 81., 26.),
            accent_foreground: hsl(0., 0., 100.),
            destructive: hsl(0., 72., 46.),
            destructive_foreground: hsl(0., 0., 100.),
            success: hsl(145., 60., 32.),
            warning: hsl(28., 90., 40.),
            border: hsl(40., 8., 84.),
            input: hsl(40., 8., 80.),
            ring: hsl(158., 81., 26.),
            sidebar: hsl(40., 10., 92.),
            chart: [
                hsl(12., 76., 61.),
                hsl(173., 58., 39.),
                hsl(197., 37., 24.),
                hsl(43., 74., 66.),
                hsl(27., 87., 67.),
            ],
        }
    }

    pub fn dark() -> Self {
        Self {
            background: hsl(220., 11., 6.),
            foreground: hsl(60., 5., 90.),
            card: hsl(216., 12., 8.),
            card_foreground: hsl(60., 5., 90.),
            popover: hsl(216., 10., 10.),
            popover_foreground: hsl(60., 5., 90.),
            primary: hsl(154., 85., 65.),
            primary_foreground: hsl(0., 0., 7.),
            secondary: hsl(216., 11., 13.),
            secondary_foreground: hsl(60., 5., 90.),
            muted: hsl(216., 12., 12.),
            muted_foreground: hsl(218., 6., 60.),
            accent: hsl(154., 85., 65.),
            accent_foreground: hsl(0., 0., 7.),
            destructive: hsl(0., 84., 71.),
            destructive_foreground: hsl(0., 0., 7.),
            success: hsl(142., 69., 58.),
            warning: hsl(27., 96., 72.),
            border: hsl(218., 10., 15.),
            input: hsl(216., 10., 19.),
            ring: hsl(154., 85., 65.),
            sidebar: hsl(220., 12., 5.),
            chart: [
                hsl(220., 70., 50.),
                hsl(160., 60., 45.),
                hsl(30., 80., 55.),
                hsl(280., 65., 60.),
                hsl(340., 75., 55.),
            ],
        }
    }

    pub fn for_appearance(appearance: Appearance) -> Self {
        match appearance {
            Appearance::Light => Self::light(),
            Appearance::Dark => Self::dark(),
        }
    }
}

/// Type and shape. The React app sets 13px body text in the system font
/// (Tailwind's `font-sans`), with SF Mono (`font-mono`) for versions,
/// shortcuts and section headings.
pub mod metrics {
    use gpui_kit::{Pixels, px};

    pub const FONT_SANS: &str = ".SystemUIFont";
    /// Preferred monospace family; [`super::KassTheme::mono_font`] falls back to Menlo.
    pub const FONT_MONO: &str = "SF Mono";
    pub const FONT_MONO_FALLBACK: &str = "Menlo";

    /// Sidebar tab labels and the version (`text-[10px]`).
    pub const TEXT_2XS: Pixels = px(10.);
    /// Monospace section headings and tooltips (`text-[11px]`).
    pub const TEXT_CAPTION: Pixels = px(11.);
    /// Descriptions under a setting (`text-xs`).
    pub const TEXT_XS: Pixels = px(12.);
    /// Body text, buttons, inputs, menus (`text-[13px]`).
    pub const TEXT_BASE: Pixels = px(13.);
    /// Setting titles, dialog descriptions (`text-sm`).
    pub const TEXT_SM: Pixels = px(14.);
    /// Dialog titles (`text-lg`).
    pub const TEXT_LG: Pixels = px(18.);

    pub const LINE_XS: Pixels = px(16.);
    pub const LINE_BASE: Pixels = px(19.5);
    pub const LINE_SM: Pixels = px(20.);
    pub const LINE_LG: Pixels = px(28.);

    /// `rounded-sm`: menu items.
    pub const RADIUS_SM: Pixels = px(4.);
    /// `rounded-md`: buttons, inputs, selects, toasts, popovers.
    pub const RADIUS_MD: Pixels = px(6.);
    /// `rounded-lg`: sidebar tabs.
    pub const RADIUS_LG: Pixels = px(8.);
    /// `rounded-xl`: dialogs.
    pub const RADIUS_XL: Pixels = px(12.);
}

/// The theme every Kass view draws with, kept as a global.
#[derive(Clone, Debug)]
pub struct KassTheme {
    pub choice: ThemeChoice,
    pub appearance: Appearance,
    pub palette: Palette,
    /// The monospace family that's installed: SF Mono, else Menlo.
    pub mono_font: SharedString,
}

impl Global for KassTheme {}

impl Deref for KassTheme {
    type Target = Palette;

    fn deref(&self) -> &Palette {
        &self.palette
    }
}

impl KassTheme {
    pub fn global(cx: &App) -> &KassTheme {
        cx.global::<KassTheme>()
    }

    pub fn is_dark(&self) -> bool {
        self.appearance == Appearance::Dark
    }

    pub fn sans_font(&self) -> SharedString {
        metrics::FONT_SANS.into()
    }
}

/// The current theme, for `render`: `let t = theme(cx); div().bg(t.background)`.
pub fn theme(cx: &App) -> &KassTheme {
    KassTheme::global(cx)
}

/// Install the theme, following macOS's appearance until [`set_theme_choice`]
/// says otherwise. Call once after `gpui_kit::init`.
pub fn init(cx: &mut App) {
    let mono_font: SharedString = if cx
        .text_system()
        .all_font_names()
        .iter()
        .any(|name| name == metrics::FONT_MONO)
    {
        metrics::FONT_MONO.into()
    } else {
        metrics::FONT_MONO_FALLBACK.into()
    };
    let appearance = system_appearance(cx.window_appearance());
    cx.set_global(KassTheme {
        choice: ThemeChoice::System,
        appearance,
        palette: Palette::for_appearance(appearance),
        mono_font,
    });
    apply_to_components(cx);
}

/// Follow the user's choice; with [`ThemeChoice::System`], macOS decides.
pub fn set_theme_choice(choice: ThemeChoice, cx: &mut App) {
    cx.global_mut::<KassTheme>().choice = choice;
    let appearance = match choice {
        ThemeChoice::System => system_appearance(cx.window_appearance()),
        ThemeChoice::Light => Appearance::Light,
        ThemeChoice::Dark => Appearance::Dark,
    };
    set_appearance(appearance, cx);
}

/// Call when macOS switches between light and dark (a window's
/// `observe_window_appearance`); a no-op unless the choice is System.
pub fn system_appearance_changed(appearance: WindowAppearance, cx: &mut App) {
    if KassTheme::global(cx).choice == ThemeChoice::System {
        set_appearance(system_appearance(appearance), cx);
    }
}

fn system_appearance(appearance: WindowAppearance) -> Appearance {
    match appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => Appearance::Dark,
        WindowAppearance::Light | WindowAppearance::VibrantLight => Appearance::Light,
    }
}

fn set_appearance(appearance: Appearance, cx: &mut App) {
    let theme = cx.global_mut::<KassTheme>();
    if theme.appearance == appearance {
        return;
    }
    theme.appearance = appearance;
    theme.palette = Palette::for_appearance(appearance);
    apply_to_components(cx);
}

/// Installs both Kass palettes as the component library's light and dark
/// themes, current one last, so the library draws in Kass's colors even if it
/// switches modes on its own.
fn apply_to_components(cx: &mut App) {
    let theme = KassTheme::global(cx);
    let current = theme.appearance;
    let other = match current {
        Appearance::Light => Appearance::Dark,
        Appearance::Dark => Appearance::Light,
    };
    let [other, current] = [other, current].map(|appearance| {
        Rc::new(component_config(
            appearance,
            &Palette::for_appearance(appearance),
            &theme.mono_font,
        ))
    });
    ComponentTheme::update(cx, |component| {
        component.apply_config(&other);
        component.apply_config(&current);
        // The React app's toasts sit bottom right, up to 420px wide.
        component.notification.placement = Anchor::BottomRight;
        component.notification.width = px(420.);
    });
}

/// `#rrggbbaa`, the form the component library's theme files take.
fn hex(color: Hsla) -> String {
    let rgba = Rgba::from(color);
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        byte(rgba.r),
        byte(rgba.g),
        byte(rgba.b),
        byte(rgba.a)
    )
}

/// The component library's theme in Kass's colors. Its names follow shadcn,
/// where "accent" is a hover tint and "primary" the call to action; Kass's
/// CSS uses `accent` for its call to action, so that's what primary gets.
pub fn component_config(appearance: Appearance, p: &Palette, mono_font: &str) -> ThemeConfig {
    let alpha = |color: Hsla, a: f32| color.alpha(a);
    let (mode, name) = match appearance {
        Appearance::Light => (ThemeMode::Light, "Kass Light"),
        Appearance::Dark => (ThemeMode::Dark, "Kass Dark"),
    };
    let colors: serde_json::Map<String, serde_json::Value> = [
        ("background", hex(p.background)),
        ("foreground", hex(p.foreground)),
        ("border", hex(p.border)),
        ("input.border", hex(p.input)),
        ("ring", hex(p.ring)),
        ("caret", hex(p.foreground)),
        ("selection.background", hex(alpha(p.accent, 0.3))),
        ("accent.background", hex(p.secondary)),
        ("accent.foreground", hex(p.foreground)),
        ("primary.background", hex(p.accent)),
        ("primary.hover.background", hex(alpha(p.accent, 0.9))),
        ("primary.active.background", hex(alpha(p.accent, 0.8))),
        ("primary.foreground", hex(p.accent_foreground)),
        ("secondary.background", hex(p.secondary)),
        ("secondary.hover.background", hex(alpha(p.secondary, 0.8))),
        ("secondary.active.background", hex(p.muted)),
        ("secondary.foreground", hex(p.secondary_foreground)),
        ("danger.background", hex(p.destructive)),
        ("danger.hover.background", hex(alpha(p.destructive, 0.9))),
        ("danger.active.background", hex(alpha(p.destructive, 0.8))),
        ("danger.foreground", hex(p.destructive_foreground)),
        ("success.background", hex(p.success)),
        ("success.foreground", hex(p.background)),
        ("warning.background", hex(p.warning)),
        ("warning.foreground", hex(p.background)),
        ("muted.background", hex(p.muted)),
        ("muted.foreground", hex(p.muted_foreground)),
        ("popover.background", hex(p.popover)),
        ("popover.foreground", hex(p.popover_foreground)),
        ("list.background", hex(p.popover)),
        ("list.hover.background", hex(p.secondary)),
        ("list.active.background", hex(p.secondary)),
        ("list.active.border", hex(p.ring)),
        ("link", hex(p.accent)),
        ("link.hover", hex(p.accent)),
        ("link.active", hex(p.accent)),
        ("switch.background", hex(p.input)),
        ("switch.thumb.background", hex(p.muted_foreground)),
        ("sidebar.background", hex(p.sidebar)),
        ("sidebar.foreground", hex(p.muted_foreground)),
        ("sidebar.border", hex(p.border)),
        ("sidebar.accent.background", hex(p.muted)),
        ("sidebar.accent.foreground", hex(p.foreground)),
        ("sidebar.primary.background", hex(p.accent)),
        ("sidebar.primary.foreground", hex(p.accent_foreground)),
        ("overlay", hex(hsla(0., 0., 0., 0.7))),
        ("progress_bar.background", hex(p.accent)),
        ("chart.1", hex(p.chart[0])),
        ("chart.2", hex(p.chart[1])),
        ("chart.3", hex(p.chart[2])),
        ("chart.4", hex(p.chart[3])),
        ("chart.5", hex(p.chart[4])),
        ("scrollbar.background", "#00000000".to_string()),
        (
            "scrollbar.thumb.background",
            hex(alpha(p.muted_foreground, 0.5)),
        ),
        ("scrollbar.thumb.hover.background", hex(p.muted_foreground)),
        ("window.border", hex(p.border)),
    ]
    .into_iter()
    .map(|(key, color)| (key.to_string(), color.into()))
    .collect();
    let value = json!({
        "name": name,
        "mode": if mode.is_dark() { "dark" } else { "light" },
        "font.family": metrics::FONT_SANS,
        "font.size": 13.0,
        "mono_font.family": mono_font,
        "mono_font.size": 12.0,
        "radius": 6,
        "radius.lg": 12,
        "shadow": false,
        "colors": colors,
    });
    serde_json::from_value(value).expect("the Kass theme is a valid theme config")
}

#[cfg(test)]
mod tests {
    use super::{Appearance, Palette, component_config, hex, hsl};

    #[test]
    fn hex_matches_the_css_colors() {
        assert_eq!(hex(hsl(0., 0., 100.)), "#ffffffff");
        // Light accent, hsl(158 81% 26%), is rgb(13 120 81).
        assert_eq!(hex(Palette::light().accent), "#0d7851ff");
    }

    #[test]
    fn both_palettes_make_a_component_theme() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let config =
                component_config(appearance, &Palette::for_appearance(appearance), "Menlo");
            assert_eq!(config.mode.is_dark(), appearance == Appearance::Dark);
            assert!(config.colors.background.is_some());
            assert!(config.colors.primary.is_some());
        }
    }
}
