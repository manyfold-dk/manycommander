#![forbid(unsafe_code)]
//! Tar listing, plain and compressed (P3 3.1, 3.2, 3.3).
//!
//! The `tar` crate reads every header; a plain tar skips member data by seeking, a
//! compressed one only by decompressing it. Pax extended headers and GNU long-name and
//! long-link headers are folded into the next member by the crate and are not nodes; a
//! GNU sparse member (the `S` type, or pax `GNU.sparse.*` keys) is skipped as "sparse
//! member". The locator is the member's data offset in the decompressed stream.
//!
//! A [`Guard`] sits between the decoder and the crate. It checks the cancel flag on every
//! read (the crate skips data in reads of 32 KiB, P-20), counts the decompressed bytes,
//! and bounds what the crate may read beyond the next header: the crate reads a GNU
//! long-name or pax header whole into memory, so a header that declares gigabytes of
//! "name" would otherwise decompress into memory. The decoders run with the window caps of
//! A-4: zstd with `window_log_max` 27, and xz behind an [`XzCap`] that refuses an LZMA2
//! dictionary above 128 MiB before the decoder allocates it.

use super::detect::Format;
use super::index::{Member, MemberKind, SPARSE, Tree, UNSUPPORTED};
use super::{ArchiveIndex, PosReader, Sink, Stop, XZ_MEMORY_MAX, ZSTD_WINDOW_LOG_MAX};
use crate::fsops::sys::Ts;
use std::cell::Cell;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What the crate may read beyond the next header before it returns the member: its
/// headers, GNU long names and pax records.
const EXT_CAP: u64 = 4 << 20;

/// The shared state of a [`Guard`] and the scan loop.
#[derive(Default)]
struct GuardState {
    /// Decompressed bytes consumed (or the position after a seek).
    pos: Cell<u64>,
    /// No read goes beyond this position.
    limit: Cell<u64>,
    over: Cell<bool>,
    cancelled: Cell<bool>,
}

/// Cancel checks and the read bound between a decoder and the tar crate.
struct Guard<R> {
    inner: R,
    st: Rc<GuardState>,
    cancel: Arc<AtomicBool>,
}

impl<R: Read> Read for Guard<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            self.st.cancelled.set(true);
            return Err(io::Error::other("cancelled"));
        }
        let pos = self.st.pos.get();
        let limit = self.st.limit.get();
        if pos >= limit {
            self.st.over.set(true);
            return Err(io::Error::other("a member header is too large"));
        }
        let max = (buf.len() as u64).min(limit - pos) as usize;
        let n = self.inner.read(&mut buf[..max])?;
        self.st.pos.set(pos + n as u64);
        Ok(n)
    }
}

impl<R: Seek> Seek for Guard<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let p = self.inner.seek(to)?;
        self.st.pos.set(p);
        Ok(p)
    }
}

/// A stream decoder and the flag its memory guard raises.
pub(crate) type Decoder = (Box<dyn Read>, Rc<Cell<bool>>);

/// The stream decoder of a compressed tar, with the window caps of A-4, and the flag its
/// memory guard raises.
pub(crate) fn decoder(format: Format, base: PosReader) -> io::Result<Decoder> {
    let memory = Rc::new(Cell::new(false));
    let r: Box<dyn Read> = match format {
        Format::TarGz => Box::new(flate2::read::MultiGzDecoder::new(base)),
        Format::TarZst => {
            let mut d = zstd::stream::read::Decoder::new(base)?;
            d.window_log_max(ZSTD_WINDOW_LOG_MAX)?;
            Box::new(d)
        }
        Format::TarXz => Box::new(lzma_rust2::XzReader::new(
            XzCap::new(BufReader::with_capacity(64 << 10, base), memory.clone()),
            true,
        )),
        Format::TarBz2 => Box::new(bzip2::read::MultiBzDecoder::new(base)),
        Format::Tar | Format::Zip => Box::new(base),
    };
    Ok((r, memory))
}

