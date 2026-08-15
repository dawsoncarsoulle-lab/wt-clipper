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
    config::{AppConfig, CaptureConfig, TriggerConfig},
    games::event::{GameEvent, GameEventKind},
    games::source::GameSource,
};

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
    events: Vec<GameEvent>,
    descriptions: Vec<String>,
    retry_count: u8,
}

impl ScheduledClip {
    fn new(
        event: GameEvent,
        reason: ClipReason,
        event_time: Instant,
        event_game_time: Option<Duration>,
        event_wall_time: SystemTime,
        delay: Duration,
    ) -> Self {
        let mut event_keys = HashSet::new();
        event_keys.insert(event.canonical_key.clone());
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
            events: vec![event.clone()],
            descriptions: vec![event.summary],
            retry_count: 0,
        }
    }

    fn add_event(
        &mut self,
        event: GameEvent,
        event_time: Instant,
        event_game_time: Option<Duration>,
        event_wall_time: SystemTime,
        delay: Duration,
    ) -> bool {
        if !self.event_keys.insert(event.canonical_key.clone()) {
            return false;
        }
        self.last_event_time = event_time;
        self.last_event_game_time = event_game_time.or(self.last_event_game_time);
        self.last_event_wall_time = event_wall_time;
        self.save_at = event_time + delay;
        let description = event.summary.clone();
        self.events.push(event);
        self.descriptions.push(description);
        if self.events.len() > 1 && self.events.iter().all(|e| e.kind == GameEventKind::Kill) {
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
            .filter(|event| event.kind == GameEventKind::Kill)
            .count()
    }
}

#[derive(Debug, Clone)]
struct DetectedEvent {
    event: GameEvent,
    reason: ClipReason,
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
    source: Box<dyn GameSource>,
    poll_interval: Duration,
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

    let player_name = source.player_name().map(str::to_owned);
    let player_name_ref = player_name.as_deref();
    let mut configured_post_event_delay = auto_config.post_event_delay;
    let mut effective_save_delay = configured_post_event_delay;
    if auto_config.include_history {
        println!(
            "[{}] include-history enabled: processing existing events",
            source.name()
        );
    } else {
        source.bootstrap(false).await;
        println!("[{}] watching for new events only", source.name());
    }

