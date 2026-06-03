use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::{capture::quality::QualityPreset, cli::CaptureSource};

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct AppConfig {
    #[serde(default)]
    pub clip: ClipConfig,
    #[serde(default, alias = "warthunder")]
    pub war_thunder: WarThunderConfig,
    #[serde(default)]
    pub triggers: TriggerConfig,
    #[serde(default)]
    pub storage: StorageConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ClipConfig {
    #[serde(default = "default_clip_seconds")]
    pub seconds: u64,
    #[serde(default = "default_segment_seconds")]
    pub segment_seconds: u64,
    #[serde(default = "default_post_event_seconds")]
    pub post_event_seconds: u64,
    #[serde(default = "default_multi_kill_window_seconds")]
    pub multi_kill_window_seconds: u64,
    #[serde(default = "default_output_dir_string")]
    pub output_dir: String,
    #[serde(default)]
    pub quality: QualityPreset,
    #[serde(default = "default_fps")]
    pub fps: u32,
    #[serde(default = "default_video_bitrate_kbps")]
    pub video_bitrate_kbps: u32,
    #[serde(default)]
    pub source: CaptureSource,
    #[serde(default)]
    pub keep_segments: bool,
    #[serde(default)]
    pub export_mode: ClipExportMode,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClipExportMode {
    Instant,
    #[default]
    Deferred,
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
        let quality = QualityPreset::High;
        let video_quality = quality.video_quality();
        Self {
            seconds: default_clip_seconds(),
            segment_seconds: default_segment_seconds(),
            post_event_seconds: default_post_event_seconds(),
            multi_kill_window_seconds: default_multi_kill_window_seconds(),
            output_dir: default_output_dir_string(),
            quality,
            fps: video_quality.fps,
            video_bitrate_kbps: video_quality.video_bitrate_kbps,
            source: CaptureSource::Window,
            keep_segments: false,
            export_mode: ClipExportMode::Deferred,
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
        let mut config: Self = toml::from_str(&content)?;
        config.clip.output_dir = expand_tilde_to_string(&config.clip.output_dir)?;
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

impl ClipConfig {
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
seconds = 20
segment_seconds = 2
post_event_seconds = 5
multi_kill_window_seconds = 8
output_dir = "~/Videos/WarThunder Clips"
quality = "high"
fps = 60
video_bitrate_kbps = 20000
source = "window"
keep_segments = false
export_mode = "deferred"

[war_thunder]
player_name = ""
poll_interval_ms = 300

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

fn default_clip_seconds() -> u64 {
    20
}

fn default_segment_seconds() -> u64 {
    2
}

fn default_post_event_seconds() -> u64 {
    5
}

fn default_multi_kill_window_seconds() -> u64 {
    8
}

fn default_output_dir_string() -> String {
    "~/Videos/WarThunder Clips".to_owned()
}

fn default_fps() -> u32 {
    60
}

fn default_video_bitrate_kbps() -> u32 {
    20_000
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

fn default_true() -> bool {
    true
}

fn default_max_clips() -> u64 {
    100
}

fn default_max_storage_gb() -> u64 {
    20
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_uses_defaults() {
        let path =
            std::env::temp_dir().join(format!("wt-clipper-missing-config-{}", std::process::id()));

        let config = AppConfig::load(Some(&path)).unwrap();

        assert_eq!(config.clip.seconds, 20);
        assert_eq!(config.clip.quality, QualityPreset::High);
        assert_eq!(config.clip.source, CaptureSource::Window);
    }

    #[test]
    fn parses_config_toml() {
        let config: AppConfig = toml::from_str(default_config_toml()).unwrap();

        assert_eq!(config.clip.seconds, 20);
        assert_eq!(config.clip.video_bitrate_kbps, 20_000);
        assert_eq!(config.clip.multi_kill_window_seconds, 8);
        assert_eq!(config.war_thunder.poll_interval_ms, 300);
        assert!(config.triggers.target_destroyed);
        assert!(config.triggers.base_destroyed);
        assert!(!config.triggers.player_destroyed);
        assert_eq!(config.storage.max_clips, 100);
    }

    #[test]
    fn ignores_removed_trigger_fields_from_old_configs() {
        let config: AppConfig = toml::from_str(
            r#"
[triggers]
target_destroyed = true
player_destroyed = true
critical_hit = true
severe_damage = true
set_afire = true
crash = true
"#,
        )
        .unwrap();

        assert!(config.triggers.target_destroyed);
        assert!(config.triggers.player_destroyed);
        assert!(config.triggers.base_destroyed);
    }

    #[test]
    fn expands_tilde_paths() {
        let home = std::env::var("HOME").unwrap();

        assert_eq!(
            expand_tilde("~/Videos/WarThunder Clips").unwrap(),
            PathBuf::from(home).join("Videos/WarThunder Clips")
        );
    }

    #[test]
    fn config_init_does_not_overwrite_without_force() {
        let dir =
            std::env::temp_dir().join(format!("wt-clipper-config-init-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        fs::write(&path, "existing").unwrap();

        assert!(AppConfig::write_default(&path, false).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "existing");

        fs::remove_dir_all(dir).unwrap();
    }
}
