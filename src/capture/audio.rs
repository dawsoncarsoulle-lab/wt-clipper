use std::process::Command;

use tracing::{debug, warn};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioCaptureSource {
    pub device: String,
}

impl AudioCaptureSource {
    pub fn source_chain(&self) -> String {
        let device = escape_gst_string(&self.device);
        format!(
            "pulsesrc device=\"{device}\" do-timestamp=true ! audioconvert ! audioresample ! audio/x-raw,channels=2 ! opusenc bitrate=128000"
        )
    }
}

pub fn resolve_system_audio_source() -> Option<AudioCaptureSource> {
    if env_flag_enabled("WT_CLIPPER_DISABLE_AUDIO") {
        debug!("system audio capture disabled by WT_CLIPPER_DISABLE_AUDIO");
        return None;
    }

    if let Ok(device) = std::env::var("WT_CLIPPER_AUDIO_DEVICE") {
        let device = device.trim();
        if !device.is_empty() {
            debug!(device, "using audio device from WT_CLIPPER_AUDIO_DEVICE");
            return Some(AudioCaptureSource {
                device: device.to_owned(),
            });
        }
    }

    match default_monitor_source() {
        Some(device) => {
            debug!(
                device,
                "using default Pulse/PipeWire monitor for audio capture"
            );
            Some(AudioCaptureSource { device })
        }
        None => {
            warn!(
                "could not resolve default Pulse/PipeWire monitor; continuing without audio capture"
            );
            None
        }
    }
}

fn default_monitor_source() -> Option<String> {
    let output = Command::new("pactl")
        .args(["get-default-sink"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let sink = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if sink.is_empty() {
        None
    } else {
        Some(format!("{sink}.monitor"))
    }
}

fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn escape_gst_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_chain_uses_pulse_monitor_and_opus() {
        let source = AudioCaptureSource {
            device: "alsa_output.test.monitor".to_owned(),
        };
        let chain = source.source_chain();

        assert!(chain.contains("pulsesrc device=\"alsa_output.test.monitor\""));
        assert!(chain.contains("opusenc bitrate=128000"));
    }

    #[test]
    fn audio_chain_escapes_device_name() {
        let source = AudioCaptureSource {
            device: "sink\"name.monitor".to_owned(),
        };

        assert!(source.source_chain().contains("sink\\\"name.monitor"));
    }
}
