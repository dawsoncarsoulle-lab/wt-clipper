use crate::warthunder::events::WarThunderEvent;

pub fn parse_gamechat_event(message: &str) -> WarThunderEvent {
    let raw = message.trim().to_owned();
    let normalized = normalize(&raw);

    if let Some(event) = parse_destroyed_message(&raw) {
        return event;
    }

    if normalized.contains("critical hit") {
        WarThunderEvent::CriticalHit { raw }
    } else if normalized.contains("severe damage") {
        WarThunderEvent::SevereDamage { raw }
    } else if contains_any(&normalized, &["base destroyed", "enemy base destroyed"]) {
        WarThunderEvent::BaseDestroyed { raw }
    } else if contains_any(
        &normalized,
        &[
            "you have been destroyed",
            "vehicle destroyed",
            "player destroyed",
        ],
    ) {
        WarThunderEvent::PlayerDestroyed { raw }
    } else {
        WarThunderEvent::Unknown(raw)
    }
}

pub fn is_personal_kill(event: &WarThunderEvent, player_name: Option<&str>) -> bool {
    let Some(player_name) = player_name.filter(|name| !name.trim().is_empty()) else {
        return false;
    };

    event.is_personal_kill(player_name)
}

fn parse_destroyed_message(raw: &str) -> Option<WarThunderEvent> {
    let destroyed_at = raw.find(" destroyed ")?;
    let attacker_part = raw[..destroyed_at].trim();
    let target = raw[destroyed_at + " destroyed ".len()..].trim();

    if attacker_part.is_empty() || target.is_empty() {
        return None;
    }

    let (attacker, vehicle) = parse_attacker_and_vehicle(attacker_part);

    Some(WarThunderEvent::TargetDestroyed {
        attacker,
        vehicle,
        target: Some(target.to_owned()),
        raw: raw.to_owned(),
    })
}

fn parse_attacker_and_vehicle(attacker_part: &str) -> (Option<String>, Option<String>) {
    let Some(open_paren) = attacker_part.rfind(" (") else {
        return (Some(attacker_part.to_owned()), None);
    };
    let Some(close_paren) = attacker_part.rfind(')') else {
        return (Some(attacker_part.to_owned()), None);
    };

    if close_paren <= open_paren {
        return (Some(attacker_part.to_owned()), None);
    }

    let attacker = attacker_part[..open_paren].trim();
    let vehicle = attacker_part[open_paren + 2..close_paren].trim();

    (non_empty_string(attacker), non_empty_string(vehicle))
}

fn non_empty_string(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

fn normalize(message: &str) -> String {
    message
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_KILL: &str = "dawson16800 (F/A-18C Early) destroyed [ai] MiG-15bis";

    #[test]
    fn parses_gamechat_kill() {
        assert_eq!(
            parse_gamechat_event(SAMPLE_KILL),
            WarThunderEvent::TargetDestroyed {
                attacker: Some("dawson16800".to_owned()),
                vehicle: Some("F/A-18C Early".to_owned()),
                target: Some("[ai] MiG-15bis".to_owned()),
                raw: SAMPLE_KILL.to_owned(),
            }
        );
    }

    #[test]
    fn extracts_attacker() {
        let WarThunderEvent::TargetDestroyed { attacker, .. } = parse_gamechat_event(SAMPLE_KILL)
        else {
            panic!("expected TargetDestroyed");
        };

        assert_eq!(attacker.as_deref(), Some("dawson16800"));
    }

    #[test]
    fn extracts_vehicle() {
        let WarThunderEvent::TargetDestroyed { vehicle, .. } = parse_gamechat_event(SAMPLE_KILL)
        else {
            panic!("expected TargetDestroyed");
        };

        assert_eq!(vehicle.as_deref(), Some("F/A-18C Early"));
    }

    #[test]
    fn extracts_target() {
        let WarThunderEvent::TargetDestroyed { target, .. } = parse_gamechat_event(SAMPLE_KILL)
        else {
            panic!("expected TargetDestroyed");
        };

        assert_eq!(target.as_deref(), Some("[ai] MiG-15bis"));
    }

    #[test]
    fn personal_kill_requires_matching_player_name() {
        let event = parse_gamechat_event(SAMPLE_KILL);

        assert!(is_personal_kill(&event, Some("dawson16800")));
        assert!(!is_personal_kill(&event, Some("other_player")));
        assert!(!is_personal_kill(&event, None));
    }

    #[test]
    fn unknown_when_message_is_not_recognized() {
        assert_eq!(
            parse_gamechat_event("Capture the point"),
            WarThunderEvent::Unknown("Capture the point".to_owned())
        );
    }
}
