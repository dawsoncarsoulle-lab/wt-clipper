use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::Context;
use chrono::Utc;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use tauri::Emitter;
use tracing::{debug, info};
use uuid::Uuid;
use wt_clipper::config::AppConfig;

const FFMPEG_REQUIRED_MESSAGE: &str =
    "FFmpeg est requis pour l'editeur video. Installe-le avec sudo apt install ffmpeg.";
const SOCIAL_WIDTH: u32 = 1080;
const SOCIAL_HEIGHT: u32 = 1920;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClipEditorMode {
    TrimOriginal,
    YoutubeHorizontal,
    SocialVertical,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SocialLayout {
    VerticalBlur,
    VerticalCrop,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Webm,
    Mp4,
}

impl OutputFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Webm => "webm",
            Self::Mp4 => "mp4",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SaveMode {
    CreateCopy,
    ReplaceOriginal,
}

impl Default for SaveMode {
    fn default() -> Self {
        Self::CreateCopy
    }
}

impl<'de> Deserialize<'de> for SaveMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Option::<String>::deserialize(deserializer)?;
        Ok(match value.as_deref() {
            Some("replace_original") => Self::ReplaceOriginal,
            _ => Self::CreateCopy,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipEditRequest {
    pub clip_path: String,
    pub metadata_path: Option<String>,
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub mode: ClipEditorMode,
    pub output_format: OutputFormat,
    pub layout: Option<SocialLayout>,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub watermark: bool,
    pub fps: u32,
    pub bitrate_kbps: u32,
    #[serde(default)]
    pub save_mode: SaveMode,
    #[serde(default = "default_backup_original")]
    pub backup_original: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditedClipResult {
    pub output_path: String,
    pub metadata_path: Option<String>,
    pub thumbnail_path: Option<String>,
    pub duration_seconds: f64,
    pub size_bytes: u64,
    pub save_mode: SaveMode,
    pub replaced_original: bool,
    pub backup_path: Option<String>,
    pub backup_metadata_path: Option<String>,
    pub backup_thumbnail_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipMediaInfo {
    pub duration_seconds: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub codec: String,
    pub container: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EditorExportProgressStep {
    Preparing,
    Trimming,
    Encoding,
    Thumbnail,
    Metadata,
    Saving,
    Done,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorExportProgressPayload {
    pub id: String,
    pub export_id: String,
    pub phase: String,
    pub stage: String,
    pub active: bool,
    pub step: EditorExportProgressStep,
    pub progress: u8,
    pub message: String,
    pub current_time: Option<f64>,
    pub duration: Option<f64>,
    pub output_path: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
struct EditedClipMetadata {
    created_by: &'static str,
    kind: &'static str,
    export_type: &'static str,
    source_video_path: String,
    output_video_path: String,
    mode: ClipEditorMode,
    layout: Option<SocialLayout>,
    start_seconds: f64,
    end_seconds: f64,
    duration_seconds: f64,
    title: Option<String>,
    subtitle: Option<String>,
    watermark: bool,
    fps: u32,
    bitrate_kbps: u32,
    created_at: String,
    reason: Option<String>,
    clip_type: Option<String>,
    source_reason: Option<String>,
    source_clip_type: Option<String>,
    thumbnail_path: Option<String>,
}

#[derive(Debug, Clone)]
struct BackupPaths {
    video_path: PathBuf,
    metadata_path: Option<PathBuf>,
    thumbnail_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct ReplacementPaths {
    original_video_path: PathBuf,
    original_metadata_path: PathBuf,
    original_thumbnail_path: PathBuf,
    target_video_path: PathBuf,
    target_metadata_path: PathBuf,
    target_thumbnail_path: PathBuf,
    tmp_video_path: PathBuf,
    tmp_metadata_path: PathBuf,
    tmp_thumbnail_path: PathBuf,
    backup_dir: PathBuf,
}

#[derive(Debug, Deserialize)]
struct SourceClipMetadata {
    reason: Option<String>,
    clip_type: Option<String>,
}

#[derive(Debug, Clone)]
struct SourceClipDetails {
    reason: Option<String>,
    clip_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FfprobeOutput {
    streams: Vec<FfprobeStream>,
    format: Option<FfprobeFormat>,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    duration: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FfprobeFormat {
    format_name: Option<String>,
    duration: Option<String>,
    size: Option<String>,
}

pub async fn export_edited_clip(
    request: ClipEditRequest,
    config: AppConfig,
    app: tauri::AppHandle,
) -> Result<EditedClipResult, String> {
    tokio::task::spawn_blocking(move || export_edited_clip_blocking(request, config, app))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

pub async fn get_clip_media_info(path: String) -> Result<ClipMediaInfo, String> {
    tokio::task::spawn_blocking(move || probe_media_info(Path::new(&path)))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

pub fn open_path(path: String) -> Result<(), String> {
    open_with_xdg(PathBuf::from(path))
}

pub fn open_parent_folder(path: String) -> Result<(), String> {
    let path = PathBuf::from(path);
    let parent = path
        .parent()
        .ok_or_else(|| "Dossier parent introuvable.".to_owned())?;
    open_with_xdg(parent.to_path_buf())
}

fn export_edited_clip_blocking(
    request: ClipEditRequest,
    config: AppConfig,
    app: tauri::AppHandle,
) -> anyhow::Result<EditedClipResult> {
    emit_editor_progress(
        &app,
        EditorExportProgressStep::Preparing,
        5,
        "Preparation de l'export...",
        true,
        None,
        None,
    );

    let result = export_edited_clip_inner(&request, &config, &app);
    match result {
        Ok(result) => {
            let message = if result.replaced_original {
                "Clip original remplace"
            } else {
                "Export termine"
            };
            emit_editor_progress(
                &app,
                EditorExportProgressStep::Done,
                100,
                message,
                false,
                Some(result.output_path.clone()),
                None,
            );
            Ok(result)
        }
        Err(error) => {
            let message = error.to_string();
            emit_editor_progress(
                &app,
                EditorExportProgressStep::Failed,
                100,
                "Erreur pendant l'export",
                false,
                None,
                Some(message.clone()),
            );
            Err(error)
        }
    }
}

fn export_edited_clip_inner(
    request: &ClipEditRequest,
    config: &AppConfig,
    app: &tauri::AppHandle,
) -> anyhow::Result<EditedClipResult> {
    ensure_command_available("ffmpeg")?;
    ensure_command_available("ffprobe")?;

    let input_path = PathBuf::from(&request.clip_path);
    validate_input_path(&input_path)?;

    emit_editor_progress(
        app,
        EditorExportProgressStep::Preparing,
        10,
        "Analyse du clip...",
        true,
        None,
        None,
    );
    let media_info = probe_media_info(&input_path)?;
    validate_trim_request(
        request.start_seconds,
        request.end_seconds,
        media_info.duration_seconds,
    )?;

    match request.save_mode {
        SaveMode::CreateCopy => export_edited_copy(request, config, &input_path, app),
        SaveMode::ReplaceOriginal => replace_original_clip_safely(request, config, &input_path, app),
    }
}

fn export_edited_copy(
    request: &ClipEditRequest,
    config: &AppConfig,
    input_path: &Path,
    app: &tauri::AppHandle,
) -> anyhow::Result<EditedClipResult> {
    let output_base_dir = output_base_dir(config, request.mode)?;
    let output_path = resolve_output_path(request, input_path, &output_base_dir)?;
    ensure_not_original(input_path, &output_path)?;
    fs::create_dir_all(&output_base_dir)
        .with_context(|| format!("Impossible de creer {}", output_base_dir.display()))?;

    let tmp_path = temporary_output_path(&output_path);
    remove_if_exists(&tmp_path)?;
    let metadata_path = output_path.with_extension("json");
    let thumbnail_path = output_path.with_extension("jpg");

    emit_editor_progress(
        app,
        EditorExportProgressStep::Trimming,
        15,
        "Application du trim...",
        true,
        None,
        None,
    );

    let export_result = run_video_export(request, input_path, &tmp_path, app);
    if let Err(error) = export_result {
        cleanup_export_failure(&tmp_path, &metadata_path, &thumbnail_path);
        return Err(error);
    }
    validate_non_empty_file(&tmp_path, "Fichier export temporaire invalide")?;

    emit_editor_progress(
        app,
        EditorExportProgressStep::Saving,
        82,
        "Sauvegarde du clip exporte...",
        true,
        Some(output_path.display().to_string()),
        None,
    );
    if output_path.exists() {
        cleanup_export_failure(&tmp_path, &metadata_path, &thumbnail_path);
        anyhow::bail!("Le fichier de sortie existe deja: {}", output_path.display());
    }
    if let Err(error) = fs::rename(&tmp_path, &output_path).with_context(|| {
        format!(
            "Impossible de finaliser l'export {}",
            output_path.display()
        )
    }) {
        cleanup_export_failure(&tmp_path, &metadata_path, &thumbnail_path);
        return Err(error);
    }

    emit_editor_progress(
        app,
        EditorExportProgressStep::Thumbnail,
        88,
        "Generation de la miniature...",
        true,
        None,
        None,
    );
    let output_media_info = probe_media_info(&output_path)?;
    let thumbnail = match generate_thumbnail(
        &output_path,
        &thumbnail_path,
        output_media_info.duration_seconds,
    ) {
        Ok(path) => Some(path),
        Err(error) => {
            debug!(%error, path = %output_path.display(), "failed to generate edited clip thumbnail");
            None
        }
    };

    emit_editor_progress(
        app,
        EditorExportProgressStep::Metadata,
        94,
        "Ecriture des metadata...",
        true,
        None,
        None,
    );
    let source_details = read_source_clip_details(request, input_path);
    write_edited_metadata(
        &metadata_path,
        request,
        input_path,
        &output_path,
        source_details,
        thumbnail.as_deref(),
        output_media_info.duration_seconds,
    )?;

    let size_bytes = fs::metadata(&output_path)
        .with_context(|| format!("Impossible de lire {}", output_path.display()))?
        .len();

    Ok(EditedClipResult {
        output_path: output_path.display().to_string(),
        metadata_path: Some(metadata_path.display().to_string()),
        thumbnail_path: thumbnail.map(|path| path.display().to_string()),
        duration_seconds: output_media_info.duration_seconds,
        size_bytes,
        save_mode: SaveMode::CreateCopy,
        replaced_original: false,
        backup_path: None,
        backup_metadata_path: None,
        backup_thumbnail_path: None,
    })
}

fn replace_original_clip_safely(
    request: &ClipEditRequest,
    config: &AppConfig,
    input_path: &Path,
    app: &tauri::AppHandle,
) -> anyhow::Result<EditedClipResult> {
    let paths = replacement_paths(config, input_path, request)?;
    fs::create_dir_all(&paths.backup_dir)
        .with_context(|| format!("Impossible de creer {}", paths.backup_dir.display()))?;
    if !request.backup_original {
        debug!("backup_original=false ignored for replace_original; backup remains mandatory");
    }

    let backup = backup_original_files(&paths)?;

    emit_editor_progress(
        app,
        EditorExportProgressStep::Trimming,
        15,
        "Application du trim...",
        true,
        None,
        None,
    );

    let export_result = run_video_export(request, input_path, &paths.tmp_video_path, app);
    if let Err(error) = export_result {
        cleanup_replacement_temps(&paths);
        return Err(error);
    }
    if let Err(error) = validate_replacement_temp_video(&paths.tmp_video_path) {
        cleanup_replacement_temps(&paths);
        return Err(error);
    }

    emit_editor_progress(
        app,
        EditorExportProgressStep::Thumbnail,
        80,
        "Generation de la miniature...",
        true,
        None,
        None,
    );
    let replacement_media_info = probe_media_info(&paths.tmp_video_path)?;
    if let Err(error) = generate_thumbnail(
        &paths.tmp_video_path,
        &paths.tmp_thumbnail_path,
        replacement_media_info.duration_seconds,
    ) {
        cleanup_replacement_temps(&paths);
        return Err(error);
    }

    emit_editor_progress(
        app,
        EditorExportProgressStep::Metadata,
        90,
        "Mise a jour des metadata...",
        true,
        None,
        None,
    );
    if let Err(error) = write_replacement_metadata(&paths.tmp_metadata_path, request, &paths, &backup)
    {
        cleanup_replacement_temps(&paths);
        return Err(error);
    }

    emit_editor_progress(
        app,
        EditorExportProgressStep::Saving,
        95,
        "Remplacement securise du clip original...",
        true,
        Some(paths.target_video_path.display().to_string()),
        None,
    );
    commit_replacement_files(&paths, &backup)?;

    let size_bytes = fs::metadata(&paths.target_video_path)
        .with_context(|| format!("Impossible de lire {}", paths.target_video_path.display()))?
        .len();

    Ok(EditedClipResult {
        output_path: paths.target_video_path.display().to_string(),
        metadata_path: Some(paths.target_metadata_path.display().to_string()),
        thumbnail_path: Some(paths.target_thumbnail_path.display().to_string()),
        duration_seconds: edited_duration(request),
        size_bytes,
        save_mode: SaveMode::ReplaceOriginal,
        replaced_original: true,
        backup_path: Some(backup.video_path.display().to_string()),
        backup_metadata_path: backup
            .metadata_path
            .as_ref()
            .map(|path| path.display().to_string()),
        backup_thumbnail_path: backup
            .thumbnail_path
            .as_ref()
            .map(|path| path.display().to_string()),
    })
}

fn run_video_export(
    request: &ClipEditRequest,
    input_path: &Path,
    tmp_path: &Path,
    app: &tauri::AppHandle,
) -> anyhow::Result<()> {
    match request.mode {
        ClipEditorMode::TrimOriginal if effective_output_format(request) == OutputFormat::Webm => {
            match run_trim_copy(request, input_path, tmp_path) {
                Ok(()) => Ok(()),
                Err(copy_error) => {
                    debug!(%copy_error, "fast WebM trim failed, retrying with re-encode");
                    emit_editor_progress(
                        app,
                        EditorExportProgressStep::Encoding,
                        60,
                        "Trim precis avec reencodage...",
                        true,
                        None,
                        None,
                    );
                    remove_if_exists(tmp_path)?;
                    run_horizontal_encode(request, input_path, tmp_path, OutputFormat::Webm)
                }
            }
        }
        ClipEditorMode::TrimOriginal => {
            emit_editor_progress(
                app,
                EditorExportProgressStep::Encoding,
                60,
                "Encodage du clip coupe...",
                true,
                None,
                None,
            );
            run_horizontal_encode(request, input_path, tmp_path, effective_output_format(request))
        }
        ClipEditorMode::YoutubeHorizontal => {
            emit_editor_progress(
                app,
                EditorExportProgressStep::Encoding,
                60,
                "Encodage YouTube horizontal...",
                true,
                None,
                None,
            );
            run_horizontal_encode(request, input_path, tmp_path, OutputFormat::Mp4)
        }
        ClipEditorMode::SocialVertical => {
            emit_editor_progress(
                app,
                EditorExportProgressStep::Encoding,
                60,
                "Encodage vertical social...",
                true,
                None,
                None,
            );
            run_social_encode(request, input_path, tmp_path, app)
        }
    }
}

fn run_trim_copy(request: &ClipEditRequest, input_path: &Path, tmp_path: &Path) -> anyhow::Result<()> {
    let mut command = Command::new("ffmpeg");
    command
        .args(["-y", "-hide_banner", "-loglevel", "error"])
        .args(["-ss", &format_seconds_arg(request.start_seconds)])
        .args(["-to", &format_seconds_arg(request.end_seconds)])
        .arg("-i")
        .arg(input_path)
        .args(["-map", "0:v:0", "-map", "0:a?", "-c", "copy"])
        .args(["-avoid_negative_ts", "make_zero"])
        .arg(tmp_path);
    run_command(command, "Trim rapide FFmpeg")
}

fn run_horizontal_encode(
    request: &ClipEditRequest,
    input_path: &Path,
    tmp_path: &Path,
    format: OutputFormat,
) -> anyhow::Result<()> {
    let mut command = Command::new("ffmpeg");
    command
        .args(["-y", "-hide_banner", "-loglevel", "error"])
        .args(["-ss", &format_seconds_arg(request.start_seconds)])
        .args(["-to", &format_seconds_arg(request.end_seconds)])
        .arg("-i")
        .arg(input_path)
        .args(["-map", "0:v:0", "-map", "0:a?"]);
    append_video_codec_args(&mut command, request, format);
    command.arg(tmp_path);
    run_command(command, "Encodage FFmpeg")
}

fn run_social_encode(
    request: &ClipEditRequest,
    input_path: &Path,
    tmp_path: &Path,
    app: &tauri::AppHandle,
) -> anyhow::Result<()> {
    let base_filter = social_filter(request, false);
    if social_text_requested(request) {
        if let Some(text_filter) = social_filter_with_text(request) {
            match run_social_command(request, input_path, tmp_path, &text_filter) {
                Ok(()) => return Ok(()),
                Err(error) => {
                    debug!(%error, "social text overlay failed, retrying without drawtext");
                    emit_editor_progress(
                        app,
                        EditorExportProgressStep::Encoding,
                        65,
                        "Texte indisponible, export video sans incrustation...",
                        true,
                        None,
                        None,
                    );
                    remove_if_exists(tmp_path)?;
                }
            }
        }
    }
    run_social_command(request, input_path, tmp_path, &base_filter)
}

fn run_social_command(
    request: &ClipEditRequest,
    input_path: &Path,
    tmp_path: &Path,
    filter: &str,
) -> anyhow::Result<()> {
    let fps = sanitized_fps(request.fps).to_string();
    let bitrate = format!("{}k", sanitized_bitrate(request.bitrate_kbps));
    let mut command = Command::new("ffmpeg");
    command
        .args(["-y", "-hide_banner", "-loglevel", "error"])
        .args(["-ss", &format_seconds_arg(request.start_seconds)])
        .args(["-to", &format_seconds_arg(request.end_seconds)])
        .arg("-i")
        .arg(input_path)
        .args(["-filter_complex", filter])
        .args(["-map", "[v]", "-map", "0:a?"])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-b:v",
            &bitrate,
            "-r",
            &fps,
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "160k",
            "-movflags",
            "+faststart",
        ])
        .arg(tmp_path);
    run_command(command, "Encodage vertical FFmpeg")
}

fn append_video_codec_args(command: &mut Command, request: &ClipEditRequest, format: OutputFormat) {
    let fps = sanitized_fps(request.fps).to_string();
    let bitrate = format!("{}k", sanitized_bitrate(request.bitrate_kbps));
    match format {
        OutputFormat::Mp4 => {
            command.args([
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-crf",
                "20",
                "-r",
                &fps,
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-b:a",
                "160k",
                "-movflags",
                "+faststart",
            ]);
        }
        OutputFormat::Webm => {
            command.args([
                "-c:v",
                "libvpx-vp9",
                "-deadline",
                "realtime",
                "-cpu-used",
                "4",
                "-b:v",
                &bitrate,
                "-r",
                &fps,
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "libopus",
                "-b:a",
                "160k",
            ]);
        }
    }
}

fn social_filter(request: &ClipEditRequest, reserve_text_space: bool) -> String {
    let foreground_y = if reserve_text_space { "(H-h)/2+28" } else { "(H-h)/2" };
    match request.layout.unwrap_or(SocialLayout::VerticalBlur) {
        SocialLayout::VerticalBlur => format!(
            "[0:v]scale={SOCIAL_WIDTH}:{SOCIAL_HEIGHT}:force_original_aspect_ratio=increase,crop={SOCIAL_WIDTH}:{SOCIAL_HEIGHT},boxblur=20:1[bg];[0:v]scale={SOCIAL_WIDTH}:-2[fg];[bg][fg]overlay=(W-w)/2:{foreground_y},format=yuv420p[v]"
        ),
        SocialLayout::VerticalCrop => format!(
            "[0:v]scale={SOCIAL_WIDTH}:{SOCIAL_HEIGHT}:force_original_aspect_ratio=increase,crop={SOCIAL_WIDTH}:{SOCIAL_HEIGHT},format=yuv420p[v]"
        ),
    }
}

fn social_filter_with_text(request: &ClipEditRequest) -> Option<String> {
    let font = find_drawtext_font()?;
    let mut filter = social_filter(request, true).trim_end_matches("[v]").to_owned();
    let title = request
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(title) = title {
        filter.push_str(&format!(
            ",drawtext=fontfile='{}':text='{}':fontcolor=white:fontsize=58:x=(w-text_w)/2:y=150:box=1:boxcolor=black@0.36:boxborderw=24",
            escape_drawtext_value(&font),
            escape_drawtext_value(&truncate_text(title, 80)),
        ));
    }
    let subtitle = request
        .subtitle
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(subtitle) = subtitle {
        filter.push_str(&format!(
            ",drawtext=fontfile='{}':text='{}':fontcolor=white:fontsize=34:x=(w-text_w)/2:y=1720:box=1:boxcolor=black@0.30:boxborderw=18",
            escape_drawtext_value(&font),
            escape_drawtext_value(&truncate_text(subtitle, 96)),
        ));
    }
    if request.watermark {
        filter.push_str(&format!(
            ",drawtext=fontfile='{}':text='WT Clip':fontcolor=white@0.82:fontsize=30:x=w-text_w-44:y=h-text_h-42:box=1:boxcolor=black@0.22:boxborderw=12",
            escape_drawtext_value(&font),
        ));
    }
    filter.push_str("[v]");
    Some(filter)
}

fn social_text_requested(request: &ClipEditRequest) -> bool {
    request.watermark
        || request
            .title
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
        || request
            .subtitle
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
}

fn find_drawtext_font() -> Option<String> {
    [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets/fonts/Inter-Regular.ttf"),
        PathBuf::from("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"),
        PathBuf::from("/usr/share/fonts/truetype/liberation2/LiberationSans-Regular.ttf"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .map(|path| path.display().to_string())
}

fn probe_media_info(path: &Path) -> anyhow::Result<ClipMediaInfo> {
    validate_input_path(path)?;
    ensure_command_available("ffprobe")?;
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration,size,format_name:stream=codec_type,codec_name,width,height,avg_frame_rate,r_frame_rate,duration",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .with_context(|| format!("Impossible de lancer ffprobe pour {}", path.display()))?;

    if !output.status.success() {
        anyhow::bail!(
            "Impossible de lire les informations du clip avec ffprobe: {}",
            stderr_summary(&output.stderr)
        );
    }

    let parsed: FfprobeOutput = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("ffprobe a retourne une reponse invalide pour {}", path.display()))?;
    let video = parsed
        .streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("video"))
        .ok_or_else(|| anyhow::anyhow!("Aucune piste video trouvee dans {}", path.display()))?;

    let format = parsed.format.as_ref();
    let duration_seconds = parse_f64(video.duration.as_deref())
        .or_else(|| format.and_then(|format| parse_f64(format.duration.as_deref())))
        .ok_or_else(|| anyhow::anyhow!("Duree du clip introuvable: {}", path.display()))?;
    let size_bytes = format
        .and_then(|format| parse_u64(format.size.as_deref()))
        .or_else(|| fs::metadata(path).ok().map(|metadata| metadata.len()))
        .unwrap_or(0);

    Ok(ClipMediaInfo {
        duration_seconds,
        width: video.width.unwrap_or(0),
        height: video.height.unwrap_or(0),
        fps: parse_fps(
            video
                .avg_frame_rate
                .as_deref()
                .or(video.r_frame_rate.as_deref()),
        ),
        codec: video
            .codec_name
            .clone()
            .unwrap_or_else(|| "unknown".to_owned()),
        container: format
            .and_then(|format| format.format_name.clone())
            .or_else(|| path.extension().and_then(|ext| ext.to_str()).map(str::to_owned))
            .unwrap_or_else(|| "unknown".to_owned()),
        size_bytes,
    })
}

fn validate_trim_request(start_seconds: f64, end_seconds: f64, duration_seconds: f64) -> anyhow::Result<()> {
    if !start_seconds.is_finite() || !end_seconds.is_finite() {
        anyhow::bail!("Le debut et la fin doivent etre des nombres valides.");
    }
    if start_seconds < 0.0 {
        anyhow::bail!("Le debut du trim doit etre positif.");
    }
    if end_seconds <= start_seconds {
        anyhow::bail!("La fin du trim doit etre apres le debut.");
    }
    if duration_seconds.is_finite() && end_seconds > duration_seconds + 0.150 {
        anyhow::bail!("La fin du trim depasse la duree du clip.");
    }
    Ok(())
}

fn validate_input_path(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("Clip introuvable: {}", path.display()))?;
    if !metadata.is_file() || metadata.len() == 0 {
        anyhow::bail!("Clip invalide ou vide: {}", path.display());
    }
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("webm") | Some("mp4") => Ok(()),
        _ => anyhow::bail!("Format video non pris en charge pour l'editeur: {}", path.display()),
    }
}

fn validate_non_empty_file(path: &Path, label: &str) -> anyhow::Result<()> {
    let metadata = fs::metadata(path).with_context(|| format!("{label}: {}", path.display()))?;
    if !metadata.is_file() || metadata.len() == 0 {
        anyhow::bail!("{label}: {}", path.display());
    }
    Ok(())
}

fn ensure_not_original(input_path: &Path, output_path: &Path) -> anyhow::Result<()> {
    let input = input_path.canonicalize().unwrap_or_else(|_| input_path.to_path_buf());
    let output_parent = output_path
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .unwrap_or_else(|| output_path.parent().unwrap_or_else(|| Path::new("")).to_path_buf());
    let output = output_parent.join(
        output_path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Nom de sortie invalide"))?,
    );
    if input == output {
        anyhow::bail!("L'editeur ne peut pas ecrire par-dessus le clip original.");
    }
    Ok(())
}

fn output_base_dir(config: &AppConfig, mode: ClipEditorMode) -> anyhow::Result<PathBuf> {
    let base = config.library.output_dir_path()?;
    Ok(match mode {
        ClipEditorMode::SocialVertical => base.join("Social"),
        ClipEditorMode::TrimOriginal | ClipEditorMode::YoutubeHorizontal => base.join("Edited"),
    })
}

fn backup_output_dir(config: &AppConfig) -> anyhow::Result<PathBuf> {
    Ok(config.library.output_dir_path()?.join("Backups"))
}

fn replacement_paths(
    config: &AppConfig,
    input_path: &Path,
    request: &ClipEditRequest,
) -> anyhow::Result<ReplacementPaths> {
    let original_video_path = input_path.to_path_buf();
    let target_video_path = replacement_target_path(input_path, request);
    if target_video_path != original_video_path && target_video_path.exists() {
        anyhow::bail!(
            "Impossible de remplacer l'original: {} existe deja.",
            target_video_path.display()
        );
    }
    let original_metadata_path = original_video_path.with_extension("json");
    let original_thumbnail_path = original_video_path.with_extension("jpg");
    let target_metadata_path = target_video_path.with_extension("json");
    let target_thumbnail_path = target_video_path.with_extension("jpg");
    Ok(ReplacementPaths {
        tmp_video_path: temporary_output_path(&target_video_path),
        tmp_metadata_path: temporary_sidecar_path(&target_metadata_path),
        tmp_thumbnail_path: temporary_sidecar_path(&target_thumbnail_path),
        backup_dir: backup_output_dir(config)?,
        original_video_path,
        original_metadata_path,
        original_thumbnail_path,
        target_video_path,
        target_metadata_path,
        target_thumbnail_path,
    })
}

fn replacement_target_path(input_path: &Path, request: &ClipEditRequest) -> PathBuf {
    let extension = effective_output_format(request).extension();
    if input_path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
    {
        input_path.to_path_buf()
    } else {
        input_path.with_extension(extension)
    }
}

fn backup_original_files(paths: &ReplacementPaths) -> anyhow::Result<BackupPaths> {
    let backup_video_path = backup_video_path(paths)?;
    fs::copy(&paths.original_video_path, &backup_video_path).with_context(|| {
        format!(
            "Impossible de sauvegarder la video originale vers {}",
            backup_video_path.display()
        )
    })?;
    let backup_metadata_path = if paths.original_metadata_path.is_file() {
        let path = backup_video_path.with_extension("json");
        fs::copy(&paths.original_metadata_path, &path).with_context(|| {
            format!(
                "Impossible de sauvegarder les metadata originales vers {}",
                path.display()
            )
        })?;
        Some(path)
    } else {
        None
    };
    let backup_thumbnail_path = if paths.original_thumbnail_path.is_file() {
        let path = backup_video_path.with_extension("jpg");
        fs::copy(&paths.original_thumbnail_path, &path).with_context(|| {
            format!(
                "Impossible de sauvegarder la miniature originale vers {}",
                path.display()
            )
        })?;
        Some(path)
    } else {
        None
    };

    Ok(BackupPaths {
        video_path: backup_video_path,
        metadata_path: backup_metadata_path,
        thumbnail_path: backup_thumbnail_path,
    })
}

fn backup_video_path(paths: &ReplacementPaths) -> anyhow::Result<PathBuf> {
    let stem = paths
        .original_video_path
        .file_stem()
        .and_then(|value| value.to_str())
        .map(sanitize_file_stem)
        .unwrap_or_else(|| "clip".to_owned());
    let extension = paths
        .original_video_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("video");
    let timestamp = Utc::now().format("%Y-%m-%d-%H-%M-%S");
    ensure_unique_path(paths.backup_dir.join(format!(
        "{stem}-original-backup-{timestamp}.{extension}"
    )))
}

fn resolve_output_path(
    request: &ClipEditRequest,
    input_path: &Path,
    output_base_dir: &Path,
) -> anyhow::Result<PathBuf> {
    let stem = input_path
        .file_stem()
        .and_then(|value| value.to_str())
        .map(sanitize_file_stem)
        .unwrap_or_else(|| "clip".to_owned());
    let mode_label = match request.mode {
        ClipEditorMode::TrimOriginal => "trim",
        ClipEditorMode::YoutubeHorizontal => "youtube",
        ClipEditorMode::SocialVertical => "vertical",
    };
    let file_name = format!(
        "{stem}_{mode_label}_{}_{}.{}",
        format_time_label(request.start_seconds),
        format_time_label(request.end_seconds),
        effective_output_format(request).extension()
    );
    ensure_unique_path(output_base_dir.join(file_name))
}

fn ensure_unique_path(path: PathBuf) -> anyhow::Result<PathBuf> {
    if !path.exists() {
        return Ok(path);
    }
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow::anyhow!("Nom de sortie invalide: {}", path.display()))?;
    let extension = path.extension().and_then(|value| value.to_str());
    for index in 1..10_000 {
        let candidate = match extension {
            Some(extension) => parent.join(format!("{stem}-{index}.{extension}")),
            None => parent.join(format!("{stem}-{index}")),
        };
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    anyhow::bail!("Impossible de trouver un nom de fichier unique pour {}", path.display())
}

fn effective_output_format(request: &ClipEditRequest) -> OutputFormat {
    match request.mode {
        ClipEditorMode::SocialVertical | ClipEditorMode::YoutubeHorizontal => OutputFormat::Mp4,
        ClipEditorMode::TrimOriginal => request.output_format,
    }
}

fn temporary_output_path(output_path: &Path) -> PathBuf {
    let parent = output_path.parent().unwrap_or_else(|| Path::new(""));
    let stem = output_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("clip");
    let extension = output_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("video");
    parent.join(format!(".{stem}.{}.tmp.{extension}", Uuid::new_v4()))
}

fn temporary_sidecar_path(output_path: &Path) -> PathBuf {
    temporary_output_path(output_path)
}

fn validate_replacement_temp_video(path: &Path) -> anyhow::Result<()> {
    validate_non_empty_file(path, "Fichier temporaire de remplacement invalide")?;
    let media = probe_media_info(path)?;
    if media.duration_seconds <= 0.0 {
        anyhow::bail!(
            "Fichier temporaire de remplacement invalide: duree video nulle"
        );
    }
    Ok(())
}

fn generate_thumbnail(
    video_path: &Path,
    thumbnail_path: &Path,
    duration_seconds: f64,
) -> anyhow::Result<PathBuf> {
    info!(
        path = %video_path.display(),
        thumbnail = %thumbnail_path.display(),
        "[EDITOR_EXPORT] thumbnail started"
    );
    if let Some(parent) = thumbnail_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Impossible de creer {}", parent.display()))?;
    }
    let mut attempts = vec![thumbnail_time_seconds(duration_seconds), 0.5, 0.1];
    attempts.dedup_by(|a, b| (*a - *b).abs() < 0.001);
    let mut last_error = None;
    for time in attempts {
        let _ = remove_if_exists(thumbnail_path);
        let mut command = Command::new("ffmpeg");
        command
            .args(["-y", "-hide_banner", "-loglevel", "error"])
            .args(["-ss", &format_seconds_arg(time)])
            .arg("-i")
            .arg(video_path)
            .args(["-frames:v", "1", "-s", "640x360"])
            .arg(thumbnail_path);
        match run_command(command, "Generation miniature FFmpeg")
            .and_then(|()| validate_non_empty_file(thumbnail_path, "Miniature invalide"))
        {
            Ok(()) => {
                info!(
                    path = %video_path.display(),
                    thumbnail = %thumbnail_path.display(),
                    thumbnail_time_seconds = time,
                    "[EDITOR_EXPORT] thumbnail done"
                );
                return Ok(thumbnail_path.to_path_buf());
            }
            Err(error) => {
                debug!(
                    %error,
                    path = %video_path.display(),
                    thumbnail_time_seconds = time,
                    "edited clip thumbnail attempt failed"
                );
                last_error = Some(error);
            }
        }
    }
    let _ = remove_if_exists(thumbnail_path);
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Generation miniature FFmpeg a echoue")))
}

fn thumbnail_time_seconds(duration_seconds: f64) -> f64 {
    if duration_seconds.is_finite() && duration_seconds > 2.0 {
        1.0
    } else if duration_seconds.is_finite() && duration_seconds > 0.0 {
        (duration_seconds * 0.25).clamp(0.1, duration_seconds.max(0.1))
    } else {
        0.1
    }
}

fn write_edited_metadata(
    metadata_path: &Path,
    request: &ClipEditRequest,
    input_path: &Path,
    output_path: &Path,
    source_details: Option<SourceClipDetails>,
    thumbnail_path: Option<&Path>,
    duration_seconds: f64,
) -> anyhow::Result<()> {
    let reason = source_details.as_ref().and_then(|details| details.reason.clone());
    let clip_type = source_details
        .as_ref()
        .and_then(|details| details.clip_type.clone());
    let metadata = EditedClipMetadata {
        created_by: "wt-clipper",
        kind: "edited_clip",
        export_type: export_type_for_mode(request.mode),
        source_video_path: input_path.display().to_string(),
        output_video_path: output_path.display().to_string(),
        mode: request.mode,
        layout: request.layout,
        start_seconds: request.start_seconds,
        end_seconds: request.end_seconds,
        duration_seconds,
        title: request.title.clone().filter(|value| !value.trim().is_empty()),
        subtitle: request.subtitle.clone().filter(|value| !value.trim().is_empty()),
        watermark: request.watermark,
        fps: sanitized_fps(request.fps),
        bitrate_kbps: sanitized_bitrate(request.bitrate_kbps),
        created_at: Utc::now().to_rfc3339(),
        reason: reason.clone(),
        clip_type: clip_type.clone(),
        source_reason: reason,
        source_clip_type: clip_type,
        thumbnail_path: thumbnail_path.map(|path| path.display().to_string()),
    };
    let json = serde_json::to_string_pretty(&metadata)?;
    fs::write(metadata_path, json)
        .with_context(|| format!("Impossible d'ecrire les metadata {}", metadata_path.display()))
}

fn write_replacement_metadata(
    metadata_path: &Path,
    request: &ClipEditRequest,
    paths: &ReplacementPaths,
    backup: &BackupPaths,
) -> anyhow::Result<()> {
    let original_metadata = read_json_value(&paths.original_metadata_path);
    let metadata = replacement_metadata_value(original_metadata, request, paths, backup);
    let json = serde_json::to_string_pretty(&metadata)?;
    fs::write(metadata_path, json)
        .with_context(|| format!("Impossible d'ecrire les metadata {}", metadata_path.display()))
}

fn replacement_metadata_value(
    original_metadata: Option<Value>,
    request: &ClipEditRequest,
    paths: &ReplacementPaths,
    backup: &BackupPaths,
) -> Value {
    let mut root = original_metadata
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let edited_at = Utc::now().to_rfc3339();
    let original_duration_seconds = root.get("duration_seconds").cloned();
    root.entry("created_by".to_owned())
        .or_insert_with(|| json!("wt-clipper"));
    root.insert("kind".to_owned(), json!("edited_replacement"));
    root.insert("export_type".to_owned(), json!(export_type_for_mode(request.mode)));
    if let Some(reason) = root.get("reason").cloned() {
        root.entry("source_reason".to_owned()).or_insert(reason);
    }
    if let Some(clip_type) = root.get("clip_type").cloned() {
        root.entry("source_clip_type".to_owned()).or_insert(clip_type);
    }
    root.insert(
        "source_video_path".to_owned(),
        json!(paths.original_video_path.display().to_string()),
    );
    root.insert(
        "output_video_path".to_owned(),
        json!(paths.target_video_path.display().to_string()),
    );
    root.insert("replaced_original".to_owned(), json!(true));
    root.insert(
        "backup_video_path".to_owned(),
        json!(backup.video_path.display().to_string()),
    );
    root.insert("duration_seconds".to_owned(), json!(edited_duration(request)));
    root.insert(
        "thumbnail_path".to_owned(),
        json!(paths.target_thumbnail_path.display().to_string()),
    );
    root.insert("edited_at".to_owned(), json!(edited_at));
    root.insert(
        "edit".to_owned(),
        json!({
            "replaced_original": true,
            "backup_video_path": backup.video_path.display().to_string(),
            "backup_metadata_path": backup.metadata_path.as_ref().map(|path| path.display().to_string()),
            "backup_thumbnail_path": backup.thumbnail_path.as_ref().map(|path| path.display().to_string()),
            "start_seconds": request.start_seconds,
            "end_seconds": request.end_seconds,
            "duration_seconds": edited_duration(request),
            "mode": request.mode,
            "layout": request.layout,
            "title": request.title.as_deref(),
            "subtitle": request.subtitle.as_deref(),
            "watermark": request.watermark,
            "fps": sanitized_fps(request.fps),
            "bitrate_kbps": sanitized_bitrate(request.bitrate_kbps),
            "edited_at": edited_at,
            "original_duration_seconds": original_duration_seconds,
        }),
    );
    Value::Object(root)
}

fn read_json_value(path: &Path) -> Option<Value> {
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn read_source_clip_details(
    request: &ClipEditRequest,
    input_path: &Path,
) -> Option<SourceClipDetails> {
    let metadata_path = request
        .metadata_path
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| input_path.with_extension("json"));
    let content = fs::read_to_string(metadata_path).ok()?;
    serde_json::from_str::<SourceClipMetadata>(&content)
        .ok()
        .map(|metadata| {
            let reason = metadata.reason;
            let clip_type = metadata
                .clip_type
                .or_else(|| reason.as_deref().and_then(clip_type_from_reason).map(str::to_owned));
            SourceClipDetails { reason, clip_type }
        })
}

fn export_type_for_mode(mode: ClipEditorMode) -> &'static str {
    match mode {
        ClipEditorMode::SocialVertical => "social",
        ClipEditorMode::TrimOriginal | ClipEditorMode::YoutubeHorizontal => "edited",
    }
}

fn clip_type_from_reason(reason: &str) -> Option<&'static str> {
    match reason {
        "target-destroyed" | "target_destroyed" => Some("kill"),
        "base-destroyed" | "base_destroyed" => Some("base"),
        "player-destroyed" | "player_destroyed" => Some("death"),
        "multi-kill" | "multi_kill" => Some("multi"),
        "manual" => Some("manual"),
        _ => None,
    }
}

fn commit_replacement_files(paths: &ReplacementPaths, backup: &BackupPaths) -> anyhow::Result<()> {
    let result = commit_replacement_files_inner(paths);
    if let Err(error) = result {
        let rollback_error = restore_original_from_backup(paths, backup).err();
        cleanup_replacement_temps(paths);
        if let Some(rollback_error) = rollback_error {
            anyhow::bail!(
                "Remplacement annule apres erreur ({error}); restauration incomplete: {rollback_error}"
            );
        }
        return Err(error);
    }
    Ok(())
}

fn commit_replacement_files_inner(paths: &ReplacementPaths) -> anyhow::Result<()> {
    if paths.target_video_path != paths.original_video_path && paths.target_video_path.exists() {
        anyhow::bail!(
            "Impossible de remplacer l'original: {} existe deja.",
            paths.target_video_path.display()
        );
    }

    fs::rename(&paths.tmp_video_path, &paths.target_video_path).with_context(|| {
        format!(
            "Impossible de remplacer la video par {}",
            paths.target_video_path.display()
        )
    })?;
    fs::rename(&paths.tmp_metadata_path, &paths.target_metadata_path).with_context(|| {
        format!(
            "Impossible de remplacer les metadata par {}",
            paths.target_metadata_path.display()
        )
    })?;
    fs::rename(&paths.tmp_thumbnail_path, &paths.target_thumbnail_path).with_context(|| {
        format!(
            "Impossible de remplacer la miniature par {}",
            paths.target_thumbnail_path.display()
        )
    })?;

    if paths.target_video_path != paths.original_video_path {
        remove_if_exists(&paths.original_video_path)?;
        remove_if_exists(&paths.original_metadata_path)?;
        remove_if_exists(&paths.original_thumbnail_path)?;
    }
    Ok(())
}

fn restore_original_from_backup(paths: &ReplacementPaths, backup: &BackupPaths) -> anyhow::Result<()> {
    if paths.target_video_path != paths.original_video_path {
        let _ = remove_if_exists(&paths.target_video_path);
        let _ = remove_if_exists(&paths.target_metadata_path);
        let _ = remove_if_exists(&paths.target_thumbnail_path);
    }

    fs::copy(&backup.video_path, &paths.original_video_path).with_context(|| {
        format!(
            "Impossible de restaurer la video originale depuis {}",
            backup.video_path.display()
        )
    })?;
    restore_optional_backup_file(&backup.metadata_path, &paths.original_metadata_path)?;
    restore_optional_backup_file(&backup.thumbnail_path, &paths.original_thumbnail_path)?;
    Ok(())
}

fn restore_optional_backup_file(backup_path: &Option<PathBuf>, original_path: &Path) -> anyhow::Result<()> {
    match backup_path {
        Some(backup_path) => {
            fs::copy(backup_path, original_path).with_context(|| {
                format!(
                    "Impossible de restaurer {} depuis {}",
                    original_path.display(),
                    backup_path.display()
                )
            })?;
        }
        None => {
            let _ = remove_if_exists(original_path);
        }
    }
    Ok(())
}

fn run_command(mut command: Command, label: &str) -> anyhow::Result<()> {
    info!("[EDITOR_EXPORT] ffmpeg started label={label}");
    let output = command
        .stdout(Stdio::null())
        .output()
        .with_context(|| format!("Impossible de lancer {label}"))?;
    if output.status.success() {
        info!("[EDITOR_EXPORT] ffmpeg exited label={label} status=success");
        return Ok(());
    }
    info!(
        status = ?output.status.code(),
        "[EDITOR_EXPORT] ffmpeg exited label={label} status=failed"
    );
    anyhow::bail!("{label} a echoue: {}", stderr_summary(&output.stderr))
}

fn ensure_command_available(command: &str) -> anyhow::Result<()> {
    match Command::new(command)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) if status.success() => Ok(()),
        Ok(_) => anyhow::bail!("{command} est installe mais ne repond pas correctement."),
        Err(error) if error.kind() == ErrorKind::NotFound => anyhow::bail!(FFMPEG_REQUIRED_MESSAGE),
        Err(error) => anyhow::bail!("Impossible de lancer {command}: {error}"),
    }
}

fn open_with_xdg(path: PathBuf) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("Chemin introuvable: {}", path.display()));
    }
    Command::new("xdg-open")
        .arg(&path)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Impossible d'ouvrir {}: {error}", path.display()))
}

fn emit_editor_progress(
    app: &tauri::AppHandle,
    step: EditorExportProgressStep,
    progress: u8,
    message: &str,
    active: bool,
    output_path: Option<String>,
    error: Option<String>,
) {
    let payload = editor_progress_payload(
        step,
        progress,
        message,
        active,
        output_path,
        error,
        None,
        None,
    );
    info!(
        id = payload.id,
        export_id = payload.export_id,
        stage = payload.stage,
        progress = payload.progress,
        "[EDITOR_EXPORT] progress"
    );
    if let Err(error) = app.emit("editor_export_progress_changed", payload) {
        debug!(%error, "failed to emit editor export progress");
    }
}

fn editor_progress_payload(
    step: EditorExportProgressStep,
    progress: u8,
    message: &str,
    active: bool,
    output_path: Option<String>,
    error: Option<String>,
    current_time: Option<f64>,
    duration: Option<f64>,
) -> EditorExportProgressPayload {
    let stage = editor_progress_step_name(step).to_owned();
    let payload = EditorExportProgressPayload {
        id: "editor-export".to_owned(),
        export_id: "editor-export".to_owned(),
        phase: stage.clone(),
        stage,
        active,
        step,
        progress: progress.clamp(1, 100),
        message: message.to_owned(),
        current_time,
        duration,
        output_path,
        error,
    };
    info!(
        active = payload.active,
        ?payload.step,
        progress = payload.progress,
        message = payload.message,
        output_path = ?payload.output_path,
        "[EDITOR_EXPORT_PROGRESS]"
    );
    payload
}

fn editor_progress_step_name(step: EditorExportProgressStep) -> &'static str {
    match step {
        EditorExportProgressStep::Preparing => "preparing",
        EditorExportProgressStep::Trimming => "trimming",
        EditorExportProgressStep::Encoding => "encoding",
        EditorExportProgressStep::Thumbnail => "thumbnail",
        EditorExportProgressStep::Metadata => "metadata",
        EditorExportProgressStep::Saving => "saving",
        EditorExportProgressStep::Done => "done",
        EditorExportProgressStep::Failed => "failed",
    }
}

