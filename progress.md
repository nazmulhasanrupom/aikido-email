# AIKIDO EMAIL: Progress

Last updated: 2026-10-17 (Auto Database part 1 complete)

## Status in one line

The sending engine (9 phases), the Reply Check (8 phases) and the Follow-up automation (8 phases) are built and tested.
Lead Management (leads, lists, tags, CSV import and export, email check, campaigns with a daily-limit feeder) is built, 9 phases.
The Email Generator (an AI writes the `{{ai:slot}}` parts of a step per lead, with a worker, guard, review mode, budgets, two LLM kinds, a sequence writer and a preview) is built, 8 phases.
Auto Database part 1 (AI column mapping for CSV import: rules, then an AI on the rest, a proposal, a confirm step, saved mappings) is built, 6 phases. 598 tests pass.
**Not yet tested:** a real Gmail or Outlook send, a real Gmail or Outlook inbox read, and Outlook thread headers on a real mailbox (no live accounts yet). Only mock servers, Mailpit and GreenMail (IMAP) were used.
**Not yet tested (Email Generator):** a real call to Anthropic, OpenAI or Ollama. Only a mock HTTP server and `FakeLlm` were used. See "Not done, or not verified (Email Generator)".

Project name : AIKIDO EMAIL
 
DESCRIPTION : 
It is a email automation for any businesses that's sending cold emails and wants to manage it all in one place . Initially , this is a copy of instantly like platforms but made in RUST and OPEN SOURCE
 
FEATURES : 
- Email Sending Automation
- Email Follow Up Automation
- Email Auto Reply Check
- Auto Database ( Connects to any existing database)
- AI Agent and Chatbot
- Email Generator
- Lead Management
 


## The 4 parts of the sending engine

| Part | What it does | Where |
| --- | --- | --- |
| 1. Account store | Saves connected accounts. Every token and password is encrypted (AES-256-GCM) before it reaches the database. | `src/accounts/`, `src/crypto.rs` |
| 2. API keys | One key per scope (`admin`, `accounts`, `sending`). Made on first boot. Only the SHA-256 hash is stored. | `src/auth/`, `src/admin/` |
| 3. Queue | Holds the emails to send. Safe with many workers (Postgres row locks). | `src/sending/queue.rs` |
| 4. Scheduler and worker | Picks the next email per account and sends it, only when every rule passes. | `src/sending/scheduler.rs`, `worker.rs`, `rules.rs` |

## What is inside the project

```
Cargo.toml, Cargo.lock
docker-compose.yml        dev Postgres 16 + Mailpit
.env.example              all settings, documented
migrations/0001_init.sql  the 8 tables, enums, indexes
migrations/0002_reply_check.sql, 0003_followups.sql, 0004_leads.sql, 0005_campaigns.sql, 0006_generator.sql, 0007_import_mapping.sql
docs/API.md               full API reference
progress.md               this file
README.md                 setup guide (Reply Check, Gmail restricted scope)
src/
  followups/              the Follow-up automation (see below)
  leads/                  Lead Management (see below): email, fields, upsert, import, export, verify, hooks, vars, backfill, routes
  leads/mapping/          Import mapping (see "Auto Database part 1"): targets, samples, rules, ai, templates, proposal, routes
  campaigns/              Campaigns (see below): routes, feeder, company stop
  generator/              Email Generator (see below): llm/ (anthropic, openai, fake), settings, slots, guard, prompt, pipeline, worker, hooks, draft, routes
  replies/                the Reply Check (see below)
  main.rs                 CLI (old commands + new `serve`)
  lib.rs                  module list
  config.rs               reads env vars
  crypto.rs               AES-256-GCM, master key, key file
  error.rs                one JSON error shape, request extractors
  state.rs                shared state (pool, crypto, config, clock)
  db.rs                   pool + migrations
  docs.rs                 OpenAPI spec, /docs page, router, CORS, timeout
  auth/
    api_keys.rs           make, hash, check, rotate, revoke, first-boot bootstrap
    middleware.rs         axum layer: checks key and scope
  admin/routes.rs         /v1/keys/*
  accounts/
    model.rs              EmailAccount, Provider, secrets types
    repo.rs               DB read/write, encrypt on save, decrypt on load
    oauth.rs              sealed `state` value for the web OAuth flow
    routes.rs             /v1/accounts/*
  email_management/       YOUR existing connectors (see "Changes to your code")
    gmailconnection/, msconnection/, imapsmtp/, oauth_common.rs
  sending/
    settings.rs           SendingSettings struct + validation
    rules.rs              pure rule checks (take `now` as input)
    queue.rs              enqueue, claim, apply result, stuck check
    scheduler.rs          the tick loop
    worker.rs             send one job, plan the result
    tokens.rs             OAuth token refresh
    routes.rs             /v1/sending/*
    senders/              MailSender trait; smtp, gmail, microsoft, dry_run, fake
tests/
  common/mod.rs           test harness (fake clock, FakeSender)
  api_keys.rs accounts.rs queue.rs engine.rs providers.rs docs.rs followups.rs leads.rs lead_imports.rs lead_links.rs campaigns.rs
  generator.rs generator_api.rs generator_clients.rs generator_logs.rs   (+ common/gharness.rs: the generator test harness)
```

About 10,000 lines for the sending engine, and about 8,000 more for the Reply Check, including tests and docs.

## What we did, in order

| Phase | Built | Result |
| --- | --- | --- |
| 1 | Config, DB pool, migrations, encryption | Encrypt then decrypt returns the same value. A wrong AAD (the account id) fails. A damaged value fails. |
| 2 | API keys: bootstrap, middleware, `/v1/keys` | First boot prints 3 keys. Second boot prints none. A wrong scope gets `403`. |
| 3 | Account store, hooks into `email_management`, `/v1/accounts` | An SMTP account saves. The DB row has only ciphertext. The API shows no secret. |
| 4 | Settings, validation, pure rules | 21 rule tests pass with a fake clock, including DST days. |
| 5 | Queue tables, job routes, idempotency | The same `idempotency_key` twice makes one job. |
| 6 | Scheduler, worker, dry-run sender, SMTP sender | Live run into Mailpit: 3 emails about 60 s apart (with a 60 s test gap). |
| 7 | Gmail sender, Microsoft sender, token refresh | Tested against mock Gmail, Graph and token servers. Not against the real services. |
| 8 | Retries, auto-pause, stuck check, suppressions | Fake errors trigger the right job status and pause. |
| 9 | `/status`, `/preview`, engine pause, OpenAPI `/docs` | A test checks that every route is in the spec with its scope. |

After that: the API reference (`docs/API.md`) and this file.

## Tests (sending engine, before the Reply Check)

Run:

```bash
docker compose up -d
DATABASE_URL=postgres://aikido:aikido@127.0.0.1:55432/aikido cargo test
```

The sending engine had 119 tests. See "Reply Check" and "Follow-up automation" below for the full count now.

