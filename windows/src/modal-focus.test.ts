import { describe, expect, it } from "vitest";
import { focusBoundaryIndex } from "./modal-focus";

describe("dialog focus boundaries", () => {
  it("wraps both ends instead of moving focus outside the dialog", () => {
    expect(focusBoundaryIndex(3, 2, false)).toBe(0);
    expect(focusBoundaryIndex(3, 0, true)).toBe(2);
    expect(focusBoundaryIndex(3, 1, false)).toBeNull();
    expect(focusBoundaryIndex(3, 1, true)).toBeNull();
  });

  it("handles missing focus and single or no available controls", () => {
    expect(focusBoundaryIndex(3, -1, false)).toBe(0);
    expect(focusBoundaryIndex(3, -1, true)).toBe(2);
    expect(focusBoundaryIndex(1, 0, false)).toBe(0);
    expect(focusBoundaryIndex(0, -1, false)).toBeNull();
  });
});
