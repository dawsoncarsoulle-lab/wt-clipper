export const DEFAULT_TIMELINE_ZOOM = 1;
export const MIN_TIMELINE_ZOOM = 1;
export const MAX_TIMELINE_ZOOM = 8;

export type TimelineLayout = {
  timelineWidth: number;
  zoom: number;
  totalDuration: number;
  scrollLeft: number;
};

export type TimelineTick = {
  seconds: number;
  label: string;
  major: boolean;
};

export type TimelineSegmentModel = {
  id: string;
  sourcePath: string;
  sourceDuration: number;
  start: number;
  end: number;
  timelineStart: number;
  timelineEnd: number;
  deleted?: boolean;
};

export function clampTimelineTime(value: number, durationSeconds: number) {
  const duration = safeDuration(durationSeconds);
  if (!Number.isFinite(value)) {
    return 0;
  }
  return Math.max(0, Math.min(duration, value));
}

export function timelineTimeFromClientX(
  clientX: number,
  rectLeft: number,
  rectWidth: number,
  durationSeconds: number,
) {
  if (!Number.isFinite(rectWidth) || rectWidth <= 0) {
    return 0;
  }
  const ratio = (clientX - rectLeft) / rectWidth;
  return clampTimelineTime(ratio * safeDuration(durationSeconds), durationSeconds);
}

export function timelineTimeToX(
  time: number,
  layoutOrDuration: TimelineLayout | number,
  timelineWidth?: number,
  zoom = 1,
) {
  const layout = normalizeLayout(layoutOrDuration, timelineWidth, zoom);
  const width = layout.timelineWidth * Math.max(1, layout.zoom);
  return (clampTimelineTime(time, layout.totalDuration) / safeDuration(layout.totalDuration)) * width - layout.scrollLeft;
}

export function xToTimelineTime(
  x: number,
  layoutOrDuration: TimelineLayout | number,
  timelineWidth?: number,
  zoom = 1,
) {
  const layout = normalizeLayout(layoutOrDuration, timelineWidth, zoom);
  const width = layout.timelineWidth * Math.max(1, layout.zoom);
  if (!Number.isFinite(width) || width <= 0) {
    return 0;
  }
  return clampTimelineTime(((x + layout.scrollLeft) / width) * safeDuration(layout.totalDuration), layout.totalDuration);
}

export function timelineTimeToSegmentTime(timelineTime: number, segment: TimelineSegmentModel) {
  return timelineToSourceTime(segment, timelineTime);
}

export function timelineTimeToSegment<T extends TimelineSegmentModel>(time: number, segments: T[]) {
  const segment = findSegmentAtTimelineTime(segments, time);
  if (!segment) {
    return null;
  }
  const localTimelineTime = Math.max(0, Math.min(segmentDuration(segment), time - segment.timelineStart));
  return {
    segment,
    localTimelineTime,
    sourceTime: segment.start + localTimelineTime,
  };
}

export function sourceTimeToSegmentLocalX(
  segment: TimelineSegmentModel,
  sourceTime: number,
  segmentPixelWidth: number,
) {
  const duration = segmentDuration(segment);
  if (!Number.isFinite(segmentPixelWidth) || segmentPixelWidth <= 0 || duration <= 0) {
    return 0;
  }
  const localSourceTime = Math.max(0, Math.min(duration, sourceTime - segment.start));
  return (localSourceTime / duration) * segmentPixelWidth;
}

export function segmentLocalXToSourceTime(
  segment: TimelineSegmentModel,
  localX: number,
  segmentPixelWidth: number,
) {
  const duration = segmentDuration(segment);
  if (!Number.isFinite(segmentPixelWidth) || segmentPixelWidth <= 0 || duration <= 0) {
    return segment.start;
  }
  const ratio = Math.max(0, Math.min(1, localX / segmentPixelWidth));
  return segment.start + ratio * duration;
}

