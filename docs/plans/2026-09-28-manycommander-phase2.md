---
title: manycommander phase 2 implementation
type: plan
status: draft
owner: manycommander
source: ../specs/2026-09-28-manycommander-phase2-design.md
created: 2026-09-28
updated: 2026-09-28
---
# manycommander phase 2 implementation

Build the [phase 2 design](../specs/2026-09-28-manycommander-phase2-design.md) (cited as
"P2 <section>", acceptance checks as `A-*`) and release it as the next minor version. The
[M1/M2 design](../specs/implemented/2026-09-27-manycommander-design.md) stays normative for
everything phase 2 does not change.

Exclusions: phase 3 (SFTP, archive browsing, image previews) gets its own design after the
release. No keymap configuration, no job queue, no xattr/ACL/ownership preservation.

Authorization: on 2026-09-28 the owner chose items 1-7, asked for this plan with an
independent (grok) review, then implementation, a grok code review, and the release, and
then phase 3 as far as the session gets. The owner is away during execution; the session
decides open points against the design and records them under "Decisions made during
execution". Nothing outside this repository is changed, except publishing the GitHub
release (T11), which the owner asked for.

Execution: this session orchestrates. Each task goes to one implementer subagent with the
task's brief, in dependency order and never two at a time, because the tasks share
`src/app/mod.rs`, `src/ui/dialog.rs` and `src/app/keys.rs` and a half-written change breaks
another agent's build. Every task commits its own paths (Conventional Commits) and runs
`scripts/check.sh quick` plus its own tests; the session runs `scripts/check.sh full` and
pushes at task boundaries. Pushes happen from the main checkout when it is clean of other
sessions' edits, otherwise from a disposable detached worktree under
`~/.manyfold-worktrees/manycommander/` (btrfs, with the publication gate's environment).

## Tasks

| Task | Paths | Dependencies | Acceptance |
|---|---|---|---|
| **T1 Grouped sources.** `JobSpec::{Copy, Move, Trash, Delete}` take `Vec<Group>` (P2 2.2); `Panel::selection_groups()`; the destination-inside-source check over the union of groups; one `Transfer` across groups. Existing single-directory entry points stay as thin wrappers so the M1 tests keep their shape. | `src/fsops/{job,copy,mv,trash,delete,plan}.rs`, `src/app/{mod,jobs}.rs`, `src/ui/dialog.rs`, `src/panel/mod.rs`, `tests/fs_*.rs` | -- | Every M1 test passes unchanged in behaviour; new tests: a two-group copy and move (standing answers carry across groups), a two-group trash and delete, destination inside a second group's source refused |
| **T2 Copy fidelity.** Sparse copy (P2 9.1) and hard-link preservation (P2 9.2) in the transfer engine, including the move flush rule for hard-linked inodes. `Meta` gains `blocks`; `Sys` gains `seek_data`/`seek_hole`, positioned `copy_file_range`, `pread`/`pwrite`, `ftruncate`, each with a failpoint step name. | `src/fsops/{sys,copy,mv,plan}.rs`, `tests/fs_fidelity.rs` | T1 | A-SP-1, A-HL-1, A-HL-2, A-HL-3; A-FS-1..13 still pass |
| **T3 Forms, links, attributes.** `ui::form` (P2 2.1); the link form and `JobSpec::Link` (P2 8.1) with the "link exists" question; the attributes form, the chmod-syntax parser and `JobSpec::Attr` (P2 8.2) with the directory two-step order. Keys `Alt+L`, `Alt+A`. | `src/ui/form.rs`, `src/fsops/{link,attr}.rs`, `src/fsops/question.rs`, `src/app/{mod,keys}.rs`, `src/ui/dialog.rs`, `tests/fs_link.rs`, `tests/fs_attr.rs` | T1 | A-LK-1, A-LK-2, A-AT-1, A-AT-2; form unit tests (focus order, toggles, submit) |
| **T4 Quick filter and compare.** Panel filter (P2 4) with I-8 mark counting (including the hidden toggle); `Ctrl+F`; `f_type` in the listing's free-space message; compare by date and size in memory, compare by content on a thread (P2 7); `Shift+F2`. | `src/panel/{mod,listing}.rs`, `src/app/{mod,keys,event,runtime}.rs`, `src/compare.rs` (new), `src/ui/{mod,panel}.rs`, `tests/app.rs`, `tests/compare.rs` | T3 (forms) | A-QF-1, A-QF-2, A-CD-1, A-CD-2 |
| **T5 Directories.** Hotlist and frecency stores, their files and merge-on-save, the zoxide import, the directories dialog, `Ctrl+D`, `z` on the command line (P2 3). | `src/dirs.rs` (new), `src/app/{mod,keys,event,runtime,state}.rs`, `src/cmdline/mod.rs`, `src/ui/dialog.rs`, `tests/dirs.rs` | T3 | A-DJ-1..4 |
| **T6 Find.** The parallel search engine (P2 5.3), `Source::Results`, `Place` history, the results tab behaviour and re-stat (P2 5.4, 5.5), the find form, `Alt+F7`. Adds `memchr`. | `src/find.rs` (new), `src/panel/{mod,listing}.rs`, `src/app/*`, `src/ui/*`, `tests/find.rs`, `tests/app.rs` | T1, T3, T4 (filter in results) | A-FD-1..4 |
| **T7 Multi-rename.** Mask engine, preview checks, the rename job with ordering and cycles, undo (P2 6), `Ctrl+M`. Adds `regex`. | `src/rename.rs` (mask engine, new), `src/fsops/rename.rs` (new), `src/app/*`, `src/ui/*`, `tests/rename.rs` | T1, T3 | A-MR-1..6 |
| **T8 Keymap audit, help, docs.** Default bindings of Ghostty and foot (`ghostty +list-keybinds --default`, foot's default `foot.ini`) and the documented defaults of Alacritty, Kitty and Omarchy's Hyprland files checked against P2 10; F1 help; `site/content/docs/*.md` (keys, file operations, a new find-and-rename page); screenshots (`cargo run --example site_screens`); `README.md` feature list. | `src/ui/help.rs`, `site/content/docs/**`, `examples/site_screens.rs`, `README.md` | T2-T7 | `scripts/site.sh check` passes; the audit table is recorded here |
| **T9 Benchmarks.** Harness entries for P-10..P-17 and P-6b; the M1 A-P-1..8 re-run; results in `docs/perf/history.md` and here. | `benches/**`, `scripts/bench/**`, `docs/perf/history.md` | T2-T7 | A-DJ-5, A-QF-3, A-FD-5, A-FD-6, A-MR-7, A-CD-3, A-SP-2, A-HL-4 recorded with measurements |
| **T10 Code review.** An independent adversarial review (grok) of the phase 2 diff, with the M1 and P2 designs as the contract; every confirmed finding fixed with a regression test. | as the findings require | T2-T9 | Findings table recorded here; `scripts/check.sh full` passes |
| **T11 Release.** Version, `CHANGELOG.md`, `.github/workflows/release.yml`, `.publish-allow.tsv` rows, the matching `v` tag, the GitHub release with the binary and its SHA-256. | `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, `.github/workflows/release.yml`, `.publish-allow.tsv` | T10 | The release workflow is green; the release page carries the tarball and checksum; the tarball's binary runs `--version` |

## Local check gate

Unchanged: `scripts/check.sh quick|full|ci|bench` (see the verification-loop overlay).
New integration test files join `cargo test --all-targets` automatically. Tests that need
`unshare -rm`, btrfs or a second filesystem use the existing `skip()` helper, so `full`
fails when the capability is missing.

## Risks and decisions

| Risk | Mitigation |
|---|---|
| The grouped-source change (T1) touches every verb's entry point. | T1 is behaviour-neutral for one group and lands first, with the full M1 suite as its regression net before any feature builds on it. |
| Hard-link preservation interacts with the move flush check (P2 9.2). | A-HL-1 moves hard-linked trees across filesystems and asserts no "source changed"; A-FS-5's sweep runs unchanged. |
| The find engine's thread pool could outlive a cancelled search or leak fds. | Workers hold fds only for directories in flight; A-FD-4 asserts the stop; the pool joins on a responsive filesystem and is abandoned only while blocked in the kernel, as listing threads are. |
| `Ctrl+M` arrives as `Enter` in a terminal without the keyboard protocol. | Documented (P2 10). All four Omarchy terminals support the protocol; the worst case is that Enter's action runs. |
| Content search and compare read user data at full speed and could starve a running job's I/O. | They are user-started and cancellable; the P-1 check during a job (A-P-1) is re-run in T9. |
| zoxide may be missing or slow. | Optional, read-only, 1 s timeout, and never on the UI thread (P2 3.3). Tests disable it through the config. |

## Verification

`scripts/check.sh full` before every push. `scripts/check.sh bench` plus the T9 additions
before the release. Manual: A-FD-7 (stalled FUSE mount), and the owner's in-terminal
confirmation of the new chords (as for the M1 T11 item).

## Review record

(filled in after the grok review)

## Execution record

### Status

| Task | State | Commit | Notes |
|---|---|---|---|
| T1 | todo | -- | |
| T2 | todo | -- | |
| T3 | todo | -- | |
| T4 | todo | -- | |
| T5 | todo | -- | |
| T6 | todo | -- | |
| T7 | todo | -- | |
| T8 | todo | -- | |
| T9 | todo | -- | |
| T10 | todo | -- | |
| T11 | todo | -- | |

Next action: grok review of the design and this plan.

### Decisions made during execution

| # | Task | Decision | Reason |
|---|---|---|---|