fn cleanup_export_failure(tmp_path: &Path, metadata_path: &Path, thumbnail_path: &Path) {
    for path in [tmp_path, metadata_path, thumbnail_path] {
        if path.exists() {
            if let Err(error) = fs::remove_file(path) {
                debug!(%error, path = %path.display(), "failed to clean editor export artifact");
            }
        }
    }
}

fn cleanup_replacement_temps(paths: &ReplacementPaths) {
    cleanup_export_failure(
        &paths.tmp_video_path,
        &paths.tmp_metadata_path,
        &paths.tmp_thumbnail_path,
    );
}

fn remove_if_exists(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        fs::remove_file(path)
            .with_context(|| format!("Impossible de supprimer {}", path.display()))?;
    }
    Ok(())
}

fn default_backup_original() -> bool {
    true
}

fn sanitize_file_stem(input: &str) -> String {
    let mut output = String::new();
    let mut last_separator = false;
    for character in input.chars() {
        let next = if character.is_ascii_alphanumeric() {
            Some(character.to_ascii_lowercase())
        } else if matches!(character, '-' | '_') {
            Some(character)
        } else if character.is_whitespace() || matches!(character, '.' | ':') {
            Some('-')
        } else {
            None
        };
        if let Some(character) = next {
            if matches!(character, '-' | '_') {
                if last_separator || output.is_empty() {
                    continue;
                }
                last_separator = true;
            } else {
                last_separator = false;
            }
            output.push(character);
        }
        if output.len() >= 80 {
            break;
        }
    }
    let output = output.trim_matches(['-', '_']).to_owned();
    if output.is_empty() {
        "clip".to_owned()
    } else {
        output
    }
}

