//! Dictation for the Writing Style page: its Dictate buttons start a take
//! here, and every take's state reaches the page, so a teach reply or a
//! style's description can be spoken instead of typed.

use std::cell::RefCell;

use gpui_kit::{App, Subscription};
use kass_writing_style::{OnTake, StyleDictation, TakeTracker};
use serde::Deserialize;

use crate::dictation;
use crate::host::{self, AppHandle, Listener};

#[derive(Default)]
struct Takes {
    tracker: TakeTracker,
    followers: Vec<(u64, OnTake)>,
    next: u64,
    listening: bool,
}

thread_local! {
    // GPUI's main thread only: takes reach it through `host::with_app`.
    static TAKES: RefCell<Takes> = RefCell::new(Takes::default());
}

#[derive(Deserialize)]
struct StateEvent {
    state: String,
    take: u64,
}

pub struct HostDictation;

impl StyleDictation for HostDictation {
    fn start(&self, _: &mut App) {
        TAKES.with_borrow_mut(|takes| takes.tracker.starting());
        let take = dictation::dictation_start(AppHandle);
        TAKES.with_borrow_mut(|takes| takes.tracker.started(take));
    }

    fn stop(&self, _: &mut App) {
        dictation::dictation_stop(AppHandle);
    }

    fn follow(&self, on_take: OnTake, _: &mut App) -> Subscription {
        let id = TAKES.with_borrow_mut(|takes| {
            if !takes.listening {
                takes.listening = true;
                AppHandle.listen("dictation:state", |event| {
                    if let Ok(StateEvent { state, take }) = serde_json::from_str(event.payload()) {
                        host::with_app(move |cx| send(&state, take, cx));
                    }
                });
            }
            takes.next += 1;
            takes.followers.push((takes.next, on_take));
            takes.next
        });
        Subscription::new(move || {
            TAKES.with_borrow_mut(|takes| takes.followers.retain(|(each, _)| *each != id));
        })
    }
}

fn send(state: &str, take: u64, cx: &mut App) {
    let (next, followers) = TAKES.with_borrow_mut(|takes| {
        let next = takes.tracker.event(state, take);
        let followers: Vec<OnTake> = takes.followers.iter().map(|(_, f)| f.clone()).collect();
        (next, followers)
    });
    if let Some(next) = next {
        for follower in followers {
            follower(next, cx);
        }
    }
}

/// The style a link asks for, from a search like `?style=<id>`.
pub fn style_in(search: &str) -> Option<String> {
    search
        .trim_start_matches('?')
        .split('&')
        .find_map(|pair| pair.strip_prefix("style="))
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::style_in;

    #[test]
    fn style_from_search() {
        assert_eq!(style_in("?style=abc"), Some("abc".into()));
        assert_eq!(style_in("?x=1&style=abc"), Some("abc".into()));
        assert_eq!(style_in("?style="), None);
        assert_eq!(style_in(""), None);
    }
}
