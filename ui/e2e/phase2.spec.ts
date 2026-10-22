import { expect, test } from "@playwright/test";
import { authed, expectInApp, newPage, OWNER, theme } from "./helpers";

// These tests build on each other: the first one makes the owner account.
test.describe.configure({ mode: "serial" });

test("a fresh server sends you to the setup page, and it asks for no setup code", async ({
  page,
}) => {
  await page.goto("/");
  await expect(page).toHaveURL(/\/setup$/);
  await expect(page.getByRole("heading", { name: "Set up AIKIDO EMAIL" })).toBeVisible();
  await expect(page.getByText("The first account becomes the owner.")).toBeVisible();
  await expect(page.getByLabel("Setup code")).toHaveCount(0);
});

test("setup checks the form before it asks the server", async ({ page }) => {
  await page.goto("/setup");
  await page.getByRole("button", { name: "Create account" }).click();
  await expect(page.getByText("Type your name.")).toBeVisible();
  await page.getByLabel("Password", { exact: true }).fill("short");
  await page.getByRole("button", { name: "Create account" }).click();
  await expect(page.getByText("Use at least 12 characters.").first()).toBeVisible();
});

test("the password meter and the show button work, and a weak password is refused by the server", async ({
  page,
}) => {
  await page.goto("/setup");
  const password = page.getByLabel("Password", { exact: true });
  await password.fill("password1234");
  await expect(page.getByText(/Strength: (Too easy|Weak)/)).toBeVisible();
  await page.getByRole("button", { name: "Show password" }).click();
  await expect(password).toHaveAttribute("type", "text");
  await page.getByRole("button", { name: "Hide password" }).click();
  await expect(password).toHaveAttribute("type", "password");

  await page.getByLabel("Your name").fill(OWNER.name);
  await page.getByLabel("Email", { exact: true }).fill(OWNER.email);
  await page.getByLabel("Workspace name").fill(OWNER.workspace);
  await page.getByRole("button", { name: "Create account" }).click();
  await expect(page.getByText("too easy to guess")).toBeVisible();
  await password.fill("correct horse battery staple");
  await expect(page.getByText("Strength: Strong")).toBeVisible();
});

test("setup makes the owner and shows the design screen", async ({ page }) => {
  await page.goto("/setup");
  await page.getByLabel("Your name").fill(OWNER.name);
  await page.getByLabel("Email", { exact: true }).fill(OWNER.email);
  await page.getByLabel("Password", { exact: true }).fill(OWNER.password);
  await page.getByLabel("Workspace name").fill(OWNER.workspace);
  await page.getByRole("button", { name: "Create account" }).click();

  await expect(page).toHaveURL(/\/onboarding\/design$/);
  await expect(page.getByRole("heading", { name: "Pick a design" })).toBeVisible();
  await page.goto("/w/default/campaigns");
  await expect(page).toHaveURL(/\/w\/default\/campaigns$/);
  await expectInApp(page);
  await expect(page.getByRole("button", { name: `Workspace ${OWNER.workspace}` })).toBeVisible();
  await expect(page.getByRole("button", { name: `Account menu for ${OWNER.name}` })).toBeVisible();

  // The cookie is the session: a reload keeps you in.
  await page.reload();
  await expectInApp(page);
  // The setup page is closed now.
  await page.goto("/setup");
  await expect(page).toHaveURL(/\/w\/default\/campaigns$/);
});

