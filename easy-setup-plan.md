# Easy Setup Plan

Date: 2026-10-22. Read `README.md` and `progress.md` first.

## Why

A user said the setup is slow and too hard. Today a new user must:

1. Find a **setup code** in the server log or in `./data/setup_code.txt`.
2. Save **8 API keys** that the first boot prints one time (`./data/initial_keys.txt`).
3. Edit `.env` by hand for Google, Microsoft and other keys, then restart.
4. Walk through a 5-step onboarding (email, sending, AI, leads, done).
5. Use Linux or macOS. Windows does not work.

## Goals

| # | Goal |
| --- | --- |
| G1 | Windows support: a one-line installer for Windows, and a Rust build that compiles on Windows. |
| G2 | No setup code. No API keys made on first boot. The **master key** stays (Settings → API → Generate master key). The user makes keys when they need them. |
| G3 | The setup wizard asks only for a **design** (Generic, Indigo, Aikido). Then it goes straight to the dashboard. |
| G4 | A new **Settings → Environment** page. The user types Google, Microsoft and other values there and saves. No `.env` edit, no restart. |
| G5 | A **Guide** item in the profile (account) menu. It opens a page with step-by-step help for each credential. Every part folds open and closed. |

## Rules for the agent

- Match the code style around you. Short comments, plain words (see `progress.md`).
- Do **not** start the server or run DB-backed tests (user rule). Check with:
  - `cargo build`, `cargo build --no-default-features`
  - `cargo test --lib <module>` (pure tests only)
  - `cd ui && npx tsc --noEmit && pnpm lint && pnpm test`
- After backend API changes, `ui/src/api/schema.d.ts` needs `pnpm gen:api`, which needs a running server. **Do not run it.** Edit `schema.d.ts` by hand to match, and tell the user to run `pnpm gen:api` later.
- Utoipa schema names are global. Give every new struct a unique name.
- Update existing tests that expect the setup code or bootstrap keys. Mark DB tests you could not run as "not run" in `progress.md`.
- Do not touch the `generic` design look. New pages use tokens, `Page`, `PageHeader`, `SectionLayout` (see design notes in `progress.md`).

---

## Phase 1: Remove the setup code (G2)

**Today:** `src/main.rs:157` calls `users::setup::ensure_setup_code`. `POST /v1/auth/setup` (`src/users/routes.rs:211-271`) checks `setup_code`. `ui/src/routes/setup.tsx` has a "Setup code" field. Note: `install.sh` already says "The first account you make becomes the owner", so the code and the installer disagree today.

**Change:**

1. `src/main.rs`: remove the `ensure_setup_code` / `announce` call. On start, call `users::setup::remove_file(&cfg)` so an old `setup_code.txt` goes away.
2. `src/users/routes.rs`:
   - Remove `setup_code` from `SetupBody`.
   - Remove the hash check and the `setup-code` rate-limit key. Keep the IP rate limit.
   - Keep the advisory lock `7306002` and the `count(*) FROM users` check inside the transaction. Only the first person can make the owner. Everyone after gets `409 setup_done`.
   - Still set `setup_code_hash = NULL` (harmless, cleans old rows).
3. `src/users/setup.rs`: delete code generation and `announce`. Keep only `remove_file` (or move it). Delete the unit tests that test the code.
4. Leave the `app_state.setup_code_hash` column. No migration needed.
5. `ui/src/routes/setup.tsx`: remove the setup code field, its schema entry and `FIELD_OF.setup_code_wrong`. Change the intro to: "Make the owner account. The first account becomes the owner."
6. After `setup()` succeeds, go to `/onboarding/$step` with `step: "design"` (see Phase 3).
7. Fix tests: `tests/` and `ui/e2e/` specs that send `setup_code`. Search: `grep -rn "setup_code\|setup-code" tests ui/src ui/e2e`.

