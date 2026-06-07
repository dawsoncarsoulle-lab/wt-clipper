import { useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, PointerEvent as ReactPointerEvent } from "react";
import {
  ArrowLeft,
  ArrowRight,
  Minus,
  Plus,
  RotateCcw,
  SkipBack,
  SkipForward,
} from "lucide-react";
import {
  buildTimelineTicks,
  clampTimelineTime,
  clampTrimEnd,
  clampTrimStart,
  DEFAULT_TIMELINE_ZOOM,
  formatTimelineTime,
  segmentDuration,
  sourceToTimelineTime,
  nextTimelineZoom,
  timelineContentWidthPercent,
  timelineTimeFromClientX,
} from "../editorTimelinePolicy";
import type { TimelineSegment, TimelineThumbnail } from "../types";

const MIN_TRIM_GAP_SECONDS = 0.25;

type InteractionMode =
  | "none"
  | "scrubbing"
  | "trim-start"
  | "trim-end"
  | "drag-playhead";

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
  onMoveSelectedLeft?: () => void;
  onMoveSelectedRight?: () => void;
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
  thumbnails?: TimelineThumbnail[];
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
  onSegmentReorder: _onSegmentReorder,
  onMoveSelectedLeft,
  onMoveSelectedRight,
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
  thumbnails = [],
}: TrimTimelineProps) {
  const viewportRef = useRef<HTMLDivElement | null>(null);
  const trackRef = useRef<HTMLDivElement | null>(null);
  const sourceTrackRef = useRef<HTMLDivElement | null>(null);
  const frameRef = useRef<number | null>(null);
  const [zoom, setZoom] = useState(DEFAULT_TIMELINE_ZOOM);
  const [interactionMode, setInteractionMode] =
    useState<InteractionMode>("none");

  const safeDuration = Math.max(MIN_TRIM_GAP_SECONDS, durationSeconds);
  const selectedSegment = selectedSegmentId
    ? segments.find(
        (segment) => segment.id === selectedSegmentId && !segment.deleted,
      )
    : segments.find((segment) => !segment.deleted);
  const selectedDuration = selectedSegment
    ? Math.max(0, selectedSegment.end - selectedSegment.start)
    : Math.max(0, endSeconds - startSeconds);
  const ticks = useMemo(
    () => buildTimelineTicks(safeDuration, zoom),
    [safeDuration, zoom],
  );
  const contentWidth = `${timelineContentWidthPercent(zoom)}%`;
  const currentPercent = secondsToPercent(currentSeconds, safeDuration);
  const playheadStyle: CSSProperties = {
    left: `${currentPercent}%`,
  };

  const sourceDuration = Math.max(
    MIN_TRIM_GAP_SECONDS,
    selectedSegment?.sourceDuration ?? safeDuration,
  );
  const sourceStartPercent = selectedSegment
    ? secondsToPercent(selectedSegment.start, sourceDuration)
    : 0;
  const sourceEndPercent = selectedSegment
    ? secondsToPercent(selectedSegment.end, sourceDuration)
    : 100;
  const sourceRangeStyle: CSSProperties = {
    left: `${sourceStartPercent}%`,
    width: `${Math.max(0, sourceEndPercent - sourceStartPercent)}%`,
  };
  const sourceStartHandleStyle: CSSProperties = {
    left: `${sourceStartPercent}%`,
  };
  const sourceEndHandleStyle: CSSProperties = { left: `${sourceEndPercent}%` };

  const activeSegments = segments.filter(
    (segment) => !segment.deleted && segmentDuration(segment) > 0,
  );
  const thumbnailsBySource = useMemo(() => {
    const map = new Map<string, TimelineThumbnail[]>();
    for (const thumbnail of thumbnails) {
      const list = map.get(thumbnail.sourceClipPath) ?? [];
      list.push(thumbnail);
      map.set(thumbnail.sourceClipPath, list);
    }
    for (const list of map.values()) {
      list.sort((a, b) => a.sourceTimeSeconds - b.sourceTimeSeconds);
    }
    return map;
  }, [thumbnails]);
  const selectedIndex = selectedSegment
    ? activeSegments.findIndex((segment) => segment.id === selectedSegment.id)
    : -1;
  const canMoveLeft = selectedIndex > 0;
  const canMoveRight =
    selectedIndex >= 0 && selectedIndex < activeSegments.length - 1;

  useEffect(
    () => () => {
      if (frameRef.current != null) {
        window.cancelAnimationFrame(frameRef.current);
        frameRef.current = null;
      }
    },
    [],
  );

  function schedule(fn: () => void) {
    if (frameRef.current != null) {
      window.cancelAnimationFrame(frameRef.current);
    }
    frameRef.current = window.requestAnimationFrame(fn);
  }

  function updatePlayheadFromPointer(clientX: number) {
    const track = trackRef.current;
    if (!track) {
      return;
    }
    const rect = track.getBoundingClientRect();
    const timelineTime = timelineTimeFromClientX(
      clientX,
      rect.left,
      rect.width,
      safeDuration,
    );
    schedule(() =>
      onCurrentChange(clampTimelineTime(timelineTime, safeDuration)),
    );
  }

  function sourceTimeFromPointer(clientX: number) {
    const track = sourceTrackRef.current;
    if (!track) {
      return 0;
    }
    const rect = track.getBoundingClientRect();
    const ratio = rect.width > 0 ? (clientX - rect.left) / rect.width : 0;
    return Math.max(0, Math.min(sourceDuration, ratio * sourceDuration));
  }

  function beginPlayheadDrag(event: ReactPointerEvent<HTMLElement>) {
    if (disabled) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    event.currentTarget.setPointerCapture(event.pointerId);
    setInteractionMode("drag-playhead");
    onScrubStart?.();
    updatePlayheadFromPointer(event.clientX);
  }

  function continuePlayheadDrag(event: ReactPointerEvent<HTMLElement>) {
    if (disabled || interactionMode !== "drag-playhead") {
      return;
    }
    event.preventDefault();
    updatePlayheadFromPointer(event.clientX);
  }

  function endPlayheadDrag(event: ReactPointerEvent<HTMLElement>) {
    if (interactionMode !== "drag-playhead") {
      return;
    }
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    setInteractionMode("none");
    onScrubEnd?.();
  }

  function beginSourceTrim(
    event: ReactPointerEvent<HTMLElement>,
    mode: "trim-start" | "trim-end",
  ) {
    if (disabled || !selectedSegment) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    event.currentTarget.setPointerCapture(event.pointerId);
    setInteractionMode(mode);
    updateSourceTrimFromPointer(event.clientX, mode);
  }

  function continueSourceTrim(event: ReactPointerEvent<HTMLElement>) {
    if (
      disabled ||
      (interactionMode !== "trim-start" && interactionMode !== "trim-end")
    ) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    updateSourceTrimFromPointer(event.clientX, interactionMode);
  }

  function endSourceTrim(event: ReactPointerEvent<HTMLElement>) {
    if (interactionMode !== "trim-start" && interactionMode !== "trim-end") {
      return;
    }
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    setInteractionMode("none");
  }

  function updateSourceTrimFromPointer(
    clientX: number,
    mode: "trim-start" | "trim-end",
  ) {
    if (!selectedSegment) {
      return;
    }
    const sourceTime = sourceTimeFromPointer(clientX);
    if (mode === "trim-start") {
      const next = clampTrimStart(
        sourceTime,
        selectedSegment.end,
        selectedSegment.sourceDuration,
        MIN_TRIM_GAP_SECONDS,
      );
      schedule(() => {
        onStartChange(next);
        if (
          currentSeconds < selectedSegment.timelineStart ||
          currentSeconds > selectedSegment.timelineEnd
        ) {
          onCurrentChange(selectedSegment.timelineStart);
        }
      });
      return;
    }
    const next = clampTrimEnd(
      sourceTime,
      selectedSegment.start,
      selectedSegment.sourceDuration,
      MIN_TRIM_GAP_SECONDS,
    );
    schedule(() => {
      onEndChange(next);
      if (currentSeconds > selectedSegment.timelineEnd) {
        onCurrentChange(selectedSegment.timelineEnd);
      }
    });
  }

  function sourceTrackSeek(event: ReactPointerEvent<HTMLDivElement>) {
    if (disabled || !selectedSegment || event.target !== event.currentTarget) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    const sourceTime = Math.max(
      selectedSegment.start,
      Math.min(selectedSegment.end, sourceTimeFromPointer(event.clientX)),
    );
    onCurrentChange(sourceToTimelineTime(selectedSegment, sourceTime));
  }

  function selectSegment(
    event: ReactPointerEvent<HTMLButtonElement>,
    segmentId: string,
  ) {
    event.preventDefault();
    event.stopPropagation();
    if (!disabled) {
      onSegmentSelect(segmentId);
    }
  }

  function changeZoom(direction: "in" | "out") {
    const next = nextTimelineZoom(zoom, direction);
    setZoom(next);
    if (direction === "in") {
      requestAnimationFrame(() =>
        keepPlayheadVisible(viewportRef.current, currentPercent),
      );
    }
  }

  return (
    <section className="editor-panel trim-panel">
      <div className="editor-panel-heading">
        <div>
          <div className="editor-kicker">Timeline</div>
          <h3>Montage</h3>
        </div>
        <div className="trim-duration">
          {formatTimelineTime(selectedDuration)} sélectionné
        </div>
      </div>

      <div className="trim-time-row">
        <span>
          Segment{" "}
          {selectedSegment
            ? formatTimelineTime(selectedSegment.start)
            : "--:--"}
        </span>
        <span>Lecture {formatTimelineTime(currentSeconds)}</span>
        <span>
          Fin{" "}
          {selectedSegment ? formatTimelineTime(selectedSegment.end) : "--:--"}
        </span>
      </div>

      <div className="timeline-toolbar">
        <div className="timeline-total">
          Total {formatTimelineTime(safeDuration)}
        </div>
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
          onPointerCancel={endPlayheadDrag}
          onPointerMove={continuePlayheadDrag}
          onPointerUp={endPlayheadDrag}
        >
          <div className="trim-ruler">
            {ticks.map((tick) => (
              <div
                className={tick.major ? "trim-tick major" : "trim-tick"}
                key={`${tick.seconds}-${tick.label}`}
                style={{
                  left: `${secondsToPercent(tick.seconds, safeDuration)}%`,
                }}
              >
                <span>{tick.label}</span>
              </div>
            ))}
          </div>
          <div
            aria-label="Timeline montage"
            className="trim-track"
            ref={trackRef}
            role="slider"
            aria-valuemin={0}
            aria-valuemax={safeDuration}
            aria-valuenow={currentSeconds}
            tabIndex={disabled ? -1 : 0}
            onPointerDown={(event) => {
              if (
                interactionMode !== "none" ||
                event.target !== event.currentTarget
              ) {
                return;
              }
              beginPlayheadDrag(event);
            }}
          >
            {activeSegments.map((segment) => {
              const left = secondsToPercent(
                segment.timelineStart,
                safeDuration,
              );
              const width = secondsToPercent(
                segmentDuration(segment),
                safeDuration,
              );
              const selected = segment.id === selectedSegmentId;
              return (
                <button
                  key={segment.id}
                  className={["timeline-segment", selected ? "selected" : ""]
                    .filter(Boolean)
                    .join(" ")}
                  draggable={false}
                  onPointerDown={(event) => selectSegment(event, segment.id)}
                  style={{ left: `${left}%`, width: `${Math.max(1, width)}%` }}
                  type="button"
                >
                  <TimelineFilmstrip
                    segment={segment}
                    thumbnails={thumbnailsBySource.get(segment.sourcePath) ?? []}
                  />
                  <span className="timeline-segment-label">
                    {segment.sourceTitle ?? "Clip"}
                  </span>
                </button>
              );
            })}
            <div
              className="trim-playhead"
              onPointerCancel={endPlayheadDrag}
              onPointerDown={beginPlayheadDrag}
              onPointerMove={continuePlayheadDrag}
              onPointerUp={endPlayheadDrag}
              style={playheadStyle}
            >
              <span>{formatTimelineTime(currentSeconds)}</span>
            </div>
          </div>
        </div>
      </div>

      <div className="segment-reorder-actions">
        <button
          className="ghost-button"
          disabled={disabled || !canMoveLeft}
          onClick={onMoveSelectedLeft}
          type="button"
        >
          <ArrowLeft className="h-4 w-4" />
          Déplacer à gauche
        </button>
        <button
          className="ghost-button"
          disabled={disabled || !canMoveRight}
          onClick={onMoveSelectedRight}
          type="button"
        >
          Déplacer à droite
          <ArrowRight className="h-4 w-4" />
        </button>
      </div>

      <div className="source-trim-panel">
        <div className="source-trim-header">
          <div>
            <div className="editor-kicker">Trim du segment sélectionné</div>
            <strong>{selectedSegment?.sourceTitle ?? "Aucun segment"}</strong>
          </div>
          <span>
            {selectedSegment
              ? `${formatTimelineTime(selectedSegment.start)} → ${formatTimelineTime(selectedSegment.end)}`
              : "--"}
          </span>
        </div>
        <div
          className="source-trim-track"
          ref={sourceTrackRef}
          onPointerDown={sourceTrackSeek}
          onPointerMove={continueSourceTrim}
          onPointerUp={endSourceTrim}
          onPointerCancel={endSourceTrim}
        >
          {selectedSegment && (
            <TimelineFilmstrip
              className="source-trim-filmstrip"
              segment={{
                ...selectedSegment,
                start: 0,
                end: selectedSegment.sourceDuration,
              }}
              thumbnails={
                thumbnailsBySource.get(selectedSegment.sourcePath) ?? []
              }
            />
          )}
          <div className="source-trim-range" style={sourceRangeStyle} />
          <button
            aria-label="Poignée début"
            className="trim-handle source-trim-handle source-trim-handle-start"
            disabled={disabled || !selectedSegment}
            onPointerDown={(event) => beginSourceTrim(event, "trim-start")}
            onPointerMove={continueSourceTrim}
            onPointerUp={endSourceTrim}
            onPointerCancel={endSourceTrim}
            style={sourceStartHandleStyle}
            type="button"
          />
          <button
            aria-label="Poignée fin"
            className="trim-handle source-trim-handle source-trim-handle-end"
            disabled={disabled || !selectedSegment}
            onPointerDown={(event) => beginSourceTrim(event, "trim-end")}
            onPointerMove={continueSourceTrim}
            onPointerUp={endSourceTrim}
            onPointerCancel={endSourceTrim}
            style={sourceEndHandleStyle}
            type="button"
          />
          <div className="source-trim-start-label">00:00</div>
          <div className="source-trim-end-label">
            {formatTimelineTime(sourceDuration)}
          </div>
        </div>
      </div>

      <div className="trim-actions">
        <button
          className="ghost-button"
          disabled={disabled || !selectedSegment}
          onClick={onSetStartToCurrent}
          type="button"
        >
          <SkipBack className="h-4 w-4" />
          Début = temps actuel
        </button>
        <button
          className="ghost-button"
          disabled={disabled || !selectedSegment}
          onClick={onSetEndToCurrent}
          type="button"
        >
          <SkipForward className="h-4 w-4" />
          Fin = temps actuel
        </button>
        <button
          className="icon-button"
          disabled={disabled}
          onClick={onReset}
          title="Reset"
          type="button"
        >
          <RotateCcw className="h-4 w-4" />
        </button>
      </div>
      <div className="trim-actions editor-cut-actions">
        <button
          className="ghost-button"
          disabled={disabled}
          onClick={onSplit}
          type="button"
        >
          Couper à la position
        </button>
        <button
          className="ghost-button destructive"
          disabled={disabled || !selectedSegmentId}
          onClick={onDeleteSegment}
          type="button"
        >
          Supprimer segment
        </button>
        <button
          className="ghost-button"
          disabled={disabled || !canRestoreSegment}
          onClick={onRestoreSegment}
          type="button"
        >
          Restaurer
        </button>
        <button
          className="ghost-button"
          disabled={disabled}
          onClick={onSetThumbnail}
          type="button"
        >
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


function TimelineFilmstrip({
  className = "",
  segment,
  thumbnails,
}: {
  className?: string;
  segment: TimelineSegment;
  thumbnails: TimelineThumbnail[];
}) {
  const visible = thumbnails
    .filter(
      (thumbnail) =>
        thumbnail.sourceTimeSeconds >= segment.start - 0.001 &&
        thumbnail.sourceTimeSeconds <= segment.end + 0.001,
    )
    .sort((a, b) => a.sourceTimeSeconds - b.sourceTimeSeconds);
  const classes = ["timeline-filmstrip", className].filter(Boolean).join(" ");

  if (visible.length === 0) {
    return <div className={classes} />;
  }

  return (
    <div className={classes}>
      {visible.map((thumbnail, index) => {
        const width = 100 / visible.length;
        return (
          <img
            alt=""
            aria-hidden="true"
            className="timeline-filmstrip-frame"
            draggable={false}
            key={thumbnail.id}
            src={thumbnail.imageUrl ?? thumbnail.imagePath}
            style={{
              left: `${index * width}%`,
              width: `${width}%`,
            }}
          />
        );
      })}
    </div>
  );
}

function secondsToPercent(seconds: number, durationSeconds: number) {
  return Math.max(0, Math.min(100, (seconds / durationSeconds) * 100));
}

function keepPlayheadVisible(
  viewport: HTMLDivElement | null,
  currentPercent: number,
) {
  if (!viewport) {
    return;
  }
  const contentWidth = viewport.scrollWidth;
  const playheadX = (contentWidth * currentPercent) / 100;
  const leftPadding = 64;
  const rightPadding = 64;
  if (playheadX < viewport.scrollLeft + leftPadding) {
    viewport.scrollLeft = Math.max(0, playheadX - leftPadding);
  } else if (
    playheadX >
    viewport.scrollLeft + viewport.clientWidth - rightPadding
  ) {
    viewport.scrollLeft = playheadX - viewport.clientWidth + rightPadding;
  }
}
