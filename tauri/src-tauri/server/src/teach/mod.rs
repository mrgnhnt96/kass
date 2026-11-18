//! Teaching a writing style by replying (docs/plans/TEACH_BY_REPLYING.md):
//! short made-up conversations the user answers, open in memory until they
//! finish. This server holds the sessions and writes each prompt; the worker
//! only generates the other side's next message. Python:
//! `backend/services/teach.py`.
//!
//! While a session is open, dictation in Kass's own window uses the style
//! being taught ([`Sessions::teaching_style`]).

pub mod catalog;
pub mod preview;
pub mod text;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use chrono::{NaiveDateTime, Utc};
use kass_text::py;
use kass_text::refinement::strip_stt_artifacts;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::proxy::Proxy;
use crate::styles::{Snapshot, KASS_BUNDLE};
use crate::worker;
use text::{Chip, Note};

pub const TARGET_REPLIES: usize = 10;
pub const MAX_CONVERSATIONS: usize = 8;
pub const MAX_REPLY_CHARS: usize = 4000;
pub const SESSION_TTL: Duration = Duration::from_secs(2 * 60 * 60);
const STARTING_CONVERSATIONS: usize = 3;
/// The prompt cache every teaching turn continues.
pub const PROMPT_CACHE_KEY: &str = "teach";
/// How captures store `created_at` (naive UTC).
const STORED: &str = "%Y-%m-%d %H:%M:%S%.6f";

struct Reply {
    written: String,
    said: Option<String>,
    shown: Option<String>,
    chips: Vec<Chip>,
}

struct Conversation {
    id: String,
    kind: &'static str,
    persona: &'static str,
    relation: &'static str,
    title: Option<&'static str>,
    /// (from_you, text), oldest first.
    messages: Vec<(bool, String)>,
    note: Option<Note>,
    seeds_used: Vec<&'static str>,
    /// Captures from before this aren't part of the current reply.
    turn_started_at: NaiveDateTime,
    tricks: Vec<&'static str>,
    replies: Vec<Reply>,
    wrapped: bool,
    turn: usize,
}

struct Session {
    id: String,
    style: String,
    conversations: Vec<Conversation>,
    suggested_kinds: Vec<String>,
    terms: Vec<String>,
    touched: Instant,
    /// Captures already part of a reply; each belongs to one.
    used_captures: HashSet<String>,
}

/// Why a teach request can't go ahead.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// The session ended or was never there.
    Expired,
    NoConversation,
    /// A 400, with the words the app shows.
    Invalid(String),
}

/// What was said and shown in Kass's window for a reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dictated {
    pub said: String,
    pub shown: String,
    pub capture_ids: Vec<String>,
}

/// The other side's next turn, as the model is asked for it.
pub struct Turn {
    pub system: String,
    pub prompt: String,
    pub trick: &'static str,
    /// The conversation so far, to catch a repeated message.
    pub messages: Vec<(bool, String)>,
}

/// A finished session: the style it taught and its replies as profile
/// examples.
pub struct Finished {
    pub style: String,
    pub examples: Vec<Value>,
}

fn now() -> NaiveDateTime {
    Utc::now().naive_utc()
}

/// The open teach sessions, by id.
#[derive(Default)]
pub struct Sessions {
    open: Mutex<HashMap<String, Session>>,
    /// Held while the worker is told the style being taught, so the latest
    /// one always lands last.
    telling: tokio::sync::Mutex<()>,
}

impl Sessions {
    /// The open sessions, without the ones left alone past [`SESSION_TTL`].
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Session>> {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        open.retain(|_, session| session.touched.elapsed() < SESSION_TTL);
        open
    }

    /// The style of the newest open session, which dictation in Kass's own
    /// window uses, and how long it stays open untouched.
    fn newest(&self) -> Option<(String, Duration)> {
        self.lock()
            .values()
            .max_by_key(|session| session.touched)
            .map(|session| {
                (
                    session.style.clone(),
                    SESSION_TTL.saturating_sub(session.touched.elapsed()),
                )
            })
    }

    pub fn teaching_style(&self) -> Option<String> {
        self.newest().map(|(style, _)| style)
    }

