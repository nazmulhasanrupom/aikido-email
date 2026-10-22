// Runs before the page is drawn, so there is no flash of the wrong look.
// Keep in step with src/theme/theme.ts. The saved choices are in localStorage (`aikido.theme`, `aikido.design`).
(() => {
  let choice = "system";
  let design = "generic";
  try {
    const saved = localStorage.getItem("aikido.theme");
    if (saved === "light" || saved === "dark" || saved === "aikido" || saved === "system")
      choice = saved;
    const look = localStorage.getItem("aikido.design");
    if (look === "generic" || look === "indigo" || look === "aikido") design = look;
  } catch {
    // no storage: use the system setting and the Generic design
  }
  const systemDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
  // the deep red theme belongs to the Generic design: the other designs show it as dark
  let theme = choice === "system" ? (systemDark ? "dark" : "light") : choice;
  if (theme === "aikido" && design !== "generic") theme = "dark";
  const root = document.documentElement;
  root.setAttribute("data-theme", theme);
  root.setAttribute("data-design", design);
  root.style.colorScheme = theme === "light" ? "light" : "dark";
})();
