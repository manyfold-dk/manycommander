# ADR 0015: Search results are a panel tab, and every job takes groups of single-component names

## Status

Accepted -- 2026-09-28.

## Context

Find (`Alt+F7`) returns files from many directories of a tree. The user then wants to act
on them: view, edit, copy, move, trash, delete, link, change attributes, rename. Total
Commander shows results in a dialog list with a "feed to listbox" button that turns them
into a temporary panel.

manycommander's verbs were built for one directory: a job opened the panel's directory as a
directory fd and acted on single names in it (TDR-1 4.3). That rule is what keeps I-5:
no job ever resolves a multi-component path, because `openat(dirfd, "a/b/c")` follows a
symlink in `a` or `b`. Results from many directories need a way into the verbs that does
not break it.

## Decision

1. **Results are a tab.** A search opens a new tab on the active side whose panel source
   is `Results(Arc<Search>)`. Its entries are paths relative to the search root, stored in
   the panel's normal compact name arena, so sorting, marks, the quick filter, F3, F4 and
   every verb work on them unchanged. `Enter` on a file goes to its directory; `Alt+Left`
   returns to the results. A re-stat (`Ctrl+R`, after every job) drops results that are gone.
2. **Every verb takes groups.** `JobSpec::{Copy, Move, Trash, Delete, Rename, Link, Attr}`
   take `Vec<Group>`, where a group is a root, a relative directory below it as single
   components, and single-component names. A directory panel produces one group; a results
   tab produces one group per relative directory of the selection.
3. **A group opens by components.** The job opens the root once, then walks the relative
   directory one component at a time with `openat(O_DIRECTORY | O_NOFOLLOW)`. A component
   that is now a symlink fails the whole group with "type changed". Every component and name
   must pass `valid_component`.
4. Checks that span the selection span all groups: the destination-inside-source check uses
   the union of the scanned directory identities, standing answers ("Overwrite all") carry
   across groups, and groups whose opened directories have the same identity are merged
   before a rename, trash, delete or link job.

## Rationale

- A tab reuses everything a panel already does. A dialog list would need its own marks,
  sorting, filter and a copy of each verb's entry point, and would drift from the panel.
- Grouping by parent directory keeps every name a single component, so I-5 holds for
  results exactly as it holds in a directory panel, and the job code keeps one traversal
  rule.
- The change was made behaviour-neutral for one group first, with the full file-operation
  test suite as its regression net, before any feature built on it.

## Consequences

- A results tab has no `..` row and no inotify watch; it is saved on exit as a tab on its
  root directory, not as results.
- Some verbs make no sense in results and are refused with "not in search results": F7,
  `Shift+F4` and compare.
- Only the three most recent results places stay in a panel's history, which bounds memory
  (P-6b).
- The group model later carried archive and remote places too: TDR-3 2.2 generalises the
  root to `Root::{Local, Archive, Remote}` and keeps the component walk for `Root::Local`.
- Each search owns its tab; leaving the tab's results cancels a running search, which keeps
  its partial results (TDR-2 E-28).

## Alternatives considered

| Alternative | Why not |
|---|---|
| Results in a dialog list | Needs its own copies of marks, sorting, filtering and every verb |
| Multi-component names relative to the search root in jobs | Resolving `a/b/c` with `openat` follows symlinks in `a` and `b` (I-5) |
| Resolve each result to an absolute path and act on that | Re-resolves paths at job time, the race TDR-1 4.3 exists to close |

## References

- [TDR-2 2.2 Grouped sources](../tdr/0002-navigation-and-bulk-tools.md#22-grouped-sources)
- [TDR-2 2.4 Panel sources and history](../tdr/0002-navigation-and-bulk-tools.md#24-panel-sources-and-history)
- [TDR-2 5.4 Results tab behaviour](../tdr/0002-navigation-and-bulk-tools.md#54-results-tab-behaviour)
- [ADR 0016: An own find engine on directory file descriptors](0016-own-find-engine.md)
- [ADR 0004: An own file-operation engine on directory file descriptors](0004-own-file-operation-engine-on-directory-fds.md)
