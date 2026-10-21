import { expect } from "@playwright/test";
import { authed as test, OWNER } from "./helpers";

// The two designs: Generic (the first look of the app) and Aikido (ink, paper and indigo).
// The choice changes the page at once, is saved in the account, and is kept after a reload.
test.describe.configure({ mode: "serial" });

test("the design changes at once, is kept after a reload, and can go back", async ({ page }) => {
  const design = () => page.evaluate(() => document.documentElement.getAttribute("data-design"));
  await page.goto("/w/default/campaigns");
  expect(await design()).toBe("generic");

  await page.getByRole("button", { name: `Account menu for ${OWNER.name}` }).click();
  await page.getByRole("menuitemradio", { name: "Aikido" }).click();
  await expect.poll(design).toBe("aikido");
  // the menu on the left is the ink rail of the Aikido design
  const aside = page.getByRole("complementary", { name: "Main menu" });
  await expect
    .poll(() => aside.evaluate((el) => getComputedStyle(el).backgroundColor))
    .toMatch(/^rgb\((20, 22, 27|10, 12, 16)\)$/);

  // saved on the account too
  await expect
    .poll(async () => {
      const me = await page.evaluate(async () => (await fetch("/v1/users/me")).json());
      return me.preferences.design;
    })
    .toBe("aikido");
  await page.reload();
  expect(await design()).toBe("aikido");

  // Settings > Account > Appearance shows both designs and goes back to Generic
  await page.goto("/w/default/settings/account");
  await expect(page.getByRole("heading", { name: "Appearance" })).toBeVisible();
  const generic = page.getByRole("button", { name: /^Generic/ });
  await expect(generic).toHaveAttribute("aria-pressed", "false");
  await generic.click();
  await expect.poll(design).toBe("generic");
  await expect(generic).toHaveAttribute("aria-pressed", "true");
});
