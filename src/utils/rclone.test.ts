import { describe, expect, it } from "vitest";
import { normalizeSubdir, resolveRemoteUrl, resolveLocalPath } from "./rclone";

describe("normalizeSubdir traversal scope", () => {
  it("rejects literal dot segments", () => {
    expect(() => normalizeSubdir("..")).toThrow();
    expect(() => normalizeSubdir("a/../b")).toThrow();
  });

  it("rejects percent-encoded dot segments the URL parser would collapse", () => {
    for (const bad of ["%2e%2e", ".%2e", "%2e.", "%2E%2E", "notes/%2e%2e", "%2e"]) {
      expect(() => normalizeSubdir(bad), `subdir ${bad} must be rejected`).toThrow();
    }
  });

  it("rejects control characters", () => {
    expect(() => normalizeSubdir("a\u0000b")).toThrow();
    expect(() => normalizeSubdir("a\nb")).toThrow();
  });

  it("rejects trailing-space/dot aliases of dot segments (Windows-style trimming)", () => {
    // Windows/IIS-family servers strip trailing ASCII spaces and dots from
    // each path component, so these spellings resolve to "." / ".." despite
    // passing an exact-match dot check.
    for (const bad of [".. ", ".. /secret", "notes/.. /x", "...", ". ", "notes/...", "..%20", "notes/..%20/x"]) {
      expect(() => normalizeSubdir(bad), `subdir ${bad} must be rejected`).toThrow();
    }
  });

  it("accepts ordinary folder names", () => {
    expect(normalizeSubdir("notes")).toBe("notes");
    expect(normalizeSubdir(" My Folder ")).toBe("My Folder");
    // A name that merely ends in a dot is not a dot-segment alias.
    expect(normalizeSubdir("report.")).toBe("report.");
    expect(normalizeSubdir("v1.2/notes")).toBe("v1.2/notes");
  });

  it("keeps encoded separators as literal names, not traversal", () => {
    // resolveRemoteUrl re-encodes the `%`, so an encoded separator reaches
    // the backend as a literal `%2f` inside one segment and cannot split
    // the path or pop a parent.
    expect(resolveRemoteUrl("https://h.example/dav", "a%2fb")).toBe(
      "https://h.example/dav/a%252fb",
    );
    expect(resolveRemoteUrl("https://h.example/dav", "a%5Cb")).toBe(
      "https://h.example/dav/a%255Cb",
    );
  });

  it("resolveRemoteUrl keeps the segment inside the base", () => {
    expect(resolveRemoteUrl("https://h.example/dav", "notes")).toBe("https://h.example/dav/notes");
  });
});

describe("resolveLocalPath containment", () => {
  it("stays under the mount directory", () => {
    expect(resolveLocalPath("/home/u/m", "a/b")).toBe("/home/u/m/a/b");
  });

  it("rejects subdirs that would escape the mount directory", () => {
    expect(() => resolveLocalPath("/home/u/m", "../outside")).toThrow();
    expect(() => resolveLocalPath("/home/u/m", "%2e%2e/outside")).toThrow();
    expect(() => resolveLocalPath("/home/u/m", "a/../../outside")).toThrow();
  });

  it("normalizes an absolute-looking subdir to a relative one", () => {
    expect(resolveLocalPath("/home/u/m", "/abs")).toBe("/home/u/m/abs");
  });
});
