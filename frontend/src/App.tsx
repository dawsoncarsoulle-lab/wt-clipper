import { useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { motion, AnimatePresence } from "framer-motion";
import {
  Activity,
  AlertTriangle,
  CheckCircle2,
  CircleDot,
  Clapperboard,
  Clock3,
  Cpu,
  FolderOpen,
  Gauge,
  HardDrive,
  LayoutDashboard,
  Play,
  RefreshCcw,
  Save,
  Search,
  Settings,
  ShieldAlert,
  Sparkles,
  Trash2,
  Video,
  Wifi,
  WifiOff,
  Wrench,
  XCircle,
  Zap,
} from "lucide-react";
import { useAppStore } from "./store";
import type { AppConfig, ClipInfo, ClipReason, DoctorReport, RuntimeStatus } from "./types";
import type { ClipStatus, ClipStatusChangedPayload, GalleryClipItem } from "./types";
import brandLogo from "./assets/brand/WT_clipper_brand.png";

const nav = [
  { id: "dashboard", label: "Dashboard", icon: LayoutDashboard },
  { id: "clips", label: "Clips", icon: Clapperboard },
  { id: "config", label: "Configuration", icon: Settings },
  { id: "diagnostics", label: "Diagnostics", icon: Wrench },
] as const;

const reasonLabel: Record<ClipReason, string> = {
  "target-destroyed": "KILL",
  "base-destroyed": "BASE",
  "player-destroyed": "DEATH",
  "multi-kill": "MULTI",
  manual: "MANUAL",
  unknown: "CLIP",
};

const processingReasonLabel: Record<ClipReason, string> = {
  "target-destroyed": "Cible détruite",
  "base-destroyed": "Base détruite",
  "player-destroyed": "Joueur détruit",
  "multi-kill": "Multi-kill",
  manual: "Clip manuel",
  unknown: "Clip",
};

export function App() {
  const store = useAppStore();
  const seenRuntimeEvents = useRef(new Set<string>());

  useEffect(() => {
    void bootstrap();
    const runtimePoll = window.setInterval(() => {
      void refreshRuntimeStatus().catch((error) =>
        console.info("[FRONTEND] runtime status refresh failed", error),
      );
    }, 1000);
    console.info("[FRONTEND] listening to wt-connected");
    console.info("[FRONTEND] listening to wt-disconnected");
    console.info("[FRONTEND] listening to buffer-progress");
    console.info("[FRONTEND] listening to kill-detected");
    console.info("[FRONTEND] listening to clip-saved");
    const unsubs = [
      listen("wt-connected", () => {
        console.info("[FRONTEND] received wt-connected");
        store.setWtConnected(true);
      }),
      listen("wt-disconnected", () => {
        console.info("[FRONTEND] received wt-disconnected");
        store.setWtConnected(false);
      }),
      listen<{ reason: ClipReason; vehicle?: string; target?: string; description: string }>(
        "kill-detected",
        (event) => {
          console.info("[FRONTEND] received kill-detected", event.payload);
          store.addEvent({
            kind: event.payload.reason,
            title: reasonLabel[event.payload.reason],
            detail: event.payload.description,
          });
        },
      ),
      listen<ClipInfo>("clip-saved", (event) => {
        console.info("[FRONTEND] received clip-saved", event.payload);
        store.addClip(event.payload);
        void refreshClips().catch((error) => {
          console.info("[FRONTEND] refresh after clip-saved failed", error);
        });
        store.addEvent({
          kind: event.payload.reason,
          title: "Clip sauvegardé",
          detail: event.payload.fileName,
        });
        store.showToast("Clip sauvegardé");
      }),
      listen<ClipStatusChangedPayload>("clip-status-changed", (event) => {
        console.info("[FRONTEND] received clip-status-changed", event.payload);
        store.updateClipStatus(event.payload);
        if (event.payload.status === "detected") {
          store.showToast("Clip détecté — création en cours...");
        } else if (event.payload.status === "ready") {
          store.showToast("Clip prêt");
        } else if (event.payload.status === "failed") {
          store.showToast("Erreur pendant la création du clip");
        }
      }),
      listen<{ message: string }>("clip-failed", (event) => {
        console.info("[FRONTEND] received clip-failed", event.payload);
        store.showToast(event.payload.message);
      }),
      listen<{ filledSecs: number; totalSecs: number }>("buffer-progress", (event) => {
        console.info("[FRONTEND] received buffer-progress", event.payload);
        store.setBuffer(event.payload.filledSecs, event.payload.totalSecs);
      }),
      listen<{ usedBytes: number }>("disk-usage", (event) =>
        store.setDiskUsedBytes(event.payload.usedBytes),
      ),
      listen<{ clips: ClipInfo[]; totalBytes: number }>("clips-loaded", (event) => {
        store.setClips(event.payload.clips);
        store.setDiskUsedBytes(event.payload.totalBytes);
      }),
      listen<DoctorReport>("diagnostics-ready", (event) => store.setDiagnostics(event.payload)),
    ];
    return () => {
      window.clearInterval(runtimePoll);
      void Promise.all(unsubs).then((items) => items.forEach((unlisten) => unlisten()));
    };
  }, []);

  async function bootstrap() {
    try {
      const [config, diagnostics] = await Promise.all([
        invoke<AppConfig>("get_config"),
        invoke<DoctorReport>("run_diagnostics"),
      ]);
      store.setConfig(config);
      store.setDiagnostics(diagnostics);
      await refreshClips();
      await refreshRuntimeStatus();
    } catch (error) {
      store.showToast(String(error));
    }
  }

  async function refreshClips() {
    const clips = await invoke<ClipInfo[]>("load_clips");
    store.setClips(clips);
    return clips;
  }

  async function refreshRuntimeStatus() {
    const status = await invoke<RuntimeStatus>("get_runtime_status");
    applyRuntimeStatus(status);
  }

  function applyRuntimeStatus(status: RuntimeStatus) {
    store.setWtConnected(status.wtConnected);
    store.setBuffer(status.bufferFilledSecs, status.bufferTotalSecs);
    for (const event of [...status.recentEvents].reverse()) {
      if (seenRuntimeEvents.current.has(event.id)) {
        continue;
      }
      seenRuntimeEvents.current.add(event.id);
      store.addEventEntry({
        id: event.id,
        at: event.at,
        kind: event.kind,
        title: reasonLabel[event.kind],
        detail: event.description,
      });
    }
  }

  return (
    <div className="min-h-screen min-w-[1000px] bg-obsidian text-zinc-100">
      <div className="noise" />
      <div className="flex h-screen overflow-hidden">
        <Sidebar />
        <main className="relative flex min-w-0 flex-1 flex-col">
          <Topbar />
          <div className="min-h-0 flex-1 overflow-y-auto px-7 pb-8 pt-5">
            <AnimatePresence mode="wait">
              <motion.div
                key={store.activeView}
                initial={{ opacity: 0, y: 10 }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0, y: -8 }}
                transition={{ duration: 0.18 }}
              >
                {store.activeView === "dashboard" && <Dashboard />}
                {store.activeView === "clips" && <Clips />}
                {store.activeView === "config" && <Configuration />}
                {store.activeView === "diagnostics" && <Diagnostics />}
              </motion.div>
            </AnimatePresence>
          </div>
        </main>
      </div>
      <AnimatePresence>
        {store.toast && (
          <motion.div
            initial={{ opacity: 0, y: 16 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0, y: 16 }}
            className="fixed bottom-6 right-7 rounded-lg border border-line bg-[#151b25]/95 px-4 py-3 text-sm shadow-premium"
          >
            {store.toast}
          </motion.div>
        )}
      </AnimatePresence>
      <ProcessingPopup />
    </div>
  );
}