**Risk (write it in README):** a fresh server open to the internet can be claimed by the first visitor. Tell users: make the owner account right after install, before you share the address. The installer already prints the address so the user opens it at once.

## Phase 2: Stop making API keys on first boot (G2)

**Today:** `src/main.rs:153-154` calls `api_keys::bootstrap` and `announce_new_keys`. These make 8 keys and print them.

**Change:**

1. `src/auth/api_keys.rs`:
   - `bootstrap`: only insert keys that come from env vars (`AIKIDO_<SCOPE>_API_KEY`). Never generate a key. Return nothing to print.
   - Delete `announce_new_keys` and the `initial_keys.txt` writer.
2. `src/main.rs`: call the new `bootstrap` (env keys only). No printing.
3. Existing installs keep their old keys. Do not revoke anything.
4. Settings → API (`ui/src/routes/w.$ws.settings.api.tsx`) already has **Generate master key** and section keys. Keep it. Check the text does not mention `initial_keys.txt`.
5. Fix tests that expect bootstrap keys. Many integration tests may use bootstrap keys to call the API. If so, add a test helper that makes a key with `api_keys::create` instead.
6. Remove `initial_keys.txt` and `setup_code.txt` text from: `README.md` (Quick start step 5, Deploy step 3, Using the engines, Troubleshooting), `.env.example`, `.env.production.example`, `docs/API.md`.

## Phase 3: One-step setup wizard: pick a design (G3)

**Today:** `ui/src/routes/onboarding.$step.tsx` has 5 steps. `src/ui_api/routes.rs:107` has `ONBOARDING_STEPS`.

**Change:**

1. Backend `src/ui_api/routes.rs`: set `ONBOARDING_STEPS` to `["design", "done"]`. Make the PATCH accept old step names too (map them to `design`) so old saved state does not break.
2. Frontend `onboarding.$step.tsx`: replace the 5 steps with one screen:
   - Title: "Pick a design".
   - Three cards: **Generic**, **Indigo**, **Aikido**. Each card has a small preview (colors and layout sketch) and one line of text. Reuse the design list and preview logic from `LookItems` in `ui/src/components/shell/Sidebar.tsx` and the Appearance section of `ui/src/routes/w.$ws.settings.account.tsx`. Do not copy the data; export it from one place.
   - Clicking a card applies the design at once (same code path as Appearance: `localStorage aikido.design`, `users.preferences.design`, `<html data-design>`).
   - One button: **Go to dashboard**. It PATCHes `/v1/ui/onboarding` with `{ done: true, step: "done" }` and goes to `/w/$ws` (Home).
   - No step bar (only one step).
3. Any unknown `$step` value shows the design screen.
4. Delete the old step components (`EmailStep`, `SendingStep`, `AiStep`, `ImportStep`, `DoneStep`) from this file. Their features stay in the app (Accounts, Settings → Sending, Integrations, Leads). Remove imports that are no longer used.
5. Home / dashboard: if there is no email account yet, the existing empty states must point to **Email Accounts** and to the **Guide**. Check `ui/src/features/home/Home.tsx` and the Generic dashboard. Add one small "Get started" card with links: Connect an email account, Add an AI model, Import leads, Open the guide. Hide it once an account exists (use `/v1/ui/onboarding` facts).
6. Update the e2e onboarding spec, or mark it to update.

## Phase 4: Settings → Environment (G4)

### What the user can set in the app

Only values that are safe to change while the server runs. Group them:

| Group | Keys | Used in |
| --- | --- | --- |
| Google (Gmail) | `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET` (secret) | `src/email_management/gmailconnection/connect.rs` via `oauth_common::var` |
| Microsoft (Outlook) | `MS_CLIENT_ID`, `MS_CLIENT_SECRET` (secret), `MS_TENANT` (default `common`) | `src/email_management/msconnection/connect.rs` |
| Sending | `AIKIDO_DRY_RUN` (on/off) | `Config.dry_run` → `RealSenders { dry_run }` in `src/main.rs` |
| Open tracking | `AIKIDO_TRACKING_CNAME_TARGET`, `AIKIDO_ACME_EMAIL` | `src/tracking/` via `Config.tracking` |

