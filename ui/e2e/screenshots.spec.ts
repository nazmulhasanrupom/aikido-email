import { expect, test } from "@playwright/test";
import { newPage } from "./helpers";

// Not a test: it saves pictures of the pages, for a look by eye. Set SCREENSHOTS_DIR to run it.
const dir = process.env.SCREENSHOTS_DIR;
test.skip(!dir, "set SCREENSHOTS_DIR to take screenshots");

for (const scheme of ["light", "dark"] as const) {
  test(`pictures: ${scheme}`, async ({ browser }) => {
    const { context, page } = await newPage(
      browser,
      { colorScheme: scheme, viewport: { width: 1280, height: 800 } },
      false,
    );
    await page.goto("/login");
    await expect(page.getByRole("heading", { name: "Log in" })).toBeVisible();
    await page.waitForTimeout(300);
    await page.screenshot({ path: `${dir}/login-${scheme}.png` });

    const { context: c2, page: p2 } = await newPage(browser, {
      colorScheme: scheme,
      viewport: { width: 1280, height: 800 },
    });
    await p2.goto("/w/default/campaigns");
    await expect(p2.getByRole("navigation", { name: "Pages" })).toBeVisible();
    await p2.screenshot({ path: `${dir}/shell-${scheme}.png` });
    await p2.getByRole("button", { name: /^Account menu/ }).click();
    await p2.waitForTimeout(300);
    await p2.screenshot({ path: `${dir}/menu-${scheme}.png` });
    await p2.keyboard.press("Escape");
    await p2.getByRole("button", { name: "Collapse the menu" }).click();
    await p2.screenshot({ path: `${dir}/slim-${scheme}.png` });
    await c2.close();
    await context.close();
  });
}

test("pictures: setup page and phone", async ({ browser }) => {
  // the setup page is closed once the owner exists, so only the phone view of the shell
  const { context, page } = await newPage(browser, { viewport: { width: 390, height: 800 } });
  await page.goto("/w/default/campaigns");
  await page.getByRole("button", { name: "Open menu" }).click();
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${dir}/phone-drawer.png` });
  await context.close();
});
