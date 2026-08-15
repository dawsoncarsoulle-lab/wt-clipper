use std::time::{Duration, SystemTime};

use async_trait::async_trait;

use crate::games::event::GameEvent;

/// Result of a single poll iteration.
#[derive(Debug, Clone, Default)]
pub struct GamePollResult {
    /// New events detected during this poll, in detection order.
    pub events: Vec<GameEvent>,
    /// Whether the game's local API was reachable during this poll.
    /// Drives the connected/disconnected status surfaced to the UI.
    pub connected: bool,
}

/// A game-agnostic source of clipping events.
///
/// Implementations wrap a game's local API client and parser, translating raw
/// responses into normalized [`GameEvent`]s. The auto-clip runtime only ever
/// interacts with a source through this trait, so adding a new game does not
/// require changes to `auto.rs`.
///
/// State (cursors, dedup caches) is owned internally by the implementation,
/// which is responsible for thread-safe access since `poll` takes `&self`.
#[async_trait]
pub trait GameSource: Send + Sync {
    /// Human-readable game name for logs and diagnostics (e.g. "War Thunder").
    fn name(&self) -> &'static str;

    /// The player identity used for personal-event detection, if configured.
    fn player_name(&self) -> Option<&str>;

    /// Initialize cursors so that only events occurring *after* startup are
    /// processed, unless `include_history` is true.
    ///
    /// Called once at startup. Safe to call again on a restart.
    async fn bootstrap(&self, include_history: bool);

    /// Poll for new events.
    ///
    /// Implementations own their cursors internally and return newly detected
    /// events plus a connectivity flag. Must not block longer than the
    /// runtime's poll interval.
    async fn poll(&self) -> GamePollResult;
}

/// A detected event enriched with host timing metadata.
#[derive(Debug, Clone)]
pub struct DetectedEvent {
    pub event: GameEvent,
    pub detected_at: std::time::Instant,
    pub detected_wall_time: SystemTime,
    pub game_time: Option<Duration>,
}

impl DetectedEvent {
    pub fn new(event: GameEvent, game_time: Option<Duration>) -> Self {
        Self {
            event,
            detected_at: std::time::Instant::now(),
            detected_wall_time: SystemTime::now(),
            game_time,
        }
    }
}
