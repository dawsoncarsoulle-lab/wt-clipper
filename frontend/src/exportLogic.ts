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

export function formatClipDuration(durationSeconds: number | null | undefined): string {
  const total = Math.max(0, Math.floor(durationSeconds ?? 0));
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
}
