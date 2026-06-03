use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use ashpd::desktop::screencast::Screencast;
use gstreamer as gst;
use serde::{Deserialize, Serialize};

use crate::capture::output::default_output_dir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DoctorStatus {
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: DoctorStatus,
    pub message: String,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionKind {
    Wayland,
    X11,
    Unknown,
}

pub async fn run_doctor(json: bool, output_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let report = build_report(output_dir).await;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", format_report_text(&report));
    }
    Ok(())
}

pub async fn build_report(output_dir: Option<PathBuf>) -> DoctorReport {
    let mut checks = Vec::new();
    let (session_kind, session_check) = check_session();
    checks.push(session_check);
    checks.extend(check_portal().await);
    checks.extend(check_gstreamer(session_kind));
    checks.push(check_war_thunder_localhost().await);
    checks.push(check_writable_dir(
        "Output dir writable",
        output_dir.map(Ok).unwrap_or_else(default_output_dir),
    ));
    checks.push(check_writable_dir(
        "Temp dir writable",
        Ok(std::env::temp_dir().join("wt-clipper-buffer")),
    ));

    let summary = summary_for_checks(&checks);
    DoctorReport { checks, summary }
}

fn check_session() -> (SessionKind, DoctorCheck) {
    let session_type = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
    let display = std::env::var("DISPLAY").ok();
    let display_suffix = match (wayland_display, display) {
        (Some(wayland), Some(display)) => {
            format!(" (WAYLAND_DISPLAY={wayland}, DISPLAY={display})")
        }
        (Some(wayland), None) => format!(" (WAYLAND_DISPLAY={wayland})"),
        (None, Some(display)) => format!(" (DISPLAY={display})"),
        (None, None) => String::new(),
    };

    match session_type.to_ascii_lowercase().as_str() {
        "wayland" => (
            SessionKind::Wayland,
            DoctorCheck::ok("Session", format!("Wayland{display_suffix}")),
        ),
        "x11" => (
            SessionKind::X11,
            DoctorCheck::ok("Session", format!("X11{display_suffix}")),
        ),
        "" => (
            SessionKind::Unknown,
            DoctorCheck::warn(
                "Session",
                format!("unknown{display_suffix}"),
                Some("XDG_SESSION_TYPE is not set"),
            ),
        ),
        other => (
            SessionKind::Unknown,
            DoctorCheck::warn(
                "Session",
                format!("unknown ({other}){display_suffix}"),
                Some("expected XDG_SESSION_TYPE=wayland or x11"),
            ),
        ),
    }
}

async fn check_portal() -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    match ashpd::zbus::Connection::session().await {
        Ok(connection) => match ashpd::zbus::fdo::DBusProxy::new(&connection).await {
            Ok(proxy) => {
                match ashpd::zbus::names::BusName::try_from("org.freedesktop.portal.Desktop") {
                    Ok(name) => match proxy.name_has_owner(name).await {
                        Ok(true) => checks.push(DoctorCheck::ok(
                            "xdg-desktop-portal available",
                            "DBus name org.freedesktop.portal.Desktop has an owner",
                        )),
                        Ok(false) => checks.push(DoctorCheck::warn(
                            "xdg-desktop-portal available",
                            "DBus name org.freedesktop.portal.Desktop has no owner",
                            Some("install/start xdg-desktop-portal and a desktop backend"),
                        )),
                        Err(error) => checks.push(DoctorCheck::warn(
                            "xdg-desktop-portal available",
                            format!("could not query DBus: {error}"),
                            Some("doctor did not call SelectSources or open any portal dialog"),
                        )),
                    },
                    Err(error) => checks.push(DoctorCheck::warn(
                        "xdg-desktop-portal available",
                        format!("invalid DBus name: {error}"),
                        Some("doctor did not call SelectSources or open any portal dialog"),
                    )),
                }
            }
            Err(error) => checks.push(DoctorCheck::warn(
                "xdg-desktop-portal available",
                format!("could not create DBus proxy: {error}"),
                Some("doctor did not call SelectSources or open any portal dialog"),
            )),
        },
        Err(error) => checks.push(DoctorCheck::warn(
            "xdg-desktop-portal available",
            format!("could not connect to session DBus: {error}"),
            Some("doctor did not call SelectSources or open any portal dialog"),
        )),
    }

    match Screencast::new().await {
        Ok(_) => checks.push(DoctorCheck::ok(
            "ScreenCast portal available",
            "org.freedesktop.portal.ScreenCast proxy created",
        )),
        Err(error) => checks.push(DoctorCheck::warn(
            "ScreenCast portal available",
            format!("unavailable: {error}"),
            Some("doctor did not create a ScreenCast session or open a selection dialog"),
        )),
    }
    checks
}

