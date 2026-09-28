---
title: manycommander phase 2 design
type: spec
status: draft
owner: manycommander
source: implemented/2026-09-27-manycommander-design.md
created: 2026-09-28
updated: 2026-09-28
---
# manycommander phase 2 design

Contents: [1 Outcome](#1-outcome) · [2 Architecture](#2-architecture-changes) ·
[3 Directories](#3-directory-hotlist-and-frecency-jump) · [4 Quick filter](#4-quick-filter) ·
[5 Find](#5-find-files-and-the-results-tab) · [6 Multi-rename](#6-multi-rename) ·
[7 Compare](#7-compare-directories) · [8 Links and attributes](#8-links-and-attributes) ·
[9 Copy fidelity](#9-copy-fidelity-sparse-files-and-hard-links) · [10 Keymap](#10-keymap) ·
[11 NFRs](#11-non-functional-requirements) · [12 Acceptance](#12-acceptance-checks) ·
[13 Alternatives](#13-alternatives-considered) · [14 Release](#14-release)

## 1. Outcome

Phase 2 brings manycommander to daily-driver parity with Double Commander and Total
Commander for the tasks a keyboard user reaches for every day: jumping to directories,
narrowing a listing, finding files, renaming many files, comparing two directories,
creating links, changing attributes, and copying sparse files and hard-link structures
faithfully. It must stay instant: every phase 2 feature has a performance target
(section 11), and the M1/M2 targets (M1 design section 13.1) keep holding.

The [M1/M2 design](implemented/2026-09-27-manycommander-design.md) stays normative for
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
| I-8 | **What you see is what you act on.** A verb acts on the marked entries that are visible (not hidden by the hidden-file toggle or the quick filter), or on the entry under the cursor when no visible entry is marked. The footer counts exactly those marks. |
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
pub struct Group { pub dir: PathBuf, pub names: Vec<OsString> }
```

`JobSpec::{Copy, Move, Trash, Delete}` take `Vec<Group>` instead of one `src_dir` and
`names`. A directory panel produces one group. Each group's directory is opened once at
job start, exactly like an M1 panel path (M1 4.3); names stay single path components, so
no job ever resolves a multi-component name relative to a directory fd (I-5). The plan
scans every group; the destination-inside-source check (M1 4.6) runs against the union
of all groups' source directories. One `Transfer` serves all groups, so standing answers
("Overwrite all", "Skip all") carry across groups. Two results with the same name copied
into one destination raise "file exists" for the second, as two sources would.

### 2.3 Threads and events

The UI thread still makes no filesystem syscalls (P-1). New producers:

| Producer | Sends |
|---|---|
| Find threads (section 5.3), up to 8 | `Find(Batch { search, entries, names })`, `Find(Done { search, stats })` |
| Compare thread (content) | `Compare(Differ { names })`, `Compare(Done)` |
| Directory-store thread | `DirsLoaded(DirStore)`, `ZoxideLoaded(Vec<(PathBuf, f64)>)` |
| Helper thread (file saves) | nothing on success; `Status(error)` on failure |

Find and compare are not jobs: they only read, so they run while a job runs. At most one
search and one content compare run at a time; starting another cancels the earlier one,
whose tab keeps its partial results and says so.

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
| Frecency | `$XDG_STATE_HOME/manycommander/dirs.tsv` (`# manycommander dirs v1`, then `rank<TAB>last_visit_epoch<TAB>path`; the path escapes `\`, bytes below 0x20, 0x7f and invalid UTF-8 as `\xNN`) | Atomically on exit, merged (section 3.3) |

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
- When the sum of ranks exceeds 10000, every rank is multiplied by 0.9 and entries below
  1 are dropped.
- Matching: the filter splits on whitespace into keywords, compared ASCII
  case-insensitively. Every keyword must occur in the path, in order; the last keyword must
  occur in the last path component. An empty filter matches everything.

**Merge on save.** A session records its visits as deltas. On exit it re-reads
`dirs.tsv`, applies its deltas (adds ranks, takes the later `last`), ages, and writes the
result atomically. Two concurrent instances therefore do not lose each other's visits;
the last writer can at most miss the other's aging step.

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
  spans a read-chunk boundary is found (chunks overlap by `needle.len() - 1` bytes).
- **Traversal.** Symlinks are never followed (I-5): a symlinked directory is a result
  candidate by name, never descended. With "Stay on this filesystem", a directory whose
  `mnt_id` differs from the root's is not descended (it can still match by name). Without
  "Hidden entries", names starting with `.` are neither matched nor descended. A directory
  that cannot be opened or read is counted in the search's error total, which the tab's
  footer shows; the search goes on.

### 5.3 The search engine

A search runs on a pool of `min(8, available_parallelism)` threads that share a work
queue of directories. Each queue item is `(parent fd, name, relative path)`:

1. `openat(parent, name, O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)`, then `statx` of the fd
   (identity and `mnt_id`). A directory that changed into a symlink fails with `ELOOP` and
   is counted as an error, not followed.
2. `getdents64` in large batches. For each entry: `d_type` decides directory versus other
   without a `statx`; `DT_UNKNOWN` costs one `statx`.
3. Name matches (and, with content, only regular files whose content matches) are
   `statx`ed for the columns and appended to the worker's batch.
4. Subdirectories go onto the queue with the parent's fd shared by `Arc`, so fds are
   bounded by the directories in flight, not the tree size.

Batches of up to 4096 results (the first batch at most 256, as in M1 3.1) go to the UI.
The results arrive in traversal order and the tab sorts them in (the M1 150 ms re-sort
spacing applies). `Esc` in the results tab while it searches sets the search's cancel
flag; the workers check it between entries and between content chunks, so a responsive
filesystem stops the search within 100 ms (A-FD-4). A worker blocked in the kernel is
abandoned as a listing thread is (M1 3.1). The search stops at 1,000,000 results and says
so.

Content search reads through a per-worker 256 KiB buffer and finds the needle with the
`memchr` crate's `memmem` finder (SIMD). A file is opened through the M1 4.3 sequence, so
a FIFO or device is never opened (I-10).

### 5.4 Results tab behaviour

| Key | In a results tab |
|---|---|
| `Enter` on a file | "Go to file": the tab navigates to the file's directory with the cursor on it; `Alt+Left` returns to the results |
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

A re-stat runs on a listing thread: for each result, `statx(open_root(root/parent), leaf,
AT_SYMLINK_NOFOLLOW)`, with the parent directories opened once each. Results that no
longer exist are dropped; the rest get fresh metadata. Marks survive by name, as in an M1
refresh.

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
is not, so invalid names survive byte-exactly. An unknown placeholder is a mask error.
Order of application: masks, then search and replace, then case.

### 6.3 Execution

The rename is a job (`JobSpec::Rename { groups, renames }`), so it follows M1 4.4 (one
job at a time, report, refresh). Per directory:

1. Open the directory once; `statx` each old name (`AT_SYMLINK_NOFOLLOW`) and record its
   identity. A missing entry fails with "disappeared".
2. Build the mapping; unchanged names drop out.
3. **Order.** An entry is ready when its new name is not the old name of another pending
   entry of the set. Ready entries rename first; each rename frees an old name, which can
   make another entry ready (chains resolve in order). What remains are cycles. For each
   cycle, one member is renamed to a temporary name `.mc-rename-<16 hex>` in the same
   directory, which frees its old name; the rest of the cycle resolves; the temporary
   finally renames to its target.
4. Every rename re-checks the old name's identity first (a different inode fails with
   "type changed") and uses `renameat2(..., RENAME_NOREPLACE)`. `EEXIST` means something
   outside the set holds the new name: the entry is skipped with "the destination exists"
   (I-3, I-9). Entries that wait for that name then find it taken and are skipped too.
   A case-only change on a case-insensitive filesystem uses the M1 4.8 intermediate-name
   path.
5. If the last step of a cycle fails, the temporary renames back to the original name; if
   that fails as well, the report names the temporary path (I-9).

Cancel is checked between renames. A rename does not fsync (as `mv`, NFR-DUR).

### 6.4 Preview errors

Computed in memory on every keystroke (P-14): an empty name, `.`, `..`, a name with `/` or
NUL, a name longer than 255 bytes, two entries of one directory with the same new name,
and a new name that equals a visible entry of the panel outside the set (the execution
re-checks this; the preview is advisory for entries it cannot see, such as hidden ones).

### 6.5 Undo

A completed rename job returns its performed renames with identities. `App` keeps the last
one. `Ctrl+Z` in the multi-rename dialog runs the reverse mapping as a new rename job, and
each entry is renamed back only if the entry under its new name still has the recorded
identity. The undo record is dropped after it runs or when the app exits.

## 7. Compare directories

`Shift+F2` opens a small form: `(•) by date and size  ( ) by content`, and
`[x] include directories`. It needs two directory panels.

**By date and size** runs in memory on the UI thread from the two listings (P-13); it
reads nothing. Only visible entries take part (I-8). For each name:

| Case | Marked |
|---|---|
| Only on one side | on that side (directories only with "include directories") |
| Both sides, both regular files, one strictly newer at the coarser mtime resolution of the two filesystems (M1 4.5) | the newer side |
| Same mtime at that resolution, different size | both sides |
| Same mtime and size, or both directories, or different types | neither |

The listing thread records each panel's filesystem type (`fstatfs` `f_type`) with the free
space it already reads, so the UI knows the resolution without a syscall. Existing marks
in both panels are cleared first. The status row summarises: `left: 3 newer, 5 only here;
right: 1 newer, 2 only here; 1 differ in size`.

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

- **Symbolic, absolute.** The target is the source's lexical absolute path (its panel
  path joined with its name, without resolving symlinks). **Symbolic, relative.** The
  target is the lexical relative path from the link's directory to the source, computed
  on both lexical absolute paths (`../a/b`).
- **Hard.** `linkat(srcdir, name, dstdir, newname, 0)`: it links the entry itself, never
  a symlink target (I-5). A directory is skipped with "directories cannot be hard-linked";
  `EXDEV` fails with "hard links cannot cross filesystems".
- A link never replaces anything: `symlinkat` and `linkat` fail atomically with `EEXIST`,
  which raises a "link exists" question with Skip, Skip all, Rename and Cancel (no
  Overwrite).

### 8.2 Change attributes (`Alt+A`)

A form:

| Field | Meaning |
|---|---|
| Mode | Octal (`0644`: sets exactly these bits) or symbolic (`u+x,g-w,o=r`, `a-x`; `=` clears the class's bits, then sets). Empty: unchanged. Pre-filled with the octal mode of the entry under the cursor when one entry is selected |
| Modification time | `YYYY-MM-DD HH:MM[:SS]` in the local time zone, or `now`. Empty: unchanged |
| `[ ]` Recursive | Apply to everything below selected directories |

The mode parses into a set mask and a clear mask: `new = (old & !clear) | set`. The form
shows the result for the first selected entry as `rw-r--r-- -> rwxr-xr-x`. It runs as
`JobSpec::Attr`:

- Each entry is opened `O_PATH | O_NOFOLLOW`, checked with `fstat`, and changed through
  `/proc/self/fd/<n>` (`chmod`, `utimensat` with `UTIME_OMIT` for the access time). The
  change reaches that inode, never a symlink target. Symlinks are skipped with "symbolic
  links have no mode of their own"; their targets are untouched (I-5).
- Recursion follows the M1 4.2 table: it descends into subvolumes and skips mount points,
  as permanent delete does.
- **Directory order.** A directory gets `old | set` before its children are visited and its
  final mode after them. Adding `r` or `x` therefore makes it traversable first, and
  removing them happens only once its children are done.
- `EPERM` (not the owner) raises the error question (M1 4.5).
- An entry whose mode and time would not change counts as unchanged, not as done.

## 9. Copy fidelity: sparse files and hard links

Both apply to F5 and to the copy step of a cross-filesystem move (M1 4.7, 4.8). The M1
guarantees are unchanged: I-2 (temporary file and atomic commit), I-3, and I-1 through
group commit.

### 9.1 Sparse files

A regular file is copied sparsely when its allocated size (`st_blocks x 512`) is smaller
than its size. The copy then walks the source's data segments with `lseek(SEEK_DATA)` and
`lseek(SEEK_HOLE)`, and copies each segment with `copy_file_range` at explicit offsets
(the `pread`/`pwrite` fallback likewise). Skipped ranges stay holes in the destination. A
final `ftruncate` to the source size creates a trailing hole. Progress counts hole bytes
as done when they are skipped, so the percentage stays true to the file size. A
filesystem without hole support in the destination (vfat) receives the zeros through
`ftruncate`, which is correct content. The change check before a move commits (M1 4.8
step 2) is unchanged. A non-sparse file keeps the M1 contiguous loop.

### 9.2 Hard links

A job keeps a map from source inode `(st_dev, st_ino)` to the first destination it
committed for that inode: `(destination directory fd, name, destination identity, source
size and mtime)`. Only files with `nlink > 1` enter the map. When the job meets another
link of a mapped inode:

1. `statx` the first destination by name in its directory. If its identity is not the
   recorded one (it was replaced), or the source's size or mtime differ from the recorded
   ones (the file changed in between), copy the data as usual.
2. Otherwise `linkat(first destination dir, first name, current destination dir,
   temporary name)`, then commit the temporary name with the M1 4.7 step 5 rules
   (`RENAME_NOREPLACE`, "file exists", Overwrite as an atomic replace). The link appears
   under its final name atomically (I-2).
3. `linkat` failing with `EXDEV`, `EMLINK`, `EPERM` or `EOPNOTSUPP` falls back to copying
   the data. Nothing is reported for a fallback; the content is the same.

Links whose other names lie outside the copied set are copied as independent files, as
`cp -a` does.

**Move.** A cross-filesystem move unlinks sources by batch (M1 4.8 step 5.3), and
unlinking one link of an inode changes that inode's `ctime` and `nlink`. The flush check
therefore compares, for an inode of which this job already unlinked `k` other links:
identity, size and mtime equal to `S0`, and `nlink == nlink0 - k`. Every other entry keeps
the full M1 check. The residual race is the M1 one, plus a write that restores the mtime
between two links' unlinks, which is documented.

## 10. Keymap

Phase 2 adds, with the M1 section 8 ownership rule applying to each:

| Key | Action | Protocol |
|---|---|---|
| `Ctrl+D` | Directories dialog (hotlist and frecency) | legacy works |
| `Ctrl+F` | Quick filter | legacy works |
| `Alt+F7` | Find files | xterm modifier encoding |
| `Ctrl+M` | Multi-rename | needs the kitty keyboard protocol: legacy `Ctrl+M` is `Enter` |
| `Shift+F2` | Compare directories | xterm modifier encoding |
| `Alt+L` | Create links | legacy works |
| `Alt+A` | Change attributes | legacy works |

None of these is bound by default in Ghostty, foot, Alacritty, Kitty or the Omarchy
Hyprland defaults; the plan's keymap audit re-checks that against each terminal's default
binding list. `Ctrl+Shift+F5` (Total Commander's link key) is not used because Kitty binds
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
| P-13 | Compare by date and size | Two 100k-entry listings <= 30 ms |
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
| NFR-RES | Find threads and fds are bounded (section 5.3). Results tabs hold no inotify watch. At most three results places per panel history. |
| NFR-REL | Find, compare and store threads run under `catch_unwind`; a panic ends that search or compare with an error message, and the app stays up. |

## 12. Acceptance checks

*auto*, *bench* and *manual* as in M1 11. All M1 checks (A-FS, A-TR, A-DEL, A-UI, A-TH,
A-LN, A-PUB, A-P) must still pass.

### 12.1 Directories and filter

| ID | Check | Type |
|---|---|---|
| A-DJ-1 | Bookmarks: add, remove, persist and reload, including a non-UTF-8 path; the file is replaced atomically; a `hotlist.toml` that does not parse is never overwritten | auto |
| A-DJ-2 | Frecency: ranking with the four time weights, aging past 10000, keyword order, last keyword in the last component, the active directory excluded; `z` loads the best match and reports "no match" | auto |
| A-DJ-3 | Two sessions' visits merge on save; unparsable lines are skipped | auto |
| A-DJ-4 | A frecency entry whose directory is gone fails to load, the panel stays, and the entry is dropped | auto |
| A-DJ-5 | P-15, including P-2 with a 5000-entry store | bench |
| A-QF-1 | Substring and glob filtering, case folding, `Esc` clears, directory change clears, refresh keeps, cursor rules | auto |
| A-QF-2 | I-8: with a filter, F5 and F8 act only on visible marked entries; the footer counts them; marks return when the filter is cleared; the same with the hidden toggle | auto |
| A-QF-3 | P-12 | bench |

### 12.2 Find

| ID | Check | Type |
|---|---|---|
| A-FD-1 | Name search: substring and glob, case rules, hidden toggle; files, directories, symlinks and special files match by name; a symlinked directory and a symlink loop are not followed; a bind mount inside the tree (`unshare -rm`) is not descended with "stay on this filesystem" and is descended without it | auto |
| A-FD-2 | Content search: literal and case-folded matches, a match across a 256 KiB chunk boundary, symlinks to files not read, a FIFO in the tree never opened (the search completes), an unreadable file counted as an error | auto |
| A-FD-3 | Results tab: names with a newline and invalid UTF-8 display escaped; F5 of results from three directories into the other panel copies all, and two same-named results raise "file exists" for the second; F8 trashes results from two directories; Enter goes to the file and `Alt+Left` returns; `Ctrl+R` drops vanished results | auto |
| A-FD-4 | `Esc` stops a search of a large tree within 100 ms; the tab says "cancelled" and keeps its results | auto |
| A-FD-5 | P-10 | bench |
| A-FD-6 | P-11 | bench |
| A-FD-7 | A search whose tree contains a stopped FUSE mount (`scripts/fixtures/stall-fuse.sh`) keeps the UI responsive; `Esc` cancels it | manual |

### 12.3 Multi-rename

| ID | Check | Type |
|---|---|---|
| A-MR-1 | Mask engine: every placeholder of 6.2, search and replace (literal, regex with groups, case folding), the case modes, byte-exact handling of invalid UTF-8 | auto |
| A-MR-2 | A swap `a <-> b`, a 3-cycle, a chain and a mix end with the intended names; no `.mc-rename-` name remains | auto |
| A-MR-3 | A new name held by an entry outside the set: that entry is skipped and the other file is untouched; duplicate new names and invalid names are refused before any write | auto |
| A-MR-4 | Failpoints at each rename of a cycle (error and cancel): every inode is afterwards reachable under its original name, its new name, or a temporary name the report states | auto (failpoint) |
| A-MR-5 | Undo restores the original names; an entry replaced since (another inode under the new name) is left alone and reported | auto |
| A-MR-6 | Case-only multi-rename on a case-insensitive filesystem (tmpfs `casefold` under `unshare -rm`) | auto |
| A-MR-7 | P-14 | bench |

### 12.4 Compare, links, attributes

| ID | Check | Type |
|---|---|---|
| A-CD-1 | Date-and-size rules of section 7 on both sides, including the 2 s vfat resolution (a panel whose recorded `f_type` is vfat), directories with and without the option, hidden and filtered entries excluded | auto |
| A-CD-2 | Content compare marks same-size differing pairs, leaves identical ones, never opens a FIFO, and cancels | auto |
| A-CD-3 | P-13 | bench |
| A-LK-1 | Symbolic links, absolute and relative (the relative one resolves to the source from its directory), for one and several entries; an existing name is never replaced | auto |
| A-LK-2 | Hard links share the inode; a symlink is hard-linked as the link itself; a directory is refused; `EXDEV` is reported | auto |
| A-AT-1 | Octal and symbolic modes on files and directories, recursive; a symlink in the tree is skipped and its target's mode is unchanged; a mount point inside the tree (`unshare -rm`) is skipped; mtime set exactly on files and directories | auto |
| A-AT-2 | Recursive `a-rx` and `u+rx` on a tree: removing does not prevent the traversal of the directory's children, adding makes an untraversable directory traversable before its children | auto |

### 12.5 Copy fidelity

| ID | Check | Type |
|---|---|---|
| A-SP-1 | A file with holes at the start, in the middle and at the end, copied and moved btrfs -> tmpfs, tmpfs -> btrfs and within btrfs: identical hash and size; destination allocation <= source allocation + 64 KiB | auto |
| A-SP-2 | P-16 | bench |
| A-HL-1 | A tree with link pairs, a triple and a file linked from outside the selection: copy and cross-filesystem move keep pairs and the triple as links (`nlink` and shared inodes), copy the outside-linked file independently, and a move reports no "source changed" | auto |
| A-HL-2 | The first destination of an inode replaced before its second link is reached: the second is copied, not linked to the stranger | auto (failpoint) |
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
