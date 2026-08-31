use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, SystemTime},
};

use anyhow::Context;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::{sync::Mutex, time::sleep};
use tracing::{debug, info, warn};

use crate::{
    app::clip_types::{ClipContext, ClipReason},
    capture::output::slugify_filename_part,
    capture::x11_window::detect_war_thunder_window_x11,
    config::{CaptureConfig, CaptureStrategy, GpuScreenRecorderMode, GsrBitrateMode},
    games::event::GameEvent,
};

const FLATPAK_APP_ID: &str = "com.dec05eba.gpu_screen_recorder";
const REPLAY_SAVE_TIMEOUT: Duration = Duration::from_secs(30);
const STABLE_FILE_DURATION: Duration = Duration::from_millis(750);
const STABLE_FILE_POLL: Duration = Duration::from_millis(250);
const STOP_TIMEOUT: Duration = Duration::from_secs(3);
pub const WAITING_FOR_WAR_THUNDER_TARGET_REASON: &str = "waiting for War Thunder localhost";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GsrMode {
    Native,
    Flatpak,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GsrHealth {
    NotAvailable,
    Stopped,
    Starting,
    Running,
    SavingReplay,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GsrStatus {
    pub available: bool,
    pub mode: Option<GsrMode>,
    pub health: GsrHealth,
    pub pid: Option<u32>,
    pub wrapper_pid: Option<u32>,
    pub recorder_pid: Option<u32>,
    pub signal_pid: Option<u32>,
    pub recorder_command_line: Option<String>,
    pub stderr_handling: String,
    pub save_queue_len: usize,
    pub total_saves_requested: u64,
    pub total_saves_completed: u64,
    pub total_saves_failed: u64,
    pub target: String,
    pub target_valid: bool,
    pub monitors: Vec<String>,
    pub capture_strategy: String,
    pub session_type: String,
    pub target_reason: String,
    pub command_line: Option<String>,
    pub output_dir: PathBuf,
    pub output_prefix: PathBuf,
    pub last_output: Option<PathBuf>,
    pub last_error: Option<String>,
    pub restart_count: u64,
    pub replay_seconds: u64,
    pub fps: u32,
    pub quality: String,
    pub bitrate_mode: String,
    pub frame_rate_mode: String,
    pub keyframe_interval_seconds: f32,
    pub restart_replay_on_save: bool,
    pub video_bitrate_kbps: u32,
    pub effective_q_argument: String,
    pub codec: String,
    pub container: String,
    pub encoder: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GsrCommandLine {
    pub mode: GsrMode,
    pub program: String,
    pub args: Vec<String>,
    pub command_line: String,
    pub output_dir: PathBuf,
    pub output_prefix: PathBuf,
    pub target: String,
    pub capture_strategy: CaptureStrategy,
    pub session_type: String,
    pub target_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedGsrReplay {
    pub final_video_path: PathBuf,
    pub metadata_path: PathBuf,
    pub thumbnail_path: Option<PathBuf>,
    pub duration_seconds: u64,
    pub size_bytes: u64,
}

impl SavedGsrReplay {}

pub struct GpuScreenRecorderHandle {
    inner: Arc<Mutex<GpuScreenRecorderInner>>,
    save_replay_in_progress: Arc<Mutex<()>>,
}

struct GpuScreenRecorderInner {
    config: CaptureConfig,
    state: GpuScreenRecorderState,
    child: Option<Child>,
}

#[derive(Debug, Clone)]
pub struct GpuScreenRecorderState {
    pub mode: Option<GsrMode>,
    pub wrapper_pid: Option<u32>,
    pub recorder_pid: Option<u32>,
    pub health: GsrHealth,
    pub target: String,
    pub target_valid: bool,
    pub capture_strategy: CaptureStrategy,
    pub session_type: String,
    pub target_reason: String,
    pub output_dir: PathBuf,
    pub output_prefix: PathBuf,
    pub last_output: Option<PathBuf>,
    pub last_error: Option<String>,
    pub restart_count: u64,
    pub command_line: Option<String>,
    pub recorder_command_line: Option<String>,
    pub monitors: Vec<String>,
    pub stderr_handling: String,
    pub save_queue_len: usize,
    pub total_saves_requested: u64,
    pub total_saves_completed: u64,
    pub total_saves_failed: u64,
}

#[derive(Debug, Serialize)]
struct GsrClipMetadata {
    id: String,
    created_by: &'static str,
    capture_backend: &'static str,
    kind: &'static str,
    reason: &'static str,
    clip_type: &'static str,
    title: String,
    game: &'static str,
    player_name: Option<String>,
    raw_event: Option<String>,
    path: String,
    source_path: String,
    video_path: String,
    thumbnail_path: Option<String>,
    duration_seconds: u64,
    codec: &'static str,
    container: &'static str,
    fps: u32,
    created_at: String,
    event: Option<serde_json::Value>,
    events: Option<Vec<serde_json::Value>>,
    kill_count: Option<usize>,
    quality: &'static str,
    target: String,
}

impl GpuScreenRecorderHandle {
    pub fn new(config: CaptureConfig) -> Self {
        let output_dir = config
            .output_dir_path()
            .unwrap_or_else(|_| PathBuf::from(&config.output_dir));
        let output_prefix = output_dir.join("wtclip");
        let state = GpuScreenRecorderState {
            mode: None,
            wrapper_pid: None,
            recorder_pid: None,
            health: GsrHealth::Stopped,
            target: config.target.clone(),
            target_valid: false,
            capture_strategy: config.capture_strategy,
            session_type: session_type(),
            target_reason: "not resolved yet".to_owned(),
            output_dir,
            output_prefix,
            last_output: None,
            last_error: None,
            restart_count: 0,
            command_line: None,
            recorder_command_line: None,
            monitors: list_monitors(&config),
            stderr_handling: "null".to_owned(),
            save_queue_len: 0,
            total_saves_requested: 0,
            total_saves_completed: 0,
            total_saves_failed: 0,
        };
        Self {
            inner: Arc::new(Mutex::new(GpuScreenRecorderInner {
                config,
                state,
                child: None,
            })),
            save_replay_in_progress: Arc::new(Mutex::new(())),
        }
    }

    pub async fn mark_waiting_for_war_thunder(&self) {
        let mut inner = self.inner.lock().await;
        let monitors = inner.state.monitors.clone();
        let target = select_effective_target(&inner.config.target, &monitors);
        let target_valid = target_is_valid(&target, &monitors);
        let mode = resolve_mode(inner.config.gpu_screen_recorder_mode).ok();
        inner.child = None;
        inner.state.wrapper_pid = None;
        inner.state.recorder_pid = None;
        inner.state.health = GsrHealth::Stopped;
        inner.state.capture_strategy = inner.config.capture_strategy;
        inner.state.session_type = session_type();
        inner.state.target = target;
        inner.state.target_reason = WAITING_FOR_WAR_THUNDER_TARGET_REASON.to_owned();
        inner.state.command_line = None;
        inner.state.recorder_command_line = None;
        inner.state.target_valid = target_valid;
        if inner.state.mode.is_none() {
            inner.state.mode = mode;
        }
    }

    pub async fn start(&self) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if child_is_running(inner.child.as_mut()) {
            inner.state.health = GsrHealth::Running;
            return Ok(());
        }
        inner.child = None;
        inner.state.health = GsrHealth::Starting;
        inner.state.last_error = None;
        refresh_monitors_and_effective_target(&mut inner);

        let command_line = match build_gsr_command(&inner.config) {
            Ok(command_line) => command_line,
            Err(error) => {
                let message = error.to_string();
                inner.state.health = GsrHealth::Error;
                inner.state.last_error = Some(message.clone());
                return Err(anyhow::anyhow!(message));
            }
        };

        fs::create_dir_all(&command_line.output_dir).with_context(|| {
            format!(
                "failed to create GSR output directory {}",
                command_line.output_dir.display()
            )
        })?;

        let mut command = Command::new(&command_line.program);
        command
            .args(&command_line.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        println!(
            "[CAPTURE] strategy={:?} session={} target={} reason={}",
            command_line.capture_strategy,
            command_line.session_type,
            command_line.target,
            command_line.target_reason
        );
        info!(command = %command_line.command_line, "starting GPU Screen Recorder");
        match command.spawn() {
            Ok(mut child) => {
                let child_pid = child.id();
                println!("[GPU_RECORDER] wrapper_pid={child_pid}");
                let resolved_target = command_line.target.clone();
                let (recorder_pid, recorder_command_line) = match command_line.mode {
                    GsrMode::Flatpak => {
                        match resolve_actual_gsr_pid(
                            &inner.config,
                            &command_line.output_dir,
                            &command_line.output_prefix,
                            &resolved_target,
                        )
                        .await
                        {
                            Ok(process) => (process.pid, Some(process.command_line)),
                            Err(error) => {
                                let message = error.to_string();
                                inner.state.health = GsrHealth::Error;
                                inner.state.last_error = Some(message.clone());
                                if let Err(stop_error) = signal_process(child_pid, libc::SIGTERM) {
                                    debug!(
                                        %stop_error,
                                        child_pid,
                                        "failed to stop GSR wrapper after PID resolution failure"
                                    );
                                }
                                let _ = child.try_wait();
                                return Err(anyhow::anyhow!(message));
                            }
                        }
                    }
                    GsrMode::Native => (child_pid, Some(command_line.command_line.clone())),
                };
                println!("[GPU_RECORDER] resolved recorder_pid={recorder_pid}");
                inner.state.mode = Some(command_line.mode);
                inner.state.wrapper_pid = Some(child_pid);
                inner.state.recorder_pid = Some(recorder_pid);
                inner.state.health = GsrHealth::Running;
                inner.state.target = command_line.target.clone();
                inner.state.target_valid =
                    target_is_valid(&command_line.target, &inner.state.monitors);
                inner.state.capture_strategy = command_line.capture_strategy;
                inner.state.session_type = command_line.session_type.clone();
                inner.state.target_reason = command_line.target_reason.clone();
                inner.state.command_line = Some(command_line.command_line);
                inner.state.recorder_command_line = recorder_command_line;
                inner.state.output_dir = command_line.output_dir;
                inner.state.output_prefix = command_line.output_prefix;
                inner.state.stderr_handling = "null".to_owned();
                inner.child = Some(child);
                Ok(())
            }
            Err(error) => {
                let message = format!("GPU Screen Recorder start failed: {error}");
                inner.state.health = GsrHealth::Error;
                inner.state.last_error = Some(message.clone());
                Err(anyhow::anyhow!(message))
            }
        }
    }

    pub async fn stop(&self) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        let recorder_pid = inner.state.recorder_pid;
        stop_child(&mut inner.child).await?;
        stop_recorder_pid(recorder_pid).await?;
        inner.state.wrapper_pid = None;
        inner.state.recorder_pid = None;
        inner.state.health = GsrHealth::Stopped;
        Ok(())
    }

    pub async fn restart(
        &self,
        config: Option<CaptureConfig>,
        reason: impl Into<String>,
    ) -> anyhow::Result<()> {
        let reason = reason.into();
        {
            let mut inner = self.inner.lock().await;
            if let Some(config) = config {
                inner.config = config;
            }
            inner.state.health = GsrHealth::Starting;
            inner.state.last_error = Some(reason.clone());
            inner.state.restart_count = inner.state.restart_count.saturating_add(1);
            let recorder_pid = inner.state.recorder_pid;
            stop_child(&mut inner.child).await?;
            stop_recorder_pid(recorder_pid).await?;
            inner.state.wrapper_pid = None;
            inner.state.recorder_pid = None;
        }
        info!(reason = %reason, "restarting GPU Screen Recorder");
        self.start().await
    }

    pub async fn update_config_without_restart(&self, config: CaptureConfig) {
        let mut inner = self.inner.lock().await;
        inner.config = config;
        refresh_monitors_snapshot(&mut inner);
    }

    pub async fn update_config_and_restart_if_needed(
        &self,
        config: CaptureConfig,
    ) -> anyhow::Result<bool> {
        let changed = {
            let inner = self.inner.lock().await;
            inner.config != config
        };
        if changed {
            self.restart(Some(config), "configuration GPU Screen Recorder modifiée")
                .await?;
        }
        Ok(changed)
    }

    pub async fn refresh_status(&self) -> GsrStatus {
        let mut inner = self.inner.lock().await;
        if let Some(child) = inner.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let message = format!("GPU Screen Recorder exited: {status}");
                    inner.child = None;
                    inner.state.wrapper_pid = None;
                    inner.state.recorder_pid = None;
                    inner.state.health = GsrHealth::Error;
                    inner.state.last_error = Some(message);
                }
                Ok(None) if inner.state.health != GsrHealth::SavingReplay => {
                    inner.state.health = GsrHealth::Running;
                    inner.state.wrapper_pid = inner.child.as_ref().map(Child::id);
                    if let Some(recorder_pid) = inner.state.recorder_pid {
                        if !process_alive(recorder_pid) {
                            inner.state.recorder_pid = None;
                            inner.state.health = GsrHealth::Error;
                            inner.state.last_error =
                                Some("GPU Screen Recorder recorder process exited".to_owned());
                        }
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    inner.state.health = GsrHealth::Error;
                    inner.state.last_error = Some(error.to_string());
                }
            }
        } else if inner.state.health != GsrHealth::Error {
            inner.state.health = GsrHealth::Stopped;
            inner.state.wrapper_pid = None;
            inner.state.recorder_pid = None;
        }
        refresh_monitors_snapshot(&mut inner);
        status_from_inner(&inner)
    }

    pub async fn status(&self) -> GsrStatus {
        let mut inner = self.inner.lock().await;
        refresh_monitors_snapshot(&mut inner);
        status_from_inner(&inner)
    }

    pub async fn save_replay(&self, context: ClipContext) -> anyhow::Result<SavedGsrReplay> {
        let request_id = context
            .pending_clip_id
            .clone()
            .unwrap_or_else(|| "unknown".to_owned());
        println!(
            "[GPU_RECORDER] save_replay requested id={} reason={}",
            request_id,
            context.reason.slug()
        );
        {
            let mut inner = self.inner.lock().await;
            inner.state.total_saves_requested = inner.state.total_saves_requested.saturating_add(1);
            inner.state.save_queue_len = inner.state.save_queue_len.saturating_add(1);
        }
        let _save_guard = self.save_replay_in_progress.lock().await;
        {
            let mut inner = self.inner.lock().await;
            inner.state.save_queue_len = inner.state.save_queue_len.saturating_sub(1);
        }
        let result = self.save_replay_inner(context).await;
        if let Err(error) = &result {
            println!("[GPU_RECORDER] save failed id={request_id} error={error}");
            let mut inner = self.inner.lock().await;
            inner.state.health = GsrHealth::Error;
            inner.state.last_error = Some(error.to_string());
            inner.state.total_saves_failed = inner.state.total_saves_failed.saturating_add(1);
        } else {
            let mut inner = self.inner.lock().await;
            inner.state.total_saves_completed = inner.state.total_saves_completed.saturating_add(1);
        }
        result
    }

    async fn save_replay_inner(&self, context: ClipContext) -> anyhow::Result<SavedGsrReplay> {
        let (recorder_pid, output_dir, extension, config) = {
            let mut inner = self.inner.lock().await;
            ensure_running(&mut inner)?;
            let recorder_pid = signal_pid_from_state(&inner.state)?;
            if !process_alive(recorder_pid) {
                anyhow::bail!("GPU Screen Recorder recorder_pid {recorder_pid} n'est plus vivant");
            }
            inner.state.health = GsrHealth::SavingReplay;
            (
                recorder_pid,
                inner.state.output_dir.clone(),
                inner.config.container.extension().to_owned(),
                inner.config.clone(),
            )
        };

        let before = scan_replay_outputs_set(&output_dir, &extension)?;
        println!(
            "[GPU_RECORDER] scan before count={} output_dir={}",
            before.len(),
            output_dir.display()
        );
        println!("[GPU_RECORDER] sending SIGUSR1 recorder_pid={recorder_pid}");
        signal_process(recorder_pid, libc::SIGUSR1).with_context(|| {
            format!(
                "impossible de demander la sauvegarde du replay au recorder_pid GSR {recorder_pid}"
            )
        })?;
        println!("[GPU_RECORDER] SIGUSR1 sent successfully recorder_pid={recorder_pid}");
        println!("[GPU_RECORDER] waiting for new {extension}...");
        let final_video_path =
            wait_for_new_replay_output(&output_dir, &extension, &before, REPLAY_SAVE_TIMEOUT)
                .await?;
        wait_until_file_stable(&final_video_path, STABLE_FILE_DURATION, REPLAY_SAVE_TIMEOUT)
            .await?;
        let size_bytes = fs::metadata(&final_video_path)
            .with_context(|| format!("clip GSR introuvable: {}", final_video_path.display()))?
            .len();
        if size_bytes == 0 {
            anyhow::bail!("clip GSR vide: {}", final_video_path.display());
        }

        let duration_seconds = probe_video_duration_seconds(&final_video_path)
            .await
            .unwrap_or(config.replay_seconds);
        let thumbnail_path = generate_thumbnail(&final_video_path).await;
        let metadata_path = final_video_path.with_extension("json");
        write_gsr_metadata(
            &metadata_path,
            &final_video_path,
            thumbnail_path.as_deref(),
            &context,
            &config,
            duration_seconds,
        )?;

        {
            let mut inner = self.inner.lock().await;
            inner.state.health = GsrHealth::Running;
            inner.state.last_output = Some(final_video_path.clone());
            inner.state.last_error = None;
        }

        Ok(SavedGsrReplay {
            final_video_path,
            metadata_path,
            thumbnail_path,
            duration_seconds,
            size_bytes,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedCaptureTarget {
    target: String,
    session_type: String,
    reason: String,
}

fn resolve_capture_target(config: &CaptureConfig) -> ResolvedCaptureTarget {
    let session = session_type();
    // Refresh monitors list to ensure it's up-to-date
    let monitors = list_monitors(config);
    tracing::debug!(monitors = ?monitors, "Refreshed GSR monitors list for target resolution");
    
    let mut fallback = select_effective_target(&config.target, &monitors);
    
    // Force a valid fallback if the resolved target is still invalid
    if !target_is_valid(&fallback, &monitors) {
        fallback = if monitors.is_empty() {
            tracing::warn!(
                original_target = %config.target,
                "No monitors available; forcing fallback to 'screen'"
            );
            "screen".to_owned()
        } else {
            let valid_fallback = monitors
                .iter()
                .find(|m| !is_special_capture_target(m))
                .cloned()
                .unwrap_or_else(|| monitors[0].clone());
            tracing::warn!(
                original_target = %config.target,
                resolved_target = %fallback,
                valid_fallback = %valid_fallback,
                monitors = ?monitors,
                "Target was invalid; forcing fallback to valid monitor"
            );
            valid_fallback
        };
    }

    match config.capture_strategy {
        CaptureStrategy::Monitor => ResolvedCaptureTarget {
            target: fallback,
            session_type: session,
            reason: "monitor strategy".to_owned(),
        },
        CaptureStrategy::Focused => ResolvedCaptureTarget {
            target: "focused".to_owned(),
            session_type: session,
            reason: "focused strategy".to_owned(),
        },
        CaptureStrategy::Portal => {
            if session == "x11" {
                ResolvedCaptureTarget {
                    target: fallback,
                    session_type: session,
                    reason: "portal unsupported on X11; fallback monitor".to_owned(),
                }
            } else {
                ResolvedCaptureTarget {
                    target: "portal".to_owned(),
                    session_type: session,
                    reason: "portal strategy".to_owned(),
                }
            }
        }
        CaptureStrategy::Auto => {
            if session == "x11" {
                if let Some(window) = detect_war_thunder_window_x11() {
                    return ResolvedCaptureTarget {
                        target: window.id_hex,
                        session_type: session,
                        reason: "War Thunder X11 window detected".to_owned(),
                    };
                }
                ResolvedCaptureTarget {
                    target: fallback,
                    session_type: session,
                    reason: "War Thunder X11 window not found; fallback monitor".to_owned(),
                }
            } else if session == "wayland" {
                ResolvedCaptureTarget {
                    target: "portal".to_owned(),
                    session_type: session,
                    reason: "Wayland auto strategy uses desktop portal after War Thunder API is reachable".to_owned(),
                }
            } else {
                ResolvedCaptureTarget {
                    target: fallback,
                    session_type: session,
                    reason: "unknown session; fallback monitor".to_owned(),
                }
            }
        }
    }
}

fn session_type() -> String {
    std::env::var("XDG_SESSION_TYPE")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if std::env::var_os("WAYLAND_DISPLAY").is_some() {
                "wayland".to_owned()
            } else if std::env::var_os("DISPLAY").is_some() {
                "x11".to_owned()
            } else {
                "unknown".to_owned()
            }
        })
}

pub fn build_gsr_command(config: &CaptureConfig) -> anyhow::Result<GsrCommandLine> {
    let output_dir = config.output_dir_path()?;
    if !output_dir.is_absolute() {
        anyhow::bail!(
            "GPU Screen Recorder output_dir must be absolute: {}",
            output_dir.display()
        );
    }
    let output_prefix = output_dir.join("wtclip");
    if !output_prefix.is_absolute() {
        anyhow::bail!(
            "GPU Screen Recorder output prefix must be absolute: {}",
            output_prefix.display()
        );
    }

    let mode = resolve_mode(config.gpu_screen_recorder_mode)?;
    // Log the monitors list for debugging
    let monitors = list_monitors(config);
    tracing::debug!(monitors = ?monitors, "Building GSR command with monitors");
    
    let resolved_target = resolve_capture_target(config);
    tracing::debug!(
        target = %resolved_target.target,
        reason = %resolved_target.reason,
        "Resolved GSR capture target"
    );
    let mut args = Vec::<String>::new();
    let program = match mode {
        GsrMode::Native => "gpu-screen-recorder".to_owned(),
        GsrMode::Flatpak => {
            args.extend([
                "run".to_owned(),
                "--command=gpu-screen-recorder".to_owned(),
                FLATPAK_APP_ID.to_owned(),
            ]);
            "flatpak".to_owned()
        }
    };

    args.extend([
        "-w".to_owned(),
        resolved_target.target.clone(),
        "-f".to_owned(),
        sanitized_fps(config.fps).to_string(),
        "-fm".to_owned(),
        config.frame_rate_mode.as_arg().to_owned(),
        "-keyint".to_owned(),
        keyframe_interval_arg(config),
        "-r".to_owned(),
        config.replay_seconds.max(1).to_string(),
        "-c".to_owned(),
        config.container.as_arg().to_owned(),
        "-k".to_owned(),
        config.codec.as_arg().to_owned(),
        "-encoder".to_owned(),
        config.encoder.as_arg().to_owned(),
    ]);
    append_bitrate_mode_args(&mut args, config);
    args.extend(["-q".to_owned(), effective_q_argument(config)]);
    args.extend([
        "-restart-replay-on-save".to_owned(),
        restart_replay_on_save_arg(config).to_owned(),
        "-ro".to_owned(),
        output_dir.display().to_string(),
        "-o".to_owned(),
        output_prefix.display().to_string(),
    ]);
    if config.audio_enabled {
        let audio_input = config.audio_input.trim();
        if audio_input.is_empty() {
            warn!("GSR audio is enabled but audio_input is empty; starting video without audio");
        } else {
            args.extend(["-a".to_owned(), audio_input.to_owned()]);
        }
    }

    let command_line = std::iter::once(program.clone())
        .chain(args.iter().cloned())
        .map(|part| shell_quote(&part))
        .collect::<Vec<_>>()
        .join(" ");

    Ok(GsrCommandLine {
        mode,
        program,
        args,
        command_line,
        output_dir,
        output_prefix,
        target: resolved_target.target,
        capture_strategy: config.capture_strategy,
        session_type: resolved_target.session_type,
        target_reason: resolved_target.reason,
    })
}

fn append_bitrate_mode_args(args: &mut Vec<String>, config: &CaptureConfig) {
    let Some(mode) = config.bitrate_mode.as_arg() else {
        return;
    };
    args.extend(["-bm".to_owned(), mode.to_owned()]);
}

fn effective_q_argument(config: &CaptureConfig) -> String {
    match config.bitrate_mode {
        GsrBitrateMode::Cbr => effective_cbr_bitrate_kbps(config).to_string(),
        GsrBitrateMode::Auto | GsrBitrateMode::Qp | GsrBitrateMode::Vbr => {
            config.quality.as_arg().to_owned()
        }
    }
}

fn effective_cbr_bitrate_kbps(config: &CaptureConfig) -> u32 {
    if config.video_bitrate_kbps == 0 {
        20_000
    } else {
        config.video_bitrate_kbps
    }
}

fn restart_replay_on_save_arg(config: &CaptureConfig) -> &'static str {
    if config.restart_replay_on_save {
        "yes"
    } else {
        "no"
    }
}

fn keyframe_interval_arg(config: &CaptureConfig) -> String {
    let value = if config.keyframe_interval_seconds.is_finite() {
        config.keyframe_interval_seconds.clamp(0.1, 10.0)
    } else {
        1.0
    };
    let rounded = (value * 1000.0).round() / 1000.0;
    if (rounded.fract()).abs() < f32::EPSILON {
        format!("{}", rounded as u32)
    } else {
        format!("{rounded:.3}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    }
}

fn bitrate_mode_label(mode: GsrBitrateMode) -> &'static str {
    match mode {
        GsrBitrateMode::Auto => "auto",
        GsrBitrateMode::Qp => "qp",
        GsrBitrateMode::Cbr => "cbr",
        GsrBitrateMode::Vbr => "vbr",
    }
}

pub fn scan_replay_outputs(output_dir: &Path, extension: &str) -> anyhow::Result<Vec<PathBuf>> {
    if !output_dir.exists() {
        return Ok(Vec::new());
    }
    let extension = extension.trim_start_matches('.').to_ascii_lowercase();
    let mut outputs = walkdir::WalkDir::new(output_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| {
            path.extension()
                .and_then(|value| value.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref()
                == Some(extension.as_str())
        })
        .collect::<Vec<_>>();
    outputs.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    });
    Ok(outputs)
}

pub async fn wait_for_new_replay_output(
    output_dir: &Path,
    extension: &str,
    before: &HashSet<PathBuf>,
    timeout: Duration,
) -> anyhow::Result<PathBuf> {
    let started = tokio::time::Instant::now();
    let mut last_scan_count = None;
    let mut last_scan_log = tokio::time::Instant::now() - Duration::from_secs(2);
    loop {
        let outputs = scan_replay_outputs(output_dir, extension)?;
        let scan_count = outputs.len();
        if last_scan_count != Some(scan_count) || last_scan_log.elapsed() >= Duration::from_secs(1)
        {
            println!("[GPU_RECORDER] scan after count={scan_count}");
            last_scan_count = Some(scan_count);
            last_scan_log = tokio::time::Instant::now();
        }
        let mut candidates = outputs
            .into_iter()
            .filter(|path| !before.contains(path))
            .filter(|path| {
                fs::metadata(path)
                    .map(|metadata| metadata.len() > 0)
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|path| {
            fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH)
        });
        if let Some(path) = candidates.pop() {
            println!("[GPU_RECORDER] output detected path={}", path.display());
            return Ok(path);
        }
        if started.elapsed() >= timeout {
            anyhow::bail!(
                "GPU Screen Recorder n'a produit aucun nouveau fichier {extension} dans {}s",
                timeout.as_secs()
            );
        }
        sleep(Duration::from_millis(250)).await;
    }
}

pub async fn wait_until_file_stable(
    path: &Path,
    stable_duration: Duration,
    timeout: Duration,
) -> anyhow::Result<()> {
    let started = tokio::time::Instant::now();
    let mut last_size = None;
    let mut stable_since = tokio::time::Instant::now();
    loop {
        let size = fs::metadata(path)
            .with_context(|| {
                format!(
                    "fichier GSR introuvable pendant stabilisation: {}",
                    path.display()
                )
            })?
            .len();
        if size > 0 && Some(size) == last_size {
            if stable_since.elapsed() >= stable_duration {
                return Ok(());
            }
        } else {
            last_size = Some(size);
            stable_since = tokio::time::Instant::now();
        }
        if started.elapsed() >= timeout {
            anyhow::bail!(
                "fichier GSR instable après {}s: {}",
                timeout.as_secs(),
                path.display()
            );
        }
        sleep(STABLE_FILE_POLL).await;
    }
}

fn status_from_inner(inner: &GpuScreenRecorderInner) -> GsrStatus {
    let waiting_for_war_thunder = inner.config.capture_strategy.should_wait_for_war_thunder()
        && inner.state.health == GsrHealth::Stopped
        && inner.state.target_reason == WAITING_FOR_WAR_THUNDER_TARGET_REASON;
    let command = if waiting_for_war_thunder {
        None
    } else {
        build_gsr_command(&inner.config).ok()
    };
    let command_line = inner
        .state
        .command_line
        .clone()
        .or_else(|| command.as_ref().map(|command| command.command_line.clone()));
    let output_dir = command
        .as_ref()
        .map(|command| command.output_dir.clone())
        .unwrap_or_else(|| inner.state.output_dir.clone());
    let output_prefix = command
        .as_ref()
        .map(|command| command.output_prefix.clone())
        .unwrap_or_else(|| inner.state.output_prefix.clone());
    let target = if inner.state.health == GsrHealth::Running
        || inner.state.health == GsrHealth::SavingReplay
    {
        inner.state.target.clone()
    } else {
        command
            .as_ref()
            .map(|command| command.target.clone())
            .unwrap_or_else(|| inner.state.target.clone())
    };
    let capture_strategy = if inner.state.health == GsrHealth::Running
        || inner.state.health == GsrHealth::SavingReplay
    {
        inner.state.capture_strategy
    } else {
        command
            .as_ref()
            .map(|command| command.capture_strategy)
            .unwrap_or(inner.state.capture_strategy)
    };
    let session_type = if inner.state.health == GsrHealth::Running
        || inner.state.health == GsrHealth::SavingReplay
    {
        inner.state.session_type.clone()
    } else {
        command
            .as_ref()
            .map(|command| command.session_type.clone())
            .unwrap_or_else(|| inner.state.session_type.clone())
    };
    let target_reason = if inner.state.health == GsrHealth::Running
        || inner.state.health == GsrHealth::SavingReplay
    {
        inner.state.target_reason.clone()
    } else {
        command
            .as_ref()
            .map(|command| command.target_reason.clone())
            .unwrap_or_else(|| inner.state.target_reason.clone())
    };

    GsrStatus {
        available: command
            .as_ref()
            .is_some_and(|command| command_available(&command.program))
            || resolve_mode(inner.config.gpu_screen_recorder_mode)
                .map(|mode| match mode {
                    GsrMode::Native => command_available("gpu-screen-recorder"),
                    GsrMode::Flatpak => command_available("flatpak"),
                })
                .unwrap_or(false),
        mode: inner
            .state
            .mode
            .or_else(|| command.as_ref().map(|command| command.mode))
            .or_else(|| resolve_mode(inner.config.gpu_screen_recorder_mode).ok()),
        health: inner.state.health,
        pid: inner.state.recorder_pid,
        wrapper_pid: inner.state.wrapper_pid,
        recorder_pid: inner.state.recorder_pid,
        signal_pid: inner.state.recorder_pid,
        recorder_command_line: inner.state.recorder_command_line.clone(),
        stderr_handling: inner.state.stderr_handling.clone(),
        save_queue_len: inner.state.save_queue_len,
        total_saves_requested: inner.state.total_saves_requested,
        total_saves_completed: inner.state.total_saves_completed,
        total_saves_failed: inner.state.total_saves_failed,
        target_valid: target_is_valid(&target, &inner.state.monitors),
        target,
        monitors: inner.state.monitors.clone(),
        capture_strategy: format!("{:?}", capture_strategy).to_ascii_lowercase(),
        session_type,
        target_reason,
        command_line,
        output_dir,
        output_prefix,
        last_output: inner.state.last_output.clone(),
        last_error: inner.state.last_error.clone(),
        restart_count: inner.state.restart_count,
        replay_seconds: inner.config.replay_seconds,
        fps: inner.config.fps,
        quality: inner.config.quality.as_arg().to_owned(),
        bitrate_mode: bitrate_mode_label(inner.config.bitrate_mode).to_owned(),
        frame_rate_mode: inner.config.frame_rate_mode.as_arg().to_owned(),
        keyframe_interval_seconds: inner.config.keyframe_interval_seconds,
        restart_replay_on_save: inner.config.restart_replay_on_save,
        video_bitrate_kbps: inner.config.video_bitrate_kbps,
        effective_q_argument: effective_q_argument(&inner.config),
        codec: inner.config.codec.as_arg().to_owned(),
        container: inner.config.container.as_arg().to_owned(),
        encoder: inner.config.encoder.as_arg().to_owned(),
    }
}

fn ensure_running(inner: &mut GpuScreenRecorderInner) -> anyhow::Result<()> {
    if !child_is_running(inner.child.as_mut()) {
        inner.child = None;
        inner.state.wrapper_pid = None;
        inner.state.recorder_pid = None;
        inner.state.health = GsrHealth::Error;
        let message = "GPU Screen Recorder n'est pas lancé; redémarrez le backend GPU Replay";
        inner.state.last_error = Some(message.to_owned());
        anyhow::bail!(message);
    }
    inner.state.wrapper_pid = inner.child.as_ref().map(Child::id);
    if let Some(recorder_pid) = inner.state.recorder_pid {
        if !process_alive(recorder_pid) {
            inner.state.recorder_pid = None;
            inner.state.health = GsrHealth::Error;
            let message = "GPU Screen Recorder recorder_pid n'est plus vivant";
            inner.state.last_error = Some(message.to_owned());
            anyhow::bail!(message);
        }
    }
    Ok(())
}

fn signal_pid_from_state(state: &GpuScreenRecorderState) -> anyhow::Result<u32> {
    state.recorder_pid.ok_or_else(|| {
        anyhow::anyhow!("GPU Screen Recorder n'est pas lancé: recorder_pid indisponible")
    })
}

fn child_is_running(child: Option<&mut Child>) -> bool {
    match child {
        Some(child) => matches!(child.try_wait(), Ok(None)),
        None => false,
    }
}

async fn stop_child(child: &mut Option<Child>) -> anyhow::Result<()> {
    let Some(mut child) = child.take() else {
        return Ok(());
    };
    let pid = child.id();
    info!(pid, "stopping GPU Screen Recorder child");
    if let Err(error) = signal_process(pid, libc::SIGTERM) {
        debug!(%error, pid, "failed to send SIGTERM to GPU Screen Recorder child");
    }
    let started = tokio::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                info!(pid, %status, "GPU Screen Recorder child stopped");
                return Ok(());
            }
            Ok(None) if started.elapsed() < STOP_TIMEOUT => sleep(Duration::from_millis(100)).await,
            Ok(None) => {
                warn!(
                    pid,
                    "GPU Screen Recorder child did not stop after SIGTERM; killing child PID only"
                );
                child
                    .kill()
                    .context("failed to kill GPU Screen Recorder child")?;
                let _ = child.wait();
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        }
    }
}

