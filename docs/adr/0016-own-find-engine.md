# ADR 0016: An own find engine on directory file descriptors

## Status

Accepted -- 2026-09-28.

## Context

Find (`Alt+F7`) searches a tree by name and, optionally, by content, and streams the results
into a results tab ([ADR 0015](0015-search-results-are-a-panel-tab.md)). It must keep the
guarantees the rest of manycommander keeps:

- **I-5:** no symlink is followed, in any path component.
- **I-10:** a content search opens regular files only, never a FIFO or a device, which
  could block a reader forever or have side effects.
- **No stall on a mount it does not enter:** a stopped FUSE daemon or a dead network mount
  inside the tree must not hold the search, and must not hold the UI.
- **Cancel within 100 ms** on a responsive filesystem, partial results kept.
- **Speed:** P-10 (name search of a 100k-entry tree within 1.5x of `fd -uu`) and P-11
  (content search within 2x of `rg -uuu -F -l`).

Rust has good walkers and Linux has fast search tools. The question was whether to build on
one of them.

## Decision

Build the engine in manycommander (`src/find.rs`) on the same primitives as the file
operations:

- A pool of `min(8, available_parallelism)` threads shares a last-in-first-out stack of
  `(parent fd, name, relative path)` items. Each directory is opened with
  `openat(O_DIRECTORY | O_NOFOLLOW)` relative to its parent's fd and identified by `statx`
  of the new fd; a visited set of `(st_dev, st_ino)` stops bind-mount loops.
- Before an open, a `statx` with `AT_NO_AUTOMOUNT | AT_STATX_DONT_SYNC` decides it: only a
  directory, never an automount trigger, and with "Stay on this filesystem" only one on the
  root's mount. A search therefore never mounts anything and never waits on a mount it does
  not enter.
- `getdents64` in large batches; `d_type` decides directory versus other without a `statx`.
- Content: the TDR-1 4.3 `O_PATH` open sequence (regular files only), a 256 KiB buffer per
  worker with chunks overlapping by `needle.len() - 1`, `memchr`'s `memmem` finder, and
  `SEEK_DATA`/`SEEK_HOLE` to skip the holes of sparse files. Name-matching files are work
  items on the shared stack, so a large flat directory is read in parallel.

## Rationale

- `walkdir` and `ignore` resolve paths rather than walking directory fds with `O_NOFOLLOW`
  identity checks, and `ignore` applies `.gitignore` rules, which a file manager must not
  apply by default.
- `fd` and `rg` as child processes return text paths: a newline in a name breaks parsing
  without `-0`, cancellation and fd use would be out of manycommander's hands, and I-10 could
  not be guaranteed.
- The engine is small, and the measurements show no speed cost. On the reference machine,
  a 100k-entry tree: name search complete in 3.83 ms, first batch on screen in 2.95 ms,
  0.29x the time of `fd -uu -j 8` (1.11x when every name matches); 1 GiB of text in 10k files:
  0.87x to 0.95x the time of `rg -uuu -F -l`.
- The last-in-first-out stack bounds open directory fds to about `workers x depth` instead
  of the tree size.

## Consequences

- manycommander owns a parallel walker and its tests: `.` and `..` never matched or
  descended, symlinked directories and symlink loops not followed, bind-mount cycles visited
  once, FIFOs never opened, matches across chunk boundaries found (A-FD-1 to A-FD-4).
- A manual check (A-FD-7) runs a search over a stopped FUSE mount; it found that a `statx` of
  the mount point waited on the stopped daemon once its attribute cache expired, which
  `AT_STATX_DONT_SYNC` fixed.
- There is no binary detection: binary files are searched as text, and a needle that needs
  hole bytes is not found in a sparse file's holes. The F1 help says so.
- At most two abandoned searches may exist; a worker blocked in the kernel cannot be
  cancelled, only abandoned (TDR-2 2.3).

## Alternatives considered

| Alternative | Why not |
|---|---|
| `walkdir` / `ignore` crates | Path-resolving traversal; `.gitignore` semantics by default |
| `fd` or `rg` as external processes | Text output, foreign cancellation and fd handling, no I-10 guarantee |
| A single-threaded walk | Misses P-10 and P-11 on large trees and on fast NVMe storage |

## References

- [TDR-2 5 Find files and the results tab](../tdr/0002-navigation-and-bulk-tools.md#5-find-files-and-the-results-tab)
- [TDR-2 11 Non-functional requirements](../tdr/0002-navigation-and-bulk-tools.md#11-non-functional-requirements)
- [Benchmark history](../perf/history.md)
- [ADR 0015: Search results are a panel tab](0015-search-results-are-a-panel-tab.md)
