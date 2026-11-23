//! Dictation for a phrase's "When I say" box (the Dictionary page's, and
//! the Captures page's phrase dialog): its mic starts a take here, and a
//! shortcut take aimed at the main window is claimed for the box before it
//! would be typed, while the box's page shows it and no other field has
//! focus. Either way the box gets the take's raw transcript, as heard:
//! cleanup may have rewritten it, or swapped in a phrase already in the
//! dictionary.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::{App, AsyncApp, Subscription};
use kass_api::types::{CaptureAppFilter, CaptureResponse};
use kass_dictionary::scopes::spoken_phrase;
use kass_dictionary::{AimsHere, OnTake, PhraseDictation, PhraseTake};
use serde::Deserialize;

use crate::dictation;
use crate::host::{self, AppHandle, Listener};
use crate::shell;

/// Where the page is in the main window.
pub const PATH: &str = "/settings/dictionary";

/// A shortcut take the box follows: the text claimed from it, once it
/// arrives, and whether the take is done.
#[derive(Debug, Default)]
struct Followed {
    text: Option<String>,
    done: bool,
}

/// What the box does next.
#[derive(Debug, PartialEq, Eq)]
enum Next {
    Show(PhraseTake),
    /// The take is over: show it idle, then fill the box from the newest
    /// capture. A shortcut take brings the text that was claimed from it.
    Fill(Option<String>),
}

/// One box's takes: the mic button's, and the shortcut takes it follows.
#[derive(Debug, Default)]
struct Takes {
    /// Set from the click until the take's id is known; its first event can
    /// come before the start call returns.
    pending: bool,
    button: Option<u64>,
    shortcut: HashMap<u64, Followed>,
}

impl Takes {
    fn starting(&mut self) {
        self.pending = true;
        self.button = None;
    }

    fn started(&mut self, take: Option<u64>) {
        self.pending = false;
        if take.is_some() {
            self.button = take;
        }
    }

    /// After `state` for `take`; `aims` is whether a shortcut take that
    /// begins now is meant for the box.
    fn event(&mut self, state: &str, take: u64, aims: bool) -> Option<Next> {
        if self.pending && self.button.is_none() && state == "preparing" {
            self.button = Some(take);
        }
        let from_button = self.button == Some(take);
        if !from_button && state == "preparing" {
            // Takes typed elsewhere are never claimed; let them go.
            self.shortcut.retain(|_, followed| !followed.done);
            if aims {
                self.shortcut.insert(take, Followed::default());
            }
        }
        if !from_button && !self.shortcut.contains_key(&take) {
            return None;
        }
        match state {
            "preparing" | "recording" => Some(Next::Show(PhraseTake::Listening { from_button })),
            "transcribing" | "refining" => Some(Next::Show(PhraseTake::Cleaning)),
            "done" if from_button => {
                self.button = None;
                Some(Next::Fill(None))
            }
            "done" => {
                let followed = self.shortcut.entry(take).or_default();
                match followed.text.take() {
                    Some(text) => {
                        self.shortcut.remove(&take);
                        Some(Next::Fill(Some(text)))
                    }
                    None => {
                        followed.done = true;
                        Some(Next::Show(PhraseTake::Idle))
                    }
                }
            }
            "cancelled" | "error" => {
                if from_button {
                    self.button = None;
                }
                self.shortcut.remove(&take);
                Some(Next::Show(PhraseTake::Idle))
            }
            _ => None,
        }
    }

    /// The box took `text`, a shortcut take's. It fills once the take is
    /// done; a take that began while another field had focus wasn't
    /// followed, so its text is all there is to wait for.
    fn claim(&mut self, take: u64, text: String) -> Option<Next> {
        let done = self
            .shortcut
            .get(&take)
            .is_none_or(|followed| followed.done);
        if done {
            self.shortcut.remove(&take);
            return Some(Next::Fill(Some(text)));
        }
        self.shortcut.insert(
            take,
            Followed {
                text: Some(text),
                done: false,
            },
        );
        None
    }
}

/// The phrase a take said: the newest capture's raw transcript when it's
/// the take's own, else the text claimed from a shortcut take. A shortcut
/// take's capture is the one that delivered its text; before it's saved,
/// the newest capture is an older one.
fn heard(capture: Option<&CaptureResponse>, claimed: Option<&str>) -> String {
    let raw = capture
        .filter(|capture| {
            let delivered = capture
                .transcript_refined
                .as_deref()
                .filter(|text| !text.is_empty())
                .unwrap_or(&capture.transcript_raw);
            claimed.is_none_or(|claimed| spoken_phrase(delivered) == spoken_phrase(claimed))
        })
        .map(|capture| capture.transcript_raw.as_str())
        .filter(|raw| !raw.is_empty());
    spoken_phrase(raw.or(claimed).unwrap_or(""))
}

