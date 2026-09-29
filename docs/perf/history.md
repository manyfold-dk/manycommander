# Performance history

`scripts/bench/run.sh` appends one section per run: the conditions, the commit, and the
result of each performance check (design 11.4, targets in 13.1; from phase 2 also the
phase 2 design's 11 and 12; from phase 3 the phase 3 design's 7.1 and 8). Reference
conditions: release build, local NVMe, warm page cache, the development laptop on AC power.

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

## 2026-09-29 13:15

Conditions: AC on, power profile performance, governor powersave, fixtures on btrfs, 8 CPUs, 1-minute load 1.25 at the start, fd 10.5, rg 15.2, hyperfine 1.20, bsdtar 3.8, zstd 1.5, xz 5.8, gzip 1.14, bzip2 1.0, OpenSSH 10.5, ImageMagick 7.1. Commit f98b799.

| Check | Result | Measurement |
|---|---|---|
| A-P-1 | PASS | p99 key-to-flush idle 1.01 ms, during a 10 GiB copy to ext4 1.68 ms (<= 16); job still running after the samples: true |
| A-P-2 | FAIL | first full frame median 104.03 ms, max 105.05 ms over 20 starts (<= 50) |
| A-P-3 | PASS | 100k listed and sorted 110.7 ms (<= 300), first batch 0.3 ms (<= 50) |
| A-P-4 | PASS | re-sort 14.8 ms, filter 0.3 ms (<= 30) |
| A-P-5 | PASS | 60 s idle: voluntary context switches 19 -> 19, CPU ticks 9 -> 9 (unchanged) |
| A-P-6 | PASS | RSS 21.0 MB with both panels on 100k entries (<= 40) |
| A-P-7 | FAIL | 4 GiB to ext4: 11.49 s vs cp 17.64 s (x0.651, <= 1.10); 50k x 4 KiB: 2.19 s vs cp -r 1.29 s (x1.702, <= 1.5); 4 GiB btrfs reflink copy 0.003 s (< 1); move 50k x 4 KiB to ext4: 28.54 s vs mv 11.40 s (x2.504, <= 2) |
| A-P-8 | PASS | a 96 MiB copy tmpfs to btrfs: no two progress updates closer than 1/15 s (fs_copy a_p_8_progress_is_capped_at_15_hz, release: 1 passed; 0 failed) |
| A-QF-3 | PASS | re-filter of 100k entries per keystroke: substring 1.19 ms, first character (all match) 0.34 ms, glob 2.59 ms (<= 16); on a pty, Ctrl+F and 100 keystrokes: key-to-flush p99 1.99 ms, max 2.37 ms (<= 16) |
| A-CD-3 | PASS | two 100k-entry listings by date and size: compare thread 8.28 ms (<= 30), UI share (copy of both panels' visible entries) 2.90 ms (<= 5) |
| A-MR-7 | PASS | one keystroke with 10k names (edit, preview, checks): name mask 1.07 ms, counter + date + search + title case 3.70 ms, regex replace (compiled again) 5.08 ms (<= 16) |
| A-DJ-5 | FAIL | 5000 frecency entries: rank and fill (Ctrl+D) 1.76 ms, re-filter per keystroke 0.400 ms (<= 16); on a pty, Ctrl+D 3.90 ms, 48 keystrokes p99 1.60 ms (<= 16); first full frame with that dirs.tsv median 104.06 ms, max 106.31 ms over 20 starts (<= 50; without it 104.03 ms) |
| A-FD-5 | PASS | 100k-entry tree, name `report` (989 results): complete 3.85 ms (<= 300), first batch 0.71 ms (<= 50); process 5.0 ms vs `fd -uu -j 8 -F` 16.2 ms (x0.311, <= 1.5). Every name (100000 results, each statx'ed for its columns): complete 20.13 ms, first batch 0.18 ms; process 21.3 ms vs `fd -uu` 18.5 ms (x1.149) |
| A-FD-6 | PASS | 1 GiB of text in 10k files, a needle in 100 of them: process 44.2 ms vs `rg -uuu -F -l -j 8` 48.2 ms (x0.917, <= 2); in-process 42.72 ms. Case-folded: 54.1 ms vs `rg -uuu -F -l -i` 63.0 ms (x0.859) |
| A-SP-2 | PASS | 16 GiB file with 8 MiB of data, btrfs to tmpfs: 0.003 s median of 5 (<= 1); allocation 8388608 bytes at the source, 8388608 at the destination (<= source + 1 MiB); same size: yes |
| A-HL-4 | PASS | 10k hard-link pairs (20k names of 4 KiB) btrfs to ext4: 0.53 s vs the same 20k names as separate files 0.68 s (x0.775, <= 1), median of 5; destination: 20000 names, 20000 with 2 links on 10000 inodes |
| P-6b | PASS | RSS 32.3 MB with both panels on 100k entries and a hidden tab of 100k results (the find of every name of the 100k-entry tree); 25.8 MB with the results tab on screen (<= 60) |
| P-1/Ctrl+R | PASS | Ctrl+R in a tab of 100k results beside a 100k-entry panel: key-to-flush p50 2.95 ms, max 3.20 ms over 10 (<= 16); the UI thread's copy of the results for the re-stat, in-process, 1.81 ms |
| P-1/refresh | PASS | UI thread when a refresh of 100k entries completes (sorted on the listing thread; swapped in, filtered, marks and cursor kept), in-process: a results tab's re-stat 0.76 ms, a directory's re-listing (M1) 0.66 ms (<= 16: a key that arrives meanwhile waits) |
| RSS/Ctrl+R | PASS | after 10 Ctrl+R, 1.5 s apart: both panels on 100k entries 21.2 MB (A-P-6 limit 40); the 100k-result tab on screen 26.0 MB, then both panels on 100k entries with it hidden 32.4 MB (P-6b limit 60) |
| P-18 | PASS | 10k-entry zip (62302903 bytes, Info-ZIP, deflate) listed completely in 16.04 ms (median of 20, max 17.61 ms; <= 50) |
| P-19 | FAIL | medians of 5; first rows <= 50 ms: pkg10k.tar.zst: first rows 2.74 ms, full scan 192.90 ms, decompress-only 165.93 ms (x1.163), `zstd -dc` 194.57 ms (<= 1.2); pkg10k.tar.gz: first rows 3.45 ms, full scan 309.92 ms, decompress-only 274.48 ms (x1.129), `gzip -dc` 526.62 ms (<= 1.2); pkg10k.tar.xz: first rows 33.74 ms, full scan 2095.51 ms, decompress-only 2059.97 ms (x1.017), `xz -dc` 332.36 ms; pkg10k.tar.bz2: first rows 55.10 ms, full scan 4736.30 ms, decompress-only 4746.11 ms (x0.998), `bzip2 -dc` 4944.32 ms; pkg10k.tar: first rows 0.19 ms, full scan 15.77 ms, decompress-only 14.02 ms (x1.125); pkg10k.7z: first rows 20.49 ms, full scan 20.50 ms; pkg92.tar.zst: first rows 0.55 ms, full scan 315.26 ms, decompress-only 314.10 ms (x1.004), `zstd -dc` 347.12 ms; pkg92.tar.gz: first rows 0.09 ms, full scan 754.28 ms, decompress-only 759.71 ms (x0.993), `gzip -dc` 1252.13 ms; pkg92.tar.xz: first rows 6.73 ms, full scan 8048.15 ms, decompress-only 7993.76 ms (x1.007), `xz -dc` 1228.32 ms; pkg92.tar.bz2: first rows 65.19 ms, full scan 13686.03 ms, decompress-only 13434.08 ms (x1.019), `bzip2 -dc` 14236.85 ms |
| P-20 | PASS | Esc 150 ms into the scan: key-to-flush of the frame that shows the directory again (<= 100): pkg92.tar.xz 0.40 ms (scan thread gone after 1.0 ms); pkg92.tar.bz2 0.41 ms (scan thread gone after 0.7 ms); pkg92.tar.gz 0.51 ms (scan thread gone after 0.4 ms); pkg92.tar.zst 0.49 ms (scan thread gone after 1.0 ms); pkg10k.tar.xz 0.68 ms (scan thread gone after 1.1 ms); pkg10k.tar.bz2 0.42 ms (scan thread gone after 0.8 ms) |
| P-21 | PASS | a .tar.zst whose big/ holds 10k entries: entering and leaving big/ 50 times, key-to-flush p99 0.71 ms (entered 50/50); leaving the archive and entering it again 50 times, p99 0.94 ms (<= 16); archive scans in the log: 1 (no rescan) |
| P-22 | PASS | the 10k-entry package to btrfs, process medians of 5 (hyperfine; <= 1.5x): pkg10k.zip: 879.1 ms vs `bsdtar -xf` 1101.7 ms (x0.798), of which the scan 20.7 ms and the job 0.855 s; 10000 entries written; pkg10k.tar.zst: 999.2 ms vs `bsdtar -xf` 683.3 ms (x1.462), of which the scan 216.0 ms and the job 0.812 s; 10000 entries written |
| P-23 | FAIL | 12 MP JPEGs (4000x3000, about 3.2 MB, camera-like) in a 100x50-cell pane at 10x20-pixel cells, from the request after the 100 ms debounce to the image's last byte at the terminal: kitty: first previews median 235.4 ms, max 235.7 ms (<= 150), cache hits max 0.9 ms (<= 16); preview thread decode 106.2 ms, scale and encode 110.3 ms; halfblocks: first previews median 132.0 ms, max 134.6 ms (<= 150), cache hits max 5.9 ms (<= 16); preview thread decode 107.7 ms, scale and encode 17.4 ms; sixel: first previews median 234.2 ms, max 236.5 ms (<= 200), cache hits max 3.3 ms (<= 16); preview thread decode 104.1 ms, scale and encode 123.4 ms |
| P-24 | PASS | 200 JPEGs of 0.75 to 12 MP, bursts of 10 keys at 30 keys/s with rests of 300 ms, kitty graphics: key-to-flush p99 1.47 ms, max 1.53 ms (<= 16); 19 transmits of about 3013914 bytes, the transmitting frame median 7.2 ms, max 12.1 ms (<= 50); decoded on: list-preview only |
| P-25 | PASS | first full frame on two 1k-entry directories with the probe, 20 starts: ghostty-like (kitty): median 4.20 ms, max 5.32 ms (<= 50), probe 0.17 ms; foot-like (sixel): median 3.96 ms, max 5.48 ms (<= 50), probe 0.17 ms; a silent terminal (halfblocks): median 103.62 ms, max 105.53 ms (<= 150), probe 100.12 ms |
| P-26 | PASS | 1 GiB through ssh to sshd -i (a ProxyCommand), medians of 5 alternating runs with the connect: download 2.206 s vs `sftp` get 2.618 s (x0.842), upload 2.235 s vs put 2.463 s (x0.908) (<= 1.2). On pipes to sftp-server vs `sftp -D`: download x0.795 (1764 MiB/s), upload x0.787 (1737 MiB/s) |
| P-27 | PASS | 10k entries with a 30 ms round trip (the latency helper, measured 30.61 ms): first rows 62.09 ms, complete 3231.9 ms = x1.025 of 103 round trips (<= 1.1), 101 batches (one per READDIR reply with names), 10000 rows (`.` and `..` dropped), 105 requests; without added latency: pipes first rows 0.63 ms, complete 47.9 ms; ssh first rows 0.70 ms, complete 57.1 ms |
| SFTP/trees | PASS | 1000 files of 4 KiB in 10 directories through ssh: download 2.683 s vs `sftp get -rp` 0.296 s (x9.080), upload 0.431 s vs `put -rp` 0.294 s (x1.466); 200 files at a 30 ms round trip: download x1.482 (37.234 s, 1418 requests), upload x1.493 (37.056 s, 1208 requests) |
| P-5b | PASS | 60 s idle with an SFTP session open (sshd -i), a cached zip index and a remote JPEG in the quick view (kitty): voluntary context switches 484 -> 484, CPU ticks 28 -> 28 (unchanged; the ssh child not counted); session open after: true |
| P-6c | PASS | both panels on 100k-entry directories plus the cached index of a 100k-entry .tar.zst, quick view off, no preview prepared: 28.3 MB; with the right panel on the same directory through SFTP (sshd -i): 28.2 MB (<= 60) |

## 2026-09-29 13:22

Conditions: AC on, power profile performance, governor powersave, fixtures on btrfs, 8 CPUs, 1-minute load 0.73 at the start, fd 10.5, rg 15.2, hyperfine 1.20, bsdtar 3.8, zstd 1.5, xz 5.8, gzip 1.14, bzip2 1.0, OpenSSH 10.5, ImageMagick 7.1. Commit b706244.

| Check | Result | Measurement |
|---|---|---|
| A-P-2 | PASS | first full frame median 12.96 ms, max 14.01 ms over 20 starts (<= 50) |
| A-DJ-5 | PASS | 5000 frecency entries: rank and fill (Ctrl+D) 1.76 ms, re-filter per keystroke 0.398 ms (<= 16); on a pty, Ctrl+D 4.12 ms, 48 keystrokes p99 1.88 ms (<= 16); first full frame with that dirs.tsv median 13.07 ms, max 13.85 ms over 20 starts (<= 50; without it 12.96 ms) |
| SFTP/trees | PASS | 1000 files of 4 KiB in 10 directories through ssh, medians of 5 alternating runs: download 0.401 s vs `sftp get -rp` 0.282 s (x1.423), upload 0.403 s vs `put -rp` 0.294 s (x1.371); 200 files at a 30 ms round trip: download x1.479 (37.090 s, 1418 requests), upload x1.494 (37.100 s, 1208 requests) |

Phase 3 (plan T10): the first run with the phase 3 checks (13:15, commit f98b799, after the
SFTP tuning in 0444cda), and the M1 and phase 2 checks on the phase 3 build. The rows
A-P-2, A-DJ-5 and SFTP/trees were re-measured after the harness fix b706244 (13:22) and
replace the first run's, which were: A-P-2 FAIL, 104.03 ms; A-DJ-5 FAIL, its first frame
104.06 ms; SFTP/trees, one run, download 2.683 s (x9.080). A-P-2 and A-DJ-5 were a harness
defect: the benchmark ran inside tmux, and the M1 driver's pty inherited `TMUX` and
`TERM_PROGRAM=tmux`, so the startup probe (P3 4.2, E-13) waited its 100 ms deadline for a
graphics reply that the pty never sends. The single tree download ran right after P-26 had
written and deleted 1 GiB on the compressing btrfs; five alternating runs replace it.

The phase 3 fixtures (`benches/p3/fixtures.rs`) are removed after a run:

- the research's package shapes, generated with fixed seeds: 10,000 entries (800
  directories up to six levels, 9,200 files with log-normal sizes around 4 KiB, two thirds
  text and one third binary-like), 155 MB as tar; and 92 entries (10 directories, 82 files
  around 1 MiB with a heavy tail, mostly binary-like), 345 MB as tar. Archived at the
  tools' default levels (`xz` with `-T0`): the 10k package as zip 62 MB, tar.gz 61 MB,
  tar.zst 59 MB, tar.xz 37 MB, tar.bz2 52 MB and 7z (bsdtar) 35 MB; the 92-entry one as
  tar.gz and tar.bz2 205 MB, tar.zst 219 MB and tar.xz 169 MB;
- a `.tar.zst` whose `big/` holds 10,000 files (P-21); the M1 100k-entry directory as
  `.tar.zst` (P-6c);
- 12 MP JPEGs from ImageMagick's plasma fractal at quality 90, 3.2 to 3.3 MB, a phone
  photo's entropy (P-23); 200 JPEGs of 0.75 to 12 MP (P-24);
- for SFTP, 1 GiB of random bytes, 10,000 empty files, and trees of 1000 and 200 files of
  4 KiB.

Methods:

- Archives are scanned in-process with a fresh index cache per scan. The decompress-only run
  of P-19 builds the scan's decoder as `archive::tar::decoder` does (the window caps
  included), over the same positioned reader, and reads it in the 32 KiB pieces the tar
  crate skips data in. P-22 is the whole process (the scan, then the job) under hyperfine
  against `bsdtar -xf`, into an empty btrfs directory after `sync`.
