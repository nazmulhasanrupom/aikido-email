-- Web UI: users, passwords, sessions, invites, first-boot state.

CREATE TABLE users (
    id                  uuid PRIMARY KEY,
    email               citext NOT NULL UNIQUE,
    name                text NOT NULL,
    -- Argon2id, PHC string
    password_hash       text NOT NULL,
    role                text NOT NULL CHECK (role IN ('owner', 'admin', 'member')),
    status              text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),
    -- theme, notification toggles, last_seen_inbox_at
    preferences         jsonb NOT NULL DEFAULT '{}',
    failed_logins       integer NOT NULL DEFAULT 0,
    locked_until        timestamptz,
    last_login_at       timestamptz,
    password_changed_at timestamptz NOT NULL DEFAULT now(),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
-- exactly one owner
CREATE UNIQUE INDEX users_one_owner_uq ON users (role) WHERE role = 'owner';

CREATE TABLE user_sessions (
    id              uuid PRIMARY KEY,
    user_id         uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- SHA-256 of the cookie token
    token_hash      bytea NOT NULL UNIQUE,
    -- SHA-256 of the CSRF token
    csrf_token_hash bytea NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    last_seen_at    timestamptz NOT NULL DEFAULT now(),
    expires_at      timestamptz NOT NULL,
    idle_expires_at timestamptz NOT NULL,
    ip              text NOT NULL DEFAULT '',
    user_agent      text NOT NULL DEFAULT ''
);
CREATE INDEX user_sessions_user_idx ON user_sessions (user_id);

CREATE TABLE user_invites (
    id         uuid PRIMARY KEY,
    email      citext NOT NULL,
    role       text NOT NULL CHECK (role IN ('admin', 'member')),
    token_hash bytea NOT NULL UNIQUE,
    invited_by uuid REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    used_at    timestamptz
);

-- One-time password reset links (the `reset-password` command, and the Team page).
CREATE TABLE user_resets (
    id         uuid PRIMARY KEY,
    user_id    uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    token_hash bytea NOT NULL UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    used_at    timestamptz
);

-- One row only.
CREATE TABLE app_state (
    id              integer PRIMARY KEY CHECK (id = 1),
    workspace_name  text NOT NULL DEFAULT 'Workspace',
    -- a small data URL, max 200 KB
    workspace_logo  text,
    onboarding      jsonb NOT NULL DEFAULT '{"step": "connect-email", "done": false, "skipped": []}',
    -- SHA-256 of the one-time setup code. Cleared when the owner is made.
    setup_code_hash bytea,
    setup_done_at   timestamptz
);
INSERT INTO app_state (id) VALUES (1);

-- Rate limits that work with many server copies. `key` is `ip:<ip>` or `email:<email>`.
CREATE TABLE login_attempts (
    key          text PRIMARY KEY,
    count        integer NOT NULL DEFAULT 0,
    window_start timestamptz NOT NULL,
    locked_until timestamptz
);
