# Roles — Peacock POS Team

One agent, one primary role. Overlap causes confusion.

| Role | Purpose | Scope | Model tier | Capabilities required |
|------|---------|-------|------------|----------------------|
| **Orchestrator** | Route work, track state, make priority calls, own `Inbox→Assigned` and `Review→Done` transitions | Reads all workspaces, writes `team/tasks/*.md`, owns state file, reports to stakeholder | **opus** (xhigh) — judgment, multi-context, prioritizes S0→S3 vs URY P0s | git, task board, plan reading, decision logging |
| **Builder-Rust** | Produce Rust artifacts — `peacock-api`, `peacock-core`, `peacock-storage`, migrations | Implements per spec, writes tests, never invents ad-hoc error kinds, keeps `cargo clippy -D warnings` green | **sonnet** for auth/guard lanes, **opus** for invoicing/COGS gapless SERIALIZABLE (dominant risk) | cargo, sqlx, argon2, jsonwebtoken, sql |
| **Builder-Web** | Produce Next.js artifacts — `peacock-web` routes, components, hooks, shell nav | Implements per `design-taste-frontend-v1`, keeps `npm run build` 10 routes, `bg-[#f9fafb] rounded-[2.5rem]` | **sonnet** | pnpm, next, tailwind, framer-motion, phosphor |
| **Reviewer** | Verify quality, push back on gaps, own `Review → In Progress` or `Review → Done` | Different role than Builder — checks spec match, edge cases, money as String, half-away, BFS merge | **opus** (xhigh) — catches what builders miss | cargo test, parity, code reading, threat model (W4_SECURITY) |
| **Ops** | Cron, health checks, dispatching, running eval harnesses, Hetzner/Vercel probes | Cheapest reliable, follows templates, never improvises | **haiku** | bash, curl, docker (when available), log capture |

## Ownership

- **Orchestrator owns all state transitions except** `In Progress → Review` (Builder) and `Review → In Progress/Done` (Reviewer). Builder never marks its own work `Done`.
- Every transition gets a comment: `[Role] Action: details — why`.
- Failed is valid: `Orchestrator Failed: reason — preserving spec at ...`.

## When NOT to use team orchestration

- Single one-off delegation → use spawn directly.
- Solo agent + trivial change → follow `AGENTS.md`, skip overhead.

## References

- `team/agents/<role>/SOUL.md` — per-agent identity, boundaries, handoff format
- `team/TASK_LIFECYCLE.md` — states and comment conventions
- `team/MODEL_ROUTING.md` — per-unit tier assignment
- `team/shared/decisions/` — ADRs for architecture choices

