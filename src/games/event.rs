use serde::{Deserialize, Serialize};

/// A normalized, game-agnostic event produced by a [`crate::games::source::GameSource`].
///
/// Each game (War Thunder, Dota 2, ...) is responsible for translating its own
/// raw messages into this generic shape so that the auto-clip runtime never
/// needs to know which game it is watching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameEvent {
    /// High-level classification used to decide clipping and grouping.
    pub kind: GameEventKind,
    /// Human-readable summary shown in the UI and saved metadata.
    pub summary: String,
    /// A short label for the actor responsible (attacker / killer / scorer).
    pub actor: Option<String>,
    /// A short label for the subject (target / victim / objective).
    pub subject: Option<String>,
    /// Optional vehicle/character/ability context (e.g. "F/A-18C Early").
    pub context: Option<String>,
    /// A canonical, normalized key used for cross-source deduplication.
    /// Two events that represent the same game moment must share this key.
    pub canonical_key: String,
}

impl GameEvent {
    pub fn new(kind: GameEventKind, summary: String, canonical_key: String) -> Self {
        Self {
            kind,
            summary,
            actor: None,
            subject: None,
            context: None,
            canonical_key,
        }
    }
}

/// High-level event categories the auto-clipper reacts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameEventKind {
    /// A personal kill credited to the configured player.
    Kill,
    /// The configured player was destroyed / killed.
    Death,
    /// An objective destroyed (base, capture point, Roshan, etc.).
    Objective,
    /// Multi-kill grouping hint emitted by sources that batch kills.
    MultiKill,
    /// Any other detected event the source wants to surface.
    Other,
}

/// Lightweight summary used by the UI bridge without leaking game-specific types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameEventSummary {
    pub kind: GameEventKind,
    pub actor: Option<String>,
    pub subject: Option<String>,
    pub summary: String,
}

impl From<&GameEvent> for GameEventSummary {
    fn from(event: &GameEvent) -> Self {
        Self {
            kind: event.kind,
            actor: event.actor.clone(),
            subject: event.subject.clone(),
            summary: event.summary.clone(),
        }
    }
}