function Sidebar() {
  const { activeView, setActiveView, config } = useAppStore();
  return (
    <aside className="z-10 flex w-[248px] shrink-0 flex-col border-r border-line bg-[#0b0e14]/92 px-4 py-5">
      <div className="mb-7 flex items-center gap-3 px-2">
        <img
          src={brandLogo}
          alt="WT Clipper"
          className="h-11 w-11 rounded-lg border border-ember/35 bg-[#0b0e14] object-cover shadow-glow"
        />
        <div>
          <div className="text-[17px] font-black tracking-wide text-white">WT CLIPPER</div>
          <div className="text-xs font-medium uppercase text-zinc-500">combat recorder</div>
        </div>
      </div>
      <button className="primary-action mb-6" onClick={() => void invoke("save_manual_clip")}>
        <Play className="h-4 w-4" />
        Clip manuel
      </button>
      <div className="space-y-1">
        {nav.map((item) => {
          const Icon = item.icon;
          const active = activeView === item.id;
          return (
            <button
              key={item.id}
              onClick={() => setActiveView(item.id)}
              className={`nav-item ${active ? "nav-item-active" : ""}`}
            >
              <Icon className="h-4 w-4" />
              {item.label}
            </button>
          );
        })}
      </div>
      <div className="mt-auto rounded-lg border border-line bg-white/[0.035] p-3">
        <div className="mb-1 text-xs uppercase text-zinc-500">Pilote</div>
        <div className="truncate text-sm font-semibold text-zinc-200">
          {config?.war_thunder.player_name || "Non configuré"}
        </div>
      </div>
    </aside>
  );
}

