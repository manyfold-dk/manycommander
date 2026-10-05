+++
title = "File operations"
description = "What copy, move, trash, delete, multi-rename, links, attributes, extraction and SFTP transfers guarantee, and what they do not preserve."
weight = 70

[extra]
group = "reference"
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
[quick filter](@/docs/find-and-rename.md#quick-filter) or the hidden-file toggle
hides is not acted on, even when it is marked. When no visible entry is marked, the verb
acts on the entry under the cursor, and on nothing when the cursor is on `..`. The footer
counts exactly the visible marks. Hidden marks are kept, and count again once their entries
show.

In a [results tab](@/docs/find-and-rename.md#the-results-tab), the verbs act on results
from many directories, and each result is handled in its own directory.

## F3 and applications

`F3` shows a file in your pager (`$PAGER`, or `pager` in the
[configuration](@/docs/configuration.md)). A file that a pager cannot show opens in its
application instead, as `Enter` opens it: `gio open` starts the desktop's default
application, and a web page opens in the default browser. A terminal program, such as a
terminal editor for text, opens in a new terminal window. Without `gio`, manycommander uses
`xdg-open`. The file's extension decides, ignoring case:

| Kind | Extensions |
|---|---|
| Pictures | `png`, `jpg`, `jpeg`, `gif`, `webp`, `bmp`, `tif`, `tiff`, `avif`, `heic`, `heif`, `ico`, `svg`, `jxl` |
| Documents | `pdf`, `epub`, `djvu`, `odt`, `ods`, `odp`, `docx`, `xlsx`, `pptx`, `doc`, `xls`, `ppt`, `rtf` |
| Audio | `mp3`, `flac`, `ogg`, `oga`, `opus`, `m4a`, `wav`, `aac` |
| Video | `mp4`, `m4v`, `mkv`, `webm`, `mov`, `avi`, `mpg`, `mpeg`, `wmv`, `ogv` |
| Web pages | `html`, `htm`, `xhtml` |

Every other file goes to the pager, Markdown included; the [quick view](@/docs/quick-view.md)
shows Markdown rendered. `F4` always edits in `$EDITOR`.

## Copy (F5)

- A copy never shows a partial file. Data goes to an unnamed temporary file in the
  destination's filesystem, which gets its name only when it is complete. Where the
  filesystem has no unnamed temporary files (vfat, exfat, most FUSE filesystems), after
  Overwrite, and for archive members, downloads and symbolic links, data goes to
  `.<name>.mc-partial-<random>` in the destination and is renamed into place when complete.
- The exception is a filesystem with neither `RENAME_NOREPLACE` nor hard links, such as some
  FUSE filesystems. There the file is created under its final name, is visible while it is
  written, and is removed after an error or a cancel.
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

![manycommander's Change attributes form for two marked files, with the mode go-r and the preview mountains.jpg: rw-r--r-- -> rw-------](/screens/attributes.svg)

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

## Extract (F5 in an archive)

Extraction is a copy whose source is an [archive](@/docs/archives.md). It keeps the copy's
guarantees: no partial file under a real name, Overwrite only after you answer, and the
report. It adds these:

- A member's name is data, never a path. A member with a `..` component, a NUL byte or an
  invalid component, and a member below a member that is not a directory, is skipped as
  "unsafe path". Nothing lands outside the destination. A leading `/` is dropped.
- Symbolic link members become symbolic links. Extraction never follows a link, not even one
  it made itself.
- Device, FIFO and socket members are skipped as "special file". Modes lose the setuid,
  setgid and sticky bits. The modification time comes from the member; ownership, ACLs and
  extended attributes are not restored.
- A hard-link member becomes a hard link when the same extraction wrote its target, made
  from the extracted file itself. Otherwise it is skipped with "hard link to a member not
  extracted".
- No member writes more than it declares. A member whose data runs past its declared size,
  or ends before it, fails with "size mismatch", and the excess is never written. A zstd or
  xz stream that needs more than a 128 MiB window fails with "archive needs too much memory
  to decode". When the declared total is larger than the destination's free space,
  manycommander asks first.
- Each member's header is checked again against what the listing saw. A difference fails
  the member with "archive changed". A truncated stream, a CRC error or a decoder error fails
  it with "archive damaged". Neither leaves a partial file.
- An existing name raises the usual questions. The archive itself is never replaced by one of
  its members: "is the archive being extracted".
- Encrypted members are skipped with "encrypted".

Like a copy, extraction does not call `fsync`.

## SFTP: uploads and changes on a server

On an [SFTP server](@/docs/sftp.md) manycommander has no directory handles, inode numbers or
no-follow opens: the protocol works on paths, and the server resolves them. The rules below
keep as much of the local guarantees as the protocol allows, and say where they are weaker.

- On a server with hard links, an upload never shows a partial file. Data goes to
  `.<name>.mc-partial-<random>` in the destination directory, and the server's
  `hardlink@openssh.com` links it into place, which fails when the name exists; then the
  temporary name is removed. manycommander never
  commits with a plain SFTP rename, because the protocol leaves open whether it replaces.
- A server without hard links gets direct writes: the file is created under its final name,
  visible while it is written, and removed after an error or a cancel. The report says so.
- After a lost connection, the report names the file that may hold partial data: the
  temporary name, or in direct-write mode the final name.
- An existing file is replaced only after you answer Overwrite, and only atomically, through
  the server's `posix-rename@openssh.com`. A server without it refuses: "the server cannot
  replace a file atomically".
- A rename on the server (`F6` within one server, `Shift+F6`) checks the new name first,
  and asks when it is taken; `F7` reports a name that exists. OpenSSH's server refuses a
  rename onto any existing name: a file, a directory or a symbolic link. Another server may
  replace it, so a name that appears between the check and the rename can be lost there.
- Symbolic links are copied as links, in both directions. manycommander checks every entry
  first and opens only regular files, and a walk (a download, a size, a delete) never enters
  a linked directory. The server resolves every path itself, though: an entry swapped between
  the check and the request goes undetected.
- A regular file swapped for a FIFO between the check and the open blocks OpenSSH's server,
  and every later request on that connection waits behind it. `Esc` cancels; when the server
  stays silent for 2 s, manycommander ends the connection with "connection lost".
- There is no trash on a server: `F8` is refused, and `Shift+F8` deletes after you type
  `delete`. A delete removes a symbolic link as a link.
- Uploaded files lose the setuid and setgid bits. Times on a server have whole seconds.
  Files hard-linked on the server arrive as separate files.

How moves survive a crash, and why a move between hosts is best-effort, is on the
[durability](@/docs/durability.md) page.
