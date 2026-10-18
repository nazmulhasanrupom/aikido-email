# AIKIDO EMAIL: UI Build Map

Last updated: 2026-10-17. For the coding agent. This is a spec, not code.

Read first: `docs/API.md` (the API), `progress.md` (what is built), `README.md` (setup).
The API is the source of truth. When this map and the code disagree on a field name, the code wins. Write the difference in `progress.md`.

---

## 0. The short version

- One web app, served by the Rust server itself (same origin). One binary to deploy.
- Stack: React + TypeScript + Vite, built to static files, embedded into the Rust binary.
- People log in with email and password. The browser never holds the 8 scoped API keys. The server gives a session cookie instead.
- Left menu: **Aikido AI, Campaigns, Leads, Inbox, Email Accounts, Reports**. (Leads and Inbox are new. See section 2.)
- Minimal look. Warm dark theme (not pure black). Warm light theme (cream, slightly orange, not pure white).
- The backend needs 11 small additions (section 4). Build them first, in phase 1.

---

## 1. Decisions taken in this map

The user can change these. The map is written with the pick marked "Picked".

| # | Question | Option A | Option B | Picked |
| --- | --- | --- | --- | --- |
| D1 | Workspaces | One workspace in v1. The switcher shows it. Routes already carry `/w/:ws/` so real workspaces drop in later. | Real multi-tenant workspaces now: `workspace_id` on every table, every query, every key. Touches all 7 migrations and about 600 tests. | **A** |
| D2 | Email variants (A/B) | Not in v1. The editor leaves room for a "Variants" tab. The AI slots already make each email different per lead. | Build step variants in the backend now (new tables, split logic, stats per variant). Needs its own build map. | **A** |
| D3 | Campaign schedule | Send days, hours and gaps stay **per email account** (as the engine works today). The campaign shows them read-only with an "Edit" link. The campaign owns only `daily_new_leads` and `timezone`. | Add a campaign-level schedule to the engine (a campaign window inside the account window). Engine change. | **A** |
| D4 | How the UI calls the API | Session cookie, checked by the Rust server (section 4, B1 to B3). Passwords hashed in Rust. | A separate Node server holds the admin key and does its own login. Two languages, two security layers. | **A** |

---

## 2. Changes to the user's draft, and why

| Draft said | This map does | Why |
| --- | --- | --- |
| Menu: Aikido AI, Campeign, Email Accounts, Reports | Adds **Leads** and **Inbox** | Leads are global in the backend (lists, tags, custom fields, blocklist, imports). A lead can be in many campaigns. Inbox shows the replies the Reply Check finds. Without it, replies have no home. |
| Setup: email, then CSV, then AI | Email, then **sending basics**, then AI, then CSV | The CSV mapping uses the AI provider. With AI after CSV, the first import gets rules only. The default time zone is `UTC`, which is wrong for most users, so "sending basics" sets it. |
| Campaign "Editor" and "Follow up" | **Editor** holds every step (first email and follow-ups, with delays). **Follow-ups** shows who is at which step, the follow-up rules, and the AI review queue. | In the backend one sequence holds all steps. Splitting step text across two tabs would confuse. |
| Editor "different variant option" | Not in v1 (D2). AI slots (`{{ai:opener}}`) are in v1. | No variant support in the backend. |
| Campaign Settings "Schedule & Limits" | Daily new leads + campaign time zone. Account windows shown read-only (D3). | The engine schedules per account. |
| Advanced "stop sending on reply" | Shown as **always on** (locked, with one line of why). Real toggles: stop the whole company on reply, allow unverified emails, AI review. | The engine always stops on a reply. It cannot be turned off. |
| User menu: Settings, theme, logout, status | Same. "Status" opens a system status page with the engine pause switches. | Engine pause is admin work. It fits the status page. |
| Settings: Account, API, Notifications, Integrations, Team | Adds **Sending** (defaults for new accounts, reply check defaults, follow-up and lead settings). | These are system-wide defaults. They are not "external", so not Integrations. |
| Integrations | A registry of cards by category. v1 has **AI models**. "Databases" shows as "Coming soon". | Ready for many more later, as asked. |
| Search bar | Command palette (`Ctrl/Cmd + K`) plus a search box in the top bar. Finds pages, settings, campaigns, leads, accounts, lists. | One place to jump anywhere. |

---

## 3. Stack

| Part | Choice | Why |
| --- | --- | --- |
| Language | TypeScript (strict) | Types from the OpenAPI spec. |
| Build | Vite | Fast. Builds plain static files. |
| UI library | React | Best ecosystem for tables, editors, charts. Easy for coding agents. |
| Routing | TanStack Router | Type-safe routes and search params (filters live in the URL). |
| Server data | TanStack Query | Caching, polling (`refetchInterval`), retries. |
| API types | `openapi-typescript` + `openapi-fetch`, generated from `/openapi.json` | The backend already has a full OpenAPI spec. No hand-written types for API data. |
| Styling | Tailwind CSS v4 with CSS variables for the theme tokens | Small, fast, easy theming. |
| Components | Radix UI primitives (via shadcn/ui copied into the repo) | Accessible menus, dialogs, popovers, tabs. We own the code. |
| Tables | TanStack Table + TanStack Virtual | 100k leads, keyset paging, row selection. |
| Text editor (plain) | CodeMirror 6 | Variables shown as chips inside plain text. |
| Text editor (HTML) | TipTap | Simple rich text. Custom node for variable chips. |
| Forms | react-hook-form + zod | Client checks. Server `422` codes are mapped onto fields. |
| Charts | Recharts | Simple line and bar charts. |
| Search palette | `cmdk` | Command palette. |
| Icons | `lucide-react` | Clean, thin icons. |
| Toasts | `sonner` | Small notices. |
| Fonts | Inter (body), JetBrains Mono (code, variables). Self-hosted with `@fontsource`. | No calls to Google. Works offline. |
| Tests | Vitest (units), Playwright (end to end against a real server) | |
| Lint and format | Biome | One tool. |
| Package manager | pnpm | |

Versions: use the latest stable release of each that is at least 2 weeks old. Pin exact versions in `pnpm-lock.yaml`.

**Not used:** Next.js (needs a Node server), Leptos (all-Rust is nice, but tables and editors are much weaker today), `localStorage` for anything secret.

### Folder

