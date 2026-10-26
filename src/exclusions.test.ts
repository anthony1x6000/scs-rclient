import { describe, expect, it } from "vitest";
import {
  excludedFoldersFor,
  exclusionScopeKey,
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

  it("treats the legacy flat array as a clean slate", () => {
    expect(parseExcludedFoldersMap([".git", "node_modules"])).toEqual({});
  });

  it("rejects non-object garbage", () => {
    expect(parseExcludedFoldersMap("nope")).toEqual({});
    expect(parseExcludedFoldersMap(7)).toEqual({});
    expect(parseExcludedFoldersMap(null)).toEqual({});
    expect(parseExcludedFoldersMap(undefined)).toEqual({});
  });

  it("rejects maps with non-list values", () => {
    expect(parseExcludedFoldersMap({ css: ".git" })).toEqual({});
    expect(parseExcludedFoldersMap({ css: [".git"], js: 3 })).toEqual({});
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
