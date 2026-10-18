import { expect } from "@playwright/test";
import { authed as test } from "./helpers";

// Email optimization: the Personalisation page and the optimized prompt editor.
// The server in these tests has no AI model, so Build with AI and the model test are not run here.
test.describe.configure({ mode: "serial" });

test("Personalisation: add text, edit the profile, save, and see the version", async ({ page }) => {
  await page.goto("/w/default/personalization");
  await expect(page.getByRole("heading", { name: "Personalisation" })).toBeVisible();
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

test("Optimised mode: three modes, plain prompts, 4 emails, save and reset", async ({ page }) => {
  await page.goto("/w/default/campaigns");
  await page.getByRole("button", { name: "New campaign" }).first().click();
  await page.getByLabel("Name", { exact: true }).fill("Optimised test");
  await page.getByRole("button", { name: "Create campaign" }).click();
  await expect(page).toHaveURL(/\/editor$/);
  const modes = page.getByRole("group", { name: "How the emails are written" });
  await expect(modes.getByRole("button")).toHaveText(["Template", "AI", "Optimised"]);
  await modes.getByRole("button", { name: "Optimised" }).click();

  // the prompts load by themselves, and no AI-mode text leaks in
  await expect(page.getByLabel("Instructions for the AI")).not.toHaveValue("");
  await expect(page.getByRole("button", { name: "Reset to default" })).toHaveCount(0);
  // plain text prompts, no JSON, and the old fields are gone
  await expect(page.getByLabel("Instructions for the AI")).toHaveValue(/# ROLE/);
  await expect(page.getByLabel("What the AI knows about you and each lead")).toHaveValue(/Company: \{\{company_name\}\}/);
  await expect(page.getByText("About your offer")).toHaveCount(0);
  await expect(page.getByLabel("Footer")).toHaveCount(0);
  // the 4 emails are made by themselves
  const four = page.getByRole("region", { name: "The 4 emails" });
  await expect(four.getByText("Primary email")).toBeVisible();
  await expect(four.getByText("Follow up 3")).toBeVisible();

  // edit the user prompt, save, reload, reset
  const user = page.getByLabel("What the AI knows about you and each lead");
  await user.fill("Lead: {{first_name}} at {{company}}.");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText(/Saved. Use Generate/)).toBeVisible();
  await page.reload();
  await expect(page.getByLabel("What the AI knows about you and each lead")).toHaveValue("Lead: {{first_name}} at {{company}}.");
  await page.getByRole("button", { name: "Reset to default" }).click();
  await expect(page.getByLabel("What the AI knows about you and each lead")).toHaveValue(/Company: \{\{company_name\}\}/);

  // back to AI mode: the old editor is unchanged, and the 3 follow-ups the mode made are gone again
  await page.getByRole("button", { name: "Save", exact: true }).isVisible();
  await modes.getByRole("button", { name: "AI" }).click();
  await expect(page.getByLabel("System prompt (all steps)")).toBeVisible();
  await expect(page.getByLabel("System prompt (all steps)")).toHaveValue("");
  await expect(page.getByRole("button", { name: /Follow-up 3|Follow up 3/ })).toHaveCount(0);
});
