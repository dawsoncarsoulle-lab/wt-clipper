use std::{
    collections::HashSet,
    path::PathBuf,
    time::{Duration, Instant, SystemTime},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, error, info};
use uuid::Uuid;

use crate::{
    app::{
        clip_types::{ClipContext, ClipReason},
        events::{AppEvent, ClipStatus, ClipStatusPayload},
    },
    capture::gpu_screen_recorder::{GpuScreenRecorderHandle, GsrHealth, SavedGsrReplay},
    config::{AppConfig, CaptureConfig, TriggerConfig, WarThunderConfig},
    warthunder::{
        client::{ChatMessage, WarThunderClient},
        events::WarThunderEvent,
        parser::parse_gamechat_event,
        recent::{RecentEventCache, RecentMessageCache},
    },
};

const EVENT_DEDUPE_TTL: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub struct AutoClipConfig {
    pub cooldown: Duration,
    pub post_event_delay: Duration,
    pub multi_kill_window: Duration,
    pub include_history: bool,
    pub triggers: TriggerConfig,
    pub ui_events: Option<mpsc::UnboundedSender<AppEvent>>,
    pub command_rx: Option<mpsc::UnboundedReceiver<AutoClipCommand>>,
    pub capture: CaptureConfig,
}

#[derive(Debug)]
pub enum AutoClipCommand {
    SaveManualClip,
    TestGsrSaveReplay {
        respond_to: oneshot::Sender<Result<PathBuf, String>>,
    },
    RestartGpuRecorder,
    Shutdown,
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
    pub message: String,
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

#[derive(Debug)]
struct Cooldown {
    duration: Duration,
    last_save: Option<Instant>,
}

impl Cooldown {
    fn new(duration: Duration) -> Self {
        Self {
            duration,
            last_save: None,
        }
    }