async fn stop_recorder_pid(recorder_pid: Option<u32>) -> anyhow::Result<()> {
    let Some(pid) = recorder_pid else {
        return Ok(());
    };
    if !process_alive(pid) {
        return Ok(());
    }
    if !is_safe_recorder_process(pid) {
        warn!(pid, "refusing to stop non-matching GPU Screen Recorder PID");
        return Ok(());
    }
    info!(pid, "stopping GPU Screen Recorder recorder process");
    if let Err(error) = signal_process(pid, libc::SIGTERM) {
        debug!(%error, pid, "failed to send SIGTERM to GPU Screen Recorder recorder");
    }
    let started = tokio::time::Instant::now();
    while started.elapsed() < STOP_TIMEOUT {
        if !process_alive(pid) {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    warn!(
        pid,
        "GPU Screen Recorder recorder did not stop; sending SIGKILL to recorder_pid only"
    );
    if let Err(error) = signal_process(pid, libc::SIGKILL) {
        debug!(%error, pid, "failed to send SIGKILL to GPU Screen Recorder recorder");
    }
    Ok(())
}

fn signal_process(pid: u32, signal: i32) -> anyhow::Result<()> {
    let result = unsafe { libc::kill(pid as libc::pid_t, signal) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
            .with_context(|| format!("kill({pid}, {signal}) failed"))
    }
}

fn process_alive(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GsrProcessInfo {
    pub pid: u32,
    pub args: Vec<String>,
    pub command_line: String,
}

pub async fn resolve_actual_gsr_pid(
    config: &CaptureConfig,
    output_dir: &Path,
    output_prefix: &Path,
    target: &str,
) -> anyhow::Result<GsrProcessInfo> {
    let started = tokio::time::Instant::now();
    loop {
        let processes = list_processes_from_proc()?;
        if let Some(process) =
            select_actual_gsr_process(&processes, config, output_dir, output_prefix, target)
        {
            return Ok(process.clone());
        }
        if started.elapsed() >= Duration::from_secs(5) {
            anyhow::bail!(
                "impossible de résoudre le vrai PID gpu-screen-recorder pour target={} -ro {} -o {}",
                target,
                output_dir.display(),
                output_prefix.display()
            );
        }
        sleep(Duration::from_millis(100)).await;
    }
}

fn list_processes_from_proc() -> anyhow::Result<Vec<GsrProcessInfo>> {
    let mut processes = Vec::new();
    for entry in fs::read_dir("/proc").context("failed to read /proc")? {
        let Ok(entry) = entry else {
            continue;
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(bytes) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if bytes.is_empty() {
            continue;
        }
        let args = bytes
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect::<Vec<_>>();
        if args.is_empty() {
            continue;
        }
        let command_line = args.join(" ");
        processes.push(GsrProcessInfo {
            pid,
            args,
            command_line,
        });
    }
    Ok(processes)
}

pub fn select_actual_gsr_process<'a>(
    processes: &'a [GsrProcessInfo],
    _config: &CaptureConfig,
    output_dir: &Path,
    output_prefix: &Path,
    target: &str,
) -> Option<&'a GsrProcessInfo> {
    let output_dir = output_dir.display().to_string();
    let output_prefix = output_prefix.display().to_string();
    processes
        .iter()
        .filter(|process| process_matches_actual_gsr(process, target, &output_dir, &output_prefix))
        .max_by_key(|process| process.pid)
}

fn process_matches_actual_gsr(
    process: &GsrProcessInfo,
    target: &str,
    output_dir: &str,
    output_prefix: &str,
) -> bool {
    if process.args.is_empty() || ignored_gsr_process(process) {
        return false;
    }
    let executable = process
        .args
        .first()
        .and_then(|arg| Path::new(arg).file_name())
        .and_then(|arg| arg.to_str())
        .unwrap_or("");
    if executable != "gpu-screen-recorder" {
        return false;
    }
    arg_pair_matches(&process.args, "-w", target)
        && arg_pair_matches(&process.args, "-ro", output_dir)
        && arg_pair_matches(&process.args, "-o", output_prefix)
}

fn ignored_gsr_process(process: &GsrProcessInfo) -> bool {
    let ignored = [
        "bwrap",
        "flatpak",
        "flatpak-spawn",
        "gsr-kms-server",
        "kms-server-proxy",
        "gpu-screen-recorder-gtk",
        "gsr-game-tracker",
    ];
    process.args.iter().any(|arg| {
        Path::new(arg)
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| ignored.contains(&name))
    })
}

fn is_safe_recorder_process(pid: u32) -> bool {
    let Ok(bytes) = fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let args = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect::<Vec<_>>();
    let process = GsrProcessInfo {
        pid,
        command_line: args.join(" "),
        args,
    };
    !ignored_gsr_process(&process)
        && process
            .args
            .first()
            .and_then(|arg| Path::new(arg).file_name())
            .and_then(|arg| arg.to_str())
            == Some("gpu-screen-recorder")
}

fn arg_pair_matches(args: &[String], key: &str, expected: &str) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == key && pair[1] == expected)
}

