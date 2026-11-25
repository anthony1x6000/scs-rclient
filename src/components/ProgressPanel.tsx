import {
  formatBytes,
  percentLabel,
  phaseLabel,
  progressDetail,
  progressRatio,
  type ProgressState,
} from "../progress";

interface ProgressPanelProps {
  progress: ProgressState;
}

/**
 * Compact transfer strip above the console: live phase, bar, file/byte
 * counters, and a terminal done/failed state. It is deliberately thin so the
 * console keeps most of the window.
 */
export default function ProgressPanel({ progress }: ProgressPanelProps) {
  if (progress.done) {
    const detail = progressDetail(progress);
    return (
      <div
        role="status"
        aria-live="polite"
        className={`shrink-0 mb-1 px-2 py-1.5 border text-[11px] flex items-baseline justify-between gap-3 ${
          progress.failed
            ? "border-red-500/50 bg-red-950/30 text-red-300"
            : "border-emerald-500/40 bg-emerald-950/25 text-emerald-200"
        }`}
      >
        <span className="font-medium">
          {progress.failed ? "Operation failed" : "Finished"}
        </span>
        <span className="truncate opacity-80">
          {progress.failed
            ? "See the console for the failing file."
            : detail || `${formatBytes(progress.bytesDone)} transferred`}
        </span>
      </div>
    );
  }

  const ratio = progressRatio(progress);
  const indeterminate = ratio === null;
  const phase = phaseLabel(progress);
  const detail = progressDetail(progress);

  return (
    <div
      role="status"
      aria-live="polite"
      className="shrink-0 mb-1 px-2 py-1.5 border border-white/15 bg-black/30 text-white"
    >
      <div className="flex items-baseline justify-between gap-3 text-[11px]">
        <span className="truncate">
          <span className="text-white/90">{phase}</span>
          {progress.subdir && (
            <span className="text-gray-400"> · {progress.subdir}</span>
          )}
        </span>
        <span className="shrink-0 text-gray-300 tabular-nums">
          {percentLabel(progress)}
          {detail && <span className="ml-2 text-gray-400">{detail}</span>}
        </span>
      </div>
      <div
        className="mt-1 h-1 w-full overflow-hidden bg-white/10"
        role="progressbar"
        aria-label={`${phase} progress`}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={indeterminate ? undefined : Math.round((ratio ?? 0) * 100)}
      >
        <div
          className={`h-full bg-white/70 transition-[width] duration-150 ${
            indeterminate ? "animate-pulse" : ""
          }`}
          style={{ width: indeterminate ? "33%" : `${(ratio ?? 0) * 100}%` }}
        />
      </div>
      {progress.file && (
        <div className="mt-1 truncate text-[10px] text-gray-400" title={progress.file}>
          {progress.file}
        </div>
      )}
    </div>
  );
}
