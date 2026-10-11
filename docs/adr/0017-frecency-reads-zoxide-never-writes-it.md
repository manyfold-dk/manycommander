# ADR 0017: Directory frecency follows zoxide's model, reads zoxide and never writes it

## Status

Accepted -- 2026-09-28.

## Context

`Ctrl+D` jumps to a bookmark or to a frequently used directory, and `z <keywords>` on the
command line does the same in one line (TDR-2 3). Ranking "frequently used" needs a store of
visits. Omarchy ships zoxide, so many users already have years of directory ranking in
zoxide's database, built from their shell.

Three questions had to be answered: which ranking model, where manycommander keeps its own
visits, and how it relates to zoxide's data.

## Decision

1. **The ranking is zoxide's model.** A visit adds 1 to a directory's rank. The score is
   `rank x w(age)`, with `w` = 4 within an hour, 2 within a day, 0.5 within a week, 0.25
   otherwise. When the sum of ranks passes 10000, every rank is multiplied by
   `0.9 x 10000 / sum` and entries below 1 are dropped. Keywords match in order, ASCII
   case-insensitively, and the last keyword must match the last path component.
2. **manycommander keeps its own store**, `$XDG_STATE_HOME/manycommander/dirs.tsv`, one
   line per directory with decimal ranks and byte-escaped paths. A session records its
   visits as deltas. On exit it takes an exclusive `flock` on a separate lock file, re-reads
   the store, adds its deltas, ages, writes a temporary file, fsyncs it, and renames it over
   the store. The lock wait is bounded at 2 s.
3. **zoxide is read, never written.** With `jump.zoxide = "auto"` (the default) and
   `zoxide` on `PATH`, the first `Ctrl+D` of a session runs `zoxide query --list --score` by
   argv, with a 1 s timeout, off the UI thread, and merges the result: a path's score is the
   larger of the two. manycommander never runs `zoxide add`, and `Delete` on a row that only
   zoxide knows hides it only until manycommander exits.
4. **The store loads after the first frame,** on the directory-store thread. Bookmarks, a
   small file, load on the boot thread.

## Rationale

- A model the user already knows makes the ranking predictable: the same directories come
  first in the dialog as in their shell's `z`.
- Merging under a lock lets two manycommander instances that exit at the same time keep
  both sets of visits; replacing the file would lose one.
- Writing into zoxide's database would change another tool's data, with its own format and
  aging, on every navigation in a file manager. Reading it gives the benefit without that.
- The store can hold thousands of lines. Loading it on the boot path would cost the first
  frame (P-2); in `state.toml`, it would also be replaced on every exit instead of merged.

## Consequences

- With a 5000-entry store on the reference machine, the dialog opens in 1.75 ms, a
  keystroke re-ranks in 0.40 ms, and the first frame stays at 13.10 ms (P-15, P-2).
- A visit is a navigation the user starts: `Enter`, parent, `cd`, history, the dialog, `z`.
  Startup, restored tabs, ancestor fallbacks, new tabs and refreshes do not count, so
  restoring a session does not inflate its directories (TDR-2 E-23).
- Only a failed navigation drops a frecency entry; a bookmark is never dropped
  automatically. A `dirs.tsv` or `hotlist.toml` that cannot be read or parsed is never
  overwritten.
- Frecency records local directories only, so `z` and the frequent list never connect to an
  SFTP server; `sftp://` bookmarks are explicit (TDR-3 5.1).
- When zoxide is missing or slow, the dialog works with manycommander's own store alone.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Write visits to zoxide (`zoxide add`) | Changes another tool's data |
| Use zoxide as the only store | Optional dependency; a user without zoxide would get no ranking |
| Keep frecency in `state.toml` | Grows to thousands of lines, delays the first frame, and is replaced rather than merged |
| A different ranking model | Unfamiliar ranking for users who already rely on zoxide |

## References

- [TDR-2 3 Directory hotlist and frecency jump](../tdr/0002-navigation-and-bulk-tools.md#3-directory-hotlist-and-frecency-jump)
- [TDR-2 11 Non-functional requirements](../tdr/0002-navigation-and-bulk-tools.md#11-non-functional-requirements)