- The pty checks run the binary with an environment of its own (`HOME`, XDG directories,
  a no-op `xdg-open`). Its terminal answers the probe as Ghostty (kitty graphics, a 10x20
  cell, the keyboard protocol), as foot (sixel), as a truecolor terminal without graphics,
  or not at all, and skips image payloads as a fast terminal reads them. An earlier version
  that parsed every byte of the payloads measured P-24's transmitting frame at 31 to 51 ms
  instead of 7 to 12 ms. "On screen" is the wall time at which the terminal side has read
  the image's last bytes; the end of the debounce is the log's `preview request` line.
  The quick view's image area is 100x50 cells (a 204x55 terminal); a 4:3 photo fills 100x38.
- SFTP runs `ssh -F` to `sshd -i` in a `ProxyCommand` (scratch keys, `BatchMode`, no port,
  no host) and `sftp-server` on pipes; `sftp` runs over the same transport (`-F` with the
  same host, or `-D`). The latency helper (`delay-pipe` in the driver) delays each
  direction by 15 ms, as `sftp -D`'s server command and as a `ProxyCommand`. Ours is the
  copy engine with a fresh session per run, connect and close included, as `sftp`'s time
  includes its connect. The in-process checks call `tune_allocator` first, as the binary's
  `main` does; T5 to T7 measured without it.
