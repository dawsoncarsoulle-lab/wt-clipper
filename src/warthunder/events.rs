#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarThunderEvent {
    TargetDestroyed {
        attacker: Option<String>,
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
                raw,
                ..
            } => attacker == player_name || raw.contains(player_name),
            Self::TargetDestroyed { raw, .. } => raw.contains(player_name),
            _ => false,
        }
    }
}
