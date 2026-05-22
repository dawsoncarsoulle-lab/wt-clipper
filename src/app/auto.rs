use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, info};

use crate::{
    capture::{
        buffer::{ClipContext, ClipReason, ReplayBufferConfig, ReplayBufferHandle},
        quality::{QualityPreset, VideoQuality},
    },
    cli::CaptureSource,
    config::WarThunderConfig,
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
    pub include_history: bool,
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
    last_clip: Option<Instant>,
}

#[derive(Debug, Clone)]
struct PendingClip {
    context: ClipContext,
    event_time: Instant,
    save_at: Instant,
    description: String,
}

impl PendingClip {
    fn new(
        context: ClipContext,
        event_time: Instant,
        delay: Duration,
        description: String,
    ) -> Self {
        Self {
            context,
            event_time,
            save_at: event_time + delay,
            description,
        }
    }

    fn is_ready(&self, now: Instant) -> bool {
        now >= self.save_at
    }
}

impl Cooldown {
    fn new(duration: Duration) -> Self {
        Self {
            duration,
            last_clip: None,
        }
    }

    fn allows(&mut self, now: Instant) -> bool {
        match self.last_clip {
            Some(last_clip) if now.duration_since(last_clip) < self.duration => false,
            _ => {
                self.last_clip = Some(now);
                true
            }
        }
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
                    if let Err(error) = save_ready_pending_clips(buffer, &mut pending_clips, Instant::now()).await {
                        break Err(error);
                    }
                }
            }
            _ = wt_tick.tick() => {
                let events = poll_personal_events(&client, &mut state, player_name).await;
                for event in events {
                    let summary = event_summary(&event);
                    println!("[WT] kill detected: {summary}");
                    if !cooldown.allows(Instant::now()) {
                        debug!(?event, "[CLIP] event ignored due to cooldown");
                        continue;
                    }

                    let pending = schedule_pending_clip(
                        clip_context_for_event(
                            &event,
                            player_name,
                            &auto_config,
                            post_event_delay,
                        ),
                        summary,
                        Instant::now(),
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
    context: ClipContext,
    description: String,
    event_time: Instant,
    delay: Duration,
) -> PendingClip {
    PendingClip::new(context, event_time, delay, description)
}

fn clip_context_for_event(
    event: &WarThunderEvent,
    player_name: Option<&str>,
    auto_config: &AutoClipConfig,
    post_event_delay: Duration,
) -> ClipContext {
    ClipContext {
        reason: clip_reason_for_event(event),
        event: Some(event.clone()),
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
    now: Instant,
) -> anyhow::Result<()> {
    let ready_indices = ready_pending_indices(pending_clips, now);
    for index in ready_indices.into_iter().rev() {
        let pending = pending_clips.remove(index);
        debug!(
            reason = ?pending.context.reason,
            event_age_ms = now.duration_since(pending.event_time).as_millis(),
            description = %pending.description,
            "saving pending auto-clip"
        );
        println!("[CLIP] saving replay...");
        match buffer.save_replay(pending.context).await? {
            Some(replay) => crate::capture::buffer::print_saved_replay(&replay),
            None => println!("[CLIP] no finalized replay segments available yet"),
        }
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
) -> Vec<WarThunderEvent> {
    let mut events = Vec::new();

    match client.fetch_gamechat(state.last_chat_id).await {
        Ok(chat) => {
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

    events
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

    fn test_clip_context() -> ClipContext {
        ClipContext {
            reason: ClipReason::TargetDestroyed,
            event: Some(kill("dawson16800")),
            player_name: Some("dawson16800".to_owned()),
            video_quality: VideoQuality::default(),
            quality_preset: QualityPreset::High,
            duration_seconds: 20,
            post_event_seconds: 5,
            segment_seconds: 2,
        }
    }

    #[test]
    fn cooldown_blocks_close_events() {
        let mut cooldown = Cooldown::new(Duration::from_secs(3));
        let now = Instant::now();

        assert!(cooldown.allows(now));
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
            test_clip_context(),
            "kill".to_owned(),
            now,
            Duration::from_secs(5),
        );

        assert_eq!(pending.context.reason, ClipReason::TargetDestroyed);
        assert_eq!(pending.event_time, now);
        assert_eq!(pending.save_at, now + Duration::from_secs(5));
        assert_eq!(pending.description, "kill");
    }

    #[test]
    fn pending_clip_is_not_ready_before_save_at() {
        let now = Instant::now();
        let pending = schedule_pending_clip(
            test_clip_context(),
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
            test_clip_context(),
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
    fn cooldown_prevents_too_many_pending_clips() {
        let mut cooldown = Cooldown::new(Duration::from_secs(3));
        let now = Instant::now();
        let mut pending = Vec::new();

        if cooldown.allows(now) {
            pending.push(schedule_pending_clip(
                test_clip_context(),
                "first".to_owned(),
                now,
                Duration::from_secs(5),
            ));
        }
        if cooldown.allows(now + Duration::from_secs(1)) {
            pending.push(schedule_pending_clip(
                test_clip_context(),
                "second".to_owned(),
                now + Duration::from_secs(1),
                Duration::from_secs(5),
            ));
        }

        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn pending_clip_can_be_dropped_on_shutdown_without_saving() {
        let mut pending = vec![schedule_pending_clip(
            test_clip_context(),
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
