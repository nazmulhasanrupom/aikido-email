import { describe, expect, it } from "vitest";
import { safeNext } from "./next";

describe("safeNext", () => {
  it("keeps a path of the app", () => {
    expect(safeNext("/w/default/leads?tab=lists")).toBe("/w/default/leads?tab=lists");
  });
  it("drops anything that could leave the app", () => {
    for (const bad of [
      "https://evil.example",
      "//evil.example",
      "/\\evil.example",
      "javascript:alert(1)",
      "",
      5,
      null,
    ]) {
      expect(safeNext(bad)).toBeUndefined();
    }
  });
  it("does not loop back to the login or setup page", () => {
    expect(safeNext("/login?next=/x")).toBeUndefined();
    expect(safeNext("/setup")).toBeUndefined();
  });
});