- Nothing dumped core: `coredumpctl` shows no entry after the runs started.

P-19's ratio target (P3 appendix A, row 31): 1.2x of the same process's decompress-only
run, for the 10k package as `.tar.zst` and `.tar.gz`. First measurement (while `xz -T0` and
`bzip2` built the 92-entry fixture on the other cores): x1.088 and x1.086. Under quiet
conditions, over five series of five scans: x1.07 to x1.17 (zst) and x1.10 to x1.13 (gz).
Reason: the scan's own work (tar headers, pax records, the index, the rows) is 12 to 35 ms
for 10,000 entries, a fixed cost per entry on top of the stream; 1.2x holds it with room
for run-to-run noise, and a doubling of the per-entry cost (x1.3 and more) fails it. The
92-entry package, with few headers, scans at x0.99 to x1.02.

SFTP tuning (P-26; `src/remote/session.rs`, 0444cda). The upload's 0.90 to 1.33x of
`sftp -D` put in T7 had two causes, and the download a third that T5 to T7 did not see:

- The bench machine's btrfs compresses (`compress=zstd:3`) and `vm.dirty_bytes` is
  256 MiB: a transfer of 512 MiB or more is throttled to writeback, for both clients, and
  the same setting varied by 1.4x between runs. The sweeps used 192 MiB after `sync`, five
  runs alternating ours and `sftp`; P-26 keeps the design's 1 GiB, five alternating runs.