| File | Tests | Covers |
| --- | --- | --- |
| unit tests in `src/` | 47 | Encryption, settings validation, rules (weekends, window edges, time zones, DST, limits, warm-up, gap range), outcome planning, sender error mapping |
| `tests/api_keys.rs` | 9 | Boot keys, env keys, 401/403, rotate, grace, last-admin guard |
| `tests/accounts.rs` | 9 | Encrypted storage, upsert, AAD binding, delete, OAuth start and callback, PATCH, old IMAP flow |
| `tests/queue.rs` | 8 | Idempotency, per-item errors, paging, cancel, requeue, suppressions |
| `tests/engine.rs` | 31 | Gap, window, parallel engine copies, stuck, auto-pause, auth failure, retries, bounces, cooldown race, limits, warm-up, pause |
| `tests/providers.rs` | 14 | Mailpit delivery, SMTP codes, Gmail and Graph mocks, token refresh, no secrets in logs |
| `tests/docs.rs` | 1 | Every route is in the OpenAPI spec with its scope |

Two tests skip themselves if Mailpit is not running.

A real bug was found by a test and fixed: two accounts could claim the same lead at the same moment and both send it. Now a per-recipient lock stops this. The test fails if the lock is removed.

## Changes to your existing code (`src/email_management/`)

- Added `login()`, `save_to_file()`, `web_start()`, `web_finish()` and `refresh()` to the Gmail and Microsoft connectors. The old commands (`add`, `list`, `token`, `remove`) still work the same.
- Added shared types to `oauth_common.rs`: `Connected`, `Refreshed`, `RefreshError`.
- Changed the `Res` error type to `Send + Sync`, so the connect functions can run inside HTTP handlers.
- Removed the `google-oauth` crate (nothing used it, and it pulled in OpenSSL). `oauth2` now uses rustls.
- `main.rs` has a new `serve` command. `gmail add`, `outlook add` and `imap add` also save into the encrypted store when `DATABASE_URL` is set.
- The IMAP connector's own file store and `ACCOUNTS_KEY` are unchanged. The engine uses its own key (`AIKIDO_ENCRYPTION_KEY`).

## Decisions that differ from the build map

| What | Why |
| --- | --- |
| Added column `engine_paused` to `sending_defaults` | The global pause must work with many server copies. |
| Added indexes `send_jobs (to_email, sent_at)` and `(status, claimed_at)` | The cross-account cooldown needs the first one. |
| Queries are checked at run time (`sqlx::query`), not by `query!` macros | Fewer build steps. The tests run every query against real Postgres. |
| `key_prefix` is `aik_<scope>_` plus 6 random characters | The first 12 characters alone are just `aik_sending_`. |
| Folder is `email_management/`, not `connectors/email_connector.rs` | It is your existing folder. |
| Hard bounce does not count as a "failure in a row". Other permanent errors do. | The bounce rate is the guard for bad recipients. |
| SMTP checks the connection before `DATA`, so later no-code errors become `Unknown` | Keeps "connection error before DATA = retry" and "timeout after DATA = stuck" exact. |
| `tls: "none"` is allowed for `localhost` only, and no login is tried there | A password must not cross an unencrypted link. |
| Settings changes work on the same day | The gap and today's cap are recomputed when settings change. |
| CORS is open to all origins | The keys travel in headers, not cookies. |

## Reply Check (built 2026-10-17)

It reads the inboxes of connected accounts (read only), finds replies, auto-replies, bounces, unsubscribes and spam complaints
that come back from our sends, and acts on them. No UI. Setup guide: `README.md`. API: `docs/API.md` section 16.

You approved section 11 (changes to the sending engine) by asking me to build the map. I made all 8 changes.

### The 5 parts

| Part | What it does | Where |
| --- | --- | --- |
| 1. Fetchers | One trait, three providers. Gmail API, Microsoft Graph, IMAP. Read only. | `src/replies/fetchers/` |
| 2. Parser and classifier | Pure functions: raw email to one label. | `parse.rs`, `dsn.rs`, `quote.rs`, `classify.rs` |
| 3. Matcher | Finds the `send_jobs` row an email answers (6 methods). | `matcher.rs` |
| 4. Action runner | Suppress, cancel, mark bounce, recheck bounce rate. One transaction with the insert. | `actions.rs`, `ingest.rs` |
| 5. Poller and sync | The timer loop, claim with `SKIP LOCKED` and a 10 minute lease, cursors, error handling. | `poller.rs`, `sync.rs`, `repo.rs` |
| Settings and API | `ReplySettings` and 16 routes under `/v1/replies/*`. | `settings.rs`, `routes.rs` |

### Phases

| Phase | Built | Result |
| --- | --- | --- |
| 1 | Migration `0002_reply_check.sql`, scope `replies`, `ReplySettings` + validation | Old database gets only the new `replies` key. Bad settings get `422`. |
| 2 | Parser, DSN reader, quote remover, classifier, 26 `.eml` fixtures, `/classify` | 15 classifier tests pass, no DB, no network. |
| 3 | Matcher + indexes | 7 matcher tests on real Postgres. |
| 4 | Actions, manual jobs, bounce-rate rewrite (`sending/health.rs`) | Reply cancels the follow-up. Same email twice (also at the same time) runs actions once. Late bounces pause the account (`bounce_rate`). |
| 5 | IMAP fetcher, sync, poller, `FakeFetcher` | 7 GreenMail tests and 18 fake-fetcher tests pass. Mail stays unread (`\Seen` checked, with a control). |
| 6 | Gmail and Microsoft fetchers, `Message-ID` readback in both senders | 13 mock-server tests pass (paging, spam, 401 refresh, revoked token, scope, rate limit, size cap, read-only methods). |
| 7 | Follow-up safety gate | Follow-up waits while the sync is stale, sends when fresh. |
| 8 | Remaining routes, status, relabel, lookup, OpenAPI | Every route is in `/docs` with its scope (CI test). |

### Tests

228 tests, 0 failures, 0 compiler warnings. New files: `classifier.rs`, `matcher.rs`, `replies_settings.rs`, `replies_actions.rs`
(includes the follow-up gate), `replies_sync.rs`, `replies_imap.rs` (needs GreenMail, skips if it is down), `replies_providers.rs`, `replies_routes.rs`.

### Changes to the sending engine (section 11)

1. Scope `replies` and key `AIKIDO_REPLIES_API_KEY`.
2. OAuth scopes: the connectors already asked for `gmail.readonly` and `Mail.Read`. The granted scopes are now saved in `email_accounts.granted_scopes`; `can_read` follows them.
3. `POST /v1/sending/jobs` accepts `"kind": "manual"`. A manual job skips only the `replied` suppression.
4. Bounce rate reads `bounced_at` and `bounce_kind` on the last `bounce_window` jobs. A send-time hard bounce sets them too. Pause reason starts with `bounce_rate:`.
5. Follow-up gate: new sending setting `reply_check_max_age_mins` (default 15, 0 = off). `/status` shows `reply_check_stale` or `reply_check_unavailable`.
6. `Message-ID` check: after a send, Gmail (read back) and Microsoft (sent-item lookup, then subject + recipient) return the real id. It is saved in `send_jobs.message_id_header`.
7. New indexes on `send_jobs`.
8. Account delete wipes `subject` and `reply_text` of inbound mail and deletes `inbox_sync_state` rows.

### Decisions that differ from the map