    /// Tell the worker the style being taught now, in the background: it
    /// still answers some styles itself.
    pub fn tell_worker(self: &Arc<Self>, proxy: &Proxy) {
        let sessions = Arc::clone(self);
        let proxy = proxy.clone();
        tokio::spawn(async move {
            let _held = sessions.telling.lock().await;
            let (style, left) = sessions.newest().unzip();
            let left = left.map_or(0.0, |left| left.as_secs_f64());
            worker::teaching(&proxy, style.as_deref(), left).await;
        });
    }

    /// Run `work` on an open session, which counts as touching it.
    fn with<T>(
        &self,
        session_id: &str,
        work: impl FnOnce(&mut Session) -> Result<T, Refused>,
    ) -> Result<T, Refused> {
        let mut open = self.lock();
        let session = open.get_mut(session_id).ok_or(Refused::Expired)?;
        session.touched = Instant::now();
        work(session)
    }

    fn with_conversation<T>(
        &self,
        session_id: &str,
        conversation_id: &str,
        work: impl FnOnce(&mut Session, usize) -> Result<T, Refused>,
    ) -> Result<T, Refused> {
        self.with(session_id, |session| {
            let index = session
                .conversations
                .iter()
                .position(|c| c.id == conversation_id)
                .ok_or(Refused::NoConversation)?;
            work(session, index)
        })
    }

    /// A new session teaching `style`, opening conversations of the user's
    /// own kinds (else the usual ones).
    pub fn start(&self, style: &str, own_kinds: Vec<String>, terms: Vec<String>) -> Value {
        let mut kinds: Vec<&'static str> = own_kinds
            .iter()
            .filter_map(|kind| catalog::kind(kind).map(|kind| kind.id))
            .take(STARTING_CONVERSATIONS)
            .collect();
        if kinds.is_empty() {
            kinds = catalog::FALLBACK_KINDS.to_vec();
        }
        let mut session = Session {
            id: uuid::Uuid::new_v4().simple().to_string(),
            style: style.to_string(),
            conversations: Vec::new(),
            suggested_kinds: own_kinds,
            terms,
            touched: Instant::now(),
            used_captures: HashSet::new(),
        };
        for kind in kinds {
            let fresh = open(kind, &used(&session));
            session.conversations.push(fresh);
        }
        let answer = as_json(&session);
        self.lock().insert(session.id.clone(), session);
        answer
    }

    /// The session as the app shows it.
    pub fn get(&self, session_id: &str) -> Result<Value, Refused> {
        self.with(session_id, |session| Ok(as_json(session)))
    }

    /// Whether the conversation is open, as a check before the body is read.
    pub fn check(&self, session_id: &str, conversation_id: Option<&str>) -> Result<(), Refused> {
        match conversation_id {
            Some(id) => self.with_conversation(session_id, id, |_, _| Ok(())),
            None => self.with(session_id, |_| Ok(())),
        }
    }

    pub fn add_conversation(&self, session_id: &str, kind: &str) -> Result<Value, Refused> {
        let kind = catalog::kind(kind)
            .ok_or_else(|| Refused::Invalid("Unknown kind of conversation".into()))?;
        self.with(session_id, |session| {
            if session.conversations.iter().filter(|c| !c.wrapped).count() >= MAX_CONVERSATIONS {
                return Err(Refused::Invalid(format!(
                    "Wrap up a conversation first; at most {MAX_CONVERSATIONS} stay open"
                )));
            }
            let fresh = open(kind.id, &used(session));
            session.conversations.push(fresh);
            Ok(as_json(session))
        })
    }

    /// Replace a conversation with a fresh one of the same kind. Its replies
    /// are kept for the run.
    pub fn new_theme(&self, session_id: &str, conversation_id: &str) -> Result<Value, Refused> {
        self.with_conversation(session_id, conversation_id, |session, index| {
            let fresh = open(session.conversations[index].kind, &used(session));
            let old = &mut session.conversations[index];
            if old.replies.is_empty() {
                *old = fresh;
            } else {
                old.wrapped = true;
                session.conversations.push(fresh);
            }
            Ok(as_json(session))
        })
    }

