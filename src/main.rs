use std::{path::PathBuf, time::Duration};

use clap::Parser;
use eframe::egui;
use tokio::sync::mpsc;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, error, info};

use wt_clipper::app::auto::{run_auto_clip, AutoClipConfig};
use wt_clipper::capture::buffer::{run_replay_buffer, ReplayBufferConfig};
use wt_clipper::capture::output::resolve_output_path_in_dir;
use wt_clipper::capture::quality::{QualityPreset, VideoQuality};
use wt_clipper::capture::recorder::{record, RecordingRequest};
use wt_clipper::cli::{CaptureSource, Cli, Command, ConfigCommand, DumpEndpoint};
use wt_clipper::config::{
    default_config_path, expand_tilde, AppConfig, ClipConfig, WarThunderConfig,
};
use wt_clipper::doctor;
use wt_clipper::ui::{
    app::WtClipperApp,
    bridge::{AppEvent, Bridge, ClipInfo, UiCommand},
};
use wt_clipper::warthunder::client::{ChatMessage, Endpoint, EndpointProbe, WarThunderClient};
use wt_clipper::warthunder::events::WarThunderEvent;
use wt_clipper::warthunder::parser::{is_personal_kill, parse_gamechat_event};
use wt_clipper::warthunder::recent::RecentMessageCache;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "wt_clipper=info".into()),
        )
        .without_time()
        .init();

    if matches!(cli.command, Command::Gui) {
        return gui_command(cli.config);
    }

    tokio::runtime::Runtime::new()?.block_on(async_main(cli))
}

