import { expect } from "@playwright/test";
import { ensureOwner, authed as test } from "./helpers";

// Automations, phase 6: build a small workflow by hand, test it, publish it and turn it on.
// It needs no AI provider and no outside call: a manual trigger and a Set fields node.
// (Written against the real server like the other specs. Not run yet: see automationsetp.md.)
test.describe.configure({ mode: "serial" });

test.beforeAll(async ({ playwright }) => {
  const request = await playwright.request.newContext();
  await ensureOwner(request);
  await request.dispose();
});

test("the list is empty at first and a blank workflow opens the editor", async ({ page }) => {
  await page.goto("/w/default/automations");
  await expect(page.getByText("No automations yet")).toBeVisible();
  await page.getByRole("link", { name: "New workflow" }).click();
  await page.getByLabel("Name", { exact: true }).fill("Hello flow");
  await page.getByRole("button", { name: "Create" }).click();
  await expect(page.getByLabel("Workflow name")).toHaveValue("Hello flow");
  await expect(page.getByText("Manual trigger").first()).toBeVisible();
});

test("a node is added from the list, its field is filled, and it is saved", async ({ page }) => {
  await page.goto("/w/default/automations");
  await page.getByRole("link", { name: "Hello flow" }).click();
  await page.getByText("Manual trigger").first().click();
  await page.getByLabel("Search nodes").fill("set fields");
  await page.getByRole("button", { name: /^Set fields/ }).click();
  await page.getByRole("button", { name: "Add a row" }).click();
  await page.getByLabel("Fields key 1").fill("greeting");
  await page
    .getByLabel("Fields value 1")
    .fill("Hello ${{ trigger.input.name | default('there') }}");
  await expect(page.getByText("Saved")).toBeVisible({ timeout: 10_000 });
});

test("Test workflow runs it and the canvas shows the result", async ({ page }) => {
  await page.goto("/w/default/automations");
  await page.getByRole("link", { name: "Hello flow" }).click();
  await page.getByRole("button", { name: "Test workflow", exact: true }).click();
  await expect(page.getByRole("status").filter({ hasText: "success" }).first()).toBeVisible({
    timeout: 20_000,
  });
  await page.getByText("Set fields").first().click();
  await page.getByRole("tab", { name: /^Output/ }).click();
  await expect(page.getByText("Hello there")).toBeVisible();
});

test("Publish makes version 1 and the workflow can be turned on", async ({ page }) => {
  await page.goto("/w/default/automations");
  await page.getByRole("link", { name: "Hello flow" }).click();
  await page.getByRole("button", { name: "Publish" }).first().click();
  await page.getByRole("button", { name: /Publish version 1/ }).click();
  await expect(page.getByText("v1 published")).toBeVisible();
  await page.getByRole("switch", { name: "Active" }).click();
  await page.getByRole("button", { name: "Turn on" }).click();
  await expect(page.getByRole("switch", { name: "Active" })).toBeChecked();
  await page.getByRole("button", { name: "Executions" }).click();
  await expect(page.getByRole("cell", { name: "test" }).first()).toBeVisible();
});

test("secrets can be added and the settings page opens", async ({ page }) => {
  await page.goto("/w/default/automations/secrets");
  await page.getByRole("button", { name: "Add a secret" }).click();
  await page.getByLabel("Name", { exact: true }).fill("SEARCH_API_KEY");
  await page.getByLabel("Value", { exact: true }).fill("sk-test-12345");
  await page.getByRole("button", { name: "Save" }).click();
  await expect(page.getByText("SEARCH_API_KEY")).toBeVisible();
  await expect(page.getByText("••••2345")).toBeVisible();
  await page.goto("/w/default/automations/settings");
  await expect(page.getByText("Blocked domains for Fetch page")).toBeVisible();
});

// Phase 7: import, export, templates, LLM guide. Not run yet.
const FLOW = JSON.stringify({
  format: "aikido-workflow",
  version: 1,
  name: "Imported by test",
  nodes: [{ id: "start", type: "trigger.manual", name: "Start" }],
  edges: [],
});

test("import from pasted JSON opens the editor with the report", async ({ page }) => {
  await page.goto("/w/default/automations");
  await page.getByRole("button", { name: "Import" }).click();
  await page.getByRole("tab", { name: "Paste JSON" }).click();
  await page.getByLabel("Workflow JSON").fill(FLOW);
  await page.getByRole("dialog").getByRole("button", { name: "Import" }).click();
  await expect(page.getByLabel("Workflow name")).toHaveValue("Imported by test");
  await expect(page.getByRole("region", { name: "Import report" })).toBeVisible();
});

test("broken JSON says the line and column", async ({ page }) => {
  await page.goto("/w/default/automations");
  await page.getByRole("button", { name: "Import" }).click();
  await page.getByRole("tab", { name: "Paste JSON" }).click();
  await page.getByLabel("Workflow JSON").fill('{\n  "nodes": [\n  "a" 1\n}');
  await page.getByRole("dialog").getByRole("button", { name: "Import" }).click();
  await expect(page.getByRole("alert").getByText(/Line \d+, column \d+/)).toBeVisible();
});

test("a template makes a draft and the report asks for the list", async ({ page }) => {
  await page.goto("/w/default/automations/new");
  await page.getByRole("button", { name: "Browse templates" }).click();
  await page.getByRole("button", { name: /Use the template Webhook form/i }).click();
  await expect(page.getByRole("region", { name: "Import report" })).toBeVisible();
  await expect(page.getByLabel(/Pick the list for/)).toBeVisible();
});

test("download asks about pins only when there are some, and the LLM guide downloads", async ({
  page,
}) => {
  await page.goto("/w/default/automations");
  const guide = page.waitForEvent("download");
  await page.getByRole("button", { name: "LLM guide" }).click();
  await page.getByRole("button", { name: "Download workprocess.md" }).click();
  expect((await guide).suggestedFilename()).toBe("workprocess.md");
  await page.getByRole("button", { name: /More for Hello flow/ }).click();
  const file = page.waitForEvent("download");
  await page.getByRole("menuitem", { name: "Download" }).click();
  expect((await file).suggestedFilename()).toMatch(/\.aikido-workflow\.json$/);
});
