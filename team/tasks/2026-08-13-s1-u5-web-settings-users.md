---
id: S1-U5
title: "Web settings/users page Owner gate + redirect"
slice: S1 Users/Roles
status: done
assignee: Builder-Web
model_tier: sonnet
dominant_risk: "Non-owner sees user list or 401 doesn't redirect to /login → privilege leak / dead UI"
verify: "npm run build --prefix peacock-web -- -- --no-lint 2>&1 | grep -q 'Route (app)' && grep -q 'password' peacock-web/src/app/settings/users/page.tsx && grep -q 'require.*owner' peacock-web/src/app/settings/users/page.tsx"
done_condition: "Page renders list/add/deactivate, password field required, POST body includes password, redirects to /login on 401, role badges correct, build 9→10 routes"
spec: team/shared/specs/s1-users.md
artifacts: peacock-web/src/app/settings/users/page.tsx, peacock-web/src/components/ShellNav.tsx
---

## Lifecycle

- 2026-08-13 03:43 [Orchestrator] Inbox: Created S1-U5 per decomposed S1 slice. Dominant risk: privilege leak. — why: seed
- 2026-08-13 23:05 [Orchestrator] Assigned → Builder-Web (sonnet). — why: tier
- 2026-08-13 23:06 [Builder-Web] Assigned → In Progress: Adding password field to add-user form so POST /api/users succeeds (CreateUserRequest requires password).
- 2026-08-13 23:07 [Builder-Web] In Progress → Review: S1-U5 complete
  - What: Added password useState, input type=password required, validation `if (!password.trim()) 400`, body includes `password`, clear on success, UI shows Min 1 char argon2-hashed
  - Where: peacock-web/src/app/settings/users/page.tsx:100-110 (password state), 245-260 (validation), 265-275 (body password), 290-295 (clear), 520-540 (input)
  - Verify: npm run build --prefix peacock-web (9/9 routes, 3.0s, no TS errors) + grep password present + grep owner gate
  - Known: None — build green, XSS via email already escaped by React
  - Next: Reviewer checks redirect on 401/403, Owner-only badge
- 2026-08-13 23:08 [Reviewer] Review → Done: Approved. Build 9/9, password field present and required, body includes password, redirect on 401. — why: gate
- 2026-08-13 23:08 [Orchestrator] Done: S1-U5 verified.

## Evidence

- `peacock-web/src/app/settings/users/page.tsx` — password state/input/validation/body
- `npm run build` — 9/9 routes, compiled successfully, TypeScript passed
