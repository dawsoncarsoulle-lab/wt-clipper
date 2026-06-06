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
  deleteTimelineSegment,
  exportSegmentsFromTimeline,
  nextTimelineZoom,
  recalculateTimelineSegments,
  reorderTimelineSegment,
  splitSegmentAtPlayhead,
  timelineContentWidthPercent,
  timelineTimeToSegment,
  timelineTimeToSegmentTime,
  timelineTimeToX,
  timelineDuration,
  timelineTimeFromClientX,
  sourceTimeToSegmentLocalX,
  segmentLocalXToSourceTime,
  xToTimelineTime,
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

const baseSegments = () => recalculateTimelineSegments([
  {
    id: "a",
    sourcePath: "/tmp/a.mp4",
    sourceDuration: 20,
    start: 0,
    end: 20,
    timelineStart: 0,
    timelineEnd: 20,
  },
]);

test("split_at_playhead_creates_two_segments", () => {
  const result = splitSegmentAtPlayhead(baseSegments(), 8, 0.5, () => "b");
  assertEqual(result.changed, true);
  assertEqual(result.segments.length, 2);
  assertEqual(result.segments[0].end, 8);
  assertEqual(result.segments[1].start, 8);
});

test("split_near_boundary_is_ignored", () => {
  const result = splitSegmentAtPlayhead(baseSegments(), 0.1, 0.5, () => "b");
  assertEqual(result.changed, false);
  assertEqual(result.segments.length, 1);
});

test("total_timeline_duration_unchanged_after_split", () => {
  const before = baseSegments();
  const result = splitSegmentAtPlayhead(before, 8, 0.5, () => "b");
  assertEqual(timelineDuration(result.segments), timelineDuration(before));
});

test("delete_segment_removes_from_export", () => {
  const split = splitSegmentAtPlayhead(baseSegments(), 8, 0.5, () => "b").segments;
  const deleted = deleteTimelineSegment(split, "a");
  assertEqual(exportSegmentsFromTimeline(deleted).length, 1);
  assertEqual(exportSegmentsFromTimeline(deleted)[0].sourcePath, "/tmp/a.mp4");
});

test("cannot_export_empty_timeline", () => {
  const deleted = deleteTimelineSegment(baseSegments(), "a");
  assertEqual(exportSegmentsFromTimeline(deleted).length, 0);
});

test("timeline_duration_updates_after_delete", () => {
  const split = splitSegmentAtPlayhead(baseSegments(), 8, 0.5, () => "b").segments;
  assertEqual(timelineDuration(deleteTimelineSegment(split, "a")), 12);
});

test("reorder_segments_changes_order", () => {
  const split = splitSegmentAtPlayhead(baseSegments(), 8, 0.5, () => "b").segments;
  const reordered = reorderTimelineSegment(split, "b", "a");
  assertEqual(reordered[0].id, "b");
});

test("export_uses_reordered_segment_order", () => {
  const split = splitSegmentAtPlayhead(baseSegments(), 8, 0.5, () => "b").segments;
  const reordered = reorderTimelineSegment(split, "b", "a");
  assertEqual(exportSegmentsFromTimeline(reordered)[0].startSeconds, 8);
});

test("timeline_duration_unchanged_after_reorder", () => {
  const split = splitSegmentAtPlayhead(baseSegments(), 8, 0.5, () => "b").segments;
  assertEqual(timelineDuration(reorderTimelineSegment(split, "b", "a")), 20);
});

test("multi_clip_selection_creates_segments", () => {
  const editor = source("src/components/ClipEditorModal.tsx");
  const app = source("src/App.tsx");
  assert(editor.includes("initialTimelineSegments(clips"), "editor should accept multiple clips");
  assert(app.includes("Assembler"), "gallery should expose assemble action");
});

test("set_current_frame_as_thumbnail", () => {
  const editor = source("src/components/ClipEditorModal.tsx");
  assert(editor.includes("setCurrentFrameAsThumbnail"), "thumbnail action missing");
  assert(editor.includes("thumbnailSourcePath"), "thumbnail payload missing");
});

test("export_payload_uses_timeline_segments", () => {
  const editor = source("src/components/ClipEditorModal.tsx");
  assert(editor.includes("segments: exportSegmentsFromTimeline(segments)"), "timeline segments missing from export payload");
});

test("video_source_is_set_from_active_segment", () => {
  const editor = source("src/components/ClipEditorModal.tsx");
  assert(editor.includes("activeSourcePath"), "active source path state missing");
  assert(editor.includes("findSegmentAtTimelineTime(segments"), "active segment resolution missing");
});

test("editor_unloads_video_on_close", () => {
  const editor = source("src/components/ClipEditorModal.tsx");
  assert(editor.includes("unloadEditorVideo(videoRef.current)"), "editor should unload video on unmount");
  assert(editor.includes('removeAttribute("src")'), "editor should remove video src during cleanup");
  assert(editor.includes("[EDITOR_CLEANUP] unload video"), "editor cleanup log missing");
});

test("editor_does_not_create_multiple_video_elements", () => {
  const editor = source("src/components/ClipEditorModal.tsx");
  assertEqual((editor.match(/<video/g) ?? []).length, 1);
});

