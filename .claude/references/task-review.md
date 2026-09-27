# Task review boundary

At task start capture base SHA, intended repo-relative paths, and initial staged, unstaged
and untracked paths. Maintain explicitly task-owned commit IDs for work across checkpoints.
For durable work use the path returned by:

```bash
git rev-parse --git-path agent-tasks/<task-id>.json
```

The owner writes this passive JSON note with normal file tools and re-reads it after resume.
Store Git facts and scope metadata only, not contents, credentials or raw configuration.
Example shape (replace the placeholder SHA with the actual Git result):

```json
{
  "base": "<full-base-sha>",
  "paths": ["src/component", "tests/component"],
  "initial_state": {"staged": [], "unstaged": [], "untracked": []},
  "owned_commits": []
}
```

For simple uncommitted work, use these Git facts directly. The optional helper ships in
both profiles and never writes notes, the index, refs or working files:

```bash
python3 .claude/helpers/review_scope.py --note <note-path>
python3 .claude/helpers/review_scope.py --base <sha> --path src/component --owned-commit <sha>
```

The helper lists candidate committed/staged/unstaged/new paths and all unrecorded commits
since the base as unattributed, including ones outside intended paths. Review their diffs
and relevance before claiming full coverage. Recorded IDs and optional `Task-Id` trailers
are evidence, not authenticated ownership; the helper does not inspect trailers or authors.
It flags paths outside scope, initial-state overlap and unattributed same-path changes.
Missing initial evidence is ambiguous, not a clean start. Renames appear as deletion plus
addition so neither path disappears from coverage.

Resolve overlaps with the owner using actual task evidence. Read current uncommitted content
and every attributed task commit, including changes canceled by a later commit. Never use
an empty main-branch range or just the last commit as the task boundary. If attribution
cannot be resolved, report incomplete coverage and preserve all ambiguous changes.

## Publication with concurrent dirty state

Never stash, restore, reset or temporarily move another session's edits to make publication
possible. If pull/rebase fails, do not push anyway. A fresh fetch or expected fast-forward
does not waive a successful pull/rebase and relevant verification before push. When safe, use an owned disposable
detached checkout containing the owned commit, pull with rebase from the intended remote
branch there, re-run relevant verification, then push normally to that branch. This needs
no new branch. Leave the original checkout/index untouched; preserve the published/rebased
commit mapping in the task evidence. Otherwise coordinate the publication dependency with
the owner. Do not use this fallback to bypass an explicit user publication gate.

The detached fallback is a concrete alternative when original dirt blocks publication.
Replace placeholders with the task-owned commit, scratch path and intended remote branch:

```bash
git worktree add --detach <owned-scratch-path> <owned-commit>
git -C <owned-scratch-path> pull --rebase origin <target-branch>
# Run the relevant repository verification in that rebased scratch checkout.
# Proceed only after pull/rebase and verification both succeeded.
git -C <owned-scratch-path> push origin HEAD:<target-branch>
```

Leave the original dirty checkout and index untouched. If a required command fails, fix it
within owned scope or report the publication dependency; do not skip directly to push.
