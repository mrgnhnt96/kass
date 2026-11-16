//! The app's button: the React `Button`'s six looks and four sizes.

use crate::theme::{metrics, theme};
use gpui_kit::assets::IconName;
use gpui_kit::component::ThemeStyled as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::rc::Rc;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonVariant {
    /// Mint fill: the main action.
    #[default]
    Default,
    /// Red fill: deleting, discarding.
    Destructive,
    /// Bordered, transparent.
    Outline,
    /// Grey fill.
    Secondary,
    /// No fill until hovered.
    Ghost,
    /// Mint text, underlined on hover.
    Link,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonSize {
    /// 36px tall.
    #[default]
    Default,
    /// 32px tall.
    Small,
    /// 40px tall.
    Large,
    /// 36px square, for an icon alone.
    Icon,
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: Option<SharedString>,
    icon: Option<IconName>,
    variant: ButtonVariant,
    size: ButtonSize,
    disabled: bool,
    loading: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
    style: StyleRefinement,
}

impl Button {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            label: None,
            icon: None,
            variant: ButtonVariant::Default,
            size: ButtonSize::Default,
            disabled: false,
            loading: false,
            tooltip: None,
            on_click: None,
            style: StyleRefinement::default(),
        }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// A 16px icon before the label (or alone, with [`ButtonSize::Icon`]).
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn destructive(self) -> Self {
        self.variant(ButtonVariant::Destructive)
    }

    pub fn outline(self) -> Self {
        self.variant(ButtonVariant::Outline)
    }

    pub fn secondary(self) -> Self {
        self.variant(ButtonVariant::Secondary)
    }

    pub fn ghost(self) -> Self {
        self.variant(ButtonVariant::Ghost)
    }

    pub fn link(self) -> Self {
        self.variant(ButtonVariant::Link)
    }

    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    pub fn small(self) -> Self {
        self.size(ButtonSize::Small)
    }

    pub fn large(self) -> Self {
        self.size(ButtonSize::Large)
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Spins the icon (a spinner if there's none) and ignores clicks.
    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl Styled for Button {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

struct Look {
    bg: Hsla,
    fg: Hsla,
    hover_bg: Hsla,
    border: Option<Hsla>,
}

fn look(variant: ButtonVariant, cx: &App) -> Look {
    let t = theme(cx);
    let clear = transparent_black();
    match variant {
        ButtonVariant::Default => Look {
            bg: t.accent,
            fg: t.accent_foreground,
            hover_bg: t.accent.opacity(0.9),
            border: None,
        },
        ButtonVariant::Destructive => Look {
            bg: t.destructive,
            fg: t.destructive_foreground,
            hover_bg: t.destructive.opacity(0.9),
            border: None,
        },
        ButtonVariant::Outline => Look {
            bg: clear,
            fg: t.foreground,
            hover_bg: t.secondary,
            border: Some(t.input),
        },
        ButtonVariant::Secondary => Look {
            bg: t.secondary,
            fg: t.secondary_foreground,
            hover_bg: t.secondary.opacity(0.8),
            border: None,
        },
        ButtonVariant::Ghost => Look {
            bg: clear,
            fg: t.foreground,
            hover_bg: t.secondary,
            border: None,
        },
        ButtonVariant::Link => Look {
            bg: clear,
            fg: t.accent,
            hover_bg: clear,
            border: None,
        },
    }
}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let look = look(self.variant, cx);
        let focus = window
            .use_keyed_state(self.id.clone(), cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();
        let focused = focus.is_focused(window);
        let interactive = !self.disabled && !self.loading;
        let (height, pad_x) = match self.size {
            ButtonSize::Default => (px(36.), px(14.)),
            ButtonSize::Small => (px(32.), px(12.)),
            ButtonSize::Large => (px(40.), px(20.)),
            ButtonSize::Icon => (px(36.), px(0.)),
        };
        let icon = self
            .icon
            .or(self.loading.then_some(IconName::LoaderCircle))
            .map(|icon| {
                let svg = svg()
                    .path(icon.path())
                    .size(px(16.))
                    .flex_none()
                    .text_color(look.fg);
                if self.loading {
                    svg.with_animation(
                        "spin",
                        Animation::new(Duration::from_millis(900)).repeat(),
                        |svg, delta| {
                            svg.with_transformation(Transformation::rotate(percentage(delta)))
                        },
                    )
                    .into_any_element()
                } else {
                    svg.into_any_element()
                }
            });
        let tooltip = self.tooltip.clone();
        let variant = self.variant;

        let mut button = div()
            .id(self.id)
            .track_focus(&focus)
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap_2()
            .h(height)
            .px(pad_x)
            .when(self.size == ButtonSize::Icon, |b| b.w(height))
            .rounded(metrics::RADIUS_MD)
            .text_size(metrics::TEXT_BASE)
            .font_weight(FontWeight::MEDIUM)
            .whitespace_nowrap()
            .bg(look.bg)
            .text_color(look.fg)
            .when_some(look.border, |b, border| b.border_1().border_color(border))
            .when(self.disabled, |b| b.opacity(0.5))
            .when(interactive, |b| {
                b.cursor_pointer()
                    .hover(move |s| {
                        let s = s.bg(look.hover_bg);
                        if variant == ButtonVariant::Link {
                            s.underline()
                        } else {
                            s
                        }
                    })
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            })
            .when_some(self.on_click.filter(|_| interactive), |b, on_click| {
                b.on_click(move |event, window, cx| on_click(event, window, cx))
            })
            .when_some(tooltip, |b, text| {
                b.tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
            })
            .when(focused, |b| b.focus_ring_style(window, cx))
            .children(icon)
            .children(self.label);
        button.style().refine(&self.style);
        button
    }
}