function Topbar() {
  const { wtConnected, bufferFilledSecs, bufferTotalSecs } = useAppStore();
  const progress = Math.round((bufferFilledSecs / Math.max(1, bufferTotalSecs)) * 100);
  return (
    <header className="z-10 flex h-[66px] shrink-0 items-center justify-between border-b border-line bg-[#0b0e14]/72 px-7 backdrop-blur">
      <div className="flex items-center gap-3">
        <StatusPill connected={wtConnected} />
        <div className="h-4 w-px bg-white/10" />
        <div className="flex items-center gap-2 text-sm text-zinc-400">
          <Gauge className="h-4 w-4 text-amberline" />
          Buffer {progress}%
        </div>
      </div>
      <div className="flex items-center gap-2 text-sm text-zinc-500">
        <CircleDot className="h-3.5 w-3.5 text-ember" />
        War Thunder localhost : 127.0.0.1:8111
      </div>
    </header>
  );
}

function StatusPill({ connected }: { connected: boolean }) {
  return (
    <div
      className={`inline-flex items-center gap-2 rounded-full border px-3 py-1.5 text-sm font-semibold ${
        connected
          ? "border-emerald-400/25 bg-emerald-400/10 text-emerald-300"
          : "border-red-400/25 bg-red-400/10 text-red-300"
      }`}
    >
      {connected ? <Wifi className="h-4 w-4" /> : <WifiOff className="h-4 w-4" />}
      {connected ? "War Thunder connecté" : "War Thunder déconnecté"}
    </div>
  );
}

