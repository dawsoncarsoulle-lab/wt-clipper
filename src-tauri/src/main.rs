use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{Emitter, Manager, State};
use tokio::sync::{mpsc, oneshot};
use tracing::debug;
use chrono::Utc;
use uuid::Uuid;
mod export_queue;
mod updater;
use export_queue::{ExportSummary, PendingClipExportDto};
use wt_clipper::{
    app::auto::{
        run_auto_clip, AutoClipCommand, AutoClipConfig,
    },
    capture::{
        buffer::{ClipReason, ReplayBufferConfig, ReplayBufferHandle, SaveReplayOutcome},
        quality::VideoQuality,
    },
    config::{default_config_path, AppConfig, ClipConfig},
    doctor::{self, DoctorReport},
    ui::bridge::{AppEvent, ClipInfo, ClipStatus, ClipStatusPayload, UiCommand},
    warthunder::client::WarThunderClient,
};

struct BackendState {
    cmd_tx: mpsc::UnboundedSender<UiCommand>,
    auto_cmd_tx: mpsc::UnboundedSender<AutoClipCommand>,
    config_path: PathBuf,
    runtime_status: Arc<Mutex<RuntimeStatus>>,
    preview_server: Arc<ClipPreviewServer>,
}

struct BackendChannels {
    cmd_tx: mpsc::UnboundedSender<UiCommand>,
    auto_cmd_tx: mpsc::UnboundedSender<AutoClipCommand>,
}

struct ClipPreviewServer {
    base_url: String,
    clips: Arc<Mutex<HashMap<String, PathBuf>>>,
}

impl ClipPreviewServer {
    fn start() -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let base_url = format!("http://{}", listener.local_addr()?);
        let clips = Arc::new(Mutex::new(HashMap::new()));
        let server_clips = clips.clone();
        thread::spawn(move || {
            for stream in listener.incoming().filter_map(Result::ok) {
                let clips = server_clips.clone();
                thread::spawn(move || handle_preview_request(stream, clips));
            }
        });
        Ok(Self { base_url, clips })
    }

    fn url_for_path(&self, path: &Path) -> Option<String> {
        let canonical = path.canonicalize().ok()?;
        let id = preview_clip_id(&canonical);
        if let Ok(mut clips) = self.clips.lock() {
            clips.insert(id.clone(), canonical);
        }
        Some(format!("{}/{}.webm", self.base_url, id))
    }
}

fn preview_clip_id(path: &Path) -> String {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn handle_preview_request(mut stream: TcpStream, clips: Arc<Mutex<HashMap<String, PathBuf>>>) {
    let mut request = [0_u8; 4096];
    let Ok(read) = stream.read(&mut request) else {
        return;
    };
    let request = String::from_utf8_lossy(&request[..read]);
    let Some(first_line) = request.lines().next() else {
        return;
    };
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    if method != "GET" && method != "HEAD" {
        write_http_status(&mut stream, "405 Method Not Allowed");
        return;
    }

    let Some(id) = path
        .trim_start_matches('/')
        .strip_suffix(".webm")
        .filter(|id| !id.is_empty())
    else {
        write_http_status(&mut stream, "404 Not Found");
        return;
    };
    let clip_path = match clips.lock().ok().and_then(|clips| clips.get(id).cloned()) {
        Some(path) => path,
        None => {
            write_http_status(&mut stream, "404 Not Found");
            return;
        }
    };
    let Ok(mut file) = File::open(&clip_path) else {
        write_http_status(&mut stream, "404 Not Found");
        return;
    };
    let Ok(size) = file.metadata().map(|metadata| metadata.len()) else {
        write_http_status(&mut stream, "500 Internal Server Error");
        return;
    };

    let range = request
        .lines()
        .find_map(|line| line.strip_prefix("Range: bytes="))
        .and_then(|range| parse_byte_range(range, size));
    let (status, start, end) = match range {
        Some((start, end)) => ("206 Partial Content", start, end),
        None => ("200 OK", 0, size.saturating_sub(1)),
    };
    let length = end.saturating_sub(start).saturating_add(1);
    let content_range = if status.starts_with("206") {
        format!("Content-Range: bytes {start}-{end}/{size}\r\n")
    } else {
        String::new()
    };
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Type: video/webm\r\nAccept-Ranges: bytes\r\n{content_range}Content-Length: {length}\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(headers.as_bytes()).is_err() || method == "HEAD" {
        return;
    }
    if file.seek(SeekFrom::Start(start)).is_err() {
        return;
    }
    let mut remaining = length;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let limit = remaining.min(buffer.len() as u64) as usize;
        let Ok(read) = file.read(&mut buffer[..limit]) else {
            return;
        };
        if read == 0 || stream.write_all(&buffer[..read]).is_err() {
            return;
        }
        remaining = remaining.saturating_sub(read as u64);
    }
}

