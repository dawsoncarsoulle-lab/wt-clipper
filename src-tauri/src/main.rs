use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde_json::json;
use tauri::{Emitter, Manager, State};
use tokio::sync::mpsc;
use tracing::debug;
use wt_clipper::{
    app::auto::{run_auto_clip, AutoClipConfig},
    capture::{
        buffer::{ClipReason, ReplayBufferConfig, ReplayBufferHandle},
        quality::VideoQuality,
    },
    config::{default_config_path, AppConfig, ClipConfig},
    doctor::{self, DoctorReport},
    ui::bridge::{AppEvent, ClipInfo, UiCommand},
    warthunder::client::WarThunderClient,
};

struct BackendState {
    cmd_tx: mpsc::UnboundedSender<UiCommand>,
    config_path: PathBuf,
}

#[tauri::command]
fn get_config(state: State<'_, BackendState>) -> Result<AppConfig, String> {
    AppConfig::load(Some(&state.config_path)).map_err(|error| error.to_string())
}

#[tauri::command]
fn save_config(config: AppConfig, state: State<'_, BackendState>) -> Result<(), String> {
    write_config(&state.config_path, &config).map_err(|error| error.to_string())?;
    state
        .cmd_tx
        .send(UiCommand::UpdateConfig(config))
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn load_clips(state: State<'_, BackendState>) -> Result<Vec<ClipInfo>, String> {
    let config = AppConfig::load(Some(&state.config_path)).map_err(|error| error.to_string())?;
    let output_dir = config
        .clip
        .output_dir_path()
        .map_err(|error| error.to_string())?;
    let (clips, _) = scan_clips(output_dir).await.map_err(|error| error.to_string())?;
    Ok(clips)
}

#[tauri::command]
async fn delete_clip(path: String, state: State<'_, BackendState>) -> Result<(), String> {
    let path = PathBuf::from(path);
    tokio::task::spawn_blocking(move || delete_clip_files(&path))
        .await
        .map_err(|error| error.to_string())?;
    let _ = state.cmd_tx.send(UiCommand::LoadClips);
    Ok(())
}

#[tauri::command]
fn open_output_folder(state: State<'_, BackendState>) -> Result<(), String> {
    state
        .cmd_tx
        .send(UiCommand::OpenOutputFolder)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn run_diagnostics(state: State<'_, BackendState>) -> Result<DoctorReport, String> {
    let config = AppConfig::load(Some(&state.config_path)).map_err(|error| error.to_string())?;
    let output_dir = config.clip.output_dir_path().ok();
    Ok(doctor::build_report(output_dir).await)
}

#[tauri::command]
fn save_manual_clip(state: State<'_, BackendState>) -> Result<(), String> {
    state
        .cmd_tx
        .send(UiCommand::SaveManualClip)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn restart_buffer(state: State<'_, BackendState>) -> Result<(), String> {
    state
        .cmd_tx
        .send(UiCommand::RestartBuffer)
        .map_err(|error| error.to_string())
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let config_path = std::env::var_os("WT_CLIPPER_CONFIG")
                .map(PathBuf::from)
                .unwrap_or_else(default_config_path);
            let config = AppConfig::load(Some(&config_path)).unwrap_or_default();
            let cmd_tx = spawn_backend(app.handle().clone(), config, config_path.clone());
            app.manage(BackendState {
                cmd_tx,
                config_path,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            load_clips,
            delete_clip,
            open_output_folder,
            run_diagnostics,
            save_manual_clip,
            restart_buffer
        ])
        .run(tauri::generate_context!())
        .expect("failed to run WT Clipper Tauri app");
}

fn spawn_backend(
    app: tauri::AppHandle,
    config: AppConfig,
    config_path: PathBuf,
) -> mpsc::UnboundedSender<UiCommand> {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime,
            Err(error) => {
                emit_app_event(
                    &app,
                    AppEvent::ClipFailed {
                        message: format!("Tokio runtime: {error}"),
                    },
                );
                return;
            }
        };

        runtime.block_on(async move {
            let output_dir = match config.clip.output_dir_path() {
                Ok(path) => path,
                Err(error) => {
                    emit_app_event(
                        &app,
                        AppEvent::ClipFailed {
                            message: format!("Output dir: {error}"),
                        },
                    );
                    return;
                }
            };

            let forward_app = app.clone();
            tokio::spawn(async move {
                forward_events(forward_app, event_rx).await;
            });

            let command_events = event_tx.clone();
            let command_output_dir = output_dir.clone();
            tokio::spawn(async move {
                command_loop(cmd_rx, command_events, command_output_dir, config_path).await;
            });

            if let Err(error) = run_auto_backend(config, output_dir, event_tx.clone()).await {
                let _ = event_tx.send(AppEvent::ClipFailed {
                    message: format!("Auto clip: {error}"),
                });
            }
        });
    });
    cmd_tx
}