fn check_gstreamer(session_kind: SessionKind) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    match gst::init() {
        Ok(()) => checks.push(DoctorCheck::ok("GStreamer initialized", "ready")),
        Err(error) => {
            checks.push(DoctorCheck::error(
                "GStreamer initialized",
                format!("failed: {error}"),
                Some("install GStreamer runtime libraries"),
            ));
            return checks;
        }
    }

    for plugin in [
        "pipewiresrc",
        "ximagesrc",
        "videoconvert",
        "videorate",
        "audioconvert",
        "audioresample",
        "vp8enc",
        "opusenc",
        "pulsesrc",
        "webmmux",
        "splitmuxsink",
        "filesink",
        "decodebin",
        "concat",
        "queue",
    ] {
        checks.push(check_gstreamer_plugin(plugin, session_kind));
    }

    checks
}

fn check_gstreamer_plugin(plugin: &str, session_kind: SessionKind) -> DoctorCheck {
    if gst::ElementFactory::find(plugin).is_some() {
        return DoctorCheck::ok(format!("plugin {plugin}"), "available");
    }

    match (plugin, session_kind) {
        ("ximagesrc", SessionKind::Wayland) => DoctorCheck::warn(
            "plugin ximagesrc",
            "missing",
            Some("not needed for Wayland/COSMIC portal capture"),
        ),
        ("pipewiresrc", SessionKind::X11) => DoctorCheck::warn(
            "plugin pipewiresrc",
            "missing",
            Some("not needed for X11 screen capture"),
        ),
        _ => DoctorCheck::error(
            format!("plugin {plugin}"),
            "missing",
            Some("install the missing GStreamer plugin package"),
        ),
    }
}

async fn check_war_thunder_localhost() -> DoctorCheck {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return DoctorCheck::warn(
                "War Thunder localhost",
                format!("could not build HTTP client: {error}"),
                Some("War Thunder may simply not be running"),
            )
        }
    };

    match client.get("http://127.0.0.1:8111/state").send().await {
        Ok(response) if response.status().is_success() => {
            DoctorCheck::ok("War Thunder localhost", "reachable")
        }
        Ok(response) => DoctorCheck::warn(
            "War Thunder localhost",
            format!("reachable but returned HTTP {}", response.status()),
            Some("War Thunder may simply not be running"),
        ),
        Err(error) => DoctorCheck::warn(
            "War Thunder localhost",
            format!("unreachable: {error}"),
            Some("War Thunder may simply not be running; start War Thunder and enter a battle"),
        ),
    }
}

fn check_writable_dir(name: &str, dir: anyhow::Result<PathBuf>) -> DoctorCheck {
    let dir = match dir {
        Ok(dir) => dir,
        Err(error) => {
            return DoctorCheck::error(
                name,
                format!("could not resolve directory: {error}"),
                Some("set HOME or pass an explicit output directory"),
            )
        }
    };

    match ensure_writable_dir(&dir) {
        Ok(()) => DoctorCheck::ok(name, dir.display().to_string()),
        Err(error) => DoctorCheck::error(
            name,
            format!("{} ({error})", dir.display()),
            Some("check directory permissions"),
        ),
    }
}

