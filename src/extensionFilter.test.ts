import { describe, expect, it } from "vitest";
import {
  MAX_EXTENSIONS,
  formatExtensionCsv,
  normalizeExtensionEntry,
  normalizeExtensionList,
  parseExtensionCsv,
  resolveExtensionFilter,
  sanitizeExtensionMap,
  type ExtensionFilterState,
} from "./extensionFilter";

function state(over: Partial<ExtensionFilterState> = {}): ExtensionFilterState {
  return {
    perDirWhitelist: {},
    perDirBlacklist: {},
    globalWhitelist: [],
    globalBlacklist: [],
    ...over,
  };
}

describe("normalizeExtensionEntry", () => {
  it("accepts entries with or without a leading dot, any case, and padding", () => {
    expect(normalizeExtensionEntry(" .HTML ")).toBe("html");
    expect(normalizeExtensionEntry(".docx")).toBe("docx");
    expect(normalizeExtensionEntry("txt")).toBe("txt");
    expect(normalizeExtensionEntry("7Z")).toBe("7z");
  });

  it("rejects empty or invalid entries", () => {
    for (const bad of ["", "   ", "a b", "h/t", "h\\t", "tar.gz", "no-dash", "x".repeat(65)]) {
      expect(normalizeExtensionEntry(bad), `entry "${bad}" must be rejected`).toBeNull();
    }
  });
});

describe("parseExtensionCsv", () => {
  it("parses the placeholder prefill format", () => {
    expect(parseExtensionCsv(".html,.txt")).toEqual({ extensions: ["html", "txt"], invalid: [] });
  });

  it("tolerates spaces, casing, dots, duplicates, and empty tokens", () => {
    expect(parseExtensionCsv(" HTML ,.txt, html ,,   ")).toEqual({
      extensions: ["html", "txt"],
      invalid: [],
    });
  });

  it("collects invalid tokens instead of silently dropping them", () => {
    expect(parseExtensionCsv("html,not one,x/y,.ok")).toEqual({
      extensions: ["html", "ok"],
      invalid: ["not one", "x/y"],
    });
  });

  it("caps the kept list at MAX_EXTENSIONS while preserving order", () => {
    const csv = Array.from({ length: MAX_EXTENSIONS + 5 }, (_, i) => `.e${i}`).join(",");
    const kept = parseExtensionCsv(csv).extensions;
    expect(kept).toHaveLength(MAX_EXTENSIONS);
    expect(kept[0]).toBe("e0");
    expect(kept[MAX_EXTENSIONS - 1]).toBe(`e${MAX_EXTENSIONS - 1}`);
  });
});

describe("formatExtensionCsv", () => {
  it("joins a list back into CSV form", () => {
    expect(formatExtensionCsv(["html", "txt"])).toBe("html,txt");
    expect(formatExtensionCsv([])).toBe("");
  });
});

describe("normalizeExtensionList", () => {
  it("sanitizes stored values: strings only, invalid dropped, deduped, capped", () => {
    expect(
      normalizeExtensionList([".HTML", "html", " txt ", 42, null, "bad one", "x/y"]),
    ).toEqual(["html", "txt"]);
  });
});

describe("resolveExtensionFilter", () => {
  const wl = ["html"];
  const bl = ["bin"];

  it("returns null when nothing is set", () => {
    expect(resolveExtensionFilter("course/a", state())).toBeNull();
    expect(resolveExtensionFilter(null, state())).toBeNull();
  });

  it("treats empty lists as unset", () => {
    expect(
      resolveExtensionFilter("course/a", state({ perDirWhitelist: { "course/a": [] } })),
    ).toBeNull();
  });

  it("whitelist wins over blacklist within the same scope", () => {
    expect(
      resolveExtensionFilter(
        "course/a",
        state({ perDirWhitelist: { "course/a": wl }, perDirBlacklist: { "course/a": bl } }),
      ),
    ).toEqual({ mode: "whitelist", extensions: wl });
    expect(
      resolveExtensionFilter(null, state({ globalWhitelist: wl, globalBlacklist: bl })),
    ).toEqual({ mode: "whitelist", extensions: wl });
  });

  it("a per-dir filter replaces the global filter entirely", () => {
    expect(
      resolveExtensionFilter(
        "course/a",
        state({
          perDirBlacklist: { "course/a": bl },
          globalWhitelist: ["png"],
          globalBlacklist: ["zip"],
        }),
      ),
    ).toEqual({ mode: "blacklist", extensions: bl });
  });

  it("falls back to the global filter for dirs without their own filter", () => {
    expect(
      resolveExtensionFilter("course/b", state({ globalBlacklist: bl })),
    ).toEqual({ mode: "blacklist", extensions: bl });
    expect(
      resolveExtensionFilter(null, state({ globalWhitelist: wl })),
    ).toEqual({ mode: "whitelist", extensions: wl });
  });

  it("normalizes the subdir key before lookup", () => {
    expect(
      resolveExtensionFilter("/course/a/", state({ perDirWhitelist: { "course/a": wl } })),
    ).toEqual({ mode: "whitelist", extensions: wl });
  });
});

describe("sanitizeExtensionMap", () => {
  it("drops invalid keys and empty lists, keeps valid entries", () => {
    const sanitized = sanitizeExtensionMap({
      "course/a": [".HTML", "html"],
      "a/../b": ["html"],
      "course/c": [],
      "course/d": "not-a-list",
    });
    expect(Object.keys(sanitized)).toEqual(["course/a"]);
    expect(sanitized["course/a"]).toEqual(["html"]);
  });

  it("keeps a folder literally named __proto__ as an own key", () => {
    // JSON.parse produces an own "__proto__" property; a plain object literal would
    // not, so this mirrors what actually comes back from the settings store.
    const stored = JSON.parse('{"__proto__": [".html"]}');
    const sanitized = sanitizeExtensionMap(stored);
    expect(Object.prototype.hasOwnProperty.call(sanitized, "__proto__")).toBe(true);
    expect(sanitized["__proto__"]).toEqual(["html"]);
    expect(
      resolveExtensionFilter("__proto__", state({ perDirWhitelist: sanitized })),
    ).toEqual({ mode: "whitelist", extensions: ["html"] });
  });
});
