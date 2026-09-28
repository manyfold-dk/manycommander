---
title: manycommander phase 2 implementation
type: plan
status: in-progress
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
| **T1 Grouped sources.** `Group { root, sub, names }` and `JobSpec::{Copy, Move, Trash, Delete}` over `Vec<Group>` (P2 2.2): the group open walks `sub` with `O_NOFOLLOW`, `valid_component` at the job boundary, the inside-source check over the union of scanned directory identities, one `Transfer` across groups, merge of groups by opened identity for trash. `Panel::selection_groups()`. Existing single-directory entry points stay as thin wrappers so the M1 tests keep their shape. | `src/fsops/{job,copy,mv,trash,delete,plan,walk}.rs`, `src/app/{mod,jobs}.rs`, `src/ui/dialog.rs`, `src/panel/mod.rs`, `tests/fs_groups.rs` | -- | Every M1 test passes; new tests: a two-group copy and move (standing answers carry across groups), a two-group trash and delete, destination inside a second group's scanned source refused, copy into a sibling of a selected file allowed, a `sub` component replaced by a symlink fails the group with "type changed", an invalid component refused before any write |
| **T2 Copy fidelity.** Sparse copy with the `ENXIO`/`EINVAL` rules (P2 9.1); hard-link preservation with in-set name counts from the plan, the regular-file-only map, and the move's deferred unlinks and deferred source-directory removal (P2 9.2). `Meta` gains `blocks`; `Sys` gains `seek_data`/`seek_hole`, positioned `copy_file_range`, `pread`/`pwrite`, `ftruncate`, each with a failpoint step name. | `src/fsops/{sys,copy,mv,plan}.rs`, `tests/fs_fidelity.rs` | T1 | A-SP-1, A-HL-1, A-HL-2, A-HL-3; A-FS-1..13 (including the A-FS-5 sweep) still pass |
| **T3 Forms, links, attributes.** `ui::form` (P2 2.1); the link form and `JobSpec::Link` (P2 8.1) with the "link exists" question; the attributes form, the P2 8.2 mode grammar and `JobSpec::Attr` without a pre-scan, changing through `/proc/self/fd/<n>`, with the intermediate/reopen/final directory order and the symlink time rule. Keys `Alt+L`, `Alt+A`. | `src/ui/form.rs`, `src/fsops/{link,attr}.rs`, `src/fsops/question.rs`, `src/app/{mod,keys}.rs`, `src/ui/dialog.rs`, `tests/fs_link.rs`, `tests/fs_attr.rs` | T1 | A-LK-1, A-LK-2, A-AT-1, A-AT-2; form unit tests (focus order, toggles, submit) |
| **T4 Quick filter and compare.** Panel filter (P2 4) with the I-8 selection predicate, visible-mark counting and cursor fallback (including the hidden toggle); `Ctrl+F` and its line ownership; compare on the compare thread with `fstatfs` there, marks applied by listing generation, by date and size and by content (P2 7); `Shift+F2`. | `src/panel/mod.rs`, `src/app/{mod,keys,event,runtime}.rs`, `src/compare.rs` (new), `src/ui/{mod,panel}.rs`, `tests/app.rs`, `tests/compare.rs` | T3 (forms) | A-QF-1, A-QF-2, A-CD-1, A-CD-2 |
| **T5 Directories.** Hotlist and frecency stores, their files, the zoxide aging factor and decimal ranks, merge-on-save under the `dirs.tsv.lock` flock, the zoxide import, the directories dialog, `Ctrl+D`, `z` on the command line (P2 3). | `src/dirs.rs` (new), `src/app/{mod,keys,event,runtime,state}.rs`, `src/cmdline/mod.rs`, `src/ui/dialog.rs`, `tests/dirs.rs` | T3 | A-DJ-1..4 |
| **T6 Find.** The parallel search engine with the LIFO stack, visited set, `AT_SYMLINK_NOFOLLOW | AT_NO_AUTOMOUNT`, hole skipping and the abandoned-search cap (P2 2.3, 5.2, 5.3); `Source::Results`, `Place` history, the results tab behaviour and the component-walk re-stat (P2 5.4, 5.5); the find form; `Alt+F7`. Adds `memchr`. | `src/find.rs` (new), `src/panel/{mod,listing}.rs`, `src/app/*`, `src/ui/*`, `tests/find.rs`, `tests/app.rs` | T1, T3, T4 (filter in results) | A-FD-1..4 |
| **T7 Multi-rename.** Mask engine with clamped ranges and the defined case modes, preview checks including regex errors, the rename job with identity-based dependencies, blocked-chain skipping, strongly-connected-component cycle breaking, temporary-name recovery, undo from every job outcome (P2 6), `Ctrl+M`, `Ctrl+Z` in the dialog. Adds `regex`. | `src/rename.rs` (mask engine, new), `src/fsops/rename.rs` (new), `src/app/*`, `src/ui/*`, `tests/rename.rs` | T1, T3, T4 (I-8 selection) | A-MR-1..6 |
| **T8 Keymap audit, help, docs.** Record the P2 10 audit table (already done during the design review); F1 help, including the results tab's `Enter` and the hole rule of content search; `site/content/docs/*.md` (keys, file operations, a new find-and-rename page); screenshots (`cargo run --example site_screens`); `README.md` feature list. | `src/ui/help.rs`, `site/content/docs/**`, `examples/site_screens.rs`, `README.md` | T2-T7 | `scripts/site.sh check` passes; the audit table is recorded here |
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