async fn async_main(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Gui => unreachable!("gui command is handled before tokio runtime creation"),
        Command::Config {
            command: ConfigCommand::Init { force },
        } => {
            let path = cli
                .config
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(default_config_path);
            AppConfig::write_default(&path, force)?;
            println!("Config written: {}", path.display());
            Ok(())
        }
        Command::Doctor { json } => {
            let config = AppConfig::load(cli.config.as_deref())?;
            doctor::run_doctor(json, Some(config.clip.output_dir_path()?)).await
        }
        Command::Record {
            duration,
            output,
            source,
            quality,
            fps,
            video_bitrate,
        } => {
            let config = AppConfig::load(cli.config.as_deref())?;
            record_command(
                &config.clip,
                duration,
                output,
                source,
                quality,
                fps,
                video_bitrate,
            )
            .await
        }
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
            let config = AppConfig::load(cli.config.as_deref())?;
            let quality_preset = quality.unwrap_or(config.clip.quality);
            let quality =
                resolve_configured_video_quality(&config.clip, quality, fps, video_bitrate)?;
            run_replay_buffer(ReplayBufferConfig {
                buffer_seconds: seconds.unwrap_or(config.clip.seconds),
                segment_seconds: segment_seconds.unwrap_or(config.clip.segment_seconds),
                output_dir: Some(resolve_output_dir(output_dir, &config.clip)?),
                source: source.unwrap_or(config.clip.source),
                keep_segments: keep_segments || config.clip.keep_segments,
                quality_preset,
                quality,
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
            let config = AppConfig::load(cli.config.as_deref())?;
            let client = WarThunderClient::new(config.war_thunder.clone())?;
            let quality_preset = quality.unwrap_or(config.clip.quality);
            let quality =
                resolve_configured_video_quality(&config.clip, quality, fps, video_bitrate)?;

            run_auto_clip(
                client,
                config.war_thunder,
                AutoClipConfig {
                    buffer_seconds: seconds.unwrap_or(config.clip.seconds),
                    segment_seconds: segment_seconds.unwrap_or(config.clip.segment_seconds),
                    output_dir: Some(resolve_output_dir(output_dir, &config.clip)?),
                    source: source.unwrap_or(config.clip.source),
                    keep_segments: keep_segments || config.clip.keep_segments,
                    quality_preset,
                    quality,
                    cooldown: Duration::from_secs(cooldown_seconds),
                    post_event_delay: Duration::from_secs(
                        post_event_seconds.unwrap_or(config.clip.post_event_seconds),
                    ),
                    multi_kill_window: Duration::from_secs(config.clip.multi_kill_window_seconds),
                    include_history,
                    triggers: config.triggers.clone(),
                    ui_events: None,
                    export_mode: config.clip.export_mode,
                    pending_export_dir: config.pending_exports.pending_export_dir_path()?,
                    command_rx: None,
                },
            )
            .await
        }
        command => {
            let config = AppConfig::load(cli.config.as_deref())?;
            let client = WarThunderClient::new(config.war_thunder.clone())?;

            match command {
                Command::Status => status(&client).await,
                Command::Config { .. } => {
                    unreachable!("config command is handled before config load")
                }
                Command::Doctor { .. } => {
                    unreachable!("doctor command is handled before config load")
                }
                Command::Dump { endpoint } => dump(&client, endpoint).await,
                Command::Watch { include_history } => {
                    watch(&client, &config.war_thunder, include_history).await
                }
                Command::Gui => unreachable!("gui command is handled before config load"),
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

fn gui_command(config_path: Option<PathBuf>) -> anyhow::Result<()> {
    launch_tauri_gui(config_path)
}

fn launch_tauri_gui(config_path: Option<PathBuf>) -> anyhow::Result<()> {
    let manifest_dir = std::env::current_dir()?;
    let tauri_dir = manifest_dir.join("src-tauri");
    if !tauri_dir.join("tauri.conf.json").exists() {
        anyhow::bail!(
            "Tauri UI not found at {}; run from the wt-clipper source tree",
            tauri_dir.display()
        );
    }

    let mut command = std::process::Command::new("cargo");
    command.arg("tauri").arg("dev").current_dir(&manifest_dir);
    if let Some(path) = config_path {
        command.env("WT_CLIPPER_CONFIG", path);
    }
    let status = command.status()?;
    if !status.success() {
        anyhow::bail!("Tauri GUI exited with status {status}");
    }
    Ok(())
}

#[allow(dead_code)]
fn legacy_egui_command(config_path: Option<PathBuf>) -> anyhow::Result<()> {
    let config = AppConfig::load(config_path.as_deref())?;
    let config_save_path = config_path.unwrap_or_else(default_config_path);
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    spawn_gui_runtime(config.clone(), config_save_path, event_tx, cmd_rx);

    let bridge = Bridge { event_rx, cmd_tx };
    let ui_config = config.clone();
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("WT Clipper")
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([920.0, 620.0]),
        ..eframe::NativeOptions::default()
    };

    eframe::run_native(
        "wt-clipper",
        native_options,
        Box::new(move |cc| Ok(Box::new(WtClipperApp::new(cc, bridge, ui_config)))),
    )
    .map_err(|error| anyhow::anyhow!("failed to launch GUI: {error}"))
}

fn spawn_gui_runtime(
    config: AppConfig,
    config_path: PathBuf,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    cmd_rx: mpsc::UnboundedReceiver<UiCommand>,
) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = event_tx.send(AppEvent::ClipFailed {
                    message: format!("Tokio runtime: {error}"),
                });
                return;
            }
        };

        runtime.block_on(async move {
            let client = match WarThunderClient::new(config.war_thunder.clone()) {
                Ok(client) => client,
                Err(error) => {
                    let _ = event_tx.send(AppEvent::ClipFailed {
                        message: format!("War Thunder client: {error}"),
                    });
                    return;
                }
            };
            let quality_preset = config.clip.quality;
            let quality = match resolve_configured_video_quality(&config.clip, None, None, None) {
                Ok(quality) => quality,
                Err(error) => {
                    let _ = event_tx.send(AppEvent::ClipFailed {
                        message: format!("Video quality: {error}"),
                    });
                    return;
                }
            };
            let output_dir = match config.clip.output_dir_path() {
                Ok(path) => path,
                Err(error) => {
                    let _ = event_tx.send(AppEvent::ClipFailed {
                        message: format!("Output dir: {error}"),
                    });
                    return;
                }
            };
            let pending_export_dir = match config.pending_exports.pending_export_dir_path() {
                Ok(path) => path,
                Err(error) => {
                    let _ = event_tx.send(AppEvent::ClipFailed {
                        message: format!("Pending export dir: {error}"),
                    });
                    return;
                }
            };

            let command_events = event_tx.clone();
            let command_output_dir = output_dir.clone();
            tokio::spawn(async move {
                command_loop(cmd_rx, command_events, command_output_dir, config_path).await;
            });

            let auto = run_auto_clip(
                client,
                config.war_thunder.clone(),
                AutoClipConfig {
                    buffer_seconds: config.clip.seconds,
                    segment_seconds: config.clip.segment_seconds,
                    output_dir: Some(output_dir),
                    source: config.clip.source,
                    keep_segments: config.clip.keep_segments,
                    quality_preset,
                    quality,
                    cooldown: Duration::from_secs(3),
                    post_event_delay: Duration::from_secs(config.clip.post_event_seconds),
                    multi_kill_window: Duration::from_secs(config.clip.multi_kill_window_seconds),
                    include_history: false,
                    triggers: config.triggers.clone(),
                    ui_events: Some(event_tx.clone()),
                    export_mode: config.clip.export_mode,
                    pending_export_dir,
                    command_rx: None,
                },
            );

            tokio::select! {
                result = auto => {
                    if let Err(error) = result {
                        error!(%error, "auto clip task failed");
                        let _ = event_tx.send(AppEvent::ClipFailed {
                            message: format!("Auto clip: {error}"),
                        });
                    }
                }
            }
        });
    });
}