fn parse_byte_range(range: &str, size: u64) -> Option<(u64, u64)> {
    if size == 0 {
        return None;
    }
    let (start, end) = range.split_once('-')?;
    let start = if start.is_empty() {
        size.saturating_sub(end.parse::<u64>().ok()?)
    } else {
        start.parse::<u64>().ok()?
    };
    let end = if end.is_empty() {
        size - 1
    } else {
        end.parse::<u64>().ok()?.min(size - 1)
    };
    (start <= end && start < size).then_some((start, end))
}

fn write_http_status(stream: &mut TcpStream, status: &str) {
    let _ = stream.write_all(
        format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    );
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeStatus {
    wt_connected: bool,
    buffer_filled_secs: f32,
    buffer_total_secs: f32,
    auto_clip_running: bool,
    clips_saved: usize,
    recent_events: Vec<RuntimeEvent>,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeEvent {
    id: String,
    at: String,
    kind: ClipReason,
    description: String,
}

impl RuntimeStatus {
    fn from_config(config: &AppConfig) -> Self {
        Self {
            wt_connected: false,
            buffer_filled_secs: 0.0,
            buffer_total_secs: config.clip.seconds as f32,
            auto_clip_running: false,
            clips_saved: 0,
            recent_events: Vec::new(),
            last_error: None,
        }
    }
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
    let (clips, _) = scan_clips(output_dir, state.preview_server.clone())
        .await
        .map_err(|error| error.to_string())?;
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
        .auto_cmd_tx
        .send(AutoClipCommand::SaveManualClip)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn export_pending_clips(state: State<'_, BackendState>) -> Result<ExportSummary, String> {
    let (respond_to, response) = oneshot::channel();
    state
        .auto_cmd_tx
        .send(AutoClipCommand::ExportPendingClips { respond_to })
        .map_err(|error| error.to_string())?;
    response.await.map_err(|error| error.to_string())
}

#[tauri::command]
async fn get_pending_export_clips(
    state: State<'_, BackendState>,
) -> Result<Vec<PendingClipExportDto>, String> {
    let (respond_to, response) = oneshot::channel();
    state
        .auto_cmd_tx
        .send(AutoClipCommand::GetPendingExportClips { respond_to })
        .map_err(|error| error.to_string())?;
    response.await.map_err(|error| error.to_string())
}

#[tauri::command]
fn restart_buffer(state: State<'_, BackendState>) -> Result<(), String> {
    state
        .cmd_tx
        .send(UiCommand::RestartBuffer)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn get_runtime_status(state: State<'_, BackendState>) -> Result<RuntimeStatus, String> {
    state
        .runtime_status
        .lock()
        .map(|status| status.clone())
        .map_err(|error| error.to_string())
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let config_path = std::env::var_os("WT_CLIPPER_CONFIG")
                .map(PathBuf::from)
                .unwrap_or_else(default_config_path);
            let config = AppConfig::load(Some(&config_path)).unwrap_or_default();
            let runtime_status = Arc::new(Mutex::new(RuntimeStatus::from_config(&config)));
            let preview_server = Arc::new(ClipPreviewServer::start()?);
            #[cfg(desktop)]
            app.handle()
                .plugin(tauri_plugin_updater::Builder::new().build())?;
            let channels = spawn_backend(
                app.handle().clone(),
                config,
                config_path.clone(),
                runtime_status.clone(),
                preview_server.clone(),
            );
            app.manage(BackendState {
                cmd_tx: channels.cmd_tx,
                auto_cmd_tx: channels.auto_cmd_tx,
                config_path,
                runtime_status,
                preview_server,
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
            export_pending_clips,
            get_pending_export_clips,
            restart_buffer,
            updater::check_for_updates,
            get_runtime_status
        ])
        .run(tauri::generate_context!())
        .expect("failed to run WT Clipper Tauri app");
}

fn spawn_backend(
    app: tauri::AppHandle,
    config: AppConfig,
    config_path: PathBuf,
    runtime_status: Arc<Mutex<RuntimeStatus>>,
    preview_server: Arc<ClipPreviewServer>,
) -> BackendChannels {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (auto_cmd_tx, auto_cmd_rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime,
            Err(error) => {
                emit_app_event(
                    &app,
                    &runtime_status,
                    &preview_server,
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
                        &runtime_status,
                        &preview_server,
                        AppEvent::ClipFailed {
                            message: format!("Output dir: {error}"),
                        },
                    );
                    return;
                }
            };

            let forward_app = app.clone();
            let forward_runtime_status = runtime_status.clone();
            let forward_preview_server = preview_server.clone();
            tokio::spawn(async move {
                forward_events(
                    forward_app,
                    forward_runtime_status,
                    forward_preview_server,
                    event_rx,
                )
                .await;
            });

            let command_events = event_tx.clone();
            let command_output_dir = output_dir.clone();
            let command_preview_server = preview_server.clone();
            tokio::spawn(async move {
                command_loop(
                    cmd_rx,
                    command_events,
                    command_output_dir,
                    config_path,
                    command_preview_server,
                )
                .await;
            });

            if let Ok(mut status) = runtime_status.lock() {
                status.auto_clip_running = true;
            }
            if let Err(error) = run_auto_backend(config, output_dir, event_tx.clone(), auto_cmd_rx).await {
                if let Ok(mut status) = runtime_status.lock() {
                    status.auto_clip_running = false;
                    status.last_error = Some(error.to_string());
                }
                let _ = event_tx.send(AppEvent::ClipFailed {
                    message: format!("Auto clip: {error}"),
                });
            } else if let Ok(mut status) = runtime_status.lock() {
                status.auto_clip_running = false;
            }
        });
    });
    BackendChannels { cmd_tx, auto_cmd_tx }
}

