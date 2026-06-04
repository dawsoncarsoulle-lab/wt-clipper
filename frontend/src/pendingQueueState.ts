import type { ClipStatus, GalleryClipItem, PendingClipExportDto } from "./types.js";

const queueStatuses = new Set<ClipStatus>([
  "waiting_post_event",
  "freezing_segments",
  "ready_to_export",
  "exporting",
  "failed",
  "expired",
]);

export function pendingDtoToGalleryItem(clip: PendingClipExportDto): GalleryClipItem {
  return {
    id: clip.id,
    status: clip.status,
    reason: clip.reason,
    createdAt: clip.createdAt,
    title: clip.title,
    progress: clip.progress ?? undefined,
    error: clip.error ?? undefined,
    exportableAt: clip.exportableAt,
    isExportable: clip.isExportable,
    canExport: clip.canExport,
    retryable: clip.retryable,
  };
}

export function mergePendingExportClips(
  existing: GalleryClipItem[],
  backendClips: PendingClipExportDto[],
): GalleryClipItem[] {
  const backendById = new Map<string, GalleryClipItem>();
  for (const clip of backendClips) {
    backendById.set(clip.id, pendingDtoToGalleryItem(clip));
  }

  const preserved = existing.filter(
    (clip) => !queueStatuses.has(clip.status) && clip.status !== "ready",
  );

  return [...backendById.values(), ...preserved];
}
