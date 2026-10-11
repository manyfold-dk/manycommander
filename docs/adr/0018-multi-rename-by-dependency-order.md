# ADR 0018: Multi-rename orders renames by dependency and breaks cycles with one temporary name

## Status

Accepted -- 2026-09-28.

## Context

The multi-rename tool (`Ctrl+M`) renames many entries at once from masks, search and
replace, a counter and case rules, with a live preview (TDR-2 6). A batch of renames can
depend on itself:

- a **chain**: `a -> b` while `b -> c`: `b` must move first;
- a **swap or cycle**: `a -> b` and `b -> a`: neither can move first without a temporary
  name;
- a **blocked chain**: `a -> b` while `b`'s own rename fails: `a` must not overwrite `b`.

On a case-insensitive directory (a casefold tmpfs, vfat, exfat), byte comparison of names
is wrong: `Foo -> bar` with `Bar -> foo` is a cycle there and two independent renames on a
case-sensitive directory.

The tool must uphold I-3 (never overwrite without a decision) and I-9: a rename never
overwrites and never strands; after any failure or cancel, every inode the job touched is
reachable under its original name, its new name, or a temporary name the report states.

## Decision

1. **Dependencies by identity.** Before renaming, the job `statx`es each old name and each
   new name (`AT_SYMLINK_NOFOLLOW`). Entry A depends on entry B when A's new name currently
   has B's identity. A new name with the entry's own identity is a case-only change.
2. **Order, not temporaries.** An entry is ready when it depends on no pending entry. Ready
   entries rename with `renameat2(RENAME_NOREPLACE)`; each success frees a name and can make
   dependents ready. An entry that fails or is skipped keeps its name, so its dependents are
   skipped with "the destination exists" and are never treated as a cycle.
3. **One temporary per cycle.** Only the strongly connected components left when nothing is
   ready are cycles. One member of each moves to `.mc-rename-<16 hex>` in the same
   directory, the rest of the cycle resolves, and the temporary finally takes its target.
   Case-only changes use the same temporary scheme.
4. **No questions, no overwrite.** `EEXIST` from something outside the set skips the entry;
   every rename re-checks the old name's identity first.
5. **Recovery and undo by identity.** After an error or a cancel, a member under a temporary
   name goes back to its original name when that name is free, else the report names the
   temporary path. Every job returns the renames it performed with identities, including
   entries left under temporary names; `Ctrl+Z` reverses them, each only if the entry under
   its new name still has the recorded identity.

## Rationale

- Ordering touches each entry once in the common case (no cycles) and leaves nothing behind;
  a crash in the middle leaves at most one temporary name per cycle.
- Comparing identities instead of bytes is correct on case-sensitive and case-insensitive
  directories alike, without knowing which one a directory is.
- A question in the middle of a cycle would hold a member under a temporary name while the
  user decides; skipping keeps the window short and the outcome reportable.
- The preview catches most errors before any write (duplicate new names, invalid names,
  names taken by listed entries outside the set), and the job re-checks them, so the
  preview may be advisory for what it cannot see.

## Consequences

- A failpoint sweep injects an error and a cancel at every rename of a cycle and of the
  temporary name (A-MR-4): every inode stays reachable under a name the report states, and a
  cancelled job still returns its undo record.
- On a casefold tmpfs under `unshare -rm` the case-only rename and the `Foo/Bar` swap pass
  (A-MR-6). Two new names that differ only in case on a case-insensitive directory are not
  detected in advance; the second fails with `EEXIST` and is skipped.
- When several entries of the set are hard links of one inode, the holder of a new name is
  the one whose old name equals it, exactly or under Unicode lowercase folding
  (TDR-2 E-38).
- The preview recomputes 10k names in 1.09 ms to 5.22 ms per keystroke on the reference
  machine, against the 16 ms target of P-14.
- A rename does not fsync, like `mv`.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Two phases: every entry to a temporary name, then every temporary to its target | Touches every file twice and leaves a temporary name per entry after a crash |
| Byte comparison of names for dependencies | Wrong on case-insensitive directories |
| Ask "file exists" during the job | Holds cycle members under temporary names while the user decides |

## References

- [TDR-2 6 Multi-rename](../tdr/0002-navigation-and-bulk-tools.md#6-multi-rename)
- [TDR-2 1.2 Invariants](../tdr/0002-navigation-and-bulk-tools.md#12-invariants)
- [TDR-2 12.3 Multi-rename acceptance checks](../tdr/0002-navigation-and-bulk-tools.md#123-multi-rename)
