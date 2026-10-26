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

  it("accepts ordinary folder names", () => {
    expect(normalizeSubdir("notes")).toBe("notes");
    expect(normalizeSubdir(" My Folder ")).toBe("My Folder");
  });

  it("keeps encoded separators as literal names, not traversal", () => {
    // resolveRemoteUrl re-encodes the `%`, so an encoded separator reaches
    // the backend as a literal `%2f` inside one segment and cannot split
    // the path or pop a parent.
    expect(resolveRemoteUrl("https://h.example/dav", "a%2fb")).toBe(
      "https://h.example/dav/a%252fb",
    );
    expect(resolveRemoteUrl("https://h.example/dav", "a%5Cb")).toBe(
      "https://h.example/dav/a%255cb",
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
});