/// Whether a decoder error is the window cap (A-4): the xz guard's flag, or libzstd's
/// "Frame requires too much memory for decoding".
pub(crate) fn is_memory_error(e: &io::Error, memory: &Cell<bool>) -> bool {
    memory.get() || e.to_string().contains("too much memory")
}

/// `n` rounded up to whole 512-byte blocks.
fn blocks(n: u64) -> Option<u64> {
    n.checked_add(511).map(|x| x & !511)
}

/// A pax `mtime`: seconds with an optional fraction, possibly negative.
fn pax_time(v: &[u8]) -> Option<Ts> {
    let s = std::str::from_utf8(v).ok()?;
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    let sec: i64 = int.parse().ok()?;
    let digits: String = frac.chars().take(9).collect();
    if !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut nsec: u32 = if digits.is_empty() {
        0
    } else {
        format!("{digits:0<9}").parse().ok()?
    };
    let mut sec = sec;
    if int.starts_with('-') && nsec > 0 {
        // -1.5 is 1.5 s before the epoch: -2 s plus 0.5 s.
        sec -= 1;
        nsec = 1_000_000_000 - nsec;
    }
    Some(Ts { sec, nsec })
}

/// Scans a tar, plain or compressed, into `tree` (P3 3.3).
pub(crate) fn scan(
    ix: &ArchiveIndex,
    format: Format,
    tree: &mut Tree,
    sink: &mut Sink<'_>,
) -> Result<(), Stop> {
    let st = Rc::new(GuardState::default());
    let base = PosReader::new(ix.file().clone(), ix.key.size).progress(ix.read.clone());
    if format == Format::Tar {
        let mut ar = ::tar::Archive::new(Guard {
            inner: base,
            st: st.clone(),
            cancel: ix.cancel.clone(),
        });
        let entries = ar.entries_with_seek().map_err(|_| Stop::Damaged)?;
        walk(entries, &st, None, tree, sink)
    } else {
        let (dec, memory) = decoder(format, base).map_err(|_| Stop::Damaged)?;
        let mut ar = ::tar::Archive::new(Guard {
            inner: dec,
            st: st.clone(),
            cancel: ix.cancel.clone(),
        });
        let entries = ar.entries().map_err(|_| Stop::Damaged)?;
        walk(entries, &st, Some(&memory), tree, sink)
    }
}

/// Why the crate's iteration failed.
fn classify(e: &io::Error, st: &GuardState, memory: Option<&Cell<bool>>) -> Stop {
    if st.cancelled.get() {
        Stop::Cancelled
    } else if memory.is_some_and(|m| is_memory_error(e, m)) {
        Stop::Memory
    } else {
        Stop::Damaged
    }
}

