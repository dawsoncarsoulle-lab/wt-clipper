mod app;
mod capture;
mod cli;
mod config;
mod warthunder;

use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, info};

use crate::app::auto::{run_auto_clip, AutoClipConfig};
use crate::capture::buffer::{run_replay_buffer, ReplayBufferConfig};
use crate::capture::output::resolve_output_path;
use crate::capture::quality::{QualityPreset, VideoQuality};
use crate::capture::recorder::{record, RecordingRequest};
use crate::cli::{CaptureSource, Cli, Command, DumpEndpoint};
use crate::config::{AppConfig, WarThunderConfig};
use crate::warthunder::client::{ChatMessage, Endpoint, EndpointProbe, WarThunderClient};
use crate::warthunder::events::WarThunderEvent;
use crate::warthunder::parser::{is_personal_kill, parse_gamechat_event};
use crate::warthunder::recent::RecentMessageCache;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "wt_clipper=info".into()),
        )
        .without_time()
        .init();

    match cli.command {
        Command::Record {
            duration,
            output,
            source,
            quality,
            fps,
            video_bitrate,
        } => record_command(duration, output, source, quality, fps, video_bitrate).await,
        Command::Buffer {
            seconds,
            segment_seconds,
            output_dir,
            source,
            quality,
            fps,
            video_bitrate,
            keep_segments,
        } => {
            run_replay_buffer(ReplayBufferConfig {
                buffer_seconds: seconds,
                segment_seconds,
                output_dir,
                source,
                keep_segments,
                quality: resolve_video_quality(quality, fps, video_bitrate)?,
            })
            .await
        }
        Command::Auto {
            seconds,
            segment_seconds,
            output_dir,
            source,
            quality,
            fps,
            video_bitrate,
            keep_segments,
            cooldown_seconds,
            post_event_seconds,
            include_history,
        } => {
            let config = AppConfig::load(&cli.config)
                .with_context(|| format!("failed to read config from {}", cli.config.display()))?;
            let client = WarThunderClient::new(config.war_thunder.clone())?;

            run_auto_clip(
                client,
                config.war_thunder,
                AutoClipConfig {
                    buffer_seconds: seconds,
                    segment_seconds,
                    output_dir,
                    source,
                    keep_segments,
                    quality: resolve_video_quality(quality, fps, video_bitrate)?,
                    cooldown: Duration::from_secs(cooldown_seconds),
                    post_event_delay: Duration::from_secs(post_event_seconds),
                    include_history,
                },
            )
            .await
        }
        command => {
            let config = AppConfig::load(&cli.config)
                .with_context(|| format!("failed to read config from {}", cli.config.display()))?;
            let client = WarThunderClient::new(config.war_thunder.clone())?;

            match command {
                Command::Status => status(&client).await,
                Command::Dump { endpoint } => dump(&client, endpoint).await,
                Command::Watch { include_history } => {
                    watch(&client, &config.war_thunder, include_history).await
                }
                Command::Record { .. } => {
                    unreachable!("record command is handled before config load")
                }
                Command::Buffer { .. } => {
                    unreachable!("buffer command is handled before config load")
                }
                Command::Auto { .. } => unreachable!("auto command is handled before config load"),
            }
        }
    }
}

fn resolve_video_quality(
    quality: QualityPreset,
    fps: Option<u32>,
    video_bitrate: Option<u32>,
) -> anyhow::Result<VideoQuality> {
    VideoQuality::with_overrides(quality, fps, video_bitrate)
}

async fn record_command(
    duration_seconds: u64,
    output: Option<std::path::PathBuf>,
    source: CaptureSource,
    quality_preset: QualityPreset,
    fps: Option<u32>,
    video_bitrate: Option<u32>,
) -> anyhow::Result<()> {
    let output_path = resolve_output_path(output)?;
    let duration = Duration::from_secs(duration_seconds);
    let quality = resolve_video_quality(quality_preset, fps, video_bitrate)?;

    println!("Recording output: {}", output_path.display());
    println!("Duration: {duration_seconds}s");
    println!("Video target: {}", quality.log_summary());
    println!("Starting recording...");

    record(RecordingRequest {
        duration,
        output_path: output_path.clone(),
        source,
        quality,
    })
    .await?;

    println!("Finalized recording.");
    println!("Success: {}", output_path.display());
    Ok(())
}

