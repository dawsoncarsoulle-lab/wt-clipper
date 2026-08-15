use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::Mutex;
use tracing::debug;

use crate::{
    config::{TriggerConfig, WarThunderConfig},
    games::{
        event::{GameEvent, GameEventKind},
        source::{GamePollResult, GameSource},
    },
    warthunder::{
        client::{ChatMessage, WarThunderClient},
        events::WarThunderEvent,
        parser::parse_gamechat_event,
        recent::{RecentEventCache, RecentMessageCache},
    },
};

const EVENT_DEDUPE_TTL: Duration = Duration::from_secs(2);

/// War Thunder implementation of [`GameSource`].
///
/// Wraps the existing localhost API client and chat parser, translating
/// [`WarThunderEvent`]s into normalized [`GameEvent`]s. All polling cursors
/// and dedup caches are owned internally.
pub struct WarThunderSource {
    client: WarThunderClient,
    config: WarThunderConfig,
    triggers: TriggerConfig,
    state: Mutex<WarThunderSourceState>,
}

#[derive(Debug)]
struct WarThunderSourceState {
    last_chat_id: u64,
    last_evt_msg_id: u64,
    last_dmg_msg_id: u64,
    seen_messages: RecentMessageCache,
    seen_events: RecentEventCache,
}

impl WarThunderSource {
    pub fn new(
        client: WarThunderClient,
        config: WarThunderConfig,
        triggers: TriggerConfig,
    ) -> Self {
        Self {
            client,
            config,
            triggers,
            state: Mutex::new(WarThunderSourceState {
                last_chat_id: 0,
                last_evt_msg_id: 0,
                last_dmg_msg_id: 0,
                seen_messages: RecentMessageCache::new(1000),
                seen_events: RecentEventCache::new(EVENT_DEDUPE_TTL),
            }),
        }
    }

    /// Reference to the underlying client, used by CLI subcommands (status,
    /// dump, watch) that still operate on the raw War Thunder API.
    pub fn client(&self) -> &WarThunderClient {
        &self.client
    }

    /// Reference to the War Thunder config section.
    pub fn config(&self) -> &WarThunderConfig {
        &self.config
    }
}

#[async_trait]
impl GameSource for WarThunderSource {
    fn name(&self) -> &'static str {
        "War Thunder"
    }

    fn player_name(&self) -> Option<&str> {
        self.config.player_name.as_deref()
    }

    async fn bootstrap(&self, include_history: bool) {
        let mut state = self.state.lock().await;
        if include_history {
            return;
        }
        if let Ok(chat) = self.client.fetch_gamechat(0).await {
            state.last_chat_id = chat.next_last_id;
            remember_messages("gamechat", chat.messages, &mut state.seen_messages);
        }
        if let Ok(hud) = self.client.fetch_hudmsg(0, 0).await {
            state.last_evt_msg_id = hud.next_last_evt_id;
            state.last_dmg_msg_id = hud.next_last_dmg_id;
            remember_messages("hud:event", hud.events, &mut state.seen_messages);
            remember_messages("hud:damage", hud.damage, &mut state.seen_messages);
        }
    }

    async fn poll(&self) -> GamePollResult {
        let mut state = self.state.lock().await;
        let player_name = self.config.player_name.as_deref();
        let mut events = Vec::new();
        let mut successful_polls = 0usize;

        match self.client.fetch_gamechat(state.last_chat_id).await {
            Ok(chat) => {
                successful_polls += 1;
                state.last_chat_id = chat.next_last_id;
                collect_personal_events(
                    "gamechat",
                    chat.messages,
                    &mut state,
                    player_name,
                    &self.triggers,
                    &mut events,
                );
            }
            Err(error) => debug!(%error, "failed to poll gamechat for auto-clip"),
        }

        match self
            .client
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
                    &mut state,
                    player_name,
                    &self.triggers,
                    &mut events,
                );
                collect_personal_events(
                    "hud:damage",
                    hud.damage,
                    &mut state,
                    player_name,
                    &self.triggers,
                    &mut events,
                );
            }
            Err(error) => debug!(%error, "failed to poll hudmsg for auto-clip"),
        }

        GamePollResult {
            events,
            connected: successful_polls > 0,
        }
    }
}

