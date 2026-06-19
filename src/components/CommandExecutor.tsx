import { useState } from "react";
import { Command } from "@tauri-apps/plugin-shell";

function CommandExecutor() {
  const [output, setOutput] = useState<string>("");
  const [isLoading, setIsLoading] = useState<boolean>(false);
  const [error, setError] = useState<string>("");

  const handleRunCommand = async () => {
    setIsLoading(true);
    setError("");
    setOutput("");
    try {
      // Create the Tauri shell command using the configured capability alias
      const command = Command.create("rclone", [
        "ls",
        ":webdav:pub/demo/docs/",
        "--webdav-url=https://a.ocv.me/"
      ]);

      // Run and wait for execution to complete
      const result = await command.execute();

      if (result.code === 0) {
        setOutput(result.stdout || "Command completed successfully but produced no output.");
      } else {
        setError(`Command failed with exit code ${result.code}`);
        setOutput(result.stderr || "No error output returned.");
      }
    } catch (err: any) {
      console.error(err);
      setError(err?.message || String(err));
    } finally {
      setIsLoading(false);
    }
  };

  return (
    <div className="w-[80%] max-w-[600px] mt-6 p-4 rounded-xl bg-white/5 backdrop-blur-md border border-white/10 shadow-2xl flex flex-col gap-3">
      <div className="flex items-center justify-between">
        <span className="text-xs font-semibold tracking-wider uppercase text-gray-400">
          Command Executor
        </span>
        <button
          onClick={handleRunCommand}
          disabled={isLoading}
          className={`px-4 py-1.5 text-xs font-semibold rounded-lg bg-gradient-to-r from-cyan-500 to-blue-600 hover:from-cyan-400 hover:to-blue-500 text-white transition-all shadow-md active:scale-95 disabled:opacity-50 disabled:pointer-events-none cursor-pointer flex items-center gap-2`}
        >
          {isLoading ? (
            <>
              <svg className="animate-spin h-3 w-3 text-white" fill="none" viewBox="0 0 24 24">
                <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" />
                <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z" />
              </svg>
              Running...
            </>
          ) : (
            "Run Rclone Command"
          )}
        </button>
      </div>

      <div className="relative">
        <textarea
          readOnly
          value={isLoading ? "Executing command: rclone ls :webdav:pub/demo/docs/ --webdav-url=https://a.ocv.me/..." : error ? `Error:\n${error}\n\nDetails:\n${output}` : output}
          placeholder="Command output will be displayed here..."
          className={`w-full h-40 p-3 rounded-lg bg-black/40 border border-white/5 text-gray-300 font-mono text-[11px] leading-relaxed resize-none outline-none focus:border-cyan-500/50 transition-colors ${
            error ? "text-red-400 border-red-900/30" : ""
          }`}
        />
        {!isLoading && !output && !error && (
          <div className="absolute inset-0 flex items-center justify-center pointer-events-none text-gray-500 text-xs">
            No command output loaded.
          </div>
        )}
      </div>
    </div>
  );
}

export default CommandExecutor;