fn walk<R: Read>(
    mut entries: ::tar::Entries<'_, R>,
    st: &GuardState,
    memory: Option<&Cell<bool>>,
    tree: &mut Tree,
    sink: &mut Sink<'_>,
) -> Result<(), Stop> {
    // Where the crate reads the next header.
    let mut next = 0u64;
    loop {
        st.limit.set(next.saturating_add(EXT_CAP));
        sink.poll(tree);
        let mut e = match entries.next() {
            None => {
                // The end of the archive is a zero block; a stream that simply stops, at
                // a header or inside a member's data, is damaged.
                return if st.pos.get() >= next.saturating_add(512) {
                    Ok(())
                } else {
                    Err(Stop::Damaged)
                };
            }
            Some(Err(err)) => return Err(classify(&err, st, memory)),
            Some(Ok(e)) => e,
        };
        let mut pax_size = None;
        let mut pax_mtime = None;
        let mut pax_sparse = false;
        if let Ok(Some(exts)) = e.pax_extensions() {
            for x in exts.flatten() {
                match x.key_bytes() {
                    b"size" => {
                        pax_size = std::str::from_utf8(x.value_bytes())
                            .ok()
                            .and_then(|v| v.parse::<u64>().ok())
                    }
                    b"mtime" => pax_mtime = pax_time(x.value_bytes()),
                    k if k.starts_with(b"GNU.sparse.") => pax_sparse = true,
                    _ => {}
                }
            }
        }
        let header = e.header();
        let stored = match pax_size {
            Some(s) => s,
            None => header.entry_size().map_err(|_| Stop::Damaged)?,
        };
        let file_pos = e.raw_file_position();
        next = file_pos
            .checked_add(blocks(stored).ok_or(Stop::Damaged)?)
            .ok_or(Stop::Damaged)?;
        let t = header.entry_type();
        if t.is_pax_global_extensions() {
            continue;
        }
        if t.is_gnu_sparse() || pax_sparse {
            // A-AR-1: bsdtar expands it; the index leaves it out, and says so.
            tree.skip(SPARSE);
            continue;
        }
        let name = e.path_bytes().into_owned();
        let link = e
            .link_name_bytes()
            .map(|l| l.into_owned())
            .unwrap_or_default();
        let kind = if t.is_file() || t.is_contiguous() {
            if name.ends_with(b"/") {
                MemberKind::Dir
            } else {
                MemberKind::File
            }
        } else if t.is_dir() || t.as_byte() == b'D' {
            MemberKind::Dir
        } else if t.is_symlink() {
            MemberKind::Symlink(&link)
        } else if t.is_hard_link() {
            MemberKind::HardLink(&link)
        } else if t.is_character_special() || t.is_block_special() || t.is_fifo() {
            MemberKind::Special
        } else {
            tree.skip(UNSUPPORTED);
            continue;
        };
        let mode = header.mode().unwrap_or(match kind {
            MemberKind::Dir => 0o755,
            _ => 0o644,
        });
        let mtime = pax_mtime.or_else(|| {
            header.mtime().ok().map(|s| Ts {
                sec: s.min(i64::MAX as u64) as i64,
                nsec: 0,
            })
        });
        let added = tree
            .add(Member {
                name: &name,
                kind,
                mode,
                size: e.size(),
                mtime,
                locator: file_pos,
                encrypted: false,
            })
            .map_err(|f| Stop::Full(f.0))?;
        sink.added(tree, added);
        sink.before_skip(stored);
    }
}

/// Where an [`XzCap`] is in the xz container.
enum Xz {
    /// The 12-byte stream header; `got` bytes seen.
    StreamHeader {
        got: u8,
    },
    /// A block header's size byte, or the index indicator.
    BlockStart,
    /// A block header of `size` bytes, collected in `buf`.
    BlockHeader {
        size: usize,
    },
    /// An LZMA2 control byte.
    Chunk,
    /// The rest of an LZMA2 chunk header.
    ChunkHeader {
        control: u8,
        got: u8,
        need: u8,
    },
    /// Chunk payload bytes.
    Data {
        left: u64,
    },
    /// Block padding, then the check.
    Padding {
        left: u8,
    },
    Check {
        left: u8,
    },
    /// The index after its indicator: the record count, the records, padding and CRC32.
    IndexCount,
    IndexRecords {
        left: u64,
        half: bool,
    },
    IndexPadding {
        left: u8,
    },
    IndexCrc {
        left: u8,
    },
    Footer {
        left: u8,
    },
    /// Zero padding between streams, or the end.
    StreamPadding,
}

#[derive(Debug)]
enum XzFail {
    Memory,
    Invalid,
}

/// Walks an xz container as its bytes pass to the decoder (A-4). A block whose LZMA2
/// dictionary exceeds [`XZ_MEMORY_MAX`] fails before the decoder sees the rest of its
/// header, so the dictionary is never allocated. An index that claims another number of
/// blocks than the stream held fails before the decoder reserves room for its records. The
/// decoder checks everything else (checksums, the index against the blocks).
pub(crate) struct XzCap<R> {
    inner: R,
    st: Xz,
    memory: Rc<Cell<bool>>,
    check: u8,
    buf: Vec<u8>,
    block_data: u64,
    blocks: u64,
    index_len: u64,
    varint: (u64, u32),
    failed: bool,
}

