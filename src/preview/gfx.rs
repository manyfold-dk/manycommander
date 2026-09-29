#![forbid(unsafe_code)]
//! The graphics layer (P3 4.7, D-5): one interface behind which the protocol can change.
//! [`prepare`] fits an image into the pane and encodes it on the preview thread; [`draw`]
//! puts it into the frame's cells; [`Screen`] tracks what the terminal holds and writes the
//! escape sequences around each frame, and [`Screen::forget`] deletes an image.
//!
//! Kitty graphics (P3 4.3): an image is transmitted once, compressed (`o=z`), in chunks of
//! 4096 base64 bytes, with `q=2` so the terminal never answers. The zlib stream is deflated
//! at level 1, unless samples of the pixels shrink by less than 5 percent at that level
//! (`compresses`): the noise of a camera photo at pane size does not compress (P-23:
//! 2,250,000 bytes to 2,244,970 in 23 ms), so its stream holds stored blocks, as large and
//! made in about 1 ms, and is still `o=z`. Outside tmux the image is placed directly at the
//! pane's cell position; inside tmux the transmit goes through tmux's passthrough with a
//! virtual placement (`U=1`), and the pane's cells hold unicode placeholders that tmux
//! draws like any text. The terminal stores at most [`MAX_STORED`] images; a replaced
//! image's placement is deleted at once, and the least recently shown image beyond the
//! bound is deleted with its data (`d=I`). Sixel is encoded by `icy_sixel` once per image
//! and size, and sent again only when its area is redrawn. Halfblocks are `▀` cells whose
//! colours come from the image. There is no `tmux` code path: the layer reads the terminal
//! only in the startup probe, and it changes no terminal or tmux setting (V-3).

use super::probe::passthrough;
use super::{Pane, Protocol};
use image::DynamicImage;
use image::metadata::Orientation;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::io::Write;
use std::sync::atomic::{AtomicU32, Ordering};

/// The terminal stores at most this many images (P3 2.6).
pub const MAX_STORED: usize = 8;

/// The kitty placeholder character (P3 4.3).
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// Base64 bytes per kitty chunk.
const CHUNK: usize = 4096;

/// The row and column diacritics of kitty's unicode placeholders (the protocol's
/// `rowcolumn-diacritics` table): the diacritic at index `n` encodes row or column `n`.
const DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1,
    0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6, 0x06D7, 0x06D8,
    0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB,
    0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954,
    0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76,
    0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71,
    0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4,
    0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1,
    0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7,
    0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2,
    0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE,
    0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1,
    0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED,
    0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1,
    0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186,
    0x1D187, 0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

fn diacritic(n: u16) -> char {
    DIACRITICS
        .get(n as usize)
        .and_then(|&c| char::from_u32(c))
        .unwrap_or('\u{0305}')
}

/// The largest placeholder grid: a row or column beyond the table cannot be encoded.
pub const MAX_PLACEHOLDER_CELLS: u16 = DIACRITICS.len() as u16;

static NEXT_ID: AtomicU32 = AtomicU32::new(0);

/// A fresh kitty image id: 24 bits, never 0, from a random start, so another program in the
/// same terminal (another tmux pane) is unlikely to use it. The placeholders carry it as
/// their 24-bit foreground colour.
pub fn next_id() -> u32 {
    if NEXT_ID.load(Ordering::Relaxed) == 0 {
        let seed = (crate::fsops::sys::random_u64() as u32 & 0x00ff_ffff).max(1);
        let _ = NEXT_ID.compare_exchange(0, seed, Ordering::Relaxed, Ordering::Relaxed);
    }
    loop {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed) & 0x00ff_ffff;
        if id != 0 {
            return id;
        }
    }
}

/// One halfblock cell: the upper and lower pixel, `None` where the image is transparent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Half {
    pub top: Option<[u8; 3]>,
    pub bottom: Option<[u8; 3]>,
}

/// An image encoded for one protocol.
#[derive(Debug)]
pub enum Body {
    /// The transmit sequence (`a=t`), placed after the frame.
    Kitty { transmit: Vec<u8> },
    /// The transmit with its virtual placement (`a=T,U=1`), wrapped for tmux; the cells hold
    /// placeholders.
    KittyTmux { transmit: Vec<u8> },
    /// The sixel sequence, sent after the frame.
    Sixel { data: Vec<u8> },
    /// One cell per entry, row by row.
    Halfblocks { cells: Vec<Half> },
}

/// A prepared image (P3 4.7): made on the preview thread, drawn by the UI thread.
#[derive(Debug)]
pub struct Prepared {
    /// The kitty image id; unique per prepared image.
    pub id: u32,
    /// The pane it was prepared for, in cells.
    pub pane: (u16, u16),
    /// The cells it covers, at most the pane.
    pub cells: (u16, u16),
    /// Its pixel size as encoded.
    pub px: (u32, u32),
    pub body: Body,
}