fn collect_personal_events(
    source: &str,
    messages: Vec<ChatMessage>,
    state: &mut WarThunderSourceState,
    player_name: Option<&str>,
    triggers: &TriggerConfig,
    events: &mut Vec<GameEvent>,
) {
    for message in messages {
        let key = raw_message_dedupe_key(source, &message);
        if let Some(key) = key {
            if state.seen_messages.contains(&key) {
                debug!(source, raw_key = %key, ignored_duplicate = true, "duplicate raw message ignored");
                continue;
            }
            state.seen_messages.insert(key);
        }

        let wt_event = parse_gamechat_event(&message.text);
        let canonical_key = canonical_wt_event_key(&wt_event);
        let event_key = canonical_key
            .as_deref()
            .map(|canonical| event_dedupe_key(canonical, &message));
        if let Some(kind) = should_clip_wt_event(&wt_event, player_name, triggers) {
            let Some(event_key) = event_key else {
                debug!(source, ?wt_event, "clip event has no canonical key");
                continue;
            };
            let now = Instant::now();
            if !state.seen_events.insert_new(event_key.clone(), now) {
                debug!(
                    source,
                    event_key = %event_key,
                    ignored_duplicate = true,
                    "duplicate canonical event ignored"
                );
                continue;
            }
            let game_time = parse_wt_message_time(message.time.as_deref());
            events.push(wt_event_to_game_event(wt_event, kind, event_key, game_time));
        } else {
            debug!(source, message = %message.text, ?wt_event, "ignoring disabled or non-matching auto-clip event");
        }
    }
}

/// Translate a parsed War Thunder event into a normalized [`GameEvent`].
fn wt_event_to_game_event(
    event: WarThunderEvent,
    kind: GameEventKind,
    canonical_key: String,
    game_time: Option<Duration>,
) -> GameEvent {
    let summary = wt_event_summary(&event);
    let (actor, subject, context) = match &event {
        WarThunderEvent::TargetDestroyed {
            attacker,
            vehicle,
            target,
            ..
        } => (attacker.clone(), target.clone(), vehicle.clone()),
        _ => (None, None, None),
    };
    let mut game_event = GameEvent::new(kind, summary, canonical_key);
    game_event.actor = actor;
    game_event.subject = subject;
    game_event.context = context;
    // Stash game time on the event is not possible on the generic type; the
    // runtime re-derives it via GameSource::event_game_time if needed. WT does
    // not expose a stable per-event time on the GameEvent itself, so we rely on
    // host wall time for grouping. (game_time is consumed by the source only.)
    let _ = game_time;
    game_event
}

fn wt_event_summary(event: &WarThunderEvent) -> String {
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

/// Returns the clip kind for an event, or `None` if it should be ignored.
pub(crate) fn should_clip_wt_event(
    event: &WarThunderEvent,
    player_name: Option<&str>,
    triggers: &TriggerConfig,
) -> Option<GameEventKind> {
    let player_name = player_name.map(str::trim).filter(|name| !name.is_empty());

    if triggers.player_destroyed && is_player_destroyed_event(event, player_name) {
        return Some(GameEventKind::Death);
    }

    if triggers.base_destroyed && is_base_destroyed_event(event) {
        return Some(GameEventKind::Objective);
    }

    if triggers.target_destroyed && is_target_destroyed_event(event, player_name) {
        return Some(GameEventKind::Kill);
    }

    None
}

fn canonical_wt_event_key(event: &WarThunderEvent) -> Option<String> {
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

fn is_clip_action(action: &str) -> bool {
    matches!(action, "destroyed" | "shot down")
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

fn normalize_key_part(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn raw_message_dedupe_key(source: &str, message: &ChatMessage) -> Option<String> {
    Some(match message.id {
        Some(id) => format!("{source}:{id}"),
        None => message.stable_key_with_prefix(source),
    })
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

    fn base_destroyed() -> WarThunderEvent {
        WarThunderEvent::TargetDestroyed {
            attacker: Some("dawson16800".to_owned()),
            action: "destroyed".to_owned(),
            vehicle: Some("F/A-18C Early".to_owned()),
            target: Some("a base".to_owned()),
            raw: "dawson16800 (F/A-18C Early) destroyed a base".to_owned(),
        }
    }

    #[test]
    fn kill_translates_to_game_event() {
        let kind = should_clip_wt_event(
            &kill("dawson16800"),
            Some("dawson16800"),
            &TriggerConfig::default(),
        );
        assert_eq!(kind, Some(GameEventKind::Kill));
    }

    #[test]
    fn base_destroyed_translates_to_objective() {
        let kind = should_clip_wt_event(
            &base_destroyed(),
            Some("dawson16800"),
            &TriggerConfig::default(),
        );
        assert_eq!(kind, Some(GameEventKind::Objective));
    }

    #[test]
    fn wt_event_to_game_event_preserves_actor_and_subject() {
        let event = kill("dawson16800");
        let game_event = wt_event_to_game_event(event, GameEventKind::Kill, "key".to_owned(), None);
        assert_eq!(game_event.actor.as_deref(), Some("dawson16800"));
        assert_eq!(game_event.subject.as_deref(), Some("[ai] MiG-15bis"));
        assert_eq!(game_event.context.as_deref(), Some("F/A-18C Early"));
        assert_eq!(game_event.kind, GameEventKind::Kill);
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
}
