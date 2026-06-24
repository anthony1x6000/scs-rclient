import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { load } from "@tauri-apps/plugin-store";
import { createRcloneCommand } from "../utils/rclone";

interface RcloneActionsProps {
  onLog: (text: string | ((prev: string) => string)) => void;
  isRunning: boolean;
  setIsRunning: (running: boolean) => void;
}

export function RcloneActions({ onLog, isRunning, setIsRunning }: RcloneActionsProps) {
  const [mountDir, setMountDir] = useState<string>("");
  const activeChildRef = useRef<any>(null);

  useEffect(() => {
    invoke<string>("get_mount_dir")
      .then(setMountDir)
      .catch(console.error);

    return () => {
      if (activeChildRef.current) {
        activeChildRef.current.kill().catch(console.error);
      }
    };
  }, []);

  const cancelCommand = async () => {
    if (activeChildRef.current) {
      onLog((prev) => prev + "\nCanceling active operation...\n");
      try {
        await activeChildRef.current.kill();
        onLog((prev) => prev + "Operation canceled by user.\n");
      } catch (e) {
        onLog((prev) => prev + `Failed to cancel operation: ${e}\n`);
      }
      activeChildRef.current = null;
      setIsRunning(false);
    }
  };

  const runRclone = async (action: 'put' | 'get' | 'put-dry' | 'get-dry' | 'ls' | 'lsd' | 'check') => {
    if (isRunning) return;
    setIsRunning(true);
    onLog("Loading configuration...\n");

    try {
      const store = await load("settings.json", { autoSave: true, defaults: {} });
      const savedBase = await store.get<{ value: string }>("webdav_url");
      const savedUser = await store.get<{ value: string }>("saved_username");
      const savedSub = await store.get<{ value: string }>("selected_subdirectory");

      const baseUrl = savedBase?.value || "";
      const username = savedUser?.value || "";
      const selectedSubdir = savedSub?.value || "";

      if (!baseUrl || !username) {
        onLog("Error: WebDAV URL and Username must be configured.\n");
        setIsRunning(false);
        return;
      }

      if (!selectedSubdir) {
        onLog("Error: Please select a subdirectory first.\n");
        setIsRunning(false);
        return;
      }

      // Retrieve password securely from the OS keyring
      onLog("Retrieving password from secure keyring...\n");
      let password = "";
      try {
        password = await invoke<string>("get_credentials", { username });
      } catch (e) {
        onLog((prev) => prev + `Error retrieving credentials: ${e}\n`);
        setIsRunning(false);
        return;
      }

      if (!password) {
        onLog("Error: Password not found. Please log in first.\n");
        setIsRunning(false);
        return;
      }

      // Obscure password because rclone expects obscured passwords for on-the-fly config
      onLog("Obscuring password...\n");
      const obscureCommand = createRcloneCommand(["obscure", password]);
      const obscureResult = await obscureCommand.execute();
      if (obscureResult.code !== 0) {
        onLog((prev) => prev + `Failed to obscure password: ${obscureResult.stderr}\n`);
        setIsRunning(false);
        return;
      }
      const obscuredPassword = obscureResult.stdout.trim();

      // Normalize slashes robustly
      let cleanBaseUrl = baseUrl.trim();
      while (cleanBaseUrl.endsWith("/")) {
        cleanBaseUrl = cleanBaseUrl.slice(0, -1);
      }

      let cleanSubdir = selectedSubdir.trim();
      while (cleanSubdir.startsWith("/")) {
        cleanSubdir = cleanSubdir.slice(1);
      }
      while (cleanSubdir.endsWith("/")) {
        cleanSubdir = cleanSubdir.slice(0, -1);
      }

      const remoteUrl = cleanSubdir ? `${cleanBaseUrl}/${cleanSubdir}` : cleanBaseUrl;

      let cleanMountDir = mountDir.trim();
      while (cleanMountDir.endsWith("/")) {
        cleanMountDir = cleanMountDir.slice(0, -1);
      }
      const localPath = cleanSubdir ? `${cleanMountDir}/${cleanSubdir}` : cleanMountDir;

      onLog(
        (prev) =>
          prev +
          `Target Subdirectory: ${selectedSubdir}\n` +
          `Local Path: ${localPath}\n` +
          `Remote URL: ${remoteUrl}\n\n` +
          `Running command...\n`
      );

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
      }

      // Add verbose flag with every run of the command to see the status
      args.push("-v");

      // Append credentials and endpoint configuration parameters dynamically
      args.push(
        `--webdav-url=${remoteUrl}`,
        `--webdav-user=${username}`,
        `--webdav-pass=${obscuredPassword}`
      );

      // Mask password for display in logs
      const displayArgs = args.map(arg => {
        if (arg.startsWith("--webdav-pass=")) {
          return "--webdav-pass=********";
        }
        return arg;
      });
      onLog((prev) => prev + `rclone ${displayArgs.join(" ")}\n\n`);

      const rcloneCmd = createRcloneCommand(args);

      // Listen for stdout / stderr real-time streams
      rcloneCmd.stdout.on("data", (data: string) => {
        onLog((prev) => prev + data);
      });

      rcloneCmd.stderr.on("data", (data: string) => {
        onLog((prev) => prev + data);
      });

      rcloneCmd.on("close", (data: any) => {
        onLog((prev) => prev + `\nCommand finished with exit code ${data.code}.\n`);
        activeChildRef.current = null;
        setIsRunning(false);
      });

      rcloneCmd.on("error", (error: string) => {
        onLog((prev) => prev + `\nCommand error: ${error}\n`);
        activeChildRef.current = null;
        setIsRunning(false);
      });

      const child = await rcloneCmd.spawn();
      activeChildRef.current = child;
    } catch (e) {
      onLog((prev) => prev + `System Error: ${e}\n`);
      setIsRunning(false);
    }
  };

  return (
    <div className="p-2">
      <ul className="grid grid-cols-2 gap-2 list-none [&_li]:bg-gray-800/25 [&_li]:border [&_li]:border-white/50 [&_li]:p-3 [&_li]:text-center">
        <li 
          onClick={() => runRclone('put-dry')} 
          className="cursor-pointer"
        >
          Put --dry-run (copy local to remote)
        </li>
        <li 
          onClick={() => runRclone('get-dry')} 
          className="cursor-pointer"
        >
          Get --dry-run (copy remote to local)
        </li>
        <li 
          onClick={() => runRclone('put')} 
          className="cursor-pointer"
        >
          Put (copy local to remote)
        </li>
        <li 
          onClick={() => runRclone('get')} 
          className="cursor-pointer"
        >
          Get (copy remote to local)
        </li>
        <li 
          onClick={() => runRclone('ls')} 
          className="cursor-pointer"
        >
          List files in remote (ls)
        </li>
        <li 
          onClick={() => runRclone('lsd')} 
          className="cursor-pointer"
        >
          List directories in remote (lsd)
        </li>
        <li 
          onClick={() => runRclone('check')} 
          className="cursor-pointer"
        >
          Check difference (check)
        </li>
      </ul>
      {isRunning && (
        <div className="mt-2">
          <button
            type="button"
            onClick={cancelCommand}
            className="w-full bg-red-950/25 border border-red-500/50 hover:bg-red-900/20 text-red-400 p-3 text-center cursor-pointer transition-all active:scale-[0.99] select-none text-xs uppercase font-light tracking-wide"
          >
            Cancel active operation
          </button>
        </div>
      )}
    </div>
  );
}
