import { describe, expect, it } from "vitest";
import {
  addFolderFromInput,
  formatBulkSummary,
  normalizeBulkSelection,
  runBulkSequential,
  toggleBulkTick,
} from "./bulk";

describe("normalizeBulkSelection", () => {
  it("keeps ticked folders in list order, not tick order", () => {
    expect(normalizeBulkSelection(["css", "js", "img"], ["img", "css"])).toEqual(["css", "img"]);
  });

  it("drops ticks for folders no longer in the list", () => {
    expect(normalizeBulkSelection(["css", "js"], ["css", "gone"])).toEqual(["css"]);
  });

  it("deduplicates ticks", () => {
    expect(normalizeBulkSelection(["css"], ["css", "css"])).toEqual(["css"]);
  });
});

describe("toggleBulkTick", () => {
  it("ticks an unticked folder", () => {
    expect(toggleBulkTick(["css"], "js")).toEqual(["css", "js"]);
  });

  it("unticks a ticked folder", () => {
    expect(toggleBulkTick(["css", "js"], "css")).toEqual(["js"]);
  });
});

describe("addFolderFromInput", () => {
  it("appends a new folder to the list and ticks it", () => {
    const result = addFolderFromInput(["css"], ["css"], "js");
    expect(result).toEqual({ ok: true, items: ["css", "js"], ticks: ["css", "js"], added: "js" });
  });

  it("ticks an existing folder without duplicating the list entry", () => {
    const result = addFolderFromInput(["css", "js"], ["css"], "js");
    expect(result).toEqual({ ok: true, items: ["css", "js"], ticks: ["css", "js"], added: "js" });
  });

  it("normalizes backslashes and surrounding slashes before adding", () => {
    const result = addFolderFromInput(["css"], [], "/new\\site\\/");
    expect(result).toEqual({ ok: true, items: ["css", "new/site"], ticks: ["new/site"], added: "new/site" });
  });

  it("rejects traversal segments with an error and no state change", () => {
    const result = addFolderFromInput(["css"], ["css"], "../up");
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error).toMatch(/\.\./);
  });

  it("rejects whitespace-only input with an error", () => {
    const result = addFolderFromInput(["css"], [], "   ");
    expect(result.ok).toBe(false);
  });
});

describe("runBulkSequential", () => {
  it("runs every folder in order", async () => {
    const ran: string[] = [];
    const outcome = await runBulkSequential(["css", "js", "img"], async (folder) => {
      ran.push(folder);
    });
    expect(ran).toEqual(["css", "js", "img"]);
    expect(outcome).toEqual({ succeeded: ["css", "js", "img"], failed: [], skipped: [] });
  });

  it("continues after a failure and records which folders failed", async () => {
    const ran: string[] = [];
    const outcome = await runBulkSequential(["css", "bad", "img"], async (folder) => {
      ran.push(folder);
      if (folder === "bad") throw new Error("boom");
    });
    expect(ran).toEqual(["css", "bad", "img"]);
    expect(outcome).toEqual({ succeeded: ["css", "img"], failed: ["bad"], skipped: [] });
  });

  it("marks every remaining folder skipped once cancelled and runs none of them", async () => {
    const ran: string[] = [];
    let cancelled = false;
    const outcome = await runBulkSequential(
      ["css", "js", "img"],
      async (folder) => {
        ran.push(folder);
        if (folder === "css") cancelled = true; // cancel fires during the first run
      },
      () => cancelled,
    );
    expect(ran).toEqual(["css"]);
    expect(outcome).toEqual({ succeeded: ["css"], failed: [], skipped: ["js", "img"] });
  });
});

describe("formatBulkSummary", () => {
  it("reports only successes when nothing failed or was skipped", () => {
    expect(formatBulkSummary({ succeeded: ["css", "js"], failed: [], skipped: [] })).toBe(
      "Bulk finished: 2 ok.",
    );
  });

  it("lists failed folders by name", () => {
    expect(formatBulkSummary({ succeeded: ["css"], failed: ["js", "img"], skipped: [] })).toBe(
      "Bulk finished: 1 ok, 2 failed (js, img).",
    );
  });

  it("mentions skipped folders after a cancellation", () => {
    expect(formatBulkSummary({ succeeded: ["css"], failed: ["js"], skipped: ["img"] })).toBe(
      "Bulk finished: 1 ok, 1 failed (js), 1 skipped (img).",
    );
  });

  it("pluralizes a single folder correctly", () => {
    expect(formatBulkSummary({ succeeded: ["css"], failed: [], skipped: [] })).toBe("Bulk finished: 1 ok.");
  });
});
