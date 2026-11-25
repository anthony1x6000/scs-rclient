import { describe, expect, it } from "vitest";
import { createSnowflakes, isSnowSeason } from "./snow";

function at(month: number, day: number): Date {
  // Local noon avoids any timezone edge at the day boundary.
  return new Date(2026, month, day, 12, 0, 0);
}

function seeded(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state * 1664525 + 1013904223) >>> 0;
    return state / 4294967296;
  };
}

describe("isSnowSeason", () => {
  it("turns on November 20th, not the day before", () => {
    expect(isSnowSeason(at(10, 19))).toBe(false);
    expect(isSnowSeason(at(10, 20))).toBe(true);
  });

  it("stays on through December 31st", () => {
    expect(isSnowSeason(at(11, 1))).toBe(true);
    expect(isSnowSeason(at(11, 31))).toBe(true);
  });

  it("is off for the rest of the year", () => {
    expect(isSnowSeason(at(0, 1))).toBe(false);
    expect(isSnowSeason(at(9, 30))).toBe(false);
    expect(isSnowSeason(at(10, 1))).toBe(false);
  });
});

describe("createSnowflakes", () => {
  it("defaults to a sparse thirty flakes", () => {
    expect(createSnowflakes()).toHaveLength(30);
  });

  it("keeps every flake faint, slow, and inside its bounds", () => {
    for (const flake of createSnowflakes(60, seeded(7))) {
      expect(flake.left).toBeGreaterThanOrEqual(0);
      expect(flake.left).toBeLessThanOrEqual(100);
      expect(flake.top).toBeGreaterThanOrEqual(0);
      expect(flake.top).toBeLessThanOrEqual(100);
      expect(flake.opacity).toBeGreaterThanOrEqual(0.06);
      expect(flake.opacity).toBeLessThanOrEqual(0.12);
      expect(flake.size).toBeGreaterThan(0);
      expect(flake.duration).toBeGreaterThanOrEqual(22);
      expect(flake.duration).toBeLessThanOrEqual(40);
      // Negative delay starts each flake mid-fall instead of at the top.
      expect(flake.delay).toBeLessThanOrEqual(0);
      expect(flake.delay).toBeGreaterThanOrEqual(-flake.duration);
      expect(flake.sway).toBeGreaterThanOrEqual(8);
      expect(flake.sway).toBeLessThanOrEqual(26);
    }
  });

  it("is deterministic for a supplied random source", () => {
    expect(createSnowflakes(10, seeded(42))).toEqual(createSnowflakes(10, seeded(42)));
  });

  it("mixes crystal glyphs with soft dots", () => {
    const flakes = createSnowflakes(30, seeded(3));
    expect(flakes.some((flake) => flake.glyph)).toBe(true);
    expect(flakes.some((flake) => !flake.glyph)).toBe(true);
  });
});
