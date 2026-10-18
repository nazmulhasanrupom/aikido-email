import { expect } from "@playwright/test";
import { authed as test } from "./helpers";

// UI update map v2, U2: variants, Save, email columns, email drawer.
test.describe.configure({ mode: "serial" });

const CSV = ["Email,First name", "kim@kilo.test,Kim", "lou@lima.test,Lou", "max@mike.test,Max"].join("\n");

test("variants, Save, email cells and a hand edit", async ({ page }) => {
  await page.goto("/w/default/campaigns");
  await page.getByRole("button", { name: "New campaign" }).first().click();
  await page.getByLabel("Name", { exact: true }).fill("Variant test");
  await page.getByRole("button", { name: "Create campaign" }).click();
  await expect(page).toHaveURL(/\/editor$/);

  await page.getByRole("textbox", { name: "Body" }).fill("Hello {{first_name|there}} from A.\n{{sender_name}}");
  await page.getByRole("button", { name: "Add a variant" }).click();
  await page.getByRole("textbox", { name: "Body" }).fill("Hi {{first_name|there}} from B.");
  await expect(page.getByText("Saved", { exact: true })).toBeVisible({ timeout: 10_000 });

  // add leads on the Leads tab
  const nav = page.getByRole("navigation", { name: "Campaign pages" });
  await nav.getByRole("link", { name: "Leads" }).click();
  await page.getByRole("button", { name: "Import CSV" }).click();
  await page.getByLabel("CSV file", { exact: true }).setInputFiles({ name: "v.csv", mimeType: "text/csv", buffer: Buffer.from(CSV) });
  await page.getByRole("button", { name: "Upload and map columns" }).click();
  await page.getByRole("button", { name: "Import", exact: true }).click();
  await expect(page.getByText("The import is done.")).toBeVisible({ timeout: 20_000 });
  await page.keyboard.press("Escape");

  // Save writes the emails
  await nav.getByRole("link", { name: "Editor" }).click();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText(/3 emails saved/)).toBeVisible({ timeout: 10_000 });

  await nav.getByRole("link", { name: "Leads" }).click();
  await expect(page.getByRole("columnheader", { name: /Primary email/ })).toBeVisible();
  await expect(page.getByText("Ready").first()).toBeVisible();
  // an even split: both variants show
  await expect(page.getByText(/^A: /).first()).toBeVisible();
  await expect(page.getByText(/^B: /).first()).toBeVisible();

  // open a cell, edit by hand
  await page.getByText("Ready").first().click();
  await page.getByLabel("Body").fill("Written by hand.");
  await page.getByRole("button", { name: "Save by hand" }).click();
  await expect(page.getByText(/This email is kept/)).toBeVisible();
});