    fn allows(&self, now: Instant) -> bool {
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
struct ScheduledClip {
    clip_id: String,
    created_at: String,
    reason: ClipReason,
    last_event_time: Instant,
    last_event_game_time: Option<Duration>,
    first_event_wall_time: SystemTime,
    last_event_wall_time: SystemTime,
    save_at: Instant,
    event_keys: HashSet<String>,
    events: Vec<WarThunderEvent>,
    descriptions: Vec<String>,
    retry_count: u8,
}

impl ScheduledClip {
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
        Self {
            clip_id: format!("clip_{}", Uuid::new_v4()),
            created_at: Utc::now().to_rfc3339(),
            reason,
            last_event_time: event_time,
            last_event_game_time: event_game_time,
            first_event_wall_time: event_wall_time,
            last_event_wall_time: event_wall_time,
            save_at: event_time + delay,
            event_keys,
            events: vec![event],
            descriptions: vec![description],
            retry_count: 0,
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

#[derive(Debug, Clone)]
struct DetectedEvent {
    event: WarThunderEvent,
    reason: ClipReason,
    canonical_key: String,
    detected_at: Instant,
    game_time: Option<Duration>,
    detected_wall_time: SystemTime,
}

fn apply_runtime_config(
    auto_config: &mut AutoClipConfig,
    config: &AppConfig,
) -> RuntimeConfigUpdateResult {
    let restart_required = auto_config.capture != config.capture;
    auto_config.triggers = config.triggers.clone();
    auto_config.post_event_delay = Duration::from_secs(config.clip.post_event_seconds);
    auto_config.multi_kill_window = Duration::from_secs(config.clip.multi_kill_window_seconds);
    auto_config.capture = config.capture.clone();

    RuntimeConfigUpdateResult {
        applied_live: true,
        restart_required,
        message: if restart_required {
            "Configuration sauvegardée; GPU Screen Recorder va être redémarré.".to_owned()
        } else {
            "Configuration appliquée au backend GPU Screen Recorder.".to_owned()
        },
    }
}

pub async fn run_auto_clip(
    client: WarThunderClient,
    warthunder_config: WarThunderConfig,
    mut auto_config: AutoClipConfig,
) -> anyhow::Result<()> {
    let mut command_rx = auto_config.command_rx.take();
    let gsr = GpuScreenRecorderHandle::new(auto_config.capture.clone());
    if auto_config
        .capture
        .capture_strategy
        .should_wait_for_war_thunder()
    {
        gsr.mark_waiting_for_war_thunder().await;
        println!(
            "[CAPTURE] strategy={:?}: waiting for War Thunder localhost before resolving capture target",
            auto_config.capture.capture_strategy
        );
    } else if let Err(error) = gsr.start().await {
        let message = format!("GPU Screen Recorder: {error}");
        error!(%message, "failed to start GSR backend");
        send_ui_event(
            &auto_config,
            AppEvent::ClipFailed {
                message: message.clone(),
            },
        );
    }
    send_gsr_status(&auto_config, &gsr).await;

    println!("GPU Replay armed for War Thunder clips.");
    println!("Auto-clip armed for personal War Thunder kills.");
    println!("Press Ctrl+C to stop.");

    let player_name = warthunder_config.player_name.as_deref();
    let mut configured_post_event_delay = auto_config.post_event_delay;
    let mut effective_save_delay = configured_post_event_delay;
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
    let mut pending_clips = Vec::<ScheduledClip>::new();
    let mut wt_tick = interval(warthunder_config.poll_interval());
    wt_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut cleanup_tick = interval(Duration::from_secs(1));
    cleanup_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_wt_connected = None;
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    let result = loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("received Ctrl+C, stopping GSR auto-clip");
                break Ok(());
            }
            _ = cleanup_tick.tick() => {
                gsr.refresh_status().await;
                send_gsr_status(&auto_config, &gsr).await;
                save_ready_pending_clips_gsr(
                    &gsr,
                    &mut pending_clips,
                    &mut cooldown,
                    Instant::now(),
                    player_name,
                    &auto_config,
                    configured_post_event_delay,
                ).await;
            }
            command = recv_auto_command(&mut command_rx), if command_rx.is_some() => {
                let Some(command) = command else {
                    command_rx = None;
                    continue;
                };
                match command {
                    AutoClipCommand::Shutdown => {
                        info!("received backend shutdown command");
                        break Ok(());
                    }
                    AutoClipCommand::SaveManualClip => {
                        handle_manual_clip_command_gsr(&gsr, player_name, &auto_config).await;
                    }
                    AutoClipCommand::TestGsrSaveReplay { respond_to } => {
                        let result =
                            handle_test_gsr_save_replay_command(&gsr, player_name, &auto_config)
                                .await
                                .map(|replay| replay.final_video_path);
                        let _ = respond_to.send(result.map_err(|error| error.to_string()));
                    }
                    AutoClipCommand::RestartGpuRecorder => {
                        if auto_config.capture.capture_strategy.should_wait_for_war_thunder()
                            && last_wt_connected != Some(true)
                        {
                            let message = "War Thunder n'est pas encore détecté; GPU Screen Recorder démarrera automatiquement quand l'API localhost sera disponible.".to_owned();
                            send_ui_event(&auto_config, AppEvent::ClipFailed { message });
                        } else if let Err(error) = gsr.restart(None, "manual GPU Screen Recorder restart request").await {
                            send_ui_event(
                                &auto_config,
                                AppEvent::ClipFailed {
                                    message: format!("Redémarrage GPU Screen Recorder: {error}"),
                                },
                            );
                        }
                        send_gsr_status(&auto_config, &gsr).await;
                    }
                    AutoClipCommand::UpdateConfig { config, respond_to } => {
                        let capture_config = config.capture.clone();
                        let mut response = apply_runtime_config(&mut auto_config, &config);
                        configured_post_event_delay = auto_config.post_event_delay;
                        effective_save_delay = configured_post_event_delay;
                        if auto_config.capture.capture_strategy.should_wait_for_war_thunder()
                            && last_wt_connected != Some(true)
                        {
                            gsr.update_config_without_restart(capture_config).await;
                            response.restart_required = false;
                            response.message = "Configuration appliquée; GSR attend War Thunder avant de résoudre la cible capture.".to_owned();
                        } else {
                            match gsr.update_config_and_restart_if_needed(capture_config).await {
                                Ok(restarted) => {
                                    response.restart_required = false;
                                    response.message = if restarted {
                                        "Configuration appliquée; GPU Screen Recorder redémarré avec la nouvelle commande.".to_owned()
                                    } else {
                                        "Configuration appliquée au backend GPU Screen Recorder.".to_owned()
                                    };
                                }
                                Err(error) => {
                                    response.restart_required = true;
                                    response.message = format!("Configuration sauvegardée; redémarrage GPU Screen Recorder impossible: {error}");
                                    send_ui_event(
                                        &auto_config,
                                        AppEvent::ClipFailed {
                                            message: response.message.clone(),
                                        },
                                    );
                                }
                            }
                        }
                        send_gsr_status(&auto_config, &gsr).await;
                        let _ = respond_to.send(response);
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
                    if wt_connected {
                        ensure_gsr_started_after_war_thunder_connected(&gsr, &auto_config).await;
                    }
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
                            "duplicate event already exists in pending GSR clip"
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
                            send_ui_event(
                                &auto_config,
                                AppEvent::ClipStatusChanged {
                                    payload: status_payload(
                                        pending,
                                        ClipStatus::Detected,
                                        pending.reason,
                                        format!(
                                            "Multi-kill GPU en attente : {} kills, sauvegarde dans {}s",
                                            pending.kill_count(),
                                            effective_save_delay.as_secs()
                                        ),
                                        Some(15),
                                        None,
                                    ),
                                },
                            );
                        }
                        continue;
                    }
                    if !cooldown.allows(now) {
                        debug!(?event, "[CLIP] cooldown active, scheduling distinct GSR clip");
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
                    println!(
                        "[CLIP] scheduled GPU replay save in {}s...",
                        effective_save_delay.as_secs()
                    );
                    send_ui_event(
                        &auto_config,
                        AppEvent::ClipStatusChanged {
                            payload: status_payload(
                                &pending,
                                ClipStatus::Detected,
                                reason,
                                format!(
                                    "Clip GPU programmé: {summary} (sauvegarde dans {}s)",
                                    effective_save_delay.as_secs()
                                ),
                                Some(10),
                                None,
                            ),
                        },
                    );
                    pending_clips.push(pending);
                }
            }
        }
    };

    if let Err(error) = gsr.stop().await {
        if result.is_ok() {
            return Err(error);
        }
        debug!(%error, "failed to stop GPU Screen Recorder after auto-clip error");
    }
    result
}

async fn ensure_gsr_started_after_war_thunder_connected(
    gsr: &GpuScreenRecorderHandle,
    auto_config: &AutoClipConfig,
) {
    let status = gsr.status().await;
    if matches!(
        status.health,
        GsrHealth::Running | GsrHealth::SavingReplay | GsrHealth::Starting
    ) {
        return;
    }
    println!("[CAPTURE] War Thunder localhost detected; resolving capture target and starting GSR");
    if let Err(error) = gsr.start().await {
        let message = format!("GPU Screen Recorder: {error}");
        error!(%message, "failed to start GSR after War Thunder localhost connection");
        send_ui_event(auto_config, AppEvent::ClipFailed { message });
    }
    send_gsr_status(auto_config, gsr).await;
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
) -> ScheduledClip {
    ScheduledClip::new(
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
    pending: &ScheduledClip,
    player_name: Option<&str>,
    auto_config: &AutoClipConfig,
    configured_post_event_delay: Duration,
) -> ClipContext {
    ClipContext {
        reason: pending.reason,
        event: pending.events.first().cloned(),
        events: pending.events.clone(),
        player_name: player_name.map(str::to_owned),
        pending_clip_id: Some(pending.clip_id.clone()),
        pending_dedupe_key: Some(pending_dedupe_key(pending)),
        duration_seconds: auto_config.capture.replay_seconds,
        post_event_seconds: configured_post_event_delay.as_secs(),
        first_event_time: Some(pending.first_event_wall_time),
        last_event_time: Some(pending.last_event_wall_time),
    }
}

fn status_payload(
    pending: &ScheduledClip,
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

fn pending_dedupe_key(pending: &ScheduledClip) -> String {
    let mut keys = pending.event_keys.iter().cloned().collect::<Vec<_>>();
    keys.sort();
    format!("{}|{}", pending.reason.slug(), keys.join("|"))
}

fn ready_pending_indices(pending_clips: &[ScheduledClip], now: Instant) -> Vec<usize> {
    pending_clips
        .iter()
        .enumerate()
        .filter_map(|(index, pending)| pending.is_ready(now).then_some(index))
        .collect()
}

fn pending_index_for_multi_kill(
    pending_clips: &[ScheduledClip],
    reason: ClipReason,
    now: Instant,
    game_time: Option<Duration>,
    multi_kill_window: Duration,
) -> Option<usize> {
    if reason != ClipReason::TargetDestroyed {
        return None;
    }

    pending_clips.iter().position(|pending| {
        matches!(
            pending.reason,
            ClipReason::TargetDestroyed | ClipReason::MultiKill
        ) && pending.is_ready(now) == false
            && game_time
                .zip(pending.last_event_game_time)
                .map(|(game_time, last_game_time)| {
                    game_time >= last_game_time
                        && game_time.saturating_sub(last_game_time) <= multi_kill_window
                })
                .unwrap_or_else(|| now.duration_since(pending.last_event_time) <= multi_kill_window)
    })
}

async fn recv_auto_command(
    command_rx: &mut Option<mpsc::UnboundedReceiver<AutoClipCommand>>,
) -> Option<AutoClipCommand> {
    match command_rx {
        Some(command_rx) => command_rx.recv().await,
        None => std::future::pending::<Option<AutoClipCommand>>().await,
    }
}

async fn save_ready_pending_clips_gsr(
    gsr: &GpuScreenRecorderHandle,
    pending_clips: &mut Vec<ScheduledClip>,
    cooldown: &mut Cooldown,
    now: Instant,
    player_name: Option<&str>,
    auto_config: &AutoClipConfig,
    configured_post_event_delay: Duration,
) {
    let ready_indices = ready_pending_indices(pending_clips, now);
    for index in ready_indices.into_iter().rev() {
        let Some(pending) = pending_clips.get(index) else {
            continue;
        };
        println!(
            "[GSR_AUTO] post_event elapsed id={} reason={}",
            pending.clip_id,
            pending.reason.slug()
        );
        let context = clip_context_for_pending(
            pending,
            player_name,
            auto_config,
            configured_post_event_delay,
        );
        send_ui_event(
            auto_config,
            AppEvent::ClipStatusChanged {
                payload: status_payload(
                    pending,
                    ClipStatus::Recording,
                    pending.reason,
                    "Sauvegarde GPU Replay...".to_owned(),
                    Some(35),
                    None,
                ),
            },
        );
        println!("[GSR_AUTO] calling gsr.save_replay id={}", pending.clip_id);
        match gsr.save_replay(context).await {
            Ok(replay) => {
                let pending = pending_clips.remove(index);
                emit_gsr_replay_saved(auto_config, &pending, replay);
                cooldown.record_save(now);
            }
            Err(error) => {
                let pending = &mut pending_clips[index];
                let should_retry = mark_gsr_save_failure_for_retry(pending, Instant::now());
                let message = if should_retry {
                    format!(
                        "GPU Screen Recorder n'a pas sauvegardé le clip: {error}; retry {}/3",
                        pending.retry_count
                    )
                } else {
                    format!("GPU Screen Recorder n'a pas sauvegardé le clip: {error}")
                };
                send_ui_event(
                    auto_config,
                    AppEvent::ClipStatusChanged {
                        payload: status_payload(
                            pending,
                            ClipStatus::Failed,
                            pending.reason,
                            if should_retry {
                                "Erreur GPU Replay, nouvel essai...".to_owned()
                            } else {
                                "Erreur GPU Replay".to_owned()
                            },
                            None,
                            Some(message.clone()),
                        ),
                    },
                );
                send_ui_event(auto_config, AppEvent::ClipFailed { message });
                cooldown.record_save(now);
                if !should_retry {
                    let _ = pending_clips.remove(index);
                }
            }
        }
        send_gsr_status(auto_config, gsr).await;
    }
}

fn mark_gsr_save_failure_for_retry(pending: &mut ScheduledClip, now: Instant) -> bool {
    pending.retry_count = pending.retry_count.saturating_add(1);
    let should_retry = pending.retry_count < 3;
    if should_retry {
        pending.save_at = now + Duration::from_secs(2);
    }
    should_retry
}

async fn handle_manual_clip_command_gsr(
    gsr: &GpuScreenRecorderHandle,
    player_name: Option<&str>,
    auto_config: &AutoClipConfig,
) {
    let mut status = ClipStatusPayload {
        id: format!("clip_{}", Uuid::new_v4()),
        status: ClipStatus::Recording,
        reason: ClipReason::Manual,
        title: "Sauvegarde GPU Replay...".to_owned(),
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
    send_ui_event(
        auto_config,
        AppEvent::ClipStatusChanged {
            payload: status.clone(),
        },
    );

    let context = ClipContext {
        reason: ClipReason::Manual,
        event: None,
        events: Vec::new(),
        player_name: player_name.map(str::to_owned),
        pending_clip_id: Some(status.id.clone()),
        pending_dedupe_key: Some(format!("manual|{}", status.id)),
        duration_seconds: auto_config.capture.replay_seconds,
        post_event_seconds: 0,
        first_event_time: None,
        last_event_time: None,
    };

    match gsr.save_replay(context).await {
        Ok(replay) => {
            status.status = ClipStatus::Ready;
            status.title = "Clip prêt".to_owned();
            status.file_path = Some(replay.final_video_path.clone());
            status.thumbnail_path = replay.thumbnail_path.clone();
            status.duration_seconds = Some(replay.duration_seconds);
            status.size_bytes = Some(replay.size_bytes);
            status.progress = Some(100);
            send_ui_event(auto_config, AppEvent::ClipStatusChanged { payload: status });
            send_ui_event(
                auto_config,
                AppEvent::ClipSaved {
                    path: replay.final_video_path,
                    reason: ClipReason::Manual,
                    duration_seconds: replay.duration_seconds,
                    size_bytes: replay.size_bytes,
                },
            );
        }
        Err(error) => {
            let message = format!("Clip manuel GPU Replay: {error}");
            status.status = ClipStatus::Failed;
            status.title = "Erreur GPU Replay".to_owned();
            status.progress = None;
            status.error = Some(message.clone());
            send_ui_event(auto_config, AppEvent::ClipStatusChanged { payload: status });
            send_ui_event(auto_config, AppEvent::ClipFailed { message });
        }
    }
    send_gsr_status(auto_config, gsr).await;
}

async fn handle_test_gsr_save_replay_command(
    gsr: &GpuScreenRecorderHandle,
    player_name: Option<&str>,
    auto_config: &AutoClipConfig,
) -> anyhow::Result<SavedGsrReplay> {
    let id = format!("gsr_test_{}", Uuid::new_v4());
    let mut status = ClipStatusPayload {
        id: id.clone(),
        status: ClipStatus::Recording,
        reason: ClipReason::Manual,
        title: "Test sauvegarde GPU Replay...".to_owned(),
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
    send_ui_event(
        auto_config,
        AppEvent::ClipStatusChanged {
            payload: status.clone(),
        },
    );

    let context = ClipContext {
        reason: ClipReason::Manual,
        event: None,
        events: Vec::new(),
        player_name: player_name.map(str::to_owned),
        pending_clip_id: Some(id.clone()),
        pending_dedupe_key: Some(format!("gsr-test|{id}")),
        duration_seconds: auto_config.capture.replay_seconds,
        post_event_seconds: 0,
        first_event_time: None,
        last_event_time: None,
    };

    println!("[GSR_AUTO] calling gsr.save_replay id={id}");
    match gsr.save_replay(context).await {
        Ok(replay) => {
            status.status = ClipStatus::Ready;
            status.title = "Test GPU Replay prêt".to_owned();
            status.file_path = Some(replay.final_video_path.clone());
            status.thumbnail_path = replay.thumbnail_path.clone();
            status.duration_seconds = Some(replay.duration_seconds);
            status.size_bytes = Some(replay.size_bytes);
            status.progress = Some(100);
            send_ui_event(auto_config, AppEvent::ClipStatusChanged { payload: status });
            send_ui_event(
                auto_config,
                AppEvent::ClipSaved {
                    path: replay.final_video_path.clone(),
                    reason: ClipReason::Manual,
                    duration_seconds: replay.duration_seconds,
                    size_bytes: replay.size_bytes,
                },
            );
            send_gsr_status(auto_config, gsr).await;
            Ok(replay)
        }
        Err(error) => {
            let message = format!("Test sauvegarde GPU Replay: {error}");
            status.status = ClipStatus::Failed;
            status.title = "Erreur GPU Replay".to_owned();
            status.progress = None;
            status.error = Some(message.clone());
            send_ui_event(auto_config, AppEvent::ClipStatusChanged { payload: status });
            send_ui_event(
                auto_config,
                AppEvent::ClipFailed {
                    message: message.clone(),
                },
            );
            send_gsr_status(auto_config, gsr).await;
            Err(anyhow::anyhow!(message))
        }
    }
}

fn emit_gsr_replay_saved(
    auto_config: &AutoClipConfig,
    pending: &ScheduledClip,
    replay: SavedGsrReplay,
) {
    let mut ready = status_payload(
        pending,
        ClipStatus::Ready,
        pending.reason,
        "Clip prêt".to_owned(),
        Some(100),
        None,
    );
    ready.file_path = Some(replay.final_video_path.clone());
    ready.thumbnail_path = replay.thumbnail_path.clone();
    ready.duration_seconds = Some(replay.duration_seconds);
    ready.size_bytes = Some(replay.size_bytes);
    send_ui_event(auto_config, AppEvent::ClipStatusChanged { payload: ready });
    send_ui_event(
        auto_config,
        AppEvent::ClipSaved {
            path: replay.final_video_path,
            reason: pending.reason,
            duration_seconds: replay.duration_seconds,
            size_bytes: replay.size_bytes,
        },
    );
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

async fn send_gsr_status(auto_config: &AutoClipConfig, gsr: &GpuScreenRecorderHandle) {
    let status = gsr.status().await;
    debug!(
        health = ?status.health,
        pid = ?status.pid,
        command = ?status.command_line,
        "queueing GSR status from auto backend"
    );
    send_ui_event(auto_config, AppEvent::GsrStatusChanged { status });
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

    fn test_auto_config(post_event_seconds: u64) -> AutoClipConfig {
        AutoClipConfig {
            cooldown: Duration::from_secs(3),
            post_event_delay: Duration::from_secs(post_event_seconds),
            multi_kill_window: Duration::from_secs(5),
            include_history: false,
            triggers: TriggerConfig::default(),
            ui_events: None,
            command_rx: None,
            capture: CaptureConfig::default(),
        }
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
    fn clip_context_uses_configured_post_event_delay() {
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
            Duration::from_secs(5),
        );
        let config = test_auto_config(5);

        let context = clip_context_for_pending(
            &pending,
            Some("dawson16800"),
            &config,
            config.post_event_delay,
        );

        assert_eq!(pending.save_at, now + Duration::from_secs(5));
        assert_eq!(context.post_event_seconds, 5);
        assert_eq!(context.duration_seconds, config.capture.replay_seconds);
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
    fn failed_save_retries_twice_then_drops() {
        let now = Instant::now();
        let mut pending = schedule_pending_clip(
            kill_with_target("[ai] MiG-15bis"),
            event_key(&kill_with_target("[ai] MiG-15bis")),
            ClipReason::TargetDestroyed,
            "kill".to_owned(),
            now,
            None,
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(5),
        );

        assert!(mark_gsr_save_failure_for_retry(&mut pending, now));
        assert_eq!(pending.retry_count, 1);
        assert_eq!(pending.save_at, now + Duration::from_secs(2));
        assert!(mark_gsr_save_failure_for_retry(
            &mut pending,
            now + Duration::from_secs(2)
        ));
        assert_eq!(pending.retry_count, 2);
        assert!(!mark_gsr_save_failure_for_retry(
            &mut pending,
            now + Duration::from_secs(4)
        ));
        assert_eq!(pending.retry_count, 3);
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
    }
}
