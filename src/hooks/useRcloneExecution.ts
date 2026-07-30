import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { load } from "@tauri-apps/plugin-store";
import { resolveRemoteUrl, resolveLocalPath, obscurePassword } from "../utils/rclone";

export interface RcloneSettings {
  baseUrl: string;
  username: string;
  selectedSubdir: string;
}

export type RcloneActionType = 'put' | 'get' | 'put-dry' | 'get-dry' | 'ls' | 'lsd' | 'check' | 'sync';

/**
 * Loads rclone target WebDAV settings from the store.
 */
export async function loadSettings(): Promise<RcloneSettings> {
  const store = await load("settings.json", { autoSave: true, defaults: {} });
  const savedBase = await store.get<{ value: string }>("webdav_url");
  const savedUser = await store.get<{ value: string }>("saved_username");
  const savedSub = await store.get<{ value: string }>("selected_subdirectory");
  return {
    baseUrl: savedBase?.value || "",
    username: savedUser?.value || "",
    selectedSubdir: savedSub?.value || "",
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
    case 'get':
      args = ["copy", ":webdav:", localPath];
      break;
    case 'get-dry':
      args = ["copy", ":webdav:", localPath, "--dry-run"];
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
  const [mountDir, setMountDir] = useState<string>("");

  useEffect(() => {
    invoke<string>("get_mount_dir")
      .then(setMountDir)
      .catch(console.error);
  }, []);

  const cancelCommand = async () => {
    onLog((prev) => prev + "Note: Operation is running in an independent terminal window. Please manage or close the terminal window directly.\n");
    setIsRunning(false);
  };

  const runRclone = async (action: RcloneActionType) => {
    if (isRunning) return;
    setIsRunning(true);
    onLog("Loading configuration...\n");

    try {
      const settings = await loadSettings();
      if (!settings.baseUrl) {
        onLog("Error: WebDAV URL must be configured.\n");
        setIsRunning(false);
        return;
      }

      if (!settings.selectedSubdir) {
        onLog("Error: Please select a subdirectory first.\n");
        setIsRunning(false);
        return;
      }

      let password = "";
      if (settings.username) {
        // Retrieve password securely from the OS keyring
        onLog("Retrieving password from secure keyring...\n");
        try {
          password = await invoke<string>("get_credentials", { username: settings.username });
        } catch (e) {
          onLog((prev) => prev + `Note: Could not retrieve credentials from keyring: ${e}. Proceeding without password.\n`);
        }
      } else {
        onLog("No username configured. Proceeding without credentials.\n");
      }

      // Obscure password because rclone expects obscured passwords for on-the-fly config
      let obscuredPassword = "";
      if (password) {
        onLog("Obscuring password...\n");
        try {
          obscuredPassword = await obscurePassword(password);
        } catch (e) {
          onLog((prev) => prev + `Warning: Failed to obscure password: ${e}. Proceeding without password.\n`);
        }
      }

      const remoteUrl = resolveRemoteUrl(settings.baseUrl, settings.selectedSubdir);
      const localPath = resolveLocalPath(mountDir, settings.selectedSubdir);
      const args = buildActionArgs(action, localPath, remoteUrl, settings.username || undefined);

      onLog(
        (prev) =>
          prev +
          `Target Subdirectory: ${settings.selectedSubdir}\n` +
          `Local Path: ${localPath}\n` +
          `Remote URL: ${remoteUrl}\n` +
          `Command: rclone ${args.join(" ")}\n\n` +
          `Launching independent terminal instance...\n`
      );

      const resultMsg = await invoke<string>("run_rclone_in_terminal", {
        args,
        envPass: obscuredPassword || null,
      });

      onLog((prev) => prev + `${resultMsg}\nIndependent terminal instance launched successfully.\n\n`);
    } catch (e) {
      onLog((prev) => prev + `System Error: ${e}\n`);
    } finally {
      setIsRunning(false);
    }
  };

  return { runRclone, cancelCommand };
}

