# Team Orchestration — Peacock POS

Operable team protocol for `peacock-pos` per `/agent-team-orchestration` + `/agentic-engineering`.

**Stack:** Rust Axum 0.7 + Postgres 16 + Next 16.3 + Hetzner `2.28.30.22:8080` + Vercel `peacockpos.vercel.app`
**Spec source of truth:** `docs/DEVELOPER_PLATFORM_PLAN.md` (S0 Auth, S1 Developer, S2 Dashboard, S3 Hardening)
**Gap checklist:** `URY-PORT-AND-GAP-PLAN.md` 127 items (B/M/K/F/I/R/T/P/S/C/X)

## Structure

```
team/
  ROLES.md              — roles, model tiers, ownership
  WORKSPACE.md          — directory layout and isolation
  MODEL_ROUTING.md      — per-unit model assignment (haiku/sonnet/opus)
  TASK_LIFECYCLE.md     — Inbox→Assigned→In Progress→Review→Done|Failed
  agents/<role>/SOUL.md — identity, scope, boundaries, handoff format
  tasks/<id>.md         — one file per task, orchestrator owns state
  shared/specs/<slice>.md
  shared/artifacts/<slice>/
  shared/reviews/<task>-review.md
  shared/decisions/<nnn>-<title>.md
  evals/capability.sh   — slice-specific gates (auth 401/403, dashboard 200)
  evals/regression.sh   — parity + core + build
scripts/eval-*.sh       — thin shims wrapping cargo test / npm run build
```

## How to run a slice

1. Orchestrator creates task `team/tasks/<id>.md` in `Inbox`.
2. Orchestrator `Inbox → Assigned` (picks Builder-Rust or Builder-Web).
3. Builder `Assigned → In Progress`, implements, advances `In Progress → Review` with handoff comment (what/where/verify/known/next).
4. Reviewer `Review → In Progress` (with feedback) or `Review → Done` (approval).
5. Orchestrator confirms `Done` and records decision if needed.
6. `Ops` dispatches eval harnesses before/after and captures `{SCRATCH}/*.log`.

No `Review` skip. Every transition gets a comment `who: what — why`.

See `TASK_LIFECYCLE.md` for comment conventions and `MODEL_ROUTING.md` for tiering.