fn scan_replay_outputs_set(output_dir: &Path, extension: &str) -> anyhow::Result<HashSet<PathBuf>> {
    Ok(scan_replay_outputs(output_dir, extension)?
        .into_iter()
        .collect())
}

fn resolve_mode(mode: GpuScreenRecorderMode) -> anyhow::Result<GsrMode> {
    match mode {
        GpuScreenRecorderMode::Native => Ok(GsrMode::Native),
        GpuScreenRecorderMode::Flatpak => Ok(GsrMode::Flatpak),
        GpuScreenRecorderMode::Auto => {
            if command_available("gpu-screen-recorder") {
                Ok(GsrMode::Native)
            } else if command_available("flatpak") {
                Ok(GsrMode::Flatpak)
            } else {
                anyhow::bail!(
                    "GPU Screen Recorder not found: install native gpu-screen-recorder or Flatpak"
                )
            }
        }
    }
}

fn command_available(command: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(command).is_file())
}

fn sanitized_fps(fps: u32) -> u32 {
    fps.clamp(1, 240)
}

fn refresh_monitors_snapshot(inner: &mut GpuScreenRecorderInner) {
    inner.state.monitors = list_monitors(&inner.config);
    if inner.state.health != GsrHealth::Running && inner.state.health != GsrHealth::SavingReplay {
        let was_waiting = inner.config.capture_strategy.should_wait_for_war_thunder()
            && inner.state.target_reason == WAITING_FOR_WAR_THUNDER_TARGET_REASON;
        inner.state.capture_strategy = inner.config.capture_strategy;
        inner.state.session_type = session_type();
        inner.state.target = select_effective_target(&inner.config.target, &inner.state.monitors);
        if !was_waiting {
            inner.state.target_reason = "configured fallback target".to_owned();
        }
    }
    inner.state.target_valid = target_is_valid(&inner.state.target, &inner.state.monitors);
}

