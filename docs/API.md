# AIKIDO EMAIL Sending Engine: API Reference

The engine stores connected email accounts and sends queued emails one at a time, with human-like timing. This document describes the HTTP API. A live, machine-readable version is served by the running server:

- `GET /docs`: interactive docs page
- `GET /openapi.json`: OpenAPI 3 spec

## Contents

1. [Quick start](#1-quick-start)
2. [Conventions](#2-conventions)
3. [Authentication and scopes](#3-authentication-and-scopes)
4. [Errors](#4-errors)
5. [System](#5-system)
6. [API keys (`admin`)](#6-api-keys-admin)
7. [Accounts (`accounts`)](#7-accounts-accounts)
8. [Sending settings (`sending`)](#8-sending-settings-sending)
9. [Account status and control (`sending`)](#9-account-status-and-control-sending)
10. [Jobs (`sending`)](#10-jobs-sending)
11. [Suppressions (`sending`)](#11-suppressions-sending)
12. [Engine control (`sending`)](#12-engine-control-sending)
13. [How the engine decides when to send](#13-how-the-engine-decides-when-to-send)
14. [Reference tables](#14-reference-tables)
15. [Running the server](#15-running-the-server)
16. [Reply Check (`replies`)](#16-reply-check-replies)
17. [Follow-ups (`followups`)](#17-follow-ups-followups)
18. [Leads (`leads`)](#18-leads-leads)
19. [Campaigns (`campaigns`)](#19-campaigns-campaigns)
20. [Email Generator (`generator`)](#20-email-generator-generator)
21. [Web app: login, team, helpers, reports](#21-web-app-login-team-helpers-reports)
25. [Email optimization (`personalization`, `campaigns`, `leads`)](#25-email-optimization-personalization-campaigns-leads)

---

## 1. Quick start

```bash
# 1. Start the server. On first boot it prints eight API keys ONE time.
DATABASE_URL=postgres://aikido:aikido@127.0.0.1:55432/aikido cargo run -- serve

export BASE=http://localhost:8080
export ACCOUNTS_KEY=aik_accounts_...   # from the first-boot output
export SENDING_KEY=aik_sending_...

# 2. Connect an SMTP account. The login is tested before it is saved.
curl -s -X POST $BASE/v1/accounts/smtp \
  -H "Authorization: Bearer $ACCOUNTS_KEY" -H "Content-Type: application/json" \
  -d '{
    "email": "outreach@example.com",
    "display_name": "Outreach",
    "smtp": { "host": "smtp.example.com", "port": 587, "tls": "starttls", "password": "app-password" }
  }'
# -> { "id": "0192...", "status": "active", "has_credentials": true, ... }

# 3. Queue an email.
curl -s -X POST $BASE/v1/sending/jobs \
  -H "Authorization: Bearer $SENDING_KEY" -H "Content-Type: application/json" \
  -d '{ "jobs": [{
    "account_id": "0192...",
    "to_email": "lead@company.com",
    "to_name": "Sam",
    "subject": "Quick question",
    "body_text": "Hi Sam, ...",
    "idempotency_key": "campaign-7-lead-42-step-1"
  }]}'

# 4. See when it will go.
curl -s "$BASE/v1/sending/accounts/0192.../status"  -H "Authorization: Bearer $SENDING_KEY"
curl -s "$BASE/v1/sending/accounts/0192.../preview?count=10" -H "Authorization: Bearer $SENDING_KEY"
```

The engine picks the job up on its next tick (every 15 seconds by default) and sends it when every rule allows (see [section 13](#13-how-the-engine-decides-when-to-send)).

---

## 2. Conventions

| Topic | Rule |
| --- | --- |
| Base path | All routes start with `/v1`, except `/health`, `/docs`, `/openapi.json`. |
| Format | Request and response bodies are JSON (`Content-Type: application/json`). |
| IDs | UUID v7. They sort by creation time. |
| Times | Always UTC, ISO 8601 (`2026-10-19T09:07:12Z`). The only exception is the human-readable `local_time` / `local` strings, which use the account time zone. |
| Body size | Up to 25 MB per request. |
| Timeout | A request that runs longer than 60 seconds fails with `408`. |
| CORS | Open to all origins. The API is protected by keys in headers, not cookies. |
| Secrets | No response ever contains a token, password, or key hash. Accounts show `has_credentials: true` instead. |
| Unknown JSON fields | Ignored in requests, except in settings `PATCH` (unknown settings are rejected). |

---

## 3. Authentication and scopes

Send the key in one of two headers:

```
Authorization: Bearer <key>
X-API-Key: <key>
```

**Master key:** one key for every action. It is the `admin` key. In the web app, go to **Settings → API** and press **Generate master key**. Use it on every route. You do not need the section keys.

Section keys are optional. A section key works only on routes of its own scope.

| Scope | Routes it opens | Made on first boot |
| --- | --- | --- |
| `admin` | `/v1/keys/*`, plus every other route | Yes |
| `accounts` | `/v1/accounts/*` | Yes |
| `sending` | `/v1/sending/*` | Yes |
| `replies` | `/v1/replies/*` | Yes |
| `followups` | `/v1/followups/*` | Yes |
| `leads` | `/v1/leads/*` | Yes |
| `campaigns` | `/v1/campaigns/*` | Yes |
| `generator` | `/v1/generator/*` | Yes |
| `integrations` | `/v1/integrations/*`, `/v1/verify/*` | Yes |
| `personalization` | `/v1/personalization/*` | Yes |

**Routes that need no key:** `GET /health`, `GET /docs`, `GET /openapi.json`, `GET /v1/accounts/oauth/{provider}/callback` (protected by a sealed `state` value), and `/v1/auth/*` (the web login, [section 21](#21-web-app-login-team-helpers-reports)).

**Web sessions.** The web app does not hold API keys. A person logs in with email and password and gets a session cookie. On the routes above, a session works instead of a key: an `owner` or `admin` acts as the `admin` scope; a `member` acts as every scope except `admin`, and is also refused on the system switches (engine pause and resume, and `PATCH` of `/v1/sending/defaults`, `/v1/replies/defaults`, `/v1/followups/settings`). A call with a cookie that changes data needs the `X-CSRF-Token` header and an `Origin` that is this app. Details in [section 21](#21-web-app-login-team-helpers-reports).

**Key format:** `aik_<scope>_<random>`, for example `aik_sending_7fK2...`. The scope in the text is only for humans. The server always checks the scope stored in the database.

**First boot.** The server makes one key for every scope that has no active key, prints them once in a box on stdout, and writes them to `<AIKIDO_DATA_DIR>/initial_keys.txt` (mode `600`). It never prints a key again. A lost key is replaced by rotating it with the admin key ([section 6](#6-api-keys-admin)). For Docker and cloud deploys, set `AIKIDO_ADMIN_API_KEY`, `AIKIDO_ACCOUNTS_API_KEY`, `AIKIDO_SENDING_API_KEY`, `AIKIDO_REPLIES_API_KEY`, `AIKIDO_FOLLOWUPS_API_KEY`, `AIKIDO_LEADS_API_KEY`, `AIKIDO_CAMPAIGNS_API_KEY`, `AIKIDO_GENERATOR_API_KEY` and the server uses those instead of making keys.

Only the SHA-256 hash of a key is stored. `last_used_at` is updated at most once a minute per key.

| Result | Status | `error.code` |
| --- | --- | --- |
| No key sent | `401` | `missing_api_key` |
| Unknown or revoked key | `401` | `invalid_api_key` |
| Valid key, wrong scope | `403` | `wrong_scope` |
| A member session on an admin-only route | `403` | `admin_only` |
| A session call without a good CSRF token or `Origin` | `403` | `csrf_failed` |

---

## 4. Errors

Every error has this shape:

```json
{ "error": { "code": "invalid_timezone", "message": "Timezone 'Dhaka' is not valid. Use an IANA name like 'Asia/Dhaka'." } }
```

`code` is a stable `snake_case` string. Match on `code`, not on `message`. The message is for people.

| Status | Meaning |
| --- | --- |
| `400` | The request is malformed (bad path, bad query, OAuth problem). |
| `401` | No key, or a bad key. |
| `403` | The key has the wrong scope. |
| `404` | The thing does not exist. |
| `409` | The thing is in the wrong state (for example, cancelling a job that already started). |
| `422` | The values are not valid (settings, JSON body, batch size). |
| `500` | Server error. `error.code` is `internal_error`. Details are in the server log only. |
| `503` | `/health` only: the database does not answer. |

Common codes that can come from any route that takes input: `invalid_json` (`422`/`400`), `invalid_path` (`400`), `invalid_query` (`400`). Each endpoint below lists its own codes.

---

## 5. System

### `GET /health`

No key. Checks that the server and the database are up.

```json
{ "status": "ok" }
```

Returns `503` with code `database_down` if the database does not answer.

---

## 6. API keys (`admin`)

All routes here need scope `admin`.

### `GET /v1/keys`

Lists keys. Never the key itself.

```json
[
  {
    "id": "0192...",
    "name": "bootstrap",
    "scope": "sending",
    "key_prefix": "aik_sending_7fK2xY",
    "created_at": "2026-10-17T07:51:05Z",
    "last_used_at": "2026-10-17T08:02:11Z",
    "revoked_at": null
  }
]
```

### `POST /v1/keys`

Makes a new key. **The plain key is returned this one time only.**

Request:

```json
{ "scope": "sending", "name": "UI backend" }
```

`scope` is one of `admin`, `accounts`, `sending`. `name` is optional.

Response `201`:

```json
{ "key": { "id": "0192...", "name": "UI backend", "scope": "sending", "key_prefix": "aik_sending_Qz8mNa", "created_at": "...", "last_used_at": null, "revoked_at": null },
  "plain_key": "aik_sending_Qz8mNa..." }
```

### `POST /v1/keys/{id}/rotate`

Makes a new key with the same scope and name, and revokes the old one. The body is optional.

Request:

```json
{ "grace_hours": 2 }
```

`grace_hours` (0 to 8760, default 0): the old key keeps working this many hours.

Response `200`:

```json
{ "key": { ... }, "plain_key": "aik_...", "old_key_id": "0192...", "old_key_revoked_at": "2026-10-17T10:00:00Z" }
```

| Code | Status | When |
| --- | --- | --- |
| `key_not_found` | 404 | No key with this id. |
| `key_revoked` | 409 | The key is already revoked. |
| `invalid_grace` | 422 | `grace_hours` is out of range. |

### `DELETE /v1/keys/{id}`

Revokes a key.

```json
{ "revoked": true }
```

| Code | Status | When |
| --- | --- | --- |
| `key_not_found` | 404 | No key with this id. |
| `key_revoked` | 409 | Already revoked. |
| `last_admin_key` | 409 | This is the last active admin key. Make another admin key first. |

---

## 7. Accounts (`accounts`)

An account is one connected inbox. Three providers exist: `gmail`, `microsoft`, `smtp`. Every secret (OAuth tokens, SMTP/IMAP passwords) is encrypted with AES-256-GCM before it touches the database. Deleting an account wipes its secrets at once.

### The account object

```json
{
  "id": "0192...",
  "email": "outreach@example.com",
  "provider": "smtp",
  "display_name": "Outreach",
  "status": "active",
  "smtp": { "host": "smtp.example.com", "port": 587, "tls": "starttls" },
  "imap": null,
  "has_credentials": true,
  "token_expires_at": null,
  "last_error": null,
  "created_at": "2026-10-17T07:51:42Z",
  "updated_at": "2026-10-17T07:51:42Z"
}
```

| Field | Notes |
| --- | --- |
| `email` | Lowercase. Unique per provider among non-deleted accounts. |
| `provider` | `gmail`, `microsoft`, or `smtp`. |
| `status` | `active`, `paused`, `error`, or `disconnected`. See [account status](#account-status). |
| `smtp`, `imap` | Host settings, or `null`. OAuth accounts have both `null`. `tls` is `implicit`, `starttls`, or `none`. |
| `has_credentials` | `true` when secrets are stored. |
| `token_expires_at` | OAuth accounts only. |
| `last_error` | Why the account was paused or disconnected, if it was. |

#### Account status

| Status | Meaning |
| --- | --- |
| `active` | Can send. |
| `paused` | Paused by a person (`POST .../pause`). |
| `error` | Paused automatically after repeated failures or a high bounce rate. See `last_error`. |
| `disconnected` | The login is no longer valid (for example, a revoked OAuth token). The user must connect the account again. |

### `POST /v1/accounts/oauth/{provider}/start`

Starts the OAuth login. `provider` is `gmail` or `microsoft`.

Response `200`:

```json
{ "auth_url": "https://accounts.google.com/o/oauth2/v2/auth?client_id=...&state=..." }
```

Send the user's browser to `auth_url`. After sign-in, the provider sends the browser to the callback below, and the account is saved.

**Needs setup:** `AIKIDO_PUBLIC_URL`, and the Google or Microsoft app credentials (`GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET`, `MS_CLIENT_ID`, `MS_CLIENT_SECRET`). Register `<AIKIDO_PUBLIC_URL>/v1/accounts/oauth/gmail/callback` (and `.../microsoft/callback`) as a redirect URI. For Google, use a "Web application" client.

Needed provider permissions: Gmail `gmail.send`; Microsoft `Mail.Send` (and `offline_access`).

| Code | Status | When |
| --- | --- | --- |
| `unknown_provider` | 404 | `provider` is not `gmail` or `microsoft`. |
| `public_url_not_set` | 400 | `AIKIDO_PUBLIC_URL` is not set. |
| `oauth_not_configured` | 400 | The provider app credentials are missing. The message names the variable. |

### `GET /v1/accounts/oauth/{provider}/callback`

**No key.** The provider sends the user's browser here. The signed `state` value (sealed with the server's encryption key, valid 10 minutes) protects it. It returns a small HTML page: "Connected: <email>. You can close this tab." When the web app opened the page in a popup, a script sends `{ type: "aikido:oauth", ok, email, code }` to that window with `postMessage` (to this origin only) and closes the popup. `ok` is `false` and `code` is an error code (below) when the login failed. A browser (it asks for `text/html`) also gets an HTML page for errors, with the same status code; any other caller gets the JSON error.

Connecting the same inbox twice updates the existing account. It does not make a duplicate. A reconnect also clears `disconnected` / `error` and un-pauses the account.

| Code | Status | When |
| --- | --- | --- |
| `oauth_denied` | 400 | The user said no, or the provider returned an error. |
| `oauth_bad_callback` | 400 | `code` or `state` is missing. |
| `oauth_bad_state` | 400 | The `state` is forged, expired, or for another provider. |
| `oauth_exchange_failed` | 400 | The provider refused the code. |

### `POST /v1/accounts/smtp`

Adds an SMTP account (IMAP is optional). **The login is tested before anything is saved.**

Request:

```json
{
  "email": "outreach@example.com",
  "display_name": "Outreach",
  "smtp": { "host": "smtp.example.com", "port": 587, "tls": "starttls", "username": "outreach@example.com", "password": "app-password" },
  "imap": { "host": "imap.example.com", "port": 993, "tls": "implicit", "password": "app-password" }
}
```

| Field | Notes |
| --- | --- |
| `email` | Required. |
| `display_name` | Optional. Used as the sender name. |
| `smtp` | Required. `host`, `port`, `tls`, `password` are required. `username` defaults to `email`. |
| `imap` | Optional. Same shape. |
| `tls` | `implicit` (port 465), `starttls` (port 587), or `none`. `none` is allowed **only for `localhost`** (for test servers such as Mailpit). No login is tried on an unencrypted connection. |

Response `201`: the [account object](#the-account-object).

| Code | Status | When |
| --- | --- | --- |
| `invalid_email` | 422 | Not a valid address. |
| `invalid_host` | 422 | Empty host, or the host has spaces. |
| `insecure_tls` | 422 | `tls: "none"` on a host that is not localhost. |
| `smtp_login_failed` | 422 | The SMTP login test failed. Nothing was saved. |
| `imap_login_failed` | 422 | The IMAP login test failed. Nothing was saved. |

### `GET /v1/accounts`

Lists accounts (not deleted), oldest first. Returns an array of [account objects](#the-account-object).

### `GET /v1/accounts/{id}`

One account. `404 account_not_found` if it does not exist or was deleted.

### `PATCH /v1/accounts/{id}`

Changes the display name or the SMTP/IMAP details. Send only what changes. Missing server fields keep their value.

```json
{
  "display_name": "New Name",
  "smtp": { "host": "smtp2.example.com", "password": "new-password" }
}
```

`smtp` and `imap` accept `host`, `port`, `tls`, `username`, `password`. If server details or passwords change, the login is tested again before saving.

| Code | Status | When |
| --- | --- | --- |
| `not_smtp_account` | 422 | The account is OAuth. Only the display name can change. |
| `incomplete_server` | 422 | The account has no such server yet and `host`, `port`, or `tls` is missing. |
| `invalid_host`, `insecure_tls`, `smtp_login_failed`, `imap_login_failed` | 422 | As in `POST /v1/accounts/smtp`. |
| `account_not_found` | 404 | |

### `POST /v1/accounts/{id}/test`

Checks that the login works. **Sends nothing.** OAuth accounts get a fresh token (this also saves it). SMTP accounts log in to SMTP and IMAP.

```json
{ "ok": true, "detail": "The login works." }
```

On failure, still `200`, with `"ok": false` and the reason in `detail`.

### `DELETE /v1/accounts/{id}`

Wipes the secrets, soft-deletes the account (the row stays, so old sent jobs still point to it), pauses its state, and cancels its queued jobs.

```json
{ "deleted": true, "cancelled_jobs": 12 }
```

The same email can be connected again afterwards as a new account.

---

## 8. Sending settings (`sending`)

Every sending rule is a setting. Nothing is hard-coded. Each account has its own settings. **New accounts get a copy of the defaults** at the moment they are made. Changing the defaults later does not change existing accounts.

Settings are stored as JSON. A setting added in a later version gets its default value on old accounts.

### The settings object

| Setting | Default | What it does |
| --- | --- | --- |
| `send_days` | `[1,2,3,4,5]` | Days allowed to send. ISO numbers: 1 = Monday, 7 = Sunday. |
| `timezone` | `"UTC"` | IANA time zone name (for example `Asia/Dhaka`). All day and hour rules use this zone. |
| `window_start` | `"09:00"` | Local time the sending window opens. Start is included. |
| `window_end` | `"17:00"` | Local time it closes. End is **not** included. No overnight windows. |
| `base_delay_secs` | `300` | Fixed gap after each send. |
| `jitter_min_secs` | `120` | Random extra gap, lowest value. |
| `jitter_max_secs` | `600` | Random extra gap, highest value. Total gap with defaults: 7 to 15 minutes. |
| `daily_limit` | `30` | Max emails per account per local day. |
| `window_start_jitter_mins` | `15` | The first email of the day goes 0 to this many minutes after the window opens. |
| `daily_limit_jitter_pct` | `0` | Each day's cap is random between `daily_limit` minus this percent and `daily_limit`. |
| `hourly_limit` | `10` | Max emails in any rolling 60 minutes. A safety net. |
| `blocked_dates` | `[]` | Dates with no sending (`"YYYY-MM-DD"`, in the account time zone). |
| `warmup_enabled` | `false` | Slow ramp-up for new inboxes. |
| `warmup_start` | `5` | Warm-up cap on the first sending day. |
| `warmup_step` | `2` | Warm-up cap grows by this much each sending day, up to `daily_limit`. |
| `per_domain_daily_limit` | `5` | Max emails per day to one recipient domain from this account. |
| `per_domain_exempt` | `["gmail.com","outlook.com","hotmail.com","yahoo.com","icloud.com"]` | Domains that skip the per-domain cap. |
| `same_recipient_cooldown_hours` | `24` | Never send two emails to the same address within this time, **from any account**. `0` turns it off. |
| `max_attempts` | `3` | Tries per job for temporary errors. |
| `retry_backoff_mins` | `[15, 60]` | Wait before try 2, then before try 3. |
| `pause_after_failures` | `3` | Pause the account after this many failures in a row. |
| `pause_bounce_rate_pct` | `5` | Pause if hard bounces are over this percent of the last `bounce_window` sends. |
| `bounce_window` | `50` | How many recent sends the bounce rate looks at. Checked only once there are at least 20 sends (or `bounce_window`, if that is lower). |
| `list_unsubscribe` | `null` | `{ "mailto": "...", "url": "https://..." }`. If set, adds `List-Unsubscribe` and (with a `url`) `List-Unsubscribe-Post` headers. Gmail and Yahoo expect these. |

### Validation

Invalid values are refused with `422` and a clear code. The full merged result is validated before it is saved.

| Code | Rule |
| --- | --- |
| `invalid_timezone` | Must be an IANA name. |
| `invalid_send_days` | Not empty, values 1 to 7, no repeats. |
| `invalid_window` | `HH:MM` format, and `window_start` before `window_end`. |
| `invalid_jitter` | `jitter_min_secs` must not be more than `jitter_max_secs`. |
| `invalid_base_delay` | `base_delay_secs` at least 60. |
| `invalid_daily_limit` | 1 to 500. |
| `invalid_daily_limit_jitter` | `daily_limit_jitter_pct` 0 to 50. |
| `invalid_bounce_rate` | `pause_bounce_rate_pct` 1 to 100. |
| `invalid_hourly_limit` | At least 1. |
| `invalid_window_jitter` | `window_start_jitter_mins` 0 to 600. |
| `invalid_warmup` | `warmup_start` at least 1. |
| `invalid_domain_limit` | `per_domain_daily_limit` at least 1. |
| `invalid_max_attempts` | At least 1. |
| `invalid_retry_backoff` | `retry_backoff_mins` needs at least `max_attempts - 1` values. |
| `invalid_pause_after_failures` | At least 1. |
| `invalid_bounce_window` | At least 1. |
| `invalid_list_unsubscribe` | Needs a `mailto` or a `url` (the URL must start with `http://` or `https://`). Use `null` to turn it off. |
| `unknown_setting` | The key is not a sending setting (typo guard on `PATCH`). |
| `invalid_settings` | A value has the wrong type. The message has details. |

A `daily_limit` above 50 is **not** an error. It comes back in `warnings`.

### Reply shape

All settings routes return:

```json
{
  "settings": { "send_days": [1,2,3,4,5], "timezone": "Asia/Dhaka", "...": "..." },
  "warnings": ["daily_limit 80 is high. Sending more than 50 emails a day from one inbox risks spam filters."]
}
```

### `GET /v1/sending/defaults`

Reads the defaults that new accounts copy.

### `PATCH /v1/sending/defaults`

Changes the defaults. Send only the fields to change. The server merges, validates the full result, then saves. Arrays are replaced as a whole. Send `"list_unsubscribe": null` to clear it.

```bash
curl -X PATCH $BASE/v1/sending/defaults -H "Authorization: Bearer $SENDING_KEY" \
  -H "Content-Type: application/json" \
  -d '{ "timezone": "America/New_York", "daily_limit": 25, "send_days": [1,2,3,4] }'
```

### `GET /v1/sending/accounts/{id}/settings`

Reads one account's settings. `404 account_not_found`.

### `PATCH /v1/sending/accounts/{id}/settings`

Same behavior as the defaults `PATCH`, for one account. A change takes effect right away: the gap timer is measured again from the last send, and today's cap is picked again.

### `POST /v1/sending/accounts/{id}/settings/reset`

Copies the defaults onto this account.

---

## 9. Account status and control (`sending`)

### `GET /v1/sending/accounts/{id}/status`

Where the account stands right now.

```json
{
  "account_id": "0192...",
  "email": "outreach@example.com",
  "account_status": "active",
  "paused": false,
  "paused_reason": null,
  "paused_at": null,
  "timezone": "Asia/Dhaka",
  "local_time": "2026-10-19 10:42:03 +06",
  "inside_window": true,
  "sent_today": 4,
  "today_limit": 28,
  "warmup_day": 0,
  "consecutive_failures": 0,
  "next_send_at": "2026-10-19T04:51:20Z",
  "last_sent_at": "2026-10-19T04:42:01Z",
  "queued_jobs": 17,
  "sending_jobs": 0,
  "wait": { "reason": "gap_not_elapsed", "until": "2026-10-19T04:51:20Z" }
}
```

| Field | Notes |
| --- | --- |
| `inside_window` | On a send day, and local time is inside the window. |
| `today_limit` | The cap picked for today (after warm-up and daily jitter). |
| `next_send_at` | Earliest time the next email may go. The engine also uses this field to park an account until a window opens. |
| `wait` | `null` means the rules allow a send right now. Otherwise: why not, and until when. |

`wait.reason` values:

| Reason | Meaning |
| --- | --- |
| `not_send_day` | Today's weekday is not in `send_days`. |
| `blocked_date` | Today is in `blocked_dates`. |
| `outside_window` | Local time is before `window_start` or at/after `window_end`. |
| `daily_limit_reached` | `sent_today` reached `today_limit`. |
| `hourly_limit_reached` | `hourly_limit` sends happened in the last 60 minutes. |
| `gap_not_elapsed` | The gap after the last send is not over. |

`wait.until` is the earliest moment **all** rules could pass.

### `POST /v1/sending/accounts/{id}/pause`

Pauses one account. The body is optional.

```json
{ "reason": "vacation" }
```

Response: `{ "paused": true }`. The account `status` becomes `paused`. Default reason: `paused by user`.

### `POST /v1/sending/accounts/{id}/resume`

Resumes a paused account and resets `consecutive_failures`. Response: `{ "paused": false }`.

| Code | Status | When |
| --- | --- | --- |
| `account_disconnected` | 409 | The login is no longer valid. Connect the account again (this also un-pauses it). |

### `GET /v1/sending/accounts/{id}/preview?count=20`

Simulates the next send times with the current rules. **Sends nothing and changes nothing.** `count` is 1 to 200 (default 20). `invalid_count` (`422`) otherwise.

```json
{
  "timezone": "Asia/Dhaka",
  "times": [
    { "at": "2026-10-19T03:07:41Z", "local": "2026-10-19 09:07:41 +06" },
    { "at": "2026-10-19T03:18:02Z", "local": "2026-10-19 09:18:02 +06" }
  ]
}
```

The random parts (gap, first-email jitter, daily cap jitter) are drawn fresh each call, so two previews differ slightly. Every time respects the days, window, limits, and gap range.

---

## 10. Jobs (`sending`)

A job is one email to send. Jobs are queued by this API and sent by the engine.

### Job lifecycle

```
queued ──► sending ──► sent
   │          ├──────► failed
   │          ├──────► stuck      (no result: may or may not have gone out)
   │          └──────► queued     (temporary error: retry later)
   ├────────► cancelled           (DELETE while queued)
   └────────► skipped             (address or domain is suppressed)

failed / stuck ──► queued         (POST .../requeue, on purpose)
```

| Status | Meaning |
| --- | --- |
| `queued` | Waiting. May have `send_after` set (a retry time, a deferral, or the caller's own "not before"). |
| `sending` | Claimed by a worker. At most one per account at a time. |
| `sent` | Handed to the provider. `sent_at` is set. |
| `failed` | Gave up (permanent error, or out of attempts). See `last_error`. |
| `cancelled` | Cancelled by a person, or its account was deleted. |
| `skipped` | The address or its domain is in the suppression list. |
| `stuck` | Sending took more than 10 minutes, or the result was unknown (for example, a timeout after the server got the data). **It is never resent on its own.** A double email to a lead is worse than a missed one. Check, then requeue if needed. |

### The job object

```json
{
  "id": "0192...",
  "account_id": "0192...",
  "idempotency_key": "campaign-7-lead-42-step-1",
  "to_email": "lead@company.com",
  "to_name": "Sam",
  "recipient_domain": "company.com",
  "subject": "Quick question",
  "body_html": null,
  "body_text": "Hi Sam, ...",
  "headers": {},
  "in_reply_to": null,
  "references": null,
  "lead_id": null,
  "campaign_id": null,
  "metadata": {},
  "priority": 0,
  "send_after": null,
  "status": "queued",
  "attempts": 0,
  "last_error": null,
  "message_id_header": null,
  "provider_message_id": null,
  "provider_thread_id": null,
  "created_at": "2026-10-19T03:00:00Z",
  "claimed_at": null,
  "sent_at": null,
  "updated_at": "2026-10-19T03:00:00Z"
}
```

`message_id_header` is the `Message-ID` we put on the email (`<uuid@sender-domain>`). It is set when the job is first claimed and stays the same across retries. `provider_message_id` and `provider_thread_id` come from Gmail (message id, thread id) or Microsoft (message id, conversation id, found by a best-effort lookup after sending).

### `POST /v1/sending/jobs`

Queues 1 to 1000 emails. **Bad items get an error each. Good items are still queued.**

Request:

```json
{
  "jobs": [{
    "account_id": "0192...",
    "to_email": "lead@company.com",
    "to_name": "Sam",
    "subject": "Quick question",
    "body_text": "Hi Sam, ...",
    "body_html": null,
    "headers": { "X-Campaign": "c7" },
    "in_reply_to": null,
    "references": null,
    "provider_thread_id": null,
    "send_after": null,
    "priority": 0,
    "idempotency_key": "campaign-7-lead-42-step-1",
    "lead_id": null,
    "campaign_id": null,
    "metadata": {}
  }]
}
```

| Field | Required | Notes |
| --- | --- | --- |
| `account_id` | yes | Must be an existing, non-deleted account. |
| `to_email` | yes | Must be a valid address. Stored lowercase. |
| `to_name` | no | |
| `subject` | yes | One line, up to 998 characters. |
| `body_text`, `body_html` | one of them | If both are given, the email is `multipart/alternative`. |
| `headers` | no | Extra headers. These are set by the engine and **cannot** be used: `From`, `To`, `Cc`, `Bcc`, `Subject`, `Message-ID`, `In-Reply-To`, `References`, `Date`, `MIME-Version`, `Content-Type`, `Content-Transfer-Encoding`, `Sender`, `List-Unsubscribe`, `List-Unsubscribe-Post`. |
| `in_reply_to`, `references` | no | Message-IDs, so a follow-up lands in the same thread. |
| `provider_thread_id` | no | Gmail thread id, for the same purpose. |
| `send_after` | no | Not before this time (UTC). |
| `priority` | no | Higher goes first. Default 0. Order is `priority` descending, then oldest first. |
| `idempotency_key` | no | 1 to 200 characters. A repeated key returns the old job instead of making a new one. |
| `lead_id`, `campaign_id`, `metadata` | no | Free hooks for later steps. `metadata` must be a JSON object. |

Response `200`:

```json
{
  "items": [
    { "index": 0, "status": "queued",    "job": { "...": "..." }, "error": null },
    { "index": 1, "status": "duplicate", "job": { "...": "..." }, "error": null },
    { "index": 2, "status": "error",     "job": null, "error": { "code": "invalid_to_email", "message": "'nope' is not a valid email address." } }
  ],
  "queued": 1,
  "duplicates": 1,
  "errors": 1
}
```

`items` is in the same order as the request. `status: "duplicate"` means the `idempotency_key` was already used, and `job` is the original job.

Per-item error codes: `invalid_to_email`, `invalid_subject`, `missing_body`, `invalid_header`, `reserved_header`, `invalid_idempotency_key`, `invalid_metadata`, `account_not_found`.

Whole-request error: `invalid_batch_size` (`422`) when `jobs` is empty or has more than 1000 items.

Queuing to a suppressed address is allowed. The job is later marked `skipped`.

### `GET /v1/sending/jobs`

Lists jobs, newest first, with cursor paging.

| Query | Notes |
| --- | --- |
| `status` | `queued`, `sending`, `sent`, `failed`, `cancelled`, `skipped`, `stuck` |
| `account_id`, `campaign_id`, `lead_id` | UUID filters |
| `limit` | 1 to 500, default 50. `invalid_limit` (`422`) otherwise. |
| `cursor` | The `next_cursor` of the previous page. |

```json
{ "jobs": [ { "...": "..." } ], "next_cursor": "0192..." }
```

`next_cursor` is `null` on the last page.

### `GET /v1/sending/jobs/{id}`

One job with its event log.

```json
{
  "job": { "...": "..." },
  "events": [
    { "id": 101, "job_id": "0192...", "account_id": "0192...", "event": "queued",  "detail": {}, "at": "2026-10-19T03:00:00Z" },
    { "id": 118, "job_id": "0192...", "account_id": "0192...", "event": "claimed", "detail": { "attempt": 1 }, "at": "2026-10-19T03:07:41Z" },
    { "id": 119, "job_id": "0192...", "account_id": "0192...", "event": "sent",    "detail": { "provider_message_id": "18c...", "provider_thread_id": "18c..." }, "at": "2026-10-19T03:07:42Z" }
  ]
}
```

`404 job_not_found`.

### `DELETE /v1/sending/jobs/{id}`

Cancels a job. Works only while it is `queued`. Returns the job.

| Code | Status | When |
| --- | --- | --- |
| `job_not_found` | 404 | |
| `wrong_job_status` | 409 | The job is not `queued`. |

### `POST /v1/sending/jobs/{id}/requeue`

Puts a `stuck` or `failed` job back in the queue, on purpose. `attempts` goes back to 0. **A stuck job may already have been delivered. Check before you requeue it.** Same error codes as cancel.

---

## 11. Suppressions (`sending`)

Blocked email addresses and whole domains. A job whose address or domain is listed is marked `skipped`, and this does **not** start the gap timer. Matching is case-insensitive. A hard bounce adds the address here on its own.

### The suppression object

```json
{ "value": "bad@company.com", "kind": "email", "reason": "manual", "created_at": "2026-10-19T03:00:00Z" }
```

`kind`: `email` or `domain`. `reason`: `unsubscribed`, `hard_bounce`, `replied`, `complained`, `manual`.

### `GET /v1/sending/suppressions`

Query: `kind` (optional), `limit` (1 to 1000, default 100), `offset`. Newest first. Returns an array.

### `POST /v1/sending/suppressions`

```json
{ "value": "spam.com", "kind": "domain", "reason": "complained" }
```

`reason` defaults to `manual`. Adding the same value again updates its reason. Returns the suppression object. `invalid_value` (`422`) for a bad address or domain.

### `DELETE /v1/sending/suppressions?value=bad@company.com&kind=email`

Unblocks a value.

```json
{ "removed": true }
```

`404 suppression_not_found` if the value is not listed.

---

## 12. Engine control (`sending`)

### `POST /v1/sending/engine/pause`

Pauses all sending, for every server copy (the flag lives in the database). Sends that are already running finish. Response: `{ "paused": true }`.

### `POST /v1/sending/engine/resume`

Response: `{ "paused": false }`.

### `GET /v1/sending/engine/status`

```json
{
  "paused": false,
  "dry_run": false,
  "jobs": { "queued": 120, "sent": 842, "failed": 3, "stuck": 1 },
  "accounts": { "active": 9, "paused": 1, "disconnected": 1 }
}
```

`jobs` and `accounts` list only statuses that have at least one row. `dry_run: true` means `AIKIDO_DRY_RUN` is on: emails are logged, not sent.

---

## 13. How the engine decides when to send

This section explains the behavior behind the API, so a UI can describe it correctly. No endpoint here.

**The loop.** Every `AIKIDO_SCHEDULER_TICK_SECS` (default 15), the engine: (1) does nothing if the engine is paused; (2) marks jobs that were `sending` for more than 10 minutes as `stuck`; (3) finds ready accounts: `active`, not paused, with a due queued job, and `next_send_at` empty or in the past; (4) handles each account in its own task (up to `AIKIDO_MAX_PARALLEL_ACCOUNTS` at once). Many server copies can run on one database. They never send from the same account at the same time.

**Per account, in order:**

1. **Day rollover.** When the local day changes: `sent_today` goes to 0, the warm-up day goes up by 1 (if the old day had sends), today's cap is picked (warm-up cap, then daily jitter, never above `daily_limit`), and `next_send_at` is set to at least the window start plus a random 0 to `window_start_jitter_mins`.
2. **Gates.** All must pass: it is a send day and not a blocked date; local time is inside the window; `sent_today < today_limit`; fewer than `hourly_limit` sends in the last 60 minutes; `now >= next_send_at`. If a gate fails, the account is parked until the earliest moment all gates pass.
3. **Claim a job.** Up to 20 due queued jobs are looked at in order (`priority` descending, then oldest). For each:
   - **Suppressed** address or domain → `skipped`.
   - **Same address got mail** within `same_recipient_cooldown_hours` from any account → the job waits (`send_after` is set to when the cooldown ends) and an event `deferred` is logged.
   - **Recipient domain** reached `per_domain_daily_limit` today (and is not exempt) → the job waits until the next allowed window start.
   - Otherwise the first job that passes is claimed.
4. **Build and send.** The OAuth token is refreshed first if it ends in less than 5 minutes (a `401` also triggers one refresh and one more try). The send has a 60 second timeout.
5. **Handle the result.**

| Result | Job | Account |
| --- | --- | --- |
| Sent | `sent`, provider ids saved | `sent_today + 1`, failures reset to 0, `next_send_at = now + base_delay_secs + random(jitter_min_secs..jitter_max_secs)` |
| Temporary error | Back to `queued` with `send_after = now + retry_backoff_mins[attempt - 1]`. Out of attempts: `failed`. | Failure count + 1. Normal gap. |
| Permanent error | `failed` | Normal gap. A **hard bounce** (bad recipient) also adds the address to suppressions and counts toward the bounce rate, but not toward "failures in a row". Other permanent errors count as a failure. |
| Rate limited | Back to `queued`. The attempt is not counted. | `next_send_at = now + Retry-After` (or 1 hour). A daily quota waits for the next local day. |
| Login failed | Back to `queued`. The attempt is not counted. | Status `disconnected`, paused, reason saved. The user must connect again. |
| Unknown (no answer, may have gone out) | `stuck`. Never auto-resent. | Normal gap. |

6. **Auto-pause.** The account is paused (status `error`, reason saved in `paused_reason` and `last_error`) when `consecutive_failures >= pause_after_failures`, or when hard bounces are over `pause_bounce_rate_pct` of the last `bounce_window` sends. Call `POST .../resume` after fixing the cause.

Every claim, send, failure, skip, deferral, and pause writes a row to the event log (see `GET /v1/sending/jobs/{id}` for job events).

**Event names** (`events[].event`): `queued`, `claimed`, `sent`, `retry_scheduled`, `failed`, `hard_bounce`, `rate_limited`, `auth_failed`, `stuck`, `skipped`, `deferred`, `cancelled`, `requeued`, `paused`, `resumed`.

**Shutdown.** On `SIGTERM` the server stops claiming new jobs, waits up to 60 seconds for sends in flight, then exits.

### Provider notes

| Provider | Notes |
| --- | --- |
| Gmail | Sends through the Gmail API (`gmail.send`). Gmail answers OK even for a dead address; the bounce arrives later as an email. A later step (reply check) must read bounces and add hard bounces to the suppression list. |
| Microsoft | Sends through Microsoft Graph `sendMail` (`Mail.Send`) with the full MIME message. Graph returns no id, so the engine looks up the sent item by `Message-ID` once after sending (needs `Mail.Read`). If that lookup fails, the provider ids stay empty; the `Message-ID` is still saved. |
| SMTP | `implicit` TLS (465) or `starttls` (587). A fresh connection is opened for every send. A connection or login error before the `DATA` step is safe to retry; a timeout after it is `Unknown` (becomes `stuck`). |

---

## 14. Reference tables

### Route list

| Method | Path | Scope |
| --- | --- | --- |
| GET | `/health` | none |
| GET | `/docs`, `/openapi.json` | none |
| GET | `/v1/accounts/oauth/{provider}/callback` | none (signed `state`) |
| POST | `/v1/accounts/oauth/{provider}/start` | `accounts` |
| POST | `/v1/accounts/smtp` | `accounts` |
| GET | `/v1/accounts` | `accounts` |
| GET, PATCH, DELETE | `/v1/accounts/{id}` | `accounts` |
| POST | `/v1/accounts/{id}/test` | `accounts` |
| GET, PATCH | `/v1/sending/defaults` | `sending` |
| GET, PATCH | `/v1/sending/accounts/{id}/settings` | `sending` |
| POST | `/v1/sending/accounts/{id}/settings/reset` | `sending` |
| GET | `/v1/sending/accounts/{id}/status` | `sending` |
| POST | `/v1/sending/accounts/{id}/pause` | `sending` |
| POST | `/v1/sending/accounts/{id}/resume` | `sending` |
| GET | `/v1/sending/accounts/{id}/preview` | `sending` |
| POST, GET | `/v1/sending/jobs` | `sending` |
| GET, DELETE | `/v1/sending/jobs/{id}` | `sending` |
| POST | `/v1/sending/jobs/{id}/requeue` | `sending` |
| GET, POST, DELETE | `/v1/sending/suppressions` | `sending` |
| POST | `/v1/sending/engine/pause` | `sending` |
| POST | `/v1/sending/engine/resume` | `sending` |
| GET | `/v1/sending/engine/status` | `sending` |
| GET, POST | `/v1/keys` | `admin` |
| POST | `/v1/keys/{id}/rotate` | `admin` |
| DELETE | `/v1/keys/{id}` | `admin` |
| (many) | `/v1/generator/*` | `generator` (see [section 20](#20-email-generator-generator)) |

### All error codes

| Code | Status |
| --- | --- |
| `missing_api_key`, `invalid_api_key` | 401 |
| `wrong_scope` | 403 |
| `invalid_json`, `invalid_path`, `invalid_query` | 400 / 422 |
| `unknown_provider`, `account_not_found`, `job_not_found`, `key_not_found`, `suppression_not_found` | 404 |
| `key_revoked`, `last_admin_key`, `wrong_job_status`, `account_disconnected` | 409 |
| `public_url_not_set`, `oauth_not_configured`, `oauth_denied`, `oauth_bad_callback`, `oauth_bad_state`, `oauth_exchange_failed` | 400 |
| `invalid_email`, `invalid_host`, `insecure_tls`, `smtp_login_failed`, `imap_login_failed`, `not_smtp_account`, `incomplete_server` | 422 |
| `invalid_grace`, `invalid_batch_size`, `invalid_limit`, `invalid_count`, `invalid_value` | 422 |
| Settings codes (see [section 8](#validation)) | 422 |
| `database_down` | 503 |
| `internal_error` | 500 |

---

## 15. Running the server

```bash
cargo run -- serve        # start the API and the scheduler
cargo run -- gmail add    # connect a Gmail account from the terminal (opens a browser)
cargo run -- outlook add  # same for Outlook
cargo run -- imap add     # same for IMAP/SMTP (asks questions)
```

The terminal `add` commands also save the account in the encrypted store when `DATABASE_URL` is set.

### Environment variables

| Name | Required | Default | Use |
| --- | --- | --- | --- |
| `DATABASE_URL` | Yes | none | Postgres 16+ connection. The server refuses to start without it. |
| `AIKIDO_ENCRYPTION_KEY` | No | auto-made in `<AIKIDO_DATA_DIR>/encryption.key` | 32-byte key, base64 (`openssl rand -base64 32`). **Back it up.** If it is lost, every account must be connected again. |
| `AIKIDO_BIND_ADDR` | No | `0.0.0.0:8080` | Server address. |
| `AIKIDO_PUBLIC_URL` | For OAuth web login and the web app | none | The address people use in the browser, for example `https://mail.example.com`. Used for the OAuth redirect links, invite links, and the `Origin` check of the web login. If it is `http://` on another host than localhost, the web login is refused. |
| `AIKIDO_INSECURE_COOKIES` | No | `false` | `true` lets the web login work over plain HTTP on any host (no `Secure` cookie). Only for a trusted network. |
| `AIKIDO_TRUST_PROXY` | No | `false` | `true`: the client IP (for login rate limits) is the last `X-Forwarded-For` entry. Set it only behind your own reverse proxy. |
| `AIKIDO_DATA_DIR` | No | `./data` | Where the auto-made key and first API keys are written (mode `600`). |
| `AIKIDO_ADMIN_API_KEY`, `AIKIDO_ACCOUNTS_API_KEY`, `AIKIDO_SENDING_API_KEY` | No | auto-made | Set your own keys for a deploy. |
| `AIKIDO_SCHEDULER_TICK_SECS` | No | `15` | Loop speed. |
| `AIKIDO_MAX_PARALLEL_ACCOUNTS` | No | `20` | Accounts handled at once per server. |
| `AIKIDO_DRY_RUN` | No | `false` | Log emails and send nothing, with real timing. |
| `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET` | For Gmail | none | Google OAuth app. |
| `MS_CLIENT_ID`, `MS_CLIENT_SECRET` | For Microsoft | none | Microsoft OAuth app. |
| `RUST_LOG` | No | `info` | Log level. |

Logs never contain tokens, passwords, or API keys. Request logs show the method and path only (no query string).

---

## 16. Reply Check (`replies`)

The Reply Check reads the inboxes of connected accounts (read only), finds mail that comes back from our sends, and acts on it.
It never marks, moves, deletes, labels or flags mail. Only mail that links to our sends is stored.
Key scope: `replies` (an `admin` key works too). Env var for your own key: `AIKIDO_REPLIES_API_KEY`.

### Labels

| Label | Meaning | Actions |
| --- | --- | --- |
| `reply` | A person answered. | `replied_at` on the job; suppress `replied` for the job address and the sender; cancel queued `automated` jobs to them (`stop_scope = domain`: their domains too, never free mail domains). |
| `auto_reply` | Out of office and other robot answers. | `on_auto_reply = stop`: same as `reply` (but no `replied_at`). `ignore`: none. |
| `unsubscribe` | Short reply with an unsubscribe keyword in the new text. | `replied_at`; suppress `unsubscribed` for the sender; cancel ALL queued jobs to it (also `manual`). |
| `bounce_hard` | Status `5.x.x` (not `5.2.2`, not `5.7.x`). | `bounced_at`, `bounce_kind = hard`; suppress `hard_bounce`; cancel ALL queued jobs; bounce-rate recheck. |
| `bounce_block` | Status `5.7.x`. | `bounce_kind = block`; bounce-rate recheck (if `count_blocks_in_bounce_rate`). |
| `bounce_soft` | Status `4.x.x`, `5.2.2`, or no code (`unparsed`). | `bounce_kind = soft`; after `soft_bounce_limit` in 30 days: suppress `hard_bounce`. |
| `complaint` | Spam complaint (feedback report). | Suppress `complained`; cancel ALL queued jobs. |
| `delayed` | "Still trying" notice. | Event only. |
| `unknown` | Related mail that could not be parsed. | Stored; event only. |

A reply, auto-reply or unsubscribe that does not match one of our sends is dropped (not stored).
The same email processed twice gives the same result: its actions run once.

A suppression of reason `replied` does not block a `manual` job (`"kind": "manual"` on `POST /v1/sending/jobs`).
Every other reason does.

### Settings

`GET/PATCH /v1/replies/defaults` and `GET/PATCH /v1/replies/accounts/{id}/settings`, `POST .../settings/reset`.
`PATCH` sends only the fields to change; the merged result is validated (`422` with a clear message). New accounts copy the defaults.

| Field | Default | Rule |
| --- | --- | --- |
| `enabled` | `true` | |
| `poll_interval_secs` | `120` | 30 to 3600 |
| `folders` | `["inbox","spam"]` | not empty; `inbox`, `spam` |
| `initial_lookback_days` | `14` | 0 to 90 |
| `match_window_days` | `30` | 1 to 180 |
| `max_messages_per_sync` | `200` | 10 to 1000 (full downloads per run) |
| `stop_scope` | `"email"` | `email`, `domain` |
| `on_auto_reply` | `"ignore"` | `ignore`, `stop` |
| `unsubscribe_keywords` | see below | up to 100 items, 1 to 100 characters each |
| `unsubscribe_max_words` | `30` | 1 to 200 |
| `auto_reply_subject_patterns` | see below | same list rules |
| `bounce_subject_patterns` | see below | same list rules |
| `soft_bounce_limit` | `3` | 1 to 10 |
| `count_blocks_in_bounce_rate` | `true` | |

Default lists: `unsubscribe_keywords`: unsubscribe, remove me, take me off, stop emailing, stop sending, do not contact, don't contact, opt out, opt-out.
`auto_reply_subject_patterns`: automatic reply, auto reply, auto-reply, autoreply, out of office, out of the office, ooo:, on vacation, away from the office, abwesenheitsnotiz, réponse automatique, respuesta automática, risposta automatica.
`bounce_subject_patterns`: undeliverable, delivery status notification, mail delivery failed, returned mail, delivery failure, failure notice, undelivered mail.

### Routes

| Method | Path | What it does |
| --- | --- | --- |
| GET / PATCH | `/v1/replies/defaults` | Read or change the defaults for new accounts. |
| GET / PATCH | `/v1/replies/accounts/{id}/settings` | Read or change one account's settings. A `folders` change updates the sync rows. |
| POST | `/v1/replies/accounts/{id}/settings/reset` | Copy the defaults onto this account. |
| GET | `/v1/replies/accounts/{id}/status` | `can_read`, `read_status`, per-folder last/next sync, last error, label counts of the last 7 days. |
| POST | `/v1/replies/accounts/{id}/sync` | Sync now (`202`). `409 reading_not_available` if the inbox cannot be read. |
| POST | `/v1/replies/accounts/{id}/pause` · `/resume` | Pause or resume the reply check of one account. Resume resets `consecutive_errors`. |
| GET | `/v1/replies/messages` | List. Filters: `label`, `account_id`, `lead_id`, `campaign_id`, `job_id`, `since`. Cursor paging (`cursor`, `limit` 1 to 500). |
| GET | `/v1/replies/messages/{id}` | One inbound email, with `actions`. |
| PATCH | `/v1/replies/messages/{id}` | `{ "label": "reply" }`. Sets `label_source = manual`, undoes the old suppressions and job marks, runs the new label's actions. Cancelled jobs are not restored. |
| GET | `/v1/replies/lookup?email=` | `replied_at`, `bounced {at, kind}`, `unsubscribed_at`, `complained_at`, `suppressed_as`. |
| POST | `/v1/replies/classify` | `{ "raw": "<base64 .eml>", "account_id"? }`. Label, reason, bounce info, reply text, and the match it would make. Writes nothing. |
| POST | `/v1/replies/engine/pause` · `/resume` | Pause or resume all reply checks. |
| GET | `/v1/replies/engine/status` | Paused or running, accounts per `read_status`, emails per label today. |

### Matching

First match wins; the method is saved in `match_method`: `dsn_original` (the original `Message-ID` in a bounce report), `in_reply_to`, `references`
(these three search all accounts), `thread` (Gmail thread id, or Microsoft conversation id), `bounce_recipient` (a send to the bounce recipient in the last 7 days),
`sender_address` (a send to the sender within `match_window_days`), `none`. Methods `thread`, `bounce_recipient` and `sender_address` look at one account only.

### Follow-up safety gate

Sending setting `reply_check_max_age_mins` (default `15`, `0` = off). A follow-up (an `automated` job with `in_reply_to`, or any job of step 2 or later of a sequence) goes out only if the oldest
folder of the account was read within that time. Otherwise `send_after = now + 5 minutes`. `GET /v1/sending/accounts/{id}/status` then shows
`wait.reason = "reply_check_stale"`, or `"reply_check_unavailable"` when `can_read` is false.

### Account fields

`GET /v1/accounts` now shows `granted_scopes`, `can_read` and `read_status` (`ok`, `reconnect_needed`, `login_failed`, `not_configured`).
`POST /v1/accounts/{id}/test` also tests the IMAP login and updates `can_read`.

### Server settings

`AIKIDO_REPLIES_API_KEY`, `AIKIDO_REPLY_TICK_SECS` (15), `AIKIDO_MAX_PARALLEL_SYNCS` (10), `AIKIDO_MAX_MESSAGE_BYTES` (10485760),
`AIKIDO_REPLY_CHECK_ENABLED` (true). Test hooks: `GMAIL_API_BASE_URL`, `MS_GRAPH_BASE_URL`.

---

## 17. Follow-ups (`followups`)

A **sequence** is an ordered list of **steps**: the first email, then follow-up 1 after 3 days, then follow-up 2 after 4 days, and so on (1 to 10 steps).
An **enrollment** puts one recipient into one sequence, sent from one account. It remembers which step is next.
When the recipient replies, bounces, unsubscribes or complains, the enrollment **stops** at once.
Key scope: `followups` (an `admin` key works too). Env var for your own key: `AIKIDO_FOLLOWUPS_API_KEY`.

### How it works

- A step becomes a `send_jobs` row **only when it is due**. A reply between two steps then needs no job to cancel: the job does not exist yet.
  Every follow-up job is `kind = automated`, so the `replied` suppression applies. The sending engine applies all its own rules to each step
  (send days, window, gap, daily cap, suppressions, cooldown, the inbox safety gate).
- The scheduler (`AIKIDO_FOLLOWUPS_ENABLED`, speed: setting `tick_secs`) takes the enrollments with `status = active` and `next_due_at <= now`
  (sequence `active`), each in its own transaction with `FOR UPDATE SKIP LOCKED`. Two server copies never make the same job twice.
  It checks again that the address is not suppressed, that no reply or hard bounce is marked on a job of the enrollment, and that the step still exists. Then it renders the step and queues the job.
- Due time: step 1 is due when enrolled (or at `start_at`). Step N is due `delay` after the **real** `sent_at` of step N-1, so a late first email pushes the rest back.
  `business_days` skip Saturday and Sunday in the time zone of the sending account. The local clock time stays the same over a DST change.
  The sending engine still decides the exact moment (window, gap, cap).
- A step is read when it is due: an edit to a step counts for every enrollment that has not made that step yet. Emails that were sent do not change.

### Enrollment states

```
active --due--> waiting_send --sent--> active (next step)  or  finished (last step)
active / waiting_send --reply, bounce, unsubscribe, complaint, manual--> stopped
active / waiting_send --auto-reply--> paused (pause_until) --time passes--> active
waiting_send --permanent send error, or stalled--> failed
```

`stopped`, `finished` and `failed` never change again. `stop_reason` says why: `reply`, `unsubscribe`, `bounce_hard`, `bounce_block`, `complaint`,
`auto_reply`, `manual`, `suppressed`, `job_cancelled` (stopped); `send_error`, `stalled`, `account_deleted`, `missing_variable`, `invalid_template`, `invalid_variables` (failed).

### Templates

`{{name}}` and `{{name|fallback}}` in the subject, `body_text` and `body_html`. Spaces inside the braces are ignored. Names have letters, digits and `_`.
Values come from the `variables` of the enrollment (text, number or true/false). Built-in names: `{{email}}`, `{{sender_name}}`, `{{sender_email}}`; a caller value with the same name does not replace them.
A value that is missing or empty uses the fallback. No fallback: the render fails. Values are escaped in `body_html`; a line break in a value becomes a space in the subject.
Step 2 and later: an empty `subject` means "Re: " plus the subject of step 1.

A missing variable is found **when you enroll**, for all steps, not when step 3 is due: `POST .../enrollments` answers `422 missing_variable` and enrolls nothing.
`PUT .../steps` refuses new text that needs a value an open enrollment does not have.

### Threading

Step 2 and later, when the sequence has `thread_followups = true` (default): `In-Reply-To` is the `Message-ID` of the last sent step, `References` has all earlier ids,
the subject is "Re: " plus the subject of step 1, and `provider_thread_id` of the earlier email is passed on (Gmail `threadId`).

| Provider | What goes out |
| --- | --- |
| SMTP | The two headers. Checked against Mailpit. |
| Gmail | The two headers in the MIME message, plus `threadId` in the API call. Gmail needs a matching subject to keep the thread. |
| Outlook (Graph) | The two headers in the MIME message (`sendMail` with MIME keeps them; JSON `sendMail` would only allow `x-` headers). **Not tested against a real mailbox.** |

If the earlier `Message-ID` is missing, the step goes out without thread headers and the enrollment gets a `warning` event (`thread_headers_missing`).

### Stop and pause (what the Reply Check does)

The Reply Check calls `followups::on_lead_stopped` in the same transaction that stores the inbound email, so the same email twice runs it once.

| Label | Action |
| --- | --- |
| `reply` | Stop, reason `reply`. A queued job is cancelled. |
| `unsubscribe` | Stop. (The Reply Check already adds the suppression.) |
| `bounce_hard` | Stop. |
| `bounce_block` | Stop (only the enrollment that was blocked). |
| `complaint` | Stop. |
| `auto_reply` | Sequence `stop_on_auto_reply = true`: stop. Else: pause for `auto_reply_pause_days` business days (default 3), then the scheduler resumes it. A queued job is cancelled and made again on resume. If the Reply Check setting `on_auto_reply` is `stop`, the enrollment is stopped like for a reply. |
| `bounce_soft`, `delayed` | No change. The sending engine retries. |

For `reply`, `unsubscribe`, `complaint` and `bounce_hard`, **every other open enrollment of the same address, in all sequences, is stopped too** (and with the Reply Check setting `stop_scope = domain`, every enrollment at that company).
The Reply Check no longer cancels the queued jobs of enrollments itself; this step does. It still cancels queued automated jobs that do not belong to an enrollment.
A step that is already being sent (`sending`) cannot be called back: it goes out, the enrollment stays stopped, and the result is written as a `step_sent_after_end` event.

### Send result

| Job result | Enrollment |
| --- | --- |
| Sent | Next step gets its due time, or the last step finishes the enrollment. |
| Hard bounce at send time | Stopped, `bounce_hard`. |
| Permanent error, or retries ran out | Failed, `send_error`. |
| Retry planned, rate limit, login problem, unknown result (`stuck`) | No change. |
| Skipped (suppressed) or cancelled | Stopped, `suppressed` or `job_cancelled`. |

**Stall.** A follow-up (step 2 or later) that waits in the send queue longer than `max_wait_days` (default 7) is cancelled and the enrollment fails with `stalled`.
This stops a follow-up going out weeks late (the account is paused, the inbox sync is stale for days). Step 1 is never failed this way.

### Routes

| Method | Path | What it does |
| --- | --- | --- |
| POST | `/v1/followups/sequences` | Make a sequence with its steps. |
| GET | `/v1/followups/sequences` | List, newest first. Filter `status`. Cursor paging (`cursor`, `limit` 1 to 500). |
| GET | `/v1/followups/sequences/{id}` | The sequence, its steps, and the number of enrollments per status. |
| PATCH | `/v1/followups/sequences/{id}` | `name`, `stop_on_auto_reply`, `auto_reply_pause_days` (1 to 30), `thread_followups`, and `action`: `pause`, `resume`, `archive`. (`campaign_id` is set by a campaign only: sending it gives `422 campaign_managed`.) |
| PUT | `/v1/followups/sequences/{id}/steps` | Replace the steps. `409 step_in_use` if an enrollment has a removed step queued. |
| POST | `/v1/followups/sequences/{id}/preview` | Render all steps for given `variables` (and `to_email`, `account_id`). Writes nothing. |
| POST | `/v1/followups/sequences/{id}/enrollments` | Enroll 1 to 1000 recipients. |
| GET | `/v1/followups/enrollments` | List. Filters: `sequence_id`, `status`, `email`. Cursor paging. |
| GET | `/v1/followups/enrollments/{id}` | One enrollment with its `events` and send `jobs`. |
| POST | `/v1/followups/enrollments/{id}/pause` · `/resume` · `/stop` | Pause until a person resumes. Resume (due time never in the past). Stop with reason `manual`. `409 enrollment_closed` when it is finished, stopped or failed. |
| GET / PATCH | `/v1/followups/settings` | `one_active_per_email` (default true), `max_wait_days` (1 to 60, default 7), `tick_secs` (5 to 3600, default 30). |
| POST | `/v1/followups/pause` · `/resume` | Pause or resume the whole follow-up engine (saved in the database, so it works with many server copies). Steps already in the send queue still go out. |
| GET | `/v1/followups/status` | Counts by status, due in the next hour, stalled, waiting too long, last tick. |

#### Make a sequence

```bash
curl -s -X POST $BASE/v1/followups/sequences -H "Authorization: Bearer $FOLLOWUPS_KEY" -H 'content-type: application/json' -d '{
  "name": "Cold outreach",
  "steps": [
    { "subject": "Quick question, {{first_name|there}}", "body_text": "Hi {{first_name|there}}, ..." },
    { "delay_amount": 3, "body_text": "Just checking in, {{first_name|there}}." },
    { "delay_amount": 4, "delay_type": "calendar_days", "body_text": "Last note from me." }
  ]
}'
```

Step 1: `delay_amount` 0 (or leave out), a `subject` is required. Step 2 and later: `delay_amount` 1 to 365, `delay_type` `business_days` (default) or `calendar_days`, `subject` empty or set.
Each step needs `body_text`, `body_html`, or both. Bad input gets `422` with `invalid_steps`, `invalid_template`, `invalid_name` or `invalid_setting`.

#### Enroll

```bash
curl -s -X POST $BASE/v1/followups/sequences/$SEQ/enrollments -H "Authorization: Bearer $FOLLOWUPS_KEY" -H 'content-type: application/json' -d '{
  "enrollments": [
    { "to_email": "ana@acme.com", "account_id": "'$ACCOUNT'", "variables": { "first_name": "Ana" }, "idempotency_key": "lead-123" }
  ]
}'
```

Per item: `to_email` or `lead_id` (one of them; both must agree), `account_id` (the one account that sends every step; it never changes), `variables`, `idempotency_key` (1 to 200 characters), `start_at`.
Every enrolled address is a lead: with only `to_email`, the lead is found by address or made (source `enrollment`). `enrollments.lead_id` is always set. The fields of the lead fill the steps (your `variables` fill what the lead has not). At send time the **current** lead fields win over the values saved at enroll time (a fixed typo goes out right). The `fallback` of a custom field fills a value that is missing.
Other item errors: `lead_not_found`, `lead_email_mismatch`.
The answer has `items` (`index`, `status` = `enrolled`, `duplicate` or `error`, `enrollment`, `error {code, message}`) and the counts `enrolled`, `duplicates`, `errors`.
Item errors: `invalid_to_email`, `account_not_found`, `invalid_variables`, `invalid_idempotency_key`, `suppressed`, `already_enrolled`. The other items are still enrolled.
The same `idempotency_key` twice returns the old enrollment (`duplicate`). With `one_active_per_email` (default), an address can be in one open enrollment at a time, in all sequences.
Whole-call errors: `422 invalid_batch_size`, `422 missing_variable` (nothing enrolled), `404`, `409 sequence_archived`. A paused sequence takes enrollments; they wait.

### Server settings

`AIKIDO_FOLLOWUPS_API_KEY`, `AIKIDO_FOLLOWUPS_ENABLED` (true; `false` = this server copy makes no steps). The sending setting `followups_first` (default `true`) puts a follow-up
in front of a first email when the account has both ready, under the same daily cap. Priority still wins over it.

---

## 18. Leads (`leads`)

A **lead** is one person you may email. One row per email address. Key scope: `leads` (an `admin` key works too). Env var for your own key: `AIKIDO_LEADS_API_KEY`.
The campaigns that use leads are in [section 19](#19-campaigns-campaigns).

### Ideas

- **The email is the key.** It is trimmed and lower-cased when saved (`  John@Example.COM ` becomes `john@example.com`). Syntax is checked: one `@`, a valid domain, 254 characters.
- **`contact_state` is the system's. `interest` is a person's.** `contact_state` (`new`, `in_sequence`, `contacted`, `replied`, `bounced`, `unsubscribed`, `completed`) is never set through the API.
  `interest` (`interested`, `meeting_booked`, `won`, `not_interested`, `wrong_person`, `lost`) is set by you. A system event never overwrites it.
- **Custom fields.** Up to 200 definitions (`/v1/leads/fields`). A lead holds a value (text, up to 1000 characters) for a field key. The key becomes the template variable `{{key}}`.
  Kinds: `text`, `number`, `url` (`http://` or `https://`), `date` (`YYYY-MM-DD`). An unknown key is refused. An empty value means "no value".
- **Tags**: short labels, lower-cased, up to 20 per lead, 50 characters each, no commas.
- **Suppressed** leads are saved like the others. The `suppressed` flag shows that the address (or its domain) is blocked. A suppressed lead is never enrolled.
- **Timeline** (`/v1/leads/{id}/events`): `created`, `updated`, `imported`, `list_added`, `list_removed`, `campaign_added`, `enrolled`, `step_sent`, `replied`, `auto_replied`, `bounced`, `unsubscribed`, `complained`, `interest_changed`, `stopped`, `note`. The other engines write the system events in the same transaction as their own work. A failed lead update is logged and undone alone: it never blocks a send or a stop.
- **The system sets `contact_state`** (see below). A reply never changes `interest`.

### The lead object

`id`, `email`, `email_domain`, `first_name`, `last_name`, `company_name`, `job_title`, `phone`, `website`, `linkedin_url`, `location`, `timezone` (an IANA name like `Asia/Dhaka`; stored only for now),
`custom_fields` (object), `tags`, `contact_state`, `interest`, `verification`, `source` (`api`, `csv`, `enrollment`, `auto_database`, `manual`), `source_ref`, `notes`,
`last_contacted_at`, `last_replied_at`, `created_at`, `updated_at`. `GET /v1/leads/{id}` adds `lists`, `suppressed` and `active_enrollment_id`.

### Contact state (set by the system)

| Event | Lead `contact_state` |
| --- | --- |
| Enrolled (also by a campaign) | `in_sequence` (not for `bounced` and `unsubscribed`: they never change again) |
| An email is sent (a step, or a manual send to a known lead) | `new` or `in_sequence` becomes `contacted`; `last_contacted_at` is set |
| The last step went out and nobody replied | `completed` |
| An enrollment is stopped by hand, or fails | `in_sequence` goes back to `contacted` (something was sent) or `new` |
| A reply | `replied`, `last_replied_at` is set. Stays so until a new enrollment. |
| An out-of-office reply | No change. A timeline event `auto_replied`. |
| A hard bounce | `bounced` |
| An unsubscribe or a spam complaint | `unsubscribed` |

`interest` is set only through the API. It is never changed by the system. `GET /v1/leads/{id}` shows `active_enrollment_id` (the sequence running now) and `campaigns`.

### `GET /v1/leads/{id}/context`

For the AI agent and the Email Generator, in one call: `lead` (with lists, campaigns, suppressed flag), `fields` (every custom field with its label, kind and this lead's value),
`events` (the last 50, newest first), and `messages` (up to 50 emails sent to the lead and up to 50 messages that came back, oldest first: `direction`, `at`, `subject`, `text`, `label`, `step_number`; texts are cut at 10000 characters).

### Old data (backfill)

`cargo run -- backfill-leads` (also runs when `serve` starts) gives every enrollment with no lead a lead (found by address, or made with source `enrollment`).
A lead made or linked this way that still says `new` gets its state, `last_contacted_at` and `last_replied_at` from the old jobs, enrollments and suppressions. Safe to run twice.
Migration `0005` clears the old `lead_id` and `campaign_id` values (they pointed at nothing, because no leads or campaigns existed) before it adds the foreign keys.

### Email check (`verification`)

`syntax_ok` (the shape is fine), `mx_ok` (the domain has a mail server), `no_mx` (it has none), `invalid`, `unknown`.
New domains get one MX lookup (3 seconds, 20 at a time), cached 7 days in `email_domains`. 1000 leads on one domain make one lookup.
A timeout or a DNS error is **not** `no_mx`: the lead stays `syntax_ok`. The setting `mx_check` turns the lookup off.
Imports check each batch. `POST /v1/leads` and `POST /v1/leads/bulk` wait up to 10 seconds for DNS before they answer.

### Leads

| Route | What it does |
| --- | --- |
| `POST /v1/leads` | Make one lead (`201`). `409 email_exists`. `422 invalid_lead` names the bad value. |
| `GET /v1/leads` | List, newest first. Filters: `q`, `list_id`, `campaign_id`, `tag`, `contact_state`, `interest`, `verification`, `suppressed`, `created_from`, `created_to`. |
| `GET /v1/leads/{id}` | One lead with `lists`, `suppressed`, `active_enrollment_id`. |
| `PATCH /v1/leads/{id}` | Send only the fields to change. `null` or empty text clears. `custom_fields` merges (`null` removes a key). `tags` replaces. `409 email_locked` if the lead has an open enrollment. `409 email_exists`. |
| `DELETE /v1/leads/{id}` | Erase (below). |
| `GET /v1/leads/{id}/events` | Timeline, newest first. `cursor` (the last event id) and `limit`. |
| `POST /v1/leads/{id}/notes` | Add a note (`{ "text": "..." }`). |
| `POST /v1/leads/bulk` | Up to 1000 leads. See below. |
| `POST /v1/leads/actions` | One action on many leads. See below. |

**Search and paging.** `q` finds part of a word in the email, first name, last name and company name (trigram index). `%` and `_` are plain characters.
`limit` is 1 to 500 (default 50). The answer is `{ leads, next_cursor }`. `cursor` is the **id of the last lead** of the previous page: the server reads that lead's position first.
If that lead was deleted, you get `400 invalid_cursor`. Start again without a cursor.

**Delete (erase).** `DELETE /v1/leads/{id}` stops the open enrollments (reason `lead_deleted`) and cancels their queued emails, adds the address to the suppression list with reason `erased`,
and deletes the lead with its lists, tags and timeline. The address is never contacted again, even if it is imported again. An older suppression keeps its reason.
To email the address again, remove it with `DELETE /v1/sending/suppressions?value=...&kind=email`. The answer: `{ deleted, stopped_enrollments, cancelled_jobs }`.

### `POST /v1/leads/bulk`

```bash
curl -s -X POST $BASE/v1/leads/bulk -H "Authorization: Bearer $LEADS_KEY" -H 'content-type: application/json' -d '{
  "dedupe_mode": "fill_empty", "list_id": "'$LIST'", "tags": ["q4"],
  "leads": [ { "email": "ana@acme.com", "first_name": "Ana", "custom_fields": { "pain_point": "slow sales" } } ]
}'
```

1 to 1000 leads. Optional `campaign_id` puts every lead in that campaign as `pending` (`404 campaign_not_found`, `409 campaign_closed`). The answer has `created`, `updated`, `skipped`, `invalid` and `items` (one per lead you sent, same order): `index`, `status` (`created`, `updated`, `skipped`, `invalid`), `lead_id`, `error {code, message}`.
A bad lead does not stop the others. `dedupe_mode` when an email exists:

| Mode | What happens |
| --- | --- |
| `skip` | The old lead is not touched (`skipped`). It still joins `list_id` and gets `tags`. |
| `fill_empty` (default) | Only empty fields of the old lead are filled. Nothing to fill: `skipped`. |
| `overwrite` | A field that has a value in the row replaces the old value. An empty value never erases data. |

Two rows with the same email: the first wins, the second is `skipped`. `custom_fields` merge key by key. Tags are only added. `interest` is never changed by an import.

### `POST /v1/leads/actions`

`{ "action": "...", "lead_ids": [...] }` or `{ "action": "...", "filter": { ... } }` (not both; the filter takes the same fields as `GET /v1/leads` and needs at least one condition).
Up to 10000 leads (`422 too_many_leads`). Actions: `add_to_list` / `remove_from_list` (`list_id`), `add_tags` / `remove_tags` (`tags`), `set_interest` (`interest`, or none to clear),
`delete` (like `DELETE /v1/leads/{id}`; `erased` has the counts), `verify` (check again, ignoring the cache; waits up to 25 seconds; `verify` has `mx_ok`, `no_mx`, `unchecked`, `leads_changed`).
The answer: `{ matched, changed }`. `add_tags` leaves a lead alone if it would pass 20 tags.

### Custom fields

| Route | What it does |
| --- | --- |
| `GET /v1/leads/fields` | All definitions. |
| `POST /v1/leads/fields` | `{ key, label?, kind?, fallback? }` (`201`). `key`: lower case letters, digits, `_`, starts with a letter, 1 to 40, not a built-in name. `422 invalid_key`, `409 field_exists`, `422 too_many_fields`. |
| `PATCH /v1/leads/fields/{key}` | Change `label` or `fallback`. The key and kind are fixed (`422 field_locked`). |
| `DELETE /v1/leads/fields/{key}` | Delete the field and remove its value from every lead (`leads_cleaned`). Refused with `409 field_in_use` (the message names the sequences) while a sequence that has an open enrollment, or the sequence of an active or paused campaign, has a step that uses `{{key}}`. An archived sequence counts too: it can be resumed. |

### Lists

`GET /v1/leads/lists` (with `member_count`), `POST` (`{ name, description? }`, `409 list_exists`), `PATCH /v1/leads/lists/{id}`, `DELETE /v1/leads/lists/{id}` (the leads stay).
A lead can be in many lists. Add and remove through `actions`, `bulk` (`list_id`) or an import (`list_id`).

### Import (CSV)

1. `POST /v1/leads/imports/preview`: multipart, field `file`. Reads the first 20 rows. Returns `delimiter`, `headers`, the guessed `mapping`, `email_column`, `sample_rows`. Nothing is saved.
2. `POST /v1/leads/imports`: multipart. Fields: `file`, and optional `mapping`, `dedupe_mode`, `list_id`, `campaign_id`, `tags` (comma separated), `create_fields` (`true`). Answer `201` with the import.
3. `GET /v1/leads/imports/{id}`: live counters. `GET /v1/leads/imports`: the list (`status`, `cursor`, `limit`). `POST /v1/leads/imports/{id}/cancel`.

```bash
curl -s -X POST $BASE/v1/leads/imports -H "Authorization: Bearer $LEADS_KEY" \
  -F file=@leads.csv -F dedupe_mode=fill_empty -F list_id=$LIST -F tags=q4,saas \
  -F 'mapping={"Email Address":"email","Company":"company_name","Pain":"custom:pain_point"}'
```

- **Mapping**: column name to `email`, `first_name`, `last_name`, `company_name`, `job_title`, `phone`, `website`, `linkedin_url`, `location`, `timezone`, `notes`, `tags`, `custom:<key>`, or `ignore`.
  Without `mapping` it is guessed from the headers (`E-mail Address`, `firstname`, `Organization`, ...). A column whose name matches a custom field key goes to that field. Columns not in a given mapping are ignored.
  No email column: `422 no_email_column`, before anything is saved. A `custom:<key>` that does not exist: `422 unknown_field`, unless `create_fields=true`.
- **File**: UTF-8, with or without BOM. The separator (`,` `;` tab `|`) is found by itself. Rows may have too few or too many cells. Limits (settings): `import_max_rows` (100000, `422 too_many_rows`), `import_max_bytes` (50 MB, `413 file_too_large`).
- **Rows**: a bad email, a value over 1000 characters, an unknown time zone or a bad custom value makes the row `invalid`. The first 1000 bad rows are in `errors` (`row` = the line in the file, `reason`). A row with the same email as an earlier row of the file is `skipped`.
- **Counters** (`created`, `updated`, `skipped`, `invalid`, `rows_done`, `total_rows`) are saved with every batch of 1000 rows, so they show live progress.
- **The worker** takes pending imports with `FOR UPDATE SKIP LOCKED` and a 120-second lease. If a server copy dies, another resumes after the last saved batch. The counters are right. An import that is tried 5 times and does not finish is `failed`.
  `AIKIDO_LEADS_ENABLED=false` makes a server copy skip the worker. The uploaded file is kept in the database and deleted when the import ends.
- Suppressed emails are imported. They are skipped when enrolled.

### Export

`GET /v1/leads/export.csv` takes the same filters as `GET /v1/leads`. It is streamed in pages, newest first, with no size limit. Columns: the lead fields, then one column per custom field (named by its key).
Tags are joined with `;`. A cell that starts with `=`, `+`, `-` or `@` gets a `'` in front, so Excel does not run it as a formula. An exported file imports back without changes.

### Settings (`/v1/leads/settings`)

`GET` and `PATCH` (send only the fields to change; `422 invalid_setting` or `unknown_setting`).

| Setting | Default | Meaning |
| --- | --- | --- |
| `import_max_rows` | 100000 | 1 to 1000000. |
| `import_max_bytes` | 52428800 | 1024 to 209715200. |
| `mx_check` | `true` | Look up the MX record of new domains. |
| `default_dedupe_mode` | `fill_empty` | Used when an import or bulk call does not say. |
| `feeder_tick_secs` | 60 | 5 to 3600. How often the campaign feeder runs. |
| `default_daily_new_leads` | 50 | 1 to 5000. The daily limit of a new campaign. |
| `free_mail_domains_extra` | `[]` | Free-mail domains to add to the built-in list (gmail.com, outlook.com, ...). The company stop skips them. |

### Server settings

`AIKIDO_LEADS_API_KEY`, `AIKIDO_CAMPAIGNS_API_KEY`, `AIKIDO_LEADS_ENABLED` (true; `false` = this server copy runs neither the CSV import worker nor the campaign feeder).
Migration `0004_leads.sql` needs the Postgres extension `pg_trgm` (in the standard contrib package; `CREATE EXTENSION` must be allowed for the database user).

---

## 19. Campaigns (`campaigns`)

A **campaign** is one sequence, a pool of sender accounts, a set of leads, and a daily limit of new leads. Key scope: `campaigns` (an `admin` key works too).
Env var for your own key: `AIKIDO_CAMPAIGNS_API_KEY`. The routes read leads and sequences themselves: this key does not need the `leads` or `followups` key.

**The main idea.** Adding 5000 leads does not enroll 5000 at once. The **feeder** enrolls up to `daily_new_leads` a day. This keeps the account caps safe and lets you stop a bad campaign early.
The sending engine still decides when each email goes out (window, gap, daily cap per account).

### Status

```
draft --launch--> active --pause--> paused --resume--> active
active --(nothing pending, every enrollment ended)--> completed
paused or completed --archive--> archived
```

Wrong state: `409 invalid_status`. A new sequence made with the campaign starts `paused`; `launch` makes the campaign and its sequence `active`. `pause` pauses the sequence too, so the follow-up engine makes no new steps
(emails already in the send queue still go out). `archive` skips the pending leads (`campaign_archived`) and archives the sequence (open enrollments are not stopped).
A campaign with no lead at all never completes. A completed or archived campaign takes no more leads.

### Routes

| Route | What it does |
| --- | --- |
| `POST /v1/campaigns` | Make a campaign (`201`, `draft`). Body: `name`, `sequence_id` (an existing sequence that no campaign owns) **or** `sequence` (a new one: same body as `POST /v1/followups/sequences`), `account_ids`, and the settings below. |
| `GET /v1/campaigns` | List (`status`, `cursor`, `limit`), with `counts`. |
| `GET /v1/campaigns/{id}` | One campaign with `accounts` (and whether each is `healthy`), `sequence`, `steps`, `counts` and `warnings`. |
| `PATCH /v1/campaigns/{id}` | `name`, `daily_new_leads`, `account_pick`, `stop_company_on_reply`, `allow_unverified`, `timezone`, `account_ids` (replaces the pool). Removing an account does not touch its open enrollments; only new enrollments use the new pool. |
| `POST /v1/campaigns/{id}/leads` | Add leads: `lead_ids` (up to 10000), `list_id`, or `filter` (the fields of `GET /v1/leads`, including `campaign_id`). Answer: `matched`, `added`, `already_in_campaign`. Allowed in `draft`, `active`, `paused`. |
| `DELETE /v1/campaigns/{id}/leads` | Take `pending` leads out (same body). Answer: `matched`, `removed`, `not_removed`. Stop an enrolled lead through its enrollment. |
| `GET /v1/campaigns/{id}/leads` | The leads, oldest added first: `state`, `skip_reason`, `enrollment_id`, `enrollment_status`. Filters `state`, `skip_reason`. `cursor` is the id of the last lead (`400 invalid_cursor` if it left the campaign). |
| `POST /v1/campaigns/{id}/launch` `pause` `resume` `archive` | Status changes. |
| `GET /v1/campaigns/{id}/stats` | `sent`, `replied`, `bounced`, `unsubscribed`, `complained`, `auto_replied`, `leads_contacted`, `reply_rate`, and the same numbers `by_step` and `by_account`. |
| `POST /v1/campaigns/feeder/pause` `resume` | Pause or resume the feeder of every campaign (a flag in the database: all server copies obey it). |
| `GET /v1/campaigns/status` | `paused`, `last_tick_at`, `tick_secs`, `active_campaigns`, `waiting` (campaigns that are stuck, with the reason) and `capacity_warnings`. |

Settings: `daily_new_leads` (1 to 5000, default from the lead setting `default_daily_new_leads`), `account_pick` (`least_loaded` default, or `round_robin`),
`stop_company_on_reply` (default `false`), `allow_unverified` (default `false`), `timezone` (IANA name; the day boundary of `daily_new_leads`; default: the sending default).

### Launch checks

`422` unless: the sequence has a step (`no_steps`), the pool has an account (`no_accounts`) and one of them is healthy (`no_healthy_account`: connected, not paused, not in a bounce-rate pause),
and every `{{variable}}` of the steps is a built-in name (`email`, `sender_name`, `sender_email`), a lead field (`first_name`, `company_name`, ...), a custom field, or has its own fallback (`missing_variables`, the message names them).
A plan that is bigger than the pool can send (`daily_new_leads` x steps over the sum of the daily caps of the healthy accounts) is **not** an error. It is in `warnings`, and in `GET /v1/campaigns/status`.

### The feeder

Every `feeder_tick_secs` (setting, default 60) for each `active` campaign, oldest pending lead first:

1. `room = daily_new_leads - leads enrolled today` (the day starts at midnight in the campaign `timezone`). No room: next campaign.
2. A lead is **skipped** with a reason, and does not use up the room: `suppressed` (also a bounced or unsubscribed lead), `unverified` (verification is not `mx_ok` or `syntax_ok`, unless `allow_unverified`),
   `active_elsewhere` (an open enrollment in another sequence, with the follow-up setting `one_active_per_email`), `missing_variable` (a step needs a value this lead does not have).
3. An account is picked from the healthy accounts of the pool: `least_loaded` (fewest open enrollments, a tie at random) or `round_robin` (the next one after the last used). If none is healthy,
   the lead stays `pending` (not skipped), the campaign gets a `warning`, and the feeder tries again next tick.
4. The lead is enrolled with the same function as the API (key `campaign:<campaign>:lead:<lead>`; a retry after a crash gets the old enrollment back). The lead becomes `in_sequence` and gets an `enrolled` event.
5. The campaign becomes `completed` when nothing is pending and every enrollment of the campaign is `finished`, `stopped` or `failed`.

With many server copies: a campaign is claimed with `FOR UPDATE SKIP LOCKED` and a 5-minute lease, each lead row is locked while it is worked, and the key makes a repeat harmless.
`AIKIDO_LEADS_ENABLED=false` makes a server copy skip the feeder.

### Company stop (`stop_company_on_reply`)

When a lead replies (label `reply`, not `auto_reply`), the other open enrollments of the same campaign with the same email domain stop (reason `company_replied`), and the pending leads of that domain are skipped (`company_replied`).
Free-mail domains are never one company: the built-in list (gmail.com, googlemail.com, outlook.com, hotmail.com, live.com, msn.com, yahoo.com, ymail.com, icloud.com, me.com, mac.com, aol.com, proton.me, protonmail.com, gmx.com, gmx.net, mail.com, zoho.com, yandex.com, fastmail.com)
plus the lead setting `free_mail_domains_extra`.

---

## 20. Email Generator (`generator`)

An AI writes the **personal part** of each sequence step, for each lead, from what the database knows about the lead. The follow-up scheduler then sends the step like any other.
Key scope: `generator` (an `admin` key works too). Env var for your own key: `AIKIDO_GENERATOR_API_KEY`. No UI.

**Lead data goes to a third-party LLM.** The name, job title, company, website, location, custom fields, up to 5 notes, a short timeline and the text of the last 2 emails sent to the lead are sent to the provider you set up.
The email address, the phone number, other leads, suppression data, keys and the text of replies are never sent. For no third party, use a local model (Ollama) as an `openai_compatible` provider.

### The main idea

```
step template:  "Hi {{first_name}}, {{ai:opener}} ... {{sender_name}}"
                                   |
                       slot "opener": instruction + max words + fallback
                                   |
generation worker (a few hours before the step is due)
   1. read the lead context   (the same function as GET /v1/leads/{id}/context)
   2. build the prompt        (fixed part first, lead part last)
   3. call the LLM            (JSON out)
   4. check the output        (the guard)
   5. save it in `generations` as ready (or pending_review)
                                   |
follow-up scheduler (when the step is due)
   ready / approved -> fill the slots, render, make the send job
   not ready        -> the fallback text, or wait (setting on_failure)
```

The AI never writes a whole email. Your words stay around the slots, so the signature, the offer and the unsubscribe line are always yours.

### Slots in a step

- In the text: `{{ai:key}}` or `{{ai:key|fallback}}` (the inline fallback is used when there is no AI text and the slot has no `fallback`).
- In the step: `ai_slots`, a list of `{ key, instruction, max_words, allow_links, fallback }` (sent in `POST /v1/followups/sequences` and `PUT .../steps`; shown in `GET`).
  `key` has letters, digits and `_`. `instruction` is 1 to 1000 characters. `max_words` is 1 to 200 (default 40). `allow_links` is `false` by default. `fallback` is up to 500 characters.
- Every `{{ai:key}}` needs an entry (`422 unknown_ai_slot`); every entry must be used (`422 unused_ai_slot`); a bad entry is `422 invalid_ai_slots`.
- `{{ai:...}}` in the subject is allowed on step 1 only (`422 ai_slot_in_threaded_subject` on step 2 and later: a changed subject breaks the thread).
- A sequence has an optional `brief` (up to 4000 characters: product, offer, audience). It goes into the prompt. Set it with `POST` or `PATCH /v1/followups/sequences`; an empty text clears it.
- Enrolling and launching do not need a value for an AI slot. The enrollment shows `wait_reason` while a due step is held.
- The step hash covers the text and the slots. When it changes, text that was written for the old step is `stale` and is made again.

### The worker

Each tick (setting `tick_secs`, default 10) the worker, if the generator is not paused and the slot provider works:

1. marks text for a changed step `stale`;
2. inserts a `pending` row for each open enrollment whose next step has slots and is due within `lookahead_hours` (a unique index on enrollment, step and hash stops doubles);
3. claims `pending` rows with `FOR UPDATE SKIP LOCKED`, a claim id and a 10 minute lease, oldest due first (a crashed copy's rows come back after the lease);
4. checks the budgets, builds the prompt, calls the model, runs the guard, saves the text (`ready`, or `pending_review` in review mode), and writes one `llm_calls` row per call.

No database transaction is open during the call. `AIKIDO_GENERATOR_ENABLED=false` makes a server copy skip the worker.

| `generations.status` | Meaning |
| --- | --- |
| `pending` | Waits for the worker (or for a retry time, or for the budget). |
| `generating` | A worker has it (lease: 10 minutes). |
| `ready` | The text can be used. |
| `pending_review` | Review mode: waits for approval. The step is held. |
| `approved` | A person approved it, or edited it by hand. |
| `rejected` | A person rejected it. The worker makes it again; the step is held meanwhile. |
| `failed` | The model's text did not pass the guard twice, the request was refused, or the tries ran out. `error_code` says why. |
| `cancelled` | The enrollment ended (stopped, failed, finished). |
| `stale` | The step changed, or the enrollment was paused for over twice the look-ahead. |

Retries: a rate limit, server error or timeout is retried inside the call (`max_attempts`, with back-off that respects `Retry-After`). If the call still fails, the row goes back to `pending` with a retry time (30 s times the attempt, at most 10 min),
and after `max_attempts` claims it is `failed`. A 400 fails the row at once. A 401 or 403 sets the provider to `auth_failed`, keeps the row `pending`, and pauses the worker until the key is replaced or a test succeeds.
An answer that is not valid JSON, or does not pass the guard, gets **one** repair try (the reason goes back to the model; the refused text does not). Then the row is `failed`.

### What the scheduler does at due time

| Text for the current step hash | `on_failure = fallback` (default) | `on_failure = wait` |
| --- | --- | --- |
| `ready` or `approved` | Use it. | Use it. |
| `pending_review` or `rejected` | Hold, wait reason `generation_review`, no time limit. | Same. |
| `failed` | Use the fallback texts at once. | Hold (`generation_pending`) until `max_wait_mins`, then the fallback texts. A hand edit (`PATCH .../generations/{id}`) ends the wait. |
| none, `pending`, `generating`, `stale` | Hold for `due_grace_mins` (default 5), then the fallback texts. `0` = at once. | Hold until `max_wait_mins` (default 120), then the fallback texts. |

The fallback of a slot is its `fallback`, else the inline `{{ai:key|fallback}}`. A slot with neither fails the enrollment with `generation_failed`.
A held step is still `active` (the stall check of `waiting_send` does not apply). The enrollment events show `step_waiting` once per wait and `generation_fallback` when a fallback was used.

### Providers

One trait, three kinds: `anthropic`, `openai_compatible` (OpenAI, OpenRouter, Ollama, LM Studio, vLLM), and a fake for tests. Plain HTTP, no SDK.

- **anthropic**: Messages API. The fixed part of the prompt is a `system` block with `cache_control`, so the next leads of the same step read it from the cache (`cached_tokens` in `/usage`). Newer models
  (Opus/Sonnet/Haiku 5.x, Fable, Mythos, Opus 4.7/4.8) accept no custom `temperature`, think by default, and know `effort`: for them the client leaves `temperature` out, sends `effort: low`, and adds 1024 to `max_tokens` for thinking.
- **openai_compatible**: `POST {base_url}/chat/completions`. `base_url` includes the version part, for example `https://api.openai.com/v1` or `http://localhost:11434/v1`. The key is optional. `response_format` JSON mode is sent when `json_mode` is true.
- The key is encrypted (AES-256-GCM, AAD = the provider id), write only, never returned, never logged.

### The guard

Every output is checked by pure functions before it is saved. A failure gets one repair try, then `failed`.

1. every slot is there and not empty; 2. the word count is within `max_words`; 3. no link unless `allow_links`; 4. no email address or phone number; 5. no leftover placeholder (`{{`, `[Name]`, `<company>`, `XXX`);
6. no AI talk (setting `banned_phrases`); 7. plain text only (no markdown); 8. no spam word (setting `spam_words`; `spam_check` = `warn` or `fail`); 9. language (setting `language`: script and stop words; warning only).

Warnings are saved in the `warnings` of the generation. The guard cannot prove that a fact is true. The prompt says "only use facts from the context", and review mode is the real check for that.

### Settings (`GET`, `PATCH /v1/generator/settings`)

| Setting | Default | Meaning |
| --- | --- | --- |
| `slot_provider_id`, `draft_provider_id` | none | The providers for per-lead slots (cheap, fast) and for the sequence writer (strong). They can be the same. |
| `lookahead_hours` | 6 | 1 to 72. Write a step when it is due within this time. |
| `on_failure` | `fallback` | `fallback` or `wait`. |
| `max_wait_mins` | 120 | 1 to 10080. With `wait`. |
| `due_grace_mins` | 5 | 0 to 120. How long a due step with no AI text yet is held before `on_failure` applies (step 1 of a new enrollment is due before the worker has run). |
| `review_required` | false | Every new text goes to `pending_review`. A campaign can override it. |
| `context_max_chars` | 4000 | 500 to 20000. The lead data is cut to this size. |
| `max_output_tokens` | 400 | 50 to 4000. Per call. |
| `temperature` | 0.7 | 0 to 2. |
| `max_attempts` | 3 | 1 to 10. Tries for one call, and claims for one text. |
| `concurrency` | 4 | 1 to 32. Calls at the same time, per server copy (and at most the provider's `max_concurrency`). |
| `daily_token_budget` | 0 | 0 = no limit. Input + output tokens of today (UTC) from `llm_calls`, all purposes. Calls in flight can pass it a little. |
| `language` | none | For example `en`. In the prompt, and for the light check. |
| `banned_phrases`, `spam_words`, `spam_check` | short lists, `warn` | Editable. |
| `tick_secs` | 10 | 1 to 3600. |
| `prices` | none | `{ "model-id": { "input": 0.5, "output": 2.0 } }`, USD per 1M tokens, to show money in `/usage`. |

A bad value is `422 invalid_setting`; an unknown name is `422 unknown_setting`; a provider id that does not exist is `422 unknown_provider`.

### Routes

| Route | What it does |
| --- | --- |
| `GET`, `PATCH /v1/generator/settings` | Read or change the settings. |
| `GET`, `POST /v1/generator/providers` | List or add a provider (`201`). Body: `kind`, `name`, `base_url`, `model`, `api_key`, `max_concurrency` (1 to 64), `timeout_secs` (5 to 600), `json_mode`. `422 invalid_provider`. |
| `PATCH`, `DELETE /v1/generator/providers/{id}` | Change (a new `api_key` also clears `auth_failed`) or remove. A provider a setting uses: `409 provider_in_use`. |
| `POST /v1/generator/providers/{id}/test` | One tiny call. Answer `{ ok, model, duration_ms, error_code, message }` (always `200` if the provider exists). |
| `POST /v1/generator/sequences/draft` | The sequence writer. Body: `brief` (`product` required; `offer`, `audience`, `tone`, `language`, `footer`), `steps` (1 to 10, default 3), `delay_days`, `save`, `name`. Answer: `steps` (a valid body for `PUT .../steps`), `sequence_id`, tokens. The draft is checked like a normal steps body and gets one repair try (`422 draft_invalid`). Step 2 and later always get an empty subject. With `save: true` it makes a **paused** sequence with the brief. |
| `POST /v1/generator/preview` | `sequence_id`, `step_number`, `lead_id`, `account_id`. Writes the step for the lead now and returns the full email, the slot text and the tokens. Saves only an `llm_calls` row. |
| `GET /v1/generator/generations` | Filters `campaign_id`, `sequence_id`, `lead_id`, `enrollment_id`, `status`; `cursor`, `limit` (keyset paging, newest first). |
| `GET /v1/generator/generations/{id}` | One row with the slot text. |
| `PATCH /v1/generator/generations/{id}` | Edit slot text by hand (`{ "slots": { "opener": "..." } }`). The guard runs. The text counts as `approved`. `409` while it is being written, closed, or the step changed. |
| `POST /v1/generator/generations/{id}/approve`, `/reject` | Review mode. Reject = the worker makes it again. |
| `POST /v1/generator/generations/bulk` | `{ "action": "approve" \| "reject" \| "regenerate", "filter": { campaign_id, sequence_id, lead_id, status, ids } }`. An empty filter is `422 empty_filter`. Answer: `matched`, `changed`. |
| `GET /v1/generator/usage` | `from`, `to` (UTC dates). Tokens (input, output, cached), calls and failed calls by day, campaign, model and purpose, plus `cost_usd` where a price is set. |
| `GET /v1/generator/status` | `paused`, `worker_enabled`, `last_tick_at`, `queue` (count per status), `budget`, the two providers, and `warnings` (`paused`, `no_slot_provider`, `provider_auth_failed`, `daily_budget_reached`, `campaign_budget_reached`, `failed_generations`, `awaiting_review`). |
| `POST /v1/generator/pause`, `/resume` | Pause the worker (a flag in the database: all server copies obey it). Text that is ready is still used. |
| `GET`, `PATCH /v1/generator/campaigns/{id}` | `review_required` (overrides the setting; `null` = use the setting) and `token_budget` (total input + output tokens for the campaign; `null` = no limit), and `tokens_used`. |

Errors of the calls that reach the model: `422 generator_not_ready` (no provider, or its key was refused), `409 daily_budget_reached`, `422 generation_<code>` (the text did not pass the guard: `generation_guard_failed`, `generation_bad_output`, ...),
`502 generation_<code>` (the provider could not answer: `generation_server_error`, `generation_rate_limited`, `generation_timeout`, ...).

### Budgets and cost

- Two providers: a cheap model for slots, a strong one for drafts.
- The fixed part of the prompt (rules, the step text, the slots, the brief) comes first and is the same for every lead of a step, so the provider can cache it. Check `cached_tokens` in `/usage`.
- A step is written just before it is due, with fresh lead data. A lead who replies never costs tokens for later steps.
- A daily budget and a campaign budget stop the calls. The rows stay `pending`; the steps use their fallback text (or wait). `status` says why.
- Launching a campaign with AI slots gives `422 generator_not_ready` if there is no working slot provider, and adds a token estimate to the campaign `warnings`.

### Changes to other parts

`DELETE /v1/leads/fields/{key}` also looks in slot instructions and fallbacks for `{{key}}`. Erasing a lead deletes its generations. A stop, a failure or a finish of an enrollment cancels its open generations;
a resume after a pause of over twice the look-ahead makes older text `stale`. The follow-up preview (`POST /v1/followups/sequences/{id}/preview`) shows `[AI: key]`, or with `generate: true` and a `lead_id` asks the model for each step with slots.


## 21. Import mapping (`leads`): Auto Database part 1

An uploaded CSV can have any column names. The server reads the headers and up to 5 real values per column, maps the sure columns by rules, asks a model about the rest, and returns one proposal.
Nothing goes into `leads` until you confirm. The import worker is the same as before (section 18).

### Flow

1. `POST /v1/leads/imports?auto_map=true` (multipart: `file`, optional `dedupe_mode`, `list_id`, `tags`, `campaign_id`). The file is stored with status `uploaded`. The answer is the import plus `proposal`.
   `auto_map` is `false` by default: the old call (start at once with `mapping` or the guess from names) works as before. `auto_map=true` with `mapping` gives `422 auto_map_with_mapping`.
2. Show the proposal. Change what you want.
3. `POST /v1/leads/imports/{id}/mapping/confirm`. The import becomes `pending` and the worker imports it.

### Routes

| Method and path | What it does |
| --- | --- |
| `POST /v1/leads/imports?auto_map=true` | Upload. Returns the import with `proposal`. |
| `POST /v1/leads/imports/{id}/mapping/propose` | Rules + AI again ("map again"). Does not use a saved mapping, so it can cost one model call. `409 import_not_uploaded` if the import was confirmed or ended. |
| `GET /v1/leads/imports/{id}/mapping` | The last proposal. Samples are masked. `?raw_samples=true` shows the real values (while the file is stored). |
| `POST /v1/leads/imports/{id}/mapping/confirm` | Body below. Checks, makes new fields, saves the mapping, queues the import. |
| `GET /v1/leads/mapping-templates` | The saved mappings. |
| `DELETE /v1/leads/mapping-templates/{id}` | Delete one. |

`POST /v1/leads/imports/{id}/cancel` also works on an `uploaded` import.

### The proposal

```json
{ "import_id": "...", "columns": [
    { "index": 0, "header": "E-mail Addr", "samples": ["j***@acme.com"], "empty_count": 0,
      "target": { "kind": "builtin", "key": "email" }, "transform": null, "source": "rule", "confidence": 1.0, "reason": "header alias" },
    { "index": 3, "header": "Sector", "samples": ["SaaS"], "empty_count": 0,
      "target": { "kind": "new_field", "key": "industry", "label": null, "field_kind": "text" }, "source": "ai", "confidence": 0.82, "reason": "business sector" } ],
  "needs_check": [3], "warnings": [], "template_used": null,
  "ai": { "used": true, "provider_id": "...", "input_tokens": 1200, "output_tokens": 300 } }
```

- Target `kind`: `builtin` (`email`, `first_name`, `last_name`, `company_name`, `job_title`, `phone`, `website`, `linkedin_url`, `location`, `timezone`, `notes`, `tags`), `custom` (a field that exists), `new_field` (made on confirm), `skip`.
- `source`: `rule`, `ai` or `template`. `transform`: `split_full_name` (on `first_name` only): "Mary Ann Smith" gives first `Mary Ann`, last `Smith`; one word gives the first name only.
- `needs_check`: indexes with a confidence under `mapping_min_confidence`.
- `warnings`: `no_ai_provider`, `ai_mapping_failed` (provider down, bad key, timeout 30 s, bad JSON twice), `ai_budget_reached`, `columns_over_100` (the rest got rules only), `no_email_column`,
  `duplicate_target:<i>`, `ai_unknown_target:<i>`, `ai_invalid_new_field_key:<i>`, `ai_missing_column:<i>`. The proposal is always complete: a failed AI step leaves the rules result.

### Confirm body

```json
{ "columns": [{ "index": 3, "target": { "kind": "skip" } }, { "index": 2, "target": { "kind": "builtin", "key": "first_name" }, "transform": "split_full_name" }],
  "dedupe_mode": "fill_empty", "list_id": null, "tags": ["q3"], "campaign_id": null, "remember": true }
```

A column left out keeps the target of the proposal. `dedupe_mode`, `list_id`, `tags` and `campaign_id` left out keep the values of the upload. `remember` (default `true`) saves the mapping for the same headers.

Hard checks (`422 mapping_invalid`, with `error.problems`, nothing made): exactly one `email`; no two columns on one target (except `tags`; a split column takes `first_name` and `last_name`);
every `custom` key exists; every `new_field` key is valid and new (or exists with the same kind); `split_full_name` only on `first_name`; column indexes (and the optional `header`) match the file; a mapped column does not repeat the name of an earlier column.
Then, in one transaction: new fields, the mapping, the template, status `pending`. Two confirms at once start it once (`409 import_not_uploaded` for the second).
The answer is the import plus `warnings`: `samples_do_not_fit:<i>` (a number, date or url field whose samples do not fit; bad rows become row errors as before) and `mostly_empty:<i>` (over 50% empty cells).

### Cost and privacy

- Rules first. The model sees only the columns the rules could not map, at most 100 columns, one call per upload (plus one repair try), 30 s timeout. Each call writes an `llm_calls` row with purpose `import_mapping` and counts in the generator's daily budget.
- With `mapping_mask_pii` (default on), emails and phone numbers are masked before the call: `j***@acme.com`, `+1 *** *** 0101`. Values are cut at 100 characters. No sample value is written to a log.
- A saved mapping (same headers, same order, SHA-256 of the lower-cased headers) is used with no model call. It must still fit: every `custom` key must exist.
- The provider: setting `ai_mapping_provider_id`, else the generator's slot provider, else its draft provider.

### Settings (`/v1/leads/settings`)

| Key | Default | What it does |
| --- | --- | --- |
| `ai_mapping_enabled` | `true` | Off: rules only. |
| `ai_mapping_provider_id` | `null` | Empty: the generator's slot provider (then draft provider). |
| `mapping_sample_rows` | `5` | 1 to 10. |
| `mapping_mask_pii` | `true` | Mask emails and phones before the model call. |
| `mapping_min_confidence` | `0.7` | 0 to 1. Under this, the column goes in `needs_check`. |
| `mapping_templates_enabled` | `true` | Reuse and save mappings. |

The old import also accepts the target `full_name` in `mapping` (same split as `split_full_name`).


---

## 21. Web app: login, team, helpers, reports

The web app (`ui/`, served at `/` by this server) uses these routes. They are in `/docs` like all others.

### Login and sessions

- Passwords: 12 to 128 characters, NFKC-normalized, no "needs a symbol" rules. Refused: weak ones (zxcvbn score under 3) and ones that hold the email or name. Hashed with Argon2id (64 MiB, 3 passes, a random salt, and a secret "pepper" derived from the master encryption key, so a stolen database alone cannot be cracked). Codes: `password_too_short`, `password_too_long`, `password_too_weak`, `password_has_personal_info` (all `422`).
- Cookie `__Host-aikido_session`: `HttpOnly; Secure; SameSite=Strict; Path=/`, no `Domain`. Only the SHA-256 of the token is stored. Idle timeout 3 days, absolute timeout 30 days. A new token at every login. A password change or reset ends the other sessions. Plain HTTP works on `localhost`. On another host the login is refused (`400 insecure_connection`) unless `AIKIDO_INSECURE_COOKIES=true` (then the cookie is `aikido_session`, without `Secure`; a warning is logged at every start).
- **CSRF.** The login (and setup, accept and reset) answer holds `csrf_token`. Send it in `X-CSRF-Token` on every call that is not a `GET`. The request must also have an `Origin` (or `Referer`) equal to `AIKIDO_PUBLIC_URL` (without it: the `Host` header). Otherwise `403 csrf_failed`. `GET /v1/auth/state` gives the same token again after a page reload. API keys do not use cookies, and the server never sends `Access-Control-Allow-Credentials`, so a page on another site can not use the cookie.
- **Rate limits** (stored in the database, shared by all server copies; a refusal is `429 too_many_attempts` with `Retry-After`): 20 failed logins per IP in 15 minutes, then the IP waits 15 minutes. Per email: from the 5th failure a wait of 1 s, 2 s, 4 s ... (a try during the wait is refused without checking the password); from the 10th failure the email is locked 15 minutes. A good login clears the email counter. An owner or admin can unlock in Team. Behind your own proxy set `AIKIDO_TRUST_PROXY=true` so the IP comes from `X-Forwarded-For`; otherwise every user looks like the proxy.
- Every failed login gives the same answer (`401 invalid_credentials`, "Email or password is wrong."), and an unknown email costs the same time as a wrong password.

| Route | Auth | What it does |
| --- | --- | --- |
| `GET /v1/auth/state` | none | `{ setup_needed, logged_in, user, csrf_token, workspace_name, workspace_logo, insecure_connection }`. The app calls it first. |
| `POST /v1/auth/setup` | none | `{ setup_code, name, email, password, workspace_name }`. Makes the owner and logs in (`201`). `409 setup_done` after the first time, `403 setup_code_wrong`. |
| `POST /v1/auth/login` | none | `{ email, password }` gives `{ user, csrf_token }` and sets the cookie. |
| `POST /v1/auth/logout` | session | Deletes the session (`204`). |
| `GET /v1/auth/invites/{token}`, `POST /v1/auth/invites/{token}/accept` | none | The invite page: shows the email and role, then `{ name, password }` makes the user and logs in. `404 invite_invalid` when used or expired. |
| `GET /v1/auth/reset/{token}`, `POST /v1/auth/reset/{token}` | none | Set a new password with a one-time link (`{ new_password }`). Ends all sessions of the user, clears the lock, logs in. `404 reset_invalid`. |
| `GET /v1/users/me`, `PATCH /v1/users/me` | session | Name, email, `preferences`. Changing the email needs `current_password`. `preferences` are merged key by key (`null` removes a key), up to 16 KB. |
| `POST /v1/users/me/password` | session | `{ current_password, new_password }` (`204`). `403 wrong_password`. |
| `GET /v1/users/me/sessions`, `DELETE /v1/users/me/sessions/{id}` | session | Your sessions (`current` marks this one). |
| `GET /v1/users`, `PATCH /v1/users/{id}`, `DELETE /v1/users/{id}` | owner, admin | The team. `PATCH` takes `role` (`admin` or `member`), `status` (`active` or `disabled`; disabling ends the sessions) and `unlock`. `403 owner_protected` for any change to the owner, `403 cannot_change_self`. |
| `POST /v1/users/invites`, `GET /v1/users/invites`, `DELETE /v1/users/invites/{id}` | owner, admin | `{ email, role }` gives `{ invite, link }`. The link is valid 7 days and is shown once. The server sends no email: copy the link. |
| `POST /v1/users/{id}/transfer-ownership` | owner | `{ password }`. The target must be an active admin. You become an admin. |
| `POST /v1/users/{id}/reset-link` | owner, admin | A one-time reset link (valid 1 hour, shown once). Only the owner can make one for the owner. |

**Roles.** `owner` (exactly one) and `admin` can do everything. `member`: campaigns, leads, inbox, accounts, reports; not API keys, the team, the system defaults or the engine switches.

**First boot.** While no user exists the server prints a one-time **setup code** (12 characters) next to the API keys and saves it to `<AIKIDO_DATA_DIR>/setup_code.txt` (mode `600`). The setup page asks for it. It dies when the owner is made.

**Lost owner password.** On the server: `aikido_email reset-password <email>` prints a one-time link (valid 1 hour). Open it in the browser.

### Helpers for the web app (`/v1/ui/*`)

A key of any scope, or a session, can read. `PATCH` needs an owner, an admin, or an `admin` key.

| Route | What it does |
| --- | --- |
| `GET /v1/ui/config` | No secrets: `{ version, public_url_set, oauth: { gmail, microsoft }, redirect_uris, dry_run, features: { replies, followups, leads, generator } }`. `oauth.*` is true when the client id and secret are set. `redirect_uris` are the addresses to enter in the Google or Microsoft console. |
| `GET /v1/ui/onboarding`, `PATCH /v1/ui/onboarding` | `{ state: { step, done, skipped }, facts: { accounts, readable_accounts, providers, leads, sending_defaults_changed } }`. Steps: `connect-email`, `sending-basics`, `ai`, `import`, `done`. `PATCH` takes `step`, `done`, `skipped`. |
| `GET /v1/ui/workspace`, `PATCH /v1/ui/workspace` | `{ name, logo }`. The logo is a PNG, JPEG, WebP, GIF or SVG data URL up to 200 KB; `"logo": null` removes it. |
| `GET /v1/ui/alerts` | The alerts for the bell, newest problems from the current state (nothing is stored): `{ id, kind, severity, title, detail, link, since }`, most serious first. Kinds: `account_disconnected`, `account_error`, `account_reconnect_needed`, `campaign_warning`, `campaign_no_healthy_account`, `generation_awaiting_review`, `provider_auth_failed`, `ai_budget_reached`, `import_failed`, `jobs_stuck`, `engine_paused`. With a session, kinds the user turned off (`preferences.notifications.<kind> = false`) are left out. |

### Reports (`campaigns`)

`GET /v1/reports/daily?from=2026-10-01&to=2026-10-31&tz=Europe/Berlin&campaign_id=&account_id=` gives `{ from, to, tz, days: [...], totals }`. One row for every day, also the empty ones: `{ date, sent, replied, auto_replied, bounced, unsubscribed, complained, new_leads_enrolled }`. Days are cut in `tz` (default: the time zone of the sending defaults). At most 366 days (`422 range_too_long`; `invalid_range`, `invalid_timezone`). `sent` counts `send_jobs` that went out. The inbound counts are people (one person counts once a day and label); `bounced` is hard bounces.

`GET /v1/reports/accounts?from=&to=&tz=` gives `{ from, to, tz, bounce_limit_pct, accounts: [{ account_id, email, status, sent, contacted, reply_rate, bounce_rate }] }`. The rates are people divided by `contacted` (0 to 1). An account is paused when its recent bounce rate passes `bounce_limit_pct`.

### Other changes for the web app

- `GET /v1/campaigns/{id}/leads` rows now also have `interest`, `contact_state`, `current_step`, `steps_total`, `next_due_at` and `wait_reason` (`waiting_delay`, `queued`, `waiting_ai_review`, `paused`, `finished`, or the stop reason such as `reply`, `bounce_hard`, `unsubscribe`, `manual`).
- Inbound messages (`GET /v1/replies/messages`) already carry what a threaded answer needs: `message_id_header`, `references_ids`, `provider_thread_id`, `account_id`, `lead_id`. Send the answer with `POST /v1/sending/jobs` (`kind: "manual"`, `in_reply_to` = the `message_id_header`, `references` = the `references_ids` plus that id, joined by spaces, `provider_thread_id`, `priority: 100`, `lead_id`).
- `429` answers have `Retry-After` and `error.retry_after_secs`.
- The web app is served at `/` (embedded from `ui/dist`; `cargo build --no-default-features` makes an API-only server). Any path that is not `/v1/*`, `/docs`, `/openapi.json` or `/health` and is not a file returns `index.html`. An unknown `/v1/...` path is always a JSON `404 not_found`. App pages carry `Content-Security-Policy`, `X-Content-Type-Options`, `Referrer-Policy`, `Permissions-Policy`, and `Strict-Transport-Security` when `AIKIDO_PUBLIC_URL` is `https`.

## 22. Emails saved per lead (`campaigns`): UI update map v2, template mode

Each campaign has `content_mode` (`template` or `ai`), `auto_write` and `footer`. The footer is added under every email.
`lead_emails` holds one saved email per lead and step (step 1 = Primary email, step 2 = Follow up 1, ...). Status: `none`, `writing`, `review`, `ready`, `stale`, `failed`, `sent`.

| Route | What it does |
| --- | --- |
| `PATCH /v1/campaigns/{id}` | Also takes `content_mode`, `auto_write`, `footer`. A new footer marks unsent emails `stale`. |
| `PUT /v1/followups/sequences/{id}/steps` | Also takes `variants`: up to 4 (keys `B` to `E`) of step 1, each `{ name, subject, body_text, body_html, ai_slots, active }`. Leave out to keep them. A change marks unsent emails `stale`. |
| `GET /v1/campaigns/{id}/leads` | Each row has `emails: [{ step_number, status, variant_key, subject_preview, edited_by_hand, error_code }]` and `custom_fields`. |
| `GET`, `PATCH /v1/campaigns/{id}/leads/{lead_id}/emails/{step}` | Read, or edit by hand (`manual`, `edited_by_hand`, `ready`). `409 email_sent`. |
| `POST /v1/campaigns/{id}/emails/estimate` | Body `{ overwrite }`. Answer: `emails`, `edited_by_hand`, `skipped_ai_steps`, `tokens`, `cost_usd`. Writes nothing. |
| `POST /v1/campaigns/{id}/emails/render` | Template mode Save. `overwrite`: `skip`, `keep_edited` (default), `all`. Answer `202` with a run (already `done`). `422 wrong_mode` in AI mode. |
| `GET /v1/campaigns/{id}/email-runs`, `GET .../{run}`, `POST .../{run}/cancel` | Runs. A template run is done when the call returns. |

Rules:
- **Variants** are split evenly in the order the leads were added (A, B, C, A, ...). Leads added in one call share a time, so the lead id breaks the tie. Variant A is step 1 itself.
- **Sender tokens.** The account is chosen later by the feeder, so Save leaves `{{sender_name}}` and `{{sender_email}}` in the saved text. They are filled when the email is queued.
- **Steps with an AI part** are not saved by Save. The Email Generator writes them when the step is due (the old path).
- **Staleness (D7).** `input_hash` hashes the template, the footer and the rendered text. A lead edit, a template change or a footer change marks unsent `ready` template emails whose hash changed as `stale`. At send time a stale email that was not edited by hand is rendered again; one edited by hand, or written by AI, is sent as saved.
- **Send time.** The scheduler uses the saved email. Template mode with no saved email renders and saves it. AI mode holds the step with `wait_reason: email_not_ready` (or `email_review`).
- **After the first Save**, leads added to the campaign get their emails at once (template mode).
- The email becomes `sent` when the send job is made, and keeps `job_id`.

## 23. AI mode and helpers (`campaigns`, `accounts`, `replies`): UI update map v2, U3 and U4

**AI mode.** `PATCH /v1/campaigns/{id}` with `content_mode: "ai"`. Prompts: `ai_system_prompt` on `PATCH /v1/followups/sequences/{id}` (up to 8000 characters) and `ai_user_prompt` on each step of `PUT .../steps`. A prompt that uses a name no lead field, custom field or fallback covers is refused (`422 missing_variable`). Prompt-only names: `brief`, `previous_emails`, `lead_notes`, and one per step for the saved email of that step: `primary_email` (step 1), `follow_up1` (step 2), `follow_up2`, and so on. An email that is not written yet reads `(not written yet)`.

| Route | What it does |
| --- | --- |
| `POST /v1/campaigns/{id}/emails/generate` | Body `{ steps?, leads?: { count } or { lead_ids } or { filter: { step, status } }, overwrite? }`. `202` with a run. The generator worker writes the emails (`writing`, then `ready`, or `review` when review is on). `422 wrong_mode`, `generator_not_ready`, `missing_prompt`. The primary email of a lead is written before its follow ups in the same run. |
| `POST /v1/campaigns/{id}/emails/estimate` | Same body. In AI mode: `emails`, `tokens`, `cost_usd` (`cost_known` is false when the model has no price), `follow_ups_without_primary`. |
| `POST /v1/campaigns/{id}/emails/test` | `{ lead_id, step }`. The filled prompts and the result. Saves only an `llm_calls` row (purpose `lead_email_test`). |
| `POST /v1/campaigns/{id}/emails/actions` | `{ action: approve / reject / regenerate / clear, ids? or filter?: { step, status } }`. |
| `POST /v1/campaigns/{id}/email-runs/{run}/cancel` | Stops a run. Rows that wait go back to `ready` (they have text) or `none`. |

- **Auto-write (D5).** With `auto_write` on, the generator worker writes the next step of an open enrollment when it is due within `lookahead_hours`. A due step with no email waits (`wait_reason: email_not_ready`, or `email_review`). After one hour the bell shows the alert `emails_not_ready`.
- **Feeder.** In AI mode a lead is enrolled only when its Primary email is `ready` (or `stale`). The others stay `pending` and use no daily room.
- **Sender in prompts.** `{{sender_name}}` in a prompt is the first account of the pool. The footer keeps the tokens until send time.

**Helpers.**

| Route | What it does |
| --- | --- |
| `GET /v1/accounts/{id}/dns-check?selector=` | SPF, DKIM, DMARC and MX of the sending domain, each `ok`, `warn`, `fail` or `unknown`, with the record to add. Kept for 1 hour. Only common DKIM selectors and the one in `selector` are looked at. |
| `GET /v1/campaigns/{id}/checklist` | Six items: accounts, domains, leads, emails for day 1, missing values, test email. `ready` of `total`. |
| `POST /v1/campaigns/{id}/emails/send-test` | `{ to, lead_id?, step? }`. A manual job from the first healthy account; the subject starts with `[Test] `. Not counted in the campaign numbers. |
| `GET /v1/campaigns/{id}/leads?missing=true` | Only leads that lack a value the emails need. Each row has `missing: [names]`. |
| `POST /v1/campaigns/{id}/duplicate` | A draft copy: sequence, prompts, variants, settings, account pool. No leads, no emails. |
| `GET /v1/campaigns/{id}/stats` | Also `by_variant` (first email: sent, replied, reply rate) when more than one variant has sends. |
| `PUT .../steps` | Also `variant_a_active` (turn off variant A; one variant must stay on). |
| `GET /v1/replies/messages?handled=false` | Only emails whose lead has no interest set. |

## 24. Apps and the Reoon verifier (`integrations`)

Scope `integrations` (key `AIKIDO_INTEGRATIONS_API_KEY`). Install, update and uninstall need an owner or admin session. A member session can do the rest.

| Route | What it does |
| --- | --- |
| `GET /v1/integrations/apps` | The app catalog (`reoon`) with `installed`, `status` (`installed`, `auth_failed`) and `sidebar`. |
| `POST /v1/integrations/apps/reoon/install` | `{ api_key }`. Tests the key with the balance call first. `422 integration_key_invalid` saves nothing. `409 already_installed`. `502 integration_unreachable`. |
| `PATCH /v1/integrations/apps/reoon` | `{ api_key?, settings? }`. A new key is tested and clears `auth_failed`. |
| `DELETE /v1/integrations/apps/reoon` | Uninstall. Cancels open runs, deletes the key, keeps the results. |
| `GET /v1/verify/reoon/balance` | Live credits: `daily_credits`, `instant_credits`. `404 not_installed`, `409 integration_auth_failed`. |
| `GET /v1/verify/reoon/leads` | The Reoon page table. `GET /v1/leads` filters plus `verification_status` (comma list, `none` = not verified) and `checked_before`. Each row has `result` (or null). |
| `POST /v1/verify/reoon/runs` | `{ lead_ids }` or `{ filter }` (up to 10000), `mode`, `force`, `idempotency_key`. Returns `{ run, estimated_credits }`. |
| `GET /v1/verify/reoon/runs`, `GET .../runs/{id}` | History (last 50) and live counters. A stopped run has `status: failed` and `error: out_of_credits` or `auth_failed`. |
| `POST .../runs/{id}/cancel`, `POST .../runs/{id}/resume` | Cancel the open items. Resume a stopped run. |
| `GET`, `PATCH /v1/verify/reoon/settings` | `mode` (`power` default, `quick`), `concurrency` (1-20, default 5), `reuse_days` (default 30), `tick_secs`, `send_rule { allowed: [status], allow_not_verified }`. `422 status_not_allowed` for `invalid`, `disabled`, `disposable`, `spamtrap`. |
| `POST /v1/verify/reoon/pause`, `.../resume` | Stop or start the worker (owner or admin sessions). |

`settings.auto_verify` (default off): new leads from the CSV import, `POST /v1/leads`, `POST /v1/leads/bulk` and the other lead writes (not enrollments) are checked at once. They join one open run with `auto: true`. A lead with a recent result is skipped for free.

`GET /v1/leads` and `GET /v1/campaigns/{id}/leads` take `verification_status` (comma list, `none` = not verified). Their rows have `verification_status`, `verification_score` and `verification_checked_at`. `GET /v1/verify/reoon/leads/{id}` gives the full result of one lead.

The lead timeline gets the event `email_verified` (`status`, `mode`, `score`). The send rule is not used by the campaign feeder yet (phase 3).

### Reoon send rule in campaigns (phase 3)

- The campaign feeder checks the rule for each lead that starts a campaign, after the `unverified` check. Rule = `campaigns.verification_rule` or the workspace `send_rule`. A lead with no result passes only if `allow_not_verified`. Not allowed: the lead is `skipped` with `skip_reason: verification_not_allowed`. That skip does not use the daily limit. Without Reoon installed there is no rule. Leads already in a sequence are never stopped by it.
- `PATCH /v1/campaigns/{id}` takes `verification_rule`: `{ allowed: [status], allow_not_verified }`, or `null` for the workspace rule. A locked status gets `422 status_not_allowed`. The campaign body has `verification_rule`.
- `POST /v1/campaigns/{id}/leads/requeue-verification` (scope `campaigns`): moves leads skipped by the rule back to `pending` when they pass now. Answer `{ moved }`. Nothing moves by itself.
- `GET /v1/campaigns/{id}` `warnings` has "N pending leads will be skipped by the verification rule." and "N pending leads are not verified."
- `GET /v1/verify/reoon/status` (scope `integrations`): `installed`, `paused`, `worker_enabled`, `last_tick_at`, `open_runs`, `pending_items`, `integration_status`.
- New alert kinds `integration_auth_failed` and `verification_out_of_credits` (both link to the Reoon page). `engine_paused` also covers the verifier.

---

## 25. Email optimization (`personalization`, `campaigns`, `leads`)

The default mail prompt, a sender profile, a field registry, and code checks on every mail. It works only for campaigns in AI mode whose prompt kind is `optimized`. Custom prompts and the slot generator are not changed.

### Lead columns

Four new columns on a lead: `industry`, `personalization_note`, `trigger_event` (the `trigger` of the prompt) and `trigger_verified` (bool, default `false`). They are in `POST/PATCH /v1/leads`, the bulk route, CSV import (header synonyms: `trigger`, `event`, `news`, `icebreaker`, `sector`, ...), CSV export and `{{variables}}`. In a CSV, `trigger_verified` accepts `yes/no`, `true/false`, `1/0`, `y/n` in any case; an empty cell is `false`; any other value is a row error. An import can set `trigger_verified` to true but never clears it. Old custom fields with one of these four keys move into the column in migration `0014`.

`GET /v1/fields` (scope `leads`) is the field registry: `key`, `label`, `column`, `group` (`sender`, `lead`, `system`), `type`, `required`, `empty`, `optimized`. Lead fields with `optimized: true` are used by the default prompt. `company` is the column `company_name`, `title` is `job_title`, `trigger` is `trigger_event`. `first_name` and `company` are required: if empty, the mail is held (`missing_required: <keys>`) with no model call.

### Sources and the sender profile (`personalization`)

| Method | Path | Does |
| --- | --- | --- |
| POST | `/v1/personalization/sources` | Add input. Body `{kind: "url", url}`, `{kind: "text", text}`, `{kind: "detail", details: [{key, value}]}` or `{kind: "guide", guide: {who_you_help, who_you_do_not_help, result_you_give, real_proof}}`. Cleaned and saved as tagged text. |
| POST | `/v1/personalization/sources/cv` | Multipart, part `file` (PDF, DOCX, TXT, up to 5 MB). Only the text is kept. A file with no text gives `422 "No text found in this file."` |
| GET | `/v1/personalization/sources` | The cleaned chunks. This is what the AI reads. |
| DELETE | `/v1/personalization/sources/{id}` | Delete one chunk. |
| DELETE | `/v1/personalization` | Delete every source and every profile version. |
| POST | `/v1/personalization/profile/generate` | Step A + B. Returns `{draft, sources, tokens_in, tokens_out}`. Saves nothing. |
| PUT | `/v1/personalization/profile` | Save a new version `{fields, other_offers?, sources_used?}`. Old versions stay. |
| GET | `/v1/personalization/profile` | `{profile}`: the newest version or `null`. |
| GET | `/v1/personalization/profile/versions` | The last 10 versions. |

- A URL is fetched with `src/net/safe_fetch.rs`: `http` and `https`, ports 80 and 443, every resolved address checked (loopback, private, link local incl. `169.254.169.254`, CGNAT, multicast, unspecified, IPv6 ULA), the connection goes to the checked address (no second DNS lookup), at most 3 redirects each checked again, 10 s, 2 MB, `text/html` and `text/plain`. Up to 3 sites; up to 4 same-site pages (`about`, `services`, `work`, `case`, `portfolio`). A blocked address gives `422 invalid_source`.
- Text limits: free text 10,000 characters, 20 detail rows. The AI gets at most about 6,000 tokens. Cut order: portfolio pages, CV, free text, extra details, guide answers last.
- `generate` and the campaign preview cost tokens: 10 per minute per user (`429 rate_limited`). A purpose `sender_profile` row goes to `llm_calls`. No AI provider: `422 generator_not_ready`.
- The code removes dashes, drops unknown source tags, clears a field with no valid tag (listed in `missing`), keeps one word of `sender_first_name`. `allow_low_personalization` is never set by the AI.
- Saving a new version marks unsent mails of optimized campaigns `stale`.

### The Optimised mode (`campaigns`)

A campaign has three modes: `template`, `ai` and `optimized` (`PATCH /v1/campaigns/{id}` `content_mode`). `template` and `ai` are unchanged. In `optimized`, one model call writes the primary email and three follow-ups for a lead, and the server saves them in the four `lead_emails` rows (steps 1 to 4).

- Switching to `optimized` makes the sequence have 4 steps: missing steps are added (3, 4 and 5 business days after the step before; existing steps are kept). More than 4 steps gives `422 too_many_steps`.
- The prompts are plain text with `{{variables}}`. The instructions are `sequences.ai_system_prompt` (`PATCH /v1/followups/sequences/{id}`), the user prompt is `ai_user_prompt` of step 1 (`PUT .../steps`). Empty means the shipped default. `GET /v1/prompts/defaults` (scope `campaigns`) gives `system`, `user` and the `variables` list: the nine sender fields of the saved profile (`sender_first_name`, `ideal_customer`, `not_a_fit`, `problem_solved`, `offer`, `outcome`, `proof_or_none`, `low_friction_offer`, `allow_low_personalization`) and the lead fields (`first_name`, `last_name`, `company_name`, `job_title`, `industry`, `website`, `personalization_note`, `trigger_event`, `trigger_verified`, `location`, `linkedin_url`, `timezone`, plus the short names `company`, `title`, `trigger` and the custom fields). A variable with no value is left out, and a line whose variables are all empty is dropped; `{{name|fallback}}` gives a value. Email, phone, notes and tags are never available.
- The server adds only the answer format: `{primary: {subject, body}, followup_1: {body}, followup_2: {body}, followup_3: {body}}`. The only check is that format (one more try if it is wrong; then the email is `failed`). Dashes are replaced by commas. Follow-ups have no subject (same thread). No footer is added.
- `POST /v1/campaigns/{id}/emails/generate` queues one row per lead (the first wanted step) and the worker fills all four. `estimate` counts leads. A sent or hand edited email is never replaced; a new sender profile version marks unsent emails `stale`.
- `POST /v1/campaigns/{id}/emails/test` in this mode answers with `emails`: all four `{step, subject, body}`. Nothing is saved.
- 10 calls a minute per user for `generate` of a profile (`429 rate_limited`).

## 26. Open tracking (`tracking`)

Count email opens without a hidden pixel. The tracker is a normal visible image served from the **sender's own domain**. Scope `tracking` (11 keys on first boot; an old database gets only the new key). It is off by default (`Campaign settings > Email tracking`).

### How it works

1. **Tracking domain.** One per sending domain (`nazmul@acme.com` gives `acme.com`), made by the server from the connected accounts. The default host is `media.acme.com`. The user adds a CNAME: type `CNAME`, name `media`, value = this server's host (`AIKIDO_TRACKING_CNAME_TARGET`, else the host of `AIKIDO_PUBLIC_URL`).
2. **Status.** `not_connected` (no CNAME), `checking` (CNAME found, certificate in progress), `connected`, `error` (CNAME removed, or the certificate failed). The DNS is checked every 5 minutes for 72 hours, then it stops (`not_found: true`) until `check`. A connected domain is checked once a day; a certificate is renewed 30 days before it ends. Certificates and keys are encrypted (AES-256-GCM, AAD = the row id).
3. **Certificates.** With `AIKIDO_TRACKING_HTTPS_ADDR` (for example `0.0.0.0:443`) the server runs a public HTTPS listener with an SNI certificate resolver and gets free certificates from Let's Encrypt (HTTP-01). Let's Encrypt must reach `AIKIDO_TRACKING_HTTP_ADDR` (port 80). Without `AIKIDO_TRACKING_HTTPS_ADDR`, your own reverse proxy serves HTTPS for the tracking hosts (it must send them to the server port; the image routes also answer on the main address) and a domain is `connected` as soon as its CNAME is found.
4. **Unlock.** A tracker can be picked only when **every** sending domain of the campaign is `connected`. If a domain turns red later, its emails go out without a tracker and the campaign shows a `warning`.
5. **Two trackers, one per campaign.** All emails of the campaign get it (first emails, follow-ups, manual emails) if they have an HTML part. Plain-text emails get none.
   - `text_image` (Type 1): the **last** match of `text_value` in the visible text of the final HTML is replaced by `<img src="https://media.acme.com/i/{token}.png" alt="{text}" width height style="vertical-align:-Npx;border:0">`. It matches across tags (`Nazmul <b>Hasan</b>`). `style_mode: auto` copies font, size, color, bold, italic, letter spacing and line height from the HTML (inline styles, `<b>`, `<i>`, `<font>`, parent tags, `<style>` blocks; the nearest tag wins; default Arial 14px `#222222`); `manual` uses `manual_style`. The image is drawn at 2x, transparent PNG, one cached image per (text, style). Three fonts are bundled (Liberation Sans for Arial and Helvetica, DejaVu Serif for Georgia and serif fonts, DejaVu Sans for Verdana); an unknown font gets the closest. Text not found: no tracker.
   - `signature_image` (Type 2): the `src` of the first image that is not inside a link (`image_mode: auto`) or of the image with `image_source_url` (`manual`) is changed to `https://media.acme.com/p/{token}`. The server downloads the image once (PNG, JPEG, GIF, WebP, at most 5 MB, SSRF-safe fetch) and serves the stored copy. **No redirect.** `refresh-image` downloads it again.
6. **Order in the send pipeline.** Template, live variables, AI slots, footer, signature and unsubscribe link, then the tracker (on the final HTML), then the MIME. The token is saved in `send_jobs.tracking_token`. It never stops a send.

### Public routes (no key)

| Method | Path | Use |
| --- | --- | --- |
| GET | `/i/{token}.png` | Type 1 image |
| GET | `/p/{token}` | Type 2 image |
| GET | `/.well-known/acme-challenge/{token}` | Let's Encrypt check |

An unknown token still gets an image (a transparent 1 pixel PNG), never an error. Headers: `Cache-Control: no-cache, no-store, must-revalidate`, `Content-Type`, `Content-Length`. The open is saved in the background (`HEAD` saves nothing). The token is 22 random letters and digits.

**Proxy and scanner opens** (`is_proxy`, `proxy_kind`): `google` (user agent has `GoogleImageProxy`), `apple` (address in `17.0.0.0/8` or `2620:149::/32`, plus `AIKIDO_TRACKING_PROXY_CIDRS`), `scanner` (opened 5 seconds or less after the send, or before the send was saved). A lead's first real open is **Opened**; a lead with only proxy opens is **Maybe opened**; more real opens are **Repeat opens**. Apple Private Relay traffic that leaves through Cloudflare, Akamai or Fastly is not in the built-in list.

### API

| Method | Path | Use |
| --- | --- | --- |
| GET | `/v1/tracking/domains` | `{domains}`: each with `host`, `status`, `record {type, name, value}`, `last_checked_at`, `error_message`, `not_found` |
| PUT | `/v1/tracking/domains/{id}` | `{subdomain}`. Not `track`, `tracking`, `click`, `open`, `pixel` (nor names with `track`, `click`, `pixel`), nor `mail`, `www`, `smtp`... `422 invalid_subdomain`. The domain starts over. |
| POST | `/v1/tracking/domains/{id}/check` | Check the DNS now; starts the 72 hour window again. A certificate is asked for in the background. |
| GET | `/v1/campaigns/{id}/tracking` | Settings plus the campaign's `domains` and `trackers_unlocked` |
| PUT | `/v1/campaigns/{id}/tracking` | Replace the settings: `enabled`, `tracker_type`, `text_value`, `style_mode`, `manual_style {font,size,color,bold,italic}`, `image_mode`, `image_source_url`. `422`: `tracking_domains_not_ready`, `no_sending_domains`, `text_required`, `invalid_manual_style`, `invalid_image_url`, `image_download_failed` |
| POST | `/v1/campaigns/{id}/tracking/preview` | Optional body (same fields). Type 1: the image as a `data:` URL, the style used and where it came from. Type 2: the image found in the first email. |
| POST | `/v1/campaigns/{id}/tracking/refresh-image` | Download the Type 2 image again (the copy keeps its id) |
| GET | `/v1/campaigns/{id}/opens` | `tracked_leads`, `opened`, `maybe_opened`, `open_rate`, `repeat_opens`, `total_opens`, `proxy_opens`, `recent` (`?limit` 1 to 100) |
| GET | `/v1/leads/{id}/opens` | `{summary, opens}` (last 200) |

`GET /v1/leads/{id}/context` has `opens` (`status`, `opens`, `proxy_opens`, `first_opened_at`, `last_opened_at`). Lead rows (`GET /v1/leads`, `GET /v1/campaigns/{id}/leads`, lead detail) have `open_status` and `open_count`. Deleting a lead (erase plus suppress) deletes its tokens and opens.

### Settings

`AIKIDO_TRACKING_CNAME_TARGET`, `AIKIDO_TRACKING_HTTPS_ADDR`, `AIKIDO_TRACKING_HTTP_ADDR`, `AIKIDO_ACME_DIRECTORY` (Let's Encrypt production by default; use the staging address to try), `AIKIDO_ACME_EMAIL`, `AIKIDO_TRACKING_ENABLED` (`false`: this copy does not check domains), `AIKIDO_TRACKING_PROXY_CIDRS`. See `.env.example`.
