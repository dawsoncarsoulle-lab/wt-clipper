use std::{
    fs,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use gst::prelude::*;
use gstreamer as gst;
use tracing::info;

use crate::{
    capture::{
        audio::{resolve_system_audio_source, AudioCaptureSource},
        portal::PortalScreencastSession,
        quality::VideoQuality,
        x11::resolve_x11_window_id,
    },
    cli::CaptureSource,
};

#[derive(Debug, Clone)]
pub struct RecordingRequest {
    pub duration: Duration,
    pub output_path: PathBuf,
    pub source: CaptureSource,
    pub quality: VideoQuality,
}

pub async fn record(request: RecordingRequest) -> anyhow::Result<()> {
    if request.duration.is_zero() {
        anyhow::bail!("recording duration must be greater than zero");
    }

    if let Some(parent) = request.output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create output directory {}", parent.display()))?;
    }

    gst::init().context("failed to initialize GStreamer")?;
    info!(video = %request.quality.log_summary(), "recording video target");
    let audio_source = resolve_system_audio_source();
    if let Some(audio_source) = &audio_source {
        info!(device = %audio_source.device, "recording system audio");
    } else {
        info!("recording without audio");
    }

    let mut portal_session = None;
    let backend = choose_backend(&std::env::var("XDG_SESSION_TYPE").unwrap_or_default());
    let pipeline_description = match backend {
        CaptureBackend::X11 => {
            let window = resolve_x11_window_id(request.source)?;
            if let Some(window) = &window {
                info!(
                    xid = %format!("{:#x}", window.id),
                    title = ?window.title,
                    class = ?window.class,
                    "capturing X11 window"
                );
            }
            pipeline_description(
                PipelineSource::X11 {
                    window_id: window.map(|window| window.id),
                },
                request.source,
                &request.output_path,
                request.quality,
                audio_source.as_ref(),
            )?
        }
        CaptureBackend::ManualPipeWirePath(path) => pipeline_description(
            PipelineSource::PipeWirePath(path),
            request.source,
            &request.output_path,
            request.quality,
            audio_source.as_ref(),
        )?,
        CaptureBackend::ManualPipeWireTarget(target) => pipeline_description(
            PipelineSource::PipeWireTarget(target),
            request.source,
            &request.output_path,
            request.quality,
            audio_source.as_ref(),
        )?,
        CaptureBackend::PortalPipeWire => {
            let session = PortalScreencastSession::start(request.source).await?;
            let description = pipeline_description(
                PipelineSource::PipeWirePortal {
                    fd: session.pipewire_fd().as_raw_fd(),
                    node_id: session.node_id(),
                },
                request.source,
                &request.output_path,
                request.quality,
                audio_source.as_ref(),
            )?;
            portal_session = Some(session);
            description
        }
    };
    info!(pipeline = %pipeline_description, "starting GStreamer pipeline");

    let result = run_pipeline(
        &pipeline_description,
        request.duration,
        &request.output_path,
    );
    if let Some(session) = &portal_session {
        if let Err(error) = session.close().await {
            if result.is_ok() {
                return Err(error);
            }
            tracing::debug!(%error, "failed to close portal session after recording error");
        }
    }

    result
}

fn run_pipeline(
    pipeline_description: &str,
    duration: Duration,
    output_path: &Path,
) -> anyhow::Result<()> {
    let element = gst::parse::launch(pipeline_description).context("failed to build pipeline")?;
    let pipeline = element
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow::anyhow!("GStreamer description did not create a pipeline"))?;

    let result = run_pipeline_inner(&pipeline, duration, output_path);
    if let Err(error) = pipeline.set_state(gst::State::Null) {
        if result.is_ok() {
            return Err(error).context("failed to stop recording pipeline");
        }
        tracing::debug!(%error, "failed to stop recording pipeline after recording error");
    }
    result
}

fn run_pipeline_inner(
    pipeline: &gst::Pipeline,
    duration: Duration,
    output_path: &Path,
) -> anyhow::Result<()> {
    pipeline
        .set_state(gst::State::Playing)
        .context("failed to start recording pipeline")?;

    std::thread::sleep(duration);

    info!("sending EOS to recording pipeline");
    pipeline.send_event(gst::event::Eos::new());
    wait_for_eos_or_error(pipeline)?;

    verify_output_file(output_path)?;
    Ok(())
}

