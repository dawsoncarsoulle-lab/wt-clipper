import { useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  Activity,
  CheckCircle2,
  Clipboard,
  Cpu,
  FileText,
  FolderOpen,
  Gauge,
  HardDrive,
  Home,
  Monitor,
  Play,
  RefreshCcw,
  Save,
  Settings,
  ShieldAlert,
  Trash2,
  Video,
  Wand2,
  XCircle,
  Zap,
} from "lucide-react";
import { ClipEditorModal } from "./components/ClipEditorModal";
import { LanguageSelector } from "./components/LanguageSelector";
import { useI18n } from "./i18n/I18nProvider";
import appLogo from "./assets/brand/WT_clipper_brand.png";
import {
  galleryBadgeClass,
  galleryBadgeLabel,
  hoverPreviewSeekSeconds,
  mountedGalleryVideoCount,
  shouldShowHoverVideo,
  shouldApplyGalleryLoadResult,
  shouldUseGalleryCache,
} from "./galleryResourcePolicy";
import { useAppStore } from "./store";
import type {
  AppConfig,
  ClipInfo,
  ClipReason,
  ClipStatus,
  ClipStatusChangedPayload,
  DoctorReport,
  GalleryClipItem,
  GsrHealth,
  RequirementCheck,
  RequirementStatus,
  RuntimeStatus,
  SystemRequirementsReport,
} from "./types";

const nav = [
  { id: "dashboard", labelKey: "nav.dashboard", icon: Home },
  { id: "clips", labelKey: "nav.clips", icon: Video },
  { id: "config", labelKey: "nav.config", icon: Settings },
  { id: "diagnostics", labelKey: "nav.diagnostics", icon: Activity },
] as const;

type Translate = ReturnType<typeof useI18n>["t"];

const GALLERY_CACHE_TTL_MS = 10_000;
const GALLERY_REFRESH_DEBOUNCE_MS = 800;
const GALLERY_AUTO_REFRESH_MS = 5000;
const HOVER_PREVIEW_START_SECONDS = 0.75;

const reasonLabelKey: Record<ClipReason, string> = {
  target_destroyed: "reason.target_destroyed",
  base_destroyed: "reason.base_destroyed",
  player_destroyed: "reason.player_destroyed",
  multi_kill: "reason.multi_kill",
  manual: "reason.manual",
  unknown: "reason.unknown",
};

const statusLabelKey: Record<ClipStatus, string> = {
  detected: "clipStatus.detected",
  recording: "clipStatus.recording",
  encoding: "clipStatus.encoding",
  saving: "clipStatus.saving",
  ready: "clipStatus.ready",
  failed: "clipStatus.failed",
};

function reasonLabel(reason: ClipReason, t: Translate) {
  return t(reasonLabelKey[reason] ?? "events.generic");
}

function clipStatusLabel(status: ClipStatus, t: Translate) {
  return t(statusLabelKey[status] ?? "clipStatus.ready");
}

function isWaitingForWarThunder(status?: RuntimeStatus | null) {
  return (
    status?.gsrCaptureStrategy === "auto" &&
    status?.gsrHealth === "stopped" &&
    status?.gsrTargetReason?.toLowerCase().includes("waiting for war thunder")
  );
}

function gsrHealthLabel(health: GsrHealth | null | undefined, t: Translate) {
  switch (health) {
    case "running":
      return t("status.gpuReplayArmed");
    case "saving_replay":
      return t("status.savingReplay");
    case "starting":
      return t("status.starting");
    case "error":
      return t("status.error");
    case "not_available":
      return t("status.notAvailable");
    case "stopped":
    default:
      return t("status.stopped");
  }
}

function gsrStatusLabel(status: RuntimeStatus | null | undefined, t: Translate) {
  if (isWaitingForWarThunder(status)) {
    return t("status.waitingForWarThunder");
  }
  return gsrHealthLabel(status?.gsrHealth, t);
}

export function App() {
  const store = useAppStore();
  const { t } = useI18n();
  const galleryRefreshTimeout = useRef<number | null>(null);
  const galleryLoadSeq = useRef(0);
  const lastGalleryLoadAt = useRef(0);
  const seenRuntimeEvents = useRef(new Set<string>());

  useEffect(() => {
    void bootstrap();

    const runtimePoll = window.setInterval(() => {
      void refreshRuntimeStatus();
    }, 4000);

    const killRefreshTimeouts = new Set<number>();
    const unsubs = [
      listen<boolean>("wt-connected", (event) => store.setWtConnected(event.payload)),
      listen("wt-disconnected", () => store.setWtConnected(false)),
      listen<{ kind: ClipReason; description?: string }>("kill-detected", (event) => {
        store.addEvent({
          kind: event.payload.kind,
          title: reasonLabel(event.payload.kind, t),
          detail: event.payload.description,
        });
        const timeout = window.setTimeout(() => {
          killRefreshTimeouts.delete(timeout);
          scheduleGalleryRefresh({ force: true });
        }, 1200);
        killRefreshTimeouts.add(timeout);
      }),
      listen<ClipInfo>("clip-saved", (event) => {
        store.addClip(event.payload);
        store.addEvent({
          kind: event.payload.reason,
          title: t("events.clipSaved"),
          detail: event.payload.fileName,
        });
        scheduleGalleryRefresh({ force: true });
      }),
      listen<ClipStatusChangedPayload>("clip-status-changed", (event) => {
        store.updateClipStatus(event.payload);
        if (event.payload.status === "ready" || event.payload.status === "saving") {
          scheduleGalleryRefresh({ force: true });
        }
      }),
      listen<{ message: string }>("clip-failed", (event) => {
        store.addEvent({ kind: "system", title: t("events.clipFailed"), detail: event.payload.message });
        store.showToast(t("events.clipFailedToast", { message: event.payload.message }));
      }),
      listen<number>("disk-usage", (event) => store.setDiskUsedBytes(event.payload)),
      listen<ClipInfo[]>("clips-loaded", (event) => store.setClips(event.payload)),
      listen<DoctorReport>("diagnostics-ready", (event) => store.setDiagnostics(event.payload)),
    ];

    void Promise.all(unsubs).then((resolved) => {
      store.setFrontendListenerCount(resolved.length);
    });

    return () => {
      window.clearInterval(runtimePoll);
      if (galleryRefreshTimeout.current != null) {
        window.clearTimeout(galleryRefreshTimeout.current);
      }
      for (const timeout of killRefreshTimeouts) {
        window.clearTimeout(timeout);
      }
      void Promise.all(unsubs).then((resolved) => {
        resolved.forEach((unlisten) => unlisten());
        store.setFrontendListenerCount(0);
      });
    };
  }, []);

  useEffect(() => {
    if (store.activeView !== "clips") {
      return;
    }
    void refreshClips({ force: true });
    const interval = window.setInterval(() => {
      void refreshClips();
    }, GALLERY_AUTO_REFRESH_MS);
    return () => window.clearInterval(interval);
  }, [store.activeView]);

  async function bootstrap() {
    try {
      const [config, diagnostics] = await Promise.all([
        invoke<AppConfig>("get_config"),
        invoke<DoctorReport>("run_diagnostics"),
      ]);
      store.setConfig(config);
      store.setDiagnostics(diagnostics);
      await Promise.all([refreshClips({ force: true }), refreshRuntimeStatus()]);
    } catch (error) {
      store.showToast(String(error));
    }
  }

  function scheduleGalleryRefresh(options: { force?: boolean; delayMs?: number } = {}) {
    if (galleryRefreshTimeout.current != null) {
      window.clearTimeout(galleryRefreshTimeout.current);
    }
    galleryRefreshTimeout.current = window.setTimeout(() => {
      galleryRefreshTimeout.current = null;
      void refreshClips({ force: options.force ?? false });
    }, options.delayMs ?? GALLERY_REFRESH_DEBOUNCE_MS);
  }

  async function refreshClips(options: { force?: boolean } = {}) {
    const now = Date.now();
    if (shouldUseGalleryCache(now, lastGalleryLoadAt.current, GALLERY_CACHE_TTL_MS, options.force ?? false)) {
      return;
    }
    const seq = ++galleryLoadSeq.current;
    const started = performance.now();
    try {
      const clips = await invoke<ClipInfo[]>("load_clips");
      if (!shouldApplyGalleryLoadResult(seq, galleryLoadSeq.current, true)) {
        return;
      }
      lastGalleryLoadAt.current = Date.now();
      store.setClips(clips);
      store.recordGalleryRefresh(Math.round(performance.now() - started));
    } catch (error) {
      store.showToast(String(error));
    }
  }

  async function refreshRuntimeStatus() {
    try {
      const status = await invoke<RuntimeStatus>("get_runtime_status");
      store.setRuntimeStatus(status);
      for (const event of status.recentEvents ?? []) {
        if (seenRuntimeEvents.current.has(event.id)) {
          continue;
        }
        seenRuntimeEvents.current.add(event.id);
        store.addEventEntry({
          id: event.id,
          at: event.at,
          kind: event.kind,
          title: reasonLabel(event.kind, t),
          detail: event.description,
        });
      }
    } catch (error) {
      console.info("[FRONTEND] runtime status skipped", error);
    }
  }

  return (
    <div className="app-shell">
      <Sidebar />
      <main className="main">
        <Topbar />
        {store.activeView === "dashboard" && <Dashboard />}
        {store.activeView === "clips" && <Clips onRefresh={() => refreshClips({ force: true })} />}
        {store.activeView === "config" && <Configuration />}
        {store.activeView === "diagnostics" && <Diagnostics />}
      </main>
      {store.toast && <div className="toast">{store.toast}</div>}
    </div>
  );
}

