# ADR 0008: Copies commit through an unnamed O_TMPFILE file and linkat

## Status

Accepted -- 2026-10-04.

## Context

The first copy engine wrote each file to a named temporary file,
`.<name>.mc-partial-<suffix>`, and committed it with `renameat2(..., RENAME_NOREPLACE)`. That
upholds I-2 (no partial destination file) and I-3 (no overwrite without a decision), but it
missed the small-file target of A-P-7: 50k files of 4 KiB within 1.5x of `cp -r`.

| Attempt | Against `cp -r` |
|---|---|
| First run | 1.82x |
| No destination lookup below 1 MiB, no repeated `copy_file_range` per filesystem pair, no `getrandom` per temporary name | 1.70x |
| Direct writes under the final name (breaks I-2; measured only) | 1.16x |

The rename itself cost 0.73 s per 50k files on ext4. A crash could also leave
`.mc-partial-*` files behind, a documented but visible residue.

## Decision

For a regular file from a local source, copied without an Overwrite answer and outside
direct-write mode:

- Open an unnamed temporary file in the destination directory:
  `openat(dir, ".", O_TMPFILE | O_WRONLY | O_CLOEXEC, 0600)`.
- Commit by giving it a name: `linkat(AT_FDCWD, "/proc/self/fd/<fd>", dir, final,
  AT_SYMLINK_FOLLOW)`. `linkat` never replaces a name, so `EEXIST` raises "file exists".
- A destination filesystem that refuses `O_TMPFILE` (`EOPNOTSUPP`, `EISDIR`, `EINVAL`: vfat,
  exfat, most FUSE filesystems) is remembered for the job and gets the named temporary file.
  A `linkat` that fails otherwise (`ENOENT` without `/proc`, `EPERM` or `EOPNOTSUPP` without
  hard links) does the same, and the file is copied again.
- After an Overwrite answer, the named temporary file and `renameat` stay: only a rename
  replaces a name atomically. Symlinks, archive members, hard links made by the copy, and
  stream sources also keep the named temporary file.

## Rationale

- I-2 holds more strongly than before: the file has no name until it is complete, so no
  partial file is ever visible, even briefly under a temporary name.
- I-3 holds: `linkat` fails atomically on an existing name.
- A failure, a cancel or a crash leaves nothing behind. The kernel frees an unnamed inode
  when its last fd closes, and the filesystem frees it at the next mount after a crash.
- `/proc/self/fd` with `AT_SYMLINK_FOLLOW` needs no capability; `AT_EMPTY_PATH` would need
  `CAP_DAC_READ_SEARCH`.
- Measured on the reference machine: 1.36x `cp -r` in the trial, and 1.31x (1.67 s against
  1.27 s) in the acceptance run of 2026-10-04, inside the 1.5x target.

## Consequences

- The `.mc-partial-*` crash residue is limited to the paths that keep a named temporary
  file. The documentation of that residue changed accordingly.
- The fast path depends on `/proc` being mounted; without it the named temporary file is
  used.
- Two commit mechanisms exist side by side, and the failpoint sweep (A-FS-5) covers both.
- Cross-filesystem moves use the same path for their copy step; their speed is governed by
  the batch size of [ADR 0007](0007-cross-filesystem-move-with-group-commit.md).

## Alternatives considered

| Alternative | Why not |
|---|---|
| Keep the named temporary file and accept the miss | The target would have to change to the measured value |
| Write directly under the final name | 1.16x, but breaks I-2 everywhere |
| `linkat` with `AT_EMPTY_PATH` | Needs `CAP_DAC_READ_SEARCH` |

## References

- [TDR-1 4.7](../tdr/0001-core.md#47-copy-f5), A-FS-5, A-FS-8, A-FS-12, A-P-7
- TDR-1 E-4, E-28; [performance history](../perf/history.md)
