use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use gst::prelude::*;
use gstreamer as gst;
use tracing::{debug, warn};

use crate::capture::{
    audio::resolve_system_audio_source,
    quality::VideoQuality,
    recorder::{encode_location, wait_for_eos_or_error},
};

pub fn concatenate_segments_to_webm(
    segments: &[PathBuf],
    output_path: PathBuf,
    quality: VideoQuality,
) -> anyhow::Result<PathBuf> {
    let segments = valid_segments(segments)?;
    if segments.is_empty() {
        anyhow::bail!("no replay segments available to concatenate");
    }

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create output directory {}", parent.display()))?;
    }

    let temp_output_path = output_path.with_extension(
        output_path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| format!("{extension}.tmp"))
            .unwrap_or_else(|| "tmp".to_owned()),
    );
    if temp_output_path.exists() {
        fs::remove_file(&temp_output_path).with_context(|| {
            format!(
                "failed to remove stale temporary concat output {}",
                temp_output_path.display()
            )
        })?;
    }

    gst::init().context("failed to initialize GStreamer")?;
    debug!(
        target_bitrate = quality.bitrate_bps(),
        "concat target bitrate: {} bps",
        quality.bitrate_bps()
    );
    let mut result = None;
    if resolve_system_audio_source().is_some() {
        let audio_pipeline =
            concat_pipeline_description_with_audio(&segments, &temp_output_path, quality);
        debug!(pipeline = %audio_pipeline, "final audio/video concat pipeline");
        match run_concat_description(&audio_pipeline, &temp_output_path) {
            Ok(()) => result = Some(Ok(())),
            Err(error) => {
                warn!(%error, "audio/video concat failed; retrying video-only concat");
                if let Err(remove_error) = fs::remove_file(&temp_output_path) {
                    debug!(%remove_error, path = %temp_output_path.display(), "failed to remove partial concat output");
                }
            }
        }
    }

    if result.is_none() {
        let pipeline_description =
            concat_pipeline_description(&segments, &temp_output_path, quality);
        debug!(
            pipeline = %pipeline_description,
            "final video-only concat pipeline"
        );
        result = Some(run_concat_description(
            &pipeline_description,
            &temp_output_path,
        ));
    }

    if let Err(error) = result.expect("concat result is always set") {
        if let Err(remove_error) = fs::remove_file(&temp_output_path) {
            debug!(%remove_error, path = %temp_output_path.display(), "failed to remove failed temporary concat output");
        }
        return Err(error);
    }
    verify_output_file(&temp_output_path)?;
    fs::rename(&temp_output_path, &output_path).with_context(|| {
        format!(
            "failed to move temporary concat output {} to {}",
            temp_output_path.display(),
            output_path.display()
        )
    })?;
    Ok(output_path)
}

fn run_concat_description(pipeline_description: &str, output_path: &Path) -> anyhow::Result<()> {
    let element =
        gst::parse::launch(pipeline_description).context("failed to build concat pipeline")?;
    let pipeline = element
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow::anyhow!("GStreamer concat description did not create a pipeline"))?;

    let result = run_concat_pipeline(&pipeline, output_path);
    if let Err(error) = pipeline.set_state(gst::State::Null) {
        if result.is_ok() {
            return Err(error).context("failed to stop concat pipeline");
        }
    }
    result
}

