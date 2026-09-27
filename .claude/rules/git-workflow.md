# Git workflow

Use the common policy's branch consent, ownership, conventional commit and rebase/push
rules. Inspect the index and working diff before explicitly staging owned paths. Resolve
same-path overlaps before staging; never sweep another session's changes into a commit.

When the authorized workflow calls for a PR, describe the problem, resulting behavior,
verification and material limitations. Follow repository review and merge requirements.
Do not infer a PR, reviewer message, merge, or branch deletion from a skill invocation.
An optional `Task-Id` trailer helps correlate commits but never proves ownership.

Use [worktrees](worktrees.md) only when branching is requested. Honor actual runtime
permissions; shell formatting and skill frontmatter cannot grant command access.