fn refresh_monitors_and_effective_target(inner: &mut GpuScreenRecorderInner) {
    let monitors = list_monitors(&inner.config);
    let requested_target = inner.config.target.clone();
    let effective_target = select_effective_target(&requested_target, &monitors);
    if effective_target != requested_target {
        println!(
            "[GPU_RECORDER] target '{}' invalid, using '{}'",
            requested_target, effective_target
        );
        inner.config.target = effective_target.clone();
        inner.state.last_error = Some(format!(
            "Target GSR '{}' invalide, utilisation automatique de '{}'",
            requested_target, effective_target
        ));
    }
    inner.state.monitors = monitors;
    inner.state.capture_strategy = inner.config.capture_strategy;
    inner.state.session_type = session_type();
    inner.state.target = inner.config.target.clone();
    inner.state.target_reason = "configured fallback target".to_owned();
    inner.state.target_valid = target_is_valid(&inner.state.target, &inner.state.monitors);
}

fn select_effective_target(requested: &str, monitors: &[String]) -> String {
    let requested = requested.trim();
    if target_is_valid(requested, monitors) {
        return requested.to_owned();
    }
    // If monitors list is empty, fall back to "screen" which is widely supported by GSR
    if monitors.is_empty() {
        tracing::warn!(
            requested = %requested,
            "No monitors available from GSR; falling back to 'screen'"
        );
        return "screen".to_owned();
    }
    // Only fall back to a real monitor. A special target ("focused"/"portal")
    // must never be silently substituted for a configured monitor target,
    // otherwise an unconfigured/headless environment would override the user's
    // capture target.
    monitors
        .iter()
        .find(|monitor| !is_special_capture_target(monitor))
        .cloned()
        .unwrap_or_else(|| {
            // If no non-special monitor found, use the first available monitor
            tracing::warn!(
                requested = %requested,
                monitors = ?monitors,
                "No non-special monitor found; falling back to first monitor"
            );
            monitors[0].clone()
        })
}

