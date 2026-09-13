import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import Dropdown from "./components/Dropdown";
import BackgroundWrapper from "./BackgroundWrapper";
import BaseWebDAVURL from "./components/BaseWebDAVUrl";
import CredentialsForm from "./components/CredentialsForm";
import SettingsView from "./components/SettingsView";
import { RcloneActions } from "./components/RcloneActions";
import RcloneConsole from "./components/RcloneConsole";
import { ensureRcloneDetected } from "./utils/rclone";
import { getTargetSubdir } from "./settings";

function App() {
  const [mountDir, setMountDir] = useState<string>("");
  const [targetSubdir, setTargetSubdir] = useState<string>("");
  const [selectedSubdir, setSelectedSubdir] = useState<string>("");
  const [showSettings, setShowSettings] = useState<boolean>(false);
  const [logs, setLogs] = useState<string>("");
  const [isRunning, setIsRunning] = useState<boolean>(false);
  const [rcloneStatus, setRcloneStatus] = useState<string>("pending");
  const mountSeq = useRef(0);

  const updateMountDir = async (subdir?: string) => {
    const seq = ++mountSeq.current;
    try {
      const resolved = await invoke<string>("get_mount_dir", {
        targetSubdir: subdir ? subdir : undefined,
      });
      if (mountSeq.current === seq) setMountDir(resolved);
    } catch (e) {
      console.error("Failed to get mount dir:", e);
    }
  };

  useEffect(() => {
    // Detect rclone sidecar on mount (surface failure instead of swallowing).
    ensureRcloneDetected().then(
      (s) => setRcloneStatus(s),
      (e) => {
        console.error(e);
        setRcloneStatus("missing");
      }
    );

    // Fetch target subdirectory from the single settings module.
    getTargetSubdir()
      .then((sub) => {
        setTargetSubdir(sub);
        updateMountDir(sub);
      })
      .catch((e) => {
        console.error(e);
        updateMountDir();
      });
  }, []);

  const handleTargetSubdirChange = (newSub: string) => {
    setTargetSubdir(newSub);
    void updateMountDir(newSub);
  };

  return (
    <BackgroundWrapper>
      <div className="flex flex-col h-screen pb-44 box-border">
        {rcloneStatus === "missing" && (
          <div className="p-2 text-xs text-red-300 border border-red-500/50 bg-red-950/25" role="alert">
            No usable rclone binary found. Install rclone or repair the sidecar, then reload.
          </div>
        )}
        <div className="px-2 text-xs text-gray-400" aria-label="Sync subdirectory">
          Sync item: {selectedSubdir || "none selected"}
        </div>
        <RcloneActions
          onLog={setLogs}
          isRunning={isRunning}
          setIsRunning={setIsRunning}
        />

        <RcloneConsole logs={logs} onClear={() => setLogs("")} />
      </div>

      <div className="bottom-0 absolute p-2 w-[100%] text-white flex flex-col gap-4">
        <div className="p-2 font-['Roboto'] font-light">
          <div className="text-nowrap">
            <Dropdown onSelect={setSelectedSubdir} />
            <div className="inline italic">
              a subdirectory of your WebDAV drive
            </div>
          </div>
          {mountDir && (
            <div className="text-[10px] text-gray-400 font-mono mt-1 opacity-70">
              mount directory: {mountDir}
            </div>
          )}
        </div>
        <div className="flex items-center gap-1 w-full">
          <div className={showSettings ? "hidden" : "flex items-center gap-1 w-full"}>
            <CredentialsForm />
            <BaseWebDAVURL />
            <button
              type="button"
              onClick={() => setShowSettings(true)}
              className="ml-2 px-3 py-1 text-xs border border-white/20 hover:border-white/40 focus:border-white/60 bg-transparent text-white outline-none cursor-pointer hover:bg-white/5 active:scale-95 transition-all text-nowrap"
            >
              Settings
            </button>
          </div>
          {showSettings && (
            <SettingsView
              onClose={() => setShowSettings(false)}
              targetSubdir={targetSubdir}
              onTargetSubdirChange={handleTargetSubdirChange}
            />
          )}
        </div>
      </div>
    </BackgroundWrapper>
  );
}

export default App;
