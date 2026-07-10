import { useState } from "react";
import { useRcloneExecution } from "../hooks/useRcloneExecution";

interface RcloneActionsProps {
  onLog: (text: string | ((prev: string) => string)) => void;
  isRunning: boolean;
  setIsRunning: (running: boolean) => void;
}

export function RcloneActions({ onLog, isRunning, setIsRunning }: RcloneActionsProps) {
  const { runRclone, cancelCommand } = useRcloneExecution(onLog, isRunning, setIsRunning);
  const [syncConfirm, setSyncConfirm] = useState(false);

  const handleSyncClick = () => {
    if (!syncConfirm) {
      setSyncConfirm(true);
    } else {
      setSyncConfirm(false);
      runRclone('sync');
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
        <li 
          onClick={handleSyncClick} 
          className="cursor-pointer"
        >
          {syncConfirm ? "This will overwrite the remote with your local, click again to confirm" : "Sync (overwrite remote)"}
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
