import { describe, expect, it } from "vitest";
import {
  barSegments,
  changeKind,
  countChangeFilters,
  countStatuses,
  filterChangeEntries,
  formatMtime,
  localEntries,
  remoteEntries,
  statusGlyph,
  statusLabel,
  uploadDiff,
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
  // `added` means the file exists only in the working copy, so only a local
  // row can show it; `removed` is the remote-only mirror image.
  const entries = [
    entry({ path: "new-local.txt", status: "added", remoteSize: null, remoteMtime: null }),
    entry({ path: "remote-only.txt", status: "removed", localSize: null, localMtime: null }),
    entry({ path: "changed.txt", status: "modified" }),
  ];

  it("keeps a new local file on the local side and a remote-only file on the remote side", () => {
    expect(localEntries(entries).map((e) => e.path)).toEqual([
      "new-local.txt",
      "changed.txt",
    ]);
    expect(remoteEntries(entries).map((e) => e.path)).toEqual([
      "remote-only.txt",
      "changed.txt",
    ]);
  });

  it("paints new local files green locally and remote-only files red remotely", () => {
    const counts = countStatuses(entries);
    expect(barSegments("local", counts).map((s) => [s.key, s.count, s.className])).toEqual([
      ["modified", 1, "bg-amber-400/80"],
      ["added", 1, "bg-emerald-400/80"],
    ]);
    expect(barSegments("remote", counts).map((s) => [s.key, s.count, s.className])).toEqual([
      ["modified", 1, "bg-amber-400/80"],
      ["removed", 1, "bg-red-400/80"],
    ]);
  });
});

describe("status presentation", () => {
  it("labels statuses by what the upload does with the file", () => {
    expect(statusLabel("added")).toBe("New · will upload");
    expect(statusLabel("removed")).toBe("Remote only · no local copy");
    expect(statusLabel("modified")).toBe("Modified");
  });

  it("keeps the green/red glyphs for add and remove", () => {
    expect(statusGlyph("added")).toBe("+");
    expect(statusGlyph("removed")).toBe("−");
    expect(statusGlyph("modified")).toBe("~");
  });
});

describe("uploadDiff", () => {
  it("treats the local copy as the new side so upload lines are green", () => {
    const rows = uploadDiff("kept\nlocal addition\n", "kept\nremote addition\n");
    expect(rows.filter((row) => row.kind === "added").map((row) => row.text)).toEqual([
      "local addition",
    ]);
    expect(rows.filter((row) => row.kind === "removed").map((row) => row.text)).toEqual([
      "remote addition",
    ]);
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

describe("change kind filtering", () => {
  const entries = [
    entry({ path: "added.txt", status: "added", localSize: null, localMtime: null }),
    entry({ path: "removed.txt", status: "removed", remoteSize: null, remoteMtime: null }),
    entry({ path: "grew.txt", status: "modified" }),
    entry({ path: "touched.txt", status: "modified", localSize: 10, remoteSize: 10 }),
  ];

  it("treats added, removed, and resized files as content changes", () => {
    expect(changeKind(entries[0]!)).toBe("content");
    expect(changeKind(entries[1]!)).toBe("content");
    expect(changeKind(entries[2]!)).toBe("content");
  });

  it("treats a timestamp-only edit as a metadata change", () => {
    expect(changeKind(entries[3]!)).toBe("metadata");
  });

  it("filters each list down to one bucket", () => {
    expect(filterChangeEntries(entries, "content").map((item) => item.path)).toEqual([
      "added.txt",
      "removed.txt",
      "grew.txt",
    ]);
    expect(filterChangeEntries(entries, "metadata").map((item) => item.path)).toEqual([
      "touched.txt",
    ]);
  });

  it("returns an unchanged copy for the all filter", () => {
    const all = filterChangeEntries(entries, "all");
    expect(all).toEqual(entries);
    expect(all).not.toBe(entries);
  });

  it("counts every bucket", () => {
    expect(countChangeFilters(entries)).toEqual({ all: 4, content: 3, metadata: 1 });
    expect(countChangeFilters([])).toEqual({ all: 0, content: 0, metadata: 0 });
  });
});
