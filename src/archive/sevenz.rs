#![forbid(unsafe_code)]
//! 7z listing and reading (P3 3.1, 3.2, 3.5): the stretch format of D-6.
//!
//! `sevenz-rust2` parses the header and decodes the blocks. Only its reader runs: its
//! extract helpers (the `util` feature) are not compiled, and no member name ever becomes a
//! path (A-1). The crate reads a whole header into memory and sizes its tables from counts
//! the header declares, so [`read_archive`] first walks the header under the bounds of A-4
//! and P3 2.6 ([`Walk`]): a header is at most [`HEADER_MAX`] bytes, as stored and decoded; a
//! compressed header is decoded here, its dictionary checked before it is allocated; and the
//! counts of files, folders and streams stay within the index's entry bound. The crate then
//! parses the plain header the walk saw, served from memory after the end of the file
//! ([`Src`]): it never decodes a header itself, and an archive rewritten in place cannot
//! hand it one that was not walked.
//!
//! Names are UTF-16 in 7z. The crate decodes them, and the index takes their UTF-8 bytes
//! under A-1. The crate refuses a whole header with a name that is not valid UTF-16, so the
//! walk replaces each unpaired surrogate with U+FFFD in the header the crate parses, and the
//! scan skips those members as "unsafe path", as `bsdtar` skips them. A member's kind and
//! mode follow libarchive: the Unix mode in the high half of the attributes (the `0x8000`
//! extension), else `0o755` for a directory attribute and `0o644` otherwise, and a 7z
//! directory (no data, not an empty file) is a directory whatever its mode says. A
//! symlink's target is its data: the scan lists the rows from the header first and reads
//! the targets after them ([`scan`]). Members of a block with an AES coder are listed and
//! flagged encrypted, and never decoded (A-AR-7).
//!
//! A block decodes from its start, so a member read ([`read_member`]) decodes the members
//! before it in its block and discards them. Extraction of a solid archive runs in one pass
//! ([`pass`], P3 3.5): each needed block decodes once per job, in block order, walking the
//! selected members inside it and stopping after the last one; a block without a selected
//! member is never decoded. Before a block decodes, its LZMA and LZMA2 dictionaries are
//! checked against the cap of A-4 ([`block_check`]). A member cannot produce more bytes than
//! its declared size: the crate bounds each member's reader to it, and the engine stops at
//! the first byte past it anyway (A-4).

use super::detect::SEVEN_Z;
use super::extract::{Decoded, ENCRYPTED_MEMBER, Expect, UNSUPPORTED_METHOD};
use super::index::{
    Added, LINK_TOO_LONG, Limits, Member, MemberKind, NodeId, Tree, UNSAFE_PATH, UNSUPPORTED,
};
use super::{
    ArchiveIndex, CHANGED_MEMBER, DAMAGED, NEEDS_MEMORY, PosReader, Sink, Stop, XZ_MEMORY_MAX,
};
use crate::fsops::copy::Flow;
use crate::fsops::origin::EachMember;
use crate::fsops::sys::Ts;
use sevenz_rust2::{Archive, ArchiveEntry, Block, BlockDecoder, NtTime, Password};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A 7z header is at most this large, as stored and decoded (A-4, P3 2.6).
pub const HEADER_MAX: u64 = 64 << 20;
/// The dictionaries of one block's LZMA and LZMA2 coders together stay within the xz cap
/// (A-4).
pub const DICT_MAX: u64 = XZ_MEMORY_MAX;
/// A 7z whose header, names included, is encrypted: nothing can be listed.
pub const ENCRYPTED_ARCHIVE: &str = "the archive is encrypted";
/// A member of a block that the block never presented.
const NOT_FOUND: &str = "not found in the archive";

/// A symlink target longer than this is not a target (`PATH_MAX`).
const LINK_MAX: u64 = 4096;
/// A folder of more coders, or a coder of more streams, is not a 7z any writer makes.
const MAX_CODERS: u64 = 64;
/// Coder properties are a few bytes (LZMA 5, AES at most 18).
const MAX_PROPS: u64 = 1024;

const K_END: u8 = 0x00;
const K_HEADER: u8 = 0x01;
const K_ARCHIVE_PROPERTIES: u8 = 0x02;
const K_ADDITIONAL_STREAMS_INFO: u8 = 0x03;
const K_MAIN_STREAMS_INFO: u8 = 0x04;
const K_FILES_INFO: u8 = 0x05;
const K_PACK_INFO: u8 = 0x06;
const K_UNPACK_INFO: u8 = 0x07;
const K_SUB_STREAMS_INFO: u8 = 0x08;
const K_SIZE: u8 = 0x09;
const K_CRC: u8 = 0x0a;
const K_FOLDER: u8 = 0x0b;
const K_CODERS_UNPACK_SIZE: u8 = 0x0c;
const K_NUM_UNPACK_STREAM: u8 = 0x0d;
const K_NAME: u8 = 0x11;
const K_ENCODED_HEADER: u8 = 0x17;

const ID_COPY: &[u8] = &[0x00];
const ID_LZMA: &[u8] = &[0x03, 0x01, 0x01];
const ID_LZMA2: &[u8] = &[0x21];
const ID_DEFLATE: &[u8] = &[0x04, 0x01, 0x08];
const ID_BZIP2: &[u8] = &[0x04, 0x02, 0x02];
const ID_AES: &[u8] = &[0x06, 0xf1, 0x07, 0x01];

/// The attribute bit that says the high 16 bits hold a Unix mode.
const UNIX_EXTENSION: u32 = 0x8000;
const ATTR_READONLY: u32 = 0x01;
const ATTR_DIRECTORY: u32 = 0x10;
const S_IFMT: u32 = 0o170000;
const S_IFSOCK: u32 = 0o140000;
const S_IFLNK: u32 = 0o120000;
const S_IFREG: u32 = 0o100000;
const S_IFBLK: u32 = 0o060000;
const S_IFDIR: u32 = 0o040000;
const S_IFCHR: u32 = 0o020000;
const S_IFIFO: u32 = 0o010000;

/// Why a 7z, or one of its blocks, cannot be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fail {
    /// A truncated file, a CRC error, a header that does not parse ([`DAMAGED`]).
    Damaged,
    /// A header or a dictionary above its cap ([`NEEDS_MEMORY`]).
    Memory,
    /// An encrypted header ([`ENCRYPTED_ARCHIVE`]).
    Encrypted,
    /// A coder or a header feature the reader lacks ([`UNSUPPORTED_METHOD`]).
    Unsupported,
    /// An index bound (P3 2.6), with its message.
    Full(String),
    Cancelled,
}

