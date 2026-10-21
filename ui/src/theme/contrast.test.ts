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

/** Generic: light, dark and Crimson (`data-theme="aikido"`). Aikido design: light and dark. */
const themes = {
  light: block(":root"),
  dark: block(':root[data-theme="dark"]'),
  crimson: block(':root[data-theme="aikido"]'),
  "aikido design light": block(':root[data-design="aikido"]'),
  "aikido design dark": block(':root[data-design="aikido"][data-theme="dark"]'),
};
const AA = 4.5;
const grounds = ["bg", "surface", "surface-2"] as const;
const textColors = ["text", "text-muted", "accent", "success", "warning", "danger", "info", "ai"];
/** Every theme sets these colors itself. */
const palette = [...grounds, "border", ...textColors, "accent-fg", "accent-soft"].sort();

describe("theme tokens", () => {
  it("every theme defines every palette color", () => {
    for (const [name, tokens] of Object.entries(themes)) {
      for (const color of palette) expect(tokens[color], `${name}: --${color}`).toMatch(/^#/);
    }
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

  // The Aikido design draws the menu on the left as an ink rail, in both themes.
  for (const theme of ["aikido design light", "aikido design dark"] as const) {
    const t = themes[theme];
    it(`${theme}: the ink rail reads well`, () => {
      for (const ground of ["sidebar-bg", "sidebar-hover", "sidebar-active"]) {
        expect(contrast(t["sidebar-fg"] as string, t[ground] as string)).toBeGreaterThanOrEqual(AA);
        expect(contrast(t["sidebar-muted"] as string, t[ground] as string)).toBeGreaterThanOrEqual(AA);
      }
      expect(contrast(t["sidebar-active-fg"] as string, t["sidebar-active"] as string)).toBeGreaterThanOrEqual(AA);
      // an icon needs 3:1 (WCAG 1.4.11)
      expect(contrast(t["sidebar-icon-active"] as string, t["sidebar-active"] as string)).toBeGreaterThanOrEqual(3);
      expect(contrast(t["sidebar-badge-fg"] as string, t["sidebar-badge"] as string)).toBeGreaterThanOrEqual(AA);
    });
  }
  it("aikido design: the initials in the menu read well", () => {
    const t = themes["aikido design light"];
    expect(contrast(t["sidebar-avatar-fg"] as string, t["sidebar-avatar"] as string)).toBeGreaterThanOrEqual(AA);
  });

  it("the dark backgrounds are not pure black and the light ones are not pure white", () => {
    expect(themes.dark.bg).not.toBe("#000000");
    expect(themes.light.bg).not.toBe("#ffffff");
    expect(themes["aikido design dark"].bg).not.toBe("#000000");
    expect(themes["aikido design light"].bg).not.toBe("#ffffff");
  });
});