function Dashboard() {
  const state = useAppStore();
  const progress = Math.min(100, (state.bufferFilledSecs / Math.max(1, state.bufferTotalSecs)) * 100);
  return (
    <div className="space-y-6">
      <section className="hero-panel">
        <div className="relative z-10 max-w-2xl">
          <div className="mb-5 flex items-center gap-3">
            <StatusPill connected={state.wtConnected} />
            <span className="rounded-full border border-line bg-white/5 px-3 py-1.5 text-sm text-zinc-400">
              {state.config?.clip.quality.toUpperCase()} · {state.config?.clip.fps} FPS
            </span>
          </div>
          <h1 className="text-4xl font-black tracking-normal text-white">Replay buffer armé</h1>
          <p className="mt-3 max-w-xl text-sm leading-6 text-zinc-400">
            Capture automatique des kills personnels, génération de clips WebM et suivi temps réel
            du serveur War Thunder local.
          </p>
          <button className="primary-action mt-7 w-fit px-5" onClick={() => void invoke("save_manual_clip")}>
            <Play className="h-4 w-4" />
            Clip manuel
          </button>
        </div>
        <div className="buffer-orb">
          <svg viewBox="0 0 120 120" className="h-40 w-40">
            <circle cx="60" cy="60" r="48" stroke="rgba(255,255,255,.08)" strokeWidth="10" fill="none" />
            <circle
              cx="60"
              cy="60"
              r="48"
              stroke="#ff5a2f"
              strokeWidth="10"
              fill="none"
              strokeLinecap="round"
              strokeDasharray={`${progress * 3.015} 301.5`}
              transform="rotate(-90 60 60)"
            />
          </svg>
          <div className="absolute text-center">
            <div className="text-3xl font-black text-white">{Math.round(progress)}%</div>
            <div className="text-xs uppercase text-zinc-500">buffer</div>
          </div>
        </div>
      </section>
      <div className="grid grid-cols-4 gap-4">
        <Metric icon={Zap} label="Kills session" value={state.sessionKills} />
        <Metric icon={Sparkles} label="Multi-kills" value={state.sessionMultiKills} />
        <Metric icon={Video} label="Clips sauvegardés" value={state.clipsSaved} />
        <Metric icon={HardDrive} label="Stockage" value={formatBytes(state.diskUsedBytes)} />
      </div>
      <section>
        <div className="mb-3 flex items-center justify-between">
          <h2 className="text-lg font-bold text-white">Événements récents</h2>
          <Activity className="h-4 w-4 text-zinc-500" />
        </div>
        <div className="event-feed">
          {state.events.length === 0 ? (
            <Empty label="Aucun événement pour cette session" />
          ) : (
            state.events.map((event) => (
              <div key={event.id} className="event-row">
                <span className="font-mono text-xs text-zinc-500">{event.at}</span>
                <span className="badge">{String(event.title)}</span>
                <span className="truncate text-sm text-zinc-300">{event.detail}</span>
              </div>
            ))
          )}
        </div>
      </section>
    </div>
  );
}

function ProcessingPopup() {
  const { processingClips } = useAppStore();
  const visibleClips = processingClips.filter(
    (clip) => clip.status !== "ready" || clip.filePath == null,
  );

  if (visibleClips.length === 0) {
    return null;
  }

  return (
    <AnimatePresence>
      <motion.aside
        initial={{ opacity: 0, y: 16, scale: 0.98 }}
        animate={{ opacity: 1, y: 0, scale: 1 }}
        exit={{ opacity: 0, y: 16, scale: 0.98 }}
        transition={{ duration: 0.18 }}
        className="fixed right-6 top-[84px] z-40 w-[360px] rounded-lg border border-line bg-[#111722]/96 p-4 shadow-premium backdrop-blur"
      >
        <div className="flex items-start justify-between gap-3">
          <div>
            <div className="text-xs uppercase tracking-wide text-zinc-500">Upload en cours</div>
            <div className="mt-1 text-base font-bold text-white">
              {visibleClips.length} clip{visibleClips.length > 1 ? "s" : ""} en traitement
            </div>
          </div>
          <RefreshCcw className="mt-0.5 h-4 w-4 animate-spin text-ember" />
        </div>
        <div className="mt-4 space-y-3">
          {visibleClips.slice(0, 3).map((clip) => (
            <div key={clip.id} className="rounded-md border border-white/10 bg-white/[0.035] p-3">
              <div className="flex items-center justify-between gap-3">
                <div className="min-w-0">
                  <div className="truncate text-sm font-bold text-white">{clip.title}</div>
                  <div className="mt-1 text-xs text-zinc-400">{processingReasonLabel[clip.reason]}</div>
                </div>
                <span className={`reason reason-${clip.reason}`}>{getClipStatusLabel(clip.status)}</span>
              </div>
              <div className="mt-3 h-2 overflow-hidden rounded-full bg-white/10">
                <div
                  className={`processing-progress ${clip.progress == null && clip.status !== "failed" ? "indeterminate" : ""}`}
                  style={{
                    width: `${Math.max(
                      6,
                      Math.min(
                        100,
                        clip.progress ?? (clip.status === "failed" ? 100 : 42),
                      ),
                    )}%`,
                  }}
                />
              </div>
              <div className="mt-2 flex items-center justify-between text-xs text-zinc-500">
                <span>{relativeCreatedAt(clip.createdAt)}</span>
                <span>{clip.status === "failed" ? "Erreur" : "En cours"}</span>
              </div>
              {clip.status === "failed" && clip.error && (
                <p className="mt-2 max-h-10 overflow-hidden text-xs text-[#ffb7aa]">{clip.error}</p>
              )}
            </div>
          ))}
        </div>
      </motion.aside>
    </AnimatePresence>
  );
}

