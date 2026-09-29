# Changelog

All notable changes to manycommander. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/). The full documentation is at
[manycommander.app](https://manycommander.app).

## [0.2.0] - 2026-09-29

The first published release. It contains the dual-pane file manager with its file-operation
guarantees, tabs and session restore, and the phase 2 tools.

### Added

- **Go to directory** (`Ctrl+D`): bookmarks and frequently used directories ranked by
  frecency, filtered as you type; `Insert` bookmarks the current directory. Reads zoxide's
  ranking when zoxide is installed (`[jump] zoxide`), never writes it. `z <keywords>` on the
  command line jumps to the best match.
- **Quick filter** (`Ctrl+F`): narrows a panel as you type, by substring or glob. Verbs act
  only on what the panel shows.
- **Find files** (`Alt+F7`): name and content search on a parallel, symlink-safe walk that
  never follows symlinks, never opens special files and stays on one filesystem by default.
  Results open in a tab that works like a panel: mark, sort, filter, view, edit, copy, move,
  trash, delete, rename, link and change attributes; `Enter` goes to the file.
- **Multi-rename** (`Ctrl+M`): name and extension masks (`[N]`, `[E]`, ranges, counter,
  parent name, date and time), search and replace with regular expressions, case modes and
  a live preview that blocks conflicts. Renames never overwrite, resolve swaps and cycles,
  and can be undone with `Ctrl+Z`.
- **Compare directories** (`Shift+F2`): by date and size, or by content, marks what differs
  in both panels.
- **Links** (`Alt+L`): relative or absolute symbolic links and hard links; an existing name
  is never replaced.
- **Attributes** (`Alt+A`): mode in octal or chmod syntax and the modification time,
  optionally recursive; symbolic links keep their targets untouched.
- **Copy fidelity**: copy and cross-filesystem move keep sparse files sparse and hard links
  within the copied set linked.
- Two panels with tabs and session restore, the F-key verbs (view, edit, copy, move,
  rename, mkdir, trash, delete), a command line, live Omarchy theming, and the file-operation
  guarantees described on the site: no partial files, no overwrite without an answer, moves
  that never lose data.

### Fixed

- Dialogs no longer panic in a terminal narrower than two columns.
