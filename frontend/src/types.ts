export type ClipReason =
  | "target-destroyed"
  | "base-destroyed"
  | "player-destroyed"
  | "multi-kill"
  | "manual"
  | "unknown";

export type BufferHealth = "starting" | "healthy" | "stalled" | "error" | "restarting";
export type CaptureBackend = "gstreamer" | "gpu_screen_recorder";
export type GsrHealth = "not_available" | "stopped" | "starting" | "running" | "saving_replay" | "error";

export type BufferStatus = {
  health: BufferHealth;
  filledSecs: number;
  totalSecs: number;
  lastSegmentPath?: string | null;
  lastSegmentModifiedAt?: string | null;
  lastSegmentAgeSecs?: number | null;
  restartCount: number;
  lastGstreamerError?: string | null;
};

export type ClipStatus =
  | "detected"
  | "recording"
  | "encoding"
  | "saving"
  | "pending_export"
  | "waiting_post_event"
  | "freezing_segments"
  | "ready_to_export"
  | "exporting"
  | "ready"
  | "failed"
  | "expired";

export type ExportMode = "instant" | "deferred";

export type ClipEditorMode =
  | "trim_original"
  | "youtube_horizontal"
  | "social_vertical";

export type SocialLayout =
  | "vertical_blur"
  | "vertical_crop";

export type SaveMode = "create_copy" | "replace_original";

export type ClipEditRequest = {
  clipPath: string;
  metadataPath?: string;
  startSeconds: number;
  endSeconds: number;
  mode: ClipEditorMode;
  outputFormat: "webm" | "mp4";
  layout?: SocialLayout;
  title?: string;
  subtitle?: string;
  watermark: boolean;
  fps: number;
  bitrateKbps: number;
  saveMode: SaveMode;
  backupOriginal: boolean;
};

export type EditedClipResult = {
  outputPath: string;
  metadataPath?: string;
  thumbnailPath?: string;
  durationSeconds: number;
  sizeBytes: number;
  saveMode: SaveMode;
  replacedOriginal: boolean;
  backupPath?: string;
  backupMetadataPath?: string;
  backupThumbnailPath?: string;
};

export type ClipMediaInfo = {
  durationSeconds: number;
  width: number;
  height: number;
  fps: number;
  codec: string;
  container: string;
  sizeBytes: number;
};

export type EditorExportProgressPayload = {
  active: boolean;
  step:
    | "preparing"
    | "trimming"
    | "encoding"
    | "thumbnail"
    | "metadata"
    | "saving"
    | "done"
    | "failed";
  progress: number;
  message: string;
  outputPath?: string;
  error?: string;
};

export type ClipInfo = {
  path: string;
  thumbnailPath?: string | null;
  previewUrl?: string | null;
  fileName: string;
  reason: ClipReason;
  sizeBytes: number;
  durationSeconds: number;
  modifiedSecsAgo: number;
};

export type ClipStatusChangedPayload = {
  id: string;
  status: ClipStatus;
  reason: ClipReason;
  title: string;
  createdAt: string;
  filePath?: string | null;
  thumbnailPath?: string | null;
  durationSeconds?: number | null;
  sizeBytes?: number | null;
  progress?: number | null;
  error?: string | null;
  exportableAt?: string | null;
  isExportable?: boolean | null;
  canExport?: boolean | null;
  retryable?: boolean | null;
};

export type GalleryClipItem = {
  id: string;
  status: ClipStatus;
  reason: ClipReason;
  createdAt: string;
  title: string;
  filePath?: string;
  thumbnailPath?: string;
  previewUrl?: string;
  durationSeconds?: number;
  sizeBytes?: number;
  progress?: number;
  error?: string;
  exportableAt?: string;
  isExportable?: boolean;
  canExport?: boolean;
  retryable?: boolean;
};

export type PendingClipExportDto = {
  id: string;
  status:
    | "waiting_post_event"
    | "freezing_segments"
    | "ready_to_export"
    | "exporting"
    | "ready"
    | "failed"
    | "expired";
  reason: ClipReason;
  title: string;
  createdAt: string;
  progress?: number | null;
  error?: string | null;
  exportableAt: string;
  isExportable: boolean;
  canExport: boolean;
  retryable: boolean;
};

