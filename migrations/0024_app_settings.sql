-- Settings that a person types in the app (Settings → Environment), for example the Google and Microsoft keys.
-- Plain values go in `value`. Secret values are sealed (AES-256-GCM) in `sealed`, with the key name as AAD.
CREATE TABLE app_settings (
  key         text PRIMARY KEY,
  value       text,
  sealed      bytea,
  nonce       bytea,
  key_version int,
  updated_by  uuid REFERENCES users(id) ON DELETE SET NULL,
  updated_at  timestamptz NOT NULL DEFAULT now()
);
