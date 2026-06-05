import { create } from "zustand";
import type {
  AppConfig,
  ClipInfo,
  ClipStatusChangedPayload,
  DoctorReport,
  EventEntry,
  GalleryClipItem,
  RuntimeStatus,
} from "./types";

type AppState = {
  activeView: "dashboard" | "clips" | "config" | "diagnostics";
  wtConnected: boolean;
  diskUsedBytes: number;
  sessionKills: number;
  sessionMultiKills: number;
  clipsSaved: number;
  clips: ClipInfo[];
  processingClips: GalleryClipItem[];
  galleryRefreshCount: number;
  galleryLastRefreshMs: number | null;
  galleryRenderedClipCount: number;
  galleryMountedVideoCount: number;
  frontendListenerCount: number;
  config: AppConfig | null;
  diagnostics: DoctorReport | null;
  runtimeStatus: RuntimeStatus | null;
  diagnosticsRunning: boolean;
  events: EventEntry[];
  toast: string | null;
  setActiveView: (view: AppState["activeView"]) => void;
  setConfig: (config: AppConfig) => void;
  setClips: (clips: ClipInfo[]) => void;
  recordGalleryRefresh: (durationMs: number) => void;
  setGalleryRenderedClipCount: (count: number) => void;
  setGalleryMountedVideoCount: (count: number) => void;
  setFrontendListenerCount: (count: number) => void;
  updateClipStatus: (payload: ClipStatusChangedPayload) => void;
  setDiagnostics: (report: DoctorReport | null) => void;
  setRuntimeStatus: (status: RuntimeStatus) => void;
  setDiagnosticsRunning: (running: boolean) => void;
  setWtConnected: (connected: boolean) => void;
  setDiskUsedBytes: (bytes: number) => void;
  addClip: (clip: ClipInfo) => void;
  addEvent: (event: Omit<EventEntry, "id" | "at">) => void;
  addEventEntry: (event: EventEntry) => void;
  showToast: (message: string) => void;
};

let toastTimeout: number | null = null;

export const useAppStore = create<AppState>((set) => ({
  activeView: "dashboard",
  wtConnected: false,
  diskUsedBytes: 0,
  sessionKills: 0,
  sessionMultiKills: 0,
  clipsSaved: 0,
  clips: [],
  processingClips: [],
  galleryRefreshCount: 0,
  galleryLastRefreshMs: null,
  galleryRenderedClipCount: 0,
  galleryMountedVideoCount: 0,
  frontendListenerCount: 0,
  config: null,
  diagnostics: null,
  runtimeStatus: null,
  diagnosticsRunning: false,
  events: [],
  toast: null,
  setActiveView: (activeView) => set({ activeView }),
  setConfig: (config) => set({ config }),
  setClips: (clips) =>
    set({
      clips,
      clipsSaved: clips.length,
      diskUsedBytes: clips.reduce((sum, clip) => sum + clip.sizeBytes, 0),
    }),
  recordGalleryRefresh: (galleryLastRefreshMs) =>
    set((state) => ({
      galleryRefreshCount: state.galleryRefreshCount + 1,
      galleryLastRefreshMs,
    })),
  setGalleryRenderedClipCount: (galleryRenderedClipCount) => set({ galleryRenderedClipCount }),
  setGalleryMountedVideoCount: (galleryMountedVideoCount) => set({ galleryMountedVideoCount }),
  setFrontendListenerCount: (frontendListenerCount) => set({ frontendListenerCount }),
  setDiagnostics: (diagnostics) => set({ diagnostics, diagnosticsRunning: false }),
  setRuntimeStatus: (runtimeStatus) =>
    set({
      runtimeStatus,
      wtConnected: runtimeStatus.wtConnected,
      clipsSaved: runtimeStatus.clipsSaved,
    }),
  setDiagnosticsRunning: (diagnosticsRunning) => set({ diagnosticsRunning }),
  setWtConnected: (wtConnected) => set({ wtConnected }),
  setDiskUsedBytes: (diskUsedBytes) => set({ diskUsedBytes }),
  addClip: (clip) =>
    set((state) => {
      const existed = state.clips.some((item) => item.path === clip.path);
      const clips = [clip, ...state.clips.filter((item) => item.path !== clip.path)];
      return {
        clips,
        processingClips: state.processingClips.filter((item) => item.filePath !== clip.path),
        clipsSaved: clips.length,
        diskUsedBytes: existed
          ? clips.reduce((sum, item) => sum + item.sizeBytes, 0)
          : state.diskUsedBytes + clip.sizeBytes,
      };
    }),
  updateClipStatus: (payload) =>
    set((state) => {
      if (payload.status === "ready") {
        return {
          processingClips: state.processingClips.filter((clip) => clip.id !== payload.id),
        };
      }
      const item: GalleryClipItem = {
        id: payload.id,
        status: payload.status,
        reason: payload.reason,
        createdAt: payload.createdAt,
        title: payload.title,
        filePath: payload.filePath ?? undefined,
        thumbnailPath: payload.thumbnailPath ?? undefined,
        durationSeconds: payload.durationSeconds ?? undefined,
        sizeBytes: payload.sizeBytes ?? undefined,
        progress: payload.progress ?? undefined,
        error: payload.error ?? undefined,
      };
      const others = state.processingClips.filter((clip) => clip.id !== payload.id);
      return { processingClips: [item, ...others] };
    }),
  addEvent: (event) =>
    set((state) => ({
      sessionKills:
        event.kind === "target_destroyed" || event.kind === "multi_kill"
          ? state.sessionKills + 1
          : state.sessionKills,
      sessionMultiKills:
        event.kind === "multi_kill" ? state.sessionMultiKills + 1 : state.sessionMultiKills,
      events: [
        {
          ...event,
          id: `${Date.now()}-${Math.random().toString(16).slice(2)}`,
          at: new Date().toLocaleTimeString("fr-FR", {
            hour: "2-digit",
            minute: "2-digit",
            second: "2-digit",
          }),
        },
        ...state.events,
      ].slice(0, 24),
    })),
  addEventEntry: (event) =>
    set((state) => {
      if (state.events.some((item) => item.id === event.id)) {
        return state;
      }
      return {
        sessionKills:
          event.kind === "target_destroyed" || event.kind === "multi_kill"
            ? state.sessionKills + 1
            : state.sessionKills,
        sessionMultiKills:
          event.kind === "multi_kill" ? state.sessionMultiKills + 1 : state.sessionMultiKills,
        events: [event, ...state.events].slice(0, 24),
      };
    }),
  showToast: (toast) => {
    if (toastTimeout != null) {
      window.clearTimeout(toastTimeout);
    }
    set({ toast });
    toastTimeout = window.setTimeout(() => {
      toastTimeout = null;
      set({ toast: null });
    }, 3200);
  },
}));
