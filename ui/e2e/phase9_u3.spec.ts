import { expect } from "@playwright/test";
import { authed as test } from "./helpers";

// UI update map v2, U3: AI mode screens. The server in these tests has no AI model, so Generate is off.
test.describe.configure({ mode: "serial" });

test("AI mode: switch, presets, prompts are saved, the privacy chip and the Generate dialog", async ({ page }) => {
  await page.goto("/w/default/campaigns");
  await page.getByRole("button", { name: "New campaign" }).first().click();
  await page.getByLabel("Name", { exact: true }).fill("AI test");
  await page.getByRole("button", { name: "Create campaign" }).click();
  await expect(page).toHaveURL(/\/editor$/);

  await page.getByRole("group", { name: "How the emails are written" }).getByRole("button", { name: "AI" }).click();
  // a new AI campaign starts with the optimized prompt; this test is about the custom prompt
  await expect(page.getByRole("button", { name: "Optimized prompt" })).toHaveAttribute("aria-pressed", "true");
  await page.getByRole("button", { name: "Custom prompt" }).click();
  await page.getByRole("button", { name: "Switch", exact: true }).click();
  await expect(page.getByLabel("System prompt (all steps)")).toBeVisible();
  await expect(page.getByText(/Set up an AI model first/).first()).toBeVisible();

  await page.getByRole("button", { name: "Presets" }).click();
  await page.getByRole("menuitem", { name: "Short and direct" }).click();
  await expect(page.getByLabel("System prompt (all steps)")).toHaveValue(/short cold emails/);
  await page.getByLabel(/User prompt: Primary email/).fill("Write to {{first_name}} {{email}}");
  await expect(page.getByText(/sends the email address/)).toBeVisible();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible({ timeout: 10_000 });
  await page.reload();
  await expect(page.getByLabel(/User prompt: Primary email/)).toHaveValue("Write to {{first_name}} {{email}}");
  await expect(page.getByRole("button", { name: "Generate" })).toBeDisabled();

  // an unknown variable is refused by the server and shown
  await page.getByLabel(/User prompt: Primary email/).fill("Write to {{nope}}");
  await expect(page.getByText(/which no lead field/)).toBeVisible({ timeout: 10_000 });
});
