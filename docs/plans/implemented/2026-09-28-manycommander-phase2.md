---
title: manycommander phase 2 implementation
type: plan
status: implemented
owner: manycommander
source: ../../specs/implemented/2026-09-28-manycommander-phase2-design.md
created: 2026-09-28
updated: 2026-10-05
---
# manycommander phase 2 implementation

Build the [phase 2 design](../../specs/implemented/2026-09-28-manycommander-phase2-design.md) (cited as
"P2 <section>", acceptance checks as `A-*`) and release it as the next minor version. The
[M1/M2 design](../../specs/implemented/2026-09-27-manycommander-design.md) stays normative for
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
| T4 | done | 9c93e9b, d001b30 | Quick filter, I-8 selection, compare thread; 170/206 tests pass; probe timings: re-filter 100k 3-6 ms, UI copy 2 ms, compare 100k x 100k 10 ms (benchmarks in T9) |
| T5 | done | 736d9d9, fe8846e | `dirs.rs`, `ui/dirs.rs`, `app/jump.rs`; 20 tests in `tests/dirs.rs` incl. a pty run with a fake zoxide; 195/231 tests pass; probe: dialog open 2.3 ms, worst keystroke 0.5 ms with 5000 entries |
| T6 | done | 3cb0461, 2a06f70, 231c388 | `find.rs`, `app/search.rs`, panel `Source`/`Place`; 22 tests in `tests/find.rs`; 232/268 tests pass; probe (98k-entry tree, warm): name search 5.2 ms vs `fd -uu -j8` 15.6 ms, content 211 MB 10.9 ms vs `rg -uuu` 14.8 ms (benchmarks in T9) |
| T7 | done | d48f1f5, 3e44c5d, a52b01b, 6f09f52 | `rename.rs`, `fsops/rename.rs`, the multi-rename tool; A-MR-1..6 (A-MR-6 on a casefold tmpfs, not skipped), A-MR-4 sweep of 144 runs; 260/301 tests pass; probe: preview of 10k names 1.3-6.5 ms |
| T8 | done | 9665941, 68e494a, 285b0ac, ed68ff1, 4b18b10, 1606987, 098d00f | F1 help, site pages (new `find-and-rename.md`), README; five phase 2 screenshots; ligatures off in all screenshots; `scripts/site.sh check` and `worker` pass |
| T9 | done | fa6761e, b9882b5, 0c47c1f | Every phase 2 target passes (see "Benchmarks (T9)"); M1 re-runs pass except A-P-7 (as in M1); two M1-era findings (refresh on the UI thread, RSS after refreshes) go to T10 |
| T10 | done | fba4a04..2b3db11 | Two grok reviews: 8 findings plus 2 benchmark findings, all fixed with tests or re-measured benchmarks; 268/310 tests pass |
| T11 | done | 0bd0c20, 6053280, the release tag on cfc2f83 | Release workflow green; the release page carries the x86_64 Linux tarball and its SHA-256 (checksum verified, binary runs `--version`); notes are the changelog section. Two CI-only test races fixed before tagging (167b1fa, cfc2f83) |

Next action: none. The owner verification of 2026-10-04 closed the open items (below), and
A-P-7 passes since 2026-10-04 (M1 plan). The plan is archived.

