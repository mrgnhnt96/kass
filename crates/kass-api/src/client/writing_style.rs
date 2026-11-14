//! Writing styles, the apps assigned to them, and teaching them. Calls
//! taking `style` work on that style; None is the default style.

use serde_json::json;

use super::{ApiClient, style_query};
use crate::encode::component;
use crate::error::{Error, Result};
use crate::types::{
    AppToConfirm, CorrectionNotesStatus, MovedCorrections, PersonalExample, TeachDictated,
    TeachFinishResult, TeachKind, TeachSession, WritingStyle, WritingStyleStatus,
    WritingStyleUpdate, WritingStylesResponse,
};

impl ApiClient {
    pub fn list_writing_styles(&self) -> Result<WritingStylesResponse> {
        self.fetch("GET", "/writing-styles")
    }

    pub fn create_writing_style(&self, name: &str) -> Result<WritingStyle> {
        self.send("POST", "/writing-styles", &json!({ "name": name }))
    }

    pub fn update_writing_style(
        &self,
        style_id: &str,
        patch: &WritingStyleUpdate,
    ) -> Result<WritingStyle> {
        self.send(
            "PATCH",
            &format!("/writing-styles/{}", component(style_id)),
            patch,
        )
    }

    pub fn delete_writing_style(&self, style_id: &str) -> Result<()> {
        self.run(
            "DELETE",
            &format!("/writing-styles/{}", component(style_id)),
        )
    }

    /// Move an app to a style, bringing or leaving its corrections.
    pub fn assign_app_style(
        &self,
        bundle_id: &str,
        style_id: &str,
        app_name: Option<&str>,
        corrections: MovedCorrections,
    ) -> Result<WritingStylesResponse> {
        let body =
            json!({ "style_id": style_id, "app_name": app_name, "corrections": corrections });
        self.send(
            "PUT",
            &format!("/writing-styles/apps/{}", component(bundle_id)),
            &body,
        )
    }

    /// Keep apps in the style they already use.
    pub fn confirm_apps(&self, apps: &[AppToConfirm]) -> Result<WritingStylesResponse> {
        self.send(
            "POST",
            "/writing-styles/apps/confirm",
            &json!({ "apps": apps }),
        )
    }

    pub fn writing_style(&self, style: Option<&str>) -> Result<WritingStyleStatus> {
        self.fetch("GET", &format!("/writing-style{}", style_query(style)))
    }

    pub fn reset_writing_style(&self, style: Option<&str>) -> Result<WritingStyleStatus> {
        self.fetch("DELETE", &format!("/writing-style{}", style_query(style)))
    }

    pub fn list_personal_examples(&self, style: Option<&str>) -> Result<Vec<PersonalExample>> {
        self.fetch(
            "GET",
            &format!("/writing-style/examples{}", style_query(style)),
        )
    }

    pub fn correction_notes(&self, style: Option<&str>) -> Result<CorrectionNotesStatus> {
        self.fetch(
            "GET",
            &format!("/writing-style/notes{}", style_query(style)),
        )
    }

    pub fn remove_correction_note(&self, note_id: &str, style: Option<&str>) -> Result<()> {
        self.run(
            "DELETE",
            &format!(
                "/writing-style/notes/{}{}",
                component(note_id),
                style_query(style)
            ),
        )
    }

    pub fn remove_personal_example(&self, example_id: &str) -> Result<()> {
        self.run(
            "DELETE",
            &format!("/writing-style/examples/{}", component(example_id)),
        )
    }

    pub fn start_teach(&self, style: Option<&str>) -> Result<TeachSession> {
        self.fetch(
            "POST",
            &format!("/writing-style/teach{}", style_query(style)),
        )
    }

    pub fn add_teach_conversation(
        &self,
        session_id: &str,
        kind: TeachKind,
    ) -> Result<TeachSession> {
        let path = format!("{}/conversations", teach_path(session_id));
        self.send("POST", &path, &json!({ "kind": kind }))
    }

    pub fn new_teach_theme(&self, session_id: &str, conversation_id: &str) -> Result<TeachSession> {
        self.fetch(
            "POST",
            &conversation_path(session_id, conversation_id, "new-theme"),
        )
    }

    pub fn wrap_teach_conversation(
        &self,
        session_id: &str,
        conversation_id: &str,
    ) -> Result<TeachSession> {
        self.fetch(
            "POST",
            &conversation_path(session_id, conversation_id, "wrap-up"),
        )
    }

    /// Send a reply; the other side's next message comes back with the session.
    pub fn send_teach_reply(
        &self,
        session_id: &str,
        conversation_id: &str,
        written: &str,
    ) -> Result<TeachSession> {
        let path = conversation_path(session_id, conversation_id, "replies");
        self.send("POST", &path, &json!({ "written": written }))
    }

    /// The cleanup of what was dictated in Kass's window this turn.
    pub fn teach_dictated(&self, session_id: &str, conversation_id: &str) -> Result<TeachDictated> {
        self.fetch(
            "GET",
            &conversation_path(session_id, conversation_id, "dictated"),
        )
    }

    /// Earlier dictation is no longer part of this conversation's reply.
    pub fn restart_teach_turn(&self, session_id: &str, conversation_id: &str) -> Result<()> {
        self.run(
            "POST",
            &conversation_path(session_id, conversation_id, "restart-turn"),
        )
    }

    pub fn finish_teach(&self, session_id: &str) -> Result<TeachFinishResult> {
        self.fetch("POST", &format!("{}/finish", teach_path(session_id)))
    }

    /// Throw a teaching session away. One that's already gone is fine.
    pub fn discard_teach(&self, session_id: &str) -> Result<()> {
        match self.run("DELETE", &teach_path(session_id)) {
            Err(Error::Status { status: 404, .. }) => Ok(()),
            result => result,
        }
    }
}

fn teach_path(session_id: &str) -> String {
    format!("/writing-style/teach/{}", component(session_id))
}

fn conversation_path(session_id: &str, conversation_id: &str, action: &str) -> String {
    format!(
        "{}/conversations/{}/{action}",
        teach_path(session_id),
        component(conversation_id)
    )
}
