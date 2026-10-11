# ADR 0025: An own terminal graphics layer instead of ratatui-image

## Status

Accepted -- 2026-09-28.

## Context

The quick view shows the picture under the cursor next to the file list. Terminals draw
pictures in three ways: the kitty graphics protocol (Ghostty, Kitty), sixel (foot), and
halfblocks (`▀` cells with 24-bit colours) everywhere else. Inside tmux, kitty graphics need
tmux's passthrough, which the user's tmux configuration may allow or not.

One rule constrains every option (V-3): manycommander never changes the user's terminal or
tmux configuration. It may place and delete its own images, nothing more.

`ratatui-image` is the obvious crate. Every one of its `Picker` constructors,
`Picker::halfblocks` included, runs `tmux set -p allow-passthrough on` when `TERM` starts
with `tmux` or `TERM_PROGRAM` is `tmux`, and no constructor skips the call.

## Decision

manycommander has its own graphics layer in `src/preview/`:

- **A startup probe** replaces crossterm's `supports_keyboard_enhancement()`, which waits
  up to 2 s for a reply. It writes one batch of queries (a kitty graphics query, `CSI 16 t`
  for the cell size, `CSI ? u` for the keyboard protocol, DA1 last) and reads with a 100 ms
  deadline, before the input thread starts (V-6). Keys typed in that window are discarded.
- **Kitty graphics** in own code: one transmit per image, direct placement at the pane's
  cells outside tmux, a virtual placement with unicode placeholders inside tmux, at most 8
  stored images, and deletes before every hand-off and on exit.
- **Halfblocks** in own code; **sixel** encoded by `icy_sixel`.
- Decoding with `image` (pure-Rust decoders, default features off) under the bounds of V-2,
  on the preview thread.

The layer has no `tmux` code path. `preview.protocol` in `config.toml` overrides the probe.

## Rationale

- `ratatui-image` breaks V-3 in every constructor. Clearing `TERM` or `TERM_PROGRAM` would
  change the process environment for every child, and a patched fork is not possible,
  because `deny.toml` refuses git sources.
- The own layer is small and fits the existing threads: the UI thread only draws (V-1), the
  preview thread reads, decodes, scales and encodes.
- It meets the targets on the reference machine: a camera-like 12 MP JPEG on screen in at
  most 148.7 ms with kitty graphics, 131.3 ms with halfblocks and 182.0 ms with sixel
  (P-23: 150, 150 and 200 ms); key-to-frame p99 1.47 ms while browsing 200 images (P-24);
  the probe costs 4.0 to 4.2 ms on terminals that answer and 103.6 ms on one that does not
  (P-25).
- It brings fewer crates than the about 56 that `ratatui-image` would have added (an
  estimate).

## Consequences

- Terminal quirks are manycommander's to handle. Ghostty releases built with Zig 0.15 crash
  on some compressed transmits
  ([ADR 0026](0026-kitty-transmits-without-deflated-zlib-data.md)); terminals differ on
  whether a screen clear removes kitty images, so every clear deletes them and transmits the
  shown one again.
- Inside tmux the DA1 reply is tmux's own, so the automatic choice never takes sixel there;
  `preview.protocol = "sixel"` does.
- A-QV-5 guards V-3: with `TERM=tmux-256color`, `TERM_PROGRAM=tmux` and `TMUX` set, and a
  recording `tmux` stub first on `PATH`, a whole quick-view session calls `tmux` zero times.
- A-QV-8 checks real Ghostty and foot by hand, and `tests/manual.rs` plays the layer's own
  sequences into both terminals.

## Alternatives considered

| Alternative | Why not |
|---|---|
| `ratatui-image` | Runs `tmux set -p allow-passthrough on` (V-3); about 56 crates |
| `viuer` | Writes to stdout outside ratatui's buffer |
| chafa | C |
| Überzug++ | An external overlay process |
| Probing on the first `Ctrl+Q` | Needs the input thread parked mid-session, and a key typed meanwhile is lost |

## References

- [TDR-3 4](../tdr/0003-archives-quick-view-and-sftp.md#4-image-quick-view), V-1 to V-6 in
  [TDR-3 1.3](../tdr/0003-archives-quick-view-and-sftp.md#13-invariants)
- [TDR-3 appendix A, E-13 to E-15](../tdr/0003-archives-quick-view-and-sftp.md#appendix-a-implementation-decisions)