async fn status(client: &WarThunderClient) -> anyhow::Result<()> {
    println!("War Thunder HTTP API: {}", client.base_url());

    let probes = client.probe_all().await;
    let reachable = probes.iter().any(EndpointProbe::is_ok);

    if reachable {
        println!("Status: reachable");
    } else {
        println!("Status: unreachable");
        println!(
            "No endpoint answered on {}. Start War Thunder and enable localhost telemetry if needed.",
            client.base_url()
        );
    }

    println!();
    println!("Endpoints:");
    for probe in &probes {
        match probe {
            EndpointProbe::Ok { endpoint, summary } => {
                let summary = summary.as_deref().unwrap_or("ok");
                println!("  {:<14} ok ({summary})", endpoint.path());
            }
            EndpointProbe::Failed { endpoint, error } => {
                println!("  {:<14} failed ({error})", endpoint.path());
            }
        }
    }

    if let Some(summary) = client.state_summary().await? {
        println!();
        println!("State summary: {summary}");
    }

    Ok(())
}

async fn dump(client: &WarThunderClient, endpoint: DumpEndpoint) -> anyhow::Result<()> {
    let path = match endpoint {
        DumpEndpoint::Gamechat => "/gamechat?lastId=0",
        DumpEndpoint::Hudmsg => "/hudmsg?lastEvt=0&lastDmg=0",
    };

    let raw = client.fetch_raw(path).await?;

    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(json) => println!("{}", serde_json::to_string_pretty(&json)?),
        Err(error) => {
            println!("Response from {path} is not valid JSON: {error}");
            println!();
            println!("{raw}");
        }
    }

    Ok(())
}

