import { describe, expect, it } from "vitest";
import {
  excludedFoldersFor,
  exclusionScopeKey,
  legacyFallbackList,
  migrateLegacyList,
  parseExcludedFoldersMap,
  normalizeExcludedFolder,
  normalizeExcludedFolders,
  MAX_EXCLUDED_FOLDERS,
} from "./exclusions";

describe("exclusionScopeKey", () => {
  it("passes through clean relative paths", () => {
    expect(exclusionScopeKey("content/enforced/1052175-dev_asteve18")).toBe(
      "content/enforced/1052175-dev_asteve18",
    );
  });

  it("trims whitespace and stray separators", () => {
    expect(exclusionScopeKey("  /css/  ")).toBe("css");
  });

  it("converts backslashes and collapses repeated separators", () => {
    expect(exclusionScopeKey("a\\\\b//c")).toBe("a/b/c");
  });

  it("drops dot segments", () => {
    expect(exclusionScopeKey("./a/./b")).toBe("a/b");
  });

  it("maps the empty selection to the root-scope key", () => {
    expect(exclusionScopeKey("")).toBe("");
  });
});

describe("parseExcludedFoldersMap", () => {
  it("accepts a valid per-directory map untouched", () => {
    const map = { css: [".git"], "a/b": ["assets/cache"] };
    expect(parseExcludedFoldersMap(map)).toEqual(map);
  });

  it("skips individually invalid scope entries instead of dropping the whole map", () => {
    const parsed = parseExcludedFoldersMap({ css: [".git"], bad: "nope", js: 3, ok: [] });
    expect(Object.keys(parsed).sort()).toEqual(["css", "ok"]);
    expect(parsed["css"]).toEqual([".git"]);
    expect(parsed["ok"]).toEqual([]);
  });

  it("treats the legacy flat array as no map at all", () => {
    expect(Object.keys(parseExcludedFoldersMap([".git", "node_modules"]))).toEqual([]);
  });

  it("returns a prototype-less map on every path", () => {
    expect(Object.getPrototypeOf(parseExcludedFoldersMap({}))).toBeNull();
    expect(Object.getPrototypeOf(parseExcludedFoldersMap([".git"]))).toBeNull();
    expect(Object.getPrototypeOf(parseExcludedFoldersMap("nope"))).toBeNull();
    expect(Object.getPrototypeOf(parseExcludedFoldersMap(null))).toBeNull();
  });

  it("keeps a __proto__ scope key as plain data", () => {
    // JSON.parse creates an own "__proto__" property; parsing must preserve it
    // as data on the prototype-less result instead of mutating a prototype.
    const stored = JSON.parse('{"__proto__": [".git"]}');
    const parsed = parseExcludedFoldersMap(stored);
    expect(Object.keys(parsed)).toEqual(["__proto__"]);
    expect(parsed["__proto__"]).toEqual([".git"]);
    expect(Object.getPrototypeOf(parsed)).toBeNull();
  });
});

describe("legacyFallbackList", () => {
  it("normalizes the stored legacy array", () => {
    expect(legacyFallbackList(["/.git/", ".git", "node_modules"])).toEqual([
      ".git",
      "node_modules",
    ]);
  });

  it("drops invalid legacy entries", () => {
    expect(legacyFallbackList(["..", "ok"])).toEqual(["ok"]);
  });

  it("is empty for non-array legacy values", () => {
    expect(legacyFallbackList(undefined)).toEqual([]);
    expect(legacyFallbackList("css")).toEqual([]);
  });
});

describe("migrateLegacyList", () => {
  it("moves the legacy flat list under the selected directory's scope key", () => {
    expect(migrateLegacyList(["/.git/", ".git", "node_modules"], "content/x")).toEqual({
      "content/x": [".git", "node_modules"],
    });
  });

  it("normalizes and caps like every other write", () => {
    const many = Array.from({ length: MAX_EXCLUDED_FOLDERS + 3 }, (_, i) => `f${i}/`);
    const migrated = migrateLegacyList(many, "css");
    expect(migrated["css"]).toHaveLength(MAX_EXCLUDED_FOLDERS);
  });

  it("drops invalid legacy entries instead of failing the migration", () => {
    expect(migrateLegacyList(["..", "ok", "\u0000bad"], "css")).toEqual({ css: ["ok"] });
  });

  it("scopes to the root key when no directory is selected", () => {
    expect(migrateLegacyList([".git"], "")).toEqual({ "": [".git"] });
  });
});

describe("excludedFoldersFor", () => {
  it("returns the stored list for the exact directory", () => {
    const byDir = { css: [".git", "build/cache"] };
    expect(excludedFoldersFor(byDir, "css")).toEqual([".git", "build/cache"]);
  });

  it("scopes by the normalized key, not the raw string", () => {
    const byDir = { "a/b": [".git"] };
    expect(excludedFoldersFor(byDir, "/a//b/")).toEqual([".git"]);
  });

  it("returns an empty list for directories with no stored exclusions", () => {
    expect(excludedFoldersFor({ css: [".git"] }, "js")).toEqual([]);
    expect(excludedFoldersFor({}, "css")).toEqual([]);
  });

  it("normalizes and dedupes stored entries before returning them", () => {
    const byDir = { css: ["/.git/", ".git", "node_modules"] };
    expect(excludedFoldersFor(byDir, "css")).toEqual([".git", "node_modules"]);
  });

  it("drops invalid stored entries", () => {
    const byDir = { css: ["..", "\u0007bad", "ok"] };
    expect(excludedFoldersFor(byDir, "css")).toEqual(["ok"]);
  });

  it("falls back to the legacy list only for scopes without an explicit entry", () => {
    const byDir = { css: [".git"] };
    expect(excludedFoldersFor(byDir, "css", [".git", "node_modules"])).toEqual([".git"]);
    expect(excludedFoldersFor(byDir, "js", [".git", "node_modules"])).toEqual([
      ".git",
      "node_modules",
    ]);
  });

  it("treats an explicit empty list as an override of the legacy fallback", () => {
    const byDir = { css: [] };
    expect(excludedFoldersFor(byDir, "css", [".git"])).toEqual([]);
  });

  it("returns nothing for unknown scopes when there is no fallback", () => {
    expect(excludedFoldersFor({ css: [".git"] }, "js", [])).toEqual([]);
  });
});

describe("normalizeExcludedFolder", () => {
  it("normalizes separators and strips surrounding slashes", () => {
    expect(normalizeExcludedFolder("/.git/")).toBe(".git");
    expect(normalizeExcludedFolder("assets\\\\cache")).toBe("assets/cache");
  });

  it("rejects traversal, control characters, and empty input", () => {
    expect(normalizeExcludedFolder("../up")).toBeNull();
    expect(normalizeExcludedFolder("a\u0000b")).toBeNull();
    expect(normalizeExcludedFolder(" / ")).toBeNull();
  });
});

describe("normalizeExcludedFolders", () => {
  it("caps the list at MAX_EXCLUDED_FOLDERS entries in order", () => {
    const many = Array.from({ length: MAX_EXCLUDED_FOLDERS + 5 }, (_, i) => `f${i}`);
    expect(normalizeExcludedFolders(many)).toHaveLength(MAX_EXCLUDED_FOLDERS);
    expect(normalizeExcludedFolders(many)[0]).toBe("f0");
  });
});