The design and this plan went through an independent adversarial review (grok) on
2026-09-28, before any implementation. 13 required findings and 3 suggestions; all
accepted, one with a change (mask ranges clamp instead of failing). The resolution table
is the design's appendix A. Plan changes from it: T7 depends on T4; T1, T2, T3, T4, T5,
T6 and T7 carry the amended mechanisms; the A-MR-6 casefold test skips with a printed
reason when the kernel refuses the mount (under `MC_REQUIRE_ALL=1` that skip fails, as
every environment skip does).

### Keymap audit (T8)

| Source | Bound there instead | Conflict with P2 10 |
|---|---|---|
| `ghostty +list-keybinds --default` | `alt+1`..`alt+9` tabs, `alt+f4` close, `ctrl+shift+f` search, `ctrl+enter` fullscreen, `ctrl+tab`, `ctrl+page_up/down`, `ctrl+alt+arrows`, font keys | none |
| foot `[key-bindings]` (default `foot.ini`) | `Control+Shift+*` clipboard, search, URL mode; font keys; `Control+f`/`Control+d` only in search and URL modes | none |
| Omarchy Hyprland defaults | `SUPER` chords, `F9`, `Alt+Tab` variants, `Alt+Print`, `Ctrl+Alt+Delete`, media keys | none |
| Alacritty, Kitty (documented defaults; not installed) | `Ctrl+Shift+*` family; Kitty `ctrl+shift+f2/f5/f6` | none (`Ctrl+Shift+F5` avoided) |

## Execution record

### Status

| Task | State | Commit | Notes |
|---|---|---|---|
| T1 | done | 793bc2b, 29fb385 | `fsops/group.rs`; `scan_all` over groups; 11 group tests + 1 app wiring test; 117/139 tests pass (default/failpoints) |
| T2 | done | fd09e07, 6b4f704 | Sparse walk, hard-link map, deferred unlinks and directory removal; A-SP-1, A-HL-1..3 and a failpoint sweep over a sparse and hard-linked move; 122/154 tests pass |
| T3 | done | f20527d, 2eb3b6d, (fix below) | `ui/form.rs`, `fsops/{link,attr}.rs`, `app/forms.rs`; A-LK-1/2, A-AT-1/2 incl. the bind-mount skip; 155/191 tests pass |
| T4 | todo | -- | |
| T5 | todo | -- | |
| T6 | todo | -- | |
| T7 | todo | -- | |
| T8 | todo | -- | |
| T9 | todo | -- | |
| T10 | todo | -- | |
| T11 | todo | -- | |

Next action: T4.

### Decisions made during execution

| # | Task | Decision | Reason |
|---|---|---|---|
| E-1 | T1 | A root that cannot be opened refuses the whole job; a failing `sub` component fails only its group (every name reported failed, not counted in `planned`). | M1 behaviour for one group; the design opens each root once. |
| E-2 | T1 | The one-name rule for a new destination path counts the selected names of all groups. | It follows what the user selected. |
| E-3 | T1 | The trash top-directory cache is keyed by (domain, whether the source directory is in that domain). | With several source directories, one group's "entry is its subvolume root" result must not be reused for another. |
| E-4 | T1 | `Purpose::{Copy, Move}` carry the panel directory next to the groups. | A typed relative destination resolves against the panel, also for a results tab (T6). |
| E-5 | T1 | Groups are merged by opened identity for trash and delete now; rename and link merge with T7 and T3. | Scope of each task. |
| E-7 | T2 | A later name of a hard-linked inode links through an `O_PATH` fd of the first destination, checked by identity, instead of `statx` then `linkat` by name. | Closes the window between the check and the link; a lost last name gives `ENOENT` and the data is copied. |
| E-8 | T2 | At settlement every committed name of the inode is `statx`ed and checked (against its `S0` and its link count at copy time) before any of them is unlinked. | Same guarantee as one `statx` per inode (any replacement changes the inode's nlink and ctime), and a name that is gone is reported as gone. |
| E-9 | T2 | After a data-copy fallback, later names link to the newest data copy; the note counts every in-set name that became a separate copy. | I-7: the report states the structure left behind. |
| E-10 | T2 | Only the first `SEEK_DATA` of a file falls back on `EINVAL`/`EOPNOTSUPP`; a later error raises the error question. The sparse size is the larger of `S0` and the last segment's end. | Before the first write nothing depends on the sparse path; after it, a silent fallback would mix two copy modes in one file. |
| E-11 | T2 | After a failed `syncfs`, deferred names from earlier batches are reported "both kept" and nothing is unlinked. | M1 4.8 step 5.2 applies to every pending source. |
| E-12 | T2 | Overwrite answered onto a name that already holds the linked inode drops the temporary link and counts the name as done. | Renaming one link over another of the same inode is a no-op that would leave the `.mc-partial-` name behind. |
| E-13 | T3 | Up/Down also move the form focus; PgUp/PgDn return to the form's owner. | T7 scrolls its preview with PgUp/PgDn. |
| E-14 | T3 | `t` sets the sticky bit only in a clause that includes `o` (so `u+t` does nothing), and `s` does nothing for `o`, as in chmod(1). | The design follows chmod(1) apart from its two stated differences. |
| E-15 | T3 | A selected entry is never skipped as a mount point; only mount points met while recursing are. | P2 8.2 is about recursion. |
| E-16 | T3 | A directory whose intermediate change or read fails is reported failed with "its entries were not visited" and the mode it was left with; no final mode is applied. `now` is resolved once at submit. With no mode and no time, `Enter` is blocked and the job refuses. | I-7; one time for every entry; no empty job. |
| E-17 | T3 | The attributes form's mode field starts empty; the current mode of a single selected entry is a hint in its label (design 8.2 amended). | The implementer found that a pre-filled field would apply a directory's mode to a whole tree when only a time was typed with "Recursive". |
| E-6 | -- | The plan and design name the release "the next minor version", not its number. | The publication gate counts an exact version in docs as a hit and scans every outgoing commit. |
