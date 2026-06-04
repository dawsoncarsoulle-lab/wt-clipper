use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, error, info};
use uuid::Uuid;

use crate::{
    capture::{
        buffer::{
            save_frozen_replay, ClipContext, ClipReason, FreezeReplayOutcome, ReplayBufferConfig,
            ReplayBufferHandle, SaveReplayOutcome, SavedReplay,
        },
        output::default_output_dir,
        quality::{QualityPreset, VideoQuality},
    },
    cli::CaptureSource,
    config::{AppConfig, ClipExportMode, TriggerConfig, WarThunderConfig},
    ui::bridge::{
        AppEvent, ClipStatus, ClipStatusPayload, ExportProgressPayload, ExportProgressStep,
    },
    warthunder::{
        client::{ChatMessage, WarThunderClient},
        events::WarThunderEvent,
        parser::parse_gamechat_event,
        recent::{RecentEventCache, RecentMessageCache},
    },
};

const EVENT_DEDUPE_TTL: Duration = Duration::from_secs(2);
const FREEZE_RETRY_DELAY: Duration = Duration::from_secs(2);
const FREEZE_NOT_READY_LOG_THROTTLE: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub struct AutoClipConfig {
    pub buffer_seconds: u64,
    pub segment_seconds: u64,
    pub output_dir: Option<PathBuf>,
    pub source: CaptureSource,
    pub keep_segments: bool,
    pub quality_preset: QualityPreset,
    pub quality: VideoQuality,
    pub cooldown: Duration,
    pub post_event_delay: Duration,
    pub multi_kill_window: Duration,
    pub include_history: bool,
    pub triggers: TriggerConfig,
    pub ui_events: Option<mpsc::UnboundedSender<AppEvent>>,
    pub export_mode: ClipExportMode,
    pub pending_export_dir: PathBuf,
    pub delete_ready_after_export: bool,
    pub command_rx: Option<mpsc::UnboundedReceiver<AutoClipCommand>>,
}

