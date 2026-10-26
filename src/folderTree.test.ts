import { describe, expect, it } from "vitest";
import {
  folderDisplayName,
  formatSize,
  groupFoldersByParent,
  parentOf,
  rootFolder,
  type RemoteFolder,
} from "./folderTree";

const f = (path: string, size = 0): RemoteFolder => ({ path, size });

describe("parentOf", () => {
  it("returns the empty root for top-level folders", () => {
    expect(parentOf("assets")).toBe("");
  });

  it("strips the last segment for nested folders", () => {
    expect(parentOf("assets/icons")).toBe("assets");
    expect(parentOf("a/b/c")).toBe("a/b");
  });
});

describe("groupFoldersByParent", () => {
  it("groups children under their parent path", () => {
    const groups = groupFoldersByParent([f(""), f("a"), f("a/b"), f("a/c"), f("z")]);
    expect([...groups.keys()].sort()).toEqual(["", "a"]);
    expect(groups.get("")).toEqual([f("a"), f("z")]);
    expect(groups.get("a")).toEqual([f("a/b"), f("a/c")]);
  });

  it("never lists the root entry as a child", () => {
    const groups = groupFoldersByParent([f(""), f("a")]);
    expect(groups.get("")).toEqual([f("a")]);
  });

  it("sorts each group by display name", () => {
    const groups = groupFoldersByParent([f("zeta"), f("alpha"), f("mid/z")]);
    expect(groups.get("")).toEqual([f("alpha"), f("mid/z"), f("zeta")]);
  });

  it("sorts siblings with nested names after their shared prefix naturally", () => {
    const groups = groupFoldersByParent([f("a"), f("a-b"), f("a/b")]);
    expect(groups.get("")).toEqual([f("a"), f("a-b"), f("a/b")]);
  });

  it("returns an empty map for an empty listing", () => {
    expect(groupFoldersByParent([]).size).toBe(0);
  });
});

describe("folderDisplayName", () => {
  it("returns the last path segment", () => {
    expect(folderDisplayName("assets/icons")).toBe("icons");
    expect(folderDisplayName("assets")).toBe("assets");
  });
});

describe("rootFolder", () => {
  it("synthesizes an empty root when the listing has none", () => {
    expect(rootFolder([])).toEqual(f("", 0));
  });

  it("uses the root entry the listing provided", () => {
    expect(rootFolder([f("", 18), f("a", 18)])).toEqual(f("", 18));
  });
});

describe("formatSize", () => {
  it("formats byte counts below 1 KiB plainly", () => {
    expect(formatSize(0)).toBe("0 B");
    expect(formatSize(512)).toBe("512 B");
    expect(formatSize(1023)).toBe("1023 B");
  });

  it("formats kibibytes with one decimal below ten", () => {
    expect(formatSize(1024)).toBe("1.0 KB");
    expect(formatSize(1536)).toBe("1.5 KB");
  });

  it("drops the decimal from ten units upward", () => {
    expect(formatSize(10 * 1024)).toBe("10 KB");
    expect(formatSize(10 * 1024 * 1024)).toBe("10 MB");
  });

  it("walks up to the largest fitting unit and stops at TB", () => {
    expect(formatSize(2 * 1024 ** 3)).toBe("2.0 GB");
    expect(formatSize(5 * 1024 ** 4)).toBe("5.0 TB");
    expect(formatSize(7 * 1024 ** 5)).toBe("7168 TB");
  });
});
