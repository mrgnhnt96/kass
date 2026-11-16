//! Teaching a style by replying to conversations (docs/plans/TEACH_BY_REPLYING.md).

use serde::{Deserialize, Serialize};

use super::writing_style::WritingStyleStatus;

/// The kinds of conversation teaching uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeachKind {
    CodingAgent,
    DesignFeedback,
    Notes,
    Writeup,
    TeamChat,
    IssueComment,
    Email,
    TextMessage,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeachAnswer {
    pub ask: String,
    pub answer: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeachTrick {
    ChangeOfMind,
    Long,
    #[serde(other)]
    Unknown,
}

/// The facts for the user's next reply, and sometimes a trick to try.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeachNote {
    #[serde(default)]
    pub facts: Option<String>,
    #[serde(default)]
    pub answers: Vec<TeachAnswer>,
    #[serde(default)]
    pub trick: Option<TeachTrick>,
    #[serde(default)]
    pub example: Option<String>,
}

/// Something a reply showed about how the user writes; the app words `code`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeachChip {
    pub code: String,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeachMessage {
    pub from_you: bool,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeachConversation {
    pub id: String,
    pub kind: TeachKind,
    pub persona: String,
    pub relation: String,
    #[serde(default)]
    pub title: Option<String>,
    pub messages: Vec<TeachMessage>,
    #[serde(default)]
    pub note: Option<TeachNote>,
    pub reply_count: u64,
    pub chips: Vec<TeachChip>,
    pub wrapped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeachSession {
    pub session_id: String,
    pub style_id: String,
    /// Replies to aim for; finishing works after one.
    pub target: u64,
    pub replies: u64,
    /// The kinds that match the style's apps.
    pub suggested_kinds: Vec<TeachKind>,
    pub conversations: Vec<TeachConversation>,
}

/// The cleanup of what was dictated in Kass's window this turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeachDictated {
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeachFinishResult {
    pub status: WritingStyleStatus,
    /// One of the user's dictations cleaned up before and after; None without one.
    #[serde(default)]
    pub before: Option<String>,
    #[serde(default)]
    pub after: Option<String>,
    pub replies: u64,
    /// Replies that were dictated, and so teach cleanup.
    pub dictated: u64,
}
