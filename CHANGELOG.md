# Changelog

All notable changes to manycommander. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/). The full documentation is at
[manycommander.app](https://manycommander.app).

## [0.6.1] - 2026-10-10

`sftp://host` opens the login directory on servers with OpenSSH before 9.9, and the binary is
15 percent smaller.

### Changed

- The quick view scales pictures to the pane with its own box filter instead of the
  `fast_image_resize` crate. The binary is 1.3 MiB (15 percent) smaller, and a scaled
  picture is the exact average of the pixels it covers, where the crate was off by one in
  about a fifth of the bytes. A 12-megapixel photo takes about 1.5 ms longer to show.

### Fixed

- `sftp://host` and `/~/...` open the login directory on a server with OpenSSH before 9.9,
  such as Ubuntu 24.04's or Debian 12's. Such a server refuses the `home-directory` request
  for the login user, and manycommander showed "Failure" instead of asking for
  `REALPATH(".")`.

## [0.6.0] - 2026-10-05

`Enter` and `F3` open files through `gio open`, so a terminal program opens in a terminal
window, and a directory on the command line starts on the left panel.

### Changed

- A directory on the command line makes the left panel active. The last session's active
  panel returns only when the command line names no directory.

### Fixed

- `Enter` and `F3` open a file through `gio open`, and through `xdg-open` only where `gio` is
  missing. On Hyprland, `xdg-open` started a terminal program, such as the terminal editor
  that handles text files, without a terminal: nothing showed, and the program never ended.
  Such a program now opens in a terminal window.

## [0.5.0] - 2026-10-05

`F3` opens pictures, documents, media and web pages in their application, the quick view
renders Markdown, and copies and moves of many small files are faster.

### Added

- `F3` on a picture, a document, audio, video or a web page opens it in its application, as
  `Enter` does: the desktop's default application through `xdg-open`, a web page in the
  default browser. The extension decides, for a regular file or a symlink to one. Every other
  file still goes to the pager. Inside an archive or on a server, the copy opens the same way
  and stays until manycommander exits; a copy the application changed is kept, and
  manycommander says where when it exits.
- The quick view renders Markdown files: headings, emphasis, code, lists, quotes, rules,
  links, pictures and tables, wrapped at the pane's width.

### Changed

- Copying many small files is faster: a copied file is written to an unnamed temporary file
  and gets its name when complete, so a crash leaves no `.mc-partial-*` file behind on ext4,
  btrfs, xfs or tmpfs. 50k files of 4 KiB copy at 1.3x `cp -r` instead of 1.7x.
- A move to another filesystem flushes every 1024 files instead of every 256: 50k small files
  move at 1.1x `mv` instead of 2.4x. After a crash, up to 1024 files can be in both places,
  never in neither.

### Fixed

- A directory that holds a stalled network or FUSE mount lists again; it stayed "(loading)".
  Listing a directory no longer triggers the automounts in it.
- The delete prompt names the unit of a small size ("2 files, 1 directory, 2 bytes"), and a
  move out of a server names the entry, not its server path without the leading `/`.

## [0.4.1] - 2026-10-04

Two fixes found by the hands-on verification of 0.4.0.

### Fixed

- The quick view no longer crashes Ghostty. Ghostty releases built with Zig 0.15 crash, with
  all their windows and tabs, when they unpack a kitty image compressed the way
  manycommander sent it. A picture that compresses now goes as a PNG; a photo that does not
  compress goes uncompressed, as before.
- `Alt+*`, `Alt+=` and the other `Alt` chords on a symbol typed with `Shift` work on every
  keyboard layout in a terminal with the kitty keyboard protocol (Ghostty, foot, Kitty). On a
  Spanish layout `Alt+*` did nothing.

## [0.4.0] - 2026-09-30

Type to filter: the keyboard goes to the panel first.

### Added

- Typing a letter filters the active panel. The quick filter opens with it, ignores case
  (also beyond ASCII: `æble` finds `Æbler.txt`) and forgives a typo: when no name contains
  the text, the names closest to it show instead, marked `fuzzy`. From four letters one
  wrong, missing or swapped letter counts (`reamde` finds `README.md`), from six also an
  extra letter, from nine two typos. The cursor goes to a name that starts with the text.
- `Ctrl+E` moves to the command line. The terminal cursor shows there only while it has
  the focus.
- The help overlay (`F1`) names the version in its top border.

### Changed

- Typing no longer goes to the command line: press `Ctrl+E` first. `Alt+Enter`, `Alt+P`,
  `Ctrl+P` and a paste still put text on the line and move there. `Enter` and `Esc` on the
  line go back to the panel.
- `Enter` on the filter line opens the entry under the cursor and keeps the filter;
  `Ctrl+F` closes the line without acting. `Backspace` on an empty filter line closes it,
  and on an empty command line goes back to the panel, instead of going to the parent
  directory.
- The function-key bar names each key with its `F`: `F1Help`, `F3View`, `F10Quit`.
- A `[` in the filter is a glob only when a later `]` closes it.

## [0.3.2] - 2026-09-30

The program is unchanged; this release is about installing it.

### Added

- Install with mise, which Omarchy ships: `mise use -g github:manyfold-dk/manycommander`.
  `omarchy update` keeps it current. The install page and the start page lead with it.
- Release tarballs carry a GitHub build provenance attestation. mise verifies it on install;
  `gh attestation verify <tarball> --repo manyfold-dk/manycommander` verifies it by hand.

### Changed

- manycommander.app: the docs navigation is grouped, and each page lists its sections; accent
  text reaches 4.5:1 contrast in every theme; the copy guarantee names the filesystems that
  keep it (ext4, btrfs, xfs).

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
