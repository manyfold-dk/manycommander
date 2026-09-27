---
name: verification-loop
description: Verify changed code, configuration or infrastructure using the affected repository checks.
---

# Verification

Read `.claude/skills/verification-loop/environment.md` in the active repository when it
exists. Preserve its exact domain commands, environment boundaries and required checks.
Otherwise use the relevant project build/test/lint commands. This skill grants no command
permissions and does not authorize deployments or live domain operations.

Use the [task review boundary](../../references/task-review.md) to cover the complete task:
explicit base, intended paths, owned commits, initial state, and current staged/unstaged/
new files. The optional read-only helper enumerates candidates; it cannot prove authorship.
Resolve every unattributed commit and same-path overlap before claiming complete coverage.

Run the checks appropriate to the changed behavior, plus mandatory repository checks.
Do not repeat successful checks or add mirror tests for reversible edits without new
changes, failures or unresolved concerns. Inspect staged changes without printing credential
values. Report the exact checks, pass/fail evidence, and skipped or unresolved work.
