use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use tokio::sync::mpsc;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, info};

use crate::{
    capture::{
        buffer::{ClipContext, ClipReason, ReplayBufferConfig, ReplayBufferHandle},
        quality::{QualityPreset, VideoQuality},
    },
    cli::CaptureSource,
    config::WarThunderConfig,
    ui::bridge::AppEvent,
    warthunder::{
        client::{ChatMessage, WarThunderClient},
        events::WarThunderEvent,
        parser::{is_personal_kill, parse_gamechat_event},
        recent::RecentMessageCache,
    },
};

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
}

impl AutoWatchState {
    fn new() -> Self {
        Self {
            last_chat_id: 0,
            last_evt_msg_id: 0,
            last_dmg_msg_id: 0,
            seen_messages: RecentMessageCache::new(1000),
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
    reason: ClipReason,
    first_event_time: Instant,
    last_event_time: Instant,
    save_at: Instant,
    events: Vec<WarThunderEvent>,
    descriptions: Vec<String>,
}

impl PendingClip {
    fn new(
        event: WarThunderEvent,
        reason: ClipReason,
        event_time: Instant,
        delay: Duration,
        description: String,
    ) -> Self {
        Self {
            reason,
            first_event_time: event_time,
            last_event_time: event_time,
            save_at: event_time + delay,
            events: vec![event],
            descriptions: vec![description],
        }
    }

    fn add_event(
        &mut self,
        event: WarThunderEvent,
        event_time: Instant,
        delay: Duration,
        description: String,
    ) {
        self.last_event_time = event_time;
        self.save_at = event_time + delay;
        self.events.push(event);
        self.descriptions.push(description);
        if self.events.len() > 1 && self.events.iter().all(is_personal_kill_event) {
            self.reason = ClipReason::MultiKill;
        }
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

pub(crate) fn effective_post_event_delay(
    post_event_delay: Duration,
    segment_seconds: u64,
) -> Duration {
    post_event_delay.max(Duration::from_secs(segment_seconds.saturating_add(1)))
}

pub async fn run_auto_clip(
    client: WarThunderClient,
    warthunder_config: WarThunderConfig,
    auto_config: AutoClipConfig,
) -> anyhow::Result<()> {
    let buffer = ReplayBufferHandle::start(ReplayBufferConfig {
        buffer_seconds: auto_config.buffer_seconds,
        segment_seconds: auto_config.segment_seconds,
        output_dir: auto_config.output_dir.clone(),
        source: auto_config.source,
        keep_segments: auto_config.keep_segments,
        quality_preset: auto_config.quality_preset,
        quality: auto_config.quality,
    })
    .await?;

    println!("Replay buffer active: {}s", auto_config.buffer_seconds);
    println!("Video target: {}", auto_config.quality.log_summary());
    println!("Auto-clip armed for personal War Thunder kills.");
    println!("Press Ctrl+C to stop.");

    let player_name = warthunder_config.player_name.as_deref();
    let post_event_delay =
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
                        filled_secs: buffer_started_at.elapsed().as_secs_f32().min(auto_config.buffer_seconds as f32),
                        total_secs: auto_config.buffer_seconds as f32,
                    });
                    if let Err(error) = save_ready_pending_clips(
                        buffer,
                        &mut pending_clips,
                        &mut cooldown,
                        Instant::now(),
                        player_name,
                        &auto_config,
                        post_event_delay,
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
                for event in events {
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
                    if reason == ClipReason::TargetDestroyed && !auto_config.target_destroyed_trigger
                    {
                        debug!(?event, "[CLIP] target_destroyed trigger disabled");
                        continue;
                    }
                    let now = Instant::now();
                    if let Some(pending) = pending_clips.first_mut() {
                        let within_window = now.duration_since(pending.last_event_time)
                            <= auto_config.multi_kill_window;
                        if within_window || now < pending.save_at {
                            pending.add_event(event, now, post_event_delay, summary);
                            println!(
                                "[CLIP] added event to pending clip, now {} kills; save delayed by {}s",
                                pending.kill_count(),
                                post_event_delay.as_secs()
                            );
                            continue;
                        }
                    }
                    if !cooldown.allows(now) {
                        debug!(?event, "[CLIP] event ignored due to cooldown");
                        continue;
                    }

                    let pending = schedule_pending_clip(
                        event,
                        reason,
                        summary,
                        now,
                        post_event_delay,
                    );
                    println!("[CLIP] scheduled replay save in {}s...", post_event_delay.as_secs());
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
    reason: ClipReason,
    description: String,
    event_time: Instant,
    delay: Duration,
) -> PendingClip {
    PendingClip::new(event, reason, event_time, delay, description)
}

fn clip_context_for_pending(
    pending: &PendingClip,
    player_name: Option<&str>,
    auto_config: &AutoClipConfig,
    post_event_delay: Duration,
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
        post_event_seconds: post_event_delay.as_secs(),
        segment_seconds: auto_config.segment_seconds,
    }
}

fn ready_pending_indices(pending_clips: &[PendingClip], now: Instant) -> Vec<usize> {
    pending_clips
        .iter()
        .enumerate()
        .filter_map(|(index, pending)| pending.is_ready(now).then_some(index))
        .collect()
}

async fn save_ready_pending_clips(
    buffer: &ReplayBufferHandle,
    pending_clips: &mut Vec<PendingClip>,
    cooldown: &mut Cooldown,
    now: Instant,
    player_name: Option<&str>,
    auto_config: &AutoClipConfig,
    post_event_delay: Duration,
) -> anyhow::Result<()> {
    let ready_indices = ready_pending_indices(pending_clips, now);
    for index in ready_indices.into_iter().rev() {
        let pending = pending_clips.remove(index);
        debug!(
            reason = ?pending.reason,
            event_age_ms = now.duration_since(pending.first_event_time).as_millis(),
            event_count = pending.events.len(),
            "saving pending auto-clip"
        );
        println!("[CLIP] saving replay...");
        let context =
            clip_context_for_pending(&pending, player_name, auto_config, post_event_delay);
        match buffer.save_replay(context).await? {
            Some(replay) => {
                crate::capture::buffer::print_saved_replay(&replay);
                if let Some(path) = replay.final_video_path.clone() {
                    let size_bytes = std::fs::metadata(&path)
                        .map(|metadata| metadata.len())
                        .unwrap_or(0);
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
            }
            None => println!("[CLIP] no finalized replay segments available yet"),
        }
        cooldown.record_save(now);
    }
    Ok(())
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
) -> (Vec<WarThunderEvent>, bool) {
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
                player_name,
                &mut events,
            );
            collect_personal_events(
                "hud:damage",
                hud.damage,
                &mut state.seen_messages,
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
    player_name: Option<&str>,
    events: &mut Vec<WarThunderEvent>,
) {
    for message in messages {
        let key = message.stable_key_with_prefix(source);
        if seen_messages.contains(&key) {
            continue;
        }
        seen_messages.insert(key);

        let event = parse_gamechat_event(&message.text);
        if is_personal_kill(&event, player_name) {
            events.push(event);
        } else {
            debug!(source, message = %message.text, ?event, "ignoring non-personal auto-clip event");
        }
    }
}

fn remember_messages(
    source: &str,
    messages: Vec<ChatMessage>,
    seen_messages: &mut RecentMessageCache,
) {
    for message in messages {
        seen_messages.insert(message.stable_key_with_prefix(source));
    }
}

fn clip_reason_for_event(event: &WarThunderEvent) -> ClipReason {
    match event {
        WarThunderEvent::TargetDestroyed { action, .. } if is_clip_action(action) => {
            ClipReason::TargetDestroyed
        }
        WarThunderEvent::TargetDestroyed { .. } => ClipReason::Unknown,
        WarThunderEvent::PlayerDestroyed { .. } => ClipReason::PlayerDestroyed,
        WarThunderEvent::Unknown(_) => ClipReason::Unknown,
        WarThunderEvent::CriticalHit { .. }
        | WarThunderEvent::SevereDamage { .. }
        | WarThunderEvent::BaseDestroyed { .. } => ClipReason::Unknown,
    }
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
        let _ = sender.send(event);
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
            Duration::from_secs(5)
        );
        assert_eq!(
            effective_post_event_delay(Duration::from_secs(1), 2),
            Duration::from_secs(3)
        );
    }

    #[test]
    fn kill_schedules_pending_clip() {
        let now = Instant::now();
        let pending = schedule_pending_clip(
            kill("dawson16800"),
            ClipReason::TargetDestroyed,
            "kill".to_owned(),
            now,
            Duration::from_secs(5),
        );

        assert_eq!(pending.reason, ClipReason::TargetDestroyed);
        assert_eq!(pending.first_event_time, now);
        assert_eq!(pending.last_event_time, now);
        assert_eq!(pending.save_at, now + Duration::from_secs(5));
        assert_eq!(pending.events.len(), 1);
        assert_eq!(pending.descriptions, vec!["kill"]);
    }

    #[test]
    fn pending_clip_is_not_ready_before_save_at() {
        let now = Instant::now();
        let pending = schedule_pending_clip(
            kill("dawson16800"),
            ClipReason::TargetDestroyed,
            "kill".to_owned(),
            now,
            Duration::from_secs(5),
        );

        assert!(!pending.is_ready(now + Duration::from_secs(4)));
        assert!(ready_pending_indices(&[pending], now + Duration::from_secs(4)).is_empty());
    }

    #[test]
    fn pending_clip_is_ready_after_save_at() {
        let now = Instant::now();
        let pending = schedule_pending_clip(
            kill("dawson16800"),
            ClipReason::TargetDestroyed,
            "kill".to_owned(),
            now,
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
            kill("dawson16800"),
            ClipReason::TargetDestroyed,
            "first".to_owned(),
            now,
            Duration::from_secs(5),
        );

        pending.add_event(
            kill("dawson16800"),
            now + Duration::from_secs(3),
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
            kill("dawson16800"),
            ClipReason::TargetDestroyed,
            "first".to_owned(),
            now,
            Duration::from_secs(5),
        );
        cooldown.record_save(now);

        pending.add_event(
            kill("dawson16800"),
            now + Duration::from_secs(1),
            Duration::from_secs(5),
            "second".to_owned(),
        );

        assert!(!cooldown.allows(now + Duration::from_secs(1)));
        assert_eq!(pending.events.len(), 2);
    }

    #[test]
    fn kill_after_saved_clip_can_create_new_pending_clip() {
        let mut cooldown = Cooldown::new(Duration::from_secs(3));
        let now = Instant::now();
        cooldown.record_save(now);
        let later = now + Duration::from_secs(4);

        let pending = cooldown.allows(later).then(|| {
            schedule_pending_clip(
                kill("dawson16800"),
                ClipReason::TargetDestroyed,
                "next".to_owned(),
                later,
                Duration::from_secs(5),
            )
        });

        assert!(pending.is_some());
        assert_eq!(pending.unwrap().save_at, later + Duration::from_secs(5));
    }

    #[test]
    fn pending_clip_can_be_dropped_on_shutdown_without_saving() {
        let mut pending = vec![schedule_pending_clip(
            kill("dawson16800"),
            ClipReason::TargetDestroyed,
            "kill".to_owned(),
            Instant::now(),
            Duration::from_secs(5),
        )];

        pending.clear();

        assert!(pending.is_empty());
    }

    #[test]
    fn auto_collects_personal_target_destroyed() {
        let mut seen = RecentMessageCache::new(100);
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
            Some("dawson16800"),
            &mut events,
        );

        assert_eq!(events.len(), 1);
        assert_eq!(
            clip_reason_for_event(&events[0]),
            ClipReason::TargetDestroyed
        );
    }

    #[test]
    fn auto_ignores_non_personal_event() {
        let mut seen = RecentMessageCache::new(100);
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
