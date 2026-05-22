use reqwest::{Client, Url};
use serde_json::{Map, Value};

use crate::config::WarThunderConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Endpoint {
    State,
    Indicators,
    HudMsg,
    GameChat,
    MapInfo,
    MapObj,
}

#[derive(Debug)]
pub enum EndpointProbe {
    Ok {
        endpoint: Endpoint,
        summary: Option<String>,
    },
    Failed {
        endpoint: Endpoint,
        error: String,
    },
}

pub struct WarThunderClient {
    base_url: Url,
    http: Client,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub id: Option<u64>,
    pub time: Option<String>,
    pub sender: Option<String>,
    pub text: String,
}

#[derive(Debug)]
pub struct ChatPoll {
    pub messages: Vec<ChatMessage>,
    pub next_last_id: u64,
}

#[derive(Debug)]
pub struct HudPoll {
    pub events: Vec<ChatMessage>,
    pub damage: Vec<ChatMessage>,
    pub next_last_evt_id: u64,
    pub next_last_dmg_id: u64,
}

impl ChatMessage {
    pub fn stable_key_with_prefix(&self, prefix: &str) -> String {
        if let Some(id) = self.id {
            return format!("{prefix}:{id}");
        }

        match (&self.time, &self.sender) {
            (Some(time), Some(sender)) => format!("{prefix}:{}|{}|{}", time, sender, self.text),
            (Some(time), None) => format!("{prefix}:{}|{}", time, self.text),
            (None, Some(sender)) => format!("{prefix}:{}|{}", sender, self.text),
            (None, None) => format!("{prefix}:{}", self.text),
        }
    }
}

impl WarThunderClient {
    pub fn new(config: WarThunderConfig) -> anyhow::Result<Self> {
        let base_url = Url::parse(&config.base_url)?;
        let http = Client::builder()
            .timeout(config.request_timeout())
            .user_agent("wt-clipper/0.1")
            .build()?;

        Ok(Self { base_url, http })
    }

    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    pub async fn probe_all(&self) -> Vec<EndpointProbe> {
        let mut probes = Vec::new();

        for endpoint in Endpoint::all() {
            match self.fetch_endpoint_json(endpoint).await {
                Ok(json) => probes.push(EndpointProbe::Ok {
                    endpoint,
                    summary: summarize_json(&json),
                }),
                Err(error) => probes.push(EndpointProbe::Failed {
                    endpoint,
                    error: friendly_error(&error),
                }),
            }
        }

        probes
    }

    pub async fn state_summary(&self) -> anyhow::Result<Option<String>> {
        let state = match self.fetch_json(Endpoint::State).await {
            Ok(state) => state,
            Err(error) if error.is_connect() => return Ok(None),
            Err(error) if error.is_timeout() => return Ok(None),
            Err(error) => return Err(error.into()),
        };

        let Some(object) = state.as_object() else {
            return Ok(summarize_json(&state));
        };

        let mut fields = Vec::new();
        for key in ["valid", "game_time", "speed", "altitude", "fuel", "army"] {
            if let Some(value) = object.get(key) {
                fields.push(format!("{key}={}", compact_value(value)));
            }
        }

        if fields.is_empty() {
            Ok(summarize_json(&state))
        } else {
            Ok(Some(fields.join(", ")))
        }
    }

    pub async fn fetch_json(&self, endpoint: Endpoint) -> Result<Value, reqwest::Error> {
        let url = self.url(endpoint.path());
        let response = self.http.get(url).send().await?.error_for_status()?;
        response.json::<Value>().await
    }

    pub async fn fetch_endpoint_json(&self, endpoint: Endpoint) -> Result<Value, reqwest::Error> {
        let url = self.url(endpoint.default_request_path());
        let response = self.http.get(url).send().await?.error_for_status()?;
        response.json::<Value>().await
    }

    pub async fn fetch_raw(&self, path_and_query: &str) -> Result<String, reqwest::Error> {
        let url = self.url(path_and_query);
        let response = self.http.get(url).send().await?.error_for_status()?;
        response.text().await
    }

    pub async fn fetch_gamechat(&self, last_id: u64) -> Result<ChatPoll, reqwest::Error> {
        let path = format!("/gamechat?lastId={last_id}");
        let json = self.fetch_path_json(&path).await?;
        let messages = extract_chat_messages(&json);
        let next_last_id = update_last_chat_id(last_id, &json, &messages);

        Ok(ChatPoll {
            messages,
            next_last_id,
        })
    }

    pub async fn fetch_hudmsg(
        &self,
        last_evt_id: u64,
        last_dmg_id: u64,
    ) -> Result<HudPoll, reqwest::Error> {
        let path = format!("/hudmsg?lastEvt={last_evt_id}&lastDmg={last_dmg_id}");
        let json = self.fetch_path_json(&path).await?;

        let events = json
            .get("events")
            .map(extract_chat_messages)
            .unwrap_or_default();
        let damage = json
            .get("damage")
            .map(extract_chat_messages)
            .unwrap_or_default();

        Ok(HudPoll {
            next_last_evt_id: update_last_chat_id(last_evt_id, &Value::Null, &events),
            next_last_dmg_id: update_last_chat_id(last_dmg_id, &Value::Null, &damage),
            events,
            damage,
        })
    }

