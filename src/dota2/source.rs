use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::dota2::config::DotaConfig;
use crate::dota2::state::{GsiState, Player};
use crate::games::event::{GameEvent, GameEventKind};
use crate::games::source::{GamePollResult, GameSource};

/// Dota 2 implementation of [`GameSource`] via Game State Integration (GSI).
///
/// Unlike War Thunder (which is polled), Dota 2 *pushes* game state to a local
/// HTTP server. `DotaSource` runs that server in the background, stores the
/// latest snapshot, and `poll()` diffs it against the previous snapshot to
/// detect new kills, deaths, and objectives.
pub struct DotaSource {
    config: DotaConfig,
    inner: Arc<Mutex<DotaInner>>,
}

struct DotaInner {
    /// The most recent GSI snapshot received, or `None` if no data yet.
    current: Option<GsiState>,
    /// The snapshot from the previous `poll()` call, used for diffing.
    last_polled: Option<GsiState>,
    /// Whether we have received at least one GSI POST since startup.
    connected: bool,
    /// Last time we received a POST, for connectivity liveness.
    last_received: Option<Instant>,
    /// Whether bootstrap has been called.
    bootstrapped: bool,
}

impl DotaSource {
    pub fn new(config: DotaConfig) -> Self {
        Self {
            config,
            inner: Arc::new(Mutex::new(DotaInner {
                current: None,
                last_polled: None,
                connected: false,
                last_received: None,
                bootstrapped: false,
            })),
        }
    }

    /// Start the background GSI HTTP server that listens for Dota 2 POSTs.
    ///
    /// This must be called before `poll()`. The server runs on the configured
    /// port and stores incoming snapshots in the shared inner state.
    pub async fn start_server(&self) -> anyhow::Result<()> {
        let addr: SocketAddr = ([127, 0, 0, 1], self.config.port).into();
        let inner = self.inner.clone();
        let expected_token = self.config.auth_token.clone();

        let listener = tokio::net::TcpListener::bind(addr).await?;
        info!("Dota 2 GSI server listening on {}", addr);

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let inner = inner.clone();
                        let token = expected_token.clone();
                        tokio::spawn(handle_gsi_connection(stream, inner, token));
                    }
                    Err(error) => {
                        warn!(%error, "GSI server accept failed");
                    }
                }
            }
        });

        Ok(())
    }
}

async fn handle_gsi_connection(
    stream: tokio::net::TcpStream,
    inner: Arc<Mutex<DotaInner>>,
    expected_token: Option<String>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buf = Vec::with_capacity(8192);
    let mut reader = stream;
    let read = reader.read_to_end(&mut buf).await;
    let body = match read {
        Ok(n) if n > 0 => &buf[..n],
        _ => return,
    };

    // Extract the JSON body (skip HTTP headers if present).
    // Dota 2 sends a proper HTTP POST with Content-Type: application/json.
    let json_str = extract_json_body(body);
    let Some(json_str) = json_str else {
        return;
    };

    // Parse and optionally validate auth token.
    let value: Value = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(error) => {
            debug!(%error, "failed to parse GSI JSON");
            return;
        }
    };

    if let Some(expected) = &expected_token {
        let received = value
            .get("auth")
            .and_then(|a| a.get("token"))
            .and_then(|t| t.as_str());
        if received != Some(expected.as_str()) {
            debug!("GSI auth token mismatch, ignoring payload");
            return;
        }
    }

    let state: GsiState = match serde_json::from_value(value) {
        Ok(s) => s,
        Err(error) => {
            debug!(%error, "failed to deserialize GSI state");
            return;
        }
    };

    let mut guard = inner.lock().await;
    // Merge: Dota 2 omits unchanged sections, so we should merge with the
    // previous state. For simplicity, we take the latest as-is since the
    // fields we care about (kills, deaths) are always present when playing.
    guard.current = Some(state);
    guard.connected = true;
    guard.last_received = Some(Instant::now());

    // Respond with 200 OK.
    let _ = reader
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
        .await;
}

/// Extract the JSON body from a raw HTTP request, skipping headers.
fn extract_json_body(raw: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(raw).ok()?;
    // Find the body after the blank line separating headers from body.
    let split = text.find("\r\n\r\n").map(|i| i + 4);
    if let Some(start) = split {
        let body = &text[start..];
        if !body.is_empty() {
            return Some(body.to_owned());
        }
    }
    // If there's no HTTP header, the whole thing might be raw JSON.
    if text.trim_start().starts_with('{') {
        return Some(text.trim().to_owned());
    }
    None
}

#[async_trait]
impl GameSource for DotaSource {
    fn name(&self) -> &'static str {
        "Dota 2"
    }

    fn player_name(&self) -> Option<&str> {
        self.config.player_name.as_deref()
    }

    async fn bootstrap(&self, _include_history: bool) {
        let mut guard = self.inner.lock().await;
        guard.bootstrapped = true;
        // On bootstrap we just mark the current snapshot as the baseline so
        // we don't emit events for kills that happened before startup.
        guard.last_polled = guard.current.clone();
    }

    async fn poll(&self) -> GamePollResult {
        let mut guard = self.inner.lock().await;

        // Connectivity: consider disconnected if no POST in the last 10s.
        let connected = guard.connected
            && guard
                .last_received
                .map(|t| t.elapsed() < Duration::from_secs(10))
                .unwrap_or(false);

        let current = match &guard.current {
            Some(state) => state.clone(),
            None => {
                return GamePollResult {
                    events: Vec::new(),
                    connected,
                };
            }
        };

        let previous = guard.last_polled.clone();
        guard.last_polled = Some(current.clone());

        let events = diff_states(&previous, &current, self.config.player_name.as_deref());

        GamePollResult { events, connected }
    }
}

