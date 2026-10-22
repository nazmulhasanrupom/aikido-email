import { expect } from "@playwright/test";
import { authed as test, OWNER } from "./helpers";

// The three designs: Generic (the first look of the app), Indigo (the same layout in ink, paper and indigo)
// and Aikido (its own layout: the pages in a bar at the top, a Home page, section menus, a status bar).
// The choice changes the page at once, is saved in the account, and is kept after a reload.
test.describe.configure({ mode: "serial" });

test("the design changes at once, is kept after a reload, and can go back", async ({ page }) => {
  const design = () => page.evaluate(() => document.documentElement.getAttribute("data-design"));
  const accountMenu = page.getByRole("button", { name: `Account menu for ${OWNER.name}` });
  await page.goto("/w/default/campaigns");
  expect(await design()).toBe("generic");

  // Indigo: the same layout; the menu on the left is an ink rail
  await accountMenu.click();
  await page.getByRole("menuitemradio", { name: "Indigo" }).click();
  await expect.poll(design).toBe("indigo");
  const aside = page.getByRole("complementary", { name: "Main menu" });
  await expect
    .poll(() => aside.evaluate((el) => getComputedStyle(el).backgroundColor))
    .toMatch(/^rgb\((20, 22, 27|10, 12, 16)\)$/);

  // Aikido: no menu on the left; the pages are in the top bar and a status bar is at the bottom
  await accountMenu.click();
  await page.getByRole("menuitemradio", { name: "Aikido" }).click();
  await expect.poll(design).toBe("aikido");
  await expect(aside).toHaveCount(0);
  await expect(
    page.getByRole("navigation", { name: "Pages" }).getByRole("link", { name: "Campaigns" }),
  ).toHaveAttribute("aria-current", "page");
  await expect(page.locator('footer[aria-label="Status"]')).toBeVisible();

  // saved on the account too
  await expect
    .poll(async () => {
      const me = await page.evaluate(async () => (await fetch("/v1/users/me")).json());
      return me.preferences.design;
    })
    .toBe("aikido");
  await page.reload();
  expect(await design()).toBe("aikido");

  // the Aikido design opens on Home
  await page.goto("/w/default");
  await expect(page).toHaveURL(/\/w\/default$/);
  await expect(page.getByRole("region", { name: "The last 7 days" })).toBeVisible();

  // Settings > Account > Appearance shows the three designs and goes back to Generic
  await page.goto("/w/default/settings/account");
  await expect(page.getByRole("heading", { name: "Appearance" })).toBeVisible();
  for (const name of ["Indigo", "Aikido"])
    await expect(page.getByRole("button", { name: new RegExp(`^${name}`) })).toBeVisible();
  const generic = page.getByRole("button", { name: /^Generic/ });
  await expect(generic).toHaveAttribute("aria-pressed", "false");
  await generic.click();
  await expect.poll(design).toBe("generic");
  await expect(generic).toHaveAttribute("aria-pressed", "true");

  // the Generic design has no Home page: it opens on the campaigns
  await page.goto("/w/default");
  await expect(page).toHaveURL(/\/w\/default\/campaigns$/);
});
