use std::{fs, path::Path};

use serde::{Deserialize, Serialize};
use tracing::debug;
use x11rb::{
    connection::Connection,
    protocol::xproto::{AtomEnum, ConnectionExt, Window},
    rust_connection::RustConnection,
};

const WAR_THUNDER_PATTERNS: &[&str] = &[
    "war thunder",
    "warthunder",
    "aces.exe",
    "aces",
    "steam_app_236390",
    "236390",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct X11WindowInfo {
    pub id: u32,
    pub id_hex: String,
    pub title: String,
    pub class_name: Option<String>,
    pub pid: Option<u32>,
    pub process_cmdline: Option<String>,
    pub score: i32,
}

pub fn detect_war_thunder_window_x11() -> Option<X11WindowInfo> {
    match list_x11_windows() {
        Ok(mut windows) => {
            windows.retain(|window| window.score > 0);
            windows.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
            windows.into_iter().next()
        }
        Err(error) => {
            debug!(%error, "failed to list X11 windows for War Thunder auto capture");
            None
        }
    }
}

pub fn list_x11_windows() -> anyhow::Result<Vec<X11WindowInfo>> {
    if std::env::var("DISPLAY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .is_none()
    {
        return Ok(Vec::new());
    }

    let (connection, screen_num) = x11rb::connect(None)?;
    let screen = &connection.setup().roots[screen_num];
    let root = screen.root;
    let client_list = intern_atom(&connection, b"_NET_CLIENT_LIST")?;
    let windows = get_window_list(&connection, root, client_list).unwrap_or_default();

    let mut infos = Vec::new();
    for window in windows {
        let title = get_window_title(&connection, window).unwrap_or_default();
        let class_name = get_wm_class(&connection, window).ok().flatten();
        let pid = get_window_pid(&connection, window).ok().flatten();
        let process_cmdline = pid.and_then(read_process_cmdline);
        let score =
            score_war_thunder_window(&title, class_name.as_deref(), process_cmdline.as_deref());
        infos.push(X11WindowInfo {
            id: window,
            id_hex: format!("0x{window:x}"),
            title,
            class_name,
            pid,
            process_cmdline,
            score,
        });
    }
    Ok(infos)
}

fn intern_atom(connection: &RustConnection, name: &[u8]) -> anyhow::Result<u32> {
    Ok(connection.intern_atom(false, name)?.reply()?.atom)
}

fn get_window_list(
    connection: &RustConnection,
    root: Window,
    client_list: u32,
) -> anyhow::Result<Vec<Window>> {
    let reply = connection
        .get_property(false, root, client_list, AtomEnum::WINDOW, 0, u32::MAX)?
        .reply()?;
    Ok(reply
        .value32()
        .map(|values| values.collect())
        .unwrap_or_default())
}

fn get_window_title(connection: &RustConnection, window: Window) -> anyhow::Result<String> {
    let net_wm_name = intern_atom(connection, b"_NET_WM_NAME")?;
    let utf8 = intern_atom(connection, b"UTF8_STRING")?;
    if let Some(title) = get_string_property(connection, window, net_wm_name, utf8)? {
        return Ok(title);
    }
    Ok(get_string_property(
        connection,
        window,
        AtomEnum::WM_NAME.into(),
        AtomEnum::STRING.into(),
    )?
    .unwrap_or_default())
}

fn get_wm_class(connection: &RustConnection, window: Window) -> anyhow::Result<Option<String>> {
    let Some(raw) = get_string_property(
        connection,
        window,
        AtomEnum::WM_CLASS.into(),
        AtomEnum::STRING.into(),
    )?
    else {
        return Ok(None);
    };
    let class = raw
        .split('\0')
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Ok((!class.trim().is_empty()).then_some(class))
}

fn get_window_pid(connection: &RustConnection, window: Window) -> anyhow::Result<Option<u32>> {
    let atom = intern_atom(connection, b"_NET_WM_PID")?;
    let reply = connection
        .get_property(false, window, atom, AtomEnum::CARDINAL, 0, 1)?
        .reply()?;
    Ok(reply.value32().and_then(|mut values| values.next()))
}

fn get_string_property(
    connection: &RustConnection,
    window: Window,
    property: u32,
    ty: u32,
) -> anyhow::Result<Option<String>> {
    let reply = connection
        .get_property(false, window, property, ty, 0, u32::MAX)?
        .reply()?;
    if reply.value.is_empty() {
        return Ok(None);
    }
    let value = String::from_utf8_lossy(&reply.value)
        .trim_matches('\0')
        .trim()
        .to_owned();
    Ok((!value.is_empty()).then_some(value))
}

fn read_process_cmdline(pid: u32) -> Option<String> {
    let path = format!("/proc/{pid}/cmdline");
    let bytes = fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let value = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| {
            let value = String::from_utf8_lossy(part).into_owned();
            Path::new(&value)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .unwrap_or(value)
        })
        .collect::<Vec<_>>()
        .join(" ");
    (!value.trim().is_empty()).then_some(value)
}

fn score_war_thunder_window(
    title: &str,
    class_name: Option<&str>,
    process_cmdline: Option<&str>,
) -> i32 {
    let title = title.to_ascii_lowercase();
    let class_name = class_name.unwrap_or_default().to_ascii_lowercase();
    let process_cmdline = process_cmdline.unwrap_or_default().to_ascii_lowercase();
    let mut score = 0;

    if title.contains("war thunder") || title.contains("warthunder") {
        score += 100;
    }
    if class_name.contains("steam_app_236390") || class_name.contains("236390") {
        score += 80;
    }
    if process_cmdline.contains("aces.exe") || process_cmdline.contains("aces") {
        score += 70;
    }
    if class_name.contains("aces.exe") || class_name.contains("aces") {
        score += 50;
    }
    if title == "aces" || title.contains("aces.exe") {
        score += 30;
    }

    for pattern in WAR_THUNDER_PATTERNS {
        if title.contains(pattern)
            || class_name.contains(pattern)
            || process_cmdline.contains(pattern)
        {
            score += 10;
        }
    }

    score
}

#[cfg(test)]
mod tests {
    use super::score_war_thunder_window;

    #[test]
    fn scores_war_thunder_title_highly() {
        assert!(score_war_thunder_window("War Thunder", None, None) >= 100);
    }

    #[test]
    fn scores_steam_app_class() {
        assert!(score_war_thunder_window("", Some("steam_app_236390"), None) >= 80);
    }

    #[test]
    fn scores_aces_cmdline() {
        assert!(score_war_thunder_window("", None, Some("steam aces.exe")) >= 70);
    }

    #[test]
    fn ignores_unrelated_window() {
        assert_eq!(
            score_war_thunder_window("Firefox", Some("firefox"), None),
            0
        );
    }
}