impl Prepared {
    /// The memory it holds, for the cache bound (P3 2.6).
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Prepared>()
            + match &self.body {
                Body::Kitty { transmit } | Body::KittyTmux { transmit } => transmit.len(),
                Body::Sixel { data } => data.len(),
                Body::Halfblocks { cells } => cells.len() * std::mem::size_of::<Half>(),
            }
    }

    pub fn protocol(&self) -> Protocol {
        match self.body {
            Body::Kitty { .. } => Protocol::Kitty,
            Body::KittyTmux { .. } => Protocol::KittyTmux,
            Body::Sixel { .. } => Protocol::Sixel,
            Body::Halfblocks { .. } => Protocol::Halfblocks,
        }
    }
}

/// Where an image of `w` x `h` pixels goes in `pane` (P3 4.4, step 4): fitted into the pane's
/// pixel box without upscaling. Returns the target pixel size and the cells it covers. For
/// halfblocks the target counts half cells: one pixel per column and two per row. Without a
/// probed cell size, cells are taken as twice as tall as wide.
pub fn fit(w: u32, h: u32, pane: Pane, halfblocks: bool) -> ((u32, u32), (u16, u16)) {
    let (cw, ch) = pane.cell.map_or((1, 2), |(w, h)| (w as u32, h as u32));
    let (cw, ch) = (cw.max(1), ch.max(1));
    let box_w = pane.cols as u64 * cw as u64;
    let box_h = pane.rows as u64 * ch as u64;
    let (w64, h64) = (w.max(1) as u64, h.max(1) as u64);
    // The scale is min(1, box_w / w, box_h / h), kept in integers.
    let (tw, th) = if w64 <= box_w && h64 <= box_h {
        (w64, h64)
    } else if box_w * h64 <= box_h * w64 {
        (box_w, (h64 * box_w / w64).max(1))
    } else {
        ((w64 * box_h / h64).max(1), box_h)
    };
    let cols = tw.div_ceil(cw as u64).clamp(1, pane.cols.max(1) as u64) as u16;
    let rows = th.div_ceil(ch as u64).clamp(1, pane.rows.max(1) as u64) as u16;
    if halfblocks {
        // A half cell is cw x ch/2 pixels.
        let hw = (tw * 2)
            .div_ceil(2 * cw as u64)
            .clamp(1, pane.cols.max(1) as u64);
        let hh = (th * 2)
            .div_ceil(ch as u64)
            .clamp(1, 2 * pane.rows.max(1) as u64);
        return ((hw as u32, hh as u32), (hw as u16, hh.div_ceil(2) as u16));
    }
    ((tw as u32, th as u32), (cols, rows))
}

/// Encodes `bytes` as standard base64 with padding.
pub fn base64(bytes: &[u8]) -> Vec<u8> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(bytes.len().div_ceil(3) * 4);
    let (whole, rest) = bytes.as_chunks::<3>();
    for c in whole {
        let n = (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32;
        out.extend_from_slice(&[
            T[(n >> 18) as usize & 63],
            T[(n >> 12) as usize & 63],
            T[(n >> 6) as usize & 63],
            T[n as usize & 63],
        ]);
    }
    match *rest {
        [a] => {
            let n = (a as u32) << 16;
            out.extend_from_slice(&[T[(n >> 18) as usize & 63], T[(n >> 12) as usize & 63]]);
            out.extend_from_slice(b"==");
        }
        [a, b] => {
            let n = (a as u32) << 16 | (b as u32) << 8;
            out.extend_from_slice(&[
                T[(n >> 18) as usize & 63],
                T[(n >> 12) as usize & 63],
                T[(n >> 6) as usize & 63],
            ]);
            out.push(b'=');
        }
        _ => {}
    }
    out
}

/// The compressibility check takes this many samples of the pixels ...
const SAMPLES: usize = 4;
/// ... of this many bytes each, spread evenly from the first byte to the last.
const SAMPLE: usize = 16 << 10;

/// Whether deflating `raw` at level 1 pays: its samples shrink by at least 5 percent. Data
/// no larger than the samples together is always deflated.
fn compresses(raw: &[u8]) -> bool {
    if raw.len() <= SAMPLES * SAMPLE {
        return true;
    }
    let step = (raw.len() - SAMPLE) / (SAMPLES - 1);
    let mut enc = flate2::write::DeflateEncoder::new(
        Vec::with_capacity(SAMPLES * SAMPLE),
        flate2::Compression::fast(),
    );
    for k in 0..SAMPLES {
        // Writing into a Vec does not fail.
        let _ = enc.write_all(&raw[k * step..k * step + SAMPLE]);
    }
    let packed = enc.finish().map_or(usize::MAX, |v| v.len());
    packed.saturating_mul(100) < SAMPLES * SAMPLE * 95
}