function Metric({ icon: Icon, label, value }: { icon: typeof Zap; label: string; value: string | number }) {
  return (
    <div className="metric-panel">
      <Icon className="h-5 w-5 text-ember" />
      <div>
        <div className="text-2xl font-black text-white">{value}</div>
        <div className="text-xs uppercase text-zinc-500">{label}</div>
      </div>
    </div>
  );
}

function Clips() {
  const { clips, processingClips, setClips, showToast } = useAppStore();
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<"all" | ClipReason>("all");
  const galleryItems = useMemo(() => {
    const readyPaths = new Set(clips.map((clip) => clip.path));
    const processing = processingClips.filter((clip) => !clip.filePath || !readyPaths.has(clip.filePath));
    const ready = clips.map(clipToGalleryItem);
    return [...processing, ...ready].filter((clip) => {
      const haystack = `${clip.title} ${clip.filePath ?? ""}`.toLowerCase();
      const matchesQuery = haystack.includes(query.toLowerCase());
      const matchesFilter = filter === "all" || clip.reason === filter;
      return matchesQuery && matchesFilter;
    });
  }, [clips, processingClips, query, filter]);
  const activeProcessingCount = processingClips.filter((clip) =>
    ["detected", "recording", "encoding", "saving"].includes(clip.status),
  ).length;

  async function refresh() {
    const next = await invoke<ClipInfo[]>("load_clips");
    setClips(next);
  }

  async function remove(path: string) {
    await invoke("delete_clip", { path });
    setClips(clips.filter((clip) => clip.path !== path));
    showToast("Clip supprimé");
  }

  return (
    <div className="space-y-5">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-black text-white">Bibliothèque</h1>
          <p className="text-sm text-zinc-500">{galleryItems.length} clips affichés</p>
        </div>
        <div className="flex gap-2">
          <button className="ghost-button" onClick={() => void refresh()}>
            <RefreshCcw className="h-4 w-4" />
            Rafraîchir
          </button>
          <button className="ghost-button" onClick={() => void invoke("open_output_folder")}>
            <FolderOpen className="h-4 w-4" />
            Dossier
          </button>
        </div>
      </div>
      <div className="toolbar">
        <div className="search-box">
          <Search className="h-4 w-4 text-zinc-500" />
          <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Rechercher un clip" />
        </div>
        <div className="segmented">
          {(["all", "target-destroyed", "base-destroyed", "multi-kill", "player-destroyed", "manual"] as const).map((item) => (
            <button key={item} className={filter === item ? "active" : ""} onClick={() => setFilter(item)}>
              {item === "all" ? "Tous" : reasonLabel[item]}
            </button>
          ))}
        </div>
      </div>
      {activeProcessingCount > 0 && (
        <div className="processing-banner">
          {activeProcessingCount} clip{activeProcessingCount > 1 ? "s" : ""} en cours de traitement
        </div>
      )}
      {galleryItems.length === 0 ? (
        <Empty label="Aucun clip trouvé" />
      ) : (
        <div className="clip-grid">
          {galleryItems.map((clip) => (
            clip.status === "ready" && clip.filePath ? (
              <ClipCard
                key={clip.id}
                clip={galleryItemToClipInfo(clip)}
                onDelete={() => void remove(clip.filePath!)}
              />
            ) : (
              <ClipProcessingCard key={clip.id} clip={clip} />
            )
          ))}
        </div>
      )}
    </div>
  );
}

function clipToGalleryItem(clip: ClipInfo): GalleryClipItem {
  return {
    id: clip.path,
    status: "ready",
    reason: clip.reason,
    createdAt: String(Date.now() - clip.modifiedSecsAgo * 1000),
    title: clip.fileName,
    filePath: clip.path,
    thumbnailPath: clip.thumbnailPath ?? undefined,
    previewUrl: clip.previewUrl ?? undefined,
    durationSeconds: clip.durationSeconds,
    sizeBytes: clip.sizeBytes,
    progress: 100,
  };
}

