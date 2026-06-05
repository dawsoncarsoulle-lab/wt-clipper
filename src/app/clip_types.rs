use std::{path::PathBuf, time::SystemTime};

use serde::{Deserialize, Serialize};

use crate::warthunder::events::WarThunderEvent;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ClipReason {
    TargetDestroyed,
    BaseDestroyed,
    PlayerDestroyed,
    MultiKill,
    Manual,
    Unknown,
}

impl ClipReason {
    pub fn slug(self) -> &'static str {
        match self {
            Self::TargetDestroyed => "target-destroyed",
            Self::BaseDestroyed => "base-destroyed",
            Self::PlayerDestroyed => "player-destroyed",
            Self::MultiKill => "multi-kill",
            Self::Manual => "manual",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClipContext {
    pub reason: ClipReason,
    pub event: Option<WarThunderEvent>,
    pub events: Vec<WarThunderEvent>,
    pub player_name: Option<String>,
    pub pending_clip_id: Option<String>,
    pub pending_dedupe_key: Option<String>,
    pub duration_seconds: u64,
    pub post_event_seconds: u64,
    pub first_event_time: Option<SystemTime>,
    pub last_event_time: Option<SystemTime>,
}

#[derive(Debug, Clone)]
pub struct SavedReplay {
    pub final_video_path: Option<PathBuf>,
    pub metadata_path: Option<PathBuf>,
    pub segments_dir: Option<PathBuf>,
}
