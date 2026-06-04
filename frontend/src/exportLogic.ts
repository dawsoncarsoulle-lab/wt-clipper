import type { ExportProgressPayload, GalleryClipItem, PendingClipExportDto } from "./types.js";

export type ExportableClip = {
  status: GalleryClipItem["status"] | PendingClipExportDto["status"];
  canExport?: boolean | null;
  retryable?: boolean | null;
};

export function canExportClip(clip: ExportableClip): boolean {
  if (clip.status === "ready_to_export") return true;
  if (clip.status === "failed") return clip.retryable === true && clip.canExport !== false;
  return false;
}

export function exportableCount(clips: ExportableClip[]): number {
  return clips.filter(canExportClip).length;
}

export function shouldShowExportButton(clips: ExportableClip[], isExporting: boolean): boolean {
  return isExporting || exportableCount(clips) > 0;
}

export function canCloseExportModal(
  isExporting: boolean,
  exportProgress: ExportProgressPayload | null,
): boolean {
  if (!isExporting) return true;
  if (!exportProgress?.active) return true;
  return exportProgress.currentStep === "done" || exportProgress.currentStep === "failed";
}

export function mapExportProgressPayload(payload: unknown): ExportProgressPayload | null {
  const value = asRecord(payload);
  if (!value) return null;

  const nestedType = readString(value.type);
  if (
    nestedType === "export-progress-changed" ||
    nestedType === "export_progress_changed" ||
    nestedType === "ExportProgressChanged"
  ) {
    return mapExportProgressPayload(value.payload);
  }

  const currentStep = readString(value.currentStep ?? value.current_step);
  if (!currentStep) return null;

  return {
    active: Boolean(value.active),
    total: readNumber(value.total),
    completed: readNumber(value.completed),
    failed: readNumber(value.failed),
    currentClipNumber: readNullableNumber(value.currentClipNumber ?? value.current_clip_number),
    currentClipId: readNullableString(value.currentClipId ?? value.current_clip_id),
    currentClipTitle: readNullableString(value.currentClipTitle ?? value.current_clip_title),
    currentStep: currentStep as ExportProgressPayload["currentStep"],
    progress: readNumber(value.progress),
    message: readString(value.message) ?? "",
  };
}

export function currentExportClipNumber(exportProgress: ExportProgressPayload): number {
  if (exportProgress.total <= 0) return 0;
  if (typeof exportProgress.currentClipNumber === "number") {
    return Math.min(Math.max(1, exportProgress.currentClipNumber), exportProgress.total);
  }

  return Math.min(exportProgress.completed + exportProgress.failed + 1, exportProgress.total);
}

export function formatClipDuration(durationSeconds: number | null | undefined): string {
  const total = Math.max(0, Math.floor(durationSeconds ?? 0));
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
}

function asRecord(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  return value as Record<string, unknown>;
}

function readNumber(value: unknown): number {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string") {
    const parsed = Number(value);
    if (Number.isFinite(parsed)) return parsed;
  }
  return 0;
}

function readNullableNumber(value: unknown): number | null {
  if (value === null || value === undefined) return null;
  return readNumber(value);
}

function readString(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function readNullableString(value: unknown): string | null {
  if (value === null || value === undefined) return null;
  return readString(value);
}
