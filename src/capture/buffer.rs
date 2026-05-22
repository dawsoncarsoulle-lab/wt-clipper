use std::{fs, os::fd::AsRawFd, path::PathBuf, time::Duration};

use anyhow::Context;
use chrono::{DateTime, Local};
use gst::prelude::*;
use gstreamer as gst;
use serde::Serialize;
use tokio::{
    sync::mpsc,
    time::{interval, MissedTickBehavior},
};
use tracing::{debug, info};
use uuid::Uuid;

use crate::{
    capture::{
        concat::concatenate_segments_to_webm,
        output::{default_output_dir, slugify_filename_part},
        portal::PortalScreencastSession,
        quality::{QualityPreset, VideoQuality},
        recorder::{
            choose_backend, encode_location, wait_for_eos_or_error, CaptureBackend, PipelineSource,
        },
        segments::{
            prune_old_segments, segment_file_name, segment_location_pattern, segments_to_keep,
            snapshot_recent_segments, ReplaySegment,
        },
    },
    cli::CaptureSource,
    warthunder::events::WarThunderEvent,
};

#[derive(Debug, Clone)]
pub struct ReplayBufferConfig {
    pub buffer_seconds: u64,
    pub segment_seconds: u64,
    pub output_dir: Option<PathBuf>,
    pub source: CaptureSource,
    pub keep_segments: bool,
    pub quality_preset: QualityPreset,
    pub quality: VideoQuality,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedReplay {
    pub final_video_path: Option<PathBuf>,
    pub metadata_path: Option<PathBuf>,
    pub segments_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipReason {
    TargetDestroyed,
    PlayerDestroyed,
    Manual,
    Unknown,
}

impl ClipReason {
    pub fn slug(self) -> &'static str {
        match self {
            Self::TargetDestroyed => "target-destroyed",
            Self::PlayerDestroyed => "player-destroyed",
            Self::Manual => "manual",
            Self::Unknown => "unknown",
        }
    }

    fn file_prefix(self) -> &'static str {
        match self {
            Self::TargetDestroyed => "kill",
            Self::PlayerDestroyed => "death",
            Self::Manual => "manual",
            Self::Unknown => "clip",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ClipContext {
    pub reason: ClipReason,
    pub event: Option<WarThunderEvent>,
    pub player_name: Option<String>,
    pub video_quality: VideoQuality,
    pub quality_preset: QualityPreset,
    pub duration_seconds: u64,
    pub post_event_seconds: u64,
    pub segment_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetDetails {
    pub display_name: String,
    pub vehicle: Option<String>,
}

pub struct ReplayBufferHandle {
    config: ReplayBufferConfig,
    temp_dir: PathBuf,
    keep_segments: usize,
    pipeline: gst::Pipeline,
    portal_session: Option<PortalScreencastSession>,
}

impl ReplayBufferHandle {
    pub async fn start(config: ReplayBufferConfig) -> anyhow::Result<Self> {
        if config.buffer_seconds == 0 {
            anyhow::bail!("buffer seconds must be greater than zero");
        }
        if config.segment_seconds == 0 {
            anyhow::bail!("segment seconds must be greater than zero");
        }

        gst::init().context("failed to initialize GStreamer")?;
        info!(video = %config.quality.log_summary(), "replay buffer video target");

        let temp_dir = std::env::temp_dir()
            .join("wt-clipper-buffer")
            .join(format!("session-{}", Uuid::new_v4()));
        fs::create_dir_all(&temp_dir)
            .with_context(|| format!("failed to create buffer directory {}", temp_dir.display()))?;

        let keep_segments = segments_to_keep(config.buffer_seconds, config.segment_seconds);
        let mut portal_session = None;
        let backend = choose_backend(&std::env::var("XDG_SESSION_TYPE").unwrap_or_default());
        let source = match backend {
            CaptureBackend::X11 => PipelineSource::X11,
            CaptureBackend::ManualPipeWirePath(path) => PipelineSource::PipeWirePath(path),
            CaptureBackend::ManualPipeWireTarget(target) => PipelineSource::PipeWireTarget(target),
            CaptureBackend::PortalPipeWire => {
                let session = PortalScreencastSession::start(config.source).await?;
                let source = PipelineSource::PipeWirePortal {
                    fd: session.pipewire_fd().as_raw_fd(),
                    node_id: session.node_id(),
                };
                portal_session = Some(session);
                source
            }
        };

        let pipeline_description = buffer_pipeline_description(
            source,
            config.source,
            segment_location_pattern(&temp_dir),
            Duration::from_secs(config.segment_seconds),
            keep_segments,
            config.quality,
        )?;
        info!(pipeline = %pipeline_description, "starting replay buffer pipeline");

        let element =
            gst::parse::launch(&pipeline_description).context("failed to build pipeline")?;
        let pipeline = element
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow::anyhow!("GStreamer description did not create a pipeline"))?;
        pipeline
            .set_state(gst::State::Playing)
            .context("failed to start replay buffer pipeline")?;

        Ok(Self {
            config,
            temp_dir,
            keep_segments,
            pipeline,
            portal_session,
        })
    }

    pub async fn save_replay(&self, context: ClipContext) -> anyhow::Result<Option<SavedReplay>> {
        let temp_dir = self.temp_dir.clone();
        let keep_segments = self.keep_segments;
        let output_dir = self.config.output_dir.clone();
        let keep_saved_segments = self.config.keep_segments;
        tokio::task::spawn_blocking(move || {
            save_replay_clip(
                &temp_dir,
                keep_segments,
                output_dir,
                keep_saved_segments,
                context,
            )
        })
        .await?
    }

    pub fn prune(&self) -> anyhow::Result<()> {
        prune_old_segments(&self.temp_dir, self.keep_segments)?;
        check_pipeline_bus(&self.pipeline)
    }

    pub async fn stop(self) -> anyhow::Result<()> {
        info!("sending EOS to replay buffer pipeline");
        self.pipeline.send_event(gst::event::Eos::new());
        let result = wait_for_eos_or_error(&self.pipeline);
        if let Err(error) = self.pipeline.set_state(gst::State::Null) {
            if result.is_ok() {
                return Err(error).context("failed to stop replay buffer pipeline");
            }
            debug!(%error, "failed to stop replay buffer pipeline after buffer error");
        }
        if let Some(session) = &self.portal_session {
            if let Err(error) = session.close().await {
                if result.is_ok() {
                    return Err(error);
                }
                debug!(%error, "failed to close portal session after buffer error");
            }
        }
        if let Err(error) = fs::remove_dir_all(&self.temp_dir) {
            debug!(%error, path = %self.temp_dir.display(), "failed to remove temporary buffer directory");
        }
        result
    }

    pub fn manual_clip_context(&self) -> ClipContext {
        ClipContext {
            reason: ClipReason::Manual,
            event: None,
            player_name: None,
            video_quality: self.config.quality,
            quality_preset: self.config.quality_preset,
            duration_seconds: self.config.buffer_seconds,
            post_event_seconds: 0,
            segment_seconds: self.config.segment_seconds,
        }
    }
}

pub async fn run_replay_buffer(config: ReplayBufferConfig) -> anyhow::Result<()> {
    let handle = ReplayBufferHandle::start(config).await?;
    run_buffer_loop(handle).await
}

async fn run_buffer_loop(handle: ReplayBufferHandle) -> anyhow::Result<()> {
    println!("Replay buffer active: {}s", handle.config.buffer_seconds);
    println!("Video target: {}", handle.config.quality.log_summary());
    println!("Press Enter to save a replay clip.");
    println!("Press Ctrl+C to stop.");

    let mut save_requests = spawn_enter_listener();
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    let mut cleanup = interval(Duration::from_secs(1));
    cleanup.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("received Ctrl+C, stopping replay buffer");
                break;
            }
            request = save_requests.recv() => {
                if request.is_some() {
                    println!("[CLIP] saving replay...");
                    let context = handle.manual_clip_context();
                    if let Some(replay) = handle.save_replay(context).await? {
                        print_saved_replay(&replay);
                    }
                }
            }
            _ = cleanup.tick() => {
                handle.prune()?;
            }
        }
    }

