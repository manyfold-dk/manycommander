---
title: manycommander M1 and M2 implementation
type: plan
status: draft
owner: manycommander
source: ../specs/2026-09-27-manycommander-design.md
created: 2026-09-27
updated: 2026-09-27
---
# manycommander M1 and M2 implementation

Build manycommander to the [design](../specs/2026-09-27-manycommander-design.md): M1 (the
MVP, including the section 13 performance targets), then M2 (tabs and restore), then the
`SUPER + E` switch. The design is normative. This plan orders the work, names the modules,
and ties every task to the design's acceptance checks (section 11 of the design, cited as
`A-*`).

Exclusions: everything in the design's "Later" row (archives, SFTP, built-in viewer or
editor, job queue, xattr/ACL/ownership, keymap configuration, mouse). No shared baseline
payload or reusable CI is vendored.

Authorization: this plan is `draft`. Implementation starts only after the owner approves it
and it moves to `ready-for-implementation`. The owner applies desktop configuration changes
(tasks T14 and T16), because `~/.config/hypr/bindings.lua` and the Omarchy hook directory
are outside this repository.

Execution: one session in the manycommander repository root works through T0-T15 in
dependency order, on the current branch, committing and pushing at each task boundary
(Conventional Commits, explicit paths). The pre-push hook runs the publication gate on every
push, because this repository is designated public.

## Decisions this plan makes

| Topic | Decision | Reason |
|---|---|---|
| Syscall layer | `rustix` (`fs`, `process`, `termios` features) for `statx` (with `STATX_MNT_ID_UNIQUE`), `renameat2` (`renameat_with`), `copy_file_range`, `syncfs`, `openat` with `O_PATH`, `linkat`, `unlinkat`, `fchmod`, `futimens`. | Safe wrappers exist for every call in section 4 of the design. `fsops/sys.rs` is therefore expected to need no `unsafe`, and the crate root can carry `#![forbid(unsafe_code)]`. If a call turns out to need `unsafe` (for example `ioprio_set`, see Risks), it goes into `sys.rs` with `#[allow]` and a comment. If the pinned `rustix` release has no named constant for `STATX_MNT_ID_UNIQUE` (`0x4000`), pass it with `StatxFlags::from_bits_retain`, and check `stx_mask` for the bit before trusting `stx_mnt_id`. |
| inotify | The `inotify` crate directly, not `notify`. | The theme watcher filters exact masks and names (`IN_MOVED_TO` for `theme`, `IN_CLOSE_WRITE` for `theme.name`); `notify` abstracts those away. The panel watchers use the same crate for consistency. |
| Signals | `signal-hook` with its iterator thread (self-pipe). | Design section 3.1; async-signal-safe |
| Terminal and UI | `crossterm` backend, `ratatui`. | Design section 3 |
| Config and theme parsing | `toml` + `serde`. | Design section 7.1 |
| `$EDITOR` / `$PAGER` splitting | `shell-words`. | Design section 6 |
| Logging (NFR-OBS) | `tracing` with a plain file writer, enabled only by `--log`. | Timestamped spans for latency and job timings |
| Test and bench dependencies | `tempfile`, `insta` (UI snapshots via `TestBackend`), `criterion` (P-3, P-4), `blake3` (content hashes), `expectrl` (drive the binary on a pty for P-1 and the UI smoke tests). | |
| Failpoints | A cargo feature `failpoints`. `sys.rs` consults a thread-local registry keyed by a step name (`copy.chunk`, `commit.rename`, `move.syncfs`, `move.statx`, `move.unlink`, `open.opath`, ...). Each point can return an injected errno, set the cancel flag, or run a closure (for example, replace a file) before the real call. Off in release builds. | A-FS-5, 8, 9, 12, 13 need deterministic injection at every step boundary |
| Worker/UI boundary | `fsops` never talks to the UI directly. It takes a `trait Interaction { fn ask(&mut self, q: Question) -> Answer; fn progress(&mut self, p: Progress); }` and a cancel `Arc<AtomicBool>`. The app wires a channel-backed implementation; tests use a scripted one. | Every safety test runs without a terminal |
| Cross-filesystem test directory | Tests read `MC_XDEV_DIR`. If it is unset, the cross-filesystem tests skip with a printed reason. Locally the repository sits on btrfs, and `MC_XDEV_DIR` points into `/dev/shm`. In CI the checkout is on the runner's disk, and `MC_XDEV_DIR=/dev/shm/mc-xdev` (tmpfs). Same-filesystem fixtures live under `target/test-tmp/`, not `/tmp`, because `/tmp` is tmpfs here. | A-FS-5, A-FS-9 |
| Bind-mount tests | Run under `unshare -rm` from a test helper binary. They skip when unprivileged user namespaces are unavailable (some CI runners restrict them), and they run locally. | A-FS-4, A-DEL-1 |

