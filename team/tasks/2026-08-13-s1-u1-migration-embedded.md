---
id: S1-U1
title: "012_users.sql migration embedded in MIGRATOR"
slice: S1 Users/Roles
status: done
assignee: Builder-Rust
model_tier: haiku
dominant_risk: "Missing migration → relation \"users\" does not exist on every auth/users test"
verify: "cargo test -p peacock-storage --lib migrator_contains_the_users_migration -- --nocapture"
done_condition: "MIGRATOR contains v12 users; cargo test passes without DB"
spec: team/shared/specs/s1-users.md
artifacts: peacock-storage/migrations/012_users.sql, peacock-storage/src/lib.rs
---

## Lifecycle

- 2026-08-13 03:40 [Orchestrator] Inbox: Created S1-U1 per decomposed S1 slice. Dominant risk: migration missing. — why: seed decomposed list
- 2026-08-13 03:41 [Orchestrator] Assigned → Builder-Rust (haiku, mechanical: copy migration, add test). — why: cheapest capable tier per MODEL_ROUTING.md
- 2026-08-13 03:41 [Builder-Rust] Assigned → In Progress: Picking up S1-U1. No questions — migration file already on disk, just verify embedding. — why: start work
- 2026-08-13 03:42 [Builder-Rust] In Progress → Review: S1-U1 complete
  - What: Verified 012_users.sql exists (uuid, email unique, password_hash, role check, restaurant/branch, active, created_by, timestamps) and MIGRATOR embeds it; added test migrator_contains_the_users_migration in peacock-storage/src/lib.rs
  - Where: peacock-storage/migrations/012_users.sql, peacock-storage/src/lib.rs:355-369
  - Verify: cargo test -p peacock-storage --lib migrator_contains_the_users_migration -- --nocapture (see {SCRATCH}/unit-verify.log)
  - Known: DB-dependent users::tests still need DATABASE_URL — out of scope for this haiku unit
  - Next: Reviewer checks migrator version ordering + uniqueness

- 2026-08-13 03:43 [Reviewer] Review → Done: Approved. `cargo test -p peacock-storage --lib migrator_contains_the_users_migration` passes (1 passed). Version 12 pinned, ordered, unique. No drift. — why: gate green
- 2026-08-13 03:43 [Orchestrator] Done: S1-U1 verified and shipped. No failures. — why: reviewer approved

## Evidence

- `peacock-storage/migrations/012_users.sql` — 012_users.sql present (verified ls)
- `peacock-storage/src/lib.rs:355-369` — test present
- `{SCRATCH}/unit-verify.log` — PASS S1-U1

