type GalleryBadgeClipType = "kill" | "multi" | "death" | "base" | "manual" | "clip";
type GalleryBadgeExportType = "edited" | "social" | "vertical";

export function shouldUseGalleryCache(nowMs: number, lastLoadMs: number, ttlMs: number, force: boolean) {
  return !force && lastLoadMs > 0 && nowMs - lastLoadMs < ttlMs;
}

export function shouldApplyGalleryLoadResult(sequence: number, latestSequence: number, mounted: boolean) {
  return mounted && sequence === latestSequence;
}

export function mountedGalleryVideoCount(activePreviewPath: string | null) {
  return activePreviewPath ? 1 : 0;
}

export function debouncedRefreshCount(eventTimesMs: number[], debounceMs: number) {
  if (eventTimesMs.length === 0) {
    return 0;
  }
  let batches = 1;
  let deadline = eventTimesMs[0] + debounceMs;
  for (const time of eventTimesMs.slice(1)) {
    if (time <= deadline) {
      deadline = time + debounceMs;
    } else {
      batches += 1;
      deadline = time + debounceMs;
    }
  }
  return batches;
}

export function shouldShowHoverVideo(videoReady: boolean, readyState: number, haveCurrentData = 2) {
  return videoReady && readyState >= haveCurrentData;
}

export function hoverPreviewSeekSeconds(startSeconds: number, durationSeconds: number | null | undefined) {
  const safeStart = Number.isFinite(startSeconds) ? Math.max(0, startSeconds) : 0;
  if (!durationSeconds || !Number.isFinite(durationSeconds) || durationSeconds <= 0) {
    return safeStart;
  }
  return Math.min(safeStart, Math.max(0, durationSeconds - 0.1));
}

export function shouldResetHoverPreview(previousPath: string | null, nextPath: string | null) {
  return previousPath !== nextPath;
}

export function shouldUnloadHoverPreview(activePath: string | null) {
  return activePath == null;
}

export function galleryBadgeLabel(
  clipType: GalleryBadgeClipType | null | undefined,
  reason: string | null | undefined,
  exportType?: GalleryBadgeExportType | string | null,
) {
  if (exportType === "edited") {
    return "Edited";
  }
  if (exportType === "social" || exportType === "vertical") {
    return "Vertical";
  }
  const normalizedType = clipType ?? clipTypeFromReason(reason);
  switch (normalizedType) {
    case "kill":
      return "KILL";
    case "multi":
      return "MULTI";
    case "death":
      return "DEATH";
    case "base":
      return "BASE";
    case "manual":
      return "MANUAL";
    default:
      return "Clip";
  }
}

export function galleryBadgeClass(
  clipType: GalleryBadgeClipType | null | undefined,
  reason: string | null | undefined,
  exportType?: GalleryBadgeExportType | string | null,
) {
  if (exportType === "edited") {
    return "edited";
  }
  if (exportType === "social" || exportType === "vertical") {
    return "vertical";
  }
  return clipType ?? clipTypeFromReason(reason) ?? "clip";
}

function clipTypeFromReason(reason: string | null | undefined): GalleryBadgeClipType | null {
  switch (reason) {
    case "target_destroyed":
    case "target-destroyed":
    case "kill":
      return "kill";
    case "multi_kill":
    case "multi-kill":
    case "multi":
      return "multi";
    case "player_destroyed":
    case "player-destroyed":
    case "death":
      return "death";
    case "base_destroyed":
    case "base-destroyed":
    case "base":
      return "base";
    case "manual":
      return "manual";
    default:
      return null;
  }
}
