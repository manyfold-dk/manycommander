# ADR 0007: Cross-filesystem moves copy and delete per file under group commit

## Status

Accepted -- 2026-09-27. Amended 2026-10-04: batches of 1024 files (first 64, then 256).

## Context

A move within one filesystem is a `rename`. A move across filesystems has to copy and then
delete, and I-1 requires that at every instant, including cancel, error, crash and power
loss, every source file's complete content exists in at least one committed location.

A source may be deleted only after its copy is durable. Durability per file means an
`fsync` of the file, of its directory and of the parent: three flushes per file. Small-file
moves would then be many times slower than `mv`, against the target of within 2x of `mv`
for 50k files of 4 KiB (P-7).

## Decision

Copy and delete per file, with **group commit**:

1. Copy each file into a temporary file and check, before the commit, that the source has
   not changed since it was opened (size, mtime, ctime).
2. Commit it atomically, then append it to the current **batch**. A file whose copy failed
   or was cancelled never joins a batch, so its source is never unlinked. Created
   directories and symlinks join the batch too.
3. Flush the batch when it holds **1024 files or 256 MiB**, when a source directory is
   finished, when the job ends, and on cancel: one `syncfs` on the destination makes every
   write, rename and directory entry of the batch durable together.
4. If `syncfs` fails, keep every source of the batch and end the job.
5. Otherwise `statx` each source again and unlink it only if its identity, size, mtime and
   ctime still match the snapshot; a changed source is kept ("source changed; kept both").
6. Remove source directories in post-order, after the flush that covers their children.

## Rationale

- One `syncfs` per batch gives the same I-1 guarantee as three flushes per file.
- The batch size trades crash residue against speed, measured on the reference machine with
  50k files of 4 KiB moved to an ext4 loop image:

  | Batch | Move time | Against `mv` |
  |---|---|---|
  | 64 files | -- | 2.2x |
  | 256 files | 27.7 s | 0.79x of that run's `mv` (35.0 s); 2.44x in a later run where `mv` took 11.8 s |
  | 1024 files | 15.5 s in the same run as 27.7 s at 256 | 1.14x (15.20 s against 13.31 s) in the acceptance run |

  The `mv` baseline varied from 35.0 s to 11.8 s between runs, so only same-run comparisons
  are meaningful. Control runs showed the copy-path change of
  [ADR 0008](0008-copies-commit-through-o-tmpfile-and-linkat.md) left moves unchanged
  (27.4-29.8 s at 256 files either way); the batch size made the difference.

## Consequences

- After a crash, up to 1024 files (the last unflushed batch) can exist in both places, never
  in neither.
- A cancelled cross-filesystem move leaves a partially moved tree. Every file is in exactly
  one place after the final flush, and running F6 again on the rest merges.
- An entry counts as moved only after its source was unlinked (TDR-1 E-10).
- A residual race remains between the last `statx` and `unlinkat`, because Linux has no
  unlink-by-fd. It is documented.
- On vfat and exfat the change check is best-effort, because their timestamps are coarse.
- Hard-linked sources need deferred unlinks so that one name's unlink does not fail the
  check for its siblings ([TDR-2 9.2](../tdr/0002-navigation-and-bulk-tools.md#92-hard-links)).

## Alternatives considered

| Alternative | Why not |
|---|---|
| `fsync` of file, directory and parent per file | Three flushes per file; small-file moves many times slower than `mv` |
| Move per top-level item, as `mv` does (copy the whole tree, then delete it) | Needs double space for the whole item, and a cancel leaves a partial destination tree to clean up |
| Relaxing I-1 (delete before the data is durable) | Not an option: a crash could lose the only copy |

## References

- [TDR-1 4.8](../tdr/0001-core.md#48-move-and-rename-f6), A-FS-5, A-FS-9, A-P-7
- [Performance history](../perf/history.md)