#[derive(Debug)]
pub enum AutoClipCommand {
    SaveManualClip,
    ExportPendingClips {
        respond_to: oneshot::Sender<ExportSummary>,
    },
    GetPendingExportClips {
        respond_to: oneshot::Sender<Vec<PendingClipExportDto>>,
    },
    DeletePendingClip {
        id: String,
        respond_to: oneshot::Sender<Result<(), String>>,
    },
    UpdateConfig {
        config: AppConfig,
        respond_to: oneshot::Sender<RuntimeConfigUpdateResult>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeConfigUpdateResult {
    pub applied_live: bool,
    pub restart_required: bool,
    pub active_export_mode: ClipExportMode,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PendingExportStatus {
    WaitingPostEvent,
    FreezingSegments,
    ReadyToExport,
    Exporting,
    Ready,
    Failed,
    Expired,
}

#[derive(Debug, Clone)]
pub struct PendingClipExport {
    pub id: String,
    pub reason: ClipReason,
    pub title: String,
    pub detected_at: SystemTime,
    pub game_time: Option<Duration>,
    pub start_time: SystemTime,
    pub end_time: SystemTime,
    pub exportable_at: SystemTime,
    pub pending_dir: PathBuf,
    pub segments_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub frozen_segments: Vec<PathBuf>,
    pub pre_event_seconds: u64,
    pub post_event_seconds: u64,
    pub status: PendingExportStatus,
    dedupe_key: String,
    events: Vec<WarThunderEvent>,
    player_name: Option<String>,
    quality: VideoQuality,
    quality_preset: QualityPreset,
    segment_seconds: u64,
    error: Option<String>,
    retryable: bool,
    next_freeze_attempt_at: Option<SystemTime>,
    last_freeze_attempt_at: Option<SystemTime>,
    last_freeze_log_at: Option<SystemTime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingClipExportDto {
    pub id: String,
    pub reason: ClipReason,
    pub title: String,
    pub created_at: String,
    pub status: PendingExportStatus,
    pub progress: Option<u8>,
    pub error: Option<String>,
    pub exportable_at: String,
    pub is_exportable: bool,
    pub can_export: bool,
    pub retryable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingClipManifest {
    id: String,
    reason: ClipReason,
    title: String,
    #[serde(default)]
    dedupe_key: Option<String>,
    detected_at: String,
    event_time: String,
    start_time: String,
    end_time: String,
    pre_event_seconds: u64,
    post_event_seconds: u64,
    segments: Vec<PathBuf>,
    status: PendingExportStatus,
    #[serde(default)]
    events: Vec<WarThunderEvent>,
    #[serde(default)]
    player_name: Option<String>,
    #[serde(default)]
    final_video_path: Option<PathBuf>,
    #[serde(default)]
    metadata_path: Option<PathBuf>,
    #[serde(default)]
    thumbnail_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExportSummary {
    pub total: usize,
    pub completed: usize,
    pub failed: usize,
}

#[derive(Debug, Clone)]
struct VerifiedExport {
    final_video_path: PathBuf,
    metadata_path: PathBuf,
    thumbnail_path: Option<PathBuf>,
    size_bytes: u64,
}

#[derive(Debug, Deserialize)]
struct ClipMetadataProbe {
    reason: Option<String>,
    duration_seconds: Option<u64>,
    post_event_seconds: Option<u64>,
    segment_seconds: Option<u64>,
    pending_clip_id: Option<String>,
    pending_dedupe_key: Option<String>,
    timestamp: Option<String>,
}

#[derive(Debug, Default)]
pub struct ExportQueue {
    pending: Vec<PendingClipExport>,
}

#[derive(Debug)]
struct AutoWatchState {
    last_chat_id: u64,
    last_evt_msg_id: u64,
    last_dmg_msg_id: u64,
    seen_messages: RecentMessageCache,
    seen_events: RecentEventCache,
}

impl AutoWatchState {
    fn new() -> Self {
        Self {
            last_chat_id: 0,
            last_evt_msg_id: 0,
            last_dmg_msg_id: 0,
            seen_messages: RecentMessageCache::new(1000),
            // War Thunder can expose the same kill through multiple localhost endpoints.
            // Keep this short so repeated identical kills are not suppressed for minutes.
            seen_events: RecentEventCache::new(EVENT_DEDUPE_TTL),
        }
    }
}

fn output_dir_for_config(auto_config: &AutoClipConfig) -> Option<PathBuf> {
    auto_config
        .output_dir
        .clone()
        .or_else(|| default_output_dir().ok())
}

fn is_non_empty_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.len() > 0)
        .unwrap_or(false)
}

fn read_clip_metadata_probe(metadata_path: &Path) -> Option<ClipMetadataProbe> {
    let content = std::fs::read_to_string(metadata_path).ok()?;
    serde_json::from_str(&content).ok()
}

fn final_output_from_manifest_is_valid(manifest: &PendingClipManifest) -> bool {
    let Some(final_video_path) = manifest.final_video_path.as_deref() else {
        return false;
    };
    let metadata_path = manifest
        .metadata_path
        .as_deref()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| final_video_path.with_extension("json"));

    is_non_empty_file(final_video_path) && metadata_path.is_file()
}

fn metadata_timestamp_after_pending(probe: &ClipMetadataProbe, detected_at: SystemTime) -> bool {
    let Some(timestamp) = probe.timestamp.as_deref() else {
        return true;
    };
    parse_system_time_rfc3339(timestamp)
        .map(|timestamp| timestamp >= detected_at)
        .unwrap_or(true)
}

fn metadata_matches_pending_identity(
    probe: &ClipMetadataProbe,
    id: &str,
    dedupe_key: &str,
    reason: ClipReason,
    duration_seconds: u64,
    post_event_seconds: u64,
    segment_seconds: u64,
    detected_at: SystemTime,
) -> bool {
    if probe.pending_clip_id.as_deref() == Some(id)
        || probe.pending_dedupe_key.as_deref() == Some(dedupe_key)
    {
        return true;
    }

    probe.pending_clip_id.is_none()
        && probe.pending_dedupe_key.is_none()
        && probe.reason.as_deref() == Some(reason.slug())
        && probe.duration_seconds == Some(duration_seconds)
        && probe.post_event_seconds == Some(post_event_seconds)
        && probe.segment_seconds == Some(segment_seconds)
        && metadata_timestamp_after_pending(probe, detected_at)
}

fn final_clip_exists_for_identity(
    auto_config: &AutoClipConfig,
    id: &str,
    dedupe_key: &str,
    reason: ClipReason,
    duration_seconds: u64,
    post_event_seconds: u64,
    segment_seconds: u64,
    detected_at: SystemTime,
) -> Option<PathBuf> {
    let output_dir = output_dir_for_config(auto_config)?;
    let entries = std::fs::read_dir(output_dir).ok()?;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("webm")
            || !is_non_empty_file(&path)
        {
            continue;
        }
        let metadata_path = path.with_extension("json");
        let Some(probe) = read_clip_metadata_probe(&metadata_path) else {
            continue;
        };
        if metadata_matches_pending_identity(
            &probe,
            id,
            dedupe_key,
            reason,
            duration_seconds,
            post_event_seconds,
            segment_seconds,
            detected_at,
        ) {
            return Some(path);
        }
    }
    None
}

fn final_clip_exists_for_manifest(
    manifest: &PendingClipManifest,
    auto_config: &AutoClipConfig,
) -> Option<PathBuf> {
    if final_output_from_manifest_is_valid(manifest) {
        return manifest.final_video_path.clone();
    }

    let detected_at = parse_system_time_rfc3339(&manifest.detected_at).ok()?;
    let dedupe_key = manifest
        .dedupe_key
        .clone()
        .unwrap_or_else(|| fallback_manifest_dedupe_key(manifest));
    final_clip_exists_for_identity(
        auto_config,
        &manifest.id,
        &dedupe_key,
        manifest.reason,
        manifest
            .pre_event_seconds
            .saturating_add(manifest.post_event_seconds)
            .max(1),
        manifest.post_event_seconds,
        auto_config.segment_seconds,
        detected_at,
    )
}

fn final_clip_exists_for_clip(
    clip: &PendingClipExport,
    auto_config: &AutoClipConfig,
) -> Option<PathBuf> {
    final_clip_exists_for_identity(
        auto_config,
        &clip.id,
        &clip.dedupe_key,
        clip.reason,
        clip.duration_seconds(),
        clip.post_event_seconds,
        clip.segment_seconds,
        clip.detected_at,
    )
}

fn manifest_segments_valid(manifest: &PendingClipManifest, pending_dir: &Path) -> bool {
    !manifest.segments.is_empty()
        && manifest
            .segments
            .iter()
            .map(|segment| pending_dir.join(segment))
            .all(|path| is_non_empty_file(&path))
}

fn clip_frozen_segments_valid(clip: &PendingClipExport) -> bool {
    !clip.frozen_segments.is_empty()
        && clip
            .frozen_segments
            .iter()
            .map(|segment| clip.pending_dir.join(segment))
            .all(|path| is_non_empty_file(&path))
}

fn delete_pending_dir(path: &Path, id: &str) {
    if !path.exists() {
        return;
    }
    match std::fs::remove_dir_all(path) {
        Ok(()) => info!(
            id = %id,
            dir = %path.display(),
            "[QUEUE] deleted stale pending dir"
        ),
        Err(error) => error!(
            %error,
            id = %id,
            dir = %path.display(),
            "failed to delete stale pending dir"
        ),
    }
}

#[derive(Debug)]
struct Cooldown {
    duration: Duration,
    last_save: Option<Instant>,
}

#[derive(Debug, Clone)]
struct PendingClip {
    clip_id: String,
    created_at: String,
    reason: ClipReason,
    first_event_time: Instant,
    last_event_time: Instant,
    last_event_game_time: Option<Duration>,
    first_event_wall_time: SystemTime,
    last_event_wall_time: SystemTime,
    save_at: Instant,
    event_keys: HashSet<String>,
    dedupe_keys: HashSet<String>,
    events: Vec<WarThunderEvent>,
    descriptions: Vec<String>,
}

impl PendingClip {
    fn new(
        event: WarThunderEvent,
        event_key: String,
        reason: ClipReason,
        event_time: Instant,
        event_game_time: Option<Duration>,
        event_wall_time: SystemTime,
        delay: Duration,
        description: String,
    ) -> Self {
        let mut event_keys = HashSet::new();
        event_keys.insert(event_key);
        let mut dedupe_keys = HashSet::new();
        dedupe_keys.insert(pending_event_dedupe_key(
            reason,
            &event,
            event_game_time,
            event_wall_time,
        ));
        Self {
            clip_id: format!("clip_{}", Uuid::new_v4()),
            created_at: Utc::now().to_rfc3339(),
            reason,
            first_event_time: event_time,
            last_event_time: event_time,
            last_event_game_time: event_game_time,
            first_event_wall_time: event_wall_time,
            last_event_wall_time: event_wall_time,
            save_at: event_time + delay,
            event_keys,
            dedupe_keys,
            events: vec![event],
            descriptions: vec![description],
        }
    }

    fn add_event(
        &mut self,
        event: WarThunderEvent,
        event_key: String,
        event_time: Instant,
        event_game_time: Option<Duration>,
        event_wall_time: SystemTime,
        delay: Duration,
        description: String,
    ) -> bool {
        if !self.event_keys.insert(event_key) {
            return false;
        }
        self.dedupe_keys.insert(pending_event_dedupe_key(
            ClipReason::TargetDestroyed,
            &event,
            event_game_time,
            event_wall_time,
        ));
        self.last_event_time = event_time;
        self.last_event_game_time = event_game_time.or(self.last_event_game_time);
        self.last_event_wall_time = event_wall_time;
        self.save_at = event_time + delay;
        self.events.push(event);
        self.descriptions.push(description);
        if self.events.len() > 1 && self.events.iter().all(is_target_destroyed_clip_event) {
            self.reason = ClipReason::MultiKill;
        }
        true
    }

    fn is_ready(&self, now: Instant) -> bool {
        now >= self.save_at
    }

    fn kill_count(&self) -> usize {
        self.events
            .iter()
            .filter(|event| is_target_destroyed_clip_event(event))
            .count()
    }
}

impl Cooldown {
    fn new(duration: Duration) -> Self {
        Self {
            duration,
            last_save: None,
        }
    }

    fn allows(&mut self, now: Instant) -> bool {
        !matches!(
            self.last_save,
            Some(last_save) if now.duration_since(last_save) < self.duration
        )
    }

    fn record_save(&mut self, now: Instant) {
        self.last_save = Some(now);
    }
}

#[derive(Debug, Clone)]
struct DetectedEvent {
    event: WarThunderEvent,
    reason: ClipReason,
    canonical_key: String,
    detected_at: Instant,
    game_time: Option<Duration>,
    detected_wall_time: SystemTime,
}

impl ExportQueue {
    pub fn load_from_pending_dir(&mut self, auto_config: &AutoClipConfig) -> anyhow::Result<()> {
        let pending_dir = &auto_config.pending_export_dir;
        info!(
            path = %pending_dir.display(),
            "[QUEUE] loading pending manifests from"
        );
        if !pending_dir.exists() {
            info!(count = 0, "[QUEUE] pending loaded count");
            return Ok(());
        }

        let mut loaded_count = 0usize;
        for entry in std::fs::read_dir(pending_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let manifest_path = path.join("manifest.json");
            if !manifest_path.is_file() {
                continue;
            }
            let manifest = match read_pending_manifest(&manifest_path) {
                Ok(manifest) => manifest,
                Err(error) => {
                    error!(
                        %error,
                        id = "<unknown>",
                        path = %manifest_path.display(),
                        reason = "invalid manifest",
                        "[QUEUE] skipped invalid pending"
                    );
                    delete_pending_dir(&path, "<unknown>");
                    continue;
                }
            };
            let id = manifest.id.clone();
            if manifest.status == PendingExportStatus::Ready {
                info!(
                    id = %id,
                    reason = "ready manifest",
                    "[QUEUE] skipped stale pending"
                );
                if auto_config.delete_ready_after_export {
                    delete_pending_dir(&path, &id);
                }
                continue;
            }
            if final_clip_exists_for_manifest(&manifest, auto_config).is_some() {
                info!(
                    id = %id,
                    reason = "final clip already exists",
                    "[QUEUE] skipped stale pending"
                );
                if auto_config.delete_ready_after_export {
                    delete_pending_dir(&path, &id);
                }
                continue;
            }
            if !manifest_segments_valid(&manifest, &path) {
                info!(
                    id = %id,
                    reason = "missing segments",
                    "[QUEUE] skipped invalid pending"
                );
                delete_pending_dir(&path, &id);
                continue;
            }
            match pending_export_from_manifest_data(manifest_path.as_path(), manifest, auto_config)
            {
                Ok(clip) => {
                    info!(
                        id = %clip.id,
                        status = ?clip.status,
                        path = %manifest_path.display(),
                        "[QUEUE] loaded pending"
                    );
                    self.add_pending_clip(clip);
                    loaded_count += 1;
                }
                Err(error) => {
                    error!(
                        %error,
                        id = %id,
                        path = %manifest_path.display(),
                        reason = "invalid manifest",
                        "[QUEUE] skipped invalid pending"
                    );
                    delete_pending_dir(&path, &id);
                }
            }
        }
        info!(count = loaded_count, "[QUEUE] pending loaded count");
        Ok(())
    }

    pub fn add_pending_clip(&mut self, clip: PendingClipExport) {
        info!(
            id = %clip.id,
            dedupe_key = %clip.dedupe_key,
            reason = ?clip.reason,
            status = ?clip.status,
            title = %clip.title,
            start_time = ?clip.start_time,
            end_time = ?clip.end_time,
            exportable_at = ?clip.exportable_at,
            pre_event_seconds = clip.pre_event_seconds,
            post_event_seconds = clip.post_event_seconds,
            "[QUEUE] add pending"
        );
        if let Some(index) = self
            .pending
            .iter()
            .position(|pending| pending.id == clip.id)
        {
            let existing_status = self.pending[index].status.clone();
            let existing_dedupe_key = self.pending[index].dedupe_key.clone();
            if clip.status == PendingExportStatus::WaitingPostEvent
                && pending_status_is_advanced(&existing_status)
            {
                info!(
                    id = %clip.id,
                    dedupe_key = %clip.dedupe_key,
                    current_status = ?existing_status,
                    attempted_status = ?clip.status,
                    "[QUEUE] skip downgrade"
                );
                info!(
                    exportable_count = self.exportable_count(),
                    total_count = self.pending.len(),
                    "[QUEUE] total_count"
                );
                return;
            }
            if existing_dedupe_key == clip.dedupe_key && existing_status == clip.status {
                info!(
                    id = %clip.id,
                    dedupe_key = %clip.dedupe_key,
                    "[QUEUE] skip duplicate pending"
                );
                info!(
                    exportable_count = self.exportable_count(),
                    total_count = self.pending.len(),
                    "[QUEUE] total_count"
                );
                return;
            }
            self.pending[index] = clip;
            info!(
                exportable_count = self.exportable_count(),
                total_count = self.pending.len(),
                "[QUEUE] total_count"
            );
            return;
        }
        if self.pending.iter().any(|pending| {
            pending.dedupe_key == clip.dedupe_key
                && !matches!(pending.status, PendingExportStatus::Ready)
        }) {
            info!(
                id = %clip.id,
                dedupe_key = %clip.dedupe_key,
                "[QUEUE] skip duplicate pending"
            );
            info!(
                exportable_count = self.exportable_count(),
                total_count = self.pending.len(),
                "[QUEUE] total_count"
            );
            return;
        }
        self.pending.push(clip);
        info!(
            exportable_count = self.exportable_count(),
            total_count = self.pending.len(),
            "[QUEUE] total_count"
        );
    }

    fn exportable_count(&self) -> usize {
        self.pending.iter().filter(|clip| clip.can_export()).count()
    }

    pub fn pending_count(&self) -> usize {
        self.pending
            .iter()
            .filter(|clip| clip.is_visible_in_pending_queue())
            .count()
    }

    pub fn pending_dtos(&mut self, auto_config: &AutoClipConfig) -> Vec<PendingClipExportDto> {
        let mut next_pending = Vec::with_capacity(self.pending.len());
        for clip in self.pending.drain(..) {
            if clip.status == PendingExportStatus::Ready {
                info!(
                    id = %clip.id,
                    reason = "ready status",
                    "[QUEUE] get_pending_export_clips skipped"
                );
                continue;
            }
            if let Some(path) = final_clip_exists_for_clip(&clip, auto_config) {
                info!(
                    id = %clip.id,
                    path = %path.display(),
                    reason = "final clip already exists",
                    "[QUEUE] get_pending_export_clips skipped"
                );
                if auto_config.delete_ready_after_export {
                    delete_pending_dir(&clip.pending_dir, &clip.id);
                }
                continue;
            }
            if clip.requires_frozen_segments() && !clip_frozen_segments_valid(&clip) {
                info!(
                    id = %clip.id,
                    reason = "missing segments",
                    "[QUEUE] get_pending_export_clips skipped"
                );
                delete_pending_dir(&clip.pending_dir, &clip.id);
                continue;
            }
            if clip.is_visible_in_pending_queue() {
                next_pending.push(clip);
            }
        }
        self.pending = next_pending;

        let dtos = self
            .pending
            .iter()
            .map(PendingClipExport::dto)
            .collect::<Vec<_>>();
        info!(count = dtos.len(), "[QUEUE] get_pending_export_clips count");
        info!(
            exportable_count = dtos.iter().filter(|clip| clip.can_export).count(),
            total_count = dtos.len(),
            "[QUEUE] total_count"
        );
        dtos
    }

    pub async fn export_pending_clips(
        &mut self,
        buffer: &ReplayBufferHandle,
        auto_config: &AutoClipConfig,
    ) -> ExportSummary {
        let not_ready = self
            .pending
            .iter()
            .filter(|clip| {
                matches!(
                    clip.status,
                    PendingExportStatus::WaitingPostEvent | PendingExportStatus::FreezingSegments
                )
            })
            .count();
        let export_ids = self
            .pending
            .iter()
            .filter(|clip| {
                clip.status == PendingExportStatus::ReadyToExport
                    || (clip.status == PendingExportStatus::Failed && clip.retryable)
            })
            .map(|clip| clip.id.clone())
            .collect::<Vec<_>>();
        let total = export_ids.len();
        let mut summary = ExportSummary {
            total,
            completed: 0,
            failed: 0,
        };

        info!(exportable_count = total, not_ready, "[EXPORT] starting");
        if total > 0 {
            if let Err(error) = preflight_export_disk_space(buffer, auto_config, total) {
                let message = format!(
                    "Espace disque insuffisant pour exporter les clips. Libérez de l'espace disque puis réessayez. {error}"
                );
                error!(%message, "deferred export preflight failed");
                summary.failed = total;
                emit_export_progress(
                    auto_config,
                    ExportProgressPayload {
                        active: false,
                        total,
                        completed: 0,
                        failed: total,
                        current_clip_id: None,
                        current_clip_title: None,
                        current_step: ExportProgressStep::Failed,
                        progress: 0,
                        message,
                    },
                );
                return summary;
            }
        }
        emit_export_progress(
            auto_config,
            ExportProgressPayload {
                active: total > 0,
                total,
                completed: 0,
                failed: 0,
                current_clip_id: None,
                current_clip_title: None,
                current_step: ExportProgressStep::Preparing,
                progress: 0,
                message: if total == 0 {
                    if not_ready > 0 {
                        "Certains clips ne sont pas encore prêts à exporter.".to_owned()
                    } else {
                        "Aucun clip en attente d'export".to_owned()
                    }
                } else {
                    "Préparation de l'export des clips...".to_owned()
                },
            },
        );

        for (position, clip_id) in export_ids.into_iter().enumerate() {
            let Some(index) = self.pending.iter().position(|clip| clip.id == clip_id) else {
                continue;
            };
            self.pending[index].status = PendingExportStatus::Exporting;
            self.pending[index].error = None;
            let clip = self.pending[index].clone();
            info!(
                id = %clip.id,
                clip_number = position + 1,
                total,
                title = %clip.title,
                reason = ?clip.reason,
                segments = clip.frozen_segments.len(),
                "[EXPORT] clip"
            );
            send_ui_event(
                auto_config,
                AppEvent::ClipStatusChanged {
                    payload: clip.status_payload(ClipStatus::Exporting, Some(0), None, None),
                },
            );

            emit_clip_export_step(
                auto_config,
                &clip,
                total,
                summary.completed,
                summary.failed,
                position,
                ExportProgressStep::Preparing,
                0.05,
                "Préparation du clip",
            );
            emit_clip_export_step(
                auto_config,
                &clip,
                total,
                summary.completed,
                summary.failed,
                position,
                ExportProgressStep::Assembling,
                0.25,
                "Assemblage des segments",
            );
            emit_clip_export_step(
                auto_config,
                &clip,
                total,
                summary.completed,
                summary.failed,
                position,
                ExportProgressStep::Encoding,
                0.85,
                "Encodage du clip",
            );

            let result = save_frozen_replay(
                clip.segments_dir.clone(),
                auto_config.output_dir.clone(),
                auto_config.keep_segments,
                clip.clip_context(),
            )
            .await;
            match result {
                Ok(replay) => {
                    emit_clip_export_step(
                        auto_config,
                        &clip,
                        total,
                        summary.completed,
                        summary.failed,
                        position,
                        ExportProgressStep::Saving,
                        0.95,
                        "Sauvegarde du clip",
                    );
                    crate::capture::buffer::print_saved_replay(&replay);
                    let final_video_path = match replay.final_video_path.as_deref() {
                        Some(path) => path.to_path_buf(),
                        None => {
                            summary.failed += 1;
                            self.mark_failed(
                                index,
                                auto_config,
                                &clip,
                                "Export terminé sans chemin vidéo final".to_owned(),
                                clip_frozen_segments_valid(&clip),
                            );
                            continue;
                        }
                    };
                    emit_clip_export_step(
                        auto_config,
                        &clip,
                        total,
                        summary.completed,
                        summary.failed,
                        position,
                        ExportProgressStep::Thumbnail,
                        0.98,
                        "Génération de la miniature",
                    );
                    let thumbnail_path = generate_clip_thumbnail(&final_video_path).await;
                    let verified = match verify_completed_export(&replay, thumbnail_path.clone()) {
                        Ok(verified) => verified,
                        Err(error) => {
                            summary.failed += 1;
                            self.mark_failed(
                                index,
                                auto_config,
                                &clip,
                                error.to_string(),
                                clip_frozen_segments_valid(&clip),
                            );
                            continue;
                        }
                    };
                    let mut ready_clip = clip.clone();
                    ready_clip.status = PendingExportStatus::Ready;
                    if let Err(error) = write_pending_manifest_with_outputs(
                        &ready_clip,
                        Some(verified.final_video_path.clone()),
                        Some(verified.metadata_path.clone()),
                        verified.thumbnail_path.clone(),
                    ) {
                        summary.failed += 1;
                        self.mark_failed(
                            index,
                            auto_config,
                            &clip,
                            error.to_string(),
                            clip_frozen_segments_valid(&clip),
                        );
                        continue;
                    }
                    self.pending[index].status = PendingExportStatus::Ready;
                    self.pending[index].error = None;
                    self.pending[index].retryable = false;
                    emit_clip_export_step(
                        auto_config,
                        &clip,
                        total,
                        summary.completed,
                        summary.failed,
                        position,
                        ExportProgressStep::Done,
                        1.0,
                        "Clip exporté",
                    );
                    summary.completed += 1;
                    let mut payload = clip.status_payload(
                        ClipStatus::Ready,
                        Some(100),
                        Some(verified.final_video_path.clone()),
                        None,
                    );
                    payload.thumbnail_path = verified.thumbnail_path.clone();
                    payload.duration_seconds = Some(clip.duration_seconds());
                    payload.size_bytes = Some(verified.size_bytes);
                    send_ui_event(auto_config, AppEvent::ClipStatusChanged { payload });
                    send_ui_event(
                        auto_config,
                        AppEvent::ClipSaved {
                            path: verified.final_video_path.clone(),
                            reason: clip.reason,
                            duration_seconds: clip.duration_seconds(),
                            size_bytes: verified.size_bytes,
                        },
                    );
                    if auto_config.delete_ready_after_export {
                        info!(id = %clip.id, "[EXPORT] removing pending dir");
                        if let Err(error) = std::fs::remove_dir_all(&clip.pending_dir) {
                            error!(
                                %error,
                                id = %clip.id,
                                path = %clip.pending_dir.display(),
                                "failed to remove pending export directory after success"
                            );
                        } else {
                            info!(id = %clip.id, "[EXPORT] pending dir removed");
                        }
                    }
                }
                Err(error) => {
                    summary.failed += 1;
                    self.mark_failed(index, auto_config, &clip, error.to_string(), true);
                }
            }
        }

        self.pending
            .retain(|clip| clip.status != PendingExportStatus::Ready);
        info!(
            count = self.pending.len(),
            "[QUEUE] pending after export count"
        );

        info!(
            total = summary.total,
            completed = summary.completed,
            failed = summary.failed,
            "[EXPORT] completed"
        );
        emit_export_progress(
            auto_config,
            ExportProgressPayload {
                active: false,
                total: summary.total,
                completed: summary.completed,
                failed: summary.failed,
                current_clip_id: None,
                current_clip_title: None,
                current_step: if summary.failed > 0 {
                    ExportProgressStep::Failed
                } else {
                    ExportProgressStep::Done
                },
                progress: 100,
                message: if not_ready > 0 && summary.completed == 0 && summary.failed == 0 {
                    "Certains clips ne sont pas encore prêts à exporter.".to_owned()
                } else if summary.failed > 0 {
                    format!(
                        "{} clips exportés, {} erreur{}",
                        summary.completed,
                        summary.failed,
                        if summary.failed > 1 { "s" } else { "" }
                    )
                } else {
                    "Export terminé".to_owned()
                },
            },
        );

        summary
    }

    pub async fn freeze_due_pending_clips(
        &mut self,
        buffer: &ReplayBufferHandle,
        auto_config: &AutoClipConfig,
    ) {
        let now = SystemTime::now();
        let due_ids = self
            .pending
            .iter()
            .filter(|clip| clip.should_attempt_freeze(now))
            .map(|clip| clip.id.clone())
            .collect::<Vec<_>>();

        for clip_id in due_ids {
            let Some(index) = self.pending.iter().position(|clip| clip.id == clip_id) else {
                continue;
            };
            let mut clip = self.pending[index].clone();
            clip.last_freeze_attempt_at = Some(now);
            send_ui_event(
                auto_config,
                AppEvent::ClipStatusChanged {
                    payload: clip.status_payload(
                        ClipStatus::FreezingSegments,
                        Some(35),
                        None,
                        None,
                    ),
                },
            );
            freeze_pending_clip_segments(buffer, &mut clip, auto_config).await;
            self.pending[index] = clip.clone();
            send_ui_event(
                auto_config,
                AppEvent::ClipStatusChanged {
                    payload: clip.status_payload(
                        clip_status_for_pending(&clip),
                        clip.dto().progress,
                        None,
                        clip.error.clone(),
                    ),
                },
            );
        }
    }

    pub fn delete_pending_clip(&mut self, id: &str) -> Result<(), String> {
        let Some(index) = self.pending.iter().position(|clip| clip.id == id) else {
            return Ok(());
        };
        let clip = self.pending.remove(index);
        if clip.pending_dir.exists() {
            std::fs::remove_dir_all(&clip.pending_dir).map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn mark_failed(
        &mut self,
        index: usize,
        auto_config: &AutoClipConfig,
        clip: &PendingClipExport,
        message: String,
        retryable: bool,
    ) {
        error!(
            id = %clip.id,
            title = %clip.title,
            error = %message,
            "deferred clip export failed"
        );
        self.pending[index].status = PendingExportStatus::Failed;
        self.pending[index].error = Some(message.clone());
        self.pending[index].retryable = retryable;
        send_ui_event(
            auto_config,
            AppEvent::ClipStatusChanged {
                payload: clip.status_payload(ClipStatus::Failed, None, None, Some(message)),
            },
        );
    }
}

impl PendingClipExport {
    fn dto(&self) -> PendingClipExportDto {
        PendingClipExportDto {
            id: self.id.clone(),
            reason: self.reason,
            title: self.title.clone(),
            created_at: system_time_rfc3339(self.detected_at),
            status: self.status.clone(),
            progress: match self.status {
                PendingExportStatus::WaitingPostEvent => None,
                PendingExportStatus::FreezingSegments => Some(35),
                PendingExportStatus::ReadyToExport => Some(100),
                PendingExportStatus::Exporting => Some(0),
                PendingExportStatus::Ready => Some(100),
                PendingExportStatus::Failed => None,
                PendingExportStatus::Expired => None,
            },
            error: self.error.clone(),
            exportable_at: system_time_rfc3339(self.exportable_at),
            is_exportable: self.is_exportable_at(SystemTime::now()),
            can_export: self.can_export(),
            retryable: self.retryable,
        }
    }

    fn is_exportable_at(&self, now: SystemTime) -> bool {
        self.exportable_at
            .duration_since(now)
            .map_or(true, |remaining| remaining.is_zero())
    }

    fn can_export(&self) -> bool {
        self.status == PendingExportStatus::ReadyToExport
            || (self.status == PendingExportStatus::Failed && self.retryable)
    }

    fn requires_frozen_segments(&self) -> bool {
        matches!(
            self.status,
            PendingExportStatus::ReadyToExport | PendingExportStatus::Exporting
        ) || (self.status == PendingExportStatus::Failed && self.retryable)
    }

    fn is_visible_in_pending_queue(&self) -> bool {
        matches!(
            self.status,
            PendingExportStatus::WaitingPostEvent
                | PendingExportStatus::FreezingSegments
                | PendingExportStatus::ReadyToExport
                | PendingExportStatus::Expired
        ) || (self.status == PendingExportStatus::Failed && self.retryable)
    }

    fn should_attempt_freeze(&self, now: SystemTime) -> bool {
        matches!(
            self.status,
            PendingExportStatus::WaitingPostEvent | PendingExportStatus::FreezingSegments
        ) && self.is_exportable_at(now)
            && self
                .next_freeze_attempt_at
                .is_none_or(|retry_at| retry_at <= now)
    }

    fn duration_seconds(&self) -> u64 {
        self.pre_event_seconds
            .saturating_add(self.post_event_seconds)
            .max(1)
    }

    fn clip_context(&self) -> ClipContext {
        ClipContext {
            reason: self.reason,
            event: self.events.first().cloned(),
            events: self.events.clone(),
            player_name: self.player_name.clone(),
            pending_clip_id: Some(self.id.clone()),
            pending_dedupe_key: Some(self.dedupe_key.clone()),
            video_quality: self.quality,
            quality_preset: self.quality_preset,
            duration_seconds: self.duration_seconds(),
            post_event_seconds: self.post_event_seconds,
            segment_seconds: self.segment_seconds,
            first_event_time: Some(self.start_time + Duration::from_secs(self.pre_event_seconds)),
            last_event_time: Some(
                self.end_time
                    .checked_sub(Duration::from_secs(self.post_event_seconds))
                    .unwrap_or(self.end_time),
            ),
        }
    }

    fn status_payload(
        &self,
        status: ClipStatus,
        progress: Option<u8>,
        file_path: Option<PathBuf>,
        error: Option<String>,
    ) -> ClipStatusPayload {
        let title = match status {
            ClipStatus::WaitingPostEvent => "Capture de la fin du clip...".to_owned(),
            ClipStatus::FreezingSegments => "Préservation des segments...".to_owned(),
            ClipStatus::ReadyToExport => self.title.clone(),
            ClipStatus::Exporting => "Export en cours...".to_owned(),
            ClipStatus::Expired => "Clip expiré".to_owned(),
            _ => self.title.clone(),
        };
        ClipStatusPayload {
            id: self.id.clone(),
            status,
            reason: self.reason,
            title,
            created_at: system_time_rfc3339(self.detected_at),
            file_path,
            thumbnail_path: None,
            duration_seconds: None,
            size_bytes: None,
            progress,
            error,
            exportable_at: Some(system_time_rfc3339(self.exportable_at)),
            can_export: can_export_for_status(status, self.retryable),
            retryable: self.retryable,
        }
    }
}

fn can_export_for_status(status: ClipStatus, retryable: bool) -> bool {
    status == ClipStatus::ReadyToExport || (status == ClipStatus::Failed && retryable)
}

pub(crate) fn effective_post_event_delay(
    post_event_delay: Duration,
    segment_seconds: u64,
) -> Duration {
    post_event_delay + Duration::from_secs(segment_seconds.saturating_add(1))
}

fn apply_runtime_config(
    auto_config: &mut AutoClipConfig,
    config: &AppConfig,
) -> RuntimeConfigUpdateResult {
    let next_output_dir = config.clip.output_dir_path().ok();
    let restart_required = auto_config.buffer_seconds != config.clip.seconds
        || auto_config.segment_seconds != config.clip.segment_seconds
        || auto_config.output_dir != next_output_dir
        || auto_config.source != config.clip.source
        || auto_config.quality_preset != config.clip.quality
        || auto_config.quality.fps != config.clip.fps
        || auto_config.quality.video_bitrate_kbps != config.clip.video_bitrate_kbps;

    auto_config.export_mode = config.clip.export_mode;
    auto_config.triggers = config.triggers.clone();
    auto_config.post_event_delay = Duration::from_secs(config.clip.post_event_seconds);
    auto_config.multi_kill_window = Duration::from_secs(config.clip.multi_kill_window_seconds);
    if let Ok(pending_export_dir) = config.pending_exports.pending_export_dir_path() {
        auto_config.pending_export_dir = pending_export_dir;
    }
    auto_config.delete_ready_after_export = config.pending_exports.delete_ready_after_export;

    let message = if restart_required {
        "Configuration appliquée partiellement; les paramètres vidéo seront actifs après redémarrage du buffer."
    } else {
        "Configuration appliquée au backend actif."
    };

    RuntimeConfigUpdateResult {
        applied_live: true,
        restart_required,
        active_export_mode: auto_config.export_mode,
        message: message.to_owned(),
    }
}

fn replay_buffer_seconds_for_auto(auto_config: &AutoClipConfig) -> u64 {
    let multi_kill_margin = auto_config
        .multi_kill_window
        .as_secs()
        .saturating_mul(8)
        .max(60);
    auto_config.buffer_seconds.saturating_add(multi_kill_margin)
}

pub async fn run_auto_clip(
    client: WarThunderClient,
    warthunder_config: WarThunderConfig,
    mut auto_config: AutoClipConfig,
) -> anyhow::Result<()> {
    let mut command_rx = auto_config.command_rx.take();
    let replay_buffer_seconds = replay_buffer_seconds_for_auto(&auto_config);
    let buffer = ReplayBufferHandle::start(ReplayBufferConfig {
        buffer_seconds: replay_buffer_seconds,
        segment_seconds: auto_config.segment_seconds,
        output_dir: auto_config.output_dir.clone(),
        source: auto_config.source,
        keep_segments: auto_config.keep_segments,
        quality_preset: auto_config.quality_preset,
        quality: auto_config.quality,
    })
    .await?;

    println!(
        "Replay buffer active: {}s ({}s solo clips, multi-kill margin enabled)",
        replay_buffer_seconds, auto_config.buffer_seconds
    );
    println!("Video target: {}", auto_config.quality.log_summary());
    println!("Auto-clip armed for personal War Thunder kills.");
    println!("Press Ctrl+C to stop.");

    let player_name = warthunder_config.player_name.as_deref();
    let mut configured_post_event_delay = auto_config.post_event_delay;
    let mut effective_save_delay =
        effective_post_event_delay(auto_config.post_event_delay, auto_config.segment_seconds);
    let mut state = AutoWatchState::new();
    if auto_config.include_history {
        println!("[WT] include-history enabled: processing existing events");
    } else {
        bootstrap(&client, &mut state).await;
        println!(
            "[WT] initialized cursors: chat={}, hud_evt={}, hud_dmg={}",
            state.last_chat_id, state.last_evt_msg_id, state.last_dmg_msg_id
        );
        println!("[WT] watching for new events only");
    }

    let mut cooldown = Cooldown::new(auto_config.cooldown);
    let mut pending_clips = Vec::<PendingClip>::new();
    let mut export_queue = ExportQueue::default();
    if let Err(error) = export_queue.load_from_pending_dir(&auto_config) {
        error!(%error, "failed to load pending exports on startup");
    }
    let mut wt_tick = interval(warthunder_config.poll_interval());
    wt_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut cleanup_tick = interval(Duration::from_secs(1));
    cleanup_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let buffer_started_at = Instant::now();
    let mut last_wt_connected = None;
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    let mut buffer = Some(buffer);
    let result = loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("received Ctrl+C, stopping auto-clip");
                break Ok(());
            }
            _ = cleanup_tick.tick() => {
                if let Some(buffer) = &buffer {
                    if let Err(error) = buffer.prune() {
                        break Err(error);
                    }
                    send_ui_event(&auto_config, AppEvent::BufferProgress {
                        filled_secs: buffer_started_at
                            .elapsed()
                            .as_secs_f32()
                            .min(replay_buffer_seconds as f32),
                        total_secs: replay_buffer_seconds as f32,
                    });
                    if let Err(error) = save_ready_pending_clips(
                        buffer,
                        &mut pending_clips,
                        &mut export_queue,
                        &mut cooldown,
                        Instant::now(),
                        player_name,
                        &auto_config,
                        configured_post_event_delay,
                    ).await {
                        break Err(error);
                    }
                    export_queue.freeze_due_pending_clips(buffer, &auto_config).await;
                }
            }
            command = recv_auto_command(&mut command_rx), if command_rx.is_some() => {
                let Some(command) = command else {
                    command_rx = None;
                    continue;
                };
                let Some(buffer) = &buffer else {
                    continue;
                };
                match command {
                    AutoClipCommand::SaveManualClip => {
                        handle_manual_clip_command(
                            buffer,
                            &mut export_queue,
                            player_name,
                            &auto_config,
                            configured_post_event_delay,
                        ).await;
                    }
                    AutoClipCommand::ExportPendingClips { respond_to } => {
                        let summary = export_queue.export_pending_clips(buffer, &auto_config).await;
                        let _ = respond_to.send(summary);
                    }
                    AutoClipCommand::GetPendingExportClips { respond_to } => {
                        let _ = respond_to.send(export_queue.pending_dtos(&auto_config));
                    }
                    AutoClipCommand::DeletePendingClip { id, respond_to } => {
                        let _ = respond_to.send(export_queue.delete_pending_clip(&id));
                    }
                    AutoClipCommand::UpdateConfig { config, respond_to } => {
                        let result = apply_runtime_config(&mut auto_config, &config);
                        configured_post_event_delay = auto_config.post_event_delay;
                        effective_save_delay = effective_post_event_delay(
                            auto_config.post_event_delay,
                            auto_config.segment_seconds,
                        );
                        info!(
                            export_mode = ?auto_config.export_mode,
                            restart_required = result.restart_required,
                            "auto backend runtime config updated"
                        );
                        let _ = respond_to.send(result);
                    }
                }
            }
            _ = wt_tick.tick() => {
                let (events, wt_connected) = poll_personal_events(&client, &mut state, player_name, &auto_config.triggers).await;
                if last_wt_connected != Some(wt_connected) {
                    send_ui_event(
                        &auto_config,
                        if wt_connected {
                            AppEvent::WtConnected
                        } else {
                            AppEvent::WtDisconnected
                        },
                    );
                    last_wt_connected = Some(wt_connected);
                }
                for detected in events {
                    let event = detected.event;
                    let summary = event_summary(&event);
                    let reason = detected.reason;
                    println!("[WT] event detected ({reason:?}): {summary}");
                    let (vehicle, target) = event_vehicle_target(&event);
                    send_ui_event(&auto_config, AppEvent::KillDetected {
                        reason,
                        vehicle,
                        target,
                        description: summary.clone(),
                    });
                    let now = detected.detected_at;
                    if pending_clips
                        .iter()
                        .any(|pending| pending.event_keys.contains(&detected.canonical_key))
                    {
                        debug!(
                            canonical_key = %detected.canonical_key,
                            "duplicate event already exists in pending clip"
                        );
                        continue;
                    }
                    if let Some(pending_index) = pending_index_for_multi_kill(
                        &pending_clips,
                        reason,
                        now,
                        detected.game_time,
                        auto_config.multi_kill_window,
                    ) {
                        let pending = &mut pending_clips[pending_index];
                        if pending.add_event(
                            event,
                            detected.canonical_key,
                            now,
                            detected.game_time,
                            detected.detected_wall_time,
                            effective_save_delay,
                            summary,
                        ) {
                            println!(
                                "[CLIP] added event to pending clip, now {} kills; save delayed by {}s",
                                pending.kill_count(),
                                effective_save_delay.as_secs()
                            );
                            if auto_config.export_mode == ClipExportMode::Deferred {
                                let clip = pending_export_from_pending(
                                    pending,
                                    player_name,
                                    &auto_config,
                                    configured_post_event_delay,
                                );
                                export_queue.add_pending_clip(clip.clone());
                                info!(
                                    id = %clip.id,
                                    reason = ?clip.reason,
                                    export_mode = ?auto_config.export_mode,
                                    "deferred pending clip emitted"
                                );
                                send_ui_event(
                                    &auto_config,
                                    AppEvent::ClipStatusChanged {
                                        payload: clip.status_payload(
                                            ClipStatus::WaitingPostEvent,
                                            None,
                                            None,
                                            None,
                                        ),
                                    },
                                );
                            } else {
                                send_ui_event(
                                    &auto_config,
                                    AppEvent::ClipStatusChanged {
                                        payload: status_payload(
                                            pending,
                                            ClipStatus::Detected,
                                            pending.reason,
                                            format!(
                                                "Multi-kill en attente : {} kills, sauvegarde dans {}s",
                                                pending.kill_count(),
                                                effective_save_delay.as_secs()
                                            ),
                                            Some(15),
                                            None,
                                        ),
                                    },
                                );
                            }
                            debug!(
                                first_event_time = ?pending.first_event_wall_time,
                                last_event_time = ?pending.last_event_wall_time,
                                kill_count = pending.kill_count(),
                                "pending clip state"
                            );
                        } else {
                            debug!("duplicate event ignored inside pending clip");
                        }
                        continue;
                    }
                    if !cooldown.allows(now) {
                        debug!(
                            ?event,
                            "[CLIP] cooldown active, but scheduling distinct detected kill"
                        );
                    }

                    let pending = schedule_pending_clip(
                        event,
                        detected.canonical_key,
                        reason,
                        summary.clone(),
                        now,
                        detected.game_time,
                        detected.detected_wall_time,
                        effective_save_delay,
                    );
                    if auto_config.export_mode == ClipExportMode::Deferred {
                        println!(
                            "[CLIP] scheduled deferred export in {}s...",
                            effective_save_delay.as_secs()
                        );
                    } else {
                        println!(
                            "[CLIP] scheduled replay save in {}s...",
                            effective_save_delay.as_secs()
                        );
                    }
                    if auto_config.export_mode == ClipExportMode::Deferred {
                        let clip = pending_export_from_pending(
                            &pending,
                            player_name,
                            &auto_config,
                            configured_post_event_delay,
                        );
                        export_queue.add_pending_clip(clip.clone());
                        info!(
                            id = %clip.id,
                            reason = ?clip.reason,
                            export_mode = ?auto_config.export_mode,
                            "deferred pending clip emitted"
                        );
                        send_ui_event(
                            &auto_config,
                            AppEvent::ClipStatusChanged {
                                payload: clip.status_payload(
                                    ClipStatus::WaitingPostEvent,
                                    None,
                                    None,
                                    None,
                                ),
                            },
                        );
                    } else {
                        send_ui_event(
                            &auto_config,
                            AppEvent::ClipStatusChanged {
                                payload: status_payload(
                                    &pending,
                                    ClipStatus::Detected,
                                    reason,
                                    format!(
                                        "Clip programmé: {summary} (sauvegarde dans {}s)",
                                        effective_save_delay.as_secs()
                                    ),
                                    Some(10),
                                    None,
                                ),
                            },
                        );
                    }
                    pending_clips.push(pending);
                }
            }
        }
    };

    if let Some(buffer) = buffer.take() {
        let stop_result = buffer.stop().await;
        if result.is_ok() {
            stop_result?;
        } else if let Err(error) = stop_result {
            debug!(%error, "failed to stop buffer after auto-clip error");
        }
    }

    result
}

fn schedule_pending_clip(
    event: WarThunderEvent,
    event_key: String,
    reason: ClipReason,
    description: String,
    event_time: Instant,
    event_game_time: Option<Duration>,
    event_wall_time: SystemTime,
    delay: Duration,
) -> PendingClip {
    PendingClip::new(
        event,
        event_key,
        reason,
        event_time,
        event_game_time,
        event_wall_time,
        delay,
        description,
    )
}

fn clip_context_for_pending(
    pending: &PendingClip,
    player_name: Option<&str>,
    auto_config: &AutoClipConfig,
    configured_post_event_delay: Duration,
) -> ClipContext {
    let event = pending.events.first().cloned();
    ClipContext {
        reason: pending.reason,
        event,
        events: pending.events.clone(),
        player_name: player_name.map(str::to_owned),
        pending_clip_id: Some(pending.clip_id.clone()),
        pending_dedupe_key: Some(pending_export_dedupe_key(pending)),
        video_quality: auto_config.quality,
        quality_preset: auto_config.quality_preset,
        duration_seconds: auto_config.buffer_seconds,
        post_event_seconds: configured_post_event_delay.as_secs(),
        segment_seconds: auto_config.segment_seconds,
        first_event_time: Some(pending.first_event_wall_time),
        last_event_time: Some(pending.last_event_wall_time),
    }
}

fn status_payload(
    pending: &PendingClip,
    status: ClipStatus,
    reason: ClipReason,
    title: String,
    progress: Option<u8>,
    error: Option<String>,
) -> ClipStatusPayload {
    ClipStatusPayload {
        id: pending.clip_id.clone(),
        status,
        reason,
        title,
        created_at: pending.created_at.clone(),
        file_path: None,
        thumbnail_path: None,
        duration_seconds: None,
        size_bytes: None,
        progress,
        error,
        exportable_at: None,
        can_export: false,
        retryable: false,
    }
}

fn pending_export_from_pending(
    pending: &PendingClip,
    player_name: Option<&str>,
    auto_config: &AutoClipConfig,
    configured_post_event_delay: Duration,
) -> PendingClipExport {
    let pre_event_seconds = auto_config
        .buffer_seconds
        .saturating_sub(configured_post_event_delay.as_secs());
    let post_event_seconds = configured_post_event_delay.as_secs();
    let event_start = pending
        .first_event_wall_time
        .checked_sub(Duration::from_secs(pre_event_seconds))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let event_end = pending.last_event_wall_time + Duration::from_secs(post_event_seconds);
    let exportable_at = event_end + pending_export_stable_margin(auto_config.segment_seconds);
    let (pending_dir, segments_dir, manifest_path) =
        pending_export_paths(&auto_config.pending_export_dir, &pending.clip_id);
    PendingClipExport {
        id: pending.clip_id.clone(),
        reason: pending.reason,
        title: pending_export_title(pending.reason, pending.kill_count()),
        detected_at: pending.first_event_wall_time,
        game_time: pending.last_event_game_time,
        start_time: event_start,
        end_time: event_end,
        exportable_at,
        pending_dir,
        segments_dir,
        manifest_path,
        frozen_segments: Vec::new(),
        pre_event_seconds,
        post_event_seconds,
        status: PendingExportStatus::WaitingPostEvent,
        dedupe_key: pending_export_dedupe_key(pending),
        events: pending.events.clone(),
        player_name: player_name.map(str::to_owned),
        quality: auto_config.quality,
        quality_preset: auto_config.quality_preset,
        segment_seconds: auto_config.segment_seconds,
        error: None,
        retryable: false,
        next_freeze_attempt_at: None,
        last_freeze_attempt_at: None,
        last_freeze_log_at: None,
    }
}

fn pending_manual_export(
    player_name: Option<&str>,
    auto_config: &AutoClipConfig,
) -> PendingClipExport {
    let detected_at = SystemTime::now();
    let pre_event_seconds = auto_config.buffer_seconds.max(1);
    let start_time = detected_at
        .checked_sub(Duration::from_secs(pre_event_seconds))
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let id = format!("clip_{}", Uuid::new_v4());
    let dedupe_key = format!("manual|{id}");
    let (pending_dir, segments_dir, manifest_path) =
        pending_export_paths(&auto_config.pending_export_dir, &id);
    PendingClipExport {
        id,
        reason: ClipReason::Manual,
        title: "Clip manuel".to_owned(),
        detected_at,
        game_time: None,
        start_time,
        end_time: detected_at,
        exportable_at: detected_at + pending_export_stable_margin(auto_config.segment_seconds),
        pending_dir,
        segments_dir,
        manifest_path,
        frozen_segments: Vec::new(),
        pre_event_seconds,
        post_event_seconds: 0,
        status: PendingExportStatus::WaitingPostEvent,
        dedupe_key,
        events: Vec::new(),
        player_name: player_name.map(str::to_owned),
        quality: auto_config.quality,
        quality_preset: auto_config.quality_preset,
        segment_seconds: auto_config.segment_seconds,
        error: None,
        retryable: false,
        next_freeze_attempt_at: None,
        last_freeze_attempt_at: None,
        last_freeze_log_at: None,
    }
}

fn pending_export_paths(base_dir: &Path, clip_id: &str) -> (PathBuf, PathBuf, PathBuf) {
    let pending_dir = base_dir.join(clip_id);
    let segments_dir = pending_dir.join("segments");
    let manifest_path = pending_dir.join("manifest.json");
    (pending_dir, segments_dir, manifest_path)
}

fn pending_export_stable_margin(segment_seconds: u64) -> Duration {
    Duration::from_secs(segment_seconds.saturating_add(1))
}

fn pending_export_title(reason: ClipReason, kill_count: usize) -> String {
    match reason {
        ClipReason::TargetDestroyed => "Cible détruite".to_owned(),
        ClipReason::BaseDestroyed => "Base détruite".to_owned(),
        ClipReason::PlayerDestroyed => "Joueur détruit".to_owned(),
        ClipReason::MultiKill => format!("Multi-kill — {} kills", kill_count.max(2)),
        ClipReason::Manual => "Clip manuel".to_owned(),
        ClipReason::Unknown => "Clip".to_owned(),
    }
}

fn pending_event_dedupe_key(
    reason: ClipReason,
    event: &WarThunderEvent,
    event_game_time: Option<Duration>,
    event_wall_time: SystemTime,
) -> String {
    let event_key = canonical_event_key(event).unwrap_or_else(|| event_summary(event));
    let timestamp = match event_game_time {
        Some(game_time) => format!("wt:{}", game_time.as_secs()),
        None => format!(
            "wall:{}",
            event_wall_time
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0)
        ),
    };
    format!("{}|{}|{}", reason.slug(), event_key, timestamp)
}

