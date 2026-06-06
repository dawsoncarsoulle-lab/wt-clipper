import { useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, PointerEvent as ReactPointerEvent } from "react";
import { Minus, Plus, RotateCcw, SkipBack, SkipForward } from "lucide-react";
import {
  buildTimelineTicks,
  clampTimelineTime,
  clampTrimEnd,
  clampTrimStart,
  DEFAULT_TIMELINE_ZOOM,
  formatTimelineTime,
  segmentDuration,
  timelineToSourceTime,
  nextTimelineZoom,
  timelineContentWidthPercent,
  timelineTimeFromClientX,
} from "../editorTimelinePolicy";
import type { TimelineSegment } from "../types";

const MIN_TRIM_GAP_SECONDS = 0.25;
const SEGMENT_DRAG_THRESHOLD_PX = 4;

type TimelineDragMode = "playhead" | "start" | "end";
type InteractionMode = "none" | "scrubbing" | "trim-start" | "trim-end" | "drag-segment" | "drag-playhead";
type SegmentDragState = {
  id: string;
  startX: number;
  dragging: boolean;
};

type TrimTimelineProps = {
  durationSeconds: number;
  startSeconds: number;
  endSeconds: number;
  currentSeconds: number;
  disabled?: boolean;
  onCurrentChange: (value: number) => void;
  segments: TimelineSegment[];
  selectedSegmentId?: string;
  onSegmentSelect: (id: string) => void;
  onSegmentReorder: (draggedId: string, targetId: string) => void;
  onStartChange: (value: number) => void;
  onEndChange: (value: number) => void;
  onScrubStart?: () => void;
  onScrubEnd?: () => void;
  onSetStartToCurrent: () => void;
  onSetEndToCurrent: () => void;
  onReset: () => void;
  onSplit: () => void;
  onDeleteSegment: () => void;
  onRestoreSegment: () => void;
  canRestoreSegment: boolean;
  onSetThumbnail: () => void;
  thumbnailTime?: number;
};

