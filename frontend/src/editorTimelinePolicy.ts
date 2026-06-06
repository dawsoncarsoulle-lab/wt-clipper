export const DEFAULT_TIMELINE_ZOOM = 1;
export const MIN_TIMELINE_ZOOM = 1;
export const MAX_TIMELINE_ZOOM = 6;

export type TimelineTick = {
  seconds: number;
  label: string;
  major: boolean;
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

function niceTickInterval(rawInterval: number) {
  const candidates = [0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300];
  return candidates.find((candidate) => candidate >= rawInterval) ?? 600;
}
