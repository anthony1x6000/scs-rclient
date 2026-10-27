import { BULK_MIN_FOLDERS } from "./bulk";

/** Preset lists are capped at this many entries; the oldest tail entry is evicted first. */
export const MAX_BULK_PRESETS = 20;

/** A saved bulk set. Preset lists are recency-ordered: index 0 = most recent. */
export interface BulkPreset {
  folders: string[];
}

/**
 * Saves a folder set as the most recent preset, or bumps the matching preset
 * back to index 0 (keeping this run's order). Members are deduplicated first,
 * so repeated ticks of one folder never form a preset.
 */
export function upsertBulkPreset(presets: readonly BulkPreset[], folders: readonly string[]): BulkPreset[] {
  const unique = [...new Set(folders)];
  if (unique.length < BULK_MIN_FOLDERS) return [...presets];
  const wanted = new Set(unique);
  const rest = presets.filter(
    (preset) => preset.folders.length !== wanted.size || preset.folders.some((folder) => !wanted.has(folder)),
  );
  return [{ folders: unique }, ...rest].slice(0, MAX_BULK_PRESETS);
}

/** Drops preset members no longer in the folder list, preserving preset order. */
export function livePresetFolders(preset: BulkPreset, items: readonly string[]): string[] {
  const present = new Set(items);
  return preset.folders.filter((folder) => present.has(folder));
}

/** Dropdown text for a preset: `<n> folders · <first member>`. */
export function presetBaseLabel(preset: BulkPreset): string {
  const [first = ""] = preset.folders;
  return `${preset.folders.length} folders · ${first}`;
}

/** Unique dropdown labels: later presets sharing a base label get " (2)", " (3)", … */
export function buildPresetLabels(presets: readonly BulkPreset[]): string[] {
  const seenBases = new Map<string, number>();
  return presets.map((preset) => {
    const base = presetBaseLabel(preset);
    const count = (seenBases.get(base) ?? 0) + 1;
    seenBases.set(base, count);
    return count === 1 ? base : `${base} (${count})`;
  });
}

/** Native title tooltip: one directory per line. */
export function presetTooltip(preset: BulkPreset): string {
  return preset.folders.join("\n");
}

/** Case-insensitive substring match over the base label and every member directory. */
export function filterBulkPresets(presets: readonly BulkPreset[], searchText: string): BulkPreset[] {
  const needle = searchText.trim().toLowerCase();
  if (needle === "") return [...presets];
  return presets.filter(
    (preset) =>
      presetBaseLabel(preset).toLowerCase().includes(needle) ||
      preset.folders.some((folder) => folder.toLowerCase().includes(needle)),
  );
}