Not in the app (the server needs them before it can read the database, or they change the listener): `DATABASE_URL`, `AIKIDO_ENCRYPTION_KEY`, `AIKIDO_DATA_DIR`, `AIKIDO_BIND_ADDR`, `AIKIDO_PUBLIC_URL`, `AIKIDO_INSECURE_COOKIES`, `AIKIDO_TRUST_PROXY`, `AIKIDO_TRACKING_HTTPS_ADDR`, `AIKIDO_TRACKING_HTTP_ADDR`, the `*_ENABLED` switches, tick and parallel numbers. Show these on the page as **read-only** ("Set in `.env` on the server"), with their current value (never secrets) and a link to the Guide.

AI providers and Reoon already have their own in-app screens (Integrations, generator providers). Do not move them. Link to them from this page.

Start with the Google and Microsoft groups (they are the main pain). Do Sending and Open tracking only if the code path can read a live value without a big refactor. If not, skip them and write that in `progress.md`.

### Backend

1. Migration `migrations/0024_app_settings.sql`:
   ```sql
   CREATE TABLE app_settings (
     key         text PRIMARY KEY,
     value       text,          -- plain values
     sealed      bytea,         -- secret values, AES-256-GCM
     nonce       bytea,
     key_version int,
     updated_by  uuid REFERENCES users(id) ON DELETE SET NULL,
     updated_at  timestamptz NOT NULL DEFAULT now()
   );
   ```
   Secrets use the existing `Crypto` (`src/crypto.rs`) with the setting key as AAD.
2. New module `src/app_settings/` (`mod.rs`, `store.rs`, `routes.rs`):
   - A fixed catalog: key, group, label, `secret: bool`, `kind` (text / bool / select), default.
   - A process-wide overlay: `static OVERLAY: RwLock<HashMap<String, String>>` (decrypted values). Load it at start in `serve()` after `Crypto::load_or_create`. Refresh it after every save.
   - `pub fn get(name) -> Option<String>`: **the env var wins** when it is set and not empty. Else the overlay value. This keeps Docker / Coolify users working.
   - Every key reports its source: `env`, `app`, or `unset`. A key set in env is read-only in the UI.
3. Make the OAuth code read the overlay:
   - `src/email_management/oauth_common.rs`: `var()` and `var_or()` check `app_settings::get(name)` (which checks env first). Keep the signatures, so the Gmail and Microsoft `Config::load()` need no change.
   - `src/ui_api/routes.rs:71` `env_set()`: use the same lookup, so `/v1/ui/config.oauth.gmail` turns true right after save.
   - Many server copies: each copy loads the overlay at start. Also re-read it every 60 s (or on `LISTEN/NOTIFY app_settings`). Pick the simpler one: a 60 s reload loop is fine.
4. Routes (admin or owner session, or `admin` key; CSRF as usual):
   - `GET /v1/settings/environment`: list of every catalog key: `key`, `group`, `label`, `secret`, `source`, `value` (secrets: never the value, only `is_set: true` and the last 4 chars), `restart_needed: false`. Plus the read-only list with values (never secrets).
   - `PUT /v1/settings/environment`: body `{ values: { KEY: "value" | null } }`. `null` clears. Reject unknown keys and keys set by env (`409 set_by_env`). Trim. Validate: client IDs not empty; `MS_TENANT` letters, digits, dots, dashes; bools `true`/`false`.
   - `POST /v1/settings/environment/test/{group}`: Google: check the client ID looks right (`*.apps.googleusercontent.com`) and that `AIKIDO_PUBLIC_URL` is set. Microsoft: call the tenant's OpenID config URL to check the tenant exists. No real login.
   - Logs never hold the values. Add the new routes to `src/docs.rs` OpenAPI and to `docs/API.md` (new section).