    handle.stop().await
}

fn spawn_enter_listener() -> mpsc::UnboundedReceiver<()> {
    let (sender, receiver) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        loop {
            let mut line = String::new();
            match stdin.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if sender.send(()).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    tracing::debug!(%error, "failed to read replay save request from stdin");
                    break;
                }
            }
        }
    });
    receiver
}

fn save_replay_clip(
    temp_dir: &std::path::Path,
    keep_segments: usize,
    output_dir: Option<PathBuf>,
    keep_saved_segments: bool,
    context: ClipContext,
) -> anyhow::Result<Option<SavedReplay>> {
    let segments = snapshot_recent_segments(temp_dir, keep_segments)?;
    if segments.is_empty() {
        println!("No finalized replay segments are available yet.");
        return Ok(None);
    }

    let created_at = Local::now();
    let parent = resolve_clip_parent(output_dir)?;
    let paths = resolve_clip_paths(&parent, &context, created_at)?;
    let segment_dir = paths.segments_dir.clone();
    fs::create_dir_all(&segment_dir)?;
    copy_segments(&segments, &segment_dir)?;

    let segment_paths = copied_segment_paths(&segment_dir)?;
    println!("[CLIP] assembling replay video...");
    match concatenate_segments_to_webm(
        &segment_paths,
        paths.final_video_path.clone(),
        context.video_quality,
    ) {
        Ok(path) => {
            let metadata = build_clip_metadata(
                &context,
                created_at,
                &path,
                keep_saved_segments.then_some(&segment_dir),
            );
            write_clip_metadata(&paths.metadata_path, &metadata)?;
            if !keep_saved_segments {
                fs::remove_dir_all(&segment_dir)?;
                Ok(Some(SavedReplay {
                    final_video_path: Some(path),
                    metadata_path: Some(paths.metadata_path),
                    segments_dir: None,
                }))
            } else {
                Ok(Some(SavedReplay {
                    final_video_path: Some(path),
                    metadata_path: Some(paths.metadata_path),
                    segments_dir: Some(segment_dir),
                }))
            }
        }
        Err(error) => {
            println!(
                "[CLIP] failed to assemble final video, segments kept at: {}",
                segment_dir.display()
            );
            Err(error)
        }
    }
}

