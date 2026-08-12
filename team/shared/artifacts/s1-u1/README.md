# Artifact — S1-U1 migration embedded

**Where:**
- `peacock-storage/migrations/012_users.sql`
- `peacock-storage/src/lib.rs:355-369` (test `migrator_contains_the_users_migration`)

**How to verify:**
```
cargo test -p peacock-storage --lib migrator_contains_the_users_migration -- --nocapture
```

**What it proves:**
- Migration file exists and is compiled into binary via `MIGRATOR`.
- Without this, every `users` query fails with `relation "users" does not exist`.

**Captured:**
- `{SCRATCH}/unit-verify.log` — PASS S1-U1