    let mut cooldown = Cooldown::new(auto_config.cooldown);
    let mut pending_clips = Vec::<ScheduledClip>::new();
    let mut wt_tick = interval(poll_interval);
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
                    player_name_ref,
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
                        handle_manual_clip_command_gsr(&gsr, player_name_ref, &auto_config).await;
                    }
                    AutoClipCommand::TestGsrSaveReplay { respond_to } => {
                        let result =
                            handle_test_gsr_save_replay_command(&gsr, player_name_ref, &auto_config)
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
                let poll_result = source.poll().await;
                let wt_connected = poll_result.connected;
                let now_instant = std::time::Instant::now();
                let now_wall = std::time::SystemTime::now();
                let events: Vec<DetectedEvent> = poll_result.events.into_iter().map(|event| {
                    let reason = game_event_kind_to_clip_reason(event.kind);
                    DetectedEvent {
                        detected_at: now_instant,
                        detected_wall_time: now_wall,
                        event,
                        reason,
                        game_time: None,
                    }
                }).collect();
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
                    let summary = detected.event.summary.clone();
                    let (vehicle, target) = (detected.event.context.clone(), detected.event.subject.clone());
                    let reason = detected.reason;
                    let canonical_key = detected.event.canonical_key.clone();
                    println!("[{}] event detected ({reason:?}): {summary}", source.name());
                    send_ui_event(&auto_config, AppEvent::KillDetected {
                        reason,
                        vehicle,
                        target,
                        description: summary.clone(),
                    });
                    let now = detected.detected_at;
                    if pending_clips
                        .iter()
                        .any(|pending| pending.event_keys.contains(&canonical_key))
                    {
                        debug!(
                            canonical_key = %canonical_key,
                            "duplicate event already exists in pending GSR clip"
                        );
                        continue;
                    }
                    let event = detected.event;
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
                            now,
                            detected.game_time,
                            detected.detected_wall_time,
                            effective_save_delay,
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
                        reason,
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
    event: GameEvent,
    reason: ClipReason,
    event_time: Instant,
    event_game_time: Option<Duration>,
    event_wall_time: SystemTime,
    delay: Duration,
) -> ScheduledClip {
    ScheduledClip::new(
        event,
        reason,
        event_time,
        event_game_time,
        event_wall_time,
        delay,
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

fn game_event_kind_to_clip_reason(kind: GameEventKind) -> ClipReason {
    match kind {
        GameEventKind::Kill => ClipReason::TargetDestroyed,
        GameEventKind::Death => ClipReason::PlayerDestroyed,
        GameEventKind::Objective => ClipReason::BaseDestroyed,
        GameEventKind::MultiKill => ClipReason::MultiKill,
        GameEventKind::Other => ClipReason::Unknown,
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

    fn kill_game_event(target: &str) -> GameEvent {
        let mut event = GameEvent::new(
            GameEventKind::Kill,
            format!("dawson16800 destroyed {target}"),
            format!("target_destroyed|dawson16800|f/a-18c early|destroyed|{target}"),
        );
        event.actor = Some("dawson16800".to_owned());
        event.subject = Some(target.to_owned());
        event.context = Some("F/A-18C Early".to_owned());
        event
    }

    fn objective_game_event() -> GameEvent {
        GameEvent::new(
            GameEventKind::Objective,
            "dawson16800 destroyed a base".to_owned(),
            "base_destroyed|dawson16800 destroyed a base".to_owned(),
        )
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
    fn clip_context_uses_configured_post_event_delay() {
        let event = kill_game_event("[ai] MiG-15bis");
        let now = Instant::now();
        let pending = schedule_pending_clip(
            event.clone(),
            ClipReason::TargetDestroyed,
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
            kill_game_event("[ai] MiG-15bis"),
            ClipReason::TargetDestroyed,
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
            kill_game_event("[ai] MiG-15bis"),
            ClipReason::TargetDestroyed,
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
            kill_game_event("[ai] MiG-15bis"),
            ClipReason::TargetDestroyed,
            now,
            None,
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(5),
        );

        let second = kill_game_event("IT-1");
        pending.add_event(
            second,
            now + Duration::from_secs(3),
            None,
            SystemTime::UNIX_EPOCH + Duration::from_secs(3),
            Duration::from_secs(5),
        );

        assert_eq!(pending.events.len(), 2);
        assert_eq!(pending.reason, ClipReason::MultiKill);
        assert_eq!(pending.save_at, now + Duration::from_secs(8));
        assert_eq!(pending.kill_count(), 2);
    }

    #[test]
    fn duplicate_kill_is_not_added_to_pending_clip() {
        let now = Instant::now();
        let event = kill_game_event("[ai] MiG-15bis");
        let mut pending = schedule_pending_clip(
            event.clone(),
            ClipReason::TargetDestroyed,
            now,
            None,
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(5),
        );

        let added = pending.add_event(
            event,
            now + Duration::from_secs(1),
            None,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1),
            Duration::from_secs(5),
        );

        assert!(!added);
        assert_eq!(pending.events.len(), 1);
        assert_eq!(pending.reason, ClipReason::TargetDestroyed);
    }

    #[test]
    fn multi_kill_requires_real_time_window() {
        let now = Instant::now();
        let pending = schedule_pending_clip(
            kill_game_event("[ai] MiG-15bis"),
            ClipReason::TargetDestroyed,
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
        let pending = schedule_pending_clip(
            kill_game_event("[ai] MiG-15bis"),
            ClipReason::TargetDestroyed,
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
        let pending = schedule_pending_clip(
            kill_game_event("[ai] MiG-15bis"),
            ClipReason::TargetDestroyed,
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
    fn multi_kill_window_uses_game_time_when_available() {
        let now = Instant::now();
        let pending = schedule_pending_clip(
            kill_game_event("IT-1"),
            ClipReason::TargetDestroyed,
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
    fn rapid_backend_read_splits_multi_kill_by_game_time() {
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
            let event = kill_game_event(target);
            // Use a unique canonical key per kill so they are distinct
            let mut event = event.clone();
            event.canonical_key = format!("{}|time:{game_seconds}", event.canonical_key);
            let detected_at = now + Duration::from_millis(game_seconds);
            if let Some(index) = pending_index_for_multi_kill(
                &pending_clips,
                ClipReason::TargetDestroyed,
                detected_at,
                Some(Duration::from_secs(game_seconds)),
                window,
            ) {
                pending_clips[index].add_event(
                    event,
                    detected_at,
                    Some(Duration::from_secs(game_seconds)),
                    SystemTime::UNIX_EPOCH + Duration::from_secs(game_seconds),
                    delay,
                );
            } else {
                pending_clips.push(schedule_pending_clip(
                    event,
                    ClipReason::TargetDestroyed,
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
    fn game_event_kind_to_clip_reason_mapping() {
        assert_eq!(
            game_event_kind_to_clip_reason(GameEventKind::Kill),
            ClipReason::TargetDestroyed
        );
        assert_eq!(
            game_event_kind_to_clip_reason(GameEventKind::Death),
            ClipReason::PlayerDestroyed
        );
        assert_eq!(
            game_event_kind_to_clip_reason(GameEventKind::Objective),
            ClipReason::BaseDestroyed
        );
        assert_eq!(
            game_event_kind_to_clip_reason(GameEventKind::MultiKill),
            ClipReason::MultiKill
        );
        assert_eq!(
            game_event_kind_to_clip_reason(GameEventKind::Other),
            ClipReason::Unknown
        );
    }

    #[test]
    fn objective_event_creates_pending_clip() {
        let now = Instant::now();
        let pending = schedule_pending_clip(
            objective_game_event(),
            ClipReason::BaseDestroyed,
            now,
            None,
            SystemTime::UNIX_EPOCH,
            Duration::from_secs(5),
        );
        assert_eq!(pending.reason, ClipReason::BaseDestroyed);
        assert_eq!(pending.kill_count(), 0);
    }
}
