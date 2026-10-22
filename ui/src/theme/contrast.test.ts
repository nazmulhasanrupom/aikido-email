import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("./tokens.css", import.meta.url), "utf8");

/** The `--name: #hex` and `--name: var(--other)` pairs of the first block whose selector matches. */
function block(selector: string): Record<string, string> {
  const start = css.indexOf(`${selector} {`);
  if (start < 0) throw new Error(`no block ${selector}`);
  const body = css.slice(css.indexOf("{", start) + 1, css.indexOf("}", start));
  const out: Record<string, string> = {};
  for (const m of body.matchAll(/--([a-z0-9-]+):\s*(#[0-9a-fA-F]{6}|var\(--[a-z0-9-]+\))\s*;/g)) {
    out[m[1] as string] = m[2] as string;
  }
  return out;
}

/** The hex color of a token, following `var(--other)`. */
function hex(tokens: Record<string, string>, name: string): string {
  let value = tokens[name];
  for (let i = 0; value?.startsWith("var(") && i < 5; i++) value = tokens[value.slice(6, -1)];
  if (!value?.startsWith("#")) throw new Error(`--${name} has no color`);
  return value;
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

/** Generic: light, dark and Crimson (`data-theme="aikido"`). The Indigo and Aikido designs: light and dark. */
const themes = {
  light: block(":root"),
  dark: block(':root[data-theme="dark"]'),
  crimson: block(':root[data-theme="aikido"]'),
  "indigo design light": block(':root[data-design="indigo"]'),
  "indigo design dark": block(':root[data-design="indigo"][data-theme="dark"]'),
  "aikido design light": block(':root[data-design="aikido"]'),
  "aikido design dark": block(':root[data-design="aikido"][data-theme="dark"]'),
};
/** A dark block of a design keeps what it does not set from the light block. */
const indigo = {
  light: themes["indigo design light"],
  dark: { ...themes["indigo design light"], ...themes["indigo design dark"] },
};
const aikido = {
  light: themes["aikido design light"],
  dark: { ...themes["aikido design light"], ...themes["aikido design dark"] },
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

  // The Indigo design draws the menu on the left as an ink rail, in both themes.
  for (const [theme, t] of Object.entries(indigo)) {
    it(`indigo design ${theme}: the ink rail reads well`, () => {
      for (const ground of ["sidebar-bg", "sidebar-hover", "sidebar-active"]) {
        expect(contrast(hex(t, "sidebar-fg"), hex(t, ground))).toBeGreaterThanOrEqual(AA);
        expect(contrast(hex(t, "sidebar-muted"), hex(t, ground))).toBeGreaterThanOrEqual(AA);
      }
      expect(contrast(hex(t, "sidebar-active-fg"), hex(t, "sidebar-active"))).toBeGreaterThanOrEqual(AA);
      // an icon needs 3:1 (WCAG 1.4.11)
      expect(contrast(hex(t, "sidebar-icon-active"), hex(t, "sidebar-active"))).toBeGreaterThanOrEqual(3);
      expect(contrast(hex(t, "sidebar-badge-fg"), hex(t, "sidebar-badge"))).toBeGreaterThanOrEqual(AA);
    });
  }
  it("indigo design: the initials in the menu read well", () => {
    const t = indigo.light;
    expect(contrast(hex(t, "sidebar-avatar-fg"), hex(t, "sidebar-avatar"))).toBeGreaterThanOrEqual(AA);
  });

  // The Aikido design has the pages in an ink bar at the top, in both themes.
  for (const [theme, t] of Object.entries(aikido)) {
    it(`aikido design ${theme}: the top bar reads well`, () => {
      for (const ground of ["topnav-bg", "topnav-hover"]) {
        expect(contrast(hex(t, "topnav-fg"), hex(t, ground))).toBeGreaterThanOrEqual(AA);
        expect(contrast(hex(t, "topnav-muted"), hex(t, ground))).toBeGreaterThanOrEqual(AA);
      }
      // the New button: ink text on a light button
      expect(contrast(hex(t, "topnav-bg"), hex(t, "topnav-fg"))).toBeGreaterThanOrEqual(AA);
      // the line under the page you are on needs 3:1 (WCAG 1.4.11)
      expect(contrast(hex(t, "topnav-mark"), hex(t, "topnav-bg"))).toBeGreaterThanOrEqual(3);
      // the number of alerts on the bell, and of new replies on Inbox
      expect(contrast(hex(t, "topnav-mark-fg"), hex(t, "topnav-mark"))).toBeGreaterThanOrEqual(AA);
      expect(contrast(hex(t, "sidebar-badge-fg"), hex(t, "sidebar-badge"))).toBeGreaterThanOrEqual(AA);
      expect(contrast(hex(t, "sidebar-avatar-fg"), hex(t, "sidebar-avatar"))).toBeGreaterThanOrEqual(AA);
    });
  }

  it("the dark backgrounds are not pure black and the light ones are not pure white", () => {
    expect(themes.dark.bg).not.toBe("#000000");
    expect(themes.light.bg).not.toBe("#ffffff");
    for (const design of ["indigo", "aikido"]) {
      expect(themes[`${design} design dark` as keyof typeof themes].bg).not.toBe("#000000");
      expect(themes[`${design} design light` as keyof typeof themes].bg).not.toBe("#ffffff");
    }
  });
});
