---
title: manycommander phase 3 implementation
type: plan
status: in-progress
owner: manycommander
source: ../specs/2026-09-28-manycommander-phase3-design.md
created: 2026-09-28
updated: 2026-09-29
---
# manycommander phase 3 implementation

Build the [phase 3 design](../specs/2026-09-28-manycommander-phase3-design.md) (cited as
"P3 <section>"; acceptance checks as `A-*`; invariants as A-1..A-5, R-1..R-6 and V-1..V-6)
and release it as the next minor version. The
[M1/M2 design](../specs/implemented/2026-09-27-manycommander-design.md) and the
[phase 2 design](../specs/2026-09-28-manycommander-phase2-design.md) stay normative for
everything phase 3 does not change.

Precondition: the [phase 2 plan](2026-09-28-manycommander-phase2.md) is complete and its
release is out. Phase 3 builds on phase 2's grouped sources, `Source` and `Place` (P2 T6),
forms and the directories dialog. T1 does not start before that: P2 T6 lands the panel's
`Source` and `Place`, and P2 T5 to T7 must have stopped editing `Group` (P3 appendix A,
finding 21).

Exclusions: writing or packing archives, nested archives, rar, iso and cpio (a bsdtar
subprocess decoder), copies between two servers, a remote trash, an in-TUI askpass dialog,
previews of video, PDF, AVIF or HEIC, keymap configuration, a job queue.

Authorization: the owner authorized phase 3 "as far as the session gets" (phase 2 plan,
Authorization) and delegated the seven decisions of P3 1.2 while away. The owner is away
during execution; the session decides open points against the design and records them under
"Decisions made during execution". Before T1, the design and this plan went through an
independent adversarial review (grok); its resolution is the design's appendix A and the
review record below. Nothing outside this repository is changed, except publishing the
GitHub release (T12), which the owner asked for.

Execution: this session orchestrates. Each task goes to one implementer subagent with the
task's brief, in dependency order and never two at a time, because the tasks share
`src/app/mod.rs`, `src/app/keys.rs`, `src/app/runtime.rs`, `src/panel/mod.rs` and
`src/ui/*`, and a half-written change breaks another agent's build. Every task commits its
own paths (Conventional Commits) and runs `scripts/check.sh quick` plus its own tests; the
session runs `scripts/check.sh full` and pushes at task boundaries. Pushes happen from the
main checkout when it is clean of other sessions' edits, otherwise from a disposable detached
worktree under `~/.manyfold-worktrees/manycommander/` (btrfs, with the publication gate's
environment).

## Tasks