#[derive(Clone)]
struct Follower {
    id: u64,
    takes: Rc<RefCell<Takes>>,
    aims: AimsHere,
    on_take: OnTake,
}

#[derive(Default)]
struct Followers {
    list: Vec<Follower>,
    next: u64,
    listening: bool,
}

thread_local! {
    // GPUI's main thread only: events reach it through `host::with_app`.
    static FOLLOWERS: RefCell<Followers> = RefCell::new(Followers::default());
}

#[derive(Deserialize)]
struct StateEvent {
    state: String,
    take: u64,
}

/// One per box: the mic's take is its own.
pub struct HostDictation {
    takes: Rc<RefCell<Takes>>,
    /// The main window's page the box is on; it takes no shortcut take
    /// while another shows.
    path: &'static str,
}

impl HostDictation {
    pub fn on(path: &'static str) -> Self {
        Self {
            takes: Rc::default(),
            path,
        }
    }
}

impl Default for HostDictation {
    fn default() -> Self {
        Self::on(PATH)
    }
}

impl PhraseDictation for HostDictation {
    fn start(&self, _: &mut App) {
        self.takes.borrow_mut().starting();
        let take = dictation::dictation_start(AppHandle);
        self.takes.borrow_mut().started(take);
    }

    fn stop(&self, _: &mut App) {
        dictation::dictation_stop(AppHandle);
    }

    fn follow(&self, aims_here: AimsHere, on_take: OnTake, _: &mut App) -> Subscription {
        let takes = self.takes.clone();
        let path = self.path;
        let id = FOLLOWERS.with_borrow_mut(|followers| {
            if !followers.listening {
                followers.listening = true;
                AppHandle.listen("dictation:state", |event| {
                    if let Ok(StateEvent { state, take }) = serde_json::from_str(event.payload()) {
                        host::with_app(move |cx| on_state(&state, take, cx));
                    }
                });
            }
            followers.next += 1;
            followers.list.push(Follower {
                id: followers.next,
                takes,
                // The page keeps its box's aim while it's hidden.
                aims: Rc::new(move |cx| shell::showing(path, cx) && aims_here(cx)),
                on_take,
            });
            followers.next
        });
        Subscription::new(move || {
            FOLLOWERS.with_borrow_mut(|followers| followers.list.retain(|each| each.id != id));
        })
    }
}

fn followers() -> Vec<Follower> {
    FOLLOWERS.with_borrow(|followers| followers.list.clone())
}

fn on_state(state: &str, take: u64, cx: &mut App) {
    for follower in followers() {
        let aims = state == "preparing" && (follower.aims)(cx);
        let next = follower.takes.borrow_mut().event(state, take, aims);
        if let Some(next) = next {
            apply(&follower, next, cx);
        }
    }
}

fn apply(follower: &Follower, next: Next, cx: &mut App) {
    match next {
        Next::Show(step) => (follower.on_take)(step, cx),
        Next::Fill(claimed) => {
            (follower.on_take)(PhraseTake::Idle, cx);
            fill(claimed, follower.on_take.clone(), cx);
        }
    }
}

/// Fills the box from the newest capture: the take just finished.
fn fill(claimed: Option<String>, on_take: OnTake, cx: &mut App) {
    let api = shell::shell_data(cx).read(cx).api.clone();
    let newest = cx.background_executor().spawn(async move {
        api.list_captures(1, 0, &CaptureAppFilter::All)
            .ok()
            .and_then(|list| list.items.into_iter().next())
    });
    cx.spawn(async move |cx: &mut AsyncApp| {
        let phrase = heard(newest.await.as_ref(), claimed.as_deref());
        if !phrase.is_empty() {
            cx.update(|cx| on_take(PhraseTake::Heard(phrase), cx));
        }
    })
    .detach();
}

/// Whether a box takes the text of `take`, a shortcut take aimed at the
/// main window, instead of the field focused there. The newest box decides
/// first.
pub async fn claim(take: u64, text: String) -> bool {
    let (tx, rx) = tokio::sync::oneshot::channel();
    host::with_app(move |cx| {
        let _ = tx.send(claim_now(take, text, cx));
    });
    rx.await.unwrap_or(false)
}

fn claim_now(take: u64, text: String, cx: &mut App) -> bool {
    let Some(follower) = followers()
        .into_iter()
        .rev()
        .find(|follower| (follower.aims)(cx))
    else {
        return false;
    };
    let next = follower.takes.borrow_mut().claim(take, text);
    if let Some(next) = next {
        apply(&follower, next, cx);
    }
    true
}

