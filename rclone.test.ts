import { describe, it, expect, vi, beforeEach } from "vitest";
import { mockIPC } from "@tauri-apps/api/mocks";
import { detectRclone, createRcloneCommand, ensureRcloneDetected } from "./src/utils/rclone";

describe("rclone sidecar detection", () => {
  beforeEach(() => {
    vi.resetModules();
  });

  it("should prioritize the sidecar if it executes successfully", async () => {
    // Mock the IPC to simulate the sidecar returning a success exit code
    mockIPC(async (cmd, args) => {
      if (cmd === 'plugin:shell|execute') {
        return {
          code: 0,
          stdout: "rclone v1.65.2",
          stderr: ""
        };
      }
    });

    // We can spy on console.log if we want to verify the output
    const consoleSpy = vi.spyOn(console, "log");

    await detectRclone();
    
    expect(consoleSpy).toHaveBeenCalledWith("Using packaged rclone sidecar.");
    
    // Test createRcloneCommand routing
    const cmd = await createRcloneCommand(["--version"]);
    // Since we can't easily introspect the internal state of Command, 
    // we just know it should have created a sidecar command without throwing.
    expect(cmd).toBeDefined();
  });
});
