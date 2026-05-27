use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

use crate::capture::quality::QualityPreset;

#[derive(Debug, Parser)]
#[command(name = "wt-clipper")]
#[command(about = "War Thunder clipper telemetry prototype")]
pub struct Cli {
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the native egui desktop interface.
    Gui,
    /// Manage wt-clipper configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Diagnose local capture dependencies without starting a capture.
    Doctor {
        /// Print checks as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Check whether the local War Thunder HTTP API is reachable.
    Status,
    /// Print raw endpoint responses for debugging parser assumptions.
    Dump {
        #[command(subcommand)]
        endpoint: DumpEndpoint,
    },
    /// Poll useful endpoints and print newly detected messages.
    Watch {
        /// Process messages already present at startup instead of only new events.
        #[arg(long)]
        include_history: bool,
    },
    /// Record the local screen to a WebM file.
    Record {
        /// Recording duration in seconds.
        #[arg(long)]
        duration: Option<u64>,
        /// Output .webm path. Defaults to ~/Videos/WarThunder Clips/manual-*.webm.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Capture source.
        #[arg(long, value_enum)]
        source: Option<CaptureSource>,
        /// Video quality preset.
        #[arg(long, value_enum)]
        quality: Option<QualityPreset>,
        /// Target frames per second. Overrides --quality.
        #[arg(long)]
        fps: Option<u32>,
        /// Target video bitrate in kbps. Overrides --quality.
        #[arg(long)]
        video_bitrate: Option<u32>,
    },
    /// Keep a rolling replay buffer and save recent segments when Enter is pressed.
    Buffer {
        /// Replay buffer duration in seconds.
        #[arg(long)]
        seconds: Option<u64>,
        /// Segment duration in seconds.
        #[arg(long)]
        segment_seconds: Option<u64>,
        /// Directory where replay clip folders are written.
        #[arg(long)]
        output_dir: Option<PathBuf>,
        /// Capture source.
        #[arg(long, value_enum)]
        source: Option<CaptureSource>,
        /// Video quality preset.
        #[arg(long, value_enum)]
        quality: Option<QualityPreset>,
        /// Target frames per second. Overrides --quality.
        #[arg(long)]
        fps: Option<u32>,
        /// Target video bitrate in kbps. Overrides --quality.
        #[arg(long)]
        video_bitrate: Option<u32>,
        /// Keep copied replay segments after the final WebM is assembled.
        #[arg(long)]
        keep_segments: bool,
    },
    /// Run replay buffer and save clips automatically on personal War Thunder kills.
    Auto {
        /// Replay buffer duration in seconds.
        #[arg(long)]
        seconds: Option<u64>,
        /// Segment duration in seconds.
        #[arg(long)]
        segment_seconds: Option<u64>,
        /// Directory where replay clip folders are written.
        #[arg(long)]
        output_dir: Option<PathBuf>,
        /// Capture source.
        #[arg(long, value_enum)]
        source: Option<CaptureSource>,
        /// Video quality preset.
        #[arg(long, value_enum)]
        quality: Option<QualityPreset>,
        /// Target frames per second. Overrides --quality.
        #[arg(long)]
        fps: Option<u32>,
        /// Target video bitrate in kbps. Overrides --quality.
        #[arg(long)]
        video_bitrate: Option<u32>,
        /// Keep copied replay segments after the final WebM is assembled.
        #[arg(long)]
        keep_segments: bool,
        /// Minimum delay between automatic clips.
        #[arg(long, default_value_t = 3)]
        cooldown_seconds: u64,
        /// Delay after a detected event before saving the replay.
        #[arg(long)]
        post_event_seconds: Option<u64>,
        /// Process events already present at startup.
        #[arg(long)]
        include_history: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Create the default user config file.
    Init {
        /// Overwrite an existing config file.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum DumpEndpoint {
    /// Dump /gamechat?lastId=0.
    Gamechat,
    /// Dump /hudmsg?lastEvt=0&lastDmg=0.
    Hudmsg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CaptureSource {
    Screen,
    #[default]
    Window,
}
