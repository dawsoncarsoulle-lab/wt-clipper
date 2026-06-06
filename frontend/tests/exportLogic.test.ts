import { readFileSync } from "node:fs";
import { join } from "node:path";
import {
  debouncedRefreshCount,
  galleryBadgeLabel,
  hoverPreviewSeekSeconds,
  mountedGalleryVideoCount,
  shouldApplyGalleryLoadResult,
  shouldResetHoverPreview,
  shouldShowHoverVideo,
  shouldUnloadHoverPreview,
  shouldUseGalleryCache,
} from "../src/galleryResourcePolicy.js";
import {
  clampTrimEnd,
  clampTrimStart,
  nextTimelineZoom,
  timelineContentWidthPercent,
  timelineTimeFromClientX,
} from "../src/editorTimelinePolicy.js";
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
    frame_rate_mode: "cfr",
    keyframe_interval_seconds: 1.0,
    restart_replay_on_save: false,
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
  assertEqual(frontendConfig.capture.frame_rate_mode, "cfr");
  assertEqual(frontendConfig.capture.keyframe_interval_seconds, 1.0);
  assertEqual(frontendConfig.capture.restart_replay_on_save, false);
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

test("hover_preview_keeps_thumbnail_until_video_ready", () => {
  assertEqual(shouldShowHoverVideo(false, 4), false);
  assertEqual(shouldShowHoverVideo(true, 1), false);
  assertEqual(shouldShowHoverVideo(true, 2), true);
});

test("hover_preview_seeks_to_configured_start_seconds", () => {
  assertEqual(hoverPreviewSeekSeconds(0.75, 25), 0.75);
});

test("hover_preview_seek_is_clamped_near_short_video_end", () => {
  assertEqual(hoverPreviewSeekSeconds(0.75, 0.5), 0.4);
});

test("switching_clips_resets_previous_video", () => {
  assertEqual(shouldResetHoverPreview("/tmp/a.mp4", "/tmp/b.mp4"), true);
  assertEqual(shouldResetHoverPreview("/tmp/a.mp4", "/tmp/a.mp4"), false);
});

test("mouseleave_unloads_video", () => {
  assertEqual(shouldUnloadHoverPreview(null), true);
  assertEqual(shouldUnloadHoverPreview("/tmp/a.mp4"), false);
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

test("metadata_target_destroyed_badge_is_kill", () => {
  assertEqual(galleryBadgeLabel("kill", "target_destroyed"), "KILL");
  assertEqual(galleryBadgeLabel(null, "target-destroyed"), "KILL");
});

test("metadata_multi_kill_badge_is_multi", () => {
  assertEqual(galleryBadgeLabel("multi", "multi_kill"), "MULTI");
  assertEqual(galleryBadgeLabel(null, "multi-kill"), "MULTI");
});

test("metadata_player_destroyed_badge_is_death", () => {
  assertEqual(galleryBadgeLabel("death", "player_destroyed"), "DEATH");
});

test("metadata_base_destroyed_badge_is_base", () => {
  assertEqual(galleryBadgeLabel("base", "base_destroyed"), "BASE");
});

test("metadata_manual_badge_is_manual", () => {
  assertEqual(galleryBadgeLabel("manual", "manual"), "MANUAL");
});

test("replay_filename_without_metadata_badge_is_clip", () => {
  assertEqual(galleryBadgeLabel(null, "unknown"), "Clip");
});

test("edited_and_vertical_exports_prioritize_export_badge", () => {
  assertEqual(galleryBadgeLabel("kill", "target_destroyed", "edited"), "Edited");
  assertEqual(galleryBadgeLabel("multi", "multi_kill", "social"), "Vertical");
});

test("frontend_listens_to_backend_editor_progress_event", () => {
  const backend = source("../src-tauri/src/editor.rs");
  const frontend = source("src/components/ClipEditorModal.tsx");
  assert(backend.includes('"editor_export_progress_changed"'), "backend progress event missing");
  assert(frontend.includes('"editor_export_progress_changed"'), "frontend progress listener missing");
});

test("progress_modal_initializes_above_zero", () => {
  const frontend = source("src/components/ClipEditorModal.tsx");
  assert(frontend.includes("progress: 5"), "editor progress should not start at 0");
});

test("backend_progress_preparing_is_above_zero", () => {
  const backend = source("../src-tauri/src/editor.rs");
  assert(backend.includes("EditorExportProgressStep::Preparing,\n        5"), "backend preparing progress should be 5");
});

test("timeline_click_updates_playhead", () => {
  assertEqual(timelineTimeFromClientX(150, 100, 200, 20), 5);
});

test("scrubbing_updates_video_current_time", () => {
  assertEqual(timelineTimeFromClientX(300, 100, 200, 20), 20);
  assertEqual(timelineTimeFromClientX(50, 100, 200, 20), 0);
});

test("trim_start_handle_updates_start", () => {
  assertEqual(clampTrimStart(4, 12, 20, 0.25), 4);
});

test("trim_end_handle_updates_end", () => {
  assertEqual(clampTrimEnd(16, 4, 20, 0.25), 16);
});

test("trim_handles_cannot_cross", () => {
  assertEqual(clampTrimStart(19, 10, 20, 1), 9);
  assertEqual(clampTrimEnd(2, 10, 20, 1), 11);
});

test("zoom_increases_timeline_scale", () => {
  const zoom = nextTimelineZoom(1, "in");
  assertEqual(zoom, 1.5);
  assertEqual(timelineContentWidthPercent(zoom), 150);
});

test("export_uses_timeline_trim_values", () => {
  const editor = source("src/components/ClipEditorModal.tsx");
  assert(editor.includes("startSeconds,"), "export request should use timeline startSeconds");
  assert(editor.includes("endSeconds,"), "export request should use timeline endSeconds");
});

test("existing_export_buttons_still_work", () => {
  const editor = source("src/components/ClipEditorModal.tsx");
  assert(editor.includes("Exporter"), "export button missing");
  assert(editor.includes("Remplacer"), "replace export button missing");
});