impl Fail {
    /// The message of a member or a load that fails with this.
    pub fn text(&self) -> String {
        match self {
            Fail::Damaged => DAMAGED.into(),
            Fail::Memory => NEEDS_MEMORY.into(),
            Fail::Encrypted => ENCRYPTED_ARCHIVE.into(),
            Fail::Unsupported => UNSUPPORTED_METHOD.into(),
            Fail::Full(m) => m.clone(),
            Fail::Cancelled => "cancelled".into(),
        }
    }

    /// How a scan stops (P3 3.3): without its header nothing lists; an index bound lists
    /// nothing and says so, as a zip that declares too many entries does.
    fn stop(self) -> Stop {
        match self {
            Fail::Cancelled => Stop::Cancelled,
            Fail::Full(m) => Stop::Full(m),
            f => Stop::Fatal(f.text()),
        }
    }
}

/// A CRC32 as 7z computes it.
fn crc32(b: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(b);
    c.sum()
}

/// The archive as the crate reads it: positioned reads on the index's held fd (P3 3.2), a
/// cancel check on every read (P-20), and, while the crate reads the header, the header
/// [`read_archive`] walked, served from memory.
pub struct Src {
    inner: PosReader,
    cancel: Arc<AtomicBool>,
    pinned: Vec<(u64, Arc<[u8]>)>,
    /// The file's length.
    len: u64,
    /// The length the crate sees: the file, then the pinned header after it.
    end: u64,
}

impl Src {
    /// Reads `len` bytes of `file`; `read` records how far (P3 3.3, A-AR-5).
    pub fn new(
        file: Arc<File>,
        len: u64,
        cancel: Arc<AtomicBool>,
        read: Option<Arc<AtomicU64>>,
    ) -> Src {
        let mut inner = PosReader::new(file, len).window(64 << 10);
        if let Some(r) = read {
            inner = inner.progress(r);
        }
        Src {
            inner,
            cancel,
            pinned: Vec::new(),
            len,
            end: len,
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Fills `buf` from `at` (the walk's own reads).
    fn read_exact_at(&mut self, at: u64, buf: &mut [u8]) -> Result<(), Fail> {
        self.inner
            .seek(SeekFrom::Start(at))
            .map_err(|_| Fail::Damaged)?;
        match super::read_full(&mut self.inner, buf) {
            _ if self.cancelled() => Err(Fail::Cancelled),
            Ok(n) if n == buf.len() => Ok(()),
            _ => Err(Fail::Damaged),
        }
    }

    /// Serves the bytes at `at` from `bytes` until [`Src::unpin`].
    fn pin(&mut self, at: u64, bytes: Arc<[u8]>) {
        self.end = self.end.max(at + bytes.len() as u64);
        self.pinned.push((at, bytes));
    }

    /// Back to the file alone.
    fn unpin(&mut self) {
        self.pinned = Vec::new();
        self.end = self.len;
    }
}

impl Read for Src {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancelled() {
            return Err(io::Error::other("cancelled"));
        }
        let pos = self.inner.position();
        for (at, bytes) in &self.pinned {
            if pos >= *at && pos - at < bytes.len() as u64 {
                let off = (pos - at) as usize;
                let n = buf.len().min(bytes.len() - off);
                buf[..n].copy_from_slice(&bytes[off..off + n]);
                self.inner.seek(SeekFrom::Current(n as i64))?;
                return Ok(n);
            }
        }
        // A file read never runs into a pinned range.
        let room = self
            .pinned
            .iter()
            .filter(|(at, _)| *at > pos)
            .map(|(at, _)| at - pos)
            .min()
            .unwrap_or(u64::MAX);
        let n = (buf.len() as u64).min(room) as usize;
        self.inner.read(&mut buf[..n])
    }
}

impl Seek for Src {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        match to {
            SeekFrom::End(d) => {
                let p = self.end.checked_add_signed(d).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "seek before the start")
                })?;
                self.inner.seek(SeekFrom::Start(p))
            }
            to => self.inner.seek(to),
        }
    }
}

// ---- the header walk (A-4, P3 2.6) -------------------------------------------------------

/// A coder of a folder, as the walk keeps it for a compressed header.
#[derive(Clone, Debug, Default)]
struct Coder {
    id: Vec<u8>,
    props: Vec<u8>,
    simple: bool,
}

/// The first folder of a StreamsInfo: what decodes a compressed header.
#[derive(Clone, Debug, Default)]
struct Folder {
    coders: Vec<Coder>,
    sizes: Vec<u64>,
    crc: Option<u32>,
}

/// What the walk keeps of a StreamsInfo: where the first pack stream is, and the first
/// folder. Everything else it only counts.
#[derive(Debug, Default)]
struct Streams {
    pack_pos: u64,
    first_pack: Option<u64>,
    first: Option<Folder>,
}

/// The names that are not valid UTF-16 (P3 3.2): the offsets of their unpaired surrogates in
/// the header, and their files.
#[derive(Debug, Default, PartialEq, Eq)]
struct BadNames {
    units: Vec<usize>,
    files: Vec<usize>,
}

/// A walk over header bytes: every read is bounds-checked, and every count the crate sizes
/// a table from is bounded by the header's length and by the index's entry bound.
struct Walk<'a> {
    b: &'a [u8],
    at: usize,
    limits: Limits,
}

impl<'a> Walk<'a> {
    fn new(b: &'a [u8], limits: Limits) -> Walk<'a> {
        Walk { b, at: 0, limits }
    }

    fn u8(&mut self) -> Result<u8, Fail> {
        let v = *self.b.get(self.at).ok_or(Fail::Damaged)?;
        self.at += 1;
        Ok(v)
    }

    fn bytes(&mut self, n: u64) -> Result<&'a [u8], Fail> {
        if n > (self.b.len() - self.at) as u64 {
            return Err(Fail::Damaged);
        }
        let s = &self.b[self.at..self.at + n as usize];
        self.at += n as usize;
        Ok(s)
    }