export function TrimTimeline({
  durationSeconds,
  startSeconds,
  endSeconds,
  currentSeconds,
  disabled = false,
  onCurrentChange,
  segments,
  selectedSegmentId,
  onSegmentSelect,
  onSegmentReorder,
  onStartChange,
  onEndChange,
  onScrubStart,
  onScrubEnd,
  onSetStartToCurrent,
  onSetEndToCurrent,
  onReset,
  onSplit,
  onDeleteSegment,
  onRestoreSegment,
  canRestoreSegment,
  onSetThumbnail,
  thumbnailTime,
}: TrimTimelineProps) {
  const viewportRef = useRef<HTMLDivElement | null>(null);
  const trackRef = useRef<HTMLDivElement | null>(null);
  const frameRef = useRef<number | null>(null);
  const [zoom, setZoom] = useState(DEFAULT_TIMELINE_ZOOM);
  const [dragMode, setDragMode] = useState<TimelineDragMode | null>(null);
  const [interactionMode, setInteractionMode] = useState<InteractionMode>("none");
  const [draggedSegmentId, setDraggedSegmentId] = useState<string | null>(null);
  const segmentDragRef = useRef<SegmentDragState | null>(null);

  const safeDuration = Math.max(MIN_TRIM_GAP_SECONDS, durationSeconds);
  const selectedDuration = Math.max(0, endSeconds - startSeconds);
  const ticks = useMemo(() => buildTimelineTicks(safeDuration, zoom), [safeDuration, zoom]);
  const contentWidth = `${timelineContentWidthPercent(zoom)}%`;
  const selectedSegment = selectedSegmentId
    ? segments.find((segment) => segment.id === selectedSegmentId)
    : undefined;

  const startPercent = secondsToPercent(selectedSegment?.timelineStart ?? 0, safeDuration);
  const endPercent = secondsToPercent(selectedSegment?.timelineEnd ?? 0, safeDuration);
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

  useEffect(() => () => {
    if (frameRef.current != null) {
      window.cancelAnimationFrame(frameRef.current);
      frameRef.current = null;
    }
  }, []);

  function updateFromPointer(clientX: number, mode: TimelineDragMode) {
    const track = trackRef.current;
    if (!track) {
      return;
    }
    const rect = track.getBoundingClientRect();
    const timelineTime = timelineTimeFromClientX(clientX, rect.left, rect.width, safeDuration);
    const apply = () => {
      if (mode === "start") {
        if (!selectedSegment) {
          return;
        }
        const sourceTime = timelineToSourceTime(selectedSegment, timelineTime);
        const next = clampTrimStart(
          sourceTime,
          selectedSegment.end,
          selectedSegment.sourceDuration,
          MIN_TRIM_GAP_SECONDS,
        );
        onStartChange(next);
        if (currentSeconds < selectedSegment.timelineStart) {
          onCurrentChange(selectedSegment.timelineStart);
        }
        return;
      }
      if (mode === "end") {
        if (!selectedSegment) {
          return;
        }
        const sourceTime = timelineToSourceTime(selectedSegment, timelineTime);
        const next = clampTrimEnd(
          sourceTime,
          selectedSegment.start,
          selectedSegment.sourceDuration,
          MIN_TRIM_GAP_SECONDS,
        );
        onEndChange(next);
        if (currentSeconds > selectedSegment.timelineEnd) {
          onCurrentChange(selectedSegment.timelineEnd);
        }
        return;
      }
      onCurrentChange(clampTimelineTime(timelineTime, safeDuration));
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
    setInteractionMode(mode === "start" ? "trim-start" : mode === "end" ? "trim-end" : "drag-playhead");
    if (mode === "playhead") {
      onScrubStart?.();
    }
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
    setInteractionMode("none");
    if (dragMode === "playhead") {
      onScrubEnd?.();
    }
  }

  function beginSegmentPointerDrag(event: ReactPointerEvent<HTMLButtonElement>, segmentId: string) {
    if (disabled) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    event.currentTarget.setPointerCapture(event.pointerId);
    segmentDragRef.current = { id: segmentId, startX: event.clientX, dragging: false };
    setDraggedSegmentId(segmentId);
    setInteractionMode("drag-segment");
  }

  function continueSegmentPointerDrag(event: ReactPointerEvent<HTMLButtonElement>, segmentId: string) {
    const drag = segmentDragRef.current;
    if (!drag || drag.id !== segmentId || disabled) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    if (!drag.dragging && Math.abs(event.clientX - drag.startX) >= SEGMENT_DRAG_THRESHOLD_PX) {
      segmentDragRef.current = { ...drag, dragging: true };
    }
  }

  function endSegmentPointerDrag(event: ReactPointerEvent<HTMLButtonElement>, segmentId: string) {
    const drag = segmentDragRef.current;
    if (!drag || drag.id !== segmentId) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    if (!drag.dragging) {
      onSegmentSelect(segmentId);
    } else {
      onSegmentReorder(segmentId, segmentDropTargetFromClientX(event.clientX, segmentId));
    }
    segmentDragRef.current = null;
    setDraggedSegmentId(null);
    setInteractionMode("none");
  }

  function cancelSegmentPointerDrag(event: ReactPointerEvent<HTMLButtonElement>, segmentId: string) {
    const drag = segmentDragRef.current;
    if (!drag || drag.id !== segmentId) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    segmentDragRef.current = null;
    setDraggedSegmentId(null);
    setInteractionMode("none");
  }

  function segmentDropTargetFromClientX(clientX: number, draggedId: string) {
    const track = trackRef.current;
    if (!track) {
      return "";
    }
    const rect = track.getBoundingClientRect();
    const timelineTime = timelineTimeFromClientX(clientX, rect.left, rect.width, safeDuration);
    const target = segments.find((segment) => (
      !segment.deleted
      && segment.id !== draggedId
      && timelineTime < segment.timelineStart + segmentDuration(segment) / 2
    ));
    return target?.id ?? "";
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
            disabled={disabled || zoom >= 8}
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
            onPointerDown={(event) => {
              if (interactionMode !== "none" || event.target !== event.currentTarget) {
                return;
              }
              beginDrag(event, "playhead");
            }}
          >
            {segments.map((segment) => {
              const left = secondsToPercent(segment.timelineStart, safeDuration);
              const width = secondsToPercent(segmentDuration(segment), safeDuration);
              const selected = segment.id === selectedSegmentId;
              return (
                <button
                  key={segment.id}
                  className={[
                    "timeline-segment",
                    selected ? "selected" : "",
                    draggedSegmentId === segment.id ? "timeline-segment--dragging" : "",
                    segment.deleted ? "deleted" : "",
                  ].filter(Boolean).join(" ")}
                  draggable={false}
                  onPointerCancel={(event) => cancelSegmentPointerDrag(event, segment.id)}
                  onPointerDown={(event) => beginSegmentPointerDrag(event, segment.id)}
                  onPointerMove={(event) => continueSegmentPointerDrag(event, segment.id)}
                  onPointerUp={(event) => endSegmentPointerDrag(event, segment.id)}
                  style={{ left: `${left}%`, width: `${Math.max(1, width)}%` }}
                  type="button"
                >
                  <span>{segment.sourceTitle ?? "Clip"}</span>
                </button>
              );
            })}
            <div className="trim-selected" style={selectedStyle} />
            <button
              aria-label="Poignée début"
              className="trim-handle trim-handle-start"
              disabled={disabled}
              onPointerDown={(event) => {
                event.stopPropagation();
                beginDrag(event, "start");
              }}
              onPointerMove={continueDrag}
              onPointerUp={endDrag}
              onPointerCancel={endDrag}
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
              onPointerMove={continueDrag}
              onPointerUp={endDrag}
              onPointerCancel={endDrag}
              style={endHandleStyle}
              type="button"
            />
            <div
              className="trim-playhead"
              onPointerCancel={endDrag}
              onPointerDown={(event) => {
                event.stopPropagation();
                beginDrag(event, "playhead");
              }}
              onPointerMove={continueDrag}
              onPointerUp={endDrag}
              style={playheadStyle}
            >
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
      <div className="trim-actions editor-cut-actions">
        <button className="ghost-button" disabled={disabled} onClick={onSplit} type="button">
          Couper à la position
        </button>
        <button className="ghost-button destructive" disabled={disabled || !selectedSegmentId} onClick={onDeleteSegment} type="button">
          Supprimer segment
        </button>
        <button className="ghost-button" disabled={disabled || !canRestoreSegment} onClick={onRestoreSegment} type="button">
          Restaurer
        </button>
        <button className="ghost-button" disabled={disabled} onClick={onSetThumbnail} type="button">
          Utiliser cette frame comme miniature
        </button>
      </div>
      {thumbnailTime != null && (
        <div className="timeline-thumbnail-note">
          Miniature choisie à {formatTimelineTime(thumbnailTime)}
        </div>
      )}
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
