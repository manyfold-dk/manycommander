---
name: plan-writing
description: Write an implementation plan when requested or when a multi-step change needs coordinated execution.
---

# Plan writing

Use the existing requirements and approved decisions. Deliver a plan-only request without
starting implementation. Existing execution authorization remains valid; a plan does not
create a new approval gate unless the user requested one or a material decision remains.

Write `docs/plans/YYYY-MM-DD-<topic>.md` unless directed otherwise. Read the
[lifecycle contract](../../references/documentation/lifecycle.md). New plans start as
`draft`; mark `ready-for-implementation` once finalized and human-approved, updating the date.
A model review alone does not authorize that transition.

Describe goal, affected paths, constraints, task dependencies, risks, acceptance commands
and expected results. Assign execution ownership for coordinated tasks. Include code only
where it resolves material uncertainty. Tasks should produce reviewable outcomes, not a
universal sequence of tiny test/commit steps. Use the [plan template](references/template.md)
when useful; scale detail to the implementer's needs.

Check requirement coverage, contradictory interfaces and actionable acceptance criteria.
For requested or warranted independent review, use the active runtime adapter and the
[review brief](references/review.md). Continue authorized
implementation using repository equipment when equipped; otherwise provide a concise
handoff. Do not repeat an execution-choice question already answered by the user.