async fn command_loop(
    mut cmd_rx: mpsc::UnboundedReceiver<UiCommand>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    output_dir: PathBuf,
    config_path: PathBuf,
) {
    while let Some(command) = cmd_rx.recv().await {
        match command {
            UiCommand::LoadClips => {
                let output_dir = output_dir.clone();
                let event_tx = event_tx.clone();
                tokio::spawn(async move {
                    match scan_clips(output_dir).await {
                        Ok((clips, total_bytes)) => {
                            let _ = event_tx.send(AppEvent::ClipsLoaded { clips, total_bytes });
                        }
                        Err(error) => {
                            let _ = event_tx.send(AppEvent::ClipFailed {
                                message: format!("Scan clips: {error}"),
                            });
                        }
                    }
                });
            }
            UiCommand::RunDiagnostics => {
                let event_tx = event_tx.clone();
                let output_dir = Some(output_dir.clone());
                tokio::spawn(async move {
                    let report = doctor::build_report(output_dir).await;
                    let _ = event_tx.send(AppEvent::DiagnosticsReady(report));
                });
            }
            UiCommand::DeleteClip(path) => {
                let event_tx = event_tx.clone();
                tokio::task::spawn_blocking(move || delete_clip_files(&path))
                    .await
                    .ok();
                let _ = event_tx.send(AppEvent::ClipFailed {
                    message: "Clip supprimé".to_owned(),
                });
                let _ = event_tx.send(AppEvent::DiskUsage { used_bytes: 0 });
            }
            UiCommand::OpenOutputFolder => {
                let dir = output_dir.clone();
                tokio::task::spawn_blocking(move || {
                    if let Err(error) = std::process::Command::new("xdg-open").arg(&dir).spawn() {
                        debug!(%error, path = %dir.display(), "failed to open output folder");
                    }
                })
                .await
                .ok();
            }
            UiCommand::UpdateConfig(config) => {
                let event_tx = event_tx.clone();
                let path = config_path.clone();
                tokio::task::spawn_blocking(move || write_config(&path, &config))
                    .await
                    .ok()
                    .and_then(Result::ok);
                let _ = event_tx.send(AppEvent::ClipFailed {
                    message: "Configuration sauvegardée".to_owned(),
                });
            }
            UiCommand::SaveManualClip => {
                let _ = event_tx.send(AppEvent::ClipFailed {
                    message: "Clip manuel indisponible pendant cette session".to_owned(),
                });
            }
            UiCommand::RestartBuffer => {
                let _ = event_tx.send(AppEvent::ClipFailed {
                    message: "Redémarrage appliqué au prochain lancement".to_owned(),
                });
            }
        }
    }
}

async fn scan_clips(output_dir: PathBuf) -> anyhow::Result<(Vec<ClipInfo>, u64)> {
    tokio::task::spawn_blocking(move || {
        let mut clips = Vec::new();
        let mut total_bytes = 0_u64;
        let now = std::time::SystemTime::now();
        for entry in walkdir::WalkDir::new(&output_dir)
            .max_depth(1)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("webm") {
                continue;
            }
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    debug!(%error, path = %path.display(), "failed to read clip metadata");
                    continue;
                }
            };
            let size_bytes = metadata.len();
            total_bytes = total_bytes.saturating_add(size_bytes);
            let modified_secs_ago = metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .unwrap_or_else(|| path.display().to_string());
            clips.push(ClipInfo {
                reason: clip_reason_from_name(&file_name),
                path: path.to_path_buf(),
                thumbnail_path: path
                    .with_extension("jpg")
                    .exists()
                    .then(|| path.with_extension("jpg")),
                preview_url: None,
                file_name,
                size_bytes,
                duration_seconds: 0,
                modified_secs_ago,
            });
        }
        clips.sort_by_key(|clip| clip.modified_secs_ago);
        Ok((clips, total_bytes))
    })
    .await?
}