async fn run_auto_backend(
    config: AppConfig,
    output_dir: PathBuf,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    auto_cmd_rx: mpsc::UnboundedReceiver<AutoClipCommand>,
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
            triggers: config.triggers.clone(),
            ui_events: Some(event_tx),
            export_mode: config.clip.export_mode,
            command_rx: Some(auto_cmd_rx),
        },
    )
    .await
}

async fn forward_events(
    app: tauri::AppHandle,
    runtime_status: Arc<Mutex<RuntimeStatus>>,
    preview_server: Arc<ClipPreviewServer>,
    mut event_rx: mpsc::UnboundedReceiver<AppEvent>,
) {
    while let Some(event) = event_rx.recv().await {
        emit_app_event(&app, &runtime_status, &preview_server, event);
    }
}

fn emit_app_event(
    app: &tauri::AppHandle,
    runtime_status: &Arc<Mutex<RuntimeStatus>>,
    preview_server: &Arc<ClipPreviewServer>,
    event: AppEvent,
) {
    update_runtime_status(runtime_status, &event);
    log_bridge_event_received(&event);
    let event_name = tauri_event_name(&event);
    let generic_event = event.clone();
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
            let preview_url = preview_server.url_for_path(&path);
            app.emit(
                "clip-saved",
                json!({
                    "path": path,
                    "thumbnailPath": null,
                    "previewUrl": preview_url,
                    "fileName": path.file_name().and_then(|name| name.to_str()).unwrap_or_default(),
                    "reason": reason,
                    "durationSeconds": duration_seconds,
                    "sizeBytes": size_bytes,
                    "modifiedSecsAgo": 0
                }),
            )
        }
        AppEvent::ClipStatusChanged { payload } => {
            let _ = app.emit("clip_status_changed", payload.clone());
            app.emit("clip-status-changed", payload)
        }
        AppEvent::ExportProgressChanged { payload } => {
            let _ = app.emit("export-progress-changed", payload.clone());
            app.emit("export_progress_changed", payload)
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

    match result {
        Ok(()) => debug!(event = event_name, "emitted Tauri event"),
        Err(error) => debug!(%error, "failed to emit Tauri event"),
    }
    if let Err(error) = app.emit("app-event", generic_event) {
        debug!(%error, "failed to emit generic Tauri app event");
    } else {
        debug!(event = "app-event", "emitted Tauri event");
    }
}