| What | Why |
| --- | --- |
| Wrong IMAP password: `can_read = false`, `read_status = login_failed`. The account is NOT set to `disconnected`. | The SMTP login may still work. Disconnecting would stop sending for a read problem. OAuth accounts are disconnected, as the map says. |
| The poller also reads an account that has a queued follow-up, even if it has no send in `match_window_days`. | Else that follow-up would wait for a sync that never runs. |
| Bounces, complaints and delayed notices with no match are still stored (only `reply`, `auto_reply`, `unsubscribe` are dropped). | Step 7 of the map names only those three labels. |
| Microsoft `conversationId` is used as the thread id (method `thread`). | The Microsoft sender already saves it in `provider_thread_id`. |
| Gmail is listed in 1 day slices; the cursor is "listing time minus 10 minutes". | The list call gives ids without dates, so it cannot be cut by count. Slices keep a long catch-up stoppable. |
| Added a 4th trait method pair `initial_cursor` and `cursor_after` to `MailFetcher`, and `close`. | Needed to save a clean cursor when the download budget ends, and to `LOGOUT` IMAP. |
| `reply_defaults` has an `engine_paused` column. | Same reason as `sending_defaults.engine_paused`: it must work with many server copies. |
| Soft bounce with an unknown recipient is never counted toward a hard suppression. | "Never hard-suppress on a guess." |

## Follow-up automation (built 2026-10-17)

A sequence is 1 to 10 steps (first email, then follow-ups). An enrollment puts one recipient in one sequence, sent from one account.
It stops at once on a reply, bounce, unsubscribe or complaint. An out-of-office reply pauses it (3 business days by default), then it goes on.
No UI. API: `docs/API.md` section 17. Setup: `README.md`.

You said "build the below", so I took my picks in sections 12 and 13 of the build map as approved (as with the Reply Check), except Outlook threading (see "Decisions that differ").

### The main idea

A step becomes a `send_jobs` row **only when it is due**. A reply between two steps then needs no job to cancel. The sending engine applies all its rules to every step.

### The parts

| Part | What it does | Where |
| --- | --- | --- |
| Template | `{{name\|fallback}}`, built-in `email`, `sender_name`, `sender_email`. Pure. HTML values escaped. | `src/followups/template.rs` |
| Due math | Business days (skip Sat and Sun in the account time zone), calendar days, DST safe. Pure. | `schedule.rs` |
| Scheduler | Tick loop: resume ended pauses, fail stalled follow-ups, make the job of each due step. One transaction per enrollment, `FOR UPDATE SKIP LOCKED`. | `scheduler.rs` |
| Lifecycle | stop, pause, resume, `on_job_result`, `on_lead_stopped`, `on_account_deleted`. | `lifecycle.rs` |
| Enroll | Up to 1000 per call, per-item errors, idempotency key, one open enrollment per address. | `enroll.rs` |
| Settings + API | `FollowupSettings` and 17 routes under `/v1/followups/*`. | `settings.rs`, `routes.rs` |

### Phases

| Phase | Built | Result |
| --- | --- | --- |
| 1 | Migration `0003`, scope `followups`, `followup_defaults`, settings validation | Old database gets only the new `followups` key. Bad settings get `422`. |
| 2 | Template render, sequences + steps routes, preview | Render tests need no DB. Missing variable gets `422`. |
| 3 | Enroll route, idempotency, `one_active_per_email` | Same key twice makes one enrollment. Same email in a second sequence is refused. Two calls at the same moment enroll once. |
| 4 | Scheduler, due math, business days | Friday + 1 business day = Monday. Two scheduler copies make one job per enrollment (40 enrollments, 3 rounds). |
| 5 | Thread headers | Mailpit shows `In-Reply-To` and `References` on the follow-up. `FakeSender` sees the right headers and `thread_id`. Gmail and Graph sender tests (older) already check `threadId` and the MIME headers. |
| 6 | `on_job_result` | A 3-step sequence runs to `finished`. A late step 1 moves steps 2 and 3. Retry, permanent error, hard bounce, suppression, cancel all handled. |
| 7 | `on_lead_stopped` | A reply between step 1 and step 2 means step 2 is never made. Auto-reply pauses, then resumes. Same reply 3 times runs once. |
| 8 | Remaining routes, status, OpenAPI, docs | Every route is in `/docs` with its scope (CI test). |

### Tests

349 tests, 0 failures, 0 compiler warnings (clippy shows only style hints). New: `tests/followups.rs` (78 tests) and 25 unit tests
(template 13, due math 7, follow-up settings 5). The list from section 16 of the map is covered:

| Test from the map | Test name (in `tests/followups.rs`) |
| --- | --- |
| Reply while step 2 is queued | `a_reply_while_step_two_is_queued_cancels_the_job_and_stops_the_row` |
| Reply after the worker claimed it | `a_reply_and_the_claim_of_the_next_step_meet_at_the_same_moment` (the stop holds the enrollment lock, the claim leaves the job; if the lock check is removed this test hangs), `a_reply_after_the_worker_claimed_the_step_only_stops_what_comes_next` |
| Sync is stale | `a_step_waits_while_the_inbox_sync_is_stale_and_goes_when_it_is_fresh` |
| Account paused for 8 days | `an_account_paused_for_eight_days_fails_the_follow_up_as_stalled` |
| Step edited after step 1 sent | `a_step_edited_after_step_one_went_out_uses_the_new_text` |
| Suppressed at due time | `a_suppressed_address_at_due_time_gets_no_job` |
| DST between steps | `the_due_time_keeps_the_local_clock_over_a_dst_change` (+ unit tests) |
| No secret and no email text in logs | `no_secret_and_no_email_text_reach_the_logs` |

### Changes to existing code (section 12 of the map)