/// The scope and kind a link asks for, from a search like
/// `?scope=app%3Acom.apple.Notes&kind=phrases`.
pub fn link(search: &str) -> (Option<String>, bool) {
    let mut scope = None;
    let mut phrases = false;
    for pair in search.trim_start_matches('?').split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "scope" if !value.is_empty() => scope = Some(decode(value)),
            "kind" => phrases = value == "phrases",
            _ => {}
        }
    }
    (scope, phrases)
}

/// A query value with its `%XX` escapes and `+` spaces undone.
fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok());
        match (escaped, bytes[i]) {
            (Some(byte), _) => {
                out.push(byte);
                i += 3;
                continue;
            }
            (None, b'+') => out.push(b' '),
            (None, byte) => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{heard, link, Next, Takes};
    use kass_api::types::CaptureResponse;
    use kass_dictionary::PhraseTake;

    fn capture(raw: &str, refined: Option<&str>) -> CaptureResponse {
        serde_json::from_value(serde_json::json!({
            "id": "c1",
            "audio_path": "",
            "source": "dictation",
            "transcript_raw": raw,
            "transcript_refined": refined,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z",
        }))
        .expect("a capture")
    }

    #[test]
    fn the_buttons_take_fills_from_its_capture() {
        let mut takes = Takes::default();
        takes.starting();
        // Its first event can beat the start call's reply.
        assert_eq!(
            takes.event("preparing", 5, false),
            Some(Next::Show(PhraseTake::Listening { from_button: true }))
        );
        takes.started(Some(5));
        assert_eq!(
            takes.event("refining", 5, false),
            Some(Next::Show(PhraseTake::Cleaning))
        );
        assert_eq!(takes.event("done", 5, false), Some(Next::Fill(None)));
        // The next take is a shortcut take, not followed unless aimed here.
        assert_eq!(takes.event("preparing", 6, false), None);
    }

    #[test]
    fn a_shortcut_take_aimed_here_fills_with_its_claimed_text() {
        let mut takes = Takes::default();
        assert_eq!(
            takes.event("preparing", 3, true),
            Some(Next::Show(PhraseTake::Listening { from_button: false }))
        );
        // Claimed before it's done: fills once it is.
        assert_eq!(takes.claim(3, "Sign off.".into()), None);
        assert_eq!(
            takes.event("done", 3, true),
            Some(Next::Fill(Some("Sign off.".into())))
        );
    }

    #[test]
    fn a_claim_after_done_or_for_an_unfollowed_take_fills_now() {
        let mut takes = Takes::default();
        takes.event("preparing", 3, true);
        assert_eq!(
            takes.event("done", 3, true),
            Some(Next::Show(PhraseTake::Idle))
        );
        assert_eq!(
            takes.claim(3, "hi".into()),
            Some(Next::Fill(Some("hi".into())))
        );
        assert_eq!(
            takes.claim(9, "there".into()),
            Some(Next::Fill(Some("there".into())))
        );
    }

    #[test]
    fn a_cancelled_take_is_forgotten() {
        let mut takes = Takes::default();
        takes.event("preparing", 4, true);
        assert_eq!(
            takes.event("cancelled", 4, true),
            Some(Next::Show(PhraseTake::Idle))
        );
        assert_eq!(takes.event("done", 4, true), None);
    }

    #[test]
    fn takes_typed_elsewhere_are_let_go() {
        let mut takes = Takes::default();
        takes.event("preparing", 1, true);
        takes.event("done", 1, true);
        takes.event("preparing", 2, false);
        assert!(takes.shortcut.is_empty());
    }

    #[test]
    fn heard_prefers_the_takes_raw_transcript() {
        let ours = capture("insert my email", Some("Insert my email."));
        assert_eq!(heard(Some(&ours), None), "insert my email");
        assert_eq!(
            heard(Some(&ours), Some("Insert my email.")),
            "insert my email"
        );
        // An older capture: the claimed text stands in.
        let older = capture("something else", None);
        assert_eq!(heard(Some(&older), Some("Sign off!")), "sign off");
        assert_eq!(heard(None, Some("Sign off!")), "sign off");
        assert_eq!(heard(None, None), "");
    }

    #[test]
    fn links_open_a_scope_and_kind() {
        assert_eq!(link(""), (None, false));
        assert_eq!(link("?kind=phrases"), (None, true));
        assert_eq!(
            link("?scope=app%3Acom.apple.Notes&kind=phrases"),
            (Some("app:com.apple.Notes".into()), true)
        );
        assert_eq!(link("?scope=style:abc"), (Some("style:abc".into()), false));
        assert_eq!(link("?scope="), (None, false));
    }
}