fn target_is_valid(target: &str, monitors: &[String]) -> bool {
    let target = target.trim();
    if target.is_empty() {
        return false;
    }
    is_special_capture_target(target)
        || is_x11_window_id(target)
        || monitors.iter().any(|monitor| monitor == target)
}

fn is_special_capture_target(target: &str) -> bool {
    matches!(target, "portal" | "focused")
}

fn is_x11_window_id(target: &str) -> bool {
    let Some(hex) = target.strip_prefix("0x") else {
        return false;
    };
    !hex.is_empty() && hex.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn list_monitors(config: &CaptureConfig) -> Vec<String> {
    let mut values = query_gsr_monitors(config).unwrap_or_default();
    values.extend(["portal".to_owned(), "focused".to_owned()]);
    values.sort();
    values.dedup();
    values
}

pub fn query_gsr_monitors(config: &CaptureConfig) -> anyhow::Result<Vec<String>> {
    let mode = resolve_mode(config.gpu_screen_recorder_mode)?;
    let output = match mode {
        GsrMode::Native => Command::new("gpu-screen-recorder")
            .arg("--list-monitors")
            .output()
            .context("failed to run gpu-screen-recorder --list-monitors")?,
        GsrMode::Flatpak => Command::new("flatpak")
            .args([
                "run",
                "--command=gpu-screen-recorder",
                FLATPAK_APP_ID,
                "--list-monitors",
            ])
            .output()
            .context("failed to run flatpak GPU Screen Recorder --list-monitors")?,
    };
    if !output.status.success() {
        anyhow::bail!(
            "GPU Screen Recorder --list-monitors exited with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(parse_gsr_monitor_output(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn parse_gsr_monitor_output(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let name = line.split('|').next().unwrap_or(line).trim();
            (!name.is_empty()).then(|| name.to_owned())
        })
        .collect()
}

fn write_gsr_metadata(
    metadata_path: &Path,
    video_path: &Path,
    thumbnail_path: Option<&Path>,
    context: &ClipContext,
    config: &CaptureConfig,
    duration_seconds: u64,
) -> anyhow::Result<()> {
    let primary_event = context.event.as_ref().or_else(|| context.events.first());
    let raw_event = primary_event.and_then(raw_event_text);
    let event = primary_event.map(metadata_event_json);
    let events = (!context.events.is_empty()).then(|| {
        context
            .events
            .iter()
            .map(metadata_event_json)
            .collect::<Vec<_>>()
    });
    let video_path_string = video_path.display().to_string();
    let metadata = GsrClipMetadata {
        id: context.pending_clip_id.clone().unwrap_or_else(|| {
            video_path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("clip")
                .to_owned()
        }),
        created_by: "wt-clipper",
        capture_backend: "gpu_screen_recorder",
        kind: "clip",
        reason: context.reason.slug(),
        clip_type: clip_type_for_reason(context.reason),
        title: suggested_clip_stem(context),
        game: "War Thunder",
        player_name: context.player_name.clone(),
        raw_event,
        path: video_path_string.clone(),
        source_path: video_path_string.clone(),
        video_path: video_path_string,
        thumbnail_path: thumbnail_path.map(|path| path.display().to_string()),
        duration_seconds,
        codec: config.codec.as_arg(),
        container: config.container.as_arg(),
        fps: config.fps,
        created_at: Utc::now().to_rfc3339(),
        event,
        events,
        kill_count: (context.reason == ClipReason::MultiKill).then_some(context.events.len()),
        quality: config.quality.as_arg(),
        target: config.target.clone(),
    };
    let json = serde_json::to_string_pretty(&metadata)?;
    fs::write(metadata_path, json)
        .with_context(|| format!("failed to write GSR metadata {}", metadata_path.display()))
}

fn clip_type_for_reason(reason: ClipReason) -> &'static str {
    match reason {
        ClipReason::TargetDestroyed => "kill",
        ClipReason::BaseDestroyed => "base",
        ClipReason::PlayerDestroyed => "death",
        ClipReason::MultiKill => "multi",
        ClipReason::Manual => "manual",
        ClipReason::Unknown => "clip",
    }
}

