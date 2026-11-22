//! Which Mac sent a request, for a Kass shared over the network (beta
//! `network`). Requests that can save a capture say which Mac sent them
//! (backend/utils/macs.py); the app shell reads this Mac's id and name
//! (app/src/this_mac.rs) and hands them to the client.

use serde::{Deserialize, Serialize};

use crate::encode;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mac {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
}

impl Mac {
    pub fn new(id: impl Into<String>, name: Option<String>) -> Self {
        Self {
            id: id.into(),
            name,
        }
    }

    /// `X-Kass-Mac` and, with a name, `X-Kass-Mac-Name` (percent-encoded).
    pub fn headers(&self) -> Vec<(&'static str, String)> {
        let mut headers = vec![("X-Kass-Mac", self.id.clone())];
        if let Some(name) = &self.name {
            headers.push(("X-Kass-Mac-Name", encode::header(name)));
        }
        headers
    }

    /// The same as query parameters (`mac`, `mac_name`), for event streams.
    pub fn query_pairs(&self) -> Vec<(&'static str, &str)> {
        let mut pairs = vec![("mac", self.id.as_str())];
        if let Some(name) = &self.name {
            pairs.push(("mac_name", name.as_str()));
        }
        pairs
    }
}
