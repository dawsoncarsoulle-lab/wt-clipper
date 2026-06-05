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
