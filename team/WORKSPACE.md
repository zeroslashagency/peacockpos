# Workspace Layout — Peacock POS

Each agent operates in isolation; deliverables go to `shared/`.

```
repo/
  team/
    README.md, ROLES.md, WORKSPACE.md, MODEL_ROUTING.md, TASK_LIFECYCLE.md
    agents/
      orchestrator/SOUL.md
      builder-rust/SOUL.md
      builder-web/SOUL.md
      reviewer/SOUL.md
      ops/SOUL.md
    tasks/<yyyy-mm-dd>-<slice>-<unit>.md   — one file per task, orchestrator owns
    shared/
      specs/<slice>.md                      — requirements, acceptance, verify cmds
      artifacts/<slice>/                    — build outputs (routes, migrations, pages)
      reviews/<task>-review.md              — reviewer notes, must address before Done
      decisions/<nnn>-<title>.md            — ADRs, who decided, why
    evals/
      capability.sh                         — slice gates (auth 401/403, dashboard 200)
      regression.sh                         — parity + core + build

  scripts/
    eval-capability.sh   — shim → team/evals/capability.sh
    eval-regression.sh   — shim → team/evals/regression.sh

  peacock-api/src/routes/*    — Rust handlers (Builder-Rust owns)
  peacock-storage/migrations/* — SQL (Builder-Rust owns)
  peacock-web/src/app/*       — Next routes (Builder-Web owns)

  {SCRATCH}/                  — /var/.../implementer (private, ephemeral)
    lifecycle.log
    evals-before.log
    evals-after.log
    unit-verify.log
```

## Rules

- Agents read/write own `agents/<role>/` freely.
- Deliverables always to `shared/` — never personal workspaces.
- Agents may read any `shared/` dir.
- Orchestrator may read all for oversight.
- `team/tasks/*.md` is the task board — Inbox is files with `status: inbox`, not a DB.
- Harness output captured to `{SCRATCH}` (private scratch dir), never shared `/tmp`; committed tests remain durable.

## Isolation in workflows

When using `.grok/workflows/*.rhai`, use `isolation: worktree` per lane so Rust/Web builders don't clobber each other. Each lane clones an isolated worktree; orchestrator merges.