1. Scope `followups` and key `AIKIDO_FOLLOWUPS_API_KEY`. First boot now makes 5 keys (tests updated).
2. `send_jobs` got `enrollment_id` and `step_number`. **`in_reply_to` and `references` already existed** from `0001`, so they were not added again.
3. The senders needed **no change**: SMTP, Gmail and Microsoft already send `In-Reply-To`, `References`, and (Gmail) `threadId`.
4. `apply_plan` (the worker's result writer) calls `followups::on_job_result` in the same transaction. `queue::cancel` and the "suppressed" skip in `claim_next` do too.
5. Queue claim order: `priority`, then follow-ups before first emails (sending setting `followups_first`, default `true`), then oldest.
6. Reply Check: `cancel_queued` no longer touches jobs of an enrollment. `followups::on_lead_stopped` (now async, in the same transaction) stops or pauses and cancels them. It is also called for auto-replies and `bounce_block` now. New `actions` entries: `enrollments_stopped`, `enrollments_paused`.
7. `claim_next` takes the enrollment row lock without waiting (`SKIP LOCKED`). A stop that holds it wins; the job is looked at again on the next tick.
8. The follow-up safety gate now also covers any job of step 2 or later (before: only jobs with `in_reply_to`), so an unthreaded follow-up is gated too.
9. Account delete fails the open enrollments of that account (`account_deleted`).

### Decisions that differ from the map

| What | Why |
| --- | --- |
| **Outlook threading: MIME headers, not Graph `reply` (map pick A, option 1).** | The Microsoft sender already sends MIME, which carries `In-Reply-To` and `References`. Graph `reply` adds quoted text, needs more permission checks, and I cannot test either way without a live mailbox. A web search did not confirm that Exchange keeps these headers, and one report says `conversationId` may still change. **Check on a real Outlook account.** If threads break, build option 1. |
| No `claimed_at` lease column on `enrollments`. | Each enrollment is handled in one transaction with a row lock. A crash rolls back. |
| Added `enrollments.step_queued_at`, `followup_defaults.last_tick_at`. | The stall check needs the queue time (with the fake clock in tests). The status route shows the last tick. |
| `followup_defaults` has `settings jsonb` + `engine_paused` (like the other two engines), not separate columns. | Same pattern as sending and replies. |
| "One open enrollment per address" is checked in code (advisory lock), not by a unique index. | A unique index cannot follow a setting (`one_active_per_email`). |
| A missing variable blocks the **whole** enroll call (`422`, nothing enrolled). Other problems are per-item errors. | The map says both "blocks the call with 422" and "per-item errors"; this keeps both. |
| Stall applies to step 2 and later only. | Step 1 of a big batch (1000 leads, 30 a day) waits longer than 7 days on purpose. |
| A step made again (paused while queued) gets key `fu:<enrollment>:<step>:<n>`. | The cancelled job still holds the first key. |
| `archive` is non-destructive: it works like pause plus no new enrollments, and can be resumed. | The map does not say what happens to open enrollments. Stopping them felt too harsh. |
| A pause (by hand or auto-reply) cancels a queued job. A job already `sending` cannot be called back. | Same limit as for a stop. |
| Step delay must be 1 to 365 for step 2 and later. | The map gave no range. |

## Lead Management (built 2026-10-17, all 9 phases)

One row per email address. Custom fields, lists, tags, bulk actions, CSV import and export, an MX email check, a timeline per lead, and **campaigns**:
a sequence, a pool of sender accounts, a set of leads and a daily limit of new leads, fed in by a loop. No UI.
API: `docs/API.md` sections 18 and 19. Build map: "Lead Management Build Map". You approved section 12 as written (change 3 as fixed), decisions A (live variables) and B (erase plus suppress), and one addition (the custom field delete guard).

### The parts

| Part | What it does | Where |
| --- | --- | --- |
| Email | Trim, lower case, syntax check. Free-mail list. Pure. | `src/leads/email.rs` |
| Fields and input | Custom field keys and value checks (text, number, url, date), tags, one lead from the outside world. | `fields.rs`, `input.rs` |
| `upsert_batch` | The one function that writes leads in bulk: 1 statement per batch, 3 duplicate modes, timeline events. Auto Database will call it later. | `upsert.rs` |
| Filter and queries | One filter for list, export, bulk actions and campaigns. Keyset paging. Erase (delete + suppress). | `query.rs`, `repo.rs` |
| Email check | MX lookup (`hickory-resolver`), 7-day cache, 3 s timeout, 20 at a time. A fake resolver for tests. | `verify.rs` |
| Import worker | Claims an import (`SKIP LOCKED` + lease + claim token), streams the CSV, 1000 rows per transaction with the counters. | `import.rs` |
| Export | Streamed CSV in pages of 1000. Formula cells escaped. | `export.rs` |
| Live variables | The values of a step: field fallbacks < the enroll snapshot < the current lead fields. | `vars.rs`, `followups/scheduler.rs` |
| Lead hooks | What the other engines tell the leads: enrolled, sent, ended, reply/bounce/unsubscribe/complaint. Each runs in a savepoint and never blocks the caller. | `hooks.rs` |
| Backfill | Old enrollments get a lead. `cargo run -- backfill-leads`, also at `serve` start. | `backfill.rs` |
| Campaigns | Create, add leads, launch checks, pause, resume, archive, stats, feeder control. | `src/campaigns/routes.rs` |
| Feeder | The tick loop: daily limit, skip reasons, account pick, complete. | `src/campaigns/feeder.rs` |
| Company stop | A reply stops the colleagues at the same company (not free mail). | `src/campaigns/company.rs` |
| API | 26 routes under `/v1/leads/*` (scope `leads`), 15 under `/v1/campaigns/*` (scope `campaigns`). | |

### Phases

| Phase | Built | Result |
| --- | --- | --- |
| 1 | Migration `0004`, scopes `leads` and `campaigns`, settings, field routes | Old database gets only new tables (a test checks the table list and that `enrollments` did not change). Bad field key gets `422`. |
| 2 | Create, read, update, delete, search, filters, paging | 10000 seeded leads: text search under 100 ms (test), trigram index usable (test). Same email in other case gets `409`. |
| 3 | Lists, tags, bulk actions, notes, timeline | A filter action changes only the matching rows. An empty filter is refused. |
| 4 | JSON bulk, CSV import worker, mapping guess, preview, 3 modes, cancel, export | 100000 rows import in about 20 s (debug build). A killed worker resumes with right counts. Export escapes `=cmd`. |
| 5 | Syntax and MX check, `email_domains` cache, `verify` action | A domain with no MX gets `no_mx`. 998 leads on one domain make 1 lookup. A timeout stays `syntax_ok`. |
| 6 | Changes 1, 2, 4 to 9 of section 12, backfill, the field delete guard | A reply on an enrolled lead sets `replied` and writes an event. Backfill twice makes no duplicates. Migration `0005` runs on an old database that has dangling ids (test). |
| 7 | Campaign tables, create, add leads, launch checks, pause, resume, archive | Launch with a missing variable gets `422` with the names. The capacity warning shows. |
| 8 | Feeder: daily limit, account pick, skip reasons, company stop, complete | Fake clock: 120 pending, limit 50: 50 today, 50 tomorrow, 20 the day after. A `gmail.com` domain never triggers the company stop. |
| 9 | Live variables (change 5), context route, stats, status, OpenAPI docs, `docs/API.md`, this file | All routes are in `/docs` (CI test). Full suite passes. |

### Tests

479 tests, 0 failures, 0 compiler warnings (clippy shows only style hints). New in this step: `tests/leads.rs` (27), `tests/lead_imports.rs` (19), `tests/lead_links.rs` (24), `tests/campaigns.rs` (46),
and unit tests (email 4, fields 4, settings 4, export 1), plus 1 in `tests/api_keys.rs`. Tests use a fake DNS resolver (`AppState::mx`), never real DNS.

Tests from section 16 of the map:

| Test from the map | Where |
| --- | --- |
| Email normalize | `create_normalizes_email_and_refuses_the_same_email_in_other_case`, unit test in `email.rs` |
| `meeting_booked` keeps its value after a reply | `a_reply_sets_replied_logs_an_event_and_keeps_the_interest` |
| Feeder and import on 2 server copies: nobody enrolled twice | `two_feeders_at_the_same_moment_enroll_each_lead_once`, `a_retry_after_a_crash_does_not_enroll_twice`, `two_workers_at_the_same_time_write_each_row_once` |
| Delete a lead with an open enrollment | `deleting_a_lead_stops_its_enrollment_and_cancels_the_queued_job` |
| A suppressed lead ends `skipped`, `suppressed` | `skipped_leads_do_not_use_up_the_daily_limit_and_say_why` |
| An account leaves the pool: open enrollments keep it | `removing_an_account_from_the_pool_leaves_open_enrollments_alone` |

Two real bugs were found by these tests and fixed: round robin used the same account for a whole tick (it read the last account once), and the feeder called a lead "busy elsewhere" when a copy had died right after making its enrollment.

### Changes to existing code (section 12 of the map)

1. Migration `0005`: FK `enrollments.lead_id` to `leads` (`ON DELETE SET NULL`). Old `lead_id` values were cleared first (nothing existed to point at).
2. FK `sequences.campaign_id` to `campaigns` and a unique index (one sequence per campaign). Old `campaign_id` values were cleared first. **`campaign_id` in the body of `POST` and `PATCH /v1/followups/sequences` now gives `422 campaign_managed`:** a campaign sets it.
3. As fixed by you: no `campaign_id` column on `enrollments`; `sequences.campaign_id` is the link.
4. The enroll route takes `lead_id` in place of `to_email`. With only an address, the lead is found by address or made (source `enrollment`). `enrollments.lead_id` is always set. A suppressed address makes no lead.
5. Live variables (decision A): `render_variables` in `src/leads/vars.rs`. The scheduler reads the lead at send time.
6. `followups::on_lead_stopped` calls `leads::hooks::on_outcome`.
7. `queue::apply_plan` calls `leads::hooks::on_sent` for every sent job (sequence step or manual send to a known lead).
8. `lifecycle::finish` and `end` call `leads::hooks::on_enrollment_ended` (finished with no reply: `completed`).
9. Backfill command and startup step.
10. Your addition: `DELETE /v1/leads/fields/{key}` gives `409 field_in_use` while a sequence with an open enrollment (archived ones too), or the sequence of an active or paused campaign, uses `{{key}}`. The message names the sequences.

### Decisions that differ from the map

| What | Why |
| --- | --- |
| Migration `0004` holds the **lead** tables. Campaign tables and changes 1 and 2 are `0005`. | Your OK. Phase 1 says "old database gets only new tables". |
| `0004` adds the value `erased` to the enum `suppression_reason`. | Decision B needs it. Your OK. |
| `lead_defaults` has `settings jsonb` (plus `feeder_paused`, `last_tick_at`), like the other three engines. | Same pattern. A new setting needs no migration. |
| `lead_imports` has extra columns `rows_done`, `claim_id`, `attempts`, `error`. | Safe resume after a crash. Your OK. |
| The cursor of `GET /v1/leads` is the lead `id`. A deleted cursor lead gives `400 invalid_cursor`. | Your instruction. |
| The MX check waits for DNS inside the request (10 s for create and bulk, 25 s for the `verify` action). Leads are saved first. | Simple, and no extra background job. A slow domain stays `syntax_ok`. |
| Import `skipped` counts: an existing lead that did not change (any mode), and a repeat of an email inside the file. `updated` only counts leads that really changed. | A second run of the same file shows `skipped`. |
| `PATCH` of a custom field cannot change `kind`. Deleting a field removes its value from every lead. | Existing values would no longer fit. "Keys must exist in `lead_fields`" must stay true. |
| The body limit of the import routes is 201 MB. The real limit is the setting `import_max_bytes` (default 50 MB). | The multipart file is held in memory once. Rows are streamed. |
| `campaign_leads.skip_reason` has two more values: `missing_variable`, `campaign_archived`. A deleted lead is removed from `campaign_leads` by the foreign key (`ON DELETE CASCADE`), so `lead_deleted` is never used. | A lead without a value for a step cannot be enrolled (the enroll call refuses a whole call on a missing variable); it needed a reason. |
| `campaigns` has two extra columns: `last_account_id` (round robin) and `warning` (why the feeder is stuck). | The map says "write a warning to the campaign status" and "the next account after the last one used". |
| A new sequence made with a campaign starts `paused`. A sequence you pass by `sequence_id` keeps its status until launch. | A draft campaign must not run. Pausing someone else's sequence with open enrollments would be a surprise. |
| `archive` also archives the campaign's sequence. Pending leads become `skipped`. | The map says only that pending leads are skipped. |
| A campaign with no lead at all never completes. | Otherwise `launch` on an empty campaign would complete it at once. |
| The custom field delete guard also looks at sequences of active and paused campaigns. | The feeder will enroll more leads into them. |
| `build_vars` accepts up to 150 values (the caller's 50 stay the limit at enroll). | The lead's fields are added to the caller's values. |
| The company stop writes a `stopped` event on the stopped leads, with reason `company_replied`. | So the timeline says why. |
| `POST /v1/leads/bulk` and the import take `campaign_id`. | Handy, and the map has it for the import. |

### Not done, or not verified (leads and campaigns)

- **Real DNS:** the `hickory-resolver` code against a real resolver is not tested (only the fake). Try `POST /v1/leads` with a real domain (expect `mx_ok`) and a made-up one (expect `no_mx`).
- **pg_trgm:** the database user must be allowed to run `CREATE EXTENSION pg_trgm` (a normal Docker Postgres is fine; some managed databases need it enabled by hand).
- **Search at 1M leads:** not measured. At 10000 leads the trigram index can serve the query; the planner chooses by cost.
- **A real campaign on live Gmail or Outlook accounts:** not run. The feeder and the company stop were tested with fake senders and the reply fixtures.
- **Feeder speed:** it enrolls one lead per transaction (about 50 per second on a dev machine). A daily limit of 5000 takes a couple of minutes.
- **Lead delete and the Reply Check:** after a lead is erased, its old inbound emails stay (without `lead_id`). The map asks only for the lead and its timeline.
- Not in v1, as the map says: UI, Auto Database (it will call `leads::upsert::upsert_batch` with source `auto_database`), paid verification, scoring, merge, sending in the lead's time zone (the `timezone` field is stored only).

## Email Generator (built 2026-10-17, all 8 phases)

An AI writes the personal part of a step, for each lead. A step holds slots like `{{ai:opener}}`; each slot has an instruction, a word limit and a fallback.
A worker fills the slots a few hours before the step is due. The follow-up scheduler then sends the step like any other. Your words stay around the slots (the signature, the offer and the unsubscribe line are always yours).
No UI. API: `docs/API.md` section 20 (20 routes under `/v1/generator/*`, scope `generator`). Build map: "Email Generator Build Map". You said "build the below", so I took the picks in section 5 (A1 slots, B1 just before due) and section 13 (changes to existing code) as approved, as with the earlier maps.

### The parts

| Part | What it does | Where |
| --- | --- | --- |
| LLM clients | One trait `LlmClient`. `anthropic` (Messages API, `cache_control` on the fixed prompt part), `openai_compatible` (`{base_url}/chat/completions`), `fake` (tests). Retry with back-off and `Retry-After`. Plain `reqwest`. | `src/generator/llm/` |
| Providers | Stored in `llm_providers`. Key encrypted with AES-256-GCM, AAD = provider id, write only. Status `ok` / `auth_failed`. | `providers.rs`, `routes.rs` |
| Slots and hash | `{{ai:key}}` / `{{ai:key\|fallback}}` in the template. `ai_slots` on a step. `step_hash` over text + slots. | `slots.rs`, `hash.rs`, `followups/template.rs` |
| Guard | 9 pure checks on every output (count, links, email, phone, placeholders, AI talk, markdown, spam words, language). | `guard.rs` |
| Prompt and parser | Fixed part first (same bytes for every lead of a step), lead part last inside a `<lead_data>` block that lead text cannot close. JSON reader. | `prompt.rs`, `parse.rs` |
| Pipeline | Prompt, call, parse, guard, one repair try. No database. | `pipeline.rs` |
| Lead context | `leads::context::build` (moved out of the route; the route output is the same). The generator asks for less: 20 events, 2 sent emails, no reply text. | `src/leads/context.rs` |
| Worker | Tick loop: stale marking, discovery (unique index), claim with `SKIP LOCKED` + 10 min lease, budgets, call, save with the claim id, one `llm_calls` row per call. | `worker.rs` |
| Hooks | Engine to generator: cancel on stop/fail/finish, stale on resume (each in a savepoint). Generator to scheduler: `ai_text` (use, hold, fallback, fail). | `hooks.rs` |
| Preview and writer | `POST /preview` (one step, one lead, now, not saved). `POST /sequences/draft` (brief in, steps out, `save: true` makes a paused sequence). | `draft.rs` |
| Review and routes | Generations list/get/edit/approve/reject/bulk, usage, status, pause, campaign overrides. | `generations.rs`, `routes.rs` |

### Phases

| Phase | Built | Result |
| --- | --- | --- |
| 1 | Migration `0006`, scope `generator`, `GeneratorSettings`, provider store (encrypted), `FakeLlm` | Old database gets only the new tables and the new key (tests). Bad settings get `422`. The key is never returned; moved to another provider id it can not be read (AAD). |
| 2 | Anthropic and OpenAI-compatible clients, retries, test route | Mock server: good answer, 429 + `Retry-After` (waits), 500 retry, 401/403 not retried, 400 not retried, timeout, closed port, non-JSON answer. A refused key sets `auth_failed`; a new key or a good test clears it. |
| 3 | `{{ai:...}}` syntax, `ai_slots`, subject rule, field delete guard | `unknown_ai_slot`, `unused_ai_slot`, `ai_slot_in_threaded_subject`, `invalid_ai_slots` all `422`. Old templates render the same (the old suites still pass). |
| 4 | Context function, prompt builder, parser, guard | Unit tests, no DB, no network. Fixed part is the same for two leads. The data block can not be closed from the inside. |
| 5 | Preview route, sequence writer | Preview with `FakeLlm` returns the full email and saves only an `llm_calls` row. Draft with `save: true` makes a paused sequence. |
| 6 | Worker, budgets, `llm_calls`, cancel and stale hooks | Two worker copies, 40 enrollments, 3 rounds: exactly 40 calls. A dead lease is picked up. Daily and campaign budgets stop the calls. |
| 7 | Scheduler hook, `on_failure`, review mode, launch check | Ready text goes into the job. Fallback, wait, grace and hand edit paths. Review holds the step until approval. A reply between steps cancels the text and no call is made. |
| 8 | Remaining routes, usage, status, OpenAPI, `docs/API.md` section 20, README note, this file | Every route is in `/docs` (CI test). Full suite passes. |

### Tests

585 tests, 0 failures on a quiet machine, 0 compiler warnings (clippy shows only style hints). New: `tests/generator.rs` (28), `tests/generator_api.rs` (17), `tests/generator_clients.rs` (15), `tests/generator_logs.rs` (1), 2 in `tests/api_keys.rs`,
and about 50 new unit tests (guard, prompt, pipeline, settings, slots, hash, parser, client readers, draft, template). The test harness is `tests/common/gharness.rs`.

Tests from section 16 of the map:

| Test from the map | Where |
| --- | --- |
| Template parse | `an_ai_slot_parses_with_or_without_an_inline_fallback...`, `bad_ai_slots_get_422`, unit tests in `template.rs` |
| Guard (unit, many cases) | `guard.rs`: one test per rule |
| Prompt builder (unit) | `prompt.rs`: `the_fixed_part_is_the_same_for_two_leads...`, `..._cut_at_the_limit`, `..._cannot_close_the_data_block...` |
| Clients (mock server) | `tests/generator_clients.rs` |
| Worker: only in the look-ahead; two copies; lease | `a_step_is_written_only_when_it_is_due_within_the_look_ahead`, `two_worker_copies_make_one_call_per_step`, `a_lease_that_ran_out_is_picked_up_again...` |
| Budget | `over_the_daily_budget_no_call_is_made...`, `the_budget_stops_the_calls_and_the_next_day_starts_them_again`, `a_campaign_budget_stops_the_calls_of_that_campaign_only` |
| Scheduler hook | `a_ready_text_is_used...`, `no_text_and_on_failure_fallback...`, `on_failure_wait_holds_the_step...`, `the_grace_time_gives_the_worker_a_chance...` |
| Step edited after generation | `a_step_edited_after_generation_makes_the_text_stale_and_a_new_one_is_used`, `the_old_text_is_never_used_for_an_edited_step` |
| Reply between steps | `a_reply_between_steps_cancels_the_text_and_no_call_is_made_for_that_step`, `a_reply_before_the_text_is_written_means_no_call_at_all` |
| Review mode | `in_review_mode_the_step_waits_for_approval_and_then_sends`, `a_rejected_text_is_made_again`, `a_campaign_can_override_the_review_mode` |
| Lead erase | `erasing_a_lead_deletes_its_generations` |
| Logs | `no_key_no_prompt_text_and_no_slot_text_reach_the_logs` |
| OpenAPI | `tests/docs.rs` (21 new routes) |

### Changes to existing code (section 13 of the map)

1. Scope `generator` and key `AIKIDO_GENERATOR_API_KEY`. First boot now makes 8 keys (tests updated; a new test checks an old database gets only the new key).
2. Template parser: `{{ai:key}}` and `{{ai:key|fallback}}`. `variables`, `required` and `used` skip AI slots. A caller value can not be named `ai:...` (a `:` is not allowed in a name).
3. Subject rule: a slot in the subject only on step 1 (`422 ai_slot_in_threaded_subject`).
4. Steps routes take `ai_slots`; the step hash covers text and slots and is saved in `sequence_steps.step_hash`. Sequences take `brief` (`POST` and `PATCH`).
5. Follow-up scheduler: a due step with slots asks `generator::hooks::ai_text`. Hold, fallback or fail with `generation_failed`. New enrollment column `wait_reason`. The stall check is untouched (a held step is `active`, not `waiting_send`).
6. Enroll check skips `ai:` keys. Launch of a campaign with slots needs a working slot provider (`422 generator_not_ready`). The campaign `warnings` get a token estimate.
7. `leads::context::build(conn, id, opts)`; the route calls it. Output unchanged.
8. Hooks: `lifecycle::end` and `finish` cancel open text, `resume` makes old text stale. Each runs in a savepoint (`leads::hooks::guarded`, now `pub(crate)`).
9. Field delete guard also looks in slot instructions and fallbacks.
10. Follow-up preview shows `[AI: key]`, or with `generate: true` and `lead_id` calls the generator.

### Decisions that differ from the map

| What | Why |
| --- | --- |
| New setting `due_grace_mins` (default 5). A due step with no AI text yet is held this long before `on_failure` applies. `0` gives the exact map behavior. | With the map's rule, step 1 of a new enrollment (due the moment it is made, before the worker runs) would almost always go out on the fallback. The grace gives the worker time. |
| A `rejected` text holds the step (like `pending_review`), with no time limit. | A person just acted on it. A step must not go out on the fallback behind their back while the new text is written. |
| Module is `pipeline.rs`, not `gen.rs`. | `gen` is a reserved word in Rust 2024. |
| `generator_campaigns` table and `GET`, `PATCH /v1/generator/campaigns/{id}` (20 routes, not about 16). | The campaign override (`review_required`, `token_budget`) needs a home. A separate table leaves the `campaigns` table and its routes alone. |
| Provider has `json_mode` (default true). | Some OpenAI-compatible servers refuse `response_format`. |
| New settings `tick_secs` (10) and `prices`. | The worker speed; `prices` is the optional money view of `/usage`. |
| A slot that is defined but not used is `422 unused_ai_slot`. | It would cost tokens for text that goes nowhere. |
| Migration `0006` adds two columns to `enrollments` (`wait_reason`, `wait_since`). The old column-count test (16) now says 18. | The map names the wait reason on the enrollment but no place for it. |
| `sequence_steps.step_hash` is a saved column. | The worker's SQL must find rows whose hash is no longer the step's hash. |
| Hand edit (`PATCH .../generations/{id}`) sets status `approved`. | A person wrote or read it. It also lets a `failed` text be rescued. |
| Sequence writer: `delay_days` (the caller's value wins), step 2 and later always get an empty subject, output limit 4000 tokens (a constant). | The map does not say how delays work. An empty subject is what keeps a thread. |
| The Anthropic client leaves out `temperature`, sends `effort: low` and adds 1024 to `max_tokens` for the newer model families (Opus/Sonnet/Haiku 5.x, Fable, Mythos, Opus 4.7/4.8). | Those models refuse a custom temperature and think by default; thinking would eat a 400 token limit. **Not checked against the real API.** |
| API errors that come from the model carry the code `generation_<reason>` (`generation_guard_failed`, `generation_server_error`, ...). `422` for a permanent failure, `502` when the provider could not answer. | One clear prefix. |
| Token estimate in the launch warnings: waiting leads x steps with slots x (1500 + `max_output_tokens`). | A rough upper bound. The real input is smaller when the provider caches. |
| The email address and the phone number of the lead are not in the prompt, and the guard refuses them in the output. | The model has no reason to know them. |

### Not done, or not verified (Email Generator)

- **A real call** to Anthropic, OpenAI, OpenRouter or Ollama: not made. Do one by hand: `POST /v1/generator/providers/{id}/test`, then one `POST /v1/generator/preview` on a real lead.
  The Anthropic request shape (system block with `cache_control`, `output_config.effort`, no `temperature` for new models, `max_tokens` head room) comes from the API docs, not from a live answer.
- **Quality of the text.** Turn on `review_required` for the first campaign and read 20 outputs before you trust it.
- **Prompt cache savings.** Check `cached_tokens` in `GET /v1/generator/usage` on a real provider. A cache needs a minimum prompt size (it depends on the model); a short step may not cache at all.
- **Fact errors** by the model. The guard cannot catch them. Review mode can.
- Tries multiply: the client tries a call up to `max_attempts` times, and the worker claims a row up to `max_attempts` times, so one text can cost up to 9 HTTP tries when a provider is down. Lower `max_attempts` if that is too many.
- A failed call has no token count (`0`), so a provider that fails after doing work is not counted in the budgets.
- The daily budget is checked before each round of calls, not in the middle of one: with `concurrency` 4, up to 3 calls can pass it.
- Provider batch APIs (about half the price, but slow), web research about the lead, AI reading of replies, A/B tests between AI versions, UI: not in v1, as the map says.
- `tests/leads.rs::search_10000_leads_is_fast_and_uses_the_trigram_index` asserts a 100 ms search. It failed once while the whole suite ran on a busy machine and passes alone. It is an old test, not part of this build.

## Auto Database part 1: AI column mapping (built 2026-10-17, all 6 phases)

An uploaded CSV can have any column names. The server reads the headers and up to 5 real values per column, maps the sure columns by rules,
asks a model about the rest, and returns one proposal. Nothing goes into `leads` until the caller confirms. The import worker is the same as before.
No UI. API: `docs/API.md` section 21 (5 new routes under `/v1/leads/*`, scope `leads`). You said "build the below", so I took the picks A1 (masked samples) and B1 (AI may propose `new_field`) and section 9 (changes to existing code) as approved.

### The parts

| Part | What it does | Where |
| --- | --- | --- |
| Targets | The list the AI can pick from (built-in columns with a description, plus every custom field). Edit the descriptions here. | `src/leads/mapping/targets.rs` |
| `SampleSet` + masking | Per column: header, 5 real values (first non-empty rows, max 500 rows read, cut at 100 chars), empty count. Masks `j***@acme.com` and `+1 *** *** 0101`. The mappers take this, never a CSV. | `samples.rs` |
| Rule mapper | The old "mapping guess" moved here, with more aliases and an email value check (4 of 5 samples). Shared by the CSV path and the later database path. | `rules.rs` (`import::guess_mapping` is a re-export) |
| AI mapper | One call (plus one repair try, 30 s timeout), fixed prompt first, data last in a `<csv_data>` block a cell cannot close. Guard: bad target to `skip`, existing `new_field` key to that field, bad key fixed or skipped. | `ai.rs` |
| Propose | Template, else rules, else AI on the open columns (max 100), then one pass so a target has one column. Saved in `lead_imports.proposal`. | `mapping/mod.rs` |
| Templates | `import_mapping_templates`, keyed by the SHA-256 of the lower-cased headers in order. Second upload of the same headers: no model call. | `templates.rs` |
| Routes | propose, get mapping (`raw_samples`), confirm, list and delete templates. `POST /v1/leads/imports?auto_map=true`. | `mapping/routes.rs`, `import.rs` |

### Phases

| Phase | Built | Result |
| --- | --- | --- |
| 1 | Migration `0007`, 6 settings, status `uploaded`, target list | Old database gets only the new column, table and enum value. Bad settings get `422`. |
| 2 | `SampleSet`, masking | Unit tests: sparse column gets its real values, 100-char cut, masks. |
| 3 | Rule mapper moved, aliases, email value check | Old import tests pass. "E-mail Addr" maps to `email`. |
| 4 | AI mapper with `FakeLlm` | Bad key becomes `skip`, two `email` columns keep one, bad JSON falls back to rules, a cell cannot close `<csv_data>`. |
| 5 | Routes | Confirm without `email` gets `422` with a `problems` list. New fields exist only after confirm. `leads` is empty before confirm. |
| 6 | Templates, budget, `llm_calls`, OpenAPI, `docs/API.md`, this file | Second upload of the same headers: 0 model calls. All 5 new routes are in `/docs` (CI test). |

### Tests

598 tests, 0 failures (run with `--no-fail-fast`; plain `cargo test` stops at the first failing binary). New: `tests/import_mapping.rs` (6) and about 8 unit tests (masking, samples, rules, guard, prompt block, template hash, settings).
The earlier full run had 1 failure that did not repeat; I did not find which test (probably the 100 ms search timing test, see above).
The test list was kept short on purpose; these cases from the map are covered:

| Test from the map | Where |
| --- | --- |
| Odd headers, full mapping with `FakeLlm` | `odd_headers_get_a_full_proposal_and_nothing_is_imported_before_confirm` |
| No provider, provider bad, over budget: rules only + warning | `no_provider_bad_json_and_budget_fall_back_to_the_rules` |
| No sample email or phone in the model request (masking on) | same as the first test (checks the `FakeLlm` request) |
| The user changes a target on confirm | `confirm_uses_the_users_target_makes_the_field_and_imports` |
| `split_full_name`: "Mary Ann Smith" and one word | same test |
| Second confirm starts the import once | same test (second confirm gets `409`; the row lock makes this safe at the same moment) |
| Same headers: 0 calls; other order: new proposal | `a_new_field_is_made_only_on_confirm_and_a_template_saves_the_next_call` |
| 150 columns: first 100 to the AI | `the_model_answer_is_guarded_and_150_columns_send_only_100` |

### Decisions that differ from the map

| What | Why |
| --- | --- |
| `auto_map` is `false` by default (the map says `true`). | The old upload answered with a started import. A default of `true` would change that for every caller and break the old tests. Send `?auto_map=true` for the new flow. |
| Built-in names are the ones in the code: `company_name`, `job_title`, `location`, `notes` (not `company`, `title`). | "The code wins." |
| The confirmed mapping is stored in the old `lead_imports.mapping` (column name to target text), so the worker needed almost no change. Only a new worker target `full_name` (the split). `lead_imports` got just `proposal jsonb`. | `mapping` already existed. |
| The provider falls back to the generator's slot provider, then its draft provider. | The map says "the generator's default provider"; the generator has no single default. |
| Confirm takes only the columns you change; the others keep the proposal. | Smaller body. |
| A mapped column that repeats an earlier column name is refused. | The worker reads columns by name; the first one would win. |
| `mapping_min_confidence` also lists rule-less columns (confidence 0) in `needs_check`. | They need a look. |
| The `uploaded` import keeps the file until it is confirmed or cancelled (no timeout clean-up). | Simple; a cancel deletes the file. |
| The generic prompt block cleaner `sanitize_tag` is now public in `generator/prompt.rs`. | One method for `<lead_data>` and `<csv_data>`. |

### Not done, or not verified (import mapping)

- **A real model call** for the mapping: not made (only `FakeLlm`). Upload a real file with `?auto_map=true` and read the proposal. The prompt asks for JSON only; the same Anthropic and OpenAI clients as the generator are used.
- **Quality on odd files** (other languages, 100 columns): not measured. Check `needs_check` and the reasons first.
- An `uploaded` import that nobody confirms stays in the database with its file. Cancel it, or add a clean-up job later.
- No `warnings` or `problems` text for the popup beyond codes and short English sentences.
- Not in this build, as the map says: the database connector (part 2), Excel, other transforms, the popup UI.

## Not done, or not verified (follow-ups)

- **Outlook thread headers on a real mailbox:** not tested (see above).
- **Gmail threading on a real account:** not tested. Needs the subject to match.
- **A real three-step sequence on one live account:** not run. Only Mailpit (SMTP) and fake senders.
- Open and click tracking, A/B steps: out of scope. (AI-written parts of a step are now the Email Generator.)
- `PUT .../steps` keeps `delay` changes for enrollments that have not made the next due time yet; a due time already computed does not move.

## Not done, or not verified (sending engine and Reply Check)

- **Reply Check on real Gmail / Outlook:** not tested. Needs live accounts with the read scope. Mock servers only.
  Check there: does the Gmail API keep our `Message-ID`? (The readback saves the real one if not.) Same for Graph `sendMail`.
- **Gmail restricted scope:** `gmail.readonly` needs Google verification for a public app. See `README.md`.
- **IMAP over TLS (993 / STARTTLS):** not tested against a real server; GreenMail ran plain on localhost.
- **IMAP `XOAUTH2`:** code exists, no test.
- Gmail push (`users.watch`) and IMAP `IDLE`: not in v1, as the map says.
- **Real Gmail send:** not tested. Needs a Google "Web application" OAuth client and `AIKIDO_PUBLIC_URL`.
- **Real Outlook send:** not tested. This includes whether Graph keeps our `Message-ID`. The best-effort sent-item lookup exists, but only a mock checked it.
- **Live OAuth login in a browser:** not tested end to end (only URL building and state sealing).
- **No Dockerfile** yet.
- **Out of scope by design (hooks exist):** UI, open/click tracking, AI reading of replies, saving a copy to the IMAP Sent folder.
  Hooks: `inbound_messages.reply_text` and `lead_id` for the AI and lead steps; `sequences.campaign_id` and `enrollments.lead_id` for campaigns and leads.
- **Clippy:** only style hints (collapsible `if`), no errors.
- **Git:** nothing is committed yet. All files are untracked.

## Next steps

1. Connect one real Gmail account and one real Outlook account. Send one test email from each.
2. Check that `Message-ID` survives on Microsoft. If not, adjust the lookup.
3. Decide on the table and index changes above (keep or revert).
4. Add a Dockerfile and a deploy setup (set the `AIKIDO_*_API_KEY` values and `AIKIDO_ENCRYPTION_KEY`).
5. Connect one real Gmail and one real Outlook account with the read scope. Send a test email, reply to it, and check you get `reply` within one poll. Send to a dead address on a real domain and check for `bounce_hard`.
6. Run one real three-step sequence on one live Gmail and one live Outlook account. Check the threads in the inbox.
7. Build the later steps that plug into the hooks: Auto Database part 2, the real database connector (it reuses `SampleSet`, the rule mapper, the AI mapper and the templates, then calls `leads::upsert::upsert_batch`), AI agent and chatbot (reads `GET /v1/leads/{id}/context`, sets `interest`). The Email Generator is built.
7a. Try the import mapping for real: upload a CSV with `POST /v1/leads/imports?auto_map=true`, read the proposal, then confirm.
7b. Try the Email Generator for real: add one provider, run `POST /v1/generator/providers/{id}/test`, preview one lead, then run the first campaign in review mode and read 20 outputs.
8. Commit the work.

## Handy facts

- Dev services: `docker compose up -d` (Postgres on `127.0.0.1:55432`, Mailpit SMTP on `1025`, web page on `http://127.0.0.1:8025`, GreenMail SMTP `3025` and IMAP `3143`). If tests fail with `PoolTimedOut`, the containers are down. Two containers named `aikido-pg` and `aikido-mailpit` from earlier testing may still be running. Remove them first: `docker rm -f aikido-pg aikido-mailpit`.
- `./data/` holds `encryption.key` and `initial_keys.txt`. It is git-ignored. **Back up `encryption.key`.**
- API reference: `docs/API.md`. Live docs: `/docs` and `/openapi.json` on the running server.