- OpenSSH's `limits@openssh.com` announces 255 KiB, which T5 adopted. It is not a whole
  number of pages: the server's `pwrite`s of an upload start off page boundaries. Uploads
  (192 MiB over pipes, 7 runs) took 0.16 to 0.17 s with 130048- and 261120-byte requests,
  0.10 to 0.125 s with 131072, 126976, 65536 and 61440.
- With the binary's fixed 128 KiB mmap threshold, every `DATA` reply frame of 128 KiB or
  more is a fresh `mmap` and `munmap`: downloads took 0.169 s with 261120-byte requests,
  0.115 s with 131072, 0.088 to 0.091 s with 126976, 122880 or 65536.
- Requests are now the server's limit capped at 124 KiB (a whole number of pages, a reply
  frame below the threshold), 128 in flight: 15.5 MiB, as `sftp` keeps with 64 of 255 KiB.
  64 requests of 64 KiB (8 MiB in flight) lose at a 30 ms round trip. The 1 MiB pipes
  stay: 256 KiB measured the same (download x0.81 against x0.80 over pipes), the 64 KiB
  default slower (x0.93).

| Transport, size | Before: 64 x 255 KiB, download / upload | After: 128 x 124 KiB |
|---|---|---|
| pipes, 192 MiB (vs `sftp -D`) | x1.446 / x1.148 | x0.796 / x0.962 |
| ssh to sshd -i, 192 MiB | x0.889 / x1.008 | x0.739 / x0.824 |
| pipes at 30 ms, 128 MiB | x0.790 / x1.152 | x0.744 / x1.088 |
| ssh to sshd -i, 1 GiB (P-26) | x0.861 / x0.974 (without `tune_allocator`) | x0.842 / x0.908 |
| pipes, 1 GiB | x0.914 / x1.110 (without `tune_allocator`) | x0.795 / x0.787 |