function galleryItemToClipInfo(clip: GalleryClipItem): ClipInfo {
  return {
    path: clip.filePath ?? "",
    thumbnailPath: clip.thumbnailPath ?? null,
    previewUrl: clip.previewUrl ?? null,
    fileName: clip.title,
    reason: clip.reason,
    sizeBytes: clip.sizeBytes ?? 0,
    durationSeconds: clip.durationSeconds ?? 0,
    modifiedSecsAgo: secondsAgo(clip.createdAt),
  };
}

function ClipProcessingCard({ clip }: { clip: GalleryClipItem }) {
  const failed = clip.status === "failed";
  const progress = clip.progress ?? (failed ? 0 : 48);
  return (
    <motion.article whileHover={{ y: -4 }} className={`clip-card processing-card ${failed ? "failed" : ""}`}>
      <div className="clip-thumb processing-thumb">
        <div className="processing-skeleton" />
        <div className="processing-loader">
          {failed ? <AlertTriangle className="h-7 w-7 text-[#ff8d7a]" /> : <RefreshCcw className="h-7 w-7 animate-spin text-ember" />}
        </div>
        <span className={`reason reason-${clip.reason}`}>{reasonLabel[clip.reason]}</span>
      </div>
      <div className="p-3">
        <div className="truncate text-sm font-bold text-white">{processingReasonLabel[clip.reason]}</div>
        <div className="mt-1 text-sm text-zinc-400">{getClipStatusLabel(clip.status)}</div>
        <div className="mt-3 h-2 overflow-hidden rounded-full bg-white/10">
          <div
            className={`processing-progress ${clip.progress == null && !failed ? "indeterminate" : ""}`}
            style={{ width: `${Math.max(4, Math.min(100, progress))}%` }}
          />
        </div>
        <div className="mt-2 flex items-center justify-between text-xs text-zinc-500">
          <span>{relativeCreatedAt(clip.createdAt)}</span>
          {!failed && <span>{Math.round(progress)}%</span>}
        </div>
        {failed && (
          <details className="mt-3 rounded-md border border-[#ff8d7a]/25 bg-[#351711]/45 px-3 py-2 text-xs text-[#ffb7aa]">
            <summary className="cursor-pointer font-bold text-[#ffd0c7]">Voir détails</summary>
            <p className="mt-2 break-words">{clip.error ?? "Erreur pendant la création du clip"}</p>
          </details>
        )}
      </div>
    </motion.article>
  );
}

function getClipStatusLabel(status: ClipStatus): string {
  switch (status) {
    case "detected":
      return "Clip détecté...";
    case "recording":
      return "Capture en cours...";
    case "encoding":
      return "Encodage du clip...";
    case "saving":
      return "Sauvegarde...";
    case "ready":
      return "Prêt";
    case "failed":
      return "Erreur";
  }
}

function ClipCard({ clip, onDelete }: { clip: ClipInfo; onDelete: () => void }) {
  const [videoFailed, setVideoFailed] = useState(false);
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const videoSrc = clip.previewUrl ?? convertFileSrc(clip.path);

  useEffect(() => {
    setVideoFailed(false);
  }, [videoSrc]);

  const playPreview = () => {
    const video = videoRef.current;
    if (!video || videoFailed) return;

    video.muted = true;
    video.play().catch((error) => {
      console.info("[FRONTEND] clip preview autoplay failed", error);
    });
  };

  const pausePreview = () => {
    const video = videoRef.current;
    if (!video) return;

    video.pause();
    video.currentTime = 0;
  };

  return (
    <motion.article
      whileHover={{ y: -4 }}
      className="clip-card"
      onMouseEnter={playPreview}
      onMouseLeave={pausePreview}
      onFocus={playPreview}
      onBlur={pausePreview}
    >
      <div className="clip-thumb">
        {!videoFailed ? (
          <video
            ref={videoRef}
            muted
            playsInline
            preload="metadata"
            loop
            onError={() => setVideoFailed(true)}
          >
            <source src={videoSrc} type="video/webm" />
          </video>
        ) : (
          <div className="clip-thumb-fallback">
            <Video className="h-9 w-9 text-ember" />
            <span>Vidéo indisponible</span>
          </div>
        )}
        <div className="clip-overlay" />
        <span className={`reason reason-${clip.reason}`}>{reasonLabel[clip.reason]}</span>
        <button className="icon-button absolute bottom-3 right-3">
          <Play className="h-4 w-4" />
        </button>
      </div>
      <div className="p-3">
        <div className="truncate text-sm font-bold text-white">{clip.fileName}</div>
        <div className="mt-2 flex items-center justify-between text-xs text-zinc-500">
          <span>{formatBytes(clip.sizeBytes)}</span>
          <span>{relativeTime(clip.modifiedSecsAgo)}</span>
        </div>
        <button className="delete-button mt-3" onClick={onDelete}>
          <Trash2 className="h-3.5 w-3.5" />
          Supprimer
        </button>
      </div>
    </motion.article>
  );
}

