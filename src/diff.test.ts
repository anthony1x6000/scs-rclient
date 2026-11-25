import { describe, expect, it } from "vitest";
import {
  canDiffText,
  diffLines,
  diffStats,
  MAX_DIFF_LINES,
  splitLines,
} from "./diff";

describe("splitLines", () => {
  it("normalizes CRLF and CR", () => {
    expect(splitLines("a\r\nb\rc")).toEqual(["a", "b", "c"]);
  });

  it("drops one trailing newline but keeps interior blank lines", () => {
    expect(splitLines("a\n\nb\n")).toEqual(["a", "", "b"]);
  });

  it("treats empty text as no lines", () => {
    expect(splitLines("")).toEqual([]);
  });
});

describe("diffLines", () => {
  it("returns only context for identical text", () => {
    const rows = diffLines("a\nb\n", "a\nb\n");
    expect(rows.map((row) => [row.kind, row.oldLine, row.newLine, row.text])).toEqual([
      ["context", 1, 1, "a"],
      ["context", 2, 2, "b"],
    ]);
    expect(diffStats(rows)).toEqual({ added: 0, removed: 0 });
  });

  it("reports an inserted line against its new line number", () => {
    const rows = diffLines("a\nc\n", "a\nb\nc\n");
    expect(rows.map((row) => [row.kind, row.text])).toEqual([
      ["context", "a"],
      ["added", "b"],
      ["context", "c"],
    ]);
    expect(rows[1]?.newLine).toBe(2);
    expect(rows[1]?.oldLine).toBeNull();
  });

  it("reports a removed line against its old line number", () => {
    const rows = diffLines("a\nb\nc\n", "a\nc\n");
    expect(rows.map((row) => [row.kind, row.text])).toEqual([
      ["context", "a"],
      ["removed", "b"],
      ["context", "c"],
    ]);
    expect(rows[1]?.oldLine).toBe(2);
    expect(rows[1]?.newLine).toBeNull();
  });

  it("represents a full rewrite as removals then additions", () => {
    const rows = diffLines("old\n", "new\n");
    expect(diffStats(rows)).toEqual({ added: 1, removed: 1 });
  });

  it("diffs against an empty side for added and removed files", () => {
    expect(diffLines("", "brand new\n").map((row) => row.kind)).toEqual(["added"]);
    expect(diffLines("gone\n", "").map((row) => row.kind)).toEqual(["removed"]);
  });

  it("normalizes line endings before comparing", () => {
    const rows = diffLines("a\r\nb\r\n", "a\nb\n");
    expect(rows.every((row) => row.kind === "context")).toBe(true);
  });

  it("refuses files past the line cap", () => {
    const tooMany = Array.from({ length: MAX_DIFF_LINES + 1 }, (_, i) => `line ${i}`).join("\n");
    expect(canDiffText(tooMany, "x")).toBe(false);
    expect(() => diffLines(tooMany, "x")).toThrow(/diff limit/);
  });

  it("counts added and removed rows for the summary", () => {
    const rows = diffLines("a\nb\nc\n", "a\nB\nc\nd\n");
    expect(diffStats(rows)).toEqual({ added: 2, removed: 1 });
  });
});
