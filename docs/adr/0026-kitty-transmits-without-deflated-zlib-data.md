# ADR 0026: Kitty transmits never carry deflated zlib data

## Status

Accepted -- 2026-10-04.

## Context

The kitty graphics protocol lets a transmit carry its pixels as a zlib stream (`o=z`). The
first quick view compressed every transmit at deflate level 1, to keep the frame that
transmits a 12 MP preview small.

The manual check of the quick view in Ghostty (A-QV-8) failed at the first picture in five
runs of five: Ghostty crashed, taking every window and tab of the terminal process with it.
The same happened through tmux's passthrough. The cause is upstream: Ghostty releases built
with Zig 0.15 inflate `o=z` with a decoder that crashes when a fixed-Huffman block holds a
match across the decoder's 64 KiB window. Deflate level 1 writes only fixed-Huffman blocks,
so any compressible picture larger than 64 KiB was likely to trigger it
(ghostty-org/ghostty discussion 14238). Ghostty's main branch has since moved to another
decoder, but released builds stay installed for a long time.

## Decision

No transmit carries deflated zlib data:

- Pixels whose samples shrink by at least 5 percent at deflate level 1 go as a PNG
  (`f=100`, fast deflate, adaptive filters). Ghostty and Kitty decode PNG with a different
  decoder.
- Pixels that do not compress go as a zlib stream of stored blocks (`o=z`), which the faulty
  decoder handles.

## Rationale

- The rule needs no terminal detection. Environment variables never choose a protocol
  (V-6), and a version check would miss builds made from other sources.
- It costs nothing measurable. The PNG is no larger than the level-1 stream and takes about
  1 ms per megapixel. The noise of a camera photo does not compress at pane size (2,250,000
  bytes became 2,244,970 in 23 ms), so stored blocks lose nothing on the pictures that
  dominate P-23.
- Kitty and every other terminal that implements the protocol decode both forms.

## Consequences

- P-23 and P-24 did not change.
- With the rule in place, the same manual check passed in Ghostty: sharp pictures, an
  EXIF-rotated photo upright, an animated GIF's first frame, a 12 MP photo in stored blocks,
  and unicode placeholders through tmux.
- `tests/manual.rs` plays the quick view's own sequences for a PNG, a stored-blocks and an
  RGBA picture into real Ghostty and foot in a nested Hyprland session, and checks that each
  terminal stays alive and shows each picture. Against the level-1 transmit it fails on the
  crash.
- The rule stays even after the faulty builds are gone: it has no cost, and dropping it
  would bring back a crash that takes the user's other windows with it.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Keep level 1 and wait for fixed Ghostty releases | A crash of the terminal loses the user's other work; old builds stay installed |
| Detect Ghostty and its version | Unreliable through tmux and under other names; environment variables never choose a protocol (V-6) |
| Deflate at a higher level with dynamic Huffman blocks | Relies on the faulty decoder handling those blocks, and costs encode time on pictures that do not compress |

## References

- [TDR-3 4.4](../tdr/0003-archives-quick-view-and-sftp.md#44-pipeline)
- [TDR-3 appendix A, E-32](../tdr/0003-archives-quick-view-and-sftp.md#appendix-a-implementation-decisions)
- [ADR 0025](0025-own-terminal-graphics-layer.md)
