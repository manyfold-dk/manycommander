# Release comparison: M1 to 0.6

Question (owner, 2026-10-05): has performance changed since the start, and has any recent
change made the application bigger or slower?

Answer: no release has made manycommander measurably slower. Every release from the M1
milestone to 0.6, measured under the same conditions, lies within run-to-run noise on each
interactive check, and every check stays far inside its target. The binary grew in two steps
that the features explain (phase 2 and phase 3). From 0.3 to 0.6 it grew 6.8 percent, most
of it in 0.5 (the Markdown parser `pulldown-cmark`); 0.6 added 2 KiB. The largest single item
is not recent: `fast_image_resize` (phase 3), about 1.26 MB of symbols; see
[the downscale experiment](2026-10-05-downscale-without-fast-image-resize.md).

## Method

[`history.md`](history.md) rows come from different days, loads and harness versions, so
they cannot answer "slower than before" on their own. This comparison measures every release
under the same conditions instead, in two sessions on 2026-10-05: M1 to 0.5 from 20:25, and
0.5 again beside 0.6 from 21:21.

- Each release is built from `git archive` of its tag on btrfs, with the release profile of
  that commit. The first session's binaries were rebuilt by `cargo bench --no-run`, the
  second session's are built as shipped (see "Size"). Releases are named by major.minor and their commit (the publication gate keeps
  exact version numbers out of `docs/`):

  | Name | Commit | Note |
  |---|---|---|
  | M1 | `585de07` | the last M1 benchmark commit; no release |
  | 0.2 | `cfc2f83` | phase 2 |
  | 0.3 | `9c2303b` | phase 3 |
  | 0.4 | `29bb44f` | type to filter, fuzzy tier |
  | 0.4 patch | `f5fe33b` | kitty PNG transmit, Alt chords |
  | 0.5 | `a62be39` | unnamed temporary file, 1024-file move batches, Markdown quick view, F3 hand-off |
  | 0.6 | `fc783af` | `gio open`, a directory argument starts on the left panel |

- One benchmark driver drives every binary of a session; it takes the binary's path. The
  first session used 0.5's `benches/driver.rs`, the second 0.6's, which also puts a no-op
  `gio` first on `PATH` (0.6 opens files with `gio open`). Its Ctrl+E before `cd` is harmless on binaries older than 0.4,
  where Ctrl+E moves to the end of the command line. M1 has no quick filter.
- Three rounds; each round runs every binary in turn (first frame, navigation, filter, RSS),
  so slow drift of the machine reaches every binary alike. The idle check runs all binaries
  at once: it counts each process's own wake-ups.
- A-P-3 and A-P-4 run each release's own `listing` benchmark (its own library code) on the
  same fixture, twice more in alternating order for the last three releases.
- Conditions: AC on, power profile performance, 8 CPUs, fixtures on btrfs, a 1-minute load
  of 0.45 to 2.10 during the rounds. One `listing` run of 0.5, taken while another session's
  tests and the screensaver started (load 4.2, every metric of that run about 10 percent up),
  is left out.

## Size

The shipped binaries, from the release tarballs on GitHub (stripped by the release
workflow); M1 had no release and is a local `cargo build --release --locked`, stripped.

| Release | Stripped binary | `.text` | `.rodata` | Tarball | Packages in `Cargo.lock` | Lines in `src/` |
|---|---|---|---|---|---|---|
| M1 | 2044 KiB | 1500 KiB | 203 KiB | n/a | 262 | 11,152 |
| 0.2 | 3839 KiB | 2771 KiB | 283 KiB | 1.70 MB | 262 | 21,979 |
| 0.3 | 7981 KiB | 6322 KiB | 440 KiB | 3.15 MB | 321 | 45,960 |
| 0.4 | 8031 KiB | 6365 KiB | 441 KiB | 3.18 MB | 321 | 47,484 |
| 0.4 patch | 8102 KiB | 6425 KiB | 448 KiB | 3.17 MB | 321 | 47,554 |
| 0.5 | 8521 KiB | 6607 KiB | 478 KiB | 3.32 MB | 323 | 48,610 |
| 0.6 | 8523 KiB | 6608 KiB | 478 KiB | 3.32 MB | 323 | 48,678 |

From 0.3 to 0.6 the binary grew 6.8 percent; 0.5 accounts for 419 KiB of it, 0.6 for 2 KiB.

