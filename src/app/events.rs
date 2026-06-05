use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::{
    app::clip_types::ClipReason, capture::gpu_screen_recorder::GsrStatus, config::AppConfig,
    doctor::DoctorReport,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ClipStatus {
    Detected,
    Recording,
    Encoding,
    Saving,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipStatusPayload {
    pub id: String,
    pub status: ClipStatus,
    pub reason: ClipReason,
    pub title: String,
    pub created_at: String,
    pub file_path: Option<PathBuf>,
    pub thumbnail_path: Option<PathBuf>,
    pub duration_seconds: Option<u64>,
    pub size_bytes: Option<u64>,
    pub progress: Option<u8>,
    pub error: Option<String>,
    pub exportable_at: Option<String>,
    pub can_export: bool,
    pub retryable: bool,
}

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
    ClipStatusChanged {
        payload: ClipStatusPayload,
    },
    ClipFailed {
        message: String,
    },
    GsrStatusChanged {
        status: GsrStatus,
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
    RestartGpuRecorder,
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
