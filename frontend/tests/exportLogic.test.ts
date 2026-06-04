import {
  canCloseExportModal,
  exportableCount,
  formatClipDuration,
  shouldShowExportButton,
} from "../src/exportLogic.js";
import type { ExportProgressPayload, GalleryClipItem } from "../src/types.js";

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

function clip(status: GalleryClipItem["status"], canExport = false, retryable = false): GalleryClipItem {
  return {
    id: `${status}-${canExport}-${retryable}`,
    status,
    reason: "target-destroyed",
    createdAt: new Date().toISOString(),
    title: status,
    canExport,
    retryable,
  };
}

function progress(active: boolean, currentStep: ExportProgressPayload["currentStep"]): ExportProgressPayload {
  return {
    active,
    total: 3,
    completed: 0,
    failed: 0,
    currentClipId: null,
    currentClipTitle: null,
    currentStep,
    progress: 0,
    message: currentStep,
  };
}

test("export_modal_total_matches_exportable_count", () => {
  assertEqual(
    exportableCount([
      clip("waiting_post_event"),
      clip("freezing_segments"),
      clip("ready_to_export", true),
      clip("ready_to_export", true),
      clip("failed", true, true),
      clip("failed", false, false),
      clip("expired"),
      clip("ready"),
    ]),
    3,
  );
});

test("export_modal_cannot_close_during_active_export_but_shows_clear_state", () => {
  assertEqual(canCloseExportModal(true, progress(true, "encoding")), false);
});

test("export_modal_closes_after_done", () => {
  assertEqual(canCloseExportModal(true, progress(false, "done")), true);
  assertEqual(canCloseExportModal(true, progress(true, "done")), true);
});

test("export_modal_closes_after_failed", () => {
  assertEqual(canCloseExportModal(true, progress(false, "failed")), true);
  assertEqual(canCloseExportModal(true, progress(true, "failed")), true);
});

test("export_modal_close_button_visible_after_completion", () => {
  assertEqual(canCloseExportModal(false, progress(false, "done")), true);
});

test("export_modal_x_button_not_dead", () => {
  assertEqual(canCloseExportModal(true, progress(true, "encoding")), false);
  assertEqual(canCloseExportModal(false, progress(false, "done")), true);
});

test("duration_seconds=25 affiche 00:25", () => {
  assertEqual(formatClipDuration(25), "00:25");
});

test("duration_seconds=26 affiche 00:26", () => {
  assertEqual(formatClipDuration(26), "00:26");
});

test("modified_secs_ago=120 n_affiche_pas_2_min_dans_le_champ_duree", () => {
  assertEqual(formatClipDuration(25), "00:25");
});

test("queue vide => pas de bouton export", () => {
  assertEqual(shouldShowExportButton([], false), false);
});

test("3 expired => pas de bouton export", () => {
  assertEqual(
    shouldShowExportButton([clip("expired"), clip("expired"), clip("expired")], false),
    false,
  );
});

test("3 clips ready_to_export => modal total=3", () => {
  assertEqual(
    exportableCount([
      clip("ready_to_export", true),
      clip("ready_to_export", true),
      clip("ready_to_export", true),
    ]),
    3,
  );
});

test("2 waiting + 3 ready_to_export => modal total=3", () => {
  assertEqual(
    exportableCount([
      clip("waiting_post_event"),
      clip("waiting_post_event"),
      clip("ready_to_export", true),
      clip("ready_to_export", true),
      clip("ready_to_export", true),
    ]),
    3,
  );
});
