# Task Lifecycle — Peacock POS

```
Inbox → Assigned → In Progress → Review → Done | Failed
```

| State | Meaning | Owner | File field `status:` |
|-------|---------|-------|----------------------|
| Inbox | New task, unassigned, spec ready | Orchestrator | `inbox` |
| Assigned | Agent selected, not yet started | Orchestrator | `assigned` |
| In Progress | Agent actively working | Assigned agent | `in_progress` |
| Review | Work complete, awaiting verification | Builder → Reviewer | `review` |
| Done | Verified and shipped | Orchestrator (on Reviewer approval) | `done` |
| Failed | Abandoned with reason | Orchestrator | `failed` |

## Transition rules

- **Orchestrator transitions:** `Inbox→Assigned`, `Assigned→In Progress` (spawn), `Review→Done` (accept), `any→Failed`.
- **Builder transitions:** `In Progress→Review` (submit with handoff).
- **Reviewer transitions:** `Review→In Progress` (feedback) or `Review→Done` (approve; Orchestrator confirms).
- **Never skip Review.** Trivial override must be documented.

## Comment conventions

Every state change gets a comment `YYYY-MM-DD HH:MM [Role] Action: details`.

### Required comments

**Starting:**
```
2026-08-13 03:40 [Builder-Rust] Starting: S1-U3 auth 401 gate. Q: should is_public_path include /api/auth/me? No — me requires auth.
```

**Blocked:**
```
2026-08-13 03:42 [Builder-Rust] Blocked: DATABASE_URL missing, cannot run users::tests — degrading to filtered lib tests + grep. Who can provision?
```

**Handoff (In Progress→Review):**
```
[Builder-Rust] Handoff: S1-U3 auth middleware complete at peacock-api/src/middleware/auth.rs
- What: is_public_path pins /health,/api/auth/login|logout; authenticate returns 401 for /api/* without peacock_session; require_role! uses forbidden (403)
- Where: peacock-api/src/middleware/auth.rs:286-340, peacock-api/src/error.rs:31-44, peacock-api/src/app.rs:59-66
- Verify: cargo test -p peacock-api --lib middleware::auth::tests -- --nocapture  (see {SCRATCH}/unit-verify.log)
- Known: X-Restaurant fallback still present in context.rs — prefer CallerContext.restaurant when session present (next unit)
- Next: Reviewer checks 401 vs 404 for unknown routes, 403 vs 401 for waiter, HS256 pin, problem+json enrichment
```

**Review feedback:**
```
[Reviewer] Feedback: 2 issues.
1. Missing test for is_public_path("/api/auth/me") must be false (auth required) — add.
2. error::tests::from_status must map 403→Forbidden — add.
Returning to Builder-Rust.
```

**Approved:**
```
[Reviewer] Approved: all issues addressed. Gate green. Ready for Orchestrator to mark Done.
```

**Done:**
```
[Orchestrator] Done: S1-U3 verified — Reviewer approved, eval delta shows waiter 401→403 fix, no regression on parity/build.
```

**Failed:**
```
[Orchestrator] Failed: superseded by aggregator decision — preserving spec at team/shared/specs/aggregator-deferred.md
```

## Decision logging

When a choice affects future lanes, write `team/shared/decisions/<nnn>-<title>.md`:

```md
# Decision: ...
Date: YYYY-MM-DD
Author: Orchestrator
Status: Accepted
Task: S1-U3

Context: ...
Options: 1) ... 2) ...
Decision: ...
Consequences: ...
```

## Task file shape

`team/tasks/<id>.md` frontmatter + lifecycle log. Orchestrator is source of truth; agents append comments, never edit frontmatter status directly except via their allowed transition.

