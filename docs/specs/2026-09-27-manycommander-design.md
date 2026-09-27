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
[8 Keymap](#8-keymap-m1) · [9 Launch](#9-launch-and-integration) ·
[10 Alternatives](#10-alternatives-considered) · [11 Acceptance](#11-acceptance-checks) ·
[12 Publication](#12-repository-and-publication)

## 1. Outcome

manycommander is a keyboard-driven, dual-pane terminal file manager for Omarchy, written in
Rust with ratatui. It follows the Total Commander tradition: two panels, the F3-F8 verbs,
a command line, and (after the MVP) tabs. It takes its colours from the active Omarchy theme
and re-themes itself live when the theme changes.

The MVP is done when manycommander can replace Double Commander on `SUPER + E` for daily
work. The acceptance checks in section 11 define "usable" for that switch.

### 1.1 Scope

| Milestone | Contents |
|---|---|
| M1 -- MVP | Two panels with navigation, sorting, selection and live refresh. F3 view and F4 edit through `$PAGER` / `$EDITOR`. F5 copy, F6 move/rename, F7 mkdir, F8 trash, Shift+F8 permanent delete with typed confirmation. Conflict handling. One file-operation job at a time on a worker thread, with progress and cancel. Command line. Live theming. Launch script and `SUPER + E` binding recipe. |
| M2 -- tabs | Per-panel tabs. The panel paths and tabs are restored on the next start. The `SUPER + E` switch happens after M2 passes its acceptance checks. |
| Later (not designed here) | Archives, SFTP and other virtual filesystems, a built-in viewer or editor, a job queue with concurrent jobs, xattr/ACL/ownership preservation, hard-link preservation, sparse-file preservation, configurable keymap, mouse support, directory compare, multi-rename. |

Explicit non-goals: portability beyond Linux, and support for terminals without truecolor
beyond a basic fallback (section 7.4).

## 2. Environment assumptions

| Assumption | Evidence | Consequence |
|---|---|---|
| Omarchy writes the active palette to `~/.local/state/omarchy/current/theme/colors.toml`. It holds flat `key = "#rrggbb"` pairs (`accent`, `selection`, `muted`, `background`, `dark_background`, `darker_background`, `lighter_background`, `foreground`, `dark_foreground`, `light_foreground`, `bright_foreground`, the eight ANSI hues and their `bright_` variants) plus `mode = "dark"\|"light"`. | Current Omarchy theme directory | Theme parser (section 7.1) |
| `omarchy-theme-set` builds the new theme in a staging directory, runs `rm -rf current/theme`, then runs `mv next-theme current/theme`. | `/usr/bin/omarchy-theme-set` | The theme directory inode changes on every switch. An inotify watch on `current/theme` itself dies with the old directory. The watch must sit on the parent `current/` (section 7.2). For a short window `colors.toml` does not exist. |
| After the swap and the app retints, `omarchy-theme-set` runs `omarchy-hook theme-set <name>`. That runs every non-`.sample` file in `~/.config/omarchy/hooks/theme-set.d/` with `bash`. | `/usr/bin/omarchy-hook` | An optional hook can signal manycommander (section 7.2). Omarchy already reloads other TUIs the same way: it sends `SIGUSR2` to btop and `SIGUSR1` to helix. |
| TUIs are launched through `xdg-terminal-exec` with an app id `org.omarchy.<name>`. The Hyprland bindings DSL has `{ tui = "<cmd>", focus = true }` for launch-or-focus. | `omarchy-launch-tui`, default `applications.lua` | Section 9 |
| Omarchy's terminals (Alacritty, Ghostty, Kitty, foot) support truecolor and the kitty keyboard protocol. | Terminal docs | `Ctrl+Enter` and similar chords are available, with fallbacks (section 8) |
| `/home` and `/` are btrfs. `/tmp` and `/run/user/<uid>` are tmpfs. Removable media is often vfat or exfat. | `findmnt` | Cross-filesystem moves are common (`~` <-> `/tmp`, `~` <-> USB). btrfs subvolumes have different `st_dev` values, and `rename(2)` across subvolumes fails with `EXDEV`, even inside one btrfs filesystem. |

## 3. Architecture

manycommander is one Cargo package with a library crate and a thin binary. The library
holds everything testable without a terminal. The binary wires the terminal and the event loop.

```
src/
  main.rs          arg parsing, terminal setup/teardown, panic hook restores the terminal
  app/             App state, the event loop, update(event) -> effects, view(frame)
  panel/           directory listing, sort, selection, cursor, per-panel watcher
  fsops/           job planning and execution: copy, move, mkdir, trash, delete
  fsops/trash.rs   freedesktop.org Trash implementation
  theme/           colors.toml parsing, role mapping, reload sources
  cmdline/         command line editing and shell hand-off
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
| Listing threads (one per load request) | `ListingReady { panel, generation, entries }` or `ListingFailed` |
| Panel watchers (inotify via `notify`, debounced 200 ms) | `DirChanged { panel }` |
| Theme watcher and `SIGUSR1` handler (`signal-hook`) | `ReloadTheme` |
| File-operation worker | `Progress`, `Ask { question, reply: Sender<Answer> }`, `JobDone { report }` |

The UI thread is the only owner of `App` state. It never performs a blocking filesystem
call. It reads directories on a listing thread so a hung network mount cannot freeze the
UI: the panel shows "loading", `Esc` abandons the load, and a stale `generation` result
is dropped. The listing thread uses `lstat` for each entry plus `stat` for symlink targets
(to classify broken links and links to directories).

### 3.2 Paths and names

All paths are `PathBuf` / `OsString` end to end. No path is converted to `String` and back.
The UI displays names lossily. It escapes control characters (a newline shows as `\n`)
and invalid UTF-8 bytes (as `\xNN`) in a distinct colour. The command line inserts names
shell-quoted from the raw bytes. A file named with a newline, a leading dash or invalid
UTF-8 must survive every operation byte-exactly (acceptance check A-FS-10).

## 4. File-operation safety (highest risk)

This section is normative for `fsops`. It is written as invariants first, then the
mechanisms that uphold them.

### 4.1 Invariants

| ID | Invariant |
|---|---|
| I-1 | **No lost data on move.** At every instant of a move, including cancel, crash and power loss after an acknowledged step, every source file's complete content exists in at least one committed location. |
| I-2 | **No partial destination file.** A destination path shows either its previous content or the complete new content, never a truncated or partially written file. |
| I-3 | **Never overwrite without a decision.** An existing destination entry is replaced only after an explicit "Overwrite" (or "Overwrite all") answer that covers it. |
| I-4 | **Never destroy the source through the destination.** An operation whose source and destination resolve to the same file (same `st_dev`/`st_ino`) never truncates or unlinks it. A directory is never copied or moved into itself or a descendant. |
| I-5 | **Symlinks are objects, not paths.** Copy, move, trash and delete act on the link, never on its target. No operation follows a symlink during tree traversal. |
| I-6 | **No silent escalation.** Trash never falls back to copying across filesystems or to permanent deletion. Move never falls back to copy+delete for a mount point. |
| I-7 | **Honest reporting.** Every entry ends the job as done, skipped (with a reason) or failed (with the OS error). The final report states which invariant-relevant state the job left, for example "move cancelled: 812 moved, 40 still at source". |

### 4.2 Job lifecycle

Every verb (F5, F6, F7, F8, Shift+F8) becomes a `Job` and goes through four phases.

1. **Confirm.** The dialog shows the source set (the marked entries, or the entry under the
   cursor if none is marked), the destination (default: the other panel's directory; the
   user can edit it, and for a single source it may name a new file name) and the verb. The
   F8 dialog shows the item count and which trash each item goes to. The Shift+F8 dialog
   shows the counts after the scan and requires the user to type `delete`.
2. **Plan.** The worker scans the source trees with `lstat` and builds the entry list and
   the byte totals for progress. The worker checks I-4 here (section 4.4). The user can
   cancel the scan.
3. **Execute.** The worker processes entries in order. Before it acts on an entry, it
   re-`lstat`s the source, because the plan can be stale. An entry that disappeared or
   changed type fails with a clear reason.
4. **Report.** The worker sends a `JobReport`: per-entry outcome, totals, and the cancel
   state. The UI shows a summary. If anything was skipped or failed, the UI shows a
   scrollable list. Both panels re-read their directories.

Only one job runs at a time. The UI stays responsive during a job, but new F5-F8 requests
are refused with "a job is running" until the job ends.

### 4.3 Questions, errors and cancel

The worker asks the UI through `Ask { question, reply }` and blocks until it gets an answer.

| Question | Answers | Default focus |
|---|---|---|
| File exists (file over file) | Overwrite, Overwrite all, Overwrite all older, Skip, Skip all, Rename (pre-filled `name (1).ext`), Cancel job | Skip |
| Directory exists (directory over directory) | Merge, Merge all, Skip, Rename, Cancel job | Merge |
| Type mismatch (file over directory, directory over file, anything over a symlink-to-directory) | Skip, Skip all, Rename, Cancel job | Skip |
| Error on entry (`EACCES`, `ENOSPC`, `EIO`, ...) | Retry, Skip, Skip all of this error kind, Cancel job | Retry for `ENOSPC`, otherwise Skip |

The dialog shows both sides with size, mtime and the read-only flag. manycommander never
replaces a directory tree with a file, or a file with a directory tree. A type mismatch can
only be skipped or renamed.

**Cancel** sets an `AtomicBool`. The worker checks it between entries and between copy
chunks (16 MiB). On cancel during a file copy, the worker deletes that file's temporary
file (section 4.5), so I-2 holds. The report lists what completed.

### 4.4 Pre-flight checks (plan phase)

| Check | Method | Result |
|---|---|---|
| Destination inside source | Walk up from the destination directory to `/`, collecting `(st_dev, st_ino)` of every ancestor. If a source directory's `(st_dev, st_ino)` is in that set, stop. The walk uses `..` via `openat`, so symlinks and bind mounts cannot hide the relation. | The job is refused before it starts |
| Same file | For each planned `(src, dst)` pair where `dst` exists, compare `(st_dev, st_ino)` of `lstat(src)` and `lstat(dst)` | "Source and destination are the same file"; entry skipped. A case-only rename on a case-insensitive filesystem is the one exception (section 4.6). |
| Special files | FIFO, socket, block or character device (from `lstat`) | Copy: skipped with reason "special file"; the worker never `open`s one, because opening a FIFO for reading blocks. Same-filesystem move and trash use `rename` and work. |
| Directory cycle | During traversal, keep the `(st_dev, st_ino)` stack of the current path; a repeat means a bind-mount loop | The subtree is skipped with a reason |
| Nested mount points | During traversal, a directory whose `st_dev` differs from its parent's is a mount point or a btrfs subvolume | Copy: descend (reading is harmless). Cross-filesystem move: skip it with the reason "mount point"; never copy+delete a mounted filesystem's contents. Permanent delete: skip it with a reason. |

### 4.5 Copy (F5)

Per regular file:

1. `openat(src, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC)`. Then `fstat` the fd; if
   it is not a regular file, skip the entry (the type changed after the plan). Then clear
   `O_NONBLOCK`.
2. Create a temporary file in the **destination directory**:
   `.<name>.mc-partial-<random>`, `O_CREAT | O_EXCL | O_WRONLY | O_CLOEXEC`, mode `0600`.
   Retry with a new random suffix on `EEXIST`.
3. Copy in 16 MiB chunks with `copy_file_range(2)`. On btrfs within one filesystem, this
   shares extents (reflink) where the kernel can. On `EXDEV`, `EOPNOTSUPP` or `ENOSYS`, use
   `read`/`write`. Check cancel between chunks and report progress.
4. `fstat` the source fd again. If size or mtime changed during the copy, report the entry as
   "source changed during copy". For copy, commit it anyway. For move, section 4.6 applies.
5. Apply metadata to the temporary file: permission bits via `fchmod`, with setuid and
   setgid cleared (ownership is not preserved, so keeping them would be unsafe); atime and
   mtime via `futimens` with nanoseconds.
6. **Commit** the temporary file to the final name:
   - No conflict: `renameat2(tmp, final, RENAME_NOREPLACE)`. On `EEXIST` (something appeared
     since the plan), unlink the temporary file and raise the "file exists" question. After
     "Overwrite", start over from step 1.
   - Overwrite answered: `renameat(tmp, final)`, which atomically replaces the old file.
     The old destination is never truncated or opened for writing. Consequences, documented
     in the UI help: (a) other hard links to the old destination keep the old content;
     (b) if the destination is a symlink, the link itself is replaced, not its target.
   - If the filesystem does not support `RENAME_NOREPLACE` (`EINVAL`, for example some
     FUSE and older vfat): use `linkat(tmp, final)` (atomic `EEXIST`), then unlink the
     temporary file. If `linkat` is unsupported too, do `lstat(final)` then `renameat`.
     That last fallback is racy, and it is documented as a known limit.
7. If any step fails or is cancelled, unlink the temporary file. After a crash, a
   `.mc-partial-*` file can remain in the destination directory. The name pattern is
   documented, and manycommander never deletes such files automatically.

Directories: create the destination directory with `mkdirat(..., 0700)` so the worker can
write into it even when the source directory is read-only. Recurse into it. Then, in
post-order, apply the source's permission bits and timestamps. When the destination
directory already exists and the answer is Merge, its own permission bits and timestamps
are left unchanged.

Symlinks: `readlinkat` and `symlinkat` create a link with the byte-identical target
string; relative targets are not rewritten (the same as `cp -P`). Commit uses the same
no-replace/replace rule as for files, via a temporary link name. manycommander never
follows links during traversal (I-5). This includes a symlink selected at the top level;
the confirm dialog states "N symbolic links are copied as links".

Not preserved in M1, and stated in the F5 dialog help: ownership, ACLs, xattrs, hard-link
structure (each link becomes an independent file), sparseness beyond what
`copy_file_range` keeps. Durability: copy does not `fsync` per file (the same as `cp`).

### 4.6 Move and rename (F6)

manycommander always **tries `renameat2(src, dst, RENAME_NOREPLACE)` first** and lets the
kernel decide whether the move stays on one filesystem. It does not predict this from
`st_dev`, because btrfs subvolumes and bind mounts make that prediction wrong in both
directions.

| `rename` result | Action |
|---|---|
| Success | Done. The move is atomic, so all metadata, hard links and xattrs survive. |
| `EEXIST` | Raise the conflict question. Overwrite file over file: `renameat` (atomic replace). Merge directory into directory: move each child recursively with the same rules, then `rmdir` the source directory if it is empty. |
| `EXDEV` | Cross-filesystem path (below). |
| `EBUSY` (mount point), other errors | Error question for this entry. Never fall back to copy+delete (I-6). |

**Cross-filesystem move** is copy then delete, done **per file**, to uphold I-1:

1. Copy the file as in section 4.5, steps 1-6.
2. `fsync` the committed destination file, then `fsync` its directory. Only after both
   succeed is the copy durable.
3. Re-check the source: step 4 of section 4.5 must show that the source did not change. Then
   `lstat` the source path again and confirm that `(st_dev, st_ino)` still matches the fd that
   was copied. If either check fails, keep the source and report "source changed; kept both".
4. `unlinkat` the source.
5. After a directory's children are all processed, `rmdir` the source directory if it is
   empty. A directory that still holds skipped or failed entries stays, and the report
   lists it.

Symlinks and empty directories on the cross-filesystem path are recreated, then the
source is removed. Special files on the cross-filesystem path are skipped (section 4.4).

Consequence of per-file granularity: a cancelled cross-filesystem move leaves a
partially moved tree. Each file is in exactly one complete place (or briefly two), never
zero. The report says how many files moved and how many remain. This state is safe and
resumable: running F6 again on the rest merges.

**Case-only rename** (`Foo` -> `foo`) on a case-insensitive filesystem: the same-file check
finds that `dst` is the same inode as `src`. manycommander renames via an intermediate
unique name in the same directory.

**Rename in place** (Shift+F6) is F6 with a single source and the destination fixed to the
same directory. It uses the same code path.

### 4.7 Make directory (F7)

The input may contain `/` and creates the missing parents (`a/b/c`). Validation rejects an
empty name, `.`, `..` and NUL. `mkdirat` uses mode `0777`, so the umask applies. If the name
already exists, manycommander reports it and moves the cursor to it. The new directory is
selected in the panel afterwards.

### 4.8 Trash (F8)

manycommander implements the freedesktop.org Trash specification itself, and does not use a
general-purpose crate. The reasons are in section 10. The trash is interoperable with
`gio trash`, Nautilus and other spec-compliant tools.

Choosing the trash directory for an entry `p` (the entry itself, not a symlink target):

1. If `lstat(p).st_dev` equals the `st_dev` of the home trash directory
   (`$XDG_DATA_HOME/Trash`, default `~/.local/share/Trash`, created `0700` if missing), use
   the home trash.
2. Otherwise find the **top directory** of `p`'s filesystem. Walk up from `p`'s parent while
   the parent's `st_dev` equals `p`'s `st_dev`. This matches the rename domain, including
   btrfs subvolumes, which `/proc/self/mountinfo` does not list when they are not separately
   mounted. Use `$top/.Trash/$uid` if `$top/.Trash` exists, is a directory, is not a symlink
   and has the sticky bit. Otherwise use `$top/.Trash-$uid`, which is created `0700` if
   missing. If it exists, it must be a directory, not a symlink, and owned by the user.
3. If neither exists or can be created, trashing this entry fails. The question offers
   Skip, or "Delete permanently..." which opens the Shift+F8 typed confirmation for this
   entry only. There is no copy to the home trash (I-6).

Trashing one entry:

1. Pick a name `N` (the original basename; on collision `N.2`, `N.3`, ...). Create
   `info/N.trashinfo` with `O_CREAT | O_EXCL`. The atomic create reserves the name.
2. Write `[Trash Info]`, `Path=` (percent-encoded original path; absolute for the home trash,
   relative to `$top` for a top-directory trash) and `DeletionDate=` (local time,
   `YYYY-MM-DDThh:mm:ss`). `fsync` the file.
3. `renameat(p, files/N)`. On failure, remove the `.trashinfo` file and report the entry.
   On `EXDEV`, the top-directory detection was wrong; report it as a bug-class error.
   Never copy.

A directory is trashed whole with one `rename`; trashing is instant regardless of size. An
entry that is already inside a trash directory is refused with "already in trash". M1 has no
trash browser or restore; restore works through `gio trash --restore` or Nautilus.
M1 does not update the optional `directorysizes` cache.

### 4.9 Permanent delete (Shift+F8)

1. The plan scans the tree and shows files, directories and bytes. The user must type
   `delete` to proceed. There is no default-Enter path.
2. Traversal is `openat`/`fstatat(AT_SYMLINK_NOFOLLOW)` relative to directory fds, so a
   path component swapped for a symlink during the delete cannot redirect it (the same
   approach as `rm -r` in coreutils). Symlinks are unlinked, never followed.
3. The worker does not descend into a directory with a different `st_dev` (mount point or
   subvolume). That directory is skipped with a reason.
4. `unlinkat` files and `unlinkat(AT_REMOVEDIR)` directories in post-order. The worker
   does not `chmod` read-only directories to force deletion; the entry fails with the OS
   error.

## 5. Panels

Each panel holds: current directory, entries, sort key and direction, a set of marked names,
a cursor (by name, so it survives refresh), the hidden-file toggle and the load generation.

- Columns: name, extension, size (directories show `<DIR>` until computed), mtime, mode.
- Sort: name (default, directories first), extension, size, mtime. `Ctrl+F3`-`Ctrl+F6`
  select the sort key, and pressing the same key again reverses the direction. Names sort by
  a natural order that uses digit runs as numbers and is case-insensitive.
- Refresh: an inotify watch on each panel's directory, debounced 200 ms, plus `Ctrl+R`.
  On refresh the panel keeps the cursor on the same name when that name still exists, and
  drops marks whose names no longer exist. If the current directory is deleted, the panel
  goes to the nearest existing ancestor.
- Footer: marked count and bytes; free space of the filesystem (`statvfs`).
- `Space` on a directory computes its size (on a listing thread, cancellable) and marks it.

## 6. Command line and hand-off

A single-line editor sits above the function-key bar. It is always the target of printable
keys that have no binding (the Total Commander behaviour).

- `Enter` with text: if the command is `cd <path>` (with `~` and `$VAR` expansion), the
  active panel changes directory internally. Otherwise manycommander suspends the TUI and runs
  `$SHELL -c '<text>'` (default `/bin/sh`) in the active panel's directory with inherited
  stdio. It then prints `[exit N] press Enter to return` and restores the TUI. Both panels
  refresh afterwards.
- `Ctrl+Enter` (fallback `Alt+Enter`): insert the shell-quoted name under the cursor.
  `Ctrl+Shift+Enter` (fallback `Alt+Shift+Enter`): insert the full path.
- `Up`/`Down` while the command line has focus: in-session history (M2 persists it).

**Suspend and resume** is shared by the command line, F3 and F4:
leave the alternate screen, disable raw mode and the keyboard protocol, spawn, wait,
then re-enable and fully redraw. If the child is killed by a signal, the TUI is still
restored. A panic hook restores the terminal before it prints the panic.

F3 runs `$PAGER` (default `less`) on the file under the cursor. F4 runs `$EDITOR` (default
`nvim`, then `vi`). Shift+F4 prompts for a new file name and opens it in `$EDITOR`. `Enter`
on a non-executable file runs `xdg-open` detached (`setsid`, stdio to `/dev/null`) and never
blocks the TUI. `Enter` on an executable file does not run it; it opens it like any other
file. Running it requires the command line. This avoids accidental execution.

## 7. Theming

### 7.1 Palette to roles

`theme::Palette` parses `colors.toml` with the `toml` crate into a map of known keys. Unknown
keys are ignored. Missing keys resolve through fallbacks. Each UI role is defined once:

| Role | Source key (fallback chain) |
|---|---|
| Panel background | none: `Color::Reset`, so the terminal's own background and opacity show through. The config option `paint_background = true` uses `background` instead. |
| Normal file | `foreground` |
| Directory | `bright_foreground`, bold |
| Executable | `green` |
| Symlink / broken symlink | `cyan` / `red` |
| Hidden entry | `dark_foreground` |
| Marked entry | `yellow` (bold) |
| Cursor row, active panel | fg `background`, bg `accent` |
| Cursor row, inactive panel | bg `selection` (-> `lighter_background`) |
| Active panel border and path | `accent` |
| Inactive panel border and path | `muted` (-> `dark_foreground`) |
| Metadata columns | `light_foreground` (-> `foreground`) |
| Dialog background / border | `dark_background` (-> `background`) / `accent` |
| Error / warning text | `red` / `yellow` |
| Function-key bar labels | fg `foreground` on bg `lighter_background` (-> `selection`), key numbers in `accent` |
| Command line prompt | `accent` |

`mode = "light"` needs no special handling, because the roles only refer to semantic keys.
The theme is also correct when `colors.toml` is absent: every role falls back to a named
ANSI colour (for example the directory role to `Color::White` bold, the cursor row to
`Color::Blue`), and the terminal renders those with its own Omarchy-retinted palette.

### 7.2 Live reload

Two sources feed one idempotent `ReloadTheme` event. The UI re-reads `colors.toml`,
parses it, and redraws only when the resulting palette differs from the current one.

1. **Watcher (primary; no installation needed).** A non-recursive inotify watch on
   `~/.local/state/omarchy/current/` (the parent, see section 2). Events whose name is
   `theme` (`IN_CREATE`, `IN_MOVED_TO`) or `theme.name` trigger a reload after a 150 ms
   debounce. If `colors.toml` is missing or does not parse, the current palette stays and
   the next event retries. The `rm -rf`/`mv` window is harmless. If `current/` does not
   exist at startup, the watcher watches the nearest existing ancestor and re-arms when
   the directory appears.
2. **Signal (optional hook).** `SIGUSR1` triggers a reload, following Omarchy's pattern for
   btop and helix. The repository ships `contrib/omarchy/theme-set-hook.sh` containing
   `pkill -USR1 -x manycommander || true`. The user installs it by copying it to
   `~/.config/omarchy/hooks/theme-set.d/manycommander`; manycommander never writes to the
   user's config itself. The hook is a fallback for systems where the watch cannot be
   placed.

Overrides for tests and non-Omarchy use: `--theme-file <path>` or `MANYCOMMANDER_THEME`
selects the file to parse and watch (in that case the parent of the file is watched).

### 7.3 Terminal retint

`omarchy-theme-set` also restarts or retints the terminal emulator. With the default
`paint_background = false`, the terminal owns the background, and manycommander owns every
foreground and highlight role. Both change in the same theme switch.

### 7.4 Colour depth

When `COLORTERM` is `truecolor` or `24bit`, roles use RGB. Otherwise every role uses its
ANSI fallback from section 7.1. There is no 256-colour approximation in M1.

## 8. Keymap (M1)

| Key | Action |
|---|---|
| `Tab` | Switch active panel |
| `Up`/`Down`/`PgUp`/`PgDn`/`Home`/`End` | Move cursor |
| `Enter` | Enter directory / run the command line if it has text / `xdg-open` a file |
| `Backspace` (command line empty) | Parent directory |
| `Insert`, `Shift+Down` | Toggle mark, move down |
| `Space` | Toggle mark (directories: also compute size) |
| `Ctrl+A` / `Ctrl+Shift+A` | Mark all / unmark all |
| `Alt+=` / `Alt+-` / `Alt+*` | Mark by glob / unmark by glob / invert marks |
| `Ctrl+S` | Quick search: jump to the next name matching the typed prefix |
| `Ctrl+H` | Toggle hidden files |
| `Ctrl+R` | Re-read both panels |
| `Ctrl+U` | Swap panels |
| `Alt+Left` / `Alt+Right` | Directory history back / forward (per panel) |
| `Ctrl+PgUp` | Parent directory |
| `Ctrl+O` | Show terminal output of the last command (suspend without running anything) |
| `F1` | Help overlay (keymap and file-operation semantics) |
| `F3`, `F4`, `Shift+F4` | View, edit, edit new file |
| `F5`, `F6`, `Shift+F6`, `F7`, `F8`, `Shift+F8` | Copy, move, rename in place, mkdir, trash, permanent delete |
| `F10`, `Alt+X` | Quit (confirms when a job is running and cancels it) |

`Ctrl+Enter`, `Ctrl+Shift+A` and `Shift+F*` need the kitty keyboard protocol, and all
Omarchy terminals support it. manycommander enables it through crossterm's
`PushKeyboardEnhancementFlags` when the terminal supports it. Every such chord has an
`Alt+` fallback listed in section 6 or in the help overlay. The keymap is compiled in for
M1; configuration comes later.

M2 adds `Ctrl+T` new tab (duplicate of current), `Ctrl+W` close tab, `Ctrl+Tab` /
`Ctrl+Shift+Tab` next/previous tab, and a tab bar row above each panel header when a panel
has more than one tab.

## 9. Launch and integration

- Installation: `cargo install --path . --root ~/.local` puts the binary in `~/.local/bin`.
  The `SUPER + E` binding needs the binary on the Hyprland session's `PATH`. The plan verifies
  that; if it is not on the `PATH`, the binding uses the absolute path.
- The binding recipe (applied by the user in `~/.config/hypr/bindings.lua`, replacing the
  current Double Commander line):

  ```lua
  o.bind("SUPER + E", "File manager (dual pane)", { tui = "manycommander", focus = true })
  ```

  `tui` launches through `omarchy-launch-tui`, so the window gets the app id
  `org.omarchy.manycommander`. With `focus = true`, a second `SUPER + E` focuses the
  running instance instead of starting another one.
- Double Commander stays installed. The switch is one line and is reversible.
- The initial directories are the process working directory for the left panel and `$HOME`
  for the right panel. M2 restores the last paths from `~/.local/state/manycommander/state.toml`.
  `manycommander <left> [<right>]` overrides both.

## 10. Alternatives considered

| Alternative | Decision |
|---|---|
| Midnight Commander with a generated truecolor skin | Rejected. mc does not reload skins live, and its keymap and dialog conventions differ from Total Commander. The file-operation invariants of section 4 would also have to be audited in a foreign C codebase instead of being owned and tested here. |
| yazi, broot, xplr | Rejected as a base. They are single-pane or Miller-column designs; a dual-pane F-key workflow would fight their model. |
| The `trash` crate | Rejected for M1. manycommander needs guarantees it can test itself: no copy fallback across devices (I-6), the link and not its target (I-5), `st_dev`-walk top-directory detection for btrfs subvolumes, and refusal to trash from inside a trash directory. The spec part needed here is small (about 300 lines). Revisit if the crate's guarantees are verified to match. |
| `std::fs::copy` / `fs_extra` | Rejected. They open the destination with `O_TRUNC`, which breaks I-2 and, when source and destination are the same inode, destroys the source (I-4). They also have no chunk-level cancel or progress. |
| tokio | Rejected. The workload is a few blocking syscalls; threads plus one channel are simpler and make cancellation explicit. |
| Theme reload by hook only | Rejected as the only source. It needs an installation step and fires after all app retints (later). The watcher is primary; the hook is an optional fallback. |
| Per-top-level-item cross-filesystem move (copy whole tree, then delete tree, as `mv` does) | Rejected. It needs double space for the whole item, and on cancel it leaves a partial destination tree to clean up. Per-file commit-then-delete upholds I-1 with the least extra space and leaves a resumable state. |

## 11. Acceptance checks

M1 is accepted when all of these pass. Checks marked *auto* are automated tests in the
repository. Checks marked *manual* are run once on an Omarchy machine and recorded in the
implementation plan.

| ID | Check | Type |
|---|---|---|
| A-FS-1 | Copy of a tree containing regular files, empty dirs, a read-only dir, relative and absolute symlinks, a broken symlink and a FIFO: files are byte-identical with mode and mtime preserved; links are links with identical targets; the FIFO is skipped and reported; the worker does not hang. | auto |
| A-FS-2 | Overwrite answered on a destination that is a hard link of another file: the other link keeps the old content, and the destination has the new content. | auto |
| A-FS-3 | Copy where destination and source are the same inode (hard link or same path): the job refuses, and the source is byte-identical afterwards. | auto |
| A-FS-4 | Copy or move of a directory into its own descendant (also via a symlinked destination path): the job is refused before any write. | auto |
| A-FS-5 | Failpoint sweep: for a cross-filesystem move of a tree, inject cancel or an I/O error at every step boundary (each chunk, commit, fsync, unlink). After each run, every source file exists complete at the source or at the destination (content hash), and no `.mc-partial-*` file remains. | auto (needs two filesystems; runs when `MC_XDEV_DIR` is set, for example to a tmpfs path) |
| A-FS-6 | Same-filesystem move of a directory preserves its inode (rename used), including hard links inside it. | auto |
| A-FS-7 | A move where `rename` returns `EXDEV` between two btrfs subvolumes of one filesystem completes through the cross-filesystem path. | manual |
| A-FS-8 | Destination file appears between plan and commit: the commit does not replace it, and the conflict question is raised. | auto (failpoint) |
| A-FS-9 | Source file modified during a cross-filesystem move: the source is kept, and the report says "source changed; kept both". | auto (failpoint) |
| A-FS-10 | Names with a newline, a leading `-`, invalid UTF-8 and 255-byte length survive copy, move, trash and delete byte-exactly, and display escaped. | auto |
| A-TR-1 | Trash in the home trash writes a spec-valid `.trashinfo` (percent-encoded absolute `Path`, `DeletionDate`); a name collision creates `N.2`; `gio trash --list` shows the entries. | auto + manual (`gio`) |
| A-TR-2 | Trash of a symlink moves the link; the target is untouched. | auto |
| A-TR-3 | Trash on a different filesystem (tmpfs, and a vfat image mounted with `udisksctl` or a loop mount) uses `$top/.Trash-$uid` with a relative `Path`. `gio trash --restore` restores it. | manual |
| A-TR-4 | When no trash directory can be used, F8 does not delete, and it offers the explicit permanent-delete path. | auto (unwritable top dir) |
| A-DEL-1 | Shift+F8 without typing `delete` does nothing. With it, the tree is removed. A symlink to a directory outside the tree leaves the outside directory intact. A nested mount point or subvolume is skipped. | auto (symlink case) + manual (mount case) |
| A-UI-1 | A panel on an unresponsive mount (for example a stopped FUSE filesystem) shows "loading", and `Esc` returns control within 100 ms. | manual |
| A-UI-2 | External changes (`touch`, `rm` in another terminal) appear within 1 s, and the cursor stays on the same name. | auto (listing layer) + manual |
| A-UI-3 | F3, F4 and the command line suspend and restore the terminal correctly, including after the child is killed with `SIGKILL`. | manual |
| A-TH-1 | Running `omarchy-theme-set <other theme>` recolours a running instance within 1 s without the hook installed. The same holds with the hook installed and the watcher disabled (`--no-theme-watch`). | manual |
| A-TH-2 | The watcher survives the `rm -rf`/`mv` swap: a test that reproduces the swap in a temporary state directory gets exactly one effective palette change, and never an empty palette. | auto |
| A-TH-3 | A missing `colors.toml`, or one that does not parse, starts with the ANSI fallback and does not crash. | auto |
| A-LN-1 | `SUPER + E` with the recipe from section 9 opens manycommander; a second press focuses it. | manual |
| A-PUB-1 | The repository contains no tenant, client, host or private-repository names (publication gate clean). | auto (pre-push) |

M2 adds: tabs open, close and switch per panel; paths and tabs restore after restart; a
missing restored path falls back to the nearest existing ancestor.

## 12. Repository and publication

- The repository is private now and becomes public later. From the first commit, it
  contains no tenant, client, host or private-repository names. Examples use generic
  paths (`~/Documents`, `/mnt/usb`).
- License: Apache-2.0 (present).
- CI: a small self-contained GitHub Actions workflow runs `cargo fmt --check`,
  `cargo clippy -- -D warnings` and `cargo test`. Adopting shared reusable CI or vendored
  agent instructions waits until the design for how a public repository consumes the shared
  baseline is settled; M1 vendors neither.