5. Unit tests (pure, no DB): env-wins rule, catalog validation, secret masking. DB tests: write them, mark "not run".

### Frontend

1. New route `ui/src/routes/w.$ws.settings.environment.tsx`. Add **Environment** to the tab list in `ui/src/routes/w.$ws.settings.tsx` (admins only, next to API).
2. Layout: one `Card` per group. Each card:
   - Title, one line of text, and a **"How to get this"** link to the matching Guide section (`/w/$ws/guide#google`).
   - The redirect URI to copy (from `/v1/ui/config.redirect_uris`), with a Copy button.
   - Fields. Secret fields use `PasswordField`, show "Saved (ends in abcd)" when set, and a **Replace** button.
   - A field set by env is disabled with the pill "Set in .env".
   - **Save** and **Test** buttons. Show success or the error from the server.
3. Below: **Server settings (read only)** table.
4. After save, invalidate the `ui config` query so **Connect Gmail / Outlook** buttons turn on at once.
5. In `ui/src/features/accounts/AddAccount.tsx`: when Gmail or Outlook is not set up, show "Add your Google keys in Settings → Environment" with a link, instead of the current `.env` text.
6. Hand-edit `ui/src/api/schema.d.ts` for the new routes. Add a `q.environment` query in `ui/src/api/queries.ts`.

## Phase 5: The Guide (G5)

1. Account menu: add **Guide** (icon `BookOpen` from lucide) above **Log out** in both menus:
   - `ui/src/components/shell/Sidebar.tsx` (Generic, Indigo), near line 446.
   - `ui/src/components/shell/AikidoShell.tsx` `AccountMenu`, near line 358.
2. New route `ui/src/routes/w.$ws.guide.tsx`, content in `ui/src/features/guide/` (`Guide.tsx`, `sections.tsx`).
3. Every section is collapsible. Use native `<details>/<summary>` (keyboard and screen reader work for free), styled with tokens. If `ui/src/components/ui/index.tsx` already has a disclosure part, use it. Add **Open all** / **Close all** buttons. Open the section named in the URL hash (`#google`) on load and scroll to it.
4. Show live values where they help: the real redirect URIs and public URL from `/v1/ui/config`, with Copy buttons. Show a green "Done" pill on a section when that credential is already set (from `/v1/settings/environment` and `/v1/ui/onboarding` facts).
5. Sections (each: what it is for, what you need, numbered steps, common errors). Plain short sentences. Write the steps from `README.md` "Connect your inboxes" and the provider docs:

| id | Title | Main steps |
| --- | --- | --- |
| `start` | Start here | What to set up first. The order: email account, then AI (optional), then leads, then campaign. |
| `google` | Gmail (Google OAuth) | Google Cloud Console → new project → enable Gmail API → OAuth consent screen (Internal for Workspace; External + Testing otherwise, add test users) → scopes `gmail.readonly`, `gmail.send` → Credentials → OAuth client ID, type **Web application** → add the redirect URI (copy from page) → copy Client ID and Secret → paste in Settings → Environment → Save → Email Accounts → Connect Gmail. Warn: Testing mode expires tokens after 7 days; `gmail.readonly` is a restricted scope for public apps. |
| `microsoft` | Outlook / Microsoft 365 | Entra admin center → App registrations → New → account types → redirect URI type **Web** (copy from page) → API permissions: delegated `Mail.Read`, `Mail.Send`, `offline_access`, `User.Read` → Grant admin consent if needed → Certificates & secrets → New client secret (copy the **Value**, not the ID) → copy Application (client) ID → tenant: `common` or your tenant ID → paste in Settings → Environment. |
| `imap` | IMAP / SMTP (any provider) | No keys needed. Hosts and ports for Gmail (app password, 2-step on), Outlook.com, Zoho, Yahoo, custom. Where to make an **app password**. |
| `ai` | AI model | Anthropic (console → API keys), OpenAI, OpenRouter, Ollama (local, base URL). Where to paste: Settings → Integrations / the AI screen. Cost and privacy note from README. |
| `reoon` | Reoon email verifier | Get the key, install the app in Settings → Integrations. |
| `api` | Master API key | Settings → API → Generate master key. Shown one time, copy it. Use `Authorization: Bearer <key>`. Section keys are optional. Link `/docs`. |
| `url` | Public address and HTTPS | What `AIKIDO_PUBLIC_URL` is, why OAuth needs it, how to change it in `.env` (Linux, macOS, Windows paths), then restart command. HTTPS with Caddy / Coolify short steps. |
| `tracking` | Open tracking domain | CNAME record steps, Cloudflare grey cloud, Coolify domain steps (from README). |
| `backup` | Back up your key | `AIKIDO_ENCRYPTION_KEY` in `.env`: where the file is on each OS, why it matters. |
| `trouble` | Problems | The README Troubleshooting table, in short form. |

