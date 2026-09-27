# Read-only remote-main inspection

Run these Git operations before archive edits. Fetch updates refs only; it must not change
HEAD, the index, or tracked/untracked working files. Do not pull, rebase, switch, reset,
restore or stash to make the inspection possible.

```bash
git branch --show-current
git rev-parse HEAD
git status --porcelain=v1 --untracked-files=all
git fetch origin refs/heads/main
git rev-parse --verify 'FETCH_HEAD^{commit}'
```

Record the returned SHA as the inspection pin immediately. Subsequent inspection uses that
literal SHA, not a moving ref. If fetch or pinning fails, freshness is unverified; stop the
archive. A detached or unexpected branch allows read-only inspection, but establish the
intended branch before making archive edits without silently creating or switching branches.

For each planned create/modify/delete, inspect the pin with `git ls-tree -r <sha> -- <path>`
and `git show <sha>:<path>`. Use `git diff <base> <sha> -- <paths>` and commit history where
needed; verify semantics and linked documentation. Do not print credential files. Existing
local tests alone do not prove the pinned tree passes; use remote CI evidence or a disposable
copy of that tree for needed checks. A local ahead or behind HEAD is not a failure: required
implementation must exist in the pinned remote tree. Local-only changes never qualify.

After inspection, confirm HEAD, index and working state are unchanged. Before publication,
fetch and re-check if remote changes affect the evidence; archive commits follow the normal
owned-change pull/rebase, reverify and push contract.
