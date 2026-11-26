
export { formatBytes } from "./progress";

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

/** What the review list is showing: everything, content edits, or metadata edits. */
export type ChangeFilter = "all" | "content" | "metadata";

export type ChangeKind = Exclude<ChangeFilter, "all">;

export interface ChangeFilterCounts {
  all: number;
  content: number;
  metadata: number;
}

/**
 * Added and removed files are content changes by definition. A modified file
 * whose size differs had its bytes change; a same-size modified file only moved
 * its timestamp, so the scan reports it as a metadata edit. Reading every body
 * to prove a same-size edit would make the scan cost one download per row.
 */
export function changeKind(entry: ChangeEntry): ChangeKind {
  if (entry.status !== "modified") return "content";
  const local = entry.localSize;
  const remote = entry.remoteSize;
  if (local !== null && remote !== null && local !== remote) return "content";
  return "metadata";
}

export function filterChangeEntries(
  entries: readonly ChangeEntry[],
  filter: ChangeFilter,
): ChangeEntry[] {
  if (filter === "all") return [...entries];
  return entries.filter((entry) => changeKind(entry) === filter);
}

export function countChangeFilters(entries: readonly ChangeEntry[]): ChangeFilterCounts {
  const counts: ChangeFilterCounts = {
    all: entries.length,
    content: 0,
    metadata: 0,
  };
  for (const entry of entries) counts[changeKind(entry)] += 1;
  return counts;
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
