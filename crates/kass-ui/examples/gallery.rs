//! Every Kass control in one window, to check them by eye.
//!
//! `cargo run -p kass-ui --example gallery` (KASS_GALLERY_THEME=dark for the
//! dark palette).

use gpui_kit::assets::IconName;
use gpui_kit::component::input::InputState;
use gpui_kit::component::select::SelectState;
use gpui_kit::prelude::*;
use gpui_kit::*;
use kass_ui::*;
use std::rc::Rc;

actions!(gallery, [Quit]);

struct Gallery {
    tab: SharedString,
    sound: bool,
    paste: bool,
    name: Entity<InputState>,
    language: Entity<SelectState<SelectOptions<&'static str>>>,
}

impl Gallery {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Add a word"));
        let language = cx.new(|cx| {
            select_state(
                vec![
                    SelectOption::new("en", "English"),
                    SelectOption::new("es", "Spanish"),
                    SelectOption::new("de", "German"),
                ],
                Some(&"en"),
                window,
                cx,
            )
        });
        cx.observe_window_appearance(window, |_, window, cx| {
            system_appearance_changed(window.appearance(), cx);
        })
        .detach();
        Self {
            tab: "settings".into(),
            sound: true,
            paste: false,
            name,
            language,
        }
    }
}

impl Render for Gallery {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx).clone();
        let this = cx.entity().downgrade();
        let tab = |id: &'static str, label: &'static str, icon: IconName| {
            let this = this.clone();
            SidebarTab::new(id, label, icon).on_click(move |_, cx| {
                let _ = this.update(cx, |this, cx| {
                    this.tab = id.into();
                    cx.notify();
                });
            })
        };
        let sidebar = Sidebar::new()
            .logo(div().size_full().rounded(metrics::RADIUS_LG).bg(t.accent))
            .tab(tab("captures", "Captures", IconName::Captions))
            .tab(tab("insights", "Insights", IconName::ChartColumn))
            .tab(tab("models", "Models", IconName::Box).alert(Some(TabAlert {
                level: AlertLevel::Warning,
                messages: vec!["The cleanup model isn't downloaded.".into()],
            })))
            .tab(tab("settings", "Settings", IconName::SlidersHorizontal))
            .active(self.tab.clone())
            .footer(SidebarFooter::Version("0.10.0-beta.7".into()));

        let buttons = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(Button::new("default").label("Save"))
            .child(Button::new("destructive").destructive().label("Delete"))
            .child(Button::new("outline").outline().label("Cancel"))
            .child(Button::new("secondary").secondary().label("Secondary"))
            .child(Button::new("ghost").ghost().label("Ghost"))
            .child(Button::new("link").link().label("Learn more"))
            .child(
                Button::new("small")
                    .small()
                    .outline()
                    .icon(IconName::Check)
                    .label("Small"),
            )
            .child(Button::new("large").large().label("Large"))
            .child(
                Button::new("icon")
                    .size(ButtonSize::Icon)
                    .ghost()
                    .icon(IconName::X)
                    .tooltip("Close"),
            )
            .child(Button::new("loading").loading(true).label("Saving"))
            .child(Button::new("disabled").disabled(true).label("Disabled"));

        let entity = cx.entity();
        let toggle = |id: &'static str, value: bool, set: fn(&mut Gallery, bool)| {
            let entity = entity.clone();
            Toggle::new(id).checked(value).on_change(move |on, _, cx| {
                let on = *on;
                entity.update(cx, |this, cx| {
                    set(this, on);
                    cx.notify();
                });
            })
        };

        let settings = SettingSection::new("Dictation")
            .description("How Kass listens and where your words go.")
            .child(
                SettingRow::new("Play sounds")
                    .description("A soft click when dictation starts and stops.")
                    .action(toggle("sound", self.sound, |this, on| this.sound = on)),
            )
            .child(
                SettingRow::new("Paste automatically")
                    .description("Type the result into the app you're using.")
                    .action(toggle("paste", self.paste, |this, on| this.paste = on)),
            )
            .child(
                SettingRow::new("Language")
                    .action(div().w(px(160.)).child(select(&self.language, cx))),
            )
            .child(
                SettingRow::new("Dictionary")
                    .description("Words Kass should always spell your way.")
                    .child(text_input(&self.name, cx)),
            )
            .child(SettingRow::new("Disabled").action(Toggle::new("off").disabled(true)));

        let overlays =
            div()
                .flex()
                .gap_2()
                .child(
                    Button::new("open-dialog")
                        .outline()
                        .label("Open dialog")
                        .on_click(|_, window, cx| {
                            open_confirm(
                                window,
                                cx,
                                Confirm {
                                    title: "Delete this capture?".into(),
                                    description: Some(
                                        "Its audio and text are removed from this Mac.".into(),
                                    ),
                                    confirm: "Delete".into(),
                                    cancel: "Cancel".into(),
                                    destructive: true,
                                    on_confirm: Rc::new(|window, cx| {
                                        show_toast(window, cx, Toast::new("Capture deleted"));
                                    }),
                                },
                            )
                        }),
                )
                .child(Button::new("toast").outline().label("Show toast").on_click(
                    |_, window, cx| {
                        show_toast(
                            window,
                            cx,
                            Toast::new("Couldn't reach the server")
                                .description("Kass will try again in a few seconds.")
                                .kind(ToastKind::Destructive)
                                .action("Retry", |_, _| {}),
                        )
                    },
                ));

        div()
            .size_full()
            .flex()
            .bg(t.background)
            .text_color(t.foreground)
            .font_family(t.sans_font())
            .text_size(metrics::TEXT_BASE)
            .child(sidebar)
            .child(
                div()
                    .id("content")
                    .flex_1()
                    .overflow_y_scroll()
                    .p(px(32.))
                    .flex()
                    .flex_col()
                    .gap_6()
                    .child(buttons)
                    .child(div().max_w(px(640.)).child(settings))
                    .child(overlays),
            )
    }
}

fn main() {
    gpui_kit::application().with_assets(Assets).run(|cx| {
        gpui_kit::init(cx);
        kass_ui::init(cx);
        match std::env::var("KASS_GALLERY_THEME").as_deref() {
            Ok("dark") => set_theme_choice(ThemeChoice::Dark, cx),
            Ok("light") => set_theme_choice(ThemeChoice::Light, cx),
            _ => {}
        }
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        let bounds = Bounds::centered(None, size(px(980.), px(760.)), cx);
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| Gallery::new(window, cx)),
        )
        .expect("open the gallery window");
        cx.activate(true);
    });
}