    fn u32(&mut self) -> Result<u32, Fail> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// A 7z NUMBER: the leading one bits of the first byte count the bytes that follow.
    fn num(&mut self) -> Result<u64, Fail> {
        let first = self.u8()? as u64;
        let mut mask = 0x80u64;
        let mut v = 0u64;
        for i in 0..8 {
            if first & mask == 0 {
                return Ok(v | ((first & (mask - 1)) << (8 * i)));
            }
            v |= (self.u8()? as u64) << (8 * i);
            mask >>= 1;
        }
        Ok(v)
    }

    /// A count the crate sizes a table from: at most the header's length, as the crate
    /// checks too, and at most the entry bound (P3 2.6).
    fn count(&mut self) -> Result<usize, Fail> {
        let n = self.num()?;
        if n > self.b.len() as u64 {
            return Err(Fail::Damaged);
        }
        if n > self.limits.entries as u64 {
            return Err(Fail::Full(self.limits.entries_message()));
        }
        Ok(n as usize)
    }

    /// `n` flags, eight to a byte, the first in the high bit.
    fn bits(&mut self, n: usize) -> Result<Vec<bool>, Fail> {
        let b = self.bytes(n.div_ceil(8) as u64)?;
        Ok((0..n).map(|i| b[i / 8] & (0x80 >> (i % 8)) != 0).collect())
    }

    /// A digest list of `n` items: which are defined, then a CRC32 for each defined one.
    fn digests(&mut self, n: usize) -> Result<Vec<Option<u32>>, Fail> {
        let defined = if self.u8()? != 0 {
            vec![true; n]
        } else {
            self.bits(n)?
        };
        defined
            .into_iter()
            .map(|d| if d { self.u32().map(Some) } else { Ok(None) })
            .collect()
    }

    /// One folder: its output stream count, and its coders when `keep`.
    fn folder(&mut self, keep: bool) -> Result<(u64, Vec<Coder>), Fail> {
        let n = self.num()?;
        if n == 0 || n > MAX_CODERS {
            return Err(Fail::Damaged);
        }
        let (mut ins, mut outs) = (0u64, 0u64);
        let mut coders = Vec::new();
        for _ in 0..n {
            let flags = self.u8()?;
            if flags & 0x80 != 0 {
                // Alternative methods: neither 7-Zip nor the crate reads them.
                return Err(Fail::Unsupported);
            }
            let id = self.bytes((flags & 0x0f) as u64)?;
            let simple = flags & 0x10 == 0;
            let (i, o) = if simple {
                (1, 1)
            } else {
                (self.num()?, self.num()?)
            };
            if i > MAX_CODERS || o > MAX_CODERS {
                return Err(Fail::Damaged);
            }
            ins += i;
            outs += o;
            let props = if flags & 0x20 != 0 {
                let len = self.num()?;
                if len > MAX_PROPS {
                    return Err(Fail::Damaged);
                }
                self.bytes(len)?
            } else {
                &[][..]
            };
            if keep {
                coders.push(Coder {
                    id: id.to_vec(),
                    props: props.to_vec(),
                    simple,
                });
            }
        }
        if outs == 0 || ins + 1 < outs {
            return Err(Fail::Damaged);
        }
        // The bind pairs, then the packed streams' indices when there are several.
        for _ in 0..outs - 1 {
            self.num()?;
            self.num()?;
        }
        let packed = ins + 1 - outs;
        if packed > 1 {
            for _ in 0..packed {
                self.num()?;
            }
        }
        Ok((outs, coders))
    }

    /// A StreamsInfo, in the order and with the optional parts the crate reads.
    fn streams(&mut self) -> Result<Streams, Fail> {
        let mut s = Streams::default();
        let mut nid = self.u8()?;
        if nid == K_PACK_INFO {
            s.pack_pos = self.num()?;
            let n = self.count()?;
            nid = self.u8()?;
            if nid == K_SIZE {
                for i in 0..n {
                    let size = self.num()?;
                    if i == 0 {
                        s.first_pack = Some(size);
                    }
                }
                nid = self.u8()?;
            }
            if nid == K_CRC {
                self.digests(n)?;
                nid = self.u8()?;
            }
            if nid != K_END {
                return Err(Fail::Damaged);
            }
            nid = self.u8()?;
        }
        // Per folder: its output streams and whether it has a CRC.
        let mut outs: Vec<u16> = Vec::new();
        let mut crcs: Vec<bool> = Vec::new();
        if nid == K_UNPACK_INFO {
            if self.u8()? != K_FOLDER {
                return Err(Fail::Damaged);
            }
            let n = self.count()?;
            if self.u8()? != 0 {
                // Folders stored elsewhere: the crate reads none.
                return Err(Fail::Unsupported);
            }
            outs.reserve_exact(n);
            for f in 0..n {
                let (o, coders) = self.folder(f == 0)?;
                // At most MAX_CODERS coders of MAX_CODERS streams each.
                outs.push(o as u16);
                if f == 0 {
                    s.first = Some(Folder {
                        coders,
                        ..Folder::default()
                    });
                }
            }
            if self.u8()? != K_CODERS_UNPACK_SIZE {
                return Err(Fail::Damaged);
            }
            for (f, &o) in outs.iter().enumerate() {
                for _ in 0..o {
                    let size = self.num()?;
                    if f == 0
                        && let Some(first) = &mut s.first
                    {
                        first.sizes.push(size);
                    }
                }
            }
            crcs = vec![false; n];
            nid = self.u8()?;
            if nid == K_CRC {
                let d = self.digests(n)?;
                for (c, d) in crcs.iter_mut().zip(&d) {
                    *c = d.is_some();
                }
                if let Some(first) = &mut s.first {
                    first.crc = d.first().copied().flatten();
                }
                nid = self.u8()?;
            }
            if nid != K_END {
                return Err(Fail::Damaged);
            }
            nid = self.u8()?;
        }
        if nid == K_SUB_STREAMS_INFO {
            let mut per = vec![1u64; outs.len()];
            nid = self.u8()?;
            if nid == K_NUM_UNPACK_STREAM {
                let mut total = 0u64;
                for p in per.iter_mut() {
                    *p = self.num()?;
                    total = total.saturating_add(*p);
                    if total > self.b.len() as u64 {
                        return Err(Fail::Damaged);
                    }
                    if total > self.limits.entries as u64 {
                        return Err(Fail::Full(self.limits.entries_message()));
                    }
                }
                nid = self.u8()?;
            }
            if nid == K_SIZE {
                for &p in &per {
                    for _ in 1..p {
                        self.num()?;
                    }
                }
                nid = self.u8()?;
            }
            let digests: u64 = per
                .iter()
                .zip(&crcs)
                .map(|(&p, &c)| if p == 1 && c { 0 } else { p })
                .sum();
            if nid == K_CRC {
                self.digests(digests as usize)?;
                nid = self.u8()?;
            }
            if nid != K_END {
                return Err(Fail::Damaged);
            }
            nid = self.u8()?;
        }
        if nid != K_END {
            return Err(Fail::Damaged);
        }
        Ok(s)
    }