#[derive(Debug)]
struct ClipPaths {
    final_video_path: PathBuf,
    metadata_path: PathBuf,
    segments_dir: PathBuf,
}

fn resolve_clip_parent(output_dir: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let parent = match output_dir {
        Some(path) => path,
        None => default_output_dir()?,
    };
    fs::create_dir_all(&parent)?;
    Ok(parent)
}

fn resolve_clip_paths(
    parent: &std::path::Path,
    context: &ClipContext,
    created_at: DateTime<Local>,
) -> anyhow::Result<ClipPaths> {
    let base = clip_file_stem(context, created_at);
    for index in 0..10_000 {
        let candidate = if index == 0 {
            base.clone()
        } else {
            format!("{base}-{index}")
        };
        let final_video_path = parent.join(format!("{candidate}.webm"));
        let metadata_path = parent.join(format!("{candidate}.json"));
        let segments_dir = parent.join(format!("{candidate}-segments"));
        if !final_video_path.exists() && !metadata_path.exists() && !segments_dir.exists() {
            return Ok(ClipPaths {
                final_video_path,
                metadata_path,
                segments_dir,
            });
        }
    }

    anyhow::bail!("could not find a unique clip path in {}", parent.display())
}

fn clip_file_stem(context: &ClipContext, created_at: DateTime<Local>) -> String {
    let timestamp = created_at.format("%Y-%m-%d-%H-%M-%S");
    match &context.event {
        Some(WarThunderEvent::TargetDestroyed {
            vehicle, target, ..
        }) if context.reason == ClipReason::TargetDestroyed => {
            let vehicle = vehicle
                .as_deref()
                .map(slugify_filename_part)
                .unwrap_or_else(|| "unknown".to_owned());
            let target_vehicle = target
                .as_deref()
                .map(parse_target_details)
                .and_then(|details| details.vehicle.or(Some(details.display_name)))
                .map(|value| slugify_filename_part(&value))
                .unwrap_or_else(|| "unknown".to_owned());
            format!("kill-{timestamp}-{vehicle}-vs-{target_vehicle}")
        }
        _ => format!("{}-{timestamp}", context.reason.file_prefix()),
    }
}

pub fn parse_target_details(target: &str) -> TargetDetails {
    let target = target.trim();
    if let Some(open_paren) = target.rfind(" (") {
        if target.ends_with(')') && open_paren + 2 < target.len() - 1 {
            let display_name = target[..open_paren].trim();
            let vehicle = target[open_paren + 2..target.len() - 1].trim();
            return TargetDetails {
                display_name: non_empty_or_unknown(display_name),
                vehicle: non_empty_string(vehicle),
            };
        }
    }

    let display_name = non_empty_or_unknown(target);
    let vehicle = target
        .strip_prefix("[ai] ")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(target);

    TargetDetails {
        display_name,
        vehicle: non_empty_string(vehicle),
    }
}

