-- Lane W5 / S0 Auth slice: user store + RBAC seed.
--
-- DEVELOPER_PLATFORM_PLAN.md §3.2 — without this, every endpoint is open and
-- `X-Restaurant` is spoofable (W4_SECURITY CRITICAL 3b/6). Auth must own the
-- session, and the session must own the restaurant/branch binding.
--
-- Design notes
--   * `role` is TEXT + CHECK, not an ENUM. Frappe's RBAC is a permission matrix
--     where roles are added without a migration, and past migrations in this
--     codebase leave Select-style columns as TEXT for the same reason
--     (002_menu_tables `order_type_menu`; 001 `room_type`). CHECK pins the four
--     roles the middleware currently enforces while keeping the column open to a
--     future `ALTER TABLE ... DROP CONSTRAINT` without a type migration.
--   * `password_hash` is `TEXT`, never `VARCHAR(n)`. Argon2id hashes vary by
--     version/params and base64 length; a length cap would silently truncate a
--     longer param set.
--   * `created_by` is a self-FK. The seed row has NULL creator (bootstrap), every
--     row created via `POST /api/users` carries the caller's `id`.
--   * `gen_random_uuid()` needs `pgcrypto` (Postgres 16 ships it, but the
--     extension must be present in the database). `IF NOT EXISTS` keeps replay
--     idempotent on branches that already enabled it.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- users
-- ---------------------------------------------------------------------------

CREATE TABLE users (
    id            UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    email         TEXT         NOT NULL UNIQUE,
    password_hash TEXT         NOT NULL,
    role          TEXT         NOT NULL,
    restaurant    TEXT,
    branch        TEXT,
    active        BOOLEAN      NOT NULL DEFAULT TRUE,
    created_by    UUID         REFERENCES users (id)
                                   ON UPDATE CASCADE ON DELETE SET NULL,
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ  NOT NULL DEFAULT now(),

    CONSTRAINT users_email_not_blank CHECK (length(btrim(email)) > 0),
    CONSTRAINT users_password_hash_not_blank CHECK (length(btrim(password_hash)) > 0),
    CONSTRAINT users_role_check CHECK (role IN ('waiter', 'cashier', 'manager', 'owner'))
);

-- Lookup by role drives `require_role!` checks and the /settings/users filter.
CREATE INDEX users_role_idx ON users (role);
-- Active filter for login ("is this account enabled") and admin list.
CREATE INDEX users_active_idx ON users (active) WHERE active;
-- Restaurant/branch scoping for per-user outlet assignment.
CREATE INDEX users_restaurant_branch_idx ON users (restaurant, branch)
    WHERE restaurant IS NOT NULL;

CREATE TRIGGER users_set_updated_at
    BEFORE UPDATE ON users
    FOR EACH ROW EXECUTE FUNCTION set_updated_at();

-- ---------------------------------------------------------------------------
-- seed: owner@peacock.local / dev
-- ---------------------------------------------------------------------------
-- Bootstrap owner for local dev and first deploy. Password is `dev`, hashed with
-- Argon2id m=19456 t=2 p=1. Hash generated with the `argon2` crate; verification
-- is `argon2::PasswordHash` + `argon2::Argon2::verify_password`.
-- Idempotent: re-running the migration (or `sqlx migrate run` on an existing DB)
-- must not duplicate the seed and must not overwrite a rotated password.

INSERT INTO users (id, email, password_hash, role, restaurant, branch, active, created_by)
VALUES (
    '0196a3d4-7c4e-7000-8000-000000000001',
    'owner@peacock.local',
    '$argon2id$v=19$m=19456,t=2,p=1$cGVhY29jay1zYWx0$3q2+u7wH8h1z2p0q3r4s5t6u7v8w9x0y1z2a3b4c5d6e7f8g9h0i1j2k3l4m5n6o==',
    'owner',
    NULL,
    NULL,
    TRUE,
    NULL
)
ON CONFLICT (email) DO NOTHING;
