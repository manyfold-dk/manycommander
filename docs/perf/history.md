# Performance history

`scripts/bench/run.sh` appends one section per run: the conditions, the commit, and the
result of each performance check (design 11.4, targets in 13.1; from phase 2 also the
phase 2 design's 11 and 12). Reference conditions: release build, local NVMe, warm page
cache, the development laptop on AC power.

## 2026-09-27 20:54

Conditions: AC on, power profile performance, governor powersave, fixtures on btrfs. Commit 4765105 (uncommitted changes in src).

| Check | Result | Measurement |
|---|---|---|
| A-P-1 | FAIL | p99 key-to-flush idle 1.10 ms, during a 10 GiB copy to ext4  ms (<= 16); job still running after the samples:  |

Harness defect, not a measurement of manycommander: the copy session's quick search matched
a fixture marker file instead of the 100k-entry directory. Fixed in the driver (`cd` on the
command line) and the fixtures (markers in their own directory).

## 2026-09-27 20:57

Conditions: AC on, power profile performance, governor powersave, fixtures on btrfs. Commit 4765105 (uncommitted changes in src).

| Check | Result | Measurement |
|---|---|---|
| A-P-1 | PASS | p99 key-to-flush idle 0.97 ms, during a 10 GiB copy to ext4 1.07 ms (<= 16); job still running after the samples: true |

## 2026-09-27 21:10

Conditions: AC on, power profile performance, governor powersave, fixtures on btrfs. Commit 4765105 (uncommitted changes in src).

| Check | Result | Measurement |
|---|---|---|
| A-P-1 | PASS | p99 key-to-flush idle 1.00 ms, during a 10 GiB copy to ext4 1.30 ms (<= 16); job still running after the samples: true |
| A-P-2 | PASS | first full frame median 22.87 ms, max 29.06 ms over 20 starts (<= 50) |
| A-P-3 | PASS | 100k listed and sorted 106.5 ms (<= 300), first batch 0.3 ms (<= 50) |
| A-P-4 | PASS | re-sort 14.4 ms, filter 0.1 ms (<= 30) |
| A-P-5 | PASS | 60 s idle: voluntary context switches 13 -> 13, CPU ticks 8 -> 8 (unchanged) |
| A-P-6 | PASS | RSS 19.7 MB with both panels on 100k entries (<= 40) |
| A-P-7 | FAIL | 4 GiB to ext4: 7.69 s vs cp 15.77 s (x0.487, <= 1.10); 50k x 4 KiB: 2.44 s vs cp -r 1.34 s (x1.821, <= 1.5); 4 GiB btrfs reflink copy 0.004 s (< 1); move 50k x 4 KiB to ext4: 87.35 s vs mv 34.96 s (x2.498, <= 2) |

## 2026-09-27 21:47

Conditions: AC on, power profile performance, governor powersave, fixtures on btrfs. Commit 585de07.

| Check | Result | Measurement |
|---|---|---|
| A-P-1 | PASS | p99 key-to-flush idle 1.22 ms, during a 10 GiB copy to ext4 1.32 ms (<= 16); job still running after the samples: true |
| A-P-2 | PASS | first full frame median 13.15 ms, max 16.39 ms over 20 starts (<= 50) |
| A-P-3 | PASS | 100k listed and sorted 110.9 ms (<= 300), first batch 0.3 ms (<= 50) |
| A-P-4 | PASS | re-sort 15.2 ms, filter 0.1 ms (<= 30) |
| A-P-5 | PASS | 60 s idle: voluntary context switches 12 -> 12, CPU ticks 8 -> 8 (unchanged) |
| A-P-6 | PASS | RSS 18.5 MB with both panels on 100k entries (<= 40) |
| A-P-7 | FAIL | 4 GiB to ext4: 7.78 s vs cp 16.13 s (x0.482, <= 1.10); 50k x 4 KiB: 2.28 s vs cp -r 1.33 s (x1.714, <= 1.5); 4 GiB btrfs reflink copy 0.004 s (< 1); move 50k x 4 KiB to ext4: 28.79 s vs mv 11.79 s (x2.442, <= 2) |

## 2026-09-27 21:52

Conditions: AC on, power profile performance, governor powersave, fixtures on btrfs, 5 tabs per panel. Commit 585de07.

| Check | Result | Measurement |
|---|---|---|
| A-P-1 | PASS | p99 key-to-flush idle 1.38 ms, during a 10 GiB copy to ext4 1.62 ms (<= 16); job still running after the samples: true |
| A-P-6 | PASS | RSS 21.1 MB with both panels on 100k entries (<= 40) |

## 2026-09-27 22:21

Conditions: AC on, power profile performance, governor powersave, fixtures on btrfs. Commit 376f3d7 (uncommitted changes in src).

| Check | Result | Measurement |
|---|---|---|
| A-P-1 | PASS | p99 key-to-flush idle 1.07 ms, during a 10 GiB copy to ext4 1.45 ms (<= 16); job still running after the samples: true |
| A-P-5 | PASS | 60 s idle: voluntary context switches 11 -> 11, CPU ticks 8 -> 8 (unchanged) |

## 2026-09-29 02:09

Conditions: AC on, power profile performance, governor powersave, fixtures on btrfs, 8 CPUs, 1-minute load 0.34 at the start, fd 10.5, rg 15.2, hyperfine 1.20. Commit fa6761e.

| Check | Result | Measurement |
|---|---|---|
| A-P-1 | PASS | p99 key-to-flush idle 1.34 ms, during a 10 GiB copy to ext4 1.54 ms (<= 16); job still running after the samples: true |
| A-P-2 | PASS | first full frame median 13.08 ms, max 13.85 ms over 20 starts (<= 50) |
| A-P-3 | PASS | 100k listed and sorted 113.4 ms (<= 300), first batch 0.3 ms (<= 50) |
| A-P-4 | PASS | re-sort 14.7 ms, filter 0.3 ms (<= 30) |
| A-P-5 | PASS | 60 s idle: voluntary context switches 18 -> 18, CPU ticks 8 -> 8 (unchanged) |
| A-P-6 | PASS | RSS 19.4 MB with both panels on 100k entries (<= 40) |
| A-P-7 | FAIL | 4 GiB to ext4: 9.23 s vs cp 20.48 s (x0.451, <= 1.10); 50k x 4 KiB: 2.20 s vs cp -r 1.28 s (x1.718, <= 1.5); 4 GiB btrfs reflink copy 0.003 s (< 1); move 50k x 4 KiB to ext4: 32.57 s vs mv 11.53 s (x2.825, <= 2) |
| A-P-8 | PASS | a 96 MiB copy tmpfs to btrfs: no two progress updates closer than 1/15 s (fs_copy a_p_8_progress_is_capped_at_15_hz, release: 1 passed; 0 failed) |
| A-QF-3 | PASS | re-filter of 100k entries per keystroke: substring 1.33 ms, first character (all match) 0.37 ms, glob 2.69 ms (<= 16); on a pty, Ctrl+F and 100 keystrokes: key-to-flush p99 1.78 ms, max 2.33 ms (<= 16) |
| A-CD-3 | PASS | two 100k-entry listings by date and size: compare thread 8.31 ms (<= 30), UI share (copy of both panels' visible entries) 3.55 ms (<= 5) |
| A-MR-7 | PASS | one keystroke with 10k names (edit, preview, checks): name mask 1.09 ms, counter + date + search + title case 3.72 ms, regex replace (compiled again) 5.22 ms (<= 16) |
| A-DJ-5 | PASS | 5000 frecency entries: rank and fill (Ctrl+D) 1.75 ms, re-filter per keystroke 0.399 ms (<= 16); on a pty, Ctrl+D 3.34 ms, 48 keystrokes p99 1.75 ms (<= 16); first full frame with that dirs.tsv median 13.10 ms, max 21.04 ms over 20 starts (<= 50; without it 13.08 ms) |
| A-FD-5 | PASS | 100k-entry tree, name `report` (989 results): complete 3.83 ms (<= 300), first batch 2.95 ms (<= 50); process 5.0 ms vs `fd -uu -j 8 -F` 17.2 ms (x0.289, <= 1.5). Every name (100000 results, each statx'ed for its columns): complete 24.18 ms, first batch 0.57 ms; process 20.5 ms vs `fd -uu` 18.5 ms (x1.108) |
| A-FD-6 | PASS | 1 GiB of text in 10k files, a needle in 100 of them: process 45.9 ms vs `rg -uuu -F -l -j 8` 50.9 ms (x0.902, <= 2); in-process 252.54 ms. Case-folded: 55.5 ms vs `rg -uuu -F -l -i` 64.2 ms (x0.865) |
| A-SP-2 | PASS | 16 GiB file with 8 MiB of data, btrfs to tmpfs: 0.004 s median of 5 (<= 1); allocation 8388608 bytes at the source, 8388608 at the destination (<= source + 1 MiB); same size: yes |
| A-HL-4 | PASS | 10k hard-link pairs (20k names of 4 KiB) btrfs to ext4: 0.52 s vs the same 20k names as separate files 0.74 s (x0.701, <= 1), median of 5; destination: 20000 names, 20000 with 2 links on 10000 inodes |
| P-6b | PASS | RSS 30.2 MB with both panels on 100k entries and a hidden tab of 100k results (the find of every name of the 100k-entry tree); 23.8 MB with the results tab on screen (<= 60) |
| P-1/Ctrl+R | PASS | Ctrl+R in a tab of 100k results beside a 100k-entry panel: key-to-flush p50 2.67 ms, max 3.41 ms over 10 (<= 16); the UI thread's copy of the results for the re-stat, in-process, 1.59 ms |
| P-1/refresh | PASS | UI thread when a refresh of 100k entries completes (sorted on the listing thread; swapped in, filtered, marks and cursor kept), in-process: a results tab's re-stat 0.84 ms, a directory's re-listing (M1) 0.69 ms (<= 16: a key that arrives meanwhile waits) |
| RSS/Ctrl+R | PASS | after 10 Ctrl+R, 1.5 s apart: both panels on 100k entries 19.5 MB (A-P-6 limit 40); the 100k-result tab on screen 24.8 MB, then both panels on 100k entries with it hidden 31.2 MB (P-6b limit 60) |

Phase 2 (plan T9): the first run with the phase 2 checks, and the M1 checks on the phase 2
build. The rows A-P-3, A-P-4, A-P-6, P-6b, P-1/Ctrl+R, P-1/refresh and RSS/Ctrl+R were
re-measured after the T10 fixes of the two findings below, at commit 59b39a9, under the same
conditions with a 1-minute load of 2.32 at the start, and replace the first measurements,
which were: A-P-3 116.2 ms; A-P-4 re-sort 14.8 ms; A-P-6 19.6 MB; P-6b 30.8 MB and 24.5 MB;
P-1/Ctrl+R p50 1.75 ms, max 2.40 ms, copy 1.85 ms; P-1/refresh FAIL, 25.26 ms and 21.52 ms
(the UI thread built the collation keys and sorted); RSS/Ctrl+R FAIL, 42.3 MB, 56.5 MB and
58.3 MB. The phase 2 fixtures (`benches/common/mod.rs`) are removed after a run:

- a tree of exactly 100k entries: 1110 directories three levels deep and 98,890 empty
  files, 1 % of them named `report_*`;
- 1 GiB of text in 10k files; 100 of the files hold the needle;
- two 100k-entry directories that differ in two thirds of their mtimes, in every 7th size
  and in 5 % of their names;
- 10k hard-link pairs of 4 KiB, beside the same 20k names as separate files;
- a 16 GiB file with eight 1 MiB data segments;
- a 5000-entry `dirs.tsv`.

fd and rg run with `-j 8`, the find engine's worker count. The process comparisons are
hyperfine medians after three warm-up runs. The in-process numbers are criterion medians,
or the driver's median of 20 (P-10) or 5 (P-11) runs.

- A-FD-6: the in-process 252.54 ms is a harness defect, not a measurement of the engine.
  A-P-1 and A-P-7 had just copied 14 GiB, and one warm-up run left part of the text tree
  out of the page cache. Warm, the same command measured 47.6 to 49.2 ms in three repeats.
  Fixed in b9882b5; the re-run below measures 47.0 ms.
- A-P-7 misses the same two parts as in M1; the owner's decision recorded in the M1 plan
  is still open. The move of 50k x 4 KiB took 32.6 s here and 30.1 s in the re-run below,
  against 28.8 s in M1's final run with the same `mv` baseline (11.5 s). Unverified
  whether phase 2 adds to it or the run-to-run variance explains it (M1 measured 27.7 s
  and 28.8 s).
- P-1/refresh (new check; finding): when a refresh of 100k entries completes, the UI
  thread swaps the new listing in, builds the collation keys of every entry and sorts it.
  That takes 25.3 ms for a results tab and 21.5 ms for a directory. The directory case is
  M1 code; A-P-1 never refreshes while it samples. A key that arrives during that work
  waits. Proposed fix: build the collation keys and the sort permutation of a refresh
  listing on the listing or re-stat thread (the request carries the sort order), so the
  UI thread only swaps; it sorts again only when the sort order changed meanwhile. Fixed
  that way in T10 (59b39a9): the UI thread's part is now 0.84 ms and 0.69 ms.
- RSS/Ctrl+R (new check; finding): each refresh of a 100k-entry listing leaves memory
  resident until RSS levels off. Over repeated sessions with 10 Ctrl+R, both panels on 100k
  entries ended at 29 to 44 MB (A-P-6 limit 40; 19.6 MB before the refreshes). With the
  100k-result tab they ended at 40 to 61 MB (P-6b limit 60; 30.8 MB before). The cause is
  glibc's dynamic mmap threshold: freeing a multi-MiB listing that was mmapped raises the
  threshold, so later listings come from the heap and stay resident after they are freed.
  With `GLIBC_TUNABLES=glibc.malloc.mmap_threshold=131072` the same sessions measured
  19.5 MB after 10 refreshes, and 30.4 MB for the P-6b state after 10 Ctrl+R; the Ctrl+R
  frame went from 1.5 to 3.1 ms p50, because the copy gets fresh pages.
  `MALLOC_ARENA_MAX=1` changed nothing. Proposed fix: set the threshold at startup
  (`mallopt(M_MMAP_THRESHOLD, 128 KiB)` in `src/fsops/sys.rs`, the one module allowed
  `unsafe`), then re-run A-P-3, A-P-4, P-1/Ctrl+R and this check. Fixed that way in T10
  (37087b9): after 10 Ctrl+R, 19.5 MB, 24.8 MB and 31.2 MB. As with the tunable, the
  Ctrl+R frame costs more (p50 2.67 ms against 1.75 ms), because the copy gets fresh pages;
  A-P-3 and A-P-4 did not change.

## 2026-09-29 02:16

Conditions: AC on, power profile performance, governor powersave, fixtures on btrfs, 8 CPUs, 1-minute load 1.36 at the start, fd 10.5, rg 15.2, hyperfine 1.20. Commit b9882b5.

| Check | Result | Measurement |
|---|---|---|
| A-P-7 | FAIL | 4 GiB to ext4: 10.44 s vs cp 19.09 s (x0.547, <= 1.10); 50k x 4 KiB: 2.22 s vs cp -r 1.39 s (x1.600, <= 1.5); 4 GiB btrfs reflink copy 0.003 s (< 1); move 50k x 4 KiB to ext4: 30.12 s vs mv 11.54 s (x2.611, <= 2) |
| A-FD-6 | PASS | 1 GiB of text in 10k files, a needle in 100 of them: process 48.5 ms vs `rg -uuu -F -l -j 8` 50.8 ms (x0.953, <= 2); in-process 46.99 ms. Case-folded: 58.2 ms vs `rg -uuu -F -l -i` 67.8 ms (x0.860) |

A-FD-6 re-run after the harness fix (b9882b5), and a second A-P-7 sample for the move.