fn non_empty_string(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value.trim().to_owned())
    }
}

fn non_empty_or_unknown(value: &str) -> String {
    non_empty_string(value).unwrap_or_else(|| "unknown".to_owned())
}

#[derive(Debug, Serialize)]
struct ClipMetadata {
    created_by: &'static str,
    version: &'static str,
    timestamp: String,
    reason: &'static str,
    player_name: Option<String>,
    attacker: Option<String>,
    vehicle: Option<String>,
    action: Option<String>,
    target: Option<String>,
    target_vehicle: Option<String>,
    raw_event: Option<String>,
    duration_seconds: u64,
    post_event_seconds: u64,
    segment_seconds: u64,
    quality: &'static str,
    fps: u32,
    video_bitrate_kbps: u32,
    event: Option<serde_json::Value>,
    segments_dir: Option<String>,
    video_path: String,
}

fn build_clip_metadata(
    context: &ClipContext,
    created_at: DateTime<Local>,
    video_path: &std::path::Path,
    segments_dir: Option<&PathBuf>,
) -> ClipMetadata {
    let (attacker, vehicle, action, target, target_vehicle, raw_event, event) = match &context.event
    {
        Some(WarThunderEvent::TargetDestroyed {
            attacker,
            action,
            vehicle,
            target,
            raw,
        }) => {
            let target_vehicle = target
                .as_deref()
                .and_then(|value| parse_target_details(value).vehicle);
            (
                attacker.clone(),
                vehicle.clone(),
                Some(action.clone()),
                target.clone(),
                target_vehicle,
                Some(raw.clone()),
                Some(serde_json::json!({
                    "type": "target_destroyed",
                    "attacker": attacker,
                    "vehicle": vehicle,
                    "action": action,
                    "target": target,
                    "raw": raw,
                })),
            )
        }
        Some(WarThunderEvent::PlayerDestroyed { raw }) => (
            None,
            None,
            None,
            None,
            None,
            Some(raw.clone()),
            Some(serde_json::json!({ "type": "player_destroyed", "raw": raw })),
        ),
        Some(WarThunderEvent::CriticalHit { raw }) => (
            None,
            None,
            None,
            None,
            None,
            Some(raw.clone()),
            Some(serde_json::json!({ "type": "critical_hit", "raw": raw })),
        ),
        Some(WarThunderEvent::SevereDamage { raw }) => (
            None,
            None,
            None,
            None,
            None,
            Some(raw.clone()),
            Some(serde_json::json!({ "type": "severe_damage", "raw": raw })),
        ),
        Some(WarThunderEvent::BaseDestroyed { raw }) => (
            None,
            None,
            None,
            None,
            None,
            Some(raw.clone()),
            Some(serde_json::json!({ "type": "base_destroyed", "raw": raw })),
        ),
        Some(WarThunderEvent::Unknown(raw)) => (
            None,
            None,
            None,
            None,
            None,
            Some(raw.clone()),
            Some(serde_json::json!({ "type": "unknown", "raw": raw })),
        ),
        None => (None, None, None, None, None, None, None),
    };

    ClipMetadata {
        created_by: "wt-clipper",
        version: env!("CARGO_PKG_VERSION"),
        timestamp: created_at.to_rfc3339(),
        reason: context.reason.slug(),
        player_name: context.player_name.clone(),
        attacker,
        vehicle,
        action,
        target,
        target_vehicle,
        raw_event,
        duration_seconds: context.duration_seconds,
        post_event_seconds: context.post_event_seconds,
        segment_seconds: context.segment_seconds,
        quality: context.quality_preset.as_str(),
        fps: context.video_quality.fps,
        video_bitrate_kbps: context.video_quality.video_bitrate_kbps,
        event,
        segments_dir: segments_dir.map(|path| path.display().to_string()),
        video_path: video_path.display().to_string(),
    }
}

fn write_clip_metadata(path: &std::path::Path, metadata: &ClipMetadata) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(metadata)?;
    fs::write(path, json).with_context(|| format!("failed to write metadata {}", path.display()))
}

