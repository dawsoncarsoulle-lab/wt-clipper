import { useMemo, useRef, useState } from "react";
import type { CSSProperties, PointerEvent as ReactPointerEvent } from "react";
import { Minus, Plus, RotateCcw, SkipBack, SkipForward } from "lucide-react";
import {
  buildTimelineTicks,
  clampTimelineTime,
  clampTrimEnd,
  clampTrimStart,
  DEFAULT_TIMELINE_ZOOM,
  formatTimelineTime,
  nextTimelineZoom,
  timelineContentWidthPercent,
  timelineTimeFromClientX,
} from "../editorTimelinePolicy";

const MIN_TRIM_GAP_SECONDS = 0.25;

type TimelineDragMode = "playhead" | "start" | "end";

type TrimTimelineProps = {
  durationSeconds: number;
  startSeconds: number;
  endSeconds: number;
  currentSeconds: number;
  disabled?: boolean;
  onCurrentChange: (value: number) => void;
  onStartChange: (value: number) => void;
  onEndChange: (value: number) => void;
  onScrubStart?: () => void;
  onScrubEnd?: () => void;
  onSetStartToCurrent: () => void;
  onSetEndToCurrent: () => void;
  onReset: () => void;
};

export function TrimTimeline({
  durationSeconds,
  startSeconds,
  endSeconds,
  currentSeconds,
  disabled = false,
  onCurrentChange,
  onStartChange,
  onEndChange,
  onScrubStart,
  onScrubEnd,
  onSetStartToCurrent,
  onSetEndToCurrent,
  onReset,
}: TrimTimelineProps) {
  const viewportRef = useRef<HTMLDivElement | null>(null);
  const trackRef = useRef<HTMLDivElement | null>(null);
  const frameRef = useRef<number | null>(null);
  const [zoom, setZoom] = useState(DEFAULT_TIMELINE_ZOOM);
  const [dragMode, setDragMode] = useState<TimelineDragMode | null>(null);

  const safeDuration = Math.max(MIN_TRIM_GAP_SECONDS, durationSeconds);
  const selectedDuration = Math.max(0, endSeconds - startSeconds);
  const ticks = useMemo(() => buildTimelineTicks(safeDuration, zoom), [safeDuration, zoom]);
  const contentWidth = `${timelineContentWidthPercent(zoom)}%`;

  const startPercent = secondsToPercent(startSeconds, safeDuration);
  const endPercent = secondsToPercent(endSeconds, safeDuration);
  const currentPercent = secondsToPercent(currentSeconds, safeDuration);
  const selectedStyle: CSSProperties = {
    left: `${startPercent}%`,
    width: `${Math.max(0, endPercent - startPercent)}%`,
  };
  const playheadStyle: CSSProperties = {
    left: `${currentPercent}%`,
  };
  const startHandleStyle: CSSProperties = {
    left: `${startPercent}%`,
  };
  const endHandleStyle: CSSProperties = {
    left: `${endPercent}%`,
  };

  function updateFromPointer(clientX: number, mode: TimelineDragMode) {
    const track = trackRef.current;
    if (!track) {
      return;
    }
    const rect = track.getBoundingClientRect();
    const time = timelineTimeFromClientX(clientX, rect.left, rect.width, safeDuration);
    const apply = () => {
      if (mode === "start") {
        const next = clampTrimStart(time, endSeconds, safeDuration, MIN_TRIM_GAP_SECONDS);
        onStartChange(next);
        if (currentSeconds < next) {
          onCurrentChange(next);
        }
        return;
      }
      if (mode === "end") {
        const next = clampTrimEnd(time, startSeconds, safeDuration, MIN_TRIM_GAP_SECONDS);
        onEndChange(next);
        if (currentSeconds > next) {
          onCurrentChange(next);
        }
        return;
      }
      onCurrentChange(clampTimelineTime(time, safeDuration));
    };

    if (frameRef.current != null) {
      window.cancelAnimationFrame(frameRef.current);
    }
    frameRef.current = window.requestAnimationFrame(apply);
  }

  function beginDrag(event: ReactPointerEvent<HTMLElement>, mode: TimelineDragMode) {
    if (disabled) {
      return;
    }
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    setDragMode(mode);
    onScrubStart?.();
    updateFromPointer(event.clientX, mode);
  }

  function continueDrag(event: ReactPointerEvent<HTMLElement>) {
    if (!dragMode || disabled) {
      return;
    }
    updateFromPointer(event.clientX, dragMode);
  }

  function endDrag(event: ReactPointerEvent<HTMLElement>) {
    if (!dragMode) {
      return;
    }
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    setDragMode(null);
    onScrubEnd?.();
  }

  function changeZoom(direction: "in" | "out") {
    const next = nextTimelineZoom(zoom, direction);
    setZoom(next);
    if (direction === "in") {
      requestAnimationFrame(() => keepPlayheadVisible(viewportRef.current, currentPercent));
    }
  }

  return (
    <section className="editor-panel trim-panel">
      <div className="editor-panel-heading">
        <div>
          <div className="editor-kicker">Timeline</div>
          <h3>Trim visuel</h3>
        </div>
        <div className="trim-duration">
          {formatTimelineTime(selectedDuration)}
        </div>
      </div>

      <div className="trim-time-row">
        <span>Début {formatTimelineTime(startSeconds)}</span>
        <span>Lecture {formatTimelineTime(currentSeconds)}</span>
        <span>Fin {formatTimelineTime(endSeconds)}</span>
      </div>

      <div className="timeline-toolbar">
        <div className="timeline-total">Total {formatTimelineTime(safeDuration)}</div>
        <div className="timeline-zoom">
          <button
            className="icon-button"
            disabled={disabled || zoom <= 1}
            onClick={() => changeZoom("out")}
            title="Zoom -"
            type="button"
          >
            <Minus className="h-4 w-4" />
          </button>
          <span>{zoom.toFixed(1)}x</span>
          <button
            className="icon-button"
            disabled={disabled || zoom >= 6}
            onClick={() => changeZoom("in")}
            title="Zoom +"
            type="button"
          >
            <Plus className="h-4 w-4" />
          </button>
        </div>
      </div>

      <div className="trim-track-shell" ref={viewportRef}>
        <div
          className="trim-timeline-content"
          style={{ width: contentWidth }}
          onPointerCancel={endDrag}
          onPointerLeave={continueDrag}
          onPointerMove={continueDrag}
          onPointerUp={endDrag}
        >
          <div className="trim-ruler">
            {ticks.map((tick) => (
              <div
                className={tick.major ? "trim-tick major" : "trim-tick"}
                key={`${tick.seconds}-${tick.label}`}
                style={{ left: `${secondsToPercent(tick.seconds, safeDuration)}%` }}
              >
                <span>{tick.label}</span>
              </div>
            ))}
          </div>
          <div
            aria-label="Timeline"
            className="trim-track"
            ref={trackRef}
            role="slider"
            aria-valuemin={0}
            aria-valuemax={safeDuration}
            aria-valuenow={currentSeconds}
            tabIndex={disabled ? -1 : 0}
            onPointerDown={(event) => beginDrag(event, "playhead")}
          >
            <div className="trim-selected" style={selectedStyle} />
            <button
              aria-label="Poignée début"
              className="trim-handle trim-handle-start"
              disabled={disabled}
              onPointerDown={(event) => {
                event.stopPropagation();
                beginDrag(event, "start");
              }}
              style={startHandleStyle}
              type="button"
            />
            <button
              aria-label="Poignée fin"
              className="trim-handle trim-handle-end"
              disabled={disabled}
              onPointerDown={(event) => {
                event.stopPropagation();
                beginDrag(event, "end");
              }}
              style={endHandleStyle}
              type="button"
            />
            <div className="trim-playhead" style={playheadStyle}>
              <span>{formatTimelineTime(currentSeconds)}</span>
            </div>
          </div>
        </div>
      </div>

      <div className="trim-actions">
        <button className="ghost-button" disabled={disabled} onClick={onSetStartToCurrent} type="button">
          <SkipBack className="h-4 w-4" />
          Début = temps actuel
        </button>
        <button className="ghost-button" disabled={disabled} onClick={onSetEndToCurrent} type="button">
          <SkipForward className="h-4 w-4" />
          Fin = temps actuel
        </button>
        <button className="icon-button" disabled={disabled} onClick={onReset} title="Reset" type="button">
          <RotateCcw className="h-4 w-4" />
        </button>
      </div>
    </section>
  );
}

function secondsToPercent(seconds: number, durationSeconds: number) {
  return Math.max(0, Math.min(100, (seconds / durationSeconds) * 100));
}

function keepPlayheadVisible(viewport: HTMLDivElement | null, currentPercent: number) {
  if (!viewport) {
    return;
  }
  const contentWidth = viewport.scrollWidth;
  const playheadX = (contentWidth * currentPercent) / 100;
  const leftPadding = 64;
  const rightPadding = 64;
  if (playheadX < viewport.scrollLeft + leftPadding) {
    viewport.scrollLeft = Math.max(0, playheadX - leftPadding);
  } else if (playheadX > viewport.scrollLeft + viewport.clientWidth - rightPadding) {
    viewport.scrollLeft = playheadX - viewport.clientWidth + rightPadding;
  }
}