    /// A plain header: its counts, and the names that are not valid UTF-16.
    fn header(&mut self) -> Result<BadNames, Fail> {
        let mut bad = BadNames::default();
        if self.u8()? != K_HEADER {
            return Err(Fail::Damaged);
        }
        let mut nid = self.u8()?;
        if nid == K_ARCHIVE_PROPERTIES {
            while self.u8()? != K_END {
                let n = self.num()?;
                self.bytes(n)?;
            }
            nid = self.u8()?;
        }
        if nid == K_ADDITIONAL_STREAMS_INFO {
            return Err(Fail::Unsupported);
        }
        if nid == K_MAIN_STREAMS_INFO {
            self.streams()?;
            nid = self.u8()?;
        }
        if nid == K_FILES_INFO {
            self.count()?;
            loop {
                let t = self.u8()?;
                if t == K_END {
                    break;
                }
                let size = self.num()?;
                let at = self.at;
                let body = self.bytes(size)?;
                if t == K_NAME {
                    Self::names(at, body, &mut bad);
                }
            }
        }
        Ok(bad)
    }

    /// The names property (its body at offset `at`): an external flag, then each name's
    /// UTF-16LE units up to a zero unit. A surrogate without its pair is noted.
    fn names(at: usize, body: &[u8], bad: &mut BadNames) {
        if body.first() != Some(&0) {
            // Names stored elsewhere: the crate refuses the header.
            return;
        }
        let unit = |i: usize| body.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let (mut i, mut file, mut broken) = (1, 0, false);
        while let Some(u) = unit(i) {
            match u {
                0 => {
                    if broken {
                        bad.files.push(file);
                    }
                    (file, broken) = (file + 1, false);
                }
                0xd800..=0xdbff if matches!(unit(i + 2), Some(0xdc00..=0xdfff)) => i += 2,
                0xd800..=0xdfff => {
                    bad.units.push(at + i);
                    broken = true;
                }
                _ => {}
            }
            i += 2;
        }
    }
}

/// An LZMA2 dictionary from its property byte (as in xz); `None` when invalid.
fn lzma2_dict(p: Option<u8>) -> Option<u64> {
    match p? {
        p if p > 40 => None,
        40 => Some(u32::MAX as u64),
        p => Some((2 | (p & 1) as u64) << (p / 2 + 11)),
    }
}

/// Decodes a compressed header of one coder under the bounds of A-4. A header chain of
/// several coders, which no writer makes, is not read.
fn decode_header(src: &mut Src, len: u64, s: &Streams) -> Result<Vec<u8>, Fail> {
    let f = s.first.as_ref().ok_or(Fail::Damaged)?;
    if f.coders.iter().any(|c| c.id == ID_AES) {
        return Err(Fail::Encrypted);
    }
    let [c] = &f.coders[..] else {
        return Err(Fail::Unsupported);
    };
    if !c.simple {
        return Err(Fail::Unsupported);
    }
    let size = *f.sizes.first().ok_or(Fail::Damaged)?;
    let pack = s.first_pack.ok_or(Fail::Damaged)?;
    if size > HEADER_MAX || pack > HEADER_MAX + (HEADER_MAX >> 6) {
        return Err(Fail::Memory);
    }
    let at = 32u64
        .checked_add(s.pack_pos)
        .filter(|a| a.checked_add(pack).is_some_and(|end| end <= len))
        .ok_or(Fail::Damaged)?;
    let mut packed = vec![0u8; pack as usize];
    src.read_exact_at(at, &mut packed)?;
    let input = &packed[..];
    let mut r: Box<dyn Read + '_> = match &c.id[..] {
        ID_COPY => Box::new(input),
        ID_LZMA => {
            let p = &c.props;
            if p.len() < 5 {
                return Err(Fail::Damaged);
            }
            // lzma-rust2 sizes the dictionary to the output at most, which HEADER_MAX bounds.
            let dict = u32::from_le_bytes([p[1], p[2], p[3], p[4]]);
            Box::new(
                lzma_rust2::LzmaReader::new_with_props(input, size, p[0], dict, None)
                    .map_err(|_| Fail::Damaged)?,
            )
        }
        ID_LZMA2 => {
            let dict = lzma2_dict(c.props.first().copied()).ok_or(Fail::Damaged)?;
            if dict > DICT_MAX {
                return Err(Fail::Memory);
            }
            Box::new(lzma_rust2::Lzma2Reader::new(input, dict as u32, None))
        }
        ID_DEFLATE => Box::new(flate2::read::DeflateDecoder::new(input)),
        ID_BZIP2 => Box::new(bzip2::read::BzDecoder::new(input)),
        _ => return Err(Fail::Unsupported),
    };
    let mut out = Vec::new();
    r.as_mut()
        .take(size)
        .read_to_end(&mut out)
        .map_err(|_| Fail::Damaged)?;
    drop(r);
    if out.len() as u64 != size || f.crc.is_some_and(|c| c != crc32(&out)) {
        return Err(Fail::Damaged);
    }
    Ok(out)
}

/// A crate error as the reason a load or a block fails.
fn classify(e: &sevenz_rust2::Error) -> Fail {
    use sevenz_rust2::Error as E;
    match e {
        E::Unsupported(_) | E::UnsupportedCompressionMethod(_) | E::ExternalUnsupported => {
            Fail::Unsupported
        }
        E::PasswordRequired | E::MaybeBadPassword(_) => Fail::Encrypted,
        E::MaxMemLimited { .. } => Fail::Memory,
        _ => Fail::Damaged,
    }
}

/// The members of block `b`, as the crate counts them.
fn block_len(archive: &Archive, b: usize) -> usize {
    let pw = Password::empty();
    let mut none = io::Cursor::new(&[][..]);
    BlockDecoder::new(1, b, archive, &pw, &mut none).entry_count()
}

