# ADR 0002: A new Linux-only file manager in Rust

## Status

Accepted -- 2026-09-27.

## Context

Omarchy is an Arch Linux and Hyprland setup whose terminal tools follow one theme that
changes live. It had no keyboard-driven, dual-pane file manager in the Total Commander
tradition that lives in the terminal and follows that theme. The candidates each missed in
a way that could not be configured away:

- **Midnight Commander** comes closest to the workflow, but it does not reload a skin while
  it runs, so a theme switch would need a restart. Its keymap and dialog conventions differ
  from Total Commander's. Above all, the file-operation guarantees this project wants (no
  partial destination file, no lost data on a cross-filesystem move, symlinks never
  followed, see [TDR-1 4.1](../tdr/0001-core.md#41-invariants)) would have to be audited and
  held in a large foreign C codebase.
- **yazi, broot and xplr** are modern and fast, but single-pane or Miller-column designs. A
  dual-pane F-key workflow would fight their model rather than extend it.

The guarantees and the speed targets ([TDR-1 13.1](../tdr/0001-core.md#131-performance-and-responsiveness))
both rest on Linux-specific system calls: `statx` with `STATX_MNT_ID_UNIQUE`, `renameat2`
with `RENAME_NOREPLACE`, `copy_file_range`, `syncfs` with error reporting, `O_PATH`,
`O_TMPFILE` and inotify.

## Decision

- Build a new program, manycommander, rather than extend or skin an existing one.
- Write it in Rust, with `ratatui` for rendering and `crossterm` for the terminal.
- Target Linux only (NFR-PORT): a current Arch kernel (6.8 or later), x86-64 first. Use the
  Linux system calls directly, through `rustix`.
- Track the latest stable Rust. There is no minimum supported Rust version promise.
- Treat Omarchy as the primary environment: its theme file, its theme-switch sequence and
  its launch helpers. Everything else still works in any truecolor terminal.

## Rationale

- Owning the file-operation engine is the point: every invariant has acceptance tests that
  inject faults at each step ([ADR 0004](0004-own-file-operation-engine-on-directory-fds.md),
  [ADR 0010](0010-local-first-verification-gate.md)). That is only practical in a codebase
  designed around those invariants from the first commit.
- Rust gives memory safety for code that parses untrusted names, archives and protocol
  replies, and a single static binary that installs without a runtime.
- A portability layer would hide exactly the system calls the guarantees depend on. Where a
  requirement conflicts with portability, the requirement wins.
- Tracking stable Rust keeps the code on current language features and lints; the binary
  releases mean users do not need a toolchain at all.

## Consequences

- No macOS, BSD or Windows build. A port would need a second engine, not a flag.
- Building from source needs a current stable toolchain; an older `rustc` fails with a
  compiler error rather than a clear message.
- The design can rely on kernel behaviour and say so, for example that a `rename` of a
  directory carries a bind mount below it along ([TDR-1 4.8](../tdr/0001-core.md#48-move-and-rename-f6)).
- Omarchy-specific integration (the theme watcher, the Hyprland binding) is optional; outside
  Omarchy, manycommander falls back to ANSI colours and the terminal's palette
  ([TDR-1 7.1](../tdr/0001-core.md#71-palette-to-roles)).

## Alternatives considered

| Alternative | Why not |
|---|---|
| Midnight Commander with a generated truecolor skin | No live skin reload; different keymap and dialog conventions; the safety guarantees would live in a foreign C codebase |
| yazi, broot or xplr as a base | Single-pane or Miller-column models |
| A portable design (macOS, BSD) | Would give up `statx` mount IDs, `renameat2`, `O_TMPFILE` and `syncfs`, on which the invariants rest |
| A minimum supported Rust version | A maintenance cost with no user benefit while releases ship as binaries |

## References

- [TDR-1 1 Outcome](../tdr/0001-core.md#1-outcome) and [TDR-1 10](../tdr/0001-core.md#10-alternatives-considered)
- [TDR-1 13.2](../tdr/0001-core.md#132-other-requirements), NFR-PORT
