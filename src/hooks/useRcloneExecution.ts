import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { loadAppSettings } from "../settings";

export interface RcloneSettings {
  baseUrl: string;
  username: string;
  selectedSubdir: string;
  targetSubdir: string;
}

export type RcloneActionType = 'put' | 'get' | 'put-dry' | 'get-dry' | 'put-checksum' | 'get-checksum' | 'ls' | 'lsd' | 'check' | 'sync';

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
  };
}

export function useRcloneExecution(
  onLog: (text: string | ((prev: string) => string)) => void,
  isRunning: boolean,
  setIsRunning: (running: boolean) => void
) {
  const runningRef = useRef(false);
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
    log((prev) => prev + "\nCanceling active operation...\n");
    try {
      await invoke("cancel_webdav_action");
    } catch (e) {
      log((prev) => prev + `Failed to cancel operation: ${e}\n`);
    }
  };

  const runRclone = async (action: RcloneActionType, opts?: { onDone?: (code: number | null) => void }) => {
    if (runningRef.current || isRunning) return;
    runningRef.current = true;
    setIsRunning(true);
    log("Loading configuration...\n");

    try {
      const settings = await loadSettings();
      if (!settings.baseUrl) {
        log("Error: WebDAV URL must be configured.\n");
        finishRun(opts?.onDone, null);
        return;
      }

      if (!settings.selectedSubdir) {
        log("Error: Please select a subdirectory first.\n");
        finishRun(opts?.onDone, null);
        return;
      }

      log(
        (prev) =>
          prev +
          `Selected Subdirectory: ${settings.selectedSubdir}\n` +
          `Running native WebDAV ${action}...\n\n`
      );

      // Listen for backend WebDAV logs
      const unlisten = await listen<string>("webdav-log", (event) => {
        log(event.payload);
      });
      unlistenRef.current = unlisten;

      await invoke("run_webdav_action", {
        action,
        baseUrl: settings.baseUrl,
        subdir: settings.selectedSubdir,
        targetSubdir: settings.targetSubdir || undefined,
        username: settings.username || undefined,
      });

      log(`\nWebDAV operation finished successfully.\n`);
      finishRun(opts?.onDone, 0);
    } catch (e: any) {
      log(`\nWebDAV Error: ${e?.message || e}\n`);
      finishRun(opts?.onDone, 1);
    }
  };

  return { runRclone, cancelCommand };
}