function Configuration() {
  const { config, setConfig, showToast } = useAppStore();
  const [draft, setDraft] = useState<AppConfig | null>(config);
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  useEffect(() => setDraft(config), [config]);
  if (!draft) return <Empty label="Configuration en chargement" />;

  const updateClip = <K extends keyof AppConfig["clip"]>(key: K, value: AppConfig["clip"][K]) =>
    setDraft({ ...draft, clip: { ...draft.clip, [key]: value } });
  const updateWt = <K extends keyof AppConfig["war_thunder"]>(
    key: K,
    value: AppConfig["war_thunder"][K],
  ) => setDraft({ ...draft, war_thunder: { ...draft.war_thunder, [key]: value } });
  const updateTrigger = <K extends keyof AppConfig["triggers"]>(key: K, value: boolean) =>
    setDraft({ ...draft, triggers: { ...draft.triggers, [key]: value } });

  async function save() {
    if (!draft) return;
    const nextConfig = draft;
    await invoke("save_config", { config: nextConfig });
    setConfig(nextConfig);
    showToast("Configuration sauvegardée");
  }

  async function checkForUpdates() {
    if (checkingUpdate) return;
    setCheckingUpdate(true);
    showToast("Recherche de mise à jour...");
    try {
      const result = await invoke<{ available: boolean }>("check_for_updates");
      if (!result.available) {
        showToast("Aucune mise à jour disponible");
      }
    } catch (error) {
      showToast(`Mise à jour: ${String(error)}`);
    } finally {
      setCheckingUpdate(false);
    }
  }

  return (
    <div className="space-y-5">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-black text-white">Configuration</h1>
          <p className="text-sm text-zinc-500">Capture, qualité, triggers et profil joueur</p>
        </div>
        <div className="flex items-center gap-2">
          <button className="ghost-button w-fit px-5" onClick={() => void checkForUpdates()} disabled={checkingUpdate}>
            <RefreshCcw className={`h-4 w-4 ${checkingUpdate ? "animate-spin" : ""}`} />
            Vérifier les mises à jour
          </button>
          <button className="primary-action w-fit px-5" onClick={() => void save()}>
            <Save className="h-4 w-4" />
            Sauvegarder
          </button>
        </div>
      </div>
      <div className="settings-grid">
        <Field label="player_name">
          <input value={draft.war_thunder.player_name ?? ""} onChange={(e) => updateWt("player_name", e.target.value || null)} />
        </Field>
        <Field label="output_dir">
          <input value={draft.clip.output_dir} onChange={(e) => updateClip("output_dir", e.target.value)} />
        </Field>
        <Field label="seconds">
          <input type="number" value={draft.clip.seconds} onChange={(e) => updateClip("seconds", Number(e.target.value))} />
        </Field>
        <Field label="segment_seconds">
          <input type="number" value={draft.clip.segment_seconds} onChange={(e) => updateClip("segment_seconds", Number(e.target.value))} />
        </Field>
        <Field label="post_event_seconds">
          <input type="number" value={draft.clip.post_event_seconds} onChange={(e) => updateClip("post_event_seconds", Number(e.target.value))} />
        </Field>
        <Field label="quality">
          <select value={draft.clip.quality} onChange={(e) => updateClip("quality", e.target.value as AppConfig["clip"]["quality"])}>
            <option value="low">low</option>
            <option value="medium">medium</option>
            <option value="high">high</option>
          </select>
        </Field>
        <Field label="fps">
          <input type="number" value={draft.clip.fps} onChange={(e) => updateClip("fps", Number(e.target.value))} />
        </Field>
        <Field label="bitrate kbps">
          <input type="number" value={draft.clip.video_bitrate_kbps} onChange={(e) => updateClip("video_bitrate_kbps", Number(e.target.value))} />
        </Field>
      </div>
      <div>
        <h2 className="mb-3 text-lg font-bold text-white">Triggers</h2>
        <div className="trigger-grid">
          <label className="trigger-toggle">
            <input type="checkbox" checked readOnly />
            <span>target_destroyed</span>
          </label>
        </div>
      </div>
    </div>
  );
}

