use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WarThunderEvent {
    TargetDestroyed {
        attacker: Option<String>,
        action: String,
        vehicle: Option<String>,
        target: Option<String>,
        raw: String,
    },
    PlayerDestroyed {
        raw: String,
    },
    CriticalHit {
        raw: String,
    },
    SevereDamage {
        raw: String,
    },
    BaseDestroyed {
        raw: String,
    },
    Unknown(String),
}

impl WarThunderEvent {
    pub fn is_personal_kill(&self, player_name: &str) -> bool {
        match self {
            Self::TargetDestroyed {
                attacker: Some(attacker),
                action,
                ..
            } => is_kill_action(action) && attacker == player_name,
            _ => false,
        }
    }
}

fn is_kill_action(action: &str) -> bool {
    matches!(action, "destroyed" | "shot down")
}