fn format_time_label(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    let hours = total / 3600;
    let minutes = (total / 60) % 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours:02}-{minutes:02}-{seconds:02}")
    } else {
        format!("{minutes:02}-{seconds:02}")
    }
}

fn format_seconds_arg(seconds: f64) -> String {
    format!("{:.3}", seconds.max(0.0))
}

fn parse_f64(value: Option<&str>) -> Option<f64> {
    value?.parse::<f64>().ok().filter(|value| value.is_finite())
}

fn parse_u64(value: Option<&str>) -> Option<u64> {
    value?.parse::<u64>().ok()
}

fn parse_fps(value: Option<&str>) -> f64 {
    let Some(value) = value else {
        return 0.0;
    };
    if value == "0/0" {
        return 0.0;
    }
    if let Some((num, den)) = value.split_once('/') {
        let numerator = num.parse::<f64>().unwrap_or(0.0);
        let denominator = den.parse::<f64>().unwrap_or(0.0);
        if denominator > 0.0 {
            return numerator / denominator;
        }
    }
    value.parse::<f64>().unwrap_or(0.0)
}

fn edited_duration(request: &ClipEditRequest) -> f64 {
    (request.end_seconds - request.start_seconds).max(0.0)
}

fn sanitized_fps(fps: u32) -> u32 {
    fps.clamp(24, 120)
}

