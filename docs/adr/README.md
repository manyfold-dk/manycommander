# Architecture decision records

An architecture decision record (ADR) states one decision: its context, the decision, why it
was made and what follows from it. An accepted ADR is not edited for substance; a changed
decision gets a new ADR that supersedes the old one, and the old one's status says so. The
[technical design records](../tdr/README.md) describe the resulting design.

To add one, copy the structure of an existing ADR, take the next number, and run
`generate-adr-index.sh` from the public
[estate-baseline](https://github.com/manyfold-dk/estate-baseline) repository
(`scripts/docs/generate-adr-index.sh <repo>`) to refresh the table below.

<!-- adr-index:begin -->
| ADR | Title | Status |
|---|---|---|
| 0001 | [Technical design records for the design, ADRs for the decisions](0001-design-records-and-decision-records.md) | Accepted -- 2026-10-11. |
| 0002 | [A new Linux-only file manager in Rust](0002-a-new-linux-only-file-manager-in-rust.md) | Accepted -- 2026-09-27. |
| 0003 | [Plain threads and one event channel; the UI thread makes no filesystem calls](0003-threads-and-one-event-channel.md) | Accepted -- 2026-09-27. |
| 0004 | [An own file-operation engine on directory file descriptors](0004-own-file-operation-engine-on-directory-fds.md) | Accepted -- 2026-09-27. |
| 0005 | [Filesystem identity is the pair (st_dev, mnt_id)](0005-filesystem-identity-is-dev-and-mount-id.md) | Accepted -- 2026-09-27. |
| 0006 | [An own implementation of the freedesktop.org Trash](0006-own-freedesktop-trash.md) | Accepted -- 2026-09-27. |
| 0007 | [Cross-filesystem moves copy and delete per file under group commit](0007-cross-filesystem-move-with-group-commit.md) | Accepted -- 2026-09-27. Amended 2026-10-04: batches of 1024 files (first 64, then 256). |
| 0008 | [Copies commit through an unnamed O_TMPFILE file and linkat](0008-copies-commit-through-o-tmpfile-and-linkat.md) | Accepted -- 2026-10-04. |
| 0009 | [Live theme through an inotify watcher, with a signal hook as fallback](0009-live-theme-by-watcher-with-signal-fallback.md) | Accepted -- 2026-09-27. |
| 0010 | [Local-first verification; a skipped test fails the gate](0010-local-first-verification-gate.md) | Accepted -- 2026-09-27. |
| 0011 | [A keymap that the terminals let through](0011-keymap-the-terminals-let-through.md) | Accepted -- 2026-09-27. Amended the same day: tabs are selected with `Ctrl+1` to `Ctrl+9`. |
| 0012 | [Typing filters the panel; the command line takes the focus with Ctrl+E](0012-typing-filters-the-panel.md) | Accepted -- 2026-09-30. |
| 0013 | [Hand files to the desktop with gio open; F3 opens what a pager cannot show](0013-hand-files-to-the-desktop-with-gio-open.md) | Accepted -- 2026-10-05. |
| 0014 | [Terminal signals during a hand-off belong to the child](0014-terminal-signals-during-a-hand-off-belong-to-the-child.md) | Accepted -- 2026-09-27; amended 2026-10-10. |
| 0015 | [Search results are a panel tab, and every job takes groups of single-component names](0015-search-results-are-a-panel-tab.md) | Accepted -- 2026-09-28. |
| 0016 | [An own find engine on directory file descriptors](0016-own-find-engine.md) | Accepted -- 2026-09-28. |
| 0017 | [Directory frecency follows zoxide's model, reads zoxide and never writes it](0017-frecency-reads-zoxide-never-writes-it.md) | Accepted -- 2026-09-28. |
| 0018 | [Multi-rename orders renames by dependency and breaks cycles with one temporary name](0018-multi-rename-by-dependency-order.md) | Accepted -- 2026-09-28. |
| 0019 | [Non-local places attach through a narrow provider, not a VFS](0019-non-local-places-through-a-narrow-provider.md) | Accepted -- 2026-09-28. |
| 0020 | [Archives are indexed by manycommander and extracted only through the local engine](0020-archives-indexed-and-extracted-by-the-local-engine.md) | Accepted -- 2026-09-28. |
| 0021 | [Archive decoders -- libzstd in C, pure Rust for the rest](0021-archive-decoders.md) | Accepted -- 2026-09-28. |
| 0022 | [SFTP is an own version 3 client over the system ssh](0022-sftp-client-over-the-system-ssh.md) | Accepted -- 2026-09-28. |
| 0023 | [SSH prompts go through the terminal hand-off, and ssh runs in its own process group](0023-ssh-prompts-through-the-terminal-hand-off.md) | Accepted -- 2026-09-28. |
| 0024 | [Uploads commit through hardlink@openssh.com; moves across hosts are best-effort](0024-upload-commit-and-best-effort-cross-host-moves.md) | Accepted -- 2026-09-28. |
| 0025 | [An own terminal graphics layer instead of ratatui-image](0025-own-terminal-graphics-layer.md) | Accepted -- 2026-09-28. |
| 0026 | [Kitty transmits never carry deflated zlib data](0026-kitty-transmits-without-deflated-zlib-data.md) | Accepted -- 2026-10-04. |
| 0027 | [An own box-filter downscale instead of fast_image_resize](0027-own-box-filter-downscale.md) | Accepted -- 2026-10-07. |
| 0028 | [Release artefacts, packages and the website](0028-release-artefacts-and-website.md) | Accepted -- 2026-09-28; amended 2026-09-30. |
| 0029 | [Repository rulesets, deploy environments and Renovate](0029-repository-protections-and-dependency-updates.md) | Accepted -- 2026-10-11. |
| 0030 | [The release builds without the right to publish](0030-release-in-two-jobs.md) | Accepted -- 2026-10-11. |
| 0031 | [The site's deploy tool installs from a lockfile](0031-site-tools-from-a-lockfile.md) | Accepted -- 2026-10-11. |
<!-- adr-index:end -->
