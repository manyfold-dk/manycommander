---
title: manycommander M1 and M2 implementation
type: plan
status: in-progress
owner: manycommander
source: ../specs/2026-09-27-manycommander-design.md
created: 2026-09-27
updated: 2026-09-27
---
# manycommander M1 and M2 implementation

Build manycommander to the [design](../specs/2026-09-27-manycommander-design.md): M1 (the
MVP, including the section 13 performance targets), then M2 (tabs and restore), then the
`SUPER + E` switch. The design is normative. This plan orders the work, names the modules,
and ties every task to the design's acceptance checks (design section 11, cited as `A-*`).

Exclusions: everything in the design's "Later" row (archives, SFTP, built-in viewer or
editor, job queue, xattr/ACL/ownership, keymap configuration, mouse). The agent baseline is
already vendored (docs profile, without the private overlay, because the repository is
designated public); T0 switches it to the app profile. No reusable CI is adopted. GitHub
Actions is deferred to the public release (T17).

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
| **T17 CI at the public release (deferred).** GitHub Actions running `check.sh full` minus the environment-dependent tests (no `MC_REQUIRE_ALL`), `cargo deny check` and the publication gate script, with `MC_XDEV_DIR=/dev/shm/mc-xdev`. | `.github/workflows/ci.yml` | public release | The workflow is green; each skipped test names its reason |

## Local check gate

`scripts/check.sh` is the verification contract until T17. Each tier includes the one before.

| Tier | Commands | When |
|---|---|---|
| `quick` | `cargo fmt --check`; `cargo clippy --all-targets --all-features -- -D warnings`; `cargo test --lib` | During work |
| `full` | `MC_REQUIRE_ALL=1 MC_XDEV_DIR=/dev/shm/mc-xdev cargo test --all-targets` and the same with `--features failpoints`; `cargo deny check`; the publication gate | Before every push (pre-push hook) |
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
| T4 | done | 4a0744a | `mv.rs` (rename first, merge, case-only rename, group commit); A-FS-4, 5 (sweep, both commit modes), 6, 7, 9a-c, 10, 11, 12, 13 (move); mount-point skip; 64-file batches |
| T5 | done | 5ab9e13 | `trash.rs`, `delete.rs`; A-TR-1 (automated), A-TR-2, A-TR-3 (automated top-directory half), A-TR-4, A-TR-5, A-DEL-1 (incl. bind mount), A-FS-10 (trash), A-FS-13 (delete) |
| T6 | done | 284fb32 | `theme/` (palette, roles, watcher), `config.rs`; A-TH-2 (replay incl. `IN_CREATE` variant; overflow via the filter), A-TH-3, `paint_background` |
| Review | done | 9d1ac98 | Grok review of `src/fsops`: 7 confirmed findings, all fixed with regression tests (see "Engine review") |
| T7-T10 | done | (this commit) | App shell, panels and listing, dialogs and job wiring, command line and hand-off; one commit (E-19) |

Next action: T11 (keymap audit).

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
| E-8 | T3 | A scripted "Skip" on the error question records the entry as failed with the OS error, not as skipped. | I-7: the entry did fail; the user chose not to retry. |

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
| 2026-09-27 | T7-T10 | `check.sh full`: PASS. `unshare -rm true`: ok; bind-mount tests run: 5. Pty sessions (expectrl + vt100): F10 quit, `SIGTSTP` to state `T` and `SIGCONT` back, `SIGTERM` with the terminal restored, F5 with "directory exists" then "file exists" answered Skip, Shift+F8 without the word, F10 during a job, the `printf '%s\\0'` one-argument insert, a `$PAGER` child receiving the keys. |
| 2026-09-27 | T6 | `check.sh full`: PASS. Theme watcher tests passed six consecutive runs. |
| 2026-09-27 | T5 | `check.sh full`: PASS. `unshare -rm true`: ok; bind-mount tests run: 5; trash top-directory tests ran on a tmpfs mounted under `unshare -rm`. |
| 2026-09-27 | T4 | `check.sh full`: PASS. `unshare -rm true`: ok; bind-mount tests run: 4. The A-FS-5 sweep ran every step boundary in both commit modes with cancel and `EIO`. A probe under `unshare -rm` confirmed that `rename(2)` of a directory carries a bind mount below it along. |
