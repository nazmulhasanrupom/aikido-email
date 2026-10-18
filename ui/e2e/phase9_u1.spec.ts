import { expect } from "@playwright/test";
import { authed as test } from "./helpers";

// UI update map v2, U1: Create column in the mapping popup, edit cells, add column.
test.describe.configure({ mode: "serial" });

const CSV = ["Email,Company Size", "dan@delta.test,50", "eve@echo.test,200"].join("\n");

test("Create column makes {{company_size}} and the Leads table shows it", async ({ page }) => {
  await page.goto("/w/default/leads");
  await page.getByRole("button", { name: "Import CSV" }).first().click();
  await page.getByLabel("CSV file", { exact: true }).setInputFiles({
    name: "size.csv",
    mimeType: "text/csv",
    buffer: Buffer.from(CSV),
  });
  await page.getByRole("button", { name: "Upload and map columns" }).click();
  await page.getByLabel("Target for Company Size").selectOption("create");
  await expect(page.getByText("{{company_size}}")).toBeVisible();
  await page.getByRole("button", { name: "Import", exact: true }).click();
  await expect(page.getByText("The import is done.")).toBeVisible({ timeout: 20_000 });
  await page.keyboard.press("Escape");
  await expect(page.getByRole("columnheader", { name: /Company Size/ })).toBeVisible();
  await expect(page.getByText("200")).toBeVisible();
});

test("a cell is edited in place and an extra column is added", async ({ page }) => {
  await page.goto("/w/default/leads");
  const cell = page.getByRole("gridcell", { name: "50" });
  await cell.dblclick();
  const input = page.getByLabel("Edit Company Size");
  await input.fill("75");
  await input.press("Enter");
  await expect(page.getByRole("gridcell", { name: "75" })).toBeVisible();

  await page.getByRole("button", { name: "Add column" }).click();
  await page.getByLabel("Column name").fill("Industry");
  await expect(page.getByText("{{industry}}")).toBeVisible();
  await page.getByRole("button", { name: "Add column" }).last().click();
  await expect(page.getByRole("columnheader", { name: /Industry/ })).toBeVisible();
});
