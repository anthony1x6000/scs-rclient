import { normalizeSubdir } from "./utils/rclone";

/** Tick counts at or above this turn the action grid into bulk mode. */
export const BULK_MIN_FOLDERS = 2;

/** Ticks for folders no longer in the list are dropped; order follows the list. */
export function normalizeBulkSelection(items: readonly string[], ticks: readonly string[]): string[] {
  const ticked = new Set(ticks);
  return items.filter((item) => ticked.has(item));
}

export function toggleBulkTick(ticks: readonly string[], folder: string): string[] {
  return ticks.includes(folder) ? ticks.filter((t) => t !== folder) : [...ticks, folder];
}

export type AddFolderResult =
  | { ok: true; items: string[]; ticks: string[]; added: string }
  | { ok: false; error: string };

/**
 * Bottom add-input semantics: normalize the raw text, then tick it — appending
 * the folder to the list first when it is new. The caller clears the input on
 * `ok` and keeps the text (for fixing) on error.
 */
export function addFolderFromInput(
  items: readonly string[],
  ticks: readonly string[],
  raw: string,
): AddFolderResult {
  let name: string;
  try {
    name = normalizeSubdir(raw);
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
  if (!name) return { ok: false, error: "Enter a subdirectory name." };
  const nextItems = items.includes(name) ? [...items] : [...items, name];
  const nextTicks = ticks.includes(name) ? [...ticks] : [...ticks, name];
  return { ok: true, items: nextItems, ticks: normalizeBulkSelection(nextItems, nextTicks), added: name };
}

export interface BulkOutcome {
  succeeded: string[];
  failed: string[];
  skipped: string[];
}

/**
 * Runs `runOne` for each folder sequentially, continuing past failures.
 * Between folders, a truthy `isCancelled()` marks every remaining folder
 * skipped without running it.
 */
export async function runBulkSequential(
  folders: readonly string[],
  runOne: (folder: string, index: number) => Promise<void>,
  isCancelled: () => boolean = () => false,
): Promise<BulkOutcome> {
  const succeeded: string[] = [];
  const failed: string[] = [];
  const skipped: string[] = [];
  for (const [index, folder] of folders.entries()) {
    if (isCancelled()) {
      skipped.push(folder);
      continue;
    }
    try {
      await runOne(folder, index);
      succeeded.push(folder);
    } catch {
      failed.push(folder);
    }
  }
  return { succeeded, failed, skipped };
}

/** Human summary line for the console log after a bulk run. */
export function formatBulkSummary(outcome: BulkOutcome): string {
  const parts = [`${outcome.succeeded.length} ok`];
  if (outcome.failed.length > 0) parts.push(`${outcome.failed.length} failed (${outcome.failed.join(", ")})`);
  if (outcome.skipped.length > 0) parts.push(`${outcome.skipped.length} skipped (${outcome.skipped.join(", ")})`);
  return `Bulk finished: ${parts.join(", ")}.`;
}
