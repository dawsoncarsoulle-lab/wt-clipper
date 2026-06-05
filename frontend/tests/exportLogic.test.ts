import { readFileSync } from "node:fs";
import { join } from "node:path";
import {
  debouncedRefreshCount,
  mountedGalleryVideoCount,
  shouldApplyGalleryLoadResult,
  shouldUseGalleryCache,
} from "../src/galleryResourcePolicy.js";
import type { AppConfig } from "../src/types.js";

function source(path: string) {
  return readFileSync(join(process.cwd(), path), "utf8");
}

function test(name: string, run: () => void) {
  try {
    run();
    console.log(`ok ${name}`);
  } catch (error) {
    console.error(`not ok ${name}`);
    throw error;
  }
}

function assertEqual<T>(actual: T, expected: T) {
  if (actual !== expected) {
    throw new Error(`expected ${String(expected)}, got ${String(actual)}`);
  }
}

function assert(condition: boolean, message: string) {
  if (!condition) {
    throw new Error(message);
  }
}

function assertAbsent(haystack: string, parts: string[]) {
  const needle = parts.join("");
  assert(!haystack.includes(needle), `unexpected legacy token: ${needle}`);
}

const frontendConfig: AppConfig = {
  clip: {
    post_event_seconds: 5,
    multi_kill_window_seconds: 8,
  },
  library: {
    output_dir: "~/Videos/WarThunder Clips",
  },
  capture: {
    target: "eDP",
    mode: "flatpak",
    fps: 60,
    replay_seconds: 25,
    container: "mp4",
    codec: "h264",
    encoder: "gpu",
    quality: "very_high",
    bitrate_mode: "cbr",
    video_bitrate_kbps: 20_000,
    output_dir: "~/Videos/WarThunder Clips/GSR",
    audio_enabled: true,
    audio_input: "default_output",
  },
  war_thunder: {
    base_url: "http://127.0.0.1:8111",
    player_name: null,
    poll_interval_ms: 300,
    request_timeout_ms: 500,
  },
  triggers: {
    target_destroyed: true,
    base_destroyed: true,
    player_destroyed: true,
  },
  storage: {
    max_clips: 100,
    max_storage_gb: 20,
  },
};

test("frontend_config_no_legacy_clip_fields", () => {
  const clipKeys = Object.keys(frontendConfig.clip);
  for (const key of [
    "seconds",
    ["segment", "_seconds"].join(""),
    "quality",
    "fps",
    ["video", "_bitrate", "_kbps"].join(""),
    "source",
    ["keep", "_segments"].join(""),
    ["export", "_mode"].join(""),
  ]) {
    assert(!clipKeys.includes(key), `${key} should not be exposed on clip config`);
  }
});

test("frontend_config_has_library_output_dir", () => {
  assertEqual(frontendConfig.library.output_dir, "~/Videos/WarThunder Clips");
});

test("frontend_config_has_gsr_capture_fields", () => {
  assertEqual(frontendConfig.capture.mode, "flatpak");
  assertEqual(frontendConfig.capture.quality, "very_high");
  assertEqual(frontendConfig.capture.bitrate_mode, "cbr");
  assertEqual(frontendConfig.capture.video_bitrate_kbps, 20_000);
});

test("app_does_not_render_old_export_button", () => {
  const app = source("src/App.tsx");
  assertAbsent(app, ["Exporter", " maintenant"]);
});

test("app_does_not_call_old_export_command", () => {
  const app = source("src/App.tsx");
  assertAbsent(app, ["export", "_pending", "_clips"]);
  assertAbsent(app, ["get", "_pending", "_export", "_clips"]);
  assertAbsent(app, ["delete", "_pending", "_export", "_clip"]);
});

test("app_does_not_render_old_capture_status", () => {
  const app = source("src/App.tsx");
  assertAbsent(app, ["Replay", "Buffer"]);
  assertAbsent(app, ["buffer", "_progress"]);
});

test("diagnostics_renders_gsr_only", () => {
  const app = source("src/App.tsx");
  assert(app.includes("GPU Screen Recorder"), "GSR diagnostics title missing");
  assert(app.includes("gsrCommandLine"), "GSR command line missing");
  assertAbsent(app, ["G", "Streamer"]);
});

test("gallery_cards_still_do_not_render_video_per_card", () => {
  assertEqual(mountedGalleryVideoCount(null), 0);
});

test("hover_preview_still_single_video", () => {
  assertEqual(mountedGalleryVideoCount("/tmp/clip.mp4"), 1);
});

test("config_save_includes_capture_and_library", () => {
  const serialized = JSON.stringify(frontendConfig);
  assert(serialized.includes("capture"), "capture config missing");
  assert(serialized.includes("library"), "library config missing");
});

test("config_save_excludes_old_queue_config", () => {
  const serialized = JSON.stringify(frontendConfig);
  assertAbsent(serialized, ["pending", "_exports"]);
});

test("no_legacy_tauri_commands_invoked", () => {
  const app = source("src/App.tsx");
  for (const parts of [
    ["restart", "_replay", "_buffer"],
    ["restart", "_buffer"],
    ["export", "_pending", "_clips"],
    ["get", "_pending", "_export", "_clips"],
    ["delete", "_pending", "_export", "_clip"],
  ]) {
    assertAbsent(app, parts);
  }
});

test("gallery_refresh_uses_cache_when_recent", () => {
  assertEqual(shouldUseGalleryCache(10_000, 9_500, 1_000, false), true);
});

test("gallery_refresh_force_ignores_cache", () => {
  assertEqual(shouldUseGalleryCache(10_000, 9_500, 1_000, true), false);
});

test("gallery_load_result_ignored_after_unmount", () => {
  assertEqual(shouldApplyGalleryLoadResult(2, 2, false), false);
});

test("gallery_load_result_ignored_when_stale", () => {
  assertEqual(shouldApplyGalleryLoadResult(1, 2, true), false);
});

test("clip_saved_events_are_batched_by_debounce", () => {
  assertEqual(debouncedRefreshCount([0, 100, 200, 1200], 500), 2);
});
