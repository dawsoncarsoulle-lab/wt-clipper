import { useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { AnimatePresence, motion } from "framer-motion";
import {
  AlertTriangle,
  Download,
  Film,
  FolderOpen,
  Monitor,
  Pause,
  Play,
  Scissors,
  Smartphone,
  Sparkles,
  XCircle,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type {
  ClipEditRequest,
  ClipEditorMode,
  ClipInfo,
  ClipMediaInfo,
  EditedClipResult,
  EditorExportProgressPayload,
  SaveMode,
  SocialLayout,
  TimelineThumbnail,
} from "../types";
import { EditorExportProgressModal } from "./EditorExportProgressModal";
import { SocialExportPreview } from "./SocialExportPreview";
import { TrimTimeline } from "./TrimTimeline";
import { useI18n } from "../i18n/I18nProvider";
import {
  activeTimelineSegments,
  deleteTimelineSegment,
  exportSegmentsFromTimeline,
  findSegmentAtTimelineTime,
  recalculateTimelineSegments,
  reorderTimelineSegment,
  restoreTimelineSegment,
  sourceToTimelineTime,
  splitSegmentAtPlayhead,
  timelineDuration,
  timelineToSourceTime,
} from "../editorTimelinePolicy";
import type { TimelineSegment } from "../types";

const MIN_TRIM_GAP_SECONDS = 0.25;

type ClipEditorModalProps = {
  clip: ClipInfo;
  clips?: ClipInfo[];
  onClose: () => void;
  onExportComplete: () => Promise<void> | void;
};

type EditorQualityPreset = "standard" | "high";

const modeOptions: Array<{
  mode: ClipEditorMode;
  labelKey: string;
  icon: LucideIcon;
}> = [
  { mode: "trim_original", labelKey: "editor.mode.trimOriginal", icon: Scissors },
  { mode: "youtube_horizontal", labelKey: "editor.mode.youtubeHorizontal", icon: Monitor },
  {
    mode: "social_vertical",
    labelKey: "editor.mode.socialVertical",
    icon: Smartphone,
  },
];

export function ClipEditorModal({
  clip,
  clips,
  onClose,
  onExportComplete,
}: ClipEditorModalProps) {
  const { t } = useI18n();
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const resumeAfterScrubRef = useRef(false);
  const segmentIdRef = useRef(0);
  const pendingSeekRef = useRef<number | null>(null);
  const playAfterSourceLoadRef = useRef(false);
  const [mediaInfo, setMediaInfo] = useState<ClipMediaInfo | null>(null);
  const [mediaError, setMediaError] = useState<string | null>(null);
  const [videoDuration, setVideoDuration] = useState(clip.durationSeconds);
  const [segments, setSegments] = useState<TimelineSegment[]>(() =>
    initialTimelineSegments(clips ?? [clip]),
  );
  const [selectedSegmentId, setSelectedSegmentId] = useState<
    string | undefined
  >(() => segments[0]?.id);
  const [timelineThumbnails, setTimelineThumbnails] = useState<
    TimelineThumbnail[]
  >([]);
  const [thumbnailSelection, setThumbnailSelection] = useState<{
    timelineTime: number;
    sourcePath: string;
    sourceTime: number;
  } | null>(null);
  const activeSegments = useMemo(
    () => activeTimelineSegments(segments),
    [segments],
  );
  const timelineTotalSeconds = timelineDuration(segments);
  const durationSeconds = Math.max(MIN_TRIM_GAP_SECONDS, timelineTotalSeconds);
  const selectedSegment = selectedSegmentId
    ? segments.find((segment) => segment.id === selectedSegmentId)
    : activeSegments[0];
  const [currentSeconds, setCurrentSeconds] = useState(0);
  const [activeSourcePath, setActiveSourcePath] = useState(clip.path);
  const [playing, setPlaying] = useState(false);
  const [mode, setMode] = useState<ClipEditorMode>("trim_original");
  const [outputFormat, setOutputFormat] = useState<"webm" | "mp4">("mp4");
  const [blurBackground, setBlurBackground] = useState(true);
  const [watermark, setWatermark] = useState(true);
  const [autoTitle, setAutoTitle] = useState(true);
  const [title, setTitle] = useState(defaultEditorTitle(clip));
  const [subtitle, setSubtitle] = useState("War Thunder");
  const [quality, setQuality] = useState<EditorQualityPreset>("standard");
  const [saveMode, setSaveMode] = useState<SaveMode>("create_copy");
  const [confirmReplaceOpen, setConfirmReplaceOpen] = useState(false);
  const [openFolderAfterExport, setOpenFolderAfterExport] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [progress, setProgress] = useState<EditorExportProgressPayload | null>(
    null,
  );
  const [result, setResult] = useState<EditedClipResult | null>(null);

  const videoSrc = videoUrlForSource(activeSourcePath, segments);
  const videoMimeType = videoMimeTypeForPath(activeSourcePath);
  const previewImageSrc = thumbnailUrlForSegment(
    selectedSegment ?? activeSegments[0],
  );
  const socialLayout: SocialLayout = blurBackground
    ? "vertical_blur"
    : "vertical_crop";
  const effectiveTitle = autoTitle ? defaultEditorTitle(clip) : title;
  const exportDuration = timelineTotalSeconds;
  const canExport =
    activeSegments.length > 0 &&
    exportDuration >= MIN_TRIM_GAP_SECONDS &&
    !exporting;
  const canReplaceOriginal = activeSegments.length === 1;

  useEffect(() => {
    const nextSegments = initialTimelineSegments(clips ?? [clip]);
    setSegments(nextSegments);
    setSelectedSegmentId(nextSegments[0]?.id);
    setThumbnailSelection(null);
    setActiveSourcePath(nextSegments[0]?.sourcePath ?? clip.path);
    setMediaInfo(null);
    setMediaError(null);
    setVideoDuration(clip.durationSeconds);
    setCurrentSeconds(0);
    setTitle(defaultEditorTitle(clip));
    setSaveMode("create_copy");
    setConfirmReplaceOpen(false);
  }, [clip, clips]);

  useEffect(() => {
    let cancelled = false;
    const uniqueSources = Array.from(
      new Set(segments.map((segment) => segment.sourcePath).filter(Boolean)),
    );
    if (uniqueSources.length === 0) {
      setTimelineThumbnails([]);
      return () => {
        cancelled = true;
      };
    }

    async function loadTimelineThumbnails() {
      try {
        const groups = await Promise.all(
          uniqueSources.map(async (sourcePath) => {
            const items = await invoke<TimelineThumbnail[]>(
              "get_timeline_thumbnails",
              {
                clipPath: sourcePath,
                count: 20,
                maxWidth: 160,
                maxHeight: 90,
              },
            );
            return items.map((item, index) => ({
              ...item,
              id: `${sourcePath}:${item.sourceTimeSeconds}:${index}`,
              imageUrl: convertFileSrc(item.imagePath),
            }));
          }),
        );
        if (!cancelled) {
          setTimelineThumbnails(groups.flat());
        }
      } catch (error) {
        console.info("[EDITOR_TIMELINE] thumbnails unavailable", error);
        if (!cancelled) {
          setTimelineThumbnails([]);
        }
      }
    }

    void loadTimelineThumbnails();
    return () => {
      cancelled = true;
    };
  }, [segments.map((segment) => segment.sourcePath).join("|")]);

  useEffect(() => {
    let cancelled = false;
    async function loadMediaInfo() {
      try {
        const info = await invoke<ClipMediaInfo>("get_clip_media_info", {
          path: clip.path,
        });
        if (!cancelled) {
          setMediaInfo(info);
          setVideoDuration(info.durationSeconds);
          setSegments((current) =>
            recalculateTimelineSegments(
              current.map((segment, index) =>
                index === 0
                  ? {
                      ...segment,
                      sourceDuration: info.durationSeconds,
                      end: Math.max(MIN_TRIM_GAP_SECONDS, info.durationSeconds),
                    }
                  : segment,
              ),
            ),
          );
        }
      } catch (error) {
        if (!cancelled) {
          setMediaError(String(error));
        }
      }
    }
    void loadMediaInfo();
    return () => {
      cancelled = true;
    };
  }, [clip.path]);

  useEffect(() => {
    const video = videoRef.current;
    console.info("[EDITOR_VIDEO] sourcePath=", activeSourcePath);
    console.info("[EDITOR_VIDEO] videoUrl=", videoSrc);
    console.info(
      "[EDITOR_VIDEO] usingPreviewUrl=",
      Boolean(segments.find((segment) => segment.sourcePath === activeSourcePath)?.sourcePreviewUrl),
    );
    setMediaError(null);
    if (!video || !videoSrc) {
      return;
    }

    const currentSrc = video.currentSrc || video.getAttribute("src") || "";
    if (currentSrc !== videoSrc) {
      video.pause();
      video.setAttribute("src", videoSrc);
      video.load();
    }
  }, [activeSourcePath, segments, videoSrc]);

  useEffect(
    () => () => {
      unloadEditorVideo(videoRef.current);
    },
    [],
  );

  useEffect(() => {
    const unsubscribe = listen<EditorExportProgressPayload>(
      "editor_export_progress_changed",
      (event) => {
        setProgress(event.payload);
      },
    );
    return () => {
      void unsubscribe.then((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      const target = event.target as HTMLElement | null;
      if (target?.tagName === "INPUT" || target?.tagName === "TEXTAREA") {
        return;
      }
      if (event.code === "Space") {
        event.preventDefault();
        void togglePlayback();
      } else if (event.key.toLowerCase() === "s") {
        event.preventDefault();
        splitAtPlayhead();
      } else if (event.key === "Delete" || event.key === "Backspace") {
        event.preventDefault();
        deleteSelectedSegment();
      } else if (event.key.toLowerCase() === "m") {
        event.preventDefault();
        setCurrentFrameAsThumbnail();
      } else if (event.key === "ArrowLeft") {
        event.preventDefault();
        setVideoCurrentTime(currentSeconds - 0.1);
      } else if (event.key === "ArrowRight") {
        event.preventDefault();
        setVideoCurrentTime(currentSeconds + 0.1);
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [currentSeconds, selectedSegmentId, segments, playing]);

  const mediaSummary = useMemo(() => {
    if (!mediaInfo) {
      return `${formatClipDuration(durationSeconds)} · ${videoMimeType.replace("video/", "").toUpperCase()}`;
    }
    const fps =
      mediaInfo.fps > 0 ? `${Math.round(mediaInfo.fps)} FPS` : t("editor.loadingFps");
    return `${mediaInfo.width}x${mediaInfo.height} · ${fps} · ${mediaInfo.codec}`;
  }, [durationSeconds, mediaInfo, videoMimeType, t]);

  function close() {
    if (exporting) {
      return;
    }
    onClose();
  }

  function setVideoCurrentTime(seconds: number) {
    const next = Math.max(0, Math.min(durationSeconds, seconds));
    syncVideoToTimeline(next, false);
    setCurrentSeconds(next);
  }

  function updateStart(value: number) {
    if (!selectedSegment) {
      return;
    }
    setSegments((current) =>
      recalculateTimelineSegments(
        current.map((segment) =>
          segment.id === selectedSegment.id
            ? {
                ...segment,
                start: Math.min(
                  Math.max(0, value),
                  segment.end - MIN_TRIM_GAP_SECONDS,
                ),
              }
            : segment,
        ),
      ),
    );
  }

  function updateEnd(value: number) {
    if (!selectedSegment) {
      return;
    }
    setSegments((current) =>
      recalculateTimelineSegments(
        current.map((segment) =>
          segment.id === selectedSegment.id
            ? {
                ...segment,
                end: Math.max(
                  Math.min(segment.sourceDuration, value),
                  segment.start + MIN_TRIM_GAP_SECONDS,
                ),
              }
            : segment,
        ),
      ),
    );
  }

  function resetTrim() {
    setSegments((current) =>
      recalculateTimelineSegments(
        current.map((segment) => ({
          ...segment,
          start: 0,
          end: segment.sourceDuration,
          deleted: false,
        })),
      ),
    );
    setSelectedSegmentId(segments[0]?.id);
    setVideoCurrentTime(0);
  }

  function beginTimelineScrub() {
    const video = videoRef.current;
    resumeAfterScrubRef.current = Boolean(video && !video.paused);
    if (video) {
      video.pause();
    }
    setPlaying(false);
  }

  function endTimelineScrub() {
    const video = videoRef.current;
    if (video && resumeAfterScrubRef.current) {
      void video.play().catch((error) => {
        console.info("[FRONTEND] editor scrub resume failed", error);
      });
    }
    resumeAfterScrubRef.current = false;
  }

  async function togglePlayback() {
    const video = videoRef.current;
    if (!video) {
      return;
    }
    if (playing) {
      video.pause();
      setPlaying(false);
      return;
    }
    const switchedSource = syncVideoToTimeline(currentSeconds, true);
    if (switchedSource) {
      return;
    }
    try {
      await video.play();
      setPlaying(true);
    } catch (error) {
      console.info("[FRONTEND] editor playback failed", error);
    }
  }

  function handleTimeUpdate() {
    const video = videoRef.current;
    if (!video) {
      return;
    }
    const segment = findSegmentAtTimelineTime(segments, currentSeconds);
    if (!segment) {
      return;
    }
    const nextTimeline = sourceToTimelineTime(segment, video.currentTime);
    setCurrentSeconds(nextTimeline);
    if (video.currentTime >= segment.end) {
      const nextSegment = activeSegments.find(
        (item) =>
          item.timelineStart >= segment.timelineEnd - 0.001 &&
          item.id !== segment.id,
      );
      if (nextSegment) {
        setCurrentSeconds(nextSegment.timelineStart);
        const switchedSource = syncVideoToTimeline(
          nextSegment.timelineStart,
          true,
        );
        if (!switchedSource && videoRef.current) {
          void videoRef.current
            .play()
            .catch((error) =>
              console.info("[FRONTEND] editor segment advance failed", error),
            );
        }
        return;
      }
      video.pause();
      setPlaying(false);
      setCurrentSeconds(durationSeconds);
    }
  }

  function handleLoadedMetadata() {
    const video = videoRef.current;
    if (!video || !Number.isFinite(video.duration)) {
      return;
    }
    console.info("[EDITOR_VIDEO] loadedmetadata duration=", video.duration);
    setVideoDuration(video.duration);
    const seekTo = pendingSeekRef.current;
    if (seekTo != null) {
      seekVideo(video, seekTo);
      pendingSeekRef.current = null;
    } else {
      const segment = findSegmentAtTimelineTime(segments, currentSeconds);
      if (segment && segment.sourcePath === activeSourcePath) {
        seekVideo(video, timelineToSourceTime(segment, currentSeconds));
      }
    }
    if (playAfterSourceLoadRef.current) {
      playAfterSourceLoadRef.current = false;
      void video
        .play()
        .then(() => setPlaying(true))
        .catch((error) => {
          console.info(
            "[FRONTEND] editor playback after source load failed",
            error,
          );
        });
    }
  }

  function handleLoadedData() {
    const video = videoRef.current;
    if (!video) {
      return;
    }
    const seekTo = pendingSeekRef.current;
    if (seekTo != null) {
      seekVideo(video, seekTo);
      pendingSeekRef.current = null;
    }
  }

  function handleVideoError() {
    const video = videoRef.current;
    const error = video?.error;
    const message = error
      ? t("editor.videoError", { code: error.code, path: activeSourcePath })
      : t("editor.videoErrorGeneric", { path: activeSourcePath });
    console.info("[EDITOR_VIDEO] error=", message);
    setMediaError(message);
  }

  function syncVideoToTimeline(timelineTime: number, playAfterLoad = false) {
    const segment = findSegmentAtTimelineTime(segments, timelineTime);
    const video = videoRef.current;
    if (!segment || !video) {
      return false;
    }
    const sourceTime = timelineToSourceTime(segment, timelineTime);
    if (activeSourcePath !== segment.sourcePath) {
      pendingSeekRef.current = sourceTime;
      playAfterSourceLoadRef.current = playAfterLoad;
      setActiveSourcePath(segment.sourcePath);
      return true;
    }
    seekVideo(video, sourceTime);
    return false;
  }

  function splitAtPlayhead() {
    const result = splitSegmentAtPlayhead(segments, currentSeconds, 0.5, () =>
      nextSegmentId(segmentIdRef),
    );
    setSegments(result.segments);
    if (result.selectedSegmentId) {
      setSelectedSegmentId(result.selectedSegmentId);
    }
  }

  function deleteSelectedSegment() {
    if (!selectedSegmentId) {
      return;
    }
    setSegments((current) => deleteTimelineSegment(current, selectedSegmentId));
  }

  function restoreDeletedSegment() {
    const deleted = segments.find((segment) => segment.deleted);
    if (!deleted) {
      return;
    }
    setSegments((current) => restoreTimelineSegment(current, deleted.id));
    setSelectedSegmentId(deleted.id);
  }

  function setCurrentFrameAsThumbnail() {
    const segment = findSegmentAtTimelineTime(segments, currentSeconds);
    if (!segment) {
      return;
    }
    setThumbnailSelection({
      timelineTime: currentSeconds,
      sourcePath: segment.sourcePath,
      sourceTime: timelineToSourceTime(segment, currentSeconds),
    });
  }

  function moveSelectedSegment(direction: "left" | "right") {
    if (!selectedSegmentId) {
      return;
    }
    setSegments((current) => {
      const active = activeTimelineSegments(current);
      const index = active.findIndex(
        (segment) => segment.id === selectedSegmentId,
      );
      if (index < 0) {
        return current;
      }
      const targetIndex = direction === "left" ? index - 1 : index + 1;
      if (targetIndex < 0 || targetIndex >= active.length) {
        return current;
      }
      const orderedIds = active.map((segment) => segment.id);
      const [moved] = orderedIds.splice(index, 1);
      orderedIds.splice(targetIndex, 0, moved);
      const rank = new Map(orderedIds.map((id, nextIndex) => [id, nextIndex]));
      const activeSorted = active
        .slice()
        .sort((a, b) => (rank.get(a.id) ?? 0) - (rank.get(b.id) ?? 0));
      const deleted = current.filter((segment) => segment.deleted);
      const next = recalculateTimelineSegments([...activeSorted, ...deleted]);
      const movedSegment = next.find((segment) => segment.id === selectedSegmentId);
      if (movedSegment) {
        window.requestAnimationFrame(() => {
          setCurrentSeconds(movedSegment.timelineStart);
        });
      }
      return next;
    });
  }

  function requestExport() {
    if (!canExport) {
      return;
    }
    if (saveMode === "replace_original" && canReplaceOriginal) {
      setConfirmReplaceOpen(true);
      return;
    }
    void exportClip();
  }

  async function exportClip() {
    if (!canExport) {
      return;
    }
    setConfirmReplaceOpen(false);
    const effectiveSaveMode =
      saveMode === "replace_original" && !canReplaceOriginal
        ? "create_copy"
        : saveMode;
    setExporting(true);
    setResult(null);
    setProgress({
      active: true,
      step: "preparing",
      progress: 5,
      message: t("editor.progress.preparing"),
    });
    const request = buildEditRequest({
      clip,
      mode,
      outputFormat,
      socialLayout,
      startSeconds: selectedSegment?.start ?? 0,
      endSeconds: selectedSegment?.end ?? durationSeconds,
      segments,
      thumbnailSelection,
      title: effectiveTitle,
      subtitle,
      watermark,
      quality,
      saveMode: effectiveSaveMode,
    });
    try {
      const nextResult = await invoke<EditedClipResult>("export_edited_clip", {
        request,
      });
      setResult(nextResult);
      setProgress({
        active: false,
        step: "done",
        progress: 100,
        message: nextResult.replacedOriginal
          ? t("editor.progress.replaced")
          : t("editor.progress.done"),
        outputPath: nextResult.outputPath,
      });
      await onExportComplete();
      if (openFolderAfterExport) {
        await invoke("open_parent_folder", { path: nextResult.outputPath });
      }
    } catch (error) {
      setProgress({
        active: false,
        step: "failed",
        progress: 100,
        message: t("editor.progress.failed"),
        error: String(error),
      });
    } finally {
      setExporting(false);
    }
  }

  return (
    <AnimatePresence>
      <motion.div
        animate={{ opacity: 1 }}
        className="modal-backdrop editor-backdrop"
        exit={{ opacity: 0 }}
        initial={{ opacity: 0 }}
      >
        <motion.section
          animate={{ opacity: 1, scale: 1, y: 0 }}
          className="clip-editor-modal"
          exit={{ opacity: 0, scale: 0.98, y: 18 }}
          initial={{ opacity: 0, scale: 0.98, y: 18 }}
        >
          <header className="editor-header">
            <div className="min-w-0">
              <div className="editor-kicker">{t("editor.kicker")}</div>
              <h2 className="truncate">{clip.fileName}</h2>
              <div className="mt-1 truncate text-sm text-zinc-500">
                {mediaSummary}
              </div>
            </div>
            <button
              className="icon-button"
              disabled={exporting}
              onClick={close}
              title={t("editor.close")}
            >
              <XCircle className="h-4 w-4" />
            </button>
          </header>

          <div className="editor-body">
            <div className="editor-main-column">
              <section className="editor-video-shell">
                <video
                  key={activeSourcePath}
                  ref={videoRef}
                  poster={previewImageSrc ?? undefined}
                  onEnded={() => setPlaying(false)}
                  onError={handleVideoError}
                  onLoadedData={handleLoadedData}
                  onLoadedMetadata={handleLoadedMetadata}
                  onPause={() => setPlaying(false)}
                  onPlay={() => setPlaying(true)}
                  onTimeUpdate={handleTimeUpdate}
                  playsInline
                  preload="auto"
                />
                <div className="editor-video-controls">
                  <button
                    className="primary-action w-fit px-4"
                    onClick={() => void togglePlayback()}
                  >
                    {playing ? (
                      <Pause className="h-4 w-4" />
                    ) : (
                      <Play className="h-4 w-4" />
                    )}
                    {playing ? t("editor.pause") : t("editor.play")}
                  </button>
                  <div className="editor-timecode">
                    {formatClipDuration(currentSeconds)} /{" "}
                    {formatClipDuration(durationSeconds)}
                  </div>
                </div>
              </section>

              <TrimTimeline
                currentSeconds={currentSeconds}
                disabled={exporting}
                durationSeconds={durationSeconds}
                endSeconds={selectedSegment?.end ?? durationSeconds}
                onCurrentChange={setVideoCurrentTime}
                onEndChange={updateEnd}
                onReset={resetTrim}
                onScrubEnd={endTimelineScrub}
                onScrubStart={beginTimelineScrub}
                onSetEndToCurrent={() => {
                  const segment = findSegmentAtTimelineTime(
                    segments,
                    currentSeconds,
                  );
                  if (segment) {
                    setSelectedSegmentId(segment.id);
                    updateEnd(timelineToSourceTime(segment, currentSeconds));
                  }
                }}
                onSetStartToCurrent={() => {
                  const segment = findSegmentAtTimelineTime(
                    segments,
                    currentSeconds,
                  );
                  if (segment) {
                    setSelectedSegmentId(segment.id);
                    updateStart(timelineToSourceTime(segment, currentSeconds));
                  }
                }}
                onStartChange={updateStart}
                onSegmentReorder={(draggedId, targetId) =>
                  setSegments((current) =>
                    reorderTimelineSegment(current, draggedId, targetId),
                  )
                }
                onMoveSelectedLeft={() => moveSelectedSegment("left")}
                onMoveSelectedRight={() => moveSelectedSegment("right")}
                onSegmentSelect={setSelectedSegmentId}
                onSplit={splitAtPlayhead}
                onDeleteSegment={deleteSelectedSegment}
                onRestoreSegment={restoreDeletedSegment}
                canRestoreSegment={segments.some((segment) => segment.deleted)}
                onSetThumbnail={setCurrentFrameAsThumbnail}
                selectedSegmentId={selectedSegmentId}
                segments={segments}
                startSeconds={selectedSegment?.start ?? 0}
                thumbnailTime={thumbnailSelection?.timelineTime}
                thumbnails={timelineThumbnails}
              />
            </div>

            <aside className="editor-side-column">
              <section className="editor-panel">
                <div className="editor-panel-heading">
                  <div>
                    <div className="editor-kicker">{t("editor.export.section")}</div>
                    <h3>{t("editor.export.preset")}</h3>
                  </div>
                  <Film className="h-5 w-5 text-ember" />
                </div>
                <div className="editor-mode-grid">
                  {modeOptions.map((option) => {
                    const Icon = option.icon;
                    return (
                      <button
                        key={option.mode}
                        className={mode === option.mode ? "active" : ""}
                        disabled={exporting}
                        onClick={() => setMode(option.mode)}
                      >
                        <Icon className="h-4 w-4" />
                        {t(option.labelKey)}
                      </button>
                    );
                  })}
                </div>
                {mode === "trim_original" && (
                  <div className="editor-inline-options mt-3">
                    <button
                      className={outputFormat === "webm" ? "active" : ""}
                      disabled={exporting}
                      onClick={() => setOutputFormat("webm")}
                    >
                      WebM
                    </button>
                    <button
                      className={outputFormat === "mp4" ? "active" : ""}
                      disabled={exporting}
                      onClick={() => setOutputFormat("mp4")}
                    >
                      MP4
                    </button>
                  </div>
                )}
              </section>

              <section className="editor-panel">
                <div className="editor-panel-heading">
                  <div>
                    <div className="editor-kicker">{t("editor.options.section")}</div>
                    <h3>{t("editor.options.socialVertical")}</h3>
                  </div>
                  <Sparkles className="h-5 w-5 text-ember" />
                </div>
                <div className="editor-toggle-list">
                  <label>
                    <input
                      checked={blurBackground}
                      disabled={exporting || mode !== "social_vertical"}
                      onChange={(event) =>
                        setBlurBackground(event.target.checked)
                      }
                      type="checkbox"
                    />
                    <span>{t("editor.options.blurBackground")}</span>
                  </label>
                  <label>
                    <input
                      checked={watermark}
                      disabled={exporting || mode !== "social_vertical"}
                      onChange={(event) => setWatermark(event.target.checked)}
                      type="checkbox"
                    />
                    <span>{t("editor.options.watermark")}</span>
                  </label>
                  <label>
                    <input
                      checked={autoTitle}
                      disabled={exporting || mode !== "social_vertical"}
                      onChange={(event) => setAutoTitle(event.target.checked)}
                      type="checkbox"
                    />
                    <span>{t("editor.options.autoTitle")}</span>
                  </label>
                </div>
                <div className="editor-field-stack">
                  <label className="field compact-field">
                    <span>{t("editor.options.title")}</span>
                    <input
                      disabled={
                        exporting || autoTitle || mode !== "social_vertical"
                      }
                      onChange={(event) => setTitle(event.target.value)}
                      value={effectiveTitle}
                    />
                  </label>
                  <label className="field compact-field">
                    <span>{t("editor.options.subtitle")}</span>
                    <input
                      disabled={exporting || mode !== "social_vertical"}
                      onChange={(event) => setSubtitle(event.target.value)}
                      value={subtitle}
                    />
                  </label>
                  <div className="editor-inline-options">
                    <button
                      className={quality === "standard" ? "active" : ""}
                      disabled={exporting}
                      onClick={() => setQuality("standard")}
                    >
                      {t("editor.quality.standard")}
                    </button>
                    <button
                      className={quality === "high" ? "active" : ""}
                      disabled={exporting}
                      onClick={() => setQuality("high")}
                    >
                      {t("editor.quality.high")}
                    </button>
                  </div>
                </div>
              </section>

              <section className="editor-panel">
                <div className="editor-panel-heading">
                  <div>
                    <div className="editor-kicker">{t("editor.save.section")}</div>
                    <h3>{t("editor.save.outputMode")}</h3>
                  </div>
                  <FolderOpen className="h-5 w-5 text-ember" />
                </div>
                <div className="editor-save-mode-list">
                  <label className={saveMode === "create_copy" ? "active" : ""}>
                    <input
                      checked={saveMode === "create_copy"}
                      disabled={exporting}
                      name="editor-save-mode"
                      onChange={() => setSaveMode("create_copy")}
                      type="radio"
                    />
                    <span>
                      <strong>{t("editor.save.createCopy")}</strong>
                      <small>{t("editor.save.createCopyDescription")}</small>
                    </span>
                  </label>
                  <label
                    className={
                      saveMode === "replace_original"
                        ? "active destructive"
                        : "destructive"
                    }
                  >
                    <input
                      checked={saveMode === "replace_original"}
                      disabled={exporting || !canReplaceOriginal}
                      name="editor-save-mode"
                      onChange={() => setSaveMode("replace_original")}
                      type="radio"
                    />
                    <span>
                      <strong>{t("editor.save.replaceOriginal")}</strong>
                      <small>{t("editor.save.replaceOriginalDescription")}</small>
                    </span>
                  </label>
                </div>
                {saveMode === "replace_original" && (
                  <div className="editor-warning">
                    <AlertTriangle className="h-4 w-4" />
                    <span>{t("editor.save.replaceWarning")}</span>
                  </div>
                )}
              </section>

              <SocialExportPreview
                layout={socialLayout}
                mode={mode}
                previewImageSrc={previewImageSrc}
                subtitle={mode === "social_vertical" ? subtitle : ""}
                title={mode === "social_vertical" ? effectiveTitle : ""}
                watermark={mode === "social_vertical" && watermark}
              />
            </aside>
          </div>

          {mediaError && <p className="editor-error">{mediaError}</p>}

          <footer className="editor-footer">
            <label className="editor-folder-toggle">
              <input
                checked={openFolderAfterExport}
                disabled={exporting}
                onChange={(event) =>
                  setOpenFolderAfterExport(event.target.checked)
                }
                type="checkbox"
              />
              <FolderOpen className="h-4 w-4" />
              {t("editor.openFolderAfterExport")}
            </label>
            <div className="flex gap-2">
              <button
                className="ghost-button"
                disabled={exporting}
                onClick={close}
              >
                {t("editor.cancel")}
              </button>
              <button
                className="primary-action w-fit px-5"
                disabled={!canExport}
                onClick={requestExport}
              >
                <Download className="h-4 w-4" />
                {exporting
                  ? t("editor.exporting")
                  : saveMode === "replace_original"
                    ? t("editor.replaceButton", { duration: formatClipDuration(exportDuration) })
                    : t("editor.exportButton", { duration: formatClipDuration(exportDuration) })}
              </button>
            </div>
          </footer>
        </motion.section>
        {confirmReplaceOpen && (
          <motion.div
            animate={{ opacity: 1 }}
            className="editor-confirm-layer"
            exit={{ opacity: 0 }}
            initial={{ opacity: 0 }}
          >
            <motion.section
              animate={{ scale: 1, y: 0 }}
              className="editor-confirm-card"
              exit={{ scale: 0.98, y: 12 }}
              initial={{ scale: 0.98, y: 12 }}
            >
              <div className="editor-confirm-icon">
                <AlertTriangle className="h-5 w-5" />
              </div>
              <div>
                <div className="editor-kicker">{t("editor.confirm.kicker")}</div>
                <h3>{t("editor.confirm.title")}</h3>
                <p>{t("editor.confirm.description")}</p>
              </div>
              <div className="editor-confirm-actions">
                <button
                  className="ghost-button"
                  disabled={exporting}
                  onClick={() => setConfirmReplaceOpen(false)}
                >
                  {t("editor.cancel")}
                </button>
                <button
                  className="primary-action w-fit px-5"
                  disabled={exporting}
                  onClick={() => void exportClip()}
                >
                  {t("editor.confirm.replace")}
                </button>
              </div>
            </motion.section>
          </motion.div>
        )}
        <EditorExportProgressModal
          progress={progress}
          result={result}
          onClose={() => setProgress(null)}
        />
      </motion.div>
    </AnimatePresence>
  );
}

function buildEditRequest({
  clip,
  mode,
  outputFormat,
  socialLayout,
  startSeconds,
  endSeconds,
  segments,
  thumbnailSelection,
  title,
  subtitle,
  watermark,
  quality,
  saveMode,
}: {
  clip: ClipInfo;
  mode: ClipEditorMode;
  outputFormat: "webm" | "mp4";
  socialLayout: SocialLayout;
  startSeconds: number;
  endSeconds: number;
  segments: TimelineSegment[];
  thumbnailSelection: { sourcePath: string; sourceTime: number } | null;
  title: string;
  subtitle: string;
  watermark: boolean;
  quality: EditorQualityPreset;
  saveMode: SaveMode;
}): ClipEditRequest {
  const social = mode === "social_vertical";
  return {
    clipPath: clip.path,
    metadataPath: metadataPathForClip(clip.path),
    startSeconds,
    endSeconds,
    segments: exportSegmentsFromTimeline(segments),
    thumbnailSourcePath: thumbnailSelection?.sourcePath,
    thumbnailTimeSeconds: thumbnailSelection?.sourceTime,
    mode,
    outputFormat: mode === "trim_original" ? outputFormat : "mp4",
    layout: social ? socialLayout : undefined,
    title: social ? title : undefined,
    subtitle: social ? subtitle : undefined,
    watermark: social && watermark,
    fps: 30,
    bitrateKbps: quality === "high" ? 12_000 : 8_000,
    saveMode,
    backupOriginal: true,
  };
}

function initialTimelineSegments(clips: ClipInfo[]): TimelineSegment[] {
  return recalculateTimelineSegments(
    clips.map((clip, index) => {
      const duration = Math.max(
        MIN_TRIM_GAP_SECONDS,
        clip.durationSeconds || MIN_TRIM_GAP_SECONDS,
      );
      return {
        id: `segment-${index + 1}`,
        sourcePath: clip.path,
        sourceClipId: clip.path,
        sourceTitle: clip.fileName,
        sourceThumbnail: clip.thumbnailPath ?? undefined,
        sourcePreviewUrl: clip.previewUrl ?? undefined,
        sourceDuration: duration,
        start: 0,
        end: duration,
        timelineStart: 0,
        timelineEnd: duration,
      };
    }),
  );
}

function videoUrlForSource(sourcePath: string, segments: TimelineSegment[]) {
  const segment = segments.find((item) => item.sourcePath === sourcePath);
  if (segment?.sourcePreviewUrl) {
    return segment.sourcePreviewUrl;
  }
  return convertFileSrc(sourcePath);
}

function thumbnailUrlForSegment(segment?: TimelineSegment | null) {
  if (!segment?.sourceThumbnail) {
    return null;
  }
  return convertFileSrc(segment.sourceThumbnail);
}

function seekVideo(video: HTMLVideoElement, seconds: number) {
  if (!Number.isFinite(seconds)) {
    return;
  }
  const duration =
    Number.isFinite(video.duration) && video.duration > 0
      ? video.duration
      : seconds;
  const next = Math.max(0, Math.min(duration, seconds));
  if (Math.abs(video.currentTime - next) > 0.03) {
    video.currentTime = next;
  }
}

function unloadEditorVideo(video: HTMLVideoElement | null) {
  if (!video) {
    return;
  }
  console.info("[EDITOR_CLEANUP] unload video");
  video.pause();
  video.removeAttribute("src");
  video.load();
}

function nextSegmentId(ref: { current: number }) {
  ref.current += 1;
  return `segment-split-${ref.current}`;
}

function metadataPathForClip(path: string): string {
  return path.replace(/\.[^/.]+$/, ".json");
}

function defaultEditorTitle(clip: ClipInfo): string {
  const name = clip.fileName.replace(/\.[^/.]+$/, "").replace(/[-_]+/g, " ");
  return name.length > 42 ? `${name.slice(0, 39)}...` : name;
}

function videoMimeTypeForPath(path: string): string {
  return path.toLowerCase().endsWith(".mp4") ? "video/mp4" : "video/webm";
}

function formatClipDuration(seconds: number) {
  const safe = Math.max(0, Math.round(seconds));
  const minutes = Math.floor(safe / 60);
  const rest = safe % 60;
  return `${String(minutes).padStart(2, "0")}:${String(rest).padStart(2, "0")}`;
}
