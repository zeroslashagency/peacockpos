# Decision: 403 Forbidden vs 401 Unauthorized for require_role!

**Date:** 2026-08-13
**Author:** Orchestrator
**Status:** Accepted
**Task:** S1-U2, S1-U3

## Context

`require_role!(caller, Owner)` was returning `ApiError::unauthorized` (401) when a valid session existed but role was insufficient (e.g., waiter calling `POST /api/users`). Clients retried login, and logs conflated "missing session" with "insufficient privilege". W4_SECURITY requires distinguishable signals, and HTTP semantics require 401 = unauthenticated, 403 = authenticated but forbidden.

## Options considered

1. Keep 401 for both missing session and insufficient role — simple but breaks HTTP, confuses clients, hides authz failures in monitoring.
2. Return 403 Forbidden for insufficient role, 401 only for missing/invalid session — correct HTTP, lets client show "ask your owner" without re-login, aligns with `ProblemKind::Forbidden`.

## Decision

Option 2. `peacock-api/src/error.rs` adds `ProblemKind::Forbidden` (403, slug `forbidden`, title `Forbidden`), `ApiError::forbidden/forbid`. `peacock-api/src/middleware/auth.rs` `require_role!` now returns `forbidden` with message `requires role X but caller has Y`. Middleware `authenticate` still returns `unauthorized` (401) for missing/invalid token, including `GET /api/*` without cookie via 401 path check.

## Consequences

- `GET /api/tables` no cookie → 401 `application/problem+json` (middleware).
- `POST /api/users` as waiter (valid session) → 403 `forbidden` (handler).
- Tests updated: `error::tests` maps Forbidden→403, `auth::tests` hierarchy, new `users::tests` `unauthenticated_is_401` vs `waiter_is_403`.
- Monitoring can now alert authz vs authn separately.
