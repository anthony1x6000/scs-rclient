import { describe, expect, it } from "vitest";
import { moveHighlight } from "./listNavigation";

describe("moveHighlight", () => {
  it("steps down and clamps at the last item", () => {
    expect(moveHighlight(0, 1, 3)).toBe(1);
    expect(moveHighlight(2, 1, 3)).toBe(2);
  });

  it("steps up and clamps at the first item", () => {
    expect(moveHighlight(1, -1, 3)).toBe(0);
    expect(moveHighlight(0, -1, 3)).toBe(0);
  });

  it("enters at the top when nothing is highlighted and moving down", () => {
    expect(moveHighlight(-1, 1, 3)).toBe(0);
  });

  it("enters at the bottom when nothing is highlighted and moving up", () => {
    expect(moveHighlight(-1, -1, 3)).toBe(2);
  });

  it("moves a page's worth of rows and clamps at both ends", () => {
    expect(moveHighlight(0, 5, 12)).toBe(5);
    expect(moveHighlight(10, 5, 12)).toBe(11);
    expect(moveHighlight(2, -5, 12)).toBe(0);
  });

  it("starts a page-down from nothing on the last row of the first page", () => {
    expect(moveHighlight(-1, 5, 12)).toBe(4);
    expect(moveHighlight(-1, 20, 12)).toBe(11);
  });

  it("starts a page-up from nothing one page above the end", () => {
    expect(moveHighlight(-1, -5, 12)).toBe(7);
  });

  it("returns -1 for an empty list", () => {
    expect(moveHighlight(-1, 1, 0)).toBe(-1);
    expect(moveHighlight(3, 1, 0)).toBe(-1);
  });

  it("keeps or clamps the current row when delta is zero", () => {
    expect(moveHighlight(4, 0, 6)).toBe(4);
    expect(moveHighlight(9, 0, 6)).toBe(5);
  });
});