/// `raw` as a zlib stream: deflated at level 1, or in stored blocks when that does not pay
/// ([`compresses`]).
fn zlib(raw: &[u8]) -> Vec<u8> {
    let (level, room) = if compresses(raw) {
        (flate2::Compression::fast(), raw.len() / 2)
    } else {
        // A stored block holds at most 64 KiB behind a 5-byte header.
        (
            flate2::Compression::none(),
            raw.len() + raw.len() / 8192 + 64,
        )
    };
    let mut enc = flate2::write::ZlibEncoder::new(Vec::with_capacity(room), level);
    // Writing into a Vec does not fail.
    let _ = enc.write_all(raw);
    enc.finish().unwrap_or_default()
}

/// The kitty transmit of pixels `raw` in format `f` (24 RGB, 32 RGBA): compressed, chunked,
/// silent (`q=2`). Inside tmux (`virtual_cells`) it also makes the virtual placement of
/// `cols` x `rows` cells and every chunk goes through the passthrough.
fn kitty_transmit(
    raw: &[u8],
    f: u8,
    px: (u32, u32),
    id: u32,
    virtual_cells: Option<(u16, u16)>,
) -> Vec<u8> {
    let data = base64(&zlib(raw));
    let n = data.len().div_ceil(CHUNK).max(1);
    let mut out = Vec::with_capacity(data.len() + n * 64);
    let mut seq = Vec::with_capacity(CHUNK + 96);
    for (i, chunk) in data
        .chunks(CHUNK)
        .chain(data.is_empty().then_some(&b""[..]))
        .enumerate()
    {
        seq.clear();
        seq.extend_from_slice(b"\x1b_G");
        if i == 0 {
            match virtual_cells {
                Some((c, r)) => {
                    let _ = write!(seq, "a=T,U=1,c={c},r={r},");
                }
                None => seq.extend_from_slice(b"a=t,"),
            }
            let _ = write!(seq, "f={f},o=z,t=d,i={id},s={},v={},", px.0, px.1);
        }
        let _ = write!(seq, "q=2,m={};", u8::from(i + 1 < n));
        seq.extend_from_slice(chunk);
        seq.extend_from_slice(b"\x1b\\");
        if virtual_cells.is_some() {
            passthrough(&seq, &mut out);
        } else {
            out.extend_from_slice(&seq);
        }
    }
    out
}

/// Whether an EXIF orientation swaps width and height.
pub fn swaps(o: Orientation) -> bool {
    matches!(
        o,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    )
}

/// `img` scaled to `w` x `h` pixels (P3 4.4, step 4) with a box filter: each target pixel
/// is the average of the source pixels it covers, as `image`'s `thumbnail` makes it, but
/// with `fast_image_resize`'s SIMD convolution (P-23: a 12 MP photo to 1000 x 750 in about
/// 6 ms instead of 70). Alpha is averaged like the colours, not premultiplied, as the
/// thumbnail does it. The colour type is kept. A 16-bit or float image, and anything the
/// resizer refuses, goes through `image`'s thumbnail.
pub fn scale(img: &DynamicImage, w: u32, h: u32) -> DynamicImage {
    use fast_image_resize::images::{Image, ImageRef};
    use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
    use image::ImageBuffer;
    let pixel = match img {
        DynamicImage::ImageLuma8(_) => PixelType::U8,
        DynamicImage::ImageLumaA8(_) => PixelType::U8x2,
        DynamicImage::ImageRgb8(_) => PixelType::U8x3,
        DynamicImage::ImageRgba8(_) => PixelType::U8x4,
        _ => return img.thumbnail_exact(w, h),
    };
    let fast = || {
        let src = ImageRef::new(img.width(), img.height(), img.as_bytes(), pixel).ok()?;
        let mut dst = Image::new(w, h, pixel);
        let box_filter = ResizeOptions::new()
            .resize_alg(ResizeAlg::Convolution(FilterType::Box))
            .use_alpha(false);
        Resizer::new().resize(&src, &mut dst, &box_filter).ok()?;
        let raw = dst.into_vec();
        Some(match pixel {
            PixelType::U8 => DynamicImage::ImageLuma8(ImageBuffer::from_raw(w, h, raw)?),
            PixelType::U8x2 => DynamicImage::ImageLumaA8(ImageBuffer::from_raw(w, h, raw)?),
            PixelType::U8x3 => DynamicImage::ImageRgb8(ImageBuffer::from_raw(w, h, raw)?),
            _ => DynamicImage::ImageRgba8(ImageBuffer::from_raw(w, h, raw)?),
        })
    };
    fast().unwrap_or_else(|| img.thumbnail_exact(w, h))
}

