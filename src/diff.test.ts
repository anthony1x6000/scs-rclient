import { describe, expect, it } from "vitest";
import {
  annotateWordDiffs,
  canDiffText,
  changedBlocks,
  diffLines,
  diffStats,
  diffWords,
  MAX_DIFF_LINES,
  splitLines,
  stepChangedBlock,
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

describe("diffWords", () => {
  it("highlights only the changed word on each side", () => {
    const diff = diffWords("the quick brown fox", "the quick red fox");
    expect(diff).not.toBeNull();
    expect(diff!.old.map((segment) => [segment.kind, segment.text])).toEqual([
      ["equal", "the quick "],
      ["removed", "brown"],
      ["equal", " fox"],
    ]);
    expect(diff!.new.map((segment) => [segment.kind, segment.text])).toEqual([
      ["equal", "the quick "],
      ["added", "red"],
      ["equal", " fox"],
    ]);
  });

  it("keeps punctuation and spacing when reconstructing either side", () => {
    const diff = diffWords("call(foo, bar)", "call(foo, baz)");
    expect(diff).not.toBeNull();
    expect(diff!.old.map((segment) => segment.text).join("")).toBe("call(foo, bar)");
    expect(diff!.new.map((segment) => segment.text).join("")).toBe("call(foo, baz)");
    expect(diff!.old.filter((segment) => segment.kind === "removed")).toEqual([
      { kind: "removed", text: "bar" },
    ]);
    expect(diff!.new.filter((segment) => segment.kind === "added")).toEqual([
      { kind: "added", text: "baz" },
    ]);
  });

  it("declines a rewrite with too little in common", () => {
    expect(diffWords("alpha beta", "gamma delta")).toBeNull();
  });

  it("ignores shared punctuation when judging similarity", () => {
    expect(diffWords("f(x)", "g(y)")).toBeNull();
    expect(diffWords("f(x)", "f(y)")).not.toBeNull();
  });

  it("declines a one-sided line", () => {
    expect(diffWords("", "new line")).toBeNull();
    expect(diffWords("old line", "")).toBeNull();
  });
});

describe("annotateWordDiffs", () => {
  it("attaches word segments to the paired removed and added rows", () => {
    const rows = annotateWordDiffs(
      diffLines("const color = red;\n", "const color = blue;\n"),
    );
    const removed = rows.find((row) => row.kind === "removed");
    const added = rows.find((row) => row.kind === "added");
    expect(removed?.segments).toEqual([
      { kind: "equal", text: "const color = " },
      { kind: "removed", text: "red" },
      { kind: "equal", text: ";" },
    ]);
    expect(added?.segments).toEqual([
      { kind: "equal", text: "const color = " },
      { kind: "added", text: "blue" },
      { kind: "equal", text: ";" },
    ]);
  });

  it("pairs rows in order inside a multi-line block", () => {
    const rows = annotateWordDiffs(
      diffLines("red apple\nblue sky\n", "red berry\nblue sea\n"),
    );
    const removed = rows.filter((row) => row.kind === "removed");
    expect(removed.map((row) => row.segments?.find((s) => s.kind === "removed")?.text)).toEqual([
      "apple",
      "sky",
    ]);
  });

  it("finds the similar row when the positional row is a rewrite", () => {
    const rows = annotateWordDiffs(
      diffLines("return foo(1);\n", "return baz(3);\nreturn foo(2);\n"),
    );
    const removed = rows.find((row) => row.kind === "removed");
    const [firstAdded, secondAdded] = rows.filter((row) => row.kind === "added");
    expect(removed?.segments?.find((segment) => segment.kind === "removed")?.text).toBe("1");
    expect(firstAdded?.segments).toBeUndefined();
    expect(secondAdded?.segments?.find((segment) => segment.kind === "added")?.text).toBe("2");
  });

  it("leaves unpaired insertions, deletions, and rewrites without word segments", () => {
    const insertion = annotateWordDiffs(diffLines("alpha\n", "alpha\nbeta\n"));
    expect(insertion.find((row) => row.kind === "added")?.segments).toBeUndefined();

    const rewrite = annotateWordDiffs(diffLines("alpha beta\n", "gamma delta\n"));
    expect(rewrite.every((row) => row.segments === undefined)).toBe(true);
  });
});

describe("changedBlocks", () => {
  it("groups adjacent added and removed rows into one block", () => {
    const rows = diffLines("a\nb\nc\nd\n", "a\nB\nC\nd\n");
    expect(changedBlocks(rows)).toEqual([{ start: 1, end: 5 }]);
  });

  it("splits blocks on context rows and ignores unchanged text", () => {
    const rows = diffLines("a\nb\nc\nd\ne\n", "a\nB\nc\nd\nE\n");
    expect(changedBlocks(rows)).toEqual([
      { start: 1, end: 3 },
      { start: 5, end: 7 },
    ]);
    expect(changedBlocks(diffLines("same\n", "same\n"))).toEqual([]);
  });
});

describe("stepChangedBlock", () => {
  it("wraps at both ends", () => {
    expect(stepChangedBlock(0, 3, 1)).toBe(1);
    expect(stepChangedBlock(2, 3, 1)).toBe(0);
    expect(stepChangedBlock(0, 3, -1)).toBe(2);
  });

  it("stays at -1 when there is nothing to jump to", () => {
    expect(stepChangedBlock(0, 0, 1)).toBe(-1);
  });
});
