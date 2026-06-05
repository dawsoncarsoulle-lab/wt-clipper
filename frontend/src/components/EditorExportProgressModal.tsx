import { invoke } from "@tauri-apps/api/core";
import { AnimatePresence, motion } from "framer-motion";
import { AlertTriangle, CheckCircle2, FileVideo, FolderOpen, XCircle } from "lucide-react";
import type { EditedClipResult, EditorExportProgressPayload } from "../types";

type EditorExportProgressModalProps = {
  progress: EditorExportProgressPayload | null;
  result: EditedClipResult | null;
  onClose: () => void;
};

export function EditorExportProgressModal({
  progress,
  result,
  onClose,
}: EditorExportProgressModalProps) {
  if (!progress) {
    return null;
  }

  const finished = !progress.active || progress.step === "done" || progress.step === "failed";
  const failed = progress.step === "failed";
  const outputPath = result?.outputPath ?? progress.outputPath;
  const replacedOriginal = result?.replacedOriginal === true;
  const backupPath = result?.backupPath;

  function close() {
    if (!finished) {
      return;
    }
    onClose();
  }

  return (
    <AnimatePresence>
      <motion.div
        animate={{ opacity: 1 }}
        className="modal-backdrop"
        exit={{ opacity: 0 }}
        initial={{ opacity: 0 }}
      >
        <motion.section
          animate={{ opacity: 1, scale: 1, y: 0 }}
          className="export-modal editor-progress-modal"
          exit={{ opacity: 0, scale: 0.98, y: 14 }}
          initial={{ opacity: 0, scale: 0.98, y: 14 }}
        >
          <div className="flex items-start justify-between gap-5">
            <div>
              <div className="text-xs uppercase tracking-wide text-zinc-500">
                {finished ? "Export éditeur" : "Export en cours"}
              </div>
              <h2 className="mt-1 text-xl font-black text-white">
                {failed
                  ? "Export impossible"
                  : finished && replacedOriginal
                    ? "Clip original remplacé"
                    : finished
                      ? "Export terminé"
                      : "Création du clip"}
              </h2>
            </div>
            <button className="icon-button" disabled={!finished} onClick={close} title="Fermer">
              <XCircle className="h-4 w-4" />
            </button>
          </div>

          <div className={`editor-progress-state ${failed ? "failed" : ""}`}>
            {failed ? (
              <AlertTriangle className="h-5 w-5 text-[#ff8d7a]" />
            ) : finished ? (
              <CheckCircle2 className="h-5 w-5 text-emerald-300" />
            ) : (
              <FileVideo className="h-5 w-5 text-ember" />
            )}
            <div className="min-w-0">
              <div className="text-sm font-bold text-white">{progress.message}</div>
              {finished && replacedOriginal && (
                <div className="mt-1 text-sm text-zinc-300">
                  Une sauvegarde a été créée dans Backups/.
                </div>
              )}
              <div className="mt-1 text-xs uppercase text-zinc-500">{progress.step}</div>
            </div>
          </div>

          <div className="mt-4 h-3 overflow-hidden rounded-full bg-white/10">
            <div className="processing-progress" style={{ width: `${progress.progress}%` }} />
          </div>
          <div className="mt-2 text-right text-xs text-zinc-500">
            {Math.round(progress.progress)}%
          </div>

          {progress.error && (
            <p className="mt-4 break-words rounded-md border border-[#ff8d7a]/25 bg-[#351711]/45 p-3 text-sm text-[#ffb7aa]">
              {progress.error}
            </p>
          )}

          {outputPath && (
            <p className="mt-4 break-words text-xs text-zinc-500">{outputPath}</p>
          )}

          {finished && replacedOriginal && backupPath && (
            <p className="mt-2 break-words text-xs text-[#ffd0c3]">
              Sauvegarde : {backupPath}
            </p>
          )}

          {finished && (
            <div className="mt-5 flex justify-end gap-2">
              {outputPath && (
                <>
                  {!replacedOriginal && (
                    <button
                      className="ghost-button"
                      onClick={() => void invoke("open_parent_folder", { path: outputPath })}
                    >
                      <FolderOpen className="h-4 w-4" />
                      Ouvrir le dossier
                    </button>
                  )}
                  <button
                    className="ghost-button"
                    onClick={() => void invoke("open_path", { path: outputPath })}
                  >
                    <FileVideo className="h-4 w-4" />
                    {replacedOriginal ? "Ouvrir clip" : "Ouvrir le fichier"}
                  </button>
                </>
              )}
              {replacedOriginal && backupPath && (
                <button
                  className="ghost-button"
                  onClick={() => void invoke("open_path", { path: backupPath })}
                >
                  <FolderOpen className="h-4 w-4" />
                  Ouvrir sauvegarde
                </button>
              )}
              <button className="primary-action w-fit px-5" onClick={close}>
                Fermer
              </button>
            </div>
          )}
        </motion.section>
      </motion.div>
    </AnimatePresence>
  );
}