function Sidebar() {
  const { activeView, setActiveView, config } = useAppStore();
  const { t } = useI18n();
  return (
    <aside className="sidebar">
      <div className="brand">
        <img className="logo brand-logo" src={appLogo} alt={t("app.logoAlt")} />
        <div>
          <strong>WT Clip</strong>
          <span>GPU Screen Recorder</span>
        </div>
      </div>
      <nav>
        {nav.map((item) => {
          const Icon = item.icon;
          const active = activeView === item.id;
          return (
            <button
              key={item.id}
              className={active ? "nav-item active" : "nav-item"}
              onClick={() => setActiveView(item.id)}
              type="button"
            >
              <Icon size={18} />
              {t(item.labelKey)}
            </button>
          );
        })}
      </nav>
      <LanguageSelector />
      <div className="sidebar-footer">
        <span>{t("sidebar.captureBackend")}</span>
        <strong>{config?.capture.target || t("sidebar.targetNotConfigured")}</strong>
      </div>
    </aside>
  );
}

function Topbar() {
  const { wtConnected, runtimeStatus } = useAppStore();
  const { t } = useI18n();
  return (
    <header className="topbar">
      <div>
        <h1>{t("app.topbar.title")}</h1>
        <p>{t("app.topbar.subtitle")}</p>
      </div>
      <div className="topbar-status">
        <StatusPill connected={wtConnected} />
        <span className={`status-chip ${runtimeStatus?.gsrHealth === "running" ? "ok" : "warn"}`}>
          <Cpu size={15} />
          {gsrStatusLabel(runtimeStatus, t)}
        </span>
        <img className="topbar-logo" src={appLogo} alt={t("app.logoAlt")} />
      </div>
    </header>
  );
}

function StatusPill({ connected }: { connected: boolean }) {
  const { t } = useI18n();
  return (
    <span className={`status-chip ${connected ? "ok" : "warn"}`}>
      <Zap size={15} />
      {connected ? t("status.wtConnected") : t("status.wtWaiting")}
    </span>
  );
}

function Dashboard() {
  const state = useAppStore();
  const { t } = useI18n();
  const status = state.runtimeStatus;
  return (
    <section className="view-grid">
      <div className="hero-panel">
        <div>
          <span className="eyebrow">{t("dashboard.backendActive")}</span>
          <h2>GPU Screen Recorder</h2>
          <p>
            {gsrStatusLabel(status, t)} · {status?.gsrMode ?? state.config?.capture.mode ?? "auto"} ·{" "}
            {status?.gsrTarget ?? state.config?.capture.target ?? t("dashboard.targetNotConfigured")}
          </p>
        </div>
        <button type="button" className="primary" onClick={() => void invoke("save_manual_clip")}>
          <Save size={18} />
          {t("actions.manualClip")}
        </button>
      </div>

      <Metric icon={Video} label={t("dashboard.metric.clips")} value={state.clips.length} />
      <Metric icon={Gauge} label={t("dashboard.metric.sessionKills")} value={state.sessionKills} />
      <Metric icon={Wand2} label={t("dashboard.metric.multiKills")} value={state.sessionMultiKills} />
      <Metric icon={HardDrive} label={t("dashboard.metric.storage")} value={formatBytes(state.diskUsedBytes)} />

      <section className="panel wide">
        <div className="panel-heading">
          <div>
            <span className="eyebrow">{t("events.activity")}</span>
            <h3>{t("events.recent")}</h3>
          </div>
        </div>
        <div className="event-list">
          {state.events.length === 0 && <Empty label={t("events.empty")} />}
          {state.events.map((event) => (
            <article key={event.id} className="event-row">
              <span>{event.at}</span>
              <strong>{event.title}</strong>
              <p>{event.detail ?? ""}</p>
            </article>
          ))}
        </div>
      </section>
    </section>
  );
}

