use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use ashpd::desktop::screencast::Screencast;
use gstreamer as gst;
use serde::{Deserialize, Serialize};

use crate::{
    capture::{
        audio::resolve_system_audio_source,
        output::default_output_dir,
        recorder::{choose_backend, CaptureBackend},
        x11::resolve_x11_window_id,
    },
    cli::CaptureSource,
};

const ONE_GIB: u64 = 1024 * 1024 * 1024;
const FIVE_GIB: u64 = 5 * ONE_GIB;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FreeSpaceReport {
    pub path: PathBuf,
    pub available_bytes: u64,
    pub required_bytes: u64,
    pub status: DoctorStatus,
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
    checks.push(check_capture_backend(session_kind));
    checks.extend(check_portal().await);
    checks.extend(check_gstreamer(session_kind));
    checks.push(check_x11_window(session_kind));
    checks.push(check_audio_source());
    checks.push(check_war_thunder_localhost().await);
    let output_dir = output_dir.map(Ok).unwrap_or_else(default_output_dir);
    match output_dir {
        Ok(output_dir) => {
            checks.push(check_writable_dir(
                "Output dir writable",
                Ok(output_dir.clone()),
            ));
            checks.push(check_free_space_for_doctor(
                "Output dir free space",
                Ok(output_dir),
                FIVE_GIB,
            ));
        }
        Err(error) => {
            let message = error.to_string();
            checks.push(DoctorCheck::error(
                "Output dir writable",
                format!("could not resolve directory: {message}"),
                Some("set HOME or pass an explicit output directory"),
            ));
            checks.push(DoctorCheck::error(
                "Output dir free space",
                format!("could not resolve directory: {message}"),
                Some("set HOME or pass an explicit output directory"),
            ));
        }
    }
    checks.push(check_writable_dir(
        "Temp dir writable",
        Ok(std::env::temp_dir().join("wt-clipper-buffer")),
    ));
    checks.push(check_free_space_for_doctor(
        "Temp dir free space",
        Ok(std::env::temp_dir().join("wt-clipper-buffer")),
        FIVE_GIB,
    ));

    let summary = summary_for_checks(&checks);
    DoctorReport { checks, summary }
}

fn check_capture_backend(session_kind: SessionKind) -> DoctorCheck {
    let session = match session_kind {
        SessionKind::X11 => "x11",
        SessionKind::Wayland => "wayland",
        SessionKind::Unknown => "",
    };
    match choose_backend(session) {
        CaptureBackend::X11 => {
            DoctorCheck::ok("Capture backend", "X11 window capture via ximagesrc")
        }
        CaptureBackend::PortalPipeWire => DoctorCheck::ok(
            "Capture backend",
            "Wayland/COSMIC portal capture via pipewiresrc",
        ),
        CaptureBackend::ManualPipeWirePath(path) => {
            DoctorCheck::ok("Capture backend", format!("manual PipeWire path {path}"))
        }
        CaptureBackend::ManualPipeWireTarget(target) => DoctorCheck::ok(
            "Capture backend",
            format!("manual PipeWire target {target}"),
        ),
    }
}

fn check_x11_window(session_kind: SessionKind) -> DoctorCheck {
    if session_kind != SessionKind::X11 {
        return DoctorCheck::ok("X11 War Thunder window", "not needed for this session");
    }

    match resolve_x11_window_id(CaptureSource::Window) {
        Ok(Some(window)) => DoctorCheck::ok(
            "X11 War Thunder window",
            format!(
                "found {:#x}{}{}",
                window.id,
                window
                    .title
                    .as_deref()
                    .map(|title| format!(" title={title:?}"))
                    .unwrap_or_default(),
                window
                    .class
                    .as_deref()
                    .map(|class| format!(" class={class:?}"))
                    .unwrap_or_default()
            ),
        ),
        Ok(None) => DoctorCheck::warn(
            "X11 War Thunder window",
            "not selected",
            Some("set source=window or use screen capture"),
        ),
        Err(error) => DoctorCheck::warn(
            "X11 War Thunder window",
            format!("not found: {error}"),
            Some("launch War Thunder first, or set WT_CLIPPER_X11_WINDOW_ID"),
        ),
    }
}

fn check_audio_source() -> DoctorCheck {
    match resolve_system_audio_source() {
        Some(source) => DoctorCheck::ok("System audio monitor", source.device),
        None => DoctorCheck::warn(
            "System audio monitor",
            "not detected",
            Some(
                "install/use pactl, or set WT_CLIPPER_AUDIO_DEVICE; audio capture will be skipped",
            ),
        ),
    }
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

pub fn check_free_space(path: &Path, required_bytes: u64) -> anyhow::Result<FreeSpaceReport> {
    fs::create_dir_all(path)?;
    let available_bytes = fs2::available_space(path)?;
    let status = if available_bytes < ONE_GIB || available_bytes < required_bytes {
        DoctorStatus::Error
    } else if available_bytes < FIVE_GIB {
        DoctorStatus::Warn
    } else {
        DoctorStatus::Ok
    };
    Ok(FreeSpaceReport {
        path: path.to_path_buf(),
        available_bytes,
        required_bytes,
        status,
    })
}

fn check_free_space_for_doctor(
    name: &str,
    dir: anyhow::Result<PathBuf>,
    required_bytes: u64,
) -> DoctorCheck {
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

    match check_free_space(&dir, required_bytes) {
        Ok(report) => {
            let message = format!(
                "{} free at {}",
                format_bytes(report.available_bytes),
                report.path.display()
            );
            match report.status {
                DoctorStatus::Ok => DoctorCheck::ok(name, message),
                DoctorStatus::Warn => DoctorCheck::warn(
                    name,
                    message,
                    Some("keep more than 5 GiB free for reliable clip creation"),
                ),
                DoctorStatus::Error => DoctorCheck::error(
                    name,
                    message,
                    Some("free disk space before recording or choose another output directory"),
                ),
            }
        }
        Err(error) => DoctorCheck::error(
            name,
            format!("{} ({error})", dir.display()),
            Some("check directory permissions and mounted filesystem state"),
        ),
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= ONE_GIB {
        format!("{:.1} GiB", bytes as f64 / ONE_GIB as f64)
    } else {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
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
