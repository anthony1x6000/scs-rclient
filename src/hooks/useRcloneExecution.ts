import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { loadAppSettings, excludedFoldersFor, type ExcludedFoldersByDir } from "../settings";
import { BULK_MIN_FOLDERS, formatBulkSummary, runBulkSequential } from "../bulk";

export interface RcloneSettings {
  baseUrl: string;
  username: string;
  selectedSubdir: string;
  targetSubdir: string;
  scanConcurrency: number;
  excludedFoldersByDir: ExcludedFoldersByDir;
  excludedFoldersLegacy: string[];
}

export type RcloneActionType = 'put' | 'get' | 'get-backup' | 'get-backup-dated' | 'put-dry' | 'get-dry' | 'put-checksum' | 'get-checksum' | 'ls' | 'lsd' | 'check' | 'sync';

/** Keep at most ~500KB / ~5000 lines of log; drop oldest with a notice. */
const MAX_LOG_CHARS = 500_000;
const MAX_LOG_LINES = 5000;

function appendCapped(prev: string, addition: string): string {
  let next = prev + addition;
  if (next.length > MAX_LOG_CHARS || next.split("\n").length > MAX_LOG_LINES + 1) {
    const lines = next.split("\n");
    const kept = lines.slice(-MAX_LOG_LINES).join("\n");
    const notice = "… [older output truncated] …\n";
    next = notice + kept.slice(-MAX_LOG_CHARS);
  }
  return next;
}

/**
 * Loads target WebDAV settings from the single settings module.
 */
export async function loadSettings(): Promise<RcloneSettings> {
  const s = await loadAppSettings();
  return {
    baseUrl: s.baseUrl,
    username: s.username,
    selectedSubdir: s.selectedSubdir,
    targetSubdir: s.targetSubdir,
    scanConcurrency: s.scanConcurrency,
    excludedFoldersByDir: s.excludedFoldersByDir,
    excludedFoldersLegacy: s.excludedFoldersLegacy,
  };
}

export function useRcloneExecution(
  onLog: (text: string | ((prev: string) => string)) => void,
  isRunning: boolean,
  setIsRunning: (running: boolean) => void
) {
  const runningRef = useRef(false);
  const cancelRequestedRef = useRef(false);
  const unlistenRef = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    return () => {
      if (unlistenRef.current) {
        unlistenRef.current();
        unlistenRef.current = null;
      }
      runningRef.current = false;
    };
  }, []);

  const log = (addition: string | ((prev: string) => string)) => {
    if (typeof addition === "string") {
      onLog((prev) => appendCapped(prev, addition));
    } else {
      onLog((prev) => {
        const next = addition(prev);
        if (next.length > prev.length && next.startsWith(prev)) {
          return appendCapped("", next);
        }
        return next.length > MAX_LOG_CHARS ? next.slice(-MAX_LOG_CHARS) : next;
      });
    }
  };

  const finishRun = (onDone?: (code: number | null) => void, code: number | null = null) => {
    if (unlistenRef.current) {
      unlistenRef.current();
      unlistenRef.current = null;
    }
    runningRef.current = false;
    setIsRunning(false);
    onDone?.(code);
  };

  const cancelCommand = async () => {
    cancelRequestedRef.current = true;
    log((prev) => prev + "\nCanceling active operation...\n");
    try {
      await invoke("cancel_webdav_action");
    } catch (e) {
      log((prev) => prev + `Failed to cancel operation: ${e}\n`);
    }
  };

  const invokeWebdavAction = async (action: RcloneActionType, settings: RcloneSettings, subdir: string) => {
    // Both backup actions are GETs whose local side is redirected under <mount>/backups;
    // the dated variant nests one level deeper under today's date.
    const isDatedBackup = action === "get-backup-dated";
    const isBackup = action === "get-backup" || isDatedBackup;
    // Exclusions are scoped to the directory this operation runs on, so one
    // folder's patterns never leak into another folder's bulk run.
    await invoke("run_webdav_action", {
      action: isBackup ? "get" : action,
      baseUrl: settings.baseUrl,
      subdir,
      targetSubdir: settings.targetSubdir || undefined,
      username: settings.username || undefined,
      concurrency: settings.scanConcurrency,
      backup: isBackup || undefined,
      backupDated: isDatedBackup || undefined,
      excludeFolders: excludedFoldersFor(settings.excludedFoldersByDir, subdir, settings.excludedFoldersLegacy),
    });
  };

  /**
   * Runs `action` on one folder (today's behavior) or, when `opts.folders`
   * holds a bulk selection, on each folder sequentially — logging a header
   * per folder, continuing past failures, and finishing with a summary.
   */
  const runRclone = async (action: RcloneActionType, opts?: { folders?: string[]; onDone?: (code: number | null) => void }) => {
    if (runningRef.current || isRunning) return;
    runningRef.current = true;
    cancelRequestedRef.current = false;
    setIsRunning(true);
    log("Loading configuration...\n");

    try {
      // Settings, including the exclusion lists, are loaded once per run;
      // edits made mid-run apply on the next run.
      const settings = await loadSettings();
      if (!settings.baseUrl) {
        log("Error: WebDAV URL must be configured.\n");
        finishRun(opts?.onDone, null);
        return;
      }

      const bulkFolders = (opts?.folders ?? []).filter(Boolean);
      const singleSubdir = bulkFolders[0] ?? settings.selectedSubdir;
      if (bulkFolders.length === 0 && !singleSubdir) {
        log("Error: Please select a subdirectory first.\n");
        finishRun(opts?.onDone, null);
        return;
      }

      // Listen for backend WebDAV logs (one listener spans the whole bulk loop).
      const unlisten = await listen<string>("webdav-log", (event) => {
        log(event.payload);
      });
      unlistenRef.current = unlisten;

      if (bulkFolders.length < BULK_MIN_FOLDERS) {
        log(
          (prev) =>
            prev +
            `Selected Subdirectory: ${singleSubdir}\n` +
            `Running native WebDAV ${action}...\n\n`
        );
        await invokeWebdavAction(action, settings, singleSubdir);
        log(`\nWebDAV operation finished successfully.\n`);
        finishRun(opts?.onDone, 0);
        return;
      }

      log(
        (prev) =>
          prev +
          `Bulk folders (${bulkFolders.length}): ${bulkFolders.join(", ")}\n` +
          `Running native WebDAV ${action} sequentially...\n\n`
      );
      const outcome = await runBulkSequential(
        bulkFolders,
        async (folder, index) => {
          log(`\n=== [${index + 1}/${bulkFolders.length}] ${folder} ===\n`);
          try {
            await invokeWebdavAction(action, settings, folder);
          } catch (e: any) {
            log(`\nWebDAV Error on ${folder}: ${e?.message || e}\n`);
            throw e;
          }
        },
        () => cancelRequestedRef.current,
      );
      log(`\n${formatBulkSummary(outcome)}\n`);
      finishRun(opts?.onDone, outcome.failed.length > 0 ? 1 : 0);
    } catch (e: any) {
      log(`\nWebDAV Error: ${e?.message || e}\n`);
      finishRun(opts?.onDone, 1);
    }
  };

  return { runRclone, cancelCommand };
}