/// Compare two GSI snapshots and emit [`GameEvent`]s for changes we care about.
fn diff_states(
    previous: &Option<GsiState>,
    current: &GsiState,
    player_name: Option<&str>,
) -> Vec<GameEvent> {
    let mut events = Vec::new();

    let prev_player = previous.as_ref().and_then(|s| s.player.as_ref());
    let curr_player = current.player.as_ref();

    if let (Some(curr), Some(prev)) = (curr_player, prev_player) {
        // Kill detection: kills increased.
        if let (Some(curr_kills), Some(prev_kills)) = (curr.kills, prev.kills) {
            if curr_kills > prev_kills {
                let count = curr_kills - prev_kills;
                let player = resolve_player_name(curr, player_name);
                let hero = current.hero.as_ref().and_then(|h| h.name.clone());
                let summary = if count > 1 {
                    format!("{player} got a multi-kill ({count} kills)")
                } else {
                    format!("{player} got a kill")
                };
                let kind = if count > 1 {
                    GameEventKind::MultiKill
                } else {
                    GameEventKind::Kill
                };
                let mut event = GameEvent::new(
                    kind,
                    summary,
                    format!("dota_kill|{}|{}", player, curr_kills),
                );
                event.actor = Some(player);
                event.context = hero;
                events.push(event);
            }
        }

        // Death detection: deaths increased.
        if let (Some(curr_deaths), Some(prev_deaths)) = (curr.deaths, prev.deaths) {
            if curr_deaths > prev_deaths {
                let player = resolve_player_name(curr, player_name);
                let summary = format!("{player} died");
                let mut event = GameEvent::new(
                    GameEventKind::Death,
                    summary,
                    format!("dota_death|{}|{}", player, curr_deaths),
                );
                event.actor = Some(player);
                events.push(event);
            }
        }
    }

    // Game state changes (e.g. game ended) could be detected here via
    // current.map.game_state vs previous.map.game_state. Left as a future
    // enhancement for objective-based clipping.

    events
}

fn resolve_player_name(player: &Player, configured: Option<&str>) -> String {
    configured
        .filter(|n| !n.trim().is_empty())
        .or_else(|| player.name.as_deref())
        .unwrap_or("Player")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dota2::state::{GsiState, Hero, Player};

    fn player(kills: u32, deaths: u32, name: &str) -> Player {
        Player {
            kills: Some(kills),
            deaths: Some(deaths),
            name: Some(name.to_owned()),
            ..Default::default()
        }
    }

    fn state(player: Player, hero: Option<Hero>) -> GsiState {
        GsiState {
            player: Some(player),
            hero,
            ..Default::default()
        }
    }

    #[test]
    fn detects_new_kill() {
        let prev = state(player(0, 0, "alice"), None);
        let curr = state(player(1, 0, "alice"), None);
        let events = diff_states(&Some(prev), &curr, Some("alice"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, GameEventKind::Kill);
        assert!(events[0].summary.contains("alice"));
    }

    #[test]
    fn detects_multi_kill() {
        let prev = state(player(0, 0, "alice"), None);
        let curr = state(player(3, 0, "alice"), None);
        let events = diff_states(&Some(prev), &curr, Some("alice"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, GameEventKind::MultiKill);
    }

    #[test]
    fn detects_death() {
        let prev = state(player(2, 0, "alice"), None);
        let curr = state(player(2, 1, "alice"), None);
        let events = diff_states(&Some(prev), &curr, Some("alice"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, GameEventKind::Death);
    }

    #[test]
    fn no_change_emits_nothing() {
        let prev = state(player(5, 2, "alice"), None);
        let curr = state(player(5, 2, "alice"), None);
        let events = diff_states(&Some(prev), &curr, Some("alice"));
        assert!(events.is_empty());
    }

    #[test]
    fn first_snapshot_emits_nothing() {
        let curr = state(player(10, 3, "alice"), None);
        let events = diff_states(&None, &curr, Some("alice"));
        assert!(events.is_empty());
    }

    #[test]
    fn kill_and_death_in_same_update() {
        let prev = state(player(0, 0, "alice"), None);
        let curr = state(player(1, 1, "alice"), None);
        let events = diff_states(&Some(prev), &curr, Some("alice"));
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|e| e.kind == GameEventKind::Kill));
        assert!(events.iter().any(|e| e.kind == GameEventKind::Death));
    }

    #[test]
    fn uses_configured_player_name_over_gsi_name() {
        let prev = state(player(0, 0, "gsi_name"), None);
        let curr = state(player(1, 0, "gsi_name"), None);
        let events = diff_states(&Some(prev), &curr, Some("configured_name"));
        assert_eq!(events[0].actor.as_deref(), Some("configured_name"));
    }

    #[test]
    fn falls_back_to_gsi_name_when_not_configured() {
        let prev = state(player(0, 0, "gsi_name"), None);
        let curr = state(player(1, 0, "gsi_name"), None);
        let events = diff_states(&Some(prev), &curr, None);
        assert_eq!(events[0].actor.as_deref(), Some("gsi_name"));
    }

    #[test]
    fn extracts_json_from_http_body() {
        let http =
            b"POST / HTTP/1.1\r\nContent-Type: application/json\r\n\r\n{\"player\":{\"kills\":1}}";
        let json = extract_json_body(http).unwrap();
        assert!(json.contains("\"kills\":1"));
    }

    #[test]
    fn extracts_raw_json_without_headers() {
        let raw = b"{\"player\":{\"kills\":1}}";
        let json = extract_json_body(raw).unwrap();
        assert!(json.contains("\"kills\":1"));
    }
}
