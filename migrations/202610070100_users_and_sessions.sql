-- Accounts and login sessions.

CREATE TABLE users (
    id            UUID PRIMARY KEY,
    -- Stored trimmed and lowercased by the app, so a plain unique index is enough.
    email         TEXT NOT NULL UNIQUE,
    -- NULL for accounts that only sign in through an OIDC provider.
    password_hash TEXT,
    role          TEXT NOT NULL CHECK (role IN ('admin', 'staff', 'customer')),
    -- Consecutive failed logins; reset on success. Drives the per-account lockout.
    failed_logins INTEGER NOT NULL DEFAULT 0,
    locked_until  TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Only the SHA-256 of each session token is stored, so a leaked database
-- doesn't hand out working logins.
CREATE TABLE sessions (
    token_hash BYTEA PRIMARY KEY,
    user_id    UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX sessions_user_id_idx ON sessions (user_id);
CREATE INDEX sessions_expires_at_idx ON sessions (expires_at);
