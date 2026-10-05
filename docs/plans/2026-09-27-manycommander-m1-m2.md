---
title: manycommander M1 and M2 implementation
type: plan
status: in-progress
owner: manycommander
source: ../specs/implemented/2026-09-27-manycommander-design.md
created: 2026-09-27
updated: 2026-10-05
---
# manycommander M1 and M2 implementation

Build manycommander to the [design](../specs/implemented/2026-09-27-manycommander-design.md): M1 (the
MVP, including the section 13 performance targets), then M2 (tabs and restore), then the
`SUPER + E` switch. The design is normative. This plan orders the work, names the modules,
and ties every task to the design's acceptance checks (design section 11, cited as `A-*`).

Exclusions: everything in the design's "Later" row (archives, SFTP, built-in viewer or
editor, job queue, xattr/ACL/ownership, keymap configuration, mouse). The agent baseline is
already vendored (docs profile, without the private overlay, because the repository is
designated public); T0 switches it to the app profile. No reusable CI is adopted. GitHub
Actions for this repository is T17.

Authorization: the owner approved this plan on 2026-09-27. Implementation of T0-T15 is
authorized. For T14, the owner also pre-authorized the session to (a) add a trial binding
line for manycommander on a free chord in `~/.config/hypr/bindings.lua`, (b) copy
`contrib/omarchy/theme-set-hook.sh` to `~/.config/omarchy/hooks/theme-set.d/manycommander`,
and (c) run `omarchy-theme-set` for A-TH-1 and switch back to the theme that was active
before. Nothing else outside the repository is authorized. A-LN-1's key press, the feel test
and T16 stay with the owner. Otherwise the owner applies desktop configuration changes
(T14, T16), because `~/.config/hypr/bindings.lua` and the Omarchy hook directory are outside
this repository. The machine setup (toolchain and tools) is maintained outside this
repository; this plan only verifies it.

Execution: one session in the manycommander repository root works through the tasks in
dependency order, on the current branch. It commits at each task boundary (Conventional
Commits, explicit paths) and pushes after `scripts/check.sh full` passes. The pre-push hooks
run both the publication gate (this repository is designated public) and
`check.sh full`.

## Decisions this plan makes

| Topic | Decision | Reason |
|---|---|---|
| Syscall layer | `rustix` (`fs`, `process`, `termios`) for `statx`, `renameat_with`, `copy_file_range`, `syncfs`, `fstatfs`, `openat` with `OFlags::PATH`, `linkat`, `unlinkat`, `fchmod`, `futimens`, `setrlimit(Nofile)`. `STATX_MNT_ID_UNIQUE` has no named constant in current rustix: pass `0x4000` with `StatxFlags::from_bits_retain` and check `stx_mask` before trusting `stx_mnt_id`. | Safe wrappers exist for every call in design section 4 |
| `unsafe` | No `forbid` at the crate root (it could not be overridden). Every module except `fsops/sys.rs` starts with `#![forbid(unsafe_code)]`, as NFR-SEC states. `sys.rs` is expected to need no `unsafe` either. No syscall outside the design is added (no `ioprio_set`). | NFR-SEC |
| inotify | The `inotify` crate for the theme watcher and the panel watchers (design 3.1 amended). | Exact masks and names (design 7.2) |
| Signals | `signal-hook` iterator (self-pipe), registered before any other thread starts; `SIGTSTP` handled by restoring the terminal, then `low_level::emulate_default_handler(SIGTSTP)`. | Design 3.1, 6; the crate documents a registration race |
| Terminal and UI | `crossterm` + `ratatui`. Keyboard enhancement: query support **before** the input thread starts, push only `DISAMBIGUATE_ESCAPE_CODES`, and handle `KeyEventKind::Press` only. The input thread polls with a short timeout and parks on a pause flag, so suspend can stop it before the terminal is handed to a child. | Otherwise the reader and the child both read the pty (A-UI-3), and bindings fire on key release |
| Other crates | `toml` + `serde` (theme, config), `shell-words` (`$EDITOR`/`$PAGER`), `jiff` (trash `DeletionDate`), `tracing` with a file writer (`--log`). | Design 4.10, 6, 7, NFR-OBS; each crate's reason goes into the commit that adds it (NFR-SUP) |
| Test and bench crates | `tempfile`, `insta`, `criterion` (`harness = false` benches whose numbers `run.sh` parses), `blake3`, `expectrl` plus `vt100` (drive the binary on a pty and read the screen). | |
| Failpoints | Cargo feature `failpoints`. The registry is an `Arc<Failpoints>` carried in the job context, not a thread-local, so injections reach the worker thread. Each named step (`copy.chunk`, `commit.rename`, `commit.link`, `move.syncfs`, `move.statx`, `move.unlink`, `open.opath`, `walk.openat`, ...) can return an errno, set the cancel flag, or run a closure before the real call. Each step keeps a hit counter that tests assert. Off in release builds. | A-FS-5, 8, 9, 12, 13 need deterministic, provably reached injections |
| Worker/UI boundary | `fsops` takes `trait Interaction { fn ask(&mut self, q: Question) -> Answer; fn progress(&mut self, p: Progress); }` and a cancel `Arc<AtomicBool>`. Tests use a scripted implementation; the app uses a channel-backed one. `Question` and `Answer` live in `fsops/question.rs`. | Every safety test runs without a terminal |
| Test directories | Same-filesystem fixtures live under `target/test-tmp/` (btrfs here). Cross-filesystem tests use `MC_XDEV_DIR` (here `/dev/shm/mc-xdev`). Each test creates its own `<dir>/<test-name>-<pid>/`, asserts that source and destination device IDs differ, and removes its directory. | Parallel `cargo test` must not share mutable directories |
| Environment-dependent tests | A test that needs btrfs, a second filesystem, user namespaces or FUSE prints `SKIP <reason>` when the environment lacks it. With `MC_REQUIRE_ALL=1`, which `check.sh full` sets, a skip fails the test. | The local gate is the authority (design section 12); a skip must never pass silently |

## Tasks