6. Keep guide text in one TS file as data (title, id, steps array), so it is easy to edit. No markdown library.
7. A UI unit test: the page renders all sections, `#google` opens that section, Open all works.

## Phase 6: Windows support (G1)

### 6a. One-line installer for Windows

1. New file `install.ps1` (PowerShell 5.1 and 7). Run with:
   ```powershell
   irm https://raw.githubusercontent.com/nazmulhasanrupom/aikido-email/main/install.ps1 | iex
   ```
2. It does the same steps as `install.sh`:
   - Check Windows 10 21H2+ / 11.
   - Docker: if `docker` is missing, try `winget install -e --id Docker.DockerDesktop` (asks for admin). If winget is missing, print the Docker Desktop link and stop. Docker Desktop needs WSL 2: if `wsl --status` fails, print `wsl --install` and say a restart is needed, then stop.
   - Wait for Docker Desktop to run (`docker info`, up to 120 s). Try to start it from `"$env:ProgramFiles\Docker\Docker\Docker Desktop.exe"`.
   - Folder: `$env:USERPROFILE\aikido-email` (or `$env:AIKIDO_DIR`).
   - Download `docker-compose.install.yml` as `docker-compose.yml`.
   - `.env`: only if missing. Make secrets with .NET `System.Security.Cryptography.RandomNumberGenerator` (no openssl): 24 bytes hex for `POSTGRES_PASSWORD`, 32 bytes base64 for `AIKIDO_ENCRYPTION_KEY`. Find a free port from 8080 up (`Test-NetConnection` is slow; use `[System.Net.Sockets.TcpListener]`). `AIKIDO_PUBLIC_URL=http://localhost:<port>`. Write the file **UTF-8 without BOM** and with LF endings (Docker compose reads a BOM badly).
   - `docker compose pull`, `docker compose up -d`, wait for `http://127.0.0.1:<port>/health` (60 s).
   - Print the address, stop, update, logs, uninstall commands. Open the browser with `Start-Process`.
   - On failure: print the last 50 log lines and the issues link.
3. New file `uninstall.ps1`: `docker compose down`, ask before `down -v` and before deleting the folder.
4. Add `.gitattributes`: `*.sh text eol=lf`, `*.ps1 text eol=crlf` (PowerShell 5.1 is safest with CRLF; check `irm | iex` still works with CRLF, it does).
5. The image is already built for `linux/amd64,linux/arm64` (`.github/workflows/release.yml`). Docker Desktop on Windows runs it as is. No image change.
6. Check the script: `pwsh -NoProfile -Command "Invoke-ScriptAnalyzer ./install.ps1"` if `pwsh` exists on this Linux box. If not, say it was not checked.

### 6b. Make the Rust code build on Windows

Native Windows builds are for developers. Users get Docker. Still, `cargo build` must not fail on Windows.