impl<R: Read> XzCap<R> {
    pub(crate) fn new(inner: R, memory: Rc<Cell<bool>>) -> XzCap<R> {
        XzCap {
            inner,
            st: Xz::StreamHeader { got: 0 },
            memory,
            check: 0,
            buf: Vec::new(),
            block_data: 0,
            blocks: 0,
            index_len: 0,
            varint: (0, 0),
            failed: false,
        }
    }

    /// The size of a block's check field for a check type (the xz format's size groups).
    fn check_size(t: u8) -> u8 {
        match t {
            0 => 0,
            1..=3 => 4,
            4..=6 => 8,
            7..=9 => 16,
            10..=12 => 32,
            _ => 64,
        }
    }

    /// Feeds one multibyte integer byte; `Some(value)` when it is complete.
    fn varint(&mut self, b: u8) -> Result<Option<u64>, XzFail> {
        let (v, n) = self.varint;
        if n >= 9 {
            return Err(XzFail::Invalid);
        }
        let v = v | ((b & 0x7f) as u64) << (7 * n);
        if b & 0x80 == 0 {
            self.varint = (0, 0);
            Ok(Some(v))
        } else {
            self.varint = (v, n + 1);
            Ok(None)
        }
    }

    /// The dictionary sizes of a complete block header.
    fn block_header(h: &[u8]) -> Result<(), XzFail> {
        let mut at = 2usize;
        let end = h.len().checked_sub(4).ok_or(XzFail::Invalid)?;
        let byte = |at: &mut usize| -> Result<u8, XzFail> {
            let b = *h.get(*at).filter(|_| *at < end).ok_or(XzFail::Invalid)?;
            *at += 1;
            Ok(b)
        };
        let vint = |at: &mut usize| -> Result<u64, XzFail> {
            let mut v = 0u64;
            for i in 0..9 {
                let b = byte(at)?;
                v |= ((b & 0x7f) as u64) << (7 * i);
                if b & 0x80 == 0 {
                    return Ok(v);
                }
            }
            Err(XzFail::Invalid)
        };
        let flags = *h.get(1).ok_or(XzFail::Invalid)?;
        let filters = (flags & 3) + 1;
        if flags & 0x40 != 0 {
            vint(&mut at)?;
        }
        if flags & 0x80 != 0 {
            vint(&mut at)?;
        }
        let mut lzma2 = false;
        for _ in 0..filters {
            let id = vint(&mut at)?;
            let size = vint(&mut at)?;
            if id == 0x21 {
                if size != 1 {
                    return Err(XzFail::Invalid);
                }
                let p = byte(&mut at)?;
                if p > 40 {
                    return Err(XzFail::Invalid);
                }
                let dict: u64 = if p == 40 {
                    u32::MAX as u64
                } else {
                    (2 | (p & 1) as u64) << (p / 2 + 11)
                };
                if dict > XZ_MEMORY_MAX {
                    return Err(XzFail::Memory);
                }
                lzma2 = true;
            } else {
                for _ in 0..size.min(h.len() as u64) {
                    byte(&mut at)?;
                }
            }
        }
        if lzma2 { Ok(()) } else { Err(XzFail::Invalid) }
    }

    fn feed(&mut self, mut data: &[u8]) -> Result<(), XzFail> {
        while !data.is_empty() {
            if let Xz::Data { left } = &mut self.st {
                let n = (*left).min(data.len() as u64);
                *left -= n;
                self.block_data += n;
                data = &data[n as usize..];
                if *left == 0 {
                    self.st = Xz::Chunk;
                }
                continue;
            }
            let b = data[0];
            data = &data[1..];
            self.byte(b)?;
        }
        Ok(())
    }