fn clip_reason_from_name(name: &str) -> wt_clipper::capture::buffer::ClipReason {
    if name.starts_with("multi-kill") {
        wt_clipper::capture::buffer::ClipReason::MultiKill
    } else if name.starts_with("base") {
        wt_clipper::capture::buffer::ClipReason::BaseDestroyed
    } else if name.starts_with("kill") {
        wt_clipper::capture::buffer::ClipReason::TargetDestroyed
    } else if name.starts_with("death") {
        wt_clipper::capture::buffer::ClipReason::PlayerDestroyed
    } else if name.starts_with("manual") {
        wt_clipper::capture::buffer::ClipReason::Manual
    } else {
        wt_clipper::capture::buffer::ClipReason::Unknown
    }
}

fn delete_clip_files(path: &std::path::Path) {
    if let Err(error) = std::fs::remove_file(path) {
        debug!(%error, path = %path.display(), "failed to delete clip video");
    }
    let mut metadata_path = path.to_path_buf();
    metadata_path.set_extension("json");
    if metadata_path.exists() {
        if let Err(error) = std::fs::remove_file(&metadata_path) {
            debug!(%error, path = %metadata_path.display(), "failed to delete clip metadata");
        }
    }
}

fn write_config(path: &std::path::Path, config: &AppConfig) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, toml::to_string_pretty(config)?)?;
    Ok(())
}

fn resolve_configured_video_quality(
    config: &ClipConfig,
    cli_quality: Option<QualityPreset>,
    fps: Option<u32>,
    video_bitrate: Option<u32>,
) -> anyhow::Result<VideoQuality> {
    let preset = cli_quality.unwrap_or(config.quality);
    let base = if cli_quality.is_some() {
        preset.video_quality()
    } else {
        VideoQuality::new(
            config.fps,
            config.video_bitrate_kbps,
            preset.video_quality().encoder_cpu_used,
        )?
    };

    VideoQuality::new(
        fps.unwrap_or(base.fps),
        video_bitrate.unwrap_or(base.video_bitrate_kbps),
        base.encoder_cpu_used,
    )
}

fn resolve_output_dir(
    cli_output_dir: Option<PathBuf>,
    config: &ClipConfig,
) -> anyhow::Result<PathBuf> {
    match cli_output_dir {
        Some(path) => expand_tilde(&path.to_string_lossy()),
        None => config.output_dir_path(),
    }
}

async fn record_command(
    clip_config: &ClipConfig,
    duration_seconds: Option<u64>,
    output: Option<std::path::PathBuf>,
    source: Option<CaptureSource>,
    quality_preset: Option<QualityPreset>,
    fps: Option<u32>,
    video_bitrate: Option<u32>,
) -> anyhow::Result<()> {
    let duration_seconds = duration_seconds.unwrap_or(clip_config.seconds);
    let output_path = resolve_output_path_in_dir(output, clip_config.output_dir_path()?)?;
    let duration = Duration::from_secs(duration_seconds);
    let quality =
        resolve_configured_video_quality(clip_config, quality_preset, fps, video_bitrate)?;

    println!("Recording output: {}", output_path.display());
    println!("Duration: {duration_seconds}s");
    println!("Video target: {}", quality.log_summary());
    println!("Starting recording...");

    record(RecordingRequest {
        duration,
        output_path: output_path.clone(),
        source: source.unwrap_or(clip_config.source),
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

    #[test]
    fn cli_quality_override_replaces_config_preset_defaults() {
        let mut config = ClipConfig::default();
        config.quality = QualityPreset::High;
        config.fps = 60;
        config.video_bitrate_kbps = 20_000;

        let quality =
            resolve_configured_video_quality(&config, Some(QualityPreset::Medium), None, None)
                .unwrap();

        assert_eq!(quality.fps, 30);
        assert_eq!(quality.video_bitrate_kbps, 10_000);
        assert_eq!(quality.encoder_cpu_used, 4);
    }

    #[test]
    fn cli_output_dir_override_replaces_config_output_dir() {
        let mut config = ClipConfig::default();
        config.output_dir = "/tmp/from-config".to_owned();

        let output_dir = resolve_output_dir(Some(PathBuf::from("/tmp/from-cli")), &config).unwrap();

        assert_eq!(output_dir, PathBuf::from("/tmp/from-cli"));
    }
}
