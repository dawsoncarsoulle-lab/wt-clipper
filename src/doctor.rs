use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use serde::{Deserialize, Serialize};

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
    checks.push(check_command("Flatpak", "flatpak"));
    checks.push(check_gsr_native());
    checks.push(check_gsr_flatpak_app());
    checks.push(check_gsr_flatpak_help());
    checks.push(check_gsr_list_monitors());
    checks.push(check_command("ffmpeg", "ffmpeg"));
    checks.push(check_command("ffprobe", "ffprobe"));
    checks.push(check_audio_config());
    checks.push(check_war_thunder_localhost().await);

    let output_dir = output_dir.unwrap_or_else(|| {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Videos")
            .join("WarThunder Clips")
    });
    checks.push(check_writable_dir(
        "Output dir writable",
        Ok(output_dir.clone()),
    ));
    checks.push(check_free_space_for_doctor(
        "Output dir free space",
        Ok(output_dir),
        FIVE_GIB,
    ));
    checks.push(check_writable_dir(
        "Temp dir writable",
        Ok(std::env::temp_dir().join("wt-clipper")),
    ));
    checks.push(check_free_space_for_doctor(
        "Temp dir free space",
        Ok(std::env::temp_dir().join("wt-clipper")),
        FIVE_GIB,
    ));
    checks.push(DoctorCheck::ok(
        "Config path",
        crate::config::default_config_path().display().to_string(),
    ));

    let summary = summary_for_checks(&checks);
    DoctorReport { checks, summary }
}

fn check_command(name: &str, program: &str) -> DoctorCheck {
    match Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => DoctorCheck::ok(name, "available"),
        Ok(status) => DoctorCheck::warn(
            name,
            format!("command exited with status {status}"),
            Some(format!("verify `{program}` is installed correctly")),
        ),
        Err(error) => DoctorCheck::warn(
            name,
            format!("not found: {error}"),
            Some(format!("install `{program}` or ensure it is in PATH")),
        ),
    }
}

fn check_gsr_native() -> DoctorCheck {
    match Command::new("gpu-screen-recorder")
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => {
            DoctorCheck::ok("GPU Screen Recorder native", "available")
        }
        Ok(status) => DoctorCheck::warn(
            "GPU Screen Recorder native",
            format!("command exited with status {status}"),
            Some("Flatpak mode can still be used"),
        ),
        Err(error) => DoctorCheck::warn(
            "GPU Screen Recorder native",
            format!("not found: {error}"),
            Some("install native gpu-screen-recorder or use Flatpak mode"),
        ),
    }
}

fn check_gsr_flatpak_app() -> DoctorCheck {
    match Command::new("flatpak")
        .args(["info", "com.dec05eba.gpu_screen_recorder"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => DoctorCheck::ok(
            "GPU Screen Recorder Flatpak",
            "com.dec05eba.gpu_screen_recorder installed",
        ),
        Ok(status) => DoctorCheck::warn(
            "GPU Screen Recorder Flatpak",
            format!("flatpak info exited with status {status}"),
            Some("install com.dec05eba.gpu_screen_recorder from Flatpak"),
        ),
        Err(error) => DoctorCheck::warn(
            "GPU Screen Recorder Flatpak",
            format!("flatpak unavailable: {error}"),
            Some("install Flatpak or use native mode"),
        ),
    }
}

fn check_gsr_flatpak_help() -> DoctorCheck {
    match Command::new("flatpak")
        .args([
            "run",
            "--command=gpu-screen-recorder",
            "com.dec05eba.gpu_screen_recorder",
            "--help",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => DoctorCheck::ok("GSR Flatpak command", "help works"),
        Ok(status) => DoctorCheck::warn(
            "GSR Flatpak command",
            format!("help exited with status {status}"),
            Some("check the Flatpak installation"),
        ),
        Err(error) => DoctorCheck::warn(
            "GSR Flatpak command",
            format!("could not run Flatpak GSR: {error}"),
            Some("check flatpak permissions and installation"),
        ),
    }
}

fn check_gsr_list_monitors() -> DoctorCheck {
    match Command::new("flatpak")
        .args([
            "run",
            "--command=gpu-screen-recorder",
            "com.dec05eba.gpu_screen_recorder",
            "--list-monitors",
        ])
        .stdin(Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => {
            let monitors = String::from_utf8_lossy(&output.stdout);
            let first_line = monitors.lines().next().unwrap_or("no monitors listed");
            DoctorCheck::ok("GSR monitors", first_line)
        }
        Ok(output) => DoctorCheck::warn(
            "GSR monitors",
            format!("list-monitors exited with status {}", output.status),
            Some("GSR may still work after the desktop grants access"),
        ),
        Err(error) => DoctorCheck::warn(
            "GSR monitors",
            format!("could not list monitors: {error}"),
            Some("verify Flatpak GSR can run"),
        ),
    }
}

fn check_audio_config() -> DoctorCheck {
    if std::env::var_os("WT_CLIPPER_AUDIO_DEVICE").is_some() {
        return DoctorCheck::ok("Audio input", "WT_CLIPPER_AUDIO_DEVICE set");
    }
    DoctorCheck::ok("Audio input", "default_output")
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
            DoctorCheck::error(
                "GSR Flatpak command",
                "missing",
                Some("install Flatpak GSR"),
            ),
        ];

        assert_eq!(summary_for_checks(&checks), "Doctor completed with errors.");
    }

    #[test]
    fn summary_reports_success() {
        let checks = vec![DoctorCheck::ok("GSR Flatpak command", "ready")];

        assert_eq!(
            summary_for_checks(&checks),
            "Doctor completed successfully."
        );
    }

    #[test]
    fn formats_report_text() {
        let report = DoctorReport {
            checks: vec![DoctorCheck::ok("GSR Flatpak command", "ready")],
            summary: "Doctor completed successfully.".to_owned(),
        };

        assert_eq!(
            format_report_text(&report),
            "WT Clipper Doctor\n\n[OK] GSR Flatpak command: ready\n\nSummary:\nDoctor completed successfully.\n"
        );
    }
}