fn copied_segment_paths(segment_dir: &std::path::Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(segment_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) == Some("webm") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

pub fn print_saved_replay(replay: &SavedReplay) {
    if let Some(path) = &replay.final_video_path {
        println!("[CLIP] saved: {}", path.display());
    } else if let Some(path) = &replay.segments_dir {
        println!("[CLIP] saved segments: {}", path.display());
    }
    if let Some(path) = &replay.metadata_path {
        println!("[CLIP] metadata: {}", path.display());
    }
}

fn copy_segments(segments: &[ReplaySegment], clip_dir: &std::path::Path) -> anyhow::Result<()> {
    for (clip_index, segment) in segments.iter().enumerate() {
        let destination = clip_dir.join(segment_file_name(clip_index as u64));
        fs::copy(&segment.path, &destination).with_context(|| {
            format!(
                "failed to copy replay segment {} to {}",
                segment.path.display(),
                destination.display()
            )
        })?;
    }
    Ok(())
}

fn check_pipeline_bus(pipeline: &gst::Pipeline) -> anyhow::Result<()> {
    let Some(bus) = pipeline.bus() else {
        return Ok(());
    };

    while let Some(message) = bus.timed_pop(gst::ClockTime::ZERO) {
        use gst::MessageView;
        match message.view() {
            MessageView::Error(error) => {
                anyhow::bail!(
                    "GStreamer error from {:?}: {} ({:?})",
                    error.src().map(|src| src.path_string()),
                    error.error(),
                    error.debug()
                );
            }
            MessageView::Eos(_) => anyhow::bail!("replay buffer pipeline ended unexpectedly"),
            _ => {}
        }
    }

    Ok(())
}

pub(crate) fn buffer_pipeline_description(
    source: PipelineSource,
    capture_source: CaptureSource,
    location_pattern: PathBuf,
    segment_duration: Duration,
    keep_segments: usize,
    quality: VideoQuality,
) -> anyhow::Result<String> {
    let source_chain = match source {
        PipelineSource::X11 => {
            if capture_source == CaptureSource::Window {
                anyhow::bail!(
                    "--source window is only supported through the Wayland portal for now"
                );
            }
            "ximagesrc use-damage=0 show-pointer=true".to_owned()
        }
        PipelineSource::PipeWirePath(path) => {
            format!(
                "pipewiresrc path=\"{}\" do-timestamp=true",
                path.replace('"', "\\\"")
            )
        }
        PipelineSource::PipeWireTarget(target) => {
            format!(
                "pipewiresrc target-object=\"{}\" do-timestamp=true",
                target.replace('"', "\\\"")
            )
        }
        PipelineSource::PipeWirePortal { fd, node_id } => {
            format!("pipewiresrc fd={fd} path={node_id} do-timestamp=true")
        }
    };
    let location = encode_location(&location_pattern);
    let max_size_time = segment_duration.as_nanos();
    let raw_caps = quality.raw_video_caps();
    let encoder = quality.vp8enc_settings();
    debug!(
        target_bitrate = quality.bitrate_bps(),
        "segment target-bitrate"
    );
    Ok(format!(
        "{source_chain} ! videoconvert ! videorate ! {raw_caps} ! queue ! {encoder} ! splitmuxsink async-finalize=true muxer-factory=webmmux max-size-time={max_size_time} max-files={keep_segments} send-keyframe-requests=true location=\"{location}\""
    ))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn builds_splitmux_pipeline() {
        let pipeline = buffer_pipeline_description(
            PipelineSource::PipeWirePortal { fd: 8, node_id: 99 },
            CaptureSource::Screen,
            Path::new("/tmp/session/segment-%06d.webm").to_path_buf(),
            Duration::from_secs(2),
            7,
            VideoQuality::default(),
        )
        .unwrap();

        assert!(pipeline.contains("pipewiresrc fd=8 path=99"));
        assert!(pipeline.contains("splitmuxsink"));
        assert!(pipeline.contains("video/x-raw,framerate=60/1"));
        assert!(pipeline.contains(
            "vp8enc deadline=1 end-usage=cbr target-bitrate=20000000 cpu-used=2 keyframe-max-dist=120"
        ));
        assert!(pipeline.contains("muxer-factory=webmmux"));
        assert!(pipeline.contains("max-size-time=2000000000"));
        assert!(pipeline.contains("max-files=7"));
    }

    #[test]
    fn clip_reason_slug() {
        assert_eq!(ClipReason::TargetDestroyed.slug(), "target-destroyed");
        assert_eq!(ClipReason::PlayerDestroyed.slug(), "player-destroyed");
        assert_eq!(ClipReason::Manual.slug(), "manual");
        assert_eq!(ClipReason::Unknown.slug(), "unknown");
    }

    #[test]
    fn saved_replay_can_hold_final_video() {
        let replay = SavedReplay {
            final_video_path: Some(PathBuf::from("/tmp/replay.webm")),
            metadata_path: Some(PathBuf::from("/tmp/replay.json")),
            segments_dir: None,
        };

        assert_eq!(
            replay.final_video_path.as_deref(),
            Some(std::path::Path::new("/tmp/replay.webm"))
        );
    }

    fn test_kill_context() -> ClipContext {
        ClipContext {
            reason: ClipReason::TargetDestroyed,
            event: Some(WarThunderEvent::TargetDestroyed {
                attacker: Some("dawson16800".to_owned()),
                action: "shot down".to_owned(),
                vehicle: Some("F/A-18C Early".to_owned()),
                target: Some("=3BEHO= BoBka_V (MiG-21bis)".to_owned()),
                raw: "dawson16800 (F/A-18C Early) shot down =3BEHO= BoBka_V (MiG-21bis)".to_owned(),
            }),
            player_name: Some("dawson16800".to_owned()),
            video_quality: VideoQuality::default(),
            quality_preset: QualityPreset::High,
            duration_seconds: 20,
            post_event_seconds: 5,
            segment_seconds: 2,
        }
    }

    #[test]
    fn parses_player_target_vehicle() {
        let details = parse_target_details("=3BEHO= BoBka_V (MiG-21bis)");

        assert_eq!(details.display_name, "=3BEHO= BoBka_V");
        assert_eq!(details.vehicle.as_deref(), Some("MiG-21bis"));
    }

    #[test]
    fn parses_ai_target_vehicle() {
        let details = parse_target_details("[ai] MiG-15bis");

        assert_eq!(details.display_name, "[ai] MiG-15bis");
        assert_eq!(details.vehicle.as_deref(), Some("MiG-15bis"));
    }

    #[test]
    fn builds_informative_kill_file_stem() {
        let created_at = DateTime::parse_from_rfc3339("2026-05-22T21:12:14+02:00")
            .unwrap()
            .with_timezone(&Local);

        assert_eq!(
            clip_file_stem(&test_kill_context(), created_at),
            "kill-2026-05-22-21-12-14-f-a-18c-early-vs-mig-21bis"
        );
    }

    #[test]
    fn builds_manual_file_stem() {
        let created_at = DateTime::parse_from_rfc3339("2026-05-22T21:12:14+02:00")
            .unwrap()
            .with_timezone(&Local);

        let context = ClipContext {
            reason: ClipReason::Manual,
            event: None,
            player_name: None,
            video_quality: VideoQuality::default(),
            quality_preset: QualityPreset::High,
            duration_seconds: 20,
            post_event_seconds: 0,
            segment_seconds: 2,
        };

        assert_eq!(
            clip_file_stem(&context, created_at),
            "manual-2026-05-22-21-12-14"
        );
    }

    #[test]
    fn metadata_json_contains_expected_fields() {
        let created_at = DateTime::parse_from_rfc3339("2026-05-22T21:12:14+02:00")
            .unwrap()
            .with_timezone(&Local);
        let metadata = build_clip_metadata(
            &test_kill_context(),
            created_at,
            Path::new("/tmp/kill.webm"),
            Some(&PathBuf::from("/tmp/kill-segments")),
        );
        let json = serde_json::to_value(&metadata).unwrap();

        assert_eq!(json["created_by"], "wt-clipper");
        assert_eq!(json["version"], "0.1.0");
        assert_eq!(json["reason"], "target-destroyed");
        assert_eq!(json["player_name"], "dawson16800");
        assert_eq!(json["attacker"], "dawson16800");
        assert_eq!(json["vehicle"], "F/A-18C Early");
        assert_eq!(json["action"], "shot down");
        assert_eq!(json["target_vehicle"], "MiG-21bis");
        assert_eq!(json["duration_seconds"], 20);
        assert_eq!(json["post_event_seconds"], 5);
        assert_eq!(json["segment_seconds"], 2);
        assert_eq!(json["quality"], "high");
        assert_eq!(json["fps"], 60);
        assert_eq!(json["video_bitrate_kbps"], 20_000);
        assert_eq!(json["segments_dir"], "/tmp/kill-segments");
    }
}