export function clampTrimStart(
  value: number,
  endSeconds: number,
  durationSeconds: number,
  minGapSeconds: number,
) {
  const maxStart = Math.max(0, Math.min(safeDuration(durationSeconds), endSeconds - minGapSeconds));
  return Math.max(0, Math.min(maxStart, Number.isFinite(value) ? value : 0));
}

export function clampTrimEnd(
  value: number,
  startSeconds: number,
  durationSeconds: number,
  minGapSeconds: number,
) {
  const duration = safeDuration(durationSeconds);
  const minEnd = Math.min(duration, startSeconds + minGapSeconds);
  return Math.max(minEnd, Math.min(duration, Number.isFinite(value) ? value : duration));
}

export function nextTimelineZoom(currentZoom: number, direction: "in" | "out") {
  const next = direction === "in" ? currentZoom + 0.5 : currentZoom - 0.5;
  return Math.max(MIN_TIMELINE_ZOOM, Math.min(MAX_TIMELINE_ZOOM, next));
}

export function timelineContentWidthPercent(zoom: number) {
  const safeZoom = Math.max(MIN_TIMELINE_ZOOM, Math.min(MAX_TIMELINE_ZOOM, zoom));
  return safeZoom * 100;
}

export function recalculateTimelineSegments<T extends TimelineSegmentModel>(segments: T[]): T[] {
  let cursor = 0;
  return segments.map((segment) => {
    const duration = segmentDuration(segment);
    if (segment.deleted) {
      return { ...segment, timelineStart: cursor, timelineEnd: cursor };
    }
    const next = { ...segment, timelineStart: cursor, timelineEnd: cursor + duration };
    cursor += duration;
    return next;
  });
}

export function activeTimelineSegments<T extends TimelineSegmentModel>(segments: T[]) {
  return segments.filter((segment) => !segment.deleted && segmentDuration(segment) > 0);
}

export function timelineDuration(segments: TimelineSegmentModel[]) {
  return activeTimelineSegments(segments).reduce((sum, segment) => sum + segmentDuration(segment), 0);
}

export function segmentDuration(segment: Pick<TimelineSegmentModel, "start" | "end">) {
  return Math.max(0, segment.end - segment.start);
}

export function findSegmentAtTimelineTime<T extends TimelineSegmentModel>(segments: T[], time: number) {
  const active = activeTimelineSegments(segments);
  return active.find((segment) => time >= segment.timelineStart && time < segment.timelineEnd)
    ?? active[active.length - 1]
    ?? null;
}

export function timelineToSourceTime(segment: TimelineSegmentModel, timelineTime: number) {
  return clampTimelineTime(segment.start + (timelineTime - segment.timelineStart), segment.sourceDuration);
}

export function sourceToTimelineTime(segment: TimelineSegmentModel, sourceTime: number) {
  return segment.timelineStart + Math.max(0, Math.min(segmentDuration(segment), sourceTime - segment.start));
}

export function splitSegmentAtPlayhead<T extends TimelineSegmentModel>(
  segments: T[],
  playhead: number,
  minSegmentSeconds: number,
  createId: () => string,
) {
  const target = findSegmentAtTimelineTime(segments, playhead);
  if (!target) {
    return { segments, selectedSegmentId: undefined, changed: false };
  }
  const sourceTime = timelineToSourceTime(target, playhead);
  if (sourceTime - target.start < minSegmentSeconds || target.end - sourceTime < minSegmentSeconds) {
    return { segments, selectedSegmentId: target.id, changed: false };
  }
  const nextId = createId();
  const nextSegments = segments.flatMap((segment) => {
    if (segment.id !== target.id) {
      return [segment];
    }
    return [
      { ...segment, end: sourceTime },
      { ...segment, id: nextId, start: sourceTime },
    ];
  });
  return {
    segments: recalculateTimelineSegments(nextSegments),
    selectedSegmentId: nextId,
    changed: true,
  };
}

export function deleteTimelineSegment<T extends TimelineSegmentModel>(segments: T[], segmentId: string) {
  return recalculateTimelineSegments(segments.map((segment) => (
    segment.id === segmentId ? { ...segment, deleted: true } : segment
  )));
}

