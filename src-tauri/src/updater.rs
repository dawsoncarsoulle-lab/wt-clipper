use serde::{Deserialize, Serialize};
use tauri_plugin_updater::UpdaterExt;
use tracing::debug;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckResult {
    pub available: bool,
    pub version: Option<String>,
    pub notes: Option<String>,
}

#[tauri::command]
pub async fn check_for_updates(app: tauri::AppHandle) -> Result<UpdateCheckResult, String> {
    let updater = app.updater().map_err(|error| error.to_string())?;
    match updater.check().await.map_err(|error| error.to_string())? {
        Some(update) => {
            let version = update.version.clone();
            let notes = update.body.clone();
            debug!(%version, has_notes = notes.is_some(), "update available, downloading and installing");
            update
                .download_and_install(
                    |chunk_length, content_length| {
                        debug!(chunk_length, ?content_length, "update download progress");
                    },
                    || {
                        debug!("update download finished");
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
            debug!("update installed, restarting application");
            app.restart()
        }
        None => Ok(UpdateCheckResult {
            available: false,
            version: None,
            notes: None,
        }),
    }
}