    fn byte(&mut self, b: u8) -> Result<(), XzFail> {
        self.st = match std::mem::replace(&mut self.st, Xz::BlockStart) {
            Xz::StreamHeader { got } => {
                const MAGIC: &[u8] = b"\xfd7zXZ\x00";
                if (got as usize) < MAGIC.len() && b != MAGIC[got as usize] {
                    return Err(XzFail::Invalid);
                }
                if got == 7 {
                    self.check = Self::check_size(b & 0x0f);
                }
                if got == 11 {
                    self.blocks = 0;
                    Xz::BlockStart
                } else {
                    Xz::StreamHeader { got: got + 1 }
                }
            }
            Xz::BlockStart if b == 0 => {
                self.index_len = 1;
                Xz::IndexCount
            }
            Xz::BlockStart => {
                self.buf.clear();
                self.buf.push(b);
                Xz::BlockHeader {
                    size: (b as usize + 1) * 4,
                }
            }
            Xz::BlockHeader { size } => {
                self.buf.push(b);
                if self.buf.len() == size {
                    Self::block_header(&self.buf)?;
                    self.block_data = 0;
                    Xz::Chunk
                } else {
                    Xz::BlockHeader { size }
                }
            }
            Xz::Chunk => {
                self.block_data += 1;
                match b {
                    0 => {
                        let left = ((4 - self.block_data % 4) % 4) as u8;
                        self.blocks += 1;
                        self.after_data(left)
                    }
                    1 | 2 => Xz::ChunkHeader {
                        control: b,
                        got: 0,
                        need: 2,
                    },
                    0x80.. => Xz::ChunkHeader {
                        control: b,
                        got: 0,
                        need: if b >= 0xc0 { 5 } else { 4 },
                    },
                    _ => return Err(XzFail::Invalid),
                }
            }
            Xz::ChunkHeader { control, got, need } => {
                self.block_data += 1;
                if got == 0 {
                    self.buf.clear();
                }
                self.buf.push(b);
                if got + 1 < need {
                    Xz::ChunkHeader {
                        control,
                        got: got + 1,
                        need,
                    }
                } else {
                    let be =
                        |i: usize| u16::from_be_bytes([self.buf[i], self.buf[i + 1]]) as u64 + 1;
                    let left = if control < 0x80 { be(0) } else { be(2) };
                    Xz::Data { left }
                }
            }
            Xz::Data { left } => Xz::Data { left },
            Xz::Padding { left } => {
                if b != 0 {
                    return Err(XzFail::Invalid);
                }
                self.after_data(left - 1)
            }
            Xz::Check { left } => {
                if left > 1 {
                    Xz::Check { left: left - 1 }
                } else {
                    Xz::BlockStart
                }
            }
            Xz::IndexCount => {
                self.index_len += 1;
                match self.varint(b)? {
                    Some(n) if n != self.blocks => return Err(XzFail::Invalid),
                    Some(0) => self.index_padding(),
                    Some(n) => Xz::IndexRecords {
                        left: n,
                        half: false,
                    },
                    None => Xz::IndexCount,
                }
            }
            Xz::IndexRecords { left, half } => {
                self.index_len += 1;
                match self.varint(b)? {
                    None => Xz::IndexRecords { left, half },
                    Some(_) if !half => Xz::IndexRecords { left, half: true },
                    Some(_) if left > 1 => Xz::IndexRecords {
                        left: left - 1,
                        half: false,
                    },
                    Some(_) => self.index_padding(),
                }
            }
            Xz::IndexPadding { left } => {
                if b != 0 {
                    return Err(XzFail::Invalid);
                }
                if left > 1 {
                    Xz::IndexPadding { left: left - 1 }
                } else {
                    Xz::IndexCrc { left: 4 }
                }
            }
            Xz::IndexCrc { left } => {
                if left > 1 {
                    Xz::IndexCrc { left: left - 1 }
                } else {
                    Xz::Footer { left: 12 }
                }
            }
            Xz::Footer { left } => {
                if left > 1 {
                    Xz::Footer { left: left - 1 }
                } else {
                    Xz::StreamPadding
                }
            }
            Xz::StreamPadding if b == 0 => Xz::StreamPadding,
            Xz::StreamPadding if b == 0xfd => Xz::StreamHeader { got: 1 },
            Xz::StreamPadding => return Err(XzFail::Invalid),
        };
        Ok(())
    }

