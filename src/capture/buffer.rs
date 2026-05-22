use std::{fs, os::fd::AsRawFd, path::PathBuf, time::Duration};

use anyhow::Context;
use gst::prelude::*;
use gstreamer as gst;
use tokio::{
    sync::mpsc,
    time::{interval, MissedTickBehavior},
};
use tracing::{debug, info};
use uuid::Uuid;

use crate::{
    capture::{
        concat::concatenate_segments_to_webm,
        output::{ensure_unique_path, resolve_replay_clip_dir_with_reason},
        portal::PortalScreencastSession,
        quality::VideoQuality,
        recorder::{
            choose_backend, encode_location, wait_for_eos_or_error, CaptureBackend, PipelineSource,
        },
        segments::{
            prune_old_segments, segment_file_name, segment_location_pattern, segments_to_keep,
            snapshot_recent_segments, ReplaySegment,
        },
    },
    cli::CaptureSource,
};

#[derive(Debug, Clone)]
pub struct ReplayBufferConfig {
    pub buffer_seconds: u64,
    pub segment_seconds: u64,
    pub output_dir: Option<PathBuf>,
    pub source: CaptureSource,
    pub keep_segments: bool,
    pub quality: VideoQuality,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedReplay {
    pub final_video_path: Option<PathBuf>,
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

    pub async fn save_replay(&self, reason: ClipReason) -> anyhow::Result<Option<SavedReplay>> {
        let temp_dir = self.temp_dir.clone();
        let keep_segments = self.keep_segments;
        let output_dir = self.config.output_dir.clone();
        let keep_saved_segments = self.config.keep_segments;
        let quality = self.config.quality;
        tokio::task::spawn_blocking(move || {
            save_replay_clip(
                &temp_dir,
                keep_segments,
                output_dir,
                reason,
                keep_saved_segments,
                quality,
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
}

pub async fn run_replay_buffer(config: ReplayBufferConfig) -> anyhow::Result<()> {
    let handle = ReplayBufferHandle::start(config).await?;
    run_buffer_loop(handle).await
}

async fn run_buffer_loop(handle: ReplayBufferHandle) -> anyhow::Result<()> {
    println!("Replay buffer active: {}s", handle.config.buffer_seconds);
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
                    if let Some(replay) = handle.save_replay(ClipReason::Manual).await? {
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
    reason: ClipReason,
    keep_saved_segments: bool,
    quality: VideoQuality,
) -> anyhow::Result<Option<SavedReplay>> {
    let segments = snapshot_recent_segments(temp_dir, keep_segments)?;
    if segments.is_empty() {
        println!("No finalized replay segments are available yet.");
        return Ok(None);
    }

    let segment_dir = resolve_replay_clip_dir_with_reason(output_dir, Some(reason.slug()))?;
    fs::create_dir_all(&segment_dir)?;
    copy_segments(&segments, &segment_dir)?;

    let segment_paths = copied_segment_paths(&segment_dir)?;
    let final_video_path = ensure_unique_path(segment_dir.with_extension("webm"))?;
    println!("[CLIP] assembling replay video...");
    match concatenate_segments_to_webm(&segment_paths, final_video_path.clone(), quality) {
        Ok(path) => {
            if !keep_saved_segments {
                fs::remove_dir_all(&segment_dir)?;
                Ok(Some(SavedReplay {
                    final_video_path: Some(path),
                    segments_dir: None,
                }))
            } else {
                Ok(Some(SavedReplay {
                    final_video_path: Some(path),
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
        assert!(pipeline.contains("target-bitrate=12000000"));
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
            segments_dir: None,
        };

        assert_eq!(
            replay.final_video_path.as_deref(),
            Some(std::path::Path::new("/tmp/replay.webm"))
        );
    }
}