export type ExportProgressStep =
  | "preparing"
  | "extracting"
  | "assembling"
  | "encoding"
  | "metadata"
  | "thumbnail"
  | "saving"
  | "done"
  | "failed";

export type ExportProgressPayload = {
  active: boolean;
  total: number;
  completed: number;
  failed: number;
  currentClipNumber?: number | null;
  currentClipId: string | null;
  currentClipTitle: string | null;
  currentStep: ExportProgressStep;
  progress: number;
  message: string;
};

export type ExportSummary = {
  total: number;
  completed: number;
  failed: number;
};

export type AppConfig = {
  clip: {
    seconds: number;
    segment_seconds: number;
    post_event_seconds: number;
    multi_kill_window_seconds: number;
    output_dir: string;
    quality: "low" | "medium" | "high";
    fps: number;
    video_bitrate_kbps: number;
    source: "screen" | "window";
    keep_segments: boolean;
    export_mode: ExportMode;
  };
  capture: {
    backend: CaptureBackend;
    target: string;
    mode: "auto" | "native" | "flatpak";
    fps: number;
    replay_seconds: number;
    container: "mp4" | "mkv";
    codec: "h264" | "hevc" | "av1";
    encoder: "gpu" | "cpu";
    quality: "medium" | "high" | "very_high" | "ultra";
    bitrate_mode: "auto" | "qp" | "cbr" | "vbr";
    video_bitrate_kbps: number;
    output_dir: string;
    audio_enabled: boolean;
    audio_input: string;
  };
  war_thunder: {
    base_url: string;
    player_name: string | null;
    poll_interval_ms: number;
    request_timeout_ms: number;
  };
  triggers: {
    target_destroyed: boolean;
    base_destroyed: boolean;
    player_destroyed: boolean;
  };
  storage: {
    max_clips: number;
    max_storage_gb: number;
  };
  pending_exports: {
    pending_export_dir: string;
    max_total_size_mb: number;
    max_age_hours: number;
    delete_ready_after_export: boolean;
  };
};

export type DoctorStatus = "ok" | "warn" | "error";

export type DoctorReport = {
  summary: string;
  checks: Array<{
    name: string;
    status: DoctorStatus;
    message: string;
    hint?: string | null;
  }>;
};

export type RuntimeStatus = {
  wtConnected: boolean;
  activeCaptureBackend: CaptureBackend;
  bufferFilledSecs: number;
  bufferTotalSecs: number;
  bufferHealth: BufferHealth;
  bufferLastSegmentPath?: string | null;
  bufferLastSegmentModifiedAt?: string | null;
  bufferLastSegmentAgeSecs?: number | null;
  bufferRestartCount: number;
  lastGstreamerError?: string | null;
  gsrAvailable: boolean;
  gsrHealth: GsrHealth;
  gsrPid?: number | null;
  gsrWrapperPid?: number | null;
  gsrRecorderPid?: number | null;
  gsrSignalPid?: number | null;
  gsrMode?: string | null;
  gsrTarget: string;
  gsrMonitors: string[];
  gsrCommandLine?: string | null;
  gsrRecorderCommandLine?: string | null;
  gsrStderrHandling: string;
  gsrSaveQueueLen: number;
  gsrTotalSavesRequested: number;
  gsrTotalSavesCompleted: number;
  gsrTotalSavesFailed: number;
  gsrOutputDir?: string | null;
  gsrOutputPrefix?: string | null;
  gsrLastOutput?: string | null;
  gsrLastError?: string | null;
  gsrRestartCount: number;
  gsrReplaySeconds: number;
  gsrFps: number;
  gsrQuality: string;
  gsrBitrateMode: string;
  gsrVideoBitrateKbps: number;
  gsrEffectiveQArgument: string;
  autoClipRunning: boolean;
  activeExportMode: ExportMode;
  configRestartRequired: boolean;
  pendingExportCount: number;
  pendingExportDir: string;
  pendingExportBytes: number;
  clipsSaved: number;
  backendFdCount?: number | null;
  galleryScanCount?: number;
  galleryLastScanMs?: number;
  galleryActiveScans?: number;
  exportProgress?: ExportProgressPayload | null;
  recentEvents: Array<{
    id: string;
    at: string;
    kind: ClipReason;
    description: string;
  }>;
  lastError?: string | null;
};

export type EventEntry = {
  id: string;
  at: string;
  kind: ClipReason | "system";
  title: string;
  detail?: string;
};