Evidence on the release (owner verification, 2026-10-04, release under test with phase 3 and
type to filter; the runbook's results file holds the logs). Every row passed:

| Checks | Rows | Evidence |
|---|---|---|
| A-DJ (go to a directory) | 8 | Frecency order, the filter, zoxide's entries, bookmark add and remove in `hotlist.toml`, `z` with and without a match, the `dirs.tsv` format |
| A-QF (quick filter, I-8) | 7 | Marks outside the filter are not acted on; copy and trash act on the visible entries only; the filter survives navigation and history |
| A-FD (find) | 11 | Name, content, hidden and case searches; stay on the filesystem; results tab navigation; copy and trash from results; re-stat drops a vanished result; cancel |
| A-MR (multi-rename) | 9 | Masks, counters, case, duplicate and mask errors, a swap cycle, undo with `Ctrl+Z` |
| A-CD (compare) | 3 | Newer, only-here, size and content differences on both sides |
| A-LK (links) | 6 | Relative and absolute symbolic links, the exists question without Overwrite, hard links, the refusals for directories and across filesystems |
| A-AT (attributes) | 5 | Mode in octal and chmod syntax, recursion that skips symbolic links, times |
| A-SP, A-HL (fidelity) | 5 | A 1 GiB sparse file stays sparse on tmpfs; hard links keep their structure through a cross-filesystem move |
| Type to filter (amendments of 2026-09-30) | 26 | Typing filters (case fold beyond ASCII, globs, the fuzzy tier), `Enter` and `Backspace` on the filter line; `Ctrl+E` focus, the terminal cursor and paste in Ghostty and foot |

### Open items

| Item | Owner | Detail |
|---|---|---|
| A-FD-7 | done (session) | `tests/manual.rs` `a_fd_7_search_over_a_stalled_fuse_mount`: first run FAILED case 1 (a stay-on-filesystem search waited on the stalled mount point's `statx`); fixed in 1210278 (`AT_STATX_DONT_SYNC` for entries the search does not enter) and 0d33b6d (results sent before a directory open); now case 1 completes in 11 ms, case 1b (the mount point matches by name) in 12 ms, case 2 blocks in a worker while keys stay at 11-12 ms, `Esc` cancels in 12 ms, the third search is refused, and the workers return on `SIGCONT`. A second run on the release (owner verification, 2026-10-04) passes, after two stale strings in `tests/manual.rs` were updated (1734005) |
| New chords in the terminals | done (2026-10-04) | In Ghostty and foot, keys typed through `wtype` in a nested Hyprland session: `Ctrl+D`, `Ctrl+F`, `Ctrl+M`, `Alt+F7`, `Shift+F2`, `Alt+L`, `Alt+A` each log the expected action; with text on the command line the panel chords leave the line alone (P2 10) |
| A-P-7 | done (2026-10-04) | The owner's decisions are implemented and A-P-7 passes in full; the M1 plan has the numbers |

### Benchmarks (T9)

Conditions and the full rows are in `docs/perf/history.md` (phase 2 section). Release build,
AC power, 8 CPUs, fixtures on btrfs, `/dev/shm` as the second filesystem.

| Check | Result | Measurement | Target |
|---|---|---|---|
| A-P-1 | PASS | p99 key-to-flush 1.34 ms idle, 1.54 ms during a 10 GiB copy to ext4 | <= 16 ms |
| A-P-2 | PASS | first full frame 13.08 ms median | <= 50 ms |
| A-P-3 | PASS | 100k entries 116.2 ms; first batch 0.3 ms | <= 300 / <= 50 ms |
| A-P-4 | PASS | re-sort 14.8 ms, filter 0.3 ms | <= 30 ms |
| A-P-5 | PASS | 60 s idle: no context switches, no CPU ticks | unchanged |
| A-P-6 | PASS | 19.6 MB | <= 40 MB |
| A-P-7 | FAIL (as M1); PASS since 2026-10-04 | 4 GiB 0.45x `cp`; reflink 0.003 s; 50k small files 1.60-1.72x `cp -r`; move 2.61-2.83x `mv`. After the owner decisions of 2026-10-04: 1.31x `cp -r`, 1.14x `mv` (M1 plan) | as M1 |
| A-P-8 | PASS | release test | <= 15 Hz |
| A-QF-3 (P-12) | PASS | re-filter 0.37-2.69 ms; pty p99 1.78 ms | <= 16 ms |
| A-CD-3 (P-13) | PASS | compare thread 8.31 ms; UI copy 3.55 ms | <= 30 / <= 5 ms |
| A-MR-7 (P-14) | PASS | 1.09 / 3.72 / 5.22 ms per keystroke (10k names) | <= 16 ms |
| A-DJ-5 (P-15) | PASS | open 1.75 ms, keystroke 0.40 ms; first frame 13.10 ms with a 5000-entry store | <= 16 ms; P-2 |
| A-FD-5 (P-10) | PASS | 3.83 ms complete, first batch 2.95 ms; 0.29x `fd -uu -j 8` (every name 1.11x) | <= 300 / <= 50 ms; <= 1.5x |
| A-FD-6 (P-11) | PASS | 0.87-0.95x `rg -uuu -F -l` | <= 2x |
| A-SP-2 (P-16) | PASS | 0.004 s; allocation 8 MiB both sides | <= 1 s |
| A-HL-4 (P-17) | PASS | 0.70x the copy without links; 10k pairs kept | no slower |
| P-6b | PASS | 30.8 MB | <= 60 MB |
| P-1 / refresh completion (new) | FAIL -> PASS after T10 (0.84 ms) | 25.3 ms (results tab), 21.5 ms (directory): the UI thread re-sorts a 100k refresh | <= 16 ms |
| RSS after repeated refreshes (new) | FAIL -> PASS after T10 (19.5 / 31.2 MB) | 42.3 MB (two 100k directories), 56-58 MB with a results tab: glibc's dynamic mmap threshold | 40 / 60 MB |

### Code review (T10)

Two independent adversarial reviews (grok) of `338bed6..HEAD`: the file-operation engine, and
everything else. Findings are fixed with regression tests; the outcome column is filled in
when T10 lands.

| # | Severity | Finding | Outcome |
|---|---|---|---|
| A1 | major (reproduced) | A failed `read_dir` in the same-file check turned a same-file move into a case-only rename, leaving `.mc-case-` || fixed fba4a04; regression test failed before |
| A2 | minor (reproduced) | Refused jobs dropped the failures of groups that could not be opened (E-1) || fixed 5690d99; test failed before (trash refusal fixed, not reachable by a test) |
| A3 | major (suspected) | Multi-rename dependency by the first hard link, wrong on case-insensitive directories || fixed b59c0b9; scenarios A and B on a casefold tmpfs failed before |
| B1 | major (reproduced) | A re-stat after cancelling a search lost batches still in flight || fixed 25e173e; test failed before |
| B2 | major (reproduced) | Help and report dialogs panicked below 2 columns (M1 code; NFR-TERM) || fixed ffc9e86, 9232a7f; 12 dialogs at 0-3 columns; failed before |
| B3 | minor (reproduced) | Lower/upper case mapped name and extension together (final sigma) || fixed d707321; failed before |
| B4 | major (suspected) | A search could open an automount trigger that has the parent's `mnt_id` || fixed 7a6f652; decision-function test (a real automount is not testable here) |
| B5 | minor (traced) | A stuck re-stat was not counted against the abandoned-thread cap || fixed 4c6b867; failed before |
| T9-1 | major (measured) | Refresh completion re-sorted 100k entries on the UI thread || fixed 59b39a9; refresh completion 0.84 ms (results), 0.69 ms (directory) |
| T9-2 | major (measured) | RSS grew with repeated refreshes || fixed 37087b9 (`mallopt` in `fsops/sys.rs`); RSS 19.5 MB, 24.8 / 31.2 MB with a results tab |

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
| E-18 | T4 | After a filter change the cursor goes to the first visible entry (or `..` when nothing matches); a non-empty filter moves the cursor off `..`, so Enter opens the first match. | Typing a filter and pressing Enter should open what was found. |
| E-19 | T4 | While the filter line is open, Up/Down/PgUp/PgDn move the panel cursor; any other non-editing key closes the line, keeps the filter and then acts (like `Ctrl+S`). A cancelled or failed load returns with its directory's filter. | The filter narrows a list the user then works in; the panel never changed directory. |
| E-20 | T4 | The footer's "M" counts all listed entries including hidden ones; the filter text takes at most a third of the footer; free space drops first. | M1 footer semantics; the footer must fit at 80 columns. |
| E-21 | T4 | Content compare reports "only here", "differ in size", "differ in content" and "could not be read" (unread pairs are not marked); it has no "newer" counts. | By content, pairs are judged by bytes, not dates. |
| E-22 | T4 | "Newer" and "differ in size" apply only to two regular files; one-sided symlinks and special files are marked; only real directories depend on "include directories". Applying clears every mark in both panels, including invisible and saved ones. Submitting while a panel loads is refused. | A refresh in flight would complete under the same generation and invalidate the result silently. |
| E-23 | T5 | A visit is a completed navigation the user starts (Enter, parent, `cd`, history, the dialog, `z`); startup, first display of a restored tab, ancestor fallbacks, new tabs and refreshes do not count. | zoxide counts directory changes, not shell starts; counting restores would inflate the restored directories. |
| E-24 | T5 | `z` starts the zoxide import like the first `Ctrl+D`, waits ("z: loading...") when the store is still loading, and falls back to the first matching bookmark. A bookmarked directory is listed once, as the bookmark. | An early `z` must not ignore the stores; no duplicate rows. |
| E-25 | T5 | Delete on a zoxide-only row hides it for the session; zoxide is never written. `hotlist.toml` with a relative path, a read error or invalid UTF-8 counts as "does not parse" and is never overwritten. A `dirs.tsv` that cannot be read is never replaced. | No data loss in files the user owns. |
| E-26 | T5 | One persistent store thread serves saves in order; at exit the runtime waits up to 2 s for queued saves before the merge. zoxide runs with `/` as its working directory. `Event::Status` reports helper-thread failures. | Last save wins; `zoxide query` omits its own working directory. |
| E-27 | T5 | Only a failed navigation drops a frecency entry; a refresh that finds the directory deleted does not. In the dialog, Delete acts on the selected row and only Backspace edits the filter. | A deleted current directory is often recreated; Delete must have one meaning. |
| E-28 | T6 | Each search owns its tab; leaving that tab's results (navigation, history, closing) cancels a running search, which keeps its partial results and says "(cancelled)". No re-stat runs while a search runs. | Batches reach only the tab that shows the search; a re-stat would lose late batches. |
| E-29 | T6 | With "stay on this filesystem", a subdirectory is `statx`ed with `AT_NO_AUTOMOUNT` before it is opened, and a different `mnt_id` is never opened. | Opening first would open (and automount) the foreign filesystem; one `statx` per directory. |
| E-30 | T6 | Content search puts matching regular files on the shared stack as work items; a worker flushes its results before a read longer than one chunk and at most every 20 ms. | A large flat directory is read in parallel (P-11), and first results show quickly (P-10). |
| E-31 | T6 | Re-stat drops results that are gone or whose walk meets a symlink or non-directory, and keeps old metadata on other errors (`EACCES`). `Enter` enters only real directories; a symlink result is "go to file". | I-5: symlinks are objects; results are not link-classified. |
| E-32 | T6 | A result is hidden when any component of its relative path starts with `.`; a results tab's hidden toggle starts on when the search included hidden entries. A result path over 65,535 bytes counts as an error. | Otherwise nothing below `.git` would show; the entry arena's limit. |
| E-33 | T7 | A name ending in `.` has no extension; the extension split also applies to directories. A lone `]`, index 0 and a reversed range are mask errors. Replacement syntax: `$1`..`$9`, `${name}`/`${12}`, `$$`; any other `$` is literal; a missing group is an error. | Default masks return every name unchanged; `]]` is the only unambiguous escape; the design's syntax, not the crate's `$name` rules. |
| E-34 | T7 | Title case uppercases the first character of each word (`2nd place -> 2nd Place`) and lowercases the extension; case applies to name and extension separately (design 6.2 amended). | Avoids `Photo.Txt` and `2Nd`. |
| E-35 | T7 | Undo is a separate `JobSpec::UndoRename` over the same engine; the record is consumed when the undo starts, and an undo keeps no record of its own. The record includes entries left under temporary names, so `Ctrl+Z` also rescues them. A directory whose identity changed is skipped whole. | Clean identity checks; I-9. |
| E-36 | T7 | Case-only changes use the engine's `.mc-rename-` temporary name and recovery, not `mv::case_rename`; a self-resolving new name with `nlink > 1` reads the directory once to rule out a second hard link. The job asks no questions: `EEXIST` skips, other errors fail, dependents are skipped. | One temporary-name scheme and one recovery path; a question mid-cycle would hold a member under a temporary name. |
| E-37 | T7 | The tool drops results nested below another selected result; the "exists" preview check uses the whole listing including hidden and filtered entries; Enter is blocked with "nothing to rename" or "a job is running" and the tool stays open. | Hidden entries exist on disk; the user keeps their settings. |
| E-38 | T10 | A multi-rename name's holder is the in-set entry of that inode whose old name equals the new name exactly, else under Unicode lowercase folding; without a match the old case-only test still applies, and another listed name equal under folding counts as a hard link outside the set. | Review A3; covers filesystems that fold more than lowercase (`ss`, normalisation). |
| E-39 | T10 | A results re-stat waits for `Search::finished()` (set after the last batch, before `Done`); batches arriving during a re-stat are kept. `Ctrl+R` while a cancelled search is stopping says so. | Review B1. |
| E-40 | T10 | Find `statx`es every subdirectory before opening it and never opens one with `STATX_ATTR_AUTOMOUNT`, with or without "stay on this filesystem". | Review B4: a search never triggers an automount. |
| E-41 | T10 | A refresh that would abandon a running load while four are abandoned is refused; the load in flight stays. | Review B5, M1 3.1 cap. |
| E-42 | T10 | Refresh listings arrive sorted from the listing thread (`ListingMsg::Listing`, `ListRequest.sort`); the UI thread swaps them in. | T9: 21-25 ms UI stall on 100k refreshes. |
| E-43 | T10 | `mallopt(M_MMAP_THRESHOLD, 128 KiB)` once at startup, in `fsops/sys.rs`, the only module allowed `unsafe`; `libc` without default features, glibc targets only. | T9: RSS grew with repeated refreshes through glibc's dynamic mmap threshold. |
| E-6 | -- | The plan and design name the release "the next minor version", not its number. | The publication gate counts an exact version in docs as a hit and scans every outgoing commit. |