fn pending_export_dedupe_key(pending: &PendingClip) -> String {
    let mut keys = pending.dedupe_keys.iter().cloned().collect::<Vec<_>>();
    keys.sort();
    format!("{}|{}", pending.reason.slug(), keys.join(";"))
}

fn pending_status_is_advanced(status: &PendingExportStatus) -> bool {
    matches!(
        status,
        PendingExportStatus::FreezingSegments
            | PendingExportStatus::ReadyToExport
            | PendingExportStatus::Exporting
            | PendingExportStatus::Ready
            | PendingExportStatus::Failed
            | PendingExportStatus::Expired
    )
}

fn fallback_manifest_dedupe_key(manifest: &PendingClipManifest) -> String {
    format!(
        "{}|{}|{}|{}",
        manifest.reason.slug(),
        manifest.title,
        manifest.event_time,
        manifest.id
    )
}

fn ready_pending_indices(pending_clips: &[PendingClip], now: Instant) -> Vec<usize> {
    pending_clips
        .iter()
        .enumerate()
        .filter_map(|(index, pending)| pending.is_ready(now).then_some(index))
        .collect()
}

fn pending_index_for_multi_kill(
    pending_clips: &[PendingClip],
    reason: ClipReason,
    event_time: Instant,
    event_game_time: Option<Duration>,
    multi_kill_window: Duration,
) -> Option<usize> {
    if reason != ClipReason::TargetDestroyed {
        return None;
    }

    pending_clips
        .iter()
        .enumerate()
        .find_map(|(index, pending)| {
            if !matches!(
                pending.reason,
                ClipReason::TargetDestroyed | ClipReason::MultiKill
            ) || !pending.events.iter().all(is_target_destroyed_clip_event)
            {
                return None;
            }

            if let (Some(event_game_time), Some(last_event_game_time)) =
                (event_game_time, pending.last_event_game_time)
            {
                return event_game_time
                    .checked_sub(last_event_game_time)
                    .filter(|elapsed| *elapsed <= multi_kill_window)
                    .map(|_| index);
            }

            event_time
                .checked_duration_since(pending.last_event_time)
                .filter(|elapsed| *elapsed <= multi_kill_window)
                .map(|_| index)
        })
}