async fn watch(
    client: &WarThunderClient,
    config: &WarThunderConfig,
    include_history: bool,
) -> anyhow::Result<()> {
    let poll_interval = config.poll_interval();
    let player_name = config.player_name.as_deref();

    info!(
        base_url = %client.base_url(),
        poll_interval_ms = poll_interval.as_millis(),
        player_name = player_name.unwrap_or("<unset>"),
        "watching War Thunder events"
    );
    println!(
        "Watching War Thunder at {} every {} ms. Press Ctrl+C to stop.",
        client.base_url(),
        poll_interval.as_millis()
    );

    let mut ticker = interval(poll_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut state = WatchState::new(1000);
    if include_history {
        println!("[WT] include-history enabled: processing existing events");
    } else {
        bootstrap_watch(client, &mut state).await;
        println!(
            "[WT] initialized cursors: chat={}, hud_evt={}, hud_dmg={}",
            state.last_chat_id, state.last_evt_msg_id, state.last_dmg_msg_id
        );
        println!("[WT] watching for new events only");
    }

    let mut reported_unreachable = false;
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("received Ctrl+C, stopping watch");
                break;
            }
            _ = ticker.tick() => {
                let mut successful_polls = 0usize;

                debug!(last_chat_id = state.last_chat_id, "polling gamechat");
                match client.fetch_gamechat(state.last_chat_id).await {
                    Ok(chat) => {
                        successful_polls += 1;
                        let previous_last_chat_id = state.last_chat_id;
                        debug!(
                            previous_last_chat_id,
                            returned_messages = chat.messages.len(),
                            next_last_chat_id = chat.next_last_id,
                            "polled gamechat"
                        );
                        state.last_chat_id = chat.next_last_id;
                        if state.last_chat_id != previous_last_chat_id {
                            debug!(
                                previous_last_chat_id,
                                updated_last_chat_id = state.last_chat_id,
                                "updated gamechat cursor"
                            );
                        }

                        process_messages("gamechat", chat.messages, &mut state.seen_messages, player_name);
                    }
                    Err(error) if error.is_connect() => {
                        debug!(endpoint = "/gamechat?lastId=<last_chat_id>", %error, "endpoint not reachable");
                    }
                    Err(error) => {
                        debug!(endpoint = "/gamechat?lastId=<last_chat_id>", %error, "failed to poll endpoint");
                    }
                }

                debug!(last_evt_msg_id = state.last_evt_msg_id, last_dmg_msg_id = state.last_dmg_msg_id, "polling hudmsg");
                match client.fetch_hudmsg(state.last_evt_msg_id, state.last_dmg_msg_id).await {
                    Ok(hud) => {
                        successful_polls += 1;
                        let previous_evt_msg_id = state.last_evt_msg_id;
                        let previous_dmg_msg_id = state.last_dmg_msg_id;
                        debug!(
                            previous_evt_msg_id,
                            previous_dmg_msg_id,
                            returned_events = hud.events.len(),
                            returned_damage = hud.damage.len(),
                            next_last_evt_id = hud.next_last_evt_id,
                            next_last_dmg_id = hud.next_last_dmg_id,
                            "polled hudmsg"
                        );
                        state.last_evt_msg_id = hud.next_last_evt_id;
                        state.last_dmg_msg_id = hud.next_last_dmg_id;
                        if state.last_evt_msg_id != previous_evt_msg_id || state.last_dmg_msg_id != previous_dmg_msg_id {
                            debug!(
                                previous_evt_msg_id,
                                previous_dmg_msg_id,
                                updated_evt_msg_id = state.last_evt_msg_id,
                                updated_dmg_msg_id = state.last_dmg_msg_id,
                                "updated hudmsg cursors"
                            );
                        }

                        process_messages("hud:event", hud.events, &mut state.seen_messages, player_name);
                        process_messages("hud:damage", hud.damage, &mut state.seen_messages, player_name);
                    }
                    Err(error) if error.is_connect() => {
                        debug!(endpoint = "/hudmsg?lastEvt=<last_evt_msg_id>&lastDmg=<last_dmg_msg_id>", %error, "endpoint not reachable");
                    }
                    Err(error) => {
                        debug!(endpoint = "/hudmsg?lastEvt=<last_evt_msg_id>&lastDmg=<last_dmg_msg_id>", %error, "failed to poll endpoint");
                    }
                }

                for endpoint in [Endpoint::State, Endpoint::MapObj] {
                    match client.fetch_endpoint_json(endpoint).await {
                        Ok(json) => {
                            successful_polls += 1;
                            for message in endpoint.extract_messages(&json) {
                                let key = format!("{}:{message}", endpoint.path());
                                if state.seen_messages.contains(&key) {
                                    debug!(endpoint = endpoint.path(), key = %key, "skipped duplicate watched message");
                                    continue;
                                }
                                state.seen_messages.insert(key);

                                let event = parse_gamechat_event(&message);
                                debug!(endpoint = endpoint.path(), message = %message, ?event, "parsed watched event");
                                if is_personal_kill(&event, player_name) {
                                    print_kill(&event);
                                } else {
                                    debug!(endpoint = endpoint.path(), message = %message, ?event, "ignoring watched event");
                                }
                            }
                        }
                        Err(error) if error.is_connect() => {
                            debug!(endpoint = endpoint.default_request_path(), %error, "endpoint not reachable");
                        }
                        Err(error) => {
                            debug!(endpoint = endpoint.default_request_path(), %error, "failed to poll endpoint");
                        }
                    }
                }

                if successful_polls == 0 && !reported_unreachable {
                    println!(
                        "War Thunder localhost API is not reachable at {}. Waiting for it to appear...",
                        client.base_url()
                    );
                    reported_unreachable = true;
                } else if successful_polls > 0 && reported_unreachable {
                    println!("War Thunder localhost API is reachable again.");
                    reported_unreachable = false;
                }
            }
        }
    }

    Ok(())
}

#[derive(Debug)]
struct WatchState {
    last_chat_id: u64,
    last_evt_msg_id: u64,
    last_dmg_msg_id: u64,
    seen_messages: RecentMessageCache,
}

impl WatchState {
    fn new(cache_len: usize) -> Self {
        Self {
            last_chat_id: 0,
            last_evt_msg_id: 0,
            last_dmg_msg_id: 0,
            seen_messages: RecentMessageCache::new(cache_len),
        }
    }
}

async fn bootstrap_watch(client: &WarThunderClient, state: &mut WatchState) {
    match client.fetch_gamechat(0).await {
        Ok(chat) => bootstrap_gamechat(state, chat.messages, chat.next_last_id),
        Err(error) => debug!(%error, "failed to bootstrap gamechat"),
    }

    match client.fetch_hudmsg(0, 0).await {
        Ok(hud) => bootstrap_hudmsg(
            state,
            hud.events,
            hud.damage,
            hud.next_last_evt_id,
            hud.next_last_dmg_id,
        ),
        Err(error) => debug!(%error, "failed to bootstrap hudmsg"),
    }
}

fn bootstrap_gamechat(state: &mut WatchState, messages: Vec<ChatMessage>, next_last_id: u64) {
    state.last_chat_id = state.last_chat_id.max(next_last_id);
    remember_messages("gamechat", messages, &mut state.seen_messages);
}

