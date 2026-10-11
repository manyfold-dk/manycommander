# ADR 0006: An own implementation of the freedesktop.org Trash

## Status

Accepted -- 2026-09-27.

## Context

F8 moves entries to the trash that Nautilus and `gio trash` use, so the user can restore
them with the desktop's tools. The freedesktop.org Trash specification defines a home trash
and two methods for a trash at the top of another filesystem. GIO adds behaviour that
compatibility needs, such as byte-wise percent-encoding of the original path.

manycommander's invariants put hard requirements on a trash implementation:

- It must never copy across filesystems and never fall back to a delete (I-6). A failed
  trash must leave the entry where it is.
- It must trash a symlink, not its target (I-5).
- It must pick the trash by the `(st_dev, mnt_id)` domain
  ([ADR 0005](0005-filesystem-identity-is-dev-and-mount-id.md)), so btrfs subvolumes and bind
  mounts get the right trash.
- It must refuse a trash directory, `files/` or `info/` that is a symlink or has the wrong
  owner, because a shared top-directory trash can be hostile.
- It must encode `Path=` byte-wise as GIO does, so names with newlines or invalid UTF-8
  restore to the original bytes, and it must cope with names of 255 bytes.

The `trash` crate was the obvious candidate. It did not document or test these guarantees,
and checking that it met them would have cost more than writing the small part of the
specification manycommander needs.

## Decision

manycommander implements the specification's trash side itself, in `fsops/trash.rs`:

- The home trash, method 1 (`$top/.Trash/$uid` under a sticky `$top/.Trash`) and method 2
  (`$top/.Trash-$uid`). Every directory in the chain is opened with `O_NOFOLLOW` and checked
  on the fd.
- The name is reserved by creating `info/N.trashinfo` with `O_EXCL`, the info file and
  `info/` are synced, then the entry is moved with `renameat2(..., RENAME_NOREPLACE)`. An
  `EXDEV` removes the info file and tries the next method; no path copies.
- A long name is shortened for the trash basename while `Path` keeps the original bytes.
- No trash browser, no restore and no `directorysizes` cache: restore is the desktop's job.

## Rationale

- The needed part of the specification is small, and every guarantee above is testable in
  isolation (A-TR-1 to A-TR-5, A-FS-10).
- An implementation that owns its checks can state them in the F1 help and in the report.

## Consequences

- manycommander maintains trash code and follows the specification and GIO's behaviour.
- Restore happens with `gio trash --restore` or Nautilus; a manual check confirms GIO
  restores manycommander's entries, including a name with a newline and a non-UTF-8 byte.
- GIO refuses to trash on system-internal mounts such as tmpfs. manycommander writes a
  specification-valid top-directory trash there, but GIO will not restore from it.
- When no trash is usable, F8 offers only Skip or the typed permanent-delete confirmation
  for that entry; it never deletes on its own.

## Alternatives considered

| Alternative | Why not |
|---|---|
| The `trash` crate | Its guarantees on cross-device copies, symlinks, domains, hostile trash directories and byte-wise encoding were not established; revisit if a version is verified to match |
| Copying across filesystems into the home trash | Breaks I-6: a "trash" that copies a large tree is a slow, partial delete |

## References

- [TDR-1 4.10](../tdr/0001-core.md#410-trash-f8), [TDR-1 11.2](../tdr/0001-core.md#112-trash-and-delete)
- TDR-1 E-13, E-14, E-15, E-33
