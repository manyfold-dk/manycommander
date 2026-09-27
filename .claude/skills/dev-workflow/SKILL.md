---
name: dev-workflow
description: Implement a feature or multi-step change using the active repository workflow and environment.
---

# Developer workflow

Explore enough context to establish the requested outcome, repository ownership and
execution constraints. Read `.claude/skills/dev-workflow/environment.md` in the active
repo when present; it supplies workflow types A/B/C, paths, worktree conventions and
exact domain/environment commands. Global copies never override the active overlay.

Work on the current branch unless the user explicitly requests another branch. Invoking
this skill alone does not request one. Record branch, base SHA, intended paths and initial
staged/unstaged/untracked state before editing. For multi-commit or resumed work, use the
[task review boundary](../../references/task-review.md) and maintain the passive task note.
Unrelated dirty files do not block independent work; preserve them and resolve overlaps.

Implement the agreed scope. When starting an approved plan in this repo, set its lifecycle
status to `in-progress` and update its date. Read-only review leaves status unchanged.
Use `blocked` when waiting on a required dependency. The
[lifecycle contract](../../references/documentation/lifecycle.md) governs transitions;
`archive-plan` owns verified completion. Route sibling plan updates to their owner.

Execute locally by default. For authorized, useful independent work, delegate only through
available runtime capabilities, with bounded scope and explicit file ownership. If a tool
is absent, work locally when equipped or route to an equipped repository session. Independent
review is available for requested, repository-required or risk-driven review.

Review the full attributed task change and current content. Run required repository/domain
checks using `verification-loop` when available or the overlay directly. Fix findings and
repeat affected checks when new evidence warrants it. Commit coherent owned work; pull
with rebase, reverify and push under the common policy and the user's release gates.
Never stash, restore, reset or temporarily move another session's edits to publish.
If pull/rebase fails, do not push anyway. Use the task-review reference's owned detached
checkout fallback when safe; otherwise coordinate the publication dependency.
Report results and unresolved coverage accurately. Cleanup is limited to authorized,
identified task-owned resources; preserve other sessions and environment boundaries.