/// Encodes `img` for `protocol` in `pane` (P3 4.4, step 4). `None` for [`Protocol::Off`] or
/// when the encoder fails.
pub fn prepare(img: &DynamicImage, pane: Pane, protocol: Protocol) -> Option<Prepared> {
    prepare_oriented(img, Orientation::NoTransforms, pane, protocol)
}

/// [`prepare`] of an image whose EXIF orientation is `o`: the fit uses the oriented size,
/// and the orientation is applied after the scale, to the small image.
pub fn prepare_oriented(
    img: &DynamicImage,
    o: Orientation,
    pane: Pane,
    protocol: Protocol,
) -> Option<Prepared> {
    if pane.cols == 0 || pane.rows == 0 || protocol == Protocol::Off {
        return None;
    }
    let halfblocks = protocol == Protocol::Halfblocks;
    let swap = swaps(o);
    let (w, h) = if swap {
        (img.height(), img.width())
    } else {
        (img.width(), img.height())
    };
    let (px, mut cells) = fit(w, h, pane, halfblocks);
    let (tw, th) = if swap { (px.1, px.0) } else { px };
    let mut small = if (img.width(), img.height()) == (tw, th) {
        img.clone()
    } else {
        scale(img, tw, th)
    };
    small.apply_orientation(o);
    let id = next_id();
    let body = match protocol {
        Protocol::Off => return None,
        Protocol::Halfblocks => {
            let rgba = small.to_rgba8();
            let (w, h) = (rgba.width(), rgba.height());
            let pix = |x: u32, y: u32| -> Option<[u8; 3]> {
                (y < h)
                    .then(|| rgba.get_pixel(x, y).0)
                    .and_then(|[r, g, b, a]| (a >= 128).then_some([r, g, b]))
            };
            let mut out = Vec::with_capacity((w * h.div_ceil(2)) as usize);
            for row in 0..h.div_ceil(2) {
                for x in 0..w {
                    out.push(Half {
                        top: pix(x, row * 2),
                        bottom: pix(x, row * 2 + 1),
                    });
                }
            }
            cells = (w as u16, h.div_ceil(2) as u16);
            Body::Halfblocks { cells: out }
        }
        Protocol::Kitty | Protocol::KittyTmux => {
            let tmux = protocol == Protocol::KittyTmux;
            if tmux {
                cells = (
                    cells.0.min(MAX_PLACEHOLDER_CELLS),
                    cells.1.min(MAX_PLACEHOLDER_CELLS),
                );
            }
            let virtual_cells = tmux.then_some(cells);
            let transmit = if small.color().has_alpha() {
                kitty_transmit(small.to_rgba8().as_raw(), 32, px, id, virtual_cells)
            } else {
                kitty_transmit(small.to_rgb8().as_raw(), 24, px, id, virtual_cells)
            };
            if tmux {
                Body::KittyTmux { transmit }
            } else {
                Body::Kitty { transmit }
            }
        }
        Protocol::Sixel => {
            let rgba = small.to_rgba8().into_raw();
            let s = icy_sixel::SixelImage::from_rgba(rgba, px.0 as usize, px.1 as usize)
                .encode()
                .ok()?;
            Body::Sixel {
                data: s.into_bytes(),
            }
        }
    };
    Some(Prepared {
        id,
        pane: (pane.cols, pane.rows),
        cells,
        px,
        body,
    })
}

/// Where `p` goes in `area`: centred horizontally, at the top.
pub fn image_rect(p: &Prepared, area: Rect) -> Rect {
    let w = p.cells.0.min(area.width);
    let h = p.cells.1.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y,
        width: w,
        height: h,
    }
}

