# Downscale without `fast_image_resize`

Question (owner, 2026-10-05): does replacing `fast_image_resize` pay off in size, in time, or
both?

Answer: in size, yes; in time, no. Replacing the crate with a box filter of 106 lines of
code (126 with comments) shrinks the stripped release binary by 1325 KiB (15.5 percent, back below
0.3's size) and drops one dependency. The scale itself becomes slower: +2.5 ms for a 12 MP
photo in isolation, +1.2 to +1.5 ms with the application's allocator setting, which is what
P-23 measured in the preview thread (about 1 percent of a first preview of 111 ms). No P-23
or P-24 figure leaves its target. The replacement is also exact, where the crate is off by
one in a fifth of the bytes. Recommendation: take it; the size gain is large and permanent,
the time cost is below what a user can perceive. Landing it supersedes E-32 of the phase 3
plan, the only document that names the crate.

Outcome (owner, 2026-10-07): taken. The replacement landed in `src/preview/gfx.rs` as in the
appendix (b862cee), with `the_box_filter_is_the_exact_mean`, which compares it with an exact
mean in integers for the four pixel layouts; E-34 of the phase 3 plan records it. The stripped
release binary is 7198 KiB against the shipped 0.6's 8522 KiB, and `Cargo.lock` holds 322
packages. A P-23 kitty A/B of the shipped 0.6 binary against this build (two alternating rounds
of ten sessions, 1-minute load below 1.5) ran on battery, outside the reference conditions:
decode took 135 ms instead of about 86, so neither binary meets 150 ms there. Scale and encode
took 15.9 to 16.0 ms against 14.1 to 14.3 ms, the first-preview median 1.4 to 1.8 ms more, as
session 2 above measured.

## Background

The quick view scales a picture to its pane before it encodes it for the terminal
(`scale` in `src/preview/gfx.rs`, P3 4.4 step 4). Phase 3 T10 (E-32, f441983) moved that
scale from `image`'s `thumbnail_exact` (67 to 73 ms for a 12 MP photo, P-23) to
`fast_image_resize`'s SIMD convolution with its box filter (about 6 ms). The
[release comparison](2026-10-05-release-comparison.md) found that crate to be the largest
item in the binary: about 1.26 MB of symbols, for four 8-bit pixel layouts (grey, grey and
alpha, RGB, RGBA) and one filter. `Resizer::resize` dispatches on the pixel type at run
time, so every pixel type, algorithm and SIMD variant is linked; its `only_u8x4` feature
would drop the layouts the preview needs.

## The replacement

`fast_image_resize`'s box filter is a mean with equal weights: a target pixel averages the
source pixels whose centres fall in its window, one target pixel wide and at least one
source pixel (`convolution/mod.rs`, `precompute_coefficients`; `filters.rs`, `box_filter`).
The replacement computes the same mean in safe Rust, in 126 lines with comments
(appendix):

- `windows` finds each target pixel's source range with the crate's own expressions (the
  window's centre, its half-open bounds, the reciprocal of its width), so both select the
  same pixels.
- `box_mean::<CH>` sums the source rows of a target row into one row of `u16` (a loop the
  compiler vectorises with the baseline x86-64 SSE2), then each target pixel's columns into
  `u32`, and divides once, rounded half up, with an exact multiply-and-shift reciprocal
  (`ceil(2^40 / n)`, exact for boxes below 2^16 pixels).
- A box taller than 257 rows (a `u16` holds 257 times 255) or of 2^16 pixels or more, a
  downscale by more than 256 times, goes through `image`'s `thumbnail_exact`, as 16-bit and
  float images already do.

`fast_image_resize` rounds twice: its horizontal pass writes an image of the same 8-bit pixel
type, which its vertical pass reads (`resizer.rs`, `resample_convolution`), and its weights
are fixed-point. It is off by one in a fifth of the bytes; the replacement divides once and
matches the exact mean.

The experiment lived in a scratch copy of the repository; the code is in `src/` since the outcome above. Beside
`scale`, the change removes the dependency from `Cargo.toml` (and so from `Cargo.lock`).

## Results

### Size

Release builds (`cargo build --release --locked`) of the 0.6 source, with and without the
replacement, stripped:

| Build | Stripped binary | `.text` | `.eh_frame` | Packages in `Cargo.lock` |
|---|---|---|---|---|
| 0.6 | 8523 KiB | 6609 KiB | 467 KiB | 323 |
| 0.6 with the replacement | 7199 KiB | 5347 KiB | 417 KiB | 322 |
| Difference | -1325 KiB (-15.5 percent) | -1262 KiB | -50 KiB | -1 |

The binary falls back below 0.3's 7981 KiB. The tarball shrinks in proportion (not
measured).

### Time of the scale alone

Medians of 31 alternating runs on noise images (the time does not depend on the pixel
values); `thumbnail_exact` as the pre-T10 reference.

| Image and target | `fast_image_resize` | Replacement | `thumbnail_exact` |
|---|---|---|---|
| 12 MP RGB, 4000x3000 to 1000x750 (P-23's fit) | 4.09 ms | 6.67 ms | 71.9 ms |
| 12 MP RGB to 1003x752 (ratio not whole) | 4.21 ms | 6.71 ms | 73.7 ms |
| 12 MP RGB to 100x75 (halfblocks) | 1.84 ms | 2.92 ms | 14.1 ms |
| 4K RGBA, 3840x2160 to 1000x562 | 2.99 ms | 6.18 ms | 24.3 ms |
| 12 MP grey to 1000x750 | 3.09 ms | 3.42 ms | 27.5 ms |
| 1080p RGB, 1920x1080 to 1000x562 | 1.60 ms | 3.07 ms | 32.2 ms |

The replacement takes 1.1 to 2.1 times as long: 2.6 ms more for a 12 MP photo. Its row
sums (2.7 ms for the 12 MP photo) vectorise; the column sums per target pixel (about 3.5 ms)
stay scalar. `fast_image_resize` selects AVX2 at run time; the release build targets the
x86-64 baseline (SSE2), and there is no `target-cpu` setting. Two variants were tried and
dropped: `u32` row sums (5.4 ms for the rows alone) and fixed-width column loops for widths
2 to 5 (no gain).

### Accuracy

| Image and target | `fast_image_resize` against the exact mean | Replacement against the exact mean | Replacement against `fast_image_resize` |
|---|---|---|---|
| 12 MP RGB to 1000x750 | max 1, 22.1 % of bytes differ | identical | max 1, 22.1 % |
| 12 MP RGB to 1003x752 | max 36, 22.1 % | max 36, 0.26 % | max 1, 21.9 % |
| 12 MP RGB to 100x75 | max 1, 4.0 % | identical | max 1, 4.0 % |
| 4K RGBA to 1000x562 | max 1, 19.1 % | identical | max 1, 19.1 % |
| 12 MP grey to 1000x750 | max 1, 22.0 % | identical | max 1, 22.0 % |
| 1080p RGB to 1000x562 | max 1, 31.8 % | identical | max 1, 31.8 % |

In the 1003x752 case some target rows have a source pixel centre exactly on their window's
bound; floating-point rounding decides which window takes it. The replacement evaluates the
crate's expression and decides as the crate does (max 1 against it); the independent
reference formula decides the other way for those rows (max 36, 0.26 % of bytes).

### In the application: P-23 and P-24

Two sessions, each with its binaries alternating, on the P-23 fixture (12 MP camera-like
JPEGs, kitty unless noted) and the P-24 burst. Session 1 (21:26): the 0.6 source and the
replacement, both built as shipped, two batches of five sessions each, per protocol. Session 2
(22:09, a quieter machine): the shipped binaries of the 0.4 patch release and of 0.6 from
GitHub and the replacement, two rounds of ten sessions each (80 first previews per binary).
Each cell gives the batches or rounds.

| Check | 0.4 patch (shipped) | 0.6 | 0.6 with the replacement | Target |
|---|---|---|---|---|
| Session 1: first preview median | | 122.8, 123.9 ms | 124.7, 122.0 ms | |
| Session 1: first preview max | | 152.5, 138.7 ms | 129.0, 138.7 ms | 150 |
| Session 1: scale and encode (preview thread) | | 17.1, 17.5 ms | 17.0, 16.6 ms | |
| Session 1: sixel median, max | | 158.4 / 156.1, 181.5 / 170.8 ms | 157.7 / 156.8, 161.0 / 162.8 ms | 200 |
| Session 1: halfblocks scale and encode | | 5.0, 4.9 ms | 5.7, 5.6 ms | |
| Session 1: P-24 key-to-flush p99, max | | 1.28, 2.33 ms | 1.16, 1.31 ms | 16 |
| Session 1: P-24 transmitting frame median, max | | 6.9, 9.9 ms | 6.9, 9.1 ms | 50 |
| Session 2: first preview median (80) | 111.2 ms | 111.2 ms | 112.6 ms | |
| Session 2: first preview max per round | 136.7, 126.0 ms | 134.9, 127.8 ms | 122.1, 121.2 ms | 150 |
| Session 2: first previews above 140 ms | 0 of 80 | 0 of 80 | 0 of 80 | |
| Session 2: decode | 87.2, 86.0 ms | 87.0, 86.7 ms | 86.3, 86.4 ms | |
| Session 2: scale and encode | 12.9, 12.9 ms | 12.9, 13.1 ms | 14.4, 14.5 ms | |

The decode (86 to 96 ms, depending on the session) dominates a first preview. Session 2
shows the replacement's cost: +1.5 ms of scale and encode, +1.4 ms on the median first
preview. Session 1, on a machine that decoded about 10 percent slower, showed none; its
0.6 kitty maximum of 152.5 ms is one first preview of twenty, the other nineteen at or below
130.6 ms. The shipped 0.4 patch release and 0.6 measure the same, so the release did not
change P-23 (see the 0.6 row of [`history.md`](history.md)).

### Why the application sees less than the micro-benchmark

The application fixes glibc's mmap threshold at 128 KiB (`fsops::sys::tune_allocator`, P-6).
`fast_image_resize` writes its horizontal pass into a temporary image of 1000 x 3000 pixels
(9 MB for RGB), which then comes from a fresh `mmap` on every scale, with its page faults;
the replacement allocates 24 KB of row sums and the 2.25 MB result. Decoding the P-23 photos
and scaling each at once, as the preview thread does (medians of 20 per variant, two runs
per setting):

| Allocator | `fast_image_resize` | Replacement | Difference |
|---|---|---|---|
| glibc's default (dynamic threshold) | 4.28, 4.20 ms | 6.76, 6.87 ms | +2.5 ms |
| The application's setting (`MC_TUNE=1` in the micro-benchmark) | 5.58, 6.18 ms | 6.91, 7.42 ms | +1.2 to +1.3 ms |

## Method

- Micro-benchmark: a standalone crate with the release profile (thin LTO, one codegen
  unit, the default x86-64 target) that includes the old `scale` verbatim from the 0.6
  source and the replacement verbatim from the experiment, and times both on the same
  noise images in alternating runs (median of 31 after 3 warm-ups), with `thumbnail_exact`
  (median of 5) for reference. Accuracy: both against an exact mean computed independently
  (each source pixel whose centre lies in the window, summed in `u64`).
- Size: release builds of the 0.6 source and of the same source with the replacement and
  without the dependency.
- P-23 and P-24: the 0.6 driver's `p3-preview` and `p3-burst` (with the harness fix
  5c701ec's driver in session 2), the binaries alternating as described above.
- Decode then scale: the micro-benchmark's `photo` mode decodes two P-23 photos with `image`
  and scales each at once to 1000 pixels wide, twelve rounds alternating the order, the
  first two rounds discarded; `MC_TUNE=1` applies the application's `mallopt` first.

## Appendix: the replacement

In place of `scale` in `src/preview/gfx.rs` of 0.6; `check.sh quick` passed with it (fmt, clippy
with all targets and features, 184 unit tests, the existing `scale` test among them).

```rust
/// `img` scaled to `w` x `h` pixels (P3 4.4, step 4) with a box filter: each target pixel
/// is the mean of the source pixels whose centres fall in its window, the average
/// `image`'s `thumbnail` makes, but as row sums and then column sums ([`box_mean`]; P-23:
/// a 12 MP photo to 1000 x 750 in a few milliseconds instead of 70). Alpha is averaged like
/// the colours, not premultiplied, as the thumbnail does it. The colour type is kept. A
/// 16-bit or float image, an empty size and a downscale by more than 256 times go through
/// `image`'s thumbnail.
pub fn scale(img: &DynamicImage, w: u32, h: u32) -> DynamicImage {
    use image::ImageBuffer;
    let (sw, sh, tw, th) = (
        img.width() as usize,
        img.height() as usize,
        w as usize,
        h as usize,
    );
    let src = img.as_bytes();
    let boxed = || {
        if sw == 0 || sh == 0 || tw == 0 || th == 0 {
            return None;
        }
        Some(match img {
            DynamicImage::ImageLuma8(_) => DynamicImage::ImageLuma8(ImageBuffer::from_raw(
                w,
                h,
                box_mean::<1>(src, sw, sh, tw, th)?,
            )?),
            DynamicImage::ImageLumaA8(_) => DynamicImage::ImageLumaA8(ImageBuffer::from_raw(
                w,
                h,
                box_mean::<2>(src, sw, sh, tw, th)?,
            )?),
            DynamicImage::ImageRgb8(_) => DynamicImage::ImageRgb8(ImageBuffer::from_raw(
                w,
                h,
                box_mean::<3>(src, sw, sh, tw, th)?,
            )?),
            DynamicImage::ImageRgba8(_) => DynamicImage::ImageRgba8(ImageBuffer::from_raw(
                w,
                h,
                box_mean::<4>(src, sw, sh, tw, th)?,
            )?),
            _ => return None,
        })
    };
    boxed().unwrap_or_else(|| img.thumbnail_exact(w, h))
}

/// The source pixels `start..end` of each of `out` target pixels along an axis of `len`
/// source pixels: those whose centres fall in the target pixel's window, which is one
/// target pixel wide and at least one source pixel. The window and its half-open bounds
/// are `fast_image_resize`'s box filter, which [`scale`] used before.
fn windows(len: usize, out: usize) -> Vec<(usize, usize)> {
    let ratio = len as f64 / out as f64;
    let width = ratio.max(1.0);
    let (radius, recip) = (0.5 * width, 1.0 / width);
    (0..out)
        .map(|o| {
            let middle = (o as f64 + 0.5) * ratio;
            let centre = middle - 0.5;
            let inside = |x: usize| {
                let t = (x as f64 - centre) * recip;
                t > -0.5 && t <= 0.5
            };
            let lo = (middle - radius).floor().max(0.0) as usize;
            let hi = ((middle + radius).ceil() as usize).min(len);
            let start = (lo..hi).find(|&x| inside(x)).unwrap_or(lo.min(len - 1));
            let end = (start..hi)
                .take_while(|&x| inside(x))
                .last()
                .map_or(start + 1, |x| x + 1);
            (start, end)
        })
        .collect()
}

/// [`scale`]'s box filter over `CH` 8-bit channels: the source rows of a target row are
/// summed into one row of `u16` (a loop the compiler vectorises), then the columns of each
/// target pixel, divided once and rounded half up. `None` for a box taller than 257 rows (a
/// `u16` holds 257 times 255) or of 2^16 pixels or more: a downscale by more than 256 times,
/// which goes through `image`'s thumbnail instead.
fn box_mean<const CH: usize>(
    src: &[u8],
    sw: usize,
    sh: usize,
    tw: usize,
    th: usize,
) -> Option<Vec<u8>> {
    let (cols, rows) = (windows(sw, tw), windows(sh, th));
    let span = |v: &[(usize, usize)]| v.iter().map(|&(a, b)| b - a).max().unwrap_or(1);
    let (widest, tallest) = (span(&cols), span(&rows));
    if tallest > 257 || widest * tallest >= 1 << 16 {
        return None;
    }
    let stride = sw * CH;
    let mut acc = vec![0u16; stride];
    // ceil(2^40 / n) for a box of n < 2^16 pixels: (x m) >> 40 is x / n for x <= 256 n, as
    // its error, below 256 n / 2^40, stays under 1 / n.
    let mut recip = vec![0u64; widest + 1];
    let mut out = vec![0u8; tw * th * CH];
    for (&(y0, y1), line) in rows.iter().zip(out.chunks_exact_mut(tw * CH)) {
        acc.fill(0);
        for row in src[y0 * stride..y1 * stride].chunks_exact(stride) {
            for (a, &v) in acc.iter_mut().zip(row) {
                *a += u16::from(v);
            }
        }
        let tall = y1 - y0;
        for (k, r) in recip.iter_mut().enumerate().skip(1) {
            *r = (1u64 << 40).div_ceil((tall * k) as u64);
        }
        let pixels = acc.as_chunks::<CH>().0;
        for (&(x0, x1), px) in cols.iter().zip(line.as_chunks_mut::<CH>().0) {
            let mut sum = [0u32; CH];
            for p in &pixels[x0..x1] {
                for c in 0..CH {
                    sum[c] += u32::from(p[c]);
                }
            }
            let (half, m) = ((tall * (x1 - x0) / 2) as u32, recip[x1 - x0]);
            for c in 0..CH {
                px[c] = ((u64::from(sum[c] + half) * m) >> 40) as u8;
            }
        }
    }
    Some(out)
}
```
