---
name: archive-plan
description: Archive a completed plan after verifying its implementation on freshly fetched remote main.
---

# Archive plan

Read the requested plan and the [lifecycle contract](../../references/documentation/lifecycle.md).
Establish branch and dirty state first. Inspect implementation against fresh remote main
without changing the checkout or index, following the [inspection procedure](references/inspection.md).
A read-only assessment never advances lifecycle status. A local ahead commit is legitimate;
local-only implementation does not qualify for archiving.

Verify every task's required behavior, not just file existence, against the pinned remote
tree. Check related documentation, including stale to-be/as-is pairs. Report missing work
or failed fetch/verification accurately and stop the archive; do not implement missing
features under an archive request. Resolve material exceptions with the user.

When verified and authorized, update `status: implemented` and `updated`, then move the
owned plan into `docs/plans/implemented/`. Re-anchor relative Markdown links and frontmatter
`source` from the new location; check every target. Update inbound links and a maintained
local index where in scope. Stage explicit owned changes, including the old and new plan
paths. Commit, pull with rebase, reverify relevant evidence and links, then push. Do not
clean up branches or worktrees based on a matching plan slug; cleanup needs explicit owned
resource evidence and authorization.

The umbrella portfolio is generated. In `manyfold`, use its repository-owned portfolio
workflow, preserving initial dirty state and staging only attributable generated changes.
In another repo, report that `manyfold` needs portfolio regeneration; do not edit a sibling.
