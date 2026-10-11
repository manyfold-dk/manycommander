# Contributing to manycommander

Thank you for looking. manycommander is a small project with one maintainer, so a short
issue before a large pull request saves both of us time.

## What fits

manycommander is a dual-pane file manager for Linux terminals, built for
[Omarchy](https://omarchy.org/) and its terminals. Changes that keep it fast, keep its file
operations safe and keep it in the Total Commander tradition fit best. Portability beyond
Linux is a non-goal ([ADR 0002](docs/adr/0002-a-new-linux-only-file-manager-in-rust.md)).

Before you start on a feature, read the design of the part you touch:
[docs/README.md](docs/README.md) explains the technical design records (TDRs), which say
how each part works, and the architecture decision records (ADRs), which say why.

## Build and run

You need Linux 6.8 or later and the latest stable Rust (`rust-toolchain.toml` selects it).

```bash
cargo run                      # debug build in the current terminal
cargo run -- ~/Downloads /tmp  # the left and right panel directories
cargo run -- --log /tmp/mc.log # a debug log: keys, timings, theme reloads; never file contents
```

## Check your change

`scripts/check.sh` is the verification gate. While you work:

```bash
scripts/check.sh quick         # rustfmt, clippy (warnings are errors), unit tests
cargo test --test fs_copy      # one integration test file
cargo test --all-targets --features failpoints   # the fault-injection suites
```

GitHub Actions runs `scripts/check.sh ci` on every pull request: every test, `cargo deny`,
and the publication gate's shape and secret checks. Some tests need btrfs, user namespaces
(`unshare -rm`) or FUSE; without them a test prints `SKIP` and its reason, and CI shows it.
Cross-filesystem tests use `MC_XDEV_DIR`, a directory on another filesystem than `target/`
(default `/dev/shm/mc-xdev`).

To run the `ci` tier locally, put a checkout of
[estate-baseline](https://github.com/manyfold-dk/estate-baseline) beside this repository, or
point `ESTATE_ROOT` at its parent directory: the publication gate comes from there.

Tests that drive the binary in a pseudo-terminal stub `xdg-open` and `gio`, so no test opens
anything on your desktop. Keep it that way in new tests (see `tests/common/`).

Checks on a real desktop run only on demand:
`MC_MANUAL=1 cargo test --test manual -- --ignored --test-threads=1`. They cover a stalled
FUSE mount, hand-offs, the trash with `gio`, a live theme switch, and the quick view's
pictures in real Ghostty and foot inside a nested Hyprland ([scripts/fixtures/](scripts/fixtures/)).
`tests/ui_keys.rs` replays the bytes Ghostty and foot send on US and Spanish keyboard layouts;
`scripts/fixtures/record-keys.py` records them again. The checks that need a person at the
keyboard are in [docs/runbooks/release-verification.md](docs/runbooks/release-verification.md).

## Rules the code keeps

- **File operations** (`src/fsops/`) uphold the invariants I-1 to I-7 of
  [TDR-1 4.1](docs/tdr/0001-core.md#41-invariants). A change there comes with a test, and
  usually with a failpoint test (`--features failpoints`).
- **The UI thread makes no filesystem syscalls.** Reading a directory, a file or a server
  happens on a worker thread ([ADR 0003](docs/adr/0003-threads-and-one-event-channel.md)).
- **`unsafe`** is allowed only in `src/fsops/sys.rs`; every other module has
  `#![forbid(unsafe_code)]`.
- **A new dependency** says why in the commit that adds it, and `cargo deny check` passes.
- **Names are bytes.** Paths stay `PathBuf`/`OsString`; the UI escapes control characters and
  invalid UTF-8 when it shows a name.

## Documentation

- User documentation is the website: [site/content/docs/](site/content/docs/). Change it in
  the same pull request as the behaviour. `scripts/site.sh serve` previews it with
  [Zola](https://www.getzola.org/); `scripts/site.sh check` checks its links.
- A change to described behaviour updates the TDR section that describes it; a decision of
  note gets a new ADR. Code comments cite the design as `TDR-1 4.7`.
- Screenshots on the site come from `cargo run --example site_screens`.

## Commits and pull requests

- [Conventional Commits](https://www.conventionalcommits.org/): `fix(fsops): ...`,
  `feat(archive): ...`, `docs: ...`, `test: ...`.
- One logical change per commit. A pull request is merged as one squashed commit, so it holds
  one logical change, and its title is the commit's Conventional Commit title.
- Tests carry the acceptance-check identifier they cover in their name (`a_fs_5_...`).
- `CHANGELOG.md` gets an entry under `[Unreleased]` for a change a user would notice.
- manycommander is licensed under MIT or Apache-2.0, at your option. Unless you explicitly
  state otherwise, any contribution intentionally submitted for inclusion in the work by you,
  as defined in the Apache-2.0 license, shall be dual licensed as above, without any
  additional terms or conditions.

## Maintainers

- `scripts/install-hooks.sh` installs a pre-push hook that runs `scripts/check.sh full`: every
  test with skips counted as failures, and the publication gate with a private name list that
  keeps private names out of this public repository. Outside contributors cannot run that list
  and do not need to.
- `scripts/check.sh bench` runs the benchmarks; results go to
  [docs/perf/history.md](docs/perf/history.md).
- Releases, the AUR package and the website: [docs/runbooks/](docs/runbooks/).

## Coding agents

`AGENTS.md` and `CLAUDE.md` hold instructions for coding agents that work on this repository.
They follow the same rules as this file.
