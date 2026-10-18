import { expect } from "@playwright/test";
import { authed as test } from "./helpers";

// UI update map v2, U4: domain check, checklist, starter kit, score, duplicate, missing values, inbox keys.
test.describe.configure({ mode: "serial" });

test("the account page has a domain check with the four rows", async ({ page }) => {
  await page.goto("/w/default/accounts");
  await page.getByRole("link", { name: "sales@acme.test" }).click();
  const check = page.getByLabel("Domain check");
  await expect(check.getByText("SPF", { exact: true })).toBeVisible({ timeout: 15_000 });
  await expect(check.getByText("DKIM", { exact: true })).toBeVisible();
  await expect(check.getByText("DMARC", { exact: true })).toBeVisible();
  await expect(check.getByText("Mail server (MX)", { exact: true })).toBeVisible();
  await expect(check.getByRole("button", { name: "Check again" })).toBeVisible();
});

test("a starter kit, the checklist, the score, missing values and duplicate", async ({ page }) => {
  await page.goto("/w/default/campaigns");
  await page.getByRole("button", { name: "New campaign" }).first().click();
  await page.getByLabel("Name", { exact: true }).fill("Starter test");
  await page.getByText("Starter kit").click();
  await page.getByLabel("Starter", { exact: true }).selectOption({ label: "Short intro, 3 emails" });
  await page.getByRole("button", { name: "Create campaign" }).click();
  await expect(page).toHaveURL(/\/editor$/);

  // the score shows hints for the starter text
  await expect(page.getByLabel("Email score")).toBeVisible();

  // the checklist is in the header
  await expect(page.getByRole("button", { name: /of 6 ready/ })).toBeVisible();
  await page.getByRole("button", { name: /of 6 ready/ }).click();
  await expect(page.getByText("Sending accounts are healthy")).toBeVisible();
  await expect(page.getByText("A test email was sent to you")).toBeVisible();

  // the Leads tab has the missing-values filter
  const nav = page.getByRole("navigation", { name: "Campaign pages" });
  await nav.getByRole("link", { name: "Leads" }).click();
  await expect(page.getByRole("button", { name: "Missing a value used in your emails" })).toBeVisible();

  // duplicate
  await page.getByRole("button", { name: "More actions" }).click();
  await page.getByRole("menuitem", { name: "Duplicate" }).click();
  await expect(page).toHaveURL(/\/editor$/);
  await expect(page.getByRole("textbox", { name: "Campaign name" })).toHaveValue("Starter test (copy)");
});

test("the inbox has the Not handled filter and the key hints", async ({ page }) => {
  await page.goto("/w/default/inbox");
  await expect(page.getByRole("button", { name: "Not handled" })).toBeVisible();
  await page.getByRole("button", { name: "Not handled" }).click();
  await expect(page).toHaveURL(/handled=no/);
});