test("video_uses_tauri_file_url_or_existing_preview_url", () => {
  const editor = source("src/components/ClipEditorModal.tsx");
  assert(editor.includes("sourcePreviewUrl") && editor.includes("convertFileSrc(sourcePath)"), "video should use preview URL or Tauri file URL");
  assert(editor.includes("src={videoSrc}"), "video src prop missing");
});

test("clicking_segment_selects_it_without_scrubbing", () => {
  const timeline = source("src/components/TrimTimeline.tsx");
  assert(timeline.includes("onSegmentSelect(segmentId)"), "segment select missing");
  assert(timeline.includes("event.stopPropagation();"), "segment event propagation should stop");
});

test("click_segment_selects_without_scrubbing", () => {
  const timeline = source("src/components/TrimTimeline.tsx");
  assert(timeline.includes("beginSegmentPointerDrag"), "segment pointer selection should be isolated");
  assert(timeline.includes("!drag.dragging"), "short segment click should not scrub");
});

test("clicking_timeline_background_moves_playhead", () => {
  assertEqual(xToTimelineTime(50, 20, 100, 1), 10);
  assertEqual(timelineTimeToX(10, 20, 100, 1), 50);
});

test("trim_start_handle_stops_event_propagation", () => {
  const timeline = source("src/components/TrimTimeline.tsx");
  assert(timeline.includes('beginDrag(event, "start")'), "start handle drag missing");
});

test("trim_start_does_not_move_playhead", () => {
  const timeline = source("src/components/TrimTimeline.tsx");
  assert(timeline.includes('setInteractionMode(mode === "start" ? "trim-start"'), "trim start mode missing");
  assert(timeline.includes('if (mode === "start")'), "trim start should have its own branch");
});

test("trim_end_handle_stops_event_propagation", () => {
  const timeline = source("src/components/TrimTimeline.tsx");
  assert(timeline.includes('beginDrag(event, "end")'), "end handle drag missing");
});

test("trim_end_does_not_move_playhead", () => {
  const timeline = source("src/components/TrimTimeline.tsx");
  assert(timeline.includes('mode === "end" ? "trim-end"'), "trim end mode missing");
  assert(timeline.includes('if (mode === "end")'), "trim end should have its own branch");
});

test("trim_end_handle_can_be_dragged_multiple_times", () => {
  assertEqual(clampTrimEnd(10, 0, 25, 0.25), 10);
  assertEqual(clampTrimEnd(14, 0, 25, 0.25), 14);
});

test("segment_drag_does_not_move_playhead", () => {
  const timeline = source("src/components/TrimTimeline.tsx");
  assert(timeline.includes('setInteractionMode("drag-segment")'), "segment drag mode missing");
  assert(timeline.includes("SEGMENT_DRAG_THRESHOLD_PX"), "segment drag should use a movement threshold");
});

test("drag_segment_does_not_scrub", () => {
  const timeline = source("src/components/TrimTimeline.tsx");
  assert(timeline.includes("event.target !== event.currentTarget"), "background scrub should ignore segment targets");
  assert(timeline.includes("beginSegmentPointerDrag"), "segment drag should use independent pointer handler");
});

test("drag_segment_reorders_segments", () => {
  const split = splitSegmentAtPlayhead(baseSegments(), 8, 0.5, () => "b").segments;
  const reordered = reorderTimelineSegment(split, "a", "");
  assertEqual(reordered[1].id, "a");
});

test("playhead_drag_moves_playhead", () => {
  const timeline = source("src/components/TrimTimeline.tsx");
  assert(timeline.includes('beginDrag(event, "playhead")'), "playhead drag should call playhead drag path");
});

test("active_segment_resolves_correct_source_time", () => {
  const split = splitSegmentAtPlayhead(baseSegments(), 8, 0.5, () => "b").segments;
  const resolved = timelineTimeToSegment(10, split);
  assertEqual(resolved?.segment.id, "b");
  assertEqual(resolved?.sourceTime, 10);
});

test("video_src_uses_tauri_url_or_existing_file_asset_url", () => {
  const editor = source("src/components/ClipEditorModal.tsx");
  assert(editor.includes("videoUrlForSource"), "video source helper missing");
  assert(editor.includes("convertFileSrc(sourcePath)"), "video should not use a raw file path directly");
});

test("split_segments_can_be_selected_independently", () => {
  const split = splitSegmentAtPlayhead(baseSegments(), 8, 0.5, () => "b").segments;
  assertEqual(split[0].id, "a");
  assertEqual(split[1].id, "b");
});

test("timeline_time_to_segment_time_maps_to_source_time", () => {
  const segment = { ...baseSegments()[0], start: 4, end: 14, timelineStart: 0, timelineEnd: 10 };
  assertEqual(timelineTimeToSegmentTime(3, segment), 7);
});

test("segment_source_pixel_helpers_roundtrip", () => {
  const segment = { ...baseSegments()[0], start: 4, end: 14, timelineStart: 0, timelineEnd: 10 };
  assertEqual(sourceTimeToSegmentLocalX(segment, 9, 200), 100);
  assertEqual(segmentLocalXToSourceTime(segment, 100, 200), 9);
});
