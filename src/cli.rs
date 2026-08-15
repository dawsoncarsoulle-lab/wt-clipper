use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "wt-clipper")]
#[command(about = "War Thunder clipper with GPU Screen Recorder capture")]
pub struct Cli {
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the Tauri desktop interface.
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
    /// Run GPU Screen Recorder and save clips automatically on detected game events.
    Auto {
        /// Which game to watch for events.
        #[arg(long, default_value = "warthunder")]
        game: String,
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
