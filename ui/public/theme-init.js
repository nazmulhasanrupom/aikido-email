// Runs before the page is drawn, so there is no flash of the wrong theme.
// Keep in step with src/theme/theme.ts. The saved choice is in localStorage (`aikido.theme`).
(() => {
  let choice = "system";
  try {
    const saved = localStorage.getItem("aikido.theme");
    if (saved === "light" || saved === "dark" || saved === "system") choice = saved;
  } catch {
    // no storage: use the system setting
  }
  const dark =
    choice === "dark" ||
    (choice === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  const root = document.documentElement;
  root.setAttribute("data-theme", dark ? "dark" : "light");
  root.style.colorScheme = dark ? "dark" : "light";
})();
