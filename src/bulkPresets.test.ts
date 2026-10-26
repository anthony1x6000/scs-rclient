import { describe, expect, it } from "vitest";
import { BULK_MIN_FOLDERS } from "./bulk";
import {
  MAX_BULK_PRESETS,
  buildPresetLabels,
  filterBulkPresets,
  livePresetFolders,
  presetBaseLabel,
  presetTooltip,
  upsertBulkPreset,
} from "./bulkPresets";

describe("upsertBulkPreset", () => {
  it("prepends a new folder set at index 0", () => {
    const presets = [{ folders: ["css", "js"] }];
    expect(upsertBulkPreset(presets, ["img", "doc"])).toEqual([
      { folders: ["img", "doc"] },
      { folders: ["css", "js"] },
    ]);
  });

  it("moves an existing member set to index 0 without duplicating, keeping the new run's order", () => {
    const presets = [
      { folders: ["css", "js"] },
      { folders: ["img", "doc", "music"] },
    ];
    expect(upsertBulkPreset(presets, ["music", "doc", "img"])).toEqual([
      { folders: ["music", "doc", "img"] },
      { folders: ["css", "js"] },
    ]);
  });

  it("caps the list at MAX_BULK_PRESETS, evicting the oldest tail entry", () => {
    const full = Array.from({ length: MAX_BULK_PRESETS }, (_, i) => ({ folders: [`d${i}`, `e${i}`] }));
    const result = upsertBulkPreset(full, ["new1", "new2"]);
    expect(result).toHaveLength(MAX_BULK_PRESETS);
    expect(result[0]).toEqual({ folders: ["new1", "new2"] });
    expect(result[result.length - 1]).toEqual({
      folders: [`d${MAX_BULK_PRESETS - 2}`, `e${MAX_BULK_PRESETS - 2}`],
    });
    expect(result.some((preset) => preset.folders.includes(`d${MAX_BULK_PRESETS - 1}`))).toBe(false);
  });

  it("returns the presets unchanged for fewer folders than BULK_MIN_FOLDERS", () => {
    const presets = [{ folders: ["css", "js"] }];
    const short = Array.from({ length: BULK_MIN_FOLDERS - 1 }, (_, i) => `f${i}`);
    expect(upsertBulkPreset(presets, short)).toEqual(presets);
  });

  it("collapses duplicate members, keeping first-occurrence order", () => {
    expect(upsertBulkPreset([], ["img", "css", "img", "doc", "css"])).toEqual([
      { folders: ["img", "css", "doc"] },
    ]);
  });
});

describe("presetBaseLabel", () => {
  it("formats as '<n> folders · <first member>'", () => {
    const folders = ["music", ...Array.from({ length: 8 }, (_, i) => `dir${i}`)];
    expect(presetBaseLabel({ folders })).toBe("9 folders · music");
  });

  it("always reads as plural since presets hold at least two folders", () => {
    expect(presetBaseLabel({ folders: ["css", "js"] })).toBe("2 folders · css");
  });
});

describe("buildPresetLabels", () => {
  it("suffixes later presets that share a base label, leaving other bases untouched", () => {
    const presets = [
      { folders: ["a", "b", "c"] },
      { folders: ["a", "d", "e"] },
      { folders: ["b", "g", "h"] },
    ];
    expect(buildPresetLabels(presets)).toEqual(["3 folders · a", "3 folders · a (2)", "3 folders · b"]);
  });

  it("numbers a third and fourth occurrence too", () => {
    const presets = Array.from({ length: 4 }, () => ({ folders: ["music", "x", "y"] }));
    expect(buildPresetLabels(presets)).toEqual([
      "3 folders · music",
      "3 folders · music (2)",
      "3 folders · music (3)",
      "3 folders · music (4)",
    ]);
  });
});

describe("livePresetFolders", () => {
  it("keeps preset order, not list order, and drops stale folders", () => {
    expect(livePresetFolders({ folders: ["img", "css", "doc"] }, ["doc", "css"])).toEqual(["css", "doc"]);
  });
});

describe("presetTooltip", () => {
  it("joins members with newlines, one directory per line", () => {
    expect(presetTooltip({ folders: ["css", "js", "img"] })).toBe("css\njs\nimg");
  });
});

describe("filterBulkPresets", () => {
  const presets = [
    { folders: ["music", "js"] },
    { folders: ["img", "doc"] },
  ];

  it("returns every preset for an empty or whitespace-only search", () => {
    expect(filterBulkPresets(presets, "")).toEqual(presets);
    expect(filterBulkPresets(presets, "   ")).toEqual(presets);
  });

  it("matches member directories case-insensitively as a substring", () => {
    expect(filterBulkPresets([{ folders: ["Music", "js"] }, { folders: ["img", "doc"] }], "JS")).toEqual([
      { folders: ["Music", "js"] },
    ]);
  });

  it("matches the base label even when no member contains the text", () => {
    expect(filterBulkPresets(presets, "2 FOLDERS · MUSIC")).toEqual([{ folders: ["music", "js"] }]);
  });
});
