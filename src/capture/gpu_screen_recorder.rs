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
    capture::{
        buffer::{ClipContext, ClipReason, SavedReplay},
        output::slugify_filename_part,
    },
    config::{CaptureConfig, GpuScreenRecorderMode},
    warthunder::events::WarThunderEvent,
};

const FLATPAK_APP_ID: &str = "com.dec05eba.gpu_screen_recorder";
const REPLAY_SAVE_TIMEOUT: Duration = Duration::from_secs(30);
const STABLE_FILE_DURATION: Duration = Duration::from_millis(750);
const STABLE_FILE_POLL: Duration = Duration::from_millis(250);
const STOP_TIMEOUT: Duration = Duration::from_secs(3);

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GsrStatus {
    pub available: bool,
    pub mode: Option<GsrMode>,
    pub health: GsrHealth,
    pub pid: Option<u32>,
    pub target: String,
    pub monitors: Vec<String>,
    pub command_line: Option<String>,
    pub output_dir: PathBuf,
    pub output_prefix: PathBuf,
    pub last_output: Option<PathBuf>,
    pub last_error: Option<String>,
    pub restart_count: u64,
    pub replay_seconds: u64,
    pub fps: u32,
    pub quality: String,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedGsrReplay {
    pub final_video_path: PathBuf,
    pub metadata_path: PathBuf,
    pub thumbnail_path: Option<PathBuf>,
    pub duration_seconds: u64,
    pub size_bytes: u64,
}

impl SavedGsrReplay {
    pub fn into_saved_replay(self) -> SavedReplay {
        SavedReplay {
            final_video_path: Some(self.final_video_path),
            metadata_path: Some(self.metadata_path),
            segments_dir: None,
        }
    }
}

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
    pub pid: Option<u32>,
    pub health: GsrHealth,
    pub target: String,
    pub output_dir: PathBuf,
    pub output_prefix: PathBuf,
    pub last_output: Option<PathBuf>,
    pub last_error: Option<String>,
    pub restart_count: u64,
    pub command_line: Option<String>,
    pub monitors: Vec<String>,
}

#[derive(Debug, Serialize)]
struct GsrClipMetadata {
    created_by: &'static str,
    capture_backend: &'static str,
    kind: &'static str,
    reason: &'static str,
    player_name: Option<String>,
    raw_event: Option<String>,
    video_path: String,
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
            pid: None,
            health: GsrHealth::Stopped,
            target: config.target.clone(),
            output_dir,
            output_prefix,
            last_output: None,
            last_error: None,
            restart_count: 0,
            command_line: None,
            monitors: list_monitors(&config),
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

    pub async fn start(&self) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if child_is_running(inner.child.as_mut()) {
            inner.state.health = GsrHealth::Running;
            return Ok(());
        }
        inner.child = None;
        inner.state.health = GsrHealth::Starting;
        inner.state.last_error = None;

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
            .stderr(Stdio::piped());

