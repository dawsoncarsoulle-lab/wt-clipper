import { create } from "zustand";
import type { AppConfig, ClipInfo, DoctorReport, EventEntry } from "./types";

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
  config: AppConfig | null;
  diagnostics: DoctorReport | null;
  diagnosticsRunning: boolean;
  events: EventEntry[];
  toast: string | null;
  setActiveView: (view: AppState["activeView"]) => void;
  setConfig: (config: AppConfig) => void;
  setClips: (clips: ClipInfo[]) => void;
  setDiagnostics: (report: DoctorReport | null) => void;
  setDiagnosticsRunning: (running: boolean) => void;
  setWtConnected: (connected: boolean) => void;
  setBuffer: (filled: number, total: number) => void;
  setDiskUsedBytes: (bytes: number) => void;
  addClip: (clip: ClipInfo) => void;
  addEvent: (event: Omit<EventEntry, "id" | "at">) => void;
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
  config: null,
  diagnostics: null,
  diagnosticsRunning: false,
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
  setDiagnosticsRunning: (diagnosticsRunning) => set({ diagnosticsRunning }),
  setWtConnected: (wtConnected) => set({ wtConnected }),
  setBuffer: (bufferFilledSecs, bufferTotalSecs) =>
    set({ bufferFilledSecs, bufferTotalSecs: Math.max(1, bufferTotalSecs) }),
  setDiskUsedBytes: (diskUsedBytes) => set({ diskUsedBytes }),
  addClip: (clip) =>
    set((state) => ({
      clips: [clip, ...state.clips.filter((item) => item.path !== clip.path)],
      clipsSaved: state.clipsSaved + 1,
      diskUsedBytes: state.diskUsedBytes + clip.sizeBytes,
    })),
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
  showToast: (toast) => {
    set({ toast });
    window.setTimeout(() => set({ toast: null }), 3200);
  },
}));
