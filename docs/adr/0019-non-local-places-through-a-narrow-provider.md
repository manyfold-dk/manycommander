# ADR 0019: Non-local places attach through a narrow provider, not a VFS

## Status

Accepted -- 2026-09-28.

## Context

Archives and SFTP servers had to become places a panel can show and a job can copy from.
The usual design for that is a virtual filesystem: one trait for every place, local disks
included, so that every verb works the same way everywhere.

The local engine does not fit behind such a trait. Its invariants I-1 to I-7
([TDR-1 4.1](../tdr/0001-core.md#41-invariants)) rest on mechanisms a generic interface
hides: directory file descriptors held for the whole job, `O_PATH` opens checked against
`statx` identities, `renameat2` with `RENAME_NOREPLACE`, `copy_file_range`, and `syncfs` group
commit. Its performance targets (P-3 listing, P-7 copy throughput) depend on the same calls.
A trait that an archive or a server can also implement either loses those guarantees or
grows a method per mechanism until it is the local engine with extra steps.

## Decision

Two narrow seams, both for non-local places only.

1. **`Provider`** (`src/provider.rs`) lists a directory, `lstat`s an entry and opens a
   regular file for reading, for an archive index or an SFTP session. A panel's `Source`
   gains archive and remote variants; rows reuse the local `Entry` and name arena, so
   sorting, marks, the quick filter and rendering are unchanged. History places name what to
   reopen, never an open index or session.
2. **`Origin`** (`src/fsops/origin.rs`) is the source side of the copy engine. The
   destination side stays the local engine: temporary file, commit, questions, progress,
   cancel and group commit. `LocalOrigin` is the existing code behind the trait;
   `ArchiveOrigin` and `RemoteOrigin` read members and server files. Extraction and download
   are therefore "a copy with a non-local source" and inherit I-2, I-3 and I-5 on the write
   side.

A non-local file arrives as a `Stream` with a declared size. The engine stops at the first
byte past the declared size and never writes it. A stream cannot be read twice, so "file
exists" at its commit keeps the finished temporary file across the question instead of
reading the source again. Upload is the only other destination engine (`remote::put`), and
its source is a `LocalOrigin`.

## Rationale

- The local engine keeps every mechanism and every test. Moving it behind `Origin` changed
  no behaviour: the full test suite and the A-FS-5 failpoint sweep ran unchanged through
  `LocalOrigin`, the failpoint step counts of a local copy, an overwrite and a hard-linked
  move were identical before and after, and the small-file copy of P-7 took 1.015 s against
  1.016 s.
- Each place keeps what it can actually promise. A server cannot identify an inode, so its
  origin keeps every source in a move (R-4); an archive is read-only, so its provider
  refuses every write.
- Non-local identities are synthetic and never equal a local one, so the same-file and
  destination-inside-source checks stay local-only without special cases.

## Consequences

- Verbs differ by place, and the refusals are explicit: the table of
  [TDR-3 2.4](../tdr/0003-archives-quick-view-and-sftp.md#24-verbs-by-place) lists every
  combination, and A-SRC-3 checks each refusal.
- There is no copy between two servers and no copy on a server; the user copies through a
  local directory.
- A tar member is read in one pass, so Retry after bytes were read fails with "read in one
  pass".
- An origin whose reader borrows its own state (a zip member's decoder) lends its bytes
  through `Origin::lend` instead of returning an owned reader, which avoids `unsafe` and
  self-referential types.

## Alternatives considered

| Alternative | Why not |
|---|---|
| A VFS trait over local I/O as well | Hides directory fds, `O_PATH`, `renameat2` and identities, on which I-1 to I-7 rest, and slows P-3 and P-7 |
| FUSE mounts (`sshfs`, `rclone mount`, archive mounts) | A dead link leaves syscalls stuck, there is no inotify, and `RENAME_NOREPLACE` and identities depend on the FUSE implementation |
| A separate extract command per format | Duplicates the questions, progress, cancel and commit of the local engine, and with them the chance to get one wrong |

## References

- [TDR-3 2.1 to 2.4](../tdr/0003-archives-quick-view-and-sftp.md#2-architecture)
- [ADR 0004](0004-own-file-operation-engine-on-directory-fds.md): the local engine
- [ADR 0020](0020-archives-indexed-and-extracted-by-the-local-engine.md),
  [ADR 0024](0024-upload-commit-and-best-effort-cross-host-moves.md)
