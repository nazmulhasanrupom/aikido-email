# AIKIDO EMAIL

Open source cold email automation in Rust. This repo has three parts that run in one server:

- **Sending engine**: stores connected inboxes and sends queued emails one by one, with human-like timing.
- **Reply Check**: reads the same inboxes, finds replies, auto-replies, bounces, unsubscribes and spam complaints
  that come back from our sends, and acts on them (blocks addresses, cancels queued emails, pauses a bad inbox).
- **Follow-ups**: send a short series of emails (a sequence) to each recipient, in the same thread, and stop at once when they
  reply, bounce, unsubscribe or complain. An out-of-office reply pauses the sequence for 3 business days.

API reference: `docs/API.md`. Live docs: `/docs` on the running server. Build progress: `progress.md`.

## Quick start

```bash
docker compose up -d                      # Postgres 16, Mailpit, GreenMail (for tests)
cp .env.example .env                      # then fill in the provider keys you need
DATABASE_URL=postgres://aikido:aikido@127.0.0.1:55432/aikido cargo run -- serve
```

The first boot also prints a one-time **setup code**. Open `http://localhost:8080`, type the code, and make the owner account (the web app).
In development run `pnpm dev` in `ui/` and open `http://localhost:5173` (set `AIKIDO_PUBLIC_URL=http://localhost:5173` so the login check passes).
Lost the owner password? `cargo run -- reset-password <email>` prints a one-time link.
The web app is built into the binary from `ui/dist` (`pnpm build` in `ui/`). `cargo build --no-default-features` makes an API-only server.
Behind a reverse proxy set `AIKIDO_PUBLIC_URL` (the browser address) and `AIKIDO_TRUST_PROXY=true`. Without HTTPS the web login does not work (except on localhost).

The first boot prints four API keys one time (`admin`, `accounts`, `sending`, `replies`) and saves them in
`./data/initial_keys.txt`. An old database gets only the new `replies` key. Back up `./data/encryption.key`.

## Reply Check: read access

The Reply Check only **reads**. It never marks mail as read, and never moves, deletes, labels or flags mail.
The user keeps using the inbox.

| Provider | Scope | Notes |
| --- | --- | --- |
| Gmail | `https://www.googleapis.com/auth/gmail.readonly` | Read only. It cannot change mail. |
| Microsoft | `Mail.Read` (delegated) | Read only. `offline_access` is asked too. |
| IMAP/SMTP | none | Uses the IMAP login saved with the account. `EXAMINE` and `BODY.PEEK` only. |

Accounts connected before the Reply Check have no saved scopes. They keep sending and show `can_read: false`
and `read_status: "reconnect_needed"`. Run the connect flow again: the same row is updated.

### Gmail setup: read this before you go public

- `gmail.readonly` is a **restricted** Google scope. A public app that outside users connect needs Google
  verification and a yearly security assessment.
- **Self-hosted inside one Google Workspace:** set the OAuth app to **Internal**. No verification is needed.
- An **External** app in **Testing** mode: Google expires its refresh tokens after **7 days**. The account then goes
  `disconnected` and must be connected again.
- IMAP for Gmail with OAuth needs the `https://mail.google.com/` scope. That scope is restricted too.
  It is not a way around the verification.
- The Gmail API may replace the `Message-ID` of a sent email. The engine reads the sent message back after each send and
  saves the real id, so replies still match. This needs the read scope.

### Microsoft setup

Register the app in Microsoft Entra with the delegated permissions `Mail.Read`, `Mail.Send`, `offline_access`, `User.Read`.
The Reply Check only sends `GET` requests to Graph.

## Reply Check settings

Everything is changeable through the API (`/v1/replies/defaults`, `/v1/replies/accounts/{id}/settings`).
There are no hard-coded keyword lists or limits. Test a keyword list without saving anything:
`POST /v1/replies/classify` with `{ "raw": "<base64 of an .eml>" }`.

Server settings (env vars):

| Name | Default | Use |
| --- | --- | --- |
| `AIKIDO_REPLIES_API_KEY` | auto-made | Your own key for a deploy. |
| `AIKIDO_REPLY_TICK_SECS` | `15` | Poller loop speed. |
| `AIKIDO_MAX_PARALLEL_SYNCS` | `10` | Accounts synced at once per server. |
| `AIKIDO_MAX_MESSAGE_BYTES` | `10485760` | Max raw email size to download. |
| `AIKIDO_REPLY_CHECK_ENABLED` | `true` | `false` turns the poller off. |

## Follow-up safety gate

