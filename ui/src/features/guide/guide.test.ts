import { describe, expect, it } from "vitest";
import { ALL_IDS, initialOpen, setOpen } from "./open";
import { SECTIONS } from "./sections";

describe("the guide", () => {
  it("has every part, with a unique id", () => {
    expect(ALL_IDS).toEqual([
      "start",
      "google",
      "microsoft",
      "imap",
      "ai",
      "reoon",
      "api",
      "url",
      "tracking",
      "backup",
      "trouble",
    ]);
    expect(new Set(ALL_IDS).size).toBe(ALL_IDS.length);
  });

  it("gives each part a title and something to read", () => {
    for (const s of SECTIONS) {
      expect(s.title.length).toBeGreaterThan(0);
      expect(s.about.length).toBeGreaterThan(0);
      expect(s.steps.length + s.errors.length).toBeGreaterThan(0);
    }
  });

  it("walks through the Google and Microsoft keys", () => {
    const text = (id: string) => JSON.stringify(SECTIONS.find((s) => s.id === id));
    expect(text("google")).toContain("gmail.send");
    expect(text("google")).toContain("Web application");
    expect(text("microsoft")).toContain("Mail.Send");
    expect(SECTIONS.find((s) => s.id === "google")?.live).toBe("redirect-google");
    expect(SECTIONS.find((s) => s.id === "microsoft")?.live).toBe("redirect-microsoft");
  });

  it("opens the part named in the address", () => {
    expect([...initialOpen("google")]).toEqual(["google"]);
    expect(initialOpen("nope").size).toBe(0);
    expect(initialOpen().size).toBe(0);
  });

  it("opens and closes one part, and all of them", () => {
    const one = setOpen(new Set(), "imap", true);
    expect(one.has("imap")).toBe(true);
    expect(setOpen(one, "imap", false).size).toBe(0);
    expect(new Set(ALL_IDS).size).toBe(SECTIONS.length);
  });
});