async fn save_ready_pending_clips(
    buffer: &ReplayBufferHandle,
    pending_clips: &mut Vec<PendingClip>,
    export_queue: &mut ExportQueue,
    cooldown: &mut Cooldown,
    now: Instant,
    player_name: Option<&str>,
    auto_config: &AutoClipConfig,
    configured_post_event_delay: Duration,
) -> anyhow::Result<()> {
    let ready_indices = ready_pending_indices(pending_clips, now);
    for index in ready_indices.into_iter().rev() {
        let pending = pending_clips.remove(index);
        debug!(
            reason = ?pending.reason,
            event_age_ms = now.duration_since(pending.first_event_time).as_millis(),
            event_count = pending.events.len(),
            first_event_time = ?pending.first_event_wall_time,
            last_event_time = ?pending.last_event_wall_time,
            expected_clip_duration_seconds = auto_config.buffer_seconds,
            "saving pending auto-clip"
        );
        let context = clip_context_for_pending(
            &pending,
            player_name,
            auto_config,
            configured_post_event_delay,
        );
        if auto_config.export_mode == ClipExportMode::Deferred {
            let clip = pending_export_from_pending(
                &pending,
                player_name,
                auto_config,
                configured_post_event_delay,
            );
            export_queue.add_pending_clip(clip.clone());
            info!(
                id = %clip.id,
                reason = ?clip.reason,
                export_mode = ?auto_config.export_mode,
                "deferred pending clip emitted"
            );
            println!(
                "[CLIP] deferred clip queued/frozen; no final replay saved until manual export"
            );
            cooldown.record_save(now);
            continue;
        }
        println!("[CLIP] saving replay...");
        send_ui_event(
            auto_config,
            AppEvent::ClipStatusChanged {
                payload: status_payload(
                    &pending,
                    ClipStatus::Recording,
                    pending.reason,
                    "Capture en cours...".to_owned(),
                    Some(35),
                    None,
                ),
            },
        );
        send_ui_event(
            auto_config,
            AppEvent::ClipStatusChanged {
                payload: status_payload(
                    &pending,
                    ClipStatus::Encoding,
                    pending.reason,
                    "Encodage du clip...".to_owned(),
                    Some(72),
                    None,
                ),
            },
        );
        match buffer.save_replay(context).await {
            Ok(SaveReplayOutcome::Saved(replay)) => {
                send_ui_event(
                    auto_config,
                    AppEvent::ClipStatusChanged {
                        payload: status_payload(
                            &pending,
                            ClipStatus::Saving,
                            pending.reason,
                            "Sauvegarde du clip...".to_owned(),
                            Some(92),
                            None,
                        ),
                    },
                );
                crate::capture::buffer::print_saved_replay(&replay);
                if let Some(path) = replay.final_video_path.clone() {
                    let size_bytes = std::fs::metadata(&path)
                        .map(|metadata| metadata.len())
                        .unwrap_or(0);
                    let mut ready = status_payload(
                        &pending,
                        ClipStatus::Ready,
                        pending.reason,
                        "Clip prêt".to_owned(),
                        Some(100),
                        None,
                    );
                    ready.file_path = Some(path.clone());
                    ready.duration_seconds = Some(auto_config.buffer_seconds);
                    ready.size_bytes = Some(size_bytes);
                    send_ui_event(auto_config, AppEvent::ClipStatusChanged { payload: ready });
                    send_ui_event(
                        auto_config,
                        AppEvent::ClipSaved {
                            path,
                            reason: pending.reason,
                            duration_seconds: auto_config.buffer_seconds,
                            size_bytes,
                        },
                    );
                }
                cooldown.record_save(now);
            }
            Ok(SaveReplayOutcome::NotReadyYet(reason)) => {
                println!("[CLIP] retrying pending replay save in 1s: {reason}");
                requeue_pending_clip(pending_clips, pending, now);
            }
            Ok(SaveReplayOutcome::SkippedTooOld(reason)) => {
                println!("[CLIP] skipped pending replay save: {reason}");
                let (title, detail) = clip_failure_messages(&reason);
                send_ui_event(
                    auto_config,
                    AppEvent::ClipStatusChanged {
                        payload: status_payload(
                            &pending,
                            ClipStatus::Failed,
                            pending.reason,
                            title.clone(),
                            None,
                            Some(detail.clone()),
                        ),
                    },
                );
                send_ui_event(auto_config, AppEvent::ClipFailed { message: detail });
                cooldown.record_save(now);
            }
            Err(error) => {
                let message = error.to_string();
                let (title, detail) = clip_failure_messages(&message);
                send_ui_event(
                    auto_config,
                    AppEvent::ClipStatusChanged {
                        payload: status_payload(
                            &pending,
                            ClipStatus::Failed,
                            pending.reason,
                            title,
                            None,
                            Some(detail.clone()),
                        ),
                    },
                );
                send_ui_event(auto_config, AppEvent::ClipFailed { message: detail });
                cooldown.record_save(now);
            }
        }
    }
    Ok(())
}

fn clip_failure_messages(message: &str) -> (String, String) {
    if is_disk_space_error(message) {
        ("Espace disque insuffisant".to_owned(), message.to_owned())
    } else {
        (
            "Erreur pendant la création du clip".to_owned(),
            message.to_owned(),
        )
    }
}

fn is_disk_space_error(message: &str) -> bool {
    let message = message.to_lowercase();
    message.contains("no space left on device")
        || message.contains("enospc")
        || message.contains("not enough space")
        || message.contains("espace disque insuffisant")
}

fn requeue_pending_clip(
    pending_clips: &mut Vec<PendingClip>,
    mut pending: PendingClip,
    now: Instant,
) {
    pending.save_at = now + Duration::from_secs(1);
    pending_clips.push(pending);
}

async fn freeze_pending_clip_segments(
    buffer: &ReplayBufferHandle,
    clip: &mut PendingClipExport,
    _auto_config: &AutoClipConfig,
) {
    clip.status = PendingExportStatus::FreezingSegments;
    clip.error = None;
    clip.retryable = false;
    info!(id = %clip.id, "[CLIP] freezing segments");

    match buffer
        .freeze_replay_segments(clip.clip_context(), clip.segments_dir.clone())
        .await
    {
        Ok(FreezeReplayOutcome::Frozen(segments)) => {
            info!(
                id = %clip.id,
                count = segments.len(),
                "[CLIP] selected segments"
            );
            clip.frozen_segments = segments
                .iter()
                .filter_map(|path| {
                    path.strip_prefix(&clip.pending_dir)
                        .ok()
                        .map(Path::to_path_buf)
                })
                .collect();
            clip.status = PendingExportStatus::ReadyToExport;
            clip.error = None;
            clip.retryable = false;
            clip.next_freeze_attempt_at = None;
            if let Err(error) = write_pending_manifest(clip) {
                clip.status = PendingExportStatus::Failed;
                clip.error = Some(error.to_string());
                clip.retryable = true;
                error!(
                    id = %clip.id,
                    error = %error,
                    "failed to write pending export manifest"
                );
            } else {
                info!(
                    id = %clip.id,
                    path = %clip.manifest_path.display(),
                    "[CLIP] manifest written"
                );
                info!(id = %clip.id, "[CLIP] ready to export");
            }
        }
        Ok(FreezeReplayOutcome::NotReadyYet(reason)) => {
            mark_freeze_not_ready(clip, reason, SystemTime::now());
        }
        Ok(FreezeReplayOutcome::SkippedTooOld(reason)) => {
            clip.status = PendingExportStatus::Expired;
            clip.error = Some(format!(
                "Clip expiré : les segments vidéo ont déjà été écrasés par le buffer. {reason}"
            ));
            clip.retryable = false;
            clip.next_freeze_attempt_at = None;
            info!(id = %clip.id, reason = ?clip.error, "[CLIP] expired");
        }
        Err(error) => {
            clip.status = PendingExportStatus::Failed;
            clip.error = Some(error.to_string());
            clip.retryable = true;
            clip.next_freeze_attempt_at = None;
            error!(id = %clip.id, error = %error, "failed to freeze pending clip");
        }
    }
}

fn mark_freeze_not_ready(clip: &mut PendingClipExport, reason: String, now: SystemTime) {
    clip.status = PendingExportStatus::FreezingSegments;
    clip.error = Some(format!("Waiting for stable segment: {reason}"));
    clip.retryable = false;
    clip.next_freeze_attempt_at = Some(now + FREEZE_RETRY_DELAY);
    if clip
        .last_freeze_log_at
        .is_none_or(|logged_at| logged_at + FREEZE_NOT_READY_LOG_THROTTLE <= now)
    {
        info!(
            id = %clip.id,
            reason = %reason,
            retry_in_secs = FREEZE_RETRY_DELAY.as_secs(),
            "deferred freeze not ready yet"
        );
        clip.last_freeze_log_at = Some(now);
    }
}

fn write_pending_manifest(clip: &PendingClipExport) -> anyhow::Result<()> {
    write_pending_manifest_with_outputs(clip, None, None, None)
}

fn write_pending_manifest_with_outputs(
    clip: &PendingClipExport,
    final_video_path: Option<PathBuf>,
    metadata_path: Option<PathBuf>,
    thumbnail_path: Option<PathBuf>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&clip.pending_dir)?;
    let manifest = PendingClipManifest {
        id: clip.id.clone(),
        reason: clip.reason,
        title: clip.title.clone(),
        dedupe_key: Some(clip.dedupe_key.clone()),
        detected_at: system_time_rfc3339(clip.detected_at),
        event_time: system_time_rfc3339(
            clip.start_time + Duration::from_secs(clip.pre_event_seconds),
        ),
        start_time: system_time_rfc3339(clip.start_time),
        end_time: system_time_rfc3339(clip.end_time),
        pre_event_seconds: clip.pre_event_seconds,
        post_event_seconds: clip.post_event_seconds,
        segments: clip.frozen_segments.clone(),
        status: clip.status.clone(),
        events: clip.events.clone(),
        player_name: clip.player_name.clone(),
        final_video_path,
        metadata_path,
        thumbnail_path,
    };
    let json = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(&clip.manifest_path, json)?;
    Ok(())
}

fn read_pending_manifest(manifest_path: &Path) -> anyhow::Result<PendingClipManifest> {
    let content = std::fs::read_to_string(manifest_path)?;
    Ok(serde_json::from_str(&content)?)
}

fn pending_export_from_manifest_data(
    manifest_path: &Path,
    manifest: PendingClipManifest,
    auto_config: &AutoClipConfig,
) -> anyhow::Result<PendingClipExport> {
    let pending_dir = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow::anyhow!("pending manifest has no parent directory"))?;
    let segments_dir = pending_dir.join("segments");
    let detected_at = parse_system_time_rfc3339(&manifest.detected_at)?;
    let start_time = parse_system_time_rfc3339(&manifest.start_time)?;
    let end_time = parse_system_time_rfc3339(&manifest.end_time)?;
    let mut status = match manifest.status {
        PendingExportStatus::ReadyToExport
        | PendingExportStatus::Exporting
        | PendingExportStatus::FreezingSegments
        | PendingExportStatus::WaitingPostEvent => PendingExportStatus::ReadyToExport,
        PendingExportStatus::Failed => PendingExportStatus::Failed,
        PendingExportStatus::Expired => PendingExportStatus::Expired,
        PendingExportStatus::Ready => PendingExportStatus::Ready,
    };
    let mut error = None;
    let mut retryable = manifest.status == PendingExportStatus::Failed;

    let missing_segment = manifest
        .segments
        .iter()
        .map(|segment| pending_dir.join(segment))
        .find(|path| !path.is_file());
    if manifest.segments.is_empty() || missing_segment.is_some() {
        status = PendingExportStatus::Expired;
        retryable = false;
        error = Some(match missing_segment {
            Some(path) => format!("Segment figé manquant: {}", path.display()),
            None => "Manifest pending sans segment figé".to_owned(),
        });
    }

    let dedupe_key = manifest
        .dedupe_key
        .clone()
        .unwrap_or_else(|| fallback_manifest_dedupe_key(&manifest));

    Ok(PendingClipExport {
        id: manifest.id,
        reason: manifest.reason,
        title: manifest.title,
        detected_at,
        game_time: None,
        start_time,
        end_time,
        exportable_at: SystemTime::now(),
        pending_dir,
        segments_dir,
        manifest_path: manifest_path.to_path_buf(),
        frozen_segments: manifest.segments,
        pre_event_seconds: manifest.pre_event_seconds,
        post_event_seconds: manifest.post_event_seconds,
        status,
        dedupe_key,
        events: manifest.events,
        player_name: manifest.player_name,
        quality: auto_config.quality,
        quality_preset: auto_config.quality_preset,
        segment_seconds: auto_config.segment_seconds,
        error,
        retryable,
        next_freeze_attempt_at: None,
        last_freeze_attempt_at: None,
        last_freeze_log_at: None,
    })
}

