---
id: S1-U4
title: "Users CRUD Owner-only + argon2"
slice: S1 Users/Roles
status: inbox
assignee: null
model_tier: opus
dominant_risk: "Plain-text password stored or hash leaks in JSON; duplicate race returns 500 not 409; non-owner can create users"
verify: "cargo test -p peacock-api --lib routes::users::tests -- --nocapture (needs DATABASE_URL; degrade to cargo test -p peacock-api --lib -- --skip users + grep -q 'argon2' peacock-api/src/routes/users.rs && grep -q 'require_role!(caller, Owner)' peacock-api/src/routes/users.rs)"
done_condition: "POST /api/users 201 with argon2 $argon2 hash, password_hash never serialized, created_by set, duplicate 409, GET+PATCH Owner-only"
spec: team/shared/specs/s1-users.md
artifacts: peacock-api/src/routes/users.rs, peacock-api/src/routes/mod.rs, peacock-api/Cargo.toml
---

## Lifecycle

- 2026-08-13 03:43 [Orchestrator] Inbox: Created S1-U4 per decomposed S1 slice. Dominant risk: password handling. — why: seed decomposed list

## Notes

- Requires DB for full test; degraded verify documented in MODEL_ROUTING.md.
- Builder: Builder-Rust opus (SQL, hashing, authz, race handling).
