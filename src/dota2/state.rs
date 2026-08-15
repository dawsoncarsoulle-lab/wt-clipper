use serde::Deserialize;

/// Top-level GSI payload sent by Dota 2 as an HTTP POST body.
///
/// Dota 2 omits sections that have not changed since the last push, so every
/// field is optional. The diff logic in [`DotaSource`](super::source::DotaSource)
/// compares successive snapshots to detect new kills/deaths.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct GsiState {
    #[serde(default)]
    pub provider: Option<Provider>,
    #[serde(default)]
    pub map: Option<Map>,
    #[serde(default)]
    pub player: Option<Player>,
    #[serde(default)]
    pub hero: Option<Hero>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Provider {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub appid: Option<u32>,
    #[serde(default)]
    pub version: Option<u32>,
    #[serde(default)]
    pub timestamp: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Map {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub game_state: Option<String>,
    #[serde(default)]
    pub clock_time: Option<i32>,
    #[serde(default)]
    pub daytime: Option<bool>,
    #[serde(default)]
    pub nightstalker_night: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Player {
    #[serde(default)]
    pub steamid: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub activity: Option<String>,
    #[serde(default)]
    pub kills: Option<u32>,
    #[serde(default)]
    pub deaths: Option<u32>,
    #[serde(default)]
    pub assists: Option<u32>,
    #[serde(default)]
    pub team_name: Option<String>,
    #[serde(default)]
    pub gold: Option<u32>,
    #[serde(default)]
    pub gpm: Option<u32>,
    #[serde(default)]
    pub xpm: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Hero {
    #[serde(default)]
    pub id: Option<u32>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub level: Option<u32>,
    #[serde(default)]
    pub alive: Option<bool>,
    #[serde(default)]
    pub health: Option<u32>,
    #[serde(default)]
    pub max_health: Option<u32>,
    #[serde(default)]
    pub mana: Option<u32>,
    #[serde(default)]
    pub max_mana: Option<u32>,
}