fn tauri_event_name(event: &AppEvent) -> &'static str {
    match event {
        AppEvent::WtConnected => "wt-connected",
        AppEvent::WtDisconnected => "wt-disconnected",
        AppEvent::KillDetected { .. } => "kill-detected",
        AppEvent::ClipSaved { .. } => "clip-saved",
        AppEvent::ClipStatusChanged { .. } => "clip-status-changed",
        AppEvent::ExportProgressChanged { .. } => "export_progress_changed",
        AppEvent::ClipFailed { .. } => "clip-failed",
        AppEvent::BufferProgress { .. } => "buffer-progress",
        AppEvent::DiskUsage { .. } => "disk-usage",
        AppEvent::ClipsLoaded { .. } => "clips-loaded",
        AppEvent::DiagnosticsReady(_) => "diagnostics-ready",
    }
}

fn log_bridge_event_received(event: &AppEvent) {
    match event {
        AppEvent::WtConnected => debug!("AppEvent::WtConnected received from backend"),
        AppEvent::WtDisconnected => {
            debug!("AppEvent::WtDisconnected received from backend")
        }
        AppEvent::BufferProgress {
            filled_secs,
            total_secs,
        } => debug!(filled_secs, total_secs, "AppEvent::BufferProgress received from backend"),
        AppEvent::KillDetected {
            reason,
            description,
            ..
        } => debug!(?reason, description, "AppEvent::KillDetected received from backend"),
        AppEvent::ClipSaved { path, reason, .. } => {
            debug!(?reason, path = %path.display(), "AppEvent::ClipSaved received from backend")
        }
        AppEvent::ClipStatusChanged { payload } => {
            debug!(
                id = %payload.id,
                ?payload.status,
                ?payload.reason,
                "AppEvent::ClipStatusChanged received from backend"
            )
        }
        AppEvent::ExportProgressChanged { payload } => {
            debug!(
                active = payload.active,
                total = payload.total,
                completed = payload.completed,
                failed = payload.failed,
                progress = payload.progress,
                ?payload.current_step,
                "AppEvent::ExportProgressChanged received from backend"
            )
        }
        AppEvent::ClipFailed { message } => {
            debug!(message, "AppEvent::ClipFailed received from backend")
        }
        AppEvent::DiskUsage { used_bytes } => {
            debug!(used_bytes, "AppEvent::DiskUsage received from backend")
        }
        AppEvent::ClipsLoaded { clips, total_bytes } => {
            debug!(clips = clips.len(), total_bytes, "AppEvent::ClipsLoaded received from backend")
        }
        AppEvent::DiagnosticsReady(_) => {
            debug!("AppEvent::DiagnosticsReady received from backend")
        }
    }
}