    pub fn wrap_up(&self, session_id: &str, conversation_id: &str) -> Result<Value, Refused> {
        self.with_conversation(session_id, conversation_id, |session, index| {
            let found = &mut session.conversations[index];
            found.wrapped = true;
            found.note = None;
            Ok(as_json(session))
        })
    }

    /// When the conversation's current turn started, and the captures
    /// already part of a reply.
    pub fn turn_window(
        &self,
        session_id: &str,
        conversation_id: &str,
    ) -> Result<(NaiveDateTime, HashSet<String>), Refused> {
        self.with_conversation(session_id, conversation_id, |session, index| {
            Ok((
                session.conversations[index].turn_started_at,
                session.used_captures.clone(),
            ))
        })
    }

    /// Captures from before now are not this turn's (the user dictated and
    /// deleted, say).
    pub fn start_turn_now(&self, session_id: &str, conversation_id: &str) -> Result<(), Refused> {
        self.with_conversation(session_id, conversation_id, |session, index| {
            session.conversations[index].turn_started_at = now();
            Ok(())
        })
    }

    pub fn record_reply(
        &self,
        session_id: &str,
        conversation_id: &str,
        written: &str,
        spoken: Option<Dictated>,
    ) -> Result<(), Refused> {
        let written = py::strip(written);
        if written.is_empty() {
            return Err(Refused::Invalid("Write or dictate a reply first".into()));
        }
        if py::len(written) > MAX_REPLY_CHARS {
            return Err(Refused::Invalid(format!(
                "Keep a reply under {MAX_REPLY_CHARS} characters"
            )));
        }
        self.with_conversation(session_id, conversation_id, |session, index| {
            if session.conversations[index].wrapped {
                return Err(Refused::Invalid("That conversation is wrapped up".into()));
            }
            let (said, shown) = match spoken {
                Some(spoken) => {
                    session.used_captures.extend(spoken.capture_ids);
                    (Some(spoken.said), Some(spoken.shown))
                }
                None => (None, None),
            };
            let found = &mut session.conversations[index];
            let chips = text::reply_chips(written, said.as_deref(), found.kind, &session.terms);
            found.replies.push(Reply {
                written: written.to_string(),
                said,
                shown,
                chips,
            });
            found.messages.push((true, written.to_string()));
            found.note = None;
            Ok(())
        })
    }

    /// The prompt for the other side's next message.
    pub fn next_turn(&self, session_id: &str, conversation_id: &str) -> Result<Turn, Refused> {
        self.with_conversation(session_id, conversation_id, |session, index| {
            let found = &mut session.conversations[index];
            found.turn += 1;
            let trick = found.tricks[found.turn % found.tricks.len()];
            let kind = catalog::kind(found.kind).expect("a conversation's kind is known");
            Ok(Turn {
                system: text::system(found.persona, kind, trick, &session.terms, |count| {
                    fastrand::usize(..count)
                }),
                prompt: text::transcript(found.persona, &found.messages),
                trick,
                messages: found.messages.clone(),
            })
        })
    }

    /// Add the other side's message: `parsed` from the model, else a written
    /// opener of the same kind as a new topic.
    pub fn take_turn(
        &self,
        session_id: &str,
        conversation_id: &str,
        trick: &'static str,
        parsed: Option<(String, Note)>,
    ) -> Result<Value, Refused> {
        self.with_conversation(session_id, conversation_id, |session, index| {
            let (message, note) = match parsed {
                Some((message, mut note)) => {
                    note.trick = catalog::noted_trick(trick);
                    (message, note)
                }
                None => {
                    let used = used(session);
                    let options = catalog::seeds_of(session.conversations[index].kind);
                    let seed = options
                        .iter()
                        .find(|seed| !used.contains(seed.id))
                        .copied()
                        .unwrap_or_else(|| options[fastrand::usize(..options.len())]);
                    session.conversations[index].seeds_used.push(seed.id);
                    (seed.message.to_string(), Note::of(seed))
                }
            };
            let found = &mut session.conversations[index];
            found.messages.push((false, message));
            found.note = Some(note);
            found.turn_started_at = now();
            Ok(as_json(session))
        })
    }