    async fn fetch_path_json(&self, path_and_query: &str) -> Result<Value, reqwest::Error> {
        let url = self.url(path_and_query);
        let response = self.http.get(url).send().await?.error_for_status()?;
        response.json::<Value>().await
    }

    fn url(&self, path_and_query: &str) -> Url {
        self.base_url
            .join(path_and_query)
            .expect("valid War Thunder endpoint path")
    }
}

impl Endpoint {
    pub fn path(self) -> &'static str {
        match self {
            Endpoint::State => "/state",
            Endpoint::Indicators => "/indicators",
            Endpoint::HudMsg => "/hudmsg",
            Endpoint::GameChat => "/gamechat",
            Endpoint::MapInfo => "/map_info.json",
            Endpoint::MapObj => "/map_obj.json",
        }
    }

    pub fn default_request_path(self) -> &'static str {
        match self {
            Endpoint::HudMsg => "/hudmsg?lastEvt=0&lastDmg=0",
            Endpoint::GameChat => "/gamechat?lastId=0",
            other => other.path(),
        }
    }

    pub fn all() -> [Self; 6] {
        [
            Self::State,
            Self::Indicators,
            Self::HudMsg,
            Self::GameChat,
            Self::MapInfo,
            Self::MapObj,
        ]
    }

    pub fn extract_messages(self, json: &Value) -> Vec<String> {
        let mut messages = Vec::new();
        match self {
            Endpoint::HudMsg => collect_text_messages(json, &mut messages),
            Endpoint::GameChat => {
                messages.extend(
                    extract_chat_messages(json)
                        .into_iter()
                        .map(|message| message.text),
                );
            }
            Endpoint::State | Endpoint::MapObj => collect_eventish_text(json, &mut messages),
            Endpoint::Indicators | Endpoint::MapInfo => {}
        }
        messages.sort();
        messages.dedup();
        messages
    }
}

impl EndpointProbe {
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }
}

fn extract_chat_messages(value: &Value) -> Vec<ChatMessage> {
    let mut messages = Vec::new();
    collect_chat_messages(value, &mut messages);
    messages.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then_with(|| left.text.cmp(&right.text))
    });
    messages.dedup();
    messages
}