fn update_runtime_status(runtime_status: &Arc<Mutex<RuntimeStatus>>, event: &AppEvent) {
    let Ok(mut status) = runtime_status.lock() else {
        return;
    };
    match event {
        AppEvent::WtConnected => status.wt_connected = true,
        AppEvent::WtDisconnected => status.wt_connected = false,
        AppEvent::BufferProgress {
            filled_secs,
            total_secs,
        } => {
            status.buffer_filled_secs = *filled_secs;
            status.buffer_total_secs = *total_secs;
        }
        AppEvent::KillDetected {
            reason,
            description,
            ..
        } => {
            status.recent_events.insert(
                0,
                RuntimeEvent {
                    id: runtime_event_id("kill"),
                    at: runtime_event_time(),
                    kind: *reason,
                    description: description.clone(),
                },
            );
            status.recent_events.truncate(24);
        }
        AppEvent::ClipFailed { message } => status.last_error = Some(message.clone()),
        AppEvent::ClipSaved { .. } => {
            status.clips_saved = status.clips_saved.saturating_add(1);
            status.last_error = None;
        }
        AppEvent::ClipsLoaded { clips, .. } => status.clips_saved = clips.len(),
        _ => {}
    }
}

fn runtime_event_id(prefix: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!("{prefix}-{millis}-{}", Uuid::new_v4())
}

fn runtime_event_time() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() % 86_400)
        .unwrap_or(0);
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

fn new_clip_status_payload(
    id: &str,
    status: ClipStatus,
    reason: ClipReason,
    title: impl Into<String>,
    progress: Option<u8>,
    error: Option<String>,
) -> ClipStatusPayload {
    ClipStatusPayload {
        id: id.to_owned(),
        status,
        reason,
        title: title.into(),
        created_at: Utc::now().to_rfc3339(),
        file_path: None,
        thumbnail_path: None,
        duration_seconds: None,
        size_bytes: None,
        progress,
        error,
    }
}