fn clip_status_for_pending(clip: &PendingClipExport) -> ClipStatus {
    match clip.status {
        PendingExportStatus::WaitingPostEvent => ClipStatus::WaitingPostEvent,
        PendingExportStatus::FreezingSegments => ClipStatus::FreezingSegments,
        PendingExportStatus::ReadyToExport => ClipStatus::ReadyToExport,
        PendingExportStatus::Exporting => ClipStatus::Exporting,
        PendingExportStatus::Ready => ClipStatus::Ready,
        PendingExportStatus::Failed => ClipStatus::Failed,
        PendingExportStatus::Expired => ClipStatus::Expired,
    }
}

async fn recv_auto_command(
    command_rx: &mut Option<mpsc::UnboundedReceiver<AutoClipCommand>>,
) -> Option<AutoClipCommand> {
    match command_rx {
        Some(command_rx) => command_rx.recv().await,
        None => std::future::pending::<Option<AutoClipCommand>>().await,
    }
}

async fn handle_manual_clip_command(
    buffer: &ReplayBufferHandle,
    export_queue: &mut ExportQueue,
    player_name: Option<&str>,
    auto_config: &AutoClipConfig,
    _configured_post_event_delay: Duration,
) {
    if auto_config.export_mode == ClipExportMode::Deferred {
        let clip = pending_manual_export(player_name, auto_config);
        export_queue.add_pending_clip(clip.clone());
        info!(
            id = %clip.id,
            reason = ?clip.reason,
            export_mode = ?auto_config.export_mode,
            "deferred pending clip emitted"
        );
        send_ui_event(
            auto_config,
            AppEvent::ClipStatusChanged {
                payload: clip.status_payload(ClipStatus::WaitingPostEvent, None, None, None),
            },
        );
        return;
    }

    let mut status = ClipStatusPayload {
        id: format!("clip_{}", Uuid::new_v4()),
        status: ClipStatus::Recording,
        reason: ClipReason::Manual,
        title: "Capture en cours...".to_owned(),
        created_at: Utc::now().to_rfc3339(),
        file_path: None,
        thumbnail_path: None,
        duration_seconds: None,
        size_bytes: None,
        progress: Some(35),
        error: None,
        exportable_at: None,
        can_export: false,
        retryable: false,
    };
    let created_at = status.created_at.clone();
    send_ui_event(
        auto_config,
        AppEvent::ClipStatusChanged {
            payload: status.clone(),
        },
    );
    status.status = ClipStatus::Encoding;
    status.title = "Encodage du clip...".to_owned();
    status.progress = Some(72);
    send_ui_event(
        auto_config,
        AppEvent::ClipStatusChanged {
            payload: status.clone(),
        },
    );

    match buffer.save_replay(buffer.manual_clip_context()).await {
        Ok(SaveReplayOutcome::Saved(replay)) => {
            if let Some(path) = replay.final_video_path {
                let size_bytes = std::fs::metadata(&path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                status.status = ClipStatus::Ready;
                status.title = "Clip prêt".to_owned();
                status.created_at = created_at;
                status.file_path = Some(path.clone());
                status.duration_seconds = Some(auto_config.buffer_seconds);
                status.size_bytes = Some(size_bytes);
                status.progress = Some(100);
                send_ui_event(auto_config, AppEvent::ClipStatusChanged { payload: status });
                send_ui_event(
                    auto_config,
                    AppEvent::ClipSaved {
                        path,
                        reason: ClipReason::Manual,
                        duration_seconds: auto_config.buffer_seconds,
                        size_bytes,
                    },
                );
            }
        }
        Ok(SaveReplayOutcome::NotReadyYet(reason))
        | Ok(SaveReplayOutcome::SkippedTooOld(reason)) => {
            status.status = ClipStatus::Failed;
            status.title = "Erreur pendant la création du clip".to_owned();
            status.progress = None;
            status.error = Some(reason.clone());
            send_ui_event(auto_config, AppEvent::ClipStatusChanged { payload: status });
            send_ui_event(
                auto_config,
                AppEvent::ClipFailed {
                    message: format!("Clip manuel: {reason}"),
                },
            );
        }
        Err(error) => {
            let message = error.to_string();
            status.status = ClipStatus::Failed;
            status.title = "Erreur pendant la création du clip".to_owned();
            status.progress = None;
            status.error = Some(message.clone());
            send_ui_event(auto_config, AppEvent::ClipStatusChanged { payload: status });
            send_ui_event(
                auto_config,
                AppEvent::ClipFailed {
                    message: format!("Clip manuel: {message}"),
                },
            );
        }
    }
}

async fn bootstrap(client: &WarThunderClient, state: &mut AutoWatchState) {
    if let Ok(chat) = client.fetch_gamechat(0).await {
        state.last_chat_id = chat.next_last_id;
        remember_messages("gamechat", chat.messages, &mut state.seen_messages);
    }

    if let Ok(hud) = client.fetch_hudmsg(0, 0).await {
        state.last_evt_msg_id = hud.next_last_evt_id;
        state.last_dmg_msg_id = hud.next_last_dmg_id;
        remember_messages("hud:event", hud.events, &mut state.seen_messages);
        remember_messages("hud:damage", hud.damage, &mut state.seen_messages);
    }
}

async fn poll_personal_events(
    client: &WarThunderClient,
    state: &mut AutoWatchState,
    player_name: Option<&str>,
    triggers: &TriggerConfig,
) -> (Vec<DetectedEvent>, bool) {
    let mut events = Vec::new();
    let mut successful_polls = 0usize;

    match client.fetch_gamechat(state.last_chat_id).await {
        Ok(chat) => {
            successful_polls += 1;
            state.last_chat_id = chat.next_last_id;
            collect_personal_events(
                "gamechat",
                chat.messages,
                &mut state.seen_messages,
                &mut state.seen_events,
                player_name,
                triggers,
                &mut events,
            );
        }
        Err(error) => debug!(%error, "failed to poll gamechat for auto-clip"),
    }

    match client
        .fetch_hudmsg(state.last_evt_msg_id, state.last_dmg_msg_id)
        .await
    {
        Ok(hud) => {
            successful_polls += 1;
            state.last_evt_msg_id = hud.next_last_evt_id;
            state.last_dmg_msg_id = hud.next_last_dmg_id;
            collect_personal_events(
                "hud:event",
                hud.events,
                &mut state.seen_messages,
                &mut state.seen_events,
                player_name,
                triggers,
                &mut events,
            );
            collect_personal_events(
                "hud:damage",
                hud.damage,
                &mut state.seen_messages,
                &mut state.seen_events,
                player_name,
                triggers,
                &mut events,
            );
        }
        Err(error) => debug!(%error, "failed to poll hudmsg for auto-clip"),
    }

    (events, successful_polls > 0)
}

fn collect_personal_events(
    source: &str,
    messages: Vec<ChatMessage>,
    seen_messages: &mut RecentMessageCache,
    seen_events: &mut RecentEventCache,
    player_name: Option<&str>,
    triggers: &TriggerConfig,
    events: &mut Vec<DetectedEvent>,
) {
    for message in messages {
        let key = raw_message_dedupe_key(source, &message);
        debug!(
            source,
            message_id = ?message.id,
            raw_message = %message.text,
            raw_key = ?key,
            "auto-clip message received"
        );
        if let Some(key) = key {
            if seen_messages.contains(&key) {
                debug!(source, raw_key = %key, ignored_duplicate = true, "duplicate raw message ignored");
                continue;
            }
            seen_messages.insert(key);
        }

        let event = parse_gamechat_event(&message.text);
        let canonical_key = canonical_event_key(&event);
        let event_key = canonical_key
            .as_deref()
            .map(|canonical_key| event_dedupe_key(canonical_key, &message));
        debug!(
            source,
            raw_message = %message.text,
            canonical_key = ?canonical_key,
            event_key = ?event_key,
            ?event,
            "auto-clip parsed message"
        );
        if let Some(reason) = should_clip_event(&event, player_name, triggers) {
            let Some(event_key) = event_key else {
                debug!(source, ?event, "clip event has no canonical key");
                continue;
            };
            let now = Instant::now();
            if !seen_events.insert_new(event_key.clone(), now) {
                debug!(
                    source,
                    event_key = %event_key,
                    ignored_duplicate = true,
                    "duplicate canonical event ignored"
                );
                continue;
            }
            events.push(DetectedEvent {
                event,
                reason,
                canonical_key: event_key,
                detected_at: now,
                game_time: parse_wt_message_time(message.time.as_deref()),
                detected_wall_time: SystemTime::now(),
            });
        } else {
            debug!(source, message = %message.text, ?event, "ignoring disabled or non-matching auto-clip event");
        }
    }
}

fn raw_message_dedupe_key(source: &str, message: &ChatMessage) -> Option<String> {
    Some(match message.id {
        Some(id) => format!("{source}:{id}"),
        None => message.stable_key_with_prefix(source),
    })
}

fn parse_wt_message_time(value: Option<&str>) -> Option<Duration> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }

    let parts = value.split(':').collect::<Vec<_>>();
    let seconds = match parts.as_slice() {
        [seconds] => seconds.parse::<u64>().ok()?,
        [minutes, seconds] => minutes
            .parse::<u64>()
            .ok()?
            .saturating_mul(60)
            .saturating_add(seconds.parse::<u64>().ok()?),
        [hours, minutes, seconds] => hours
            .parse::<u64>()
            .ok()?
            .saturating_mul(60 * 60)
            .saturating_add(minutes.parse::<u64>().ok()?.saturating_mul(60))
            .saturating_add(seconds.parse::<u64>().ok()?),
        _ => return None,
    };

    Some(Duration::from_secs(seconds))
}

fn event_dedupe_key(canonical_key: &str, message: &ChatMessage) -> String {
    match message
        .time
        .as_deref()
        .map(str::trim)
        .filter(|time| !time.is_empty())
    {
        Some(time) => format!("{canonical_key}|time:{}", normalize_key_part(time)),
        None => canonical_key.to_owned(),
    }
}

fn remember_messages(
    source: &str,
    messages: Vec<ChatMessage>,
    seen_messages: &mut RecentMessageCache,
) {
    for message in messages {
        if let Some(key) = raw_message_dedupe_key(source, &message) {
            seen_messages.insert(key);
        }
    }
}

pub(crate) fn should_clip_event(
    event: &WarThunderEvent,
    player_name: Option<&str>,
    triggers: &TriggerConfig,
) -> Option<ClipReason> {
    let player_name = player_name.map(str::trim).filter(|name| !name.is_empty());

    if triggers.player_destroyed && is_player_destroyed_event(event, player_name) {
        return Some(ClipReason::PlayerDestroyed);
    }

    if triggers.base_destroyed && is_base_destroyed_event(event) {
        return Some(ClipReason::BaseDestroyed);
    }

    if triggers.target_destroyed && is_target_destroyed_event(event, player_name) {
        return Some(ClipReason::TargetDestroyed);
    }

    None
}

fn canonical_event_key(event: &WarThunderEvent) -> Option<String> {
    match event {
        WarThunderEvent::TargetDestroyed {
            attacker,
            action,
            vehicle,
            target,
            raw,
        } => Some(format!(
            "target_destroyed|{}|{}|{}|{}",
            normalize_key_part(attacker.as_deref().unwrap_or("")),
            normalize_key_part(vehicle.as_deref().unwrap_or("")),
            normalize_key_part(action),
            normalize_key_part(target.as_deref().unwrap_or(raw))
        )),
        WarThunderEvent::PlayerDestroyed { raw } => {
            Some(format!("player_destroyed|{}", normalize_key_part(raw)))
        }
        WarThunderEvent::CriticalHit { raw } => {
            Some(format!("critical_hit|{}", normalize_key_part(raw)))
        }
        WarThunderEvent::SevereDamage { raw } => {
            Some(format!("severe_damage|{}", normalize_key_part(raw)))
        }
        WarThunderEvent::BaseDestroyed { raw } => {
            Some(format!("base_destroyed|{}", normalize_key_part(raw)))
        }
        WarThunderEvent::Unknown(raw) => {
            let raw = normalize_key_part(raw);
            (!raw.is_empty()).then(|| format!("unknown|{raw}"))
        }
    }
}

fn normalize_key_part(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_clip_action(action: &str) -> bool {
    matches!(action, "destroyed" | "shot down")
}

fn is_target_destroyed_clip_event(event: &WarThunderEvent) -> bool {
    matches!(
        event,
        WarThunderEvent::TargetDestroyed { action, .. } if is_clip_action(action)
    )
}

fn is_target_destroyed_event(event: &WarThunderEvent, player_name: Option<&str>) -> bool {
    let Some(player_name) = player_name else {
        return false;
    };
    match event {
        WarThunderEvent::TargetDestroyed {
            attacker: Some(attacker),
            action,
            target,
            ..
        } => {
            is_clip_action(action)
                && same_player(attacker, player_name)
                && !target_contains_player(target.as_deref(), player_name)
                && !target_is_base(target.as_deref())
        }
        _ => false,
    }
}

fn is_base_destroyed_event(event: &WarThunderEvent) -> bool {
    match event {
        WarThunderEvent::BaseDestroyed { raw } => raw_mentions_base_destroyed(raw),
        WarThunderEvent::TargetDestroyed {
            action,
            target,
            raw,
            ..
        } => {
            action == "destroyed"
                && (target_is_base(target.as_deref()) || raw_mentions_base_destroyed(raw))
        }
        WarThunderEvent::Unknown(raw) => raw_mentions_base_destroyed(raw),
        _ => false,
    }
}

fn is_player_destroyed_event(event: &WarThunderEvent, player_name: Option<&str>) -> bool {
    match event {
        WarThunderEvent::PlayerDestroyed { raw } => raw_mentions_player_destroyed(raw),
        WarThunderEvent::TargetDestroyed { action, target, .. } => {
            is_clip_action(action)
                && player_name.is_some_and(|player_name| {
                    target_contains_player(target.as_deref(), player_name)
                })
        }
        WarThunderEvent::Unknown(raw) => raw_mentions_player_destroyed(raw),
        _ => false,
    }
}

fn same_player(value: &str, player_name: &str) -> bool {
    normalize_key_part(value) == normalize_key_part(player_name)
}

fn target_contains_player(target: Option<&str>, player_name: &str) -> bool {
    let Some(target) = target else {
        return false;
    };
    normalize_key_part(target).contains(&normalize_key_part(player_name))
}

fn target_is_base(target: Option<&str>) -> bool {
    let Some(target) = target else {
        return false;
    };
    let target = normalize_key_part(target);
    target == "a base" || target.contains("base")
}

fn raw_mentions_base_destroyed(raw: &str) -> bool {
    let raw = normalize_key_part(raw);
    raw.contains("base destroyed")
        || raw.contains("enemy base destroyed")
        || raw.contains("destroyed a base")
        || raw.contains("destroyed enemy base")
}

fn raw_mentions_player_destroyed(raw: &str) -> bool {
    let raw = normalize_key_part(raw);
    raw.contains("you have been destroyed")
        || raw.contains("vehicle destroyed")
        || raw.contains("player destroyed")
}

fn event_summary(event: &WarThunderEvent) -> String {
    match event {
        WarThunderEvent::TargetDestroyed {
            attacker,
            action,
            vehicle,
            target,
            raw,
        } => match (attacker, target, vehicle) {
            (Some(attacker), Some(target), Some(vehicle)) => {
                format!("{attacker} {action} {target} with {vehicle}")
            }
            (Some(attacker), Some(target), None) => format!("{attacker} {action} {target}"),
            _ => raw.clone(),
        },
        WarThunderEvent::PlayerDestroyed { raw }
        | WarThunderEvent::CriticalHit { raw }
        | WarThunderEvent::SevereDamage { raw }
        | WarThunderEvent::BaseDestroyed { raw } => raw.clone(),
        WarThunderEvent::Unknown(raw) => raw.clone(),
    }
}

fn event_vehicle_target(event: &WarThunderEvent) -> (Option<String>, Option<String>) {
    match event {
        WarThunderEvent::TargetDestroyed {
            vehicle, target, ..
        } => (vehicle.clone(), target.clone()),
        _ => (None, None),
    }
}

fn emit_clip_export_step(
    auto_config: &AutoClipConfig,
    clip: &PendingClipExport,
    total: usize,
    completed: usize,
    failed: usize,
    position: usize,
    current_step: ExportProgressStep,
    clip_progress: f32,
    label: &str,
) {
    let progress = if total == 0 {
        100
    } else {
        (((completed as f32 + clip_progress) / total as f32) * 100.0)
            .round()
            .clamp(0.0, 100.0) as u8
    };
    emit_export_progress(
        auto_config,
        ExportProgressPayload {
            active: true,
            total,
            completed,
            failed,
            current_clip_id: Some(clip.id.clone()),
            current_clip_title: Some(clip.title.clone()),
            current_step,
            progress,
            message: format!("{label} {} / {}...", position + 1, total),
        },
    );
}

fn preflight_export_disk_space(
    buffer: &ReplayBufferHandle,
    auto_config: &AutoClipConfig,
    total: usize,
) -> anyhow::Result<()> {
    let bytes_per_second =
        u64::from(auto_config.quality.video_bitrate_kbps).saturating_mul(1000) / 8;
    let per_clip = bytes_per_second
        .saturating_mul(auto_config.buffer_seconds.saturating_add(10))
        .saturating_mul(2);
    let required_bytes = per_clip.saturating_mul(total as u64);
    let output_dir = buffer
        .output_dir()
        .or(auto_config.output_dir.as_deref())
        .ok_or_else(|| anyhow::anyhow!("output directory unavailable"))?;
    let output = crate::doctor::check_free_space(output_dir, required_bytes)?;
    let temp = crate::doctor::check_free_space(buffer.temp_dir(), required_bytes)?;
    info!(
        output_dir = %output.path.display(),
        output_available_bytes = output.available_bytes,
        temp_dir = %temp.path.display(),
        temp_available_bytes = temp.available_bytes,
        required_bytes,
        total,
        "deferred export free space check"
    );
    if output.available_bytes < required_bytes || temp.available_bytes < required_bytes {
        anyhow::bail!(
            "output_available_bytes={} temp_available_bytes={} required_bytes={}",
            output.available_bytes,
            temp.available_bytes,
            required_bytes
        );
    }
    Ok(())
}

async fn generate_clip_thumbnail(path: &Path) -> Option<PathBuf> {
    let video_path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let thumbnail_path = video_path.with_extension("jpg");
        let output = std::process::Command::new("ffmpeg")
            .args(["-y", "-i"])
            .arg(&video_path)
            .args(["-vframes", "1", "-s", "640x360"])
            .arg(&thumbnail_path)
            .output()
            .ok()?;
        if output.status.success() && thumbnail_path.exists() {
            Some(thumbnail_path)
        } else {
            debug!(
                path = %video_path.display(),
                status = ?output.status.code(),
                stderr = %String::from_utf8_lossy(&output.stderr),
                "failed to generate clip thumbnail"
            );
            None
        }
    })
    .await
    .ok()
    .flatten()
}

