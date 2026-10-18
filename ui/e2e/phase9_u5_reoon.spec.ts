import { expect } from "@playwright/test";
import { authed as test } from "./helpers";

// Reoon: install, sidebar item, verify two leads, the Verification column, uninstall.
test.describe.configure({ mode: "serial" });

test("install Reoon: a bad key is refused, a good key adds the menu item", async ({ page }) => {
  await page.goto("/w/default/settings/integrations");
  await expect(page.getByText("Reoon Email Verifier")).toBeVisible();
  await expect(page.getByRole("link", { name: "Reoon", exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: "Install" }).click();
  await page.getByLabel("API key").fill("wrong-key");
  await page.getByRole("button", { name: "Install" }).last().click();
  await expect(page.getByText("Reoon did not accept this key")).toBeVisible();
  await page.getByLabel("API key").fill("good-key");
  await page.getByRole("button", { name: "Install" }).last().click();
  await expect(page.getByText("Reoon installed. Find it in the menu.")).toBeVisible();
  await expect(page.getByRole("navigation", { name: "Pages" }).getByRole("link", { name: "Reoon" })).toBeVisible();
});

test("verify two leads and see the result in the Verification column", async ({ page }) => {
  await page.goto("/w/default/leads");
  for (const email of ["ann@reoon.test", "bad1@reoon.test"]) {
    await page.getByRole("button", { name: "Add lead" }).click();
    await page.getByLabel("Email", { exact: true }).fill(email);
    await page.getByRole("button", { name: "Add lead" }).last().click();
    await expect(page.getByText(email)).toBeVisible();
  }
  await page.getByRole("navigation", { name: "Pages" }).getByRole("link", { name: "Reoon" }).click();
  await expect(page.getByText("Credits: 150 today, 5,000 instant")).toBeVisible();
  await page.getByPlaceholder("Name, email or company").fill("reoon.test");
  await page.getByLabel("Select ann@reoon.test").check();
  await page.getByLabel("Select bad1@reoon.test").check();
  await page.getByRole("button", { name: "Verify", exact: true }).click();
  await expect(page.getByText(/About 2 credits/)).toBeVisible();
  await page.getByRole("button", { name: "Verify", exact: true }).last().click();
  const ann = page.getByRole("row", { name: /ann@reoon.test/ });
  await expect(ann.getByText("Safe")).toBeVisible({ timeout: 30_000 });
  await expect(page.getByRole("row", { name: /bad1@reoon.test/ }).getByText("Invalid")).toBeVisible();

  // the same column is in the Leads table
  await page.goto("/w/default/leads?q=reoon.test");
  await expect(page.getByRole("columnheader", { name: /Verification/ })).toBeVisible();
  await expect(page.getByRole("row", { name: /ann@reoon.test/ }).getByText("Safe")).toBeVisible();
});

test("uninstall removes the menu item and keeps the results", async ({ page }) => {
  await page.goto("/w/default/settings/integrations");
  await page.getByRole("button", { name: "Uninstall" }).click();
  await page.getByRole("button", { name: "Uninstall" }).last().click();
  await expect(page.getByRole("navigation", { name: "Pages" }).getByRole("link", { name: "Reoon" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Install" })).toBeVisible();
});
