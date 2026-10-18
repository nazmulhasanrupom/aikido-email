<div align="center">

# AIKIDO EMAIL

**Open source cold email automation, written in Rust.**
Send, follow up, detect replies, manage leads and write personal text with AI. All in one self-hosted server.

![Rust](https://img.shields.io/badge/Rust-2024-orange?logo=rust)
![PostgreSQL](https://img.shields.io/badge/PostgreSQL-16-336791?logo=postgresql&logoColor=white)
![React](https://img.shields.io/badge/React-19-61dafb?logo=react&logoColor=black)
![Docker](https://img.shields.io/badge/Docker-ready-2496ed?logo=docker&logoColor=white)
![License](https://img.shields.io/badge/license-Apache%202.0-blue)
![Tests](https://img.shields.io/badge/tests-663%20Rust%20%7C%2061%20UI%20%7C%2020%20E2E-brightgreen)

[Quick start](#-quick-start) · [Features](#-features) · [Configuration](#-configuration) · [Deploy](#-deploy-to-production) · [API](#-api) · [Development](#-development)

</div>

---

## Table of contents

- [What is it?](#-what-is-it)
- [Features](#-features)
- [How it works](#-how-it-works)
- [Quick start](#-quick-start)
- [Connect your inboxes](#-connect-your-inboxes)
- [Configuration](#-configuration)
- [Using the engines](#-using-the-engines)
- [Deploy to production](#-deploy-to-production)
- [API](#-api)
- [Development](#-development)
- [Project layout](#-project-layout)
- [Project status](#-project-status)
- [Troubleshooting](#-troubleshooting)
- [License](#-license)

---

## 📖 What is it?

AIKIDO EMAIL is an open source alternative to cold email platforms like Instantly. You run it on your own server. You keep your data.

You connect your inboxes (Gmail, Outlook or any IMAP/SMTP). You import leads. You write a sequence of emails. AIKIDO sends them with human-like timing, stops when someone replies, and shows everything in a web app.

## ✨ Features

| Area | What you get |
| --- | --- |
| **Sending engine** | Queue with per-inbox daily limits, warm-up, send windows, time zones, random gaps, retries, auto-pause on bad bounce rates. Safe with many server copies. |
| **Reply Check** | Reads inboxes (read only) and finds replies, auto-replies, bounces, unsubscribes and spam complaints. Blocks addresses, cancels queued emails, pauses a bad inbox. |
| **Follow-ups** | Sequences of 1 to 10 steps, sent in the same thread. Stops at once on reply, bounce, unsubscribe or complaint. Out-of-office pauses for 3 business days. |
| **Lead management** | Leads, custom fields, lists, tags, bulk actions, CSV import and export, MX email check, timeline per lead. |
| **Campaigns** | A sequence, a pool of sender inboxes, a set of leads and a daily limit of new leads. Stops colleagues at the same company when one replies. |
| **Email Generator (AI)** | `{{ai:opener}}` slots filled per lead by Anthropic, OpenAI-compatible APIs or a local model (Ollama). Guard checks, review mode, budgets, preview. |
| **Email optimization** | A default cold-mail prompt (Core, Primary, Follow-up) with a server-built user prompt, a PERSONALIZATION page that turns your link, CV and notes into a sender profile, code checks on every mail, and HOLD for mails that fail. |
| **AI column mapping** | Upload any CSV. Rules and an AI map the columns. You confirm before anything is saved. |
| **Web app** | Setup wizard, accounts, leads, campaigns editor, inbox, reports, settings, status page, alerts, `Ctrl K` search. |
| **Security** | Secrets encrypted with AES-256-GCM. Scoped API keys (SHA-256 hashed). CSRF protection. Logs never hold tokens or passwords. |
| **Everything is an API** | 100+ routes with a live OpenAPI spec at `/docs`. |

> **Planned:** AI agent and chatbot, Auto Database part 2 (connect to any existing database).

## 🧠 How it works

```
                    ┌────────────────────────── one Rust server ──────────────────────────┐
  Web app  ───────► │  Leads ─► Campaigns ─► Follow-ups ─► Sending queue ─► Inbox senders │ ─► Gmail / Outlook / SMTP
  (React)           │              ▲              ▲                │                      │
  REST API ───────► │       Email Generator   Reply Check ◄────────┴── Inbox readers ◄────│ ◄─ Gmail / Outlook / IMAP
                    └─────────────────────────────┬───────────────────────────────────────┘
                                                  ▼
                                            PostgreSQL 16
```

Key ideas:

- **A step becomes a job only when it is due.** A reply between two steps needs nothing to cancel.
- **Every rule runs on every send.** Daily cap, window, gap, warm-up, suppressions and bounce rate.
- **Replies stop the sequence.** Follow-ups wait if the inbox was not read recently (the safety gate).
- **Many copies are safe.** Work is claimed with Postgres row locks (`SKIP LOCKED`).

## 🚀 Quick start

### Requirements

| Tool | Version | Why |
| --- | --- | --- |
| [Rust](https://rustup.rs) | stable, edition 2024 (1.85+) | Build the server |
| [Docker](https://docs.docker.com/get-docker/) + Compose | any recent | Postgres and test mail servers |
| [Node.js](https://nodejs.org) | 22 | Build the web app |
| [pnpm](https://pnpm.io) | 10 | `npm i -g pnpm@10` |
| `openssl` | any | Make keys |

### 1. Get the code

```bash
git clone <your-repo-url> aikido_email
cd aikido_email
```

### 2. Start the dev services

```bash
docker compose -f docker-compose.dev.yml up -d
```

This starts:

| Service | Address | Use |
| --- | --- | --- |
| Postgres 16 | `127.0.0.1:55432` (user, password, db: `aikido`) | The database |
| Mailpit | SMTP `1025`, web page <http://127.0.0.1:8025> | Catches test emails. Nothing leaves your machine. |
| GreenMail | SMTP `3025`, IMAP `3143` | Mail server for the Reply Check tests |

### 3. Configure

```bash
cp .env.example .env
```

For a first try you only need `DATABASE_URL` (already set in the example). Add Google or Microsoft keys later, when you connect real inboxes.

### 4. Build the web app

```bash
cd ui
pnpm install
pnpm build          # writes ui/dist, which the server embeds
cd ..
```

> Skip this step to get a small placeholder page, or build an API-only server with `cargo build --no-default-features`.

### 5. Run the server

```bash
cargo run -- serve
```

The **first boot** prints two things one time. Save them.

| Printed | Saved in | Use |
| --- | --- | --- |
| A **setup code** | `./data/setup_code.txt` | Make the owner account in the web app |
| **8 API keys** (`admin`, `accounts`, `sending`, `replies`, `followups`, `leads`, `campaigns`, `generator`) | `./data/initial_keys.txt` | Call the API |

An **encryption key** is also made at `./data/encryption.key`.

> ⚠️ **Back up `./data/encryption.key`.** If you lose it, every saved inbox password and token is unreadable. You must connect all inboxes again.

### 6. Open the app

1. Go to <http://localhost:8080>.
2. Type the setup code and make the owner account.
3. Follow the onboarding steps.

Tip: set `AIKIDO_DRY_RUN=true` for the first run. The server then logs emails and sends nothing, with real timing.

### Lost the owner password?

```bash
cargo run -- reset-password you@example.com
```

It prints a one-time link.

## 📬 Connect your inboxes

You can connect inboxes in the web app (**Email Accounts**) or the API.

| Provider | What you need | Notes |
| --- | --- | --- |
| **Gmail** | Google Cloud OAuth client, type **Web application** | Scopes: `gmail.readonly`, `gmail.send` |
| **Outlook / Microsoft 365** | App in Microsoft Entra | Delegated: `Mail.Read`, `Mail.Send`, `offline_access`, `User.Read` |
| **IMAP / SMTP** | Host, port, login | Any provider. The password is encrypted before it is saved. |

### Gmail setup

1. In Google Cloud Console, make an OAuth client of type **Web application**.
2. Add the redirect URI: `<AIKIDO_PUBLIC_URL>/v1/accounts/oauth/gmail/callback`
3. Put `GOOGLE_CLIENT_ID` and `GOOGLE_CLIENT_SECRET` in `.env`.
4. Set `AIKIDO_PUBLIC_URL` (for local work: `http://localhost:8080`).

Read this before you go public:

- `gmail.readonly` is a **restricted** Google scope. A public app needs Google verification and a yearly security assessment.
- **Self-hosted in one Google Workspace?** Set the OAuth app to **Internal**. No verification needed.
- An **External** app in **Testing** mode: Google expires refresh tokens after **7 days**. The account goes `disconnected` and must be connected again.
- IMAP for Gmail with OAuth needs the restricted `https://mail.google.com/` scope. It is not a way around verification.

### Microsoft setup

1. In Microsoft Entra, register an app.
2. Add the redirect URI: `<AIKIDO_PUBLIC_URL>/v1/accounts/oauth/microsoft/callback`
3. Add the delegated permissions listed above.
4. Put `MS_CLIENT_ID`, `MS_CLIENT_SECRET` and `MS_TENANT` (`common` by default) in `.env`.

### Command line alternative

```bash
cargo run -- gmail add     # opens a browser
cargo run -- outlook add
cargo run -- imap add      # asks questions; needs ACCOUNTS_KEY in .env
```

With `DATABASE_URL` set, these also save into the encrypted store.

### Reply Check is read only

It never marks mail as read. It never moves, deletes, labels or flags mail. IMAP uses `EXAMINE` and `BODY.PEEK` only.
Accounts connected before the Reply Check show `can_read: false` and `reconnect_needed`. Connect them again to grant the read scope.

## ⚙️ Configuration

All settings are environment variables. Put them in `.env` (development) or in your host (production). Full commented list: [`.env.example`](.env.example).

### Core

| Variable | Default | Description |
| --- | --- | --- |
| `DATABASE_URL` | **required** | Postgres 16+ connection string |
| `AIKIDO_PUBLIC_URL` | none | The browser address, e.g. `https://mail.example.com`. Needed for OAuth, invite links and the login check. |
| `AIKIDO_ENCRYPTION_KEY` | auto-made in data dir | 32 bytes, base64: `openssl rand -base64 32`. **Back it up.** |
| `AIKIDO_BIND_ADDR` | `0.0.0.0:8080` | Server address |
| `AIKIDO_DATA_DIR` | `./data` | Holds the key and first API keys (mode `600`) |
| `AIKIDO_TRUST_PROXY` | `false` | `true` behind your own reverse proxy (reads `X-Forwarded-For`) |
| `AIKIDO_INSECURE_COOKIES` | `false` | `true` allows web login over plain HTTP. Trusted networks only. |
| `AIKIDO_DRY_RUN` | `false` | Log emails, send nothing |
| `RUST_LOG` | `info` | Log level |

### Providers

| Variable | Use |
| --- | --- |
| `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET` | Gmail OAuth |
| `MS_CLIENT_ID`, `MS_CLIENT_SECRET`, `MS_TENANT` | Outlook OAuth |
| `ACCOUNTS_KEY` | Encrypts IMAP passwords of the terminal `imap add` command |

### Engines

| Variable | Default | Description |
| --- | --- | --- |
| `AIKIDO_SCHEDULER_TICK_SECS` | `15` | Sending loop speed |
| `AIKIDO_MAX_PARALLEL_ACCOUNTS` | `20` | Inboxes handled at once per server |
| `AIKIDO_REPLY_TICK_SECS` | `15` | Reply poller speed |
| `AIKIDO_MAX_PARALLEL_SYNCS` | `10` | Inboxes read at once per server |
| `AIKIDO_MAX_MESSAGE_BYTES` | `10485760` | Largest raw email to download |
| `AIKIDO_REPLY_CHECK_ENABLED` | `true` | `false` turns the reply poller off |
| `AIKIDO_FOLLOWUPS_ENABLED` | `true` | `false`: this copy skips the follow-up scheduler |
| `AIKIDO_LEADS_ENABLED` | `true` | `false`: this copy skips the CSV import worker |
| `AIKIDO_GENERATOR_ENABLED` | `true` | `false`: this copy writes no AI text |

Set the `*_ENABLED` switches to `false` on a second server copy to split the work.

### API keys

If you set none, they are made on first boot. To use your own (for Docker or cloud deploys):

`AIKIDO_ADMIN_API_KEY`, `AIKIDO_ACCOUNTS_API_KEY`, `AIKIDO_SENDING_API_KEY`, `AIKIDO_REPLIES_API_KEY`, `AIKIDO_FOLLOWUPS_API_KEY`, `AIKIDO_LEADS_API_KEY`, `AIKIDO_CAMPAIGNS_API_KEY`, `AIKIDO_GENERATOR_API_KEY`

Each key has one scope. A wrong scope gets `403`. Only the SHA-256 hash is stored.

## 🧰 Using the engines

Set these once in your shell for the examples below:

```bash
export BASE=http://localhost:8080
export FOLLOWUPS_KEY=aik_followups_...   # from ./data/initial_keys.txt
export GENERATOR_KEY=aik_generator_...
```

### Follow-up sequences

```bash
# 1. A sequence: first email, then two follow-ups (3 and 4 business days later)
curl -s -X POST $BASE/v1/followups/sequences \
  -H "Authorization: Bearer $FOLLOWUPS_KEY" -H 'content-type: application/json' -d '{
  "name": "Cold outreach",
  "steps": [
    { "subject": "Quick question, {{first_name|there}}", "body_text": "Hi {{first_name|there}}, ..." },
    { "delay_amount": 3, "body_text": "Just checking in." },
    { "delay_amount": 4, "body_text": "Last note from me." }
  ]
}'

# 2. Enroll recipients (up to 1000 per call). One account sends every step to a recipient.
curl -s -X POST $BASE/v1/followups/sequences/$SEQ/enrollments \
  -H "Authorization: Bearer $FOLLOWUPS_KEY" -H 'content-type: application/json' -d '{
  "enrollments": [{ "to_email": "ana@acme.com", "account_id": "'$ACCOUNT'",
                    "variables": { "first_name": "Ana" }, "idempotency_key": "lead-123" }]
}'
```

Template syntax: `{{name}}` and `{{name|fallback}}`. Built-in values: `email`, `sender_name`, `sender_email`.

**Follow-up safety gate.** A follow-up goes out only if the inbox was read within `reply_check_max_age_mins` (sending setting, default `15`, `0` = off). Otherwise it waits 5 minutes and tries again. Use an account with `can_read: true`.

### Leads and campaigns

Leads need the Postgres extension `pg_trgm`. A normal Docker Postgres allows it. Some managed databases need it enabled by hand.

In the web app: **Leads → Import**, drop a CSV, check the AI column mapping, confirm. Then **Campaigns → New**, pick a sequence, sender inboxes, and a daily limit of new leads. API details: `docs/API.md`, sections 18 to 21.

### Email Generator (AI)

An AI writes the personal part of each step, per lead, a few hours before the step is due.

```bash
# 1. Add a provider and test it (the key is encrypted and never shown again)
curl -X POST $BASE/v1/generator/providers -H "Authorization: Bearer $GENERATOR_KEY" \
  -H 'content-type: application/json' \
  -d '{"kind":"anthropic","name":"Claude","model":"claude-haiku-5-5","api_key":"sk-ant-..."}'
curl -X POST $BASE/v1/generator/providers/$PROVIDER_ID/test -H "Authorization: Bearer $GENERATOR_KEY"

# 2. Use it for slots, with review mode on
curl -X PATCH $BASE/v1/generator/settings -H "Authorization: Bearer $GENERATOR_KEY" \
  -H 'content-type: application/json' \
  -d "{\"slot_provider_id\":\"$PROVIDER_ID\",\"review_required\":true}"

# 3. A step with a slot (put this in the steps of a sequence)
#    {"subject":"Hello {{first_name|there}}",
#     "body_text":"Hi {{first_name|there}},\n{{ai:opener}}\nBest, {{sender_name}}",
#     "ai_slots":[{"key":"opener","instruction":"One friendly line about their company.",
#                  "max_words":30,"fallback":"I came across your company."}]}

# 4. Look at one before you launch
#    POST /v1/generator/preview, then read 20 outputs: GET /v1/generator/generations
```

Supported providers: `anthropic`, and any `openai_compatible` server (OpenAI, OpenRouter, Ollama, ...).

> 🔒 **Privacy.** Lead data goes to the LLM provider you choose. The prompt holds name, job title, company, website, location, custom fields, a few notes, a short timeline and the last two emails sent to the lead. The **email address, phone number and reply text are never sent.** For zero third parties, run a local model (e.g. Ollama) as an `openai_compatible` provider.

> ✅ **Tip.** Keep `review_required` on for your first campaign. Read about 20 outputs before you trust it.

#### AI mode: the model writes the whole email

A campaign has a **content mode**. In `template` mode you write the email once and **Save** fills it for every lead. In `ai` mode you write a **system prompt** (one per campaign) and a **user prompt** (one per step). **Generate** writes one email per lead and step and saves it as a column of that campaign (`Primary email`, `Follow up 1`, ...). What you read is what goes out. A lead edit marks the email **Out of date**; use **Regenerate** to write it again.

Prompts take every lead variable (`{{first_name|there}}`, custom fields, `{{sender_name}}`) and three names that only prompts have: `{{brief}}` (the "About your offer" text), `{{previous_emails}}` (the saved earlier emails of this lead) and `{{lead_notes}}` (the last 5 notes). The server adds the output rules itself (JSON, plain text, no placeholders, a word limit). The campaign **footer** is added after the AI text. The AI never writes it.

> 🔒 **Privacy in AI mode.** A prompt carries whatever variables you put in it. The guard still blocks an email address or phone number in the **output**, but if you write `{{email}}` or `{{phone}}` in a prompt, that value goes to the AI company. The editor shows a warning chip when you do.

## 🌐 Deploy to production

`docker-compose.yml` runs the app and Postgres. The `Dockerfile` builds the web app and the server into one small image (non-root user, read-only filesystem, all capabilities dropped).

### 1. Make the settings

```bash
cp .env.production.example .env
```

Fill in the three required values:

| Variable | How |
| --- | --- |
| `POSTGRES_PASSWORD` | `openssl rand -hex 24` (letters and digits only) |
| `AIKIDO_PUBLIC_URL` | Your HTTPS address, no trailing slash |
| `AIKIDO_ENCRYPTION_KEY` | `openssl rand -base64 32` — **back it up** |

### 2. Start it

**Coolify:**
1. New resource → Docker Compose → pick this repo.
2. Paste the variables in the **Environment** tab.
3. Give the `app` service your domain, port `8080`.

**Any Docker server:**

```bash
docker compose up -d --build
```

Then put a reverse proxy with **HTTPS** in front of port `8080` (Caddy, Nginx, Traefik). Without HTTPS the web login does not work, except on localhost. Behind a proxy keep `AIKIDO_TRUST_PROXY=true`.

If you have no proxy network, publish the port to localhost only: `ports: ["127.0.0.1:8080:8080"]`.

### 3. Check it

- `GET /health` answers `{"status":"ok"}` when the database works.
- Read the app log for the **setup code** (also in `/data/setup_code.txt`).
- Open your address and make the owner account.

### 4. Back up

| Volume | Holds |
| --- | --- |
| `aikido_data` | Encryption key, first API keys |
| `pgdata` | The database |

Connect Gmail or Outlook: register `<AIKIDO_PUBLIC_URL>/v1/accounts/oauth/gmail/callback` and `.../microsoft/callback` in the provider apps.

## 📚 API

- **Reference:** [`docs/API.md`](docs/API.md) (21 sections)
- **Live docs:** `GET /docs` on a running server
- **OpenAPI spec:** `GET /openapi.json`

Authenticate with `Authorization: Bearer <key>`.

| Scope | Covers |
| --- | --- |
| `admin` | API keys (`/v1/keys/*`) |
| `accounts` | Connected inboxes |
| `sending` | Settings, queue, jobs, suppressions, engine control |
| `replies` | Reply Check |
| `followups` | Sequences and enrollments |
| `leads` | Leads, lists, tags, imports, mapping |
| `campaigns` | Campaigns |
| `generator` | AI providers, settings, generations, preview |

The web app uses cookie sessions with CSRF protection instead of keys.

## 🛠 Development

### Run the stack for coding

```bash
docker compose -f docker-compose.dev.yml up -d

# Terminal 1: the server. The public URL must be the dev server address.
AIKIDO_PUBLIC_URL=http://localhost:5173 cargo run -- serve

# Terminal 2: the web app with hot reload (proxies /v1 to 127.0.0.1:8080)
cd ui && pnpm install && pnpm dev     # http://localhost:5173
```

The port must stay `5173` so the login check passes.

### Tests

```bash
# Rust (needs the dev Postgres; Mailpit and GreenMail tests skip themselves if they are down)
DATABASE_URL=postgres://aikido:aikido@127.0.0.1:55432/aikido cargo test

# Web app
cd ui
pnpm test                 # unit tests (vitest)
pnpm lint && pnpm typecheck
e2e/run.sh                # browser tests (Playwright + Chrome) against a fresh server and database
                          # needs: dev services up, `cargo build`, `pnpm build`
```

| Suite | Count |
| --- | --- |
| Rust | 663 |
| UI unit | 61 |
| Browser (E2E) | 20 |

### Useful commands

| Command | What it does |
| --- | --- |
| `cargo run -- serve` | Start API, workers and web app |
| `cargo run -- reset-password <email>` | Print a one-time reset link |
| `cargo run -- backfill-leads` | Give old enrollments a lead (also runs at `serve` start) |
| `cargo build --no-default-features` | API-only server, no web app |
| `pnpm gen:api` (in `ui/`) | Regenerate API types from a running server. Run it after every backend change. |

### Tech stack

| Layer | Tools |
| --- | --- |
| Server | Rust 2024, Tokio, Axum, SQLx |
| Database | PostgreSQL 16 (+ `pg_trgm`) |
| Mail | Lettre (SMTP), async-imap, Gmail API, Microsoft Graph |
| Crypto | AES-256-GCM, SHA-256, rustls |
| Web app | React 19, TypeScript, Vite, Tailwind CSS 4, TanStack Router and Query, React Hook Form, Zod |
| Quality | Vitest, Playwright, Biome |

## 🗂 Project layout

```
.
├── src/
│   ├── main.rs            CLI: serve, reset-password, backfill-leads, gmail/outlook/imap add
│   ├── config.rs          Reads env vars
│   ├── crypto.rs          AES-256-GCM, master key
│   ├── auth/              API keys and scope middleware
│   ├── accounts/          Connected inboxes (encrypted at rest)
│   ├── email_management/  Gmail, Microsoft and IMAP/SMTP connectors
│   ├── sending/           Queue, scheduler, worker, rules, senders
│   ├── replies/           Reply Check: fetchers, parser, classifier, matcher
│   ├── followups/         Sequences, templates, scheduler, lifecycle
│   ├── leads/             Leads, import/export, MX check, AI column mapping
│   ├── campaigns/         Campaigns, daily-limit feeder, company stop
│   ├── generator/         AI email generator: LLM clients, guard, worker
│   └── docs.rs            OpenAPI spec and /docs page
├── migrations/            SQL migrations (run automatically)
├── tests/                 Integration tests (real Postgres)
├── ui/                    React web app (embedded into the binary)
├── docs/API.md            Full API reference
├── Dockerfile             Web app + server in one image
├── docker-compose.yml     Production: app + Postgres
├── docker-compose.dev.yml Development: Postgres, Mailpit, GreenMail
├── .env.example           Development settings
├── .env.production.example Production settings
└── progress.md            Detailed build log and decisions
```

## 📈 Project status

Built and tested: sending engine, Reply Check, follow-ups, lead management, campaigns, Email Generator, AI column mapping, and the full web app.

**Not yet tested against real services** (only mock servers, Mailpit and GreenMail were used):

- A real Gmail or Outlook **send**, and a real inbox **read**
- Outlook thread headers on a real mailbox
- A real call to Anthropic, OpenAI or Ollama
- IMAP over TLS on a real server
- A real Google or Microsoft login popup in a browser
- Firefox and Safari (only Chrome was tried)

Please try these on a test account first, and report what you find. `progress.md` lists every decision and open item.

**Roadmap:** AI agent and chatbot, Auto Database part 2 (connect to any existing database).

## 🩺 Troubleshooting

| Problem | Fix |
| --- | --- |
| Server refuses to start | `DATABASE_URL` is missing, or Postgres is down. Run `docker compose -f docker-compose.dev.yml up -d`. |
| Web login fails | Set `AIKIDO_PUBLIC_URL` to the exact browser address. Use HTTPS, or `localhost` in development. In dev the port must be `5173`. |
| Login fails behind a proxy | Set `AIKIDO_TRUST_PROXY=true`. |
| Saved inboxes stopped working | The encryption key changed or was lost. Restore `encryption.key` or connect the inboxes again. |
| Gmail account goes `disconnected` after 7 days | The Google app is in **Testing** mode. Use **Internal**, or publish and verify the app. |
| Account shows `reconnect_needed` | It was connected before the Reply Check. Connect it again. |
| Follow-ups do not go out | The inbox was not read lately (safety gate). Check `/v1/sending/accounts/{id}/status` for `reply_check_stale`. |
| `CREATE EXTENSION pg_trgm` fails | Enable `pg_trgm` by hand on your managed database. |
| Page shows a small placeholder | `ui/dist` is not built. Run `pnpm build` in `ui/`, then `cargo build`. |
| Lost the owner password | `cargo run -- reset-password <email>` |
| First-boot keys scrolled away | Read `./data/initial_keys.txt` (or `/data/initial_keys.txt` in Docker). |

## 📄 License

Copyright 2026 Nazmul Hasan Rupom. Licensed under the [Apache License 2.0](LICENSE).

You can use it, change it, sell it and share it, for personal or commercial work. It includes a patent grant.
You must keep the license text and the copyright notices, and mark the files you changed.

The libraries this project uses keep their own licenses.

---

<div align="center">

Built in Rust. Self-hosted. Open source.

</div>