export function restoreTimelineSegment<T extends TimelineSegmentModel>(segments: T[], segmentId: string) {
  return recalculateTimelineSegments(segments.map((segment) => (
    segment.id === segmentId ? { ...segment, deleted: false } : segment
  )));
}

export function reorderTimelineSegment<T extends TimelineSegmentModel>(
  segments: T[],
  draggedId: string,
  targetId: string,
) {
  if (draggedId === targetId) {
    return segments;
  }
  const dragged = segments.find((segment) => segment.id === draggedId);
  if (!dragged) {
    return segments;
  }
  const withoutDragged = segments.filter((segment) => segment.id !== draggedId);
  const targetIndex = withoutDragged.findIndex((segment) => segment.id === targetId);
  if (targetIndex < 0) {
    return recalculateTimelineSegments([...withoutDragged, dragged]);
  }
  return recalculateTimelineSegments([
    ...withoutDragged.slice(0, targetIndex),
    dragged,
    ...withoutDragged.slice(targetIndex),
  ]);
}

export function exportSegmentsFromTimeline(segments: TimelineSegmentModel[]) {
  return activeTimelineSegments(segments).map((segment, index) => ({
    sourcePath: segment.sourcePath,
    startSeconds: segment.start,
    endSeconds: segment.end,
    order: index,
  }));
}

export function buildTimelineTicks(durationSeconds: number, zoom: number): TimelineTick[] {
  const duration = safeDuration(durationSeconds);
  const targetTickCount = Math.max(6, Math.round(8 * Math.max(1, zoom)));
  const interval = niceTickInterval(duration / targetTickCount);
  const ticks: TimelineTick[] = [];
  for (let seconds = 0; seconds <= duration + 0.001; seconds += interval) {
    const rounded = Math.min(duration, Math.round(seconds * 100) / 100);
    ticks.push({
      seconds: rounded,
      label: formatTimelineTime(rounded),
      major: Math.abs(rounded % (interval * 2)) < 0.001 || rounded === 0,
    });
  }
  if (ticks[ticks.length - 1]?.seconds !== duration) {
    ticks.push({ seconds: duration, label: formatTimelineTime(duration), major: true });
  }
  return ticks;
}

export function formatTimelineTime(seconds: number) {
  const safe = Math.max(0, seconds);
  const minutes = Math.floor(safe / 60);
  const rest = safe - minutes * 60;
  if (safe < 60 && Math.abs(rest - Math.round(rest)) > 0.01) {
    return `${String(minutes).padStart(2, "0")}:${rest.toFixed(1).padStart(4, "0")}`;
  }
  return `${String(minutes).padStart(2, "0")}:${String(Math.round(rest)).padStart(2, "0")}`;
}

function safeDuration(durationSeconds: number) {
  return Number.isFinite(durationSeconds) && durationSeconds > 0 ? durationSeconds : 1;
}

function normalizeLayout(
  layoutOrDuration: TimelineLayout | number,
  timelineWidth = 1,
  zoom = 1,
): TimelineLayout {
  if (typeof layoutOrDuration === "object") {
    return {
      timelineWidth: Math.max(1, layoutOrDuration.timelineWidth),
      zoom: Math.max(MIN_TIMELINE_ZOOM, layoutOrDuration.zoom),
      totalDuration: safeDuration(layoutOrDuration.totalDuration),
      scrollLeft: Math.max(0, layoutOrDuration.scrollLeft),
    };
  }
  return {
    timelineWidth: Math.max(1, timelineWidth),
    zoom: Math.max(MIN_TIMELINE_ZOOM, zoom),
    totalDuration: safeDuration(layoutOrDuration),
    scrollLeft: 0,
  };
}

function niceTickInterval(rawInterval: number) {
  const candidates = [0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300];
  return candidates.find((candidate) => candidate >= rawInterval) ?? 600;
}
