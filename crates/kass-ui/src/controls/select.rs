//! Dropdowns, in the component library's select, sized like the React
//! `SelectTrigger`: 32px tall, white (card) fill, 13px text.

use crate::theme::{metrics, theme};
use gpui_kit::component::IndexPath;
use gpui_kit::component::searchable_list::SearchableListItem;
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::*;

/// One choice: what it stands for and what it says.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectOption<V: Clone + PartialEq> {
    pub value: V,
    pub label: SharedString,
}

impl<V: Clone + PartialEq> SelectOption<V> {
    pub fn new(value: V, label: impl Into<SharedString>) -> Self {
        Self {
            value,
            label: label.into(),
        }
    }
}

impl<V: Clone + PartialEq> SearchableListItem for SelectOption<V> {
    type Value = V;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &V {
        &self.value
    }
}

pub type SelectOptions<V> = Vec<SelectOption<V>>;

/// The state behind a [`select`], with `selected` chosen if it's one of `options`.
pub fn select_state<V: Clone + PartialEq + 'static>(
    options: SelectOptions<V>,
    selected: Option<&V>,
    window: &mut Window,
    cx: &mut Context<SelectState<SelectOptions<V>>>,
) -> SelectState<SelectOptions<V>> {
    let index = selected
        .and_then(|value| options.iter().position(|option| &option.value == value))
        .map(IndexPath::new);
    SelectState::new(options, index, window, cx)
}

/// A dropdown over `state` (made with [`select_state`]); subscribe to the
/// state's `SelectEvent::Confirm` to hear about changes.
pub fn select<V: Clone + PartialEq + 'static>(
    state: &Entity<SelectState<SelectOptions<V>>>,
    cx: &App,
) -> Select<SelectOptions<V>> {
    let t = theme(cx);
    Select::new(state)
        .h(px(32.))
        .rounded(metrics::RADIUS_MD)
        .bg(t.card)
        .text_size(metrics::TEXT_BASE)
}
