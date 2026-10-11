# ADR 0005: Filesystem identity is the pair (st_dev, mnt_id)

## Status

Accepted -- 2026-09-27.

## Context

Several decisions in a file manager depend on whether two directories are "the same
filesystem": whether a move can be a `rename`, whether a traversal has crossed into a mount
point that a move or delete must not touch, which trash directory an entry belongs to, and
whether a destination lies inside a source through a bind mount.

`rename(2)` works only within one mount of one filesystem, and on btrfs only within one
subvolume. `st_dev` alone cannot express that:

- Bind mounts share the `st_dev` of the filesystem they show, so a bind mount looks like
  "the same filesystem" although it is a mount point.
- btrfs subvolumes that are not mount points have their own `st_dev`, so they look like
  "another filesystem" although they live inside one mount.

On a default Omarchy install, `/` and `/home` are separate btrfs subvolume mounts, so both
cases are everyday ones, not corner cases.

## Decision

- Every identity decision uses the pair `(st_dev, mnt_id)`, with `mnt_id` from `statx` and
  `STATX_MNT_ID_UNIQUE`.
- manycommander always tries `rename` first and lets the kernel decide. The pair decides
  only what happens after `EXDEV`, and how far a traversal goes:

  | Child compared with its parent | Copy | Cross-filesystem move | Permanent delete | Trash |
  |---|---|---|---|---|
  | Same `mnt_id`, same `st_dev` | descend | descend | descend | same top directory |
  | Same `mnt_id`, different `st_dev` (btrfs subvolume) | descend | descend | descend | own top directory |
  | Different `mnt_id` (mount point, bind mount) | descend | skip: "mount point" | skip: "mount point" | separate top directory |

- Inode identity `(st_dev, st_ino)` decides "same file" and directory cycles.
- A trash "domain" is equal `(st_dev, mnt_id)`.

## Rationale

- The pair distinguishes all four cases that matter, from one `statx` the engine already
  makes for every entry.
- Trying `rename` first keeps the kernel the authority: the table never predicts success,
  it only bounds the fallback.
- `STATX_MNT_ID_UNIQUE` is stable across remounts, unlike a reused short mount ID.

## Consequences

- manycommander needs a kernel that reports `STATX_MNT_ID_UNIQUE` (6.8 or later,
  [ADR 0002](0002-a-new-linux-only-file-manager-in-rust.md)).
- A move or delete that meets a mount point skips it and reports the reason; it never
  copy-deletes a mounted filesystem (I-6).
- Removing a btrfs subvolume root as part of a delete may fail; the failure is reported, not
  hidden.
- `EXDEV` from a trash rename is a normal outcome when a mount appears concurrently; the
  engine tries the next trash method.
- A-FS-4, A-FS-7 and A-DEL-1 test bind mounts under `unshare -rm` and unprivileged btrfs
  subvolumes.

## Alternatives considered

| Alternative | Why not |
|---|---|
| `st_dev` only | Wrong for bind mounts and for btrfs subvolumes, in opposite directions |
| Parsing `/proc/self/mountinfo` per path | Racy and slow, and it resolves paths the engine deliberately does not resolve |
| Predicting `rename` success instead of trying it | The kernel knows better; a wrong prediction either fails needlessly or copy-deletes |

## References

- [TDR-1 4.2](../tdr/0001-core.md#42-filesystem-identity), [TDR-1 4.6](../tdr/0001-core.md#46-pre-flight-checks-plan-phase),
  [TDR-1 4.10](../tdr/0001-core.md#410-trash-f8)
