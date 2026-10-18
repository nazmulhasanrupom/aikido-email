import { expect } from "@playwright/test";
import { authed as test } from "./helpers";

// Builds on phase2.spec.ts (the owner exists). The tests share one database, so they run in order.
test.describe.configure({ mode: "serial" });

const CSV = [
  "E-mail Addr,Given name,Org,Role,Mood",
  "ana@acme.test,Ana,Acme,CEO,happy",
  "bo@beta.test,Bo,Beta,CTO,calm",
  "cy@gamma.test,Cy,Gamma,COO,tired",
].join("\n");

test("the setup wizard connects a mail server, sets sending basics and ends in the app", async ({
  page,
}) => {
  await page.goto("/onboarding/connect-email");
  await page.getByRole("button", { name: "Connect account" }).click();
  await page.getByRole("button", { name: /^Other/ }).click();
  await page.getByLabel("Email address").fill("sales@acme.test");
  await page.getByLabel("Host", { exact: true }).first().fill("127.0.0.1");
  await page.getByLabel("Port", { exact: true }).first().fill("1025");
  await page.getByLabel("Security", { exact: true }).first().selectOption("none");
  await page.getByLabel("Password", { exact: true }).first().fill("secret");
  await page.getByLabel("Also read replies").uncheck();
  await page.getByRole("button", { name: "Test and save" }).click();
  await expect(page.getByText("sales@acme.test")).toBeVisible();
  await expect(page.getByText(/we cannot see replies/i)).toBeVisible();
  await page.getByRole("button", { name: "Next" }).click();

  await expect(page.getByRole("heading", { name: "Sending basics" })).toBeVisible();
  await page.getByLabel("Emails per day per account").fill("25");
  await page.getByRole("button", { name: "Next" }).click();

  await expect(page.getByRole("heading", { name: /Connect an AI model/ })).toBeVisible();
  await page.getByRole("button", { name: "Skip for now" }).click();
  await expect(page.getByRole("heading", { name: /Import leads/ })).toBeVisible();
  await page.getByRole("button", { name: "Skip for now" }).click();
  await page.getByRole("button", { name: "Go to dashboard" }).click();
  await expect(page).toHaveURL(/\/w\/default\/campaigns/);
});

test("an account setting is saved and read back", async ({ page }) => {
  await page.goto("/w/default/accounts");
  await page.getByRole("link", { name: "sales@acme.test" }).click();
  await page.getByRole("link", { name: "Sending" }).click();
  await expect(page.getByLabel("Emails per day")).toHaveValue("25");
  await page.getByLabel("Emails per day").fill("12");
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Saved" })).toBeVisible();
  await page.reload();
  await expect(page.getByLabel("Emails per day")).toHaveValue("12");
});

test("a CSV with odd headers is mapped, imported and shown in Leads", async ({ page }) => {
  await page.goto("/w/default/leads");
  await page.getByRole("button", { name: "Import CSV" }).first().click();
  await page.getByLabel("CSV file", { exact: true }).setInputFiles({
    name: "odd.csv",
    mimeType: "text/csv",
    buffer: Buffer.from(CSV),
  });
  await page.getByRole("button", { name: "Upload and map columns" }).click();
  await expect(page.getByRole("heading", { name: "Match the columns of your file" })).toBeVisible();
  // the rules know the e-mail column; change the "Mood" column to a new skip
  await expect(page.getByLabel("Target for E-mail Addr")).toHaveValue("builtin:email");
  await page.getByRole("button", { name: "Import", exact: true }).click();
  await expect(page.getByText("The import is done.")).toBeVisible({ timeout: 20_000 });
  await page.keyboard.press("Escape");
  await expect(page.getByText("ana@acme.test")).toBeVisible();
  await page.getByRole("button", { name: "Open ana@acme.test" }).click();
  await expect(page.getByRole("tab", { name: "Timeline" })).toBeVisible();
});

