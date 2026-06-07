export type ClipReason =
  | "target_destroyed"
  | "base_destroyed"
  | "player_destroyed"
  | "multi_kill"
  | "manual"
  | "unknown";

export type ClipType = "kill" | "multi" | "death" | "base" | "manual" | "clip";
export type ClipExportType = "edited" | "social" | "vertical";

export type GsrHealth = "not_available" | "stopped" | "starting" | "running" | "saving_replay" | "error";

export type ClipStatus = "detected" | "recording" | "encoding" | "saving" | "ready" | "failed";

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
  segments?: EditorSegmentExport[];
  thumbnailSourcePath?: string;
  thumbnailTimeSeconds?: number;
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

export type TimelineSegment = {
  id: string;
  sourcePath: string;
  sourceClipId?: string;
  sourceTitle?: string;
  sourceThumbnail?: string;
  sourcePreviewUrl?: string;
  sourceDuration: number;
  start: number;
  end: number;
  timelineStart: number;
  timelineEnd: number;
  deleted?: boolean;
};

export type EditorTimelineState = {
  segments: TimelineSegment[];
  playhead: number;
  zoom: number;
  selectedSegmentId?: string;
  thumbnailTime?: number;
  thumbnailSourcePath?: string;
};

export type EditorSegmentExport = {
  sourcePath: string;
  startSeconds: number;
  endSeconds: number;
  order: number;
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
  id?: string;
  exportId?: string;
  phase?: string;
  stage?: string;
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
  currentTime?: number | null;
  duration?: number | null;
  outputPath?: string;
  error?: string;
};

export type ClipInfo = {
  path: string;
  thumbnailPath?: string | null;
  previewUrl?: string | null;
  fileName: string;
  reason: ClipReason;
  clipType?: ClipType | null;
  exportType?: ClipExportType | null;
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
};

export type GalleryClipItem = {
  id: string;
  status: ClipStatus;
  reason: ClipReason;
  clipType?: ClipType | null;
  exportType?: ClipExportType | null;
  createdAt: string;
  title: string;
  filePath?: string;
  thumbnailPath?: string;
  previewUrl?: string;
  durationSeconds?: number;
  sizeBytes?: number;
  progress?: number;
  error?: string;
};

export type AppConfig = {
  clip: {
    post_event_seconds: number;
    multi_kill_window_seconds: number;
  };
  library: {
    output_dir: string;
  };
  capture: {
    target: string;
    mode: "auto" | "native" | "flatpak";
    fps: number;
    replay_seconds: number;
    container: "mp4" | "mkv";
    codec: "h264" | "hevc" | "av1";
    encoder: "gpu" | "cpu";
    quality: "medium" | "high" | "very_high" | "ultra";
    bitrate_mode: "auto" | "qp" | "cbr" | "vbr";
    frame_rate_mode: "cfr" | "vfr" | "content";
    keyframe_interval_seconds: number;
    restart_replay_on_save: boolean;
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
  gsrAvailable: boolean;
  gsrHealth: GsrHealth;
  gsrPid?: number | null;
  gsrWrapperPid?: number | null;
  gsrRecorderPid?: number | null;
  gsrSignalPid?: number | null;
  gsrMode?: string | null;
  gsrTarget: string;
  gsrTargetValid: boolean;
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
  gsrFrameRateMode: string;
  gsrKeyframeIntervalSeconds: number;
  gsrRestartReplayOnSave: boolean;
  gsrVideoBitrateKbps: number;
  gsrEffectiveQArgument: string;
  autoClipRunning: boolean;
  configRestartRequired: boolean;
  clipsSaved: number;
  backendFdCount?: number | null;
  galleryScanCount?: number;
  galleryLastScanMs?: number;
  galleryActiveScans?: number;
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