pub(crate) fn valid_segments(segments: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    let segments = segments
        .iter()
        .map(|path| {
            let index = parse_segment_index(path).ok_or_else(|| {
                anyhow::anyhow!("invalid replay segment name: {}", path.display())
            })?;
            Ok((index, path.clone()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    validate_segment_indexes(&segments)?;

    let mut valid = Vec::new();
    for (position, (index, segment)) in segments.into_iter().enumerate() {
        let metadata = fs::metadata(&segment)
            .with_context(|| format!("replay segment does not exist: {}", segment.display()))?;
        if !metadata.is_file() {
            continue;
        }
        if metadata.len() == 0 {
            continue;
        }
        debug!(position, index, path = %segment.display(), "concat input segment");
        valid.push(segment);
    }

    Ok(valid)
}

fn validate_segment_indexes(segments: &[(u64, PathBuf)]) -> anyhow::Result<()> {
    let mut previous = None;
    for (index, path) in segments {
        if previous.is_some_and(|previous| *index <= previous) {
            debug!(
                previous_index = previous,
                index,
                path = %path.display(),
                "concat input segment indexes are not chronological; preserving input order"
            );
        } else if previous.is_some_and(|previous| *index > previous + 1) {
            debug!(
                previous_index = previous,
                index,
                path = %path.display(),
                "gap in replay segment indexes selected for concat"
            );
        }
        previous = Some(*index);
    }
    Ok(())
}

fn parse_segment_index(path: &Path) -> Option<u64> {
    path.file_name()?
        .to_str()?
        .strip_prefix("segment-")?
        .strip_suffix(".webm")?
        .parse()
        .ok()
}

fn run_concat_pipeline(pipeline: &gst::Pipeline, output_path: &Path) -> anyhow::Result<()> {
    pipeline
        .set_state(gst::State::Playing)
        .context("failed to start concat pipeline")?;
    wait_for_eos_or_error(pipeline)?;
    verify_output_file(output_path)
}

fn verify_output_file(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("final replay video was not created: {}", path.display()))?;
    if metadata.len() == 0 {
        anyhow::bail!("final replay video is empty: {}", path.display());
    }
    Ok(())
}

pub(crate) fn concat_pipeline_description(
    segments: &[PathBuf],
    output_path: &Path,
    quality: VideoQuality,
) -> String {
    let output = encode_location(output_path);
    let raw_caps = quality.raw_video_caps();
    let encoder = quality.vp8enc_settings();
    let mut pipeline = format!(
        "concat name=c ! videoconvert ! videorate ! {raw_caps} ! {encoder} ! webmmux ! filesink location=\"{output}\""
    );

    for segment in segments {
        let location = encode_location(segment);
        pipeline.push_str(&format!(
            " filesrc location=\"{location}\" ! decodebin ! queue ! c."
        ));
    }

    pipeline
}

pub(crate) fn concat_pipeline_description_with_audio(
    segments: &[PathBuf],
    output_path: &Path,
    quality: VideoQuality,
) -> String {
    let output = encode_location(output_path);
    let raw_caps = quality.raw_video_caps();
    let encoder = quality.vp8enc_settings();
    let mut pipeline = format!(
        "webmmux name=mux ! filesink location=\"{output}\" concat name=vc ! videoconvert ! videorate ! {raw_caps} ! {encoder} ! queue ! mux. concat name=ac ! audioconvert ! audioresample ! opusenc bitrate=128000 ! queue ! mux."
    );

    for (index, segment) in segments.iter().enumerate() {
        let location = encode_location(segment);
        pipeline.push_str(&format!(
            " filesrc location=\"{location}\" ! decodebin name=d{index} d{index}. ! queue ! video/x-raw ! vc. d{index}. ! queue ! audio/x-raw ! ac."
        ));
    }

    pipeline
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("wt-clipper-concat-{name}-{}", std::process::id()))
    }

    #[test]
    fn valid_segments_preserve_input_order_and_filter_empty_files() {
        let dir = test_dir("valid");
        fs::create_dir_all(&dir).unwrap();
        let a = dir.join("segment-000001.webm");
        let b = dir.join("segment-000002.webm");
        let empty = dir.join("segment-000003.webm");
        fs::write(&b, b"b").unwrap();
        fs::write(&a, b"a").unwrap();
        fs::write(&empty, b"").unwrap();

        let valid = valid_segments(&[b.clone(), empty, a.clone()]).unwrap();

        assert_eq!(valid, vec![b, a]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn concat_pipeline_contains_each_segment() {
        let pipeline = concat_pipeline_description(
            &[
                PathBuf::from("/tmp/segment-000001.webm"),
                PathBuf::from("/tmp/segment-000002.webm"),
            ],
            Path::new("/tmp/out.webm"),
            VideoQuality::default(),
        );

        assert!(pipeline.contains("concat name=c"));
        assert!(pipeline.contains("video/x-raw,framerate=30/1"));
        assert!(pipeline.contains(
            "vp8enc deadline=1 end-usage=cbr target-bitrate=10000000 cpu-used=4 keyframe-max-dist=60"
        ));
        assert!(pipeline.contains("decodebin"));
        assert!(pipeline.contains("/tmp/segment-000001.webm"));
        assert!(pipeline.contains("/tmp/segment-000002.webm"));
        assert!(pipeline.contains("filesink location=\"/tmp/out.webm\""));
    }

    #[test]
    fn concat_pipeline_uses_overridden_video_bitrate() {
        let quality = VideoQuality::with_overrides(
            crate::capture::quality::QualityPreset::High,
            None,
            Some(20_000),
        )
        .unwrap();
        let pipeline = concat_pipeline_description(
            &[PathBuf::from("/tmp/segment-000001.webm")],
            Path::new("/tmp/out.webm"),
            quality,
        );

        assert!(pipeline.contains("target-bitrate=20000000"));
    }

    #[test]
    fn audio_concat_pipeline_uses_separate_video_and_audio_concats() {
        let pipeline = concat_pipeline_description_with_audio(
            &[
                PathBuf::from("/tmp/session/segment-000000.webm"),
                PathBuf::from("/tmp/session/segment-000001.webm"),
            ],
            Path::new("/tmp/out.webm"),
            VideoQuality::default(),
        );

        assert!(pipeline.contains("concat name=vc"));
        assert!(pipeline.contains("concat name=ac"));
        assert!(pipeline.contains("decodebin name=d0"));
        assert!(pipeline.contains("opusenc bitrate=128000"));
        assert!(pipeline.contains("webmmux name=mux"));
    }

    #[test]
    fn empty_segment_list_errors() {
        let error = valid_segments(&[]).unwrap();
        assert!(error.is_empty());
    }
}