| Task | Paths | Dependencies | Acceptance |
|---|---|---|---|
| **T1 Source seam.** `Provider`, `VPath`, `Caps`, `PlaceError` (P3 2.1). The copy engine's source side: the trait `Origin`, `OriginFile`, `Order` and `LocalOrigin` in `fsops::origin`, with today's scan and `O_PATH` open moved behind the trait (P3 2.3). The opened-group type `fsops::group::Source` is renamed `OpenGroup`, so the crate has one `Source`, the panel's (P3 1.4). `Transfer::file` takes an `OriginFile`: `Local` keeps `copy_file_range`, sparse files and the hard-link map; `Stream` reads into the job buffer, stops at the first byte past `declared` without writing it, and keeps its temporary file across "file exists" (the M1 4.7 amendment). The move batch removes sources through `Origin::remove`. `Root` in `Group`, with the P2 group open on `Root::Local` only; `Dest` in `JobSpec::{Copy, Move}`; `Source::{Archive, Remote}` (the panel source of P2 2.4) and `Place::{Archive, Remote}` with the history rules (P3 2.2), without UI yet. The P3 2.4 refusal table in `app`. A test-only in-memory stream `Origin`. | `src/provider.rs` (new), `src/fsops/{origin (new), copy, mv, plan, group, job}.rs`, `src/panel/mod.rs`, `src/app/{mod,jobs}.rs`, `tests/fs_origin.rs` (new) | phase 2 complete (P2 T6 has landed `Source` and `Place`; P2 T5 to T7 no longer edit `Group`) | A-SRC-1, A-SRC-2, A-SRC-3; every M1 and P2 test passes, including the A-FS-5 sweep with failpoints |
| **T2 Archive index and listing.** Adds `zip`, `tar`, `flate2`, `zstd`, `lzma-rust2` and `bzip2` with the P3 2.7 features, and the `zip` writer and `tar` `Builder` as dev-dependencies, each with its reason. `zip` is a release in the patched range of RUSTSEC-2025-0168, with `deflate-flate2-zlib-rs` and `deflate64` instead of the `deflate` meta-feature. `deny.toml` allows the SPDX bzip2 licence of `libbz2-rs-sys` (D-2). Its identifier carries a dotted release number, so it is written only in `deny.toml`, and the same commit adds the `.publish-allow.tsv` row `exact version`, `*`, `deny.toml` (tab-separated); commit messages and documents name the licence in words. Detection by name and magic, with the format named in the failure (P3 3.1); `ArchiveIndex` with A-1, implicit directories, duplicates, hard-link targets resolved as `VPath`s, pax and GNU long-name headers folded, GNU sparse members skipped, encrypted flags, zip byte names with the Unicode path rule, and times, the held fd with positioned reads and the position-keeping adapter, and `StatKey` (P3 3.2); the zstd and xz window caps (A-4); the index cache (4 indexes, 128 MB, 1,000,000 entries); the streaming scan with progress, `Esc`, abandonment under `MAX_ABANDONED`, navigation during the scan and re-reads (P3 2.5, 3.3). `Source::Archive` and `Place::Archive` in the UI: `Enter` by name, `Alt+O`, `..`, the title, the footer, `Space` from the index, `Ctrl+R` on a changed key. Hostile fixtures committed with the script that made them. | `Cargo.toml`, `Cargo.lock`, `deny.toml`, `.publish-allow.tsv`, `src/archive/{mod,detect,index,zip,tar}.rs` (new), `src/panel/{mod,listing}.rs`, `src/app/{mod,keys,event,runtime}.rs`, `src/ui/panel.rs`, `tests/archive_list.rs` (new), `tests/fixtures/archive/**` (new) | T1 | A-AR-1 with the `bsdtar` differential under `TZ=UTC`, A-AR-4, the listing halves of A-AR-2 and A-AR-3, the index half of A-RES-1; `cargo deny` clean; the publication gate passes |
| **T3 Archive view and extract.** `ArchiveOrigin`: zip by locator, tar in one pass with directories first and the 64-fd LRU (P3 3.5). `Provider::open_read` for members, which the view preparation and T4's archive previews use. A-2 to A-5 on the write side: the header re-check keyed on the winning node's locator, with every other header skipped; the output count that stops a member at its declared size ("size mismatch"); special files and privilege bits; hard links made from an `O_PATH` fd of the destination inode after an identity check; the free-space question; a destination entry that is the archive itself never replaced. The runtime view directory (with the `mkdtemp`-style private directory when `XDG_RUNTIME_DIR` is unset) and the view-preparation thread (P3 3.4) for F3, F4 and `Enter` on members: the `0600` copy, the identity check that counts a rename-over as an edit, and the kept edited copy; SFTP reuses both in T6. The P3 2.4 refusals; the encrypted refusal. | `src/archive/{extract (new), mod}.rs`, `src/viewtemp.rs` (new), `src/fsops/{origin,copy}.rs`, `src/app/{mod,jobs,handoff,event,runtime}.rs`, `src/ui/dialog.rs`, `tests/archive_extract.rs` (new) | T2 | A-AR-2, A-AR-3, A-AR-5, A-AR-6, A-AR-7 |
| **T4 Image quick view.** The own graphics layer (D-5, P3 4.7): kitty graphics (direct placement, unicode placeholders inside tmux), halfblocks, and sixel through `icy_sixel`; no `ratatui-image`. The startup probe (P3 4.2) in place of `detect_enhancement()` in `src/app/runtime.rs`: one write with the graphics, cell-size, keyboard-protocol and DA1 queries, one read loop with a 100 ms deadline, keys in the window discarded, and inside tmux reading on after DA1 until the graphics reply or the deadline. The `preview` module (the `gfx` interface, the preview thread with the 1 s abandonment under `MAX_ABANDONED`, the cache, the card, the text head); the V-2 header check before decode; the quick view mode (`Ctrl+Q`, `Tab`, the title, verbs unchanged, P3 4.1); `Ctrl+Q` and `Alt+Q` always active; the debounce as an event-loop deadline; the V-4 drawing rules (modals, hand-offs, reload, exit); `Alt+Q` and V-5 for compressed-tar members (the remote part lands in T6); `preview.protocol` in the config. Adds `image` and `icy_sixel`, and records their crate count. The local preview can land before the archive half. | `Cargo.toml`, `Cargo.lock`, `src/preview/{mod,probe,gfx,worker,cache,card}.rs` (new), `src/app/{mod,keys,event,runtime,term,handoff}.rs`, `src/ui/{mod,panel}.rs`, `src/config.rs`, `tests/preview.rs` (new), `tests/ui_session.rs` | T2; T3 for archive-member previews (`Provider::open_read` for members, the archive half of A-QV-6) | A-QV-1 to A-QV-5; A-QV-6 for archive members; the A-QV-8 checklist prepared for the owner |
| **T5 SFTP client and transport.** The address grammar (P3 5.1). The codec with its bounds, `INIT` and `VERSION` without a request id, the reader thread, reply slots, cancel with the 2 s drain window and the stuck-session rule, pipelined read and write windows, and session loss (P3 2.5, 5.3). The transport (P3 5.2): the argv with the fixed options directly after the program, and the rejection of `-o`, `-A`, `-X`, `-Y`, `-t` and `-e` in `sftp.ssh`; ssh's own process group; `rustix::termios::tcsetpgrp`, with `SIGTTOU` blocked on the calling thread through `nix`'s `pthread_sigmask` (adds `nix` with `signal`); the stderr thread and its tail; the connect hand-off that polls for `VERSION` and checks ssh's group with `waitid`, with the failure screen; stop as cancel. A scripted test server on the codec, and a latency helper that forwards the pipes with an injected delay. | `src/remote/{mod,url,proto,transport,session}.rs` (new), `src/app/{handoff,runtime,signals}.rs`, `src/config.rs`, `Cargo.toml`, `Cargo.lock`, `tests/sftp_proto.rs` (new), `tests/sftp_ssh.rs` (new), `tests/common/**` | T1 | A-SF-1; the protocol half of A-SF-2 (the library against `sftp-server` on pipes); A-SF-5; A-SF-6 |
| **T6 SFTP 3a.** `RemoteProvider`: a batch per `READDIR` reply, name checks, the symlink pass, the home directory through the `home-directory` extension (one string argument, an `SSH_FXP_NAME` reply) or `REALPATH(".")`, `statvfs` (P3 5.3, 5.4). `Source::Remote` and `Place::Remote`; the session pool (4 sessions, least recently used, shared by `Ctrl+T`, P3 5.7); `cd sftp://` and relative `cd`; bookmark `url` entries and `Insert` on a remote panel. `RemoteOrigin`: the scan with 8 listings in flight, the pipelined download, the `FSTAT` check, cancel with the drain window (P3 5.5). F5 remote to local; F3, F4 and `Enter` through the view directory; `Alt+Q` for remote files; session loss in panels and jobs; `Ctrl+R` reconnect; 3b verbs refused until T7. | `src/remote/{provider,tree,pool}.rs` (new), `src/panel/*`, `src/app/*`, `src/cmdline/mod.rs`, `src/dirs.rs`, `src/ui/*`, `tests/sftp_browse.rs` (new) | T4, T5 | A-SF-2, A-SF-3, A-SF-4; the remote half of A-QV-6; the session half of A-RES-1 |
| **T7 SFTP 3b.** `remote::put` with the R-1 hard-link commit and the direct-write fallback (the report names a possibly partial final name after a lost session), and the R-2 `posix-rename` overwrite. F5 and F6 local to remote, best-effort with the confirm note (R-4); F6 remote to local, which keeps the remote sources and says so (R-4); F6 within one session and Shift+F6; F7; Shift+F8 on a remote panel; the F8 refusal (R-5); the F4 write-back question (P3 5.6). Failpoints on the remote steps for the A-SF-9 sweep. | `src/remote/{put,delete,rename}.rs` (new), `src/fsops/{mv,origin,failpoints}.rs`, `src/app/*`, `src/ui/dialog.rs`, `tests/sftp_write.rs` (new) | T6 | A-SF-7, A-SF-8, A-SF-9, A-SF-10 |
| **T8 7z (stretch).** `sevenz-rust2` (default features off) in the index and in `ArchiveOrigin`; solid-block extraction that decodes each block once per job; fixtures made with `bsdtar --format 7zip`. When the session runs short, T8 is recorded as postponed; no task depends on it. | `Cargo.toml`, `Cargo.lock`, `src/archive/{sevenz (new), detect, index, extract}.rs`, `tests/archive_list.rs`, `tests/archive_extract.rs` | T3 | A-AR-1 and A-AR-5 with 7z fixtures; `cargo deny` clean |
| **T9 Keymap audit, help, docs.** Confirm the audit table below. The F1 help: archives are read-only; `Enter` on archives, and `xdg-open` on the command line for a file that fails the magic check; R-2's and R-3's residual races, and that OpenSSH's rename refuses an existing file, directory or symlink; a remote FIFO swap that blocks the server and ends the session; no remote trash; "the server cannot replace a file atomically"; moves across hosts are best-effort, "not synced on the server", and a download move keeps its remote sources; the direct-write mode's possibly partial final name after a lost session; the connect hand-off and ssh stopping in the background; the tmux `allow-passthrough` option; the `sftp.ssh` key (the fixed options win; `-o`, `-A`, `-X`, `-Y`, `-t` and `-e` are rejected) and the `preview.protocol` key. `site/content/docs/*.md` (new pages for archives, the quick view and SFTP; keys; file operations); screenshots (`cargo run --example site_screens`); the `README.md` feature list. | `src/ui/help.rs`, `site/content/docs/**`, `examples/site_screens.rs`, `README.md`, `tests/ui_keys.rs` | T3-T7; T8 when it runs | `scripts/site.sh check` passes; A-KM-1; the audit table confirmed |
| **T10 Benchmarks.** Harness entries for P-18 to P-27, P-5b and P-6c: generated archives of the P3 7.1 shapes, compressed with the system tools; `sshd -i` through a `ProxyCommand` and the latency helper for SFTP; the tuned pipelining values recorded. P-19's ratio set against the same process's decompress-only run after the first measurement and recorded with its reason; P-6c measured without a prepared preview. The M1 A-P-1..8 and P2 benches re-run. Results in `docs/perf/history.md` and here. | `benches/**`, `scripts/bench/**`, `docs/perf/history.md` | T3-T7; T8 when it runs | A-AR-8, A-QV-7, A-SF-11 recorded with measurements |
| **T11 Code review.** An independent adversarial review (grok) of the phase 3 diff, with the M1, P2 and P3 designs as the contract; focus on A-1..A-5, R-1..R-6, V-1..V-6, the process-group handling and the codec bounds. Every confirmed finding is fixed with a regression test. A fix that touches code a T10 bench measures re-runs that bench and replaces its `docs/perf/history.md` rows in the same commit. | as the findings require; `docs/perf/history.md` for re-run benches | T1-T10 | Findings table recorded here; `scripts/check.sh full` passes |
| **T12 Release.** Version, `CHANGELOG.md`, `.publish-allow.tsv` rows, the matching `v` tag, the GitHub release with the binary and its SHA-256 (P3 11). The changelog names the D-2 licence in words, never by its dotted identifier. | `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, `.publish-allow.tsv` | T11 | The release workflow is green; the release page carries the tarball and checksum; the tarball's binary runs `--version` |

## Local check gate

Unchanged: `scripts/check.sh quick|full|ci|bench` (see the verification-loop overlay). New
integration test files join `cargo test --all-targets` automatically. The new tests need
`bsdtar`, `zstd`, `xz`, `bzip2`, `/usr/lib/ssh/sftp-server` and `sshd`, which the development
laptop has. They use the existing `skip()` helper, so `full` fails when a tool is missing and
`ci` skips with the reason. The `sshd -i` tests need no root and no listening port.
`sftp-server` exits on stdin EOF without flushing its replies, so a harness keeps stdin open
until it has read them.

## Risks and decisions

| Risk | Mitigation |
|---|---|
| The source split (T1) moves the core of the copy engine. | T1 is behaviour-neutral and lands first. The full M1 and P2 suites, including the A-FS-5 failpoint sweep, are its regression net before any feature builds on it. |
| Hostile archives exercise parsers in-process. | Scans run on worker threads under `catch_unwind`, which catches Rust panics; a fault in libzstd ends the process (P3 D-1, NFR-REL). The output count and the window caps bound a bomb (A-4). A-AR-2 and A-AR-3 use committed hostile fixtures. A `cargo-fuzz` target on the index builder is optional after T3 if time allows. |
| Phase 2 is not complete when phase 3 is due. | T1 waits; nothing in phase 3 starts before it (precondition). |
| The own graphics layer is new code (D-5: `ratatui-image` fails the tmux rule). | It fits inside T4 (the research estimates 500 to 800 lines, about half a task more). A-QV-3, A-QV-4 and A-QV-5 test it; A-QV-8 is the owner's check. |
| The process-group handling of ssh is subtle. | A-SF-6 runs it in a pty. Every connect (the command line, a bookmark, history, reconnect) goes through the one connect path. |
| The development laptop has no SSH setup. | Every automated SFTP check uses `sftp-server` on pipes or `sshd -i` through a `ProxyCommand`. A-SF-12 is the owner's manual check. |
| Unicode placeholders may not render in Ghostty. | A-QV-8; direct placement outside tmux, halfblocks inside. |
| About 30 new crates for archives, plus those of `image` and `icy_sixel` (T4 counts them). | `cargo deny` in every task that adds a crate; minimal features (P3 2.7). |
| The D-2 licence identifier carries a dotted release number. | It appears only in `deny.toml`, with its `.publish-allow.tsv` row in the same commit (T2); the publication gate runs before every push. |
| 7z scope. | T8 is a stretch task and can be skipped. |

## Verification

`scripts/check.sh full` before every push. `scripts/check.sh bench` plus the T10 additions
before the release. Manual, by the owner, recorded here: A-QV-8 (Ghostty, foot, tmux),
A-SF-12 (a real server), and the in-terminal confirmation of `Ctrl+Q`, `Alt+Q` and `Alt+O`.

## Review record

The design and this plan went through an independent adversarial review (grok) on
2026-09-28, before any implementation. Its probes ran against the OpenSSH `sftp-server` and
`sshd` installed on the development machine in a temporary directory, with no network. 24
required findings and 14 suggestions; all accepted. Two resolutions go beyond the
reviewer's fix: a cancelled request ends its session only when the server stays silent for
a 2 s drain window (finding 8), and `sftp.ssh` also rejects `-A`, `-X`, `-Y`, `-t` and `-e`,
which override an `-o` wherever they stand (finding 26). The resolution table is the
design's appendix A. Plan changes from it:

- T1 is blocked until phase 2 is complete (P2 T6 lands `Source` and `Place`; P2 T5 to T7
  stop editing `Group`). The copy engine's source side is named `Origin` (`fsops::origin`),
  and T1 renames the opened group `fsops::group::Source` to `OpenGroup`.
- T3 provides `Provider::open_read` for members. T4 depends on T3 for archive-member
  previews, and T4 builds the own image layer (kitty graphics, halfblocks, `icy_sixel`)
  instead of the `ratatui-image` gate, which the review showed to fail.
- T2 pins `zip` to the patched range of RUSTSEC-2025-0168 and writes the D-2 licence
  identifier only in `deny.toml`, with its allow row in the same commit.
- T1, T2, T3, T5, T6 and T7 carry the amended mechanisms (the output count, the locator-keyed
  re-check, links from the destination inode, positioned reads, the retained `Stream`
  temporary file, best-effort moves that keep remote sources, the stuck-session rule, the
  `waitid` hand-off, the `home-directory` extension). T9's help list and T10's P-19 and
  P-6c rules follow the design.
- Benchmark re-run rule: a T11 fix that touches code a T10 bench measures re-runs that bench
  and replaces its `docs/perf/history.md` rows in the same commit.

### Keymap audit (T9)

Done during the design; T9 confirms it against the final keymap.

| Source | Bound there instead | Conflict with P3 6 |
|---|---|---|
| `ghostty +list-keybinds --default` | `ctrl+shift+q` quit, `alt+1`..`alt+9` tabs, `alt+f4` close, `ctrl+page_up/down` tabs, `ctrl+alt+arrows` splits | none (`Ctrl+PgDn`, Total Commander's archive key, avoided) |
| foot `[key-bindings]` (default `foot.ini`) | `Control+Shift+*` clipboard, search and URL mode; `Control+Shift+q` only in a commented example | none |
| Omarchy Hyprland defaults | `SUPER` chords (`SUPER + CTRL + Q` is the calculator, `SUPER + O` pops a window out), `F9`, `Alt+Tab` variants, `Alt+Print`, `Ctrl+Alt+Delete`, media keys | none |
| tmux default root table (a server with `-f /dev/null`) | mouse bindings only; no `C-q`, `M-q` or `M-o` | none |
| Omarchy tmux configuration (`/usr/share/omarchy/config/tmux/tmux.conf`, installed) | prefix `q` reloads the configuration; root `M-1`..`M-9` select windows; no `C-q`, `M-q` or `M-o` | none |
| Alacritty, Kitty (documented defaults; not installed) | `Ctrl+Shift+*` family; Kitty `ctrl+shift+q` closes a tab | none |

## Execution record

### Status

| Task | State | Commit | Notes |
|---|---|---|---|
| T1 | done | 2c9f14a, 797863b, 172b117, 2b5a60c, f8b2e3d | `provider.rs`, `fsops/origin.rs`, `OpenGroup`, `Root`/`Dest`, places and the refusal table; failpoint step counts of local copy, overwrite and hard-linked move identical before and after; P-7 small files unchanged (copy 1.015 vs 1.016 s); 285/332 tests pass |
| T2 | done | 2545758, 4d93ae8, ee4de19 | `src/archive/*`, `app/archives.rs`; 21 hostile fixtures with `make.py`; bsdtar differential clean (two recorded exclusions); 315/363 tests pass; `cargo deny` and the gate clean; probe: 10k-entry zip about 8 ms, a real 10k-entry package first rows 0.2 ms and full scan 1.21x decompress-only |
| T3 | todo | -- | |
| T4 | todo | -- | |
| T5 | todo | -- | |
| T6 | todo | -- | |
| T7 | todo | -- | |
| T8 | todo | -- | stretch |
| T9 | todo | -- | |
| T10 | todo | -- | |
| T11 | todo | -- | |
| T12 | todo | -- | |

Next action: T3.

### Decisions made during execution

| # | Task | Decision | Reason |
|---|---|---|---|
| E-1 | T1 | `Origin` and `copy_from` are public; `scan` plans all groups of a job in one call; `remove` returns `Removed`. | The test origin lives in an integration test; the inside-source check spans all groups; the report messages stay unchanged. |
| E-2 | T1 | Archive and remote roots, `Dest::Remote` and the views hold `Arc<dyn Provider>` until T2 and T5 narrow them; history places name what to reopen and never hold an index or a session. | The concrete types arrive later; a place is built without asking the provider. |
| E-3 | T1 | Every `Stream` keeps its finished temporary file across "file exists", a failed commit and direct-write detection (in direct-write mode the kept file is copied into the final name). Read-side stream failures fail the entry without a retry question; write-side errors keep the M1 question, and Retry reopens through `Origin::open`. | A stream is never read twice (P3 2.3); keeping the file stays within I-2 and I-3. |
| E-4 | T1 | Verbs that later tasks deliver are refused with "not available here yet"; the open refusal texts are "not in an archive" and "not on a server"; F6 out of an archive says "archives are read-only". | Safe until T2, T3, T6 and T7 remove those rows. |
| E-5 | T2 | `zstd` without default features (no legacy formats), on the version line `zip` uses; `ArchiveView.index` narrowed to `Arc<ArchiveIndex>`, `Root::Archive` stays a provider until T3. | One copy of zstd; T3 narrows the root with extraction. |
| E-6 | T2 | Memory caps beyond the design: a tar GNU long-name or pax header is read through a 4 MiB guard ("archive damaged"); an xz container walker refuses an LZMA2 dictionary above 128 MiB and a record count that differs from the blocks; a zip whose last end record declares more entries than the cap is refused before the `zip` crate reads the central directory. | The crates read these wholly into memory; a small compressed file could exhaust memory (A-4). The residual zip case (a failing last end record, an earlier one declaring many entries) is recorded. |
| E-7 | T2 | A damaged zip local header skips that entry as "damaged member"; a tar without its end block is "archive damaged"; the scan sends found rows before inflating any member of 256 KiB or more. | I-7; first rows of a real package went from 439 ms to 0.2 ms. |
| E-8 | T2 | `Ctrl+R` on an unchanged archive re-lists from the index and keeps marks; on a changed one it rescans in place and `Esc` returns to the old view. Only regular files open as archives (a symlink named `x.zip` keeps `xdg-open`). In an archive `Alt+P` is refused and `Alt+O` gives the nested-archive message. Synthesized directories show no date. | I-5 for the symlink; consistent refusals. |
