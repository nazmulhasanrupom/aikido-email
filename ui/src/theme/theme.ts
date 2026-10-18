import { useSyncExternalStore } from "react";
import { readStored, writeStored } from "@/lib/safe-storage";

export type ThemeChoice = "light" | "dark" | "aikido" | "system";
export type ResolvedTheme = "light" | "dark" | "aikido";

/** Keep the key in step with public/theme-init.js. */
export const THEME_KEY = "aikido.theme";

export function isChoice(value: unknown): value is ThemeChoice {
  return value === "light" || value === "dark" || value === "aikido" || value === "system";
}

export function readChoice(): ThemeChoice {
  const saved = readStored(THEME_KEY);
  return isChoice(saved) ? saved : "system";
}

function systemIsDark(): boolean {
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

export function resolve(choice: ThemeChoice): ResolvedTheme {
  if (choice === "system") return systemIsDark() ? "dark" : "light";
  return choice;
}

let current: ThemeChoice = readChoice();
const listeners = new Set<() => void>();

function paint() {
  const theme = resolve(current);
  const root = document.documentElement;
  root.setAttribute("data-theme", theme);
  root.style.colorScheme = theme === "light" ? "light" : "dark";
}

/** Cross-fade to the new theme where the browser can, and not when the system asks for less motion. */
function paintSmoothly() {
  const doc = document as Document & { startViewTransition?: (cb: () => void) => unknown };
  const still = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  if (doc.startViewTransition && !still) doc.startViewTransition(paint);
  else paint();
}

/** Set the theme now and remember it in this browser. */
export function applyChoice(choice: ThemeChoice): void {
  const changed = choice !== current;
  current = choice;
  writeStored(THEME_KEY, choice);
  if (changed) paintSmoothly();
  else paint();
  for (const l of listeners) l();
}

/** Follow the system setting while the choice is "system". Call once at start. */
export function watchSystemTheme(): void {
  const query = window.matchMedia("(prefers-color-scheme: dark)");
  query.addEventListener("change", () => {
    if (current === "system") {
      paint();
      for (const l of listeners) l();
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