/// Reads a 7z's header (P3 3.2): the signature header and its CRC, the next header within
/// [`HEADER_MAX`] and its CRC, a compressed header decoded under the caps of A-4, and the
/// walk of its counts and names; the crate then parses that plain header. An archive
/// without members has an empty next header, which the crate does not read.
pub fn read_archive(src: &mut Src, len: u64, limits: Limits) -> Result<Archive, Fail> {
    read_header(src, len, limits).map(|(a, _)| a)
}

/// [`read_archive`], with the files whose names are not valid UTF-16.
fn read_header(src: &mut Src, len: u64, limits: Limits) -> Result<(Archive, Vec<usize>), Fail> {
    if len < 32 {
        return Err(Fail::Damaged);
    }
    let mut sig = [0u8; 32];
    src.read_exact_at(0, &mut sig)?;
    let le32 = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let le64 = |b: &[u8]| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
    if !sig.starts_with(SEVEN_Z) {
        return Err(Fail::Damaged);
    }
    if sig[6] != 0 {
        return Err(Fail::Unsupported);
    }
    if crc32(&sig[12..32]) != le32(&sig[8..12]) {
        return Err(Fail::Damaged);
    }
    let (offset, size, crc) = (le64(&sig[12..20]), le64(&sig[20..28]), le32(&sig[28..32]));
    if size == 0 {
        return Ok((Archive::default(), Vec::new()));
    }
    if size > HEADER_MAX {
        return Err(Fail::Memory);
    }
    let at = 32u64
        .checked_add(offset)
        .filter(|a| a.checked_add(size).is_some_and(|end| end <= len))
        .ok_or(Fail::Damaged)?;
    let mut raw = vec![0u8; size as usize];
    src.read_exact_at(at, &mut raw)?;
    if crc32(&raw) != crc {
        return Err(Fail::Damaged);
    }
    let mut plain = if raw[0] == K_ENCODED_HEADER {
        let s = Walk::new(&raw[1..], limits).streams()?;
        decode_header(src, len, &s)?
    } else {
        raw
    };
    let bad = Walk::new(&plain, limits).header()?;
    for &u in &bad.units {
        plain[u..u + 2].copy_from_slice(&0xfffdu16.to_le_bytes());
    }
    // The crate parses the walked header, placed right after the end of the file, through a
    // signature header that points at it.
    let mut start = [0u8; 32];
    start[..8].copy_from_slice(&sig[..8]);
    start[12..20].copy_from_slice(&(len - 32).to_le_bytes());
    start[20..28].copy_from_slice(&(plain.len() as u64).to_le_bytes());
    start[28..32].copy_from_slice(&crc32(&plain).to_le_bytes());
    let start_crc = crc32(&start[12..32]);
    start[8..12].copy_from_slice(&start_crc.to_le_bytes());
    src.pin(0, Arc::from(&start[..]));
    src.pin(len, plain.into());
    let parsed = src
        .seek(SeekFrom::Start(0))
        .map_err(sevenz_rust2::Error::from)
        .and_then(|_| Archive::read(src, &Password::empty()));
    src.unpin();
    let archive = parsed.map_err(|e| match src.cancelled() {
        true => Fail::Cancelled,
        false => classify(&e),
    })?;
    if archive.files.len() > limits.entries {
        return Err(Fail::Full(limits.entries_message()));
    }
    // Every block's members lie inside the file list, and each streamed file has a block,
    // so no index into the crate's tables is out of range.
    let mut streamed = 0usize;
    for b in 0..archive.blocks.len() {
        let n = block_len(&archive, b);
        let start = archive.stream_map.block_first_file_index.get(b).copied();
        if n > 0 && start.is_none_or(|s| s.checked_add(n).is_none_or(|e| e > archive.files.len())) {
            return Err(Fail::Damaged);
        }
        streamed += n;
    }
    let files = archive.files.iter().filter(|f| f.has_stream).count();
    if streamed != files {
        return Err(Fail::Damaged);
    }
    let mut archive = archive;
    let mut bad = bad.files;
    canonical(&mut archive, &mut bad);
    Ok((archive, bad))
}

/// The crate decodes a block's members as a run of the file list, from the block's first
/// file. 7z lets a file without data stand between two members of one block; the crate
/// would then take it for the block's next member and never reach the last one. Such a list
/// is put in order: the files with data first, in their order, then the others; the index
/// and every later read use that order (locators are its indices). A list whose runs hold
/// only files with data stays as it is.
fn canonical(archive: &mut Archive, bad: &mut [usize]) {
    let counts: Vec<usize> = (0..archive.blocks.len())
        .map(|b| block_len(archive, b))
        .collect();
    let first = &archive.stream_map.block_first_file_index;
    let interleaved = counts.iter().enumerate().any(|(b, &n)| {
        n > 0
            && archive.files[first[b]..first[b] + n]
                .iter()
                .any(|f| !f.has_stream)
    });
    if !interleaved {
        return;
    }
    let mut order: Vec<usize> = (0..archive.files.len()).collect();
    order.sort_by_key(|&i| !archive.files[i].has_stream);
    let mut moved = vec![0usize; order.len()];
    for (new, &old) in order.iter().enumerate() {
        moved[old] = new;
    }
    let mut files: Vec<Option<ArchiveEntry>> = std::mem::take(&mut archive.files)
        .into_iter()
        .map(Some)
        .collect();
    archive.files = order.iter().filter_map(|&i| files[i].take()).collect();
    for b in bad.iter_mut() {
        *b = moved[*b];
    }
    let map = &mut archive.stream_map;
    map.file_block_index = vec![None; archive.files.len()];
    let mut next = 0;
    for (b, &n) in counts.iter().enumerate() {
        map.block_first_file_index[b] = next;
        for slot in &mut map.file_block_index[next..next + n] {
            *slot = Some(b);
        }
        next += n;
    }
}

// ---- entries ---------------------------------------------------------------------------

/// The kind of a 7z entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
    Symlink,
    Special,
    /// A file-type field no Unix system uses.
    Other,
}