fn bootstrap_hudmsg(
    state: &mut WatchState,
    events: Vec<ChatMessage>,
    damage: Vec<ChatMessage>,
    next_last_evt_id: u64,
    next_last_dmg_id: u64,
) {
    state.last_evt_msg_id = state.last_evt_msg_id.max(next_last_evt_id);
    state.last_dmg_msg_id = state.last_dmg_msg_id.max(next_last_dmg_id);
    remember_messages("hud:event", events, &mut state.seen_messages);
    remember_messages("hud:damage", damage, &mut state.seen_messages);
}

fn remember_messages(
    source: &str,
    messages: Vec<ChatMessage>,
    seen_messages: &mut RecentMessageCache,
) {
    for message in messages {
        seen_messages.insert(message.stable_key_with_prefix(source));
    }
}

fn process_messages(
    source: &str,
    messages: Vec<ChatMessage>,
    seen_messages: &mut RecentMessageCache,
    player_name: Option<&str>,
) {
    for message in messages {
        let key = message.stable_key_with_prefix(source);
        debug!(
            source,
            message_id = ?message.id,
            message_time = ?message.time,
            message_sender = ?message.sender,
            key = %key,
            "extracted message"
        );

        if seen_messages.contains(&key) {
            debug!(source, key = %key, message = %message.text, "skipped duplicate message");
            continue;
        }
        seen_messages.insert(key);

        let event = parse_gamechat_event(&message.text);
        debug!(source, message = %message.text, ?event, "parsed event");
        if is_personal_kill(&event, player_name) {
            print_kill(&event);
        } else {
            debug!(source, message = %message.text, ?event, "ignoring non-personal or non-kill event");
        }
    }
}

fn print_kill(event: &WarThunderEvent) {
    let WarThunderEvent::TargetDestroyed {
        attacker,
        action,
        vehicle,
        target,
        raw,
    } = event
    else {
        return;
    };

    let Some(attacker) = attacker else {
        println!("[WT] kill detected: {raw}");
        return;
    };
    let Some(target) = target else {
        println!("[WT] kill detected: {raw}");
        return;
    };

    if let Some(vehicle) = vehicle {
        println!("[WT] kill detected: {attacker} {action} {target} with {vehicle}");
    } else {
        println!("[WT] kill detected: {attacker} {action} {target}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: u64, text: &str) -> ChatMessage {
        ChatMessage {
            id: Some(id),
            time: Some((id * 10).to_string()),
            sender: None,
            text: text.to_owned(),
        }
    }

    #[test]
    fn bootstrap_initializes_cursors_to_max_values() {
        let mut state = WatchState::new(1000);

        bootstrap_gamechat(&mut state, vec![message(1, "chat")], 7);
        bootstrap_hudmsg(
            &mut state,
            vec![message(2, "event")],
            vec![message(4, "damage")],
            2,
            4,
        );

        assert_eq!(state.last_chat_id, 7);
        assert_eq!(state.last_evt_msg_id, 2);
        assert_eq!(state.last_dmg_msg_id, 4);
    }

    #[test]
    fn bootstrap_marks_existing_messages_seen_without_processing_events() {
        let mut state = WatchState::new(1000);
        let kill = message(4, "dawson16800 (F/A-18C Early) destroyed [ai] MiG-15bis");

        bootstrap_hudmsg(&mut state, Vec::new(), vec![kill], 0, 4);

        assert!(state.seen_messages.contains("hud:damage:4"));
        assert_eq!(state.seen_messages.len(), 1);
    }

    #[test]
    fn include_history_starts_from_zero_and_empty_cache() {
        let state = WatchState::new(1000);

        assert_eq!(state.last_chat_id, 0);
        assert_eq!(state.last_evt_msg_id, 0);
        assert_eq!(state.last_dmg_msg_id, 0);
        assert_eq!(state.seen_messages.len(), 0);
    }

    #[test]
    fn bootstrap_fills_cache_for_gamechat_and_hudmsg() {
        let mut state = WatchState::new(1000);

        bootstrap_gamechat(&mut state, vec![message(1, "chat")], 1);
        bootstrap_hudmsg(
            &mut state,
            vec![message(2, "event")],
            vec![message(3, "damage")],
            2,
            3,
        );

        assert!(state.seen_messages.contains("gamechat:1"));
        assert!(state.seen_messages.contains("hud:event:2"));
        assert!(state.seen_messages.contains("hud:damage:3"));
        assert_eq!(state.seen_messages.len(), 3);
    }
}
