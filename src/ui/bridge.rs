use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::{capture::buffer::ClipReason, config::AppConfig, doctor::DoctorReport};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum AppEvent {
    WtConnected,
    WtDisconnected,
    KillDetected {
        reason: ClipReason,
        vehicle: Option<String>,
        target: Option<String>,
        description: String,
    },
    ClipSaved {
        path: PathBuf,
        reason: ClipReason,
        duration_seconds: u64,
        size_bytes: u64,
    },
    ClipFailed {
        message: String,
    },
    BufferProgress {
        filled_secs: f32,
        total_secs: f32,
    },
    DiskUsage {
        used_bytes: u64,
    },
    ClipsLoaded {
        clips: Vec<ClipInfo>,
        total_bytes: u64,
    },
    DiagnosticsReady(DoctorReport),
}

#[derive(Debug, Clone)]
pub enum UiCommand {
    SaveManualClip,
    DeleteClip(PathBuf),
    OpenOutputFolder,
    UpdateConfig(AppConfig),
    RestartBuffer,
    LoadClips,
    RunDiagnostics,
}

pub struct Bridge {
    pub event_rx: mpsc::UnboundedReceiver<AppEvent>,
    pub cmd_tx: mpsc::UnboundedSender<UiCommand>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipInfo {
    pub path: PathBuf,
    pub thumbnail_path: Option<PathBuf>,
    pub preview_url: Option<String>,
    pub file_name: String,
    pub reason: ClipReason,
    pub size_bytes: u64,
    pub duration_seconds: u64,
    pub modified_secs_ago: u64,
}