/// An entry's kind and permission bits, as libarchive reads them: the Unix mode in the high
/// half of the attributes when the extension bit says so; else `0o755` with the directory
/// attribute and `0o644` without it, less the write bits when read-only. A 7z directory (no
/// data, not an empty file) is a directory whatever the mode says.
fn kind_of(e: &ArchiveEntry) -> (Kind, u32) {
    let attrs = if e.has_windows_attributes {
        e.windows_attributes
    } else {
        0
    };
    let mut mode = if attrs & UNIX_EXTENSION != 0 && attrs >> 16 != 0 {
        attrs >> 16
    } else {
        let base = if attrs & ATTR_DIRECTORY != 0 {
            S_IFDIR | 0o755
        } else {
            S_IFREG | 0o644
        };
        match attrs & ATTR_READONLY {
            0 => base,
            _ => base & !0o222,
        }
    };
    if e.is_directory {
        mode = mode & !S_IFMT | S_IFDIR;
    }
    let kind = match mode & S_IFMT {
        S_IFDIR => Kind::Dir,
        0 | S_IFREG => Kind::File,
        S_IFLNK => Kind::Symlink,
        S_IFCHR | S_IFBLK | S_IFIFO | S_IFSOCK => Kind::Special,
        _ => Kind::Other,
    };
    (kind, mode & 0o7777)
}

/// A Windows file time (100 ns since 1601) as Unix time.
fn nt_time(t: NtTime) -> Ts {
    const EPOCH: i128 = 116_444_736_000_000_000;
    let d = u64::from(t) as i128 - EPOCH;
    Ts {
        sec: d.div_euclid(10_000_000) as i64,
        nsec: (d.rem_euclid(10_000_000) * 100) as u32,
    }
}

/// The block that holds the data of streamed file `i`.
fn block_of(archive: &Archive, i: usize) -> Result<usize, String> {
    archive
        .stream_map
        .file_block_index
        .get(i)
        .copied()
        .flatten()
        .filter(|&b| b < archive.blocks.len())
        .ok_or_else(|| DAMAGED.to_string())
}

/// Whether a block has an AES coder: its members are encrypted (A-AR-7).
fn locked(block: &Block) -> bool {
    block.coders.iter().any(|c| c.encoder_method_id() == ID_AES)
}

/// Whether block `b` may decode here (A-4): no AES coder ("encrypted"), and its LZMA and
/// LZMA2 dictionaries together within [`DICT_MAX`]. An LZMA dictionary counts at most the
/// coder's output, as lzma-rust2 sizes it; an LZMA2 dictionary is allocated whole.
pub(crate) fn block_check(block: &Block) -> Result<(), String> {
    if locked(block) {
        return Err(ENCRYPTED_MEMBER.into());
    }
    let mut dict = 0u64;
    for c in &block.coders {
        let p = c.properties();
        match c.encoder_method_id() {
            ID_LZMA if p.len() >= 5 => {
                let d = u32::from_le_bytes([p[1], p[2], p[3], p[4]]) as u64;
                dict = dict.saturating_add(d.min(block.get_unpack_size_for_coder(c)));
            }
            ID_LZMA2 => dict = dict.saturating_add(lzma2_dict(p.first().copied()).unwrap_or(0)),
            _ => {}
        }
    }
    if dict > DICT_MAX {
        return Err(NEEDS_MEMORY.into());
    }
    Ok(())
}

/// Decodes block `b` once, from its start (P3 3.5): each member the crate presents goes to
/// `f` with its file index and its bytes, in block order. `f` returns `true` to go on; the
/// member's unread bytes are then decoded and discarded, so the next member starts at its
/// own first byte. `Err`: the block broke before `f` stopped it.
fn walk_block(
    archive: &Archive,
    b: usize,
    src: &mut Src,
    f: &mut dyn FnMut(usize, &mut dyn Read) -> bool,
) -> Result<(), String> {
    let start = *archive
        .stream_map
        .block_first_file_index
        .get(b)
        .ok_or(DAMAGED)?;
    let pw = Password::empty();
    let mut k = 0usize;
    let mut broke: Option<String> = None;
    let dec = BlockDecoder::new(1, b, archive, &pw, src);
    let r = dec.for_each_entries(&mut |_, r: &mut dyn Read| {
        let i = start + k;
        k += 1;
        let mut d = Decoded(r);
        if !f(i, &mut d) {
            return Ok(false);
        }
        match io::copy(&mut d, &mut io::sink()) {
            Ok(_) => Ok(true),
            Err(e) => {
                broke = Some(e.to_string());
                Err(sevenz_rust2::Error::Other("the block broke".into()))
            }
        }
    });
    match (r, broke) {
        (_, Some(why)) => Err(why),
        (Ok(_), None) => Ok(()),
        (Err(e), None) => Err(classify(&e).text()),
    }
}

/// The file index at `locator` when its entry is the regular file the index stored (A-5):
/// the same name after A-1's split and the same size; else "archive changed".
fn checked(archive: &Archive, locator: u64, expect: &Expect) -> Result<usize, String> {
    let i = usize::try_from(locator)
        .ok()
        .filter(|&i| i < archive.files.len())
        .ok_or(CHANGED_MEMBER)?;
    let e = &archive.files[i];
    let size = if e.has_stream { e.size } else { 0 };
    if kind_of(e).0 != Kind::File || !expect.matches(e.name.as_bytes(), size) {
        return Err(CHANGED_MEMBER.into());
    }
    Ok(i)
}

/// Lends the bytes of the member at `locator` to `f` (F3, F4, the quick view, P3 3.4; and
/// extraction by locator, P3 3.5): the entry there must be the one the index stored (A-5),
/// and its block decodes from its start, discarding the members before it. `decodes`
/// counts the block.
pub(crate) fn read_member<T>(
    archive: &Archive,
    src: &mut Src,
    locator: u64,
    expect: &Expect,
    decodes: Option<&AtomicU64>,
    f: &mut dyn FnMut(&mut dyn Read) -> T,
) -> Result<T, String> {
    let i = checked(archive, locator, expect)?;
    let e = &archive.files[i];
    if !e.has_stream || e.size == 0 {
        return Ok(f(&mut io::empty()));
    }
    let b = block_of(archive, i)?;
    block_check(&archive.blocks[b])?;
    if let Some(n) = decodes {
        n.fetch_add(1, Ordering::SeqCst);
    }
    let mut out = None;
    walk_block(archive, b, src, &mut |j, r| {
        if j == i {
            out = Some(f(r));
            return false;
        }
        true
    })?;
    out.ok_or_else(|| NOT_FOUND.into())
}