```
ui/                         the web app (new)
  index.html
  vite.config.ts            dev proxy: /v1, /docs, /openapi.json, /health -> 127.0.0.1:8080
  src/
    api/schema.d.ts         generated: pnpm gen:api (reads http://127.0.0.1:8080/openapi.json)
    api/client.ts           openapi-fetch client, CSRF header, 401 handling, error mapping
    api/errors.ts           error code -> plain words (section 12)
    theme/tokens.css        colors (section 6)
    routes/                 one file per route (section 7)
    components/             shared: Sidebar, TopBar, DataTable, VariableChip, StatusPill, ...
    features/               onboarding, campaigns, editor, leads, import, inbox, accounts, reports, settings, integrations
    lib/                    template parser (mirror of the Rust one), time zones, formatting
  e2e/                      Playwright tests
src/web/                    Rust: static file serving (new, section 4 B3)
src/users/                  Rust: users, sessions, setup, invites (new, section 4 B1)
src/ui_api/                 Rust: /v1/ui/* routes (new, section 4)
```

Dev: run `cargo run -- serve` and `pnpm dev` in `ui/`. Open `http://localhost:5173`. The Vite proxy keeps one origin, so the cookie works.
Release: `pnpm build` writes `ui/dist`. The Rust build embeds it with `rust-embed` behind a cargo feature `ui` (on by default). `cargo build --no-default-features` makes an API-only server.

---

## 4. Backend additions (build in phase 1)

Same rule as earlier maps: these change existing code. They need the user's OK. Each one is small.

### B1. Users, passwords, sessions (`src/users/`, migration `0008_users.sql`)

Tables:

