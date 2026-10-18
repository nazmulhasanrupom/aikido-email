import { describe, expect, it } from "vitest";
import { scoreEmail } from "./emailScore";

describe("scoreEmail", () => {
  it("counts words, links and questions and ignores variable names", () => {
    const s = scoreEmail("Hi {{first_name|there}}, see https://x.com now. Is this useful?", []);
    expect(s.words).toBe(8);
    expect(s.links).toBe(1);
    expect(s.questions).toBe(1);
  });
  it("finds spam words", () => {
    expect(scoreEmail("Act now and win. Is it ok?", ["act now", "winner"]).spamWords).toEqual(["act now"]);
  });
  it("gives a hint for a long email with no question", () => {
    const s = scoreEmail(`${"word ".repeat(140)}.`, []);
    expect(s.hints.some((h) => h.text.includes("Shorter emails"))).toBe(true);
    expect(s.hints.some((h) => h.text.includes("question"))).toBe(true);
  });
  it("says a good length is good", () => {
    const s = scoreEmail(`${"We help teams ship faster with less stress. ".repeat(8)}Is this useful?`, []);
    expect(s.hints.some((h) => h.tone === "ok")).toBe(true);
  });
});
