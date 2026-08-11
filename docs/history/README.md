# Historical Documentation

This directory contains superseded plans, lane delivery reports, and historical decision records. They have archival value but are **not current documentation**.

## ⚠️ Important Notes

- **`RUST_MIGRATION_PLAN.md` contains documented fabrications** and is explicitly marked as SUPERSEDED. Do not follow its guidance.
- The numbered phases and lanes referenced in these documents were part of an incremental migration strategy that has since evolved.
- Current architecture, API documentation, and ground truth live in `/docs` (one level up) and in `/docs/MASTER_PLAN.md`.

## What's Here

- `PLAN.md`, `PLAN-REVIEW.md` — original migration decision records
- `RUST_MIGRATION_PLAN.md`, `RUST_MIGRATION_PLAN_V2.md` — superseded migration strategies
- `PHASE_2_3_PLAN.md`, `PHASE_2_3_COMPLETE.md`, `PHASE_4_5_PLAN.md` — phase planning docs
- `IMPLEMENTATION_SUMMARY.md` — summary of work completed up to a snapshot in time
- `lanes/` — per-lane delivery reports (build-process artifacts, not user documentation)

## Current Documentation

For up-to-date information:
- Start with `/README.md` at the repository root
- Read `/docs/MASTER_PLAN.md` for the execution strategy
- See `/docs/ARCHITECTURE.md` for crate boundaries and design
- Consult `/docs/API.md` for HTTP endpoints and contracts
- Check `/docs/GROUND-TRUTH.md` for verified system facts