authed("every menu item opens its page and is marked as current", async ({ page }) => {
  await page.goto("/w/default/campaigns");
  const nav = page.getByRole("navigation", { name: "Pages" });
  const items: [string, RegExp][] = [
    ["Aikido AI", /\/ai$/],
    ["Campaigns", /\/campaigns$/],
    ["Leads", /\/leads$/],
    ["Inbox", /\/inbox$/],
    ["Email Accounts", /\/accounts$/],
    ["Reports", /\/reports$/],
  ];
  for (const [name, url] of items) {
    await nav.getByRole("link", { name }).click();
    await expect(page).toHaveURL(url);
    await expect(nav.getByRole("link", { name })).toHaveAttribute("aria-current", "page");
  }
  await page.goto("/w/default/ai");
  await expect(page.getByRole("heading", { name: "Aikido AI is coming soon." })).toBeVisible();
  // an unknown workspace goes to the only one
  await page.goto("/w/other/leads");
  await expect(page).toHaveURL(/\/w\/default\/campaigns$/);
});

authed("the user menu has settings, theme, status and log out", async ({ page }) => {
  await page.goto("/w/default/campaigns");
  await page.getByRole("button", { name: `Account menu for ${OWNER.name}` }).click();
  await expect(page.getByRole("menuitem", { name: "Settings" })).toBeVisible();
  await expect(page.getByRole("menuitemradio", { name: "Light" })).toBeVisible();
  await expect(page.getByRole("menuitemradio", { name: "Dark" })).toBeVisible();
  await expect(page.getByRole("menuitemradio", { name: "System" })).toBeChecked();
  await page.getByRole("menuitem", { name: "Status" }).click();
  await expect(page).toHaveURL(/\/w\/default\/status$/);
});