| Task | Owner / paths | Dependencies | Acceptance |
|---|---|---|---|
| **T0 Toolchain check and skeleton.** Verify the tools installed by the machine setup (stable Rust with rustfmt and clippy, cargo-deny, cargo-insta, hyperfine, perf, rclone as the user-mountable FUSE daemon, fusermount3). `cargo init` with library and binary; module tree from design section 3; per-module `forbid`; `rust-toolchain.toml` (stable); release profile (`lto = "thin"`, `codegen-units = 1`, `panic = "unwind"`); `deny.toml`. `scripts/check.sh` with `quick`, `full`, `bench` tiers (below). `scripts/install-hooks.sh` installs `.git/hooks/pre-push` running `check.sh full` (a global `core.hooksPath` hook, where present, is expected to call the repository's own hook). Write the repository-owned `.claude/skills/dev-workflow/environment.md` and `.claude/skills/verification-loop/environment.md` (the `check.sh` tiers, `MC_XDEV_DIR`, `MC_REQUIRE_ALL`, work on the current branch), then re-vendor the agent baseline with `--profile app` and run the baseline's `check.sh .`. Own content in `CLAUDE.md`/`AGENTS.md` goes outside the vendored marker block. | machine setup: tools; this session: `Cargo.toml`, `rust-toolchain.toml`, `deny.toml`, `src/**`, `scripts/check.sh`, `scripts/install-hooks.sh`, `.claude/skills/*/environment.md`, the re-vendored baseline files | -- | `check.sh full` passes on the skeleton; a push runs it through the hook; the baseline check passes on the app profile; tool versions recorded in the execution record |
| **T1 Syscall layer and identity.** `fsops/sys.rs` wrappers with failpoints; `FsIdentity { dev, ino, mnt_id }`; the design 4.2 relation (same / subvolume / mount); `RLIMIT_NOFILE` raise. | `src/fsops/sys.rs`, `src/fsops/identity.rs`, `tests/identity.rs` | T0 | Fixtures built inside the test: a directory and its child classify as same; a subvolume created with `btrfs subvolume create` classifies as subvolume; a bind mount under `unshare -rm` classifies as mount. No assertion depends on this machine's `/` and `/home`. A failpoint returns its errno and its hit counter increments. |
| **T2 Traversal, plan, questions.** Directory-fd walker (design 4.3) and the `O_PATH` open-then-reopen; plan scan with byte totals; pre-flight checks (4.6); `Question`/`Answer` and the errno mapping (4.5). | `src/fsops/walk.rs`, `src/fsops/plan.rs`, `src/fsops/question.rs` | T1 | Planner-level A-FS-3, A-FS-4 (symlinked destination and bind mount) and A-FS-13 (swapped component during the walk) |
| **T3 Copy and mkdir engine.** Design 4.7 (temporary file, `copy_file_range` loop, metadata, commit with `RENAME_NOREPLACE` / `linkat` / direct-write mode, symlinks, post-order directory metadata, merge), "Overwrite all older" with `fstatfs` resolution, cancel, progress capped at 15 Hz, F7 (4.9). Worker threads run under `catch_unwind`; a panic becomes a failed `JobDone`. | `src/fsops/copy.rs`, `src/fsops/mkdir.rs`, `src/fsops/job.rs` | T2 | A-FS-1 run twice (under `target/test-tmp/` and under `MC_XDEV_DIR`), A-FS-2, A-FS-3 (engine: source byte-identical), A-FS-4 (engine: no write), A-FS-8, A-FS-10 (copy), A-FS-12 (copy, direct-write counter asserted), A-FS-13 (copy), A-P-8. Failpoint tests for `EMFILE`, `ENOMEM`, `ENAMETOOLONG` fail the entry and the job continues. An injected panic yields a failed report. |
| **T4 Move engine with group commit.** Design 4.8: `renameat2` first with the errno table, mount-point skip before `rename`, merge on `EEXIST`/`ENOTEMPTY`, case-only rename (report names the intermediate path), the cross-filesystem path with the change check before commit, batches (64 files / 256 MiB, flush on directory end, job end and cancel), `syncfs`, `statx` then `unlinkat` on the directory fd, post-order `rmdir`, direct-write batch rules. Batch limits are constants in one place. | `src/fsops/mv.rs` | T3 | A-FS-4 (move), A-FS-5 (sweep in both commit modes, reached-step and step-specific predicates), A-FS-6, A-FS-7 (unprivileged subvolumes), A-FS-9 (a, c with failpoints; b with a real writer, only when the source is btrfs), A-FS-10 (move), A-FS-11, A-FS-12 (move), A-FS-13 (move); case-only rename test where the second rename returns `EIO` |
| **T5 Trash and permanent delete.** Design 4.10 (domains, methods 1 and 2 with every symlink and ownership check, reservation on both sides, basename shortening, GIO `Path` encoding with a test decoder, `jiff` date, fsync order, `EXDEV` fallthrough) and 4.11. | `src/fsops/trash.rs`, `src/fsops/delete.rs` | T2, T3 | A-TR-1 (automated part), A-TR-2, A-TR-4, A-TR-5, A-DEL-1 (bind mount under `unshare -rm`), A-FS-10 (trash, including the 255-byte name), A-FS-13 (delete); `XDG_DATA_HOME` points at a test directory in every test |
| **T6 Theme and config.** `colors.toml` parser, role table and fallbacks (7.1), `NO_COLOR`/`COLORTERM` (7.4), the parent-directory watcher with the exact filter (`IN_MOVED_TO` or `IN_CREATE` for `theme`, `IN_CLOSE_WRITE` for `theme.name`, `IN_Q_OVERFLOW`) and 50 ms debounce, `--theme-file`, `--no-theme-watch`. `config.rs` for `~/.config/manycommander/config.toml` (`paint_background`, pager/editor overrides). | `src/theme/**`, `src/config.rs`, `tests/fixtures/theme/*.toml` | T0 | A-TH-2 (full sequence replay, including an `IN_CREATE` variant and an overflow), A-TH-3; a config fixture with `paint_background = true` selects the `background` role |
| **T7 App shell.** Signal registration first, then terminal setup (enhancement query, push, then the input thread), panic hook, event loop blocking on the channel, shared suspend/resume (pause and drain the input thread, pop the protocol, leave raw mode; the reverse on resume), `SIGTSTP`/`SIGCONT`/`SIGTERM`/`SIGHUP`/`SIGINT`, `--log`, first-flush timestamp, `--exit-after-first-frame`. `ReloadTheme` asks a listing thread to load the palette; the UI thread receives a parsed palette only. Initial directories (cwd left, `$HOME` right, `manycommander <left> [<right>]`). F10/`Alt+X` confirm and cancel a running job. `Ctrl+U` swaps panels. | `src/main.rs`, `src/app/**` | T6 | `expectrl` tests: start and quit with F10; `SIGTSTP` puts the process in state `T`, `SIGCONT` resumes it, F10 quits; `SIGTERM` exits with the terminal restored. A test double fails if `theme::load` runs on the UI thread. Manual start/quit in Ghostty and foot. |
| **T8 Panels and listing.** Listing threads (batched, second-pass link classification, generations, the four-thread abandoned limit, directory sizes, `statvfs`) under `catch_unwind`; inotify refresh with cursor-by-name; nearest-existing-ancestor when the current directory is deleted; natural case-insensitive sort as an index permutation; marks and glob marking; quick search; hidden toggle; per-panel history; compact entry storage; visible-row rendering; escaped name display. | `src/panel/**`, `src/ui/panel.rs`, `benches/listing.rs` | T7 | A-UI-2 (listing layer); `insta` snapshots at 80x24 and 200x60, including the A-FS-10 names; deleting the current directory moves the panel to its parent; an injected listing panic yields `ListingFailed` and the UI stays up; the P-3/P-4 benches run (targets are gated in T12) |
| **T9 Dialogs and job wiring.** Channel-backed `Interaction`; per-verb phases (4.4) including the Shift+F8 typed confirm after the scan; question dialogs with both sides' metadata; progress with cancel and "cancel pending"; report view; "a job is running" refusal; panel refresh after a job; F1 help overlay. | `src/ui/dialog*.rs`, `src/app/jobs.rs` | T3, T4, T5, T8 | `insta` snapshots per question type; `expectrl`: F5 of a directory with "file exists" answered Skip, report checked; Shift+F8 without typing `delete` deletes nothing; F10 during a job asks, then cancels |
| **T10 Command line and hand-off.** Line editor with the ownership rule (design 8), in-session history, `cd` with limited expansion, byte-oriented quoting for `Ctrl+Enter`/`Alt+Enter`/`Alt+P`, `$SHELL -c` through suspend/resume with the exit prompt, F3/F4/Shift+F4 by argv, `setsid -f xdg-open` with reaping, `Ctrl+O`. | `src/cmdline/**`, `src/app/handoff.rs` | T7, T8 | A-FS-10 (command-line clause: `printf '%s\0'` receives one argument per inserted hostile name); `expectrl`: a `$PAGER` child receives keystrokes while manycommander does not |
| **T11 Keymap audit.** Check every chord in design section 8 against the Omarchy default configs of Ghostty and foot (installed), Alacritty and Kitty (configs read from the Omarchy defaults; interactive tests only if the owner installs them), and the Hyprland defaults. Fix collisions in the spec and the code together. | `src/app/keys.rs`, this plan, the spec if a chord changes | T10 | Every chord reaches manycommander in Ghostty and foot (manual, recorded); no chord in the table is bound by any of the four terminals' defaults |
| **T12 Benchmark harness and tuning.** `scripts/bench/`: fixture generator (100k-entry directory, 50k 4 KiB files, 4 GiB and 10 GiB files), an ext4 loop image for A-P-1 (created unprivileged with `mkfs.ext4` on a file, attached with `udisksctl loop-setup` and mounted with `udisksctl mount`), and `run.sh` for A-P-1 to A-P-7 (release build, no `failpoints`): pty session with p99 from `--log` and a job-running assertion; first-flush timestamp; criterion numbers parsed against targets; `/proc` counter deltas; `cp`/`mv` comparisons. Records conditions (AC power, power profile). Tune until every target passes. | `scripts/bench/**`, `benches/**` | T9, T10 | `run.sh <dir>` prints PASS for A-P-1 to A-P-7; numbers and conditions recorded in the execution record and appended to `docs/perf/history.md` |
| **T13 Contrib and install.** `contrib/omarchy/theme-set-hook.sh`; README (install, binding recipe, hook, file-operation semantics, durability contract); `cargo install --path . --root ~/.local`; check the Hyprland session `PATH` and pick a bare name or absolute path for the binding. | `contrib/**`, `README.md` | T12 | Binary resolvable from the Hyprland session `PATH`, or the recipe uses the absolute path; publication gate clean |
| **T14 M1 manual acceptance.** Owner: trial-chord binding; hook install for A-TH-1 part 2; the live `omarchy-theme-set` switch. Session: runs and records A-TR-1 (`gio`), A-TR-3 (tmpfs, and a vfat image via `udisksctl`), A-UI-1 with `scripts/fixtures/stall-fuse.sh` (`rclone mount <dir> <mnt> --daemon` on an empty mount point under `/tmp`, the rclone process `SIGSTOP`ped, `trap` resumes it and runs `fusermount3 -u`), A-UI-3, A-TH-1, A-LN-1. | owner: `~/.config/hypr/bindings.lua`, hook, theme switch; session: `scripts/fixtures/**`, records | T13 | Every design 11.1-11.4 check recorded as passed with an evidence line |
| **T15 M2 tabs and restore.** Per-panel tabs (`Ctrl+T`, `Ctrl+W` with an empty line, `Alt+PgUp`/`Alt+PgDn`, `Alt+1`-`Alt+9`), tab bar, watches on visible tabs only, `state.toml` (paths, tabs, command history) written atomically, restore with ancestor fallback. | `src/panel/tabs.rs`, `src/app/state.rs`, `src/ui/tabs.rs` | T9, T10 | Design 11.5 (state round-trip and missing-path fallback automated; an `expectrl` tab session); A-P-1 and A-P-6 re-run with 5 tabs per panel |
| **T16 `SUPER + E` switch.** The owner replaces the Double Commander line with the design section 9 binding. | owner: `~/.config/hypr/bindings.lua` | T14, T15 | A-LN-1 on `SUPER + E`; the Double Commander line stays as a comment for rollback |
| **T17 CI at the public release.** GitHub Actions running `check.sh ci`: `full` minus the environment-dependent tests (no `MC_REQUIRE_ALL`), `cargo deny check` and the publication gate script with `--names none`, with `MC_XDEV_DIR=/dev/shm/mc-xdev`. | `.github/workflows/ci.yml`, `scripts/check.sh` | public release | The workflow is green; each skipped test names its reason |

## Local check gate

`scripts/check.sh` is the verification contract. `quick`, `full` and `bench` stack: each
includes the one before. `ci` is what GitHub Actions runs. It is `full` without
`MC_REQUIRE_ALL`, and its publication gate checks shapes and scanners only.

| Tier | Commands | When |
|---|---|---|
| `quick` | `cargo fmt --check`; `cargo clippy --all-targets --all-features -- -D warnings`; `cargo test --lib` | During work |
| `full` | `MC_REQUIRE_ALL=1 MC_XDEV_DIR=/dev/shm/mc-xdev cargo test --all-targets` and the same with `--features failpoints`; `cargo deny check`; the publication gate | Before every push (pre-push hook) |
| `ci` | `quick`, then `cargo test --all-targets` (and with `--features failpoints`) with skips allowed, `cargo deny check`, publication gate `--names none` | GitHub Actions, every push and pull request |
| `bench` | `scripts/bench/run.sh` | Milestone sign-off, and when a change touches listing, rendering or copy paths |

Expected: all pass. `full` also records, for a push that touches `src/fsops/`, one line of
evidence that `unshare -rm true` works on this machine and that the bind-mount tests ran.

## Risks and decisions

| Risk | Mitigation |
|---|---|
| `syncfs` cost on a busy btrfs filesystem could miss A-P-7 for small-file moves. | Batch limits are tunable constants (T4). T12 measures 32/64/256-file batches. If `syncfs` still misses, record the measurement and bring the tradeoff to the owner. Relaxing I-1 is not an option. |
| P-1 at 16 ms p99 during a large copy depends on the worker not starving the UI thread. | Progress capped at 15 Hz (A-P-8); no filesystem syscalls on the UI thread. If it still misses, record the measurement and bring it to the owner; there is no out-of-design fix in this plan. |
| Laptop benchmarks are noisy (thermal, power profile, background load). | Fixed conditions recorded with each run; medians of repeated runs; criterion baselines for regressions. |
| A user namespace, btrfs or FUSE capability may disappear on this machine (kernel or policy change). | `MC_REQUIRE_ALL=1` turns the resulting skips into failures, so the change is noticed immediately. |
| The kitty keyboard protocol differs per terminal. | T11 tests the installed terminals; every protocol-dependent chord has a legacy fallback. |
| The Omarchy theme-set sequence may change. | A-TH-2 encodes the current sequence; the `SIGUSR1` hook is the fallback. |
| Delegated review or probe scripts that call syscalls from Python with hand-written ctypes structs can corrupt memory and report wrong values. | Delegation briefs forbid hand-rolled ctypes structs for kernel ABIs; values come from `stat`, `findmnt` or `os.stat()`. Values from sandboxed delegates (mount IDs, namespace-dependent results) are re-checked on the host. |

No material decision is open.

## Verification

`scripts/check.sh full` before every push, and `scripts/check.sh bench` plus the T14 manual
checks before M1 sign-off (T15's re-runs before M2 sign-off). Known limits, from the design:
vfat/exfat change detection and crash consistency are best-effort; a residual race exists
between the pre-unlink `statx` and `unlinkat`; M1 does not preserve ownership, ACLs, xattrs,
hard-link structure or sparseness.

## Review record

The plan's adversarial review (2026-09-27) raised 32 findings. Plan-level findings are
resolved in this revision: per-module `forbid` and no `ioprio_set` (1); fixture-built
identity tests (2); A-FS-1 on both filesystems (3); acceptance ids repeated in the engine
tasks (4, 5); job-scoped failpoints with hit counters (6); per-test directories and the
btrfs gate for A-FS-9b (7); required local evidence for user namespaces (8); the stall-FUSE
fixture (9); tool installation through the machine setup, `/proc`-based A-P-5, installed
terminals (10); A-P-8 (11); cross-filesystem A-P-1 (12); `catch_unwind` and errno tests (13);
off-thread theme load (14); the missing behaviours assigned to T6-T8 (15); `question.rs` in
T2 (16); the crossterm and signal-hook ordering rules (17); section 3.1 amended and the
`IN_CREATE` fixture (18); T15 no longer waits for T14 (25); parsed criterion results (26);
T17 mirrors the local gate (27); intermediate-path report (28); `jiff` (29); first-flush
timing (30, 31); overflow handling (32). Design-level findings are recorded in the design's
appendix B. The review's reading of this machine's mount IDs came from inside its sandbox's
mount namespace and was replaced with host values.

## Execution record

Filled in during implementation: tool versions, task commits, benchmark numbers and
conditions, keymap audit results, `unshare` evidence, manual check evidence. This section is
the state that survives a session compaction: the next action is always in "Status".

### Status

| Task | State | Commit | Notes |
|---|---|---|---|
| T0 | done | f751661 | skeleton, gate, hooks, app profile |
| T1 | done | b4d2b56 | `sys.rs`, `identity.rs`, failpoint registry; 5 tests incl. subvolume and bind mount |
| T2 | done | 88da70e | `walk.rs`, `plan.rs`, `question.rs`; planner-level A-FS-3, A-FS-4 (incl. symlinked destination and bind mount), A-FS-13 |
| T3 | done | 41e9d29 | `copy.rs` (transfer engine), `mkdir.rs`, `job.rs`; A-FS-1 (btrfs, tmpfs, both directions across), 2, 3, 4, 8, 10, 12, 13 (copy), A-P-8, errno and panic tests |
| T4 | done | 4a0744a | `mv.rs` (rename first, merge, case-only rename, group commit); A-FS-4, 5 (sweep, both commit modes), 6, 7, 9a-c, 10, 11, 12, 13 (move); mount-point skip; 64-file batches (256 since T12) |
| T5 | done | 5ab9e13 | `trash.rs`, `delete.rs`; A-TR-1 (automated), A-TR-2, A-TR-3 (automated top-directory half), A-TR-4, A-TR-5, A-DEL-1 (incl. bind mount), A-FS-10 (trash), A-FS-13 (delete) |
| T6 | done | 284fb32 | `theme/` (palette, roles, watcher), `config.rs`; A-TH-2 (replay incl. `IN_CREATE` variant; overflow via the filter), A-TH-3, `paint_background` |
| Review | done | 9d1ac98 | Grok review of `src/fsops`: 7 confirmed findings, all fixed with regression tests (see "Engine review") |
| T7-T10 | done | 44f8b69 | App shell, panels and listing, dialogs and job wiring, command line and hand-off; one commit (E-19) |
| T11 | done | 8d3d0fc, 082f1be | Four collisions resolved in spec and code. The owner confirmed the chords in Ghostty and foot on 2026-10-04 (see "Keymap audit"); `Alt+*` failed in Ghostty on a Spanish layout and is fixed in 082f1be |
| T12 | done; A-P-7 passes since 2026-10-04 | 4765105, 9a38542, c546110, acb8053 and the OD-1b commit | Harness; A-P-1 to A-P-6 pass. A-P-7 missed two of four parts until the owner's decisions OD-1a (copies through an unnamed `O_TMPFILE` file, acb8053: 50k files 1.31x `cp -r`) and OD-1b (1024-file move batches: 1.14x `mv`); see "Open items" |
| T13 | done | 7ca6fc4 | README, theme-set hook; the owner ran `cargo install` (E-30); `~/.local/bin` is on the Hyprland session `PATH` |
| T14 | done | 376f3d7 | Every session check recorded (see "M1 acceptance"); A-LN-1's key press is the owner's |
| T15 | done | e82ee53 | Tabs, `state.toml`, restore; A-P-1 and A-P-6 re-run with 5 tabs per panel |
| T16 | done (2026-09-29) | -- | At the owner's request the session installed the latest release in `~/.local/bin` and switched `SUPER + E` to the design section 9 binding (bare name `manycommander`); the Double Commander line stays as a comment for rollback. `hyprctl configerrors` is empty and `hyprctl binds` lists `SUPER + E` as "File manager (dual pane)". The owner pressed `SUPER + E` on 2026-10-04: A-LN-1 passes on the release (see "M1 acceptance") |
| T17 | done | 263bfab | `.github/workflows/ci.yml` runs `scripts/check.sh ci`; skips print their reason |

Next action: the owner's feel test on the next release, the open question about the active
side, and the owner's choice for `xdg-open` and a terminal handler ("Open items").

### Tool versions (T0)

Recorded as major.minor: the publication gate refuses exact versions in use in this public
repository (PUBLISH-02), so the patch level is left out.

| Tool | Version |
|---|---|
| rustc / cargo / clippy (stable) | 1.98 |
| rustfmt | 1.9 |
| cargo-deny | 0.20 |
| cargo-insta | 1.48 |
| hyperfine | 1.20 |
| perf | 7.2 |
| rclone | 1.75 |
| fusermount3 | 3.18 |
| btrfs-progs | 7.1 |
| util-linux (`unshare`, `findmnt`) | 2.42 |
| gio (GLib) | 2.88 |
| Ghostty / foot | 1.3 / 1.28 |
| Kernel | 7.2 (Arch) |

### Decisions made during execution

| # | Task | Decision | Reason |
|---|---|---|---|
| E-1 | T0 | `.publish-allow.tsv` allows `exact version` in `Cargo.lock` and `Cargo.toml` (file-scoped `*` rows); dependencies in `Cargo.toml` are written as major.minor. | The gate's version shape matches every locked dependency; library versions of the build are not a deployed system's version. |
| E-2 | T0 | `check.sh` reads the private name-list path from `MC_PUBLISH_NAMES` or the untracked `.publish-gate.confidential.env`; the gate script comes from the `estate-baseline` checkout under `ESTATE_ROOT`. | The name list and the repository that holds it are themselves deny-listed values and must not appear in this public repository. The pre-push hook still runs the estate's gate at the locked commit. |
| E-3 | T0 | Tool versions recorded as major.minor. | PUBLISH-02 (exact versions in use). |
| E-4 | T3 | Copy checks the destination name with `statx` before writing and asks then; the commit still uses `RENAME_NOREPLACE`, and an `EEXIST` there re-runs the check and asks again (A-FS-8). | No large copy is written only to find a conflict; I-3 still rests on the atomic commit, not the check. |
| E-5 | T3 | Temporary names shorten the original name by bytes so `.<name>.mc-partial-<16 hex>` fits in 255 bytes. | A 255-byte name must survive copy and move (A-FS-10). |
| E-6 | T3 | Several sources copied to a destination that does not exist are refused ("no such directory"); a single source to a missing path is a copy under the new name. | No implicit `mkdir -p` of a mistyped destination. |
| E-7 | T3 | A refused `fchmod` (`EPERM`, `EOPNOTSUPP`) on vfat or exfat is not an error; everywhere else it raises the error question. | Design 4.7: those filesystems keep mode bits only to their own resolution. |
| E-9 | T4 | A cancel that arrives after the job's last checkpoint (the final chunk of the last file) lets the job finish; the report then says "done", not "cancelled". | Honest reporting (I-7): nothing was left undone. |
| E-10 | T4 | A cross-filesystem move counts an entry as moved only after the flush unlinked its source; a `syncfs`, `statx` or unlink failure counts it as failed with "source kept" / "kept both". | I-7: the report states the state the job left. |
| E-11 | T4 | The mount-point skip applies to entries moved one by one (top level, merge, cross-filesystem traversal). A same-filesystem `rename` of a whole directory carries any mount below it along, which the kernel allows. | Nothing is copy-deleted either way (I-6); verified under `unshare -rm`. |
| E-12 | T4 | A-FS-9b's writer starts once the destination's `.mc-partial-` file exists and repeats the run if it got no append in; the source must then be kept, caught either before the commit or at the flush. | A timed writer was flaky under parallel test load; both detection points satisfy the check. |
| E-13 | T5 | An entry in the home trash's domain uses only the home trash; when that trash is refused (a symlink, a wrong owner), the entry gets the "no usable trash" question and does not fall through to the top-directory methods. | Design 4.10 step 1-2 order; GIO behaves the same; no silent use of an unexpected trash. |
| E-14 | T5 | The home trash's domain is the data home's, or its nearest existing ancestor's when it does not exist yet. | The trash is created there on first use. |
| E-15 | T5 | Trash names follow the design (`N.2`, `N.3` appended), not GIO's insertion before the first dot. | Design 4.10 and A-TR-1 name `N.2`; restore works with any name. |
| E-16 | T5 | Top-directory trash tests run on a tmpfs mounted inside `unshare -rm`, where the user is root-mapped: "no usable trash" (A-TR-4) is produced by a regular file at `.Trash-$uid`, because permission bits do not stop the namespace's root. | Tests must not create trash directories on the machine's real filesystems. |
| E-17 | T6 | `IN_Q_OVERFLOW` is tested through the watcher's event filter; the replay tests use the real inotify watcher. | An unprivileged test cannot shrink the kernel's inotify queue to force a real overflow. |
| E-18 | T6 | The debounce window starts at the first triggering event and is not extended by later ones. | Bounds reload latency for P-9 (200 ms); a trigger after the window only causes a second reload with no effective change. |
| E-19 | T7-T10 | T7, T8, T9 and T10 are one commit. | `App::update`, the runtime and the key map serve all four; the per-task acceptance tests are all in it. |
| E-20 | T7 | The input thread blocks in `poll` on the tty and a wake pipe without a timeout (not a short-timeout poll); crossterm's buffer is drained with zero-timeout polls; suspend parks the thread through the pipe and waits for the acknowledgement. | A timeout would wake an idle process periodically and fail P-5 / A-P-5; the parked thread still never reads while a child owns the terminal. |
| E-21 | T7 | While a handed-off child runs, `SIGINT`, `SIGQUIT` and `SIGTSTP` are ignored by manycommander. `SIGQUIT` is registered so it never kills manycommander without the terminal restore. | The child shares the process group: the terminal's Ctrl+C, Ctrl+\\ and Ctrl+Z belong to the child. |
| E-22 | T9 | Job progress is a status row, not a modal dialog; the panels stay usable during a job. `Esc` (empty line, no load in progress) asks to cancel the job. | The design keeps the UI responsive during a job and refuses a second job; A-P-1 navigates during a copy. |
| E-23 | T9 | Quitting with a running job cancels it and waits for the worker to return before exiting; a second quit request or a signal does the same without asking. | Exiting mid-move would leave `.mc-partial-*` files and an unflushed batch. |
| E-24 | T10 | `cd` also removes shell quotes (`'...'`, `"..."`, backslash) besides the `~` and `$VAR` expansion. | `Ctrl+Enter` inserts quoted names; `cd <inserted name>` must work. No command substitution or globbing is added. |
| E-25 | T10 | Ctrl+O shows the terminal's normal screen, which holds the last command's output, until a key is pressed; output is not captured. | The command runs on the normal screen with inherited stdio. |
| E-26 | T7 | The first-full-frame log line is written when both panels have finished their first listing, not at the first flush. | P-2 defines the full frame with both panels on their directories. |
| E-27 | T11 | Keys the terminals claim are dropped or replaced (Keymap audit); the old chords are not kept as hidden aliases, except `Alt+digit` for tabs. | One chord per action keeps the help and the audit exact. |
| E-28 | T12 | Files below 1 MiB skip the destination lookup before writing; `copy_file_range` is not retried for a filesystem pair that refused it; temporary names use a per-job random base and a counter. | P-7; I-3 still rests on the atomic commit, and the check and the question follow a conflict at commit. |
| E-29 | T12 | Every pty-driven test and benchmark puts a no-op `xdg-open` first on `PATH`; the latency session enters directories with `cd`; `udisksctl` always runs with `--no-user-interaction`. | An early benchmark run opened a fixture file in the desktop browser through `Enter`, and an interactive `udisksctl loop-delete` raised a polkit prompt; test runs must never reach the desktop. |
| E-30 | T13 | `cargo install --path . --root ~/.local` was not run; the trial binding uses the absolute path of the release build. | It writes outside the repository, and the overnight authorization named only the three T14 desktop changes. The session `PATH` does contain `~/.local/bin`, so the recipe's bare name works after the owner installs. |
| E-31 | T15 | A hidden tab releases its listing and keeps directory, sort, cursor name and marks; it reloads when shown. | NFR-RES watches only visible tabs, so a shown tab needs a reload anyway; memory stays bounded by the visible panels (A-P-6 with 5 tabs: 21.1 MB). |
| E-32 | T15 | Command-line directories win over restored tabs, which win over the working directory and `$HOME`. | Design 9: arguments override; M2 restores the last paths. |
| E-33 | T14 | A-TR-3's `gio --restore` on tmpfs is not achievable: GIO refuses system-internal mounts such as `/dev/shm` and `/tmp`. The tmpfs layout is verified; the `gio` restore was shown on vfat and ext4. | Measured: `gio trash` on `/dev/shm` answers "Trashing on system internal mounts is not supported". |
| E-34 | T7 | The UI backend never asks the terminal for the cursor position (ratatui calls it in `Terminal::new` and `clear`). | crossterm's query read the terminal on the UI thread; a key arriving with the reply was lost until the next key (found by the T15 tab session test). |
| E-35 | T7 (after M1) | `SIGWINCH` goes through manycommander's signal thread as a resize event, and crossterm reads the terminal through its level-triggered `use-dev-tty` source (the input thread's drain polls with 1 ms instead of 0). | A terminal going fullscreen did not redraw until a key arrived: crossterm only saw the resize when the input thread read. Its default edge-triggered source also drops the terminal's readiness when `SIGWINCH` arrives in the same batch, which left the next key stuck. A-P-1 (1.07 / 1.45 ms) and A-P-5 (no idle wakeups) re-measured after the change. |
| E-8 | T3 | A scripted "Skip" on the error question records the entry as failed with the OS error, not as skipped. | I-7: the entry did fail; the user chose not to retry. |

### Benchmarks (T12, T15)

`scripts/bench/run.sh` on the reference laptop (AC on, power profile performance, governor
powersave, fixtures on btrfs, the cross-filesystem target an ext4 image on a loop device).
Full numbers per run are in [docs/perf/history.md](../perf/history.md). The final full run
(commit 585de07):

| Check | Result | Measurement | Target |
|---|---|---|---|
| A-P-1 | PASS | p99 key-to-flush 1.22 ms idle, 1.32 ms during a 10 GiB copy to ext4 (job still running) | <= 16 ms |
| A-P-2 | PASS | first full frame 13.2 ms median, 16.4 ms max over 20 starts | <= 50 ms |
| A-P-3 | PASS | 100k entries listed and sorted 110.9 ms; first batch 0.3 ms | <= 300 ms; <= 50 ms |
| A-P-4 | PASS | re-sort 15.2 ms; hidden filter 0.1 ms | <= 30 ms |
| A-P-5 | PASS | 60 s idle: 12 -> 12 voluntary context switches, 8 -> 8 CPU ticks | unchanged |
| A-P-6 | PASS | 18.5 MB RSS, both panels on 100k entries | <= 40 MB |
| A-P-7 | FAIL | 4 GiB to ext4 0.48x `cp` (pass); 4 GiB btrfs reflink copy 0.004 s (pass); 50k x 4 KiB copy 1.71x `cp -r` (miss, <= 1.5x); move of 50k x 4 KiB to ext4 2.44x `mv` (miss, <= 2x) | see left |

M2 re-runs with 5 tabs per panel (T15): A-P-1 p99 1.38 ms idle, 1.62 ms during the copy;
A-P-6 21.1 MB. Hidden tabs release their listing (E-31), so memory stays with the two
visible panels.

A-P-7 tuning (the plan's "record the measurement and the best tuning attempt"):

| Attempt | Small-file copy vs `cp -r` | Small-file move vs `mv` |
|---|---|---|
| T12 first run | 1.82x | 2.50x (87.4 s vs 35.0 s) |
| No destination lookup below 1 MiB, no repeated `copy_file_range` per filesystem pair, no `getrandom` per temporary name | 1.70x | -- |
| 256-file batches instead of 64 (design 4.8 amended, I-1 unchanged) | -- | 27.7 s (0.79x of that run's 35.0 s `mv`) |
| Final run | 1.71x | 2.44x (28.8 s vs 11.8 s: the `mv` baseline itself varied from 35.0 s to 11.8 s between runs) |

Measured, not adopted (they need the owner's decision):

| Option | Measurement | Trade-off |
|---|---|---|
| Commit a copied file with `O_TMPFILE` + `linkat` instead of the named temporary file + `RENAME_NOREPLACE` (design 4.7 steps 2 and 5), keeping the named file as the fallback where `O_TMPFILE` is unsupported (vfat, exfat, most FUSE) | 1.36x `cp -r` (the rename costs 0.73 s per 50k files on ext4; direct writes, which break I-2, measured 1.16x) | Upholds I-2 and I-3 (`linkat` fails with `EEXIST` atomically; no partial name is ever visible; nothing is left after a crash). Changes the design's commit mechanism and the documented `.mc-partial-*` crash residue |
| 1024-file batches | move 15.5 s (vs 27.7 s at 256) | I-1 unchanged; after a crash up to 1024 files can be in both places |

### Keymap audit (T11)

Sources read on this machine: `ghostty +list-keybinds --default` and the Omarchy Ghostty
config; foot's shipped defaults (`/etc/xdg/foot/foot.ini`) and the Omarchy foot config; the
Omarchy Alacritty and Kitty configs (both terminals are not installed, so their built-in
defaults were not read here: unverified); every Hyprland binding under the Omarchy defaults
and the user's `bindings.lua`.

| Chord in the design | Claimed by | Resolution |
|---|---|---|
| `Shift+Down` (mark, move down) | Ghostty `adjust_selection:down` | Dropped; `Insert` remains |
| `Ctrl+PgUp` (parent directory) | Ghostty `previous_tab` | Replaced by `Alt+Up` |
| `Ctrl+Enter` (insert name) | Ghostty `toggle_fullscreen` | Dropped; `Alt+Enter` is the chord |
| `Alt+1`-`Alt+9` (M2: go to tab) | Ghostty `goto_tab` | `Ctrl+Alt+1`-`Ctrl+Alt+9`, then `Ctrl+1`-`Ctrl+9` by the owner's decision after M1 (Ghostty and foot claim only `Ctrl+0`); `Alt+digit` still works where it arrives (foot) |
| `Esc` | Ghostty `end_search` | Kept: the action only applies while Ghostty's search is open |
| -- | Omarchy terminals: `Shift+Insert` paste, `Ctrl+Insert` copy, `Shift+Enter`, `Alt+Shift+Enter` | Not used |
| -- | Ghostty and foot: `Ctrl+=`/`Ctrl+-`/`Ctrl+0` font, `Shift+PgUp/PgDn/Home/End` scroll, `Ctrl+Shift+*`, `Ctrl+Tab`, `Ctrl+Alt+arrows`, `Alt+F4` | Not used |
| -- | Hyprland without `SUPER`: `F9`, `Alt+Tab`, `Alt+Shift+Tab`, `Ctrl+Alt+Tab`, `Ctrl+Alt+Shift+Tab`, `Ctrl+Alt+Delete`, `Print`, `Alt+Print`, media keys | Not used |

Found while testing the encodings: legacy `Ctrl+F3` is `CSI 1;5 R`, the same bytes as a
cursor position report, so crossterm cannot read it. The kitty protocol sends `CSI 13;5 ~`;
design section 8 now says `Ctrl+F3` needs the protocol, which all four terminals support.

Automated evidence: `tests/ui_keys.rs` sends every chord of the table as the bytes a
terminal emits, in legacy xterm encoding and with the kitty protocol negotiated, and checks
the action manycommander logs for each (`--log` records every key with its action). Since
2026-10-04 it also replays the bytes Ghostty and foot really send on US and Spanish layouts
(`tests/fixtures/keys/`, recorded by `scripts/fixtures/record-keys.py` with Hyprland's
`send_shortcut` in a nested Hyprland, so each key goes through the layout's keymap). The
hand-written encodings had missed the shifted and base-layout keys that broke `Alt+*`.

Owner item (done 2026-10-04, owner-verification runbook Step 1.1, 42 rows): in Ghostty
and foot every chord of design section 8 logs one `key` line with the expected action,
with two findings:

- `Alt+*` on a Spanish layout arrived in Ghostty as `+` with `Shift` and `Alt` and did
  nothing. With only `DISAMBIGUATE_ESCAPE_CODES` the kitty protocol reports an `Alt` chord on
  a shifted symbol as the unshifted key. Fixed in 082f1be: the push adds
  `REPORT_ALTERNATE_KEYS` (design section 8 and appendix C); `tests/ui_keys.rs` sends the US
  and Spanish encodings. Earlier passes of the row came through tmux (no protocol) or
  `wtype` (no real `Shift`).
- `Alt+1` in a Ghostty window with one tab reached manycommander (`GotoTab(1)`), although
  Ghostty binds `Alt+1` to `Alt+8` to its own tabs.

### M1 acceptance (T14)

Every check of design 11.1-11.4, with its evidence. *auto* checks run in `scripts/check.sh
full`; the session ran the manual checks from `tests/manual.rs` (`MC_MANUAL=1`, see its
header) on 2026-09-27.

| Check | Result | Evidence |
|---|---|---|
| A-FS-1 | pass | `fs_copy::a_fs_1_copy_tree_btrfs`, `..._xdev_dir` (tmpfs), `..._across_filesystems` (both directions) |
| A-FS-2 | pass | `fs_copy::a_fs_2_overwrite_keeps_other_hard_link` |
| A-FS-3 | pass | `fs_plan::same_file_is_refused_in_plan`, `fs_copy::a_fs_3_same_inode_is_refused` |
| A-FS-4 | pass | `fs_plan::destination_inside_source_is_refused` (incl. symlinked destination), `fs_plan::bind_mount_of_source_subdir_as_destination_is_refused` (`unshare -rm`), `fs_copy::a_fs_4_...`, `fs_move::a_fs_4_...` |
| A-FS-5 | pass | `fs_move::failpoints::a_fs_5_failpoint_sweep` (every step boundary, both commit modes, cancel and `EIO`, reached-step assertions) |
| A-FS-6 | pass | `fs_move::a_fs_6_same_filesystem_move_keeps_inodes` |
| A-FS-7 | pass | `fs_move::a_fs_7_move_between_unprivileged_subvolumes` |
| A-FS-8 | pass | `fs_copy::failpoints::a_fs_8_destination_appears_before_commit` |
| A-FS-9 | pass | `fs_move::failpoints::a_fs_9a_...`, `fs_move::a_fs_9b_real_writer_...` (btrfs -> tmpfs), `fs_move::failpoints::a_fs_9c_...` |
| A-FS-10 | pass | `fs_copy::a_fs_10_...`, `fs_move::a_fs_10_...` (same and cross filesystem), `fs_trash::a_fs_10_...` (255-byte name shortened, `Path` decodes), `app::panel_snapshot_*`, `ui_session::a_fs_10_command_line_insert_is_one_argument` |
| A-FS-11 | pass | `fs_move::a_fs_11_directory_exists_and_type_mismatch` |
| A-FS-12 | pass | `fs_copy::failpoints::a_fs_12_direct_write_mode`, `fs_move::failpoints::a_fs_12_move_in_direct_write_mode` (destination bytes hashed) |
| A-FS-13 | pass | `fs_plan::swapped_component_during_walk_is_not_followed`, `fs_copy`, `fs_move`, `fs_delete` `a_fs_13_*` |
| A-TR-1 | pass | `fs_trash::a_tr_1_home_trash_info_and_collisions`; manual: `gio trash --list` shows both entries, `gio trash --restore` restores the name with a newline and a non-UTF-8 byte (gio in a private D-Bus session with its own data and runtime directories) |
| A-TR-2 | pass | `fs_trash::a_tr_2_symlink_is_trashed_not_its_target` |
| A-TR-3 | pass with one amendment | `fs_trash::a_tr_3_top_directory_methods` (tmpfs in `unshare -rm`, methods 1 and 2); manual: vfat image via `udisksctl` -> method 2, relative `Path`, listed and restored by `gio`; ext4 image with a sticky `.Trash` -> method 1, listed and restored by `gio`; `/dev/shm` (tmpfs) -> method 2 with a relative `Path`, but GIO refuses tmpfs as a system-internal mount ("Trashing on system internal mounts is not supported"), so `gio --restore` cannot apply there (design appendix C) |
| A-TR-4 | pass | `fs_trash::a_tr_4_no_usable_trash_never_deletes_on_its_own` |
| A-TR-5 | pass | `fs_trash::a_tr_5_symlinked_trash_parts_are_refused` |
| A-DEL-1 | pass | `fs_delete::a_del_1_typed_confirmation_and_symlinks`, `fs_delete::a_del_1_bind_mount_inside_tree_is_skipped`, `ui_session::shift_f8_without_typing_delete_deletes_nothing` |
| A-UI-1 | pass | manual (`scripts/fixtures/stall-fuse.sh`): "(loading)" shown; Esc returned in 11 ms; a second load of the stuck directory refused ("previous load of this directory is still blocked"); another directory loaded meanwhile; F10 quit with the load still blocked |
| A-UI-2 | pass | `app::a_ui_2_refresh_keeps_the_cursor_on_its_name`, `app::panel_watcher_reports_changes_within_a_second`; manual: `touch` and `rm` on screen after 208 ms each, cursor kept on its name |
| A-UI-3 | pass | manual: F3 (`less`, quit and SIGKILL), F4 (editor SIGKILLed: "[killed by signal 9]"), the command line (child SIGKILLed, exit prompt), SIGTSTP (state T) and SIGCONT, SIGTERM (exit 0, terminal restored); also `ui_session::sigtstp_*`, `sigterm_*` |
| A-TH-1 | pass | manual, live `omarchy-theme-set`: watcher without hook 62 ms and 64 ms from the `mv` of `current/theme` to the new accent on the border; hook with `--no-theme-watch` 36 ms and 44 ms from the hook's start; back on the original theme afterwards |
| A-TH-2 | pass | `theme::a_th_2_theme_set_sequence_yields_one_change` (incl. the `IN_CREATE` variant), `theme::next_theme_events_alone_do_not_reload`, overflow via the filter unit test |
| A-TH-3 | pass | `theme::a_th_3_fixtures` |
| A-LN-1 | pass | Trial binding on `SUPER + ALT + E` (free in the Omarchy defaults and the user's bindings), loaded by Hyprland (`hyprctl binds`: modmask 72, key E; `hyprctl configerrors` empty). The owner pressed it: it opens manycommander, and a second press focuses it. The session then read `hyprctl clients -j`: class `org.omarchy.manycommander` |
| A-PUB-1 | pass | publication gate in `check.sh full` and the pre-push hook: clean on every push |
| A-P-1 to A-P-6 | pass | "Benchmarks" |
| A-P-7 | fail | "Benchmarks": two of four parts missed after tuning; options for the owner listed there |
| A-P-8 | pass | `fs_copy::a_p_8_progress_is_capped_at_15_hz` |

Evidence on the release (owner verification, 2026-10-04; the runbook's results file holds
the logs):

| Check | Result | Evidence |
|---|---|---|
| A-LN-1 | pass | The owner pressed `SUPER + E`: one window opened; with the focus elsewhere a second press focused it; one window of class `org.omarchy.manycommander`. `hyprctl configerrors` empty, `hyprctl binds` lists the binding |
| A-TH-1 | pass | Live `omarchy-theme-set`: the watcher without the hook 72 ms and 78 ms; the hook with `--no-theme-watch` 24 ms and 25 ms. One reload per sequence was not observed live; A-TH-2 covers it automatically |
| A-UI-1 | pass | Stalled FUSE mount: "(loading)", `Esc` back at once, the second load refused, another directory listed, `F10` quit with the load blocked |
| A-UI-2 | pass | An external `touch` and `rm` on screen after 229 ms and 230 ms; the cursor kept its name |
| A-UI-3 | pass | `less`, the editor and a command-line child each killed with `SIGKILL`; job control stop and `fg`; `SIGTERM` exit with the terminal restored |
| A-TR-1 | pass | A name with a newline and a non-UTF-8 byte restored by `gio trash --restore` (by its `trash://` URI, because `gio` lists the original path of such a name as `(null)`); a plain name restored |
| A-TR-3 | pass | vfat image: method 2, relative `Path`, restored by `gio` |
| T15 restore (design 11.5) | pass | Three restored tabs; a tab whose directory is gone shows its nearest parent; `Ctrl+P` recalls the saved history |

Evidence for the amendments of 2026-10-05 (M1 6, P3 4.6) on the release that carries them
(owner verification, 2026-10-05, runbook Step 5.1, OV-F3-1 to OV-F3-7): every row passes.
`F3` opens a picture in the image viewer and a web page in the browser; `notes.md` goes to
the pager as text; `Ctrl+Q` renders it as Markdown; `F3` on a picture in a zip opens the
view copy in the image viewer, and no new entry is left in the view directory after `F10`.
The first run of OV-F3-2 failed on the runbook's fixture: a one-line `<h1>` file is
`text/plain` to `xdg-open`, which types by content, and the text handler (a terminal editor
with `Terminal=true`) started without a terminal and never ended. The retry with a full HTML
document passed; the runbook now uses one, and OV-F3-7 compares against the copies earlier
runs kept. The hand-off gap behind the failure is an open item below.

Desktop changes made (all pre-authorized): the trial binding line in
`~/.config/hypr/bindings.lua`; `contrib/omarchy/theme-set-hook.sh` copied to
`~/.config/omarchy/hooks/theme-set.d/manycommander` (left installed); four
`omarchy-theme-set` switches (`tokyo-night` -> `catppuccin` and back, twice), ending on
`tokyo-night`. Transient test fixtures (an rclone FUSE mount under `/tmp`, loop devices for
the vfat and ext4 images, a `.Trash-1000` on `/dev/shm`) were removed after each check.

### Open items

| Item | Owner | Detail |
|---|---|---|
| T11 chord confirmation | done (owner, 2026-10-04) | See "Keymap audit": one failure (`Alt+*`, layout-dependent), fixed in 082f1be |
| A-P-7 | done (2026-10-04) | Owner decisions of 2026-10-04, implemented: OD-1a, a local file copied without an Overwrite answer is written to an unnamed `O_TMPFILE` file and committed with `linkat` (design 4.7 amendment, acb8053); OD-1b, 1024-file move batches (design 4.8). `scripts/bench/run.sh` A-P-7 passes in full: 50k x 4 KiB copy 1.31x `cp -r`, move to ext4 1.14x `mv` (15.2 s vs 13.3 s), 4 GiB 0.54x `cp`, reflink 0.003 s (`docs/perf/history.md`, 2026-10-04 21:51). Four control runs showed the unnamed file leaves moves unchanged (27.4-29.8 s at 256-file batches either way); the batches made the difference |
| Feel test | owner | "OK so far" (2026-09-27). The owner keeps it open (2026-10-04) until a working day on the release that carries the two verification fixes. `SUPER + E` stays on manycommander |
| T16 key press | done (owner, 2026-10-04) | See "M1 acceptance": A-LN-1 on the release |
| Listing beside a stalled mount | done (2026-10-04) | Found by the owner verification (A-UI-1): once rclone's 1 s attribute cache expired, the parent directory of a stalled FUSE mount also listed as "(loading)", because the listing's `statx` of the mount point waited on the stopped daemon. The listing now passes `AT_STATX_DONT_SYNC` and `AT_NO_AUTOMOUNT` (design 3.1, appendix C); `tests/manual.rs` `a_ui_1_stuck_fuse_mount` lists the parent after the cache expired, and fails without the change |
| Command-line directories and the active side | owner | `manycommander LEFT RIGHT` restores the active side from `state.toml` instead of starting on the left panel. Design question: should directories on the command line also reset the active side? |
| `xdg-open` and a terminal handler | owner | Found by OV-F3-2 (2026-10-05). `Enter` on a file (M1 6) and `F3` on a file named as a picture, document, medium or web page both run `setsid -f xdg-open <path>` with stdio on `/dev/null` (`handoff::open`). Outside a known desktop environment (Hyprland) `xdg-open` types the file by content and runs the handler's `Exec` line itself, `Terminal=true` or not. When that handler is a terminal program, the program starts without a terminal, shows nothing and does not end: three `F3` presses left three editor processes. `F3` meets this when the name and the content disagree; `Enter` on a text file meets it whenever the text handler is a terminal editor (by code reading, not run live). Options: hand off through `gio open`, which types by name first (`gio info` gives `text/html` for the same file) and launches a `Terminal=true` handler in a terminal (unverified: GIO lists `xdg-terminal-exec` among its terminals; not run); or skip a `Terminal=true` handler and say so; or document the limit |

### Engine review (after T5)

A read-only adversarial review of `src/fsops` by a Grok delegate (brief: no hand-rolled
ctypes structs for kernel ABIs; sandbox-derived values re-checked on the host) reported
seven confirmed findings and no speculative ones. All were verified against the code and
fixed before T9, each with a regression test (commit 9d1ac98).

| # | Severity | Finding | Resolution |
|---|---|---|---|
| 1 | high | A cross-filesystem move of a directory-only tree removed source directories without a `syncfs` covering the new ones (I-1) | Created destination directories join the batch as sync-only entries |
| 2 | medium | "still at source" lost one entry per skipped directory (I-7) | The report counts settled entries separately from issues |
| 3 | medium | A moved directory did not appear in the summary (I-7) | The summary counts directories |
| 4 | medium | Source-directory `rmdir` (move, delete) acted on the name without an identity check | `statx` and inode comparison before `rmdir`; a replacement is kept and reported |
| 5 | medium | Trash reported failure after a successful rename when the `files/` fsync failed (I-7) | A note; the entry counts as trashed |
| 6 | medium | Directory metadata had no vfat/exfat exemption, and a failure still removed the source directory | Directories share the file rule (E-7); a real failure keeps the source directory |
| 7 | medium | A-FS-12 did not hash destination bytes | Both A-FS-12 tests hash the destination |

The review also confirmed as sound: `O_NOFOLLOW` traversal with identity checks, the
`O_PATH` open-then-reopen, no trash copy across filesystems, the mount-point skip before
`rename`, and that the A-FS-5 predicates can fail.

### Evidence log

| Date | Task | Evidence |
|---|---|---|
| 2026-09-27 | T0 | `scripts/check.sh full`: PASS (0 tests). `unshare -rm true`: ok. Baseline `check.sh`: conforms on the app profile. |
| 2026-09-27 | T1 | `check.sh full`: PASS. `unshare -rm true`: ok; bind-mount tests run: 1. Unprivileged `btrfs subvolume create` under `target/test-tmp/` and its removal with `rmdir` both work. |
| 2026-09-27 | T2 | `check.sh full`: PASS. `unshare -rm true`: ok; bind-mount tests run: 3. |
| 2026-09-27 | T3 | `check.sh full`: PASS. `unshare -rm true`: ok; bind-mount tests run: 3. |
| 2026-09-27 | Owner | A-LN-1 key press and focus: OK. Install: done (`~/.local/bin/manycommander`). Feel test: OK so far. State file: set by the owner after the benchmark pollution (fixed in ddcdcd1). |
| 2026-09-27 | T12-T15 | `check.sh full`: PASS before every push. Benchmarks and manual checks as recorded above. |
| 2026-09-27 | T7-T10 | `check.sh full`: PASS. `unshare -rm true`: ok; bind-mount tests run: 5. Pty sessions (expectrl + vt100): F10 quit, `SIGTSTP` to state `T` and `SIGCONT` back, `SIGTERM` with the terminal restored, F5 with "directory exists" then "file exists" answered Skip, Shift+F8 without the word, F10 during a job, the `printf '%s\0'` one-argument insert, a `$PAGER` child receiving the keys. |
| 2026-09-27 | T6 | `check.sh full`: PASS. Theme watcher tests passed six consecutive runs. |
| 2026-09-27 | T5 | `check.sh full`: PASS. `unshare -rm true`: ok; bind-mount tests run: 5; trash top-directory tests ran on a tmpfs mounted under `unshare -rm`. |
| 2026-09-27 | T4 | `check.sh full`: PASS. `unshare -rm true`: ok; bind-mount tests run: 4. The A-FS-5 sweep ran every step boundary in both commit modes with cancel and `EIO`. A probe under `unshare -rm` confirmed that `rename(2)` of a directory carries a bind mount below it along. |
