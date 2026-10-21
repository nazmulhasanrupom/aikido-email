import { useSyncExternalStore } from "react";
import { readStored, writeStored } from "@/lib/safe-storage";

/**
 * Two settings decide the look:
 * - the design: `generic` (the first look of the app) or `aikido` (ink, paper and indigo, round shapes);
 * - the theme: light, dark or system. The Generic design also has `aikido`, the deep red theme ("Crimson").
 * Both are saved in this browser and in the account, and `public/theme-init.js` paints them before the first frame.
 */
export type ThemeChoice = "light" | "dark" | "aikido" | "system";
export type ResolvedTheme = "light" | "dark" | "aikido";
export type DesignChoice = "generic" | "aikido";

/** Keep the keys in step with public/theme-init.js. */
export const THEME_KEY = "aikido.theme";
export const DESIGN_KEY = "aikido.design";

export function isChoice(value: unknown): value is ThemeChoice {
  return value === "light" || value === "dark" || value === "aikido" || value === "system";
}

export function isDesign(value: unknown): value is DesignChoice {
  return value === "generic" || value === "aikido";
}

export function readChoice(): ThemeChoice {
  const saved = readStored(THEME_KEY);
  return isChoice(saved) ? saved : "system";
}

export function readDesign(): DesignChoice {
  const saved = readStored(DESIGN_KEY);
  return isDesign(saved) ? saved : "generic";
}

function systemIsDark(): boolean {
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

let current: ThemeChoice = readChoice();
let design: DesignChoice = readDesign();
const listeners = new Set<() => void>();

/** The theme to paint. The deep red theme belongs to the Generic design: the Aikido design shows it as dark. */
export function resolve(choice: ThemeChoice, d: DesignChoice = design): ResolvedTheme {
  if (choice === "system") return systemIsDark() ? "dark" : "light";
  if (choice === "aikido" && d === "aikido") return "dark";
  return choice;
}

/** The choice to show as picked in a menu: the red theme reads as Dark in the Aikido design. */
export function shownChoice(choice: ThemeChoice, d: DesignChoice): ThemeChoice {
  return choice === "aikido" && d === "aikido" ? "dark" : choice;
}

/** The themes each design offers, in menu order. */
export function themeOptions(d: DesignChoice): { value: ThemeChoice; label: string }[] {
  const base: { value: ThemeChoice; label: string }[] = [
    { value: "light", label: "Light" },
    { value: "dark", label: "Dark" },
  ];
  if (d === "generic") base.push({ value: "aikido", label: "Crimson" });
  base.push({ value: "system", label: "System" });
  return base;
}

function paint() {
  const theme = resolve(current);
  const root = document.documentElement;
  root.setAttribute("data-theme", theme);
  root.setAttribute("data-design", design);
  root.style.colorScheme = theme === "light" ? "light" : "dark";
}

/** Cross-fade to the new look where the browser can, and not when the system asks for less motion. */
function paintSmoothly() {
  const doc = document as Document & { startViewTransition?: (cb: () => void) => unknown };
  const still = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  if (doc.startViewTransition && !still) doc.startViewTransition(paint);
  else paint();
}

function changed() {
  for (const l of listeners) l();
}

/** Set the theme now and remember it in this browser. */
export function applyChoice(choice: ThemeChoice): void {
  const moved = choice !== current;
  current = choice;
  writeStored(THEME_KEY, choice);
  if (moved) paintSmoothly();
  else paint();
  changed();
}

/** Set the design now and remember it in this browser. */
export function applyDesign(next: DesignChoice): void {
  const moved = next !== design;
  design = next;
  writeStored(DESIGN_KEY, next);
  if (moved) paintSmoothly();
  else paint();
  changed();
}

/** Follow the system setting while the choice is "system". Call once at start. */
export function watchSystemTheme(): void {
  const query = window.matchMedia("(prefers-color-scheme: dark)");
  query.addEventListener("change", () => {
    if (current === "system") {
      paint();
      changed();
    }
  });
  paint();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useThemeChoice(): ThemeChoice {
  return useSyncExternalStore(subscribe, () => current);
}

export function useResolvedTheme(): ResolvedTheme {
  return useSyncExternalStore(subscribe, () => resolve(current));
}

export function useDesign(): DesignChoice {
  return useSyncExternalStore(subscribe, () => design);
}