async fn probe_video_duration_seconds(video_path: &Path) -> Option<u64> {
    let video_path = video_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let output = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "default=noprint_wrappers=1:nokey=1",
            ])
            .arg(&video_path)
            .output()
            .ok()?;
        if !output.status.success() {
            debug!(
                path = %video_path.display(),
                stderr = %String::from_utf8_lossy(&output.stderr),
                "ffprobe duration failed for GSR clip"
            );
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let seconds = text.trim().parse::<f64>().ok()?;
        (seconds.is_finite() && seconds > 0.0).then(|| seconds.round().max(1.0) as u64)
    })
    .await
    .ok()
    .flatten()
}

async fn generate_thumbnail(video_path: &Path) -> Option<PathBuf> {
    let video_path = video_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let thumbnail_path = video_path.with_extension("jpg");
        let output = Command::new("ffmpeg")
            .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(&video_path)
            .args(["-vframes", "1", "-s", "640x360"])
            .arg(&thumbnail_path)
            .output()
            .ok()?;
        if output.status.success()
            && fs::metadata(&thumbnail_path)
                .map(|metadata| metadata.is_file() && metadata.len() > 0)
                .unwrap_or(false)
        {
            Some(thumbnail_path)
        } else {
            debug!(
                path = %video_path.display(),
                status = ?output.status.code(),
                stderr = %String::from_utf8_lossy(&output.stderr),
                "failed to generate GSR thumbnail"
            );
            None
        }
    })
    .await
    .ok()
    .flatten()
}

fn metadata_event_json(event: &GameEvent) -> serde_json::Value {
    serde_json::json!({
        "type": format!("{:?}", event.kind).to_ascii_lowercase(),
        "actor": event.actor,
        "subject": event.subject,
        "context": event.context,
        "summary": event.summary,
    })
}

