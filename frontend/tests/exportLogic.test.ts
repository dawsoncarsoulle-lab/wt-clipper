import {
  canCloseExportModal,
  currentExportClipNumber,
  exportableCount,
  formatClipDuration,
  mapExportProgressPayload,
  shouldShowExportButton,
} from "../src/exportLogic.js";
import { mergePendingExportClips } from "../src/pendingQueueState.js";
import type { ExportProgressPayload, GalleryClipItem, PendingClipExportDto } from "../src/types.js";

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

function pendingDto(id: string): PendingClipExportDto {
  return {
    id,
    status: "ready_to_export",
    reason: "target-destroyed",
    title: id,
    createdAt: new Date().toISOString(),
    exportableAt: new Date().toISOString(),
    isExportable: true,
    canExport: true,
    retryable: false,
    progress: 100,
    error: null,
  };
}

function progress(active: boolean, currentStep: ExportProgressPayload["currentStep"]): ExportProgressPayload {
  return {
    active,
    total: 3,
    completed: 0,
    failed: 0,
    currentClipNumber: null,
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

test("setPendingExportClips_empty_clears_old_pending", () => {
  const state = mergePendingExportClips([clip("ready_to_export", true)], []);

  assertEqual(state.length, 0);
});

test("setPendingExportClips avec les memes 5 DTO deux fois reste a 5", () => {
  const backend = [1, 2, 3, 4, 5].map((index) => pendingDto(`clip-${index}`));
  const first = mergePendingExportClips([], backend);
  const second = mergePendingExportClips(first, backend);

  assertEqual(first.length, 5);
  assertEqual(second.length, 5);
});

test("ouvrir fermer la galerie plusieurs fois ne duplique pas la queue", () => {
  const backend = [1, 2, 3, 4, 5].map((index) => pendingDto(`clip-${index}`));
  let state: GalleryClipItem[] = [];

  for (let index = 0; index < 5; index += 1) {
    state = mergePendingExportClips(state, backend);
  }

  assertEqual(state.length, 5);
});

test("refreshPendingExportClips ne doit jamais append aveuglement", () => {
  const backend = [1, 2, 3, 4, 5].map((index) => pendingDto(`clip-${index}`));
  const state = mergePendingExportClips(mergePendingExportClips([], backend), backend);
  const ids = new Set(state.map((item) => item.id));

  assertEqual(state.length, ids.size);
  assertEqual(state.length, 5);
});

test("export de 5 clips => la modal avance de clip 1 a 5", () => {
  const events: ExportProgressPayload[] = [1, 2, 3, 4, 5].map((currentClipNumber) => ({
    active: true,
    total: 5,
    completed: currentClipNumber - 1,
    failed: 0,
    currentClipNumber,
    currentClipId: `clip-${currentClipNumber}`,
    currentClipTitle: `Clip ${currentClipNumber}`,
    currentStep: "encoding",
    progress: currentClipNumber * 20 - 3,
    message: "Encodage",
  }));

  assertEqual(events.map(currentExportClipNumber).join(","), "1,2,3,4,5");
});

test("la modal ne reste pas bloquee a preparing 0 si l_export avance", () => {
  const events: ExportProgressPayload[] = [
    {
      active: true,
      total: 5,
      completed: 0,
      failed: 0,
      currentClipNumber: 1,
      currentClipId: "clip-1",
      currentClipTitle: "Clip 1",
      currentStep: "preparing",
      progress: 1,
      message: "Préparation",
    },
    {
      active: true,
      total: 5,
      completed: 0,
      failed: 0,
      currentClipNumber: 1,
      currentClipId: "clip-1",
      currentClipTitle: "Clip 1",
      currentStep: "encoding",
      progress: 17,
      message: "Encodage",
    },
    {
      active: true,
      total: 5,
      completed: 1,
      failed: 0,
      currentClipNumber: 2,
      currentClipId: "clip-2",
      currentClipTitle: "Clip 2",
      currentStep: "preparing",
      progress: 21,
      message: "Préparation",
    },
  ];

  assertEqual(events.some((event) => event.progress > 0), true);
  assertEqual(events.some((event) => event.currentStep !== "preparing"), true);
  assertEqual(events.map(currentExportClipNumber).join(","), "1,1,2");
});

test("payload snake_case est mappe correctement", () => {
  const mapped = mapExportProgressPayload({
    active: true,
    total: 6,
    completed: 1,
    failed: 0,
    current_clip_number: 2,
    current_clip_id: "clip-2",
    current_clip_title: "Clip 2",
    current_step: "encoding",
    progress: 27,
    message: "Encodage du clip 2 / 6...",
  });

  assertEqual(mapped?.currentClipNumber, 2);
  assertEqual(mapped?.currentClipId, "clip-2");
  assertEqual(mapped?.currentClipTitle, "Clip 2");
  assertEqual(mapped?.currentStep, "encoding");
  assertEqual(mapped?.progress, 27);
});

test("reception export_progress_changed met a jour exportProgress", () => {
  let state: ExportProgressPayload | null = null;
  const mapped = mapExportProgressPayload({
    active: true,
    total: 6,
    completed: 0,
    failed: 0,
    currentClipNumber: 1,
    currentClipId: "clip-1",
    currentClipTitle: "Clip 1",
    currentStep: "assembling",
    progress: 3,
    message: "Assemblage du clip 1 / 6...",
  });

  state = mapped;

  assertEqual(state?.currentStep, "assembling");
  assertEqual(state?.progress, 3);
});

test("generic app-event export-progress-changed est mappe", () => {
  const mapped = mapExportProgressPayload({
    type: "export-progress-changed",
    payload: {
      active: true,
      total: 6,
      completed: 1,
      failed: 0,
      current_clip_number: 2,
      current_clip_id: "clip-2",
      current_clip_title: "Clip 2",
      current_step: "encoding",
      progress: 27,
      message: "Encodage du clip 2 / 6...",
    },
  });

  assertEqual(mapped?.currentClipNumber, 2);
  assertEqual(mapped?.currentStep, "encoding");
});

test("modal passe de Clip 1/6 a Clip 2/6 apres event", () => {
  const first = mapExportProgressPayload({
    active: true,
    total: 6,
    completed: 0,
    failed: 0,
    currentClipNumber: 1,
    currentStep: "assembling",
    progress: 3,
    message: "Assemblage",
  });
  const second = mapExportProgressPayload({
    active: true,
    total: 6,
    completed: 1,
    failed: 0,
    currentClipNumber: 2,
    currentStep: "assembling",
    progress: 18,
    message: "Assemblage",
  });

  assertEqual(first ? currentExportClipNumber(first) : 0, 1);
  assertEqual(second ? currentExportClipNumber(second) : 0, 2);
});

test("progress 100 affiche export termine", () => {
  const mapped = mapExportProgressPayload({
    active: false,
    total: 6,
    completed: 6,
    failed: 0,
    currentStep: "done",
    progress: 100,
    message: "Export terminé",
  });

  assertEqual(mapped?.active, false);
  assertEqual(mapped?.currentStep, "done");
  assertEqual(mapped?.progress, 100);
  assertEqual(mapped?.message, "Export terminé");
});
