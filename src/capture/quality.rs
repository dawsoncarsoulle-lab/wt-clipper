use clap::ValueEnum;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum QualityPreset {
    Low,
    Medium,
    High,
}

impl QualityPreset {
    pub fn video_quality(self) -> VideoQuality {
        match self {
            Self::Low => VideoQuality {
                fps: 30,
                video_bitrate_kbps: 4_000,
                encoder_cpu_used: 6,
            },
            Self::Medium => VideoQuality {
                fps: 30,
                video_bitrate_kbps: 8_000,
                encoder_cpu_used: 4,
            },
            Self::High => VideoQuality {
                fps: 60,
                video_bitrate_kbps: 12_000,
                encoder_cpu_used: 4,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoQuality {
    pub fps: u32,
    pub video_bitrate_kbps: u32,
    pub encoder_cpu_used: u32,
}

impl Default for VideoQuality {
    fn default() -> Self {
        QualityPreset::High.video_quality()
    }
}

impl VideoQuality {
    pub fn new(fps: u32, video_bitrate_kbps: u32, encoder_cpu_used: u32) -> anyhow::Result<Self> {
        if fps == 0 {
            anyhow::bail!("fps must be greater than zero");
        }
        if video_bitrate_kbps == 0 {
            anyhow::bail!("video bitrate must be greater than zero");
        }
        if encoder_cpu_used > 16 {
            anyhow::bail!("VP8 encoder cpu-used must be between 0 and 16");
        }
        Ok(Self {
            fps,
            video_bitrate_kbps,
            encoder_cpu_used,
        })
    }

    pub fn with_overrides(
        preset: QualityPreset,
        fps: Option<u32>,
        video_bitrate_kbps: Option<u32>,
    ) -> anyhow::Result<Self> {
        let base = preset.video_quality();
        Self::new(
            fps.unwrap_or(base.fps),
            video_bitrate_kbps.unwrap_or(base.video_bitrate_kbps),
            base.encoder_cpu_used,
        )
    }

    pub fn bitrate_bps(self) -> u32 {
        self.video_bitrate_kbps.saturating_mul(1000)
    }

    pub fn keyframe_max_dist(self) -> u32 {
        self.fps.saturating_mul(2).max(1)
    }

    pub fn raw_video_caps(self) -> String {
        format!("video/x-raw,framerate={}/1", self.fps)
    }

    pub fn vp8enc_settings(self) -> String {
        format!(
            "vp8enc deadline=1 cpu-used={} target-bitrate={} keyframe-max-dist={}",
            self.encoder_cpu_used,
            self.bitrate_bps(),
            self.keyframe_max_dist()
        )
    }

    pub fn log_summary(self) -> String {
        format!(
            "{} fps, {} kbps, VP8/WebM",
            self.fps, self.video_bitrate_kbps
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_have_expected_values() {
        assert_eq!(QualityPreset::Low.video_quality().video_bitrate_kbps, 4_000);
        assert_eq!(
            QualityPreset::Medium.video_quality().video_bitrate_kbps,
            8_000
        );
        assert_eq!(QualityPreset::High.video_quality().fps, 60);
        assert_eq!(
            QualityPreset::High.video_quality().video_bitrate_kbps,
            12_000
        );
    }

    #[test]
    fn converts_kbps_to_bits_per_second() {
        let quality = VideoQuality::new(60, 12_000, 4).unwrap();
        assert_eq!(quality.bitrate_bps(), 12_000_000);
    }

    #[test]
    fn keyframe_interval_is_two_seconds() {
        assert_eq!(
            VideoQuality::new(30, 8_000, 4).unwrap().keyframe_max_dist(),
            60
        );
        assert_eq!(
            VideoQuality::new(60, 12_000, 4)
                .unwrap()
                .keyframe_max_dist(),
            120
        );
    }

    #[test]
    fn cli_overrides_preset_values() {
        let quality =
            VideoQuality::with_overrides(QualityPreset::Low, Some(60), Some(16_000)).unwrap();
        assert_eq!(quality.fps, 60);
        assert_eq!(quality.video_bitrate_kbps, 16_000);
        assert_eq!(quality.encoder_cpu_used, 6);
    }
}