The build flavour matters for sizes. `cargo bench --no-run` rebuilds the package's binary
into `target/release/` with the dev-dependencies' features unified in, and they switch
`regex` (and `regex-syntax`, `regex-automata`) from `unicode-case` and `unicode-perl` to the
whole `unicode` set: 212 KiB more `.rodata` (the Unicode property tables), 24 KiB more
relocations and 16 KiB more relocated data. A first pass of this comparison measured such
binaries and overstated every release from 0.2 by 260 to 303 KiB (stripped); the table above
uses the shipped binaries, and a local `cargo build --release --locked` matches them to
within 1 KiB. `scripts/bench/run.sh`
runs `cargo bench --no-run` after its release build, so every row of
[`history.md`](history.md) measured that flavour too; for timing it makes no measurable
difference (see "Speed and memory").

Where the growth comes from (symbol sizes from `nm -S`, grouped by crate):

| Step | Largest additions |
|---|---|
| 0.2 to 0.3 (+4.0 MiB stripped) | `fast_image_resize` +1265 KiB, manycommander +793 KiB, `image_webp` +183 KiB, `zune_jpeg` +167 KiB, `image` +159 KiB, `lzma_rust2` +124 KiB |
| 0.4 patch to 0.5 (+419 KiB stripped) | `pulldown_cmark` +107 KiB, manycommander +40 KiB, `unicase` +9 KiB; `.text` +182 KiB and `.rodata` +30 KiB in all; the other sections were not broken down |

`fast_image_resize` is larger than any other dependency and close to manycommander's own
code (1.6 MB) although the quick view uses one operation of it: a box-filter downscale of
four 8-bit pixel layouts. `scale` (`src/preview/gfx.rs`) chooses the pixel type at run time,
so every pixel type, algorithm and SIMD variant of the crate is linked. Its `only_u8x4`
feature would drop the grey and RGB layouts the preview needs.

## Speed and memory

Medians of the three rounds (A-P-3 and A-P-4: each run). M1 to 0.5 come from the first
session, on binaries rebuilt by `cargo bench --no-run` (see "Size"); the 0.6 column comes
from the second, with the binaries as shipped (`cargo build --release --locked`), three
rounds of 0.5 and 0.6 alternating. 0.6's A-P-3 and A-P-4 come from the `run.sh` row of the
same evening in [`history.md`](history.md).

| Check | M1 | 0.2 | 0.3 | 0.4 | 0.4 patch | 0.5 | 0.6 | Target |
|---|---|---|---|---|---|---|---|---|
| A-P-2 first full frame, ms | 13.06 | 12.90 | 12.88 | 12.88 | 12.80 | 12.78 | 12.68 | 50 |
| A-P-1 key-to-flush on 100k entries, p50 ms | 0.66 | 0.66 | 0.67 | 0.68 | 0.68 | 0.69 | 0.67 | |
| A-P-1 key-to-flush, p99 ms | 0.98 | 1.17 | 1.07 | 0.96 | 0.98 | 0.89 | 0.89 | 16 |
| A-QF-3 filter per keystroke on a pty, p50 ms | n/a | 1.45 | 1.42 | 1.53 | 1.54 | 1.53 | 1.53 | 16 |
| A-P-5 wake-ups in 60 s idle | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| A-P-6 RSS, both panels on 100k entries, MB | 18.5 | 19.5 | 22.4 | 22.4 | 22.3 | 22.8 | 22.8 | 40 |
| RSS after 10 Ctrl+R, MB | 38.5 | 19.7 | 22.6 | 22.6 | 22.5 | 23.1 | 23.0 | 40 |
| A-P-3 100k listed and sorted, ms | 105 | 121 | 119, 116, 114 | 118 | 112, 112, 115 | 107, 116 | 113 | 300 |
| A-P-4 re-sort, ms | 15.0 | 15.3 | 16.5, 16.7, 16.2 | 16.6 | 14.3, 14.8, 16.3 | 14.5, 15.0 | 15.5 | 30 |

The second session's 0.5, built as shipped, measured the same as the first session's: first
frame 12.76 ms, key-to-flush p50 0.69 and p99 0.92 ms, filter p50 1.51 ms, no idle wake-ups.
Its RSS was 22.0 and 22.2 MB against the first session's 22.8 and 23.1 MB, and 0.6 measured
22.8 and 23.0 MB beside it; the RSS point below explains the difference.

Reading:

- A-P-3 and A-P-4: one criterion run varies by about 7 percent between runs of the same
  binary, more than any difference between releases. The same check in `history.md`
  measured 106.5 to 116.2 ms across all runs since M1.
