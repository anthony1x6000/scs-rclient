
/** Shared byte formatting (kept local so this branch does not depend on the
 * progress PR; the two copies are consolidated when both land). */
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

export type ChangeStatus = "added" | "removed" | "modified";

/** Mirrors the camelCase `ChangeEntry` serialized by the Rust command. */
export interface ChangeEntry {
  path: string;
  status: ChangeStatus;
  localSize: number | null;
  localMtime: number | null;
  remoteSize: number | null;
  remoteMtime: number | null;
}

export type PreviewKind = "text" | "binary" | "tooLarge" | "missing";

export interface PreviewSide {
  kind: PreviewKind;
  size: number;
  mtime: number | null;
  text: string | null;
}

export interface ChangePreview {
  path: string;
  local: PreviewSide;
  remote: PreviewSide;
}

export interface StatusCounts {
  added: number;
  removed: number;
  modified: number;
}

export function countStatuses(entries: readonly ChangeEntry[]): StatusCounts {
  const counts: StatusCounts = { added: 0, removed: 0, modified: 0 };
  for (const entry of entries) counts[entry.status] += 1;
  return counts;
}

/**
 * Left bar: what exists locally and is not identical remotely. Remote-only
 * additions have no local row, so they are excluded.
 */
export function localEntries(entries: readonly ChangeEntry[]): ChangeEntry[] {
  return entries.filter((entry) => entry.status !== "added");
}

/** Right bar: remote rows. Locally-removed files have no remote row. */
export function remoteEntries(entries: readonly ChangeEntry[]): ChangeEntry[] {
  return entries.filter((entry) => entry.status !== "removed");
}

export interface BarSegment {
  key: ChangeStatus;
  label: string;
  count: number;
  className: string;
}

/**
 * The statuses each side bar can show. Local can show removed + modified;
 * remote can show added + modified. Order is the visual segment order.
 */
export function barSegments(
  side: "local" | "remote",
  counts: StatusCounts,
): BarSegment[] {
  if (side === "local") {
    return [
      { key: "modified", label: "modified", count: counts.modified, className: "bg-amber-400/80" },
      { key: "removed", label: "removed", count: counts.removed, className: "bg-red-400/80" },
    ];
  }
  return [
    { key: "modified", label: "modified", count: counts.modified, className: "bg-amber-400/80" },
    { key: "added", label: "added", count: counts.added, className: "bg-emerald-400/80" },
  ];
}

export function statusLabel(status: ChangeStatus): string {
  switch (status) {
    case "added":
      return "Added remotely";
    case "removed":
      return "Removed remotely";
    case "modified":
      return "Modified";
  }
}

export function statusGlyph(status: ChangeStatus): string {
  switch (status) {
    case "added":
      return "+";
    case "removed":
      return "−";
    case "modified":
      return "~";
  }
}

export function statusTextClass(status: ChangeStatus): string {
  switch (status) {
    case "added":
      return "text-emerald-300";
    case "removed":
      return "text-red-300";
    case "modified":
      return "text-amber-300";
  }
}

export function sideSize(entry: ChangeEntry, side: "local" | "remote"): number | null {
  return side === "local" ? entry.localSize : entry.remoteSize;
}

export function formatMtime(mtime: number | null): string {
  if (mtime === null || !Number.isFinite(mtime)) return "unknown";
  const date = new Date(mtime * 1000);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(
    date.getHours(),
  )}:${pad(date.getMinutes())}`;
}