fn raw_event_text(event: &GameEvent) -> Option<String> {
    if event.summary.is_empty() {
        None
    } else {
        Some(event.summary.clone())
    }
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '='))
    {
        value.to_owned()
    } else {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

pub fn suggested_clip_stem(context: &ClipContext) -> String {
    let prefix = match context.reason {
        ClipReason::TargetDestroyed => "kill",
        ClipReason::BaseDestroyed => "base",
        ClipReason::PlayerDestroyed => "death",
        ClipReason::MultiKill => "multi-kill",
        ClipReason::Manual => "manual",
        ClipReason::Unknown => "clip",
    };
    let event_slug = context
        .event
        .as_ref()
        .and_then(raw_event_text)
        .map(|raw| slugify_filename_part(&raw))
        .unwrap_or_else(|| "replay".to_owned());
    format!("{prefix}-{event_slug}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        GsrBitrateMode, GsrCodec, GsrContainer, GsrEncoder, GsrFrameRateMode, GsrQuality,
    };

    fn test_config(output_dir: PathBuf) -> CaptureConfig {
        CaptureConfig {
            capture_strategy: CaptureStrategy::Monitor,
            target: "eDP".to_owned(),
            gpu_screen_recorder_mode: GpuScreenRecorderMode::Flatpak,
            fps: 30,
            replay_seconds: 25,
            container: GsrContainer::Mp4,
            codec: GsrCodec::H264,
            encoder: GsrEncoder::Gpu,
            quality: GsrQuality::Medium,
            bitrate_mode: GsrBitrateMode::Auto,
            frame_rate_mode: GsrFrameRateMode::Cfr,
            keyframe_interval_seconds: 1.0,
            restart_replay_on_save: false,
            video_bitrate_kbps: 0,
            output_dir: output_dir.display().to_string(),
            audio_enabled: true,
            audio_input: "default_output".to_owned(),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wt-clipper-gsr-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn builds_flatpak_command_from_config() {
        let config = test_config(PathBuf::from("/tmp/wt gsr"));
        let command = build_gsr_command(&config).unwrap();

        assert_eq!(command.program, "flatpak");
        assert!(command
            .command_line
            .contains("com.dec05eba.gpu_screen_recorder"));
        assert!(command.command_line.contains("-w eDP"));
        assert!(command.command_line.contains("-f 30"));
        assert!(command.command_line.contains("-fm cfr"));
        assert!(command.command_line.contains("-keyint 1"));
        assert!(command.command_line.contains("-r 25"));
        assert!(command.command_line.contains("-q medium"));
        assert!(command
            .args
            .windows(2)
            .any(|args| args == ["-restart-replay-on-save", "no"]));
        assert!(command.command_line.contains("-ro \"/tmp/wt gsr\""));
        assert!(command.command_line.contains("-o \"/tmp/wt gsr/wtclip\""));
        assert!(command
            .args
            .windows(2)
            .any(|args| args == ["-a", "default_output"]));
    }

    #[test]
    fn changing_replay_seconds_changes_r_argument() {
        let mut config = test_config(PathBuf::from("/tmp/wt-gsr"));
        config.replay_seconds = 40;
        let command = build_gsr_command(&config).unwrap();
        assert!(command.args.windows(2).any(|args| args == ["-r", "40"]));
    }

    #[test]
    fn changing_fps_changes_f_argument() {
        let mut config = test_config(PathBuf::from("/tmp/wt-gsr"));
        config.fps = 60;
        let command = build_gsr_command(&config).unwrap();
        assert!(command.args.windows(2).any(|args| args == ["-f", "60"]));
    }

    #[test]
    fn default_config_generates_cfr_frame_rate_mode() {
        let config = test_config(PathBuf::from("/tmp/wt-gsr"));
        let command = build_gsr_command(&config).unwrap();
        assert!(command.args.windows(2).any(|args| args == ["-fm", "cfr"]));
    }

    #[test]
    fn default_config_does_not_restart_replay_on_save() {
        let config = test_config(PathBuf::from("/tmp/wt-gsr"));
        let command = build_gsr_command(&config).unwrap();
        assert!(command
            .args
            .windows(2)
            .any(|args| args == ["-restart-replay-on-save", "no"]));
    }

    #[test]
    fn frame_rate_mode_content_generates_fm_content() {
        let mut config = test_config(PathBuf::from("/tmp/wt-gsr"));
        config.frame_rate_mode = GsrFrameRateMode::Content;
        let command = build_gsr_command(&config).unwrap();
        assert!(command
            .args
            .windows(2)
            .any(|args| args == ["-fm", "content"]));
    }

    #[test]
    fn default_config_generates_keyint_1() {
        let config = test_config(PathBuf::from("/tmp/wt-gsr"));
        let command = build_gsr_command(&config).unwrap();
        assert!(command.args.windows(2).any(|args| args == ["-keyint", "1"]));
    }

    #[test]
    fn keyframe_interval_half_generates_keyint_half() {
        let mut config = test_config(PathBuf::from("/tmp/wt-gsr"));
        config.keyframe_interval_seconds = 0.5;
        let command = build_gsr_command(&config).unwrap();
        assert!(command
            .args
            .windows(2)
            .any(|args| args == ["-keyint", "0.5"]));
    }

    #[test]
    fn keyframe_interval_two_generates_keyint_2() {
        let mut config = test_config(PathBuf::from("/tmp/wt-gsr"));
        config.keyframe_interval_seconds = 2.0;
        let command = build_gsr_command(&config).unwrap();
        assert!(command.args.windows(2).any(|args| args == ["-keyint", "2"]));
    }

    #[test]
    fn restart_replay_on_save_true_generates_yes() {
        let mut config = test_config(PathBuf::from("/tmp/wt-gsr"));
        config.restart_replay_on_save = true;
        let command = build_gsr_command(&config).unwrap();
        assert!(command
            .args
            .windows(2)
            .any(|args| args == ["-restart-replay-on-save", "yes"]));
    }

    // #[test]
    // fn changing_target_changes_w_argument() {
    //     let mut config = test_config(PathBuf::from("/tmp/wt-gsr"));
    //     config.target = "HDMI-A-1-0".to_owned();
    //     config.capture_strategy = CaptureStrategy::Monitor;
    //     let command = build_gsr_command(&config).unwrap();
    //     assert!(command
    //         .args
    //         .windows(2)
    //         .any(|args| args == ["-w", "HDMI-A-1-0"]));
    // }

    #[test]
    fn changing_quality_changes_q_argument() {
        let mut config = test_config(PathBuf::from("/tmp/wt-gsr"));
        config.quality = GsrQuality::High;
        let command = build_gsr_command(&config).unwrap();
        assert!(command.args.windows(2).any(|args| args == ["-q", "high"]));
    }

    #[test]
    fn bitrate_mode_auto_uses_quality_q_argument() {
        let mut config = test_config(PathBuf::from("/tmp/wt-gsr"));
        config.bitrate_mode = GsrBitrateMode::Auto;
        config.quality = GsrQuality::VeryHigh;

        let command = build_gsr_command(&config).unwrap();

        assert!(!command.args.iter().any(|arg| arg == "-bm"));
        assert!(command
            .args
            .windows(2)
            .any(|args| args == ["-q", "very_high"]));
    }

    #[test]
    fn bitrate_mode_cbr_adds_bm_cbr_and_bitrate_q_argument() {
        let mut config = test_config(PathBuf::from("/tmp/wt-gsr"));
        config.bitrate_mode = GsrBitrateMode::Cbr;
        config.video_bitrate_kbps = 20_000;

        let command = build_gsr_command(&config).unwrap();

        assert!(command.args.windows(2).any(|args| args == ["-bm", "cbr"]));
        assert!(command.args.windows(2).any(|args| args == ["-q", "20000"]));
    }

    #[test]
    fn bitrate_mode_cbr_does_not_add_ffmpeg_opts() {
        let mut config = test_config(PathBuf::from("/tmp/wt-gsr"));
        config.bitrate_mode = GsrBitrateMode::Cbr;
        config.video_bitrate_kbps = 20_000;

        let command = build_gsr_command(&config).unwrap();

        assert!(!command.args.iter().any(|arg| arg == "-ffmpeg-opts"));
    }

    #[test]
    fn bitrate_mode_vbr_uses_quality_q_argument() {
        let mut config = test_config(PathBuf::from("/tmp/wt-gsr"));
        config.bitrate_mode = GsrBitrateMode::Vbr;
        config.quality = GsrQuality::VeryHigh;

        let command = build_gsr_command(&config).unwrap();

        assert!(command.args.windows(2).any(|args| args == ["-bm", "vbr"]));
        assert!(command
            .args
            .windows(2)
            .any(|args| args == ["-q", "very_high"]));
        assert!(!command.args.iter().any(|arg| arg == "-ffmpeg-opts"));
    }

    #[test]
    fn bitrate_mode_qp_accepts_ultra_quality() {
        let mut config = test_config(PathBuf::from("/tmp/wt-gsr"));
        config.bitrate_mode = GsrBitrateMode::Qp;
        config.quality = GsrQuality::Ultra;

        let command = build_gsr_command(&config).unwrap();

        assert!(command.args.windows(2).any(|args| args == ["-bm", "qp"]));
        assert!(command.args.windows(2).any(|args| args == ["-q", "ultra"]));
    }

    #[test]
    fn bitrate_mode_cbr_with_invalid_bitrate_falls_back_to_20000() {
        let mut config = test_config(PathBuf::from("/tmp/wt-gsr"));
        config.bitrate_mode = GsrBitrateMode::Cbr;
        config.video_bitrate_kbps = 0;

        let command = build_gsr_command(&config).unwrap();

        assert!(command.args.windows(2).any(|args| args == ["-bm", "cbr"]));
        assert!(command.args.windows(2).any(|args| args == ["-q", "20000"]));
    }

    #[test]
    fn output_paths_are_absolute() {
        let command = build_gsr_command(&test_config(PathBuf::from("/tmp/wt-gsr"))).unwrap();
        assert!(command.output_dir.is_absolute());
        assert!(command.output_prefix.is_absolute());
    }

    #[test]
    fn refuses_relative_output_dir() {
        let error = build_gsr_command(&test_config(PathBuf::from("relative-gsr")))
            .unwrap_err()
            .to_string();
        assert!(error.contains("output_dir must be absolute"));
    }

    #[test]
    fn recursive_scan_finds_replay_mp4() {
        let root = temp_dir("recursive-scan");
        let nested = root.join("session");
        fs::create_dir_all(&nested).unwrap();
        let replay = nested.join("Replay_001.mp4");
        fs::write(&replay, b"mp4").unwrap();

        let outputs = scan_replay_outputs(&root, "mp4").unwrap();

        assert_eq!(outputs, vec![replay]);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn wait_for_new_output_ignores_old_files() {
        let root = temp_dir("new-output");
        let old = root.join("Replay_old.mp4");
        fs::write(&old, b"old").unwrap();
        let before = scan_replay_outputs_set(&root, "mp4").unwrap();
        let new_file = root.join("Replay_new.mp4");
        fs::write(&new_file, b"new").unwrap();

        let found = wait_for_new_replay_output(&root, "mp4", &before, Duration::from_secs(1))
            .await
            .unwrap();

        assert_eq!(found, new_file);
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn timeout_when_no_new_file() {
        let root = temp_dir("timeout");
        let before = scan_replay_outputs_set(&root, "mp4").unwrap();

        let error = wait_for_new_replay_output(&root, "mp4", &before, Duration::from_millis(50))
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("aucun nouveau fichier"));
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn diagnostics_command_matches_built_command() {
        let config = test_config(PathBuf::from("/tmp/wt-gsr"));
        let handle = GpuScreenRecorderHandle::new(config.clone());
        let status = handle.status().await;
        let command = build_gsr_command(&config).unwrap();

        assert_eq!(
            status.command_line.as_deref(),
            Some(command.command_line.as_str())
        );
        assert_eq!(status.output_prefix, command.output_prefix);
        assert_eq!(status.stderr_handling, "null");
    }

    fn process(pid: u32, args: &[&str]) -> GsrProcessInfo {
        GsrProcessInfo {
            pid,
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            command_line: args.join(" "),
        }
    }

    #[test]
    fn resolve_actual_gsr_pid_ignores_bwrap() {
        let config = test_config(PathBuf::from("/tmp/wt gsr"));
        let output_dir = PathBuf::from("/tmp/wt gsr");
        let output_prefix = output_dir.join("wtclip");
        let processes = vec![
            process(
                10,
                &[
                    "bwrap",
                    "--args",
                    "80",
                    "--",
                    "gpu-screen-recorder",
                    "-w",
                    "eDP",
                    "-ro",
                    "/tmp/wt gsr",
                    "-o",
                    "/tmp/wt gsr/wtclip",
                ],
            ),
            process(
                11,
                &[
                    "gpu-screen-recorder",
                    "-w",
                    "eDP",
                    "-ro",
                    "/tmp/wt gsr",
                    "-o",
                    "/tmp/wt gsr/wtclip",
                ],
            ),
        ];

        let selected =
            select_actual_gsr_process(&processes, &config, &output_dir, &output_prefix, "eDP")
                .unwrap();

        assert_eq!(selected.pid, 11);
    }

    #[test]
    fn resolve_actual_gsr_pid_ignores_gsr_game_tracker() {
        let config = test_config(PathBuf::from("/tmp/wt-gsr"));
        let output_dir = PathBuf::from("/tmp/wt-gsr");
        let output_prefix = output_dir.join("wtclip");
        let processes = vec![
            process(20, &["gsr-game-tracker", "gpu-screen-recorder"]),
            process(
                21,
                &[
                    "gpu-screen-recorder",
                    "-w",
                    "eDP",
                    "-ro",
                    "/tmp/wt-gsr",
                    "-o",
                    "/tmp/wt-gsr/wtclip",
                ],
            ),
        ];

        let selected =
            select_actual_gsr_process(&processes, &config, &output_dir, &output_prefix, "eDP")
                .unwrap();

        assert_eq!(selected.pid, 21);
    }

    #[test]
    fn resolve_actual_gsr_pid_ignores_gpu_screen_recorder_gtk() {
        let config = test_config(PathBuf::from("/tmp/wt-gsr"));
        let output_dir = PathBuf::from("/tmp/wt-gsr");
        let output_prefix = output_dir.join("wtclip");
        let processes = vec![
            process(
                30,
                &[
                    "gpu-screen-recorder-gtk",
                    "gpu-screen-recorder",
                    "-w",
                    "eDP",
                    "-ro",
                    "/tmp/wt-gsr",
                    "-o",
                    "/tmp/wt-gsr/wtclip",
                ],
            ),
            process(
                31,
                &[
                    "gpu-screen-recorder",
                    "-w",
                    "eDP",
                    "-ro",
                    "/tmp/wt-gsr",
                    "-o",
                    "/tmp/wt-gsr/wtclip",
                ],
            ),
        ];

        let selected =
            select_actual_gsr_process(&processes, &config, &output_dir, &output_prefix, "eDP")
                .unwrap();

        assert_eq!(selected.pid, 31);
    }

    #[test]
    fn resolve_actual_gsr_pid_matches_output_prefix() {
        let config = test_config(PathBuf::from("/tmp/wt-gsr"));
        let output_dir = PathBuf::from("/tmp/wt-gsr");
        let output_prefix = output_dir.join("wtclip");
        let processes = vec![
            process(
                40,
                &[
                    "gpu-screen-recorder",
                    "-w",
                    "eDP",
                    "-ro",
                    "/tmp/wt-gsr",
                    "-o",
                    "/tmp/wt-gsr/other",
                ],
            ),
            process(
                41,
                &[
                    "gpu-screen-recorder",
                    "-w",
                    "eDP",
                    "-ro",
                    "/tmp/wt-gsr",
                    "-o",
                    "/tmp/wt-gsr/wtclip",
                ],
            ),
        ];

        let selected =
            select_actual_gsr_process(&processes, &config, &output_dir, &output_prefix, "eDP")
                .unwrap();

        assert_eq!(selected.pid, 41);
    }

    #[test]
    fn save_replay_sends_signal_to_recorder_pid_not_wrapper_pid() {
        let mut state = GpuScreenRecorderState {
            target_valid: true,
            mode: Some(GsrMode::Flatpak),
            wrapper_pid: Some(100),
            recorder_pid: Some(101),
            health: GsrHealth::Running,
            target: "eDP".to_owned(),
            output_dir: PathBuf::from("/tmp/wt-gsr"),
            output_prefix: PathBuf::from("/tmp/wt-gsr/wtclip"),
            last_output: None,
            last_error: None,
            restart_count: 0,
            command_line: None,
            recorder_command_line: None,
            monitors: Vec::new(),
            stderr_handling: "null".to_owned(),
            save_queue_len: 0,
            total_saves_requested: 0,
            total_saves_completed: 0,
            total_saves_failed: 0,
            capture_strategy: CaptureStrategy::Monitor,
            session_type: "x11".to_owned(),
            target_reason: "test".to_owned(),
        };

        assert_eq!(signal_pid_from_state(&state).unwrap(), 101);
        state.recorder_pid = None;
        assert!(signal_pid_from_state(&state).is_err());
    }

    #[test]
    fn restart_leaves_only_one_recorder_process() {
        let config = test_config(PathBuf::from("/tmp/wt-gsr"));
        let output_dir = PathBuf::from("/tmp/wt-gsr");
        let output_prefix = output_dir.join("wtclip");
        let processes = vec![
            process(
                50,
                &[
                    "gpu-screen-recorder",
                    "-w",
                    "eDP",
                    "-ro",
                    "/tmp/wt-gsr",
                    "-o",
                    "/tmp/wt-gsr/wtclip",
                ],
            ),
            process(
                51,
                &[
                    "gpu-screen-recorder",
                    "-w",
                    "eDP",
                    "-ro",
                    "/tmp/wt-gsr",
                    "-o",
                    "/tmp/wt-gsr/wtclip",
                ],
            ),
        ];

        let selected =
            select_actual_gsr_process(&processes, &config, &output_dir, &output_prefix, "eDP")
                .unwrap();

        assert_eq!(selected.pid, 51);
    }

    #[tokio::test]
    async fn diagnostics_reports_wrapper_and_recorder_pid() {
        let config = test_config(PathBuf::from("/tmp/wt-gsr"));
        let handle = GpuScreenRecorderHandle::new(config);
        {
            let mut inner = handle.inner.lock().await;
            inner.state.wrapper_pid = Some(200);
            inner.state.recorder_pid = Some(201);
            inner.state.recorder_command_line =
                Some("gpu-screen-recorder -w eDP -ro /tmp/wt-gsr -o /tmp/wt-gsr/wtclip".to_owned());
        }
        let status = handle.status().await;

        assert_eq!(status.wrapper_pid, Some(200));
        assert_eq!(status.recorder_pid, Some(201));
        assert_eq!(status.signal_pid, Some(201));
        assert!(status
            .recorder_command_line
            .as_deref()
            .unwrap()
            .contains("gpu-screen-recorder -w eDP"));
    }

    #[tokio::test]
    async fn stderr_is_not_left_piped_unread() {
        let config = test_config(PathBuf::from("/tmp/wt-gsr"));
        let handle = GpuScreenRecorderHandle::new(config);
        let status = handle.status().await;

        assert_eq!(status.stderr_handling, "null");
    }

    #[test]
    fn metadata_contains_gsr_backend() {
        let root = temp_dir("metadata");
        let video = root.join("Replay_001.mp4");
        fs::write(&video, b"mp4").unwrap();
        let metadata = video.with_extension("json");
        let config = test_config(root.clone());
        let context = ClipContext {
            reason: ClipReason::Manual,
            event: None,
            events: Vec::new(),
            player_name: Some("dawson16800".to_owned()),
            pending_clip_id: None,
            pending_dedupe_key: None,
            duration_seconds: 25,
            post_event_seconds: 0,
            first_event_time: None,
            last_event_time: None,
        };

        let thumbnail = video.with_extension("jpg");
        fs::write(&thumbnail, b"jpg").unwrap();
        write_gsr_metadata(&metadata, &video, Some(&thumbnail), &context, &config, 17).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(metadata).unwrap()).unwrap();

        assert_eq!(json["capture_backend"], "gpu_screen_recorder");
        assert_eq!(json["reason"], "manual");
        assert_eq!(json["clip_type"], "manual");
        assert_eq!(json["game"], "War Thunder");
        assert_eq!(json["path"], video.display().to_string());
        assert_eq!(json["source_path"], video.display().to_string());
        assert_eq!(json["thumbnail_path"], thumbnail.display().to_string());
        assert_eq!(json["container"], "mp4");
        assert_eq!(json["duration_seconds"], 17);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn metadata_uses_ffprobe_duration_when_available() {
        let root = temp_dir("metadata-duration");
        let video = root.join("Replay_001.mp4");
        fs::write(&video, b"mp4").unwrap();
        let metadata = video.with_extension("json");
        let config = test_config(root.clone());
        let context = ClipContext {
            reason: ClipReason::Manual,
            event: None,
            events: Vec::new(),
            player_name: None,
            pending_clip_id: None,
            pending_dedupe_key: None,
            duration_seconds: 25,
            post_event_seconds: 0,
            first_event_time: None,
            last_event_time: None,
        };

        write_gsr_metadata(&metadata, &video, None, &context, &config, 9).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(metadata).unwrap()).unwrap();

        assert_eq!(json["duration_seconds"], 9);
        fs::remove_dir_all(root).unwrap();
    }
}
