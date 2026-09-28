+++
title = "File operations"
description = "What copy, move, trash, delete, multi-rename, links and attributes guarantee, and what they do not preserve."
weight = 60
+++

Every file operation runs as one job on a worker thread, with progress and cancel, while
the panels stay responsive. One job runs at a time. Copy, move and delete first plan: they
scan the sources and count files and bytes, and you can cancel the plan. Before a job acts
on an entry, it checks that the entry is still the one it planned for. Every job ends with
a report. Each entry is done, skipped with a reason,
or failed with the operating system's error. A cancelled job says what it left, for
example "move cancelled: 812 moved, 40 still at source".

## What you see is what you act on

A verb acts on the marked entries that are visible. An entry that the
[quick filter](@/docs/find-and-rename.md#quick-filter-ctrl-f) or the hidden-file toggle
hides is not acted on, even when it is marked. When no visible entry is marked, the verb
acts on the entry under the cursor, and on nothing when the cursor is on `..`. The footer
counts exactly the visible marks. Hidden marks are kept, and count again once their entries
show.

In a [results tab](@/docs/find-and-rename.md#the-results-tab), the verbs act on results
from many directories, and each result is handled in its own directory.

## Copy (F5)

- A copy never shows a partial file. Data goes to `.<name>.mc-partial-<random>` in the
  destination and is renamed into place when complete.
- An existing file is replaced only after you answer Overwrite, Overwrite all or Overwrite
  all older, and then atomically. The old file is never truncated. The other answers are
  Skip, Skip all, Rename and Cancel job.
- Symbolic links are copied as links, never followed.
- Sparse files stay sparse: manycommander copies only the data and leaves the holes as
  holes. A destination filesystem without holes, such as vfat, receives the zeros.
- Files that are hard-linked to each other within the copied set stay hard-linked in the
  destination. A file whose other names lie outside the selection is copied as a separate
  file, as `cp -a` does. When the destination cannot take a hard link, the data is copied,
  and the report says how many links became separate files.
- Not preserved: ownership, ACLs and extended attributes.

![manycommander asking whether to overwrite mountains.jpg in ~/Pictures, with the choices Overwrite, Overwrite all, Overwrite all older, Skip, Skip all, Rename and Cancel job](/screens/dialog.svg)

## Move and rename (F6, Shift+F6)

- On one filesystem, a move is a rename.
- Across filesystems, manycommander copies, checks that the source did not change during
  the copy, and deletes the source only after the copy is durable. A source that changed is
  kept.
- A move across filesystems keeps sparse files and hard links like a copy. It deletes the
  source names of a hard-linked file only after all of its selected names are copied.
- Mount points are skipped, never copied and deleted.
- On vfat and exfat, the change check is best-effort, because their timestamps are coarse.

## Multi-rename (Ctrl+M)

The masks, the preview and examples are on the
[find and rename](@/docs/find-and-rename.md#multi-rename-ctrl-m) page. The rename itself
keeps these guarantees:

- It never overwrites. A new name that an entry outside the renamed set holds skips that
  entry with "the destination exists", and the other file stays untouched.
- Chains and cycles work. In `a -> b`, `b -> c`, `b` is renamed first. In a swap or a
  longer cycle, one member first moves to a temporary name `.mc-rename-<random>` in the same
  directory, which frees its old name.
- When an entry fails or is skipped, the entries that need its old name are skipped too,
  never forced.
- After an error or a cancel, every file is under its original name, its new name, or a
  temporary name that the report states.
- Like `mv`, a rename does not call `fsync`.

`Ctrl+Z` in the multi-rename dialog undoes the last multi-rename of the session, also one
that was cancelled or failed. Each file is renamed back only if the entry under its new name
is still the same file; an entry that was replaced since stays as it is, and the report
names it. The undo is gone once it has run, or when manycommander exits.

## Links (Alt+L)

`Alt+L` creates links to the selected entries. The form proposes the other panel's
directory as the destination, or `<other panel>/<name>` for one entry.

| Type | Result |
|---|---|
| Symbolic, relative (the default) | A symlink whose target is the relative path from the link's directory to the source, such as `../photos/rome.jpg` |
| Symbolic, absolute | A symlink whose target is the source's absolute path |
| Hard | Another name for the same file |

- Symlink targets are the paths as the panels show them; manycommander resolves no
  symlink to compute them.
- A hard link links the entry itself: a selected symlink gets a second name, and its target
  is not touched. A directory is skipped with "directories cannot be hard-linked". A hard
  link to another filesystem fails with "hard links cannot cross filesystems".
- A link never replaces anything. When the name exists, you choose Skip, Skip all, Rename
  or Cancel job; there is no Overwrite.

## Attributes (Alt+A)

`Alt+A` changes the mode, the modification time, or both, of the selected entries.

| Field | Meaning |
|---|---|
| Mode | Octal or symbolic, as below. Empty: unchanged. For one selected entry, the label shows its current mode, such as `Mode (now 0644)` |
| Modification time | `YYYY-MM-DD HH:MM` or `YYYY-MM-DD HH:MM:SS` in the local time zone, or `now`. Empty: unchanged |
| Recursive | Also change everything below the selected directories |

The mode field starts empty, so an untouched field never changes a mode, not even with
Recursive on. The form shows the change for the first selected entry, such as
`rw-r--r-- -> rwxr-xr-x`.

| Mode | Meaning |
|---|---|
| `644`, `0644`, `4755` | Octal, one to four digits: exactly these bits, every other bit cleared |
| `u+x`, `g-w`, `o=r` | Clauses `[ugoa]*[+-=][rwxXst]*`, as in chmod(1), separated by commas: `u+x,g-w,o=r` |
| `+x`, `-w` | A clause without `u`, `g`, `o` or `a` means `a`, and ignores the umask |
| `a+X` | `X` adds execute where the entry is a directory or already has an execute bit |
| `u+s`, `g+s`, `+t` | `s` is setuid for `u` and setgid for `g`; `t` is the sticky bit |
| `go=` | `=` clears the class's `rwx` bits, and its `s` or `t`, then sets the ones given |

Anything else is an error that the form shows before it runs.

- The job changes each entry itself, never a symlink target. A symlink has no mode of its
  own: a mode-only change skips it, and a time change sets the link's own time.
- A recursive change descends into btrfs subvolumes and skips mount points, as permanent
  delete does.
- A directory is made readable before its entries are read, and loses read or execute
  permission only after its entries are done, so `a-rx` and `u+rx` both work on a whole
  tree. If the final change of a directory fails, the report names the mode it was left
  with.
- An entry you do not own raises the error question.
- An entry whose mode and time would not change counts as unchanged.

## Trash (F8)

- Trash follows the freedesktop.org Trash specification and is compatible with GIO:
  `gio trash --restore` and Nautilus restore what manycommander trashed.
- Files on the home filesystem go to `~/.local/share/Trash`. Files on another filesystem go
  to that filesystem's own `.Trash-$UID`, or to a shared `.Trash` that passes the
  specification's checks.
- Trash never copies across filesystems and never deletes. A directory is trashed whole,
  with one rename, so trashing is instant whatever its size.
- When no trash can take an entry, you choose Skip, or a permanent delete of that entry
  alone behind the typed `delete` confirmation.

## Delete permanently (Shift+F8)

- manycommander counts files, directories and bytes first, and deletes only after you type
  `delete`.
- Symbolic links are removed, never followed.
- Mount points and bind mounts inside the tree are skipped.
- Read-only directories are not forced open: their entries fail with the error.

How moves survive a crash is on the [durability](@/docs/durability.md) page.
