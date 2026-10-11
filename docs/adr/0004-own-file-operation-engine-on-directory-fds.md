# ADR 0004: An own file-operation engine on directory file descriptors

## Status

Accepted -- 2026-09-27.

## Context

A file manager's copy and move are the operations a user trusts with data. manycommander
states its guarantees as invariants ([TDR-1 4.1](../tdr/0001-core.md#41-invariants)): no lost
data on a move (I-1), no partial destination file (I-2), no overwrite without an explicit
decision (I-3), never destroying the source through the destination (I-4), symlinks treated
as objects and never followed (I-5), no silent escalation from trash to delete (I-6), and an
honest report of every entry (I-7).

The ready-made building blocks break these:

- `std::fs::copy` and `fs_extra` open the destination with `O_TRUNC`. A failure leaves a
  truncated file under the final name (I-2), and when source and destination are the same
  inode, the truncate destroys the source (I-4). Neither offers cancellation or progress
  between chunks.
- Path-based APIs resolve the whole path on every call. A directory component replaced by a
  symlink during a long job silently redirects the rest of the job (I-5).

## Decision

manycommander has its own engine in `src/fsops`:

- A job opens its source and destination directories once, as directory fds. Every later
  operation is relative to a directory fd: `openat`, `statx`, `renameat2`, `unlinkat`,
  `mkdirat`, `symlinkat`, `linkat`. No step re-resolves an absolute path.
- Descending opens a child with `O_DIRECTORY | O_NOFOLLOW` and compares its identity with
  the identity the job's scan recorded. A regular file is opened `O_PATH | O_NOFOLLOW`, checked with `fstat`, and only
  then reopened for reading through `/proc/self/fd`, so a FIFO or device is never opened for
  I/O.
- Writes go to a temporary file that is committed atomically, by `renameat2` with
  `RENAME_NOREPLACE` or by `linkat` ([TDR-1 4.7](../tdr/0001-core.md#47-copy-f5),
  [ADR 0008](0008-copies-commit-through-o-tmpfile-and-linkat.md)).
- System calls go through `rustix`'s safe wrappers. Every module except `fsops/sys.rs` has
  `#![forbid(unsafe_code)]`; `sys.rs` has one `unsafe` call, glibc's `mallopt` at startup.
- Failpoints, a cargo feature off in release builds, can inject an error or a cancel at
  every named step, and tests assert that each injected step was reached
  ([ADR 0010](0010-local-first-verification-gate.md)).

## Rationale

- Each invariant maps to a mechanism that can be tested: I-2 to the atomic commit, I-3 to
  `RENAME_NOREPLACE` and `linkat` failing with `EEXIST`, I-4 to identity checks before any
  write, I-5 to `O_NOFOLLOW` on every component.
- Directory fds make the job immune to renames and symlink swaps of its ancestors, which a
  path-based engine cannot be.
- `rustix` covers every call the design needs, so the engine needs no `unsafe` of its own.

## Consequences

- The engine is a large part of the codebase (`fsops/copy.rs` alone is about 2,500 lines)
  and needs the fault-injection suite to stay correct. A-FS-1 to A-FS-13 are its contract
  ([TDR-1 11.1](../tdr/0001-core.md#111-file-operations)).
- A job holds one directory fd per level of the path; manycommander raises the
  `RLIMIT_NOFILE` soft limit to the hard limit at startup.
- `STATX_MNT_ID_UNIQUE` has no named constant in `rustix`; the engine passes the raw bit and
  checks `stx_mask` before it trusts the field.
- One residual race remains and is documented: Linux has no unlink-by-fd, so a source
  replaced between the last `statx` and `unlinkat` of a move can still be removed
  ([TDR-1 4.8](../tdr/0001-core.md#48-move-and-rename-f6)).
- Later sources (archives, SFTP) attach on the source side of this engine, so every byte
  written to a local disk still goes through it ([ADR 0019](0019-non-local-places-through-a-narrow-provider.md)).

## Alternatives considered

| Alternative | Why not |
|---|---|
| `std::fs::copy`, `fs_extra` | `O_TRUNC` breaks I-2 and I-4; no chunk-level cancel or progress |
| Path-based operations with `lstat` checks | Every call re-resolves the path; a swapped component redirects the job |
| `libc` or `nix` calls directly | Needs `unsafe` throughout the engine; `rustix` offers safe wrappers for the same calls |
| Shelling out to `cp` and `mv` | No per-entry questions, no honest per-entry report, no control over commit or durability |

## References

- [TDR-1 4](../tdr/0001-core.md#4-file-operation-safety-highest-risk), [TDR-1 3.3](../tdr/0001-core.md#33-implementation-basis)
- [ADR 0005](0005-filesystem-identity-is-dev-and-mount-id.md), [ADR 0007](0007-cross-filesystem-move-with-group-commit.md)
