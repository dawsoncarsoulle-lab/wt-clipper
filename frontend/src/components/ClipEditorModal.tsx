import { useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { AnimatePresence, motion } from "framer-motion";
import {
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
import { formatClipDuration } from "../exportLogic";
import type {
  ClipEditRequest,
  ClipEditorMode,
  ClipInfo,
  ClipMediaInfo,
  EditedClipResult,
  EditorExportProgressPayload,
  SocialLayout,
} from "../types";
import { EditorExportProgressModal } from "./EditorExportProgressModal";
import { SocialExportPreview } from "./SocialExportPreview";
import { TrimTimeline } from "./TrimTimeline";

const MIN_TRIM_GAP_SECONDS = 0.25;

type ClipEditorModalProps = {
  clip: ClipInfo;
  onClose: () => void;
  onExportComplete: () => Promise<void> | void;
};

type QualityPreset = "standard" | "high";

const modeOptions: Array<{
  mode: ClipEditorMode;
  label: string;
  icon: LucideIcon;
}> = [
  { mode: "trim_original", label: "Original coupé", icon: Scissors },
  { mode: "youtube_horizontal", label: "YouTube", icon: Monitor },
  { mode: "social_vertical", label: "TikTok / Reels / Shorts", icon: Smartphone },
];

export function ClipEditorModal({ clip, onClose, onExportComplete }: ClipEditorModalProps) {
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const [mediaInfo, setMediaInfo] = useState<ClipMediaInfo | null>(null);
  const [mediaError, setMediaError] = useState<string | null>(null);
  const [videoDuration, setVideoDuration] = useState(clip.durationSeconds);
  const durationSeconds = Math.max(
    MIN_TRIM_GAP_SECONDS,
    mediaInfo?.durationSeconds ?? videoDuration ?? clip.durationSeconds,
  );
  const [currentSeconds, setCurrentSeconds] = useState(0);
  const [startSeconds, setStartSeconds] = useState(0);
  const [endSeconds, setEndSeconds] = useState(Math.max(MIN_TRIM_GAP_SECONDS, clip.durationSeconds));
  const [playing, setPlaying] = useState(false);
  const [mode, setMode] = useState<ClipEditorMode>("trim_original");
  const [outputFormat, setOutputFormat] = useState<"webm" | "mp4">("webm");
  const [blurBackground, setBlurBackground] = useState(true);
  const [watermark, setWatermark] = useState(true);
  const [autoTitle, setAutoTitle] = useState(true);
  const [title, setTitle] = useState(defaultEditorTitle(clip));
  const [subtitle, setSubtitle] = useState("War Thunder");
  const [quality, setQuality] = useState<QualityPreset>("standard");
  const [openFolderAfterExport, setOpenFolderAfterExport] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [progress, setProgress] = useState<EditorExportProgressPayload | null>(null);
  const [result, setResult] = useState<EditedClipResult | null>(null);

  const videoSrc = clip.previewUrl ?? convertFileSrc(clip.path);
  const videoMimeType = videoMimeTypeForPath(clip.path);
  const socialLayout: SocialLayout = blurBackground ? "vertical_blur" : "vertical_crop";
  const effectiveTitle = autoTitle ? defaultEditorTitle(clip) : title;
  const exportDuration = Math.max(0, endSeconds - startSeconds);
  const canExport = exportDuration >= MIN_TRIM_GAP_SECONDS && !exporting;

  useEffect(() => {
    setMediaInfo(null);
    setMediaError(null);
    setVideoDuration(clip.durationSeconds);
    setCurrentSeconds(0);
    setStartSeconds(0);
    setEndSeconds(Math.max(MIN_TRIM_GAP_SECONDS, clip.durationSeconds));
    setTitle(defaultEditorTitle(clip));
  }, [clip]);

  useEffect(() => {
    let cancelled = false;
    async function loadMediaInfo() {
      try {
        const info = await invoke<ClipMediaInfo>("get_clip_media_info", { path: clip.path });
        if (!cancelled) {
          setMediaInfo(info);
          setVideoDuration(info.durationSeconds);
          setEndSeconds(Math.max(MIN_TRIM_GAP_SECONDS, info.durationSeconds));
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

  const mediaSummary = useMemo(() => {
    if (!mediaInfo) {
      return `${formatClipDuration(durationSeconds)} · ${videoMimeType.replace("video/", "").toUpperCase()}`;
    }
    const fps = mediaInfo.fps > 0 ? `${Math.round(mediaInfo.fps)} FPS` : "FPS inconnu";
    return `${mediaInfo.width}x${mediaInfo.height} · ${fps} · ${mediaInfo.codec}`;
  }, [durationSeconds, mediaInfo, videoMimeType]);

  function close() {
    if (exporting) {
      return;
    }
    onClose();
  }

  function setVideoCurrentTime(seconds: number) {
    const video = videoRef.current;
    const next = Math.max(0, Math.min(durationSeconds, seconds));
    if (video) {
      video.currentTime = next;
    }
    setCurrentSeconds(next);
  }

  function updateStart(value: number) {
    const next = Math.min(Math.max(0, value), endSeconds - MIN_TRIM_GAP_SECONDS);
    setStartSeconds(next);
    if (currentSeconds < next) {
      setVideoCurrentTime(next);
    }
  }

  function updateEnd(value: number) {
    const next = Math.max(Math.min(durationSeconds, value), startSeconds + MIN_TRIM_GAP_SECONDS);
    setEndSeconds(next);
    if (currentSeconds > next) {
      setVideoCurrentTime(next);
    }
  }

  function resetTrim() {
    setStartSeconds(0);
    setEndSeconds(durationSeconds);
    setVideoCurrentTime(0);
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
    if (video.currentTime < startSeconds || video.currentTime >= endSeconds) {
      video.currentTime = startSeconds;
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
    setCurrentSeconds(video.currentTime);
    if (video.currentTime >= endSeconds) {
      video.pause();
      video.currentTime = startSeconds;
      setPlaying(false);
      setCurrentSeconds(startSeconds);
    }
  }

  function handleLoadedMetadata() {
    const video = videoRef.current;
    if (!video || !Number.isFinite(video.duration)) {
      return;
    }
    setVideoDuration(video.duration);
    setEndSeconds((current) =>
      current <= MIN_TRIM_GAP_SECONDS ? Math.max(MIN_TRIM_GAP_SECONDS, video.duration) : current,
    );
  }

  async function exportClip() {
    if (!canExport) {
      return;
    }
    setExporting(true);
    setResult(null);
    setProgress({
      active: true,
      step: "preparing",
      progress: 0,
      message: "Préparation de l'export...",
    });
    const request = buildEditRequest({
      clip,
      mode,
      outputFormat,
      socialLayout,
      startSeconds,
      endSeconds,
      title: effectiveTitle,
      subtitle,
      watermark,
      quality,
    });
    try {
      const nextResult = await invoke<EditedClipResult>("export_edited_clip", { request });
      setResult(nextResult);
      setProgress({
        active: false,
        step: "done",
        progress: 100,
        message: "Export terminé",
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
        message: "Erreur pendant l'export",
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
              <div className="editor-kicker">Éditeur vidéo</div>
              <h2 className="truncate">{clip.fileName}</h2>
              <div className="mt-1 truncate text-sm text-zinc-500">{mediaSummary}</div>
            </div>
            <button className="icon-button" disabled={exporting} onClick={close} title="Fermer">
              <XCircle className="h-4 w-4" />
            </button>
          </header>

          <div className="editor-body">
            <div className="editor-main-column">
              <section className="editor-video-shell">
                <video
                  ref={videoRef}
                  onEnded={() => setPlaying(false)}
                  onLoadedMetadata={handleLoadedMetadata}
                  onPause={() => setPlaying(false)}
                  onPlay={() => setPlaying(true)}
                  onTimeUpdate={handleTimeUpdate}
                  playsInline
                  preload="metadata"
                >
                  <source src={videoSrc} type={videoMimeType} />
                </video>
                <div className="editor-video-controls">
                  <button className="primary-action w-fit px-4" onClick={() => void togglePlayback()}>
                    {playing ? <Pause className="h-4 w-4" /> : <Play className="h-4 w-4" />}
                    {playing ? "Pause" : "Lecture"}
                  </button>
                  <div className="editor-timecode">
                    {formatClipDuration(currentSeconds)} / {formatClipDuration(durationSeconds)}
                  </div>
                </div>
              </section>

              <TrimTimeline
                currentSeconds={currentSeconds}
                disabled={exporting}
                durationSeconds={durationSeconds}
                endSeconds={endSeconds}
                onEndChange={updateEnd}
                onReset={resetTrim}
                onSetEndToCurrent={() => updateEnd(currentSeconds)}
                onSetStartToCurrent={() => updateStart(currentSeconds)}
                onStartChange={updateStart}
                startSeconds={startSeconds}
              />
            </div>

            <aside className="editor-side-column">
              <section className="editor-panel">
                <div className="editor-panel-heading">
                  <div>
                    <div className="editor-kicker">Export</div>
                    <h3>Preset</h3>
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
                        {option.label}
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
                    <div className="editor-kicker">Options</div>
                    <h3>Social vertical</h3>
                  </div>
                  <Sparkles className="h-5 w-5 text-ember" />
                </div>
                <div className="editor-toggle-list">
                  <label>
                    <input
                      checked={blurBackground}
                      disabled={exporting || mode !== "social_vertical"}
                      onChange={(event) => setBlurBackground(event.target.checked)}
                      type="checkbox"
                    />
                    <span>Fond flou</span>
                  </label>
                  <label>
                    <input
                      checked={watermark}
                      disabled={exporting || mode !== "social_vertical"}
                      onChange={(event) => setWatermark(event.target.checked)}
                      type="checkbox"
                    />
                    <span>Watermark WT Clip</span>
                  </label>
                  <label>
                    <input
                      checked={autoTitle}
                      disabled={exporting || mode !== "social_vertical"}
                      onChange={(event) => setAutoTitle(event.target.checked)}
                      type="checkbox"
                    />
                    <span>Titre automatique</span>
                  </label>
                </div>
                <div className="editor-field-stack">
                  <label className="field compact-field">
                    <span>Titre</span>
                    <input
                      disabled={exporting || autoTitle || mode !== "social_vertical"}
                      onChange={(event) => setTitle(event.target.value)}
                      value={effectiveTitle}
                    />
                  </label>
                  <label className="field compact-field">
                    <span>Sous-titre</span>
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
                      Standard
                    </button>
                    <button
                      className={quality === "high" ? "active" : ""}
                      disabled={exporting}
                      onClick={() => setQuality("high")}
                    >
                      Haute
                    </button>
                  </div>
                </div>
              </section>

              <SocialExportPreview
                layout={socialLayout}
                mode={mode}
                subtitle={mode === "social_vertical" ? subtitle : ""}
                title={mode === "social_vertical" ? effectiveTitle : ""}
                videoMimeType={videoMimeType}
                videoSrc={videoSrc}
                watermark={mode === "social_vertical" && watermark}
              />
            </aside>
          </div>

          {mediaError && (
            <p className="editor-error">{mediaError}</p>
          )}

          <footer className="editor-footer">
            <label className="editor-folder-toggle">
              <input
                checked={openFolderAfterExport}
                disabled={exporting}
                onChange={(event) => setOpenFolderAfterExport(event.target.checked)}
                type="checkbox"
              />
              <FolderOpen className="h-4 w-4" />
              Ouvrir dossier après export
            </label>
            <div className="flex gap-2">
              <button className="ghost-button" disabled={exporting} onClick={close}>
                Annuler
              </button>
              <button
                className="primary-action w-fit px-5"
                disabled={!canExport}
                onClick={() => void exportClip()}
              >
                <Download className="h-4 w-4" />
                {exporting ? "Export en cours..." : `Exporter ${formatClipDuration(exportDuration)}`}
              </button>
            </div>
          </footer>
        </motion.section>
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
  title,
  subtitle,
  watermark,
  quality,
}: {
  clip: ClipInfo;
  mode: ClipEditorMode;
  outputFormat: "webm" | "mp4";
  socialLayout: SocialLayout;
  startSeconds: number;
  endSeconds: number;
  title: string;
  subtitle: string;
  watermark: boolean;
  quality: QualityPreset;
}): ClipEditRequest {
  const social = mode === "social_vertical";
  return {
    clipPath: clip.path,
    metadataPath: metadataPathForClip(clip.path),
    startSeconds,
    endSeconds,
    mode,
    outputFormat: mode === "trim_original" ? outputFormat : "mp4",
    layout: social ? socialLayout : undefined,
    title: social ? title : undefined,
    subtitle: social ? subtitle : undefined,
    watermark: social && watermark,
    fps: 30,
    bitrateKbps: quality === "high" ? 12_000 : 8_000,
  };
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
