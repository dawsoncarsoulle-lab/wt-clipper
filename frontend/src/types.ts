export type ClipReason =
  | "target-destroyed"
  | "base-destroyed"
  | "player-destroyed"
  | "multi-kill"
  | "manual"
  | "unknown";

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
  | "thumbnail"
  | "saving"
  | "done"
  | "failed";

export type ExportProgressPayload = {
  active: boolean;
  total: number;
  completed: number;
  failed: number;
  currentClipId?: string | null;
  currentClipTitle?: string | null;
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
  bufferFilledSecs: number;
  bufferTotalSecs: number;
  autoClipRunning: boolean;
  activeExportMode: ExportMode;
  configRestartRequired: boolean;
  pendingExportCount: number;
  pendingExportDir: string;
  pendingExportBytes: number;
  clipsSaved: number;
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