| Table | Columns |
| --- | --- |
| `users` | `id` uuid v7, `email` (lower case, unique), `name`, `password_hash` (PHC string), `role` (`owner`, `admin`, `member`), `status` (`active`, `disabled`), `preferences` jsonb (theme, notification toggles), `failed_logins` int, `locked_until`, `password_changed_at`, `created_at`, `updated_at` |
| `user_sessions` | `id`, `user_id`, `token_hash` (SHA-256), `csrf_token_hash`, `created_at`, `last_seen_at`, `expires_at`, `idle_expires_at`, `ip` (text, for the user's own list), `user_agent` (cut to 200 chars) |
| `user_invites` | `id`, `email`, `role`, `token_hash`, `invited_by`, `expires_at` (7 days), `used_at` |
| `app_state` | one row: `workspace_name`, `workspace_logo` (small data URL, max 200 KB), `onboarding` jsonb (`{ step, done, skipped: [] }`), `setup_done_at` |
| `login_attempts` | `key` (`ip:<ip>` or `email:<email>`), `count`, `window_start`, `locked_until` (for rate limits that work with many server copies) |

**Passwords.**

- Hash: **Argon2id** (`argon2` crate), PHC string format. Parameters: memory 64 MiB, iterations 3, parallelism 1, 16-byte random salt from `OsRng`, 32-byte output. These are above the OWASP minimum (19 MiB, 2 iterations).
- Pepper: a 32-byte secret made with HKDF-SHA256 from the master encryption key (`info = "aikido-password-pepper-v1"`), passed as the Argon2 `secret`. A stolen database alone cannot be cracked offline. (The user must back up `encryption.key` anyway.)
- Rehash on login when the stored parameters are older than the current ones.
- Hashing runs in `spawn_blocking`. A semaphore allows at most 4 hashes at once per server copy (stops a login flood from eating memory).
- Rules (NIST 800-63B style): 12 to 128 characters, Unicode NFKC normalized. No "must have a symbol" rules. Refuse weak passwords with `zxcvbn` (score under 3) and refuse the email or name inside the password. Show a strength meter in the UI.
- Compare with constant-time functions only. For an unknown email, still run one Argon2 verify against a dummy hash (same timing, no "user does not exist" leak). One error message for every failure: "Email or password is wrong."

**Rate limits and lockout.**

- Per IP: 20 failed logins in 15 minutes, then wait 15 minutes.
- Per email: 5 failures, then a growing delay (1 s, 2 s, 4 s ... up to 60 s). 10 failures: locked 15 minutes. The owner can unlock in Team.
- Stored in `login_attempts`, so all server copies share it.

**Sessions.**

- Token: 32 random bytes (`OsRng`), base64url. Only its SHA-256 is stored.
- Cookie: `__Host-aikido_session`, `HttpOnly`, `Secure`, `SameSite=Strict`, `Path=/`, no `Domain`.
- Idle timeout 3 days, absolute timeout 30 days. `last_seen_at` is written at most once a minute.
- New token at every login (no session fixation). Logout deletes the row. A password change deletes every other session of that user.
- `http://localhost` works (browsers treat it as secure). Plain HTTP on another host is refused with a clear page, unless `AIKIDO_INSECURE_COOKIES=true` (logs a loud warning every boot).

**CSRF.** The login answer gives a `csrf_token` (also stored hashed on the session). The UI sends it in `X-CSRF-Token` on every non-GET request. The server also checks that `Origin` (or `Referer`) equals `AIKIDO_PUBLIC_URL`. Fail: `403 csrf_failed`.

**First boot (setup code).** When no user exists, the server prints a one-time **setup code** (12 characters, same box as the API keys) and writes it to `./data/setup_code.txt` (mode `600`). The setup page asks for it. This stops a stranger from claiming a fresh server that is open to the internet. The code dies when the owner is made.

**Roles.**

| Role | Can do |
| --- | --- |
| `owner` | Everything. Exactly one. Can move ownership to an admin. |
| `admin` | Everything except remove or demote the owner. Manages API keys and team. |
| `member` | Campaigns, leads, inbox, accounts, reports. Not: API keys, team, Settings > Sending, engine pause. |

**Routes** (`/v1/auth/*` need no key; the rest need a session):

| Route | What it does |
| --- | --- |
| `GET /v1/auth/state` | `{ setup_needed, logged_in, user, csrf_token }`. The app calls it first. |
| `POST /v1/auth/setup` | `{ setup_code, name, email, password, workspace_name }`. Makes the owner and logs in. `409 setup_done` after the first time. |
| `POST /v1/auth/login` | `{ email, password }`. Sets the cookie. Returns `user`, `csrf_token`. |
| `POST /v1/auth/logout` | Deletes the session. |
| `GET`, `PATCH /v1/users/me` | Name, email, `preferences`. |
| `POST /v1/users/me/password` | `{ current_password, new_password }`. Logs out other sessions. |
| `GET /v1/users/me/sessions`, `DELETE /v1/users/me/sessions/{id}` | See and end own sessions. |
| `GET /v1/users`, `PATCH /v1/users/{id}` (role, status, unlock), `DELETE /v1/users/{id}` | Team (owner, admin). |
| `POST /v1/users/invites`, `GET /v1/users/invites`, `DELETE /v1/users/invites/{id}` | Make an invite link (shown once). Email sending of invites is not in v1: the admin copies the link. |
| `GET /v1/auth/invites/{token}`, `POST /v1/auth/invites/{token}/accept` | Invite page: name + password. Then logged in. |
| `POST /v1/users/{id}/transfer-ownership` | Owner only. Asks for the owner's password again. |

### B2. One auth layer for keys and sessions

The current middleware checks an API key and its scope. Add: if there is no key header, look for the session cookie. A valid session gets the scopes of its role (`owner` and `admin` = `admin` scope; `member` = every scope except `admin`, plus the B1 rules above). Cookie requests must pass the CSRF check. Key requests do not use cookies, so CORS stays open for them, but `Access-Control-Allow-Credentials` is never sent. A cross-site page can never use the cookie.

### B3. Serve the UI (`src/web/`)

- Static files from the embedded `ui/dist` at `/`. Any unknown path that is not `/v1/*`, `/docs`, `/openapi.json` or `/health` returns `index.html` (single page app).
- Hashed assets (`/assets/*`): `Cache-Control: public, max-age=31536000, immutable`. `index.html`: `no-cache`.
- Security headers on every UI response: `Content-Security-Policy: default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: same-origin`, `Permissions-Policy: camera=(), microphone=(), geolocation=()`, and `Strict-Transport-Security: max-age=31536000` when `AIKIDO_PUBLIC_URL` starts with `https`.
- The `/docs` page keeps its own policy.

### B4. App config (`GET /v1/ui/config`)

No secrets. `{ version, public_url_set, oauth: { gmail: bool, microsoft: bool }, dry_run, features: { replies, followups, leads, generator } }`. The UI hides "Connect Google" when Google is not set up and shows the setup steps instead.

### B5. Onboarding state (`GET`, `PATCH /v1/ui/onboarding`)

Reads and writes `app_state.onboarding`. The server also returns facts so the wizard can skip done steps: `{ accounts: n, readable_accounts: n, providers: n, leads: n, sending_defaults_changed: bool }`.

### B6. Daily numbers for charts (`GET /v1/reports/daily`)

Query: `from`, `to` (dates), `tz` (IANA, default the sending default), optional `campaign_id`, `account_id`. Answer: one row per day: `sent`, `replied`, `auto_replied`, `bounced`, `unsubscribed`, `complained`, `new_leads_enrolled`. Counted from `send_jobs`, `inbound_messages` and lead events. Max range 366 days. Scope `campaigns`. Also `GET /v1/reports/accounts?from&to`: per account `sent`, `bounce_rate`, `reply_rate`, `status`.

### B7. Campaign lead rows (check, add if missing)

`GET /v1/campaigns/{id}/leads` rows need `current_step`, `steps_total`, `next_due_at`, `wait_reason`, plus the lead's `first_name`, `last_name`, `company_name`, `interest`, `contact_state`. Add what is missing (one join). The Leads tab must not make one call per row.

### B8. OAuth popup end

The callback page (`GET /v1/accounts/oauth/{provider}/callback`) now also runs a small script: `window.opener?.postMessage({ type: "aikido:oauth", ok, email, code }, location.origin)` then `window.close()`. Same origin, so it is safe. The old text stays for people who opened it in a tab.

### B9. Manual reply from the Inbox (check, add if missing)

The inbound message object needs `message_id_header`, `references` and `provider_thread_id`, so the UI can send a reply in the same thread with `POST /v1/sending/jobs` (`"kind": "manual"`, `in_reply_to`, `references`, `provider_thread_id`, `priority: 100`, `lead_id`). Add what is missing.

### B10. Alerts (`GET /v1/ui/alerts`)

One call for the bell, polled every 60 s. Each alert: `{ id, kind, severity, title, detail, link, since }`. Kinds: `account_disconnected`, `account_error`, `account_reconnect_needed` (cannot read), `campaign_warning`, `campaign_no_healthy_account`, `generation_awaiting_review`, `provider_auth_failed`, `ai_budget_reached`, `import_failed`, `jobs_stuck`, `engine_paused` (each of the 5 engines). Users turn kinds off in Settings > Notifications (stored in `users.preferences`, filtered on the server).

### B11. Tests for B1 to B10

Must pass, on top of the current suite:
wrong password and unknown email take about the same time; the 6th failure is delayed; lockout works across two server copies; a session cookie without CSRF header gets `403`; a request from another `Origin` gets `403`; a member gets `403` on `/v1/keys`; setup twice gets `409`; a password change ends the other sessions; the SPA fallback never hides a `/v1` 404; every new route is in `/docs` (the CI test).

---

## 5. Look and feel

**Minimal.** Lots of space. One accent color. No gradients, no heavy shadows (1 px borders instead). Rounded corners 8 px (cards), 6 px (inputs, buttons). Density: 14 px body, 13 px in tables, row height 40 px.

**Words.** Short. Plain. Active voice. Use the UI words in section 12, never raw codes. Buttons say what they do ("Connect account", not "Submit"). No em dashes in UI text.

**Motion.** 150 ms fades and slides only. Off when the system asks for reduced motion.

---

## 6. Theme tokens

Three modes: Light, Dark, System (default System). Saved in `users.preferences.theme`, and copied to a cookie-free `localStorage` key `aikido.theme` only to stop a white flash before login. Set `data-theme` on `<html>` and `color-scheme`.

| Token | Light (warm cream) | Dark (warm charcoal, not black) | Use |
| --- | --- | --- | --- |
| `--bg` | `#F6F1E9` | `#262624` | Page |
| `--surface` | `#FBF8F3` | `#2E2E2B` | Cards, sidebar, menus |
| `--surface-2` | `#F0E9DE` | `#363632` | Hover, table header, input fill |
| `--border` | `#E3D9CA` | `#45443F` | Lines |
| `--text` | `#2B2621` | `#ECE8DF` | Main text (warm off-white in dark) |
| `--text-muted` | `#6F665B` | `#A8A196` | Labels, help text |
| `--accent` | `#C4673A` | `#D98559` | Primary buttons, links, focus ring |
| `--accent-fg` | `#FFFFFF` | `#1F1E1C` | Text on accent |
| `--accent-soft` | `#F3DFD2` | `#4A3528` | Selected row, active menu item |
| `--success` | `#3E7B4F` | `#74B183` | |
| `--warning` | `#A86B12` | `#DDA94A` | |
| `--danger` | `#B3433A` | `#E5806F` | |
| `--info` | `#3B6A8E` | `#7FA9CC` | |
| `--ai` | `#7A5BC1` | `#A68BE6` | AI slot chips, AI badges |

Every text color must reach WCAG AA (4.5:1) on `--bg`, `--surface` and `--surface-2`. Add a unit test that checks the contrast of each pair.

---

## 7. Screens and routes

```
/setup                        first boot (setup code, owner account)
/login
/invite/:token
/onboarding/:step             connect-email | sending-basics | ai | import | done
/w/:ws/ai                     Aikido AI (coming soon)
/w/:ws/campaigns
/w/:ws/campaigns/:id/editor | leads | followups | settings | reports
/w/:ws/leads                  ?tab=all|lists|fields|imports|blocklist  (filters in the URL)
/w/:ws/leads/:id              lead drawer over the list
/w/:ws/inbox
/w/:ws/accounts
/w/:ws/accounts/:id           overview | sending | reply-check | activity
/w/:ws/reports
/w/:ws/settings/account | api | notifications | integrations | integrations/:id | team | sending
/w/:ws/status
```

In v1 `:ws` is always `default` (D1).

### 7.1 Shell

```
+----------------------+------------------------------------------------------+
| [logo] Workspace  v  |        [ Search  Ctrl K ]                  (bell)    |
|                      |------------------------------------------------------|
|  Aikido AI   (soon)  |                                                      |
|  Campaigns           |                 page content                         |
|  Leads               |                                                      |
|  Inbox          (3)  |                                                      |
|  Email Accounts      |                                                      |
|  Reports             |                                                      |
|                      |                                                      |
|  (N) Nazmul       ^  |                                                      |
+----------------------+------------------------------------------------------+
```

- Sidebar 232 px. Collapses to 56 px (icons only). Below 768 px it becomes a slide-in drawer.
- **Workspace (top left):** logo + name. Menu: workspace settings (name, logo), "New workspace" shown with a "Later" tag and no action (D1).
- **Inbox badge:** count of new replies since the user last opened Inbox (`last_seen_inbox_at` in preferences).
- **User (bottom left):** avatar (initials) + name. Menu: Settings, Theme (Light / Dark / System), Status, Log out.
- **Top bar:** search box (opens the palette), bell (alerts from B10).
- **Banners** (thin bar under the top bar, one at a time, most serious first): "Sending is paused for everyone" (admin sees "Resume"), "Dry run: emails are logged, not sent", "1 account is disconnected" (link).

### 7.2 Search (command palette)

`Ctrl/Cmd + K` or click the box. Groups, top to bottom:

1. **Pages and settings:** a static index in code: every page, tab and each settings group (for example "Daily limit" opens Email Accounts > account > Sending, "API keys" opens Settings > API). Matched in the browser.
2. **Campaigns, Email accounts, Lists:** the lists are small. Fetch once, cache 60 s, match in the browser.
3. **Leads:** `GET /v1/leads?q=...&limit=5`, 200 ms debounce, min 2 characters.

Keyboard: arrows, Enter, Esc. Also actions: "New campaign", "Connect email account", "Import leads", "Toggle theme".

### 7.3 First boot and login

**Setup page:** one card. Fields: setup code (help: "Printed in the server log on first start, and saved in `./data/setup_code.txt`"), your name, email, password (strength meter, show/hide), workspace name. Button: "Create account". Then go to onboarding.

**Login page:** email, password, "Log in". One error text. After lockout: "Too many tries. Try again in 15 minutes." No "forgot password" by email in v1. Help text: "Ask an admin to reset it." Recovery for a lost owner password: CLI command `cargo run -- reset-password <email>` (prints a one-time link). Add it in phase 1.

### 7.4 Onboarding (after setup)

A centered card with a step bar: **Email > Sending > AI > Leads > Done**. Back and Next. The state is saved (B5), so a reload resumes. Steps that are already done (from B5 facts) show a check and can be passed.

1. **Connect an email account** (required, at least 1). Same component as Email Accounts > Add (section 7.8). After it connects, show its reading status. If `can_read` is false: one warning line ("We can send, but we cannot see replies. Follow-ups will wait.") and a "Fix" link.
2. **Sending basics** (writes `PATCH /v1/sending/defaults`, and copies to the accounts made in step 1 with `.../settings/reset`). Fields: time zone (search list, guess from the browser), send days (Mon to Sun chips, Mon to Fri on), hours (09:00 to 17:00), emails per day per account (30, with the warning above 50), warm-up for new inboxes (toggle). One line under it: "You can change all of this later per account."
3. **AI (optional, "Skip for now").** Pick a provider card (Anthropic, OpenAI, OpenRouter, Ollama, Other OpenAI-compatible). Fields per kind (key, model, base URL). "Test" calls `POST /v1/generator/providers/{id}/test`. On success set it as the slot, draft and mapping provider (`PATCH /v1/generator/settings`). Show the privacy note from README ("Lead data goes to this provider. Use Ollama to keep it local.").
4. **Import leads (optional, "Skip for now").** The CSV import flow (section 7.6). Optional: put them in a new list.
5. **Done.** "You are ready." Three cards: Create your first campaign, Go to Leads, Read the docs (`/docs`). Button "Go to dashboard" opens Campaigns.

### 7.5 Campaigns

**List page.** Title "Campaigns", button "New campaign".
Empty state: one line ("Campaigns send a set of emails to your leads, one by one.") and the button.
Table: Name, Status (pill), Leads (enrolled / total), Sent, Reply rate, Accounts, Created. Row click opens the campaign. Filter by status. Data: `GET /v1/campaigns` (with `counts`). Reply rate and sent come from `counts` if present, else from `GET /v1/campaigns/{id}/stats` for the visible rows only (max 25 calls, cached).

**New campaign dialog.** Name. Then pick a start:

- **Blank:** `POST /v1/campaigns` with a starter sequence of 1 step: subject `Quick question, {{first_name|there}}`, body `Hi {{first_name|there}},\n\n\n\nBest,\n{{sender_name}}`. The API needs a step with a subject, so a blank start uses this text.
- **Write with AI** (only when a draft provider works): brief form (product, offer, audience, tone, language, footer, number of steps, days between). Calls `POST /v1/generator/sequences/draft` with `save: true`, then `POST /v1/campaigns` with that `sequence_id`. Show a "Writing..." state.

Then open the Editor tab.

**Campaign header (all tabs).** Name (click to rename), status pill, tabs (Editor, Leads, Follow-ups, Settings, Reports). Right side: main button by status: Draft = **Launch**, Active = **Pause**, Paused = **Resume**. Menu: Archive.

**Launch dialog.** Calls `POST /v1/campaigns/{id}/launch`. On `422` list each problem in words (`no_steps`, `no_accounts`, `no_healthy_account`, `missing_variables` with the names, `generator_not_ready`) with a link to fix it. Before the call, show `warnings` from `GET /v1/campaigns/{id}` (capacity, token estimate) and a summary: leads pending, accounts, emails per day, first email goes out at (earliest `next_send_at` of the pool).

#### Editor tab

```
+---------------------------+-------------------------------------------------+
| Steps                     |  Email 1                       Text | HTML      |
|                           |  Subject  [ Quick question, {first_name|there} ]|
| [1] Email 1               |                                                 |
|      wait 3 business days |  Hi {first_name|there},                         |
| [2] Follow-up 1           |  {AI: opener}                                   |
|      wait 4 business days |  ...                                            |
| [3] Follow-up 2           |                                                 |
| + Add follow-up           |  [ { } Variable ]  [ * AI part ]   [ Preview ]  |
|                           |                                                 |
| About your offer (brief)  |  Saved                                          |
+---------------------------+-------------------------------------------------+
```

- **Steps rail.** Step 1 is "Email 1". Others are "Follow-up N". Between them: "Wait [3] [business days v]" (1 to 365; business or calendar). Add up to 10 steps. Drag to reorder (not step 1). Delete with confirm. Data: `GET /v1/followups/sequences/{id}`, saved with `PUT /v1/followups/sequences/{id}/steps` (the whole list).
- **Autosave.** 1.5 s after the last change. Show "Saving..." then "Saved". Map `422` codes to the step and field. `409 step_in_use`: "This step has emails waiting in the queue. Stop them first or keep the step."
- **Subject.** Step 1: required. Step 2 and later with thread on (default): no subject field, one line: "Sent as a reply in the same thread (Re: + the first subject)." With thread off: optional subject field.
- **Body mode switch: Text | HTML.**
  - Text: CodeMirror. Saves `body_text` only.
  - HTML: TipTap with bold, italic, link, bullet list, numbered list. Saves `body_html` and a plain version in `body_text` (made in the browser: links become `text (url)`), so the email is `multipart/alternative`.
  - Text to HTML: lines become paragraphs. HTML to Text: confirm "Formatting will be removed."
  - HTML is sanitized in the browser before save (DOMPurify, allow only the tags above). The server still escapes variable values.
- **Variables (placeholders).** Typing `{{` or clicking "{ } Variable" opens a menu with groups: Lead (first_name, last_name, company_name, job_title, website, location, ...), Custom fields (from `GET /v1/leads/fields`), Sender (sender_name, sender_email), Recipient (email). Inserted as a chip that reads `first_name` with a small `| there` part. Click a chip: popover to set or clear the fallback ("If empty, use: ___"). The stored text is plain `{{first_name|there}}`. An unknown name gets a red underline and a hover note. Mirror the Rust template rules in `ui/src/lib/template.ts` (spaces ignored, names `[A-Za-z0-9_]`, `{{ai:key}}` form). Unit tests share the same cases as the Rust tests.
- **AI parts.** "AI part" button (purple `--ai` color). Dialog: name (key), what the AI should write (instruction), max words (default 40), allow links (off), fallback text (shown as recommended). Inserts `{{ai:key}}` and adds the entry to the step's `ai_slots`. Click an AI chip to edit. Not allowed in a threaded subject (the menu hides it there). If no slot provider works: the button shows "Set up AI first" linking to Integrations.
- **About your offer (brief).** Under the rail. Text area (max 4000), saved to the sequence `brief` with `PATCH /v1/followups/sequences/{id}`. Help: "The AI reads this when it writes AI parts."
- **Write with AI.** Small button above the rail. Same brief form as the dialog. Shows the draft side by side. "Use these steps" replaces all steps (confirm).
- **Preview.** Opens a right drawer: pick a lead (search in the campaign's leads, or "Sample values" with editable fields) and an account. Shows each step as an email card (From, To, Subject, body). Calls `POST /v1/followups/sequences/{id}/preview`. AI parts show as `[AI: opener]` until the user clicks "Write AI parts for this lead" (`generate: true`, says "Uses AI tokens").
- **Send test email.** In the preview drawer: "Send to me" (the user's email, editable). Makes a `manual` job with `priority: 100`. Tell the user: "Queued. It follows the account's sending hours. Expected at <time>." (from `GET /v1/sending/accounts/{id}/status`).
- **Live campaign.** Banner: "This campaign is live. Changes apply to emails not written yet. Sent emails do not change."
- **Room for later:** a hidden "Variants" tab slot in the step header (D2).

#### Leads tab

- Top: counts by state (Pending, In sequence, Done, Skipped) as filter chips. Skipped shows reasons on hover.
- Buttons: **Add leads** (menu: From a list, From all leads with a filter, Import CSV), **Export** (`GET /v1/leads/export.csv?campaign_id=...`), Remove pending (on selection).
- Table (virtual, keyset paging with `cursor`): Email, Name, Company, State, **Follow-up status** ("Step 2 of 3, next Tue 10:00", or "Stopped: replied", or "Waiting for AI review"), Interest, Last activity. Data: `GET /v1/campaigns/{id}/leads` (B7).
- Row click: lead drawer (section 7.6). Row menu: Stop sequence (`POST /v1/followups/enrollments/{id}/stop`), Pause, Resume.
- Import CSV here passes `campaign_id`.

#### Follow-ups tab

- **Follow-up rules** card (sequence `PATCH`): Keep follow-ups in the same thread (`thread_followups`); On an out-of-office reply: Pause for [3] business days / Stop (`stop_on_auto_reply`, `auto_reply_pause_days`). A locked row: "A reply, bounce, unsubscribe or spam complaint always stops the sequence."
- **Waiting for you** (shows only when items exist): AI texts in review (`GET /v1/generator/generations?campaign_id=...&status=pending_review`). Each card: lead, step, the AI text in place inside the email, warnings. Buttons: Approve, Edit (`PATCH .../generations/{id}`), Write again (reject). Bulk: "Approve all" (with confirm). Also failed AI texts with "Edit by hand".
- **Enrollments** table (`GET /v1/followups/enrollments?sequence_id=...`): Lead, Account, Step, Status, Next send, Reason. Filters by status. Row actions: pause, resume, stop. Row click: enrollment drawer with its `events` and `jobs` (`GET /v1/followups/enrollments/{id}`).

#### Settings tab

Sections, each saved with its own "Save" button (`PATCH /v1/campaigns/{id}`):

1. **Accounts to use.** Checklist of accounts with health (status pill, reading ok, today's cap). How to pick an account for a new lead: Least busy (default) / Take turns. Help: "Each lead gets one account for the whole sequence."
2. **Schedule and limits.** New leads per day (1 to 5000). Day starts at midnight in: [time zone]. Below: a read-only table of the chosen accounts: days, hours, emails per day, time zone, with "Edit" links (D3). A capacity bar: "Needs about X emails a day. Your accounts can send Y." (from `warnings`).
3. **Advanced.** Stop on reply: locked on. Stop everyone at the same company when one person replies (`stop_company_on_reply`, help: free email domains like gmail.com are never one company). Send to emails we could not check (`allow_unverified`). AI review: Use the default / Always review / Never review (`PATCH /v1/generator/campaigns/{id}` `review_required`). AI token limit for this campaign (`token_budget`, shows `tokens_used`).
4. **Danger.** Archive campaign (confirm with the name).

#### Reports tab

- Date range picker (Last 7 days, 30 days, All, Custom).
- Cards: Sent, Leads contacted, Replies, Reply rate, Bounces, Unsubscribes, Spam complaints (`GET /v1/campaigns/{id}/stats`).
- Line chart per day: sent and replies (B6 with `campaign_id`).
- Table by step (`by_step`): step, sent, replies, reply rate. Table by account (`by_account`).
- AI usage: tokens and cost for this campaign (`GET /v1/generator/usage`, filtered by campaign).
- Link: "See replies in Inbox" (Inbox filtered by campaign).

### 7.6 Leads

Tabs: **All leads, Lists, Custom fields, Imports, Blocklist.**

**All leads.** Search box (`q`). Filter bar (chips): List, Tag, Campaign, Contact state, Interest, Email check, Blocked, Created date. Filters live in the URL. Table: checkbox, Email, Name, Company, Job title, State, Interest, Email check, Tags, Created. Keyset paging (`cursor` = last lead id; on `400 invalid_cursor` start again).
Selection bar actions (`POST /v1/leads/actions`, by ids or "all that match the filter" up to 10000): Add to list, Remove from list, Add tags, Remove tags, Set interest, Check emails again, Add to campaign (`POST /v1/campaigns/{id}/leads`), Delete.
**Delete** is an erase: dialog says "These leads are deleted and their emails are blocked forever. Their sequences stop." Type "DELETE" to confirm.
Buttons: Add lead (form), Import CSV, Export (current filter).

**Lead drawer** (`GET /v1/leads/{id}` + `/events` + `/context`): header (name, email, company, interest select). Tabs: Details (edit built-in and custom fields; email is locked while in a sequence), Timeline (events, newest first, plain words), Emails (sent and received, from `context.messages`), Notes (add note). Footer: lists, tags, campaigns, "Blocked" badge.

**Lists.** Table with member count. Create, rename, delete ("The leads stay.").

**Custom fields.** Table: key (shown as `{{key}}`), label, kind, fallback. Create (key rules shown inline). Edit label and fallback. Delete: on `409 field_in_use` show the named sequences.

**Imports.** History table (`GET /v1/leads/imports`): file, status, created, updated, skipped, invalid, started. Running ones show a live bar (poll every 2 s). Row: details with the error rows (row number, reason). Cancel.

**Blocklist.** `GET /v1/sending/suppressions`. Columns: value, type (email or domain), reason in words, added. Add (email or domain). Remove (confirm: "We may email this address again.").

**CSV import flow (shared by onboarding, Leads and campaign Leads):**

1. Drop zone. Limits shown (50 MB, 100000 rows, from lead settings). Options: duplicates (Fill empty fields / Skip / Overwrite, each with one line of help), add to list, tags.
2. Upload with `POST /v1/leads/imports?auto_map=true`. Show "Reading columns..." (the AI call can take up to 30 s).
3. **Mapping popup** (the proposal): one row per column: CSV header, 2 to 3 masked samples, arrow, target select (grouped: Built-in, Custom fields, "+ New field: <key>", Skip), a source badge (Rule / AI / Saved), a "Check this" mark for `needs_check`. On a column mapped to first name: "Split full name" toggle. Warnings at the top in words. Buttons: "Map again" (`.../mapping/propose`), "Show real values" (`raw_samples=true`), "Remember for files with these columns" (on), **Import**.
4. Confirm with `POST /v1/leads/imports/{id}/mapping/confirm`, sending only changed columns. On `422 mapping_invalid`, mark each problem on its row.
5. Progress bar until done. Then a summary: created, updated, skipped, invalid, with "See errors". Then "Add these to a campaign" (optional).

### 7.7 Inbox

The replies the Reply Check found. Two panes: list on the left, message on the right.

- Filters (chips): Replies (default, label `reply`), Out of office, Unsubscribes, Bounces, Spam complaints, All. Plus Campaign, Account, Interest.
- List row: sender name, company, campaign, first line of the reply, time, interest dot. Unread = newer than `last_seen_inbox_at`.
- Message pane: the reply text, the email it answered (subject, step), lead card (link to lead), and actions:
  - **Interest** buttons: Interested, Meeting booked, Won, Not interested, Wrong person, Lost (`PATCH /v1/leads/{id}` `interest`).
  - **Wrong label?** menu (`PATCH /v1/replies/messages/{id}` `label`). Confirm: "This changes what we blocked or stopped."
  - **Reply** box (plain text). Sends with B9. Note under the button: "Goes out from <account>, in the same thread, at the next free slot (usually a few minutes inside sending hours)."
- Data: `GET /v1/replies/messages` with cursor paging, polled every 60 s.

### 7.8 Email Accounts

**List.** Button "Add account". Table: Email, Provider (icon), Status (pill), Replies ("Reading" / "Cannot read" with reason), Today (sent / limit), Warm-up (day N or off), Next send (time or wait reason in words), menu (Pause, Resume, Test login, Delete). Data: `GET /v1/accounts` + `GET /v1/sending/accounts/{id}/status` for each (poll 30 s). Top: "Pause all sending" is NOT here (Status page).

**Add account dialog.** Three big cards:

- **Google (Gmail, Google Workspace):** `POST /v1/accounts/oauth/gmail/start`, open `auth_url` in a popup (500 x 650). Wait for the B8 message, or poll `GET /v1/accounts` every 2 s for up to 5 minutes. On success: "Connected: <email>". If `oauth_not_configured`: show the setup steps (env vars from API.md section 7, the redirect URI to copy, and the Gmail restricted-scope note from README).
- **Microsoft (Outlook, Office 365):** same with `microsoft`.
- **Other (IMAP / SMTP):** form in two blocks. SMTP: host, port (465 / 587 select, other allowed), security (SSL/TLS, STARTTLS), username (defaults to email), password. IMAP (strongly suggested, help: "Needed to see replies and stop follow-ups."): same fields, port 993 / 143. A "Same login as SMTP" toggle. Presets drop-down for common hosts (Zoho, Yandex, Fastmail, Namecheap Private Email) filling host and port only. Button "Test and save" (`POST /v1/accounts/smtp`; the server tests before saving). Show `smtp_login_failed` / `imap_login_failed` in words with tips (app passwords).

**Account page** tabs:

- **Overview:** status, why paused (`last_error` in words), Resume button, today's numbers, "Next 20 send times" (`GET .../preview?count=20`, local times), reading status per folder, "Sync now".
- **Sending:** the sending settings form (`/v1/sending/accounts/{id}/settings`), grouped: Days and hours (time zone, days, window, blocked dates), Pace (fixed gap, random extra gap min and max, shown as "7 to 15 minutes between emails"), Limits (per day, per hour, random daily variation, per company domain per day with exempt domains, same-person cooldown), Warm-up (on, start, step, live line "Day 4: 11 emails"), Safety (retries, pause after N failures, pause when bounce rate over N% of last M), Unsubscribe header (mailto, URL), Reply check gate (`reply_check_max_age_mins`). Show `warnings` from the server. "Reset to defaults" button.
- **Reply check:** settings (`/v1/replies/accounts/{id}/settings`): on/off, check every N minutes, folders, look back days, out-of-office action, stop scope, keyword lists (tag inputs), soft bounce limit. A "Test a keyword list" tool: upload an `.eml`, call `POST /v1/replies/classify`, show the label and why. Pause and resume reply check.
- **Activity:** jobs for this account (`GET /v1/sending/jobs?account_id=...`): to, subject, status, time. Row: events. Stuck or failed: "Send again" with the warning "It may already have been delivered."
- **Danger:** Delete account (confirm with the email): "Queued emails from this account are cancelled. Its sequences stop."

### 7.9 Reports (whole system)

- Date range picker and time zone note.
- Cards: Sent, Replies, Reply rate, Bounce rate, Unsubscribes, Spam complaints, Leads added.
- Chart: per day sent, replies, bounces (B6).
- Campaigns table: name, status, sent, replies, reply rate, bounces (from stats, cached).
- Accounts table: email, sent, reply rate, bounce rate, status (B6 `/v1/reports/accounts`). A bounce rate near the pause limit is orange.
- Leads: new leads per day, by source (from lead `created_at` and `source`, via `GET /v1/leads` counts; if too slow, add to B6).
- AI: tokens per day, cost (`GET /v1/generator/usage`).

### 7.10 Aikido AI

A centered empty state: icon, "Aikido AI is coming soon.", one line: "Ask questions about your leads and campaigns, and let the AI handle replies." No other UI. The route exists so the menu item works.

### 7.11 Settings

Left sub-menu: Account, API, Notifications, Integrations, Team, Sending. Members see only Account, Notifications and Integrations (they can manage AI providers).

- **Account:** name, email, password change, theme, active sessions (device, last seen, "Log out" each, "Log out everywhere else").
- **API:** keys table (`GET /v1/keys`): name or scope, prefix, last used, created. "New key" (pick scope) shows the key **once** with copy and a warning. Rotate (shows new key once). Revoke. `409 last_admin_key` in words. Box: API base URL, link to `/docs`.
- **Notifications:** toggles per alert kind (B10). Note: "Email alerts come later."
- **Integrations:** a grid of cards from a registry in code (`features/integrations/registry.ts`): `{ id, category, name, description, icon, status(): 'connected' | 'not_connected' | 'error' | 'soon', page }`. Categories in order: AI models, Databases (Soon: "Connect your database to import leads", Auto Database part 2), CRM (Soon), Webhooks (Soon). New integrations are one registry entry plus one page.
  - **AI models page:** Providers list (`GET /v1/generator/providers`): name, kind, model, status, "Test", edit (a new key clears `auth_failed`), delete (`409 provider_in_use` in words). Add provider (same form as onboarding). Then three blocks:
    - Which model does what: writes AI parts (`slot_provider_id`), writes whole sequences (`draft_provider_id`), maps CSV columns (`ai_mapping_provider_id` in lead settings, "Same as AI parts" default).
    - Rules (`PATCH /v1/generator/settings`): review every AI text first, write this many hours before sending, when the AI fails (use fallback / wait up to N minutes), language, max words output, temperature, daily token limit, banned phrases, spam words and spam check mode. CSV mapping: AI on/off, mask emails and phones, confidence level, remember mappings.
    - Usage: tokens and cost per day and per model (`GET /v1/generator/usage`). Prices editor (`prices`).
    - Privacy note (from README).
- **Team:** members (name, email, role, status, last login), change role, disable, unlock, remove. "Invite" makes a link (copy, expires in 7 days). Pending invites list with revoke. Owner: "Transfer ownership".
- **Sending (admin):** Defaults for new accounts (sending settings form, same component), Reply check defaults (same component as the account tab), Follow-ups (one active sequence per email, max wait days, engine speed), Leads (import limits, MX check, default duplicates mode, default new leads per day, extra free-mail domains, feeder speed). Each block has its own Save.

### 7.12 Status page (user menu > Status)

Cards, polled every 15 s:

| Card | Data | Admin action |
| --- | --- | --- |
| Server | `GET /health`, version, dry run | none |
| Sending | `GET /v1/sending/engine/status` | Pause / Resume all sending |
| Reply check | `GET /v1/replies/engine/status` | Pause / Resume |
| Follow-ups | `GET /v1/followups/status` | Pause / Resume |
| Campaign feeder | `GET /v1/campaigns/status` | Pause / Resume |
| AI writer | `GET /v1/generator/status` | Pause / Resume |

Each pause asks to confirm and says what keeps going (for example: "Emails already in the queue still go out" for follow-ups).

---

## 8. API client rules

- Every call goes through `ui/src/api/client.ts`. Same origin, `credentials: "same-origin"`.
- Non-GET calls add `X-CSRF-Token` (kept in memory from `/v1/auth/state` or login; never in `localStorage`).
- `401`: clear the cache, go to `/login?next=<path>`. `403 csrf_failed`: call `/v1/auth/state` once, retry once.
- Errors: read `error.code`, show the words from section 12, else `error.message`. Never show a raw stack or code alone.
- Lists use the cursor paging of each route. Never load all rows.
- Polling only on visible pages (TanStack Query `refetchIntervalInBackground: false`).
- Dates: the API gives UTC. Show local times in the **account** time zone for send times, and in the **browser** time zone for everything else. Show the zone name next to send times.
- Regenerate `schema.d.ts` after every backend change (`pnpm gen:api`). CI fails if it is out of date.

---

## 9. Security rules for the UI

- No API key, password or token in `localStorage`, `sessionStorage`, URLs or logs.
- No `dangerouslySetInnerHTML`, except the sanitized email preview, rendered inside a sandboxed `<iframe sandbox="">` with `srcdoc` (no scripts, no same-origin).
- Reply texts and lead fields are shown as text, never as HTML.
- Every destructive action has a confirm dialog. Erase and account delete need typed confirmation.
- API keys and invite links are shown once, with a copy button and a "You will not see this again" line.
- Export CSV comes from the server (formula escaping is already done there).

---

## 10. Accessibility and quality

- Keyboard for everything. Visible focus ring (`--accent`, 2 px).
- WCAG AA contrast in both themes (unit test, section 6).
- Every input has a label. Every icon button has an `aria-label`.
- Tables: sortable headers are buttons, row actions reachable by keyboard.
- Loading: skeleton rows, not spinners, for tables.
- Empty states: one line of what this is, one button.
- Works from 1280 px wide down to 768 px. Below that: reading and simple actions only (Inbox, lead drawer, status, pause). The editor says "Use a bigger screen".

---

## 11. Build phases

| Phase | Build | Done when |
| --- | --- | --- |
| 1 | Backend B1 to B11. Migration `0008`. `reset-password` CLI. | All B11 tests pass. Old suite passes. Every new route is in `/docs`. |
| 2 | `ui/` app: Vite, Tailwind tokens, themes, shell (sidebar, top bar, user menu), router, API client, setup page, login, logout, rust-embed serving. | Fresh database: setup code -> owner -> logged in -> shell shows. Theme switch has no flash. `cargo build` serves the UI on 8080. |
| 3 | Onboarding wizard. Email Accounts (list, add Google, add Microsoft, add SMTP/IMAP, account page with all tabs). | Mailpit SMTP account added through the UI. Settings saved and read back. OAuth popup tested against the mock server path (B8 message). |
| 4 | Leads: all tabs, lead drawer, CSV import flow with the mapping popup. | Import a 10000-row CSV with odd headers using `FakeLlm`. Mapping changed in the popup. Counters move live. Export opens in Excel without formulas running. |
| 5 | Campaigns list, new campaign, Editor (steps, delays, variables, AI parts, Text/HTML, brief, write with AI, preview, send test). | A 3-step sequence with variables and one AI part saves and previews. The TS template parser passes the same cases as the Rust parser. |
| 6 | Campaign Leads tab, Follow-ups tab (rules, review queue, enrollments), Settings tab, Launch flow. | End to end in dry run: create, add 20 leads, launch, see steps move, approve AI texts, pause, resume. |
| 7 | Inbox, Reports (B6), campaign Reports tab. | A reply fixture shows in Inbox. Interest change shows in the lead. Manual reply makes a threaded `manual` job. Charts show dry-run data. |
| 8 | Settings (account, API, notifications, integrations registry + AI page, team + invites, sending), Status page, alerts bell, command palette, polish, accessibility pass, Playwright suite, update `progress.md` and `README.md`. | Playwright suite passes in both themes. Lighthouse accessibility 95+. Member role cannot open admin pages (UI and API both refuse). |

Playwright runs against: Postgres + Mailpit + GreenMail (docker compose), the server with `AIKIDO_DRY_RUN=true` where real sending is not needed, and `FakeLlm` for AI.

---

## 12. Codes to words (start of the list; extend in `api/errors.ts`)

| Code or value | UI words |
| --- | --- |
| account `active` / `paused` / `error` / `disconnected` | Active / Paused / Stopped (problem) / Needs reconnect |
| `read_status` `reconnect_needed` / `login_failed` / `not_configured` | Reconnect to see replies / Inbox login failed / Reply reading not set up |
| wait `not_send_day` | Not a sending day |
| wait `blocked_date` | Day off |
| wait `outside_window` | Outside sending hours |
| wait `daily_limit_reached` | Daily limit reached |
| wait `hourly_limit_reached` | Hourly limit reached |
| wait `gap_not_elapsed` | Waiting between emails |
| wait `reply_check_stale` | Checking the inbox for replies first |
| wait `reply_check_unavailable` | Cannot read the inbox, follow-ups wait |
| job `stuck` | Unknown result (may have been sent) |
| job `skipped` | Skipped (blocked address) |
| enrollment `waiting_send` | In the send queue |
| `stop_reason` `reply` / `company_replied` / `auto_reply` / `manual` / `stalled` | Replied / A colleague replied / Out of office / Stopped by you / Waited too long |
| `skip_reason` `suppressed` / `unverified` / `active_elsewhere` / `missing_variable` | Blocked / Email not checked / In another campaign / Missing a value |
| label `reply` / `auto_reply` / `unsubscribe` / `bounce_hard` / `bounce_soft` / `bounce_block` / `complaint` | Reply / Out of office / Unsubscribe / Bounced (bad address) / Bounced (temporary) / Blocked by server / Spam complaint |
| suppression reason `replied` / `hard_bounce` / `unsubscribed` / `complained` / `manual` / `erased` | Replied / Bad address / Unsubscribed / Spam complaint / Added by you / Lead deleted |
| `verification` `mx_ok` / `syntax_ok` / `no_mx` / `invalid` / `unknown` | Checked / Looks right / Domain has no mail server / Not valid / Not checked |
| `contact_state` `new` / `in_sequence` / `contacted` / `replied` / `bounced` / `unsubscribed` / `completed` | New / In a sequence / Contacted / Replied / Bounced / Unsubscribed / Finished, no reply |
| `smtp_login_failed` | We could not log in to the SMTP server. Check the host, port and password. Many providers need an app password. |
| `oauth_not_configured` | Google (or Microsoft) login is not set up on this server yet. |
| `missing_variables` | Some leads have no value for: {names}. Add a fallback or fill the field. |
| `generator_not_ready` | Set up an AI model first. |
| `field_in_use` | This field is used in: {sequences}. Remove it there first. |

---

## 13. Not in v1

Real workspaces (D1), A/B variants (D2), campaign-level schedule (D3), two-factor login (TOTP), password reset by email, SSO, email alerts, invite emails, Aikido AI chat, Auto Database part 2 (database connectors), CRM and webhook integrations, open and click tracking, mobile editor, translations (keep all UI text in one `strings.ts` so it can be translated later).

## 14. Check by hand after the build

- Login through HTTPS on a real domain (cookie, HSTS, CSP do not block anything).
- Google and Microsoft popup login with real apps (`AIKIDO_PUBLIC_URL` must equal the UI origin).
- An email preview with odd HTML does not break the page.
- Leads table with 100000 rows scrolls smoothly.
- Both themes on a cheap laptop screen: the cream and the charcoal look calm, text is easy to read.