/// Draws `p` into the frame's cells in `area` (P3 4.7) and returns the rectangle it covers.
/// Halfblocks and unicode placeholders are cells; a direct kitty placement and a sixel are
/// drawn by the terminal after the frame over the blank cells the frame leaves there
/// ([`Screen::after_draw`]).
pub fn draw(buf: &mut Buffer, area: Rect, p: &Prepared) -> Rect {
    let r = image_rect(p, area);
    match &p.body {
        Body::Halfblocks { cells } => {
            let w = p.cells.0 as usize;
            for (i, h) in cells.iter().enumerate() {
                let (x, y) = ((i % w) as u16, (i / w) as u16);
                if x >= r.width || y >= r.height {
                    continue;
                }
                let Some(c) = buf.cell_mut((r.x + x, r.y + y)) else {
                    continue;
                };
                let rgb = |p: [u8; 3]| Color::Rgb(p[0], p[1], p[2]);
                match (h.top, h.bottom) {
                    (Some(t), Some(b)) => {
                        c.set_char('▀').set_fg(rgb(t)).set_bg(rgb(b));
                    }
                    (Some(t), None) => {
                        c.set_char('▀').set_fg(rgb(t)).set_bg(Color::Reset);
                    }
                    (None, Some(b)) => {
                        c.set_char('▄').set_fg(rgb(b)).set_bg(Color::Reset);
                    }
                    (None, None) => {
                        c.set_char(' ');
                    }
                }
            }
        }
        Body::KittyTmux { .. } => {
            let [_, r8, g8, b8] = p.id.to_be_bytes();
            let mut sym = String::with_capacity(12);
            for y in 0..r.height {
                for x in 0..r.width {
                    let Some(c) = buf.cell_mut((r.x + x, r.y + y)) else {
                        continue;
                    };
                    sym.clear();
                    sym.push(PLACEHOLDER);
                    sym.push(diacritic(y));
                    sym.push(diacritic(x));
                    c.set_symbol(&sym)
                        .set_fg(Color::Rgb(r8, g8, b8))
                        .set_diff_option(CellDiffOption::ForcedWidth(std::num::NonZeroU16::MIN));
                }
            }
        }
        Body::Kitty { .. } | Body::Sixel { .. } => {}
    }
    r
}

fn apc(tmux: bool, body: &str, out: &mut Vec<u8>) {
    let seq = format!("\x1b_G{body}\x1b\\");
    if tmux {
        passthrough(seq.as_bytes(), out);
    } else {
        out.extend_from_slice(seq.as_bytes());
    }
}

/// Moves the cursor to `r`'s top-left for a placement, keeping the position ratatui set
/// (`ESC 7` ... `ESC 8`).
fn at(r: Rect, body: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b7");
    let _ = write!(out, "\x1b[{};{}H", r.y as u32 + 1, r.x as u32 + 1);
    out.extend_from_slice(body);
    out.extend_from_slice(b"\x1b8");
}

/// What the terminal holds, and the counters `--log` and the tests read.
#[derive(Debug, Default)]
pub struct Screen {
    pub protocol: Protocol,
    /// Kitty images the terminal stores, least recently shown first (P3 2.6).
    stored: Vec<u32>,
    /// The direct kitty placement on screen.
    placed: Option<(u32, Rect)>,
    /// The sixel on screen.
    sixel: Option<(u32, Rect)>,
    pub transmits: u64,
    pub transmit_bytes: u64,
    pub deletes: u64,
}

impl Screen {
    pub fn new(protocol: Protocol) -> Screen {
        Screen {
            protocol,
            ..Screen::default()
        }
    }

    fn tmux(&self) -> bool {
        self.protocol == Protocol::KittyTmux
    }

    /// The kitty image ids the terminal stores.
    pub fn stored(&self) -> &[u32] {
        &self.stored
    }

    /// Whether the frame that may show `want` must clear the screen first: a sixel that is
    /// no longer wanted is only removed by rewriting its cells (P3 4.5).
    pub fn needs_clear(&self, want: Option<&Prepared>) -> bool {
        self.sixel
            .is_some_and(|(s, _)| Some(s) != want.map(|p| p.id))
    }

    /// Before ratatui draws a frame whose view may show `want` (P3 4.5): a kitty image is
    /// transmitted unless the terminal stores it, a placement that no longer matches is
    /// deleted, and images beyond [`MAX_STORED`] are deleted with their data. A frame that
    /// clears the screen calls [`Screen::forget_all`] first.
    pub fn before_draw(&mut self, want: Option<&Prepared>, out: &mut Vec<u8>) {
        let id = want.map(|p| p.id);
        if let Some(p) = want
            && let Body::Kitty { transmit } | Body::KittyTmux { transmit } = &p.body
        {
            if let Some(k) = self.stored.iter().position(|&s| s == p.id) {
                self.stored.remove(k);
            } else {
                out.extend_from_slice(transmit);
                self.transmits += 1;
                self.transmit_bytes += transmit.len() as u64;
                tracing::debug!(id = p.id, bytes = transmit.len(), "preview transmit");
            }
            self.stored.push(p.id);
            while self.stored.len() > MAX_STORED {
                let old = self.stored.remove(0);
                self.delete(old, true, out);
            }
        }
        if let Some((p, _)) = self.placed
            && Some(p) != id
        {
            self.delete(p, false, out);
            self.placed = None;
        }
    }