    /// After a block's LZMA2 data: `left` padding bytes, then the check.
    fn after_data(&self, left: u8) -> Xz {
        if left > 0 {
            Xz::Padding { left }
        } else if self.check > 0 {
            Xz::Check { left: self.check }
        } else {
            Xz::BlockStart
        }
    }

    fn index_padding(&self) -> Xz {
        match ((4 - self.index_len % 4) % 4) as u8 {
            0 => Xz::IndexCrc { left: 4 },
            left => Xz::IndexPadding { left },
        }
    }
}

impl<R: Read> Read for XzCap<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.failed {
            return Err(io::Error::other(super::DAMAGED));
        }
        let n = self.inner.read(buf)?;
        if let Err(f) = self.feed(&buf[..n]) {
            self.failed = true;
            return Err(match f {
                XzFail::Memory => {
                    self.memory.set(true);
                    io::Error::other(super::NEEDS_MEMORY)
                }
                XzFail::Invalid => io::Error::other(super::DAMAGED),
            });
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pax_times_keep_their_fraction() {
        assert_eq!(pax_time(b"12"), Some(Ts { sec: 12, nsec: 0 }));
        assert_eq!(
            pax_time(b"12.5"),
            Some(Ts {
                sec: 12,
                nsec: 500_000_000
            })
        );
        assert_eq!(
            pax_time(b"-1.25"),
            Some(Ts {
                sec: -2,
                nsec: 750_000_000
            })
        );
        assert_eq!(pax_time(b"x"), None);
        assert_eq!(pax_time(b"1.x"), None);
    }

    /// An xz stream through the guard decodes as without it; a block that asks for a
    /// dictionary above the cap fails with the memory flag before any decoding.
    #[test]
    fn the_xz_guard_passes_valid_streams_and_refuses_large_dictionaries() {
        use lzma_rust2::{XzOptions, XzWriter};
        use std::io::Write;
        let data: Vec<u8> = (0..300_000u32)
            .flat_map(|i| (i % 251).to_le_bytes())
            .collect();
        let mut xz = Vec::new();
        for _ in 0..2 {
            // Two concatenated streams, each of several blocks.
            let mut opts = XzOptions::with_preset(1);
            opts.set_block_size(Some(std::num::NonZeroU64::new(100_000).unwrap()));
            let mut w = XzWriter::new(&mut xz, opts).unwrap();
            w.write_all(&data).unwrap();
            w.finish().unwrap();
        }
        let memory = Rc::new(Cell::new(false));
        let mut r = lzma_rust2::XzReader::new(XzCap::new(&xz[..], memory.clone()), true);
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        assert_eq!(out.len(), data.len() * 2);
        assert!(!memory.get());

        // The first block header's LZMA2 dictionary property, raised to 256 MiB.
        let mut big = xz.clone();
        let h = 12;
        assert_eq!(big[h + 2], 0x21, "the filter id");
        big[h + 4] = 32;
        let memory = Rc::new(Cell::new(false));
        let mut r = lzma_rust2::XzReader::new(XzCap::new(&big[..], memory.clone()), true);
        let err = r.read_to_end(&mut Vec::new()).unwrap_err();
        assert!(memory.get(), "{err}");
        assert!(is_memory_error(&err, &memory));
    }
}