/// The one pass of a solid 7z (P3 3.5): `wanted` maps each wanted member's locator to its
/// key and what the index stored for it. Members that fail A-5 and empty members go first;
/// then each block that holds a wanted member decodes once, in block order, and `each` gets
/// its wanted members in block order, the others decoded and discarded; the block stops
/// after its last wanted member. A block that cannot decode fails its members with the
/// reason. `decodes` counts the blocks decoded (A-AR-5).
pub(crate) fn pass(
    src: &mut Src,
    archive: &Archive,
    wanted: &HashMap<u64, (u64, Expect)>,
    cancel: &AtomicBool,
    decodes: &AtomicU64,
    each: &mut EachMember<'_>,
) -> Result<(), String> {
    let mut locators: Vec<u64> = wanted.keys().copied().collect();
    locators.sort_unstable();
    let mut blocks: BTreeMap<usize, BTreeMap<usize, u64>> = BTreeMap::new();
    for loc in locators {
        let (key, expect) = &wanted[&loc];
        let at = checked(archive, loc, expect).and_then(|i| {
            let e = &archive.files[i];
            if !e.has_stream || e.size == 0 {
                return Ok(None);
            }
            Ok(Some((block_of(archive, i)?, i)))
        });
        let flow = match at {
            Err(why) => each(*key, Err(why)),
            Ok(None) => each(*key, Ok(&mut io::empty())),
            Ok(Some((b, i))) => {
                blocks.entry(b).or_default().insert(i, *key);
                Flow::Continue
            }
        };
        if flow == Flow::Stop {
            return Ok(());
        }
    }
    for (b, mut members) in blocks {
        let failed = match block_check(&archive.blocks[b]) {
            Ok(()) => {
                decodes.fetch_add(1, Ordering::SeqCst);
                let mut stop = false;
                let r = walk_block(archive, b, src, &mut |i, r| {
                    if let Some(key) = members.remove(&i)
                        && each(key, Ok(r)) == Flow::Stop
                    {
                        stop = true;
                        return false;
                    }
                    !members.is_empty()
                });
                if stop || cancel.load(Ordering::SeqCst) {
                    return Ok(());
                }
                r.err()
            }
            Err(why) => Some(why),
        };
        // Members the block never presented are left to the engine: "not found".
        if let Some(why) = failed {
            for key in members.into_values() {
                if each(key, Err(why.clone())) == Flow::Stop {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

// ---- the scan (P3 3.3) -------------------------------------------------------------------

/// Scans a 7z into `tree` (P3 3.3): the header lists every member at once, the rows streaming
/// to the panel as they go into the index; then each block that holds a symlink decodes
/// once, up to its last symlink, for the targets ([`read_links`]).
pub(crate) fn scan(ix: &ArchiveIndex, tree: &mut Tree, sink: &mut Sink<'_>) -> Result<(), Stop> {
    let len = ix.key.size;
    let mut src = Src::new(
        ix.file().clone(),
        len,
        ix.cancel.clone(),
        Some(ix.read.clone()),
    );
    let (archive, bad) = read_header(&mut src, len, tree.limits()).map_err(Fail::stop)?;
    let bad: HashSet<usize> = bad.into_iter().collect();
    ix.solid.store(archive.is_solid, Ordering::Relaxed);
    let locked: Vec<bool> = archive.blocks.iter().map(locked).collect();
    let mut links: HashMap<usize, NodeId> = HashMap::new();
    for (i, e) in archive.files.iter().enumerate() {
        if i % 256 == 0 && sink.cancelled() {
            return Err(Stop::Cancelled);
        }
        sink.poll(tree);
        if bad.contains(&i) {
            // Not valid UTF-16: bsdtar skips it too (A-1).
            tree.skip(UNSAFE_PATH);
            continue;
        }
        let (kind, mode) = kind_of(e);
        let size = if e.has_stream { e.size } else { 0 };
        let encrypted = e.has_stream && block_of(&archive, i).is_ok_and(|b| locked[b]);
        let member = match kind {
            _ if e.is_anti_item => None,
            Kind::Dir => Some(MemberKind::Dir),
            Kind::File => Some(MemberKind::File),
            Kind::Symlink if size > LINK_MAX => {
                tree.skip(LINK_TOO_LONG);
                continue;
            }
            Kind::Symlink => Some(MemberKind::Symlink(b"")),
            Kind::Special => Some(MemberKind::Special),
            Kind::Other => None,
        };
        let Some(member) = member else {
            // An anti-item (a deletion marker of an update) or an unknown file type.
            tree.skip(UNSUPPORTED);
            continue;
        };
        let added = tree
            .add(Member {
                name: e.name.as_bytes(),
                kind: member,
                mode,
                size,
                mtime: e
                    .has_last_modified_date
                    .then(|| nt_time(e.last_modified_date)),
                locator: i as u64,
                encrypted,
            })
            .map_err(|f| Stop::Full(f.0))?;
        if kind == Kind::Symlink
            && let Added::New(id) | Added::Replaced(id) = added
        {
            if encrypted || size == 0 {
                // Never decoded (A-AR-7), or no data: no target, as no symlink has an empty
                // one.
                tree.set_link_target(id, None);
            } else {
                links.insert(i, id);
            }
        }
        sink.added(tree, added);
    }
    if links.is_empty() {
        return Ok(());
    }
    // The rows go out before any block decodes (P-19: first rows at once).
    sink.flush();
    let read = read_links(&archive, &mut src, tree, sink, links);
    if sink.cancelled() {
        return Err(Stop::Cancelled);
    }
    sink.relink(tree);
    read
}

/// The symlink targets (P3 3.2): each block that holds a symlink decodes once, in block
/// order, up to its last symlink. A symlink whose target cannot be read gets none (it is
/// broken, and extraction refuses it), and the listing says why: "archive damaged", or the
/// memory cap.
fn read_links(
    archive: &Archive,
    src: &mut Src,
    tree: &mut Tree,
    sink: &mut Sink<'_>,
    links: HashMap<usize, NodeId>,
) -> Result<(), Stop> {
    let mut blocks: BTreeMap<usize, HashMap<usize, NodeId>> = BTreeMap::new();
    for (&i, &id) in &links {
        if let Ok(b) = block_of(archive, i) {
            blocks.entry(b).or_default().insert(i, id);
        }
    }
    let mut read: HashSet<usize> = HashSet::new();
    let mut stop = None;
    for (b, mut want) in blocks {
        if let Err(why) = block_check(&archive.blocks[b]) {
            stop.get_or_insert(if why == NEEDS_MEMORY {
                Stop::Memory
            } else {
                Stop::Damaged
            });
            continue;
        }
        let r = walk_block(archive, b, src, &mut |i, r| {
            // A panel that moves into another directory meanwhile is served (P3 3.3).
            sink.poll(tree);
            if let Some(id) = want.remove(&i) {
                let mut target = Vec::new();
                let whole = r.take(LINK_MAX + 1).read_to_end(&mut target).is_ok();
                // A later duplicate may have replaced the node (the last wins).
                if whole && target.len() as u64 <= LINK_MAX && tree.node(id).locator == i as u64 {
                    tree.set_link_target(id, Some(&target));
                    read.insert(i);
                }
            }
            !want.is_empty()
        });
        if r.is_err() {
            if src.cancelled() {
                return Err(Stop::Cancelled);
            }
            stop.get_or_insert(Stop::Damaged);
        }
    }
    for (i, id) in links {
        if !read.contains(&i) && tree.node(id).locator == i as u64 {
            tree.set_link_target(id, None);
            stop.get_or_insert(Stop::Damaged);
        }
    }
    stop.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(b: &[u8]) -> Walk<'_> {
        Walk::new(b, Limits::default())
    }

    /// 7z NUMBERs: the leading one bits of the first byte count the bytes that follow.
    #[test]
    fn numbers_decode_as_7z_writes_them() {
        for (bytes, v) in [
            (&[0x05][..], 5u64),
            (&[0x7f], 0x7f),
            (&[0x80, 0xc8], 200),
            (&[0x81, 0xd6], 470),
            (&[0x83, 0xff], 1023),
            (&[0xc0, 0x00, 0x01], 0x100),
            (&[0xff, 1, 2, 3, 4, 5, 6, 7, 8], 0x0807_0605_0403_0201),
        ] {
            assert_eq!(walk(bytes).num(), Ok(v), "{bytes:02x?}");
        }
        assert_eq!(walk(&[0x80]).num(), Err(Fail::Damaged), "cut short");
    }

    /// Counts are bounded by the header's length and by the entry bound (P3 2.6).
    #[test]
    fn counts_are_bounded() {
        // 5 files in a header of 2 bytes after it: more than its length.
        assert_eq!(walk(&[0x09, 0, 0]).count(), Err(Fail::Damaged));
        let small = Limits {
            entries: 3,
            ..Limits::default()
        };
        let body = [0x04u8; 8];
        let mut w = Walk::new(&body, small);
        assert_eq!(
            w.count(),
            Err(Fail::Full("listing stopped at 3 entries".into()))
        );
        // A file count far beyond the header: the crate would size a table from it.
        let mut h = vec![K_HEADER, K_FILES_INFO, 0xe0, 0x00, 0x00, 0x80];
        h.resize(64, 0);
        assert_eq!(walk(&h).header(), Err(Fail::Damaged));
    }

    /// Digests: all defined, or a bit vector first in the high bit.
    #[test]
    fn digests_and_bits() {
        let b = [0x01, 1, 0, 0, 0, 2, 0, 0, 0];
        assert_eq!(walk(&b).digests(2), Ok(vec![Some(1), Some(2)]));
        let b = [0x00, 0b0100_0000, 7, 0, 0, 0];
        assert_eq!(walk(&b).digests(3), Ok(vec![None, Some(7), None]));
    }

    /// Unpaired surrogates are noted by offset and file; a valid pair is not.
    #[test]
    fn names_that_are_not_utf16() {
        let units: Vec<u16> = vec![0x61, 0, 0xd83d, 0xde00, 0, 0x62, 0xd800, 0x63, 0, 0xdc00, 0];
        let mut body = vec![0u8];
        for u in &units {
            body.extend_from_slice(&u.to_le_bytes());
        }
        let mut bad = BadNames::default();
        Walk::names(100, &body, &mut bad);
        assert_eq!(bad.files, [2, 3]);
        assert_eq!(bad.units, [100 + 1 + 2 * 6, 100 + 1 + 2 * 9]);
    }

    #[test]
    fn lzma2_dictionaries() {
        assert_eq!(lzma2_dict(Some(16)), Some(1 << 20));
        assert_eq!(lzma2_dict(Some(17)), Some(3 << 19));
        assert_eq!(lzma2_dict(Some(32)), Some(256 << 20));
        assert_eq!(lzma2_dict(Some(40)), Some(u32::MAX as u64));
        assert_eq!(lzma2_dict(Some(41)), None);
    }

    #[test]
    fn windows_times_become_unix_times() {
        let t = nt_time(NtTime::from(116_444_736_000_000_000 + 17_000_000_005));
        assert_eq!(
            t,
            Ts {
                sec: 1_700,
                nsec: 500
            }
        );
        let before = nt_time(NtTime::from(116_444_736_000_000_000 - 5));
        assert_eq!(
            before,
            Ts {
                sec: -1,
                nsec: 999_999_500
            }
        );
    }

    #[test]
    fn kinds_and_modes_from_the_attributes() {
        let e = |attrs: Option<u32>, dir: bool| ArchiveEntry {
            has_windows_attributes: attrs.is_some(),
            windows_attributes: attrs.unwrap_or(0),
            is_directory: dir,
            ..ArchiveEntry::default()
        };
        let unix = |m: u32| Some((m << 16) | UNIX_EXTENSION);
        assert_eq!(kind_of(&e(unix(0o100640), false)), (Kind::File, 0o640));
        assert_eq!(kind_of(&e(unix(0o104755), false)), (Kind::File, 0o4755));
        assert_eq!(kind_of(&e(unix(0o120777), false)), (Kind::Symlink, 0o777));
        assert_eq!(kind_of(&e(unix(0o040700), true)), (Kind::Dir, 0o700));
        assert_eq!(kind_of(&e(unix(0o010644), false)), (Kind::Special, 0o644));
        assert_eq!(kind_of(&e(unix(0o170644), false)).0, Kind::Other);
        // A 7z directory is one whatever its mode says, as libarchive reads it.
        assert_eq!(kind_of(&e(unix(0o010644), true)), (Kind::Dir, 0o644));
        assert_eq!(kind_of(&e(None, true)), (Kind::Dir, 0o644));
        assert_eq!(kind_of(&e(Some(ATTR_DIRECTORY), true)), (Kind::Dir, 0o755));
        assert_eq!(kind_of(&e(Some(ATTR_READONLY), false)), (Kind::File, 0o444));
        assert_eq!(kind_of(&e(None, false)), (Kind::File, 0o644));
        // The extension bit without a mode: the defaults.
        assert_eq!(
            kind_of(&e(Some(UNIX_EXTENSION), false)),
            (Kind::File, 0o644)
        );
    }
}
