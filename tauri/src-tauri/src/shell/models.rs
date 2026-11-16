//! The Models page's place in the shell: the models folder the page's store
//! keeps (moved through a shell command, which restarts the server there),
//! and the settings the shell shares with it.

use gpui_kit::{App, Entity, Task};
use kass_api::types::CaptureSettings;
use kass_models::ModelsHost;

use super::data::ShellData;
use crate::prefs;

pub struct ShellModels {
    pub data: Entity<ShellData>,
}

impl ModelsHost for ShellModels {
    fn manages_storage(&self, cx: &App) -> bool {
        // Another Mac's models live on that Mac.
        self.data.read(cx).prefs.remote_server_url.is_none()
    }

    fn custom_models_dir(&self, cx: &App) -> Option<String> {
        self.data.read(cx).prefs.custom_models_dir.clone()
    }

    fn set_models_dir(&self, dir: Option<String>, cx: &mut App) -> Task<Result<(), String>> {
        cx.spawn(async move |_| prefs::set_models_dir(dir).await)
    }

    fn settings_changed(&self, settings: &CaptureSettings, cx: &mut App) {
        let settings = settings.clone();
        self.data.update(cx, |data, cx| {
            data.settings = Some(settings);
            cx.notify();
        });
    }
}
