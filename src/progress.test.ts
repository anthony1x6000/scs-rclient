import { describe, expect, it } from "vitest";
import {
  formatBytes,
  percentLabel,
  phaseLabel,
  progressDetail,
  progressRatio,
  type WebdavProgress,
} from "./progress";

function progress(overrides: Partial<WebdavProgress> = {}): WebdavProgress {
  return {
    subdir: "assets",
    action: "get",
    phase: "download",
    completed: 0,
    total: 0,
    bytesDone: 0,
    bytesTotal: 0,
    file: null,
    ...overrides,
  };
}

describe("formatBytes", () => {
  it("formats zero and bytes without decimals", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
  });

  it("scales through KB, MB, and GB", () => {
    expect(formatBytes(1024)).toBe("1.0 KB");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(1024 * 1024)).toBe("1.0 MB");
    expect(formatBytes(3.25 * 1024 * 1024 * 1024)).toBe("3.3 GB");
  });

  it("drops decimals at three digits so the label stays short", () => {
    expect(formatBytes(100 * 1024)).toBe("100 KB");
  });

  it("never renders negative or non-finite values", () => {
    expect(formatBytes(-5)).toBe("0 B");
    expect(formatBytes(Number.NaN)).toBe("0 B");
  });
});

describe("progressRatio", () => {
  it("is null before the backend knows a total", () => {
    expect(progressRatio(progress())).toBeNull();
  });

  it("uses the file count once transfer work starts", () => {
    expect(progressRatio(progress({ total: 10, completed: 4 }))).toBeCloseTo(0.4);
  });

  it("uses bytes so a single large download moves mid-file", () => {
    expect(
      progressRatio(
        progress({ total: 1, completed: 0, bytesTotal: 1000, bytesDone: 250 }),
      ),
    ).toBeCloseTo(0.25);
  });

  it("takes the larger of the two ratios and clamps to 1", () => {
    expect(
      progressRatio(
        progress({ total: 10, completed: 1, bytesTotal: 100, bytesDone: 90 }),
      ),
    ).toBeCloseTo(0.9);
    expect(
      progressRatio(progress({ total: 2, completed: 2, bytesDone: 5, bytesTotal: 1 })),
    ).toBe(1);
  });
});

describe("percentLabel", () => {
  it("shows rounding of the ratio and an ellipsis while indeterminate", () => {
    expect(percentLabel(progress())).toBe("…");
    expect(percentLabel(progress({ total: 3, completed: 1 }))).toBe("33%");
  });
});

describe("phaseLabel", () => {
  it("maps backend phases to transfer verbs", () => {
    expect(phaseLabel(progress({ phase: "download" }))).toBe("Downloading");
    expect(phaseLabel(progress({ phase: "upload" }))).toBe("Uploading");
    expect(phaseLabel(progress({ phase: "scan" }))).toBe("Scanning");
  });

  it("falls back to an unknown phase instead of rendering nothing", () => {
    expect(phaseLabel(progress({ phase: "custom" }))).toBe("custom");
  });
});

describe("progressDetail", () => {
  it("combines files and bytes when both are known", () => {
    expect(
      progressDetail(
        progress({ total: 10, completed: 4, bytesTotal: 2048, bytesDone: 1024 }),
      ),
    ).toBe("4/10 files · 1.0 KB / 2.0 KB");
  });

  it("clamps the file count so a stale event never prints 11/10", () => {
    expect(progressDetail(progress({ total: 10, completed: 11 }))).toBe("10/10 files");
  });

  it("omits unknown dimensions", () => {
    expect(progressDetail(progress())).toBe("");
    expect(progressDetail(progress({ total: 3 }))).toBe("0/3 files");
  });
});