    /// End the session: its style and its replies as profile examples.
    pub fn finish(&self, session_id: &str) -> Result<Finished, Refused> {
        let mut open = self.lock();
        let session = open.get(session_id).ok_or(Refused::Expired)?;
        if reply_count(session) == 0 {
            return Err(Refused::Invalid(
                "Reply at least once before finishing".into(),
            ));
        }
        let session = open.remove(session_id).expect("the session is open");
        let examples = session
            .conversations
            .iter()
            .flat_map(|c| {
                c.replies.iter().map(|reply| {
                    json!({
                        "paragraph_id": format!("teach:{}", c.kind),
                        "said": reply.said,
                        // A typed reply is its own cleanup: no example, no
                        // habit evidence.
                        "shown": if reply.said.is_some() { reply.shown.as_deref() } else { Some(reply.written.as_str()) },
                        "written": reply.written,
                    })
                })
            })
            .collect();
        Ok(Finished {
            style: session.style,
            examples,
        })
    }

    pub fn discard(&self, session_id: &str) {
        self.lock().remove(session_id);
    }

    /// A reset or deleted style drops its open sessions.
    pub fn forget_style(&self, style: &str) {
        self.lock().retain(|_, session| session.style != style);
    }

    #[cfg(test)]
    fn age(&self, session_id: &str, by: Duration) {
        let mut open = self.open.lock().unwrap();
        let session = open.get_mut(session_id).unwrap();
        session.touched = Instant::now().checked_sub(by).unwrap();
    }
}

/// Every seed the session's conversations have used.
fn used(session: &Session) -> HashSet<&'static str> {
    session
        .conversations
        .iter()
        .flat_map(|c| c.seeds_used.iter().copied())
        .collect()
}

/// A new conversation of `kind`, from a seed no conversation in the session
/// has used yet.
fn open(kind: &'static str, used: &HashSet<&'static str>) -> Conversation {
    let options = catalog::seeds_of(kind);
    let fresh: Vec<_> = options
        .iter()
        .filter(|seed| !used.contains(seed.id))
        .copied()
        .collect();
    let fresh = if fresh.is_empty() { options } else { fresh };
    let seed = fresh[fastrand::usize(..fresh.len())];
    // Later turns rotate through the tricks, starting anywhere.
    let start = fastrand::usize(..catalog::TRICKS.len());
    let mut tricks = catalog::TRICKS.to_vec();
    tricks.rotate_left(start);
    Conversation {
        id: uuid::Uuid::new_v4().simple().to_string(),
        kind,
        persona: seed.persona,
        relation: seed.relation,
        title: seed.title,
        messages: vec![(false, seed.message.to_string())],
        note: Some(Note::of(seed)),
        seeds_used: vec![seed.id],
        turn_started_at: now(),
        tricks,
        replies: Vec::new(),
        wrapped: false,
        turn: 0,
    }
}

fn reply_count(session: &Session) -> usize {
    session.conversations.iter().map(|c| c.replies.len()).sum()
}

/// Every reply's chips, newest first, each once.
fn conversation_chips(found: &Conversation) -> Vec<&Chip> {
    let mut seen = HashSet::new();
    found
        .replies
        .iter()
        .rev()
        .flat_map(|reply| &reply.chips)
        .filter(|chip| seen.insert((chip.code, chip.value.clone())))
        .collect()
}

fn as_json(session: &Session) -> Value {
    json!({
        "session_id": session.id,
        "style_id": session.style,
        "target": TARGET_REPLIES,
        "replies": reply_count(session),
        "suggested_kinds": session.suggested_kinds,
        "conversations": session.conversations.iter().map(|c| json!({
            "id": c.id,
            "kind": c.kind,
            "persona": c.persona,
            "relation": c.relation,
            "title": c.title,
            "messages": c.messages.iter().map(|(from_you, text)| json!({
                "from_you": from_you,
                "text": text,
            })).collect::<Vec<_>>(),
            "note": c.note,
            "reply_count": c.replies.len(),
            "chips": conversation_chips(c),
            "wrapped": c.wrapped,
        })).collect::<Vec<_>>(),
    })
}

