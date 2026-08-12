# Model Routing — Peacock POS

Match cost to cognitive demand. Per-unit dominant risk drives tier.

| Tier | Model | Use when | Cost | Peacock examples |
|------|-------|----------|------|------------------|
| **haiku** | haiku / gpt-4o-mini | Mechanical, spec-following, template, no branching logic | $ | copy migration file, add `Forbidden` variant, wire `X-CSRF` header, run eval harness, health check echo |
| **sonnet** | sonnet 4.5 / spark | Standard feature work, follows spec, moderate branching | $$ | auth middleware 401 gate, JWT verify, CORS allowlist, dashboard summary aggregation, settings/users page, Recharts wiring |
| **opus** | opus 4.6/5 / muse-spark xhigh | Judgment, gapless SERIALIZABLE, money half-away, BOM MAX_LEVEL=2, threat model | $$$ | gapless `UPDATE RETURNING` invoicing, invoice `half-away-from-zero` + `NUMERIC(18,6)` as String, BFS merge cluster, KOT station routing, audit log, W4_SECURITY review |

## S1 Users/Roles — decomposed units (see `team/shared/specs/s1-users.md`)

| Unit | Title | Dominant risk | Verify command (independently runnable) | Tier | Done condition |
|------|-------|---------------|------------------------------------------|------|----------------|
| S1-U1 | `012_users.sql` migration embedded | Missing migration → `relation "users" does not exist` on every auth test | `cargo test -p peacock-storage --lib migrator_contains_the_users_migration` | haiku | `MIGRATOR` contains v12 `users`, `cargo test` passes without DB |
| S1-U2 | `ApiError::Forbidden` 403 mapping | Authz failure leaks as 401 → client retries login forever | `cargo test -p peacock-api --lib error::tests::forbidden_maps_to_403` (custom) + `error::tests::domain_*` | haiku | `ProblemKind::Forbidden` → 403, `type` slug `forbidden`, `require_role!` uses `forbidden` |
| S1-U3 | Auth middleware 401 gate + JWT verify | Anon passes `GET /api/tables` as 200 → spoofable `X-Restaurant` | `cargo test -p peacock-api --lib middleware::auth::tests` | sonnet | `/api/*` without cookie → 401 `problem+json`, `is_public_path` pinned, `Authorization: Bearer` fallback, HS256 pinned |
| S1-U4 | Users CRUD Owner-only + argon2 | Plain-text password or hash leaks in JSON, duplicate race → 500 | `cargo test -p peacock-api --lib routes::users::tests` (needs DB; degraded to `--lib -- --skip users` + structural check) | opus | `POST /api/users` 201 with argon2 `$argon2` hash, `password_hash` never serialized, `created_by` set, duplicate → 409, `GET /api/users` + `PATCH /api/users/:id` Owner-only |
| S1-U5 | Web `settings/users` page Owner gate + redirect | Non-owner sees user list, or 401 doesn't redirect to `/login` | `npm run build` + `grep -r "settings/users" peacock-web/src` + `grep -q "role.*owner" peacock-web/src/app/settings/users/page.tsx` | sonnet | Page renders list/add/deactivate, redirects to `/login` on 401, role badges correct, build 10 routes |

## Routing rule

If verify command needs DB (`users::tests`) and `DATABASE_URL` absent, degrade: run `cargo test -p peacock-api --lib -- --skip users` plus structural `grep` that `users.rs` contains `argon2` and `password_hash` and `require_role!(caller, Owner)`. Record degrade in `{SCRATCH}`.

## Anti-patterns

- Don't use opus for haiku work (copying a migration, adding a header).
- Don't use haiku for opus work (gapless invoicing, BOM walk).
- Builder-Web never touches Rust; Builder-Rust never touches Next.