fn verify_completed_export(
    replay: &SavedReplay,
    thumbnail_path: Option<PathBuf>,
) -> anyhow::Result<VerifiedExport> {
    let final_video_path = replay
        .final_video_path
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Export terminé sans vidéo finale"))?;
    let metadata_path = replay
        .metadata_path
        .clone()
        .unwrap_or_else(|| final_video_path.with_extension("json"));
    let metadata = std::fs::metadata(&final_video_path).map_err(|error| {
        anyhow::anyhow!(
            "Vidéo finale introuvable après export {}: {error}",
            final_video_path.display()
        )
    })?;
    if !metadata.is_file() || metadata.len() == 0 {
        anyhow::bail!(
            "Vidéo finale invalide après export {}",
            final_video_path.display()
        );
    }
    info!(
        path = %final_video_path.display(),
        size_bytes = metadata.len(),
        "[EXPORT] final clip verified"
    );

    if !metadata_path.is_file() {
        anyhow::bail!(
            "Metadata finale introuvable après export {}",
            metadata_path.display()
        );
    }
    info!(
        path = %metadata_path.display(),
        "[EXPORT] metadata verified"
    );

    let verified_thumbnail = thumbnail_path.filter(|path| {
        let valid = is_non_empty_file(path);
        if valid {
            info!(path = %path.display(), "[EXPORT] thumbnail verified");
        } else {
            debug!(
                path = %path.display(),
                "thumbnail path returned but file is missing or empty"
            );
        }
        valid
    });

    Ok(VerifiedExport {
        final_video_path,
        metadata_path,
        thumbnail_path: verified_thumbnail,
        size_bytes: metadata.len(),
    })
}

fn emit_export_progress(auto_config: &AutoClipConfig, payload: ExportProgressPayload) {
    send_ui_event(auto_config, AppEvent::ExportProgressChanged { payload });
}

fn send_ui_event(auto_config: &AutoClipConfig, event: AppEvent) {
    if let Some(sender) = &auto_config.ui_events {
        debug!(?event, "queueing AppEvent from auto backend");
        if let Err(error) = sender.send(event) {
            debug!(%error, "failed to queue AppEvent from auto backend");
        }
    } else {
        debug!(?event, "no ui_events channel configured for AppEvent");
    }
}

fn system_time_rfc3339(time: SystemTime) -> String {
    chrono::DateTime::<Utc>::from(time).to_rfc3339()
}

