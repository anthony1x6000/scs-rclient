/** Snapshot streamed by the backend on the `webdav-progress` event. */
export interface WebdavProgress {
  subdir: string;
  action: string;
  phase: string;
  completed: number;
  total: number;
  bytesDone: number;
  bytesTotal: number;
  file: string | null;
}

/** Progress plus the terminal flags the hook adds when an operation ends. */
export interface ProgressState extends WebdavProgress {
  done: boolean;
  failed: boolean;
}

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const decimals = unit === 0 || value >= 100 ? 0 : 1;
  return `${value.toFixed(decimals)} ${units[unit]}`;
}

/**
 * Fraction complete, or null while the backend has not reported a total yet
 * (the initial remote scan). File count and byte count both contribute so a
 * single large download still moves the bar mid-file.
 */
export function progressRatio(progress: WebdavProgress): number | null {
  const fileRatio = progress.total > 0 ? progress.completed / progress.total : 0;
  const byteRatio =
    progress.bytesTotal > 0 ? progress.bytesDone / progress.bytesTotal : 0;
  const ratio = Math.max(fileRatio, byteRatio);
  if (progress.total <= 0 && progress.bytesTotal <= 0) return null;
  return Math.min(1, Math.max(0, ratio));
}

export function percentLabel(progress: WebdavProgress): string {
  const ratio = progressRatio(progress);
  return ratio === null ? "…" : `${Math.round(ratio * 100)}%`;
}

export function phaseLabel(progress: WebdavProgress): string {
  switch (progress.phase) {
    case "scan":
      return "Scanning";
    case "download":
      return "Downloading";
    case "upload":
      return "Uploading";
    case "preview":
      return "Checking";
    case "compare":
      return "Comparing";
    case "delete":
      return "Deleting remote files";
    case "list":
      return "Listing";
    default:
      return progress.phase || "Working";
  }
}

/** One-line detail for the active operation: files, bytes, and current file. */
export function progressDetail(progress: WebdavProgress): string {
  const parts: string[] = [];
  if (progress.total > 0) {
    parts.push(`${Math.min(progress.completed, progress.total)}/${progress.total} files`);
  }
  if (progress.bytesTotal > 0) {
    parts.push(`${formatBytes(progress.bytesDone)} / ${formatBytes(progress.bytesTotal)}`);
  }
  return parts.join(" · ");
}
