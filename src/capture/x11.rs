use anyhow::Context;
use x11rb::{
    connection::Connection,
    protocol::xproto::{Atom, AtomEnum, ConnectionExt, Window},
    rust_connection::RustConnection,
};

use crate::cli::CaptureSource;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct X11WindowSelection {
    pub id: u32,
    pub title: Option<String>,
    pub class: Option<String>,
}

pub fn resolve_x11_window_id(source: CaptureSource) -> anyhow::Result<Option<X11WindowSelection>> {
    if source == CaptureSource::Screen {
        return Ok(None);
    }

    if let Ok(raw_id) = std::env::var("WT_CLIPPER_X11_WINDOW_ID") {
        let id = parse_x11_window_id(&raw_id)
            .with_context(|| format!("invalid WT_CLIPPER_X11_WINDOW_ID value: {raw_id}"))?;
        return Ok(Some(X11WindowSelection {
            id,
            title: Some("WT_CLIPPER_X11_WINDOW_ID".to_owned()),
            class: None,
        }));
    }

    find_war_thunder_window()?.map_or_else(
        || {
            anyhow::bail!(
                "War Thunder window not found on X11. Launch War Thunder first, or set WT_CLIPPER_X11_WINDOW_ID to the window id."
            )
        },
        |window| Ok(Some(window)),
    )
}

fn parse_x11_window_id(value: &str) -> anyhow::Result<u32> {
    let trimmed = value.trim();
    let without_prefix = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"));
    if let Some(hex) = without_prefix {
        return u32::from_str_radix(hex, 16).context("failed to parse hex X11 window id");
    }

    trimmed
        .parse::<u32>()
        .context("failed to parse decimal X11 window id")
}

fn find_war_thunder_window() -> anyhow::Result<Option<X11WindowSelection>> {
    let (connection, screen_num) =
        x11rb::connect(None).context("failed to connect to X11 display")?;
    let root = connection.setup().roots[screen_num].root;
    let atoms = X11Atoms::load(&connection)?;
    let mut windows = Vec::new();
    collect_windows(&connection, root, &mut windows)?;

    let mut best = None;
    let mut best_score = 0;
    for window in windows {
        let title = read_text_property(&connection, window, atoms.net_wm_name)
            .or_else(|| read_text_property(&connection, window, atoms.wm_name));
        let class = read_text_property(&connection, window, atoms.wm_class);
        let score = war_thunder_window_score(title.as_deref(), class.as_deref());
        if score > best_score {
            best_score = score;
            best = Some(X11WindowSelection {
                id: window,
                title,
                class,
            });
        }
    }

    Ok(best)
}

fn collect_windows<C: Connection>(
    connection: &C,
    window: Window,
    output: &mut Vec<Window>,
) -> anyhow::Result<()> {
    output.push(window);
    let tree = connection
        .query_tree(window)
        .with_context(|| format!("failed to query X11 tree for window {window:#x}"))?
        .reply()
        .with_context(|| format!("failed to read X11 tree for window {window:#x}"))?;
    for child in tree.children {
        collect_windows(connection, child, output)?;
    }
    Ok(())
}

fn read_text_property<C: Connection>(connection: &C, window: Window, atom: Atom) -> Option<String> {
    let property = connection
        .get_property(false, window, atom, AtomEnum::ANY, 0, u32::MAX)
        .ok()?
        .reply()
        .ok()?;
    decode_x11_text(&property.value)
}

fn decode_x11_text(value: &[u8]) -> Option<String> {
    let decoded = String::from_utf8_lossy(value)
        .split('\0')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if decoded.is_empty() {
        None
    } else {
        Some(decoded)
    }
}

fn war_thunder_window_score(title: Option<&str>, class: Option<&str>) -> u8 {
    let title = title.unwrap_or_default().to_ascii_lowercase();
    let class = class.unwrap_or_default().to_ascii_lowercase();
    let combined = format!("{title} {class}");

    if title.contains("war thunder") {
        100
    } else if class.contains("war thunder") {
        95
    } else if class.contains("aces.exe") || class.contains("aces") {
        90
    } else if combined.contains("gaijin") && combined.contains("war") {
        80
    } else {
        0
    }
}

struct X11Atoms {
    wm_name: Atom,
    wm_class: Atom,
    net_wm_name: Atom,
}

impl X11Atoms {
    fn load(connection: &RustConnection) -> anyhow::Result<Self> {
        Ok(Self {
            wm_name: intern_atom(connection, b"WM_NAME")?,
            wm_class: intern_atom(connection, b"WM_CLASS")?,
            net_wm_name: intern_atom(connection, b"_NET_WM_NAME")?,
        })
    }
}

fn intern_atom(connection: &RustConnection, name: &[u8]) -> anyhow::Result<Atom> {
    Ok(connection
        .intern_atom(false, name)
        .with_context(|| {
            format!(
                "failed to request X11 atom {}",
                String::from_utf8_lossy(name)
            )
        })?
        .reply()
        .with_context(|| format!("failed to read X11 atom {}", String::from_utf8_lossy(name)))?
        .atom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_decimal_and_hex_x11_ids() {
        assert_eq!(parse_x11_window_id("12345").unwrap(), 12345);
        assert_eq!(parse_x11_window_id("0x3a00007").unwrap(), 0x3a00007);
    }

    #[test]
    fn decodes_null_separated_x11_text() {
        assert_eq!(
            decode_x11_text(b"aces.exe\0Aces.exe\0").as_deref(),
            Some("aces.exe Aces.exe")
        );
    }

    #[test]
    fn scores_war_thunder_window_names() {
        assert!(war_thunder_window_score(Some("War Thunder"), None) > 0);
        assert!(war_thunder_window_score(None, Some("aces.exe Aces.exe")) > 0);
        assert_eq!(war_thunder_window_score(Some("WT Clipper"), None), 0);
    }
}