    /// After ratatui drew a frame whose view shows `shown` (P3 4.5): a direct kitty image is
    /// placed at its cells, a sixel is sent, unless the terminal shows them there already.
    /// Returns `true` when a sixel must be removed by another frame that clears first.
    pub fn after_draw(&mut self, shown: Option<(&Prepared, Rect)>, out: &mut Vec<u8>) -> bool {
        match shown {
            Some((p, r)) => match &p.body {
                Body::Kitty { .. } => {
                    if self.placed != Some((p.id, r)) && self.stored.contains(&p.id) {
                        let cmd = format!("\x1b_Ga=p,i={},p=1,C=1,q=2\x1b\\", p.id);
                        at(r, cmd.as_bytes(), out);
                        self.placed = Some((p.id, r));
                    }
                }
                Body::Sixel { data } => match self.sixel {
                    Some(s) if s == (p.id, r) => {}
                    Some(_) => return true,
                    None => {
                        at(r, data, out);
                        self.sixel = Some((p.id, r));
                        tracing::debug!(id = p.id, bytes = data.len(), "preview sixel");
                    }
                },
                _ => {}
            },
            None => {
                if let Some((p, _)) = self.placed.take() {
                    self.delete(p, false, out);
                }
                if self.sixel.is_some() {
                    return true;
                }
            }
        }
        false
    }

    /// Deletes image `id`: its placement (`d=i`), or with `data` also what the terminal
    /// stores (`d=I`).
    fn delete(&mut self, id: u32, data: bool, out: &mut Vec<u8>) {
        let d = if data { 'I' } else { 'i' };
        apc(self.tmux(), &format!("a=d,d={d},i={id},q=2"), out);
        self.deletes += 1;
    }

    /// Deletes image `id` with its data (P3 4.7, `forget(id)`).
    pub fn forget(&mut self, id: u32, out: &mut Vec<u8>) {
        if let Some(k) = self.stored.iter().position(|&s| s == id) {
            self.stored.remove(k);
            self.delete(id, true, out);
        }
        if self.placed.is_some_and(|(p, _)| p == id) {
            self.placed = None;
        }
    }

