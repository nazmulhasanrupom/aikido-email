-- Open tracking: tracking domains (one per sending domain), per-campaign settings, images, tokens and opens.

CREATE TYPE tracking_domain_status AS ENUM ('not_connected', 'checking', 'connected', 'error');

CREATE TABLE tracking_domains (
    id                  uuid PRIMARY KEY,
    -- `acme.com` from `nazmul@acme.com`
    sending_domain      text NOT NULL UNIQUE,
    -- `media` gives `media.acme.com`
    subdomain           text NOT NULL DEFAULT 'media',
    -- the host the CNAME must point to (kept from the server config when the row was made)
    cname_target        text NOT NULL,
    status              tracking_domain_status NOT NULL DEFAULT 'not_connected',
    -- encrypted (AES-256-GCM, AAD = the row id): version(4) + nonce(12) + ciphertext
    cert                bytea,
    cert_key            bytea,
    cert_expires_at     timestamptz,
    last_checked_at     timestamptz,
    error_message       text,
    -- the 5 minute DNS check stops here and the row shows "not found"; "Check now" moves it 72 hours on
    check_until         timestamptz NOT NULL,
    -- certificate retries
    cert_attempts       integer NOT NULL DEFAULT 0,
    next_cert_attempt_at timestamptz,
    -- one server copy works on a domain at a time
    lease_until         timestamptz,
    created_at          timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE tracking_settings (
    campaign_id      uuid PRIMARY KEY REFERENCES campaigns (id) ON DELETE CASCADE,
    enabled          boolean NOT NULL DEFAULT false,
    -- null until the user picks a tracker (they unlock when all sending domains are connected)
    tracker_type     text CHECK (tracker_type IN ('text_image', 'signature_image')),
    text_value       text,
    style_mode       text NOT NULL DEFAULT 'auto' CHECK (style_mode IN ('auto', 'manual')),
    manual_style     jsonb,
    image_mode       text NOT NULL DEFAULT 'auto' CHECK (image_mode IN ('auto', 'manual')),
    image_source_url text,
    -- why emails went out without a tracker, if they did
    warning          text,
    warning_at       timestamptz,
    updated_at       timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE tracking_assets (
    id           uuid PRIMARY KEY,
    kind         text NOT NULL CHECK (kind IN ('text_image', 'signature_image')),
    -- sha-256 of the text and style (Type 1), or of the source URL (Type 2)
    cache_key    text NOT NULL,
    file         bytea NOT NULL,
    content_type text NOT NULL,
    width        integer,
    height       integer,
    -- Type 1: how far the drawn baseline is above the bottom edge (shown pixels)
    baseline_drop integer,
    source_url   text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (kind, cache_key)
);

CREATE TABLE tracking_tokens (
    token       text PRIMARY KEY,
    send_job_id uuid NOT NULL,
    campaign_id uuid,
    lead_id     uuid,
    asset_id    uuid NOT NULL REFERENCES tracking_assets (id),
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX tracking_tokens_job_idx ON tracking_tokens (send_job_id);
CREATE INDEX tracking_tokens_campaign_idx ON tracking_tokens (campaign_id);
CREATE INDEX tracking_tokens_lead_idx ON tracking_tokens (lead_id);

CREATE TABLE email_opens (
    id          bigserial PRIMARY KEY,
    token       text NOT NULL,
    send_job_id uuid NOT NULL,
    lead_id     uuid,
    campaign_id uuid,
    ip          text,
    user_agent  text,
    is_proxy    boolean NOT NULL DEFAULT false,
    -- `apple`, `google` or `scanner` when is_proxy
    proxy_kind  text,
    opened_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX email_opens_campaign_idx ON email_opens (campaign_id, opened_at);
CREATE INDEX email_opens_lead_idx ON email_opens (lead_id, opened_at);
CREATE INDEX email_opens_job_idx ON email_opens (send_job_id);

-- the token of the tracker in the email (change 2 of the build map)
ALTER TABLE send_jobs ADD COLUMN tracking_token text;

-- Let's Encrypt: the account key (encrypted like the certificates) and the answers to the HTTP-01 challenge.
-- The challenge is in the database because the check may reach another server copy.
CREATE TABLE tracking_acme_account (
    directory   text PRIMARY KEY,
    credentials bytea NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE tracking_acme_challenges (
    token             text PRIMARY KEY,
    key_authorization text NOT NULL,
    created_at        timestamptz NOT NULL DEFAULT now()
);
