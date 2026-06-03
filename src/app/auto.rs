use std::{
    collections::HashSet,
    path::PathBuf,
    time::{Duration, Instant, SystemTime},
};

use chrono::Utc;
use tokio::sync::mpsc;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, info};
use uuid::Uuid;

use crate::{
    capture::{
        buffer::{
            ClipContext, ClipReason, ReplayBufferConfig, ReplayBufferHandle, SaveReplayOutcome,
        },
        quality::{QualityPreset, VideoQuality},
    },
    cli::CaptureSource,
    config::WarThunderConfig,
    ui::bridge::{AppEvent, ClipStatus, ClipStatusPayload},
    warthunder::{
        client::{ChatMessage, WarThunderClient},
        events::WarThunderEvent,
        parser::{is_personal_kill, parse_gamechat_event},
        recent::{RecentEventCache, RecentMessageCache},
    },
};

const EVENT_DEDUPE_TTL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
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
    pub target_destroyed_trigger: bool,
    pub ui_events: Option<mpsc::UnboundedSender<AppEvent>>,
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
        self.last_event_time = event_time;
        self.last_event_game_time = event_game_time.or(self.last_event_game_time);
        self.last_event_wall_time = event_wall_time;
        self.save_at = event_time + delay;
        self.events.push(event);
        self.descriptions.push(description);
        if self.events.len() > 1 && self.events.iter().all(is_personal_kill_event) {
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
            .filter(|event| is_personal_kill_event(event))
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
    canonical_key: String,
    detected_at: Instant,
    game_time: Option<Duration>,
    detected_wall_time: SystemTime,
}

pub(crate) fn effective_post_event_delay(
    post_event_delay: Duration,
    segment_seconds: u64,
) -> Duration {
    post_event_delay + Duration::from_secs(segment_seconds.saturating_add(1))
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
    auto_config: AutoClipConfig,
) -> anyhow::Result<()> {
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
    let configured_post_event_delay = auto_config.post_event_delay;
    let effective_save_delay =
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
                        &mut cooldown,
                        Instant::now(),
                        player_name,
                        &auto_config,
                        configured_post_event_delay,
                    ).await {
                        break Err(error);
                    }
                }
            }
            _ = wt_tick.tick() => {
                let (events, wt_connected) = poll_personal_events(&client, &mut state, player_name).await;
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
                    println!("[WT] kill detected: {summary}");
                    let reason = clip_reason_for_event(&event);
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
                            send_ui_event(
                                &auto_config,
                                AppEvent::ClipStatusChanged {
                                    payload: status_payload(
                                        pending,
                                        ClipStatus::Detected,
                                        pending.reason,
                                        format!(
                                            "Multi-kill en attente: {} kills, sauvegarde dans {}s",
                                            pending.kill_count(),
                                            effective_save_delay.as_secs()
                                        ),
                                        Some(15),
                                        None,
                                    ),
                                },
                            );
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
                    println!(
                        "[CLIP] scheduled replay save in {}s...",
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
                                    "Clip programmé: {summary} (sauvegarde dans {}s)",
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
    }
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
    event_time: Instant,
    event_game_time: Option<Duration>,
    multi_kill_window: Duration,
) -> Option<usize> {
    pending_clips
        .iter()
        .enumerate()
        .find_map(|(index, pending)| {
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
        println!("[CLIP] saving replay...");
        let context = clip_context_for_pending(
            &pending,
            player_name,
            auto_config,
            configured_post_event_delay,
        );
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
                send_ui_event(
                    auto_config,
                    AppEvent::ClipStatusChanged {
                        payload: status_payload(
                            &pending,
                            ClipStatus::Failed,
                            pending.reason,
                            "Erreur pendant la création du clip".to_owned(),
                            None,
                            Some(reason.clone()),
                        ),
                    },
                );
                send_ui_event(
                    auto_config,
                    AppEvent::ClipFailed {
                        message: format!("Clip ignoré: {reason}"),
                    },
                );
                cooldown.record_save(now);
            }
            Err(error) => {
                let message = error.to_string();
                send_ui_event(
                    auto_config,
                    AppEvent::ClipStatusChanged {
                        payload: status_payload(
                            &pending,
                            ClipStatus::Failed,
                            pending.reason,
                            "Erreur pendant la création du clip".to_owned(),
                            None,
                            Some(message.clone()),
                        ),
                    },
                );
                send_ui_event(
                    auto_config,
                    AppEvent::ClipFailed {
                        message: format!("Clip: {message}"),
                    },
                );
                cooldown.record_save(now);
            }
        }
    }
    Ok(())
}