/// What was said and shown in Kass's window since `since`, joined in order,
/// leaving out captures already part of a reply.
pub fn dictated(
    conn: &Connection,
    since: NaiveDateTime,
    used: &HashSet<String>,
) -> rusqlite::Result<Option<Dictated>> {
    let mut query = conn.prepare(
        "SELECT id, transcript_raw, transcript_refined FROM captures \
         WHERE created_at >= ?1 AND source != 'command' AND transcript_raw != '' \
         AND (app_bundle_id = ?2 OR app_bundle_id IS NULL) ORDER BY created_at",
    )?;
    let rows = query
        .query_map(
            params![since.format(STORED).to_string(), KASS_BUNDLE],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|row| !used.contains(&row.0))
        .collect();
    let said = rows
        .iter()
        .map(|(_, raw, _)| py::strip(&strip_stt_artifacts(raw)).to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let said = py::strip(&said);
    if said.is_empty() {
        return Ok(None);
    }
    let shown = rows
        .iter()
        .map(|(_, raw, refined)| {
            py::strip(refined.as_deref().filter(|r| !r.is_empty()).unwrap_or(raw))
        })
        .collect::<Vec<_>>()
        .join(" ");
    Ok(Some(Dictated {
        said: said.to_string(),
        shown: py::strip(&shown).to_string(),
        capture_ids: rows.into_iter().map(|(id, _, _)| id).collect(),
    }))
}

/// The apps dictated into that use `style`, with their categories, then the
/// apps assigned to it that weren't dictated into.
pub fn style_apps(
    conn: &Connection,
    styles: &Snapshot,
    style: &str,
) -> rusqlite::Result<Vec<(String, Option<String>)>> {
    let mut query = conn.prepare(
        "SELECT app_bundle_id, max(app_category) FROM captures \
         WHERE app_bundle_id IS NOT NULL AND app_bundle_id != ?1 GROUP BY app_bundle_id",
    )?;
    let mut found = query
        .query_map(params![KASS_BUNDLE], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<(String, Option<String>)>>>()?;
    found.retain(|(bundle, _)| styles.for_app(Some(bundle)).id == style);
    let seen: HashSet<String> = found.iter().map(|(bundle, _)| bundle.clone()).collect();
    for (bundle, assigned) in &styles.apps {
        if assigned == style && !seen.contains(bundle) {
            found.push((bundle.clone(), None));
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(answer: &Value) -> Vec<String> {
        answer["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn a_session_opens_the_users_kinds_else_the_usual_ones() {
        let sessions = Sessions::default();
        let own = sessions.start("personal", vec!["email".into(), "poem".into()], vec![]);
        assert_eq!(own["conversations"].as_array().unwrap().len(), 1);
        assert_eq!(own["conversations"][0]["kind"], "email");
        assert_eq!(own["suggested_kinds"], json!(["email", "poem"]));
        assert_eq!(own["target"], 10);
        let usual = sessions.start("work", vec![], vec![]);
        let kinds: Vec<_> = usual["conversations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, catalog::FALLBACK_KINDS);
        assert_eq!(sessions.teaching_style().as_deref(), Some("work"));
    }

    #[test]
    fn replies_turns_and_finishing() {
        let sessions = Sessions::default();
        let started = sessions.start("personal", vec!["team_chat".into()], vec![]);
        let id = started["session_id"].as_str().unwrap();
        let conversation = &ids(&started)[0];
        assert_eq!(
            sessions.finish(id).err(),
            Some(Refused::Invalid(
                "Reply at least once before finishing".into()
            ))
        );
        assert_eq!(
            sessions.record_reply(id, conversation, "  ", None),
            Err(Refused::Invalid("Write or dictate a reply first".into()))
        );
        let spoken = Dictated {
            said: "yep no wait will do".into(),
            shown: "Yep, will do.".into(),
            capture_ids: vec!["c1".into()],
        };
        sessions
            .record_reply(id, conversation, " yep will do ", Some(spoken))
            .unwrap();
        assert!(sessions
            .turn_window(id, conversation)
            .unwrap()
            .1
            .contains("c1"));
        let turn = sessions.next_turn(id, conversation).unwrap();
        assert!(turn.prompt.ends_with("User: yep will do"));
        let answer = sessions
            .take_turn(
                id,
                conversation,
                "long",
                text::parse_turn("MESSAGE: great\nTELL: a -> b"),
            )
            .unwrap();
        let shown = &answer["conversations"][0];
        assert_eq!(shown["messages"][2]["text"], "great");
        assert_eq!(shown["note"]["trick"], "long");
        assert_eq!(shown["chips"][0]["code"], "correction_resolved");
        assert_eq!(answer["replies"], 1);
        sessions
            .record_reply(id, conversation, "typed this", None)
            .unwrap();
        let fallback = sessions.take_turn(id, conversation, "plain", None).unwrap();
        let messages = fallback["conversations"][0]["messages"].as_array().unwrap();
        assert!(catalog::seeds_of("team_chat")
            .iter()
            .any(|seed| messages[4]["text"] == seed.message));
        let finished = sessions.finish(id).unwrap();
        assert_eq!(finished.style, "personal");
        assert_eq!(
            finished.examples,
            [
                json!({"paragraph_id": "teach:team_chat", "said": "yep no wait will do", "shown": "Yep, will do.", "written": "yep will do"}),
                json!({"paragraph_id": "teach:team_chat", "said": null, "shown": "typed this", "written": "typed this"}),
            ]
        );
        assert_eq!(sessions.get(id).err(), Some(Refused::Expired));
        assert_eq!(sessions.teaching_style(), None);
    }

    #[test]
    fn conversations_are_added_replaced_wrapped_and_expire() {
        let sessions = Sessions::default();
        let started = sessions.start("personal", vec!["email".into()], vec![]);
        let id = started["session_id"].as_str().unwrap().to_string();
        assert_eq!(
            sessions.add_conversation(&id, "poem").err(),
            Some(Refused::Invalid("Unknown kind of conversation".into()))
        );
        for _ in 0..7 {
            sessions.add_conversation(&id, "notes").unwrap();
        }
        assert_eq!(
            sessions.add_conversation(&id, "notes").err(),
            Some(Refused::Invalid(
                "Wrap up a conversation first; at most 8 stay open".into()
            ))
        );
        let first = ids(&sessions.get(&id).unwrap())[0].clone();
        let replaced = sessions.new_theme(&id, &first).unwrap();
        assert_eq!(ids(&replaced).len(), 8);
        assert_ne!(ids(&replaced)[0], first);
        let first = ids(&replaced)[0].clone();
        sessions
            .record_reply(&id, &first, "Sounds good.", None)
            .unwrap();
        let kept = sessions.new_theme(&id, &first).unwrap();
        assert_eq!(ids(&kept).len(), 9);
        assert_eq!(kept["conversations"][0]["wrapped"], true);
        let wrapped = sessions.wrap_up(&id, &ids(&kept)[1]).unwrap();
        assert_eq!(wrapped["conversations"][1]["note"], Value::Null);
        assert_eq!(
            sessions.wrap_up(&id, "nope").err(),
            Some(Refused::NoConversation)
        );
        assert_eq!(
            sessions.record_reply(&id, &first, "more", None),
            Err(Refused::Invalid("That conversation is wrapped up".into()))
        );
        sessions.age(&id, SESSION_TTL + Duration::from_secs(1));
        assert_eq!(sessions.get(&id).err(), Some(Refused::Expired));
        assert_eq!(sessions.teaching_style(), None);
    }

    #[test]
    fn forgetting_a_style_ends_its_sessions() {
        let sessions = Sessions::default();
        let kept = sessions.start("work", vec![], vec![]);
        let dropped = sessions.start("personal", vec![], vec![]);
        sessions.forget_style("personal");
        assert!(sessions
            .get(dropped["session_id"].as_str().unwrap())
            .is_err());
        assert!(sessions.get(kept["session_id"].as_str().unwrap()).is_ok());
        assert_eq!(sessions.teaching_style().as_deref(), Some("work"));
    }
}
