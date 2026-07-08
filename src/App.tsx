import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import Dropdown from "./components/Dropdown";
import BackgroundWrapper from "./BackgroundWrapper";
import BaseWebDAVURL from "./components/BaseWebDAVUrl";
import CredentialsForm from "./components/CredentialsForm";
import SettingsView from "./components/SettingsView";
import { RcloneActions } from "./components/RcloneActions";
import RcloneConsole from "./components/RcloneConsole";
import { ensureRcloneDetected } from "./utils/rclone";

function App() {
  const [mountDir, setMountDir] = useState<string>("");
  const [showSettings, setShowSettings] = useState<boolean>(false);
  const [logs, setLogs] = useState<string>("");
  const [isRunning, setIsRunning] = useState<boolean>(false);

  useEffect(() => {
    // Detect rclone sidecar on mount
    ensureRcloneDetected();

    // Fetch and set mount directory
    invoke<string>("get_mount_dir")
      .then(setMountDir)
      .catch(console.error);
  }, []);

  return (
    <BackgroundWrapper>
      <div className="flex flex-col h-screen pb-44 box-border">
        <RcloneActions 
          onLog={setLogs} 
          isRunning={isRunning} 
          setIsRunning={setIsRunning} 
        />
        
        <RcloneConsole logs={logs} />
      </div>

      <div className="bottom-0 absolute p-2 w-[100%] text-white flex flex-col gap-4">
        <div className="p-2 font-['Roboto'] font-light">
          <div className="text-nowrap">
            <Dropdown />
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
          {!showSettings ? (
            <>
              <CredentialsForm />
              <BaseWebDAVURL />
              <button
                type="button"
                onClick={() => setShowSettings(true)}
                className="ml-2 px-3 py-1 text-xs border border-white/20 hover:border-white/40 focus:border-white/60 bg-transparent text-white outline-none cursor-pointer hover:bg-white/5 active:scale-95 transition-all text-nowrap"
              >
                Settings
              </button>
            </>
          ) : (
            <SettingsView onClose={() => setShowSettings(false)} />
          )}
        </div>
      </div>
    </BackgroundWrapper>
  );
}

export default App;