function Diagnostics() {
  const { diagnostics, diagnosticsRunning, setDiagnostics, setDiagnosticsRunning } = useAppStore();
  async function run() {
    setDiagnosticsRunning(true);
    setDiagnostics(await invoke<DoctorReport>("run_diagnostics"));
  }
  const counts = diagnostics?.checks.reduce(
    (acc, check) => ({ ...acc, [check.status]: acc[check.status] + 1 }),
    { ok: 0, warn: 0, error: 0 },
  );
  return (
    <div className="space-y-5">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-black text-white">Diagnostics</h1>
          <p className="text-sm text-zinc-500">{diagnostics?.summary ?? "Analyse des dépendances locales"}</p>
        </div>
        <button className="ghost-button" onClick={() => void run()}>
          <RefreshCcw className={`h-4 w-4 ${diagnosticsRunning ? "animate-spin" : ""}`} />
          Relancer
        </button>
      </div>
      <div className="grid grid-cols-3 gap-4">
        <Metric icon={CheckCircle2} label="OK" value={counts?.ok ?? 0} />
        <Metric icon={AlertTriangle} label="Warn" value={counts?.warn ?? 0} />
        <Metric icon={XCircle} label="Error" value={counts?.error ?? 0} />
      </div>
      <div className="diagnostic-list">
        {!diagnostics ? (
          <Empty label="Diagnostics en chargement" />
        ) : (
          diagnostics.checks.map((check) => <DiagnosticRow key={check.name} check={check} />)
        )}
      </div>
    </div>
  );
}

function DiagnosticRow({ check }: { check: DoctorReport["checks"][number] }) {
  const Icon = check.status === "ok" ? CheckCircle2 : check.status === "warn" ? ShieldAlert : XCircle;
  return (
    <div className="diagnostic-row">
      <Icon className={`h-5 w-5 status-${check.status}`} />
      <div className="min-w-0">
        <div className="font-semibold text-white">{check.name}</div>
        <div className="text-sm text-zinc-400">{check.message}</div>
        {check.hint && <div className="mt-1 text-xs text-zinc-500">{check.hint}</div>}
      </div>
    </div>
  );
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="field">
      <span>{label}</span>
      {children}
    </label>
  );
}

function Empty({ label }: { label: string }) {
  return (
    <div className="empty-state">
      <Cpu className="h-8 w-8 text-zinc-600" />
      <span>{label}</span>
    </div>
  );
}

function formatBytes(bytes: number) {
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} GB`.replace("GB", "MB");
  return `${(bytes / 1024 / 1024 / 1024).toFixed(1)} GB`;
}

function relativeTime(seconds: number) {
  if (seconds < 60) return "à l'instant";
  if (seconds < 3600) return `${Math.round(seconds / 60)} min`;
  if (seconds < 86400) return `${Math.round(seconds / 3600)} h`;
  return `${Math.round(seconds / 86400)} j`;
}

function secondsAgo(value: string) {
  const numeric = Number(value);
  const timestamp = Number.isFinite(numeric) ? numeric : Date.parse(value);
  if (!Number.isFinite(timestamp)) return 0;
  return Math.max(0, Math.floor((Date.now() - timestamp) / 1000));
}

function relativeCreatedAt(value: string) {
  return secondsAgo(value) < 5 ? "Il y a quelques secondes" : relativeTime(secondsAgo(value));
}