fn parse_system_time_rfc3339(value: &str) -> anyhow::Result<SystemTime> {
    let parsed = chrono::DateTime::parse_from_rfc3339(value)?;
    Ok(SystemTime::from(parsed.with_timezone(&Utc)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kill(attacker: &str) -> WarThunderEvent {
        WarThunderEvent::TargetDestroyed {
            attacker: Some(attacker.to_owned()),
            action: "destroyed".to_owned(),
            vehicle: Some("F/A-18C Early".to_owned()),
            target: Some("[ai] MiG-15bis".to_owned()),
            raw: format!("{attacker} (F/A-18C Early) destroyed [ai] MiG-15bis"),
        }
    }

    fn kill_with_target(target: &str) -> WarThunderEvent {
        WarThunderEvent::TargetDestroyed {
            attacker: Some("dawson16800".to_owned()),
            action: "destroyed".to_owned(),
            vehicle: Some("F/A-18C Early".to_owned()),
            target: Some(target.to_owned()),
            raw: format!("dawson16800 (F/A-18C Early) destroyed {target}"),
        }
    }

    fn base_destroyed() -> WarThunderEvent {
        WarThunderEvent::TargetDestroyed {
            attacker: Some("dawson16800".to_owned()),
            action: "destroyed".to_owned(),
            vehicle: Some("F/A-18C Early".to_owned()),
            target: Some("a base".to_owned()),
            raw: "dawson16800 (F/A-18C Early) destroyed a base".to_owned(),
        }
    }

    fn player_destroyed_by_enemy() -> WarThunderEvent {
        WarThunderEvent::TargetDestroyed {
            attacker: Some("Enemy".to_owned()),
            action: "shot down".to_owned(),
            vehicle: Some("MiG-29".to_owned()),
            target: Some("dawson16800 (F/A-18C Early)".to_owned()),
            raw: "Enemy (MiG-29) shot down dawson16800 (F/A-18C Early)".to_owned(),
        }
    }

    fn event_key(event: &WarThunderEvent) -> String {
        canonical_event_key(event).unwrap()
    }

    fn test_auto_config(post_event_seconds: u64, segment_seconds: u64) -> AutoClipConfig {
        AutoClipConfig {
            buffer_seconds: 25,
            segment_seconds,
            output_dir: None,
            source: CaptureSource::Window,
            keep_segments: false,
            quality_preset: QualityPreset::Medium,
            quality: VideoQuality::default(),
            cooldown: Duration::from_secs(3),
            post_event_delay: Duration::from_secs(post_event_seconds),
            multi_kill_window: Duration::from_secs(5),
            include_history: false,
            triggers: TriggerConfig::default(),
            ui_events: None,
            export_mode: ClipExportMode::Instant,
            pending_export_dir: std::env::temp_dir().join("wt-clipper-test-pending"),
            delete_ready_after_export: true,
            command_rx: None,
        }
    }

    fn pending_export_for_test(
        id: &str,
        dedupe_key: &str,
        status: PendingExportStatus,
        pending_dir: PathBuf,
    ) -> PendingClipExport {
        let segments_dir = pending_dir.join("segments");
        PendingClipExport {
            id: id.to_owned(),
            reason: ClipReason::TargetDestroyed,
            title: "Cible détruite".to_owned(),
            detected_at: SystemTime::UNIX_EPOCH + Duration::from_secs(10_000),
            game_time: Some(Duration::from_secs(120)),
            start_time: SystemTime::UNIX_EPOCH + Duration::from_secs(9_980),
            end_time: SystemTime::UNIX_EPOCH + Duration::from_secs(10_005),
            exportable_at: SystemTime::UNIX_EPOCH + Duration::from_secs(10_006),
            pending_dir: pending_dir.clone(),
            segments_dir,
            manifest_path: pending_dir.join("manifest.json"),
            frozen_segments: vec![PathBuf::from("segments/segment-000000.webm")],
            pre_event_seconds: 20,
            post_event_seconds: 5,
            status,
            dedupe_key: dedupe_key.to_owned(),
            events: vec![kill("dawson16800")],
            player_name: Some("dawson16800".to_owned()),
            quality: VideoQuality::default(),
            quality_preset: QualityPreset::Medium,
            segment_seconds: 2,
            error: None,
            retryable: false,
            next_freeze_attempt_at: None,
            last_freeze_attempt_at: None,
            last_freeze_log_at: None,
        }
    }

    fn write_test_segment(pending_dir: &Path) {
        let segments_dir = pending_dir.join("segments");
        std::fs::create_dir_all(&segments_dir).unwrap();
        std::fs::write(segments_dir.join("segment-000000.webm"), b"segment").unwrap();
    }

    #[test]
    fn cooldown_blocks_close_independent_saves() {
        let mut cooldown = Cooldown::new(Duration::from_secs(3));
        let now = Instant::now();

        assert!(cooldown.allows(now));
        cooldown.record_save(now);
        assert!(!cooldown.allows(now + Duration::from_secs(1)));
        assert!(cooldown.allows(now + Duration::from_secs(4)));
    }

    #[test]
    fn effective_delay_is_at_least_segment_plus_one() {
        assert_eq!(
            effective_post_event_delay(Duration::from_secs(5), 2),
            Duration::from_secs(8)
        );
        assert_eq!(
            effective_post_event_delay(Duration::from_secs(1), 2),
            Duration::from_secs(4)
        );
    }

    #[test]
    fn should_clip_event_returns_target_destroyed_for_personal_kill() {
        assert_eq!(
            should_clip_event(
                &kill("dawson16800"),
                Some("dawson16800"),
                &TriggerConfig::default()
            ),
            Some(ClipReason::TargetDestroyed)
        );
    }

    #[test]
    fn should_clip_event_returns_base_destroyed_for_destroyed_a_base() {
        assert_eq!(
            should_clip_event(
                &base_destroyed(),
                Some("dawson16800"),
                &TriggerConfig::default()
            ),
            Some(ClipReason::BaseDestroyed)
        );
    }

    #[test]
    fn should_clip_event_returns_player_destroyed_when_player_is_target() {
        let triggers = TriggerConfig {
            player_destroyed: true,
            ..TriggerConfig::default()
        };

        assert_eq!(
            should_clip_event(&player_destroyed_by_enemy(), Some("dawson16800"), &triggers),
            Some(ClipReason::PlayerDestroyed)
        );
    }

    #[test]
    fn auto_replay_buffer_keeps_extra_margin_for_multi_kills() {
        let config = test_auto_config(5, 5);

        assert_eq!(config.buffer_seconds, 25);
        assert_eq!(replay_buffer_seconds_for_auto(&config), 85);
    }

    #[test]
    fn clip_context_uses_configured_post_event_delay_not_effective_save_delay() {
        let event = kill_with_target("[ai] MiG-15bis");
        let now = Instant::now();
        let pending = schedule_pending_clip(
            event.clone(),
            event_key(&event),
            ClipReason::TargetDestroyed,
            "kill".to_owned(),
            now,
            None,
            SystemTime::UNIX_EPOCH + Duration::from_secs(100),
            effective_post_event_delay(Duration::from_secs(5), 5),
        );
        let config = test_auto_config(5, 5);

        let context = clip_context_for_pending(
            &pending,
            Some("dawson16800"),
            &config,
            config.post_event_delay,
        );

        assert_eq!(pending.save_at, now + Duration::from_secs(11));
        assert_eq!(context.post_event_seconds, 5);
        assert_eq!(context.segment_seconds, 5);
    }

    #[test]
    fn not_ready_pending_clip_is_requeued_without_recording_cooldown() {
        let mut cooldown = Cooldown::new(Duration::from_secs(3));
        let now = Instant::now();
        let event = kill_with_target("[ai] MiG-15bis");
        let pending = schedule_pending_clip(
            event.clone(),
            event_key(&event),
            ClipReason::TargetDestroyed,
            "kill".to_owned(),
            now,
            None,
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(5),
        );
        let mut pending_clips = Vec::new();

        requeue_pending_clip(&mut pending_clips, pending, now + Duration::from_secs(5));

        assert_eq!(pending_clips.len(), 1);
        assert_eq!(pending_clips[0].save_at, now + Duration::from_secs(6));
        assert!(cooldown.allows(now + Duration::from_secs(5)));
    }

    #[test]
    fn kill_schedules_pending_clip() {
        let now = Instant::now();
        let pending = schedule_pending_clip(
            kill_with_target("[ai] MiG-15bis"),
            event_key(&kill_with_target("[ai] MiG-15bis")),
            ClipReason::TargetDestroyed,
            "kill".to_owned(),
            now,
            None,
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(5),
        );

        assert_eq!(pending.reason, ClipReason::TargetDestroyed);
        assert_eq!(pending.first_event_time, now);
        assert_eq!(pending.last_event_time, now);
        assert_eq!(pending.last_event_game_time, None);
        assert_eq!(pending.save_at, now + Duration::from_secs(5));
        assert_eq!(pending.events.len(), 1);
        assert_eq!(pending.descriptions, vec!["kill"]);
    }

    #[test]
    fn pending_clip_is_not_ready_before_save_at() {
        let now = Instant::now();
        let pending = schedule_pending_clip(
            kill_with_target("[ai] MiG-15bis"),
            event_key(&kill_with_target("[ai] MiG-15bis")),
            ClipReason::TargetDestroyed,
            "kill".to_owned(),
            now,
            None,
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(5),
        );

        assert!(!pending.is_ready(now + Duration::from_secs(4)));
        assert!(ready_pending_indices(&[pending], now + Duration::from_secs(4)).is_empty());
    }

    #[test]
    fn pending_clip_is_ready_after_save_at() {
        let now = Instant::now();
        let pending = schedule_pending_clip(
            kill_with_target("[ai] MiG-15bis"),
            event_key(&kill_with_target("[ai] MiG-15bis")),
            ClipReason::TargetDestroyed,
            "kill".to_owned(),
            now,
            None,
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(5),
        );

        assert!(pending.is_ready(now + Duration::from_secs(5)));
        assert_eq!(
            ready_pending_indices(&[pending], now + Duration::from_secs(6)),
            vec![0]
        );
    }

    #[test]
    fn second_close_kill_is_added_to_pending_clip() {
        let now = Instant::now();
        let mut pending = schedule_pending_clip(
            kill_with_target("[ai] MiG-15bis"),
            event_key(&kill_with_target("[ai] MiG-15bis")),
            ClipReason::TargetDestroyed,
            "first".to_owned(),
            now,
            Some(Duration::from_secs(30)),
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(5),
        );

        let second = kill_with_target("IT-1");
        pending.add_event(
            second.clone(),
            event_key(&second),
            now + Duration::from_secs(3),
            Some(Duration::from_secs(34)),
            SystemTime::UNIX_EPOCH + Duration::from_secs(3),
            Duration::from_secs(5),
            "second".to_owned(),
        );

        assert_eq!(pending.events.len(), 2);
        assert_eq!(pending.reason, ClipReason::MultiKill);
        assert_eq!(pending.save_at, now + Duration::from_secs(8));
        assert_eq!(pending.kill_count(), 2);
    }

    #[test]
    fn cooldown_does_not_prevent_adding_to_pending_clip() {
        let mut cooldown = Cooldown::new(Duration::from_secs(3));
        let now = Instant::now();
        let mut pending = schedule_pending_clip(
            kill_with_target("[ai] MiG-15bis"),
            event_key(&kill_with_target("[ai] MiG-15bis")),
            ClipReason::TargetDestroyed,
            "first".to_owned(),
            now,
            None,
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(5),
        );
        cooldown.record_save(now);

        let second = kill_with_target("IT-1");
        pending.add_event(
            second.clone(),
            event_key(&second),
            now + Duration::from_secs(1),
            None,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1),
            Duration::from_secs(5),
            "second".to_owned(),
        );

        assert!(!cooldown.allows(now + Duration::from_secs(1)));
        assert_eq!(pending.events.len(), 2);
    }

    #[test]
    fn duplicate_kill_is_not_added_to_pending_clip() {
        let now = Instant::now();
        let event = kill_with_target("[ai] MiG-15bis");
        let mut pending = schedule_pending_clip(
            event.clone(),
            event_key(&event),
            ClipReason::TargetDestroyed,
            "first".to_owned(),
            now,
            None,
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(5),
        );

        let added = pending.add_event(
            event.clone(),
            event_key(&event),
            now + Duration::from_secs(1),
            None,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1),
            Duration::from_secs(5),
            "duplicate".to_owned(),
        );

        assert!(!added);
        assert_eq!(pending.events.len(), 1);
        assert_eq!(pending.reason, ClipReason::TargetDestroyed);
    }

    #[test]
    fn multi_kill_requires_real_time_window() {
        let now = Instant::now();
        let first = kill_with_target("[ai] MiG-15bis");
        let pending = schedule_pending_clip(
            first.clone(),
            event_key(&first),
            ClipReason::TargetDestroyed,
            "first".to_owned(),
            now,
            None,
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(10),
        );
        let later = now + Duration::from_secs(6);

        assert!(later < pending.save_at);
        assert_eq!(
            pending_index_for_multi_kill(
                &[pending],
                ClipReason::TargetDestroyed,
                later,
                None,
                Duration::from_secs(5)
            ),
            None
        );
    }

    #[test]
    fn distinct_kill_inside_window_uses_existing_pending_clip() {
        let now = Instant::now();
        let first = kill_with_target("[ai] MiG-15bis");
        let pending = schedule_pending_clip(
            first.clone(),
            event_key(&first),
            ClipReason::TargetDestroyed,
            "first".to_owned(),
            now,
            None,
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(10),
        );

        assert_eq!(
            pending_index_for_multi_kill(
                &[pending],
                ClipReason::TargetDestroyed,
                now + Duration::from_secs(4),
                None,
                Duration::from_secs(5)
            ),
            Some(0)
        );
    }

    #[test]
    fn base_destroyed_inside_window_does_not_use_target_pending_clip() {
        let now = Instant::now();
        let first = kill_with_target("[ai] MiG-15bis");
        let pending = schedule_pending_clip(
            first.clone(),
            event_key(&first),
            ClipReason::TargetDestroyed,
            "first".to_owned(),
            now,
            None,
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(10),
        );

        assert_eq!(
            pending_index_for_multi_kill(
                &[pending],
                ClipReason::BaseDestroyed,
                now + Duration::from_secs(2),
                None,
                Duration::from_secs(5)
            ),
            None
        );
    }

    #[test]
    fn player_destroyed_inside_window_does_not_use_target_pending_clip() {
        let now = Instant::now();
        let first = kill_with_target("[ai] MiG-15bis");
        let pending = schedule_pending_clip(
            first.clone(),
            event_key(&first),
            ClipReason::TargetDestroyed,
            "first".to_owned(),
            now,
            None,
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(10),
        );

        assert_eq!(
            pending_index_for_multi_kill(
                &[pending],
                ClipReason::PlayerDestroyed,
                now + Duration::from_secs(2),
                None,
                Duration::from_secs(5)
            ),
            None
        );
    }

    #[test]
    fn parse_wt_message_time_accepts_common_formats() {
        assert_eq!(
            parse_wt_message_time(Some("0:30")),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse_wt_message_time(Some("1:03")),
            Some(Duration::from_secs(63))
        );
        assert_eq!(
            parse_wt_message_time(Some("1:02:03")),
            Some(Duration::from_secs(3723))
        );
        assert_eq!(
            parse_wt_message_time(Some("83")),
            Some(Duration::from_secs(83))
        );
        assert_eq!(parse_wt_message_time(Some("")), None);
    }

    #[test]
    fn multi_kill_window_uses_war_thunder_time_when_available() {
        let now = Instant::now();
        let first = kill_with_target("IT-1");
        let pending = schedule_pending_clip(
            first.clone(),
            event_key(&first),
            ClipReason::TargetDestroyed,
            "first".to_owned(),
            now,
            Some(Duration::from_secs(44)),
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(10),
        );

        assert_eq!(
            pending_index_for_multi_kill(
                &[pending],
                ClipReason::TargetDestroyed,
                now + Duration::from_millis(1),
                Some(Duration::from_secs(63)),
                Duration::from_secs(8)
            ),
            None
        );
    }

    #[test]
    fn rapid_backend_read_splits_multi_kill_by_war_thunder_time() {
        let now = Instant::now();
        let delay = Duration::from_secs(11);
        let window = Duration::from_secs(8);
        let kills = [
            ("IT-1", 30),
            ("T-62", 34),
            ("T-55A", 40),
            ("T-10M", 44),
            ("T-62", 63),
        ];
        let mut pending_clips = Vec::new();

        for (target, game_seconds) in kills {
            let event = kill_with_target(target);
            let detected_at = now + Duration::from_millis(game_seconds);
            if let Some(index) = pending_index_for_multi_kill(
                &pending_clips,
                ClipReason::TargetDestroyed,
                detected_at,
                Some(Duration::from_secs(game_seconds)),
                window,
            ) {
                pending_clips[index].add_event(
                    event.clone(),
                    format!("{}|time:{game_seconds}", event_key(&event)),
                    detected_at,
                    Some(Duration::from_secs(game_seconds)),
                    SystemTime::UNIX_EPOCH + Duration::from_secs(game_seconds),
                    delay,
                    target.to_owned(),
                );
            } else {
                pending_clips.push(schedule_pending_clip(
                    event.clone(),
                    format!("{}|time:{game_seconds}", event_key(&event)),
                    ClipReason::TargetDestroyed,
                    target.to_owned(),
                    detected_at,
                    Some(Duration::from_secs(game_seconds)),
                    SystemTime::UNIX_EPOCH + Duration::from_secs(game_seconds),
                    delay,
                ));
            }
        }

        assert_eq!(pending_clips.len(), 2);
        assert_eq!(pending_clips[0].reason, ClipReason::MultiKill);
        assert_eq!(pending_clips[0].kill_count(), 4);
        assert_eq!(
            pending_clips[0].last_event_game_time,
            Some(Duration::from_secs(44))
        );
        assert_eq!(pending_clips[1].reason, ClipReason::TargetDestroyed);
        assert_eq!(pending_clips[1].kill_count(), 1);
        assert_eq!(
            pending_clips[1].last_event_game_time,
            Some(Duration::from_secs(63))
        );
    }

    #[test]
    fn kill_after_saved_clip_can_create_new_pending_clip() {
        let mut cooldown = Cooldown::new(Duration::from_secs(3));
        let now = Instant::now();
        cooldown.record_save(now);
        let later = now + Duration::from_secs(4);

        let pending = cooldown.allows(later).then(|| {
            schedule_pending_clip(
                kill_with_target("IT-1"),
                event_key(&kill_with_target("IT-1")),
                ClipReason::TargetDestroyed,
                "next".to_owned(),
                later,
                None,
                SystemTime::UNIX_EPOCH + Duration::from_secs(4),
                Duration::from_secs(5),
            )
        });

        assert!(pending.is_some());
        assert_eq!(pending.unwrap().save_at, later + Duration::from_secs(5));
    }

    #[test]
    fn pending_clip_can_be_dropped_on_shutdown_without_saving() {
        let mut pending = vec![schedule_pending_clip(
            kill_with_target("[ai] MiG-15bis"),
            event_key(&kill_with_target("[ai] MiG-15bis")),
            ClipReason::TargetDestroyed,
            "kill".to_owned(),
            Instant::now(),
            None,
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(5),
        )];

        pending.clear();

        assert!(pending.is_empty());
    }

    #[test]
    fn auto_collects_personal_target_destroyed() {
        let mut seen = RecentMessageCache::new(100);
        let mut seen_events = RecentEventCache::new(Duration::from_secs(120));
        let mut events = Vec::new();
        collect_personal_events(
            "hud:damage",
            vec![ChatMessage {
                id: Some(1),
                time: None,
                sender: None,
                text: "dawson16800 (F/A-18C Early) destroyed [ai] MiG-15bis".to_owned(),
            }],
            &mut seen,
            &mut seen_events,
            Some("dawson16800"),
            &TriggerConfig::default(),
            &mut events,
        );

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reason, ClipReason::TargetDestroyed);
        assert_eq!(events[0].game_time, None);
    }

    #[test]
    fn canonical_dedupe_ignores_same_kill_from_gamechat_then_hud_damage() {
        let mut seen_messages = RecentMessageCache::new(100);
        let mut seen_events = RecentEventCache::new(Duration::from_secs(120));
        let mut events = Vec::new();
        let text = "dawson16800 (F/A-18C Early) destroyed [ai] MiG-15bis".to_owned();

        collect_personal_events(
            "gamechat",
            vec![ChatMessage {
                id: Some(1),
                time: None,
                sender: None,
                text: text.clone(),
            }],
            &mut seen_messages,
            &mut seen_events,
            Some("dawson16800"),
            &TriggerConfig::default(),
            &mut events,
        );
        collect_personal_events(
            "hud:damage",
            vec![ChatMessage {
                id: Some(99),
                time: None,
                sender: None,
                text,
            }],
            &mut seen_messages,
            &mut seen_events,
            Some("dawson16800"),
            &TriggerConfig::default(),
            &mut events,
        );

        assert_eq!(events.len(), 1);
    }

    #[test]
    fn repeated_identical_kills_with_different_message_times_are_distinct() {
        let mut seen_messages = RecentMessageCache::new(100);
        let mut seen_events = RecentEventCache::new(Duration::from_secs(120));
        let mut events = Vec::new();
        let text = "dawson16800 (F/A-18C Early) destroyed [ai] MiG-15bis".to_owned();

        collect_personal_events(
            "gamechat",
            vec![
                ChatMessage {
                    id: Some(1),
                    time: Some("0:47".to_owned()),
                    sender: None,
                    text: text.clone(),
                },
                ChatMessage {
                    id: Some(2),
                    time: Some("2:50".to_owned()),
                    sender: None,
                    text,
                },
            ],
            &mut seen_messages,
            &mut seen_events,
            Some("dawson16800"),
            &TriggerConfig::default(),
            &mut events,
        );

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].game_time, Some(Duration::from_secs(47)));
        assert_eq!(events[1].game_time, Some(Duration::from_secs(170)));
    }

    #[test]
    fn same_timed_kill_from_gamechat_and_hud_is_deduped() {
        let mut seen_messages = RecentMessageCache::new(100);
        let mut seen_events = RecentEventCache::new(Duration::from_secs(120));
        let mut events = Vec::new();
        let text = "dawson16800 (F/A-18C Early) destroyed [ai] MiG-15bis".to_owned();

        collect_personal_events(
            "gamechat",
            vec![ChatMessage {
                id: Some(1),
                time: Some("0:47".to_owned()),
                sender: None,
                text: text.clone(),
            }],
            &mut seen_messages,
            &mut seen_events,
            Some("dawson16800"),
            &TriggerConfig::default(),
            &mut events,
        );
        collect_personal_events(
            "hud:damage",
            vec![ChatMessage {
                id: Some(99),
                time: Some("0:47".to_owned()),
                sender: None,
                text,
            }],
            &mut seen_messages,
            &mut seen_events,
            Some("dawson16800"),
            &TriggerConfig::default(),
            &mut events,
        );

        assert_eq!(events.len(), 1);
    }

    #[test]
    fn repeated_raw_event_is_ignored() {
        let mut seen_messages = RecentMessageCache::new(100);
        let mut seen_events = RecentEventCache::new(Duration::from_secs(120));
        let mut events = Vec::new();
        let text = "dawson16800 (F/A-18C Early) destroyed [ai] MiG-15bis".to_owned();

        collect_personal_events(
            "hud:damage",
            vec![
                ChatMessage {
                    id: Some(1),
                    time: None,
                    sender: None,
                    text: text.clone(),
                },
                ChatMessage {
                    id: Some(2),
                    time: None,
                    sender: None,
                    text,
                },
            ],
            &mut seen_messages,
            &mut seen_events,
            Some("dawson16800"),
            &TriggerConfig::default(),
            &mut events,
        );

        assert_eq!(events.len(), 1);
    }

    #[test]
    fn raw_message_dedupe_uses_id_or_stable_content_key() {
        let with_id = ChatMessage {
            id: Some(42),
            time: None,
            sender: None,
            text: "dawson16800 (F/A-18C Early) destroyed [ai] MiG-15bis".to_owned(),
        };
        let without_id = ChatMessage {
            id: None,
            time: None,
            sender: None,
            text: "dawson16800 (F/A-18C Early) destroyed [ai] MiG-15bis".to_owned(),
        };

        assert_eq!(
            raw_message_dedupe_key("hud:damage", &with_id),
            Some("hud:damage:42".to_owned())
        );
        assert_eq!(
            raw_message_dedupe_key("hud:damage", &without_id),
            Some("hud:damage:dawson16800 (F/A-18C Early) destroyed [ai] MiG-15bis".to_owned())
        );
    }

    #[test]
    fn production_event_dedupe_expires_quickly_for_repeated_identical_kills() {
        let mut state = AutoWatchState::new();
        let now = Instant::now();

        assert!(state
            .seen_events
            .insert_new("same-canonical-kill".to_owned(), now));
        assert!(state.seen_events.insert_new(
            "same-canonical-kill".to_owned(),
            now + EVENT_DEDUPE_TTL + Duration::from_millis(1)
        ));
    }

    #[test]
    fn different_targets_are_distinct_events() {
        let mut seen_messages = RecentMessageCache::new(100);
        let mut seen_events = RecentEventCache::new(Duration::from_secs(120));
        let mut events = Vec::new();

        collect_personal_events(
            "hud:damage",
            vec![
                ChatMessage {
                    id: Some(1),
                    time: None,
                    sender: None,
                    text: "dawson16800 (F/A-18C Early) destroyed [ai] MiG-15bis".to_owned(),
                },
                ChatMessage {
                    id: Some(2),
                    time: None,
                    sender: None,
                    text: "dawson16800 (F/A-18C Early) destroyed IT-1".to_owned(),
                },
            ],
            &mut seen_messages,
            &mut seen_events,
            Some("dawson16800"),
            &TriggerConfig::default(),
            &mut events,
        );

        assert_eq!(events.len(), 2);
    }

    #[test]
    fn different_targets_with_same_message_time_are_distinct_events() {
        let mut seen_messages = RecentMessageCache::new(100);
        let mut seen_events = RecentEventCache::new(Duration::from_secs(120));
        let mut events = Vec::new();

        collect_personal_events(
            "hud:damage",
            vec![
                ChatMessage {
                    id: Some(1),
                    time: Some("1:12".to_owned()),
                    sender: None,
                    text: "dawson16800 (F/A-18C Early) destroyed [ai] MiG-15bis".to_owned(),
                },
                ChatMessage {
                    id: Some(2),
                    time: Some("1:12".to_owned()),
                    sender: None,
                    text: "dawson16800 (F/A-18C Early) destroyed IT-1".to_owned(),
                },
            ],
            &mut seen_messages,
            &mut seen_events,
            Some("dawson16800"),
            &TriggerConfig::default(),
            &mut events,
        );

        assert_eq!(events.len(), 2);
    }

    #[test]
    fn auto_collects_base_destroyed_as_base_reason() {
        let mut seen = RecentMessageCache::new(100);
        let mut seen_events = RecentEventCache::new(Duration::from_secs(120));
        let mut events = Vec::new();
        collect_personal_events(
            "hud:damage",
            vec![ChatMessage {
                id: Some(1),
                time: Some("2:43".to_owned()),
                sender: None,
                text: "dawson16800 (F/A-18C Early) destroyed a base".to_owned(),
            }],
            &mut seen,
            &mut seen_events,
            Some("dawson16800"),
            &TriggerConfig::default(),
            &mut events,
        );

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reason, ClipReason::BaseDestroyed);
    }

    #[test]
    fn auto_collects_player_destroyed_when_trigger_enabled() {
        let mut seen = RecentMessageCache::new(100);
        let mut seen_events = RecentEventCache::new(Duration::from_secs(120));
        let mut events = Vec::new();
        let triggers = TriggerConfig {
            player_destroyed: true,
            ..TriggerConfig::default()
        };
        collect_personal_events(
            "hud:damage",
            vec![ChatMessage {
                id: Some(1),
                time: Some("2:43".to_owned()),
                sender: None,
                text: "Enemy (MiG-29) shot down dawson16800 (F/A-18C Early)".to_owned(),
            }],
            &mut seen,
            &mut seen_events,
            Some("dawson16800"),
            &triggers,
            &mut events,
        );

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].reason, ClipReason::PlayerDestroyed);
    }

    #[test]
    fn auto_ignores_non_personal_event() {
        let mut seen = RecentMessageCache::new(100);
        let mut seen_events = RecentEventCache::new(Duration::from_secs(120));
        let mut events = Vec::new();
        collect_personal_events(
            "hud:damage",
            vec![ChatMessage {
                id: Some(1),
                time: None,
                sender: None,
                text: "other (MiG-15bis) destroyed [ai] F-86".to_owned(),
            }],
            &mut seen,
            &mut seen_events,
            Some("dawson16800"),
            &TriggerConfig::default(),
            &mut events,
        );

        assert!(events.is_empty());
    }

    #[test]
    fn bootstrap_seen_messages_prevent_history_reprocessing() {
        let mut state = AutoWatchState::new();
        remember_messages(
            "hud:damage",
            vec![ChatMessage {
                id: Some(7),
                time: None,
                sender: None,
                text: "history".to_owned(),
            }],
            &mut state.seen_messages,
        );

        assert!(state.seen_messages.contains("hud:damage:7"));
    }

    #[test]
    fn target_destroyed_reason_slug() {
        assert_eq!(
            should_clip_event(
                &kill("dawson16800"),
                Some("dawson16800"),
                &TriggerConfig::default()
            )
            .unwrap()
            .slug(),
            "target-destroyed"
        );
    }

    #[test]
    fn deferred_target_destroyed_adds_pending_export_without_saving() {
        let config = test_auto_config(5, 2);
        let event_time = SystemTime::now() + Duration::from_secs(10);
        let pending = PendingClip::new(
            kill("dawson16800"),
            "kill-1".to_owned(),
            ClipReason::TargetDestroyed,
            Instant::now(),
            Some(Duration::from_secs(120)),
            event_time,
            Duration::from_secs(1),
            "Cible détruite".to_owned(),
        );

        let clip = pending_export_from_pending(
            &pending,
            Some("dawson16800"),
            &config,
            Duration::from_secs(5),
        );

        assert_eq!(clip.reason, ClipReason::TargetDestroyed);
        assert_eq!(clip.status, PendingExportStatus::WaitingPostEvent);
        assert_eq!(clip.pre_event_seconds, 20);
        assert_eq!(clip.post_event_seconds, 5);
        assert_eq!(clip.start_time, event_time - Duration::from_secs(20));
        assert_eq!(clip.end_time, event_time + Duration::from_secs(5));
        assert_eq!(
            clip.exportable_at,
            event_time
                + Duration::from_secs(5)
                + pending_export_stable_margin(config.segment_seconds)
        );
        assert!(!clip.is_exportable_at(event_time + Duration::from_secs(5)));
        assert!(clip.is_exportable_at(clip.exportable_at));
    }

    #[test]
    fn manual_clip_in_deferred_mode_adds_pending_export() {
        let mut config = test_auto_config(5, 2);
        config.export_mode = ClipExportMode::Deferred;

        let clip = pending_manual_export(Some("dawson16800"), &config);

        assert_eq!(clip.reason, ClipReason::Manual);
        assert_eq!(clip.status, PendingExportStatus::WaitingPostEvent);
        assert_eq!(clip.pre_event_seconds, config.buffer_seconds);
        assert_eq!(clip.post_event_seconds, 0);
        assert_eq!(clip.title, "Clip manuel");
        assert_eq!(
            clip.exportable_at,
            clip.detected_at + pending_export_stable_margin(config.segment_seconds)
        );
        assert!(!clip.dto().is_exportable);
    }

    #[test]
    fn update_config_applies_export_mode_live() {
        let mut auto_config = test_auto_config(5, 2);
        auto_config.export_mode = ClipExportMode::Instant;
        let mut app_config = AppConfig::default();
        app_config.clip.export_mode = ClipExportMode::Deferred;
        app_config.clip.post_event_seconds = 9;
        app_config.clip.multi_kill_window_seconds = 12;
        app_config.triggers.target_destroyed = false;

        let result = apply_runtime_config(&mut auto_config, &app_config);

        assert!(result.applied_live);
        assert_eq!(result.active_export_mode, ClipExportMode::Deferred);
        assert_eq!(auto_config.export_mode, ClipExportMode::Deferred);
        assert_eq!(auto_config.post_event_delay, Duration::from_secs(9));
        assert_eq!(auto_config.multi_kill_window, Duration::from_secs(12));
        assert!(!auto_config.triggers.target_destroyed);
    }

    #[test]
    fn update_config_marks_capture_changes_restart_required() {
        let mut auto_config = test_auto_config(5, 2);
        let mut app_config = AppConfig::default();
        app_config.clip.export_mode = ClipExportMode::Deferred;
        app_config.clip.seconds = auto_config.buffer_seconds + 5;

        let result = apply_runtime_config(&mut auto_config, &app_config);

        assert!(result.restart_required);
        assert_eq!(auto_config.export_mode, ClipExportMode::Deferred);
        assert_ne!(auto_config.buffer_seconds, app_config.clip.seconds);
    }

    #[test]
    fn get_pending_export_clips_returns_detected_kill() {
        let config = test_auto_config(5, 2);
        let event_time = SystemTime::now() + Duration::from_secs(10);
        let pending = PendingClip::new(
            kill("dawson16800"),
            "kill-1".to_owned(),
            ClipReason::TargetDestroyed,
            Instant::now(),
            Some(Duration::from_secs(120)),
            event_time,
            Duration::from_secs(1),
            "Cible détruite".to_owned(),
        );
        let mut queue = ExportQueue::default();

        queue.add_pending_clip(pending_export_from_pending(
            &pending,
            Some("dawson16800"),
            &config,
            Duration::from_secs(5),
        ));

        let dtos = queue.pending_dtos(&config);
        assert_eq!(dtos.len(), 1);
        assert_eq!(dtos[0].status, PendingExportStatus::WaitingPostEvent);
        assert_eq!(dtos[0].reason, ClipReason::TargetDestroyed);
        assert!(!dtos[0].is_exportable);
    }

    #[test]
    fn three_kills_create_three_exportable_pending_clips() {
        let mut config = test_auto_config(5, 2);
        let root = std::env::temp_dir().join(format!(
            "wt-clipper-three-kills-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        config.output_dir = Some(root.join("output"));
        let mut queue = ExportQueue::default();

        for index in 0..3 {
            let pending_dir = root.join(format!("clip-{index}"));
            write_test_segment(&pending_dir);
            let mut clip = pending_export_for_test(
                &format!("clip-{index}"),
                &format!("target-destroyed|kill-{index}"),
                PendingExportStatus::ReadyToExport,
                pending_dir,
            );
            clip.retryable = false;
            queue.add_pending_clip(clip);
        }

        let dtos = queue.pending_dtos(&config);

        assert_eq!(dtos.len(), 3);
        assert_eq!(dtos.iter().filter(|clip| clip.can_export).count(), 3);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn same_kill_from_gamechat_and_hudmsg_creates_one_pending_clip() {
        let mut seen_messages = RecentMessageCache::new(100);
        let mut seen_events = RecentEventCache::new(Duration::from_secs(120));
        let mut events = Vec::new();
        let text = "dawson16800 (F/A-18C Early) destroyed [ai] MiG-15bis".to_owned();

        collect_personal_events(
            "gamechat",
            vec![ChatMessage {
                id: Some(1),
                time: Some("0:47".to_owned()),
                sender: None,
                text: text.clone(),
            }],
            &mut seen_messages,
            &mut seen_events,
            Some("dawson16800"),
            &TriggerConfig::default(),
            &mut events,
        );
        collect_personal_events(
            "hud:damage",
            vec![ChatMessage {
                id: Some(99),
                time: Some("0:47".to_owned()),
                sender: None,
                text,
            }],
            &mut seen_messages,
            &mut seen_events,
            Some("dawson16800"),
            &TriggerConfig::default(),
            &mut events,
        );

        assert_eq!(events.len(), 1);
    }

    #[test]
    fn add_pending_clip_is_idempotent() {
        let root = std::env::temp_dir().join(format!(
            "wt-clipper-idempotent-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let pending_dir = root.join("clip-idempotent");
        let clip = pending_export_for_test(
            "clip-idempotent",
            "target-destroyed|same",
            PendingExportStatus::WaitingPostEvent,
            pending_dir,
        );
        let mut queue = ExportQueue::default();

        queue.add_pending_clip(clip.clone());
        queue.add_pending_clip(clip);

        assert_eq!(queue.pending.len(), 1);
    }

    #[test]
    fn add_pending_clip_does_not_downgrade_ready_to_export() {
        let root = std::env::temp_dir().join(format!(
            "wt-clipper-no-downgrade-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let pending_dir = root.join("clip-ready");
        let mut ready = pending_export_for_test(
            "clip-ready",
            "target-destroyed|ready",
            PendingExportStatus::ReadyToExport,
            pending_dir.clone(),
        );
        ready.retryable = false;
        let waiting = pending_export_for_test(
            "clip-ready",
            "target-destroyed|ready-updated",
            PendingExportStatus::WaitingPostEvent,
            pending_dir,
        );
        let mut queue = ExportQueue::default();

        queue.add_pending_clip(ready);
        queue.add_pending_clip(waiting);

        assert_eq!(queue.pending.len(), 1);
        assert_eq!(queue.pending[0].status, PendingExportStatus::ReadyToExport);
    }

    #[test]
    fn not_ready_yet_is_not_failure() {
        let config = test_auto_config(5, 2);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        let pending = PendingClip::new(
            kill("dawson16800"),
            "kill-not-ready".to_owned(),
            ClipReason::TargetDestroyed,
            Instant::now(),
            Some(Duration::from_secs(120)),
            now,
            Duration::from_secs(1),
            "Cible détruite".to_owned(),
        );
        let mut clip = pending_export_from_pending(
            &pending,
            Some("dawson16800"),
            &config,
            Duration::from_secs(5),
        );
        clip.exportable_at = now - Duration::from_secs(1);

        mark_freeze_not_ready(
            &mut clip,
            "target_end=100 latest_stable_end=95".to_owned(),
            now,
        );

        assert_eq!(clip.status, PendingExportStatus::FreezingSegments);
        assert_ne!(clip.status, PendingExportStatus::Failed);
        assert_ne!(clip.status, PendingExportStatus::Expired);
        assert!(!clip.retryable);
        assert!(clip
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("Waiting for stable segment"));
        assert!(!clip.should_attempt_freeze(now + Duration::from_secs(1)));
        assert!(clip.should_attempt_freeze(now + FREEZE_RETRY_DELAY));
    }

    #[test]
    fn load_manifests_on_startup() {
        let mut config = test_auto_config(5, 2);
        let root = std::env::temp_dir().join(format!(
            "wt-clipper-load-manifest-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        config.pending_export_dir = root.clone();
        config.output_dir = Some(root.join("output"));
        let clip_dir = root.join("clip_manifest");
        let segments_dir = clip_dir.join("segments");
        std::fs::create_dir_all(&segments_dir).unwrap();
        std::fs::write(segments_dir.join("segment-000000.webm"), b"segment").unwrap();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(20_000);
        let manifest = PendingClipManifest {
            id: "clip_manifest".to_owned(),
            reason: ClipReason::TargetDestroyed,
            title: "Cible détruite".to_owned(),
            detected_at: system_time_rfc3339(now),
            event_time: system_time_rfc3339(now),
            start_time: system_time_rfc3339(now - Duration::from_secs(20)),
            end_time: system_time_rfc3339(now + Duration::from_secs(5)),
            pre_event_seconds: 20,
            post_event_seconds: 5,
            segments: vec![PathBuf::from("segments/segment-000000.webm")],
            status: PendingExportStatus::ReadyToExport,
            dedupe_key: Some("target-destroyed|clip_manifest".to_owned()),
            events: vec![kill("dawson16800")],
            player_name: Some("dawson16800".to_owned()),
            final_video_path: None,
            metadata_path: None,
            thumbnail_path: None,
        };
        std::fs::write(
            clip_dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let mut queue = ExportQueue::default();
        queue.load_from_pending_dir(&config).unwrap();

        assert_eq!(queue.pending.len(), 1);
        assert_eq!(queue.pending[0].status, PendingExportStatus::ReadyToExport);
        assert!(queue.pending[0].can_export());
        assert_eq!(queue.pending_dtos(&config)[0].can_export, true);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn load_from_pending_dir_skips_stale_pending_when_final_clip_exists() {
        let mut config = test_auto_config(5, 2);
        let root = std::env::temp_dir().join(format!(
            "wt-clipper-stale-manifest-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let output = root.join("output");
        config.pending_export_dir = root.join("pending");
        config.output_dir = Some(output.clone());
        std::fs::create_dir_all(&output).unwrap();
        let clip_dir = config.pending_export_dir.join("clip_stale");
        let segments_dir = clip_dir.join("segments");
        std::fs::create_dir_all(&segments_dir).unwrap();
        std::fs::write(segments_dir.join("segment-000000.webm"), b"segment").unwrap();
        let final_video = output.join("kill-2026-06-04.webm");
        let metadata = output.join("kill-2026-06-04.json");
        std::fs::write(&final_video, b"video").unwrap();
        std::fs::write(
            &metadata,
            serde_json::json!({
                "reason": "target-destroyed",
                "duration_seconds": 25,
                "post_event_seconds": 5,
                "segment_seconds": 2,
                "pending_clip_id": "clip_stale",
                "pending_dedupe_key": "target-destroyed|clip_stale",
                "timestamp": "2026-06-04T12:00:00Z"
            })
            .to_string(),
        )
        .unwrap();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(20_000);
        let manifest = PendingClipManifest {
            id: "clip_stale".to_owned(),
            reason: ClipReason::TargetDestroyed,
            title: "Cible détruite".to_owned(),
            dedupe_key: Some("target-destroyed|clip_stale".to_owned()),
            detected_at: system_time_rfc3339(now),
            event_time: system_time_rfc3339(now),
            start_time: system_time_rfc3339(now - Duration::from_secs(20)),
            end_time: system_time_rfc3339(now + Duration::from_secs(5)),
            pre_event_seconds: 20,
            post_event_seconds: 5,
            segments: vec![PathBuf::from("segments/segment-000000.webm")],
            status: PendingExportStatus::ReadyToExport,
            events: vec![kill("dawson16800")],
            player_name: Some("dawson16800".to_owned()),
            final_video_path: None,
            metadata_path: None,
            thumbnail_path: None,
        };
        std::fs::write(
            clip_dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let mut queue = ExportQueue::default();
        queue.load_from_pending_dir(&config).unwrap();

        assert!(queue.pending.is_empty());
        assert!(!clip_dir.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn load_from_pending_dir_skips_invalid_pending_with_missing_segments() {
        let mut config = test_auto_config(5, 2);
        let root = std::env::temp_dir().join(format!(
            "wt-clipper-invalid-manifest-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        config.pending_export_dir = root.clone();
        let clip_dir = root.join("clip_invalid");
        std::fs::create_dir_all(&clip_dir).unwrap();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(20_000);
        let manifest = PendingClipManifest {
            id: "clip_invalid".to_owned(),
            reason: ClipReason::TargetDestroyed,
            title: "Cible détruite".to_owned(),
            dedupe_key: Some("target-destroyed|clip_invalid".to_owned()),
            detected_at: system_time_rfc3339(now),
            event_time: system_time_rfc3339(now),
            start_time: system_time_rfc3339(now - Duration::from_secs(20)),
            end_time: system_time_rfc3339(now + Duration::from_secs(5)),
            pre_event_seconds: 20,
            post_event_seconds: 5,
            segments: vec![PathBuf::from("segments/missing.webm")],
            status: PendingExportStatus::ReadyToExport,
            events: vec![kill("dawson16800")],
            player_name: Some("dawson16800".to_owned()),
            final_video_path: None,
            metadata_path: None,
            thumbnail_path: None,
        };
        std::fs::write(
            clip_dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let mut queue = ExportQueue::default();
        queue.load_from_pending_dir(&config).unwrap();

        assert!(queue.pending.is_empty());
        assert!(!clip_dir.exists());
    }

    #[test]
    fn final_clip_verified_before_pending_cleanup() {
        let root = std::env::temp_dir().join(format!(
            "wt-clipper-final-verify-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let video = root.join("clip.webm");
        let metadata = root.join("clip.json");
        std::fs::write(&video, b"video").unwrap();
        std::fs::write(&metadata, b"{}").unwrap();
        let replay = SavedReplay {
            final_video_path: Some(video.clone()),
            metadata_path: Some(metadata.clone()),
            segments_dir: None,
        };

        let verified = verify_completed_export(&replay, None).unwrap();

        assert_eq!(verified.final_video_path, video);
        assert_eq!(verified.metadata_path, metadata);
        assert!(root.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn successful_export_removes_pending_dir() {
        let root = std::env::temp_dir().join(format!(
            "wt-clipper-remove-pending-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let pending_dir = root.join("pending").join("clip-success");
        std::fs::create_dir_all(&pending_dir).unwrap();
        std::fs::write(pending_dir.join("manifest.json"), b"{}").unwrap();

        delete_pending_dir(&pending_dir, "clip-success");

        assert!(!pending_dir.exists());
        if root.exists() {
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn failed_export_keeps_pending_dir() {
        let root = std::env::temp_dir().join(format!(
            "wt-clipper-failed-keeps-pending-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        let pending_dir = root.join("pending").join("clip-failed");
        std::fs::create_dir_all(&pending_dir).unwrap();
        let video = root.join("clip.webm");
        std::fs::write(&video, b"video").unwrap();
        let replay = SavedReplay {
            final_video_path: Some(video),
            metadata_path: Some(root.join("missing.json")),
            segments_dir: None,
        };

        assert!(verify_completed_export(&replay, None).is_err());
        assert!(pending_dir.exists());

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn multi_kill_creates_one_pending_export_with_multi_title() {
        let config = test_auto_config(5, 2);
        let event_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let mut pending = PendingClip::new(
            kill("dawson16800"),
            "kill-1".to_owned(),
            ClipReason::TargetDestroyed,
            Instant::now(),
            Some(Duration::from_secs(120)),
            event_time,
            Duration::from_secs(1),
            "Kill 1".to_owned(),
        );
        assert!(pending.add_event(
            kill_with_target("[ai] MiG-21"),
            "kill-2".to_owned(),
            Instant::now() + Duration::from_secs(2),
            Some(Duration::from_secs(122)),
            event_time + Duration::from_secs(2),
            Duration::from_secs(1),
            "Kill 2".to_owned(),
        ));

        let clip = pending_export_from_pending(
            &pending,
            Some("dawson16800"),
            &config,
            Duration::from_secs(5),
        );

        assert_eq!(clip.reason, ClipReason::MultiKill);
        assert_eq!(clip.events.len(), 2);
        assert_eq!(clip.title, "Multi-kill — 2 kills");
    }
}
