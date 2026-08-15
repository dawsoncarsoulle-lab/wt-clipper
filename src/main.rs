use std::{path::PathBuf, time::Duration};

use clap::Parser;
use tracing::debug;

use wt_clipper::{
    app::auto::{run_auto_clip, AutoClipConfig},
    cli::{Cli, Command, ConfigCommand, DumpEndpoint},
    config::{default_config_path, AppConfig},
    doctor,
    warthunder::{
        client::{Endpoint, EndpointProbe, WarThunderClient},
        parser::{is_personal_kill, parse_gamechat_event},
        recent::RecentMessageCache,
        source::WarThunderSource,
    },
};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "wt_clipper=info".into()),
        )
        .without_time()
        .init();

    if matches!(cli.command, Command::Gui) {
        return launch_tauri_gui(cli.config);
    }

    tokio::runtime::Runtime::new()?.block_on(async_main(cli))
}

async fn async_main(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Gui => unreachable!("gui command is handled before tokio runtime creation"),
        Command::Config {
            command: ConfigCommand::Init { force },
        } => {
            let path = cli
                .config
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(default_config_path);
            AppConfig::write_default(&path, force)?;
            println!("Config written: {}", path.display());
            Ok(())
        }
        Command::Doctor { json } => {
            let config = AppConfig::load(cli.config.as_deref())?;
            doctor::run_doctor(json, Some(config.capture.output_dir_path()?)).await
        }
        Command::Auto {
            cooldown_seconds,
            post_event_seconds,
            include_history,
        } => {
            let config = AppConfig::load(cli.config.as_deref())?;
            let client = WarThunderClient::new(config.war_thunder.clone())?;
            let source = Box::new(WarThunderSource::new(
                client,
                config.war_thunder.clone(),
                config.triggers.clone(),
            ));
            let poll_interval = config.war_thunder.poll_interval();
            run_auto_clip(
                source,
                poll_interval,
                AutoClipConfig {
                    cooldown: Duration::from_secs(cooldown_seconds),
                    post_event_delay: Duration::from_secs(
                        post_event_seconds.unwrap_or(config.clip.post_event_seconds),
                    ),
                    multi_kill_window: Duration::from_secs(config.clip.multi_kill_window_seconds),
                    include_history,
                    triggers: config.triggers.clone(),
                    ui_events: None,
                    command_rx: None,
                    capture: config.capture.clone(),
                },
            )
            .await
        }
        command => {
            let config = AppConfig::load(cli.config.as_deref())?;
            let client = WarThunderClient::new(config.war_thunder.clone())?;

            match command {
                Command::Status => status(&client).await,
                Command::Dump { endpoint } => dump(&client, endpoint).await,
                Command::Watch { include_history } => {
                    watch(&client, &config.war_thunder, include_history).await
                }
                Command::Gui
                | Command::Config { .. }
                | Command::Doctor { .. }
                | Command::Auto { .. } => unreachable!("command handled above"),
            }
        }
    }
}

fn launch_tauri_gui(config_path: Option<PathBuf>) -> anyhow::Result<()> {
    let manifest_dir = std::env::current_dir()?;
    let tauri_dir = manifest_dir.join("src-tauri");
    if !tauri_dir.join("tauri.conf.json").exists() {
        anyhow::bail!(
            "Tauri UI not found at {}; run from the wt-clipper source tree",
            tauri_dir.display()
        );
    }

    let mut command = std::process::Command::new("cargo");
    command.arg("tauri").arg("dev").current_dir(&manifest_dir);
    if let Some(path) = config_path {
        command.env("WT_CLIPPER_CONFIG", path);
    }
    let status = command.status()?;
    if !status.success() {
        anyhow::bail!("Tauri GUI exited with status {status}");
    }
    Ok(())
}

async fn status(client: &WarThunderClient) -> anyhow::Result<()> {
    for probe in client.probe_all().await {
        match probe {
            EndpointProbe::Ok { endpoint, summary } => {
                println!(
                    "{:<12} OK {}",
                    endpoint_label(endpoint),
                    summary.unwrap_or_default()
                );
            }
            EndpointProbe::Failed { endpoint, error } => {
                println!("{:<12} FAIL {}", endpoint_label(endpoint), error);
            }
        }
    }
    Ok(())
}

async fn dump(client: &WarThunderClient, endpoint: DumpEndpoint) -> anyhow::Result<()> {
    match endpoint {
        DumpEndpoint::Gamechat => {
            println!("{}", client.fetch_raw("/gamechat?lastId=0").await?);
        }
        DumpEndpoint::Hudmsg => {
            println!("{}", client.fetch_raw("/hudmsg?lastEvt=0&lastDmg=0").await?);
        }
    }
    Ok(())
}

async fn watch(
    client: &WarThunderClient,
    config: &wt_clipper::config::WarThunderConfig,
    include_history: bool,
) -> anyhow::Result<()> {
    let mut last_chat_id = 0_u64;
    let mut last_evt_msg_id = 0_u64;
    let mut last_dmg_msg_id = 0_u64;
    let mut seen = RecentMessageCache::new(1000);
    if !include_history {
        if let Ok(chat) = client.fetch_gamechat(0).await {
            last_chat_id = chat.next_last_id;
            for message in chat.messages {
                seen.insert(message.stable_key_with_prefix("gamechat"));
            }
        }
        if let Ok(hud) = client.fetch_hudmsg(0, 0).await {
            last_evt_msg_id = hud.next_last_evt_id;
            last_dmg_msg_id = hud.next_last_dmg_id;
            for message in hud.events.into_iter().chain(hud.damage) {
                seen.insert(message.stable_key_with_prefix("hud"));
            }
        }
    }

    let player = config.player_name.as_deref();
    let mut ticker = tokio::time::interval(config.poll_interval());
    loop {
        ticker.tick().await;

        if let Ok(chat) = client.fetch_gamechat(last_chat_id).await {
            last_chat_id = chat.next_last_id;
            for message in chat.messages {
                let key = message.stable_key_with_prefix("gamechat");
                if !seen.contains(&key) {
                    seen.insert(key);
                    print_message("gamechat", &message.text, player);
                }
            }
        }

        if let Ok(hud) = client.fetch_hudmsg(last_evt_msg_id, last_dmg_msg_id).await {
            last_evt_msg_id = hud.next_last_evt_id;
            last_dmg_msg_id = hud.next_last_dmg_id;
            for message in hud.events.into_iter().chain(hud.damage) {
                let key = message.stable_key_with_prefix("hud");
                if !seen.contains(&key) {
                    seen.insert(key);
                    print_message("hud", &message.text, player);
                }
            }
        }
    }
}

fn print_message(source: &str, text: &str, player: Option<&str>) {
    let event = parse_gamechat_event(text);
    let personal = player
        .map(|player| is_personal_kill(&event, Some(player)))
        .unwrap_or(false);
    println!(
        "[{source}] {}{}",
        text,
        if personal { "  <-- personal kill" } else { "" }
    );
    debug!(?event, personal, "parsed WT event");
}

fn endpoint_label(endpoint: Endpoint) -> &'static str {
    match endpoint {
        Endpoint::MapObj => "map_obj",
        Endpoint::Indicators => "indicators",
        Endpoint::GameChat => "gamechat",
        Endpoint::MapInfo => "map_info",
        Endpoint::State => "state",
        Endpoint::HudMsg => "hudmsg",
    }
}
