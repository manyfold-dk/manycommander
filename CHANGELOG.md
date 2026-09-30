# Changelog

All notable changes to manycommander. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/). The full documentation is at
[manycommander.app](https://manycommander.app).

## [Unreleased]

### Added

- Install with mise, which Omarchy ships: `mise use -g github:manyfold-dk/manycommander`.
  `omarchy update` keeps it current. The install page and the start page lead with it.
- Release tarballs carry a GitHub build provenance attestation. mise verifies it on install;
  `gh attestation verify <tarball> --repo manyfold-dk/manycommander` verifies it by hand.

## [0.3.1] - 2026-09-29

### Changed

- SFTP transfers of many small files are about twice as fast: a file now takes three round
  trips to the server in each direction instead of six. Large files are unchanged.

### Fixed

- Inside tmux, the quick view no longer picks sixel because tmux itself supports it; it uses
  kitty graphics through tmux or half blocks. `preview.protocol = "sixel"` still selects it.
- The keys, install and archives pages: key alternatives inside Omarchy's tmux, installing a
  release instead of `main`, and where a failed archive check is reported.

## [0.3.0] - 2026-09-29

Archives, an image quick view and SFTP, built on a narrow source seam that leaves the local
file-operation engine and its guarantees unchanged.

### Added

- **Archives**: `Enter` (or `Alt+O`) opens zip, tar, `.tar.gz`, `.tar.zst` (including Arch
  `.pkg.tar.zst` packages), `.tar.xz`, `.tar.bz2` and 7z archives as read-only directories.
  Rows stream in while a compressed archive is scanned. `F3` and `F4` view a member through
  a private copy; `F5` extracts through the same engine as a copy, so an extracted file is
  never partial and never replaces anything without an answer. Hostile archives are safe:
  `..` and absolute names, symbolic links planted to redirect later members, special files,
  setuid bits, decompression bombs and oversized headers are refused or bounded.
- **Quick view** (`Ctrl+Q`): the other panel previews the entry under the cursor. Images use
  kitty graphics (Ghostty, Kitty), sixel (foot) or half blocks, decoded off the UI thread;
  other files get an information card with a text head. `Alt+Q` previews archive members and
  remote files on request. manycommander never changes the terminal's or tmux's settings.
- **SFTP**: `cd sftp://[user@]host[:port]/path` browses a server through the system `ssh`, so
  your `ssh_config`, agent, known hosts and jump hosts apply unchanged and host keys are
  never trusted silently. `F5` downloads and uploads; `F6`, `Shift+F6`, `F7` and `Shift+F8`
  work on the server. On servers with OpenSSH's hard-link extension an upload is committed
  with a hard link, so a file on the server is never partial; an overwrite is atomic or
  refused. Moves between hosts are best-effort and
  say so before they start. Bookmarks can hold server addresses. Up to four connections stay
  open.

### Fixed

- A search whose tree held a stalled network or FUSE mount no longer waits on it, and
  results found before a worker blocks are shown at once.
- A panic while searching or reading an archive member now ends that search or read instead
  of the program.

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