async fn command_loop(
    mut cmd_rx: mpsc::UnboundedReceiver<UiCommand>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    output_dir: PathBuf,
    config_path: PathBuf,
    preview_server: Arc<ClipPreviewServer>,
) {
    while let Some(command) = cmd_rx.recv().await {
        match command {
            UiCommand::LoadClips => {
                let output_dir = output_dir.clone();
                let event_tx = event_tx.clone();
                let preview_server = preview_server.clone();
                tokio::spawn(async move {
                    match scan_clips(output_dir, preview_server).await {
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
                let preview_server = preview_server.clone();
                tokio::task::spawn_blocking(move || delete_clip_files(&path))
                    .await
                    .ok();
                let _ = event_tx.send(AppEvent::ClipFailed {
                    message: "Clip supprimé".to_owned(),
                });
                tokio::spawn(async move {
                    match scan_clips(reload_dir, preview_server).await {
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
                    let clip_id = format!("clip_{}", Uuid::new_v4());
                    let _ = event_tx.send(AppEvent::ClipStatusChanged {
                        payload: new_clip_status_payload(
                            &clip_id,
                            ClipStatus::Detected,
                            ClipReason::Manual,
                            "Clip manuel détecté...",
                            Some(10),
                            None,
                        ),
                    });
                    let config = match AppConfig::load(Some(&config_path)) {
                        Ok(config) => config,
                        Err(error) => {
                            let message = error.to_string();
                            let _ = event_tx.send(AppEvent::ClipStatusChanged {
                                payload: new_clip_status_payload(
                                    &clip_id,
                                    ClipStatus::Failed,
                                    ClipReason::Manual,
                                    "Erreur pendant la création du clip",
                                    None,
                                    Some(message.clone()),
                                ),
                            });
                            let _ = event_tx.send(AppEvent::ClipFailed {
                                message: format!("Config: {message}"),
                            });
                            return;
                        }
                    };
                    if let Err(error) =
                        save_standalone_manual_clip(
                            config,
                            output_dir,
                            event_tx.clone(),
                            clip_id.clone(),
                        )
                            .await
                    {
                        let message = error.to_string();
                        let _ = event_tx.send(AppEvent::ClipStatusChanged {
                            payload: new_clip_status_payload(
                                &clip_id,
                                ClipStatus::Failed,
                                ClipReason::Manual,
                                "Erreur pendant la création du clip",
                                None,
                                Some(message.clone()),
                            ),
                        });
                        let _ = event_tx.send(AppEvent::ClipFailed {
                            message: format!("Clip manuel: {message}"),
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
    clip_id: String,
) -> anyhow::Result<()> {
    let quality = resolve_configured_video_quality(&config.clip)?;
    let mut status = new_clip_status_payload(
        &clip_id,
        ClipStatus::Recording,
        ClipReason::Manual,
        "Capture en cours...",
        Some(35),
        None,
    );
    let created_at = status.created_at.clone();
    let _ = event_tx.send(AppEvent::ClipStatusChanged {
        payload: status.clone(),
    });
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
    status.status = ClipStatus::Encoding;
    status.title = "Encodage du clip...".to_owned();
    status.progress = Some(72);
    let _ = event_tx.send(AppEvent::ClipStatusChanged {
        payload: status.clone(),
    });
    match handle.save_replay(handle.manual_clip_context()).await {
        Ok(SaveReplayOutcome::Saved(replay)) => {
            status.status = ClipStatus::Saving;
            status.title = "Sauvegarde du clip...".to_owned();
            status.progress = Some(92);
            let _ = event_tx.send(AppEvent::ClipStatusChanged {
                payload: status.clone(),
            });
            if let Some(path) = replay.final_video_path {
                let size_bytes = std::fs::metadata(&path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                status.status = ClipStatus::Ready;
                status.title = "Clip prêt".to_owned();
                status.file_path = Some(path.clone());
                status.duration_seconds = Some(config.clip.seconds);
                status.size_bytes = Some(size_bytes);
                status.progress = Some(100);
                status.created_at = created_at;
                let _ = event_tx.send(AppEvent::ClipStatusChanged { payload: status });
                let _ = event_tx.send(AppEvent::ClipSaved {
                    path,
                    reason: ClipReason::Manual,
                    duration_seconds: config.clip.seconds,
                    size_bytes,
                });
            }
        }
        Ok(SaveReplayOutcome::NotReadyYet(reason))
        | Ok(SaveReplayOutcome::SkippedTooOld(reason)) => {
            status.status = ClipStatus::Failed;
            status.title = "Erreur pendant la création du clip".to_owned();
            status.progress = None;
            status.error = Some(reason.clone());
            let _ = event_tx.send(AppEvent::ClipStatusChanged { payload: status });
            let _ = event_tx.send(AppEvent::ClipFailed {
                message: format!("Clip manuel: {reason}"),
            });
        }
        Err(error) => {
            let message = error.to_string();
            status.status = ClipStatus::Failed;
            status.title = "Erreur pendant la création du clip".to_owned();
            status.progress = None;
            status.error = Some(message.clone());
            let _ = event_tx.send(AppEvent::ClipStatusChanged { payload: status });
            let _ = event_tx.send(AppEvent::ClipFailed {
                message: format!("Clip manuel: {message}"),
            });
        }
    }
    handle.stop().await
}

async fn scan_clips(
    output_dir: PathBuf,
    preview_server: Arc<ClipPreviewServer>,
) -> anyhow::Result<(Vec<ClipInfo>, u64)> {
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
                thumbnail_path: path.with_extension("jpg").exists().then(|| path.with_extension("jpg")),
                preview_url: preview_server.url_for_path(path),
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
    } else if name.starts_with("base") {
        ClipReason::BaseDestroyed
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
