import { useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  Activity,
  CheckCircle2,
  Cpu,
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
  RuntimeStatus,
} from "./types";

const nav = [
  { id: "dashboard", label: "Tableau", icon: Home },
  { id: "clips", label: "Clips", icon: Video },
  { id: "config", label: "Configuration", icon: Settings },
  { id: "diagnostics", label: "Diagnostics", icon: Activity },
] as const;

const GALLERY_CACHE_TTL_MS = 10_000;
const GALLERY_REFRESH_DEBOUNCE_MS = 800;
const GALLERY_AUTO_REFRESH_MS = 5000;
const HOVER_PREVIEW_START_SECONDS = 0.75;

const reasonLabel: Record<ClipReason, string> = {
  target_destroyed: "Cible détruite",
  base_destroyed: "Base détruite",
  player_destroyed: "Joueur détruit",
  multi_kill: "Multi-kill",
  manual: "Manuel",
  unknown: "Clip",
};

const statusLabel: Record<ClipStatus, string> = {
  detected: "Détecté",
  recording: "Capture",
  encoding: "Encodage",
  saving: "Sauvegarde",
  ready: "Prêt",
  failed: "Erreur",
};

function gsrHealthLabel(health?: GsrHealth | null) {
  switch (health) {
    case "running":
      return "GPU Replay armé";
    case "saving_replay":
      return "Sauvegarde replay";
    case "starting":
      return "Démarrage";
    case "error":
      return "Erreur";
    case "not_available":
      return "Indisponible";
    case "stopped":
    default:
      return "Arrêté";
  }
}

export function App() {
  const store = useAppStore();
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
          title: reasonLabel[event.payload.kind] ?? "Événement",
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
          title: "Clip sauvegardé",
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
        store.addEvent({ kind: "system", title: "Clip échoué", detail: event.payload.message });
        store.showToast(`Clip échoué: ${event.payload.message}`);
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
          title: reasonLabel[event.kind] ?? "Événement",
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
  return (
    <aside className="sidebar">
      <div className="brand">
        <img className="logo brand-logo" src={appLogo} alt="WT Clipper" />
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
              {item.label}
            </button>
          );
        })}
      </nav>
      <div className="sidebar-footer">
        <span>Capture GSR</span>
        <strong>{config?.capture.target || "Target non configurée"}</strong>
      </div>
    </aside>
  );
}

function Topbar() {
  const { wtConnected, runtimeStatus } = useAppStore();
  return (
    <header className="topbar">
      <div>
        <h1>War Thunder clips</h1>
        <p>Capture replay GPU Screen Recorder, galerie et exports sociaux.</p>
      </div>
      <div className="topbar-status">
        <StatusPill connected={wtConnected} />
        <span className={`status-chip ${runtimeStatus?.gsrHealth === "running" ? "ok" : "warn"}`}>
          <Cpu size={15} />
          {gsrHealthLabel(runtimeStatus?.gsrHealth)}
        </span>
        <img className="topbar-logo" src={appLogo} alt="WT Clip" />
      </div>
    </header>
  );
}

function StatusPill({ connected }: { connected: boolean }) {
  return (
    <span className={`status-chip ${connected ? "ok" : "warn"}`}>
      <Zap size={15} />
      {connected ? "War Thunder connecté" : "War Thunder en attente"}
    </span>
  );
}

