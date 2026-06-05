use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{Emitter, Manager, State};
use tokio::sync::{mpsc, oneshot};
use tracing::debug;
use uuid::Uuid;
mod export_queue;
mod editor;
mod updater;
use export_queue::{ExportSummary, PendingClipExportDto};
use wt_clipper::{
    app::auto::{run_auto_clip, AutoClipCommand, AutoClipConfig, RuntimeConfigUpdateResult},
    capture::{
        buffer::{BufferHealth, ClipReason},
        gpu_screen_recorder::{GsrHealth, GsrStatus},
        quality::VideoQuality,
    },
    config::{default_config_path, AppConfig, CaptureBackend, ClipConfig, ClipExportMode},
    doctor::{self, DoctorReport},
    ui::bridge::{AppEvent, ClipInfo, ExportProgressPayload, UiCommand},
    warthunder::client::WarThunderClient,
};

struct BackendState {
    cmd_tx: mpsc::UnboundedSender<UiCommand>,
    auto_cmd_tx: mpsc::UnboundedSender<AutoClipCommand>,
    config_path: PathBuf,
    runtime_status: Arc<Mutex<RuntimeStatus>>,
    preview_server: Arc<ClipPreviewServer>,
    shutdown_requested: Arc<AtomicBool>,
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
        let extension = canonical
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_else(|| "webm".to_owned());
        if let Ok(mut clips) = self.clips.lock() {
            clips.insert(id.clone(), canonical);
        }
        Some(format!("{}/{}.{}", self.base_url, id, extension))
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

