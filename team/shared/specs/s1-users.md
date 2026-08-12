# Spec — S1 Users / Roles (Owner-only CRUD)

**Slice:** S1 Developer — `POST /api/users`, `GET /api/users`, `PATCH /api/users/:id` Owner-only, argon2, `012_users.sql`
**Source:** `docs/DEVELOPER_PLATFORM_PLAN.md §3.2` + `URY-PORT-AND-GAP-PLAN.md` P0 X4/F1
**Parent task:** S1 Users (decomposed into 5 units, each 15 min, single dominant risk)

## Acceptance (done when)

- `012_users.sql` embedded in `MIGRATOR` (v12 `users`) — `cargo test -p peacock-storage --lib migrator_contains_the_users_migration` passes.
- `ApiError::Forbidden` → 403 with slug `forbidden` — waiter `POST /api/users` → 403 not 401.
- `GET /api/tables` without `peacock_session` → 401 `application/problem+json` (not 200, not 404); `/health` and `/api/auth/login|logout` remain public; `OPTIONS` bypasses.
- `POST /api/users` as owner → 201 with `{id,email,role}` and `password_hash` never serialized, hash starts `$argon2`, `created_by` set to caller; duplicate email → 409.
- `peacock-web/src/app/settings/users/page.tsx` Owner-only, redirects to `/login` on 401, lists/add/deactivate, `npm run build` 10 routes.

## Non-goals

- Aggregator, inventory, full 127 URY gaps — out of slice.

## Verify commands (each unit independently runnable)

See `team/MODEL_ROUTING.md` table + `team/tasks/*.md` per unit.

## Threats (W4_SECURITY)

- `test-secret-key` HMAC must not exist — `config.rs` rejects it.
- `X-Restaurant` spoof — `CallerContext.restaurant` preferred when session present (next unit).
- `*` CORS with credentials — rejected at `Config::from_source`.