pub(crate) fn wait_for_eos_or_error(pipeline: &gst::Pipeline) -> anyhow::Result<()> {
    let bus = pipeline
        .bus()
        .ok_or_else(|| anyhow::anyhow!("recording pipeline has no bus"))?;

    loop {
        let message = bus
            .timed_pop(gst::ClockTime::from_seconds(10))
            .ok_or_else(|| anyhow::anyhow!("timed out waiting for recording finalization"))?;

        use gst::MessageView;
        match message.view() {
            MessageView::Eos(_) => return Ok(()),
            MessageView::Error(error) => {
                anyhow::bail!(
                    "GStreamer error from {:?}: {} ({:?})",
                    error.src().map(|src| src.path_string()),
                    error.error(),
                    error.debug()
                );
            }
            MessageView::Warning(warning) => {
                let source = warning.src().map(|src| src.path_string());
                let message = warning.error().to_string();
                let debug_msg = warning.debug().map(|debug| debug.to_string());
                tracing::warn!(
                    source = ?source,
                    error = %message,
                    debug = ?debug_msg,
                    "[EXPORT_PIPELINE] GStreamer warning"
                );
                if gstreamer_warning_is_fatal(&message, debug_msg.as_deref()) {
                    anyhow::bail!(
                        "GStreamer warning from {:?}: {} ({:?})",
                        source,
                        message,
                        debug_msg
                    );
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn gstreamer_warning_is_fatal(message: &str, debug: Option<&str>) -> bool {
    let message = message.to_ascii_lowercase();
    let debug = debug.unwrap_or_default().to_ascii_lowercase();
    let text = format!("{message} {debug}");
    text.contains("splitmuxsink")
        || text.contains("will not work")
        || text.contains("could not add sink")
}

fn verify_output_file(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("recording output was not created: {}", path.display()))?;

    if metadata.len() == 0 {
        anyhow::bail!("recording output is empty: {}", path.display());
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CaptureBackend {
    X11,
    ManualPipeWirePath(String),
    ManualPipeWireTarget(String),
    PortalPipeWire,
}

pub(crate) fn choose_backend(session_type: &str) -> CaptureBackend {
    if let Ok(path) = std::env::var("WT_CLIPPER_PIPEWIRE_PATH") {
        return CaptureBackend::ManualPipeWirePath(path);
    }

    if let Ok(target) = std::env::var("WT_CLIPPER_PIPEWIRE_TARGET") {
        return CaptureBackend::ManualPipeWireTarget(target);
    }

    if session_type.eq_ignore_ascii_case("x11") {
        CaptureBackend::X11
    } else {
        CaptureBackend::PortalPipeWire
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PipelineSource {
    X11 { window_id: Option<u32> },
    PipeWirePath(String),
    PipeWireTarget(String),
    PipeWirePortal { fd: i32, node_id: u32 },
}

pub(crate) fn pipeline_description(
    source: PipelineSource,
    capture_source: CaptureSource,
    output_path: &Path,
    quality: VideoQuality,
    audio_source: Option<&AudioCaptureSource>,
) -> anyhow::Result<String> {
    let location = escape_gst_string(&output_path.to_string_lossy());
    let raw_caps = quality.raw_video_caps();
    let encoder = quality.vp8enc_settings();

    match source {
        PipelineSource::X11 { window_id } => {
            let source_chain = x11_source_chain(capture_source, window_id)?;
            Ok(recording_mux_pipeline(
                &source_chain,
                &raw_caps,
                &encoder,
                &location,
                audio_source,
            ))
        }
        PipelineSource::PipeWirePath(path) => {
            let path = escape_gst_string(&path);
            let source_chain = format!("pipewiresrc path=\"{path}\" do-timestamp=true");
            Ok(recording_mux_pipeline(
                &source_chain,
                &raw_caps,
                &encoder,
                &location,
                audio_source,
            ))
        }
        PipelineSource::PipeWireTarget(target) => {
            let target = escape_gst_string(&target);
            let source_chain = format!("pipewiresrc target-object=\"{target}\" do-timestamp=true");
            Ok(recording_mux_pipeline(
                &source_chain,
                &raw_caps,
                &encoder,
                &location,
                audio_source,
            ))
        }
        PipelineSource::PipeWirePortal { fd, node_id } => {
            let source_chain = format!("pipewiresrc fd={fd} path={node_id} do-timestamp=true");
            Ok(recording_mux_pipeline(
                &source_chain,
                &raw_caps,
                &encoder,
                &location,
                audio_source,
            ))
        }
    }
}

fn recording_mux_pipeline(
    video_source_chain: &str,
    raw_caps: &str,
    encoder: &str,
    location: &str,
    audio_source: Option<&AudioCaptureSource>,
) -> String {
    let video_chain = format!(
        "{video_source_chain} ! queue max-size-buffers=4 leaky=downstream ! videoconvert ! videorate ! {raw_caps} ! queue max-size-buffers=8 leaky=downstream ! {encoder}"
    );
    if let Some(audio_source) = audio_source {
        let audio_chain = audio_source.source_chain();
        format!(
            "webmmux name=mux ! filesink location=\"{location}\" {video_chain} ! queue ! mux. {audio_chain} ! queue ! mux."
        )
    } else {
        format!("{video_chain} ! webmmux ! filesink location=\"{location}\"")
    }
}

pub(crate) fn x11_source_chain(
    capture_source: CaptureSource,
    window_id: Option<u32>,
) -> anyhow::Result<String> {
    match capture_source {
        CaptureSource::Screen => {
            Ok("ximagesrc use-damage=0 do-timestamp=true show-pointer=true".to_owned())
        }
        CaptureSource::Window => {
            let window_id = window_id.ok_or_else(|| {
                anyhow::anyhow!("window capture requested on X11 but no X11 window id was resolved")
            })?;
            Ok(format!(
                "ximagesrc use-damage=0 do-timestamp=true show-pointer=true xid={window_id}"
            ))
        }
    }
}

fn escape_gst_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(crate) fn encode_location(value: &Path) -> String {
    escape_gst_string(&value.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Mutex};

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn chooses_x11_backend_on_x11() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("WT_CLIPPER_PIPEWIRE_PATH");
        std::env::remove_var("WT_CLIPPER_PIPEWIRE_TARGET");

        assert_eq!(choose_backend("x11"), CaptureBackend::X11);
    }

    #[test]
    fn chooses_portal_backend_on_wayland() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("WT_CLIPPER_PIPEWIRE_PATH");
        std::env::remove_var("WT_CLIPPER_PIPEWIRE_TARGET");

        assert_eq!(choose_backend("wayland"), CaptureBackend::PortalPipeWire);
    }

    #[test]
    fn manual_pipewire_path_overrides_session_type() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("WT_CLIPPER_PIPEWIRE_PATH", "42");
        std::env::remove_var("WT_CLIPPER_PIPEWIRE_TARGET");

        assert_eq!(
            choose_backend("x11"),
            CaptureBackend::ManualPipeWirePath("42".to_owned())
        );

        std::env::remove_var("WT_CLIPPER_PIPEWIRE_PATH");
    }

    #[test]
    fn builds_portal_pipewire_pipeline() {
        let pipeline = pipeline_description(
            PipelineSource::PipeWirePortal { fd: 8, node_id: 99 },
            CaptureSource::Screen,
            Path::new("/tmp/out.webm"),
            VideoQuality::default(),
            None,
        )
        .unwrap();

        assert!(pipeline.contains("pipewiresrc fd=8 path=99"));
        assert!(pipeline.contains("queue max-size-buffers=4 leaky=downstream"));
        assert!(pipeline.contains("video/x-raw,framerate=30/1"));
        assert!(pipeline.contains("queue max-size-buffers=8 leaky=downstream"));
        assert!(pipeline.contains(
            "vp8enc deadline=1 end-usage=cbr target-bitrate=10000000 cpu-used=4 keyframe-max-dist=60"
        ));
        assert!(pipeline.contains("webmmux"));
        assert!(pipeline.contains("filesink location=\"/tmp/out.webm\""));
    }

    #[test]
    fn builds_x11_pipeline() {
        let pipeline = pipeline_description(
            PipelineSource::X11 { window_id: None },
            CaptureSource::Screen,
            Path::new("/tmp/out.webm"),
            VideoQuality::default(),
            None,
        )
        .unwrap();

        assert!(pipeline.contains("ximagesrc"));
        assert!(pipeline.contains("vp8enc"));
        assert!(pipeline.contains("target-bitrate=10000000"));
    }

    #[test]
    fn x11_window_request_uses_resolved_window_pipeline() {
        let pipeline = pipeline_description(
            PipelineSource::X11 {
                window_id: Some(0x3a00007),
            },
            CaptureSource::Window,
            Path::new("/tmp/out.webm"),
            VideoQuality::default(),
            None,
        )
        .unwrap();

        assert!(pipeline.contains("ximagesrc"));
        assert!(pipeline.contains("xid=60817415"));
        assert!(pipeline.contains("filesink location=\"/tmp/out.webm\""));
    }

    #[test]
    fn x11_window_request_without_window_id_errors() {
        let error = pipeline_description(
            PipelineSource::X11 { window_id: None },
            CaptureSource::Window,
            Path::new("/tmp/out.webm"),
            VideoQuality::default(),
            None,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("no X11 window id"));
    }

    #[test]
    fn recording_pipeline_can_mux_system_audio() {
        let audio = AudioCaptureSource {
            device: "alsa_output.test.monitor".to_owned(),
        };
        let pipeline = pipeline_description(
            PipelineSource::PipeWirePortal { fd: 8, node_id: 99 },
            CaptureSource::Screen,
            Path::new("/tmp/out.webm"),
            VideoQuality::default(),
            Some(&audio),
        )
        .unwrap();

        assert!(pipeline.contains("webmmux name=mux"));
        assert!(pipeline.contains("pulsesrc device=\"alsa_output.test.monitor\""));
        assert!(pipeline.contains("opusenc bitrate=128000"));
    }

    #[test]
    fn splitmuxsink_warning_is_fatal() {
        assert!(gstreamer_warning_is_fatal(
            "Could not add sink_28 element",
            Some("splitmuxsink will not work")
        ));
        assert!(!gstreamer_warning_is_fatal("latency redistribution", None));
    }
}