## Tasks

| Task | Owner / paths | Dependencies | Acceptance |
|---|---|---|---|
| **T0 Toolchain and skeleton.** Install the stable Rust toolchain (`omarchy-install-dev-env rust`, which runs rustup). `cargo init` with library and binary. Module tree from design section 3. `rust-toolchain.toml` (stable), release profile (`lto = "thin"`, `codegen-units = 1`, `panic = "unwind"`), `#![forbid(unsafe_code)]`, `deny.toml`, CI workflow (fmt, clippy `-D warnings`, test with and without `failpoints`, `cargo deny check`, `MC_XDEV_DIR=/dev/shm/mc-xdev`). `CLAUDE.md`/`AGENTS.md` are not added (baseline payload deferred). | owner runs the rustup install; session: `Cargo.toml`, `rust-toolchain.toml`, `deny.toml`, `src/**`, `.github/workflows/ci.yml` | -- | `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, `cargo deny check` pass locally; the CI run on the pushed commit is green |
| **T1 Syscall layer and identity.** `fsops/sys.rs` wrappers with failpoints. `FsIdentity { dev, ino, mnt_id }` and the section 4.2 relation (same / subvolume / mount). `RLIMIT_NOFILE` raise. | session: `src/fsops/sys.rs`, `src/fsops/identity.rs` | T0 | Unit tests: `/` vs `/home` classify as different mounts; a directory and its child classify as same; a failpoint returns the injected errno. `cargo test --features failpoints` passes |
| **T2 Traversal and plan.** Directory-fd walker (design 4.3), the `O_PATH` open-then-reopen for regular files, the plan scan with byte totals, and the pre-flight checks (4.6): destination inside source (scan set plus `..` walk), same file, special files, directory cycles, identity boundaries per verb. | session: `src/fsops/walk.rs`, `src/fsops/plan.rs` | T1 | A-FS-3, A-FS-4 (including the bind-mount case), A-FS-13 as automated tests |
| **T3 Copy and mkdir engine.** Section 4.7: temporary file, `copy_file_range` loop with the errno fallbacks, metadata, commit with `RENAME_NOREPLACE` / `linkat` / the `O_EXCL` direct-write mode, symlink copy, directory post-order metadata, merge. The `Interaction` questions and errno mapping (4.5), including "Overwrite all older" at the coarser resolution. Cancel and progress (at most ~15 Hz). F7 (4.9). | session: `src/fsops/copy.rs`, `src/fsops/mkdir.rs`, `src/fsops/question.rs` | T2 | A-FS-1, A-FS-2, A-FS-8, A-FS-10 (copy and display), A-FS-12 automated; `.mc-partial-*` never survives a cancelled or failed test run |
| **T4 Move engine with group commit.** Section 4.8: `renameat2` first with the errno table, merge on `EEXIST`/`ENOTEMPTY`, case-only rename, cross-filesystem path with the pre-commit change check, batches (64 files / 256 MiB, flush on directory end, job end and cancel), `syncfs`, `statx`-then-`unlinkat` on the directory fd, post-order `rmdir`. The batch limits are constants in one place, so benchmarks can tune them. | session: `src/fsops/mv.rs` | T3 | A-FS-5 (full failpoint sweep), A-FS-6, A-FS-9 (a, b, c), A-FS-11 automated with `MC_XDEV_DIR` set |
| **T5 Trash and permanent delete.** Section 4.10 (domains, method 1 and 2 with every symlink and ownership check, name reservation on both sides, GIO `Path` encoding with a decoder for tests, fsync order, `EXDEV` fallthrough) and section 4.11. | session: `src/fsops/trash.rs`, `src/fsops/delete.rs` | T2 | A-TR-1 (automated part), A-TR-2, A-TR-4, A-TR-5, A-DEL-1 automated; `XDG_DATA_HOME` pointed at a temporary directory in every test |
| **T6 Theme module.** `colors.toml` parser, role table and fallbacks (design 7.1), `NO_COLOR` and `COLORTERM` handling (7.4), the parent-directory watcher with the exact event filter and 50 ms debounce, `--theme-file`, `--no-theme-watch`. Fixtures copied from the real theme shape (generic values, no machine paths). | session: `src/theme/**`, `tests/fixtures/theme/*.toml` | T0 | A-TH-2 (replay of the full theme-set sequence: `next-theme` create/fill/delete, `IN_DELETE theme`, `IN_MOVED_TO theme`, `theme.name` rewrite) and A-TH-3 automated |
| **T7 App shell.** Terminal setup and teardown, panic hook, signal thread (`SIGUSR1`, `SIGTERM`, `SIGHUP`, `SIGINT`, `SIGTSTP`, `SIGCONT`), the event loop that blocks on the channel (tick only while a spinner or progress dialog is visible), shared suspend/resume, keyboard-protocol push/pop, `--log`, `--exit-after-first-frame`, `ReloadTheme` wiring with full redraw. | session: `src/main.rs`, `src/app/**` | T6 | The binary starts and quits cleanly in Alacritty, Ghostty, Kitty and foot; `kill -TERM` and `kill -TSTP`/`-CONT` leave a usable terminal (the non-child part of A-UI-3); an `expectrl` smoke test starts the binary on a pty and quits with F10 |
| **T8 Panels and listing.** Listing threads with batched `ListingBatch`, second-pass link classification, generations, the abandoned-thread limit, directory-size and `statvfs` requests, inotify refresh with cursor-by-name, sort (natural, case-insensitive, index permutation), marks and glob marking, quick search, hidden toggle, per-panel history, compact entry storage, visible-row rendering. | session: `src/panel/**`, `src/ui/panel.rs` | T7 | A-UI-2 (automated listing-layer part); `insta` snapshots of a panel at 80x24 and 200x60; criterion benches for P-3 and P-4 compile and run |
| **T9 Dialogs and job wiring.** Channel-backed `Interaction`, per-verb phases (design 4.4) including the Shift+F8 typed confirm after the scan, question dialogs with both sides' metadata, progress dialog with cancel and "cancel pending", report view, the "a job is running" refusal, panel refresh after a job, F1 help overlay (keymap, file-operation semantics, durability contract). | session: `src/ui/dialog*.rs`, `src/app/jobs.rs` | T3, T4, T5, T8 | `insta` snapshots for each question type; an `expectrl` test copies a directory with F5, answers "file exists" with Skip, and checks the report; Shift+F8 without typing `delete` deletes nothing |
| **T10 Command line and hand-off.** Line editor and the key ownership rule (design 8), history (in-session), `cd` with limited expansion, byte-oriented quoting for `Ctrl+Enter`/`Alt+Enter`/`Alt+P`, `$SHELL -c` spawn with suspend/resume and the exit prompt, F3/F4/Shift+F4 argv spawning, `setsid -f xdg-open` with reaping, `Ctrl+O`. | session: `src/cmdline/**`, `src/app/handoff.rs` | T7, T8 | A-FS-10 (command-line part: one argument per inserted hostile name) automated; `expectrl` test runs `printf '%s\0'` on an inserted name |
| **T11 Keymap audit.** Check every chord in design section 8 against the Omarchy default configs of Alacritty, Ghostty, Kitty and foot, and against the Hyprland defaults. Record the result in this plan's "Execution record". Fix collisions in the spec and the code together. | session: `src/app/keys.rs`, this plan, the spec if a chord changes | T10 | Every chord reaches manycommander in all four terminals (manual, recorded); no chord in the table is bound by a terminal default |
| **T12 Benchmark harness and tuning.** `scripts/bench/`: a fixture generator (100k-entry directory, 50k 4 KiB files, a 4 GiB file) under a path given by argument; `run.sh` that runs A-P-1 (pty-driven session, p99 from the `--log` latencies), A-P-2 (`hyperfine`), A-P-3/A-P-4 (criterion), A-P-5 (`perf stat`), A-P-6 (`/proc/<pid>/status`), A-P-7 (against `cp`/`mv`), and prints measured vs target. Tune until every target passes; record the numbers. | session: `scripts/bench/**`, `benches/**` | T9, T10 | `scripts/bench/run.sh <dir>` prints PASS for A-P-1 to A-P-7 under the design's reference conditions; results recorded in the "Execution record" |
| **T13 Contrib and install.** `contrib/omarchy/theme-set-hook.sh`, README sections (install, binding recipe, hook, file-operation semantics summary, durability contract), `cargo install --path . --root ~/.local`. Check the Hyprland session `PATH` (`tr '\0' '\n' < /proc/$(pgrep -x Hyprland)/environ \| grep '^PATH='`) and pick a bare name or absolute path for the binding. | session: `contrib/**`, `README.md` | T12 | The binary runs from the Hyprland session `PATH` or the recipe uses the absolute path; README renders; publication gate clean |
| **T14 M1 manual acceptance.** The owner applies the trial-chord binding and, for A-TH-1 part 2, installs the hook. The session runs and records A-FS-7 (btrfs loop image, needs sudo from the owner), A-TR-1 (`gio` part), A-TR-3, A-UI-1 (stopped FUSE mount), A-UI-3, A-TH-1, A-LN-1. M1 is done when every M1 check is recorded as passed. | owner: `~/.config/hypr/bindings.lua`, hook install, sudo steps; session: records in this plan | T13 | Every section 11.1-11.4 check recorded as passed, with the evidence line (command output or log excerpt) |
| **T15 M2 tabs and restore.** Per-panel tabs (`Ctrl+T`, `Ctrl+W` when the line is empty, `Alt+PgUp`/`Alt+PgDn`, `Alt+1`-`Alt+9`), tab bar, watches only on visible tabs, `state.toml` (paths, tabs, command history) written atomically (temporary file + rename) on quit and on tab changes, restore with nearest-existing-ancestor fallback. | session: `src/panel/tabs.rs`, `src/app/state.rs`, `src/ui/tabs.rs` | T14 | Design 11.5 checks automated where possible (state round-trip, missing-path fallback), plus an `expectrl` tab session; A-P-1 and A-P-6 re-run with 5 tabs per panel still pass |
| **T16 `SUPER + E` switch.** The owner replaces the Double Commander line with the design section 9 binding. | owner: `~/.config/hypr/bindings.lua` | T15 | A-LN-1 re-run on `SUPER + E`; the Double Commander line is kept as a comment for rollback |

## Risks and decisions

| Risk | Mitigation |
|---|---|
| `syncfs` cost on a busy btrfs filesystem could miss A-P-7 for small-file moves. | The batch limits are tunable constants (T4). T12 measures 32/64/256-file batches. If `syncfs` still misses, record the measurement and bring the tradeoff back to the owner. Relaxing I-1 is not an option. |
| P-1 at 16 ms p99 during a 10 GiB copy depends on the worker not starving the UI thread (page-cache pressure, progress flood). | Progress capped at ~15 Hz; the UI does no filesystem syscalls; T12 measures during a copy. If it misses, lower the worker's I/O priority (`ioprio_set` idle class) before anything else. |
| Stopped-FUSE and btrfs-subvolume fixtures need privileges or setup the CI runner lacks. | They are manual checks in T14. Automated tests skip with a printed reason when the environment lacks them, and never pass silently. |
| Unprivileged user namespaces can be restricted on CI runners. | Bind-mount tests skip there with a reason and run locally before each push that touches `fsops`. |
| The kitty keyboard protocol behaves differently per terminal (foot, Alacritty). | T11 tests all four terminals. Every protocol-dependent chord has a legacy fallback. |
| The Omarchy theme-set sequence may change in a future Omarchy release. | A-TH-2 encodes the current sequence as a fixture. The `SIGUSR1` hook is the fallback. Watch Omarchy updates for changes to `omarchy-theme-set`. |
| Delegated review or probe scripts that call syscalls from Python with hand-written ctypes structs can corrupt memory and produce wrong values. | Delegation briefs forbid hand-rolled ctypes structs for kernel ABIs. Values come from `stat`, `findmnt` or `os.stat()`. |

No material decision is open. Batch sizes and I/O priority are tuning inside the design's
limits, recorded in the execution record.

## Verification

Required before every push (the session runs them; CI repeats them):

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
MC_XDEV_DIR=/dev/shm/mc-xdev cargo test --features failpoints
cargo deny check
```

Expected: all pass. Tests that need an unavailable environment print `SKIP <reason>`, and
the session reports every skip. Before M1 and M2 sign-off, additionally run
`scripts/bench/run.sh <fixture dir>` (all PASS) and complete the T14 manual checks.

Known limits, taken from the design: vfat/exfat change detection and crash consistency are
best-effort; a residual race exists between the pre-unlink `statx` and `unlinkat`; M1 does
not preserve ownership, ACLs, xattrs, hard-link structure or sparseness.

## Execution record

Filled in during implementation: task commits, benchmark numbers, keymap audit results,
manual check evidence.
