import { useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { createRcloneCommand, resolveRemoteUrl, resolveLocalPath, obscurePassword, ensureRcloneDetected } from "../utils/rclone";
import { loadAppSettings } from "../settings";
import { Child, TerminatedPayload } from "@tauri-apps/plugin-shell";

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
 * Loads rclone target WebDAV settings from the single settings module.
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

/**
 * Builds the arguments list for launching the rclone subprocess.
 */
export function buildActionArgs(
  action: RcloneActionType,
  localPath: string,
  remoteUrl: string,
  username?: string
): string[] {
  let args: string[] = [];
  switch (action) {
    case 'put':
      args = ["copy", localPath, ":webdav:"];
      break;
    case 'put-dry':
      args = ["copy", localPath, ":webdav:", "--dry-run"];
      break;
    case 'put-checksum':
      args = ["copy", localPath, ":webdav:", "--checksum"];
      break;
    case 'get':
      args = ["copy", ":webdav:", localPath];
      break;
    case 'get-dry':
      args = ["copy", ":webdav:", localPath, "--dry-run"];
      break;
    case 'get-checksum':
      args = ["copy", ":webdav:", localPath, "--checksum"];
      break;
    case 'ls':
      args = ["ls", ":webdav:"];
      break;
    case 'lsd':
      args = ["lsd", ":webdav:"];
      break;
    case 'check':
      args = ["check", localPath, ":webdav:"];
      break;
    case 'sync':
      args = ["sync", localPath, ":webdav:"];
      break;
  }
  args.push("-v");
  args.push(`--webdav-url=${remoteUrl}`);
  if (username) {
    args.push(`--webdav-user=${username}`);
  }
  return args;
}

export function useRcloneExecution(
  onLog: (text: string | ((prev: string) => string)) => void,
  isRunning: boolean,
  setIsRunning: (running: boolean) => void
) {
  const activeChildRef = useRef<Child | null>(null);
  // Synchronous guard: refs update immediately, unlike state (fixes double-spawn race).
  const runningRef = useRef(false);

  useEffect(() => {
    return () => {
      if (activeChildRef.current) {
        activeChildRef.current.kill().catch(console.error);
        activeChildRef.current = null;
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
        // `addition` callbacks in this file always append; cap defensively.
        if (next.length > prev.length && next.startsWith(prev)) {
          return appendCapped("", next);
        }
        return next.length > MAX_LOG_CHARS ? next.slice(-MAX_LOG_CHARS) : next;
      });
    }
  };

  const finishRun = (onDone?: (code: number | null) => void, code: number | null = null) => {
    activeChildRef.current = null;
    runningRef.current = false;
    setIsRunning(false);
    onDone?.(code);
  };

  const cancelCommand = async () => {
    if (activeChildRef.current) {
      log((prev) => prev + "\nCanceling active operation...\n");
      const child = activeChildRef.current;
      activeChildRef.current = null;
      try {
        await child.kill();
        log((prev) => prev + "Operation canceled by user.\n");
      } catch (e) {
        log((prev) => prev + `Failed to cancel operation: ${e}\n`);
      }
      runningRef.current = false;
      setIsRunning(false);
    }
  };

  const runRclone = async (action: RcloneActionType, opts?: { onDone?: (code: number | null) => void }) => {
    if (runningRef.current || isRunning) return;
    runningRef.current = true;
    setIsRunning(true);
    log("Loading configuration...\n");

    try {
      // Await sidecar detection on the run path (no stale-default race).
      try {
        await ensureRcloneDetected();
      } catch (e) {
        log(`Error: rclone is unavailable: ${e}\n`);
        finishRun(opts?.onDone, null);
        return;
      }

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

      let password = "";
      if (settings.username) {
        // Retrieve password securely from the OS keyring — fail closed.
        log("Retrieving password from secure keyring...\n");
        try {
          password = await invoke<string>("get_credentials", { username: settings.username });
        } catch (e) {
          log(`Error: Could not retrieve credentials from keyring: ${e}. Aborting (will not run unauthenticated).\n`);
          finishRun(opts?.onDone, null);
          return;
        }
      } else {
        log("No username configured. Proceeding without credentials.\n");
      }

      // Obscure password because rclone expects obscured passwords for on-the-fly config.
      // Fail closed: never silently proceed unauthenticated after an obscure failure.
      let obscuredPassword = "";
      if (password) {
        log("Obscuring password...\n");
        try {
          obscuredPassword = await obscurePassword(password);
        } catch (e) {
          log(`Error: Failed to obscure password: ${e}. Aborting (will not run unauthenticated).\n`);
          finishRun(opts?.onDone, null);
          return;
        }
      }

      const resolvedMountDir = await invoke<string>("get_mount_dir", {
        targetSubdir: settings.targetSubdir || undefined,
      });

      let remoteUrl: string;
      let localPath: string;
      try {
        remoteUrl = resolveRemoteUrl(settings.baseUrl, settings.selectedSubdir);
        localPath = resolveLocalPath(resolvedMountDir, settings.selectedSubdir);
      } catch (e) {
        log(`Error: Invalid path configuration: ${e}\n`);
        finishRun(opts?.onDone, null);
        return;
      }

      log(
        (prev) =>
          prev +
          `Selected Subdirectory: ${settings.selectedSubdir}\n` +
          `Local Path: ${localPath}\n` +
          `Remote URL: ${remoteUrl}\n\n` +
          `Running command...\n`
      );

      const args = buildActionArgs(action, localPath, remoteUrl, settings.username || undefined);
      // Sanitized echo: action + paths only, never credentials.
      log(`rclone ${action}: local ↔ remote sync operation (see paths above)\n\n`);

      const env = obscuredPassword ? { RCLONE_WEBDAV_PASS: obscuredPassword } : undefined;
      const rcloneCmd = createRcloneCommand(args, env);

      const dataHandler = (data: string) => {
        log(data);
      };

      rcloneCmd.stdout.on("data", dataHandler);
      rcloneCmd.stderr.on("data", dataHandler);

      rcloneCmd.on("close", (data: TerminatedPayload) => {
        log(`\nCommand finished with exit code ${data.code}.\n`);
        finishRun(opts?.onDone, data.code);
      });

      rcloneCmd.on("error", (error: string) => {
        log(`\nCommand error: ${error}\n`);
        finishRun(opts?.onDone, null);
      });

      const child = await rcloneCmd.spawn();
      activeChildRef.current = child;
    } catch (e) {
      log(`System Error: ${e}\n`);
      finishRun(opts?.onDone, null);
    }
  };

  return { runRclone, cancelCommand };
}
