import {
  type Browser,
  type BrowserContext,
  test as base,
  expect,
  type Page,
} from "@playwright/test";

export const OWNER = {
  name: "Olivia Owner",
  email: "owner@example.com",
  password: "correct horse battery staple",
  workspace: "Acme Outreach",
};

export async function logIn(page: Page, email = OWNER.email, password = OWNER.password) {
  await page.goto("/login");
  await page.getByLabel("Email", { exact: true }).fill(email);
  await page.getByLabel("Password", { exact: true }).fill(password);
  await page.getByRole("button", { name: "Log in" }).click();
}

export async function expectInApp(page: Page) {
  await expect(page.getByRole("navigation", { name: "Pages" })).toBeVisible();
}

export function theme(page: Page) {
  return page.evaluate(() => document.documentElement.getAttribute("data-theme"));
}

export const BASE = process.env.AIKIDO_E2E_URL ?? "http://localhost:8080";

/** Log in with the API, so the context holds the session cookie. Faster than the form. */
export async function apiLogin(
  context: BrowserContext,
  email = OWNER.email,
  password = OWNER.password,
) {
  const res = await context.request.post(`${BASE}/v1/auth/login`, {
    data: { email, password },
    headers: { origin: BASE },
  });
  expect(res.ok(), await res.text()).toBe(true);
}

/** A new browser window with its own cookies, already logged in unless told otherwise. */
export async function newPage(
  browser: Browser,
  options: Parameters<Browser["newContext"]>[0] = {},
  login = true,
) {
  const context = await browser.newContext({ baseURL: BASE, ...options });
  if (login) await apiLogin(context);
  return { context, page: await context.newPage() };
}

/** `test`, but the page is logged in as the owner. */
export const authed = base.extend({
  page: async ({ page, context }, use) => {
    await apiLogin(context);
    await use(page);
  },
});
