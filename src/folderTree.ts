/**
 * Pure helpers for turning the backend's flat remote-folder listing into the
 * picker's expandable tree. No Tauri imports so it stays unit-testable.
 */

/** One folder of the remote tree; `path` is relative to the listed directory. */
export interface RemoteFolder {
  /** Slash-separated relative path; "" is the listed directory itself. */
  path: string;
  /** Total bytes of all files beneath this folder. */
  size: number;
}

/** Parent path of `path`; top-level folders live under the "" root. */
export function parentOf(path: string): string {
  const idx = path.lastIndexOf("/");
  return idx === -1 ? "" : path.slice(0, idx);
}

/** Last path segment, shown as the folder's row label. */
export function folderDisplayName(path: string): string {
  const idx = path.lastIndexOf("/");
  return idx === -1 ? path : path.slice(idx + 1);
}

/**
 * Groups a flat listing by parent path so each tree level can be derived
 * client-side: expanding a folder never needs another WebDAV request.
 * The root entry ("") is never listed as a child; groups sort by path.
 */
export function groupFoldersByParent(folders: readonly RemoteFolder[]): Map<string, RemoteFolder[]> {
  const groups = new Map<string, RemoteFolder[]>();
  for (const folder of folders) {
    if (folder.path === "") continue;
    const parent = parentOf(folder.path);
    const list = groups.get(parent);
    if (list) list.push(folder);
    else groups.set(parent, [folder]);
  }
  for (const list of groups.values()) {
    list.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
  }
  return groups;
}

/** The root entry of a listing, synthesized as empty when the server sent none. */
export function rootFolder(folders: readonly RemoteFolder[]): RemoteFolder {
  return folders.find((folder) => folder.path === "") ?? { path: "", size: 0 };
}

/** Human-readable byte size: binary units, one decimal under ten, none above. */
export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value < 10 ? value.toFixed(1) : value.toFixed(0)} ${units[unit]}`;
}
