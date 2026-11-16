//! What the Models page asks of the app around it: the models folder, which
//! the app keeps and starts the server with. Every method has a default that
//! does nothing, so the page runs on its own, as in the example.

use gpui_kit::{App, Task};
use kass_api::types::CaptureSettings;

pub trait ModelsHost {
    /// The app runs this Mac's server, so it can move the models folder.
    fn manages_storage(&self, _cx: &App) -> bool {
        false
    }

    /// The folder the user picked for models, if not the default.
    fn custom_models_dir(&self, _cx: &App) -> Option<String> {
        None
    }

    /// Keep models in `dir` (the default with `None`) and restart the server
    /// there; resolves once it's back up, or with why not.
    fn set_models_dir(&self, _dir: Option<String>, _cx: &mut App) -> Task<Result<(), String>> {
        Task::ready(Err("This app can't move the models folder".into()))
    }

    /// The page changed which models dictation uses.
    fn settings_changed(&self, _settings: &CaptureSettings, _cx: &mut App) {}

    /// The page downloaded, deleted or unloaded a model.
    fn models_changed(&self, _cx: &mut App) {}
}

/// The host when the page runs on its own.
pub struct NoHost;

impl ModelsHost for NoHost {}
