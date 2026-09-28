# verification-loop -- environment specifics (manycommander)

`scripts/check.sh` is the verification contract. Each tier includes the one before.

| Tier | Commands | When |
|---|---|---|
| `quick` | `cargo fmt --check`; `cargo clippy --all-targets -- -D warnings`, with and without `--all-features`; `cargo test --lib` | During work |
| `full` | `MC_REQUIRE_ALL=1 MC_XDEV_DIR=/dev/shm/mc-xdev cargo test --all-targets`, and the same with `--features failpoints`; `cargo deny check`; the publication gate | Before every push (the pre-push hook runs it) |
| `bench` | `scripts/bench/run.sh` (release build, no failpoints) | Milestone sign-off, and when a change touches listing, rendering or copy paths |

```bash
scripts/check.sh quick
scripts/check.sh full
scripts/check.sh bench
```

Expected: the last line is `check.sh <tier>: PASS`.

The site has its own gate, because `full` runs before every push and the site needs tools a
Rust-only change does not (zola, and node for wrangler). It fails, never skips, when a tool
is missing. The `site` workflow runs the same commands on every change under `site/`.

| Command | What it checks | When |
|---|---|---|
| `scripts/site.sh check` | `zola build`, then `zola check` (internal links and anchors) | A change under `site/`, `scripts/site.sh` or the site workflow |
| `scripts/site.sh worker` | `wrangler deploy --dry-run`, then `wrangler dev`: apex 200 with the `_headers` headers, www 301 to the apex, the 404 page | A change to `site/worker.js`, `site/wrangler.jsonc`, `site/static/_headers` or `site/tools.env` |

## Environment variables

| Variable | Meaning |
|---|---|
| `MC_XDEV_DIR` | A directory on a different filesystem than `target/` (btrfs here). `full` defaults it to `/dev/shm/mc-xdev` (tmpfs). Cross-filesystem tests assert that the device IDs differ |
| `MC_REQUIRE_ALL` | `full` sets it to `1`. A test that needs btrfs, a second filesystem, user namespaces or FUSE prints `SKIP <reason>` without it, and fails with it |
| `MC_PUBLISH_NAMES` | Path of the estate's private name list for the publication gate; read from the untracked `.publish-gate.confidential.env` when unset |
| `ESTATE_ROOT` | Directory that holds the `estate-baseline` checkout (default: the parent of this repository) |

## Evidence

`full` prints an evidence block: whether `unshare -rm true` works, and how many bind-mount
tests ran. Record that line in the plan's execution record for every push that touches
`src/fsops/`.

## If a check fails

- `fmt`: run `cargo fmt --all`.
- A snapshot: `cargo insta review`, accept only a reviewed, intended change.
- A `SKIP` that fails under `MC_REQUIRE_ALL=1`: the machine lost a capability (btrfs test
  directory, user namespaces, FUSE, `MC_XDEV_DIR` on a second filesystem). Restore it; do not
  unset `MC_REQUIRE_ALL`.
- The publication gate: remove the value, or add a shape row (never a name) to
  `.publish-allow.tsv`.