    let path = path.trim_start_matches('/');
    let Some((id, extension)) = path.rsplit_once('.') else {
        write_http_status(&mut stream, "404 Not Found");
        return;
    };
    if id.is_empty() || !matches!(extension, "webm" | "mp4") {
        write_http_status(&mut stream, "404 Not Found");
        return;
    }
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
    let content_type = video_content_type(&clip_path);
    let headers = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nAccept-Ranges: bytes\r\n{content_range}Content-Length: {length}\r\nConnection: close\r\n\r\n"
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

fn video_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("mp4") => "video/mp4",
        _ => "video/webm",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeStatus {
    wt_connected: bool,
    active_capture_backend: CaptureBackend,
    buffer_filled_secs: f32,
    buffer_total_secs: f32,
    buffer_health: BufferHealth,
    buffer_last_segment_path: Option<PathBuf>,
    buffer_last_segment_modified_at: Option<String>,
    buffer_last_segment_age_secs: Option<u64>,
    buffer_restart_count: u64,
    last_gstreamer_error: Option<String>,
    gsr_available: bool,
    gsr_health: GsrHealth,
    gsr_pid: Option<u32>,
    gsr_wrapper_pid: Option<u32>,
    gsr_recorder_pid: Option<u32>,
    gsr_signal_pid: Option<u32>,
    gsr_mode: Option<String>,
    gsr_target: String,
    gsr_monitors: Vec<String>,
    gsr_command_line: Option<String>,
    gsr_recorder_command_line: Option<String>,
    gsr_stderr_handling: String,
    gsr_save_queue_len: usize,
    gsr_total_saves_requested: u64,
    gsr_total_saves_completed: u64,
    gsr_total_saves_failed: u64,
    gsr_output_dir: Option<PathBuf>,
    gsr_output_prefix: Option<PathBuf>,
    gsr_last_output: Option<PathBuf>,
    gsr_last_error: Option<String>,
    gsr_restart_count: u64,
    gsr_replay_seconds: u64,
    gsr_fps: u32,
    gsr_quality: String,
    auto_clip_running: bool,
    active_export_mode: ClipExportMode,
    config_restart_required: bool,
    pending_export_count: usize,
    pending_export_dir: PathBuf,
    pending_export_bytes: u64,
    clips_saved: usize,
    recent_events: Vec<RuntimeEvent>,
    export_progress: Option<ExportProgressPayload>,
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

#[derive(Debug, Deserialize)]
struct ClipMetadataInfo {
    reason: Option<ClipReason>,
    duration_seconds: Option<f64>,
}

impl RuntimeStatus {
    fn from_config(config: &AppConfig) -> Self {
        Self {
            wt_connected: false,
            active_capture_backend: config.capture.backend,
            buffer_filled_secs: 0.0,
            buffer_total_secs: config.clip.seconds as f32,
            buffer_health: BufferHealth::Starting,
            buffer_last_segment_path: None,
            buffer_last_segment_modified_at: None,
            buffer_last_segment_age_secs: None,
            buffer_restart_count: 0,
            last_gstreamer_error: None,
            gsr_available: false,
            gsr_health: GsrHealth::Stopped,
            gsr_pid: None,
            gsr_wrapper_pid: None,
            gsr_recorder_pid: None,
            gsr_signal_pid: None,
            gsr_mode: None,
            gsr_target: config.capture.target.clone(),
            gsr_monitors: Vec::new(),
            gsr_command_line: None,
            gsr_recorder_command_line: None,
            gsr_stderr_handling: "null".to_owned(),
            gsr_save_queue_len: 0,
            gsr_total_saves_requested: 0,
            gsr_total_saves_completed: 0,
            gsr_total_saves_failed: 0,
            gsr_output_dir: config.capture.output_dir_path().ok(),
            gsr_output_prefix: config.capture.output_dir_path().ok().map(|path| path.join("wtclip")),
            gsr_last_output: None,
            gsr_last_error: None,
            gsr_restart_count: 0,
            gsr_replay_seconds: config.capture.replay_seconds,
            gsr_fps: config.capture.fps,
            gsr_quality: config.capture.quality.as_arg().to_owned(),
            auto_clip_running: false,
            active_export_mode: config.clip.export_mode,
            config_restart_required: false,
            pending_export_count: 0,
            pending_export_dir: config
                .pending_exports
                .pending_export_dir_path()
                .unwrap_or_else(|_| PathBuf::from("")),
            pending_export_bytes: 0,
            clips_saved: 0,
            recent_events: Vec::new(),
            export_progress: None,
            last_error: None,
        }
    }
}

#[tauri::command]
fn get_config(state: State<'_, BackendState>) -> Result<AppConfig, String> {
    AppConfig::load(Some(&state.config_path)).map_err(|error| error.to_string())
}

#[tauri::command]
async fn save_config(
    config: AppConfig,
    state: State<'_, BackendState>,
) -> Result<RuntimeConfigUpdateResult, String> {
    write_config(&state.config_path, &config).map_err(|error| error.to_string())?;
    let status_config = config.clone();
    let (respond_to, response) = oneshot::channel();
    state
        .auto_cmd_tx
        .send(AutoClipCommand::UpdateConfig { config, respond_to })
        .map_err(|error| error.to_string())?;
    let result = response.await.map_err(|error| error.to_string())?;
    if let Ok(mut status) = state.runtime_status.lock() {
        status.active_capture_backend = status_config.capture.backend;
        status.active_export_mode = result.active_export_mode;
        status.config_restart_required = result.restart_required;
        status.gsr_target = status_config.capture.target.clone();
        status.gsr_replay_seconds = status_config.capture.replay_seconds;
        status.gsr_fps = status_config.capture.fps;
        status.gsr_quality = status_config.capture.quality.as_arg().to_owned();
        status.gsr_output_dir = status_config.capture.output_dir_path().ok();
        status.gsr_output_prefix = status.gsr_output_dir.as_ref().map(|path| path.join("wtclip"));
        status.pending_export_dir = status_config
            .pending_exports
            .pending_export_dir_path()
            .unwrap_or_else(|_| status.pending_export_dir.clone());
        status.pending_export_bytes = directory_size(&status.pending_export_dir);
        status.last_error = None;
    }
    Ok(result)
}

#[tauri::command]
async fn load_clips(state: State<'_, BackendState>) -> Result<Vec<ClipInfo>, String> {
    let config = AppConfig::load(Some(&state.config_path)).map_err(|error| error.to_string())?;
    let roots = gallery_scan_roots_for_config(&config).map_err(|error| error.to_string())?;
    let (clips, _) = scan_clips(roots, state.preview_server.clone())
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
async fn test_gsr_save_replay(state: State<'_, BackendState>) -> Result<String, String> {
    let (respond_to, response) = oneshot::channel();
    state
        .auto_cmd_tx
        .send(AutoClipCommand::TestGsrSaveReplay { respond_to })
        .map_err(|error| error.to_string())?;
    response
        .await
        .map_err(|error| error.to_string())?
        .map(|path| path.to_string_lossy().into_owned())
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
    let clips = response.await.map_err(|error| error.to_string())?;
    if let Ok(mut status) = state.runtime_status.lock() {
        status.pending_export_count = clips.len();
        status.pending_export_bytes = directory_size(&status.pending_export_dir);
    }
    Ok(clips)
}

#[tauri::command]
async fn delete_pending_export_clip(id: String, state: State<'_, BackendState>) -> Result<(), String> {
    let (respond_to, response) = oneshot::channel();
    state
        .auto_cmd_tx
        .send(AutoClipCommand::DeletePendingClip { id, respond_to })
        .map_err(|error| error.to_string())?;
    response.await.map_err(|error| error.to_string())?
}

#[tauri::command]
fn restart_replay_buffer(state: State<'_, BackendState>) -> Result<(), String> {
    state
        .cmd_tx
        .send(UiCommand::RestartReplayBuffer)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn restart_buffer(state: State<'_, BackendState>) -> Result<(), String> {
    restart_replay_buffer(state)
}

#[tauri::command]
fn get_runtime_status(state: State<'_, BackendState>) -> Result<RuntimeStatus, String> {
    state
        .runtime_status
        .lock()
        .map(|status| status.clone())
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn export_edited_clip(
    request: editor::ClipEditRequest,
    app: tauri::AppHandle,
    state: State<'_, BackendState>,
) -> Result<editor::EditedClipResult, String> {
    let config = AppConfig::load(Some(&state.config_path)).map_err(|error| error.to_string())?;
    let result = editor::export_edited_clip(request, config, app).await?;
    let _ = state.cmd_tx.send(UiCommand::LoadClips);
    Ok(result)
}

#[tauri::command]
async fn get_clip_media_info(path: String) -> Result<editor::ClipMediaInfo, String> {
    editor::get_clip_media_info(path).await
}

#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    editor::open_path(path)
}

#[tauri::command]
fn open_parent_folder(path: String) -> Result<(), String> {
    editor::open_parent_folder(path)
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
            let shutdown_requested = Arc::new(AtomicBool::new(false));
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
                shutdown_requested,
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
            test_gsr_save_replay,
            export_pending_clips,
            get_pending_export_clips,
            delete_pending_export_clip,
            restart_replay_buffer,
            restart_buffer,
            updater::check_for_updates,
            get_runtime_status,
            export_edited_clip,
            get_clip_media_info,
            open_path,
            open_parent_folder
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<BackendState>();
                if !state.shutdown_requested.swap(true, Ordering::SeqCst) {
                    api.prevent_close();
                    let auto_cmd_tx = state.auto_cmd_tx.clone();
                    let window = window.clone();
                    std::thread::spawn(move || {
                        let _ = auto_cmd_tx.send(AutoClipCommand::Shutdown);
                        std::thread::sleep(Duration::from_secs(4));
                        let _ = window.close();
                    });
                }
            }
        })
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
    let returned_auto_cmd_tx = auto_cmd_tx.clone();
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
            let command_auto_tx = auto_cmd_tx.clone();
            tokio::spawn(async move {
                command_loop(
                    cmd_rx,
                    command_events,
                    command_auto_tx,
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
    BackendChannels {
        cmd_tx,
        auto_cmd_tx: returned_auto_cmd_tx,
    }
}

async fn run_auto_backend(
    config: AppConfig,
    output_dir: PathBuf,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    auto_cmd_rx: mpsc::UnboundedReceiver<AutoClipCommand>,
) -> anyhow::Result<()> {
    let client = WarThunderClient::new(config.war_thunder.clone())?;
    let quality_preset = config.clip.quality;
    let mut quality = resolve_configured_video_quality(&config.clip)?;
    let capture_backend = config.capture.backend;
    if capture_backend == CaptureBackend::GpuScreenRecorder {
        quality.fps = config.capture.fps;
    }
    let buffer_seconds = if capture_backend == CaptureBackend::GpuScreenRecorder {
        config.capture.replay_seconds
    } else {
        config.clip.seconds
    };
    let export_mode = if capture_backend == CaptureBackend::GpuScreenRecorder {
        ClipExportMode::Instant
    } else {
        config.clip.export_mode
    };
    run_auto_clip(
        client,
        config.war_thunder.clone(),
        AutoClipConfig {
            buffer_seconds,
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
            export_mode,
            pending_export_dir: config.pending_exports.pending_export_dir_path()?,
            delete_ready_after_export: config.pending_exports.delete_ready_after_export,
            command_rx: Some(auto_cmd_rx),
            capture: config.capture.clone(),
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
        AppEvent::BufferProgress { status } => app.emit("buffer-progress", status),
        AppEvent::GsrStatusChanged { status } => app.emit("gsr-status-changed", status),
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
        AppEvent::GsrStatusChanged { .. } => "gsr-status-changed",
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
        AppEvent::BufferProgress { status } => debug!(
            health = ?status.health,
            filled_secs = status.filled_secs,
            total_secs = status.total_secs,
            restart_count = status.restart_count,
            "AppEvent::BufferProgress received from backend"
        ),
        AppEvent::GsrStatusChanged { status } => debug!(
            health = ?status.health,
            pid = ?status.pid,
            command = ?status.command_line,
            "AppEvent::GsrStatusChanged received from backend"
        ),
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
                ?payload.current_clip_number,
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
        AppEvent::BufferProgress { status: buffer_status } => {
            status.buffer_filled_secs = buffer_status.filled_secs;
            status.buffer_total_secs = buffer_status.total_secs;
            status.buffer_health = buffer_status.health;
            status.buffer_last_segment_path = buffer_status.last_segment_path.clone();
            status.buffer_last_segment_modified_at = buffer_status.last_segment_modified_at.clone();
            status.buffer_last_segment_age_secs = buffer_status.last_segment_age_secs;
            status.buffer_restart_count = buffer_status.restart_count;
            status.last_gstreamer_error = buffer_status.last_gstreamer_error.clone();
        }
        AppEvent::GsrStatusChanged { status: gsr } => {
            apply_gsr_runtime_status(&mut status, gsr);
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
        AppEvent::ExportProgressChanged { payload } => {
            status.export_progress = Some(payload.clone());
        }
        AppEvent::ClipsLoaded { clips, .. } => status.clips_saved = clips.len(),
        _ => {}
    }
}

fn apply_gsr_runtime_status(status: &mut RuntimeStatus, gsr: &GsrStatus) {
    status.gsr_available = gsr.available;
    status.gsr_health = gsr.health;
    status.gsr_pid = gsr.pid;
    status.gsr_wrapper_pid = gsr.wrapper_pid;
    status.gsr_recorder_pid = gsr.recorder_pid;
    status.gsr_signal_pid = gsr.signal_pid;
    status.gsr_mode = gsr.mode.map(|mode| format!("{mode:?}").to_ascii_lowercase());
    status.gsr_target = gsr.target.clone();
    status.gsr_monitors = gsr.monitors.clone();
    status.gsr_command_line = gsr.command_line.clone();
    status.gsr_recorder_command_line = gsr.recorder_command_line.clone();
    status.gsr_stderr_handling = gsr.stderr_handling.clone();
    status.gsr_save_queue_len = gsr.save_queue_len;
    status.gsr_total_saves_requested = gsr.total_saves_requested;
    status.gsr_total_saves_completed = gsr.total_saves_completed;
    status.gsr_total_saves_failed = gsr.total_saves_failed;
    status.gsr_output_dir = Some(gsr.output_dir.clone());
    status.gsr_output_prefix = Some(gsr.output_prefix.clone());
    status.gsr_last_output = gsr.last_output.clone();
    status.gsr_last_error = gsr.last_error.clone();
    status.gsr_restart_count = gsr.restart_count;
    status.gsr_replay_seconds = gsr.replay_seconds;
    status.gsr_fps = gsr.fps;
    status.gsr_quality = gsr.quality.clone();
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

fn directory_size(path: &Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.metadata().ok())
        .filter(|metadata| metadata.is_file())
        .map(|metadata| metadata.len())
        .sum()
}

async fn command_loop(
    mut cmd_rx: mpsc::UnboundedReceiver<UiCommand>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    auto_cmd_tx: mpsc::UnboundedSender<AutoClipCommand>,
    output_dir: PathBuf,
    config_path: PathBuf,
    preview_server: Arc<ClipPreviewServer>,
) {
    while let Some(command) = cmd_rx.recv().await {
        match command {
            UiCommand::LoadClips => {
                let output_dir = output_dir.clone();
                let config_path = config_path.clone();
                let event_tx = event_tx.clone();
                let preview_server = preview_server.clone();
                tokio::spawn(async move {
                    let roots = AppConfig::load(Some(&config_path))
                        .ok()
                        .and_then(|config| gallery_scan_roots_for_config(&config).ok())
                        .unwrap_or_else(|| gallery_scan_roots_for_base_dirs(vec![output_dir]));
                    match scan_clips(roots, preview_server).await {
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
                    match scan_clips(gallery_scan_roots_for_base_dirs(vec![reload_dir]), preview_server).await {
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
                let runtime_config = config.clone();
                let result = tokio::task::spawn_blocking(move || write_config(&path, &config))
                    .await
                    .ok()
                    .and_then(Result::ok);
                let message = if result.is_some() {
                    let (respond_to, response) = oneshot::channel();
                    match auto_cmd_tx.send(AutoClipCommand::UpdateConfig {
                        config: runtime_config,
                        respond_to,
                    }) {
                        Ok(()) => response
                            .await
                            .map(|result| result.message)
                            .unwrap_or_else(|error| format!("Configuration sauvegardée; backend: {error}")),
                        Err(error) => format!("Configuration sauvegardée; backend: {error}"),
                    }
                } else {
                    "La configuration n'a pas pu être sauvegardée".to_owned()
                };
                let _ = event_tx.send(AppEvent::ClipFailed {
                    message,
                });
            }
            UiCommand::SaveManualClip => {
                if let Err(error) = auto_cmd_tx.send(AutoClipCommand::SaveManualClip) {
                    let _ = event_tx.send(AppEvent::ClipFailed {
                        message: format!("Clip manuel: {error}"),
                    });
                }
            }
            UiCommand::RestartReplayBuffer | UiCommand::RestartBuffer => {
                if let Err(error) = auto_cmd_tx.send(AutoClipCommand::RestartReplayBuffer) {
                    let _ = event_tx.send(AppEvent::ClipFailed {
                        message: format!("Redémarrage buffer: {error}"),
                    });
                }
            }
        }
    }
}

async fn scan_clips(
    roots: Vec<PathBuf>,
    preview_server: Arc<ClipPreviewServer>,
) -> anyhow::Result<(Vec<ClipInfo>, u64)> {
    tokio::task::spawn_blocking(move || {
        let mut clips = Vec::new();
        let mut total_bytes = 0_u64;
        let mut seen = std::collections::HashSet::new();
        let now = std::time::SystemTime::now();
        for root in roots {
            for entry in walkdir::WalkDir::new(&root)
                .into_iter()
                .filter_map(Result::ok)
            {
                let path = entry.path();
                if !is_supported_gallery_video(path) {
                    continue;
                }
                let dedupe_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                if !seen.insert(dedupe_path) {
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
                let metadata_info = read_clip_metadata_info(&path.with_extension("json"));
                clips.push(ClipInfo {
                    reason: metadata_info
                        .as_ref()
                        .and_then(|metadata| metadata.reason)
                        .unwrap_or_else(|| clip_reason_from_name(&file_name)),
                    path: path.to_path_buf(),
                    thumbnail_path: path.with_extension("jpg").exists().then(|| path.with_extension("jpg")),
                    preview_url: preview_server.url_for_path(path),
                    file_name,
                    size_bytes,
                    duration_seconds: metadata_info
                        .and_then(|metadata| metadata.duration_seconds)
                        .map(|duration| duration.max(0.0).round() as u64)
                        .unwrap_or(0),
                    modified_secs_ago,
                });
            }
        }
        clips.sort_by_key(|clip| clip.modified_secs_ago);
        Ok((clips, total_bytes))
    })
    .await?
}

fn gallery_scan_roots_for_config(config: &AppConfig) -> anyhow::Result<Vec<PathBuf>> {
    let mut base_dirs = vec![config.clip.output_dir_path()?];
    if config.capture.backend == CaptureBackend::GpuScreenRecorder {
        base_dirs.push(config.capture.output_dir_path()?);
    }
    Ok(gallery_scan_roots_for_base_dirs(base_dirs))
}

fn gallery_scan_roots_for_base_dirs(base_dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for output_dir in base_dirs {
        for root in [
            output_dir.clone(),
            output_dir.join("Edited"),
            output_dir.join("Social"),
        ] {
            let key = root.clone();
            if seen.insert(key) {
                roots.push(root);
            }
        }
    }
    roots
}

fn is_supported_gallery_video(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("webm" | "mp4")
    )
}

fn read_clip_metadata_info(path: &Path) -> Option<ClipMetadataInfo> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
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
