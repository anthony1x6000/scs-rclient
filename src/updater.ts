import { useState, useEffect, useCallback, useRef } from "react";
import { check, type Update, type DownloadEvent } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { getVersion } from "@tauri-apps/api/app";

export function isTauri(): boolean {
  if (typeof window === "undefined") return false;
  const win = window as unknown as { __TAURI_INTERNALS__?: unknown; __TAURI__?: unknown };
  return Boolean(win.__TAURI_INTERNALS__ || win.__TAURI__);
}

export interface UpdateInfo {
  version: string;
  currentVersion: string;
  date?: string | undefined;
  body?: string | undefined;
}

export interface UpdateProgress {
  downloaded: number;
  total?: number | undefined;
  percentage?: number | undefined;
}

export type UpdateState =
  | { status: "idle" }
  | { status: "checking" }
  | { status: "uptodate"; version: string }
  | { status: "available"; update: Update; info: UpdateInfo }
  | { status: "downloading"; progress: UpdateProgress; info: UpdateInfo }
  | { status: "installing"; info: UpdateInfo }
  | { status: "restarting" }
  | { status: "error"; message: string };

/**
 * Retrieves current application version, falling back to 0.1.0 in dev/mock environments.
 */
export async function getAppVersion(): Promise<string> {
  if (!isTauri()) return "0.1.0";
  try {
    return await getVersion();
  } catch (e) {
    console.warn("Unable to fetch app version via Tauri API:", e);
    return "0.1.0";
  }
}

/**
 * Queries GitHub Releases endpoint configured in tauri.conf.json for application updates.
 */
export async function checkForUpdates(): Promise<Update | null> {
  if (!isTauri()) {
    console.info("Tauri updater: not running in desktop runtime environment");
    return null;
  }
  return await check();
}

/**
 * Downloads update package from release assets, verifies signature, installs, and relaunches.
 */
export async function installUpdate(
  update: Update,
  onProgress?: (progress: UpdateProgress) => void,
  timeoutMs = 600000 // 10 minutes timeout
): Promise<void> {
  let downloaded = 0;
  let total: number | undefined;

  const downloadPromise = update.downloadAndInstall((event: DownloadEvent) => {
    if (event.event === "Started") {
      total = event.data.contentLength;
      onProgress?.({ downloaded, total, percentage: total ? 0 : undefined });
    } else if (event.event === "Progress") {
      downloaded += event.data.chunkLength;
      const percentage = total && total > 0 ? Math.min(100, Math.round((downloaded / total) * 100)) : undefined;
      onProgress?.({ downloaded, total, percentage });
    } else if (event.event === "Finished") {
      onProgress?.({ downloaded, total, percentage: 100 });
    }
  });

  const timeoutPromise = new Promise<never>((_, reject) =>
    setTimeout(() => reject(new Error("Update download timed out. Check your connection and try again.")), timeoutMs)
  );

  await Promise.race([downloadPromise, timeoutPromise]);

  try {
    await relaunch();
  } catch (err) {
    console.error("Application relaunch failed after update installation:", err);
    throw err;
  }
}

/**
 * React hook managing the full update lifecycle (checking, notifications, downloading, and installing).
 */
export function useAppUpdater() {
  const [state, setState] = useState<UpdateState>({ status: "idle" });
  const [currentVersion, setCurrentVersion] = useState<string>("0.1.0");
  const activeUpdateRef = useRef<Update | null>(null);
  const resetTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    getAppVersion().then(setCurrentVersion).catch(() => {});
    return () => {
      if (resetTimerRef.current) clearTimeout(resetTimerRef.current);
    };
  }, []);

  const clearResetTimer = () => {
    if (resetTimerRef.current) {
      clearTimeout(resetTimerRef.current);
      resetTimerRef.current = null;
    }
  };

  const scheduleReset = (delayMs = 3000) => {
    clearResetTimer();
    resetTimerRef.current = setTimeout(() => {
      setState((prev) => (prev.status === "uptodate" || prev.status === "error" ? { status: "idle" } : prev));
    }, delayMs);
  };

  const checkUpdates = useCallback(async (interactive = true) => {
    clearResetTimer();
    setState({ status: "checking" });

    try {
      const update = await checkForUpdates();
      if (update) {
        activeUpdateRef.current = update;
        const info: UpdateInfo = {
          version: update.version,
          currentVersion: update.currentVersion,
          date: update.date,
          body: update.body,
        };
        setState({ status: "available", update, info });
      } else {
        activeUpdateRef.current = null;
        if (interactive) {
          const ver = currentVersion || (await getAppVersion());
          setState({ status: "uptodate", version: ver });
          scheduleReset(3000);
        } else {
          setState({ status: "idle" });
        }
      }
    } catch (e: unknown) {
      activeUpdateRef.current = null;
      console.warn("Error checking for updates from GitHub releases:", e);
      if (interactive) {
        let msg = "Failed to check for updates";
        if (e instanceof Error) {
          const raw = e.message.toLowerCase();
          if (raw.includes("network") || raw.includes("fetch") || raw.includes("connection") || raw.includes("dns")) {
            msg = "Network error checking for updates";
          } else if (raw.includes("timeout")) {
            msg = "Update check timed out";
          } else if (raw.includes("signature") || raw.includes("key")) {
            msg = "Update signature verification failed";
          }
        }
        setState({ status: "error", message: msg });
        scheduleReset(3000);
      } else {
        setState({ status: "idle" });
      }
    }
  }, [currentVersion]);

  const install = useCallback(async () => {
    if (state.status !== "available") return;
    const targetUpdate = state.update;
    const targetInfo = state.info;

    setState({
      status: "downloading",
      info: targetInfo,
      progress: { downloaded: 0 },
    });

    try {
      await installUpdate(targetUpdate, (progress) => {
        setState((prev) =>
          prev.status === "downloading"
            ? { status: "downloading", info: targetInfo, progress }
            : prev
        );
      });
      setState({ status: "restarting" });
    } catch (e: unknown) {
      activeUpdateRef.current = null;
      console.error("Failed to install update:", e);
      let msg = "Update installation failed";
      if (e instanceof Error) {
        const raw = e.message.toLowerCase();
        if (raw.includes("timeout")) {
          msg = "Download timed out";
        } else if (raw.includes("signature") || raw.includes("verify")) {
          msg = "Signature verification failed";
        } else if (raw.includes("network") || raw.includes("connection")) {
          msg = "Network connection lost during download";
        }
      }
      setState({ status: "error", message: msg });
      scheduleReset(4000);
    }
  }, [state]);

  const dismiss = useCallback(() => {
    clearResetTimer();
    setState({ status: "idle" });
  }, []);

  return {
    state,
    currentVersion,
    checkUpdates,
    install,
    dismiss,
  };
}
