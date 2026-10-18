import { expect } from "@playwright/test";
import { authed as test } from "./helpers";

// Email optimization: the PERSONALIZATION page and the optimized prompt editor.
// The server in these tests has no AI model, so Build with AI and the model test are not run here.
test.describe.configure({ mode: "serial" });

test("PERSONALIZATION: add text, edit the profile, save, and see the version", async ({ page }) => {
  await page.goto("/w/default/personalization");
  await expect(page.getByRole("heading", { name: "PERSONALIZATION" })).toBeVisible();
  await expect(page.getByText(/No profile is saved yet/)).toBeVisible();

  await page.getByLabel("Tell us about you and your offer").fill("I help dental groups get patients with paid search.\n\nI help dental groups get patients with paid search.");
  await page.getByRole("button", { name: "Add text" }).click();
  // the cleaned text shows once (the repeated line is dropped)
  await expect(page.getByText("text", { exact: true })).toBeVisible();
  await expect(page.getByText("I help dental groups get patients with paid search.")).toHaveCount(1);

  await page.getByLabel("Name", { exact: true }).fill("Dana");
  await page.getByLabel("Offer", { exact: true }).fill("Paid search for dental groups");
  await expect(page.getByText("Needs your input").first()).toBeVisible();
  await page.getByLabel("Allow low personalization").check();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText(/Saved. New emails use this profile/)).toBeVisible();
  await expect(page.getByText("Saved versions (1)")).toBeVisible();

  await page.reload();
  await expect(page.getByLabel("Offer", { exact: true })).toHaveValue("Paid search for dental groups");
  await expect(page.getByLabel("Allow low personalization")).toBeChecked();
  await expect(page.getByText(/No profile is saved yet/)).toHaveCount(0);

  // building with AI needs a provider
  await page.getByRole("button", { name: "Build with AI" }).click();
  await expect(page.getByText(/Set up an AI provider first/)).toBeVisible();
});

test("optimized prompt: tabs, the built user prompt, edit, save and reset", async ({ page }) => {
  await page.goto("/w/default/campaigns");
  await page.getByRole("button", { name: "New campaign" }).first().click();
  await page.getByLabel("Name", { exact: true }).fill("Optimized test");
  await page.getByRole("button", { name: "Create campaign" }).click();
  await expect(page).toHaveURL(/\/editor$/);
  await page.getByRole("group", { name: "How the emails are written" }).getByRole("button", { name: "AI" }).click();

  await expect(page.getByRole("button", { name: "Optimized prompt" })).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByRole("tab", { name: "Primary mail" })).toBeVisible();
  await expect(page.getByLabel("Primary mail block (step 1)")).toHaveValue(/under 80 words/);
  await page.getByRole("tab", { name: "Follow-ups" }).click();
  await expect(page.getByLabel("Follow-up block (step 2 and later)")).toHaveValue(/under 60 words/);
  await expect(page.getByText("Change this in the Follow-ups tab")).toBeVisible();

  // edit the follow-up block, save, reload, reset
  await page.getByLabel("Follow-up block (step 2 and later)").fill("# STEP RULES\nMy own follow-up rule.");
  await page.getByRole("button", { name: "Save prompt" }).click();
  await expect(page.getByText("Saved for this campaign.")).toBeVisible();
  await page.reload();
  await page.getByRole("tab", { name: "Follow-ups" }).click();
  await expect(page.getByLabel("Follow-up block (step 2 and later)")).toHaveValue(/My own follow-up rule/);
  await page.getByRole("button", { name: "Reset to default" }).click();
  await expect(page.getByLabel("Follow-up block (step 2 and later)")).toHaveValue(/under 60 words/);

  // removing a section the checks need shows a warning
  await page.getByText(/Core \(shared by both tabs\)/).click();
  await page.getByLabel("Core block").fill("# ROLE\nshort");
  await expect(page.getByText(/The checks need the sections/)).toBeVisible();
  // and no profile warning is shown once a profile is saved (the first test saved one)
  await expect(page.getByText(/Mails will be HOLD/)).toHaveCount(0);
});
