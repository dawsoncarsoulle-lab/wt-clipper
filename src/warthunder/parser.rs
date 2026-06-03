use crate::warthunder::events::WarThunderEvent;

pub fn parse_gamechat_event(message: &str) -> WarThunderEvent {
    let raw = message.trim().to_owned();
    let normalized = normalize(&raw);

    if let Some(event) = parse_combat_action_message(&raw) {
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

const COMBAT_ACTIONS: &[&str] = &[
    "critically damaged",
    "severely damaged",
    "has crashed",
    "destroyed",
    "shot down",
    "set afire",
];

fn parse_combat_action_message(raw: &str) -> Option<WarThunderEvent> {
    let (action, action_start, action_end) = find_combat_action(raw)?;
    let attacker_part = raw[..action_start].trim();
    let target = raw[action_end..].trim();

    if attacker_part.is_empty() || (target.is_empty() && action != "has crashed") {
        return None;
    }

    let (attacker, vehicle) = parse_attacker_and_vehicle(attacker_part);

    Some(WarThunderEvent::TargetDestroyed {
        attacker,
        action: action.to_owned(),
        vehicle,
        target: non_empty_string(target),
        raw: raw.to_owned(),
    })
}

fn find_combat_action(raw: &str) -> Option<(&'static str, usize, usize)> {
    COMBAT_ACTIONS
        .iter()
        .filter_map(|action| {
            let needle = format!(" {action}");
            raw.find(&needle).and_then(|start| {
                let action_start = start + 1;
                let action_end = action_start + action.len();
                let attacker_part = raw[..action_start].trim();
                if attacker_part.ends_with(')')
                    && raw[action_end..]
                        .chars()
                        .next()
                        .is_none_or(char::is_whitespace)
                {
                    Some((*action, action_start, action_end))
                } else {
                    None
                }
            })
        })
        .min_by_key(|(_, start, _)| *start)
}

fn parse_attacker_and_vehicle(attacker_part: &str) -> (Option<String>, Option<String>) {
    let input = attacker_part.trim();

    if !input.ends_with(')') {
        return (non_empty_string(input), None);
    }

    let mut depth = 0usize;
    let mut matching_open = None;

    for (index, ch) in input.char_indices().rev() {
        match ch {
            ')' => depth += 1,
            '(' => {
                if depth == 0 {
                    continue;
                }

                depth -= 1;

                if depth == 0 {
                    matching_open = Some(index);
                    break;
                }
            }
            _ => {}
        }
    }

    let Some(open_paren) = matching_open else {
        return (non_empty_string(input), None);
    };

    // Require the actor/vehicle separator to be " ("
    // so we do not split weird malformed strings.
    if open_paren == 0 || !input[..open_paren].ends_with(' ') {
        return (non_empty_string(input), None);
    }

    let close_paren = input.len() - 1;
    let attacker = input[..open_paren].trim();
    let vehicle = input[open_paren + 1..close_paren].trim();

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
    const SAMPLE_AIR_KILL: &str =
        "dawson16800 (F/A-18C Early) shot down =3BEHO= BoBka_V (MiG-21bis)";

    #[test]
    fn parses_gamechat_kill() {
        assert_eq!(
            parse_gamechat_event(SAMPLE_KILL),
            WarThunderEvent::TargetDestroyed {
                attacker: Some("dawson16800".to_owned()),
                action: "destroyed".to_owned(),
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
    fn parses_real_air_kill_with_shot_down_action() {
        assert_eq!(
            parse_gamechat_event(SAMPLE_AIR_KILL),
            WarThunderEvent::TargetDestroyed {
                attacker: Some("dawson16800".to_owned()),
                action: "shot down".to_owned(),
                vehicle: Some("F/A-18C Early".to_owned()),
                target: Some("=3BEHO= BoBka_V (MiG-21bis)".to_owned()),
                raw: SAMPLE_AIR_KILL.to_owned(),
            }
        );
    }

    #[test]
    fn parses_ground_kill_with_symbol_vehicle() {
        let raw = "dawson16800 (◍M1A1 HC) destroyed IT-1";

        assert_eq!(
            parse_gamechat_event(raw),
            WarThunderEvent::TargetDestroyed {
                attacker: Some("dawson16800".to_owned()),
                action: "destroyed".to_owned(),
                vehicle: Some("◍M1A1 HC".to_owned()),
                target: Some("IT-1".to_owned()),
                raw: raw.to_owned(),
            }
        );
    }

    #[test]
    fn parses_player_death_without_treating_it_as_personal_kill() {
        let raw = "=TR3TA= QSR matejslay9 (JA37DI) shot down dawson16800 (F/A-18C Early)";
        let event = parse_gamechat_event(raw);

        assert_eq!(
            event,
            WarThunderEvent::TargetDestroyed {
                attacker: Some("=TR3TA= QSR matejslay9".to_owned()),
                action: "shot down".to_owned(),
                vehicle: Some("JA37DI".to_owned()),
                target: Some("dawson16800 (F/A-18C Early)".to_owned()),
                raw: raw.to_owned(),
            }
        );
        assert!(!is_personal_kill(&event, Some("dawson16800")));
    }

    #[test]
    fn parses_personal_damage_but_does_not_make_it_a_personal_kill() {
        for action in ["severely damaged", "critically damaged", "set afire"] {
            let raw = format!("dawson16800 (F/A-18C Early) {action} =3BEHO= BoBka_V (MiG-21bis)");
            let event = parse_gamechat_event(&raw);

            let WarThunderEvent::TargetDestroyed {
                attacker,
                action: parsed_action,
                vehicle,
                target,
                ..
            } = &event
            else {
                panic!("expected TargetDestroyed for {action}");
            };

            assert_eq!(attacker.as_deref(), Some("dawson16800"));
            assert_eq!(parsed_action, action);
            assert_eq!(vehicle.as_deref(), Some("F/A-18C Early"));
            assert_eq!(target.as_deref(), Some("=3BEHO= BoBka_V (MiG-21bis)"));
            assert!(!is_personal_kill(&event, Some("dawson16800")));
        }
    }

    #[test]
    fn parses_crash_action_without_target() {
        let raw = "dawson16800 (F/A-18C Early) has crashed";

        assert_eq!(
            parse_gamechat_event(raw),
            WarThunderEvent::TargetDestroyed {
                attacker: Some("dawson16800".to_owned()),
                action: "has crashed".to_owned(),
                vehicle: Some("F/A-18C Early".to_owned()),
                target: None,
                raw: raw.to_owned(),
            }
        );
    }

    #[test]
    fn preserves_spaced_and_symbolic_names() {
        let raw = "某 玩家 =ABC= Long Name (F/A-18C Early) shot down =3BEHO= BoBka_V (MiG-21bis)";

        assert_eq!(
            parse_gamechat_event(raw),
            WarThunderEvent::TargetDestroyed {
                attacker: Some("某 玩家 =ABC= Long Name".to_owned()),
                action: "shot down".to_owned(),
                vehicle: Some("F/A-18C Early".to_owned()),
                target: Some("=3BEHO= BoBka_V (MiG-21bis)".to_owned()),
                raw: raw.to_owned(),
            }
        );
    }

    #[test]
    fn personal_kill_requires_matching_player_name() {
        let event = parse_gamechat_event(SAMPLE_KILL);

        assert!(is_personal_kill(&event, Some("dawson16800")));
        assert!(!is_personal_kill(&event, Some("other_player")));
        assert!(!is_personal_kill(&event, None));
    }

    #[test]
    fn personal_kill_accepts_shot_down() {
        let event = parse_gamechat_event(SAMPLE_AIR_KILL);

        assert!(is_personal_kill(&event, Some("dawson16800")));
    }

    #[test]
    fn non_personal_target_destroyed_is_not_personal_kill() {
        let event = parse_gamechat_event("other (MiG-15bis) destroyed [ai] F-86");

        assert!(!is_personal_kill(&event, Some("dawson16800")));
    }

    #[test]
    fn enemy_shot_down_player_name_parses_as_target_destroyed_for_death_decision() {
        let raw = "Enemy (MiG-29) shot down dawson16800 (F/A-18C Early)";

        assert_eq!(
            parse_gamechat_event(raw),
            WarThunderEvent::TargetDestroyed {
                attacker: Some("Enemy".to_owned()),
                action: "shot down".to_owned(),
                vehicle: Some("MiG-29".to_owned()),
                target: Some("dawson16800 (F/A-18C Early)".to_owned()),
                raw: raw.to_owned(),
            }
        );
    }

    #[test]
    fn player_destroyed_a_base_parses_as_destroyed_action() {
        let raw = "dawson16800 (F/A-18C Early) destroyed a base";

        assert_eq!(
            parse_gamechat_event(raw),
            WarThunderEvent::TargetDestroyed {
                attacker: Some("dawson16800".to_owned()),
                action: "destroyed".to_owned(),
                vehicle: Some("F/A-18C Early".to_owned()),
                target: Some("a base".to_owned()),
                raw: raw.to_owned(),
            }
        );
    }

    #[test]
    fn damage_messages_are_not_destroyed_events() {
        for raw in [
            "dawson16800 (F/A-18C Early) critically damaged Enemy",
            "dawson16800 (F/A-18C Early) severely damaged Enemy",
            "dawson16800 (F/A-18C Early) set afire Enemy",
        ] {
            let event = parse_gamechat_event(raw);
            assert!(!is_personal_kill(&event, Some("dawson16800")));
        }
    }

    #[test]
    fn unknown_when_message_is_not_recognized() {
        assert_eq!(
            parse_gamechat_event("Capture the point"),
            WarThunderEvent::Unknown("Capture the point".to_owned())
        );
    }

    #[test]
    fn parses_ground_kill_with_nested_vehicle_parentheses() {
        let raw = "dawson16800 (Leopard 2 (OTCo)) destroyed IT-1";

        assert_eq!(
            parse_gamechat_event(raw),
            WarThunderEvent::TargetDestroyed {
                attacker: Some("dawson16800".to_owned()),
                action: "destroyed".to_owned(),
                vehicle: Some("Leopard 2 (OTCo)".to_owned()),
                target: Some("IT-1".to_owned()),
                raw: raw.to_owned(),
            }
        );

        assert!(is_personal_kill(
            &parse_gamechat_event(raw),
            Some("dawson16800")
        ));
    }

    #[test]
    fn parses_ground_kill_with_target_parentheses() {
        let raw = "dawson16800 (Leopard 2 (OTCo)) destroyed T-64A (1971)";

        assert_eq!(
            parse_gamechat_event(raw),
            WarThunderEvent::TargetDestroyed {
                attacker: Some("dawson16800".to_owned()),
                action: "destroyed".to_owned(),
                vehicle: Some("Leopard 2 (OTCo)".to_owned()),
                target: Some("T-64A (1971)".to_owned()),
                raw: raw.to_owned(),
            }
        );

        assert!(is_personal_kill(
            &parse_gamechat_event(raw),
            Some("dawson16800")
        ));
    }
}
