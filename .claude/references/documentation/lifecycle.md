# Plan and spec lifecycle

## Plan and Spec Frontmatter

Every plan (`docs/plans/`) and spec (`docs/specs/`) MUST begin with a YAML
frontmatter block. It is the machine-readable source of truth for status; the
folder location is a coarse cross-check.

```yaml
---
title: <short title>          # optional; falls back to the first "# heading"
type: plan | spec
status: draft | ready-for-implementation | in-progress | blocked | implemented | postponed | superseded | abandoned
priority: P0 | P1 | P2 | P3   # optional; human-set, overrides the computed rank
owner: <name or repo>         # optional
source: <relative link to the review/ADR/spec that spawned it>   # optional
created: YYYY-MM-DD
updated: YYYY-MM-DD            # bump whenever status changes
---
```

### Status vocabulary

| Status | Meaning |
|--------|---------|
| `draft` | Being designed or written; not yet approved. |
| `ready-for-implementation` | Written, reviewed, approved; queued, not started. |
| `in-progress` | Implementation underway. |
| `blocked` | Active but waiting on a dependency. |
| `implemented` | Verified complete; belongs in `implemented/`. |
| `postponed` | Deliberately deferred. |
| `superseded` | Replaced by another plan. |
| `abandoned` | Dropped without completion. |

### Transition ownership

Each transition has one owner so status changes are predictable. Whenever you
change a plan's `status`, bump its `updated` date in the same edit.

| Transition | Owner | Trigger |
|------------|-------|---------|
| -> `draft` | `plan-design` / `plan-writing` | spec or plan file created |
| `draft` -> `ready-for-implementation` | `plan-writing` | plan finalized + human-approved |
| `ready-for-implementation` -> `in-progress` | `dev-workflow` | implementation starts |
| any -> `blocked` | `dev-workflow` / manual | dependency wait |
| `in-progress` -> `implemented` | `archive-plan` | verified, then `git mv` to `implemented/` |
| -> `postponed` / `superseded` / `abandoned` | manual | explicit decision |
