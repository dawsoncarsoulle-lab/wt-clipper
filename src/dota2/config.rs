use serde::{Deserialize, Serialize};

/// Configuration for the Dota 2 Game State Integration source.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct DotaConfig {
    /// Local port for the GSI HTTP server to listen on. Dota 2 will POST
    /// game state JSON to http://127.0.0.1:<port>/.
    #[serde(default = "default_gsi_port")]
    pub port: u16,
    /// Optional auth token. If set, Dota 2 must send the same token in its
    /// GSI config under "auth"."token".
    #[serde(default)]
    pub auth_token: Option<String>,
    /// The player's Steam name, used for personal kill detection. If empty,
    /// the GSI "player.name" field is used instead.
    #[serde(default)]
    pub player_name: Option<String>,
}

impl Default for DotaConfig {
    fn default() -> Self {
        Self {
            port: default_gsi_port(),
            auth_token: None,
            player_name: None,
        }
    }
}

fn default_gsi_port() -> u16 {
    3838
}
