use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::{capture::buffer::ClipReason, config::AppConfig, doctor::DoctorReport};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ClipStatus {
    Detected,
    Recording,
    Encoding,
    Saving,
    #[serde(rename = "pending_export")]
    PendingExport,
    #[serde(rename = "waiting_post_event")]
    WaitingPostEvent,
    #[serde(rename = "freezing_segments")]
    FreezingSegments,
    #[serde(rename = "ready_to_export")]
    ReadyToExport,
    Exporting,
    Ready,
    Failed,
    Expired,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExportProgressStep {
    Preparing,
    Extracting,
    Assembling,
    Encoding,
    Thumbnail,
    Saving,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportProgressPayload {
    pub active: bool,
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
    pub current_clip_id: Option<String>,
    pub current_clip_title: Option<String>,
    pub current_step: ExportProgressStep,
    pub progress: u8,
    pub message: String,
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
    ExportProgressChanged {
        payload: ExportProgressPayload,
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
