use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct AppConfig {
    #[serde(default)]
    pub clip: ClipConfig,
    #[serde(default)]
    pub library: LibraryConfig,
    #[serde(default)]
    pub capture: CaptureConfig,
    #[serde(default, alias = "warthunder")]
    pub war_thunder: WarThunderConfig,
    #[serde(default)]
    pub triggers: TriggerConfig,
    #[serde(default)]
    pub storage: StorageConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ClipConfig {
    #[serde(default = "default_post_event_seconds")]
    pub post_event_seconds: u64,
    #[serde(default = "default_multi_kill_window_seconds")]
    pub multi_kill_window_seconds: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct LibraryConfig {
    #[serde(default = "default_library_output_dir_string")]
    pub output_dir: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct CaptureConfig {
    #[serde(default = "default_capture_strategy")]
    pub capture_strategy: CaptureStrategy,
    #[serde(default = "default_capture_target")]
    pub target: String,
    #[serde(default, alias = "gpu_screen_recorder_mode", rename = "mode")]
    pub gpu_screen_recorder_mode: GpuScreenRecorderMode,
    #[serde(default = "default_gsr_fps")]
    pub fps: u32,
    #[serde(default = "default_capture_replay_seconds")]
    pub replay_seconds: u64,
    #[serde(default)]
    pub container: GsrContainer,
    #[serde(default)]
    pub codec: GsrCodec,
    #[serde(default)]
    pub encoder: GsrEncoder,
    #[serde(default)]
    pub quality: GsrQuality,
    #[serde(default)]
    pub bitrate_mode: GsrBitrateMode,
    #[serde(default)]
    pub frame_rate_mode: GsrFrameRateMode,
    #[serde(default = "default_keyframe_interval_seconds")]
    pub keyframe_interval_seconds: f32,
    #[serde(default)]
    pub restart_replay_on_save: bool,
    #[serde(default = "default_gsr_video_bitrate_kbps")]
    pub video_bitrate_kbps: u32,
    #[serde(default = "default_gsr_output_dir_string")]
    pub output_dir: String,
    #[serde(default = "default_capture_audio_enabled")]
    pub audio_enabled: bool,
    #[serde(default = "default_capture_audio_input")]
    pub audio_input: String,
}


#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStrategy {
    /// Pick the best capture target for the current session.
    /// X11: try the War Thunder window after the localhost API is reachable, then fallback to monitor.
    /// Wayland: use the system portal when available after the localhost API is reachable, then fallback to monitor.
    #[default]
    Auto,
    /// Always capture the configured monitor/target.
    Monitor,
    /// Capture the currently focused window. Advanced fallback mode.
    Focused,
    /// Use desktop portal. Mainly useful on Wayland; unsupported by GSR on X11.
    Portal,
}

impl CaptureStrategy {
    pub fn should_wait_for_war_thunder(self) -> bool {
        matches!(self, Self::Auto | Self::Portal)
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GpuScreenRecorderMode {
    Auto,
    Native,
    #[default]
    Flatpak,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GsrContainer {
    #[default]
    Mp4,
    Mkv,
}

impl GsrContainer {
    pub fn as_arg(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mkv => "mkv",
        }
    }

    pub fn extension(self) -> &'static str {
        self.as_arg()
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GsrCodec {
    #[default]
    H264,
    Hevc,
    Av1,
}

impl GsrCodec {
    pub fn as_arg(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Av1 => "av1",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GsrEncoder {
    #[default]
    Gpu,
    Cpu,
}

impl GsrEncoder {
    pub fn as_arg(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::Cpu => "cpu",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GsrQuality {
    Medium,
    High,
    #[default]
    VeryHigh,
    Ultra,
}

impl GsrQuality {
    pub fn as_arg(self) -> &'static str {
        match self {
            Self::Medium => "medium",
            Self::High => "high",
            Self::VeryHigh => "very_high",
            Self::Ultra => "ultra",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GsrBitrateMode {
    Auto,
    Qp,
    #[default]
    Cbr,
    Vbr,
}

impl GsrBitrateMode {
    pub fn as_arg(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::Qp => Some("qp"),
            Self::Cbr => Some("cbr"),
            Self::Vbr => Some("vbr"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GsrFrameRateMode {
    #[default]
    Cfr,
    Vfr,
    Content,
}

impl GsrFrameRateMode {
    pub fn as_arg(self) -> &'static str {
        match self {
            Self::Cfr => "cfr",
            Self::Vfr => "vfr",
            Self::Content => "content",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct WarThunderConfig {
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub player_name: Option<String>,
    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms: u64,
    #[serde(default = "default_request_timeout_ms")]
    pub request_timeout_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TriggerConfig {
    #[serde(default = "default_true")]
    pub target_destroyed: bool,
    #[serde(default = "default_true")]
    pub base_destroyed: bool,
    #[serde(default)]
    pub player_destroyed: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct StorageConfig {
    #[serde(default = "default_max_clips")]
    pub max_clips: u64,
    #[serde(default = "default_max_storage_gb")]
    pub max_storage_gb: u64,
}

impl Default for ClipConfig {
    fn default() -> Self {
        Self {
            post_event_seconds: default_post_event_seconds(),
            multi_kill_window_seconds: default_multi_kill_window_seconds(),
        }
    }
}

impl Default for LibraryConfig {
    fn default() -> Self {
        Self {
            output_dir: default_library_output_dir_string(),
        }
    }
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            capture_strategy: default_capture_strategy(),
            target: default_capture_target(),
            gpu_screen_recorder_mode: GpuScreenRecorderMode::Flatpak,
            fps: default_gsr_fps(),
            replay_seconds: default_capture_replay_seconds(),
            container: GsrContainer::Mp4,
            codec: GsrCodec::H264,
            encoder: GsrEncoder::Gpu,
            quality: GsrQuality::VeryHigh,
            bitrate_mode: GsrBitrateMode::Cbr,
            frame_rate_mode: GsrFrameRateMode::Cfr,
            keyframe_interval_seconds: default_keyframe_interval_seconds(),
            restart_replay_on_save: false,
            video_bitrate_kbps: default_gsr_video_bitrate_kbps(),
            output_dir: default_gsr_output_dir_string(),
            audio_enabled: default_capture_audio_enabled(),
            audio_input: default_capture_audio_input(),
        }
    }
}

impl Default for WarThunderConfig {
    fn default() -> Self {
        Self {
            base_url: default_base_url(),
            player_name: None,
            poll_interval_ms: default_poll_interval_ms(),
            request_timeout_ms: default_request_timeout_ms(),
        }
    }
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            target_destroyed: true,
            base_destroyed: true,
            player_destroyed: false,
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            max_clips: default_max_clips(),
            max_storage_gb: default_max_storage_gb(),
        }
    }
}

impl AppConfig {
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let path = path
            .map(Path::to_path_buf)
            .unwrap_or_else(default_config_path);
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(&path)?;
        let mut value: toml::Value = toml::from_str(&content)?;
        Self::migrate_legacy_fields(&mut value);
        let mut config: Self = value.try_into()?;
        config.library.output_dir = expand_tilde_to_string(&config.library.output_dir)?;
        config.capture.output_dir = expand_tilde_to_string(&config.capture.output_dir)?;
        if config.capture.bitrate_mode == GsrBitrateMode::Cbr
            && config.capture.video_bitrate_kbps == 0
        {
            config.capture.video_bitrate_kbps = default_gsr_video_bitrate_kbps();
        }
        if config
            .war_thunder
            .player_name
            .as_deref()
            .is_some_and(|name| name.trim().is_empty())
        {
            config.war_thunder.player_name = None;
        }
        Ok(config)
    }

    fn migrate_legacy_fields(value: &mut toml::Value) {
        let Some(root) = value.as_table_mut() else {
            return;
        };

        let legacy_clip_output_dir = root
            .get("clip")
            .and_then(toml::Value::as_table)
            .and_then(|clip| clip.get("output_dir"))
            .cloned();
        let library = root
            .entry("library".to_owned())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        if let (Some(output_dir), Some(library)) = (legacy_clip_output_dir, library.as_table_mut())
        {
            library.entry("output_dir".to_owned()).or_insert(output_dir);
        }

        root.remove("pending_exports");
        if let Some(capture) = root.get_mut("capture").and_then(toml::Value::as_table_mut) {
            capture.remove("backend");
        }
        if let Some(clip) = root.get_mut("clip").and_then(toml::Value::as_table_mut) {
            for key in [
                "seconds",
                "segment_seconds",
                "output_dir",
                "quality",
                "fps",
                "video_bitrate_kbps",
                "source",
                "keep_segments",
                "export_mode",
            ] {
                clip.remove(key);
            }
        }
    }

    pub fn write_default(path: &Path, force: bool) -> anyhow::Result<()> {
        if path.exists() && !force {
            anyhow::bail!(
                "config already exists at {}; pass --force to overwrite",
                path.display()
            );
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, default_config_toml())
            .map_err(|error| anyhow::anyhow!("failed to write {}: {error}", path.display()))
    }
}

impl LibraryConfig {
    pub fn output_dir_path(&self) -> anyhow::Result<PathBuf> {
        expand_tilde(&self.output_dir)
    }
}

impl CaptureConfig {
    pub fn output_dir_path(&self) -> anyhow::Result<PathBuf> {
        expand_tilde(&self.output_dir)
    }
}

impl WarThunderConfig {
    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.poll_interval_ms.clamp(250, 500))
    }

    pub fn request_timeout(&self) -> Duration {
        Duration::from_millis(self.request_timeout_ms)
    }
}

pub fn default_config_path() -> PathBuf {
    config_home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("wt-clipper")
        .join("config.toml")
}

pub fn expand_tilde(path: &str) -> anyhow::Result<PathBuf> {
    if path == "~" {
        return home_dir().ok_or_else(|| anyhow::anyhow!("HOME is not set"));
    }

    if let Some(rest) = path.strip_prefix("~/") {
        return home_dir()
            .map(|home| home.join(rest))
            .ok_or_else(|| anyhow::anyhow!("HOME is not set"));
    }

    Ok(PathBuf::from(path))
}

fn expand_tilde_to_string(path: &str) -> anyhow::Result<String> {
    Ok(expand_tilde(path)?.display().to_string())
}

pub fn default_config_toml() -> &'static str {
    r#"[clip]
post_event_seconds = 5
multi_kill_window_seconds = 8

[library]
output_dir = "~/Videos/WarThunder Clips"

[capture]
capture_strategy = "auto"
target = ""
mode = "flatpak"
fps = 60
replay_seconds = 25
container = "mp4"
codec = "h264"
encoder = "gpu"
quality = "very_high"
bitrate_mode = "cbr"
frame_rate_mode = "cfr"
keyframe_interval_seconds = 1.0
restart_replay_on_save = false
video_bitrate_kbps = 20000
output_dir = "~/Videos/WarThunder Clips/GSR"
audio_enabled = true
audio_input = "default_output"

[war_thunder]
base_url = "http://127.0.0.1:8111"
player_name = ""
poll_interval_ms = 300
request_timeout_ms = 500

[triggers]
target_destroyed = true
base_destroyed = true
player_destroyed = false

[storage]
max_clips = 100
max_storage_gb = 20
"#
}

fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".config")))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn default_post_event_seconds() -> u64 {
    5
}

fn default_multi_kill_window_seconds() -> u64 {
    8
}

fn default_library_output_dir_string() -> String {
    "~/Videos/WarThunder Clips".to_owned()
}

fn default_gsr_output_dir_string() -> String {
    "~/Videos/WarThunder Clips/GSR".to_owned()
}

fn default_capture_audio_enabled() -> bool {
    true
}

fn default_capture_audio_input() -> String {
    "default_output".to_owned()
}

fn default_capture_strategy() -> CaptureStrategy {
    CaptureStrategy::Auto
}

fn default_capture_target() -> String {
    String::new()
}

fn default_capture_replay_seconds() -> u64 {
    25
}

fn default_gsr_fps() -> u32 {
    60
}

fn default_gsr_video_bitrate_kbps() -> u32 {
    20_000
}

fn default_keyframe_interval_seconds() -> f32 {
    1.0
}

fn default_base_url() -> String {
    "http://127.0.0.1:8111".to_owned()
}

fn default_poll_interval_ms() -> u64 {
    300
}

fn default_request_timeout_ms() -> u64 {
    500
}

fn default_max_clips() -> u64 {
    100
}

fn default_max_storage_gb() -> u64 {
    20
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_clip_output_dir_migrates_to_library_output_dir() {
        let mut value: toml::Value = toml::from_str(
            r#"
[clip]
output_dir = "/tmp/library"
post_event_seconds = 7

[capture]
backend = "gpu_screen_recorder"
"#,
        )
        .unwrap();

        AppConfig::migrate_legacy_fields(&mut value);
        let config: AppConfig = value.try_into().unwrap();

        assert_eq!(config.library.output_dir, "/tmp/library");
        assert_eq!(config.clip.post_event_seconds, 7);
    }

    #[test]
    fn old_pending_exports_loads_successfully() {
        let mut value: toml::Value = toml::from_str(
            r#"
[pending_exports]
pending_export_dir = "/tmp/pending"
"#,
        )
        .unwrap();

        AppConfig::migrate_legacy_fields(&mut value);
        let config: AppConfig = value.try_into().unwrap();

        assert_eq!(config.capture.replay_seconds, 25);
    }

    #[test]
    fn clean_config_serializes_without_obsolete_fields() {
        let content = toml::to_string(&AppConfig::default()).unwrap();

        for obsolete in [
            "pending_exports",
            "segment_seconds",
            "source",
            "keep_segments",
            "export_mode",
            "backend",
        ] {
            assert!(
                !content.contains(obsolete),
                "serialized config still contains {obsolete}: {content}"
            );
        }
    }
}
