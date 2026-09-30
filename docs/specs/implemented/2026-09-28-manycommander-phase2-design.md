---
title: manycommander phase 2 design
type: spec
status: implemented
owner: manycommander
source: 2026-09-27-manycommander-design.md
created: 2026-09-28
updated: 2026-09-30
---
# manycommander phase 2 design

Contents: [1 Outcome](#1-outcome) · [2 Architecture](#2-architecture-changes) ·
[3 Directories](#3-directory-hotlist-and-frecency-jump) · [4 Quick filter](#4-quick-filter) ·
[5 Find](#5-find-files-and-the-results-tab) · [6 Multi-rename](#6-multi-rename) ·
[7 Compare](#7-compare-directories) · [8 Links and attributes](#8-links-and-attributes) ·
[9 Copy fidelity](#9-copy-fidelity-sparse-files-and-hard-links) · [10 Keymap](#10-keymap) ·
[11 NFRs](#11-non-functional-requirements) · [12 Acceptance](#12-acceptance-checks) ·
[13 Alternatives](#13-alternatives-considered) · [14 Release](#14-release) ·
[A Review resolution](#appendix-a-review-resolution)

## 1. Outcome

Phase 2 brings manycommander to daily-driver parity with Double Commander and Total
Commander for the tasks a keyboard user reaches for every day: jumping to directories,
narrowing a listing, finding files, renaming many files, comparing two directories,
creating links, changing attributes, and copying sparse files and hard-link structures
faithfully. It must stay instant: every phase 2 feature has a performance target
(section 11), and the M1/M2 targets (M1 design section 13.1) keep holding.

The [M1/M2 design](2026-09-27-manycommander-design.md) stays normative for
everything this document does not change. Its invariants I-1 to I-7 hold for every new
verb. Section numbers of that document are cited as "M1 4.7".

### 1.1 Scope

| # | Feature | Section |
|---|---|---|
| 1 | Directory hotlist (bookmarks) and frecency jump, `z` on the command line | 3 |
| 2 | Quick filter | 4 |
| 3 | Find files (name and content) with a results tab that the verbs operate on | 5 |
| 4 | Multi-rename with preview, conflict checks and undo | 6 |
| 5 | Compare directories (date and size, or content) | 7 |
| 6 | Create symbolic and hard links; change mode and modification time | 8 |
| 7 | Copy fidelity: sparse files and hard links within the copied set | 9 |

Out of scope (phase 3 and later): archives, SFTP and other virtual filesystems, image
previews, a built-in viewer or editor, a job queue, xattr/ACL/ownership preservation,
directory synchronisation, a configurable keymap, mouse support.

### 1.2 New invariants

| ID | Invariant |
|---|---|
| I-8 | **What you see is what you act on.** A verb acts on the marked entries that are visible (not hidden by the hidden-file toggle or the quick filter). When no visible entry is marked, it acts on the entry under the cursor, and on nothing when the cursor is on `..`. The footer counts exactly the visible marks. Marks on invisible entries are kept and count again when their entries become visible. |
| I-9 | **A rename never overwrites and never strands.** Multi-rename, links and every rename step use `RENAME_NOREPLACE` or `O_EXCL`-equivalent creation. After any failure or cancel, every inode the job touched is reachable under its original name, its new name, or a temporary name that the report states. |
| I-10 | **Read-only tools never open a special file.** Find (content) and compare (content) open regular files only, through the M1 4.3 `O_PATH` sequence. They never follow a symlink and never open a FIFO or a device. |

## 2. Architecture changes

### 2.1 Forms

Find, multi-rename, links, attributes and compare need multi-field dialogs. A single
`ui::form` module provides them. A form is a list of fields; each field is a text line
(the existing `cmdline::Line`), a checkbox, or a choice among labelled options. `Tab`
and `Shift+Tab` (`BackTab`) move the focus, `Space` toggles a checkbox, `Left`/`Right`
change a choice when it has the focus, `Enter` submits from any field, and `Esc` closes.
A form may have a custom body below its fields (the multi-rename preview). Forms own
their state; `App` builds them and acts on the submitted values, as with M1 dialogs.

### 2.2 Grouped sources

A directory panel's selection is one directory and a list of names. A results tab
(section 5) holds entries from many directories. Every verb therefore takes **groups**:

```rust
pub struct Group { pub root: PathBuf, pub sub: Vec<OsString>, pub names: Vec<OsString> }
```

`root` is a panel path; `sub` is the relative directory below it as single components
(empty for a directory panel); `names` are single components. `JobSpec::{Copy, Move,
Trash, Delete, Rename, Link, Attr}` take `Vec<Group>`. A directory panel produces one
group. `Panel::selection_groups()` puts all selected results that share a relative
directory into one group.

**Opening a group.** The job opens `root` once, like an M1 panel path (M1 4.3). It then
walks `sub` one component at a time with `openat(O_DIRECTORY | O_NOFOLLOW)`: a component
that is now a symlink fails the whole group with "type changed" (I-5). No job opens a
joined path such as `root/a/b`, because that would follow symlinks in `a` and `b`. Every
component of `sub` and every name must pass `valid_component` (M1: not empty, `.`, `..`,
no `/`, no NUL); the job refuses a group that fails it before any write.

The plan scans every group. The destination-inside-source check (M1 4.6) uses the union,
across groups, of the directory identities the scan recorded (the selected directories
and their subdirectories), exactly as M1 does for one group; a group's own parent
directory is not in that set, so copying into a sibling of a selected file stays allowed.
One `Transfer` serves all groups, so standing answers ("Overwrite all", "Skip all") carry
across groups. Two results with the same name copied into one destination raise "file
exists" for the second, as two sources would. Groups whose opened directories have the
same identity (for example through a bind mount) are merged before a rename, trash or
link job, so each directory is handled once.

### 2.3 Threads and events

The UI thread still makes no filesystem syscalls (P-1). New producers:

| Producer | Sends |
|---|---|
| Find threads (section 5.3), up to 8 | `Find(Batch { search, entries, names })`, `Find(Done { search, stats })` |
| Compare thread | `Compare(Marks { left, right, summary })`, `Compare(Progress)`, `Compare(Done)` |
| Directory-store thread | `DirsLoaded(DirStore)`, `ZoxideLoaded(Vec<(PathBuf, f64)>)` |
| Helper thread (file saves) | nothing on success; `Status(error)` on failure |

Find and compare are not jobs: they only read, so they run while a job runs. At most one
search and one compare run at a time; starting another cancels the earlier one, whose tab
keeps its partial results and says so. A search whose workers stay blocked in the kernel
after a cancel counts as abandoned. At most two abandoned searches may exist; while two
do, a new search is refused with "previous searches are still blocked" (the M1 3.1 rule
for listing threads, applied to searches).

### 2.4 Panel sources and history

`Panel` gains `source: Source`: `Dir` (M1) or `Results(Arc<Search>)`. A results panel's
`dir` is the search root; its entry names are paths relative to the root
(`sub/dir/name`), stored in the same compact arena (M1 13.1), so sorting, marking,
rendering and the quick filter work unchanged. `Panel::selection_groups()` splits the
selected relative paths at their last `/` into groups.

The per-panel history (M1 5) stores `Place::Dir(PathBuf)` or `Place::Results(Arc<Search>)`.
Going back to a results place shows its entries again after a re-stat (section 5.5).
Only the three most recent results places stay in any panel's history; older ones are
dropped from it, which bounds memory (P-6).

## 3. Directory hotlist and frecency jump

### 3.1 The directories dialog

`Ctrl+D` opens one dialog, "Go to directory". It has a filter line and a list:

1. **Bookmarks** (marked `*`) that match the filter, in the order the user added them.
2. **Frequent directories** that match the filter, ranked by frecency (section 3.3).
   The active panel's own directory is excluded, as zoxide excludes the working directory.

`Up`/`Down`/`PgUp`/`PgDn` move, `Enter` loads the selected directory into the active
panel, `Insert` adds the active panel's directory to the bookmarks, `Delete` removes the
selected bookmark or forgets the selected frequent directory, and `Esc` closes. Paths
under `$HOME` show as `~/...`. The dialog filters in memory on every keystroke (P-15).

A directory that no longer exists fails to load as any navigation does (M1 5: the panel
stays and shows the error). Its frecency entry is dropped. A bookmark is never dropped
automatically.

### 3.2 Persistence

| Data | File | Written |
|---|---|---|
| Bookmarks | `$XDG_CONFIG_HOME/manycommander/hotlist.toml`, `[[dir]] path = ...` (bytes as in `state.toml`: a TOML string when UTF-8 without control characters, else a byte array) | Atomically (temporary file, fsync, rename) on a helper thread after every change |
| Frecency | `$XDG_STATE_HOME/manycommander/dirs.tsv` (`# manycommander dirs v1`, then `rank<TAB>last_visit_epoch<TAB>path`; `rank` is a decimal number; the path escapes `\`, bytes below 0x20, 0x7f and invalid UTF-8 as `\xNN`) | Atomically on exit, merged under a lock (section 3.3) |

`hotlist.toml` is small and loads on the boot thread. A `hotlist.toml` that does not
parse is reported once and never overwritten: adding a bookmark then fails with "hotlist.toml
does not parse; fix or remove it". `dirs.tsv` can hold thousands of lines, so it loads on
the directory-store thread after the first frame (P-2). Lines that do not parse are skipped.

### 3.3 Frecency

The store follows zoxide's model, so ranking is familiar:

- Every completed navigation of a directory panel (not a refresh) records a visit:
  `rank += 1`, `last = now`.
- Score = `rank x w(age)`, with `w` = 4 within an hour, 2 within a day, 0.5 within a week,
  0.25 otherwise.
- Aging as in zoxide: when the sum of ranks exceeds `max_age` (10000), every rank is
  multiplied once by `0.9 x max_age / sum` and entries below 1 are dropped. Ranks are
  `f64` and stored as decimals.
- Matching: the filter splits on whitespace into keywords, compared ASCII
  case-insensitively. Every keyword must occur in the path, in order; the last keyword must
  occur in the last path component. An empty filter matches everything.

**Merge on save.** A session records its visits as deltas. On exit it takes an exclusive
`flock` on `dirs.tsv.lock` (a separate file, because the atomic rename replaces
`dirs.tsv` itself), re-reads `dirs.tsv`, applies its deltas (adds ranks, takes the later
`last`), ages, writes the result to a temporary file, fsyncs, renames it over `dirs.tsv`,
and releases the lock. Two instances that exit at the same time therefore serialise, and
neither loses the other's visits. The lock wait is bounded (2 s); on timeout the session
skips saving and prints why on stderr after the terminal is restored.

**zoxide.** Omarchy ships zoxide. When `jump.zoxide` is `"auto"` (the default) and
`zoxide` is on `PATH`, the first `Ctrl+D` of a session runs `zoxide query --list --score`
on the directory-store thread, with a 1 s timeout, and merges the result: a path's score is
the larger of the two. manycommander never writes to zoxide's database. `jump.zoxide =
"off"` disables it.

### 3.4 `z` on the command line

`z <keywords>` is intercepted like `cd` (M1 6): the active panel loads the best-scoring
match (same rules as the dialog, zoxide results included once loaded). No match shows
"z: no match". `z` with no keywords opens the dialog.

## 4. Quick filter

`Ctrl+F` opens the filter line on the status row, pre-filled with the panel's current
filter. Each keystroke re-filters the panel at once (P-12). `Enter` keeps the filter and
closes the line; `Esc` clears the filter and closes; `Ctrl+F` again closes and keeps it.

- A filter without `*`, `?` or `[` matches as an ASCII case-insensitive substring of the
  name. With one of them, it is a glob over the whole name (the M1 mark-glob matcher),
  ASCII case-insensitive.
- Directories are filtered like files; `..` always stays.
- The filter survives a refresh (watcher, `Ctrl+R`, job end) and is cleared when the
  panel changes directory. In a results tab it matches the relative path.
- The footer shows `N of M entries (filter: text)`.
- Marks on entries the filter hides are kept but do not count and are not acted on (I-8);
  they count again when the filter goes away. The hidden-file toggle follows the same rule
  (an M1 edge case that I-8 fixes).
- The cursor stays on its entry when that entry stays visible; otherwise it moves to the
  first visible row.

## 5. Find files and the results tab

### 5.1 The find dialog

`Alt+F7` opens a form:

| Field | Default |
|---|---|
| Search in | the active panel's directory (a results tab: its root) |
| Name | empty (matches every name) |
| Containing text | empty (no content search) |
| `[x]` Hidden entries | the active panel's hidden toggle |
| `[x]` Stay on this filesystem | on |
| `[ ]` Match case | off |

`Enter` starts the search. It opens a new results tab on the active side, titled
`find: <name> <text>`, and shows results as they arrive.

### 5.2 Matching

- **Name.** Without `*`, `?` or `[`: a substring of the entry name. With them: a glob over
  the whole name. ASCII case-insensitive unless "Match case". Directories, files, symlinks
  and special files all match by name.
- **Content.** A literal byte string (UTF-8 of what was typed). Only regular files whose
  name matches are read (I-10). Case-insensitive search folds ASCII in both needle and
  data. Binary files are searched like text; there is no binary detection. A match that
  spans a read-chunk boundary is found (chunks overlap by `needle.len() - 1` bytes). A
  sparse file's holes are not read (`SEEK_DATA`/`SEEK_HOLE`, with the section 9.1 error
  rules): a hole holds only NUL bytes, so a needle that needs hole bytes is not found
  there, which the F1 help states.
- **Traversal.** `.` and `..` from `getdents64` are never matched or queued. Symlinks are
  never followed (I-5): a symlinked directory is a result candidate by name, never
  descended. Every `statx` of the walk uses `AT_SYMLINK_NOFOLLOW | AT_NO_AUTOMOUNT`; the
  one before a directory is opened, and the one for a result's columns, also use
  `AT_STATX_DONT_SYNC`, so a search never waits on a mount it does not enter (a stalled
  network or FUSE mount; A-FD-7). A
  search keeps a visited set of directory `(st_dev, st_ino)`; a repeat (a bind-mount loop)
  is not descended again, whether or not "Stay on this filesystem" is on. With "Stay on
  this filesystem", a directory whose `mnt_id` differs from the root's is not descended
  (it can still match by name). Without "Hidden entries", names starting with `.` are
  neither matched nor descended. A directory that cannot be opened or read is counted in
  the search's error total, which the tab's footer shows; the search goes on.

### 5.3 The search engine

A search runs on a pool of `min(8, available_parallelism)` threads that share a
last-in-first-out work stack of directories. Each item is `(parent fd, name, relative
path)`:

1. `openat(parent, name, O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)`, then `statx` of the fd
   (identity and `mnt_id`); the item's reference to the parent fd is dropped at once. A
   directory that changed into a symlink fails with `ELOOP` and is counted as an error,
   not followed. A repeated identity (visited set) is skipped.
2. `getdents64` in large batches. For each entry: `d_type` decides directory versus other
   without a `statx`; `DT_UNKNOWN` costs one `statx(AT_SYMLINK_NOFOLLOW |
   AT_NO_AUTOMOUNT)`, and only a result that says directory is queued.
3. Name matches (and, with content, only regular files whose content matches) are
   `statx`ed for the columns and appended to the worker's batch.
4. Subdirectories go onto the stack with the directory's fd shared by `Arc`; the fd closes
   when its last queued child has been opened.

**Fd bound.** Because the stack is last-in-first-out, each worker mostly descends
depth-first, and an fd stays open only while a child of its directory waits on the stack.
Open directory fds are therefore bounded by about `workers x depth`, not by the tree size.
`EMFILE` on a directory counts as an error for that directory; the search goes on, and
`RLIMIT_NOFILE` is already raised to the hard limit (M1 4.3).

Batches of up to 4096 results (the first batch at most 256, as in M1 3.1) go to the UI.
The results arrive in traversal order and the tab sorts them in (the M1 150 ms re-sort
spacing applies). `Esc` in the results tab while it searches, and while no dialog is
open, sets the search's cancel flag; the workers check it between entries and between
content chunks, so a responsive filesystem stops the search within 100 ms (A-FD-4). A
worker blocked in the kernel is abandoned (section 2.3 caps abandoned searches). The
search stops at 1,000,000 results and says so.

Content search reads through a per-worker 256 KiB buffer and finds the needle with the
`memchr` crate's `memmem` finder (SIMD). A file is opened through the M1 4.3 sequence, so
a FIFO or device is never opened (I-10).

### 5.4 Results tab behaviour

| Key | In a results tab |
|---|---|
| `Enter` on a file | "Go to file": the tab navigates to the file's directory with the cursor on it; `Alt+Left` returns to the results. (In a directory panel `Enter` opens a file with `xdg-open`; here `F3`/`F4` open it. The F1 help says so.) |
| `Enter` on a directory | The tab navigates into it; `Alt+Left` returns |
| `Backspace`, `Alt+Up` | The tab navigates to the search root |
| `F3`, `F4`, `Alt+Enter`, `Alt+P` | As in a directory panel, on the result's full path (`Alt+Enter` inserts the relative path, which the command line resolves from the root, the tab's working directory) |
| `F5`, `F6`, `F8`, `Shift+F8`, `Alt+L`, `Alt+A`, `Ctrl+M` | Grouped jobs (section 2.2) |
| `Shift+F6` | Rename of the result in its own directory |
| `F7`, `Shift+F4`, `Shift+F2` | Refused with "not in search results" |
| `Ctrl+R`, and after every job | Re-stat (section 5.5) |
| `Esc` | Cancels the running search |

A results tab has no `..` row and no inotify watch. It is not saved in `state.toml`;
on exit it is saved as a tab on its root directory.

### 5.5 Re-stat

A re-stat runs on a listing thread: it opens each result's directory once with the
section 2.2 component walk (root, then `O_NOFOLLOW` per component) and `statx`es the
leaf with `AT_SYMLINK_NOFOLLOW`. Results that no longer exist, or whose directory walk
meets a symlink, are dropped; the rest get fresh metadata. Marks survive by name, as in an
M1 refresh.

## 6. Multi-rename

### 6.1 The dialog

`Ctrl+M` opens the multi-rename tool for the selection (I-8; in a results tab, grouped by
directory). It fills the panel area: a form at the top and a live preview below.

| Field | Default | Meaning |
|---|---|---|
| Name mask | `[N]` | New name without extension (6.2) |
| Extension mask | `[E]` | New extension; empty means no extension and no dot |
| Search | empty | Text to replace in the result of the masks |
| Replace | empty | Replacement (`$1`..`$9` in regex mode) |
| `[ ]` Regex | off | Search is a regular expression (`regex::bytes`) |
| `[ ]` Match case | off | For Search |
| Case | unchanged | unchanged / lower / upper / title |
| Counter start, step, digits | 1, 1, 1 | For `[C]` |

The preview lists `old -> new` for every selected entry, in panel order, with a status per
row: unchanged, ok, or an error (6.4). `PgUp`/`PgDn` scroll it. `Enter` runs the rename
only when no row has an error; otherwise it shows the first error. `Ctrl+Z` undoes the last
multi-rename of this session (6.5).

### 6.2 Masks

| Placeholder | Value |
|---|---|
| `[N]` | Name without extension (the M1 extension rule: after the last `.` that is not the first byte) |
| `[N2]`, `[N2-5]`, `[N2-]`, `[N-3]` | Characters 2, 2 to 5, 2 to end, the last 3 of the name (1-based) |
| `[E]`, `[E1-3]` | Extension, and character ranges of it |
| `[C]` | Counter: start + index x step, zero-padded to digits |
| `[P]` | Name of the parent directory |
| `[Y]`, `[M]`, `[D]`, `[h]`, `[m]`, `[s]` | Modification time in the local time zone: year, month, day, hour, minute, second (zero-padded) |
| `[[`, `]]` | Literal `[` and `]` |

A character is a Unicode scalar value where the name is valid UTF-8, and one byte where it
is not, so invalid names survive byte-exactly. A range is clamped to the text: `[N2-5]`
of `ab` is `b`, `[N4]` of `ab` is empty, and `[N-3]` of `ab` is `ab`. An unknown or
malformed placeholder is a mask error. Order of application: masks, then search and
replace, then case.

**Search and replace.** Literal mode replaces every non-overlapping occurrence, left to
right; with "Match case" off, ASCII letters compare case-insensitively. Regex mode uses
`regex::bytes::RegexBuilder` with a 1 MiB compiled-size limit and `case_insensitive`
from "Match case"; `$1`..`$9` and `${name}` expand in the replacement. The pattern is
compiled once per edit of the field, not per name.

**Case.** The case mode applies to the new name's name part and extension separately (the
M1 extension rule splits them). `lower` and `upper` apply Unicode case mapping to the
valid UTF-8 runs of both parts and leave invalid bytes unchanged. `title` lowercases every
character of the name part, then uppercases the first character of each word, where a word
starts at the beginning and after a space, `_`, `-` or `.` inside the name part; it
lowercases the extension (`my photo.JPG -> My Photo.jpg`).

### 6.3 Execution

The rename is a job (`JobSpec::Rename { groups, renames }`), so it follows M1 4.4 (one
job at a time, report, refresh). Per directory:

1. Open the directory once (section 2.2); `statx` each old name (`AT_SYMLINK_NOFOLLOW`)
   and record its identity. A missing entry fails with "disappeared".
2. Build the mapping; unchanged names drop out.
3. **Dependencies.** `statx(AT_SYMLINK_NOFOLLOW)` each new name. When it returns the
   identity of another entry of the set, entry A depends on entry B (A's new name is held
   by B). Comparing identities rather than bytes makes a case-insensitive directory work:
   `Foo -> bar` with `Bar -> foo` is a cycle there and two independent renames on a
   case-sensitive directory. A new name that returns the entry's own identity is a
   case-only change (M1 4.8 intermediate-name path).
4. **Order.** An entry is ready when it depends on no pending entry. Ready entries rename;
   each successful rename frees an old name and can make dependents ready (chains resolve
   in order). An entry whose rename fails or is skipped keeps occupying its old name, so
   the entries that depend on it are skipped with "the destination exists" and never
   passed to cycle breaking. Only the strongly connected components that remain when
   nothing is ready are cycles.
5. **Cycles.** One member of a cycle is renamed to a temporary name
   `.mc-rename-<16 hex>` in the same directory (`RENAME_NOREPLACE`; on `EEXIST` a new
   random suffix, as for M1 partial names). That frees its old name; the rest of the cycle
   resolves; the temporary finally renames to its target.
6. Every rename re-checks the old name's identity first (a different inode fails with
   "type changed") and uses `renameat2(..., RENAME_NOREPLACE)`. `EEXIST` means something
   outside the set holds the new name: the entry is skipped with "the destination exists"
   (I-3, I-9).
7. **Recovery.** After an error or a cancel, a member that sits under a temporary name is
   renamed back to its original name if that name is free; otherwise the report names the
   temporary path (I-9). Cancel is checked between renames.

A rename does not fsync (as `mv`, NFR-DUR). Two new names that differ only in case on a
case-insensitive directory are not detected in advance; the second fails with `EEXIST` and
is skipped, so nothing is overwritten.

### 6.4 Preview errors

Computed in memory on every keystroke (P-14): a mask error (unknown or malformed
placeholder), a regex that does not compile or exceeds the size limit, an empty name,
`.`, `..`, a name with `/` or NUL, a name longer than 255 bytes, two entries of one
directory with the same new name, and a new name that equals a listed entry of the panel
outside the set (the execution re-checks this; the preview is advisory for entries it
cannot see). Any error blocks `Enter`.

### 6.5 Undo

Every rename job, completed, cancelled or failed, returns the renames it performed with
identities. `App` keeps the last non-empty one. `Ctrl+Z` in the multi-rename dialog runs the reverse mapping as a new rename job, and
each entry is renamed back only if the entry under its new name still has the recorded
identity. The undo record is dropped after it runs or when the app exits.

## 7. Compare directories

`Shift+F2` opens a small form: `(•) by date and size  ( ) by content`, and
`[x] include directories`. It needs two directory panels.

Compare runs on the compare thread, so the UI thread never spends the compare's time
(P-1). The UI thread hands it a copy of each panel's visible entries (name, kind, size,
mtime) and the two directory paths; only visible entries take part (I-8). The thread
reads each directory's filesystem type with one `fstatfs` for the mtime resolution, and
sends back the entry indices to mark on each side and the summary. The UI applies the
marks only if both panels still show the same listing generation; otherwise it discards
them and says "the directories changed; compare again".

**By date and size** reads no file. For each name:

| Case | Marked |
|---|---|
| Only on one side | on that side (directories only with "include directories") |
| Both sides, both regular files, one strictly newer at the coarser mtime resolution of the two filesystems (M1 4.5) | the newer side |
| Same mtime at that resolution, different size | both sides |
| Same mtime and size, or both directories, or different types | neither |

Existing marks in both panels are cleared when the marks are applied. The status row
summarises: `left: 3 newer, 5 only here; right: 1 newer, 2 only here; 1 differ in size`.

**By content** first applies the date-and-size rules for unique names, then compares every
same-named pair of regular files: different sizes differ without reading; equal sizes are
read on the compare thread in 1 MiB chunks through the M1 4.3 open sequence (I-10) and
compared. Differing pairs are marked on both sides. The status row shows progress; `Esc`
cancels.

## 8. Links and attributes

### 8.1 Create links (`Alt+L`)

A form: the destination (for one entry `<other panel>/<name>`, for several the other
panel's directory) and a choice `(•) symbolic, relative ( ) symbolic, absolute ( ) hard`.
It runs as `JobSpec::Link`.

- **Symbolic, absolute.** The target is the source's lexical absolute path: the group's
  `root` joined with its `sub` components and the name, without resolving symlinks.
  **Symbolic, relative.** The
  target is the lexical relative path from the link's directory to the source, computed
  on both lexical absolute paths (`../a/b`).
- **Hard.** `linkat(srcdir, name, dstdir, newname, 0)`: it links the entry itself, never
  a symlink target (I-5). A directory is skipped with "directories cannot be hard-linked";
  `EXDEV` fails with "hard links cannot cross filesystems".
- A link never replaces anything: `symlinkat` and `linkat` fail atomically with `EEXIST`,
  which raises a "link exists" question with Skip, Skip all, Rename and Cancel (no
  Overwrite).
- A link is created under its final name directly: a new name that did not exist cannot
  show partial content, so no temporary name is needed (I-2 holds trivially).

### 8.2 Change attributes (`Alt+A`)

A form:

| Field | Meaning |
|---|---|
| Mode | Octal or symbolic (grammar below). Empty: unchanged. The field starts empty; when one entry is selected, its label shows the entry's current octal mode as a hint (`Mode (now 0644)`), so an untouched field never applies a mode, recursively or not |
| Modification time | `YYYY-MM-DD HH:MM[:SS]` in the local time zone, or `now`. Empty: unchanged |
| `[ ]` Recursive | Apply to everything below selected directories |

**Mode grammar.** Octal: one to four octal digits (`644`, `0644`, `4755`); it sets exactly
those bits (set = the value, clear = every other bit of `07777`). Symbolic: comma-separated
clauses `[ugoa]*[+-=][rwxXst]*`, as chmod(1), with two differences stated in the form's
help: a clause without a class means `a` and ignores the umask, and `X` means "execute
where the entry is a directory or already has an execute bit". `s` is setuid for `u` and
setgid for `g`; `t` is the sticky bit. `=` clears the class's `rwx` (and its `s`/`t`)
bits, then sets. Anything else is a form error that blocks `Enter`.

The mode parses into a set mask and a clear mask per entry: `new = (old & !clear) | set`
(with `X` resolved per entry). The form shows the result for the first selected entry as
`rw-r--r-- -> rwxr-xr-x`. It runs as `JobSpec::Attr`. The job does not scan first: a
directory the change makes traversable could not be scanned before the change, so it
traverses while it executes and reports progress as a running count.

- Each entry is opened `O_PATH | O_NOFOLLOW` and checked with `fstat`. Its mode changes
  through `chmod("/proc/self/fd/<n>")` and its time through `utimensat` on the same path
  with `UTIME_OMIT` for the access time. Both reach that inode, never a symlink target
  (`fchmod` on an `O_PATH` fd fails with `EBADF`, so it is not used).
- A symlink keeps its mode (Linux has none to change; the call fails with `EOPNOTSUPP`).
  When the form sets a time, the link's own time is set with `utimensat(dirfd, name,
  AT_SYMLINK_NOFOLLOW)`; its target is untouched (I-5). When the form only sets a mode, the
  link is skipped with "symbolic links have no mode of their own".
- Recursion follows the M1 4.2 table: it descends into subvolumes and skips mount points,
  as permanent delete does.
- **Directory order.** A directory first gets the intermediate mode `old | set` through its
  `O_PATH` fd. Then it is reopened `O_RDONLY | O_DIRECTORY | O_NOFOLLOW` (its identity
  compared with the `O_PATH` fd's) to read its entries, and its children are processed.
  Then it gets its final mode. Adding `r` or `x` therefore makes a directory readable
  before it is read, and removing them happens only after its children are done. If the
  final change fails, the report names the intermediate mode the directory was left with.
- `EPERM` (not the owner) raises the error question (M1 4.5).
- An entry whose mode and time would not change counts as unchanged, not as done.

## 9. Copy fidelity: sparse files and hard links

Both apply to F5 and to the copy step of a cross-filesystem move (M1 4.7, 4.8). The M1
guarantees are unchanged: I-2 (temporary file and atomic commit), I-3, and I-1 through
group commit.

### 9.1 Sparse files

A regular file takes the sparse path when its allocated size (`st_blocks x 512`) is
smaller than its size; that test is only a fast path, and every other file keeps the M1
contiguous loop. The sparse path walks the source's data segments:

1. `lseek(fd, off, SEEK_DATA)`. `ENXIO` means no data after `off`: the walk ends.
   `EINVAL` or `EOPNOTSUPP` (no hole support in the source filesystem) abandons the sparse
   path for this file before anything was written and uses the M1 contiguous loop.
2. `lseek(fd, data, SEEK_HOLE)` gives the segment's end.
3. The segment is copied with `copy_file_range` at explicit input and output offsets, in
   a loop that handles short counts like M1 4.7 step 3; its fallback is `pread`/`pwrite`
   at the same offsets. Writing at an offset past the destination's end leaves the range
   before it as a hole.
4. At the end, `ftruncate` to the source size creates a trailing hole; an all-hole file
   is only this `ftruncate`.

Skipped ranges stay holes in the destination (measured on btrfs and tmpfs). A destination
filesystem without holes (vfat) receives zeros through `ftruncate` and the positioned
writes, which is the same content. Progress counts hole bytes as done when they are
skipped, so the percentage stays true to the file size. The change check before a move
commits (M1 4.8 step 2) is unchanged.

### 9.2 Hard links

Only regular files take part. The plan counts, for every source inode `(st_dev, st_ino)`
with `nlink > 1`, how many of its names lie inside the selection (its **in-set names**). A
job keeps a map from such an inode to the first destination it committed for it:
`(destination directory fd, name, destination identity, source size, mtime and ctime)`.
When the job meets another in-set name of a mapped inode:

1. `statx` the first destination by name in its directory. If it is gone (`ENOENT`), or
   its identity is not the recorded one (it was replaced), or the source's current size,
   mtime or ctime differ from the recorded ones (the file changed in between), copy the
   data as usual.
2. Otherwise `linkat(first destination dir, first name, current destination dir,
   temporary name)`, then commit the temporary name with the M1 4.7 step 5 rules
   (`RENAME_NOREPLACE`, "file exists", Overwrite as an atomic replace). The link appears
   under its final name atomically (I-2).
3. `linkat` failing with `EXDEV`, `EMLINK`, `EPERM` or `EOPNOTSUPP` falls back to copying
   the data. The report counts fallbacks in one note ("N hard links were copied as
   separate files") so it states the structure the job left (I-7).

Names whose other links lie outside the selection are copied as independent files, as
`cp -a` does for links outside its arguments.

**Move.** Unlinking one name of an inode changes that inode's `ctime` and `nlink`, so the
M1 flush check (M1 4.8 step 5.3) would fail for its other names, and relaxing that check
would let a rewrite that restores the mtime slip through (I-1). The move therefore
**defers** the unlinks of a multi-linked inode:

- A committed name of an inode that still has unsettled in-set names (not yet committed,
  skipped or failed) is not unlinked at a flush; it stays pending into later batches.
  Its destination is still covered by the batch's `syncfs`.
- When the last in-set name of the inode settles, the next flush takes one `statx` of the
  inode, before any of its names is unlinked, and compares it with each committed name's
  `S0` in full (identity, size, mtime, ctime, and `nlink` equal to the `nlink` at the
  first copy). The names whose `S0` matches are then unlinked one after the other with no
  further metadata check; the others are kept ("source changed; kept both"). The residual
  race is the M1 one between `statx` and `unlinkat`, extended over those consecutive
  unlinks.
- A source directory that still holds a deferred name when its children are done is not
  removed then; it is removed after the final flush, in the order the directories
  finished (post-order), if it is empty by then.

## 10. Keymap

Phase 2 adds these chords. The M1 section 8 ownership rule decides what they do while
the command line holds text:

| Key | Line empty | Line has text | Protocol |
|---|---|---|---|
| `Ctrl+D` | Directories dialog (hotlist and frecency) | ignored | legacy works |
| `Ctrl+F` | Quick filter | ignored | legacy works |
| `Ctrl+M` | Multi-rename | ignored (it must not run the line) | needs the kitty keyboard protocol: legacy `Ctrl+M` is `Enter` |
| `Alt+F7` | Find files | same (an F-key) | xterm modifier encoding |
| `Shift+F2` | Compare directories | same (an F-key) | xterm modifier encoding |
| `Alt+L` | Create links | same (always active, like `Alt+=`) | legacy works |
| `Alt+A` | Change attributes | same (always active) | legacy works |

`Ctrl+Z` acts only inside the multi-rename dialog (undo) and is ignored elsewhere; raw
mode clears `ISIG`, so it arrives as a key, and a real `SIGTSTP` still suspends (M1
A-UI-3). manycommander pushes only `DISAMBIGUATE_ESCAPE_CODES`, which is enough to tell
`Ctrl+M` from `Enter`.

None of these is bound by default in Ghostty (`ghostty +list-keybinds --default`), foot
(`[key-bindings]` of the default `foot.ini`; its `Control+f` and `Control+d` exist only
in the search and URL modes), the Omarchy Hyprland bindings (all use `SUPER`, apart from
`F9`, `Alt+Tab` variants, `Alt+Print`, `Ctrl+Alt+Delete` and media keys), Alacritty or
Kitty (both put their defaults on `Ctrl+Shift`). The plan records that audit. `Ctrl+Shift+F5` (Total Commander's link key) is not used because Kitty binds
it to reloading its config.

The function-key bar's `F2` slot stays empty; `Shift+F2` is not shown there.

## 11. Non-functional requirements

Reference conditions are M1 13.1's. The M1 targets P-1 to P-9 and NFR-* keep holding.
New targets:

| ID | Requirement | Target |
|---|---|---|
| P-10 | Name search | 100k-entry tree: complete <= 300 ms, first results on screen <= 50 ms, and within 1.5x of `fd -uu` on the same tree |
| P-11 | Content search | 1 GiB of text in 10k files: within 2x of `rg -uuu -F -l` |
| P-12 | Quick filter | Re-filter of 100k entries <= 16 ms (each keystroke meets P-1) |
| P-13 | Compare by date and size | Two 100k-entry listings compared <= 30 ms on the compare thread; the UI thread's share (copying the visible entries) <= 5 ms |
| P-14 | Multi-rename preview | 10k names recomputed and checked <= 16 ms per keystroke |
| P-15 | Directories dialog | 5000 frecency entries filtered and ranked <= 16 ms per keystroke; P-2 unchanged with that store present |
| P-16 | Sparse copy | A 16 GiB file with 8 MiB of data, btrfs to tmpfs: <= 1 s; destination allocation <= source allocation + 1 MiB |
| P-17 | Hard-link copy | 10k hard-link pairs: no slower than copying the same 20k files without links; destination keeps 10k pairs |
| P-6b | Memory | Both panels on 100k-entry directories plus a 100k-result tab: <= 60 MB RSS |

Other requirements:

| ID | Requirement |
|---|---|
| NFR-SEC | Unchanged. The `regex` crate is the only new parser of user input with non-linear risk; it guarantees linear-time matching. zoxide runs by argv with a timeout, never through a shell. |
| NFR-SUP | New dependencies: `memchr` (content search) and `regex` (multi-rename). Each states its reason in the commit that adds it; `cargo deny` stays clean. |
| NFR-RES | Find threads and directory fds are bounded (section 5.3: about `workers x depth` fds; at most two abandoned searches, section 2.3). Results tabs hold no inotify watch. At most three results places per panel history. |
| NFR-REL | Find, compare and store threads run under `catch_unwind`; a panic ends that search or compare with an error message, and the app stays up. |

## 12. Acceptance checks

*auto*, *bench* and *manual* as in M1 11. All M1 checks (A-FS, A-TR, A-DEL, A-UI, A-TH,
A-LN, A-PUB, A-P) must still pass.

### 12.1 Directories and filter

| ID | Check | Type |
|---|---|---|
| A-DJ-1 | Bookmarks: add, remove, persist and reload, including a non-UTF-8 path; the file is replaced atomically; a `hotlist.toml` that does not parse is never overwritten | auto |
| A-DJ-2 | Frecency: ranking with the four time weights, aging past 10000, keyword order, last keyword in the last component, the active directory excluded; `z` loads the best match and reports "no match" | auto |
| A-DJ-3 | Two writers that overlap in time (two threads each holding their own deltas and saving concurrently) both keep their visits; aging uses the zoxide factor and decimal ranks round-trip; unparsable lines are skipped | auto |
| A-DJ-4 | A frecency entry whose directory is gone fails to load, the panel stays, and the entry is dropped | auto |
| A-DJ-5 | P-15, including P-2 with a 5000-entry store | bench |
| A-QF-1 | Substring and glob filtering, case folding, `Esc` clears, directory change clears, refresh keeps, cursor rules | auto |
| A-QF-2 | I-8: with a filter, F5 and F8 act only on visible marked entries and the footer counts them; when only hidden marks exist, the footer shows none and F8 acts on the cursor entry, or does nothing on `..`; marks return when the filter is cleared; the same with the hidden toggle | auto |
| A-QF-3 | P-12 | bench |

### 12.2 Find

| ID | Check | Type |
|---|---|---|
| A-FD-1 | Name search: substring and glob, case rules, hidden toggle; files, directories, symlinks and special files match by name; `.` and `..` never appear or get descended with hidden entries on; a symlinked directory and a symlink loop are not followed; a bind mount inside the tree (`unshare -rm`) is not descended with "stay on this filesystem" and is descended without it; a bind mount of an ancestor inside the tree (a cycle) finishes with each directory visited once | auto |
| A-FD-2 | Content search: literal and case-folded matches, a match across a 256 KiB chunk boundary, symlinks to files not read, a FIFO in the tree never opened (the search completes), an unreadable file counted as an error, a sparse file's data found without reading its holes | auto |
| A-FD-3 | Results tab: names with a newline and invalid UTF-8 display escaped; F5 of results from three directories into the other panel copies all, and two same-named results raise "file exists" for the second; F8 trashes results from two directories; Enter goes to the file and `Alt+Left` returns; `Ctrl+R` drops vanished results | auto |
| A-FD-4 | `Esc` stops a search of a large tree within 100 ms; the tab says "cancelled" and keeps its results | auto |
| A-FD-5 | P-10 | bench |
| A-FD-6 | P-11 | bench |
| A-FD-7 | A search whose tree contains a stopped FUSE mount (`scripts/fixtures/stall-fuse.sh`) keeps the UI responsive; `Esc` cancels it | manual |

### 12.3 Multi-rename

| ID | Check | Type |
|---|---|---|
| A-MR-1 | Mask engine: every placeholder of 6.2, search and replace (literal, regex with groups, case folding), the case modes, byte-exact handling of invalid UTF-8 | auto |
| A-MR-2 | A swap `a <-> b`, a 3-cycle, a chain, a chain blocked by an outside entry (the blocked entry and its dependents are skipped, never cycle-broken) and a mix end with the intended names; no `.mc-rename-` name remains | auto |
| A-MR-3 | A new name held by an entry outside the set: that entry is skipped and the other file is untouched; duplicate new names and invalid names are refused before any write | auto |
| A-MR-4 | Failpoints at each rename of a cycle and of the temporary name (error and cancel): every inode is afterwards reachable under its original name, its new name, or a temporary name the report states; a cancelled job still returns its undo record | auto (failpoint) |
| A-MR-5 | Undo restores the original names; an entry replaced since (another inode under the new name) is left alone and reported | auto |
| A-MR-6 | On a case-insensitive directory (tmpfs `casefold` under `unshare -rm`): a case-only rename, and the swap `Foo -> bar`, `Bar -> foo` treated as a cycle. The test skips with a printed reason when the kernel refuses the casefold mount | auto |
| A-MR-7 | P-14 | bench |

### 12.4 Compare, links, attributes

| ID | Check | Type |
|---|---|---|
| A-CD-1 | Date-and-size rules of section 7 on both sides, including the 2 s vfat resolution (the compare function given the vfat `f_type`), directories with and without the option, hidden and filtered entries excluded; marks for a listing that changed meanwhile are discarded | auto |
| A-CD-2 | Content compare marks same-size differing pairs, leaves identical ones, never opens a FIFO, and cancels | auto |
| A-CD-3 | P-13 | bench |
| A-LK-1 | Symbolic links, absolute and relative (the relative one resolves to the source from its directory), for one and several entries; an existing name is never replaced | auto |
| A-LK-2 | Hard links share the inode; a symlink is hard-linked as the link itself; a directory is refused; `EXDEV` is reported | auto |
| A-AT-1 | The mode grammar (octal of one to four digits, `u+x,g-w,o=r`, `a-x`, `+x`, `X`, `u+s`, `g+s`, `+t`, errors); octal and symbolic modes on files and directories, recursive; a symlink in the tree keeps its target's mode unchanged, and a time-only change sets the link's own mtime; a mount point inside the tree (`unshare -rm`) is skipped; mtime set exactly on files and directories | auto |
| A-AT-2 | Recursive `a-rx` and `u+rx` on a tree: removing does not prevent the traversal of the directory's children; adding makes a mode `000` directory readable before its children are processed; a failed final change reports the intermediate mode | auto |

### 12.5 Copy fidelity

| ID | Check | Type |
|---|---|---|
| A-SP-1 | Files with holes at the start, in the middle and at the end, and an all-hole file, copied and moved btrfs -> tmpfs, tmpfs -> btrfs and within btrfs: identical hash and size; destination allocation <= source allocation + 64 KiB; `SEEK_DATA` failing with `EINVAL` (failpoint) falls back to the contiguous copy with the same content | auto |
| A-SP-2 | P-16 | bench |
| A-HL-1 | A tree with link pairs, a triple and a file linked from outside the selection: copy and cross-filesystem move keep pairs and the triple as links (`nlink` and shared inodes), copy the outside-linked file independently, and a move reports no "source changed" and removes every source directory, including one that held a deferred name | auto |
| A-HL-2 | The first destination of an inode replaced before its second link is reached: the second is copied, not linked to the stranger. A move where the source inode is rewritten with its mtime restored after the first name is committed: no source name of that inode is unlinked whose `S0` no longer matches, and the new bytes survive at the source | auto (failpoint) |
| A-HL-3 | `linkat` failing with `EMLINK` or `EPERM` (failpoint) falls back to copying the data | auto (failpoint) |
| A-HL-4 | P-17 | bench |

## 13. Alternatives considered

| Alternative | Decision |
|---|---|
| Search results in a dialog list instead of a tab | Rejected. A tab reuses marks, sorting, the quick filter, F3/F4 and every verb; a dialog would need its own copies of each. |
| Multi-component names relative to the search root in jobs | Rejected. Resolving `a/b/c` with `openat` follows symlinks in `a` and `b` (I-5). Grouping by parent directory keeps every name a single component. |
| `walkdir` / `ignore` crates for find | Rejected. They resolve paths rather than walking directory fds with `O_NOFOLLOW` identity checks, and `ignore` applies gitignore rules a file manager must not apply by default. The engine is small. |
| `fd` or `rg` as external processes | Rejected. Results would arrive as text paths (newlines in names break parsing without `-0`), cancellation and fd safety would be out of manycommander's hands, and I-10 could not be guaranteed. |
| Two-phase multi-rename through temporaries for every entry | Rejected. Ordering chains and using one temporary per cycle touches each file once, and leaves nothing behind in the common case. |
| Tri-state checkboxes for attributes | Rejected in favour of chmod syntax: it expresses set, clear and unchanged per bit in one line, and every Linux user already knows it. |
| Writing visits to zoxide (`zoxide add`) | Rejected. manycommander reads zoxide's ranking but does not change another tool's data. |
| Frecency store in `state.toml` | Rejected. It can grow to thousands of lines; loading it must not delay the first frame (P-2), and it is written by merge, not replaced. |

## 14. Release

Phase 2 ships as the next minor version (the minor number of `Cargo.toml` goes up by one):

- The new version in `Cargo.toml` and a `CHANGELOG.md` entry.
- `.github/workflows/release.yml`: on a `v*` tag, build the release binary for
  `x86_64-unknown-linux-gnu` on GitHub's runner, package `manycommander-<version>-x86_64-linux.tar.gz`
  with the license and README, publish it with its SHA-256 as a GitHub release whose notes
  are the changelog entry.
- The site's documentation pages (keys, file operations, a new "Find and rename" page) and
  the screenshots describe phase 2.
- `.publish-allow.tsv` allows the product's own version in `CHANGELOG.md` and the release
  workflow's tool pins, like the existing CI rows.

## Appendix A. Review resolution

The first draft (commit `d9aa2ec`) went through an independent adversarial model review
(grok) on 2026-09-28. Its probes ran on this kernel in temporary directories on tmpfs and
btrfs; `unshare` was not available to it, so vfat, ext4 and tmpfs casefold were not
probed. Findings 1-13 were marked required, 14-16 suggestions.

| # | Finding | Resolution |
|---|---|---|
| 1 | Find would descend `.`/`..`; no visited set; `DT_UNKNOWN` statx flags unnamed | Accepted (5.2, 5.3); A-FD-1 extended |
| 2 | The find fd bound was false; abandoned workers uncapped | Accepted: LIFO stack with per-item parent references and the stated bound (5.3); abandoned searches capped at two (2.3); NFR-RES amended. The suggested root-relative path walk per directory was not adopted, because it costs `depth` opens per directory (P-10) |
| 3 | The relaxed hard-link flush check breaks I-1 | Accepted: deferred unlinks with one full check per inode (9.2); map restricted to regular files, `ENOENT` and ctime handled; A-HL-1, A-HL-2 extended |
| 4 | `SEEK_DATA` errors undefined | Accepted (9.1); A-SP-1 extended |
| 5 | `fchmod` on `O_PATH` fails; the `O_PATH` fd cannot be listed | Accepted (8.2): `/proc/self/fd` chmod and `utimensat`, reopen for reading after the intermediate mode, no pre-scan; A-AT-2 extended |
| 6 | Grouped sources re-opened joined paths; wrong inside-source set; link target lost `sub` | Accepted (2.2, 5.5, 8.1) |
| 7 | Blocked chains treated as cycles; temporary collisions; undo lost on cancel | Accepted (6.3, 6.5); A-MR-2, A-MR-4, A-MR-6 extended |
| 8 | Aging differed from zoxide; the merge could lose visits | Accepted (3.2, 3.3); A-DJ-3 extended |
| 9 | Compare on the UI thread broke P-1 | Accepted: the whole compare runs on the compare thread (7); P-13 amended |
| 10 | I-8's selection predicate and cursor fallback were ambiguous | Accepted (1.2); A-QF-2 extended |
| 11 | The mask engine could not fail | Accepted with one change: ranges past the end clamp (as in Total Commander) instead of failing, which is defined and testable; invalid regexes are errors; case modes defined (6.2, 6.4) |
| 12 | Plan ordering and checks that could not fail | Accepted in the plan: T7 after T4, the design amended before T1, A-MR-6 skips with a reason |
| 13 | Ownership of the new chords with a non-empty line | Accepted (10) |
| 14 | Content search read holes; compare could run before `f_type` | Accepted: holes skipped (5.2); the compare thread reads `f_type` itself (7) |
| 15 | Enter and Esc in the results tab | Accepted (5.3, 5.4) |
| 16 | Chmod grammar; symlink times; silent link fallbacks | Accepted (8.2, 9.2) |
| -- | (T3) A pre-filled mode field would apply the entry's mode to a whole tree when only a time was typed with "Recursive" | The field starts empty; the current mode is a hint in the label (8.2) |