function Metric({ icon: Icon, label, value }: { icon: typeof Zap; label: string; value: string | number }) {
  return (
    <div className="metric-card">
      <Icon size={20} />
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function Clips({ onRefresh }: { onRefresh: () => void }) {
  const { t } = useI18n();
  const { clips, processingClips, setClips, showToast, setGalleryRenderedClipCount, setGalleryMountedVideoCount } =
    useAppStore();
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<"all" | ClipReason>("all");
  const [editingClip, setEditingClip] = useState<ClipInfo | null>(null);
  const [editingClips, setEditingClips] = useState<ClipInfo[] | null>(null);
  const [selectedClipPaths, setSelectedClipPaths] = useState<string[]>([]);
  const [activePreviewClip, setActivePreviewClip] = useState<ClipInfo | null>(null);

  const galleryItems = useMemo(() => {
    const readyPaths = new Set(clips.map((clip) => clip.path));
    const transient = processingClips.filter((clip) => !clip.filePath || !readyPaths.has(clip.filePath));
    return [...transient, ...clips.map(clipToGalleryItem)]
      .filter((clip) => {
        const haystack = `${clip.title} ${clip.filePath ?? ""}`.toLowerCase();
        const matchesQuery = haystack.includes(query.toLowerCase());
        const matchesFilter = filter === "all" || clip.reason === filter;
        return matchesQuery && matchesFilter;
      })
      .sort(compareGalleryItems);
  }, [clips, filter, processingClips, query]);

  useEffect(() => {
    setGalleryRenderedClipCount(galleryItems.length);
  }, [galleryItems.length, setGalleryRenderedClipCount]);

  useEffect(() => {
    setGalleryMountedVideoCount(mountedGalleryVideoCount(activePreviewClip?.path ?? null));
  }, [activePreviewClip, setGalleryMountedVideoCount]);

  async function remove(path: string) {
    try {
      await invoke("delete_clip", { path });
      setClips(clips.filter((clip) => clip.path !== path));
      showToast(t("gallery.deletedToast"));
    } catch (error) {
      showToast(String(error));
    }
  }

  const selectedReadyClips = clips.filter((clip) => selectedClipPaths.includes(clip.path));

  function toggleClipSelection(path: string) {
    setSelectedClipPaths((current) =>
      current.includes(path) ? current.filter((item) => item !== path) : [...current, path],
    );
  }

  function openSelectedInEditor() {
    if (selectedReadyClips.length === 0) {
      return;
    }
    setEditingClip(selectedReadyClips[0]);
    setEditingClips(selectedReadyClips);
  }

  return (
    <section className="panel">
      <div className="panel-heading">
        <div>
          <span className="eyebrow">{t("gallery.eyebrow")}</span>
          <h2>{t("gallery.title")}</h2>
        </div>
        <button type="button" className="secondary" onClick={onRefresh}>
          <RefreshCcw size={17} />
          {t("actions.refresh")}
        </button>
      </div>

      <div className="toolbar">
        <input
          type="search"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder={t("gallery.searchPlaceholder")}
        />
        <select value={filter} onChange={(event) => setFilter(event.target.value as "all" | ClipReason)}>
          <option value="all">{t("gallery.filter.all")}</option>
          {Object.keys(reasonLabelKey).map((value) => (
            <option key={value} value={value}>
              {reasonLabel(value as ClipReason, t)}
            </option>
          ))}
        </select>
        <button
          type="button"
          className="secondary"
          disabled={selectedReadyClips.length === 0}
          onClick={openSelectedInEditor}
        >
          <Wand2 size={16} />
          {t("actions.assemble", { count: selectedReadyClips.length })}
        </button>
      </div>

      <div className="clip-grid">
        {galleryItems.length === 0 && <Empty label={t("gallery.empty")} />}
        {galleryItems.map((clip) =>
          clip.status === "ready" && clip.filePath ? (
            <ClipCard
              key={clip.id}
              clip={galleryItemToClipInfo(clip)}
              activePreviewPath={activePreviewClip?.path ?? null}
              selected={Boolean(clip.filePath && selectedClipPaths.includes(clip.filePath))}
              onSelectToggle={toggleClipSelection}
              onPreviewChange={setActivePreviewClip}
              onDelete={remove}
              onEdit={(clip) => {
                setEditingClip(clip);
                setEditingClips([clip]);
              }}
            />
          ) : (
            <ClipProcessingCard key={clip.id} clip={clip} />
          ),
        )}
      </div>

      <HoverVideoPreview clip={editingClip ? null : activePreviewClip} startSeconds={HOVER_PREVIEW_START_SECONDS} />

      {editingClip && (
        <ClipEditorModal
          clip={editingClip}
          clips={editingClips ?? [editingClip]}
          onClose={() => {
            setEditingClip(null);
            setEditingClips(null);
          }}
          onExportComplete={async () => {
            setEditingClip(null);
            setEditingClips(null);
            setSelectedClipPaths([]);
            onRefresh();
          }}
        />
      )}
    </section>
  );
}

function ClipProcessingCard({ clip }: { clip: GalleryClipItem }) {
  const { t } = useI18n();
  const progress = clip.progress ?? (clip.status === "failed" ? 0 : 48);
  return (
    <article className="clip-card processing">
      <div className="clip-thumb placeholder">
        <Activity size={28} />
      </div>
      <div className="clip-meta">
        <span className={`badge ${galleryBadgeClass(clip.clipType, clip.reason, clip.exportType)}`}>
          {galleryBadgeLabel(clip.clipType, clip.reason, clip.exportType)}
        </span>
        <h3>{clip.title}</h3>
        <p>{clip.error ?? clipStatusLabel(clip.status, t)}</p>
        {clip.status !== "failed" && (
          <div className="progress">
            <span style={{ width: `${Math.max(8, Math.min(100, progress))}%` }} />
          </div>
        )}
      </div>
    </article>
  );
}

function ClipCard({
  clip,
  activePreviewPath,
  selected,
  onSelectToggle,
  onPreviewChange,
  onDelete,
  onEdit,
}: {
  clip: ClipInfo;
  activePreviewPath: string | null;
  selected: boolean;
  onSelectToggle: (path: string) => void;
  onPreviewChange: (clip: ClipInfo | null) => void;
  onDelete: (path: string) => void;
  onEdit: (clip: ClipInfo) => void;
}) {
  const previewActive = activePreviewPath === clip.path;
  const thumbnailSrc = clip.thumbnailPath ? convertFileSrc(clip.thumbnailPath) : null;
  const { t } = useI18n();

  return (
    <article
      className={previewActive ? "clip-card preview-active" : "clip-card"}
      data-preview-path={clip.path}
      onMouseEnter={() => onPreviewChange(clip)}
      onMouseLeave={() => onPreviewChange(null)}
    >
      <div className="clip-thumb">
        {thumbnailSrc ? (
          <img src={thumbnailSrc} alt="" loading="lazy" />
        ) : (
          <FilmFallback />
        )}
        <label className="clip-select-toggle" onClick={(event) => event.stopPropagation()}>
          <input
            checked={selected}
            onChange={() => onSelectToggle(clip.path)}
            type="checkbox"
          />
        </label>
      </div>
      <div className="clip-meta">
        <span className={`badge ${galleryBadgeClass(clip.clipType, clip.reason, clip.exportType)}`}>
          {galleryBadgeLabel(clip.clipType, clip.reason, clip.exportType)}
        </span>
        <h3>{clip.fileName}</h3>
        <p>
          {formatClipDuration(clip.durationSeconds)} · {formatBytes(clip.sizeBytes)} ·{" "}
          {relativeTime(clip.modifiedSecsAgo, t)}
        </p>
      </div>
      <div className="clip-actions">
        <button type="button" title={t("actions.edit")} onClick={() => onEdit(clip)}>
          <Wand2 size={16} />
        </button>
        <button type="button" title={t("actions.openFolder")} onClick={() => void invoke("open_parent_folder", { path: clip.path })}>
          <FolderOpen size={16} />
        </button>
        <button type="button" title={t("actions.delete")} onClick={() => onDelete(clip.path)}>
          <Trash2 size={16} />
        </button>
      </div>
    </article>
  );
}

function HoverVideoPreview({ clip, startSeconds }: { clip: ClipInfo | null; startSeconds: number }) {
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const cleanupTimer = useRef<number | null>(null);
  const [ready, setReady] = useState(false);
  const [targetRect, setTargetRect] = useState<DOMRect | null>(null);
  const [readyState, setReadyState] = useState(0);

  useEffect(() => {
    setReady(false);
    setReadyState(0);
    if (cleanupTimer.current != null) {
      window.clearTimeout(cleanupTimer.current);
      cleanupTimer.current = null;
    }

    const video = videoRef.current;
    if (!video) {
      return;
    }

    releaseVideoElement(video);
    if (!clip) {
      setTargetRect(null);
      return;
    }

    const thumb = findPreviewThumb(clip.path);
    if (!thumb) {
      return;
    }
    setTargetRect(thumb.getBoundingClientRect());

    let cancelled = false;
    const src = clip.previewUrl ?? convertFileSrc(clip.path);

    function markReadyAndPlay() {
      if (cancelled || !video) {
        return;
      }
      const seekTo = hoverPreviewSeekSeconds(startSeconds, video.duration);
      try {
        if (Number.isFinite(video.duration) && video.duration > seekTo) {
          video.currentTime = seekTo;
        }
      } catch (error) {
        console.info("[FRONTEND] hover preview seek skipped", error);
      }
      if (!shouldShowHoverVideo(true, video.readyState)) {
        return;
      }
      setReadyState(video.readyState);
      setReady(true);
      void video.play().catch((error) => {
        console.info("[FRONTEND] hover preview playback failed", error);
      });
    }

    const onLoadedMetadata = () => {
      if (cancelled) {
        return;
      }
      try {
        video.currentTime = hoverPreviewSeekSeconds(startSeconds, video.duration);
      } catch (error) {
        console.info("[FRONTEND] hover preview metadata seek skipped", error);
      }
    };
    const onLoadedData = () => {
      if (!cancelled) {
        setReadyState(video.readyState);
      }
    };
    const onCanPlay = () => markReadyAndPlay();

    video.addEventListener("loadedmetadata", onLoadedMetadata);
    video.addEventListener("loadeddata", onLoadedData);
    video.addEventListener("canplay", onCanPlay);
    video.src = src;
    video.load();

    cleanupTimer.current = window.setTimeout(() => {
      if (!cancelled) {
        setReadyState(video.readyState);
      }
    }, 250);

    return () => {
      cancelled = true;
      video.removeEventListener("loadedmetadata", onLoadedMetadata);
      video.removeEventListener("loadeddata", onLoadedData);
      video.removeEventListener("canplay", onCanPlay);
      if (cleanupTimer.current != null) {
        window.clearTimeout(cleanupTimer.current);
        cleanupTimer.current = null;
      }
      releaseVideoElement(video);
    };
  }, [clip, startSeconds]);

  useEffect(() => {
    if (!clip) {
      return;
    }
    const clipPath = clip.path;
    function updateRect() {
      const thumb = findPreviewThumb(clipPath);
      setTargetRect(thumb?.getBoundingClientRect() ?? null);
    }
    window.addEventListener("scroll", updateRect, true);
    window.addEventListener("resize", updateRect);
    return () => {
      window.removeEventListener("scroll", updateRect, true);
      window.removeEventListener("resize", updateRect);
    };
  }, [clip]);

  const visible = Boolean(clip && targetRect && shouldShowHoverVideo(ready, readyState));

  return (
    <video
      ref={videoRef}
      className="hover-video-preview"
      style={
        targetRect
          ? {
              left: `${targetRect.left}px`,
              top: `${targetRect.top}px`,
              width: `${targetRect.width}px`,
              height: `${targetRect.height}px`,
              opacity: visible ? 1 : 0,
              pointerEvents: "none",
            }
          : { opacity: 0, pointerEvents: "none" }
      }
      muted
      playsInline
      loop
      preload="metadata"
    />
  );
}

function findPreviewThumb(path: string) {
  const cards = Array.from(document.querySelectorAll<HTMLElement>("[data-preview-path]"));
  const card = cards.find((item) => item.dataset.previewPath === path);
  return card?.querySelector<HTMLElement>(".clip-thumb") ?? null;
}

function Configuration() {
  const { t } = useI18n();
  const { config, runtimeStatus, setConfig, setRuntimeStatus, showToast } = useAppStore();
  const [draft, setDraft] = useState<AppConfig | null>(config);
  const [saving, setSaving] = useState(false);
  const [checkingUpdate, setCheckingUpdate] = useState(false);

  useEffect(() => {
    setDraft(config);
  }, [config]);

  if (!draft) {
    return <Empty label={t("config.loading")} />;
  }

  const updateClip = <K extends keyof AppConfig["clip"]>(key: K, value: AppConfig["clip"][K]) =>
    setDraft({ ...draft, clip: { ...draft.clip, [key]: value } });
  const updateLibrary = <K extends keyof AppConfig["library"]>(key: K, value: AppConfig["library"][K]) =>
    setDraft({ ...draft, library: { ...draft.library, [key]: value } });
  const updateCapture = <K extends keyof AppConfig["capture"]>(key: K, value: AppConfig["capture"][K]) =>
    setDraft({ ...draft, capture: { ...draft.capture, [key]: value } });
  const updateWt = <K extends keyof AppConfig["war_thunder"]>(key: K, value: AppConfig["war_thunder"][K]) =>
    setDraft({ ...draft, war_thunder: { ...draft.war_thunder, [key]: value } });
  const updateTrigger = <K extends keyof AppConfig["triggers"]>(key: K, value: boolean) =>
    setDraft({ ...draft, triggers: { ...draft.triggers, [key]: value } });
  const updateStorage = <K extends keyof AppConfig["storage"]>(key: K, value: AppConfig["storage"][K]) =>
    setDraft({ ...draft, storage: { ...draft.storage, [key]: value } });

  const detectedTargets = Array.from(
    new Set([
      ...(runtimeStatus?.gsrMonitors ?? []),
      draft.capture.target,
    ].filter(Boolean)),
  );
  const targetValid = runtimeStatus?.gsrTargetValid ?? detectedTargets.includes(draft.capture.target);

  async function save() {
    const nextConfig = draft;
    if (!nextConfig) {
      return;
    }
    setSaving(true);
    try {
      await invoke("save_config", { config: nextConfig });
      const status = await invoke<RuntimeStatus>("get_runtime_status");
      setConfig(nextConfig);
      setRuntimeStatus(status);
      showToast(t("config.savedToast"));
    } catch (error) {
      showToast(String(error));
    } finally {
      setSaving(false);
    }
  }

  async function checkForUpdates() {
    setCheckingUpdate(true);
    try {
      const result = await invoke<{ available: boolean }>("check_for_updates");
      showToast(result.available ? t("config.updateAvailable") : t("config.upToDate"));
    } catch (error) {
      showToast(String(error));
    } finally {
      setCheckingUpdate(false);
    }
  }

  return (
    <section className="config-layout">
      <ConfigSection title={t("config.section.clip")}>
        <Field label={t("config.field.postEventDelay")}>
          <input
            type="number"
            min={0}
            value={draft.clip.post_event_seconds}
            onChange={(event) => updateClip("post_event_seconds", Number(event.target.value))}
          />
        </Field>
        <Field label={t("config.field.multiKillWindow")}>
          <input
            type="number"
            min={1}
            value={draft.clip.multi_kill_window_seconds}
            onChange={(event) => updateClip("multi_kill_window_seconds", Number(event.target.value))}
          />
        </Field>
      </ConfigSection>

      <ConfigSection title={t("config.section.library")}>
        <Field label={t("config.field.libraryFolder")}>
          <input value={draft.library.output_dir} onChange={(event) => updateLibrary("output_dir", event.target.value)} />
        </Field>
      </ConfigSection>

      <ConfigSection title="GPU Screen Recorder">
        <Field label={t("config.field.captureStrategy")}>
          <select
            value={draft.capture.capture_strategy}
            onChange={(event) => updateCapture("capture_strategy", event.target.value as AppConfig["capture"]["capture_strategy"])}
          >
            <option value="auto">{t("config.strategy.auto")}</option>
            <option value="monitor">{t("config.strategy.monitor")}</option>
            <option value="focused">{t("config.strategy.focused")}</option>
            <option value="portal">{t("config.strategy.portal")}</option>
          </select>
          <p className="help-text">
            {t("config.strategyHelp.auto")}
          </p>
        </Field>
        <Field label={draft.capture.capture_strategy === "monitor" ? t("config.field.target") : t("config.field.fallbackTarget")}>
          {detectedTargets.length > 0 ? (
            <select
              value={draft.capture.target}
              onChange={(event) => updateCapture("target", event.target.value)}
            >
              {detectedTargets.map((target) => (
                <option key={target} value={target}>
                  {target}
                </option>
              ))}
            </select>
          ) : (
            <input value={draft.capture.target} onChange={(event) => updateCapture("target", event.target.value)} />
          )}
          <p className={targetValid ? "help-text" : "help-text warning-text"}>
            {targetValid
              ? t("config.target.valid", {
                  detected: runtimeStatus?.gsrMonitors?.length
                    ? t("config.target.detectedSuffix", { targets: runtimeStatus.gsrMonitors.join(", ") })
                    : "",
                })
              : t("config.target.invalid", {
                  targets: runtimeStatus?.gsrMonitors?.length
                    ? t("config.target.detectedList", { targets: runtimeStatus.gsrMonitors.join(", ") })
                    : t("config.target.detectedEnd"),
                })}
          </p>
        </Field>
        <Field label={t("config.field.mode")}>
          <select value={draft.capture.mode} onChange={(event) => updateCapture("mode", event.target.value as AppConfig["capture"]["mode"])}>
            <option value="auto">Auto</option>
            <option value="native">Native</option>
            <option value="flatpak">Flatpak</option>
          </select>
        </Field>
        <Field label={t("config.field.fps")}>
          <input type="number" min={1} value={draft.capture.fps} onChange={(event) => updateCapture("fps", Number(event.target.value))} />
        </Field>
        <Field label={t("config.field.fpsMode")}>
          <select
            value={draft.capture.frame_rate_mode}
            onChange={(event) => updateCapture("frame_rate_mode", event.target.value as AppConfig["capture"]["frame_rate_mode"])}
          >
            <option value="cfr">{t("config.fpsMode.cfr")}</option>
            <option value="vfr">{t("config.fpsMode.vfr")}</option>
            <option value="content">{t("config.fpsMode.content")}</option>
          </select>
        </Field>
        <Field label={t("config.field.keyframeInterval")}>
          <input
            type="number"
            min={0.1}
            max={10}
            step={0.1}
            list="keyframe-presets"
            value={draft.capture.keyframe_interval_seconds}
            onChange={(event) => updateCapture("keyframe_interval_seconds", Number(event.target.value))}
          />
          <datalist id="keyframe-presets">
            <option value="0.5" />
            <option value="1.0" />
            <option value="2.0" />
          </datalist>
        </Field>
        <Field label={t("config.field.replayDuration")}>
          <input
            type="number"
            min={5}
            value={draft.capture.replay_seconds}
            onChange={(event) => updateCapture("replay_seconds", Number(event.target.value))}
          />
        </Field>
        <Field label={t("config.field.container")}>
          <select value={draft.capture.container} onChange={(event) => updateCapture("container", event.target.value as AppConfig["capture"]["container"])}>
            <option value="mp4">MP4</option>
            <option value="mkv">MKV</option>
          </select>
        </Field>
        <Field label={t("config.field.codec")}>
          <select value={draft.capture.codec} onChange={(event) => updateCapture("codec", event.target.value as AppConfig["capture"]["codec"])}>
            <option value="h264">H.264</option>
            <option value="hevc">HEVC</option>
            <option value="av1">AV1</option>
          </select>
        </Field>
        <Field label={t("config.field.encoder")}>
          <select value={draft.capture.encoder} onChange={(event) => updateCapture("encoder", event.target.value as AppConfig["capture"]["encoder"])}>
            <option value="gpu">GPU</option>
            <option value="cpu">CPU</option>
          </select>
        </Field>
        <Field label={t("config.field.bitrateMode")}>
          <select
            value={draft.capture.bitrate_mode}
            onChange={(event) => updateCapture("bitrate_mode", event.target.value as AppConfig["capture"]["bitrate_mode"])}
          >
            <option value="auto">Auto</option>
            <option value="qp">{t("config.bitrate.qp")}</option>
            <option value="vbr">VBR</option>
            <option value="cbr">{t("config.bitrate.cbr")}</option>
          </select>
        </Field>
        {draft.capture.bitrate_mode === "cbr" ? (
          <Field label={t("config.field.videoBitrateKbps")}>
            <input
              type="number"
              min={1000}
              step={500}
              value={draft.capture.video_bitrate_kbps}
              onChange={(event) => updateCapture("video_bitrate_kbps", Number(event.target.value))}
            />
          </Field>
        ) : (
          <Field label={t("config.field.quality")}>
            <select value={draft.capture.quality} onChange={(event) => updateCapture("quality", event.target.value as AppConfig["capture"]["quality"])}>
              <option value="medium">Medium</option>
              <option value="high">High</option>
              <option value="very_high">Very high</option>
              <option value="ultra">Ultra</option>
            </select>
          </Field>
        )}
        <p className="help-text">
          {t("config.captureHelp.bitrate")}
        </p>
        <Field label={t("config.field.restartReplayOnSave")} className="toggle-field">
          <div className="checkbox-row">
            <input
              type="checkbox"
              checked={draft.capture.restart_replay_on_save}
              onChange={(event) => updateCapture("restart_replay_on_save", event.target.checked)}
            />
            <strong>{draft.capture.restart_replay_on_save ? t("config.restartReplay.yes") : t("config.restartReplay.no")}</strong>
          </div>
        </Field>
        <Field label={t("config.field.audio")} className="toggle-field">
          <div className="checkbox-row">
            <input
              type="checkbox"
              checked={draft.capture.audio_enabled}
              onChange={(event) => updateCapture("audio_enabled", event.target.checked)}
            />
            <strong>{t("config.audio.enabled")}</strong>
          </div>
        </Field>
        <Field label={t("config.field.audioInput")}>
          <input value={draft.capture.audio_input} onChange={(event) => updateCapture("audio_input", event.target.value)} />
        </Field>
        <Field label={t("config.field.captureFolder")}>
          <input value={draft.capture.output_dir} onChange={(event) => updateCapture("output_dir", event.target.value)} />
        </Field>
      </ConfigSection>

      <ConfigSection title={t("config.section.warThunder")}>
        <Field label={t("config.field.baseUrl")}>
          <input value={draft.war_thunder.base_url} onChange={(event) => updateWt("base_url", event.target.value)} />
        </Field>
        <Field label={t("config.field.player")}>
          <input
            value={draft.war_thunder.player_name ?? ""}
            onChange={(event) => updateWt("player_name", event.target.value || null)}
          />
        </Field>
        <Field label={t("config.field.pollMs")}>
          <input
            type="number"
            min={100}
            value={draft.war_thunder.poll_interval_ms}
            onChange={(event) => updateWt("poll_interval_ms", Number(event.target.value))}
          />
        </Field>
        <Field label={t("config.field.timeoutMs")}>
          <input
            type="number"
            min={100}
            value={draft.war_thunder.request_timeout_ms}
            onChange={(event) => updateWt("request_timeout_ms", Number(event.target.value))}
          />
        </Field>
      </ConfigSection>

      <ConfigSection title={t("config.section.triggers")}>
        {Object.entries(draft.triggers).map(([key, value]) => (
          <label key={key} className="inline-toggle">
            <input type="checkbox" checked={value} onChange={(event) => updateTrigger(key as keyof AppConfig["triggers"], event.target.checked)} />
            {reasonLabel(key as ClipReason, t) ?? key}
          </label>
        ))}
      </ConfigSection>

      <ConfigSection title={t("config.section.storage")}>
        <Field label={t("config.field.maxClips")}>
          <input type="number" min={1} value={draft.storage.max_clips} onChange={(event) => updateStorage("max_clips", Number(event.target.value))} />
        </Field>
        <Field label={t("config.field.maxGb")}>
          <input
            type="number"
            min={1}
            value={draft.storage.max_storage_gb}
            onChange={(event) => updateStorage("max_storage_gb", Number(event.target.value))}
          />
        </Field>
      </ConfigSection>

      <div className="config-actions">
        <button type="button" className="primary" onClick={save} disabled={saving}>
          <Save size={17} />
          {saving ? t("actions.saving") : t("actions.save")}
        </button>
        <button type="button" className="secondary" onClick={checkForUpdates} disabled={checkingUpdate}>
          <RefreshCcw size={17} />
          {checkingUpdate ? t("actions.checking") : t("actions.checkUpdates")}
        </button>
        {runtimeStatus?.configRestartRequired && <span className="status-chip warn">{t("config.restartRequired")}</span>}
      </div>
    </section>
  );
}

function Diagnostics() {
  const { t } = useI18n();
  const { diagnostics, runtimeStatus, setDiagnostics, setDiagnosticsRunning, setRuntimeStatus, diagnosticsRunning, showToast } =
    useAppStore();
  const [gsrTestRunning, setGsrTestRunning] = useState(false);
  const [requirements, setRequirements] = useState<SystemRequirementsReport | null>(null);

  useEffect(() => {
    void refreshDiagnostics({ showSpinner: true, includeDoctor: true });
    const interval = window.setInterval(() => {
      void refreshDiagnostics({ showSpinner: false, includeDoctor: false });
    }, 4000);
    return () => window.clearInterval(interval);
  }, []);

  async function refreshDiagnostics({ showSpinner, includeDoctor }: { showSpinner: boolean; includeDoctor: boolean }) {
    if (showSpinner) {
      setDiagnosticsRunning(true);
    }
    try {
      const [status, systemRequirements, report] = await Promise.all([
        invoke<RuntimeStatus>("get_runtime_status"),
        invoke<SystemRequirementsReport>("get_system_requirements"),
        includeDoctor ? invoke<DoctorReport>("run_diagnostics") : Promise.resolve(null),
      ]);
      setRuntimeStatus(status);
      setRequirements(systemRequirements);
      if (report) {
        setDiagnostics(report);
      } else if (showSpinner) {
        setDiagnosticsRunning(false);
      }
    } catch (error) {
      showToast(String(error));
      if (showSpinner) {
        setDiagnosticsRunning(false);
      }
    }
  }

  async function copyTextFromCommand(command: "get_diagnostics_report" | "get_recent_logs", successKey: string) {
    try {
      const text = await invoke<string>(command);
      await navigator.clipboard.writeText(text);
      showToast(t(successKey));
    } catch (error) {
      showToast(t("diagnostics.messages.copyError", { message: String(error) }));
    }
  }

  async function copyInstallCommand(command: string) {
    try {
      await navigator.clipboard.writeText(command);
      showToast(t("diagnostics.messages.copySuccess"));
    } catch (error) {
      showToast(t("diagnostics.messages.copyError", { message: String(error) }));
    }
  }

  async function openDiagnosticsFolder(command: "open_config_folder" | "open_output_folder") {
    try {
      await invoke(command);
    } catch (error) {
      showToast(String(error));
    }
  }

  async function restartGpuRecorder() {
    try {
      await invoke("restart_gpu_recorder");
      const status = await invoke<RuntimeStatus>("get_runtime_status");
      setRuntimeStatus(status);
      showToast(t("diagnostics.restartedToast"));
    } catch (error) {
      showToast(String(error));
    }
  }

  async function testGsrSaveReplay() {
    setGsrTestRunning(true);
    try {
      const path = await invoke<string>("test_gsr_save_replay");
      const status = await invoke<RuntimeStatus>("get_runtime_status");
      setRuntimeStatus(status);
      showToast(t("diagnostics.replaySavedToast", { path }));
    } catch (error) {
      showToast(String(error));
    } finally {
      setGsrTestRunning(false);
    }
  }

  const requirementChecks = requirements ? requirementsList(requirements) : [];
  const counts = requirementChecks.reduce(
    (acc, check) => ({ ...acc, [check.status]: acc[check.status] + 1 }),
    { ok: 0, warning: 0, error: 0, missing: 0, unknown: 0 },
  );

  return (
    <section className="diagnostics-grid">
      <div className="panel wide">
        <div className="panel-heading">
          <div>
            <span className="eyebrow">{t("diagnostics.eyebrow")}</span>
            <h2>{t("diagnostics.requirements.title")}</h2>
            <p>{t("diagnostics.requirements.subtitle")}</p>
          </div>
          <div className="button-row">
            <button
              type="button"
              className="secondary"
              onClick={() => void refreshDiagnostics({ showSpinner: true, includeDoctor: true })}
              disabled={diagnosticsRunning}
            >
              <RefreshCcw size={17} />
              {diagnosticsRunning ? t("actions.analyzing") : t("diagnostics.actions.refresh")}
            </button>
            <button
              type="button"
              className="secondary"
              onClick={() => void copyTextFromCommand("get_diagnostics_report", "diagnostics.messages.copySuccess")}
            >
              <FileText size={17} />
              {t("diagnostics.actions.copyReport")}
            </button>
            <button
              type="button"
              className="secondary"
              onClick={() => void copyTextFromCommand("get_recent_logs", "diagnostics.messages.copySuccess")}
            >
              <Clipboard size={17} />
              {t("diagnostics.actions.copyLogs")}
            </button>
            <button type="button" className="secondary" onClick={restartGpuRecorder}>
              <Cpu size={17} />
              {t("diagnostics.actions.restartGpuRecorder")}
            </button>
            <button type="button" className="primary" onClick={testGsrSaveReplay} disabled={gsrTestRunning}>
              <Play size={17} />
              {t("diagnostics.actions.testGsrSave")}
            </button>
          </div>
        </div>
        <div className="status-summary">
          <span className="status-chip ok">{counts.ok} {t("diagnostics.status.ok")}</span>
          <span className="status-chip warn">{counts.warning} {t("diagnostics.status.warning")}</span>
          <span className="status-chip error">{counts.error} {t("diagnostics.status.error")}</span>
          <span className="status-chip error">{counts.missing} {t("diagnostics.status.missing")}</span>
          <span className="status-chip">{counts.unknown} {t("diagnostics.status.unknown")}</span>
        </div>
        {requirements && (
          <div className="kv-grid diagnostics-context">
            <KeyValue label={t("diagnostics.key.appVersion")} value={requirements.app_version} />
            <KeyValue label={t("diagnostics.key.sessionType")} value={requirements.session_type ?? "-"} />
            <KeyValue label={t("diagnostics.key.mode")} value={requirements.capture_mode} />
            <KeyValue label={t("diagnostics.key.captureStrategy")} value={requirements.capture_strategy} />
            <KeyValue label={t("diagnostics.key.configuredTarget")} value={requirements.configured_target || "-"} />
            <KeyValue label={t("diagnostics.key.effectiveTarget")} value={requirements.effective_target ?? "-"} />
            <KeyValue label={t("diagnostics.key.targetReason")} value={requirements.target_reason ?? "-"} />
          </div>
        )}
        <div className="requirements-grid">
          {requirements ? (
            requirementChecks.map((check) => (
              <RequirementCard
                key={check.id}
                check={check}
                label={requirementLabel(check, t)}
                onCopyCommand={check.command ? () => void copyInstallCommand(check.command ?? "") : undefined}
                onOpenFolder={
                  check.id === "output_dir"
                    ? () => void openDiagnosticsFolder("open_output_folder")
                    : check.id === "config_dir"
                      ? () => void openDiagnosticsFolder("open_config_folder")
                      : undefined
                }
              />
            ))
          ) : (
            <Empty label={diagnostics?.summary ?? t("config.loading")} />
          )}
        </div>
      </div>

      <div className="panel wide">
        <div className="panel-heading">
          <div>
            <span className="eyebrow">{t("diagnostics.runtime")}</span>
            <h3>{t("diagnostics.gsrState")}</h3>
          </div>
        </div>
        <div className="kv-grid">
          <KeyValue label={t("diagnostics.key.available")} value={runtimeStatus?.gsrAvailable ? t("status.yes") : t("status.no")} />
          <KeyValue label={t("diagnostics.key.state")} value={gsrStatusLabel(runtimeStatus, t)} />
          <KeyValue label={t("diagnostics.key.mode")} value={runtimeStatus?.gsrMode ?? "-"} />
          <KeyValue label={t("diagnostics.key.wrapperPid")} value={runtimeStatus?.gsrWrapperPid ?? "-"} />
          <KeyValue label={t("diagnostics.key.recorderPid")} value={runtimeStatus?.gsrRecorderPid ?? "-"} />
          <KeyValue label={t("diagnostics.key.signalPid")} value={runtimeStatus?.gsrSignalPid ?? "-"} />
          <KeyValue label={t("diagnostics.key.captureStrategy")} value={runtimeStatus?.gsrCaptureStrategy ?? "-"} />
          <KeyValue label={t("diagnostics.key.sessionType")} value={runtimeStatus?.gsrSessionType ?? "-"} />
          <KeyValue label={t("diagnostics.key.effectiveTarget")} value={runtimeStatus?.gsrTarget ?? "-"} />
          <KeyValue label={t("diagnostics.key.targetReason")} value={runtimeStatus?.gsrTargetReason ?? "-"} />
          <KeyValue label={t("diagnostics.key.targetValid")} value={runtimeStatus?.gsrTargetValid ? t("status.yes") : t("status.no")} />
          <KeyValue label={t("diagnostics.key.detectedTargets")} value={runtimeStatus?.gsrMonitors?.join(", ") || "-"} />
          <KeyValue label={t("diagnostics.key.fps")} value={runtimeStatus?.gsrFps ?? "-"} />
          <KeyValue label={t("diagnostics.key.replaySeconds")} value={runtimeStatus?.gsrReplaySeconds ?? "-"} />
          <KeyValue label={t("diagnostics.key.quality")} value={runtimeStatus?.gsrQuality ?? "-"} />
          <KeyValue label={t("diagnostics.key.bitrateMode")} value={runtimeStatus?.gsrBitrateMode ?? "-"} />
          <KeyValue label={t("diagnostics.key.frameRateMode")} value={runtimeStatus?.gsrFrameRateMode ?? "-"} />
          <KeyValue label={t("diagnostics.key.keyframeInterval")} value={runtimeStatus?.gsrKeyframeIntervalSeconds ?? "-"} />
          <KeyValue
            label={t("diagnostics.key.restartReplay")}
            value={runtimeStatus?.gsrRestartReplayOnSave ? t("status.yes") : t("status.no")}
          />
          <KeyValue label={t("diagnostics.key.videoBitrate")} value={runtimeStatus?.gsrVideoBitrateKbps ?? "-"} />
          <KeyValue label={t("diagnostics.key.effectiveQ")} value={runtimeStatus?.gsrEffectiveQArgument ?? "-"} />
          <KeyValue label={t("diagnostics.key.saveQueue")} value={runtimeStatus?.gsrSaveQueueLen ?? "-"} />
          <KeyValue label={t("diagnostics.key.savesRequested")} value={runtimeStatus?.gsrTotalSavesRequested ?? "-"} />
          <KeyValue label={t("diagnostics.key.savesCompleted")} value={runtimeStatus?.gsrTotalSavesCompleted ?? "-"} />
          <KeyValue label={t("diagnostics.key.savesFailed")} value={runtimeStatus?.gsrTotalSavesFailed ?? "-"} />
          <KeyValue label={t("diagnostics.key.fdBackend")} value={runtimeStatus?.backendFdCount ?? "-"} />
          <KeyValue label={t("diagnostics.key.galleryScans")} value={runtimeStatus?.galleryScanCount ?? "-"} />
          <KeyValue label={t("diagnostics.key.lastScanMs")} value={runtimeStatus?.galleryLastScanMs ?? "-"} />
          <KeyValue label={t("diagnostics.key.activeScans")} value={runtimeStatus?.galleryActiveScans ?? "-"} />
          <KeyValue label={t("diagnostics.key.lastOutput")} value={runtimeStatus?.gsrLastOutput ?? "-"} />
          <KeyValue label={t("diagnostics.key.gsrError")} value={runtimeStatus?.gsrLastError ?? "-"} />
        </div>
        <pre className="command-line">{runtimeStatus?.gsrCommandLine ?? t("diagnostics.commandUnavailable")}</pre>
      </div>
    </section>
  );
}

function requirementsList(report: SystemRequirementsReport): RequirementCheck[] {
  return [
    report.war_thunder_api,
    report.flatpak,
    report.gsr_flatpak,
    report.gsr_native,
    report.ffmpeg,
    report.ffprobe,
    report.output_dir,
    report.config_dir,
  ];
}

function requirementLabel(check: RequirementCheck, t: Translate) {
  const labels: Record<string, string> = {
    war_thunder_api: t("diagnostics.tools.warThunderApi"),
    flatpak: t("diagnostics.tools.flatpak"),
    gsr_flatpak: t("diagnostics.tools.gsrFlatpak"),
    gsr_native: t("diagnostics.tools.gsrNative"),
    ffmpeg: t("diagnostics.tools.ffmpeg"),
    ffprobe: t("diagnostics.tools.ffprobe"),
    output_dir: t("diagnostics.tools.outputDir"),
    config_dir: t("diagnostics.tools.configDir"),
  };
  return labels[check.id] ?? check.label;
}

function RequirementCard({
  check,
  label,
  onCopyCommand,
  onOpenFolder,
}: {
  check: RequirementCheck;
  label: string;
  onCopyCommand?: () => void;
  onOpenFolder?: () => void;
}) {
  const { t } = useI18n();
  const Icon = requirementIcon(check.status);
  return (
    <article className={`requirement-card ${requirementClass(check.status)}`}>
      <div className="requirement-card-heading">
        <Icon size={19} />
        <div>
          <strong>{label}</strong>
          <span className={`status-chip ${requirementClass(check.status)}`}>{requirementStatusLabel(check.status, t)}</span>
        </div>
      </div>
      <p>{requirementSummary(check, t)}</p>
      {check.details && <small>{check.details}</small>}
      {(check.version || check.path) && (
        <div className="requirement-meta">
          {check.version && <code>{check.version}</code>}
          {check.path && <code>{check.path}</code>}
        </div>
      )}
      <div className="button-row">
        {onCopyCommand && (
          <button type="button" className="secondary" onClick={onCopyCommand}>
            <Clipboard size={15} />
            {t("diagnostics.actions.copyCommand")}
          </button>
        )}
        {onOpenFolder && (
          <button type="button" className="secondary" onClick={onOpenFolder}>
            <FolderOpen size={15} />
            {check.id === "config_dir" ? t("diagnostics.actions.openConfigFolder") : t("diagnostics.actions.openOutputFolder")}
          </button>
        )}
      </div>
    </article>
  );
}

function requirementIcon(status: RequirementStatus) {
  if (status === "ok") return CheckCircle2;
  if (status === "warning" || status === "unknown") return ShieldAlert;
  return XCircle;
}

function requirementClass(status: RequirementStatus) {
  if (status === "ok") return "ok";
  if (status === "warning" || status === "unknown") return "warn";
  return "error";
}

function requirementStatusLabel(status: RequirementStatus, t: Translate) {
  return t(`diagnostics.status.${status}`);
}

function requirementSummary(check: RequirementCheck, t: Translate) {
  if (check.summary_key) {
    return t(check.summary_key);
  }
  const fallbackKey = `diagnostics.summary.${check.id}.${check.status}`;
  const translated = t(fallbackKey);
  return translated === fallbackKey ? check.summary : translated;
}

function ConfigSection({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="panel config-section">
      <div className="panel-heading">
        <h3>{title}</h3>
      </div>
      <div className="form-grid">{children}</div>
    </section>
  );
}

function DiagnosticRow({ check }: { check: DoctorReport["checks"][number] }) {
  const Icon = check.status === "ok" ? CheckCircle2 : check.status === "warn" ? ShieldAlert : XCircle;
  return (
    <article className={`diagnostic-row ${check.status}`}>
      <Icon size={18} />
      <div>
        <strong>{check.name}</strong>
        <p>{check.message}</p>
        {check.hint && <small>{check.hint}</small>}
      </div>
    </article>
  );
}

function Field({ label, children, className }: { label: string; children: ReactNode; className?: string }) {
  return (
    <label className={className ? `field ${className}` : "field"}>
      <span>{label}</span>
      {children}
    </label>
  );
}

function KeyValue({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="kv">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function Empty({ label }: { label: string }) {
  return <div className="empty-state">{label}</div>;
}

function FilmFallback() {
  return (
    <div className="clip-fallback">
      <Monitor size={30} />
    </div>
  );
}

function clipToGalleryItem(clip: ClipInfo): GalleryClipItem {
  return {
    id: clip.path,
    status: "ready",
    reason: clip.reason,
    clipType: clip.clipType,
    exportType: clip.exportType,
    createdAt: String(Date.now() - clip.modifiedSecsAgo * 1000),
    title: clip.fileName,
    filePath: clip.path,
    thumbnailPath: clip.thumbnailPath ?? undefined,
    previewUrl: clip.previewUrl ?? undefined,
    durationSeconds: clip.durationSeconds,
    sizeBytes: clip.sizeBytes,
  };
}

function galleryItemToClipInfo(clip: GalleryClipItem): ClipInfo {
  return {
    path: clip.filePath ?? clip.id,
    thumbnailPath: clip.thumbnailPath,
    previewUrl: clip.previewUrl,
    fileName: clip.title,
    reason: clip.reason,
    clipType: clip.clipType,
    exportType: clip.exportType,
    sizeBytes: clip.sizeBytes ?? 0,
    durationSeconds: clip.durationSeconds ?? 0,
    modifiedSecsAgo: secondsAgo(clip.createdAt),
  };
}

function compareGalleryItems(a: GalleryClipItem, b: GalleryClipItem) {
  const rank = (status: ClipStatus) => (status === "failed" ? 1 : status === "ready" ? 2 : 0);
  const rankDiff = rank(a.status) - rank(b.status);
  if (rankDiff !== 0) {
    return rankDiff;
  }
  return secondsAgo(a.createdAt) - secondsAgo(b.createdAt);
}

function releaseVideoElement(video: HTMLVideoElement) {
  video.pause();
  video.removeAttribute("src");
  video.load();
}

function formatClipDuration(seconds: number) {
  const safe = Math.max(0, Math.round(seconds));
  const minutes = Math.floor(safe / 60);
  const rest = safe % 60;
  return `${String(minutes).padStart(2, "0")}:${String(rest).padStart(2, "0")}`;
}

function formatBytes(bytes: number) {
  if (!Number.isFinite(bytes) || bytes <= 0) {
    return "0 B";
  }
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index += 1;
  }
  return `${value.toFixed(index === 0 ? 0 : 1)} ${units[index]}`;
}

function relativeTime(seconds: number, t: Translate) {
  if (seconds < 60) {
    return t("gallery.modifiedNow");
  }
  if (seconds < 3600) {
    return t("gallery.modifiedMinutes", { count: Math.floor(seconds / 60) });
  }
  if (seconds < 86_400) {
    return t("gallery.modifiedHours", { count: Math.floor(seconds / 3600) });
  }
  return t("gallery.modifiedDays", { count: Math.floor(seconds / 86_400) });
}

function secondsAgo(value: string) {
  const numeric = Number(value);
  const timestamp = Number.isFinite(numeric) ? numeric : Date.parse(value);
  if (!Number.isFinite(timestamp)) {
    return 0;
  }
  return Math.max(0, Math.round((Date.now() - timestamp) / 1000));
}
