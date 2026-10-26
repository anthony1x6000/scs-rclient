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

  it("resolveRemoteUrl keeps the segment inside the base", () => {
    expect(resolveRemoteUrl("https://h.example/dav", "notes")).toBe("https://h.example/dav/notes");
  });
});

describe("resolveLocalPath containment", () => {
  it("stays under the mount directory", () => {
    expect(resolveLocalPath("/home/u/m", "a/b")).toBe("/home/u/m/a/b");
  });
});
