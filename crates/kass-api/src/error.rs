use serde_json::Value;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The server answered with an error; `status` is the HTTP status (409
    /// for a duplicate, say) and `message` its `detail`.
    #[error("{message}")]
    Status { status: u16, message: String },
    /// The server couldn't be reached, or the connection broke.
    #[error("couldn't reach the Kass server: {0}")]
    Transport(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The server answered with something that isn't what was expected.
    #[error("unexpected response from the Kass server: {0}")]
    Decode(#[from] serde_json::Error),
}

impl Error {
    /// The HTTP status, when the server answered with an error.
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Status { status, .. } => Some(*status),
            _ => None,
        }
    }

    pub(crate) fn transport(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Error::Transport(Box::new(error))
    }

    /// An error response: its message is the body's `detail` (see
    /// `format_detail`), or the status text when the body isn't JSON.
    pub(crate) fn from_response(status: u16, status_text: &str, body: &[u8]) -> Self {
        let fallback = format!("HTTP error! status: {status}");
        let detail = match serde_json::from_slice::<Value>(body) {
            Ok(Value::Object(mut object)) => object.remove("detail"),
            Ok(_) => None,
            Err(_) if status_text.is_empty() => None,
            Err(_) => Some(Value::String(status_text.to_string())),
        };
        Error::Status {
            status,
            message: format_detail(detail.as_ref(), fallback),
        }
    }
}

impl From<ureq::Error> for Error {
    fn from(error: ureq::Error) -> Self {
        match error {
            ureq::Error::Status(status, response) => {
                let status_text = response.status_text().to_string();
                let body = read_body(response).unwrap_or_default();
                Error::from_response(status, &status_text, &body)
            }
            ureq::Error::Transport(transport) => Error::Transport(Box::new(transport)),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::transport(error)
    }
}

/// The whole body; `into_string` would stop at 10 MB.
pub(crate) fn read_body(response: ureq::Response) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    std::io::Read::read_to_end(&mut response.into_reader(), &mut body)?;
    Ok(body)
}

/// FastAPI's `detail`: a string, a list of validation errors (`msg` or
/// `message` each), or an object with a `message`; anything else as JSON.
/// The same as `formatErrorDetail` in client.ts.
pub(crate) fn format_detail(detail: Option<&Value>, fallback: String) -> String {
    match detail {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                [item.get("msg"), item.get("message")]
                    .into_iter()
                    .flatten()
                    .find(|value| truthy(value))
                    .map(text_of)
                    .unwrap_or_else(|| item.to_string())
            })
            .collect::<Vec<_>>()
            .join("; "),
        Some(Value::Object(object)) => match object.get("message") {
            Some(Value::String(message)) => message.clone(),
            _ => Value::Object(object.clone()).to_string(),
        },
        _ => fallback,
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(status: u16, status_text: &str, body: &str) -> (u16, String) {
        match Error::from_response(status, status_text, body.as_bytes()) {
            Error::Status { status, message } => (status, message),
            other => panic!("not a status error: {other:?}"),
        }
    }

    #[test]
    fn string_detail() {
        assert_eq!(
            message(409, "Conflict", r#"{"detail":"Already in the dictionary"}"#),
            (409, "Already in the dictionary".into())
        );
    }

    #[test]
    fn validation_errors_join_their_messages() {
        let body = r#"{"detail":[{"loc":["body","written"],"msg":"Field required"},{"message":"Too long"},{"code":7}]}"#;
        assert_eq!(
            message(422, "Unprocessable Entity", body).1,
            r#"Field required; Too long; {"code":7}"#
        );
    }

    #[test]
    fn object_detail_uses_its_message_or_json() {
        assert_eq!(
            message(
                400,
                "",
                r#"{"detail":{"message":"No space left","free_mb":3}}"#
            )
            .1,
            "No space left"
        );
        assert_eq!(
            message(400, "", r#"{"detail":{"code":"busy"}}"#).1,
            r#"{"code":"busy"}"#
        );
    }

    #[test]
    fn missing_detail_falls_back_to_the_status() {
        assert_eq!(
            message(500, "", r#"{"error":"x"}"#).1,
            "HTTP error! status: 500"
        );
        assert_eq!(
            message(500, "", r#"{"detail":null}"#).1,
            "HTTP error! status: 500"
        );
        assert_eq!(message(502, "", "<html>").1, "HTTP error! status: 502");
    }

    #[test]
    fn a_body_that_isnt_json_gives_the_status_text() {
        assert_eq!(
            message(503, "Service Unavailable", "upstream down"),
            (503, "Service Unavailable".into())
        );
    }
}