test("a campaign: write the editor, it saves itself, preview, add leads and launch", async ({
  page,
}) => {
  await page.goto("/w/default/campaigns");
  await page.getByRole("button", { name: "New campaign" }).first().click();
  await page.getByLabel("Name", { exact: true }).fill("Spring outreach");
  await page.getByRole("button", { name: "Create campaign" }).click();
  await expect(page).toHaveURL(/\/editor$/);

  const body = page.getByRole("textbox", { name: "Body" });
  await body.fill(
    "Hi {{first_name|there}},\n\nSaw {{company_name|your company}}.\n\nBest,\n{{sender_name}}",
  );
  await page.getByRole("button", { name: "Add follow-up" }).click();
  await page.getByRole("textbox", { name: "Body" }).fill("Just checking in, {{first_name|there}}.");
  await expect(page.getByText("Saved", { exact: true })).toBeVisible({ timeout: 10_000 });
  await page.reload();
  await page.getByRole("button", { name: /Follow-up 1/ }).click();
  await expect(page.getByRole("textbox", { name: "Body" })).toHaveValue(
    "Just checking in, {{first_name|there}}.",
  );

  // preview with sample values
  await page.getByRole("button", { name: "Preview" }).click();
  await page.getByRole("button", { name: "Show emails" }).click();
  await expect(page.getByText("Saw Acme.")).toBeVisible();
  await page.keyboard.press("Escape");

  // settings: choose the account, then add leads from a list and launch
  await page.getByRole("navigation", { name: "Campaign pages" }).getByRole("link", { name: "Settings" }).click();
  await page.getByLabel("sales@acme.test").check();
  await page.getByRole("button", { name: "Save" }).first().click();
  await expect(page.getByText("Accounts saved.")).toBeVisible();
  await page.getByRole("navigation", { name: "Campaign pages" }).getByRole("link", { name: "Leads" }).click();
  await page.getByRole("button", { name: "Import CSV" }).click();
  await page
    .getByLabel("CSV file", { exact: true })
    .setInputFiles({ name: "more.csv", mimeType: "text/csv", buffer: Buffer.from(CSV) });
  await page.getByRole("button", { name: "Upload and map columns" }).click();
  await page.getByRole("button", { name: "Import", exact: true }).click();
  await expect(page.getByText("The import is done.")).toBeVisible({ timeout: 20_000 });
  await page.keyboard.press("Escape");
  await expect(page.getByText("ana@acme.test")).toBeVisible();

  await page.getByRole("button", { name: "Launch" }).click();
  await page.getByRole("button", { name: "Launch campaign" }).click();
  await expect(page.getByRole("button", { name: "Pause" })).toBeVisible({ timeout: 10_000 });
});

test("the search, the bell, Inbox, Reports, Settings and Status all open", async ({ page }) => {
  await page.goto("/w/default/campaigns");
  await expect(page.getByRole("navigation", { name: "Pages" })).toBeVisible();
  await page.keyboard.press("Control+k");
  await page.getByPlaceholder("Search...").fill("Spring");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/\/campaigns\/.+/);

  await page.getByRole("button", { name: /^Alerts/ }).click();
  await expect(page.getByText("Alerts", { exact: true })).toBeVisible();
  await page.keyboard.press("Escape");

  await page.goto("/w/default/inbox");
  await expect(page.getByRole("heading", { name: "Inbox" })).toBeVisible();
  await page.goto("/w/default/reports");
  await expect(page.getByRole("heading", { name: "Reports" })).toBeVisible();
  await expect(page.getByText("Reply rate").first()).toBeVisible();
  await page.goto("/w/default/settings/api");
  await expect(page.getByRole("button", { name: "New key" })).toBeVisible();
  await page.goto("/w/default/settings/team");
  await expect(page.getByText("owner@example.com")).toBeVisible();
  await page.goto("/w/default/settings/integrations/ai");
  await expect(page.getByRole("heading", { name: "Models" })).toBeVisible();
  await page.goto("/w/default/status");
  await expect(page.getByRole("heading", { name: "Sending" })).toBeVisible();
});