async fn run_auto_backend(
    config: AppConfig,
    output_dir: PathBuf,
    event_tx: mpsc::UnboundedSender<AppEvent>,
) -> anyhow::Result<()> {
    let client = WarThunderClient::new(config.war_thunder.clone())?;
    let quality_preset = config.clip.quality;
    let quality = resolve_configured_video_quality(&config.clip)?;
    run_auto_clip(
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
            target_destroyed_trigger: config.triggers.target_destroyed,
            ui_events: Some(event_tx),
        },
    )
    .await
}

async fn forward_events(app: tauri::AppHandle, mut event_rx: mpsc::UnboundedReceiver<AppEvent>) {
    while let Some(event) = event_rx.recv().await {
        emit_app_event(&app, event);
    }
}

fn emit_app_event(app: &tauri::AppHandle, event: AppEvent) {
    let result = match event {
        AppEvent::WtConnected => app.emit("wt-connected", json!({})),
        AppEvent::WtDisconnected => app.emit("wt-disconnected", json!({})),
        AppEvent::KillDetected {
            reason,
            vehicle,
            target,
            description,
        } => app.emit(
            "kill-detected",
            json!({
                "reason": reason,
                "vehicle": vehicle,
                "target": target,
                "description": description
            }),
        ),
        AppEvent::ClipSaved {
            path,
            reason,
            duration_seconds,
            size_bytes,
        } => {
            let thumbnail_path = ensure_thumbnail(&path);
            app.emit(
                "clip-saved",
                json!({
                    "path": path,
                    "thumbnailPath": thumbnail_path,
                    "fileName": path.file_name().and_then(|name| name.to_str()).unwrap_or_default(),
                    "reason": reason,
                    "durationSeconds": duration_seconds,
                    "sizeBytes": size_bytes,
                    "modifiedSecsAgo": 0
                }),
            )
        }
        AppEvent::ClipFailed { message } => app.emit("clip-failed", json!({ "message": message })),
        AppEvent::BufferProgress {
            filled_secs,
            total_secs,
        } => app.emit(
            "buffer-progress",
            json!({ "filledSecs": filled_secs, "totalSecs": total_secs }),
        ),
        AppEvent::DiskUsage { used_bytes } => {
            app.emit("disk-usage", json!({ "usedBytes": used_bytes }))
        }
        AppEvent::ClipsLoaded { clips, total_bytes } => {
            app.emit(
                "clips-loaded",
                json!({ "clips": clips, "totalBytes": total_bytes }),
            )
        }
        AppEvent::DiagnosticsReady(report) => app.emit("diagnostics-ready", report),
    };

    if let Err(error) = result {
        debug!(%error, "failed to emit Tauri event");
    }
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
                let reload_dir = output_dir.clone();
                let reload_events = event_tx.clone();
                tokio::task::spawn_blocking(move || delete_clip_files(&path))
                    .await
                    .ok();
                let _ = event_tx.send(AppEvent::ClipFailed {
                    message: "Clip supprimé".to_owned(),
                });
                tokio::spawn(async move {
                    match scan_clips(reload_dir).await {
                        Ok((clips, total_bytes)) => {
                            let _ = reload_events.send(AppEvent::ClipsLoaded { clips, total_bytes });
                        }
                        Err(error) => {
                            let _ = reload_events.send(AppEvent::ClipFailed {
                                message: format!("Scan clips: {error}"),
                            });
                        }
                    }
                });
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
                let path = config_path.clone();
                let result = tokio::task::spawn_blocking(move || write_config(&path, &config))
                    .await
                    .ok()
                    .and_then(Result::ok);
                let message = if result.is_some() {
                    "Configuration sauvegardée"
                } else {
                    "La configuration n'a pas pu être sauvegardée"
                };
                let _ = event_tx.send(AppEvent::ClipFailed {
                    message: message.to_owned(),
                });
            }
            UiCommand::SaveManualClip => {
                let event_tx = event_tx.clone();
                let output_dir = output_dir.clone();
                let config_path = config_path.clone();
                tokio::spawn(async move {
                    let _ = event_tx.send(AppEvent::ClipFailed {
                        message: "Clip manuel demandé".to_owned(),
                    });
                    let config = match AppConfig::load(Some(&config_path)) {
                        Ok(config) => config,
                        Err(error) => {
                            let _ = event_tx.send(AppEvent::ClipFailed {
                                message: format!("Config: {error}"),
                            });
                            return;
                        }
                    };
                    if let Err(error) =
                        save_standalone_manual_clip(config, output_dir, event_tx.clone()).await
                    {
                        let _ = event_tx.send(AppEvent::ClipFailed {
                            message: format!("Clip manuel: {error}"),
                        });
                    }
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

async fn save_standalone_manual_clip(
    config: AppConfig,
    output_dir: PathBuf,
    event_tx: mpsc::UnboundedSender<AppEvent>,
) -> anyhow::Result<()> {
    let quality = resolve_configured_video_quality(&config.clip)?;
    let handle = ReplayBufferHandle::start(ReplayBufferConfig {
        buffer_seconds: config.clip.seconds,
        segment_seconds: config.clip.segment_seconds,
        output_dir: Some(output_dir),
        source: config.clip.source,
        keep_segments: config.clip.keep_segments,
        quality_preset: config.clip.quality,
        quality,
    })
    .await?;
    tokio::time::sleep(Duration::from_secs(config.clip.seconds.min(5))).await;
    if let Some(replay) = handle.save_replay(handle.manual_clip_context()).await? {
        if let Some(path) = replay.final_video_path {
            let size_bytes = std::fs::metadata(&path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            let _ = event_tx.send(AppEvent::ClipSaved {
                path,
                reason: ClipReason::Manual,
                duration_seconds: config.clip.seconds,
                size_bytes,
            });
        }
    }
    handle.stop().await
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
            let thumbnail_path = ensure_thumbnail(path);
            clips.push(ClipInfo {
                reason: clip_reason_from_name(&file_name),
                path: path.to_path_buf(),
                thumbnail_path,
                file_name,
                size_bytes,
                duration_seconds: 0,
                modified_secs_ago,
            });
        }
        clips.sort_by_key(|clip| std::cmp::Reverse(clip.modified_secs_ago));
        Ok((clips, total_bytes))
    })
    .await?
}

fn clip_reason_from_name(name: &str) -> ClipReason {
    if name.starts_with("multi-kill") {
        ClipReason::MultiKill
    } else if name.starts_with("kill") {
        ClipReason::TargetDestroyed
    } else if name.starts_with("death") {
        ClipReason::PlayerDestroyed
    } else if name.starts_with("manual") {
        ClipReason::Manual
    } else {
        ClipReason::Unknown
    }
}

fn delete_clip_files(path: &Path) {
    if let Err(error) = std::fs::remove_file(path) {
        debug!(%error, path = %path.display(), "failed to delete clip video");
    }
    let metadata_path = path.with_extension("json");
    if metadata_path.exists() {
        if let Err(error) = std::fs::remove_file(&metadata_path) {
            debug!(%error, path = %metadata_path.display(), "failed to delete clip metadata");
        }
    }
    let thumbnail_path = path.with_extension("jpg");
    if thumbnail_path.exists() {
        if let Err(error) = std::fs::remove_file(&thumbnail_path) {
            debug!(%error, path = %thumbnail_path.display(), "failed to delete clip thumbnail");
        }
    }
}

fn write_config(path: &Path, config: &AppConfig) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, toml::to_string_pretty(config)?)?;
    Ok(())
}

fn resolve_configured_video_quality(config: &ClipConfig) -> anyhow::Result<VideoQuality> {
    let base = config.quality.video_quality();
    VideoQuality::new(config.fps, config.video_bitrate_kbps, base.encoder_cpu_used)
}

fn ensure_thumbnail(path: &Path) -> Option<PathBuf> {
    let thumbnail_path = path.with_extension("jpg");
    if thumbnail_path.exists() {
        return Some(thumbnail_path);
    }

    match std::process::Command::new("ffmpeg")
        .args(["-y", "-i"])
        .arg(path)
        .args(["-vframes", "1", "-s", "640x360"])
        .arg(&thumbnail_path)
        .output()
    {
        Ok(output) if output.status.success() && thumbnail_path.exists() => Some(thumbnail_path),
        Ok(output) => {
            debug!(
                status = %output.status,
                path = %path.display(),
                stderr = %String::from_utf8_lossy(&output.stderr),
                "failed to generate clip thumbnail"
            );
            None
        }
        Err(error) => {
            debug!(%error, path = %path.display(), "failed to run ffmpeg for clip thumbnail");
            None
        }
    }
}
