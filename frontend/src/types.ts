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
  | "ready"
  | "failed";

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
  };
  war_thunder: {
    base_url: string;
    player_name: string | null;
    poll_interval_ms: number;
    request_timeout_ms: number;
  };
  triggers: {
    target_destroyed: boolean;
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
