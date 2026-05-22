use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use gst::prelude::*;
use gstreamer as gst;
use tracing::debug;

use crate::capture::{
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

    gst::init().context("failed to initialize GStreamer")?;
    debug!(
        target_bitrate = quality.bitrate_bps(),
        "concat target bitrate: {} bps",
        quality.bitrate_bps()
    );
    let pipeline_description = concat_pipeline_description(&segments, &output_path, quality);
    debug!(
        pipeline = %pipeline_description,
        "final concat pipeline"
    );
    let element =
        gst::parse::launch(&pipeline_description).context("failed to build concat pipeline")?;
    let pipeline = element
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow::anyhow!("GStreamer concat description did not create a pipeline"))?;

    let result = run_concat_pipeline(&pipeline, &output_path);
    if let Err(error) = pipeline.set_state(gst::State::Null) {
        if result.is_ok() {
            return Err(error).context("failed to stop concat pipeline");
        }
    }
    result?;
    Ok(output_path)
}

pub(crate) fn valid_segments(segments: &[PathBuf]) -> anyhow::Result<Vec<PathBuf>> {
    let mut segments = segments.to_vec();
    segments.sort();

    let mut valid = Vec::new();
    for segment in segments {
        let metadata = fs::metadata(&segment)
            .with_context(|| format!("replay segment does not exist: {}", segment.display()))?;
        if !metadata.is_file() {
            continue;
        }
        if metadata.len() == 0 {
            continue;
        }
        valid.push(segment);
    }

    Ok(valid)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("wt-clipper-concat-{name}-{}", std::process::id()))
    }

    #[test]
    fn valid_segments_are_sorted_and_empty_files_filtered() {
        let dir = test_dir("valid");
        fs::create_dir_all(&dir).unwrap();
        let a = dir.join("segment-000001.webm");
        let b = dir.join("segment-000002.webm");
        let empty = dir.join("segment-000003.webm");
        fs::write(&b, b"b").unwrap();
        fs::write(&a, b"a").unwrap();
        fs::write(&empty, b"").unwrap();

        let valid = valid_segments(&[b.clone(), empty, a.clone()]).unwrap();

        assert_eq!(valid, vec![a, b]);
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
        assert!(pipeline.contains("video/x-raw,framerate=60/1"));
        assert!(pipeline.contains(
            "vp8enc deadline=1 end-usage=cbr target-bitrate=20000000 cpu-used=2 keyframe-max-dist=120"
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
    fn empty_segment_list_errors() {
        let error = valid_segments(&[]).unwrap();
        assert!(error.is_empty());
    }
}
