import { create } from "zustand";
import type {
  AppConfig,
  ClipInfo,
  ClipStatusChangedPayload,
  DoctorReport,
  EventEntry,
  ExportProgressPayload,
  GalleryClipItem,
  PendingClipExportDto,
  RuntimeStatus,
} from "./types";

type AppState = {
  activeView: "dashboard" | "clips" | "config" | "diagnostics";
  wtConnected: boolean;
  bufferFilledSecs: number;
  bufferTotalSecs: number;
  diskUsedBytes: number;
  sessionKills: number;
  sessionMultiKills: number;
  clipsSaved: number;
  clips: ClipInfo[];
  processingClips: GalleryClipItem[];
  config: AppConfig | null;
  diagnostics: DoctorReport | null;
  runtimeStatus: RuntimeStatus | null;
  diagnosticsRunning: boolean;
  exportProgress: ExportProgressPayload | null;
  events: EventEntry[];
  toast: string | null;
  setActiveView: (view: AppState["activeView"]) => void;
  setConfig: (config: AppConfig) => void;
  setClips: (clips: ClipInfo[]) => void;
  updateClipStatus: (payload: ClipStatusChangedPayload) => void;
  setPendingExportClips: (clips: PendingClipExportDto[]) => void;
  setExportProgress: (payload: ExportProgressPayload | null) => void;
  setDiagnostics: (report: DoctorReport | null) => void;
  setRuntimeStatus: (status: RuntimeStatus) => void;
  setDiagnosticsRunning: (running: boolean) => void;
  setWtConnected: (connected: boolean) => void;
  setBuffer: (filled: number, total: number) => void;
  setDiskUsedBytes: (bytes: number) => void;
  addClip: (clip: ClipInfo) => void;
  addEvent: (event: Omit<EventEntry, "id" | "at">) => void;
  addEventEntry: (event: EventEntry) => void;
  showToast: (message: string) => void;
};

export const useAppStore = create<AppState>((set) => ({
  activeView: "dashboard",
  wtConnected: false,
  bufferFilledSecs: 0,
  bufferTotalSecs: 20,
  diskUsedBytes: 0,
  sessionKills: 0,
  sessionMultiKills: 0,
  clipsSaved: 0,
  clips: [],
  processingClips: [],
  config: null,
  diagnostics: null,
  runtimeStatus: null,
  diagnosticsRunning: false,
  exportProgress: null,
  events: [],
  toast: null,
  setActiveView: (activeView) => set({ activeView }),
  setConfig: (config) => set({ config, bufferTotalSecs: config.clip.seconds }),
  setClips: (clips) =>
    set({
      clips,
      clipsSaved: clips.length,
      diskUsedBytes: clips.reduce((sum, clip) => sum + clip.sizeBytes, 0),
    }),
  setDiagnostics: (diagnostics) => set({ diagnostics, diagnosticsRunning: false }),
  setRuntimeStatus: (runtimeStatus) =>
    set({
      runtimeStatus,
      bufferFilledSecs: runtimeStatus.bufferFilledSecs,
      bufferTotalSecs: Math.max(1, runtimeStatus.bufferTotalSecs),
      wtConnected: runtimeStatus.wtConnected,
    }),
  setDiagnosticsRunning: (diagnosticsRunning) => set({ diagnosticsRunning }),
  setWtConnected: (wtConnected) => set({ wtConnected }),
  setBuffer: (bufferFilledSecs, bufferTotalSecs) =>
    set({ bufferFilledSecs, bufferTotalSecs: Math.max(1, bufferTotalSecs) }),
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
  setPendingExportClips: (clips) =>
    set((state) => {
      const items = clips.map((clip): GalleryClipItem => ({
        id: clip.id,
        status: clip.status,
        reason: clip.reason,
        createdAt: clip.createdAt,
        title: clip.title,
        progress: clip.progress ?? undefined,
        error: clip.error ?? undefined,
        exportableAt: clip.exportableAt,
        isExportable: clip.isExportable,
        canExport: clip.canExport,
        retryable: clip.retryable,
      }));
      const itemIds = new Set(items.map((item) => item.id));
      const others = state.processingClips.filter((clip) => !itemIds.has(clip.id));
      return { processingClips: [...items, ...others] };
    }),
  setExportProgress: (exportProgress) => set({ exportProgress }),
  addEvent: (event) =>
    set((state) => ({
      sessionKills:
        event.kind === "target-destroyed" || event.kind === "multi-kill"
          ? state.sessionKills + 1
          : state.sessionKills,
      sessionMultiKills:
        event.kind === "multi-kill" ? state.sessionMultiKills + 1 : state.sessionMultiKills,
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
          event.kind === "target-destroyed" || event.kind === "multi-kill"
            ? state.sessionKills + 1
            : state.sessionKills,
        sessionMultiKills:
          event.kind === "multi-kill" ? state.sessionMultiKills + 1 : state.sessionMultiKills,
        events: [event, ...state.events].slice(0, 24),
      };
    }),
  showToast: (toast) => {
    set({ toast });
    window.setTimeout(() => set({ toast: null }), 3200);
  },
}));
