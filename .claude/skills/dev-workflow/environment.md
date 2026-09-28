# dev-workflow -- environment specifics (manycommander)

The generic workflow is the vendored `SKILL.md`. This overlay supplies manycommander's
commands and conventions.

## Branch and worktree

Work on the current branch (`main`) unless the owner explicitly asks for a branch
(BRANCH-01). When a branch is requested, create it as a worktree under
`~/.manyfold-worktrees/manycommander/<task-name>` on branch `feature/<task-name>`. A push
from such a worktree needs `ESTATE_ROOT=~/Developer/Private` on the push command, because
the global pre-push hook looks for the estate checkouts beside the checkout's top level.

## Workflow types

| Type | Paths | Notes |
|---|---|---|
| A: File operations | `src/fsops/**`, `tests/fs_*.rs`, `tests/trash*.rs`, `tests/identity.rs` | Highest risk (design section 4). Every change keeps I-1..I-7; run `full` before the push, and record the `unshare` evidence line |
| B: UI, panels, theme, command line | `src/app/**`, `src/panel/**`, `src/ui/**`, `src/theme/**`, `src/cmdline/**`, `src/config.rs`, `tests/ui_*.rs`, `tests/snapshots/**` | `insta` snapshots: review with `cargo insta review`, never accept blindly |
| C: Performance | `benches/**`, `scripts/bench/**` | Run `scripts/check.sh bench` when a change touches listing, rendering or copy paths; append results to `docs/perf/history.md` |
| Docs | `docs/**`, `README.md`, `contrib/**` | The repository is designated public: no tenant, client, host or private repository names |
| Site | `site/**`, `scripts/site.sh`, `.github/workflows/site.yml`, `examples/site_screens.rs` | User documentation lives in `site/content/docs/`, not in `README.md`. Run `scripts/site.sh check` and `scripts/site.sh worker` before the push; `check.sh full` does not build the site, the `site` workflow does. After a UI change, regenerate the screenshots with `cargo run --example site_screens` |

## Environment setup

No setup beyond the machine toolchain (stable Rust with rustfmt and clippy, cargo-deny,
cargo-insta, hyperfine, perf, rclone, fusermount3), which the machine setup maintains.

- Run `scripts/install-hooks.sh` once per clone. It installs `.git/hooks/pre-push`, which runs
  `scripts/check.sh full`.
- The publication gate needs the path of the estate's private name list in
  `MC_PUBLISH_NAMES`, set in the environment or in the untracked
  `.publish-gate.confidential.env` (ignored by `*.confidential.*`). Never commit the path's
  target or any term from it.
- Test directories: same-filesystem fixtures under `target/test-tmp/`; cross-filesystem
  fixtures under `MC_XDEV_DIR` (default `/dev/shm/mc-xdev`, tmpfs).

## Verification

Run the `verification-loop` skill; its `environment.md` holds the tiers.

## Publication

Commit per task (Conventional Commits, explicit paths). Push after `scripts/check.sh full`
passes; the pre-push hook runs it again together with the estate's publication gate.
