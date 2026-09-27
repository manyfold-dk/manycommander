# Performance history

`scripts/bench/run.sh` appends one section per run: the conditions, the commit, and the
result of each performance check (design 11.4, targets in 13.1). Reference conditions:
release build, local NVMe, warm page cache, the development laptop on AC power.

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