| File | Problem | Fix |
| --- | --- | --- |
| `src/crypto.rs:9,83` | `std::os::unix::fs::OpenOptionsExt`, `.mode(0o600)` | Put the `use` and `.mode()` under `#[cfg(unix)]`. |
| `src/auth/api_keys.rs:12,312` | same | Gone after Phase 2. Check. |
| `src/users/setup.rs:10,59` | same | Gone after Phase 1. Check. |
| `src/email_management/oauth_common.rs:44-50` | `PermissionsExt`, `from_mode` | `#[cfg(unix)]` block. |
| `src/email_management/oauth_common.rs:27-33` | uses `HOME` | Fall back to `USERPROFILE`, or use `dirs::config_dir()` if the `dirs` crate is already in `Cargo.lock`. |
| `src/main.rs:338` | `tokio::signal::unix` | `#[cfg(unix)]` for SIGTERM. On other systems wait only for `tokio::signal::ctrl_c()`. |
| `.env.example` | `BROWSER_CMD=xdg-open` | Comment: Windows uses `cmd /c start`, macOS `open`. Make the code pick the default per OS when the var is empty. |

Then search for more: `grep -rn "unix\|/dev/\|xdg-open\|HOME" src`. Check with `rustup target add x86_64-pc-windows-gnu` and `cargo check --target x86_64-pc-windows-gnu` if the target can be added. If linking tools are missing, `cargo check` is enough (it does not link). If the target cannot be installed, say so.

### 6c. Docs

README **Install**: add a Windows block next to the Linux/macOS one, with the `irm | iex` line, the requirement (Docker Desktop with WSL 2), and the PowerShell versions of stop / start / update / logs / uninstall (`cd $env:USERPROFILE\aikido-email; docker compose down`).

## Phase 7: Docs and wrap-up

1. `README.md`:
   - Install: Windows block (Phase 6c).
   - Quick start step 5 and 6: no setup code, no keys. "Open the app, make the owner account, pick a design."
   - Connect your inboxes: "Paste the keys in **Settings → Environment**, or put them in `.env`. `.env` wins." Point to the in-app Guide.
   - Configuration → API keys: no keys on first boot; make the master key in Settings → API.
   - Features table: add Environment page and Guide.
   - Troubleshooting: remove "First-boot keys scrolled away". Add "Connect Gmail is greyed out → add the keys in Settings → Environment".
   - Requirements line: "Linux, macOS or Windows".
2. `.env.example`, `.env.production.example`, `.env.install.example`: remove setup code and initial key text. Note that Google and Microsoft keys can be set in the app.
3. `docs/API.md`: new section for `/v1/settings/environment`. Update `/v1/auth/setup` (no `setup_code`).
4. `progress.md`: new section "Easy setup (2026-10-22)": parts, decisions, what was checked, what was not run (DB tests, e2e, Windows install on a real Windows machine, Google/Microsoft real login with in-app keys, `pnpm gen:api`).
5. Final checks: `cargo build`, `cargo build --no-default-features`, `cargo test --lib`, `cd ui && npx tsc --noEmit && pnpm lint && pnpm test`. Report the real results.

## Order of work

1. Phase 1 and 2 (backend removals, small, unblock tests).
2. Phase 4 backend (store, overlay, routes).
3. Phase 3 (wizard) and Phase 4 frontend.
4. Phase 5 (Guide).
5. Phase 6 (Windows).
6. Phase 7 (docs, checks).

## Open questions for the user

1. **Setup code removal and safety.** Without the code, anyone who opens a fresh public server first becomes the owner. Plan: remove it as asked, and warn in README and installer output. Other choice: keep the code only when `AIKIDO_PUBLIC_URL` is not `localhost` (more safe, less simple). Default in this plan: remove it fully.
2. **Env vs app value.** Plan: `.env` wins when set, the field shows "Set in .env" and is locked. Other choice: the app value wins. Default: `.env` wins (Docker and Coolify users keep control).
