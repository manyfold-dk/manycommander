---
title: manycommander phase 3 design
type: spec
status: draft
owner: manycommander
source: 2026-09-28-manycommander-phase2-design.md
created: 2026-09-28
updated: 2026-09-28
---
# manycommander phase 3 design

Contents: [1 Outcome](#1-outcome) · [2 Architecture](#2-architecture) ·
[3 Archives](#3-archives) · [4 Image quick view](#4-image-quick-view) · [5 SFTP](#5-sftp) ·
[6 Keymap](#6-keymap) · [7 NFRs](#7-non-functional-requirements) ·
[8 Acceptance](#8-acceptance-checks) · [9 Alternatives](#9-alternatives-considered) ·
[10 Risks](#10-risks) · [11 Release](#11-release) ·
[A Review resolution](#appendix-a-review-resolution)

## 1. Outcome

Phase 3 takes manycommander beyond the local filesystem without weakening it. The user
browses archives as read-only directories and extracts from them, looks at images in a
quick view next to the file list, and browses and transfers files on SSH servers over
SFTP. The local file-operation engine and its invariants I-1 to I-7 stay as they are:
archives and servers attach through a narrow seam on the source side of the copy engine,
and every byte written to a local disk still goes through the M1 machinery. Every new
feature has a performance target (section 7).

The [M1/M2 design](implemented/2026-09-27-manycommander-design.md) and the
[phase 2 design](2026-09-28-manycommander-phase2-design.md) stay normative for everything
this document does not change. They are cited as "M1 4.7" and "P2 2.2". Phase 3 starts
after the phase 2 release.

### 1.1 Scope

| # | Feature | Section |
|---|---|---|
| 1 | The non-local source seam: a narrow provider for archives and SFTP, and the copy engine's source-side split | 2 |
| 2 | Archive browsing: read-only zip, tar, tar.gz, tar.zst (including `.pkg.tar.zst`), tar.xz and tar.bz2; F3/F4 on members; F5 extract | 3 |
| 3 | Image quick view (`Ctrl+Q`): kitty graphics, sixel, halfblocks, and an info card for everything else | 4 |
| 4 | SFTP 3a: connect, browse, F3, download (F5 remote to local) | 5.1-5.5, 5.7 |
| 5 | SFTP 3b: upload, mkdir, rename, delete, moves across hosts, F4 write-back | 5.6 |
| 6 | Stretch: 7z browsing and extraction | 3 |

Out of scope: writing or packing archives, nested archives, find inside archives or on
servers, extraction of encrypted zip entries (they are listed), multi-volume archives, a
bsdtar subprocess decoder for rar, iso and cpio, copies between two servers, server-side
copy, a remote trash, an in-TUI password prompt, previews of video, PDF, AVIF or HEIC, and
FTP, SMB or cloud storage.

### 1.2 Decisions

The owner delegated seven decisions for this phase while away. They are recorded here with
their reasons.

| # | Decision | Reason |
|---|---|---|
| D-1 | zstd through the C `zstd` crate | The main use case is pacman's `.pkg.tar.zst`. Listing the largest cached package took 510 ms with libzstd and 1500 ms with the pure-Rust `ruzstd` (3x). libzstd is heavily fuzzed upstream. `cc` compiles it from the crate's bundled sources, and nothing links a system library. The in-process C risk is accepted for this one narrow decoder; it is not accepted for libarchive's many parsers (section 9). |
| D-2 | `.tar.bz2` is supported, and `deny.toml` allows the SPDX `bzip2` licence (the identifier `bzip2-` followed by the upstream release number) | The licence is permissive and BSD-style. The `bzip2` crate's default backend is pure Rust, and the C backend carries the same licence, so the only alternative was to drop the format. |
| D-3 | SFTP is an own synchronous SFTP version 3 client over the system `ssh -s <host> sftp` | No async runtime and no crypto crates: zero new crates. OpenSSH keeps `ssh_config` (including `Include` and `Match`), the agent, FIDO keys, certificates, `known_hosts` with hashed, wildcard, `@revoked` and `@cert-authority` entries, `ProxyJump` and the host-key policy. The pure-Rust alternative would re-implement host-key policy on a partial config parser and lose RSA keys (section 9). |
| D-4 | SFTP write operations (3b) are in phase 3, after 3a is solid | 3b reuses 3a's session, codec and scan. The plan orders T7 after T6. |
| D-5 | Images use `ratatui-image` with `image` (pure-Rust decoders, default features off), wrapped in a `preview` module so they can be swapped, but only if manycommander can use it without changing the user's tmux options | Every `ratatui-image` `Picker` constructor runs `tmux set -p allow-passthrough on` inside tmux, and manycommander never changes the user's tmux or terminal configuration (V-3). The first image task verifies a path without that side effect. If the side effect cannot be avoided without `unsafe` or a change to the process environment, manycommander implements its own kitty graphics and halfblocks layer, with `icy_sixel` for sixel (section 4.7). |
| D-6 | 7z (`sevenz-rust2`) is a stretch goal at the end of phase 3; a bsdtar subprocess decoder (rar, iso, cpio) is out of phase 3 | 7z is pure Rust and fits the index model. The subprocess decoder needs its own process and format-conversion design. |
| D-7 | Connect-time SSH prompts use the existing terminal hand-off (as F3 and F4); an in-TUI askpass comes later | The hand-off exists and is tested (M1 A-UI-3). ssh prompts on `/dev/tty` itself, so manycommander never sees a password. |

### 1.3 New invariants

Invariant IDs have one number (A-1); acceptance checks carry a group code (A-AR-1). I-1 to
I-10 hold for every new verb that writes to a local disk.

**Archives**

| ID | Invariant |
|---|---|
| A-1 | **Member paths are data, never paths.** The index splits a member name on `/`, drops empty and `.` components, and drops a leading `/` (the footer counts such members). A member with a `..` component, a NUL, or a component that fails `valid_component`, and a member whose parent path names a non-directory member, is skipped with the reason "unsafe path" and is never extracted (I-7). Nothing joins a member name onto a filesystem path; that join is the cause of RUSTSEC-2025-0168, RUSTSEC-2026-0067 and RUSTSEC-2026-0245. manycommander never calls a crate's `extract` or `unpack` function. |
| A-2 | **Extraction writes only through the local engine.** Every write uses directory fds, a per-component `O_DIRECTORY` + `O_NOFOLLOW` walk from the destination root, and the temporary-file commit of M1 4.7 (I-2, I-3). A symlink member becomes a symlink (`symlinkat`) and is never traversed (I-5). No verb writes into an archive, and an archive panel is never a destination. |
| A-3 | **No special files and no privilege bits.** Device, FIFO and socket members are skipped as "special file" (M1 4.6). Modes are masked to `0o777` (no setuid, setgid or sticky bit), and ownership is not restored. A hard-link member becomes a `linkat` to a member that the same job extracted earlier, checked by identity (P2 9.2); otherwise it is skipped with "hard link to a member not extracted". |
| A-4 | **Bounded amplification.** The plan compares the declared uncompressed total with the destination's `statvfs` free space and asks before an overrun. A member that produces more or fewer bytes than its header declares fails with "size mismatch". A listing stops at 1,000,000 entries or at the index memory cap (section 2.6). A decompression bomb costs CPU time only while its scan is visible and cancellable. |
| A-5 | **Parser consistency.** The `tar` crate is at least the release that fixes RUSTSEC-2026-0067 and RUSTSEC-2026-0068, so the PAX size is honoured. Before it writes a member, extraction compares the member's header (name, kind, size) with the index node; a difference fails the member with "archive changed". A truncated stream, a CRC error or a decoder error fails the member with "archive damaged" and never commits a partial file (I-2). |

**Remote (SFTP)**

| ID | Invariant |
|---|---|
| R-1 (I-2) | **No partial remote file.** An upload writes `.<name>.mc-partial-<random>` in the destination directory, opened `CREAT` + `EXCL` + `WRITE`, sets its mode and times, and closes it. It commits with `hardlink@openssh.com(temporary, final)`, which fails when the final name exists, then removes the temporary name. `SSH_FXP_RENAME` never commits an upload, because protocol version 3 leaves open whether it replaces. A server without hard links (the extension missing, or refused as unsupported) gets the M1 direct-write mode: the final name is created `CREAT` + `EXCL` and written directly, is visible while it is written, and is removed on failure or cancel (the M1 I-2 exception; the report says so). On error or cancel the temporary name is removed; after a lost session, the report names it. |
| R-2 (I-3) | **Never replace on the server without a decision.** A commit that fails raises the M1 "file exists" question when an `LSTAT` finds the name; version 3 reports "exists" and other failures alike as `SSH_FX_FAILURE`. Overwrite uses `posix-rename@openssh.com`, an atomic `rename(2)`. A server without it refuses the overwrite with "the server cannot replace a file atomically". A rename or mkdir on the server `LSTAT`s the new name first and raises the question when it is taken. |
| R-3 (I-5) | **Symlinks stay objects as far as the protocol allows.** manycommander never opens a symlink for data: it `LSTAT`s the entry and requires a regular file first. Symlinks are copied as symlinks. A recursive walk (scan, delete) `LSTAT`s every directory before `OPENDIR` and never descends a symlink. SFTP is path-based and has no `O_NOFOLLOW` and no inode numbers, so the server resolves intermediate components, and a swap between the `LSTAT` and the request goes undetected. The F1 help states that this is weaker than I-5 on local disks. |
| R-4 (I-1) | **A move across hosts deletes its source only after the destination is committed and durable.** Download: the local group commit (M1 4.8) syncs the batch; then each remote source is removed only if its `LSTAT` still shows the planned size and mtime. SFTP has no inode or ctime, so this check is weaker than M1's. Upload: the temporary file is synced with `fsync@openssh.com` before its commit, and the local source is then unlinked with the M1 4.8 check. A server without `fsync@openssh.com` makes the move best-effort, as vfat is in M1: the confirm dialog says so before the job starts, and the report says "not synced on the server". |
| R-5 (I-6) | **No remote trash.** F8 on a remote panel is refused with "no trash on the server; Shift+F8 deletes permanently". Shift+F8 keeps the typed `delete` confirmation. |
| R-6 | **The transport is the user's ssh, unmodified.** It spawns by argv and never through a shell. The host and user are validated (section 5.1) and follow `--`. manycommander passes only options that switch off what an SFTP transport does not need (forwarding, local and remote commands, a tty, the escape character). It never passes an option that relaxes host-key checking or authentication, such as `StrictHostKeyChecking=no` or `UserKnownHostsFile=/dev/null`, and it never answers an ssh prompt itself. |

**Preview**

| ID | Invariant |
|---|---|
| V-1 | **The UI thread only draws.** Reading, decoding, resizing and encoding a preview run on the preview thread. The UI thread draws a prepared image or card, so P-1 holds while previews load. |
| V-2 | **A preview opens only what it may.** A local preview opens a regular file through the M1 4.3 `O_PATH` sequence (I-10): never a symlink target, a FIFO or a device. It reads at most 64 MB, and the decoder runs with limits of 16384 x 16384 pixels and 256 MB of allocation. Anything else gets the info card, never a panic. |
| V-3 | **manycommander never changes the user's terminal or tmux configuration.** It runs no `tmux` command and writes no sequence that changes a persistent terminal setting. It only places, and later deletes, its own images. |
| V-4 | **No stale pixels.** An image shows only while the quick view is visible and no modal overlaps it (a dialog, form, question, progress dialog, the help overlay, the output view). manycommander deletes its images when it replaces them, before a hand-off and before it exits. |
| V-5 | **Browsing never pulls data.** Cursor movement previews only local files and members of random-access archives (zip, 7z, plain tar). A remote file or a member of a compressed tar is previewed only on an explicit key (`Alt+Q`). |
| V-6 | **The probe cannot eat keys or stall the start.** The terminal query runs once at startup, before the input thread exists, and ends at the terminal's DA1 reply or after 100 ms. A terminal that answers nothing gets halfblocks. |

### 1.4 Amendments to earlier designs

| Where | Amendment |
|---|---|
| M1 NFR-SEC | "No network access" becomes "no network access except the SFTP connections the user opens" (section 7.2). |
| M1 6, `Enter` on a file | `Enter` on a file with a recognised archive name browses the archive (section 3.1) instead of running `xdg-open`. `xdg-open` stays reachable through the command line. |
| M1 6, `cd` | `cd sftp://...` connects (section 5.1). A relative `cd` in an archive or remote panel navigates inside it. |
| M1 4.5, "Overwrite all older" | The coarser-resolution rule covers archive and remote sides: 1 s for tar and SFTP, 2 s for zip members without an extended timestamp. |
| P2 2.2 | `Group.root` becomes `Root` (section 2.2). |
| P2 2.4 | `Source` and `Place` gain archive and remote variants (section 2.2). |
| P2 3 | Bookmarks may hold `sftp://` addresses. Frecency records local directories only. |
| `deny.toml` | The SPDX `bzip2` licence is allowed (D-2). |

## 2. Architecture

### 2.1 The seam: a provider, not a VFS

The local engine keeps its directory fds, `O_PATH` opens, `renameat2` and `statx`
identities. Abstracting local I/O behind a trait would weaken I-1 to I-7 and slow P-3 and
P-7. Only non-local places go through a narrow provider in `src/provider.rs`:

```rust
/// A non-local place: an archive index or an SFTP session.
pub trait Provider: Send + Sync {
    /// What the place allows: read-only for archives; per server extension for SFTP.
    fn caps(&self) -> Caps;
    /// Lists one directory on a listing thread, sending `ListingMsg` batches.
    fn list(&self, dir: &VPath, out: &mut dyn FnMut(ListingMsg), cancel: &AtomicBool)
        -> Result<(), PlaceError>;
    fn lstat(&self, path: &VPath) -> Result<Meta, PlaceError>;
    /// A regular file's content, for F3, F4 and the quick view; never a symlink.
    fn open_read(&self, path: &VPath, cancel: &AtomicBool)
        -> Result<Box<dyn Read + Send>, PlaceError>;
}
```

`VPath` is a list of byte components, and each passes `valid_component` (M1 3.2: bytes end
to end, no `String` round trip). `Meta` is the existing `fsops::sys::Meta` with a synthetic
identity that never equals a local one. The same-file and destination-inside-source checks
(M1 4.6) apply only to local sources. Rows reuse `Entry` and the compact name arena, so
sorting, marks, the quick filter and rendering work unchanged.

### 2.2 Panel sources, places and groups

`Source` (P2 2.4) gains two variants:

```rust
pub enum Source {
    Dir,
    Results(Arc<Search>),
    Archive(ArchiveView), // { index: Arc<ArchiveIndex>, archive: PathBuf, inner: VPath }
    Remote(RemoteView),   // { session: Arc<Session>, dir: VPath }
}
```

`Panel.dir` stays a local directory: the directory that holds the archive, or the local
directory the tab showed before it connected. It is the command line's working directory.
`..` at the archive root or at the remote root `/` returns the panel to it; leaving an
archive puts the cursor on the archive. The panel title shows `<archive path>:/<inner>` or
`sftp://[user@]host[:port]/<dir>`.

History places name what to reopen, not what is open, so the history never pins an index or
a connection:

| Place | Going back |
|---|---|
| `Place::Archive { archive: PathBuf, key: StatKey, inner: VPath }` | Reopens through the index cache (a hit is instant); a changed `key` rescans |
| `Place::Remote { target: Target, dir: VPath }` | Reuses the open session for `target`, else reconnects (section 5.7) |

Jobs take groups (P2 2.2) whose root is now:

```rust
pub enum Root { Local(PathBuf), Archive(Arc<ArchiveIndex>), Remote(Arc<Session>) }
```

For an archive, `sub` is the inner directory below the archive root. For a remote group, it
is the absolute directory on the server. `sub` and `names` stay single byte components.
`JobSpec::{Copy, Move}` take a destination `Dest::{Local(PathBuf), Remote { session, dir }}`;
`Mkdir` and `Delete` accept a remote place (3b).

A hidden tab (M2) keeps its place and releases its index or session reference, as it
releases its listing. On exit, an archive tab is saved as the directory that holds the
archive, and a remote tab as its local directory. manycommander never connects at startup
(NFR-SEC).

### 2.3 The copy engine's source side

`fsops::copy::Transfer` opens each source through `open_for_read` on a directory fd today.
Phase 3 moves the source side behind `fsops::source::SourceTree`. The destination side stays
as it is: temporary file, commit, questions, progress, cancel and group commit.

```rust
pub(crate) trait SourceTree {
    /// Plans one group into the existing `Plan` of `Node`s.
    fn scan(&self, group: &Group, rep: &mut Reporter) -> Result<Plan, Refusal>;
    /// Tree order (local, zip, 7z, SFTP) or one pass in stream order (tar).
    fn order(&self) -> Order;
    fn open(&self, node: &Node, cancel: &AtomicBool) -> Result<SourceFile, EntryError>;
    fn read_link(&self, node: &Node) -> Result<OsString, EntryError>;
    /// A move's source removal, after the destination batch is durable (R-4).
    fn remove(&self, node: &Node, planned: &Meta) -> Result<Removed, EntryError>;
    fn is_local(&self) -> bool;
}
pub(crate) enum SourceFile {
    Local { fd: OwnedFd, meta: Meta },
    Stream { reader: Box<dyn Read + Send>, declared: u64 },
}
```

| Implementation | Source |
|---|---|
| `LocalTree` | Today's code behind the trait: the group open (P2 2.2), the `O_PATH` read open (M1 4.3), `copy_file_range`, sparse files and the hard-link map (P2 9) |
| `ArchiveTree` | The index: zip and 7z members by locator, tar in one pass (section 3.5) |
| `RemoteTree` | A session: pipelined reads (section 5.5) |

A `Stream` file is read into the job buffer and written to the temporary file. It has no
`copy_file_range`, no hole detection and no hard-link map, and its byte count must equal
`declared` (A-4). Extraction and download are therefore "copy with a non-local source".
They inherit I-2, I-3 and I-5 on the write side, and the questions, progress and cancel; the
M1 and P2 test suites guard the move of the local code behind the trait. Upload is the only
new destination engine (`remote::put`, section 5.6). Its source is a `LocalTree`, so the
read side keeps I-5.

### 2.4 Verbs by place

| From \ to | Local directory | Archive | Remote, same session | Remote, other session |
|---|---|---|---|---|
| Local | M1 | refused: "archives are read-only" | F5 upload; F6 upload, then delete (3b) | same |
| Archive | F5 extract; F6 refused | refused | refused | refused |
| Remote | F5 download (3a); F6 download, then delete (3b, R-4) | refused | F6 renames on the server (3b); F5 refused: "no copy on the server; copy through a local directory" | refused: "copy through a local directory" |
| Results tab (P2 5) | P2 | refused | as Local (3b) | as Local (3b) |

| Verb in the panel | Archive | Remote |
|---|---|---|
| `Enter` on a directory, `Backspace`, `Alt+Up` | Navigate; at the archive root, back to `Panel.dir` | Navigate; at `/`, back to `Panel.dir` |
| `Enter` on a file | F3 | F3 |
| F3 | Member copy in the runtime directory, then `$PAGER` (3.4) | Download to the runtime directory, then `$PAGER` (5.5) |
| F4 | As F3 with `$EDITOR`; edits are not written back (3.4) | 3a: as in an archive; 3b: asks to upload changes (5.6) |
| F7, Shift+F6, Shift+F8 | refused | 3b |
| F8 | refused | refused (R-5) |
| Shift+F4, `Alt+L`, `Alt+A`, `Ctrl+M`, `Alt+F7` | refused | refused |
| Shift+F2 (compare) | By date and size only; by content is refused | same |
| `Space` on a directory | Size from the index, at once | Size by a cancellable walk on the server |
| `Ctrl+R` | Rescans when the archive's stat key changed | Re-lists; reconnects a lost session |
| `Ctrl+Q` quick view | Zip, 7z and plain tar members on cursor rest; compressed-tar members on `Alt+Q` | `Alt+Q` only (V-5) |

A refusal is a status-line message before any work, as P2's "not in search results".

### 2.5 Threads and events

The M1 3.1 model stays: plain threads, one `mpsc` channel into the UI thread, and no
filesystem or network syscalls on the UI thread (P-1).

| Producer | Sends |
|---|---|
| Archive scan (a listing thread) | `Listing(Batch)` for the shown inner directory, `Listing(Progress { scanned, total })` at most every 100 ms, `Listing(Done)`, `Listing(Failed)` |
| View preparation (a listing thread) | `View(Progress)`, `View(Ready { path })`, `View(Failed)` for F3 and F4 of a member or a remote file |
| Remote listing (a listing thread) | `Listing(Batch)` per `READDIR` reply; `LinkTargets` after the symlink pass; `FreeSpace` from `statvfs@openssh.com` |
| Session reader, one per session | Replies to the waiting callers by request id; `Remote(SessionLost { session, reason })` to the UI |
| Session stderr, one per session | Nothing to the UI. ssh's stderr goes to the terminal during the connect hand-off, and into a 4 KiB tail after it |
| Preview thread, one | `Preview(Ready { generation, image })`, `Preview(Card { generation, card })` |

The UI thread performs a connect itself, inside the terminal hand-off (section 5.2), as it
does for F3. The event loop waits with a deadline only while a preview debounce is pending,
as with the M1 tick (P-5).

Cancel follows the `AtomicBool` pattern. SFTP requests never get stuck in the kernel: the
network I/O happens in the `ssh` child, and manycommander's threads wait on reply slots that
they re-check for cancel at least every 50 ms. A cancelled request's late reply is dropped by
id. A dead connection fails every outstanding request.

### 2.6 Resource bounds

| Resource | Bound |
|---|---|
| Archive indexes | 1,000,000 entries per archive; at most 4 cached and 128 MB in total; an index that a visible tab shows is never evicted |
| Archive fds | One per cached index (section 3.2) |
| Tar extraction | An LRU of 64 open destination directory fds (section 3.5) |
| SFTP sessions | At most 4, each with one `ssh` child, one reader thread and one stderr thread (section 5.7) |
| Preview | One preview thread; 8 prepared images and 64 MB in its cache; at most 8 images stored in the terminal |
| Runtime copies | Removed when the hand-off returns; an edited copy is kept and reported |
| inotify | No watch in archive or remote panels |

An idle manycommander with open sessions, a cached index and a quick view has no periodic
wakeups: the session threads block in `read(2)` on pipes, and keepalives are ssh's business
(P-5).

### 2.7 Dependencies

| Crate | Features | Reason |
|---|---|---|
| `zip` | default features off; deflate through `flate2` with `zlib-rs`, deflate64, bzip2, lzma and xz; its zstd method only if it uses the same `zstd` crate line as below | zip reading from the central directory; `name_raw` for byte names |
| `tar` | default features off (no `xattr`); at least the release that fixes RUSTSEC-2026-0067 and RUSTSEC-2026-0068 | tar reading through `Entry` headers and `path_bytes`, never `unpack` |
| `flate2` | `zlib-rs` backend | gzip (measured faster than the `gzip` tool) |
| `zstd` | default: C libzstd, compiled by `cc` | D-1 |
| `lzma-rust2` | without the `optimization` feature | xz. Pure Rust, and `zip` and `sevenz-rust2` use it too. The C `liblzma` is 1.6x faster, but its supply-chain history (CVE-2024-3094) weighs against it |
| `bzip2` | default: pure-Rust backend | D-2 |
| `sevenz-rust2` (stretch) | default features off (no AES, no compression, no PPMd) | D-6 |
| `image` | default features off; `jpeg`, `png`, `gif`, `webp`, `bmp` | Decoding with `Limits` and EXIF orientation |
| `ratatui-image` (gated), or `icy_sixel` | `ratatui-image`: default features off plus `crossterm` (its default links libchafa) | D-5 |
| `nix` | `signal` | The safe `pthread_sigmask` for the connect hand-off (section 5.2); the lock file already carries it for a dev-dependency |
| dev: `zip` writer, `tar` `Builder` | `zip` `deflate` | Generated test fixtures (section 8.2) |

The research counted about 30 new crates for archives and about 56 for `ratatui-image`,
against 99 today. `cargo deny` stays clean with the D-2 licence row. `multiple-versions =
"warn"` tolerates the known duplicates, which are aligned where the crates allow: an older
major of `rustix`, `rand` and `miniz_oxide` behind `ratatui-image`, and a second
`lzma-rust2` line behind `sevenz-rust2`. Each dependency states its reason in the commit that
adds it (NFR-SUP).

## 3. Archives

### 3.1 Formats and detection

| Format | Names (ASCII case-insensitive) | Magic | Reader | Access |
|---|---|---|---|---|
| zip | `.zip .jar .apk .whl` | `PK\x03\x04`, or `PK\x05\x06` when empty | `zip` | random (central directory) |
| tar | `.tar` | `ustar` at offset 257, or a valid v7 header checksum | `tar` | random (the stream is the file) |
| tar.gz | `.tar.gz .tgz` | `1f 8b` | `flate2`, every member | sequential |
| tar.zst | `.tar.zst .tzst` (so `.pkg.tar.zst`) | `28 b5 2f fd` | `zstd`, every frame | sequential |
| tar.xz | `.tar.xz .txz` | `fd 37 7a 58 5a 00` | `lzma-rust2`, every stream | sequential |
| tar.bz2 | `.tar.bz2 .tbz2 .tbz` | `BZh` | `bzip2`, every stream | sequential |
| 7z (stretch) | `.7z` | `37 7a bc af 27 1c` | `sevenz-rust2` | by entry; a solid block decodes from its start |

`Enter` on a file whose name ends in one of these opens it as an archive. `Alt+O` opens any
file as one (for `.epub`, `.docx`, `.crate` and other renamed containers). The listing
thread opens the file through the M1 4.3 `O_PATH` sequence, so a FIFO or device is never
opened, and checks the magic. For a compressed tar it also checks that the first 512
decompressed bytes form a tar header. A mismatch fails the load like any failed navigation
(M1 5: the panel stays and shows "not a zip archive"). A member of an archive is not opened
as an archive: "nested archives are not supported; extract it first".

### 3.2 The index

`ArchiveIndex` holds one node per entry: parent, name (in an arena), kind, mode, size,
mtime, link target, and a locator. The locator is a zip entry index, a tar offset in the
decompressed stream (from `Entry::raw_file_position`), or a 7z entry index. Building the
index applies A-1 and these rules:

- Implicit directories are synthesized (mode `0o755`, no time).
- The last of duplicate members wins, as in tar. A later member of another kind that would
  replace a directory with children is skipped as "conflicting member".
- A tar hard-link member points at the node of the earlier member it names. One that names
  a missing or later member is listed but not extractable (A-3).
- Zip names come from `name_raw` as bytes; CP437 is not decoded. Zip times come from the
  extended-timestamp field when present, else from the DOS time as local time (2 s
  resolution).
- Encrypted zip entries are flagged: listed, never extracted ("encrypted").

The index keeps the archive's read fd, opened at scan time. Every later read (F3, F5, the
quick view) uses that fd with positioned reads, so a replaced archive name cannot redirect
it. The cache key is `StatKey = (st_dev, st_ino, size, mtime, ctime)`. When a refresh of the
containing directory, or `Ctrl+R`, sees another key for the archive's name, the panel says
"the archive changed on disk; Ctrl+R re-reads", and `Ctrl+R` then rescans. A cache of at
most 4 indexes (section 2.6) makes leaving and re-entering an archive free.

### 3.3 Listing and navigation

A zip lists from its central directory in one read: 10k entries took 21 ms. A tar lists by
reading every header, and a compressed tar only by decompressing all of it. With libzstd,
the 10k-entry package (about 14 MB as `.pkg.tar.zst`) took 110 ms and a 345 MB package
510 ms; xz and bzip2 take seconds for the larger one.

The scan therefore streams. Rows of the shown directory arrive in batches (the first at most
256, as in M1 3.1), and the footer shows "reading archive: 48 of 98 MB". `Esc` returns the
panel to its previous place within 100 ms (P-20); the scan checks cancel between blocks of at
most 1 MiB of decompressed data. Entering a subdirectory during the scan shows its members
known so far and keeps appending. A later duplicate that replaces a node of the shown
directory makes the panel re-read that directory from the index. When the scan ends, the
index is complete and cached. Navigation inside it builds each listing from memory on a
listing thread, never on the UI thread.

The footer shows the entry count, the unpacked total, and the skipped members ("3 members
not shown: unsafe path"). There is no inotify watch inside an archive.

### 3.4 View and edit a member

F3, F4 and `Enter` on a member copy it into
`$XDG_RUNTIME_DIR/manycommander/view/<random>/<name>`:

- manycommander creates `manycommander/` and `view/` with mode `0700`, opens each with
  `O_DIRECTORY` + `O_NOFOLLOW`, and checks that the user owns it. A symlink or a foreign
  owner refuses the view. Without `XDG_RUNTIME_DIR`, the same layout goes under a random
  name in the system temporary directory.
- The copy runs on a listing thread through the local engine, as an extraction of one member
  into the new, empty directory. The status row shows progress, and `Esc` cancels. It is not
  a job, so it runs while a job runs. A member above 256 MB asks first.
- The hand-off then runs `$PAGER` or `$EDITOR` on the copy (M1 6). The copy's mode is
  `0400`.
- After the child exits, the directory is removed. If the copy changed (size, mtime or
  ctime), it is kept, and the status line says "archives are read-only; your edited copy is
  at <path>".

### 3.5 Extract (F5)

Extraction is a copy job with an `ArchiveTree` source (2.3). Its plan comes from the index,
so it reads nothing from the archive before the destination checks. The confirm dialog shows
the counts and the declared total; A-4's free-space question comes from the plan.

- **Zip and 7z** walk the plan in tree order and open members by locator. A 7z member in a
  solid block decodes the block from its start.
- **Tar**, plain and compressed, is extracted in one pass in stream order. The job first
  creates the selected directories in tree order (M1 4.7: `mkdirat` with `0700`). Then it
  reads the stream once and commits each selected member into its directory, which it
  reaches from the destination root by the component walk with `O_NOFOLLOW` (A-2); an LRU
  keeps 64 directory fds open. It stops after the last selected member. Directory modes and
  times follow in post-order, as in M1 4.7.
- Symlink members become symlinks under a temporary name and commit like files (M1 4.7).
  Hard-link and special members follow A-3.
- Questions ("file exists", "directory exists", errors), cancel and the report are M1's. A
  cancelled member leaves no partial name (I-2).
- The mode is masked to `0o777` and the mtime comes from the member. Ownership, xattrs and
  ACLs are not restored.

Extraction does not fsync, like copy (NFR-DUR).

## 4. Image quick view

### 4.1 Quick view mode

`Ctrl+Q` turns the inactive side into a quick view of the active panel's cursor entry.
`Ctrl+Q` again turns it back. While the view is on:

- Cursor movement in the active panel updates the view after the debounce (4.4).
- `Tab` swaps sides: the panel under the view becomes active and visible, and the view moves
  to the other side. The view is always the inactive side.
- The hidden panel keeps its directory, listing and watch. Verbs are unchanged: F5 and F6
  still default to the hidden panel's directory, which the view's title bar shows ("other
  panel: ~/Pictures").
- The view shows an image (4.3) for a supported image, and the info card (4.6) for
  everything else.

### 4.2 Terminal probe

At startup, where `detect_enhancement()` runs today (`src/app/runtime.rs:290`, before
`Input::start`), manycommander writes one batch of queries to the terminal in raw mode and
reads the replies:

| Query | The reply means |
|---|---|
| kitty graphics: `ESC _G i=<id>,s=1,v=1,a=q,t=d,f=24;AAAA ESC \` | `ESC _G i=<id>;OK ESC \`: kitty graphics |
| `CSI 16 t` | `CSI 6 ; <height> ; <width> t`: the cell size in pixels |
| `CSI c` (DA1), last | The end of the replies. A parameter `4` means sixel |

The keyboard-protocol query may join this batch; one round trip is cheaper for P-2, and T4
decides. The read ends at the DA1 reply, which every terminal sends after the earlier
replies, or after 100 ms (V-6). Inside tmux (`TMUX` set), the graphics query goes through
tmux's passthrough (`ESC Ptmux;` with doubled escapes); it gets a reply only when the user's
tmux allows passthrough. Environment variables never enable a protocol on their own. Bytes
that are not replies, such as keys typed during the probe, are dropped. Without a
`CSI 16 t` reply, the cell size comes from the `TIOCGWINSZ` pixel fields; without either,
only halfblocks are used. The result lasts the session; the cell size is read again on
resize.

### 4.3 Protocols

| Terminal | Protocol | Notes |
|---|---|---|
| Ghostty, Kitty | kitty graphics | One transmit per image. A replaced image is deleted (`a=d,d=I,i=<id>`), so the terminal's image storage (320 MB per screen in Ghostty) never fills. Unicode placeholders are used where they render; reports on their Ghostty support conflict, so A-QV-8 checks them. Without them, the image is placed directly at the pane's cell position outside tmux |
| foot | sixel | Encoded once per image and size; re-sent only when its area is redrawn. The pane never reaches the last row, which avoids a sixel scrolling the screen |
| tmux in Ghostty | kitty through passthrough with unicode placeholders when the probe got a reply; else halfblocks | manycommander never sets `allow-passthrough` (V-3); the F1 help names the option |
| tmux in foot | sixel when tmux reports it in DA1 | tmux re-encodes sixel itself |
| Alacritty, others | halfblocks (`▀` with 24-bit colours) | Needs truecolor. With `NO_COLOR` or without truecolor, only the card |

`preview.protocol = "auto" | "kitty" | "sixel" | "halfblocks" | "off"` in `config.toml`
overrides the probe; the default is `auto`.

### 4.4 Pipeline

1. **Debounce.** A cursor change starts a 100 ms timer, and each further change restarts it.
   Rapid navigation never starts a decode or transmits an image.
2. **Request.** The UI thread hands the preview thread the latest request: generation,
   place, entry, the pane's size in cells and pixels, and the protocol. An older request
   that has not started is replaced.
3. **Read.** The thread opens the entry (V-2): a local file through the `O_PATH` sequence,
   an archive member or a remote file through `Provider::open_read` (V-5 decides when). It
   reads at most 64 MB, detects the format by magic bytes, and decodes with `image::Limits`.
4. **Prepare.** It applies the EXIF orientation, takes the first frame of an animated GIF or
   WebP, fits the image into the pane's pixel box without upscaling, and encodes it for the
   protocol.
5. **Deliver.** It sends `Preview(Ready { generation, image })`. The UI drops stale
   generations and draws the image in the next frame.

The cache keeps 8 prepared images and at most 64 MB. Its key is the entry's
`(st_dev, st_ino, mtime, size)` (for a non-local entry, its place and metadata), the pane's
cells and pixel size, and the protocol. A kitty image that the terminal still stores is
placed again without a transmit. A pane resize prepares the image again off-thread, and the
pane shows the card meanwhile.

Measurements on the development laptop (release build, a pane of 100x50 cells at 10x20 px):
a 12 MP JPEG decodes in 33 ms and scales in 75 ms; a kitty encode takes 4 ms and a sixel
encode 37 ms. The frame that transmits a kitty image writes up to 4 MB uncompressed. The
transmit is compressed (`o=z`), and the debounce keeps such frames out of rapid navigation
(P-24).

### 4.5 Drawing rules

- V-4: while any modal overlaps the view, or `Ctrl+O` shows the output, the pane shows the
  card instead of the image. Sixel pixels are not cells, so the view clears its region by
  rewriting the region's cells.
- Before a hand-off (F3, F4, the command line, a connect, `SIGTSTP`), manycommander deletes
  its kitty images. The full redraw after the resume places them again from the cache.
- A theme reload's full redraw (M1 7.2) places the image again. Halfblock colours come from
  the image, not the theme.
- On exit, manycommander deletes its images before it leaves the alternate screen.

### 4.6 The info card

For everything that is not a previewable image, the pane shows a card: the name (escaped as
in M1 3.2), kind, size, mtime, mode, numeric owner, a symlink's target (the link is never
followed, V-2), an image's pixel size, and the reason when there is no image ("image larger
than 16384 x 16384 px", "file larger than 64 MB", "remote file: Alt+Q previews it"). A
regular file without a NUL byte in its first 8 KiB also shows its first lines: at most
64 KiB, read on the preview thread, with control characters escaped and tabs expanded. A
directory's card computes no size.

### 4.7 The graphics layer and its gate

`src/preview/` holds the probe (own code), the preview thread and its cache, the card, and
`gfx`. `gfx` is one interface behind which the protocol implementation can be swapped:
`prepare(image, cells, cell pixels, protocol) -> Prepared`, `draw(frame, area, &Prepared)`
and `forget(id)`.

T4 starts with the gate of D-5. It builds the kitty, sixel and halfblocks protocols from the
probed cell size and protocol without calling any `ratatui-image` code path that runs `tmux`
or reads the terminal. A-QV-5 enforces the gate: with `TMUX` set and a recording `tmux` stub
first on `PATH`, a whole quick-view session calls `tmux` zero times. If `ratatui-image` has no
such path without `unsafe` or a change to the process environment (removing `TMUX` from it
is such a change), T4 implements the own layer instead: kitty graphics (direct placement,
and unicode placeholders inside tmux), halfblocks, and sixel through `icy_sixel`. The
research estimates 500 to 800 lines for it. A patched fork is not an option, because
`deny.toml` refuses git sources. An upstream opt-out is welcome, but T4 does not wait for it.

## 5. SFTP

### 5.1 Addresses and connecting

A remote place is `sftp://[user@]host[:port][/path]`. It opens from:

- `cd sftp://...` on the command line (M1 6). In a remote panel a relative `cd` navigates on
  the server; a `cd` whose path starts with `/`, `~` or `$` is local, as in M1.
- A bookmark in `hotlist.toml` (`[[dir]] url = "sftp://..."`, P2 3.2). `Insert` in the
  directories dialog on a remote panel adds one. Frecency records local directories only, so
  `z` and the frequent list never connect.
- `Alt+Left` or `Alt+Right` onto a remote place, and `Ctrl+R` on a lost session.

Nothing else opens a connection: not the start, not a restored tab, not a preview
(NFR-SEC).

The address grammar is strict, because `ssh_config` can expand the host and the user into
`ProxyCommand` and `Match exec` shell commands (the CVE-2023-51385 shape):

| Part | Rule |
|---|---|
| user | `[A-Za-z0-9._][A-Za-z0-9._-]*`, at most 64 bytes |
| host | an `ssh_config` alias or DNS name, `[A-Za-z0-9_][A-Za-z0-9._-]*`, at most 253 bytes; or a bracketed IPv6 literal of hex digits, `:` and `.` |
| port | 1 to 65535 |
| path | Percent-decoded to bytes. Empty, `/~` or `/~/...` is relative to the login directory; anything else is absolute. `.` is dropped, `..` removes the previous component and is never sent, and every other component passes `valid_component` |
| anything else | A query, fragment or parameter is refused: "not a supported sftp:// address" |

A connection is keyed by `(user, host, port)` as typed. A second address with the same key
reuses the open session.

### 5.2 Transport

The argv is `sftp.ssh` from `config.toml` (default `["ssh"]`; tests put `-F <file>` there),
followed by:

```text
-oForwardAgent=no -oForwardX11=no -oClearAllForwardings=yes -oPermitLocalCommand=no
-oRequestTTY=no -oRemoteCommand=none -e none [-l <user>] [-p <port>] -s -- <host> sftp
```

The first four options are the ones `sftp(1)` passes to `ssh`. The others keep a user's
`RequestTTY`, `RemoteCommand` or escape character away from an SFTP channel (R-6). stdin and
stdout are pipes that carry the protocol. stderr is a pipe that the session's stderr thread
reads.

**Process group and prompts.** The M1 hand-off runs its child in manycommander's process
group (`src/app/signals.rs:12`), so the terminal's `Ctrl+C` reaches every process in that
group. An `ssh` child in the same group would therefore die from a `Ctrl+C` typed into
`less` during a later F3. `ssh` starts in its own process group instead (`process_group(0)`,
a safe std API). A connect runs inside the hand-off (D-7):

1. The terminal is handed off as for F3: the input thread parks, the alternate screen is
   left, and the terminal returns to cooked mode. manycommander prints
   "connecting to <host> ... (Ctrl+C cancels)".
2. manycommander spawns `ssh`, makes ssh's group the terminal's foreground group
   (`tcsetpgrp`), and sends `SSH_FXP_INIT`. ssh prompts on `/dev/tty` for a passphrase, a
   password, a new host key or a FIDO touch; `Ctrl+C` reaches only ssh. The stderr thread
   copies ssh's stderr to the terminal.
3. When `SSH_FXP_VERSION` arrives, manycommander takes the foreground back with `SIGTTOU`
   blocked on the calling thread (`nix`'s safe `pthread_sigmask`), because a background
   group that calls `tcsetpgrp` is otherwise stopped. The TUI resumes, and ssh's stderr now
   goes into the 4 KiB tail.
4. When ssh exits or stops first (a wrong password, a refused host key, `Ctrl+C`,
   `Ctrl+Z`), manycommander ends ssh's group, takes the foreground back, shows
   `[connection failed] press Enter to return` under ssh's messages, and the panel stays
   where it was.

OpenSSH's own host-key checking decides; manycommander never weakens it (R-6).

**Closing.** A session closes by closing ssh's stdin, waiting up to 500 ms, then killing
and reaping the child. manycommander closes every session on exit. If manycommander dies,
the pipes close, and ssh exits on EOF.

### 5.3 Protocol client

`src/remote/` holds an SFTP version 3 client (draft-ietf-secsh-filexfer-02, plus the OpenSSH
extensions in its `PROTOCOL` file), with no `unsafe`:

- **Codec.** A packet is a 32-bit length, a type and a request id. A reply longer than
  256 KiB plus its header, OpenSSH's own maximum, ends the session. Every string length and
  count is checked against the bytes left in the packet before any allocation (compare
  RUSTSEC-2026-0154).
- **Requests.** `INIT`, `OPEN`, `CLOSE`, `READ`, `WRITE`, `LSTAT`, `FSTAT`, `SETSTAT`,
  `FSETSTAT`, `OPENDIR`, `READDIR`, `REMOVE`, `MKDIR`, `RMDIR`, `REALPATH`, `STAT`, `RENAME`,
  `READLINK`, `SYMLINK`, and the `@openssh.com` extensions `posix-rename`, `hardlink`,
  `fsync`, `statvfs`, `limits` and `home-directory`. An extension is used only when
  `SSH_FXP_VERSION` announces it (`Caps`). OpenSSH's `SYMLINK` takes its two paths in
  reverse order (target, then link), and the client sends them that way.
- **Threads.** The session reader thread routes each reply by id to its caller's reply slot.
  Callers write packets under one mutex. Listing threads and the job worker make blocking
  calls with the cancel check of section 2.5. The UI thread never calls a session.
- **Pipelining.** Reads and writes keep a window of requests outstanding. The window starts
  from `sftp(1)`'s defaults (64 requests of 32 KiB); the request size rises to the
  `limits@openssh.com` read and write lengths, at most 256 KiB. T10 tunes both against P-26
  and records the values. Replies may arrive in any order. A short read re-requests the
  missing range, and `SSH_FX_EOF` ends the file.
- **Session loss.** Stdout EOF, a decode error or ssh's exit marks the session lost. Every
  outstanding request fails with "connection lost", the child is reaped, and
  `Remote(SessionLost)` reaches the UI with ssh's last stderr line.

### 5.4 Browsing

A listing sends `OPENDIR`, then `READDIR` until `SSH_FX_EOF`, then `CLOSE`. OpenSSH returns up
to 100 names per reply, with attributes, so a listing needs no request per entry. Each reply
becomes one `Listing(Batch)`, and rows appear after the first reply (P-27). `.` and `..` are
dropped. A name with `/` or NUL from the server is skipped and counted in the footer ("2
entries with invalid names not shown"). `longname` is ignored. Attributes give size, uid,
gid, mode, atime and mtime in whole seconds. An entry without a mode shows as "unknown type",
and verbs skip it. Symlink targets are classified after `Listing(Done)` by a second pass of
pipelined `STAT`s, as M1 3.1 does locally. A listing stops at 1,000,000 entries.

`sftp://host` and `/~` resolve through `home-directory@openssh.com`, else through
`REALPATH(".")`. The footer shows free space from `statvfs@openssh.com` when the server has
it. `Esc` during a remote load returns the panel at once (M1 3.1). The listing thread closes
its handle when it sees the cancel.

### 5.5 Download and view (3a)

**F5 remote to local** is a copy job with a `RemoteTree` source (2.3). The scan `LSTAT`s each
selected name and walks directories with `READDIR` (R-3), with up to 8 directory listings in
flight. Each regular file is opened for reading and `FSTAT`ed: it must still be a regular
file of the planned size. It is then read with the pipelined window into the local engine's
temporary file (I-2, I-3). Symlinks are created as symlinks (`READLINK`). The mode (masked as
in M1 4.7) and the times (whole seconds) are applied. SFTP version 3 has no inode numbers, so
hard links on the server arrive as separate files, and the same-file check does not apply.
Cancel closes the handle, and late replies are dropped.

**F3, F4 and `Enter`** on a remote file download it into the runtime view directory exactly
as section 3.4 does for a member: the cap, the question, progress, cleanup, and an edited
copy kept. In 3a an edited F4 copy is not uploaded.

**Session loss during a job** fails the job's remaining entries with "connection lost"
(I-7). A file in progress leaves no partial local name. The panel keeps its rows, says
"connection lost -- Ctrl+R reconnects", and refuses verbs until then.

### 5.6 Write operations (3b)

| Verb | Mechanism |
|---|---|
| F5 local to remote | `remote::put`. Directories: `MKDIR` with `0700`, then the final mode and times in post-order (M1 4.7). Files: R-1. Symlinks: `SYMLINK` under the final name, because a new name cannot show partial content (P2 8.1); a failure `LSTAT`s the name to raise "file exists". Special files are skipped |
| F6 local to remote | F5; then the M1 4.8 group commit unlinks each local source whose upload was synced with `fsync@openssh.com` and committed (R-4) |
| F6 remote to local | F5 download; then, after the local batch's `syncfs`, each remote source whose `LSTAT` still matches is removed, and its directories with `RMDIR` in post-order (R-4) |
| F6 between two panels on one session, Shift+F6 | `LSTAT` of the new name (R-2), then `SSH_FXP_RENAME`. For regular files OpenSSH's rename refuses to replace (it links, then unlinks). For directories and symlinks, and on other servers, a name that appears between the `LSTAT` and the rename can be replaced; the F1 help states this |
| F7 | `MKDIR` per missing component. An existing name is reported, and the cursor moves to it (M1 4.9) |
| Shift+F8 | The M1 typed `delete` confirmation after a scan. `REMOVE` for files and symlinks, `RMDIR` for directories in post-order. The walk never descends a symlink (R-3) |
| F8 | Refused (R-5) |
| F4 on a remote file | After the editor exits, a changed copy raises a question: upload it, or keep the local copy. When the remote file's size or mtime changed since the download, the question also offers "save as `name (1)`". An upload replaces through R-2 |

An upload reads through a `LocalTree` source (directory fds, the `O_PATH` open), so I-5 holds
on the local side. Questions, standing answers, progress, cancel and reports are M1's. An
upload does not fsync unless it is part of a move (NFR-DUR).

### 5.7 Sessions

- At most 4 sessions are open (NFR-RES). A session is in use while a visible tab shows a
  place on it or a job uses it. A session that is not in use stays open for a quick return,
  so going back in the history or showing a hidden tab rarely reconnects. Opening a
  fifth closes the least recently used session that is not in use; when all four are in use,
  the connect is refused with "4 connections are open; close a remote tab".
- `Ctrl+T` on a remote tab shares its session.
- A lost session stays lost until the user reconnects with `Ctrl+R` or opens the place again.
  manycommander never reconnects on its own.
- The command line runs in `Panel.dir` (2.2). `Alt+Enter` inserts the quoted name, and
  `Alt+P` the quoted remote path.

## 6. Keymap

Phase 3 adds these keys. The M1 section 8 ownership rule decides what they do while the
command line holds text:

| Key | Line empty | Line has text | Protocol |
|---|---|---|---|
| `Ctrl+Q` | Toggle the quick view | same (it does not edit the line) | Legacy works: raw mode clears `IXON`, so the XON byte reaches the application |
| `Alt+Q` | Preview a remote file or a compressed-tar member in the quick view (V-5); nothing while the view is off | same | Legacy works |
| `Alt+O` | Open the file under the cursor as an archive | ignored | Legacy works |
| `Enter` on a recognised archive | Browse it (M1: `xdg-open`) | Run the line (M1) | -- |
| `Enter` on a member or a remote file | View it (F3) | Run the line (M1) | -- |

Total Commander opens an archive with `Ctrl+PgDn`. Ghostty binds `ctrl+page_down` to its next
tab, so manycommander uses `Alt+O`.

None of the new chords is bound by default in Ghostty (`ghostty +list-keybinds --default`
binds `ctrl+shift+q`, `alt+1` to `alt+9` and `alt+f4`, but not `ctrl+q`, `alt+q` or
`alt+o`), foot (the default `foot.ini` binds none of them; `Control+Shift+q` appears only in
a commented example), the Omarchy Hyprland bindings (`SUPER + CTRL + Q` is the calculator;
the other bindings are as in P2 10), tmux's default root table, Alacritty or Kitty (both put
their defaults on `Ctrl+Shift`). The plan records the audit.

On the command line, `cd sftp://[user@]host[:port][/path]` connects (5.1).

## 7. Non-functional requirements

### 7.1 Performance

Reference conditions are M1 13.1's. The M1 targets P-1 to P-9 and the P2 targets P-10 to
P-17 keep holding. The archive fixtures have the research's shapes: a 10k-entry package
(155 MB as tar) and a 92-entry package (345 MB as tar), generated at bench time and
compressed into every format.

| ID | Requirement | Target |
|---|---|---|
| P-18 | Zip listing | A 10k-entry zip listed completely <= 50 ms |
| P-19 | Compressed tar listing | First rows <= 50 ms. A full scan of the 10k-entry `.tar.zst` within 1.2x of `zstd -dc` to `/dev/null`, and of its `.tar.gz` within 1.2x of `gzip -dc`. `.tar.xz` and `.tar.bz2` have no ratio target; they stream rows with progress |
| P-20 | Scan cancel | `Esc` returns the panel <= 100 ms during any scan |
| P-21 | Inside a scanned archive | Entering and leaving a 10k-entry directory of a cached index: key-to-frame p99 <= 16 ms, with no rescan |
| P-22 | Extraction | The 10k-entry package to btrfs within 1.5x of `bsdtar -xf`, as zip and as `.tar.zst` |
| P-23 | Preview latency | A 12 MP JPEG in a 100x50-cell pane on screen <= 150 ms after the debounce ends (kitty and halfblocks; sixel <= 200 ms); a cache hit <= 16 ms |
| P-24 | Preview and responsiveness | Key-to-frame p99 <= 16 ms over a scripted run through 200 images (bursts at 30 keys/s, rests of 300 ms); the frame that transmits a kitty image <= 50 ms; no decode, resize or encode on the UI thread |
| P-25 | Probe | P-2 holds in Ghostty and foot with the probe; a terminal that does not answer costs <= 100 ms, once |
| P-26 | SFTP throughput | A 1 GiB download and upload through `ssh` to `sshd -i` (a `ProxyCommand`, no network): within 1.2x of `sftp get` and `sftp put` over the same transport |
| P-27 | SFTP listing | Rows after the first `READDIR` reply. 10k entries with a 30 ms injected round trip in <= 1.1x the time of 101 round trips. No request per entry, except one pipelined `STAT` per symlink |
| P-5b | Idle | 60 s idle with an open session, a cached index and a quick view shown: no wakeups of manycommander (the A-P-5 method; the `ssh` child is not counted) |
| P-6c | Memory | Both panels on 100k-entry directories plus one cached 100k-entry archive index: <= 60 MB RSS |

### 7.2 Other requirements

| ID | Requirement |
|---|---|
| NFR-SEC | **Amended.** Filenames never reach a shell unquoted (M1 6). F3/F4, `xdg-open` and `ssh` spawn by argv. **No network access except the SFTP connections the user opens** (5.1), through the system `ssh` under the user's OpenSSH configuration; never at startup and never in the background. No telemetry. manycommander never relaxes the user's ssh configuration (R-6) and never changes the terminal's or tmux's configuration (V-3). Archive and image parsers run on worker threads under the limits of A-4 and V-2. `unsafe` stays confined to `fsops/sys.rs`, and phase 3 adds none. |
| NFR-SUP | New dependencies per section 2.7, each with its reason in its commit. `cargo deny` is clean with the D-2 licence row. `tar` is at least the release that fixes RUSTSEC-2026-0067 and RUSTSEC-2026-0068. `zip` has its default features off. No code calls a crate's `extract` or `unpack`. |
| NFR-RES | Section 2.6. |
| NFR-REL | Scan, view-preparation, preview, session-reader and session-stderr threads run under `catch_unwind`. A panic ends that scan, view or preview with an error; a panic in a session thread loses that session. The app stays up. A decoder panic inside a job fails the job (M1). |
| NFR-TERM | The quick view is usable at 80x24 (a 40-column pane). Without truecolor, or with `NO_COLOR`, it shows the card only. |
| NFR-OBS | `--log` adds scan timings, preview stage timings and transmit bytes, and SFTP request counts and round trips. It never logs file contents, and it never sees ssh prompts, which go to the terminal. |
| NFR-DUR | Extraction and download do not fsync, like copy. Moves across hosts follow R-4. The F1 help states both. |

## 8. Acceptance checks

*auto*, *bench* and *manual* as in M1 11. All M1 and P2 checks must still pass. Tests that
need `bsdtar`, `zstd`, `xz`, `bzip2`, `sftp-server` or `sshd` print SKIP without them and fail
under `MC_REQUIRE_ALL` (the `check.sh full` convention).

### 8.1 Seam

| ID | Check | Type |
|---|---|---|
| A-SRC-1 | The source split is behaviour-neutral: every M1 and P2 test passes, and the A-FS-5 failpoint sweep runs unchanged through `LocalTree` | auto |
| A-SRC-2 | An in-memory `Stream` source copied into a local directory: files byte-identical, symlinks as links; failpoints and cancel at each chunk and at the commit leave no `.mc-partial-` name; an existing name raises "file exists"; a stream longer or shorter than declared fails with "size mismatch" and commits nothing | auto (failpoint) |
| A-SRC-3 | Every refused combination of section 2.4 is refused before any work, with its message (app-level test) | auto |

### 8.2 Archives

| ID | Check | Type |
|---|---|---|
| A-AR-1 | Generated fixtures in every format (zip, tar, tar.gz, tar.zst, tar.xz, tar.bz2; 7z when T8 runs), including concatenated gzip members, zstd frames, xz streams and bzip2 streams: 10k entries, deep trees, implicit directories, duplicates (the last wins), a leading `./`, a leading `/` (stripped and counted), names with a newline and with invalid UTF-8 (displayed escaped), PAX long names, symlinks and hard links. For every fixture the listing matches `bsdtar`'s mtree output in names, types, sizes, link targets, modes and mtimes | auto |
| A-AR-2 | Hostile fixtures (small committed binaries, with the script that made them): `../` and absolute members, a NUL in a name, a member below a symlink member (the CVE-2025-29787 shape), a symlink plus a directory chmod (the RUSTSEC-2026-0067 shape), a PAX size override (the RUSTSEC-2026-0068 shape), a hard link to an outside path and one to a missing member, device and FIFO members, setuid modes, an overlapping-entry zip. The listing shows the stated skips. After F5 of everything into an empty directory, a recursive listing of its parent differs from the one before only inside the destination; there is no special file, no mode above `0o777`, and no followed symlink | auto |
| A-AR-3 | Damage: truncated `.zst`, `.xz`, `.gz` and `.bz2`, a zip CRC error, and a member longer and one shorter than its header. The member fails with "archive damaged" or "size mismatch", and no partial file remains. Listing a truncated archive shows the members before the damage and "archive damaged" | auto |
| A-AR-4 | Navigation: `Enter` by name; a `.zip` that is not a zip fails and the panel stays; `Alt+O` on an odd name; `..` at the root lands on the archive; the title; `Alt+Left` and `Alt+Right`; `Esc` during a scan; re-entering hits the cache (a scan counter); an archive replaced by rename keeps the view on the indexed inode and says "changed on disk", and `Ctrl+R` rescans | auto |
| A-AR-5 | F5: zip members by locator in any order; tar in one pass that stops after the last selected member (bytes read asserted); directory modes in post-order; symlinks as links; hard links as links within the job; "file exists" with Overwrite, Skip and Rename; the free-space question (failpoint); cancel mid-member leaves no partial name; F6, F7, F8, Shift+F8, `Alt+L`, `Alt+A` and `Ctrl+M` in an archive are refused; an archive panel as destination is refused | auto (failpoint) |
| A-AR-6 | F3 and F4 on a member: the runtime directory is `0700`; a symlinked `view/` is refused; the copy is removed after the pager exits; a member above 256 MB asks first; an edited F4 copy is kept and its path reported | auto |
| A-AR-7 | An encrypted zip entry is listed, and F3 and F5 refuse it with "encrypted" | auto |
| A-AR-8 | P-18, P-19, P-20, P-21, P-22, P-6c | bench |

### 8.3 Quick view

| ID | Check | Type |
|---|---|---|
| A-QV-1 | Probe parsing from canned replies: kitty OK with a cell size and DA1; a foot-style DA1 with `4` and a cell size; DA1 only; tmux-wrapped replies; no reply within 100 ms; garbage and interleaved key bytes. Each picks the protocol of section 4.3; environment variables alone never enable one | auto |
| A-QV-2 | Pipeline: no preview request during a burst of 30 cursor moves 20 ms apart, and one after the burst; stale generations dropped; a cache hit decodes nothing; a 20000x20000 PNG header, a truncated JPEG, a FIFO named `x.png`, a 100 MB file and a symlink to an image each give the card with its reason, and nothing panics | auto |
| A-QV-3 | `TestBackend` snapshots: halfblocks of a known image, the card with an escaped name, a text head with control characters, a binary file | auto |
| A-QV-4 | pty test in which the test acts as a kitty-capable terminal: exactly one transmit per image, a delete for each replaced image, no image while a dialog overlaps, deletes before a hand-off and before exit; key-to-frame from `--log` stays within P-1 while previews are pending | auto |
| A-QV-5 | V-3 and the D-5 gate: with `TMUX` set and a recording `tmux` stub first on `PATH`, a quick-view session through all three protocols invokes `tmux` zero times, and the output contains no terminal-mode sequence beyond M1's (alternate screen, bracketed paste, cursor, keyboard protocol) | auto |
| A-QV-6 | V-5: moving over remote files and compressed-tar members reads nothing from them (provider read counters); `Alt+Q` loads the entry under the cursor | auto |
| A-QV-7 | P-23, P-24, P-25 | bench |
| A-QV-8 | Ghostty: a kitty image with placeholders or, if they fail, direct placement. foot: sixel. Ghostty in tmux with `allow-passthrough on`: kitty through passthrough; with it off: halfblocks, and `tmux show -p allow-passthrough` is the same before and after. A dialog over an image; a theme reload, an F3 hand-off, a resize and a font-size change during a preview | manual |

### 8.4 SFTP

| ID | Check | Type |
|---|---|---|
| A-SF-1 | Codec: every packet type round-trips (`insta` snapshots); a length above the maximum, truncated strings, counts larger than the packet and unknown ids end the session without an allocation beyond the packet bound | auto |
| A-SF-2 | Against `sftp-server -e -d <dir>` on pipes: the version and extensions; the home directory; a 10k-entry listing in at most 101 `READDIR` replies with one batch per reply; names with a newline, invalid UTF-8 and 255 bytes byte-exact; the symlink pass. From a scripted server: names with `/`, `..` and NUL are skipped and counted | auto |
| A-SF-3 | Download through the local engine: 0 B, 1 B, 1 MiB + 1 and 100 MiB files byte-identical, also with out-of-order replies and short reads from the scripted server; mode and mtime applied; "file exists"; cancel mid-file leaves no partial name, and the session stays usable; a remote symlink arrives as a symlink; a remote FIFO is skipped as "special file" and never opened | auto |
| A-SF-4 | Session loss: the server killed mid-listing and mid-download. The panel keeps its rows and says "connection lost"; the job reports every entry (I-7); no partial name remains; the `ssh` child is reaped; `Ctrl+R` reconnects | auto |
| A-SF-5 | Real `ssh` with a generated `ssh_config` whose `ProxyCommand` runs `sshd -i` (a scratch host key, `AuthorizedKeysFile`, `UserKnownHostsFile`): key authentication works; an unknown host key with `StrictHostKeyChecking ask` prompts in the hand-off, and the pty test answers; a changed host key makes ssh refuse, and manycommander shows ssh's message; the spawned argv (recorded by a wrapper) has the fixed options and `--` before the host; a host that starts with `-` or contains a shell metacharacter or `%` is refused before any spawn | auto |
| A-SF-6 | Process groups: with a session open, a `Ctrl+C` typed into the pager of a local F3 leaves the session working; after a connect, the terminal's foreground group (`tcgetpgrp`) is manycommander's and keys reach the TUI; `Ctrl+C` at a password prompt returns to the TUI with "connection failed" | auto |
| A-SF-7 | Upload (R-1, R-2): the commit through `hardlink` and `REMOVE`; an existing name raises "file exists"; Overwrite uses `posix-rename`; a server without it refuses the overwrite; a server without `hardlink` uses direct write (visible while written, removed on cancel); failures injected with `sftp-server -P <requests>`, and cancel at every step, leave no `.mc-partial-` name, except after a lost session, when the report names it | auto |
| A-SF-8 | 3b verbs: F7 (an existing name reported); Shift+F6 with the `LSTAT` check; F6 within one session; Shift+F8 of a tree that holds a symlink to a directory outside it (the outside stays intact); F8 refused with the R-5 message; remote to remote across sessions refused; a read-only server (`sftp-server -R`) fails per entry | auto |
| A-SF-9 | Moves across hosts (R-4): a failpoint sweep in the A-FS-5 style over local-to-remote and remote-to-local moves holds A-FS-5's general predicate; a remote source changed between download and removal is kept ("source changed; kept both"); a server without `fsync` gets the confirm note and "not synced on the server" | auto (failpoint) |
| A-SF-10 | F4 on a remote file: an unchanged copy uploads nothing; a changed one asks; a remote file changed since the download offers "save as `name (1)`" | auto |
| A-SF-11 | P-26, P-27, P-5b | bench |
| A-SF-12 | A real server from the owner's `ssh_config`: `ProxyJump`; the agent or a passphrase prompt through the hand-off; `Ctrl+C` at the prompt; a dropped network ends the session within ssh's own keepalive settings; reconnect | manual |

### 8.5 General

| ID | Check | Type |
|---|---|---|
| A-KM-1 | The section 6 keys with the line empty and with text | auto |
| A-RES-1 | A fifth connect closes the least recently used session not in use, or is refused when all four are in use; a fifth archive index evicts one that no tab shows; the 1,000,000-entry cap stops a listing with its message | auto |

## 9. Alternatives considered

| Alternative | Decision |
|---|---|
| A general VFS trait over local I/O as well | Rejected. I-1 to I-7 rest on directory fds, `O_PATH`, `renameat2` and identities. A trait would hide them and slow P-3 and P-7. Only non-local places use the provider. |
| libarchive in-process (`compress-tools`) | Rejected. A memory bug in one of its many parsers would kill the app (NFR-REL), it has recent parser CVEs (CVE-2025-5914, CVE-2025-5915), it returns names only, and it links a system library. |
| `bsdtar` as a subprocess for every format | Rejected for phase 3. It adds a process and a format conversion per operation for formats the Rust readers cover. It stays the later path for rar, iso and cpio (D-6), and it is the differential oracle of A-AR-1. |
| `ruzstd` (pure-Rust zstd) | Rejected by D-1: 3x slower on the main use case. |
| C `liblzma` | Rejected. `lzma-rust2` is 1.6x slower, but pure Rust, and `zip` and `sevenz-rust2` already use it. |
| `sevenz-rust` (the original crate) | Rejected: RUSTSEC-2026-0245 (path traversal, no fix) and RUSTSEC-2026-0246 (unmaintained). |
| `russh` and `russh-sftp` | Rejected by D-3: tokio, about 120 more crates, a C crypto backend, a config parser without `Include` or `Match`, `known_hosts` without wildcards, `@revoked` or `@cert-authority`, and the `rsa` advisory RUSTSEC-2023-0071, which forces RSA off. |
| `openssh` and `openssh-sftp-client` | Rejected: the transport of D-3, but async, with tokio and a multiplex master. It is the fallback if the own client's protocol work proves costly. |
| `ssh2` (libssh2) | Rejected: C with OpenSSL, no `ssh_config`, no `ProxyJump`. |
| `sshfs` or `rclone mount` | Rejected as the feature: a dead link leaves syscalls stuck, there is no inotify, and `RENAME_NOREPLACE` and identities depend on FUSE. It stays a documented workaround. |
| `SSH_FXP_RENAME` as the upload commit | Rejected. Protocol version 3 does not say whether it replaces an existing name, and OpenSSH's no-replace rename is a property of one server. `hardlink@openssh.com` is defined as `link(2)`, which never replaces (R-1). |
| A first connect attempt with `BatchMode=yes`, to skip the hand-off when no prompt is needed | Deferred. It doubles the connection setup whenever a prompt is needed and hides FIDO touch messages. The later askpass dialog removes the screen switch instead. |
| ssh in manycommander's process group | Rejected: a `Ctrl+C` during a later hand-off would end every session (5.2). |
| Previews of remote files on every cursor rest | Rejected (V-5): browsing would pull data over the network. |
| The terminal probe on the first `Ctrl+Q` | Rejected. It needs the input thread parked in mid-session, and a key typed meanwhile is lost. The startup probe costs one round trip before the first frame. |
| `viuer`, chafa, Überzug++ | Rejected: `viuer` writes to stdout outside ratatui's buffer; chafa is C; Überzug++ is an external overlay process. |
| Restoring remote tabs at startup | Rejected: manycommander connects only when the user asks (NFR-SEC). |
| `xdg-open` of a member or a remote file on `Enter` | Deferred. The temporary copy would outlive manycommander's knowledge of the application that uses it. `Enter` views the file (F3). |

## 10. Risks

| Risk | Where | Mitigation |
|---|---|---|
| A compressed tar lists only by decompressing all of it (seconds for xz and bzip2) | archives | Streamed rows, progress, `Esc`, the index cache, libzstd (D-1) |
| A parser differential or path traversal in an archive crate | archives | A-1 to A-5; no crate `extract` or `unpack`; the fixed `tar` release; hostile fixtures and the `bsdtar` differential (A-AR-1, A-AR-2) |
| libzstd is C in-process | archives | D-1: one narrow, heavily fuzzed decoder on a worker thread; A-AR-3 feeds it truncated input |
| ssh wants the terminal after the connect (for example `ControlMaster ask`) and stops in the background | SFTP | The operation waits until cancelled, and cancel ends the session. The F1 help names the cause |
| A `ControlPersist` master keeps ssh's stderr open | SFTP | A session ends on stdout EOF or the child's exit and never waits for stderr EOF |
| A server without `posix-rename`, `hardlink` or `fsync` | SFTP | A refused overwrite, the direct-write mode, "not synced on the server" (R-1, R-2, R-4) |
| The path-based protocol weakens I-5 and the move check | SFTP | R-3, R-4, and the F1 help |
| Megabyte kitty transmits and sixel redraws cost frame time | preview | Debounce, compressed transmit, redraw only on change, hide under modals; P-24 measures it |
| Ghostty's unicode placeholder support is disputed | preview | A-QV-8; direct placement outside tmux, halfblocks inside |
| `ratatui-image` changes tmux options | preview | The D-5 gate and A-QV-5; the own layer otherwise |
| About 86 new crates, some in duplicate lines | all | `cargo deny`; minimal features (section 2.7); alignment when adding |
| The SPDX identifier of D-2 contains a dotted release number, which the publication gate reads as a version | release | T2 adds a matching `.publish-allow.tsv` row for `deny.toml` |

## 11. Release

Phase 3 ships as the next minor version, as phase 2 did (P2 14): the version in
`Cargo.toml`, a `CHANGELOG.md` entry, the release workflow on a `v` tag with the tarball and
its SHA-256, and `.publish-allow.tsv` rows for the product's own version. The site gains
pages for archives, the quick view and SFTP, and the keys page and screenshots describe
phase 3.

## Appendix A. Review resolution

Pending. The independent adversarial review (grok) of this draft and its plan runs before
T1. Its findings and their resolution go here.
