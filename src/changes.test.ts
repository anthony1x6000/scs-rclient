import { describe, expect, it } from "vitest";
import {
  barSegments,
  countStatuses,
  formatMtime,
  localEntries,
  remoteEntries,
  statusGlyph,
  statusLabel,
  type ChangeEntry,
} from "./changes";

function entry(overrides: Partial<ChangeEntry> = {}): ChangeEntry {
  return {
    path: "file.txt",
    status: "modified",
    localSize: 10,
    localMtime: 1_700_000_000,
    remoteSize: 12,
    remoteMtime: 1_700_000_100,
    ...overrides,
  };
}

describe("countStatuses", () => {
  it("counts every status", () => {
    const entries = [
      entry({ status: "added", localSize: null, localMtime: null }),
      entry({ status: "removed", remoteSize: null, remoteMtime: null }),
      entry({ status: "modified" }),
      entry({ path: "second.txt", status: "modified" }),
    ];
    expect(countStatuses(entries)).toEqual({ added: 1, removed: 1, modified: 2 });
  });

  it("returns zeroes for an empty review", () => {
    expect(countStatuses([])).toEqual({ added: 0, removed: 0, modified: 0 });
  });
});

describe("side bars", () => {
  const entries = [
    entry({ path: "added.txt", status: "added", localSize: null, localMtime: null }),
    entry({ path: "removed.txt", status: "removed", remoteSize: null, remoteMtime: null }),
    entry({ path: "changed.txt", status: "modified" }),
  ];

  it("splits modified files onto both sides and single-sided files to one", () => {
    expect(localEntries(entries).map((e) => e.path)).toEqual([
      "removed.txt",
      "changed.txt",
    ]);
    expect(remoteEntries(entries).map((e) => e.path)).toEqual([
      "added.txt",
      "changed.txt",
    ]);
  });

  it("builds local and remote bar segments in display order", () => {
    const counts = countStatuses(entries);
    expect(barSegments("local", counts).map((s) => [s.key, s.count])).toEqual([
      ["modified", 1],
      ["removed", 1],
    ]);
    expect(barSegments("remote", counts).map((s) => [s.key, s.count])).toEqual([
      ["modified", 1],
      ["added", 1],
    ]);
  });
});

describe("status presentation", () => {
  it("labels statuses from the local working-copy perspective", () => {
    expect(statusLabel("added")).toBe("Added remotely");
    expect(statusLabel("removed")).toBe("Removed remotely");
    expect(statusLabel("modified")).toBe("Modified");
    expect(statusGlyph("added")).toBe("+");
    expect(statusGlyph("removed")).toBe("−");
    expect(statusGlyph("modified")).toBe("~");
  });
});

describe("formatMtime", () => {
  it("renders the local timestamp as YYYY-MM-DD HH:mm", () => {
    const local = new Date(2026, 10, 25, 16, 5).getTime() / 1000;
    expect(formatMtime(local)).toBe("2026-11-25 16:05");
  });

  it("handles unknown timestamps", () => {
    expect(formatMtime(null)).toBe("unknown");
  });
});
