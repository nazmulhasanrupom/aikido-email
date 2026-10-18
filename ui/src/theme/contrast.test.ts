import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("./tokens.css", import.meta.url), "utf8");

/** The `--name: #hex` pairs of the first block whose selector matches. */
function block(selector: string): Record<string, string> {
  const start = css.indexOf(`${selector} {`);
  if (start < 0) throw new Error(`no block ${selector}`);
  const body = css.slice(css.indexOf("{", start) + 1, css.indexOf("}", start));
  const out: Record<string, string> = {};
  for (const m of body.matchAll(/--([a-z0-9-]+):\s*(#[0-9a-fA-F]{6})\s*;/g)) {
    out[m[1] as string] = m[2] as string;
  }
  return out;
}

function luminance(hex: string): number {
  const channel = (i: number) => {
    const c = Number.parseInt(hex.slice(1 + i * 2, 3 + i * 2), 16) / 255;
    return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(0) + 0.7152 * channel(1) + 0.0722 * channel(2);
}

function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x) as [number, number];
  return (hi + 0.05) / (lo + 0.05);
}

const themes = {
  light: block(":root"),
  dark: block(':root[data-theme="dark"]'),
  aikido: block(':root[data-theme="aikido"]'),
};
const AA = 4.5;
const grounds = ["bg", "surface", "surface-2"] as const;
const textColors = ["text", "text-muted", "accent", "success", "warning", "danger", "info", "ai"];

describe("theme tokens", () => {
  it("both themes define every token", () => {
    const names = Object.keys(themes.light).sort();
    expect(names).toContain("accent-soft");
    expect(Object.keys(themes.dark).sort()).toEqual(names);
    expect(Object.keys(themes.aikido).sort()).toEqual(names);
  });

  for (const [theme, tokens] of Object.entries(themes)) {
    for (const color of textColors) {
      for (const ground of grounds) {
        it(`${theme}: --${color} on --${ground} reaches AA`, () => {
          const ratio = contrast(tokens[color] as string, tokens[ground] as string);
          expect(ratio, `${ratio.toFixed(2)}:1`).toBeGreaterThanOrEqual(AA);
        });
      }
    }

    it(`${theme}: text on a primary button and on a selected row reaches AA`, () => {
      expect(
        contrast(tokens["accent-fg"] as string, tokens.accent as string),
      ).toBeGreaterThanOrEqual(AA);
      expect(
        contrast(tokens.text as string, tokens["accent-soft"] as string),
      ).toBeGreaterThanOrEqual(AA);
      expect(
        contrast(tokens["text-muted"] as string, tokens["accent-soft"] as string),
      ).toBeGreaterThanOrEqual(AA);
      expect(
        contrast(tokens.accent as string, tokens["accent-soft"] as string),
      ).toBeGreaterThanOrEqual(AA);
    });
  }

  it("the dark background is not pure black and the light one is not pure white", () => {
    expect(themes.dark.bg).not.toBe("#000000");
    expect(themes.light.bg).not.toBe("#ffffff");
  });
});