fn ensure_writable_dir(dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(dir)?;
    let probe = dir.join(format!(".wt-clipper-doctor-{}", std::process::id()));
    fs::write(&probe, b"doctor")?;
    fs::remove_file(&probe)?;
    Ok(())
}

impl DoctorCheck {
    pub fn ok(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: DoctorStatus::Ok,
            message: message.into(),
            hint: None,
        }
    }

    pub fn warn(
        name: impl Into<String>,
        message: impl Into<String>,
        hint: Option<impl Into<String>>,
    ) -> Self {
        Self {
            name: name.into(),
            status: DoctorStatus::Warn,
            message: message.into(),
            hint: hint.map(Into::into),
        }
    }

    pub fn error(
        name: impl Into<String>,
        message: impl Into<String>,
        hint: Option<impl Into<String>>,
    ) -> Self {
        Self {
            name: name.into(),
            status: DoctorStatus::Error,
            message: message.into(),
            hint: hint.map(Into::into),
        }
    }
}

pub fn format_report_text(report: &DoctorReport) -> String {
    let mut output = String::from("WT Clipper Doctor\n\n");
    for check in &report.checks {
        output.push_str(&format_check_text(check));
    }
    output.push_str("\nSummary:\n");
    output.push_str(&report.summary);
    output.push('\n');
    output
}

pub fn format_check_text(check: &DoctorCheck) -> String {
    let status = match check.status {
        DoctorStatus::Ok => "OK",
        DoctorStatus::Warn => "WARN",
        DoctorStatus::Error => "ERROR",
    };
    let mut line = format!("[{status}] {}: {}\n", check.name, check.message);
    if let Some(hint) = &check.hint {
        line.push_str(&format!("       hint: {hint}\n"));
    }
    line
}

pub fn summary_for_checks(checks: &[DoctorCheck]) -> String {
    if checks
        .iter()
        .any(|check| check.status == DoctorStatus::Error)
    {
        "Doctor completed with errors.".to_owned()
    } else if checks
        .iter()
        .any(|check| check.status == DoctorStatus::Warn)
    {
        "Doctor completed with warnings.".to_owned()
    } else {
        "Doctor completed successfully.".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_check_with_hint() {
        let check = DoctorCheck::warn(
            "War Thunder localhost",
            "unreachable",
            Some("start War Thunder and enter a battle"),
        );

        assert_eq!(
            format_check_text(&check),
            "[WARN] War Thunder localhost: unreachable\n       hint: start War Thunder and enter a battle\n"
        );
    }

    #[test]
    fn summary_reports_warnings() {
        let checks = vec![
            DoctorCheck::ok("Session", "Wayland"),
            DoctorCheck::warn("War Thunder localhost", "unreachable", Some("not fatal")),
        ];

        assert_eq!(
            summary_for_checks(&checks),
            "Doctor completed with warnings."
        );
    }

    #[test]
    fn summary_reports_errors() {
        let checks = vec![
            DoctorCheck::ok("Session", "Wayland"),
            DoctorCheck::error("plugin vp8enc", "missing", Some("install plugins")),
        ];

        assert_eq!(summary_for_checks(&checks), "Doctor completed with errors.");
    }

    #[test]
    fn summary_reports_success() {
        let checks = vec![DoctorCheck::ok("Session", "Wayland")];

        assert_eq!(
            summary_for_checks(&checks),
            "Doctor completed successfully."
        );
    }

    #[test]
    fn formats_report_text() {
        let report = DoctorReport {
            checks: vec![DoctorCheck::ok("Session", "Wayland")],
            summary: "Doctor completed successfully.".to_owned(),
        };

        assert_eq!(
            format_report_text(&report),
            "WT Clipper Doctor\n\n[OK] Session: Wayland\n\nSummary:\nDoctor completed successfully.\n"
        );
    }
}
