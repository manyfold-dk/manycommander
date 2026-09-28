---
title: manycommander phase 3 implementation
type: plan
status: draft
owner: manycommander
source: ../specs/2026-09-28-manycommander-phase3-design.md
created: 2026-09-28
updated: 2026-09-28
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
forms and the directories dialog.

Exclusions: writing or packing archives, nested archives, rar, iso and cpio (a bsdtar
subprocess decoder), copies between two servers, a remote trash, an in-TUI askpass dialog,
previews of video, PDF, AVIF or HEIC, keymap configuration, a job queue.

Authorization: the owner authorized phase 3 "as far as the session gets" (phase 2 plan,
Authorization) and delegated the seven decisions of P3 1.2 while away. The owner is away
during execution; the session decides open points against the design and records them under
"Decisions made during execution". Before T1, the design and this plan go through an
independent adversarial review (grok); its resolution goes to the design's appendix A and to
the review record below. Nothing outside this repository is changed, except publishing the
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
| **T1 Source seam.** `Provider`, `VPath`, `Caps`, `PlaceError` (P3 2.1). `SourceTree`, `SourceFile`, `Order` and `LocalTree`, with today's scan and `O_PATH` open moved behind the trait (P3 2.3). `Transfer::file` takes a `SourceFile`: `Local` keeps `copy_file_range`, sparse files and the hard-link map; `Stream` reads into the job buffer with the declared-size check. The move batch removes sources through `SourceTree::remove`. `Root` in `Group`, `Dest` in `JobSpec::{Copy, Move}`, `Source::{Archive, Remote}` and `Place::{Archive, Remote}` with the history rules (P3 2.2), without UI yet. The P3 2.4 refusal table in `app`. A test-only in-memory stream `SourceTree`. | `src/provider.rs` (new), `src/fsops/{source (new), copy, mv, plan, group, job}.rs`, `src/panel/mod.rs`, `src/app/{mod,jobs}.rs`, `tests/fs_source.rs` (new) | phase 2 complete | A-SRC-1, A-SRC-2, A-SRC-3; every M1 and P2 test passes, including the A-FS-5 sweep with failpoints |
| **T2 Archive index and listing.** Adds `zip`, `tar`, `flate2`, `zstd`, `lzma-rust2` and `bzip2` with the P3 2.7 features, and the `zip` writer and `tar` `Builder` as dev-dependencies, each with its reason. `deny.toml` allows the SPDX `bzip2` licence (D-2); `.publish-allow.tsv` gains an `exact version` row for `deny.toml`, because that identifier carries the licence's dotted release number. Detection by name and magic (P3 3.1); `ArchiveIndex` with A-1, implicit directories, duplicates, hard-link resolution, encrypted flags, zip byte names and times, the held fd and `StatKey` (P3 3.2); the index cache (4 indexes, 128 MB, 1,000,000 entries); the streaming scan with progress, `Esc`, navigation during the scan and re-reads (P3 3.3). `Source::Archive` and `Place::Archive` in the UI: `Enter` by name, `Alt+O`, `..`, the title, the footer, `Space` from the index, `Ctrl+R` on a changed key. Hostile fixtures committed with the script that made them. | `Cargo.toml`, `Cargo.lock`, `deny.toml`, `.publish-allow.tsv`, `src/archive/{mod,detect,index,zip,tar}.rs` (new), `src/panel/{mod,listing}.rs`, `src/app/{mod,keys,event,runtime}.rs`, `src/ui/panel.rs`, `tests/archive_list.rs` (new), `tests/fixtures/archive/**` (new) | T1 | A-AR-1 with the `bsdtar` differential, A-AR-4, the listing halves of A-AR-2 and A-AR-3, the index half of A-RES-1; `cargo deny` clean; the publication gate passes |
| **T3 Archive view and extract.** `ArchiveTree`: zip by locator, tar in one pass with directories first and the 64-fd LRU (P3 3.5). A-2 to A-5 on the write side: the header re-check against the index, "size mismatch", special files and privilege bits, hard links through `linkat` checked by identity; the free-space question. The runtime view directory and the view-preparation thread (P3 3.4) for F3, F4 and `Enter` on members, with the kept edited copy; SFTP reuses both in T6. The P3 2.4 refusals; the encrypted refusal. | `src/archive/{extract (new), mod}.rs`, `src/viewtemp.rs` (new), `src/fsops/{source,copy}.rs`, `src/app/{mod,jobs,handoff,event,runtime}.rs`, `src/ui/dialog.rs`, `tests/archive_extract.rs` (new) | T2 | A-AR-2, A-AR-3, A-AR-5, A-AR-6, A-AR-7 |
| **T4 Image quick view.** First the D-5 gate: `ratatui-image` (default features off, `crossterm`) against A-QV-5, with the result recorded under "Decisions made during execution"; if it fails, the own layer with `icy_sixel` (P3 4.7). Then the startup probe (P3 4.2) where `detect_enhancement()` runs, with the decision whether the keyboard query joins it; the `preview` module (the `gfx` interface, the preview thread, the cache, the card, the text head); the quick view mode (`Ctrl+Q`, `Tab`, the title, verbs unchanged, P3 4.1); the debounce as an event-loop deadline; the V-4 drawing rules (modals, hand-offs, reload, exit); `Alt+Q` and V-5 for compressed-tar members (the remote part lands in T6); `preview.protocol` in the config. Adds `image` and the chosen graphics crate. | `Cargo.toml`, `Cargo.lock`, `src/preview/{mod,probe,gfx,worker,cache,card}.rs` (new), `src/app/{mod,keys,event,runtime,term,handoff}.rs`, `src/ui/{mod,panel}.rs`, `src/config.rs`, `tests/preview.rs` (new), `tests/ui_session.rs` | T2 (archive members through the provider) | The gate result recorded; A-QV-1 to A-QV-5; A-QV-6 for archive members; the A-QV-8 checklist prepared for the owner |
| **T5 SFTP client and transport.** The address grammar (P3 5.1). The codec with its bounds, the reader thread, reply slots, cancel, pipelined read and write windows, and session loss (P3 5.3). The transport (P3 5.2): the argv with `sftp.ssh` from the config, ssh's own process group, `tcsetpgrp` with `SIGTTOU` blocked (adds `nix` with `signal`), the stderr thread and its tail, the connect hand-off with the failure screen, and stop as cancel. A scripted test server on the codec, and a latency helper that forwards the pipes with an injected delay. | `src/remote/{mod,url,proto,transport,session}.rs` (new), `src/app/{handoff,runtime,signals}.rs`, `src/config.rs`, `Cargo.toml`, `Cargo.lock`, `tests/sftp_proto.rs` (new), `tests/sftp_ssh.rs` (new), `tests/common/**` | T1 | A-SF-1; the protocol half of A-SF-2 (the library against `sftp-server` on pipes); A-SF-5; A-SF-6 |
| **T6 SFTP 3a.** `RemoteProvider`: a batch per `READDIR` reply, name checks, the symlink pass, the home directory, `statvfs` (P3 5.4). `Source::Remote` and `Place::Remote`; the session pool (4 sessions, least recently used, shared by `Ctrl+T`, P3 5.7); `cd sftp://` and relative `cd`; bookmark `url` entries and `Insert` on a remote panel. `RemoteTree`: the scan with 8 listings in flight, the pipelined download, the `FSTAT` check (P3 5.5). F5 remote to local; F3, F4 and `Enter` through the view directory; `Alt+Q` for remote files; session loss in panels and jobs; `Ctrl+R` reconnect; 3b verbs refused until T7. | `src/remote/{provider,tree,pool}.rs` (new), `src/panel/*`, `src/app/*`, `src/cmdline/mod.rs`, `src/dirs.rs`, `src/ui/*`, `tests/sftp_browse.rs` (new) | T4, T5 | A-SF-2, A-SF-3, A-SF-4; the remote half of A-QV-6; the session half of A-RES-1 |
| **T7 SFTP 3b.** `remote::put` with the R-1 hard-link commit and the direct-write fallback, and the R-2 `posix-rename` overwrite. F5 and F6 local to remote; F6 remote to local with the R-4 removal; F6 within one session and Shift+F6; F7; Shift+F8 on a remote panel; the F8 refusal (R-5); the F4 write-back question (P3 5.6). Failpoints on the remote steps for the A-SF-9 sweep. | `src/remote/{put,delete,rename}.rs` (new), `src/fsops/{mv,source,failpoints}.rs`, `src/app/*`, `src/ui/dialog.rs`, `tests/sftp_write.rs` (new) | T6 | A-SF-7, A-SF-8, A-SF-9, A-SF-10 |
| **T8 7z (stretch).** `sevenz-rust2` (default features off) in the index and in `ArchiveTree`; solid-block extraction; fixtures made with `bsdtar --format 7zip`. When the session runs short, T8 is recorded as postponed; no task depends on it. | `Cargo.toml`, `Cargo.lock`, `src/archive/{sevenz (new), detect, index, extract}.rs`, `tests/archive_list.rs`, `tests/archive_extract.rs` | T3 | A-AR-1 and A-AR-5 with 7z fixtures; `cargo deny` clean |
| **T9 Keymap audit, help, docs.** Confirm the audit table below. The F1 help: archives are read-only; `Enter` on archives; R-2's and R-3's residual races; no remote trash; "the server cannot replace a file atomically"; "not synced on the server"; the connect hand-off and ssh stopping in the background; the tmux `allow-passthrough` option; the `sftp.ssh` and `preview.protocol` config keys. `site/content/docs/*.md` (new pages for archives, the quick view and SFTP; keys; file operations); screenshots (`cargo run --example site_screens`); the `README.md` feature list. | `src/ui/help.rs`, `site/content/docs/**`, `examples/site_screens.rs`, `README.md`, `tests/ui_keys.rs` | T3-T7; T8 when it runs | `scripts/site.sh check` passes; A-KM-1; the audit table confirmed |
| **T10 Benchmarks.** Harness entries for P-18 to P-27, P-5b and P-6c: generated archives of the P3 7.1 shapes, compressed with the system tools; `sshd -i` through a `ProxyCommand` and the latency helper for SFTP; the tuned pipelining values recorded. The M1 A-P-1..8 and P2 benches re-run. Results in `docs/perf/history.md` and here. | `benches/**`, `scripts/bench/**`, `docs/perf/history.md` | T3-T7; T8 when it runs | A-AR-8, A-QV-7, A-SF-11 recorded with measurements |
| **T11 Code review.** An independent adversarial review (grok) of the phase 3 diff, with the M1, P2 and P3 designs as the contract; focus on A-1..A-5, R-1..R-6, V-1..V-6, the process-group handling and the codec bounds. Every confirmed finding is fixed with a regression test. | as the findings require | T1-T10 | Findings table recorded here; `scripts/check.sh full` passes |
| **T12 Release.** Version, `CHANGELOG.md`, `.publish-allow.tsv` rows, the matching `v` tag, the GitHub release with the binary and its SHA-256 (P3 11). | `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, `.publish-allow.tsv` | T11 | The release workflow is green; the release page carries the tarball and checksum; the tarball's binary runs `--version` |

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
| Hostile archives exercise parsers in-process. | Scans run on worker threads under `catch_unwind`; A-AR-2 and A-AR-3 use committed hostile fixtures. A `cargo-fuzz` target on the index builder is optional after T3 if time allows. |
| The D-5 gate fails. | The own layer fits inside T4 (the research estimates about half a task more). T4 records the outcome as a decision. |
| The process-group handling of ssh is subtle. | A-SF-6 runs it in a pty. Every connect (the command line, a bookmark, history, reconnect) goes through the one connect path. |
| The development laptop has no SSH setup. | Every automated SFTP check uses `sftp-server` on pipes or `sshd -i` through a `ProxyCommand`. A-SF-12 is the owner's manual check. |
| Unicode placeholders may not render in Ghostty. | A-QV-8; direct placement outside tmux, halfblocks inside. |
| About 86 new crates. | `cargo deny` in every task that adds a crate; minimal features (P3 2.7). |
| 7z scope. | T8 is a stretch task and can be skipped. |

## Verification

`scripts/check.sh full` before every push. `scripts/check.sh bench` plus the T10 additions
before the release. Manual, by the owner, recorded here: A-QV-8 (Ghostty, foot, tmux),
A-SF-12 (a real server), and the in-terminal confirmation of `Ctrl+Q`, `Alt+Q` and `Alt+O`.

## Review record

Pending. The independent adversarial review (grok) of the design and this plan runs before
T1.

### Keymap audit (T9)

Done during the design; T9 confirms it against the final keymap.

| Source | Bound there instead | Conflict with P3 6 |
|---|---|---|
| `ghostty +list-keybinds --default` | `ctrl+shift+q` quit, `alt+1`..`alt+9` tabs, `alt+f4` close, `ctrl+page_up/down` tabs, `ctrl+alt+arrows` splits | none (`Ctrl+PgDn`, Total Commander's archive key, avoided) |
| foot `[key-bindings]` (default `foot.ini`) | `Control+Shift+*` clipboard, search and URL mode; `Control+Shift+q` only in a commented example | none |
| Omarchy Hyprland defaults | `SUPER` chords (`SUPER + CTRL + Q` is the calculator), `F9`, `Alt+Tab` variants, `Alt+Print`, `Ctrl+Alt+Delete`, media keys | none |
| tmux default root table (a server with `-f /dev/null`) | mouse bindings only; no `C-q`, `M-q` or `M-o` | none |
| Alacritty, Kitty (documented defaults; not installed) | `Ctrl+Shift+*` family; Kitty `ctrl+shift+q` closes a tab | none |

## Execution record

### Status

| Task | State | Commit | Notes |
|---|---|---|---|
| T1 | todo | -- | |
| T2 | todo | -- | |
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

Next action: the grok review of the design and this plan, then T1.

### Decisions made during execution

| # | Task | Decision | Reason |
|---|---|---|---|