Findings:

- P-19 (FAIL, `.tar.bz2` only): first rows of the 10k package 55.1 ms and of the 92-entry
  one 65.2 ms (<= 50); every other format passes (`.tar.xz` 33.7 and 6.7 ms). Cause: the
  scan's first batch waits up to 30 ms to gather rows (`Sink::maybe_flush`,
  `src/archive/mod.rs`), and only a member of 256 KiB or more flushes it earlier. A bzip2
  block (900 KB of tar) takes about 27 ms to decode, so when the 30 ms mark falls inside the
  second block, the first rows wait for that block. Proposed fix: send the first batch as
  soon as the watched directory has a row, without the 30 ms wait (later batches keep their
  100 ms), and re-run P-19. Beside it, no target: the pure-Rust xz decoder reads the 10k
  package at about 75 MB/s; `xz -dc` takes a sixth of that time, with threads over the blocks
  that `xz -T0` wrote.
- P-23 (FAIL, kitty and sixel): a camera-like 12 MP JPEG is on screen 235 ms (kitty,
  <= 150) and 234 ms (sixel, <= 200) after the debounce; halfblocks 132 ms and every cache
  hit (at most 5.9 ms) pass. Cause: the preview thread, not the UI: the JPEG decode takes
  104 to 108 ms, the scale to 1000x750 (`thumbnail_exact`) about 75 ms, the kitty or sixel
  encode 30 to 45 ms; the transmitting frame 7 to 12 ms. T4's 123 to 132 ms came from a
  smooth synthetic gradient; T4 recorded 236 ms for a high-entropy image, which is what a
  camera JPEG costs. Proposed fix: decode JPEGs at a reduced DCT scale (1/2 or 1/4, which
  the pane allows; the `image` crate's decoder has no scaled decode, so this is a decoder
  choice for the owner) or scale with a SIMD resizer, then re-run P-23.
- A-P-7 (FAIL) misses the same two parts as in M1 and phase 2 (50k x 4 KiB x1.70; the move
  x2.50); the owner decision recorded in the M1 plan is still open.
- SFTP/trees (no target): 1000 files of 4 KiB through ssh, download x1.42 and upload x1.37
  of `sftp get -rp` and `put -rp`; 200 files at 30 ms, x1.48 and x1.49. Cause: round trips
  per file. A download makes 6 (`LSTAT` for R-3, `OPEN`, `FSTAT`, `READ`, the `READ` that
  meets EOF, the final `FSTAT` of E-22; `CLOSE` is not waited for), `sftp` 4; an upload 6
  (`OPEN`, `WRITE`, `FSETSTAT`, `CLOSE`, then `hardlink` and `REMOVE` for R-1), `put -p` 4.
  Pipelining across files was not done: the copy engine takes one entry at a time
  (`Origin::lend`), so it would restructure `RemoteOrigin` and the engine rather than tune
  a constant. Proposed: end a download at the planned size and send the final `FSTAT` with
  the last `READ` (it checks the size and mtime anyway), and send `FSETSTAT` and `CLOSE`
  behind an upload's last `WRITE` (OpenSSH answers in order): 4 round trips each.
