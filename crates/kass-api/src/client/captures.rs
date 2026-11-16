//! Captures: listing, uploading, refining and their audio.

use super::ApiClient;
use crate::encode::{component, form};
use crate::error::{Result, read_body};
use crate::multipart::Multipart;
use crate::sse::{SseCancel, SseStream};
use crate::types::{
    CaptureAppFilter, CaptureAppsResponse, CaptureCreateOptions, CaptureCreateResponse,
    CaptureListResponse, CaptureRefineRequest, CaptureResponse, CaptureSource, CaptureUpload,
    Message, UsagePeriod, UsageStatsResponse,
};

impl ApiClient {
    /// A page of captures, newest first. The app asks for 50 from offset 0.
    pub fn list_captures(
        &self,
        limit: u32,
        offset: u32,
        app: &CaptureAppFilter,
    ) -> Result<CaptureListResponse> {
        let (limit, offset) = (limit.to_string(), offset.to_string());
        let query = form(
            [("limit", limit.as_str()), ("offset", offset.as_str())]
                .into_iter()
                .chain(app.query_pair()),
        );
        self.fetch("GET", &format!("/captures?{query}"))
    }

    pub fn list_capture_apps(&self) -> Result<CaptureAppsResponse> {
        self.fetch("GET", "/captures/apps")
    }

    pub fn capture_stats(
        &self,
        period: UsagePeriod,
        app: &CaptureAppFilter,
    ) -> Result<UsageStatsResponse> {
        let query = form(
            [("period", period.as_str())]
                .into_iter()
                .chain(app.query_pair()),
        );
        self.fetch("GET", &format!("/captures/stats?{query}"))
    }

    pub fn capture(&self, capture_id: &str) -> Result<CaptureResponse> {
        self.fetch("GET", &format!("/captures/{}", component(capture_id)))
    }

    /// Upload audio to transcribe. The source is `file` unless set.
    pub fn create_capture(
        &self,
        upload: &CaptureUpload,
        options: &CaptureCreateOptions,
    ) -> Result<CaptureCreateResponse> {
        let mut body = Multipart::new();
        body.file(
            "file",
            &upload.file_name,
            &upload.content_type,
            &upload.bytes,
        );
        body.text(
            "source",
            options.source.unwrap_or(CaptureSource::File).as_str(),
        );
        if let Some(language) = options
            .language
            .as_deref()
            .filter(|language| !language.is_empty())
        {
            body.text("language", language);
        }
        if let Some(model) = options.stt_model {
            body.text("stt_model", model.as_str());
        }
        let response = self
            .request("POST", &self.url("/captures"))
            .set("Content-Type", &body.content_type())
            .send_bytes(&body.finish())?;
        super::decode(response)
    }

    pub fn delete_capture(&self, capture_id: &str) -> Result<Message> {
        self.fetch("DELETE", &format!("/captures/{}", component(capture_id)))
    }

    pub fn refine_capture(
        &self,
        capture_id: &str,
        request: &CaptureRefineRequest,
    ) -> Result<CaptureResponse> {
        self.send(
            "POST",
            &format!("/captures/{}/refine", component(capture_id)),
            request,
        )
    }

    /// Where the capture's recording plays from, on the current server.
    pub fn capture_audio_url(&self, capture_id: &str) -> String {
        self.url(&format!("/captures/{}/audio", component(capture_id)))
    }

    /// The capture's recording (a WAV file).
    pub fn capture_audio(&self, capture_id: &str) -> Result<Vec<u8>> {
        read_body(
            self.request("GET", &self.capture_audio_url(capture_id))
                .call()?,
        )
    }

    /// `/captures/events`: which captures changed, learning that finished,
    /// and the Macs sharing this server. Map each event with
    /// `CaptureEvent::from`.
    pub fn capture_events(&self, cancel: &SseCancel) -> Result<SseStream> {
        let mac = self.mac();
        let pairs = mac
            .as_ref()
            .map(|mac| mac.query_pairs())
            .unwrap_or_default();
        let mut url = self.url("/captures/events");
        if !pairs.is_empty() {
            url = format!("{url}?{}", form(pairs));
        }
        self.events(&url, cancel)
    }
}