authed("the workspace menu shows the workspace and a disabled new workspace", async ({ page }) => {
  await page.goto("/w/default/campaigns");
  await page.getByRole("button", { name: `Workspace ${OWNER.workspace}` }).click();
  await expect(page.getByRole("menuitem", { name: /New workspace/ })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  await page.keyboard.press("Escape");
});

authed(
  "the theme changes at once, is kept after a reload, and does not flash",
  async ({ page }) => {
    await page.goto("/w/default/campaigns");
    await page.getByRole("button", { name: `Account menu for ${OWNER.name}` }).click();
    await page.getByRole("menuitemradio", { name: "Dark" }).click();
    await expect.poll(() => theme(page)).toBe("dark");
    const darkBg = await page.evaluate(() => getComputedStyle(document.body).backgroundColor);
    expect(darkBg).toBe("rgb(38, 38, 36)");

    // saved on the account too
    await expect
      .poll(async () => {
        const me = await page.evaluate(async () => (await fetch("/v1/users/me")).json());
        return me.preferences.theme;
      })
      .toBe("dark");

    // No flash: hold back the app script. The page must already be dark before the app starts.
    await page.route(/\/assets\/index-.*\.js$/, async (route) => {
      await new Promise((r) => setTimeout(r, 1500));
      await route.continue();
    });
    await page.goto("/w/default/campaigns", { waitUntil: "commit" });
    await page.waitForFunction(() => document.body !== null);
    expect(await theme(page)).toBe("dark");
    expect(await page.evaluate(() => getComputedStyle(document.body).backgroundColor)).toBe(
      "rgb(38, 38, 36)",
    );
    await expectInApp(page);
    await page.unroute(/\/assets\/index-.*\.js$/);

    await page.getByRole("button", { name: `Account menu for ${OWNER.name}` }).click();
    await page.getByRole("menuitemradio", { name: "Light" }).click();
    await expect.poll(() => theme(page)).toBe("light");
    expect(await page.evaluate(() => getComputedStyle(document.body).backgroundColor)).toBe(
      "rgb(246, 241, 233)",
    );
    await page.getByRole("button", { name: `Account menu for ${OWNER.name}` }).click();
    await page.getByRole("menuitemradio", { name: "System" }).click();
  },
);

test("the system setting is followed while the choice is System", async ({ browser }) => {
  const { context, page } = await newPage(browser, { colorScheme: "dark" });
  await page.goto("/w/default/campaigns");
  await expectInApp(page);
  expect(await theme(page)).toBe("dark");
  await page.emulateMedia({ colorScheme: "light" });
  await expect.poll(() => theme(page)).toBe("light");
  await context.close();
});

authed("the menu can be made narrow, and that is remembered", async ({ page }) => {
  await page.goto("/w/default/campaigns");
  const aside = page.getByRole("complementary", { name: "Main menu" });
  await expect(aside).toHaveCSS("width", "232px");
  await page.getByRole("button", { name: "Collapse the menu" }).click();
  await expect(aside).toHaveCSS("width", "56px");
  await page.reload();
  await expect(aside).toHaveCSS("width", "56px");
  await page.getByRole("button", { name: "Expand the menu" }).click();
  await expect(aside).toHaveCSS("width", "232px");
});

test("on a phone the menu is a drawer", async ({ browser }) => {
  const { context, page } = await newPage(browser, { viewport: { width: 600, height: 800 } });
  await page.goto("/w/default/campaigns");
  const nav = page.getByRole("navigation", { name: "Pages" });
  await expect(page).toHaveURL(/\/campaigns$/);
  await expect(nav).not.toBeInViewport();
  await page.getByRole("button", { name: "Open menu" }).click();
  await expect(nav).toBeInViewport();
  await nav.getByRole("link", { name: "Leads" }).click();
  await expect(page).toHaveURL(/\/leads$/);
  await expect(nav).not.toBeInViewport();
  // no sideways scroll
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(
    true,
  );
  await context.close();
});

authed(
  "log out ends the session; a protected page sends you to the login and back",
  async ({ page }) => {
    await page.goto("/w/default/leads");
    await page.getByRole("button", { name: `Account menu for ${OWNER.name}` }).click();
    await page.getByRole("menuitem", { name: "Log out" }).click();
    await expect(page).toHaveURL(/\/login$/);

    await page.goto("/w/default/leads");
    await expect(page).toHaveURL(/\/login\?next=%2Fw%2Fdefault%2Fleads$/);
    await page.getByLabel("Email", { exact: true }).fill(OWNER.email);
    await page.getByLabel("Password", { exact: true }).fill("wrong wrong wrong wrong");
    await page.getByRole("button", { name: "Log in" }).click();
    await expect(page.getByText("Email or password is wrong.")).toBeVisible();
    await page.getByLabel("Password", { exact: true }).fill(OWNER.password);
    await page.getByRole("button", { name: "Log in" }).click();
    await expect(page).toHaveURL(/\/w\/default\/leads$/);
    await expectInApp(page);
  },
);

authed("a login link cannot send you to another site", async ({ page }) => {
  await page.goto("/login?next=https://evil.example/");
  await expect(page).toHaveURL(/\/w\/default\/campaigns$/);
});

authed(
  "when the session ends in the middle of work, the next call goes to the login",
  async ({ page }) => {
    await page.goto("/w/default/reports");
    await expectInApp(page);
    // the server forgets this session (here: the cookie is gone)
    await page.context().clearCookies();
    // a change that calls the server: the theme is saved on the account
    await page.getByRole("button", { name: `Account menu for ${OWNER.name}` }).click();
    await page.getByRole("menuitemradio", { name: "Dark" }).click();
    await expect(page).toHaveURL(/\/login\?next=%2Fw%2Fdefault%2Freports$/);
    // after logging in again you are back where you were
    await page.getByLabel("Email", { exact: true }).fill(OWNER.email);
    await page.getByLabel("Password", { exact: true }).fill(OWNER.password);
    await page.getByRole("button", { name: "Log in" }).click();
    await expect(page).toHaveURL(/\/w\/default\/reports$/);
  },
);

test("the login page has a labelled form and works with the keyboard", async ({ page }) => {
  await page.goto("/login");
  await page.getByLabel("Email", { exact: true }).fill(OWNER.email);
  await page.keyboard.press("Tab");
  await page.keyboard.type(OWNER.password);
  await page.keyboard.press("Enter");
  await expectInApp(page);
});