fn requeue_pending_clip(
    pending_clips: &mut Vec<PendingClip>,
    mut pending: PendingClip,
    now: Instant,
) {
    pending.save_at = now + Duration::from_secs(1);
    pending_clips.push(pending);
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
                &mut events,
            );
            collect_personal_events(
                "hud:damage",
                hud.damage,
                &mut state.seen_messages,
                &mut state.seen_events,
                player_name,
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
        if is_personal_kill(&event, player_name) {
            let Some(event_key) = event_key else {
                debug!(source, ?event, "personal kill has no canonical key");
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
                canonical_key: event_key,
                detected_at: now,
                game_time: parse_wt_message_time(message.time.as_deref()),
                detected_wall_time: SystemTime::now(),
            });
        } else {
            debug!(source, message = %message.text, ?event, "ignoring non-personal auto-clip event");
        }
    }
}

fn raw_message_dedupe_key(source: &str, message: &ChatMessage) -> Option<String> {
    message.id.map(|id| format!("{source}:{id}"))
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

fn clip_reason_for_event(event: &WarThunderEvent) -> ClipReason {
    match event {
        WarThunderEvent::TargetDestroyed { action, .. } if is_clip_action(action) => {
            ClipReason::TargetDestroyed
        }
        WarThunderEvent::TargetDestroyed { .. } => ClipReason::Unknown,
        WarThunderEvent::PlayerDestroyed { .. } => ClipReason::PlayerDestroyed,
        WarThunderEvent::BaseDestroyed { .. } => ClipReason::BaseDestroyed,
        WarThunderEvent::Unknown(_) => ClipReason::Unknown,
        WarThunderEvent::CriticalHit { .. } | WarThunderEvent::SevereDamage { .. } => {
            ClipReason::Unknown
        }
    }
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

fn is_personal_kill_event(event: &WarThunderEvent) -> bool {
    matches!(
        event,
        WarThunderEvent::TargetDestroyed { action, .. } if is_clip_action(action)
    )
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
            quality_preset: QualityPreset::High,
            quality: VideoQuality::default(),
            cooldown: Duration::from_secs(3),
            post_event_delay: Duration::from_secs(post_event_seconds),
            multi_kill_window: Duration::from_secs(5),
            include_history: false,
            target_destroyed_trigger: true,
            ui_events: None,
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
            pending_index_for_multi_kill(&[pending], later, None, Duration::from_secs(5)),
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
                now + Duration::from_secs(4),
                None,
                Duration::from_secs(5)
            ),
            Some(0)
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
            &mut events,
        );

        assert_eq!(events.len(), 1);
        assert_eq!(
            clip_reason_for_event(&events[0].event),
            ClipReason::TargetDestroyed
        );
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
            &mut events,
        );

        assert_eq!(events.len(), 1);
    }

    #[test]
    fn raw_message_dedupe_only_uses_real_war_thunder_ids() {
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
        assert_eq!(raw_message_dedupe_key("hud:damage", &without_id), None);
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
            &mut events,
        );

        assert_eq!(events.len(), 2);
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
            clip_reason_for_event(&kill("dawson16800")).slug(),
            "target-destroyed"
        );
    }
}