- A-P-1 and A-QF-3: round 2 widened the p99 tails of three releases alike (1.87 to 2.50 ms
  key-to-flush, one filter keystroke of 7.90 ms); the medians stay tight in all rounds. The
  table's medians do not depend on that round.
- The one cost a recent change adds: from 0.4 a filter keystroke takes about 0.1 ms more
  (1.42 to 1.53 ms), the case-folding, typo-tolerant matcher (`d7d7571`, `a1e4449`). The
  target is 16 ms.
- RSS: the step at 0.3 (+3 MB) is phase 3's code and libraries. Every later difference is
  file-backed. `RssAnon` (the heap) is 14.6 to 14.7 MB in 0.3, the 0.4 patch, 0.5 and 0.6
  alike, while `RssFile` (the binary's code pages and the libraries) moves between 7.1 and
  8.3 MB from build to build: 7.7 to 7.8 MB for 0.3 and the 0.4 patch, 8.3 MB for the first
  session's 0.5, 7.1 to 7.4 MB for the second session's 0.5, 8.2 MB for 0.6, 7.2 to 7.4 MB
  for 0.6 with the downscale replacement. On a page fault the kernel also maps the cached
  pages around it (fault-around), so the layout of the code changes how many pages a run
  touches. A-P-6 therefore varies
  by about 1 MB with the build, not with the heap; those pages are clean and shared, and
  the kernel can reclaim them.
- M1's RSS after refreshes (38.5 MB) is glibc's dynamic mmap threshold, fixed in phase 2 by
  `tune_allocator` (37087b9).
- The Markdown quick view (0.5) is the only new per-frame work, and no check covers it. By
  inspection the card wraps at most a screen of lines per frame (`markdown::lines` stops at
  the area's height), not the whole 64 KiB head; the parse runs once on the preview thread.

## Faster since M1

From [`history.md`](history.md); the last column is the 0.6 run of 2026-10-05:

| What | Then | 0.6 |
|---|---|---|
| A-P-7 move of 50k x 4 KiB to ext4, against `mv` | x2.44 (M1) | x1.25 (1024-file batches) |
| A-P-7 copy of 50k x 4 KiB, against `cp -r` | x1.71 (M1) | x1.30 (unnamed temporary file) |
| P-23 first preview of a 12 MP JPEG, kitty, median | 235 ms (first phase 3 run) | 111 to 113 ms on a quiet machine (PNG transmit, SIMD downscale) |
| SFTP 1000 small files through ssh, download and upload against `sftp` | x1.42 and x1.37 | x0.86 and x1.02 (three round trips per file) |
| RSS after 10 Ctrl+R | 38.5 MB (M1) | 21.8 MB (phase 2 allocator setting) |

The 0.6 run passed every check but P-23, whose kitty maximum (155.2 ms against 150) came
from the state of the machine late in the run: alone on a quiet machine the same binary
passed with 129.9 ms, and the shipped 0.4 patch release and 0.6 measured the same tail (the
notes of that run in [`history.md`](history.md)).

## Not covered

- Not repeated across releases: A-P-1 during a 10 GiB copy, A-P-7, and the phase 2 and
  phase 3 checks. The 0.6 run of `scripts/bench/run.sh` in [`history.md`](history.md) covers
  them for 0.6 only.
- The Markdown quick view's frame cost has no check (see above).
- Side finding, not a performance matter: the tests build with the dev-dependencies, so
  they run `regex` with the whole `unicode` feature set, while the shipped binary has only
  `unicode-case` and `unicode-perl`. A pattern that uses another Unicode class (`\p{Greek}`,
  for example) compiles under `cargo test` and fails in the shipped binary; no test covers
  the shipped feature set.

## Repeating it

Build each tag from `git archive` under a directory on btrfs with `cargo build --release
--locked` and copy `release/manycommander` out before any `cargo bench` runs in that target
directory (it rebuilds the binary with the dev-dependencies' features); build the `listing`
bench executable after that. Then drive every binary with the newest `driver`:
`driver first-frame BIN k1a k1b 20`, `navigate BIN SRC many 400`, `filter BIN SRC many 100`,
`rss BIN many many 10`, `idle BIN k1a 60`, with `SRC` the M1 fixtures under `target/bench/src`.
Run the `listing` executables with `--bench --noplot`, `CRITERION_HOME` per run and
`MC_BENCH_DIR` at the fixtures. Wait for a 1-minute load below 1.5 before each phase, and
check `ps` when a round's tails widen: other sessions' test runs share the machine.
