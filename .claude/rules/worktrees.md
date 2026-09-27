# Worktrees

This applies once the user has asked for a branch (BRANCH-01).

Read the active repo's `dev-workflow/environment.md` for its worktree root, helper and
branch format. Inspect branch, HEAD, and dirty state before setup. A new worktree does not
carry uncommitted changes; preserve those changes and resolve required dependencies with
their owner. Do not stash or copy another session's changes implicitly.

Start an equipped session in the intended worktree when its instructions, tools or
permissions differ. Absolute filesystem paths work; they do not load session equipment or
grant authority. Worktrees have separate indexes but share refs and repository metadata.
Use `git rev-parse --git-path agent-tasks/<task-id>.json` for worktree-local task notes.

Clean up only worktrees/branches explicitly identified as task-owned and authorized for
removal. A matching name is insufficient evidence. Preserve dirty work and other sessions.
