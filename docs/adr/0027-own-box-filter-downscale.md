# ADR 0027: An own box-filter downscale instead of fast_image_resize

## Status

Accepted -- 2026-10-07.

## Context

The quick view scales a picture to its pane before it encodes it for the terminal
([TDR-3 4.4](../tdr/0003-archives-quick-view-and-sftp.md#44-pipeline), step 4). With
`image`'s `thumbnail_exact` the scale took 67 to 73 ms of a 235 ms first preview of a 12 MP
photo, against the 150 ms target of P-23. The scale then moved to `fast_image_resize`'s box
filter, at about 4 to 6 ms.

A comparison of release builds later found that crate to be the largest item in the binary:
about 1.26 MB of symbols. Its `Resizer::resize` dispatches on the pixel type at run time, so
every pixel type, algorithm and SIMD variant is linked, while the preview needs four 8-bit
layouts (grey, grey with alpha, RGB, RGBA) and one filter. Its `only_u8x4` feature would
drop layouts the preview needs.

## Decision

The downscale is an own box filter in safe Rust (`scale`, `windows` and `box_mean` in
`src/preview/gfx.rs`, about 106 lines):

- `windows` finds each target pixel's source range with the same expressions as the crate,
  so both select the same source pixels.
- `box_mean` sums the source rows of a target row into one row of `u16` (a loop the compiler
  vectorises with the baseline x86-64 SSE2), then each target pixel's columns into `u32`,
  and divides once, rounded half up, with an exact multiply-and-shift reciprocal.
- A box taller than 257 rows or of 2^16 pixels or more (a downscale by more than 256 times)
  goes through `image`'s `thumbnail_exact`, as 16-bit and float images already do.

## Rationale

Measured on the reference machine:

| Measure | `fast_image_resize` | Own box filter |
|---|---|---|
| Stripped release binary | baseline | 1324 KiB smaller (15.5 percent), one package fewer |
| Scale alone, 12 MP RGB to 1000x750 | 4.09 ms | 6.67 ms |
| Scale and encode in the preview thread | 12.9 ms | 14.4 ms |
| First preview of a 12 MP photo, median of 80 | 111.2 ms | 112.6 ms |
| Accuracy against the exact mean | off by one in about 22 percent of bytes | identical |

The size gain is large and permanent; the time cost, 1.2 to 1.5 ms per 12 MP preview in the
application, is below what a user can perceive and keeps P-23 and P-24 inside their
targets. The application's fixed mmap threshold
([TDR-2 E-43](../tdr/0002-navigation-and-bulk-tools.md#appendix-a-implementation-decisions))
narrows the gap further: the crate's 9 MB intermediate image came from a fresh `mmap` on
every scale, while the own filter allocates 24 KB of row sums.

## Consequences

- The filter is manycommander's code, held by `the_box_filter_is_the_exact_mean`, which
  compares it with an exact integer mean for the four pixel layouts.
- The build has no run-time SIMD dispatch; the column sums stay scalar. Two variants were
  tried and dropped: `u32` row sums and fixed-width column loops.
- The JPEG decode, not the scale, dominates a first preview (86 to 105 ms); the scale is no
  longer a lever for P-23.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Keep `fast_image_resize` | The largest item in the binary, for one filter and four layouts |
| Its `only_u8x4` feature | Drops grey, grey with alpha and RGB |
| `image`'s `thumbnail_exact` | 67 to 73 ms per 12 MP photo |

## References

- [Downscale without `fast_image_resize`](../perf/2026-10-05-downscale-without-fast-image-resize.md):
  the full measurements
- [TDR-3 appendix A, E-32, E-34](../tdr/0003-archives-quick-view-and-sftp.md#appendix-a-implementation-decisions)
