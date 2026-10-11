# ADR 0024: Uploads commit through hardlink@openssh.com; moves across hosts are best-effort

## Status

Accepted -- 2026-09-28.

## Context

On a local disk manycommander never shows a partial file (I-2), never replaces a name
without the user's decision (I-3) and never loses data in a move (I-1)
([TDR-1 4.1](../tdr/0001-core.md#41-invariants)). SFTP version 3 offers less:

- `SSH_FXP_RENAME` does not say whether it replaces an existing name. OpenSSH's server
  refuses; another server may replace.
- Status codes do not tell "exists" from other failures; both are `SSH_FX_FAILURE`.
- There are no inode numbers, so a file cannot be identified after it was read.
- Nothing makes a directory entry durable on the server. `fsync@openssh.com` syncs file
  data only.

## Decision

**R-1, no partial remote file.** An upload writes `.<name>.mc-partial-<random>`, opened with
`CREAT` and `EXCL`, sets its mode and times, closes it, and commits it with
`hardlink@openssh.com(temporary, final)`. That extension is defined as `link(2)`, which
never replaces a name. The temporary name is then removed. A server without the extension,
or one that refuses it with "permission denied", gets direct-write mode: the final name is
created with `CREAT` and `EXCL` and written in place, is visible while it is written, and is
removed on failure or cancel. The report says which mode ran, and after a lost session it
names the path that may hold partial data.

**R-2, never replace without a decision.** A failed commit `LSTAT`s the name and raises
"file exists" when it is there. Overwrite uses `posix-rename@openssh.com`, an atomic
`rename(2)`. A server without it refuses the overwrite.

**R-4, moves across hosts are best-effort.** The confirm dialog says so on every server.

- Upload move: the temporary file is synced with `fsync@openssh.com` when the server has
  it, committed, and the local source is then unlinked with the local engine's flush check.
  Without `fsync` the report says "not synced on the server".
- Download move: the local group commit syncs the batch, and the remote source is kept.
  The dialog and the report say "remote sources kept: the server cannot identify them";
  Shift+F8 removes them when the user decides to.

## Rationale

- `hardlink` gives the atomic, never-replacing commit that `SSH_FXP_RENAME` does not
  guarantee, so on OpenSSH an upload behaves like a local copy.
- Keeping remote sources avoids deleting a file that was never read: size and a one-second
  mtime match a replacement written in the same second.
- Claiming I-1 for a move would promise what the protocol cannot deliver; the word
  "best-effort" before the job is honest reporting (I-7).

## Consequences

- On a server without `hardlink`, an upload is visible while it is written. The website
  and the F1 help say so next to the SFTP feature.
- A download move leaves the remote files in place; the user deletes them explicitly.
- A small file takes three round trips per upload: `OPEN`; the writes, `FSETSTAT`, the
  optional `fsync` and `CLOSE`; then `hardlink` and `REMOVE`.
- Hard-linked local files in an upload move are kept after their first name ("source
  changed; kept both"): safe, but noisy.
- A failpoint sweep in the A-FS-5 style over local-to-remote moves (A-SF-9) and an
  injection sweep over uploads with `sftp-server -P` (A-SF-7) hold these rules.

## Alternatives considered

| Alternative | Why not |
|---|---|
| `SSH_FXP_RENAME` as the upload commit | Version 3 leaves open whether it replaces; OpenSSH's refusal is one server's property |
| Removing a remote source after a download when size and mtime still match | Could delete a replacement that was never read |
| Refusing uploads to servers without `hardlink` | Leaves those servers unusable; direct-write mode with a clear report is the same exception the local engine makes ([TDR-1 4.7](../tdr/0001-core.md#47-copy-f5)) |
| Claiming I-1 when the server has `fsync@openssh.com` | The entry that `hardlink` creates is still not synced |

## References

- R-1, R-2, R-4 in [TDR-3 1.3](../tdr/0003-archives-quick-view-and-sftp.md#13-invariants);
  [TDR-3 5.6](../tdr/0003-archives-quick-view-and-sftp.md#56-write-operations-3b)
- [TDR-3 appendix A, E-24 to E-27](../tdr/0003-archives-quick-view-and-sftp.md#appendix-a-implementation-decisions)
- [ADR 0007](0007-cross-filesystem-move-with-group-commit.md): the local move