A follow-up (an automated email with `in_reply_to`, or step 2 or later of a sequence) goes out only if the inbox was read within
`reply_check_max_age_mins` (sending setting, default 15, `0` = off). Otherwise it waits 5 minutes and tries again.
`/v1/sending/accounts/{id}/status` shows `reply_check_stale`, or `reply_check_unavailable` when the account cannot be read.

## Follow-ups

Setup is one call per sequence and one call per batch of recipients. See `docs/API.md` section 17 for the full story.

```bash
# 1. A sequence: first email, then two follow-ups (3 and 4 business days later)
curl -s -X POST $BASE/v1/followups/sequences -H "Authorization: Bearer $FOLLOWUPS_KEY" -H 'content-type: application/json' -d '{
  "name": "Cold outreach",
  "steps": [
    { "subject": "Quick question, {{first_name|there}}", "body_text": "Hi {{first_name|there}}, ..." },
    { "delay_amount": 3, "body_text": "Just checking in." },
    { "delay_amount": 4, "body_text": "Last note from me." }
  ]
}'
# 2. Enroll recipients (up to 1000 per call). One account sends every step to a recipient.
curl -s -X POST $BASE/v1/followups/sequences/$SEQ/enrollments -H "Authorization: Bearer $FOLLOWUPS_KEY" -H 'content-type: application/json' -d '{
  "enrollments": [{ "to_email": "ana@acme.com", "account_id": "'$ACCOUNT'", "variables": { "first_name": "Ana" }, "idempotency_key": "lead-123" }]
}'
```

Follow-ups need the Reply Check to work well: it is what tells the follow-up step to stop. Use a connected account that can read its inbox
(`can_read: true`), or the follow-up waits for the inbox gate (see above).

| Name | Default | Use |
| --- | --- | --- |
| `AIKIDO_FOLLOWUPS_API_KEY` | auto-made | Your own key for a deploy. |
| `AIKIDO_FOLLOWUPS_ENABLED` | `true` | `false` makes this server copy skip the follow-up scheduler. |

## Leads

Contacts, lists, tags, CSV import and export, email checks. See `docs/API.md` section 18. Needs the Postgres extension `pg_trgm`.

| Name | Default | Use |
| --- | --- | --- |
| `AIKIDO_LEADS_API_KEY`, `AIKIDO_CAMPAIGNS_API_KEY` | auto-made | Your own keys for a deploy. |
| `AIKIDO_LEADS_ENABLED` | `true` | `false` makes this server copy skip the CSV import worker. |

## Email Generator

An AI writes the personal part (`{{ai:opener}}` slots) of each sequence step, for each lead, a few hours before the step is due. See `docs/API.md` section 20.

**Privacy: lead data goes to the LLM provider you choose.** The prompt holds the lead's name, job title, company, website, location, custom fields, a few notes, a short timeline and the text of the last two emails sent to the lead.
The email address, the phone number and the text of replies are not sent. For no third party, run a local model (for example Ollama) and add it as an `openai_compatible` provider.

```bash
# 1. add a provider and test it (the key is encrypted and never shown again)
curl -X POST $BASE/v1/generator/providers -H "Authorization: Bearer $GENERATOR_KEY" -H 'content-type: application/json' \
  -d '{"kind":"anthropic","name":"Claude","model":"claude-haiku-5-5","api_key":"sk-ant-..."}'
curl -X POST $BASE/v1/generator/providers/$PROVIDER_ID/test -H "Authorization: Bearer $GENERATOR_KEY"
# 2. use it for slots
curl -X PATCH $BASE/v1/generator/settings -H "Authorization: Bearer $GENERATOR_KEY" -H 'content-type: application/json' \
  -d "{\"slot_provider_id\":\"$PROVIDER_ID\",\"review_required\":true}"
# 3. a step with a slot (send this in the steps of a sequence)
#    {"subject":"Hello {{first_name|there}}","body_text":"Hi {{first_name|there}},\n{{ai:opener}}\nBest, {{sender_name}}",
#     "ai_slots":[{"key":"opener","instruction":"One friendly line about their company.","max_words":30,"fallback":"I came across your company."}]}
# 4. look at one before you launch: POST /v1/generator/preview, then read 20 outputs in review mode (GET /v1/generator/generations)
```

| Name | Default | Use |
| --- | --- | --- |
| `AIKIDO_GENERATOR_API_KEY` | auto-made | Your own key for a deploy. |
| `AIKIDO_GENERATOR_ENABLED` | `true` | `false` makes this server copy skip the generation worker. |

## Tests

```bash
docker compose up -d
DATABASE_URL=postgres://aikido:aikido@127.0.0.1:55432/aikido cargo test
```

GreenMail tests (`tests/replies_imap.rs`) skip themselves if GreenMail is not running.