    /// Before a hand-off and before exit (P3 4.5): every stored kitty image is deleted with
    /// its data; the next full frame transmits and places again. A sixel goes with the
    /// alternate screen.
    pub fn forget_all(&mut self, out: &mut Vec<u8>) {
        for id in std::mem::take(&mut self.stored) {
            self.delete(id, true, out);
        }
        self.placed = None;
        self.sixel = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(i.as_bytes()), o.as_bytes(), "{i}");
        }
    }

    #[test]
    fn fitting_never_upscales_and_keeps_the_aspect() {
        let pane = Pane {
            cols: 100,
            rows: 50,
            cell: Some((10, 20)),
        };
        // A 12 MP photo into a 1000 x 1000 px box.
        assert_eq!(fit(4000, 3000, pane, false), ((1000, 750), (100, 38)));
        // A small icon stays small.
        assert_eq!(fit(16, 16, pane, false), ((16, 16), (2, 1)));
        // Tall.
        assert_eq!(fit(1000, 4000, pane, false), ((250, 1000), (25, 50)));
        // Halfblocks: a half cell is 10 x 10 px here.
        assert_eq!(fit(4000, 3000, pane, true), ((100, 75), (100, 38)));
        let unknown = Pane {
            cols: 40,
            rows: 20,
            cell: None,
        };
        assert_eq!(fit(80, 80, unknown, true), ((40, 40), (40, 20)));
    }

    #[test]
    fn diacritics_are_combining_marks() {
        assert_eq!(DIACRITICS.len(), 297);
        assert!(DIACRITICS.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(diacritic(0), '\u{0305}');
        assert_eq!(diacritic(296), '\u{1D244}');
    }

    fn kitty(id: u32) -> Prepared {
        Prepared {
            id,
            pane: (10, 5),
            cells: (4, 2),
            px: (40, 40),
            body: Body::Kitty {
                transmit: format!("T{id}").into_bytes(),
            },
        }
    }

    #[test]
    fn a_kitty_image_is_transmitted_once_and_placed_where_it_shows() {
        let mut s = Screen::new(Protocol::Kitty);
        let r = Rect::new(3, 4, 4, 2);
        let a = kitty(1);
        let mut out = Vec::new();
        s.before_draw(Some(&a), &mut out);
        assert!(!s.after_draw(Some((&a, r)), &mut out));
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(text.starts_with("T1"), "{text:?}");
        assert!(
            text.contains("\x1b[5;4H\x1b_Ga=p,i=1,p=1,C=1,q=2\x1b\\"),
            "{text:?}"
        );
        // The same image again: nothing to write.
        out.clear();
        s.before_draw(Some(&a), &mut out);
        s.after_draw(Some((&a, r)), &mut out);
        assert!(out.is_empty(), "{:?}", String::from_utf8_lossy(&out));
        // Replaced: the old placement goes, the new image comes.
        let b = kitty(2);
        s.before_draw(Some(&b), &mut out);
        s.after_draw(Some((&b, r)), &mut out);
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(text.contains("a=d,d=i,i=1,q=2"), "{text:?}");
        assert!(text.contains("T2"));
        // Back to the first: stored, so placed without a transmit.
        out.clear();
        s.before_draw(Some(&a), &mut out);
        s.after_draw(Some((&a, r)), &mut out);
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(!text.contains("T1") && text.contains("a=p,i=1"), "{text:?}");
        // A modal: the placement goes.
        out.clear();
        s.before_draw(None, &mut out);
        s.after_draw(None, &mut out);
        assert!(String::from_utf8_lossy(&out).contains("a=d,d=i,i=1"));
        // Hand-off: everything is deleted with its data.
        out.clear();
        s.forget_all(&mut out);
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(
            text.contains("a=d,d=I,i=1") && text.contains("a=d,d=I,i=2"),
            "{text:?}"
        );
        assert_eq!(s.transmits, 2);
    }

    #[test]
    fn the_terminal_stores_at_most_eight_images() {
        let mut s = Screen::new(Protocol::Kitty);
        let mut out = Vec::new();
        for id in 1..=10 {
            s.before_draw(Some(&kitty(id)), &mut out);
        }
        assert_eq!(s.stored(), &[3, 4, 5, 6, 7, 8, 9, 10]);
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(text.contains("a=d,d=I,i=1,") && text.contains("a=d,d=I,i=2,"));
    }

    #[test]
    fn a_sixel_that_goes_away_clears_the_frame() {
        let mut s = Screen::new(Protocol::Sixel);
        let p = Prepared {
            id: 9,
            pane: (10, 5),
            cells: (4, 2),
            px: (40, 40),
            body: Body::Sixel {
                data: b"\x1bPq#0~\x1b\\".to_vec(),
            },
        };
        let r = Rect::new(0, 1, 4, 2);
        let mut out = Vec::new();
        assert!(!s.needs_clear(Some(&p)));
        s.before_draw(Some(&p), &mut out);
        assert!(!s.after_draw(Some((&p, r)), &mut out));
        assert!(String::from_utf8_lossy(&out).contains("\x1bPq"));
        out.clear();
        // Unchanged: not sent again.
        assert!(!s.needs_clear(Some(&p)));
        assert!(!s.after_draw(Some((&p, r)), &mut out));
        assert!(out.is_empty());
        // A modal: the frame clears first.
        assert!(s.needs_clear(None));
        s.forget_all(&mut out);
        s.before_draw(None, &mut out);
        assert!(!s.after_draw(None, &mut out));
        // Shown again after the clear: sent again.
        s.before_draw(Some(&p), &mut out);
        s.after_draw(Some((&p, r)), &mut out);
        assert!(String::from_utf8_lossy(&out).contains("\x1bPq"));
        // Moved without a clear: another frame must clear.
        assert!(s.after_draw(Some((&p, Rect::new(20, 1, 4, 2))), &mut out));
    }

    #[test]
    fn tmux_transmits_go_through_the_passthrough() {
        let img =
            DynamicImage::ImageRgb8(image::RgbImage::from_pixel(20, 10, image::Rgb([1, 2, 3])));
        let pane = Pane {
            cols: 10,
            rows: 5,
            cell: Some((10, 20)),
        };
        let p = prepare(&img, pane, Protocol::KittyTmux).unwrap();
        let Body::KittyTmux { transmit } = &p.body else {
            panic!("{p:?}")
        };
        assert!(transmit.starts_with(b"\x1bPtmux;\x1b\x1b_Ga=T,U=1,c=2,r=1,f=24,o=z,t=d,i="));
        assert!(transmit.ends_with(b"\x1b\x1b\\\x1b\\"));
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 5));
        let r = draw(&mut buf, Rect::new(0, 0, 10, 5), &p);
        assert_eq!(r, Rect::new(4, 0, 2, 1));
        let [_, a, b, c] = p.id.to_be_bytes();
        let cell = &buf[(5, 0)];
        assert_eq!(cell.symbol(), "\u{10EEEE}\u{0305}\u{030D}");
        assert_eq!(cell.fg, Color::Rgb(a, b, c));
    }

    /// A box filter: an exact quarter of a four-colour image keeps its colours; the colour
    /// type stays; a 16-bit image goes through `image`'s thumbnail at the same size.
    #[test]
    fn scaling_averages_boxes_and_keeps_the_colour_type() {
        let quad = DynamicImage::ImageRgb8(image::RgbImage::from_fn(8, 8, |x, y| {
            match (x < 4, y < 4) {
                (true, true) => image::Rgb([255, 0, 0]),
                (false, true) => image::Rgb([0, 255, 0]),
                (true, false) => image::Rgb([0, 0, 255]),
                (false, false) => image::Rgb([255, 255, 255]),
            }
        }));
        let small = scale(&quad, 2, 2);
        let DynamicImage::ImageRgb8(rgb) = &small else {
            panic!("{:?}", small.color())
        };
        assert_eq!(
            rgb.as_raw(),
            &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]
        );
        let grey =
            DynamicImage::ImageLuma8(image::GrayImage::from_raw(2, 1, vec![0, 255]).unwrap());
        let DynamicImage::ImageLuma8(g) = scale(&grey, 1, 1) else {
            panic!()
        };
        assert!((127..=128).contains(&g.as_raw()[0]), "{g:?}");
        for img in [
            DynamicImage::ImageLumaA8(image::GrayAlphaImage::new(40, 20)),
            DynamicImage::ImageRgba8(image::RgbaImage::new(40, 20)),
            DynamicImage::ImageRgb16(image::ImageBuffer::new(40, 20)),
        ] {
            let small = scale(&img, 10, 5);
            assert_eq!((small.width(), small.height()), (10, 5));
            assert_eq!(small.color(), img.color());
        }
    }

    /// The inverse of [`base64`].
    fn unbase64(s: &[u8]) -> Vec<u8> {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = Vec::new();
        for q in s.chunks(4) {
            let mut n = 0u32;
            let mut have = 0;
            for (i, &c) in q.iter().enumerate() {
                if c != b'=' {
                    n |= (T.iter().position(|&t| t == c).unwrap() as u32) << (18 - 6 * i);
                    have += 1;
                }
            }
            out.extend_from_slice(&n.to_be_bytes()[1..have]);
        }
        out
    }

    /// The zlib stream a kitty transmit carries: its chunks' payloads, base64-decoded.
    fn transmitted(p: &Prepared) -> Vec<u8> {
        let Body::Kitty { transmit } = &p.body else {
            panic!("{p:?}")
        };
        let mut b64 = Vec::new();
        for cmd in transmit
            .split(|&b| b == 0x1b)
            .filter(|c| c.starts_with(b"_G"))
        {
            let at = cmd.iter().position(|&b| b == b';').unwrap();
            b64.extend_from_slice(&cmd[at + 1..]);
        }
        unbase64(&b64)
    }

    /// Whether a zlib stream holds only stored deflate blocks, up to its checksum.
    fn only_stored(z: &[u8]) -> bool {
        let mut i = 2;
        loop {
            let Some(&h) = z.get(i) else {
                return false;
            };
            let Some(len) = z.get(i + 1..i + 5).and_then(|b| {
                let (len, nlen) = (
                    u16::from_le_bytes([b[0], b[1]]),
                    u16::from_le_bytes([b[2], b[3]]),
                );
                (len == !nlen).then_some(len as usize)
            }) else {
                return false;
            };
            if (h >> 1) & 3 != 0 {
                return false;
            }
            i += 5 + len;
            if h & 1 == 1 {
                return i + 4 == z.len();
            }
        }
    }

    /// P3 4.3, P-23: a smooth image is deflated at level 1 and shrinks; the noise of a
    /// photo is sent in stored blocks; both streams inflate to the exact pixels.
    #[test]
    fn a_kitty_transmit_deflates_what_compresses_and_stores_noise() {
        let pane = Pane {
            cols: 100,
            rows: 50,
            cell: Some((10, 20)),
        };
        // A gradient in steps, as a wallpaper at pane size is.
        let smooth =
            image::RgbImage::from_fn(256, 128, |x, y| image::Rgb([(x / 4) as u8, y as u8, 100]));
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let noise = image::RgbImage::from_fn(256, 128, |_, _| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let [a, b, c, ..] = seed.to_le_bytes();
            image::Rgb([a, b, c])
        });
        for (img, stored) in [(smooth, false), (noise, true)] {
            let raw = img.as_raw().clone();
            assert!(raw.len() > SAMPLES * SAMPLE, "the samples are a part of it");
            let p = prepare(&DynamicImage::ImageRgb8(img), pane, Protocol::Kitty).unwrap();
            assert_eq!(p.px, (256, 128), "not scaled");
            let z = transmitted(&p);
            assert_eq!(only_stored(&z), stored);
            if !stored {
                assert!(z.len() < raw.len() / 2, "{} of {}", z.len(), raw.len());
            }
            let mut back = Vec::new();
            std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(&z[..]), &mut back)
                .unwrap();
            assert!(
                back == raw,
                "the stream inflates to the pixels (stored: {stored})"
            );
        }
    }
}