        info!(command = %command_line.command_line, "starting GPU Screen Recorder");
        match command.spawn() {
            Ok(child) => {
                inner.state.mode = Some(command_line.mode);
                inner.state.pid = Some(child.id());
                inner.state.health = GsrHealth::Running;
                inner.state.command_line = Some(command_line.command_line);
                inner.state.output_dir = command_line.output_dir;
                inner.state.output_prefix = command_line.output_prefix;
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
        stop_child(&mut inner.child).await?;
        inner.state.pid = None;
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
            stop_child(&mut inner.child).await?;
            inner.state.pid = None;
        }
        info!(reason = %reason, "restarting GPU Screen Recorder");
        self.start().await
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
                    inner.state.pid = None;
                    inner.state.health = GsrHealth::Error;
                    inner.state.last_error = Some(message);
                }
                Ok(None) if inner.state.health != GsrHealth::SavingReplay => {
                    inner.state.health = GsrHealth::Running;
                    inner.state.pid = inner.child.as_ref().map(Child::id);
                }
                Ok(None) => {}
                Err(error) => {
                    inner.state.health = GsrHealth::Error;
                    inner.state.last_error = Some(error.to_string());
                }
            }
        } else if inner.state.health != GsrHealth::Error {
            inner.state.health = GsrHealth::Stopped;
            inner.state.pid = None;
        }
        status_from_inner(&inner)
    }

    pub async fn status(&self) -> GsrStatus {
        let inner = self.inner.lock().await;
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
        let _save_guard = self.save_replay_in_progress.lock().await;
        let result = self.save_replay_inner(context).await;
        if let Err(error) = &result {
            println!("[GPU_RECORDER] save failed id={request_id} error={error}");
            let mut inner = self.inner.lock().await;
            inner.state.health = GsrHealth::Error;
            inner.state.last_error = Some(error.to_string());
        }
        result
    }

    async fn save_replay_inner(&self, context: ClipContext) -> anyhow::Result<SavedGsrReplay> {
        let (pid, output_dir, extension, config) = {
            let mut inner = self.inner.lock().await;
            ensure_running(&mut inner)?;
            let pid = inner.state.pid.ok_or_else(|| {
                anyhow::anyhow!("GPU Screen Recorder n'est pas lancé: PID indisponible")
            })?;
            inner.state.health = GsrHealth::SavingReplay;
            (
                pid,
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
        println!("[GPU_RECORDER] sending SIGUSR1 pid={pid}");
        signal_process(pid, libc::SIGUSR1).with_context(|| {
            format!("impossible de demander la sauvegarde du replay au PID GSR {pid}")
        })?;
        println!("[GPU_RECORDER] SIGUSR1 sent successfully pid={pid}");
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

        let metadata_path = final_video_path.with_extension("json");
        write_gsr_metadata(&metadata_path, &final_video_path, &context, &config)?;
        let thumbnail_path = generate_thumbnail(&final_video_path).await;

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
            duration_seconds: config.replay_seconds,
            size_bytes,
        })
    }
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
        config.target.clone(),
        "-f".to_owned(),
        sanitized_fps(config.fps).to_string(),
        "-r".to_owned(),
        config.replay_seconds.max(1).to_string(),
        "-c".to_owned(),
        config.container.as_arg().to_owned(),
        "-k".to_owned(),
        config.codec.as_arg().to_owned(),
        "-encoder".to_owned(),
        config.encoder.as_arg().to_owned(),
        "-q".to_owned(),
        config.quality.as_arg().to_owned(),
        "-restart-replay-on-save".to_owned(),
        "yes".to_owned(),
        "-ro".to_owned(),
        output_dir.display().to_string(),
        "-o".to_owned(),
        output_prefix.display().to_string(),
    ]);

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
    })
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
    let command = build_gsr_command(&inner.config).ok();
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
    GsrStatus {
        available: command
            .as_ref()
            .is_some_and(|command| command_available(&command.program)),
        mode: inner
            .state
            .mode
            .or_else(|| command.as_ref().map(|command| command.mode)),
        health: inner.state.health,
        pid: inner.state.pid,
        target: inner.config.target.clone(),
        monitors: inner.state.monitors.clone(),
        command_line,
        output_dir,
        output_prefix,
        last_output: inner.state.last_output.clone(),
        last_error: inner.state.last_error.clone(),
        restart_count: inner.state.restart_count,
        replay_seconds: inner.config.replay_seconds,
        fps: inner.config.fps,
        quality: inner.config.quality.as_arg().to_owned(),
        codec: inner.config.codec.as_arg().to_owned(),
        container: inner.config.container.as_arg().to_owned(),
        encoder: inner.config.encoder.as_arg().to_owned(),
    }
}