function Dashboard() {
  const state = useAppStore();
  const status = state.runtimeStatus;
  return (
    <section className="view-grid">
      <div className="hero-panel">
        <div>
          <span className="eyebrow">Backend actif</span>
          <h2>GPU Screen Recorder</h2>
          <p>
            {gsrHealthLabel(status?.gsrHealth)} · {status?.gsrMode ?? state.config?.capture.mode ?? "auto"} ·{" "}
            {status?.gsrTarget ?? state.config?.capture.target ?? "target non configurée"}
          </p>
        </div>
        <button type="button" className="primary" onClick={() => void invoke("save_manual_clip")}>
          <Save size={18} />
          Clip manuel
        </button>
      </div>

      <Metric icon={Video} label="Clips" value={state.clips.length} />
      <Metric icon={Gauge} label="Kills session" value={state.sessionKills} />
      <Metric icon={Wand2} label="Multi-kills" value={state.sessionMultiKills} />
      <Metric icon={HardDrive} label="Stockage" value={formatBytes(state.diskUsedBytes)} />

      <section className="panel wide">
        <div className="panel-heading">
          <div>
            <span className="eyebrow">Activité</span>
            <h3>Événements récents</h3>
          </div>
        </div>
        <div className="event-list">
          {state.events.length === 0 && <Empty label="Aucun événement reçu." />}
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
      showToast("Clip supprimé");
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
          <span className="eyebrow">Galerie</span>
          <h2>Clips</h2>
        </div>
        <button type="button" className="secondary" onClick={onRefresh}>
          <RefreshCcw size={17} />
          Rafraîchir
        </button>
      </div>

      <div className="toolbar">
        <input
          type="search"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Rechercher un clip"
        />
        <select value={filter} onChange={(event) => setFilter(event.target.value as "all" | ClipReason)}>
          <option value="all">Tous</option>
          {Object.entries(reasonLabel).map(([value, label]) => (
            <option key={value} value={value}>
              {label}
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
          Assembler ({selectedReadyClips.length})
        </button>
      </div>

      <div className="clip-grid">
        {galleryItems.length === 0 && <Empty label="Aucun clip dans la bibliothèque." />}
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
        <p>{clip.error ?? statusLabel[clip.status]}</p>
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
          {relativeTime(clip.modifiedSecsAgo)}
        </p>
      </div>
      <div className="clip-actions">
        <button type="button" title="Éditer" onClick={() => onEdit(clip)}>
          <Wand2 size={16} />
        </button>
        <button type="button" title="Ouvrir le dossier" onClick={() => void invoke("open_parent_folder", { path: clip.path })}>
          <FolderOpen size={16} />
        </button>
        <button type="button" title="Supprimer" onClick={() => onDelete(clip.path)}>
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
  const { config, runtimeStatus, setConfig, setRuntimeStatus, showToast } = useAppStore();
  const [draft, setDraft] = useState<AppConfig | null>(config);
  const [saving, setSaving] = useState(false);
  const [checkingUpdate, setCheckingUpdate] = useState(false);

  useEffect(() => {
    setDraft(config);
  }, [config]);

  if (!draft) {
    return <Empty label="Configuration en chargement." />;
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
      showToast("Configuration sauvegardée");
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
      showToast(result.available ? "Mise à jour disponible" : "WT Clip est à jour");
    } catch (error) {
      showToast(String(error));
    } finally {
      setCheckingUpdate(false);
    }
  }

  return (
    <section className="config-layout">
      <ConfigSection title="Clip">
        <Field label="Délai après événement">
          <input
            type="number"
            min={0}
            value={draft.clip.post_event_seconds}
            onChange={(event) => updateClip("post_event_seconds", Number(event.target.value))}
          />
        </Field>
        <Field label="Fenêtre multi-kill">
          <input
            type="number"
            min={1}
            value={draft.clip.multi_kill_window_seconds}
            onChange={(event) => updateClip("multi_kill_window_seconds", Number(event.target.value))}
          />
        </Field>
      </ConfigSection>

      <ConfigSection title="Bibliothèque">
        <Field label="Dossier bibliothèque">
          <input value={draft.library.output_dir} onChange={(event) => updateLibrary("output_dir", event.target.value)} />
        </Field>
      </ConfigSection>

      <ConfigSection title="GPU Screen Recorder">
        <Field label="Target">
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
              ? `Target valide${runtimeStatus?.gsrMonitors?.length ? ` · détectés: ${runtimeStatus.gsrMonitors.join(", ")}` : ""}`
              : `Target introuvable. Sélectionne une valeur détectée${runtimeStatus?.gsrMonitors?.length ? `: ${runtimeStatus.gsrMonitors.join(", ")}` : "."}`}
          </p>
        </Field>
        <Field label="Mode">
          <select value={draft.capture.mode} onChange={(event) => updateCapture("mode", event.target.value as AppConfig["capture"]["mode"])}>
            <option value="auto">Auto</option>
            <option value="native">Native</option>
            <option value="flatpak">Flatpak</option>
          </select>
        </Field>
        <Field label="FPS">
          <input type="number" min={1} value={draft.capture.fps} onChange={(event) => updateCapture("fps", Number(event.target.value))} />
        </Field>
        <Field label="Mode FPS">
          <select
            value={draft.capture.frame_rate_mode}
            onChange={(event) => updateCapture("frame_rate_mode", event.target.value as AppConfig["capture"]["frame_rate_mode"])}
          >
            <option value="cfr">Constant (CFR)</option>
            <option value="vfr">Variable (VFR)</option>
            <option value="content">Content</option>
          </select>
        </Field>
        <Field label="Intervalle keyframe">
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
        <Field label="Durée replay">
          <input
            type="number"
            min={5}
            value={draft.capture.replay_seconds}
            onChange={(event) => updateCapture("replay_seconds", Number(event.target.value))}
          />
        </Field>
        <Field label="Conteneur">
          <select value={draft.capture.container} onChange={(event) => updateCapture("container", event.target.value as AppConfig["capture"]["container"])}>
            <option value="mp4">MP4</option>
            <option value="mkv">MKV</option>
          </select>
        </Field>
        <Field label="Codec">
          <select value={draft.capture.codec} onChange={(event) => updateCapture("codec", event.target.value as AppConfig["capture"]["codec"])}>
            <option value="h264">H.264</option>
            <option value="hevc">HEVC</option>
            <option value="av1">AV1</option>
          </select>
        </Field>
        <Field label="Encodeur">
          <select value={draft.capture.encoder} onChange={(event) => updateCapture("encoder", event.target.value as AppConfig["capture"]["encoder"])}>
            <option value="gpu">GPU</option>
            <option value="cpu">CPU</option>
          </select>
        </Field>
        <Field label="Mode bitrate">
          <select
            value={draft.capture.bitrate_mode}
            onChange={(event) => updateCapture("bitrate_mode", event.target.value as AppConfig["capture"]["bitrate_mode"])}
          >
            <option value="auto">Auto</option>
            <option value="qp">QP qualité constante</option>
            <option value="vbr">VBR</option>
            <option value="cbr">CBR bitrate fixe</option>
          </select>
        </Field>
        {draft.capture.bitrate_mode === "cbr" ? (
          <Field label="Bitrate vidéo kbps">
            <input
              type="number"
              min={1000}
              step={500}
              value={draft.capture.video_bitrate_kbps}
              onChange={(event) => updateCapture("video_bitrate_kbps", Number(event.target.value))}
            />
          </Field>
        ) : (
          <Field label="Qualité">
            <select value={draft.capture.quality} onChange={(event) => updateCapture("quality", event.target.value as AppConfig["capture"]["quality"])}>
              <option value="medium">Medium</option>
              <option value="high">High</option>
              <option value="very_high">Very high</option>
              <option value="ultra">Ultra</option>
            </select>
          </Field>
        )}
        <p className="help-text">
          Pour War Thunder en 1080p60, 20000-30000 kbps donne une meilleure qualité mais augmente la taille des fichiers.
        </p>
        <Field label="Redémarrer le replay après sauvegarde" className="toggle-field">
          <div className="checkbox-row">
            <input
              type="checkbox"
              checked={draft.capture.restart_replay_on_save}
              onChange={(event) => updateCapture("restart_replay_on_save", event.target.checked)}
            />
            <strong>{draft.capture.restart_replay_on_save ? "Oui, vider le buffer" : "Non, conserver le buffer"}</strong>
          </div>
        </Field>
        <Field label="Audio" className="toggle-field">
          <div className="checkbox-row">
            <input
              type="checkbox"
              checked={draft.capture.audio_enabled}
              onChange={(event) => updateCapture("audio_enabled", event.target.checked)}
            />
            <strong>Activé</strong>
          </div>
        </Field>
        <Field label="Entrée audio">
          <input value={draft.capture.audio_input} onChange={(event) => updateCapture("audio_input", event.target.value)} />
        </Field>
        <Field label="Dossier capture GSR">
          <input value={draft.capture.output_dir} onChange={(event) => updateCapture("output_dir", event.target.value)} />
        </Field>
      </ConfigSection>

      <ConfigSection title="War Thunder">
        <Field label="Base URL">
          <input value={draft.war_thunder.base_url} onChange={(event) => updateWt("base_url", event.target.value)} />
        </Field>
        <Field label="Joueur">
          <input
            value={draft.war_thunder.player_name ?? ""}
            onChange={(event) => updateWt("player_name", event.target.value || null)}
          />
        </Field>
        <Field label="Poll ms">
          <input
            type="number"
            min={100}
            value={draft.war_thunder.poll_interval_ms}
            onChange={(event) => updateWt("poll_interval_ms", Number(event.target.value))}
          />
        </Field>
        <Field label="Timeout ms">
          <input
            type="number"
            min={100}
            value={draft.war_thunder.request_timeout_ms}
            onChange={(event) => updateWt("request_timeout_ms", Number(event.target.value))}
          />
        </Field>
      </ConfigSection>

      <ConfigSection title="Déclencheurs">
        {Object.entries(draft.triggers).map(([key, value]) => (
          <label key={key} className="inline-toggle">
            <input type="checkbox" checked={value} onChange={(event) => updateTrigger(key as keyof AppConfig["triggers"], event.target.checked)} />
            {reasonLabel[key as ClipReason] ?? key}
          </label>
        ))}
      </ConfigSection>

      <ConfigSection title="Stockage">
        <Field label="Max clips">
          <input type="number" min={1} value={draft.storage.max_clips} onChange={(event) => updateStorage("max_clips", Number(event.target.value))} />
        </Field>
        <Field label="Max Go">
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
          {saving ? "Sauvegarde..." : "Sauvegarder"}
        </button>
        <button type="button" className="secondary" onClick={checkForUpdates} disabled={checkingUpdate}>
          <RefreshCcw size={17} />
          {checkingUpdate ? "Vérification..." : "Vérifier les mises à jour"}
        </button>
        {runtimeStatus?.configRestartRequired && <span className="status-chip warn">Redémarrage GSR requis</span>}
      </div>
    </section>
  );
}

function Diagnostics() {
  const { diagnostics, runtimeStatus, setDiagnostics, setDiagnosticsRunning, setRuntimeStatus, diagnosticsRunning, showToast } =
    useAppStore();
  const [gsrTestRunning, setGsrTestRunning] = useState(false);

  async function run() {
    setDiagnosticsRunning(true);
    try {
      const [report, status] = await Promise.all([
        invoke<DoctorReport>("run_diagnostics"),
        invoke<RuntimeStatus>("get_runtime_status"),
      ]);
      setDiagnostics(report);
      setRuntimeStatus(status);
    } catch (error) {
      showToast(String(error));
      setDiagnosticsRunning(false);
    }
  }

  async function restartGpuRecorder() {
    try {
      await invoke("restart_gpu_recorder");
      const status = await invoke<RuntimeStatus>("get_runtime_status");
      setRuntimeStatus(status);
      showToast("GPU recorder redémarré");
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
      showToast(`Replay GSR sauvegardé: ${path}`);
    } catch (error) {
      showToast(String(error));
    } finally {
      setGsrTestRunning(false);
    }
  }

  const checks = diagnostics?.checks ?? [];
  const counts = checks.reduce(
    (acc, check) => ({ ...acc, [check.status]: acc[check.status] + 1 }),
    { ok: 0, warn: 0, error: 0 },
  );

  return (
    <section className="diagnostics-grid">
      <div className="panel wide">
        <div className="panel-heading">
          <div>
            <span className="eyebrow">Diagnostics</span>
            <h2>GPU Screen Recorder</h2>
          </div>
          <div className="button-row">
            <button type="button" className="secondary" onClick={run} disabled={diagnosticsRunning}>
              <RefreshCcw size={17} />
              {diagnosticsRunning ? "Analyse..." : "Relancer"}
            </button>
            <button type="button" className="secondary" onClick={restartGpuRecorder}>
              <Cpu size={17} />
              Redémarrer GPU Recorder
            </button>
            <button type="button" className="primary" onClick={testGsrSaveReplay} disabled={gsrTestRunning}>
              <Play size={17} />
              Tester sauvegarde GSR
            </button>
          </div>
        </div>
        <div className="status-summary">
          <span className="status-chip ok">{counts.ok} OK</span>
          <span className="status-chip warn">{counts.warn} avertissements</span>
          <span className="status-chip error">{counts.error} erreurs</span>
        </div>
        <div className="diagnostic-list">
          {checks.map((check) => (
            <DiagnosticRow key={check.name} check={check} />
          ))}
        </div>
      </div>

      <div className="panel wide">
        <div className="panel-heading">
          <div>
            <span className="eyebrow">Runtime</span>
            <h3>État GSR</h3>
          </div>
        </div>
        <div className="kv-grid">
          <KeyValue label="Disponible" value={runtimeStatus?.gsrAvailable ? "Oui" : "Non"} />
          <KeyValue label="État" value={gsrHealthLabel(runtimeStatus?.gsrHealth)} />
          <KeyValue label="Mode" value={runtimeStatus?.gsrMode ?? "-"} />
          <KeyValue label="PID wrapper" value={runtimeStatus?.gsrWrapperPid ?? "-"} />
          <KeyValue label="PID recorder" value={runtimeStatus?.gsrRecorderPid ?? "-"} />
          <KeyValue label="PID signal" value={runtimeStatus?.gsrSignalPid ?? "-"} />
          <KeyValue label="Target" value={runtimeStatus?.gsrTarget ?? "-"} />
          <KeyValue label="Target valide" value={runtimeStatus?.gsrTargetValid ? "Oui" : "Non"} />
          <KeyValue label="Targets détectées" value={runtimeStatus?.gsrMonitors?.join(", ") || "-"} />
          <KeyValue label="FPS" value={runtimeStatus?.gsrFps ?? "-"} />
          <KeyValue label="Replay seconds" value={runtimeStatus?.gsrReplaySeconds ?? "-"} />
          <KeyValue label="Quality" value={runtimeStatus?.gsrQuality ?? "-"} />
          <KeyValue label="Bitrate mode" value={runtimeStatus?.gsrBitrateMode ?? "-"} />
          <KeyValue label="Frame rate mode" value={runtimeStatus?.gsrFrameRateMode ?? "-"} />
          <KeyValue label="Keyframe interval seconds" value={runtimeStatus?.gsrKeyframeIntervalSeconds ?? "-"} />
          <KeyValue
            label="Restart replay on save"
            value={runtimeStatus?.gsrRestartReplayOnSave ? "yes" : "no"}
          />
          <KeyValue label="Video bitrate kbps" value={runtimeStatus?.gsrVideoBitrateKbps ?? "-"} />
          <KeyValue label="Effective -q" value={runtimeStatus?.gsrEffectiveQArgument ?? "-"} />
          <KeyValue label="Save queue" value={runtimeStatus?.gsrSaveQueueLen ?? "-"} />
          <KeyValue label="Saves requested" value={runtimeStatus?.gsrTotalSavesRequested ?? "-"} />
          <KeyValue label="Saves completed" value={runtimeStatus?.gsrTotalSavesCompleted ?? "-"} />
          <KeyValue label="Saves failed" value={runtimeStatus?.gsrTotalSavesFailed ?? "-"} />
          <KeyValue label="FD backend" value={runtimeStatus?.backendFdCount ?? "-"} />
          <KeyValue label="Scans galerie" value={runtimeStatus?.galleryScanCount ?? "-"} />
          <KeyValue label="Dernier scan ms" value={runtimeStatus?.galleryLastScanMs ?? "-"} />
          <KeyValue label="Scans actifs" value={runtimeStatus?.galleryActiveScans ?? "-"} />
          <KeyValue label="Dernière sortie" value={runtimeStatus?.gsrLastOutput ?? "-"} />
          <KeyValue label="Erreur GSR" value={runtimeStatus?.gsrLastError ?? "-"} />
        </div>
        <pre className="command-line">{runtimeStatus?.gsrCommandLine ?? "Commande GSR indisponible"}</pre>
      </div>
    </section>
  );
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

function relativeTime(seconds: number) {
  if (seconds < 60) {
    return "à l'instant";
  }
  if (seconds < 3600) {
    return `${Math.floor(seconds / 60)} min`;
  }
  if (seconds < 86_400) {
    return `${Math.floor(seconds / 3600)} h`;
  }
  return `${Math.floor(seconds / 86_400)} j`;
}

function secondsAgo(value: string) {
  const numeric = Number(value);
  const timestamp = Number.isFinite(numeric) ? numeric : Date.parse(value);
  if (!Number.isFinite(timestamp)) {
    return 0;
  }
  return Math.max(0, Math.round((Date.now() - timestamp) / 1000));
}
