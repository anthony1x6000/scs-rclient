import { useState } from "react";
import { useRcloneExecution, type RcloneActionType } from "../hooks/useRcloneExecution";

interface RcloneActionsProps {
  onLog: (text: string | ((prev: string) => string)) => void;
  isRunning: boolean;
  setIsRunning: (running: boolean) => void;
}

interface ActionDef {
  id: RcloneActionType;
  label: string;
  preview?: boolean;
}

const ACTIONS: ActionDef[] = [
  { id: "put-dry", label: "Put --dry-run (copy local to remote)", preview: true },
  { id: "get-dry", label: "Get --dry-run (copy remote to local)", preview: true },
  { id: "put", label: "Put (copy local to remote)" },
  { id: "get", label: "Get (copy remote to local)" },
  { id: "put-checksum", label: "Put --checksum (hash check local to remote)" },
  { id: "get-checksum", label: "Get --checksum (hash check remote to local)" },
  { id: "ls", label: "List files in remote (ls)", preview: true },
  { id: "lsd", label: "List directories in remote (lsd)", preview: true },
  { id: "check", label: "Check difference (check)", preview: true },
];

export function RcloneActions({ onLog, isRunning, setIsRunning }: RcloneActionsProps) {
  const { runRclone, cancelCommand } = useRcloneExecution(onLog, isRunning, setIsRunning);
  const [syncArmed, setSyncArmed] = useState(false);
  const [previewOk, setPreviewOk] = useState(false);
  const [syncDetail, setSyncDetail] = useState("");

  const runWithCompletion = (action: RcloneActionType) => {
    if (isRunning) return;
    runRclone(action, {
      onDone: (code) => {
        if (code === 0 && ACTIONS.some((a) => a.id === action && a.preview)) {
          setPreviewOk(true);
        }
      },
    });
  };

  const handleSyncClick = () => {
    if (isRunning) return;
    if (!syncArmed) {
      setSyncDetail("");
      setSyncArmed(true);
      return;
    }
    if (!previewOk) {
      onLog("Sync blocked: run a --dry-run, check, or ls preview successfully first.\n");
      return;
    }
    setSyncArmed(false);
    runRclone("sync");
  };

  return (
    <div className="p-2">
      <ul className="grid grid-cols-2 gap-2 list-none">
        {ACTIONS.map((action) => (
          <li key={action.id} className="bg-gray-800/25 border border-white/50 p-0 text-center">
            <button
              type="button"
              disabled={isRunning}
              onClick={() => runWithCompletion(action.id)}
              className="w-full p-3 cursor-pointer disabled:opacity-40 disabled:cursor-not-allowed focus-visible:outline focus-visible:outline-2 focus-visible:outline-white"
            >
              {action.label}
            </button>
          </li>
        ))}
        <li className="bg-gray-800/25 border border-white/50 p-0 text-center">
          <button
            type="button"
            disabled={isRunning}
            onClick={handleSyncClick}
            aria-live="polite"
            className="w-full p-3 cursor-pointer disabled:opacity-40 disabled:cursor-not-allowed focus-visible:outline focus-visible:outline-2 focus-visible:outline-white"
          >
            {syncArmed
              ? previewOk
                ? "Sync will DELETE remote files missing locally. Click again to confirm."
                : "Run a --dry-run / check / ls preview first, then click again to arm sync"
              : "Sync (overwrite remote)"}
          </button>
        </li>
      </ul>
      {syncArmed && (
        <div className="mt-2 flex gap-2">
          <button
            type="button"
            onClick={() => setSyncArmed(false)}
            className="px-3 py-1 text-xs border border-white/20 hover:border-white/40 bg-transparent text-white cursor-pointer"
          >
            Cancel sync
          </button>
          <span className="text-xs text-gray-400 self-center" aria-live="polite">
            {syncDetail || "Armed: review the preview output above before confirming."}
          </span>
        </div>
      )}
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