fn collect_chat_messages(value: &Value, messages: &mut Vec<ChatMessage>) {
    match value {
        Value::String(text) => push_chat_message(messages, None, text),
        Value::Array(items) => {
            if let Some(message) = chat_message_from_array(items) {
                messages.push(message);
            } else {
                for item in items {
                    collect_chat_messages(item, messages);
                }
            }
        }
        Value::Object(object) => {
            if let Some(message) = chat_message_from_object(object) {
                messages.push(message);
            } else {
                for value in object.values() {
                    collect_chat_messages(value, messages);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn chat_message_from_object(object: &Map<String, Value>) -> Option<ChatMessage> {
    let id = first_u64(object, &["id", "lastId", "msgId", "msg_id"]);
    let time = first_scalar_string(object, &["time", "timestamp", "t"]);
    let sender = first_string(object, &["sender", "player", "from", "name"]).map(str::to_owned);

    let text = ["msg", "message", "text"]
        .iter()
        .filter_map(|key| object.get(*key).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    if text.is_empty() {
        None
    } else {
        Some(ChatMessage {
            id,
            time,
            sender,
            text,
        })
    }
}

fn chat_message_from_array(items: &[Value]) -> Option<ChatMessage> {
    let id = items.iter().find_map(value_as_u64);
    let strings = items
        .iter()
        .filter_map(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .collect::<Vec<_>>();
    let text = strings.join(" ");

    if text.is_empty() {
        None
    } else {
        Some(ChatMessage {
            id,
            time: strings
                .first()
                .copied()
                .filter(|value| looks_like_time(value))
                .map(str::to_owned),
            sender: None,
            text,
        })
    }
}

pub(crate) fn update_last_chat_id(
    current_last_id: u64,
    json: &Value,
    messages: &[ChatMessage],
) -> u64 {
    next_last_id(json, messages)
        .filter(|next_last_id| *next_last_id > current_last_id)
        .unwrap_or(current_last_id)
}

fn next_last_id(json: &Value, messages: &[ChatMessage]) -> Option<u64> {
    find_named_u64(json, &["lastId", "last_id", "nextLastId", "next_last_id"])
        .or_else(|| messages.iter().filter_map(|message| message.id).max())
}

fn find_named_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    match value {
        Value::Object(object) => first_u64(object, keys).or_else(|| {
            object
                .values()
                .find_map(|value| find_named_u64(value, keys))
        }),
        Value::Array(items) => items.iter().find_map(|value| find_named_u64(value, keys)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

fn first_u64(object: &Map<String, Value>, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(value_as_u64))
}

fn first_string<'a>(object: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn first_scalar_string(object: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        let value = object.get(*key)?;
        if let Some(text) = value
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            Some(text.to_owned())
        } else if value.is_number() || value.is_boolean() {
            Some(value.to_string())
        } else {
            None
        }
    })
}

fn value_as_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

fn push_chat_message(messages: &mut Vec<ChatMessage>, id: Option<u64>, text: &str) {
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        messages.push(ChatMessage {
            id,
            time: None,
            sender: None,
            text: trimmed.to_owned(),
        });
    }
}

fn looks_like_time(value: &str) -> bool {
    let mut parts = value.split(':');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(left), Some(right), None)
            if !left.is_empty()
                && !right.is_empty()
                && left.chars().all(|ch| ch.is_ascii_digit())
                && right.chars().all(|ch| ch.is_ascii_digit())
    )
}

fn collect_text_messages(value: &Value, messages: &mut Vec<String>) {
    match value {
        Value::String(text) => push_message(messages, text),
        Value::Array(items) => {
            for item in items {
                collect_text_messages(item, messages);
            }
        }
        Value::Object(object) => {
            let mut selected = Vec::new();
            for key in ["msg", "message", "text", "sender", "player", "mode"] {
                if let Some(Value::String(text)) = object.get(key) {
                    selected.push(text.as_str());
                }
            }

            if selected.is_empty() {
                for value in object.values() {
                    collect_text_messages(value, messages);
                }
            } else {
                push_message(messages, &selected.join(" "));
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn collect_eventish_text(value: &Value, messages: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            let lower = text.to_lowercase();
            if [
                "destroyed",
                "critical",
                "severe",
                "mission",
                "damage",
                "hit",
            ]
            .iter()
            .any(|pattern| lower.contains(pattern))
            {
                push_message(messages, text);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_eventish_text(item, messages);
            }
        }
        Value::Object(object) => {
            for value in object.values() {
                collect_eventish_text(value, messages);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn push_message(messages: &mut Vec<String>, text: &str) {
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        messages.push(trimmed.to_owned());
    }
}

fn summarize_json(value: &Value) -> Option<String> {
    match value {
        Value::Object(object) => Some(format!("object with {} keys", object.len())),
        Value::Array(array) => Some(format!("array with {} items", array.len())),
        Value::Null => Some("null".to_owned()),
        Value::Bool(value) => Some(format!("bool {value}")),
        Value::Number(value) => Some(format!("number {value}")),
        Value::String(value) => Some(format!("string {} chars", value.len())),
    }
}

fn compact_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

fn friendly_error(error: &reqwest::Error) -> String {
    if error.is_connect() {
        "connection refused or API unavailable".to_owned()
    } else if error.is_timeout() {
        "request timed out".to_owned()
    } else if error.is_status() {
        error
            .status()
            .map(|status| format!("HTTP {status}"))
            .unwrap_or_else(|| "HTTP error".to_owned())
    } else if error.is_decode() {
        "invalid JSON response".to_owned()
    } else {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn updates_last_chat_id_with_highest_message_id() {
        let messages = vec![
            ChatMessage {
                id: Some(12),
                time: None,
                sender: None,
                text: "old".to_owned(),
            },
            ChatMessage {
                id: Some(15),
                time: None,
                sender: None,
                text: "new".to_owned(),
            },
        ];

        assert_eq!(update_last_chat_id(10, &json!([]), &messages), 15);
    }

    #[test]
    fn does_not_move_last_chat_id_backwards() {
        let messages = vec![ChatMessage {
            id: Some(9),
            time: None,
            sender: None,
            text: "old".to_owned(),
        }];

        assert_eq!(
            update_last_chat_id(10, &json!({"lastId": 9}), &messages),
            10
        );
    }

    #[test]
    fn message_without_id_uses_content_key() {
        let message = ChatMessage {
            id: None,
            time: Some("1:27".to_owned()),
            sender: Some("dawson16800".to_owned()),
            text: "dawson16800 (F/A-18C Early) destroyed [ai] MiG-15bis".to_owned(),
        };

        assert_eq!(
            message.stable_key_with_prefix("chat"),
            "chat:1:27|dawson16800|dawson16800 (F/A-18C Early) destroyed [ai] MiG-15bis"
        );
    }

    #[test]
    fn updates_hud_damage_cursor_with_highest_damage_id() {
        let damage = vec![
            ChatMessage {
                id: Some(1),
                time: Some("83".to_owned()),
                sender: None,
                text: "kill one".to_owned(),
            },
            ChatMessage {
                id: Some(4),
                time: Some("167".to_owned()),
                sender: None,
                text: "kill four".to_owned(),
            },
        ];

        assert_eq!(update_last_chat_id(0, &json!({}), &damage), 4);
    }
}
