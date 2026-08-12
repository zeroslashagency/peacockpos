---
id: S1-U5
title: "Web settings/users page Owner gate + redirect"
slice: S1 Users/Roles
status: inbox
assignee: null
model_tier: sonnet
dominant_risk: "Non-owner sees user list or 401 doesn't redirect to /login → privilege leak / dead UI"
verify: "npm run build && grep -q 'require.*owner' peacock-web/src/app/settings/users/page.tsx || grep -q 'role.*owner' peacock-web/src/app/settings/users/page.tsx"
done_condition: "Page renders list/add/deactivate, redirects to /login on 401, role badges correct, build 10 routes"
spec: team/shared/specs/s1-users.md
artifacts: peacock-web/src/app/settings/users/page.tsx, peacock-web/src/components/ShellNav.tsx
---

## Lifecycle

- 2026-08-13 03:43 [Orchestrator] Inbox: Created S1-U5 per decomposed S1 slice. Dominant risk: privilege leak. — why: seed decomposed list

## Notes

- Builder: Builder-Web sonnet.
- Verify: npm run build must pass 10 routes; grep checks for owner gate.