fn ensure_running(inner: &mut GpuScreenRecorderInner) -> anyhow::Result<()> {
    if !child_is_running(inner.child.as_mut()) {
        inner.child = None;
        inner.state.pid = None;
        inner.state.health = GsrHealth::Error;
        let message = "GPU Screen Recorder n'est pas lancé; redémarrez le backend GPU Replay";
        inner.state.last_error = Some(message.to_owned());
        anyhow::bail!(message);
    }
    inner.state.pid = inner.child.as_ref().map(Child::id);
    Ok(())
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

fn signal_process(pid: u32, signal: i32) -> anyhow::Result<()> {
    let result = unsafe { libc::kill(pid as libc::pid_t, signal) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
            .with_context(|| format!("kill({pid}, {signal}) failed"))
    }
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

fn list_monitors(config: &CaptureConfig) -> Vec<String> {
    let mut values = vec![
        config.target.clone(),
        "portal".to_owned(),
        "focused".to_owned(),
    ];
    values.sort();
    values.dedup();
    values
}

fn write_gsr_metadata(
    metadata_path: &Path,
    video_path: &Path,
    context: &ClipContext,
    config: &CaptureConfig,
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
    let metadata = GsrClipMetadata {
        created_by: "wt-clipper",
        capture_backend: "gpu_screen_recorder",
        kind: "clip",
        reason: context.reason.slug(),
        player_name: context.player_name.clone(),
        raw_event,
        video_path: video_path.display().to_string(),
        duration_seconds: config.replay_seconds,
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

fn metadata_event_json(event: &WarThunderEvent) -> serde_json::Value {
    match event {
        WarThunderEvent::TargetDestroyed {
            attacker,
            action,
            vehicle,
            target,
            raw,
        } => serde_json::json!({
            "type": "target_destroyed",
            "attacker": attacker,
            "vehicle": vehicle,
            "action": action,
            "target": target,
            "raw": raw,
        }),
        WarThunderEvent::PlayerDestroyed { raw } => {
            serde_json::json!({ "type": "player_destroyed", "raw": raw })
        }
        WarThunderEvent::CriticalHit { raw } => {
            serde_json::json!({ "type": "critical_hit", "raw": raw })
        }
        WarThunderEvent::SevereDamage { raw } => {
            serde_json::json!({ "type": "severe_damage", "raw": raw })
        }
        WarThunderEvent::BaseDestroyed { raw } => {
            serde_json::json!({ "type": "base_destroyed", "raw": raw })
        }
        WarThunderEvent::Unknown(raw) => serde_json::json!({ "type": "unknown", "raw": raw }),
    }
}

fn raw_event_text(event: &WarThunderEvent) -> Option<String> {
    match event {
        WarThunderEvent::TargetDestroyed { raw, .. }
        | WarThunderEvent::PlayerDestroyed { raw }
        | WarThunderEvent::CriticalHit { raw }
        | WarThunderEvent::SevereDamage { raw }
        | WarThunderEvent::BaseDestroyed { raw } => Some(raw.clone()),
        WarThunderEvent::Unknown(raw) => Some(raw.clone()),
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
    use crate::config::{CaptureBackend, GsrCodec, GsrContainer, GsrEncoder, GsrQuality};

    fn test_config(output_dir: PathBuf) -> CaptureConfig {
        CaptureConfig {
            backend: CaptureBackend::GpuScreenRecorder,
            target: "eDP".to_owned(),
            gpu_screen_recorder_mode: GpuScreenRecorderMode::Flatpak,
            fps: 30,
            replay_seconds: 25,
            container: GsrContainer::Mp4,
            codec: GsrCodec::H264,
            encoder: GsrEncoder::Gpu,
            quality: GsrQuality::Medium,
            output_dir: output_dir.display().to_string(),
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
        assert!(command.command_line.contains("-r 25"));
        assert!(command.command_line.contains("-q medium"));
        assert!(command.command_line.contains("-ro \"/tmp/wt gsr\""));
        assert!(command.command_line.contains("-o \"/tmp/wt gsr/wtclip\""));
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
    fn changing_target_changes_w_argument() {
        let mut config = test_config(PathBuf::from("/tmp/wt-gsr"));
        config.target = "HDMI-A-1-0".to_owned();
        let command = build_gsr_command(&config).unwrap();
        assert!(command
            .args
            .windows(2)
            .any(|args| args == ["-w", "HDMI-A-1-0"]));
    }

    #[test]
    fn changing_quality_changes_q_argument() {
        let mut config = test_config(PathBuf::from("/tmp/wt-gsr"));
        config.quality = GsrQuality::High;
        let command = build_gsr_command(&config).unwrap();
        assert!(command.args.windows(2).any(|args| args == ["-q", "high"]));
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
            video_quality: Default::default(),
            quality_preset: Default::default(),
            duration_seconds: 25,
            post_event_seconds: 0,
            segment_seconds: 2,
            first_event_time: None,
            last_event_time: None,
        };

        write_gsr_metadata(&metadata, &video, &context, &config).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(metadata).unwrap()).unwrap();

        assert_eq!(json["capture_backend"], "gpu_screen_recorder");
        assert_eq!(json["reason"], "manual");
        assert_eq!(json["container"], "mp4");
        fs::remove_dir_all(root).unwrap();
    }
}
