import type { CSSProperties } from "react";
import { RotateCcw, SkipBack, SkipForward } from "lucide-react";
import { formatClipDuration } from "../exportLogic";

const MIN_TRIM_GAP_SECONDS = 0.25;

type TrimTimelineProps = {
  durationSeconds: number;
  startSeconds: number;
  endSeconds: number;
  currentSeconds: number;
  disabled?: boolean;
  onStartChange: (value: number) => void;
  onEndChange: (value: number) => void;
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
  onStartChange,
  onEndChange,
  onSetStartToCurrent,
  onSetEndToCurrent,
  onReset,
}: TrimTimelineProps) {
  const safeDuration = Math.max(MIN_TRIM_GAP_SECONDS, durationSeconds);
  const startPercent = (startSeconds / safeDuration) * 100;
  const endPercent = (endSeconds / safeDuration) * 100;
  const currentPercent = (Math.min(currentSeconds, safeDuration) / safeDuration) * 100;
  const selectedStyle: CSSProperties = {
    left: `${Math.max(0, Math.min(100, startPercent))}%`,
    width: `${Math.max(0, Math.min(100, endPercent - startPercent))}%`,
  };
  const playheadStyle: CSSProperties = {
    left: `${Math.max(0, Math.min(100, currentPercent))}%`,
  };

  function changeStart(value: number) {
    onStartChange(Math.min(Math.max(0, value), endSeconds - MIN_TRIM_GAP_SECONDS));
  }

  function changeEnd(value: number) {
    onEndChange(Math.max(Math.min(safeDuration, value), startSeconds + MIN_TRIM_GAP_SECONDS));
  }

  return (
    <section className="editor-panel trim-panel">
      <div className="editor-panel-heading">
        <div>
          <div className="editor-kicker">Trim</div>
          <h3>Début / fin</h3>
        </div>
        <div className="trim-duration">
          {formatClipDuration(endSeconds - startSeconds)}
        </div>
      </div>

      <div className="trim-time-row">
        <span>{formatClipDuration(startSeconds)}</span>
        <span>{formatClipDuration(currentSeconds)}</span>
        <span>{formatClipDuration(endSeconds)}</span>
      </div>

      <div className="trim-track-shell">
        <div className="trim-track">
          <div className="trim-selected" style={selectedStyle} />
          <div className="trim-playhead" style={playheadStyle} />
        </div>
        <input
          aria-label="Début du trim"
          className="trim-range trim-range-start"
          disabled={disabled}
          max={safeDuration}
          min={0}
          onChange={(event) => changeStart(Number(event.target.value))}
          step={0.1}
          type="range"
          value={startSeconds}
        />
        <input
          aria-label="Fin du trim"
          className="trim-range trim-range-end"
          disabled={disabled}
          max={safeDuration}
          min={0}
          onChange={(event) => changeEnd(Number(event.target.value))}
          step={0.1}
          type="range"
          value={endSeconds}
        />
      </div>

      <div className="trim-actions">
        <button className="ghost-button" disabled={disabled} onClick={onSetStartToCurrent}>
          <SkipBack className="h-4 w-4" />
          Début = temps actuel
        </button>
        <button className="ghost-button" disabled={disabled} onClick={onSetEndToCurrent}>
          <SkipForward className="h-4 w-4" />
          Fin = temps actuel
        </button>
        <button className="icon-button" disabled={disabled} onClick={onReset} title="Reset">
          <RotateCcw className="h-4 w-4" />
        </button>
      </div>
    </section>
  );
}
