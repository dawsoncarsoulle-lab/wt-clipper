use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use crate::config::{AppConfig, GpuScreenRecorderMode};
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

#[derive(Debug, Clone, Serialize)]
pub struct SystemRequirementsReport {
    pub app_version: String,
    pub os: Option<String>,
    pub session_type: Option<String>,

    pub war_thunder_api: RequirementCheck,
    pub flatpak: RequirementCheck,
    pub gsr_flatpak: RequirementCheck,
    pub gsr_native: RequirementCheck,
    pub ffmpeg: RequirementCheck,
    pub ffprobe: RequirementCheck,

    pub capture_mode: String,
    pub capture_strategy: String,
    pub configured_target: String,
    pub effective_target: Option<String>,
    pub target_reason: Option<String>,

    pub output_dir: RequirementCheck,
    pub config_dir: RequirementCheck,

    pub install_commands: InstallCommands,
    pub logs: DiagnosticLogsSummary,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RequirementCheck {
    pub id: String,
    pub label: String,
    pub status: RequirementStatus,
    pub summary: String,
    pub summary_key: Option<String>,
    pub details: Option<String>,
    pub command: Option<String>,
    pub version: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequirementStatus {
    Ok,
    Warning,
    Error,
    Missing,
    Unknown,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InstallCommands {
    pub install_gsr_flatpak: String,
    pub install_ffmpeg_apt: String,
    pub install_ffmpeg_pacman: String,
    pub install_ffmpeg_dnf: String,
    pub install_flatpak_apt: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DiagnosticLogsSummary {
    pub recent_logs: Vec<String>,
    pub logs_path: Option<String>,
    pub can_copy_logs: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiagnosticRuntimeContext {
    pub war_thunder_connected: Option<bool>,
    pub effective_target: Option<String>,
    pub target_reason: Option<String>,
    pub gsr_command_line: Option<String>,
    pub recent_logs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandProbe {
    status: RequirementStatus,
    summary: String,
    details: Option<String>,
    version: Option<String>,
    path: Option<String>,
    output: Option<String>,
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

pub async fn build_system_requirements_report(
    config: &AppConfig,
    runtime: Option<DiagnosticRuntimeContext>,
) -> SystemRequirementsReport {
    let runtime = runtime.unwrap_or_default();
    let flatpak = check_flatpak_requirement();
    let mut gsr_flatpak = check_gsr_flatpak_requirement(&flatpak);
    let mut gsr_native = check_gsr_native_requirement();
    apply_gsr_mode_warning(
        &config.capture.gpu_screen_recorder_mode,
        &mut gsr_flatpak,
        &mut gsr_native,
    );

    let output_dir = config
        .library
        .output_dir_path()
        .or_else(|_| config.capture.output_dir_path());
    let config_dir = crate::config::default_config_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    SystemRequirementsReport {
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        os: Some(std::env::consts::OS.to_owned()),
        session_type: std::env::var("XDG_SESSION_TYPE").ok(),
        war_thunder_api: check_war_thunder_api_requirement(
            &config.war_thunder.base_url,
            runtime.war_thunder_connected,
        )
        .await,
        flatpak,
        gsr_flatpak,
        gsr_native,
        ffmpeg: check_versioned_command_requirement(
            "ffmpeg",
            "FFmpeg",
            "ffmpeg",
            &["-version"],
            Some(install_commands().install_ffmpeg_apt),
        ),
        ffprobe: check_versioned_command_requirement(
            "ffprobe",
            "FFprobe",
            "ffprobe",
            &["-version"],
            Some(install_commands().install_ffmpeg_apt),
        ),
        capture_mode: format!("{:?}", config.capture.gpu_screen_recorder_mode).to_ascii_lowercase(),
        capture_strategy: format!("{:?}", config.capture.capture_strategy).to_ascii_lowercase(),
        configured_target: config.capture.target.clone(),
        effective_target: runtime.effective_target,
        target_reason: runtime.target_reason,
        output_dir: check_directory_requirement("output_dir", "Output directory", output_dir),
        config_dir: check_directory_requirement("config_dir", "Config directory", Ok(config_dir)),
        install_commands: install_commands(),
        logs: DiagnosticLogsSummary {
            recent_logs: diagnostic_logs_with_command(
                runtime.recent_logs,
                runtime.gsr_command_line,
            ),
            logs_path: None,
            can_copy_logs: true,
        },
    }
}

pub fn format_system_requirements_report(report: &SystemRequirementsReport) -> String {
    let mut output = String::from("WT Clip Diagnostics Report\n==========================\n\n");
    output.push_str(&format!("Version: {}\n", report.app_version));
    output.push_str(&format!(
        "OS: {}\n",
        report.os.as_deref().unwrap_or("unknown")
    ));
    output.push_str(&format!(
        "Session: {}\n",
        report.session_type.as_deref().unwrap_or("unknown")
    ));
    output.push_str(&format!("Capture mode: {}\n", report.capture_mode));
    output.push_str(&format!("Capture strategy: {}\n", report.capture_strategy));
    output.push_str(&format!(
        "Configured target: {}\n",
        report.configured_target
    ));
    if let Some(target) = &report.effective_target {
        output.push_str(&format!("Effective target: {target}\n"));
    }
    if let Some(reason) = &report.target_reason {
        output.push_str(&format!("Target reason: {reason}\n"));
    }
    output.push('\n');

    for check in [
        &report.war_thunder_api,
        &report.flatpak,
        &report.gsr_flatpak,
        &report.gsr_native,
        &report.ffmpeg,
        &report.ffprobe,
        &report.output_dir,
        &report.config_dir,
    ] {
        output.push_str(&format!(
            "{}: {} - {}\n",
            check.label,
            requirement_status_label(&check.status),
            check.summary
        ));
        if let Some(version) = &check.version {
            output.push_str(&format!("  version: {version}\n"));
        }
        if let Some(path) = &check.path {
            output.push_str(&format!("  path: {path}\n"));
        }
        if let Some(details) = &check.details {
            output.push_str(&format!("  details: {details}\n"));
        }
    }

    output.push_str("\nInstall commands:\n");
    output.push_str(&format!(
        "GSR Flatpak: {}\n",
        report.install_commands.install_gsr_flatpak
    ));
    output.push_str(&format!(
        "Flatpak Debian/Ubuntu/Pop!_OS: {}\n",
        report.install_commands.install_flatpak_apt
    ));
    output.push_str(&format!(
        "FFmpeg Debian/Ubuntu/Pop!_OS: {}\n",
        report.install_commands.install_ffmpeg_apt
    ));
    output.push_str(&format!(
        "FFmpeg Arch: {}\n",
        report.install_commands.install_ffmpeg_pacman
    ));
    output.push_str(&format!(
        "FFmpeg Fedora: {}\n",
        report.install_commands.install_ffmpeg_dnf
    ));

    output.push_str("\nRecent logs:\n");
    for line in &report.logs.recent_logs {
        output.push_str(line);
        output.push('\n');
    }
    output
}

pub fn format_recent_logs(report: &SystemRequirementsReport) -> String {
    let mut output = String::new();
    if let Some(path) = &report.logs.logs_path {
        output.push_str(&format!("Logs path: {path}\n\n"));
    }
    for line in &report.logs.recent_logs {
        output.push_str(line);
        output.push('\n');
    }
    if output.trim().is_empty() {
        "No persistent log file configured yet.\n".to_owned()
    } else {
        output
    }
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

fn check_flatpak_requirement() -> RequirementCheck {
    let probe = run_command_probe("flatpak", &["--version"]);
    requirement_from_probe(
        "flatpak",
        "Flatpak",
        probe,
        "Flatpak is available.",
        "Flatpak is not installed.",
        Some(install_commands().install_flatpak_apt),
    )
}

fn check_gsr_flatpak_requirement(flatpak: &RequirementCheck) -> RequirementCheck {
    if flatpak.status == RequirementStatus::Missing {
        return RequirementCheck::warning(
            "gsr_flatpak",
            "GPU Screen Recorder Flatpak",
            "Flatpak is missing, so WT Clip cannot check the GSR Flatpak app.",
            Some("diagnostics.summary.gsrFlatpakFlatpakMissing"),
            Some("Install Flatpak first, then install com.dec05eba.gpu_screen_recorder."),
            Some(install_commands().install_gsr_flatpak),
        );
    }

    let info = run_command_probe("flatpak", &["info", "com.dec05eba.gpu_screen_recorder"]);
    if !flatpak_gsr_info_is_installed(&info) {
        return RequirementCheck::missing(
            "gsr_flatpak",
            "GPU Screen Recorder Flatpak",
            "GSR Flatpak app is not installed.",
            Some("diagnostics.summary.gsrFlatpakMissing"),
            info.details,
            Some(install_commands().install_gsr_flatpak),
        );
    }

    let help = run_command_probe(
        "flatpak",
        &[
            "run",
            "--command=gpu-screen-recorder",
            "com.dec05eba.gpu_screen_recorder",
            "--help",
        ],
    );
    if gsr_flatpak_binary_reachable(&help) {
        RequirementCheck {
            id: "gsr_flatpak".to_owned(),
            label: "GPU Screen Recorder Flatpak".to_owned(),
            status: RequirementStatus::Ok,
            summary: "GPU Screen Recorder Flatpak is available.".to_owned(),
            summary_key: Some("diagnostics.summary.gsrFlatpakAvailable".to_owned()),
            details: help.details,
            command: Some(
                "flatpak run --command=gpu-screen-recorder com.dec05eba.gpu_screen_recorder"
                    .to_owned(),
            ),
            version: info.version.or(help.version),
            path: None,
        }
    } else {
        RequirementCheck::warning(
            "gsr_flatpak",
            "GPU Screen Recorder Flatpak",
            "GSR Flatpak is installed, but the recorder command could not be verified.",
            Some("diagnostics.summary.gsrFlatpakCommandFailed"),
            help.details,
            Some(install_commands().install_gsr_flatpak),
        )
    }
}

fn check_gsr_native_requirement() -> RequirementCheck {
    let probe = run_command_probe("gpu-screen-recorder", &["--help"]);
    requirement_from_probe(
        "gsr_native",
        "GPU Screen Recorder Native",
        probe,
        "GSR native command is available.",
        "GSR native command is not installed.",
        None,
    )
}

fn check_versioned_command_requirement(
    id: &str,
    label: &str,
    program: &str,
    args: &[&str],
    install_command: Option<String>,
) -> RequirementCheck {
    let probe = run_command_probe(program, args);
    requirement_from_probe(
        id,
        label,
        probe,
        &format!("{label} is available."),
        &format!("{label} is not installed."),
        install_command,
    )
}

async fn check_war_thunder_api_requirement(
    base_url: &str,
    runtime_connected: Option<bool>,
) -> RequirementCheck {
    let url = format!("{}/state", base_url.trim_end_matches('/'));
    if runtime_connected == Some(true) {
        return RequirementCheck::ok(
            "war_thunder_api",
            "War Thunder API",
            "War Thunder local API is reachable.",
            Some("diagnostics.summary.warThunderReachable"),
            Some("HTTP 200 OK (runtime connected)".to_owned()),
            None,
            None,
        );
    }

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return RequirementCheck::unknown(
                "war_thunder_api",
                "War Thunder API",
                "Waiting for War Thunder. This is normal if the game is closed.",
                Some("diagnostics.summary.waitingForWarThunder"),
                Some(format!("Could not build HTTP client: {error}")),
            )
        }
    };

    match client.get(&url).send().await {
        Ok(response) if response.status().is_success() => RequirementCheck::ok(
            "war_thunder_api",
            "War Thunder API",
            "War Thunder local API is reachable.",
            Some("diagnostics.summary.warThunderReachable"),
            Some(format!("GET {url} returned HTTP {}", response.status())),
            None,
            None,
        ),
        Ok(response) => RequirementCheck::warning(
            "war_thunder_api",
            "War Thunder API",
            "Waiting for War Thunder. This is normal if the game is closed.",
            Some("diagnostics.summary.waitingForWarThunder"),
            Some(format!("GET {url} returned HTTP {}", response.status())),
            None,
        ),
        Err(error) => RequirementCheck::warning(
            "war_thunder_api",
            "War Thunder API",
            "Waiting for War Thunder. This is normal if the game is closed.",
            Some("diagnostics.summary.waitingForWarThunder"),
            Some(format!("GET {url} failed: {error}")),
            None,
        ),
    }
}

fn check_directory_requirement(
    id: &str,
    label: &str,
    dir: anyhow::Result<PathBuf>,
) -> RequirementCheck {
    let dir = match dir {
        Ok(dir) => dir,
        Err(error) => {
            return RequirementCheck::error(
                id,
                label,
                "Directory path could not be resolved.",
                Some("diagnostics.summary.directoryNotWritable"),
                Some(error.to_string()),
                None,
            )
        }
    };
    match check_writable_directory_path(&dir) {
        DirectoryWritableStatus::Writable(path) => RequirementCheck::ok(
            id,
            label,
            "Directory exists and is writable.",
            Some("diagnostics.summary.directoryWritable"),
            None,
            None,
            Some(path.display().to_string()),
        ),
        DirectoryWritableStatus::MissingParentWritable(path) => RequirementCheck::warning(
            id,
            label,
            "Directory does not exist yet, but the parent is writable.",
            Some("diagnostics.summary.directoryParentWritable"),
            Some(path.display().to_string()),
            None,
        ),
        DirectoryWritableStatus::NotWritable(path, error) => RequirementCheck::error(
            id,
            label,
            "Directory is not writable.",
            Some("diagnostics.summary.directoryNotWritable"),
            Some(format!("{} ({error})", path.display())),
            None,
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DirectoryWritableStatus {
    Writable(PathBuf),
    MissingParentWritable(PathBuf),
    NotWritable(PathBuf, String),
}

fn check_writable_directory_path(dir: &Path) -> DirectoryWritableStatus {
    let absolute = absolute_path(dir);
    if absolute.exists() {
        return match write_probe_file(&absolute) {
            Ok(()) => DirectoryWritableStatus::Writable(absolute),
            Err(error) => DirectoryWritableStatus::NotWritable(absolute, error.to_string()),
        };
    }

    let parent = absolute.parent().map(Path::to_path_buf);
    match parent {
        Some(parent) if parent.exists() => match write_probe_file(&parent) {
            Ok(()) => DirectoryWritableStatus::MissingParentWritable(absolute),
            Err(error) => DirectoryWritableStatus::NotWritable(absolute, error.to_string()),
        },
        _ => DirectoryWritableStatus::NotWritable(
            absolute,
            "parent directory does not exist".to_owned(),
        ),
    }
}

fn write_probe_file(dir: &Path) -> io::Result<()> {
    let probe = dir.join(".wt-clip-write-test");
    fs::write(&probe, b"wt-clip")?;
    fs::remove_file(&probe)?;
    Ok(())
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn run_command_probe(program: &str, args: &[&str]) -> CommandProbe {
    match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = combined_output(&stdout, &stderr);
            let version = first_useful_line(&stdout).or_else(|| first_useful_line(&stderr));
            CommandProbe {
                status: RequirementStatus::Ok,
                summary: "available".to_owned(),
                details: version.clone(),
                version,
                path: command_path(program),
                output: Some(combined),
            }
        }
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = combined_output(&stdout, &stderr);
            CommandProbe {
                status: RequirementStatus::Error,
                summary: format!("command exited with status {}", output.status),
                details: first_useful_line(&stderr)
                    .or_else(|| first_useful_line(&stdout))
                    .or_else(|| Some(format!("command exited with status {}", output.status))),
                version: first_useful_line(&stdout),
                path: command_path(program),
                output: Some(combined),
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => CommandProbe {
            status: RequirementStatus::Missing,
            summary: format!("{program} not found"),
            details: Some(error.to_string()),
            version: None,
            path: None,
            output: None,
        },
        Err(error) => CommandProbe {
            status: RequirementStatus::Error,
            summary: format!("could not run {program}"),
            details: Some(error.to_string()),
            version: None,
            path: None,
            output: None,
        },
    }
}

fn requirement_from_probe(
    id: &str,
    label: &str,
    probe: CommandProbe,
    ok_summary: &str,
    missing_summary: &str,
    install_command: Option<String>,
) -> RequirementCheck {
    match probe.status {
        RequirementStatus::Ok => RequirementCheck::ok(
            id,
            label,
            ok_summary,
            default_summary_key(id, RequirementStatus::Ok),
            probe.details,
            None,
            probe.path,
        )
        .with_version(probe.version),
        RequirementStatus::Missing => RequirementCheck::missing(
            id,
            label,
            missing_summary,
            default_summary_key(id, RequirementStatus::Missing),
            probe.details,
            install_command,
        ),
        _ => RequirementCheck::error(
            id,
            label,
            &probe.summary,
            default_summary_key(id, RequirementStatus::Error),
            probe.details,
            install_command,
        ),
    }
}

fn combined_output(stdout: &str, stderr: &str) -> String {
    [stdout.trim(), stderr.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn gsr_flatpak_binary_reachable(probe: &CommandProbe) -> bool {
    if probe.status == RequirementStatus::Ok {
        return true;
    }
    let output = probe
        .output
        .as_deref()
        .or(probe.details.as_deref())
        .unwrap_or_default()
        .to_ascii_lowercase();
    output.contains("usage:")
        || output.contains("gpu-screen-recorder")
        || output.contains("-w <window_id|monitor|focused|portal|region|v4l2_device_path>")
}

fn flatpak_gsr_info_is_installed(probe: &CommandProbe) -> bool {
    probe.status == RequirementStatus::Ok
}

fn command_path(program: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .map(|path| path.display().to_string())
}

fn first_useful_line(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(240).collect())
}

fn default_summary_key(id: &str, status: RequirementStatus) -> Option<&'static str> {
    match (id, status) {
        ("flatpak", RequirementStatus::Ok) => Some("diagnostics.summary.flatpakAvailable"),
        ("flatpak", RequirementStatus::Missing) => Some("diagnostics.summary.flatpakMissing"),
        ("gsr_native", RequirementStatus::Ok) => Some("diagnostics.summary.gsrNativeAvailable"),
        ("gsr_native", RequirementStatus::Missing) => Some("diagnostics.summary.gsrNativeMissing"),
        ("ffmpeg", RequirementStatus::Ok) => Some("diagnostics.summary.ffmpegAvailable"),
        ("ffmpeg", RequirementStatus::Missing) => Some("diagnostics.summary.ffmpegMissing"),
        ("ffprobe", RequirementStatus::Ok) => Some("diagnostics.summary.ffprobeAvailable"),
        ("ffprobe", RequirementStatus::Missing) => Some("diagnostics.summary.ffprobeMissing"),
        ("output_dir", RequirementStatus::Ok) | ("config_dir", RequirementStatus::Ok) => {
            Some("diagnostics.summary.directoryWritable")
        }
        _ => None,
    }
}

fn apply_gsr_mode_warning(
    mode: &GpuScreenRecorderMode,
    gsr_flatpak: &mut RequirementCheck,
    gsr_native: &mut RequirementCheck,
) {
    let flatpak_ok = gsr_flatpak.status == RequirementStatus::Ok;
    let native_ok = gsr_native.status == RequirementStatus::Ok;
    match mode {
        GpuScreenRecorderMode::Flatpak if !flatpak_ok && native_ok => {
            gsr_flatpak.status = RequirementStatus::Warning;
            gsr_flatpak.summary =
                "GSR native is installed, but WT Clip is configured to use Flatpak.".to_owned();
            gsr_flatpak.summary_key =
                Some("diagnostics.summary.gsrNativeInstalledButFlatpakConfigured".to_owned());
        }
        GpuScreenRecorderMode::Native if !native_ok && flatpak_ok => {
            gsr_native.status = RequirementStatus::Warning;
            gsr_native.summary =
                "GSR Flatpak is installed, but WT Clip is configured to use native.".to_owned();
            gsr_native.summary_key =
                Some("diagnostics.summary.gsrFlatpakInstalledButNativeConfigured".to_owned());
        }
        GpuScreenRecorderMode::Flatpak | GpuScreenRecorderMode::Native
            if gsr_flatpak.status == RequirementStatus::Missing
                && gsr_native.status == RequirementStatus::Missing =>
        {
            let summary = "GPU Screen Recorder is missing.".to_owned();
            gsr_flatpak.status = RequirementStatus::Error;
            gsr_native.status = RequirementStatus::Error;
            gsr_flatpak.summary = summary.clone();
            gsr_native.summary = summary;
            gsr_flatpak.summary_key =
                Some("diagnostics.summary.gpuScreenRecorderMissing".to_owned());
            gsr_native.summary_key =
                Some("diagnostics.summary.gpuScreenRecorderMissing".to_owned());
        }
        _ => {}
    }
}

fn install_commands() -> InstallCommands {
    InstallCommands {
        install_gsr_flatpak: "flatpak install flathub com.dec05eba.gpu_screen_recorder".to_owned(),
        install_ffmpeg_apt: "sudo apt install ffmpeg".to_owned(),
        install_ffmpeg_pacman: "sudo pacman -S ffmpeg".to_owned(),
        install_ffmpeg_dnf: "sudo dnf install ffmpeg".to_owned(),
        install_flatpak_apt: "sudo apt install flatpak".to_owned(),
    }
}

fn requirement_status_label(status: &RequirementStatus) -> &'static str {
    match status {
        RequirementStatus::Ok => "OK",
        RequirementStatus::Warning => "Warning",
        RequirementStatus::Error => "Error",
        RequirementStatus::Missing => "Missing",
        RequirementStatus::Unknown => "Unknown",
    }
}

fn diagnostic_logs_with_command(
    mut recent_logs: Vec<String>,
    gsr_command_line: Option<String>,
) -> Vec<String> {
    if let Some(command) = gsr_command_line.filter(|line| !line.trim().is_empty()) {
        recent_logs.push(format!("[GPU_RECORDER] command: {command}"));
    }
    if recent_logs.is_empty() {
        recent_logs.push("No persistent log file configured yet.".to_owned());
    }
    recent_logs
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

impl RequirementCheck {
    fn ok(
        id: impl Into<String>,
        label: impl Into<String>,
        summary: impl Into<String>,
        summary_key: Option<impl Into<String>>,
        details: Option<String>,
        command: Option<String>,
        path: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status: RequirementStatus::Ok,
            summary: summary.into(),
            summary_key: summary_key.map(Into::into),
            details,
            command,
            version: None,
            path,
        }
    }

    fn warning(
        id: impl Into<String>,
        label: impl Into<String>,
        summary: impl Into<String>,
        summary_key: Option<impl Into<String>>,
        details: Option<impl Into<String>>,
        command: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status: RequirementStatus::Warning,
            summary: summary.into(),
            summary_key: summary_key.map(Into::into),
            details: details.map(Into::into),
            command,
            version: None,
            path: None,
        }
    }

    fn error(
        id: impl Into<String>,
        label: impl Into<String>,
        summary: impl Into<String>,
        summary_key: Option<impl Into<String>>,
        details: Option<impl Into<String>>,
        command: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status: RequirementStatus::Error,
            summary: summary.into(),
            summary_key: summary_key.map(Into::into),
            details: details.map(Into::into),
            command,
            version: None,
            path: None,
        }
    }

    fn missing(
        id: impl Into<String>,
        label: impl Into<String>,
        summary: impl Into<String>,
        summary_key: Option<impl Into<String>>,
        details: Option<impl Into<String>>,
        command: Option<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status: RequirementStatus::Missing,
            summary: summary.into(),
            summary_key: summary_key.map(Into::into),
            details: details.map(Into::into),
            command,
            version: None,
            path: None,
        }
    }

    fn unknown(
        id: impl Into<String>,
        label: impl Into<String>,
        summary: impl Into<String>,
        summary_key: Option<impl Into<String>>,
        details: Option<impl Into<String>>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            status: RequirementStatus::Unknown,
            summary: summary.into(),
            summary_key: summary_key.map(Into::into),
            details: details.map(Into::into),
            command: None,
            version: None,
            path: None,
        }
    }

    fn with_version(mut self, version: Option<String>) -> Self {
        self.version = version;
        self
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
    fn parses_first_useful_command_output_line() {
        assert_eq!(
            first_useful_line("\nffmpeg version 6.1 Copyright\nmore"),
            Some("ffmpeg version 6.1 Copyright".to_owned())
        );
    }

    #[test]
    fn maps_missing_probe_to_missing_requirement() {
        let check = requirement_from_probe(
            "ffmpeg",
            "FFmpeg",
            CommandProbe {
                status: RequirementStatus::Missing,
                summary: "ffmpeg not found".to_owned(),
                details: Some("not found".to_owned()),
                version: None,
                path: None,
                output: None,
            },
            "FFmpeg is available.",
            "FFmpeg is not installed.",
            Some("sudo apt install ffmpeg".to_owned()),
        );
        assert_eq!(check.status, RequirementStatus::Missing);
        assert_eq!(check.command.as_deref(), Some("sudo apt install ffmpeg"));
    }

    #[test]
    fn flatpak_mode_warns_when_only_native_gsr_is_installed() {
        let mut flatpak = RequirementCheck::missing(
            "gsr_flatpak",
            "GPU Screen Recorder Flatpak",
            "missing",
            None::<String>,
            None::<String>,
            None,
        );
        let mut native = RequirementCheck::ok(
            "gsr_native",
            "GPU Screen Recorder Native",
            "ok",
            None::<String>,
            None,
            None,
            Some("/usr/bin/gpu-screen-recorder".to_owned()),
        );

        apply_gsr_mode_warning(&GpuScreenRecorderMode::Flatpak, &mut flatpak, &mut native);

        assert_eq!(flatpak.status, RequirementStatus::Warning);
        assert!(flatpak.summary.contains("configured to use Flatpak"));
        assert_eq!(native.status, RequirementStatus::Ok);
    }

    #[test]
    fn native_mode_warns_when_only_flatpak_gsr_is_installed() {
        let mut flatpak = RequirementCheck::ok(
            "gsr_flatpak",
            "GPU Screen Recorder Flatpak",
            "ok",
            None::<String>,
            None,
            None,
            None,
        );
        let mut native = RequirementCheck::missing(
            "gsr_native",
            "GPU Screen Recorder Native",
            "missing",
            None::<String>,
            None::<String>,
            None,
        );

        apply_gsr_mode_warning(&GpuScreenRecorderMode::Native, &mut flatpak, &mut native);

        assert_eq!(native.status, RequirementStatus::Warning);
        assert!(native.summary.contains("configured to use native"));
        assert_eq!(flatpak.status, RequirementStatus::Ok);
    }

    #[test]
    fn missing_both_gsr_modes_is_error() {
        let mut flatpak = RequirementCheck::missing(
            "gsr_flatpak",
            "GPU Screen Recorder Flatpak",
            "missing",
            None::<String>,
            None::<String>,
            None,
        );
        let mut native = RequirementCheck::missing(
            "gsr_native",
            "GPU Screen Recorder Native",
            "missing",
            None::<String>,
            None::<String>,
            None,
        );

        apply_gsr_mode_warning(&GpuScreenRecorderMode::Flatpak, &mut flatpak, &mut native);

        assert_eq!(flatpak.status, RequirementStatus::Error);
        assert_eq!(native.status, RequirementStatus::Error);
        assert_eq!(flatpak.summary, "GPU Screen Recorder is missing.");
    }

    #[test]
    fn flatpak_gsr_usage_output_counts_as_available() {
        let probe = CommandProbe {
            status: RequirementStatus::Error,
            summary: "command exited with status 1".to_owned(),
            details: Some(
                "usage: gpu-screen-recorder -w <window_id|monitor|focused|portal|region|v4l2_device_path>"
                    .to_owned(),
            ),
            version: None,
            path: None,
            output: Some(
                "usage: gpu-screen-recorder -w <window_id|monitor|focused|portal|region|v4l2_device_path>"
                    .to_owned(),
            ),
        };

        assert!(gsr_flatpak_binary_reachable(&probe));
    }

    #[test]
    fn flatpak_gsr_info_success_counts_as_installed() {
        let info = CommandProbe {
            status: RequirementStatus::Ok,
            summary: "available".to_owned(),
            details: Some("Name: com.dec05eba.gpu_screen_recorder".to_owned()),
            version: None,
            path: None,
            output: Some("Name: com.dec05eba.gpu_screen_recorder".to_owned()),
        };

        assert!(flatpak_gsr_info_is_installed(&info));
    }

    #[test]
    fn flatpak_mode_does_not_report_missing_when_flatpak_gsr_is_installed() {
        let mut flatpak = RequirementCheck::warning(
            "gsr_flatpak",
            "GPU Screen Recorder Flatpak",
            "GSR Flatpak is installed, but the recorder command could not be verified.",
            Some("diagnostics.summary.gsrFlatpakCommandFailed"),
            None::<String>,
            None,
        );
        let mut native = RequirementCheck::missing(
            "gsr_native",
            "GPU Screen Recorder Native",
            "missing",
            None::<String>,
            None::<String>,
            None,
        );

        apply_gsr_mode_warning(&GpuScreenRecorderMode::Flatpak, &mut flatpak, &mut native);

        assert_ne!(flatpak.status, RequirementStatus::Error);
        assert_ne!(flatpak.summary, "GPU Screen Recorder is missing.");
    }

    #[test]
    fn war_thunder_closed_is_warning_not_error() {
        let check = RequirementCheck::warning(
            "war_thunder_api",
            "War Thunder API",
            "Waiting for War Thunder. This is normal if the game is closed.",
            Some("diagnostics.summary.waitingForWarThunder"),
            Some("GET http://127.0.0.1:8111/state failed"),
            None,
        );

        assert_eq!(check.status, RequirementStatus::Warning);
    }

    #[test]
    fn diagnostics_report_uses_current_state() {
        let mut report = sample_requirements_report();
        report.war_thunder_api = RequirementCheck::ok(
            "war_thunder_api",
            "War Thunder API",
            "War Thunder local API is reachable.",
            Some("diagnostics.summary.warThunderReachable"),
            Some("HTTP 200 OK (runtime connected)".to_owned()),
            None,
            None,
        );

        let text = format_system_requirements_report(&report);

        assert!(text.contains("War Thunder API: OK"));
        assert!(text.contains("HTTP 200 OK"));
    }

    #[test]
    fn diagnostics_report_contains_capture_mode_and_strategy() {
        let report = sample_requirements_report();
        let text = format_system_requirements_report(&report);
        assert!(text.contains("Capture mode: flatpak"));
        assert!(text.contains("Capture strategy: auto"));
    }

    #[test]
    fn diagnostics_report_contains_ffmpeg_and_ffprobe() {
        let report = sample_requirements_report();
        let text = format_system_requirements_report(&report);
        assert!(text.contains("FFmpeg: OK"));
        assert!(text.contains("FFprobe: OK"));
    }

    #[test]
    fn diagnostics_report_does_not_include_sensitive_env() {
        let report = sample_requirements_report();
        let text = format_system_requirements_report(&report);
        assert!(!text.contains("GITHUB_TOKEN"));
        assert!(!text.contains("WT_CLIPPER_UPDATE"));
        assert!(!text.contains("SECRET"));
    }

    #[test]
    fn output_dir_writable_check_with_tempdir() {
        let dir =
            std::env::temp_dir().join(format!("wt-clipper-doctor-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let result = check_writable_directory_path(&dir);
        let _ = fs::remove_dir_all(&dir);
        assert!(matches!(result, DirectoryWritableStatus::Writable(_)));
    }

    #[test]
    fn install_commands_non_empty() {
        let commands = install_commands();
        assert!(!commands.install_gsr_flatpak.is_empty());
        assert!(!commands.install_ffmpeg_apt.is_empty());
        assert!(!commands.install_ffmpeg_pacman.is_empty());
        assert!(!commands.install_ffmpeg_dnf.is_empty());
        assert!(!commands.install_flatpak_apt.is_empty());
    }

    fn sample_requirements_report() -> SystemRequirementsReport {
        let ok = |id: &str, label: &str| {
            RequirementCheck::ok(
                id,
                label,
                "available",
                None::<String>,
                None,
                None,
                Some("/usr/bin/tool".to_owned()),
            )
        };
        SystemRequirementsReport {
            app_version: "0.2.3".to_owned(),
            os: Some("linux".to_owned()),
            session_type: Some("wayland".to_owned()),
            war_thunder_api: RequirementCheck::warning(
                "war_thunder_api",
                "War Thunder API",
                "Waiting for War Thunder. This is normal if the game is closed.",
                Some("diagnostics.summary.waitingForWarThunder"),
                None::<String>,
                None,
            ),
            flatpak: ok("flatpak", "Flatpak"),
            gsr_flatpak: ok("gsr_flatpak", "GPU Screen Recorder Flatpak"),
            gsr_native: RequirementCheck::missing(
                "gsr_native",
                "GPU Screen Recorder Native",
                "missing",
                None::<String>,
                None::<String>,
                None,
            ),
            ffmpeg: ok("ffmpeg", "FFmpeg"),
            ffprobe: ok("ffprobe", "FFprobe"),
            capture_mode: "flatpak".to_owned(),
            capture_strategy: "auto".to_owned(),
            configured_target: "eDP".to_owned(),
            effective_target: Some("portal".to_owned()),
            target_reason: Some("wayland auto".to_owned()),
            output_dir: ok("output_dir", "Output directory"),
            config_dir: ok("config_dir", "Config directory"),
            install_commands: install_commands(),
            logs: DiagnosticLogsSummary {
                recent_logs: vec!["No persistent log file configured yet.".to_owned()],
                logs_path: None,
                can_copy_logs: true,
            },
        }
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
