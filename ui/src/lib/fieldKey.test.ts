import { describe, expect, it } from "vitest";
import { guessKind, keyFromName, keyOrFallback } from "./fieldKey";

describe("keyFromName", () => {
  it("makes a snake_case key", () => {
    expect(keyFromName("Company Size")).toBe("company_size");
    expect(keyFromName("Pain-point (main)")).toBe("pain_point_main");
  });
  it("starts with a letter and stops at 40", () => {
    expect(keyFromName("1st contact")).toBe("st_contact");
    expect(keyFromName("a".repeat(60))).toHaveLength(40);
  });
  it("gives field_n for a name with no Latin letters", () => {
    expect(keyFromName("কোম্পানি")).toBe("");
    expect(keyOrFallback("কোম্পানি", 2)).toBe("field_2");
  });
});

describe("guessKind", () => {
  it("guesses from samples", () => {
    expect(guessKind(["10", "25"])).toBe("number");
    expect(guessKind(["https://a.com", "www.b.com"])).toBe("url");
    expect(guessKind(["2026-01-02"])).toBe("date");
    expect(guessKind(["x", "1"])).toBe("text");
  });
});
