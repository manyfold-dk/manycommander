# ADR 0010: Local-first verification; a skipped test fails the gate

## Status

Accepted -- 2026-09-27.

## Context

Many of manycommander's guarantees can only be tested on real kernels and filesystems:
moves between two btrfs subvolumes, a bind mount inside a moved tree, a cross-filesystem
move onto tmpfs, a stalled FUSE mount. Hosted CI runners offer no btrfs, usually no user
namespaces and no FUSE. A test that skips silently in such an environment reports green
while it tested nothing.

The file-operation invariants also need deterministic fault injection: an error or a cancel
at every chunk, commit, `syncfs`, `statx` and unlink of a move, with proof that the injected
step was actually reached. And the repository is public, so every push must be checked for
values that must not be published.

## Decision

- **One script is the gate.** `scripts/check.sh` has stacking tiers: `quick` (format, lint,
  unit tests), `full` (every test, with and without failpoints, `cargo deny`, the
  publication gate), `ci`, and `bench` (`full` plus the benchmark harness).
- **Skips fail locally.** A test that lacks btrfs, a second filesystem, user namespaces or
  FUSE prints `SKIP <reason>`. `full` sets `MC_REQUIRE_ALL=1`, which turns every skip into a
  failure. The maintainer's machine is the authority, and `scripts/install-hooks.sh` makes a
  push run `full`.
- **CI is the same command list with skips allowed.** GitHub Actions runs `check.sh ci`:
  `full` without `MC_REQUIRE_ALL`; each skip names its reason. The publication gate runs
  there with its shape checks and secret scanners, without the private name list.
- **Failpoints are a cargo feature.** The registry is an `Arc<Failpoints>` carried in the
  job context, so injections reach the worker thread. Each named step can return an errno,
  set the cancel flag or run a closure, and keeps a hit counter that tests assert. The
  feature is off in release builds.
- **Test directories.** Same-filesystem fixtures live under `target/test-tmp/`;
  cross-filesystem fixtures under `MC_XDEV_DIR` (default `/dev/shm/mc-xdev`). Each test owns
  `<dir>/<test-name>-<pid>/`.
- **Tests never reach the desktop.** Every pty test and benchmark puts a no-op `gio` and
  `xdg-open` first on `PATH`, and `udisksctl` runs with `--no-user-interaction`.
- Checks that need a person or a real desktop run on demand (`MC_MANUAL=1`,
  `tests/manual.rs`) and through the release verification runbook.

## Rationale

- Coverage that silently disappears is worse than a red build. Making a skip fail turns a
  lost capability (a kernel or policy change) into an immediate, named failure.
- A single script keeps local runs, the pre-push hook and CI on one command list.
- Failpoints carried in the job context, with reached-step assertions, make the sweep over
  every step boundary deterministic, and a predicate that cannot fail is caught.

## Consequences

- An outside contributor usually cannot pass `full`; `quick` while working and `ci` before a
  pull request are the contributor tiers. The maintainer runs `full` before a push.
- CI is a safety net, not the authority: it cannot run the bind-mount, btrfs or FUSE tests.
- `full` takes several minutes, because it runs the whole suite twice (with and without
  failpoints).
- Benchmarks run on the reference machine and are recorded in
  [docs/perf/history.md](../perf/history.md); they are not part of CI.

## Alternatives considered

| Alternative | Why not |
|---|---|
| CI as the authority | Runners lack btrfs, user namespaces and FUSE; the most important tests would never run |
| Skips that pass | Lost coverage stays invisible |
| Thread-local failpoints | Do not reach the worker thread that runs the job |
| A mocked filesystem layer | Tests the mock, not the kernel behaviour the invariants depend on |

## References

- [TDR-1 3.3](../tdr/0001-core.md#33-implementation-basis), [TDR-1 11](../tdr/0001-core.md#11-acceptance-checks),
  [TDR-1 12](../tdr/0001-core.md#12-repository-and-publication)
- TDR-1 E-29; [CONTRIBUTING.md](../../CONTRIBUTING.md)