fn sanitized_bitrate(bitrate_kbps: u32) -> u32 {
    bitrate_kbps.clamp(2_000, 30_000)
}

fn stderr_summary(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let summary = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_else(|| text.trim());
    if summary.is_empty() {
        "aucun detail fourni par FFmpeg".to_owned()
    } else {
        summary.chars().take(500).collect()
    }
}

fn escape_drawtext_value(input: &str) -> String {
    input
        .chars()
        .map(|character| match character {
            '\\' => "\\\\".to_owned(),
            ':' => "\\:".to_owned(),
            '\'' => "\\'".to_owned(),
            '%' => "\\%".to_owned(),
            '\n' | '\r' | '\t' => " ".to_owned(),
            _ => character.to_string(),
        })
        .collect::<String>()
}

fn truncate_text(input: &str, max_chars: usize) -> String {
    input.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(mode: ClipEditorMode) -> ClipEditRequest {
        ClipEditRequest {
            clip_path: "/tmp/source.webm".to_owned(),
            metadata_path: None,
            start_seconds: 8.0,
            end_seconds: 19.0,
            mode,
            output_format: OutputFormat::Webm,
            layout: Some(SocialLayout::VerticalBlur),
            title: Some("Test".to_owned()),
            subtitle: Some("Subtitle".to_owned()),
            watermark: true,
            fps: 30,
            bitrate_kbps: 10_000,
            save_mode: SaveMode::CreateCopy,
            backup_original: true,
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wt-clipper-editor-test-{name}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn trim_request_validates_start_end() {
        assert!(validate_trim_request(0.0, 1.0, 2.0).is_ok());
        assert!(validate_trim_request(-0.1, 1.0, 2.0).is_err());
        assert!(validate_trim_request(1.0, 1.0, 2.0).is_err());
        assert!(validate_trim_request(1.0, 3.0, 2.0).is_err());
    }

    #[test]
    fn export_never_overwrites_original() {
        let dir = temp_dir("overwrite");
        let input = dir.join("clip.webm");
        fs::write(&input, b"source").unwrap();
        let output = resolve_output_path(&request(ClipEditorMode::TrimOriginal), &input, &dir)
            .unwrap();

        assert_ne!(input, output);
        assert!(ensure_not_original(&input, &output).is_ok());
    }

    #[test]
    fn edited_output_path_is_unique() {
        let dir = temp_dir("unique");
        let input = dir.join("clip.webm");
        fs::write(&input, b"source").unwrap();
        let first = resolve_output_path(&request(ClipEditorMode::TrimOriginal), &input, &dir)
            .unwrap();
        fs::write(&first, b"existing").unwrap();
        let second = resolve_output_path(&request(ClipEditorMode::TrimOriginal), &input, &dir)
            .unwrap();

        assert_ne!(first, second);
        assert!(second
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap()
            .contains("-1.webm"));
    }

    #[test]
    fn social_export_uses_mp4() {
        let dir = temp_dir("social");
        let input = dir.join("clip.webm");
        fs::write(&input, b"source").unwrap();
        let output = resolve_output_path(&request(ClipEditorMode::SocialVertical), &input, &dir)
            .unwrap();

        assert_eq!(output.extension().and_then(|value| value.to_str()), Some("mp4"));
    }

    #[test]
    fn missing_ffmpeg_returns_clear_error() {
        let error = ensure_command_available("wt-clipper-definitely-missing-ffmpeg")
            .unwrap_err()
            .to_string();

        assert!(error.contains("sudo apt install ffmpeg"));
    }

    #[test]
    fn metadata_created_for_edited_clip() {
        let dir = temp_dir("metadata");
        let input = dir.join("clip.webm");
        let output = dir.join("clip_trim_00-08_00-19.webm");
        let metadata = output.with_extension("json");
        fs::write(&input, b"source").unwrap();

        write_edited_metadata(
            &metadata,
            &request(ClipEditorMode::TrimOriginal),
            &input,
            &output,
            Some(SourceClipDetails {
                reason: Some("target-destroyed".to_owned()),
                clip_type: Some("kill".to_owned()),
            }),
            Some(&output.with_extension("jpg")),
            11.0,
        )
        .unwrap();

        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(metadata).unwrap()).unwrap();
        assert_eq!(value["kind"], "edited_clip");
        assert_eq!(value["export_type"], "edited");
        assert_eq!(value["duration_seconds"], 11.0);
        assert_eq!(value["reason"], "target-destroyed");
        assert_eq!(value["clip_type"], "kill");
        assert_eq!(value["source_reason"], "target-destroyed");
        assert_eq!(value["source_clip_type"], "kill");
        assert_eq!(value["thumbnail_path"], output.with_extension("jpg").display().to_string());
    }

    #[test]
    fn social_export_metadata_uses_vertical_export_type() {
        let dir = temp_dir("social-metadata");
        let input = dir.join("clip.webm");
        let output = dir.join("clip_vertical_00-08_00-19.mp4");
        let metadata = output.with_extension("json");
        fs::write(&input, b"source").unwrap();

        write_edited_metadata(
            &metadata,
            &request(ClipEditorMode::SocialVertical),
            &input,
            &output,
            Some(SourceClipDetails {
                reason: Some("multi-kill".to_owned()),
                clip_type: Some("multi".to_owned()),
            }),
            Some(&output.with_extension("jpg")),
            11.0,
        )
        .unwrap();

        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(metadata).unwrap()).unwrap();
        assert_eq!(value["export_type"], "social");
        assert_eq!(value["reason"], "multi-kill");
        assert_eq!(value["clip_type"], "multi");
    }

    #[test]
    fn source_clip_type_is_derived_from_legacy_reason() {
        let dir = temp_dir("legacy-source-metadata");
        let input = dir.join("Replay_001.mp4");
        let metadata = input.with_extension("json");
        fs::write(&input, b"mp4").unwrap();
        fs::write(&metadata, br#"{"reason":"base-destroyed"}"#).unwrap();

        let details = read_source_clip_details(&request(ClipEditorMode::TrimOriginal), &input)
            .expect("source metadata");

        assert_eq!(details.reason.as_deref(), Some("base-destroyed"));
        assert_eq!(details.clip_type.as_deref(), Some("base"));
    }

    #[test]
    fn thumbnail_time_is_not_zero_for_normal_videos() {
        assert_eq!(thumbnail_time_seconds(25.0), 1.0);
        assert_eq!(thumbnail_time_seconds(2.0), 0.5);
        assert_eq!(thumbnail_time_seconds(0.2), 0.1);
    }

    #[test]
    fn backend_progress_payload_preparing_is_above_zero() {
        let payload = editor_progress_payload(
            EditorExportProgressStep::Preparing,
            5,
            "Preparation de l'export...",
            true,
            None,
            None,
            None,
            None,
        );

        assert_eq!(payload.progress, 5);
        assert_eq!(payload.stage, "preparing");
        assert_eq!(payload.phase, "preparing");
        assert_eq!(payload.export_id, "editor-export");
    }

    #[test]
    fn failed_export_removes_tmp_file() {
        let dir = temp_dir("cleanup");
        let tmp = dir.join(".clip.tmp.webm");
        let metadata = dir.join("clip.json");
        let thumbnail = dir.join("clip.jpg");
        fs::write(&tmp, b"partial").unwrap();
        fs::write(&metadata, b"{}").unwrap();
        fs::write(&thumbnail, b"jpg").unwrap();

        cleanup_export_failure(&tmp, &metadata, &thumbnail);

        assert!(!tmp.exists());
        assert!(!metadata.exists());
        assert!(!thumbnail.exists());
    }

    #[test]
    fn thumbnail_path_for_edited_clip_is_jpg() {
        let output = PathBuf::from("/tmp/clip_vertical_00-08_00-19.mp4");

        assert_eq!(
            output.with_extension("jpg").extension().and_then(|value| value.to_str()),
            Some("jpg")
        );
    }

    #[test]
    fn create_copy_does_not_modify_original() {
        let dir = temp_dir("copy-original");
        let input = dir.join("clip.webm");
        fs::write(&input, b"source").unwrap();
        let output = resolve_output_path(&request(ClipEditorMode::TrimOriginal), &input, &dir)
            .unwrap();

        assert!(input.exists());
        assert_ne!(input, output);
        assert_eq!(fs::read(&input).unwrap(), b"source");
    }

    #[test]
    fn replace_original_creates_backup() {
        let dir = temp_dir("backup");
        let backup_dir = dir.join("Backups");
        let input = dir.join("clip.webm");
        let metadata = input.with_extension("json");
        let thumbnail = input.with_extension("jpg");
        fs::write(&input, b"source").unwrap();
        fs::write(&metadata, br#"{"reason":"manual"}"#).unwrap();
        fs::write(&thumbnail, b"thumb").unwrap();
        let mut request = request(ClipEditorMode::TrimOriginal);
        request.save_mode = SaveMode::ReplaceOriginal;
        let paths = replacement_paths_for_test(&input, &backup_dir, &request);
        fs::create_dir_all(&paths.backup_dir).unwrap();

        let backup = backup_original_files(&paths).unwrap();

        assert!(backup.video_path.exists());
        assert!(backup.metadata_path.as_ref().is_some_and(|path| path.exists()));
        assert!(backup.thumbnail_path.as_ref().is_some_and(|path| path.exists()));
    }

    #[test]
    fn replace_original_uses_temp_file() {
        let dir = temp_dir("temp");
        let input = dir.join("clip.webm");
        fs::write(&input, b"source").unwrap();
        let mut request = request(ClipEditorMode::TrimOriginal);
        request.save_mode = SaveMode::ReplaceOriginal;
        let paths = replacement_paths_for_test(&input, &dir.join("Backups"), &request);

        assert_ne!(paths.tmp_video_path, paths.original_video_path);
        assert!(paths
            .tmp_video_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap()
            .contains(".tmp."));
    }

    #[test]
    fn replace_original_preserves_original_on_failure() {
        let dir = temp_dir("rollback");
        let input = dir.join("clip.webm");
        let metadata = input.with_extension("json");
        let thumbnail = input.with_extension("jpg");
        fs::write(&input, b"source").unwrap();
        fs::write(&metadata, b"old-meta").unwrap();
        fs::write(&thumbnail, b"old-thumb").unwrap();
        let mut request = request(ClipEditorMode::TrimOriginal);
        request.save_mode = SaveMode::ReplaceOriginal;
        let paths = replacement_paths_for_test(&input, &dir.join("Backups"), &request);
        fs::create_dir_all(&paths.backup_dir).unwrap();
        let backup = backup_original_files(&paths).unwrap();
        fs::write(&paths.tmp_video_path, b"edited").unwrap();

        let error = commit_replacement_files(&paths, &backup).unwrap_err();

        assert!(error.to_string().contains("metadata"));
        assert_eq!(fs::read(&input).unwrap(), b"source");
        assert_eq!(fs::read(&metadata).unwrap(), b"old-meta");
        assert_eq!(fs::read(&thumbnail).unwrap(), b"old-thumb");
    }

    #[test]
    fn replace_original_updates_metadata() {
        let dir = temp_dir("replace-metadata");
        let input = dir.join("kill.webm");
        let backup = BackupPaths {
            video_path: dir.join("Backups").join("kill-original-backup.webm"),
            metadata_path: Some(dir.join("Backups").join("kill-original-backup.json")),
            thumbnail_path: None,
        };
        let paths = replacement_paths_for_test(
            &input,
            &dir.join("Backups"),
            &request(ClipEditorMode::TrimOriginal),
        );
        let original = json!({
            "reason": "target-destroyed",
            "clip_type": "kill",
            "player_name": "Dawson",
            "vehicle": "F-16",
            "duration_seconds": 20
        });

        let metadata = replacement_metadata_value(
            Some(original),
            &request(ClipEditorMode::TrimOriginal),
            &paths,
            &backup,
        );

        assert_eq!(metadata["kind"], "edited_replacement");
        assert_eq!(metadata["export_type"], "edited");
        assert_eq!(metadata["reason"], "target-destroyed");
        assert_eq!(metadata["clip_type"], "kill");
        assert_eq!(metadata["source_reason"], "target-destroyed");
        assert_eq!(metadata["source_clip_type"], "kill");
        assert_eq!(metadata["player_name"], "Dawson");
        assert_eq!(metadata["edit"]["replaced_original"], true);
        assert_eq!(metadata["duration_seconds"], 11.0);
    }

    #[test]
    fn replace_original_updates_thumbnail() {
        let dir = temp_dir("replace-thumb");
        let input = dir.join("clip.webm");
        let thumbnail = input.with_extension("jpg");
        fs::write(&input, b"source").unwrap();
        fs::write(&thumbnail, b"old-thumb").unwrap();
        let mut request = request(ClipEditorMode::TrimOriginal);
        request.save_mode = SaveMode::ReplaceOriginal;
        let paths = replacement_paths_for_test(&input, &dir.join("Backups"), &request);
        fs::create_dir_all(&paths.backup_dir).unwrap();
        let backup = backup_original_files(&paths).unwrap();
        fs::write(&paths.tmp_video_path, b"edited").unwrap();
        fs::write(&paths.tmp_metadata_path, b"new-meta").unwrap();
        fs::write(&paths.tmp_thumbnail_path, b"new-thumb").unwrap();

        commit_replacement_files(&paths, &backup).unwrap();

        assert_eq!(fs::read(&thumbnail).unwrap(), b"new-thumb");
        assert_eq!(
            fs::read(backup.thumbnail_path.expect("thumbnail backup")).unwrap(),
            b"old-thumb"
        );
    }

    #[test]
    fn replace_original_result_contains_backup_path() {
        let result = EditedClipResult {
            output_path: "/tmp/clip.webm".to_owned(),
            metadata_path: Some("/tmp/clip.json".to_owned()),
            thumbnail_path: Some("/tmp/clip.jpg".to_owned()),
            duration_seconds: 10.0,
            size_bytes: 42,
            save_mode: SaveMode::ReplaceOriginal,
            replaced_original: true,
            backup_path: Some("/tmp/Backups/clip-backup.webm".to_owned()),
            backup_metadata_path: None,
            backup_thumbnail_path: None,
        };

        assert!(result.replaced_original);
        assert!(result.backup_path.is_some());
    }

    #[test]
    fn replace_original_does_not_duplicate_gallery_entry() {
        let dir = temp_dir("no-duplicate");
        let input = dir.join("clip.webm");
        fs::write(&input, b"source").unwrap();
        let mut request = request(ClipEditorMode::SocialVertical);
        request.save_mode = SaveMode::ReplaceOriginal;
        let paths = replacement_paths_for_test(&input, &dir.join("Backups"), &request);
        fs::create_dir_all(&paths.backup_dir).unwrap();
        let backup = backup_original_files(&paths).unwrap();
        fs::write(&paths.tmp_video_path, b"edited-mp4").unwrap();
        fs::write(&paths.tmp_metadata_path, b"new-meta").unwrap();
        fs::write(&paths.tmp_thumbnail_path, b"new-thumb").unwrap();

        commit_replacement_files(&paths, &backup).unwrap();

        assert!(!input.exists());
        assert!(dir.join("clip.mp4").exists());
    }

    fn replacement_paths_for_test(
        input: &Path,
        backup_dir: &Path,
        request: &ClipEditRequest,
    ) -> ReplacementPaths {
        let target_video_path = replacement_target_path(input, request);
        ReplacementPaths {
            original_video_path: input.to_path_buf(),
            original_metadata_path: input.with_extension("json"),
            original_thumbnail_path: input.with_extension("jpg"),
            target_metadata_path: target_video_path.with_extension("json"),
            target_thumbnail_path: target_video_path.with_extension("jpg"),
            tmp_video_path: temporary_output_path(&target_video_path),
            tmp_metadata_path: temporary_sidecar_path(&target_video_path.with_extension("json")),
            tmp_thumbnail_path: temporary_sidecar_path(&target_video_path.with_extension("jpg")),
            target_video_path,
            backup_dir: backup_dir.to_path_buf(),
        }
    }
}
