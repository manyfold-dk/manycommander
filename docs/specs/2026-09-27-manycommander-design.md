---
title: manycommander design
type: spec
status: draft
owner: manycommander
created: 2026-09-27
updated: 2026-09-27
---
# manycommander design

Contents: [1 Outcome](#1-outcome) · [2 Environment](#2-environment-assumptions) ·
[3 Architecture](#3-architecture) · [4 File-operation safety](#4-file-operation-safety-highest-risk) ·
[5 Panels](#5-panels) · [6 Command line](#6-command-line-and-hand-off) · [7 Theming](#7-theming) ·
[8 Keymap](#8-keymap) · [9 Launch](#9-launch-and-integration) ·
[10 Alternatives](#10-alternatives-considered) · [11 Acceptance](#11-acceptance-checks) ·
[12 Publication](#12-repository-and-publication) · [13 NFRs](#13-non-functional-requirements) ·
[A Review resolution](#appendix-a-review-resolution) ·
[B Plan-review amendments](#appendix-b-amendments-from-the-plan-review)

## 1. Outcome

manycommander is a keyboard-driven, dual-pane terminal file manager for Omarchy, written in
Rust with ratatui. It follows the Total Commander tradition: two panels, the F3-F8 verbs,
a command line, and tabs. It takes its colours from the active Omarchy theme and re-themes
itself live when the theme changes. It must feel instant (section 13).

manycommander replaces Double Commander on `SUPER + E` once it is usable. "Usable" means
the M1 and M2 acceptance checks in section 11 pass. The owner decided that tabs (M2) come
before the switch. M1 verifies the launch path with a trial binding.

### 1.1 Scope

| Milestone | Contents |
|---|---|
| M1 -- MVP | Two panels with navigation, sorting, selection and live refresh. F3 view and F4 edit through `$PAGER` / `$EDITOR`. F5 copy, F6 move/rename, F7 mkdir, F8 trash, Shift+F8 permanent delete with typed confirmation. Conflict handling. One file-operation job at a time on a worker thread, with progress and cancel. Command line. Live theming. The launch integration, verified on a trial binding. The performance targets in section 13. |
| M2 -- tabs, then the switch | Per-panel tabs. The panel paths, tabs and command history are restored on the next start. After the M2 checks pass, `SUPER + E` moves from Double Commander to manycommander. |
| Later (not designed here) | Archives, SFTP and other virtual filesystems, a built-in viewer or editor, a job queue with concurrent jobs, xattr/ACL/ownership preservation, hard-link preservation, sparse-file preservation, configurable keymap, mouse support, directory compare, multi-rename, a trash browser. |

Explicit non-goals: portability beyond Linux (NFR-PORT), and support for terminals without
truecolor beyond a basic fallback (section 7.4).

## 2. Environment assumptions

| Assumption | Evidence | Consequence |
|---|---|---|
| Omarchy writes the active palette to `~/.local/state/omarchy/current/theme/colors.toml`. The file holds `mode`, `accent`, `selection`, `muted`, four background keys (`background`, `dark_background`, `darker_background`, `lighter_background`), four foreground keys, the hues `red`, `yellow`, `orange`, `green`, `cyan`, `blue`, `magenta`, `brown`, and `bright_` variants of red, yellow, green, cyan, blue, magenta. It has no `black` or `white`. | Current theme file | Theme parser and fixtures (section 7.1) |
| `omarchy-theme-set` recreates `current/next-theme` inside `current/`, fills it, runs `rm -rf current/theme`, runs `mv current/next-theme current/theme`, then rewrites `current/theme.name`. | `/usr/bin/omarchy-theme-set` | The theme directory inode changes on every switch, so a watch on `current/theme` dies with the old directory. The watch must sit on `current/` and must filter events (section 7.2). |
| After the swap, `omarchy-theme-set` runs the app retint commands in parallel and waits for them. Then it runs `omarchy-hook theme-set <name>`, which runs every non-`.sample` file in `~/.config/omarchy/hooks/theme-set.d/` with `bash`. | `/usr/bin/omarchy-theme-set`, `/usr/bin/omarchy-hook` | The hook fires later than the swap. Omarchy reloads other TUIs by signal: `SIGUSR2` to btop, `SIGUSR1` to helix. |
| The terminal process survives a theme switch. Alacritty re-reads its config, kitty gets `SIGUSR1`, ghostty gets `SIGUSR2`. foot is recoloured by an OSC sequence written to the pty of its child process. | `omarchy-restart-terminal`, `omarchy-theme-set-foot` | manycommander keeps running across a switch. The foot OSC write can interleave with a frame, so a full redraw follows every reload (section 7.2). |
| TUIs launch through `xdg-terminal-exec` with app id `org.omarchy.<name>`. The Hyprland bindings DSL has `{ tui = "<cmd>", focus = true }`, which uses `omarchy-launch-or-focus-tui`. The current `SUPER + E` line is a user binding (`launch = "doublecmd"`) in `~/.config/hypr/bindings.lua`, not an Omarchy default. | `omarchy-launch-tui`, default `applications.lua`, user `bindings.lua` | Section 9 |
| Omarchy's terminals (Alacritty, Ghostty, Kitty, foot) support truecolor and the kitty keyboard protocol. kitty and ghostty bind `Ctrl+Tab`, `Ctrl+Shift+Tab` and `Ctrl+Shift+Enter` for their own tabs and windows. On the development laptop, Ghostty (the `xdg-terminal-exec` default) and foot are installed. | Terminal default configs, `xdg-terminals.list` | The keymap avoids those chords (section 8). Terminal-specific checks run in the installed terminals. |
| `/` and `/home` are separate btrfs subvolume mounts with different `st_dev`. `/tmp` and `/run/user/<uid>` are tmpfs. Removable media is often vfat or exfat. | `stat`, `findmnt` | Cross-filesystem moves are common. Filesystem identity needs both `st_dev` and the mount ID (section 4.2). |
| The kernel is a current Arch kernel (>= 6.8). | `uname -r` | `statx` with `STATX_MNT_ID_UNIQUE`, `renameat2`, `copy_file_range`, and `syncfs` error reporting are available (NFR-PORT). |

## 3. Architecture

manycommander is one Cargo package with a library crate and a thin binary. The library
holds everything testable without a terminal. The binary wires the terminal and the event loop.

```
src/
  main.rs          arg parsing, terminal setup/teardown, panic hook, signal wiring
  app/             App state, the event loop, update(event) -> effects, view(frame)
  panel/           listing, sort, selection, cursor, per-panel watcher
  fsops/           job planning and execution: copy, move, mkdir, trash, delete
  fsops/sys.rs     the only module with `unsafe`: statx, renameat2, copy_file_range, syncfs, O_PATH
  fsops/trash.rs   freedesktop.org Trash implementation
  theme/           colors.toml parsing, role mapping, reload sources
  cmdline/         command line editing, shell quoting, hand-off
  ui/              widgets: panels, dialogs, progress, function-key bar
  config.rs        ~/.config/manycommander/config.toml
```

### 3.1 Threads and events

manycommander uses plain threads and one `std::sync::mpsc` channel into the UI thread. It
uses no async runtime. The workload is a small number of long blocking syscalls, and threads
make cancel and backpressure explicit.

| Producer | Sends |
|---|---|
| Input thread (crossterm `read`) | `Key`, `Resize`, `Paste` |
| Listing threads | `ListingBatch { panel, generation, entries }`, `ListingDone`, `ListingFailed`, `LinkTargets { panel, generation, kinds }`, `DirSize`, `FreeSpace` |
| Panel watchers (the `inotify` crate, debounced 200 ms) | `DirChanged { panel }` |
| Theme watcher | `ReloadTheme` |
| Signal thread (`signal-hook` iterator; the handler only writes to a self-pipe) | `ReloadTheme` on `SIGUSR1`; `Quit` on `SIGTERM`, `SIGHUP`, `SIGINT`; `Suspend`/`Resume` on `SIGTSTP`/`SIGCONT` |
| File-operation worker | `Progress` (at most ~15 Hz), `Ask { question, reply: Sender<Answer> }`, `JobDone { report }` |

The UI thread owns all `App` state and makes **no filesystem syscalls** (P-1). Listing,
link classification, directory sizes and `statvfs` run on listing threads.

**Listing.** A load opens the directory, reads entries and `statx`es each one relative to
the directory fd (`AT_SYMLINK_NOFOLLOW`). It sends entries in batches, so the panel shows
rows before a 100k-entry directory is complete. Symlink targets are classified in a second
pass, after `ListingDone`. A stuck target (a dead mount) therefore never delays the listing.

**Stuck syscalls.** A thread blocked in the kernel cannot be cancelled. manycommander does
not pretend otherwise:

- `Esc` during a load returns the panel to its previous directory at once. The blocked
  thread is abandoned, and its late result is dropped by `generation`.
- At most four abandoned listing threads may exist. A further load into a directory whose
  previous load is still stuck is refused with "previous load of this directory is still
  blocked".
- A job whose worker is blocked shows "cancel pending -- the filesystem is not responding".
  No other job starts until the worker returns.

### 3.2 Paths and names

All paths are `PathBuf` / `OsString` end to end. No path is converted to `String` and back.
The UI displays names lossily. It escapes control characters (a newline shows as `\n`)
and invalid UTF-8 bytes (as `\xNN`) in a distinct colour. The command line inserts names
with a byte-oriented shell quote (section 6).

## 4. File-operation safety (highest risk)

This section is normative for `fsops`. It gives invariants first, then the mechanisms that
uphold them.

### 4.1 Invariants

| ID | Invariant |
|---|---|
| I-1 | **No lost data on move.** At every instant of a move, including cancel and error, every source file's complete content exists in at least one committed location. After a crash or power loss, the same holds on filesystems that honour `syncfs` (btrfs, ext4, xfs). tmpfs loses its content on power loss by nature. vfat and exfat are best-effort, because their metadata updates are not crash-atomic. |
| I-2 | **No partial destination file.** A destination path shows either its previous content or the complete new content, never a truncated or partially written file. The one exception is a filesystem that supports neither `RENAME_NOREPLACE` nor hard links (section 4.7, commit). There, the file is written under its final name, is visible while it is written, and is removed on failure or cancel. |
| I-3 | **Never overwrite without a decision.** An existing destination entry is replaced only after an explicit "Overwrite" (or "Overwrite all" / "Overwrite all older") answer that covers it. There is no exception. |
| I-4 | **Never destroy the source through the destination.** An operation whose source and destination are the same inode never truncates or unlinks it. A directory is never copied or moved into itself or a descendant, including through a bind mount. |
| I-5 | **Symlinks are objects, not paths.** Copy, move, trash and delete act on the link, never on its target. No traversal follows a symlink, in the last path component or in any earlier one. |
| I-6 | **No silent escalation.** Trash never copies across filesystems. Move never copy-deletes a mount point. A trash failure never deletes. The only permanent delete reachable from a failed trash is the typed `delete` confirmation for that one entry (section 4.10). |
| I-7 | **Honest reporting.** Every entry ends the job as done, skipped (with a reason) or failed (with the OS error). The report states the state the job left, for example "move cancelled: 812 moved, 40 still at source". |

### 4.2 Filesystem identity

`rename(2)` works only within one mount of one filesystem, and on btrfs only within one
subvolume. `st_dev` alone cannot tell these apart: bind mounts share `st_dev`, and
btrfs subvolumes that are not mount points have their own `st_dev`. manycommander uses the
pair `(st_dev, mnt_id)` from `statx` (`STATX_MNT_ID_UNIQUE`) for every identity decision.

| Child directory compared with its parent | Meaning | Copy traversal | Cross-filesystem move | Permanent delete | Trash top directory |
|---|---|---|---|---|---|
| Same `mnt_id`, same `st_dev` | Same filesystem and subvolume | descend | descend | descend | same |
| Same `mnt_id`, different `st_dev` | btrfs subvolume inside the mount | descend | descend (copy+delete allowed) | descend; removing the subvolume root itself may fail and is reported | the subvolume root is its own top directory |
| Different `mnt_id` | Mount point or bind mount | descend (reading is harmless; the cycle check in section 4.6 applies) | skip with reason "mount point" | skip with reason "mount point" | separate top directory |

manycommander always **tries `rename` first** and lets the kernel decide. The table only
decides what happens after `EXDEV`, and how far traversal goes.

### 4.3 Safe traversal and opening

Every job opens its source and destination directories once, at job start, as directory
fds. The user-visible panel path is resolved once, at that moment. From then on, every
operation is relative to a directory fd (`openat`, `statx`, `renameat2`, `unlinkat`,
`mkdirat`, `symlinkat`, `linkat`). No later step re-resolves an absolute path. A directory
component swapped for a symlink during the job therefore cannot redirect it (I-5).

Descending into a child directory: `openat(dirfd, name, O_DIRECTORY | O_NOFOLLOW |
O_CLOEXEC)`, then `statx` of the new fd, compared with the planned identity. A mismatch
fails the entry with "type changed".

Opening a regular file for reading:

1. `openat(dirfd, name, O_PATH | O_NOFOLLOW | O_CLOEXEC)`. `ELOOP` means the entry is now a
   symlink: fail it with "type changed", and do not follow.
2. `fstat` the `O_PATH` fd. If it is not a regular file, fail it with "type changed". A device
   node or FIFO is never opened for I/O, so opening one cannot have side effects or block.
3. Reopen the same inode for reading through `/proc/self/fd/<n>` with `O_RDONLY | O_CLOEXEC`,
   and confirm that `(st_dev, st_ino)` still matches.

The job holds one directory fd per level of the current path. At startup,
manycommander raises the `RLIMIT_NOFILE` soft limit to the hard limit. `EMFILE` fails the
entry, not the job.

### 4.4 Job lifecycle

Every verb becomes a `Job`. The UI thread builds a job from state it already has (marked
names, cursor, the other panel's path) and does no filesystem work (P-1).

| Verb | Phases |
|---|---|
| F5, F6 | Confirm (source set, editable destination; for a single source the destination may be a new name) -> Plan on the worker -> Execute -> Report |
| F7 | Name prompt -> Execute -> Report |
| F8 | Confirm ("Move N items to trash?", counts from panel state) -> Execute (the worker picks each item's trash, section 4.10) -> Report |
| Shift+F8 | Confirm -> Plan on the worker (counts of files, directories and bytes) -> Typed confirm showing those counts; the user must type `delete`, and there is no default-Enter path -> Execute -> Report |

**Plan** scans the source trees with `statx` and builds the entry list, the byte totals for
progress, and the pre-flight results (section 4.6). The user can cancel it.
**Execute** processes entries in order. Before it acts on an entry, it checks the entry's
identity again, because the plan can be stale. An entry that disappeared or changed type
fails with a clear reason.
**Report** gives the per-entry outcome, totals, and the cancel state. If anything was skipped
or failed, the UI shows a scrollable list. Both panels then re-read their directories.

Only one job runs at a time. The UI stays responsive during a job. A new F5-F8 request is
refused with "a job is running" until the job ends.

### 4.5 Questions, errors and cancel

The worker asks the UI through `Ask { question, reply }` and blocks until it gets an answer.

| Question | Raised on | Answers | Default focus |
|---|---|---|---|
| File exists | `EEXIST` for file over file, or over a symlink to a file. The dialog says "the link is replaced; its target is not touched". | Overwrite, Overwrite all, Overwrite all older, Skip, Skip all, Rename (pre-filled `name (1).ext`), Cancel job | Skip |
| Directory exists | `EEXIST` (empty target) or `ENOTEMPTY` for directory over directory | Merge, Merge all, Skip, Rename, Cancel job | Merge |
| Type mismatch | `EISDIR`, `ENOTDIR`, or directory over a symlink | Skip, Skip all, Rename, Cancel job | Skip |
| Error on entry | any other errno (`EACCES`, `ENOSPC`, `EIO`, ...) | Retry, Skip, Skip all of this errno, Cancel job | Retry for `ENOSPC`, otherwise Skip |

The dialog shows both sides with size, mtime and the read-only flag. manycommander never
replaces a directory tree with a file, or a file with a directory tree.

"Overwrite all older" compares mtimes at the coarser resolution of the two filesystems,
from `fstatfs` `f_type`: 2 seconds for vfat (`0x4d44`), 10 ms for exfat (`0x2011bab0`),
nanoseconds otherwise. It overwrites only when the destination is strictly older. Equal
mtimes skip.

**Cancel** sets an `AtomicBool`. The worker checks it between entries and between copy
chunks. On cancel it removes the temporary file of the file in progress, completes the move
batch in progress (section 4.8), and stops. A worker blocked in the kernel observes cancel
only when the syscall returns (section 3.1).

### 4.6 Pre-flight checks (plan phase)

| Check | Method | Result |
|---|---|---|
| Destination inside source | During the scan, record `(st_dev, st_ino)` of every source directory. Refuse the job if the destination directory's nearest existing ancestor-or-self has an identity in that set. Also walk `..` upward from the destination and refuse if a source directory appears. The set catches bind mounts of a source subtree; the walk catches the ordinary case before the scan reaches it. | The job is refused before any write |
| Same file | For each planned `(src, dst)` pair where `dst` exists, compare `(st_dev, st_ino)` | "Source and destination are the same file", skipped. A case-only rename is the one exception (section 4.8). |
| Special files | FIFO, socket, block or character device | Copy and cross-filesystem move: skipped with reason "special file". Same-filesystem move and trash use `rename` and work. |
| Directory cycle | Keep the `(st_dev, st_ino)` stack of the current path; a repeat means a bind-mount loop | Skip the subtree with a reason |
| Identity boundaries | Section 4.2 table | Per verb, as in the table |

### 4.7 Copy (F5)

Per regular file:

1. Open the source as in section 4.3 and record its snapshot `S0 = (st_dev, st_ino, size,
   mtime_ns, ctime_ns)`.
2. Create a temporary file in the destination directory:
   `.<name>.mc-partial-<random>`, `O_CREAT | O_EXCL | O_WRONLY | O_CLOEXEC`, mode `0600`.
   Retry with a new random suffix on `EEXIST`.
3. Copy with `copy_file_range` in a loop. Each call requests up to 16 MiB. The loop adds the
   returned count to both offsets, and continues until the call returns 0 and the copied
   total equals the size in `S0` (or the source reports EOF). It retries on `EINTR`. It
   falls back to `read`/`write` on `EXDEV`, `EOPNOTSUPP` or `ENOSYS`, and on `EINVAL` when
   source and destination are different inodes. `EINVAL` on the same inode fails the entry
   (I-4). Within one btrfs filesystem, the kernel shares extents (reflink). Cancel is
   checked between calls.
4. Apply metadata to the temporary file: permission bits via `fchmod` with setuid and
   setgid cleared (ownership is not preserved); atime and mtime via `futimens`.
5. **Commit** the temporary file to the final name:
   - No conflict: `renameat2(tmp, final, RENAME_NOREPLACE)`. On `EEXIST`, something appeared
     since the plan: unlink the temporary file and raise "file exists". After "Overwrite",
     start again from step 1.
   - Overwrite answered: `renameat(tmp, final)`, which atomically replaces the old entry. The
     old destination is never truncated or opened for writing. Other hard links to the old
     destination keep the old content. A symlink at the destination is replaced as a link.
   - `RENAME_NOREPLACE` unsupported (`EINVAL`, seen on some FUSE filesystems): use
     `linkat(tmp, final)`, which fails atomically with `EEXIST`, then unlink the temporary
     file. If `linkat` fails with `EPERM`, `EOPNOTSUPP` or `ENOTSUP` (no hard links), the
     filesystem supports neither. manycommander then remembers this for the destination
     filesystem, deletes the temporary file, and writes the rest of this job's files there
     directly under the final name with `O_CREAT | O_EXCL` (I-2 exception). In this
     **direct-write mode**, the `O_EXCL` create takes the place of the commit, and the file
     counts as committed only after its last byte and its metadata are written. A failed or
     cancelled file is unlinked. There is no `lstat`-then-`rename` path, because it could
     overwrite (I-3).
6. On any failure or cancel, unlink the temporary file. After a crash, a `.mc-partial-*`
   file can remain in the destination directory. The name pattern is documented, and
   manycommander never deletes such files automatically.

Directories: create the destination directory with `mkdirat(..., 0700)`, so the worker can
write into it even when the source directory is read-only. Recurse into it. Then, in
post-order, apply the source's permission bits and timestamps. When the answer to "directory
exists" is Merge, the existing directory's own permission bits and timestamps stay unchanged.

Symlinks: `readlinkat`, then `symlinkat` under a temporary name. The link target is
byte-identical; relative targets are not rewritten (the same as `cp -P`). The link is
committed with the same rules as a file. A symlink selected at the top level is copied as a
link too. The confirm dialog states "N symbolic links are copied as links".

Not preserved in M1, and stated in the F5 dialog help: ownership, ACLs, xattrs, hard-link
structure (each link becomes an independent file), and sparseness beyond what
`copy_file_range` keeps. On vfat and exfat, mode bits and timestamps are preserved only to
the filesystem's own resolution. Copy does not fsync (NFR-DUR).

### 4.8 Move and rename (F6)

manycommander first tries `renameat2(src, dst, RENAME_NOREPLACE)`:

| Result | Action |
|---|---|
| Success | Done. The move is atomic, and all metadata, hard links and xattrs survive. |
| `EEXIST` (file) | "File exists". Overwrite: `renameat` (atomic replace). |
| `EEXIST` (empty directory) or `ENOTEMPTY` | "Directory exists". Merge: move each child with these same rules, then `rmdir` the source directory if it is empty. |
| `EISDIR`, `ENOTDIR` | "Type mismatch" |
| `EXDEV` | Cross-filesystem move (below), limited by the section 4.2 table |
| `EBUSY` and other errors | Error question for this entry. Never copy+delete as a fallback for these (I-6). |

A planned entry whose `mnt_id` differs from its parent's is a mount point. It is skipped
with the reason "mount point" before any `rename` is attempted. `EBUSY` then only arises
for an entry that is busy for another reason.

**Cross-filesystem move** copies, then deletes, per file, with **group commit** for durability:

1. Copy the file as in section 4.7, steps 1-4.
2. **Change check, before commit.** `fstat` the source fd and compare it with `S0`. If size,
   mtime or ctime differ, unlink the temporary file, keep the source, and report "source
   changed during move". On vfat and exfat, timestamps are too coarse for this check to be
   reliable. There it is best-effort, and the F6 help says so.
3. Commit as in section 4.7, step 5. In direct-write mode, the change check of step 2 runs
   after the last byte is written, and a failed check unlinks the destination name.
4. Append the entry (source directory fd, name, `S0`) to the current **batch** only after it
   is committed. A file whose copy was cancelled or failed never joins a batch, so its
   source is never unlinked. Directories and symlinks created at the destination join the
   batch as well.
5. **Flush the batch** when it holds 64 files or 256 MiB, when a source directory is finished,
   when the job ends, and on cancel:
   1. Run `syncfs` on the destination directory fd. Every write, rename, `mkdir` and
      `symlink` of the batch, and every parent directory entry, is then durable on
      filesystems that honour `syncfs`.
   2. If `syncfs` fails, keep every source in the batch, report the error, and end the job.
      Writeback errors are not per-entry.
   3. Otherwise, for each file in the batch: `statx(srcdirfd, name, AT_SYMLINK_NOFOLLOW)`.
      Unlink the source with `unlinkat(srcdirfd, name)` only if identity, size, mtime and
      ctime still equal `S0`. Otherwise keep it and report "source changed; kept both".
      Linux has no unlink-by-fd, so a replacement between the `statx` and the `unlinkat` is
      still possible. That residual race is documented.
6. `rmdir` a source directory in post-order, after the flush that covers its children, if
   it is empty. A directory that still holds skipped or failed entries stays, and the report
   lists it.

Group commit replaces a per-file `fsync` of file, directory and parent. One `syncfs` per
batch makes new files, new directories and renames durable together. The cost is one
filesystem flush per 64 files instead of three flushes per file (P-7).

Consequences for the user: a cancelled cross-filesystem move leaves a partially moved tree.
After the final flush, every file is in exactly one place. A crash can leave the files of the
last unflushed batch in both places, never in neither. The state is resumable: running F6
again on the rest merges.

**Case-only rename** (`Foo` -> `foo`) on a case-insensitive filesystem: the same-file check
finds that `dst` is the same inode as `src`. manycommander renames via an intermediate
unique name in the same directory. If the second rename fails, the report names the
intermediate path, so the user can find the file.

**Rename in place** (Shift+F6) is F6 with a single source and the destination fixed to the
same directory.

### 4.9 Make directory (F7)

The input may contain `/` and creates missing parents (`a/b/c`). Validation rejects an
empty name, `.`, `..` and NUL. `mkdirat` uses mode `0777`, so the umask applies. If the name
already exists, manycommander reports it and moves the cursor to it. Otherwise the cursor
moves to the new directory.

### 4.10 Trash (F8)

manycommander implements the freedesktop.org Trash specification 1.0 itself, compatible
with GIO (`gio trash`, Nautilus). Section 10 explains why it does not use a crate.

**Trash directories are opened, not resolved.** Every trash directory, and its `files/` and
`info/` subdirectories, is opened with `O_DIRECTORY | O_NOFOLLOW` and checked with `fstat`
on the fd. A symlink anywhere in that chain is refused. `files/` and `info/` are created
`0700` when missing.

**Choosing the trash for entry `p`** (the entry itself, never a symlink target). "Same
domain" means equal `(st_dev, mnt_id)` (section 4.2).

1. **Home trash**, `$XDG_DATA_HOME/Trash` (default `~/.local/share/Trash`, created `0700`),
   if it is in the same domain as `p`.
2. Otherwise find the **top directory** of `p`: walk up from `p`'s parent while the parent is
   in the same domain as `p`. This matches GIO's `st_dev` walk and also stops at mount
   boundaries.
3. **Method 1:** if `$top/.Trash` exists, is a directory, is not a symlink, and has the
   sticky bit, use `$top/.Trash/$uid`, creating it `0700` if missing. If `$top/.Trash`
   exists but fails those checks, skip method 1. The specification requires that; it
   indicates a misconfigured or hostile shared trash.
4. **Method 2:** `$top/.Trash-$uid`, created `0700` if missing. If it exists, it must be a
   directory, not a symlink, and owned by the user.
5. If no method works, the trash question offers Skip, or "Delete permanently...", which
   opens the typed `delete` confirmation for that entry only (I-6).

**Trashing one entry:**

1. Pick a name `N`: the original basename, then `N.2`, `N.3`, ... A name is free only when
   neither `info/N.trashinfo` nor `files/N` exists (GIO's rule). If `N.trashinfo` would
   exceed `NAME_MAX` (255 bytes), shorten `N` by bytes until `N`, a collision suffix and
   `.trashinfo` fit. `Path` still holds the original, unshortened bytes. Reserve the name by
   creating `info/N.trashinfo` with `O_CREAT | O_EXCL`.
2. Write `[Trash Info]`, `Path=` and `DeletionDate=` (local time, `YYYY-MM-DDThh:mm:ss`,
   formatted with the `jiff` crate).
   `Path` is the original path, absolute for the home trash and relative to `$top` for a
   top-directory trash. It is percent-encoded byte-wise as GIO does it
   (`g_uri_escape_string` with `/` allowed): unreserved ASCII and `/` stay literal, and every
   other byte, including non-UTF-8 bytes, becomes `%XX`.
3. `fsync` the info file, then `fsync` the `info/` directory.
4. `renameat2(p, files/N, RENAME_NOREPLACE)`. On `EEXIST`, remove the info file and pick the
   next name. On `EXDEV` (the domain check was defeated, for example by a concurrent mount),
   remove the info file and try the next method. That is a normal failure, not a bug. On any
   other failure, remove the info file and report the entry.
5. `fsync` the `files/` directory.

A directory is trashed whole with one `rename`, so trashing is instant regardless of size.
An entry inside a trash directory is refused with "already in trash". M1 has no trash
browser and no restore; restore works through `gio trash --restore` or Nautilus. M1 does
not update the optional `directorysizes` cache.

### 4.11 Permanent delete (Shift+F8)

1. Phases as in section 4.4: confirm, plan, then the typed `delete` confirmation showing
   files, directories and bytes.
2. Traversal follows section 4.3. Symlinks are unlinked, never followed.
3. Traversal follows the section 4.2 table. It does not descend into a different `mnt_id`
   (mount point or bind mount); that entry is skipped with a reason. It descends into a
   btrfs subvolume; removing the subvolume root may then fail, and the failure is reported.
4. `unlinkat` files and `unlinkat(AT_REMOVEDIR)` directories in post-order. manycommander
   does not `chmod` read-only directories to force deletion; the entry fails with the OS
   error.

## 5. Panels

Each panel holds: current directory, entries, sort key and direction, a set of marked names,
a cursor (by name, so it survives refresh), the hidden-file toggle, the load generation, and
per-panel directory history.

- Columns: name, extension, size (directories show `<DIR>` until computed), mtime, mode.
- Sort: name (default, directories first), extension, size, mtime. `Ctrl+F3`-`Ctrl+F6`
  select the key, and the same key again reverses the direction. Names sort naturally
  (digit runs compare as numbers) and case-insensitively. A sort order is an index
  permutation over the stored entries (P-4).
- Refresh: an inotify watch on each panel's directory, debounced 200 ms, plus `Ctrl+R`.
  The cursor stays on the same name when that name still exists. Marks whose names no longer
  exist are dropped. If the current directory is deleted, the panel goes to the nearest
  existing ancestor.
- Footer: marked count and bytes; free space (`statvfs`, from a listing thread).
- `Space` on a directory computes its size on a listing thread (cancellable) and marks it.

## 6. Command line and hand-off

A single-line editor sits above the function-key bar. Printable keys without a binding go
to it (the Total Commander behaviour). Key ownership depends on whether the line is empty
(section 8).

- `Enter` with text: if the text is `cd <path>`, the active panel changes directory
  internally. The path expands only a leading `~` and `$VAR` / `${VAR}`; there is no command
  substitution and no globbing. Otherwise manycommander suspends the TUI and spawns argv
  `[$SHELL, "-c", text]` (default shell `/bin/sh`), with the active panel's directory as
  working directory and inherited stdio. It then prints `[exit N] press Enter to return` and
  restores the TUI. Both panels refresh afterwards.
- Inserting names (`Ctrl+Enter` / `Alt+Enter` for the name, `Alt+P` for the full path) uses
  a byte-oriented single-quote shell quote: the name is wrapped in `'...'`, and each `'` in
  it becomes `'\''`. Newlines and invalid UTF-8 bytes pass through unchanged inside the
  quotes, so the shell receives exactly one argument.

**Suspend and resume** is shared by the command line, F3, F4 and `SIGTSTP`: leave the
alternate screen, pop the keyboard protocol, disable raw mode, run or stop, then re-enable
everything and fully redraw. A child killed by a signal still leads to a restore. A panic
hook restores the terminal before it prints the panic. `SIGTERM`, `SIGHUP` and `SIGINT` quit
through the same restore path. `SIGKILL` of manycommander itself cannot be handled. Under
`xdg-terminal-exec -e` the terminal window closes with the process, so no broken shell
remains.

F3 runs `$PAGER` (default `less`) and F4 runs `$EDITOR` (default `nvim`, then `vi`). The
variable is split with shell-word rules into argv, and the file path is appended as a
separate argument; no shell is involved. Shift+F4 prompts for a new file name and opens it
the same way.

`Enter` on a file that is not executable runs `setsid -f xdg-open <path>` with stdio on
`/dev/null`. A helper thread reaps the short-lived `setsid` process, so no zombie remains.
`Enter` on an executable file opens it the same way and does not run it; running needs the
command line.

## 7. Theming

### 7.1 Palette to roles

`theme::Palette` parses `colors.toml` with the `toml` crate into a map of known keys. Unknown
keys are ignored. Missing keys resolve through fallbacks. Each UI role is defined once:

| Role | Source key (fallback chain) |
|---|---|
| Panel background | none: `Color::Reset`, so the terminal's background and opacity show through. `paint_background = true` in the config uses `background` instead. |
| Normal file | `foreground` |
| Directory | `bright_foreground`, bold |
| Executable | `green` |
| Symlink / broken symlink | `cyan` / `red` |
| Hidden entry | `dark_foreground` |
| Marked entry | `yellow`, bold, with a `▸` marker glyph |
| Cursor row, active panel | fg `background`, bg `accent` (-> `blue`) |
| Cursor row, inactive panel | bg `selection` (-> `lighter_background`) |
| Active panel border and path | `accent` (-> `blue`) |
| Inactive panel border and path | `muted` (-> `dark_foreground`) |
| Metadata columns | `light_foreground` (-> `foreground`) |
| Dialog background / border | `dark_background` (-> `background`) / `accent` (-> `blue`) |
| Error / warning text | `red` / `yellow` |
| Function-key bar labels | fg `foreground` on bg `lighter_background` (-> `selection`), key numbers in `accent` |
| Command line prompt | `accent` (-> `blue`) |

`mode = "light"` needs no special handling, because the roles refer only to semantic keys.
When `colors.toml` is absent or does not parse, every role falls back to a named ANSI colour
(for example directory -> `Color::White` bold, cursor row -> `Color::Blue`). The terminal
renders those with its own Omarchy-retinted palette.

### 7.2 Live reload

Two sources feed one idempotent `ReloadTheme` event. The UI thread asks a listing thread to
read and parse `colors.toml` (P-1). When the parsed palette differs from the current one, the
UI clears and fully redraws. The full redraw also repairs any frame that an external OSC
recolour (foot) interleaved with.

1. **Watcher (primary; no installation needed).** A non-recursive inotify watch on
   `~/.local/state/omarchy/current/`. The theme switch produces create and delete events for
   `next-theme`, `IN_DELETE` for `theme`, `IN_MOVED_TO` for `theme`, and `IN_MODIFY` /
   `IN_CLOSE_WRITE` for `theme.name`. Only `IN_MOVED_TO` or `IN_CREATE` for `theme`, and
   `IN_CLOSE_WRITE` for `theme.name`, trigger a reload, after a 50 ms debounce. An
   `IN_Q_OVERFLOW` also triggers a reload, because it may hide one of those events. Everything
   else, including every `next-theme` event and `IN_DELETE`, is ignored. If `colors.toml`
   is missing or does not parse, the current palette stays. If `current/` does not exist at
   startup, the watcher watches the nearest existing ancestor and re-arms when it appears.
   Omarchy never edits `colors.toml` in place, so the parent watch does not need to see
   in-place edits.
2. **Signal (optional hook).** `SIGUSR1` triggers a reload, following Omarchy's pattern for
   btop and helix. The signal thread (section 3.1) turns it into `ReloadTheme`. The repository
   ships `contrib/omarchy/theme-set-hook.sh` containing `pkill -USR1 -x manycommander || true`.
   The user copies it to `~/.config/omarchy/hooks/theme-set.d/manycommander`; manycommander
   never writes to the user's config. The hook fires only after all app retints, so it is
   slower than the watcher. It is the fallback for systems where the watch cannot be placed.

Flags: `--theme-file <path>` (or `MANYCOMMANDER_THEME`) selects the file to parse. In that
mode, the watcher watches the file's parent and also reacts to `IN_CLOSE_WRITE` and
`IN_MOVED_TO` of the file's own name, so in-place edits work. `--no-theme-watch` disables
the watcher; `SIGUSR1` still works.

### 7.3 Terminal retint

`omarchy-theme-set` also retints the terminal emulator (section 2). With the default
`paint_background = false`, the terminal owns the background, and manycommander owns every
foreground and highlight role. Both change in the same theme switch.

### 7.4 Colour depth

When `COLORTERM` is `truecolor` or `24bit`, roles use RGB. Otherwise every role uses its
ANSI fallback from section 7.1. There is no 256-colour approximation. `NO_COLOR` forces the
ANSI fallback without colour attributes; bold and the marker glyph keep cursor and marks
distinguishable (NFR-TERM).

## 8. Keymap

**Ownership rule.** When the command line is empty, panel bindings apply. When it holds
text, the line-editing keys below go to the line, and the panel keeps only cursor movement
(`Up`, `Down`, `PgUp`, `PgDn`) and the F-keys. `Esc` clears the line.

| Key | Line empty | Line has text |
|---|---|---|
| `Enter` | Enter directory / `xdg-open` a file | Run the command line |
| `Backspace` (also `Ctrl+H` / `0x08`) | Parent directory | Delete character before the cursor |
| `Left` / `Right`, `Home` / `End` | Home/End: first/last entry | Move within the line |
| `Ctrl+A` | Mark all | Line start |
| `Ctrl+E` | -- | Line end |
| `Ctrl+U` | Swap panels | Delete to line start |
| `Ctrl+K` | -- | Delete to line end |
| `Ctrl+W` | M2: close tab | Delete word before the cursor |
| `Ctrl+P` / `Ctrl+N` | Previous / next command from history into the line | Same |

Always active:

| Key | Action |
|---|---|
| `Tab` | Switch active panel |
| `Up`/`Down`/`PgUp`/`PgDn` | Move the cursor |
| `Insert`, `Shift+Down` | Toggle mark, move down |
| `Space` | Toggle mark (directories: also compute size) -- only when the line is empty; otherwise a space character |
| `Alt+=` / `Alt+-` / `Alt+*` | Mark by glob / unmark by glob (default glob `*` unmarks all) / invert marks |
| `Ctrl+S` | Quick search: jump to the next name that matches the typed prefix |
| `Alt+.` | Toggle hidden files |
| `Ctrl+R` | Re-read both panels |
| `Alt+Left` / `Alt+Right` | Directory history back / forward (per panel) |
| `Ctrl+PgUp` | Parent directory |
| `Ctrl+Enter` (fallback `Alt+Enter`) | Insert the quoted name under the cursor into the line |
| `Alt+P` | Insert the quoted full path under the cursor into the line |
| `Ctrl+O` | Show the terminal output of the last command |
| `Ctrl+F3`-`Ctrl+F6` | Sort by name, extension, size, mtime |
| `F1` | Help overlay: keymap, file-operation semantics, durability contract |
| `F3`, `F4`, `Shift+F4` | View, edit, edit new file |
| `F5`, `F6`, `Shift+F6`, `F7`, `F8`, `Shift+F8` | Copy, move, rename in place, mkdir, trash, permanent delete |
| `F10`, `Alt+X` | Quit (confirms when a job is running, then cancels it) |

Protocol notes: `Shift+F*`, `Ctrl+F*` and `Ctrl+PgUp` use the xterm modifier encoding and
work without the kitty keyboard protocol. Only `Ctrl+Enter` needs the protocol, and it has
the `Alt+Enter` fallback. manycommander enables the protocol with crossterm's
`PushKeyboardEnhancementFlags` when the terminal supports it. No binding uses
`Ctrl+Tab`, `Ctrl+Shift+Tab`, `Ctrl+Shift+Enter` or any `SUPER` chord; kitty, ghostty and
Hyprland own those. Raw mode disables `IXON`, so `Ctrl+S` reaches the application. The plan
checks every chord against the Omarchy default configs of the four terminals.

M2 adds `Ctrl+T` (new tab, duplicate of the current one), `Ctrl+W` (close tab, line empty),
`Alt+PgUp` / `Alt+PgDn` (previous/next tab) and `Alt+1`-`Alt+9` (go to tab). `Alt+[` is
avoided because in legacy encoding it is the CSI introducer. A panel with more than
one tab shows a tab bar row above its header.

## 9. Launch and integration

- Installation: `cargo install --path . --root ~/.local` puts the binary in `~/.local/bin`.
  The binding needs the binary on the Hyprland session's `PATH`. The plan verifies that; if
  it is not on that `PATH`, the binding uses the absolute path.
- The binding (applied by the user in `~/.config/hypr/bindings.lua`):

  ```lua
  o.bind("SUPER + E", "File manager (dual pane)", { tui = "manycommander", focus = true })
  ```

  `tui` launches through `omarchy-launch-tui`, so the window's class is
  `org.omarchy.manycommander`. With `focus = true`, a second press focuses the running
  instance instead of starting another one.
- M1 verifies this on a trial chord that is free in the Omarchy defaults. In M2, the same
  line replaces the Double Commander line on `SUPER + E`. Double Commander stays installed,
  and the switch is one reversible line.
- Initial directories: the process working directory on the left, `$HOME` on the right.
  `manycommander <left> [<right>]` overrides both. M2 restores the last paths and tabs from
  `~/.local/state/manycommander/state.toml`.

## 10. Alternatives considered

| Alternative | Decision |
|---|---|
| Midnight Commander with a generated truecolor skin | Rejected. mc does not reload skins live, and its keymap and dialog conventions differ from Total Commander. The file-operation invariants of section 4 would also have to be audited in a foreign C codebase instead of being owned and tested here. |
| yazi, broot, xplr | Rejected as a base. They are single-pane or Miller-column designs; a dual-pane F-key workflow would fight their model. |
| The `trash` crate | Rejected for M1. manycommander needs guarantees it tests itself: no copy fallback across devices (I-6), the link and not its target (I-5), `(st_dev, mnt_id)` domain detection for btrfs subvolumes and bind mounts, the symlink checks on every trash directory, and GIO-compatible byte-wise `Path` encoding. The needed part of the specification is small. Revisit if the crate is verified to match. |
| `std::fs::copy` / `fs_extra` | Rejected. They open the destination with `O_TRUNC`, which breaks I-2 and, when source and destination are the same inode, destroys the source (I-4). They also have no chunk-level cancel or progress. |
| tokio | Rejected. The workload is a few blocking syscalls; threads plus one channel are simpler and make cancellation explicit. |
| Theme reload by hook only | Rejected as the only source. It needs an installation step and fires after all app retints. The watcher is primary; the hook is an optional fallback. |
| Per-top-level-item cross-filesystem move (`mv` style) | Rejected. It needs double space for the whole item, and on cancel it leaves a partial destination tree to clean up. |
| Per-file `fsync` of file, directory and parent on move | Rejected for P-7. Three flushes per file make small-file moves many times slower than `mv`. Group commit with one `syncfs` per batch gives the same I-1 guarantee. |

## 11. Acceptance checks

M1 is accepted when all M1 checks pass. *auto* checks are automated tests in the
repository, run by the local check gate (section 12). In that gate a skipped test counts as
a failure; a test that cannot run elsewhere skips there with a printed reason. *bench*
checks run through the benchmark harness in release mode under the section 13.1 reference
conditions. *manual* checks run once on an Omarchy machine, and the implementation plan
records the result.

### 11.1 File operations

| ID | Check | Type |
|---|---|---|
| A-FS-1 | On btrfs and tmpfs: copy of a tree with regular files, empty directories, a read-only directory, relative and absolute symlinks, a broken symlink and a FIFO. Files are byte-identical with mode and mtime preserved to the nanosecond. Links are links with identical targets. The FIFO is skipped and reported, and the worker does not hang. | auto |
| A-FS-2 | Overwrite answered on a destination that is a hard link of another file: the other link keeps the old content, and the destination has the new content. | auto |
| A-FS-3 | Copy where destination and source are the same inode (hard link or same path): the entry is refused, and the source is byte-identical afterwards. | auto |
| A-FS-4 | Copy or move of a directory into its own descendant is refused before any write. This holds through a symlinked destination path, and through a bind mount of a source subdirectory (the bind-mount case runs under `unshare -rm`). | auto |
| A-FS-5 | Failpoint sweep over a cross-filesystem move of a tree (`MC_XDEV_DIR` on a different filesystem, for example tmpfs): inject cancel or an I/O error at every step boundary (each chunk, commit, `syncfs`, `statx`, unlink), in both commit modes (temporary file and direct write). Each run first asserts that the injected step was reached. The general predicate: every file whose source was unlinked has a destination whose hash equals the pre-move hash; every destination name that exists has the pre-move hash; every source that still exists has the pre-move hash; no `.mc-partial-*` name remains. Each injection also has a step-specific predicate, for example: cancel at the first chunk leaves every source in place and no destination name for that file; a `syncfs` error unlinks no source of that batch; a changed source at the unlink step keeps both. | auto |
| A-FS-6 | Same-filesystem move of a directory preserves its inode (rename used), including hard links inside it. | auto |
| A-FS-7 | Fixture of two btrfs subvolumes created unprivileged (`btrfs subvolume create`) in a btrfs test directory. Move from subvolume A to subvolume B of a tree with a file, a directory, a symlink and a nested subvolume. `rename` returns `EXDEV`. The move completes through the cross-filesystem path, the nested subvolume's files are moved (not skipped as a mount point), and the source tree is empty afterwards except for the nested subvolume root if the kernel refuses its removal. | auto (needs a btrfs test directory) |
| A-FS-8 | A destination file appears between plan and commit: the commit does not replace it, and "file exists" is raised. | auto (failpoint) |
| A-FS-9 | Source changed during a cross-filesystem move. (a) Failpoint: the snapshot differs at the pre-commit check: no destination is committed, and the source is kept. (b) A real writer thread appends to the source during the copy on btrfs -> tmpfs: same outcome. (c) The source is replaced by rename between commit and flush: the new inode is kept, and the report says "source changed; kept both". | auto |
| A-FS-10 | Names with a newline, a leading `-`, a single quote, invalid UTF-8 and a 255-byte length survive copy and move byte-exactly and display escaped (panel snapshot). Trash round-trip: the `.trashinfo` `Path` decodes to the original bytes, including for the 255-byte name, whose trash basename is shortened. A command-line insert of such a name followed by `printf '%s\0'` yields exactly one argument equal to the name. | auto |
| A-FS-11 | Directory over non-empty directory on a same-filesystem move raises "directory exists" (from `ENOTEMPTY`), and Merge completes. File over directory raises "type mismatch". | auto |
| A-FS-12 | Copy and move onto a filesystem without `RENAME_NOREPLACE` and without hard links (the `sys` failpoints return `EINVAL` from `renameat2` and `EPERM` from `linkat`): the test asserts that direct-write mode ran; no existing file is overwritten without an answer; a cancelled file leaves no destination name; on a move, the source of a cancelled file is not unlinked. | auto |
| A-FS-13 | A source directory component replaced by a symlink during a copy, move or delete is not followed: the entry fails with "type changed". | auto (failpoint) |

### 11.2 Trash and delete

| ID | Check | Type |
|---|---|---|
| A-TR-1 | Home trash: spec-valid `.trashinfo` (GIO-encoded absolute `Path`, `DeletionDate`); a name collision on either `info/` or `files/` picks `N.2`; `gio trash --list` shows the entries, and `gio trash --restore` restores a name containing a newline and a non-UTF-8 byte to its original path. | auto + manual (`gio`) |
| A-TR-2 | Trash of a symlink moves the link; the target is untouched. | auto |
| A-TR-3 | Top-directory trash: on tmpfs and on a loop-mounted vfat image mounted with the user's `uid`, the trash is `$top/.Trash-$uid` with a relative `Path`, and `gio trash --restore` restores it. With a prepared sticky `$top/.Trash`, method 1 (`.Trash/$uid`) is used. | manual |
| A-TR-4 | No usable trash (unwritable top directory): F8 does not delete, and it offers only Skip or the typed permanent-delete confirmation. | auto |
| A-TR-5 | A symlinked `$XDG_DATA_HOME/Trash`, `files/` or `info/` is refused and nothing is moved. | auto |
| A-DEL-1 | Shift+F8 without typing `delete` does nothing. With it, the tree is removed. A symlink to a directory outside the tree leaves that directory intact. A bind mount inside the tree (under `unshare -rm`) is skipped. | auto |

### 11.3 UI, theme and launch

| ID | Check | Type |
|---|---|---|
| A-UI-1 | A panel on an unresponsive mount (a stopped FUSE filesystem) shows "loading"; `Esc` returns the panel to its previous directory within 100 ms; a second load of the same directory is refused while the first is stuck; other directories still load. | manual |
| A-UI-2 | External changes (`touch`, `rm` in another terminal) appear within 1 s, and the cursor stays on the same name. | auto (listing layer) + manual |
| A-UI-3 | F3, F4 and the command line suspend and restore the terminal, including after the child is killed with `SIGKILL`. `SIGTSTP`/`SIGCONT` sent to manycommander leave and restore the terminal. `SIGTERM` exits with the terminal restored. | manual |
| A-TH-1 | With `paint_background = false`: `omarchy-theme-set <other theme>` changes the active-panel border (an RGB role) within 200 ms of the `mv` of `current/theme`, with the watcher on and no hook installed. With `--no-theme-watch` and the hook installed, it changes within 200 ms of the hook process start. | manual (timed through NFR-OBS log lines) |
| A-TH-2 | Replay of the full theme-set event sequence (section 7.2) in a temporary state directory yields exactly one effective palette change and never an empty palette. | auto |
| A-TH-3 | Fixtures: the real `colors.toml` shape (with `orange`, `brown`, `mode`); one without `accent` (roles fall back to `blue`); a missing file and an unparsable file (ANSI fallback, no crash). | auto |
| A-LN-1 | The section 9 binding on the trial chord opens manycommander; a second press focuses it; `hyprctl clients -j` shows the focused window's `class` as `org.omarchy.manycommander`. | manual |
| A-PUB-1 | The repository contains no tenant, client, host or private-repository names (publication gate clean). | auto (pre-push) |

### 11.4 Performance (section 13.1)

| ID | Check | Type |
|---|---|---|
| A-P-1 | Scripted navigation session in a 100k-entry directory, idle and during a 10 GiB cross-filesystem copy (btrfs to an ext4 loop image, so no reflink): p99 key-to-flush <= 16 ms. The harness asserts the job is still running while the samples are taken. | bench |
| A-P-2 | Start to first full flush <= 50 ms, read from the first-flush timestamp in the log (not process exit). | bench |
| A-P-3 | 100k entries listed and sorted <= 300 ms; first batch visible <= 50 ms. | bench |
| A-P-4 | Re-sort or filter of 100k entries <= 30 ms. | bench |
| A-P-5 | 60 s idle: `/proc/<pid>/status` `voluntary_ctxt_switches` and `/proc/<pid>/stat` `utime`/`stime` are unchanged between two samples 60 s apart. | bench |
| A-P-6 | RSS <= 40 MB with both panels on 100k-entry directories. | bench |
| A-P-7 | Copy: a 4 GiB file within 10 % of `cp`; 50k 4 KiB files within 1.5x of `cp -r`; a 4 GiB same-filesystem btrfs copy in under 1 s. Cross-filesystem move of 50k 4 KiB files within 2x of `mv`. | bench |
| A-P-8 | A scripted copy of a multi-chunk file delivers at most 15 progress updates per second. | auto |

### 11.5 M2

Tabs open, close and switch per panel with the section 8 keys. Paths, tabs and command
history restore after restart. A restored path that no longer exists falls back to the
nearest existing ancestor. After these pass, `SUPER + E` switches (section 9).

## 12. Repository and publication

- The repository is private now and becomes public later. From the first commit, it
  contains no tenant, client, host or private-repository names. Examples use generic
  paths (`~/Documents`, `/mnt/usb`).
- License: Apache-2.0 (present).
- Verification is local-first. `scripts/check.sh` is the gate: `quick` (format, lint,
  unit tests), `full` (everything automated, with skips counted as failures) and `bench`
  (section 11.4). The repository's own pre-push hook runs `full`. GitHub Actions comes when
  the repository goes public; it then runs the same command list, and tests that need this
  laptop's btrfs, user namespaces or FUSE skip there with a reason. Adopting shared reusable
  CI is out of scope.
- Agent instructions: the shared agent baseline is vendored without the private overlay,
  because the repository is designated public. Repository-owned content in `CLAUDE.md` and
  `AGENTS.md` stays outside the vendored marker block.

## 13. Non-functional requirements

manycommander must feel instant. Where a requirement conflicts with portability, the
requirement wins: portability is an explicit non-goal (NFR-PORT).

### 13.1 Performance and responsiveness

Reference conditions: release build, local NVMe, warm page cache, the Omarchy laptop the
project is developed on. Section 11.4 holds the checks.

| ID | Requirement | Target |
|---|---|---|
| P-1 | Key to rendered frame | p99 <= 16 ms while navigating a 100k-entry directory, also while a job runs |
| P-2 | Start to first full frame | <= 50 ms, both panels on ~1k-entry directories |
| P-3 | Large directory listing | 100k entries listed and sorted <= 300 ms; rows appear in batches; the UI never waits |
| P-4 | Re-sort or filter | <= 30 ms for 100k entries |
| P-5 | Idle cost | 0 % CPU and no periodic wakeups when idle |
| P-6 | Memory | <= 40 MB RSS with both panels on 100k-entry directories |
| P-7 | Copy and move throughput | Large files within 10 % of `cp`; 50k small files within 1.5x of `cp -r`; same-filesystem btrfs copies of large files in near-constant time (reflink); small-file cross-filesystem moves within 2x of `mv` |
| P-8 | Progress cost | Progress messages capped at 15 Hz |
| P-9 | Theme reload | New palette on screen <= 200 ms after the theme directory swap |

Design consequences:

- The UI thread makes no filesystem syscalls (section 3.1).
- The view renders only visible rows. Each panel stores entries once, in a compact form
  (name bytes plus a small fixed metadata struct). Sort orders are index permutations.
- The event loop blocks on its channel. A tick timer runs only while a spinner or a
  progress dialog is visible (P-5).
- Moves use group commit (section 4.8) (P-7).
- Release profile: `lto = "thin"`, `codegen-units = 1`, `panic = "unwind"` (NFR-REL).

### 13.2 Other requirements

| ID | Area | Requirement |
|---|---|---|
| NFR-PORT | Portability | Non-goal. Linux only, current Arch kernel (>= 6.8), x86_64 first. Linux-specific syscalls (`statx`, `renameat2`, `copy_file_range`, `syncfs`, `O_PATH`, inotify) are used directly. The project tracks the latest stable Rust; there is no MSRV promise. |
| NFR-REL | Reliability | No panic leaves the terminal in raw mode or the alternate screen. A panic on a worker or listing thread fails that job or load, reports it, and leaves the app running. `EMFILE`, `ENOMEM` and `ENAMETOOLONG` during traversal fail the entry, not the process. |
| NFR-SEC | Security | Filenames never reach a shell unquoted (section 6). F3/F4 and `xdg-open` spawn by argv. No network access and no telemetry. `unsafe` is confined to `fsops/sys.rs`; every other module has `#![forbid(unsafe_code)]`. |
| NFR-SUP | Supply chain | `cargo-deny` in the local gate (and later CI) checks advisories, licences (compatible with Apache-2.0) and duplicate heavy dependencies. Each new dependency states its reason in the commit that adds it. |
| NFR-RES | Resource hygiene | inotify watches are bounded: one per visible panel plus the theme watch (M2: only visible tabs are watched). Directory fds during traversal are bounded by tree depth under the raised `RLIMIT_NOFILE` (section 4.3). At most four abandoned listing threads (section 3.1). |
| NFR-TERM | Terminal | Fully usable at 80x24; below that, columns drop without a panic. `NO_COLOR` is honoured. Cursor and marked rows stay distinguishable without colour (marker glyph and bold). |
| NFR-OBS | Observability | `--log <file>` enables a debug log with per-frame key-to-flush latency, listing and job timings, and theme reload timestamps. It is off by default and never contains file contents. `--exit-after-first-frame` supports A-P-2. |
| NFR-DUR | Durability contract | Copy does not fsync (the same as `cp`). Move upholds I-1 through group commit (section 4.8). The F1 help states both. |

## Appendix A. Review resolution

The first draft (commit `a421ba4`) went through an independent adversarial model review on
2026-09-27. The table records how each finding was resolved. Findings 1-16 were marked
required; 17-25 were suggestions. Local probe values from the review that came from a
defective `statx` probe were re-checked with `stat`/`findmnt` before use. `/` and `/home` are
separate mounts (mount IDs 33 and 63), as section 2 states. The review's further claim that
the user's home directory below `/home` was a second mount of the `@home` subvolume did
not hold: it lies inside the `/home` mount. The underlying `rename(2)` rule for mount points
holds as documented.

| # | Finding | Resolution |
|---|---|---|
| 1 | Move durability misses new directories, symlinks and fsync errors | Accepted, resolved by group commit with `syncfs` (4.8); `syncfs` failure keeps sources; I-1 scoped per filesystem (4.1) |
| 2 | Change check after commit; mtime unreliable; path race before unlink | Accepted: check before commit; ctime added; vfat/exfat best-effort; `statx`+`unlinkat` on the directory fd with the residual race stated (4.8); A-FS-9 extended |
| 3 | `st_dev` is not the rename domain | Accepted: `(st_dev, mnt_id)` model (4.2); `EXDEV` into trash is a normal failure (4.10) |
| 4 | Bind mount defeats the destination-inside-source walk | Accepted: scan-set check plus walk (4.6); A-FS-4 extended |
| 5 | `ENOTEMPTY`/`EISDIR`/`ENOTDIR` not mapped | Accepted (4.5, 4.8); A-FS-11 |
| 6 | Short `copy_file_range` and `EINVAL` handling | Accepted (4.7 step 3) |
| 7 | Racy rename fallback breaks I-3; vfat claim wrong | Accepted: fallback removed; `O_EXCL` direct write as the named I-2 exception (4.1, 4.7); A-FS-12 |
| 8 | Only delete is safe against a swapped parent; open before fstat | Accepted: directory-fd traversal and `O_PATH` open for all verbs (4.3); A-FS-13 |
| 9 | Trash layout diverges from the specification and GIO | Accepted in full (4.10); A-TR-1, A-TR-3, A-TR-5 |
| 10 | I-6 contradicts the trash failure offer | Accepted: I-6 reworded (4.1) |
| 11 | Confirm needs data only the scan has | Accepted: per-verb phases, typed confirm after the scan (4.4) |
| 12 | Cancel and Esc cannot interrupt a blocked syscall | Accepted: abandoned-thread model and limits (3.1); A-UI-1 extended |
| 13 | SUPER+E switch in M1 vs M2 contradiction | Contradiction resolved in the opposite direction from the suggested fix: the owner decided the switch follows M2. M1 verifies the launch on a trial chord (1, 9, 11.3) |
| 14 | Command-line keys collide with panel keys; `Ctrl+H` = Backspace; fallbacks missing | Accepted: ownership rule, hidden toggle on `Alt+.`, full-path insert on `Alt+P` (8). Correction: `Shift+F*` do not need the kitty protocol (xterm modifier encoding) |
| 15 | Shell quoting, `cd` expansion, argv spawning, zombies | Accepted (6); A-FS-10 extended |
| 16 | Acceptance checks that cannot fail | Accepted: A-FS-1, 5, 7, 10, A-TR-3, A-TH-1 rewritten; `--no-theme-watch` defined (7.2) |
| 17 | Full theme-set event sequence | Accepted (2, 7.2); A-TH-2 replays it |
| 18 | Terminal survives the switch; foot OSC race | Accepted: full redraw after reload (7.2); section 2 corrected |
| 19 | Signal handler safety | Accepted: self-pipe signal thread (3.1) |
| 20 | "Wrong in both directions" overclaims | Accepted: sentence replaced by the 4.2 model |
| 21 | Suspend of manycommander itself | Accepted for `SIGTSTP`/`SIGCONT`/`SIGTERM` (6). Not accepted for `SIGKILL`, which no process can handle (6) |
| 22 | "Overwrite all older" undefined | Accepted (4.5) |
| 23 | Symlink-to-file destination | Accepted (4.5) |
| 24 | Real `colors.toml` fixture | Accepted (2, A-TH-3) |
| 25 | Focus check by class | Accepted (A-LN-1) |

## Appendix B. Amendments from the plan review

The implementation plan's adversarial review (2026-09-27) found these design-level issues.
Plan-only findings are resolved in the plan.

| Plan-review # | Issue | Amendment |
|---|---|---|
| 5, 24 | A 255-byte name cannot be trashed as `N.trashinfo` | Basename shortening; `Path` keeps the original bytes (4.10, A-FS-10) |
| 6, 23 | A-FS-5 and A-FS-12 passed with inert failpoints | Reached-step assertions and step-specific predicates (A-FS-5, A-FS-12) |
| 11 | P-8 had no failing check | A-P-8 |
| 12 | A reflinked copy ends before A-P-1 samples | Cross-filesystem copy to an ext4 loop image, job-running assertion (A-P-1) |
| 18 | Section 3.1 named `notify`; the plan uses the `inotify` crate | 3.1 amended |
| 19 | Appendix A preamble was ambiguous about which mount claim failed | Preamble made precise with mount IDs |
| 20 | Direct-write mode could unlink the source of a partial destination | Commit point and batch entry defined for direct-write mode (4.7, 4.8) |
| 21 | A mount point inside a moved tree hit `EBUSY` instead of being skipped | Skip before `rename` (4.8) |
| 22 | exfat mtime resolution is 10 ms, not 2 s | `f_type`-based resolution (4.5) |
| 28 | Case-only rename could strand the file silently | Report names the intermediate path (4.8) |
| 29 | `DeletionDate` needs a date crate | `jiff` (4.10) |
| 30, 31 | A-P-2 and A-P-5 had loose measurements | First-flush timestamp; `/proc` counter deltas (A-P-2, A-P-5) |
| 32 | inotify overflow could drop the theme event | `IN_Q_OVERFLOW` triggers a reload (7.2) |
| -- | Owner decision: verification stays local until the repository is public | Section 12; section 11 intro (skips fail in the local gate); A-FS-7 automated with unprivileged subvolumes |
