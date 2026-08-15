pub mod event;
pub mod source;

pub use event::{GameEvent, GameEventKind, GameEventSummary};
pub use source::{DetectedEvent, GamePollResult, GameSource};
