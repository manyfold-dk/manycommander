#![forbid(unsafe_code)]
//! Zip listing from the central directory (TDR-3 3.1, 3.2, 3.3).
//!
//! The `zip` crate reads the central directory through a [`PosReader`]; each entry's
//! metadata comes from `by_index_raw`, which reads nothing but the entry's local header.
//! Names are `name_raw`: the central-directory bytes, or the UTF-8 name of a valid Unicode
//! path extra field, which the crate puts in their place; CP437 is not decoded. Times come
//! from the extended-timestamp field when present, else from the DOS time as local time at
//! 2 s resolution. Encrypted entries are flagged: listed, never extracted. A symlink's
//! target is the entry's content, read up to `PATH_MAX` bytes.
//!
//! The crate materializes the whole central directory, so the entry count of the end record
//! it will use is checked first (`central`): an archive that declares more than the index
//! bound is not listed at all, and a member read's later parse of it is refused the same way
//! (TDR-3 2.6, 3.2).
//!
//! The crate's xz and LZMA decoders allocate the dictionary an entry declares, up to 4 GiB,
//! before its first byte. Those two methods decode here instead (`read_entry`), from the
//! entry's raw bytes under the caps of A-4; every other method decodes in the crate (zstd
//! within libzstd's default window limit, which is A-4's cap).

use super::extract::{Decoded, ENCRYPTED_MEMBER, UNSUPPORTED_METHOD};
use super::index::{DAMAGED_MEMBER, LINK_TOO_LONG, Limits, Member, MemberKind, Tree};
use super::tar::XzCap;
use super::{
    ArchiveIndex, DAMAGED, NEEDS_MEMORY, PosReader, S_IFDIR, S_IFLNK, S_IFMT, S_IFREG, Sink, Stop,
    XZ_MEMORY_MAX, read_target,
};
use crate::fsops::sys::{Sys, Ts};
use std::cell::Cell;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::ops::ControlFlow;
use std::os::unix::fs::FileExt;
use std::rc::Rc;
use zip::read::ZipFile;
use zip::result::ZipError;
use zip::{CompressionMethod, ExtraField, ZipArchive};

const EOCD: &[u8; 4] = b"PK\x05\x06";
const EOCD64: &[u8; 4] = b"PK\x06\x06";
const LOCATOR64: &[u8; 4] = b"PK\x06\x07";
const CENTRAL_HEADER: &[u8; 4] = b"PK\x01\x02";

/// `n` bytes of the file at `at`; `None` when it has fewer.
fn read_at(file: &File, at: u64, n: usize) -> Option<Vec<u8>> {
    let mut b = vec![0u8; n];
    file.read_exact_at(&mut b, at).ok()?;
    Some(b)
}

fn le16(b: &[u8], at: usize) -> u64 {
    u16::from_le_bytes([b[at], b[at + 1]]) as u64
}

fn le32(b: &[u8], at: usize) -> u64 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as u64
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap_or_default())
}

/// Each place `magic` lies whole in `[from, to)` of the file, in order or last first, to `f`
/// until it breaks. A window that cannot be read ends the search, as it ends the crate's.
fn search<T>(
    file: &File,
    magic: &[u8; 4],
    (from, to): (u64, u64),
    back: bool,
    f: &mut dyn FnMut(u64) -> ControlFlow<T>,
) -> Option<T> {
    const WINDOW: u64 = 64 << 10;
    let (mut start, mut end) = (from, to);
    while start < end {
        let (lo, hi) = if back {
            (end.saturating_sub(WINDOW).max(from), end)
        } else {
            (start, start.saturating_add(WINDOW).min(to))
        };
        let buf = read_at(file, lo, (hi - lo) as usize)?;
        let hits: Box<dyn Iterator<Item = usize>> = if back {
            Box::new(memchr::memmem::rfind_iter(&buf, magic))
        } else {
            Box::new(memchr::memmem::find_iter(&buf, magic))
        };
        for i in hits {
            if let ControlFlow::Break(t) = f(lo + i as u64) {
                return Some(t);
            }
        }
        // The next window overlaps by three bytes: a magic across the edge lies whole in
        // it, and one found here does not.
        match back {
            true if lo > from => end = lo + 3,
            false if hi < to => start = hi - 3,
            _ => break,
        }
    }
    None
}

/// How far before the end of the file the `zip` crate looks for an end record: its
/// `size_of::<Zip32CentralDirectoryEnd>()` (32 bytes on a 64-bit target), the magic, and the
/// 64 KiB comment limit. The entry-cap test of `tests/archive_list.rs` pins the edge.
const EOCD_REACH: u64 = 32 + 4 + (1 << 16);

/// How many entries the `zip` crate reads into memory from this archive (TDR-3 2.6), from the
/// end record it uses, found as the crate finds it: the last end-of-central-directory record
/// within `EOCD_REACH` of the end that passes its checks, a zip64 one when any field says so
/// and a locator precedes it, its zip64 record the first from where the locator points that
/// passes them. `None` when the crate finds none and fails.
///
/// When the crate cannot read the central directory of that record, it tries the records
/// before it, unless the directory ends where the record starts; this check does not follow
/// it there, which would read the whole directory at every open.
fn declared_entries(file: &File, len: u64) -> Option<u64> {
    search(
        file,
        EOCD,
        (len.saturating_sub(EOCD_REACH), len),
        true,
        &mut |p| match end_record(file, len, p) {
            Some(n) => ControlFlow::Break(n),
            None => ControlFlow::Continue(()),
        },
    )
}

/// The entries the end record at `p` makes the crate read; `None` when the crate goes on to
/// the records before it, as it does after a check fails, before it reads any entry.
fn end_record(file: &File, len: u64, p: u64) -> Option<u64> {
    let r = read_at(file, p, 22)?;
    if p + 22 + le16(&r, 20) > len {
        return None;
    }
    let (disk, disk_cd) = (le16(&r, 4), le16(&r, 6));
    let (on_disk, total) = (le16(&r, 8), le16(&r, 10));
    let (cd_size, cd_off) = (le32(&r, 12), le32(&r, 16));
    let zip64 = total == 0xffff || cd_size == 0xffff_ffff || cd_off == 0xffff_ffff;
    let locator = p
        .checked_sub(20)
        .filter(|_| zip64)
        .and_then(|at| Some((at, read_at(file, at, 20)?)))
        .filter(|(_, l)| l.starts_with(LOCATOR64));
    let Some((at, l)) = locator else {
        // The crate takes only a record of one disk (disk 0, its count the total), then one
        // of no entries whose directory does not start after it at once, and one whose
        // central directory has a header where it points or after; it reads `total` entries.
        if disk != 0 || disk_cd != 0 || on_disk != total {
            return None;
        }
        let found = if total == 0 {
            cd_off <= p
        } else {
            cd_off < p
                && search(file, CENTRAL_HEADER, (cd_off, p), false, &mut |_| {
                    ControlFlow::Break(())
                })
                .is_some()
        };
        return found.then_some(total);
    };
    let (disk_cd, eocd64, disks) = (le32(&l, 4), le64(&l, 8), le32(&l, 16));
    if eocd64 >= at || disks > 1 {
        return None;
    }
    search(file, EOCD64, (eocd64, at), false, &mut |q| {
        let Some(r) = read_at(file, q, 56) else {
            return ControlFlow::Continue(());
        };
        let size = le64(&r, 4);
        let (n, cd) = (le64(&r, 32), le64(&r, 48));
        // One disk only: disk 0, its count the total.
        if size < 40
            || size.saturating_add(12) != at - q
            || le32(&r, 20) != disk_cd
            || le32(&r, 16) != 0
            || le64(&r, 24) != n
            || q < n.saturating_mul(46).saturating_add(cd)
        {
            return ControlFlow::Continue(());
        }
        // The crate takes this record, then fails on a central directory on another disk.
        ControlFlow::Break((disk_cd == 0).then_some(n))
    })
    .flatten()
}

/// Why a central directory was not read ([`central`]).
pub(crate) enum NotRead {
    /// Its end record declares more entries than the index allows (TDR-3 2.6).
    TooMany,
    /// The crate could not read it.
    Damaged,
}

/// The central directory over `reader`, read only when the end record the crate will use
/// declares at most `limits.entries` entries: the crate sizes its entry list from that count
/// before it reads an entry (TDR-3 2.6, E-6). The scan and every later parse (TDR-3 3.2) read
/// it through here. `sys` consults the step `archive.central` just before the crate reads.
pub(crate) fn central(
    file: &File,
    len: u64,
    reader: PosReader,
    limits: Limits,
    sys: &Sys,
) -> Result<ZipArchive<PosReader>, NotRead> {
    if declared_entries(file, len).is_some_and(|n| n > limits.entries as u64) {
        return Err(NotRead::TooMany);
    }
    sys.hit("archive.central").map_err(|_| NotRead::Damaged)?;
    ZipArchive::new(reader).map_err(|_| NotRead::Damaged)
}

/// The DOS date and time as local time in `tz` (TDR-3 3.2).
fn dos_time(d: zip::DateTime, tz: &jiff::tz::TimeZone) -> Option<Ts> {
    let dt = jiff::civil::DateTime::new(
        d.year() as i16,
        d.month() as i8,
        d.day() as i8,
        d.hour() as i8,
        d.minute() as i8,
        d.second() as i8,
        0,
    )
    .ok()?;
    let t = dt.to_zoned(tz.clone()).ok()?.timestamp();
    Some(Ts {
        sec: t.as_second(),
        nsec: 0,
    })
}

/// Scans a zip's central directory into `tree` (TDR-3 3.3).
pub(crate) fn scan(
    ix: &ArchiveIndex,
    tree: &mut Tree,
    sink: &mut Sink<'_>,
    tz: &jiff::tz::TimeZone,
) -> Result<(), Stop> {
    let limits: Limits = tree.limits();
    let reader = PosReader::new(ix.file().clone(), ix.key.size)
        .window(4096)
        .progress(ix.read.clone());
    let mut za =
        central(ix.file(), ix.key.size, reader, limits, &Sys::default()).map_err(|e| match e {
            NotRead::TooMany => Stop::Full(limits.entries_message()),
            NotRead::Damaged => Stop::Fatal(DAMAGED.into()),
        })?;
    for i in 0..za.len() {
        if sink.cancelled() {
            return Err(Stop::Cancelled);
        }
        sink.poll(tree);
        let (name, mode, is_dir_name, size, encrypted, mtime) = match za.by_index_raw(i) {
            Ok(f) => {
                let ext = f.extra_data_fields().find_map(|x| match x {
                    ExtraField::ExtendedTimestamp(t) => t.mod_time(),
                    _ => None,
                });
                let mtime = match ext {
                    Some(s) => Some(Ts {
                        sec: s as i64,
                        nsec: 0,
                    }),
                    None => f.last_modified().and_then(|d| dos_time(d, tz)),
                };
                (
                    f.name_raw().to_vec(),
                    f.unix_mode(),
                    f.name_raw().ends_with(b"/"),
                    f.size(),
                    f.encrypted(),
                    mtime,
                )
            }
            Err(_) => {
                tree.skip(DAMAGED_MEMBER);
                continue;
            }
        };
        let kind_bits = mode.map_or(0, |m| m & S_IFMT);
        let target;
        let kind = if is_dir_name || kind_bits == S_IFDIR {
            MemberKind::Dir
        } else if kind_bits == S_IFLNK {
            target = if encrypted {
                Vec::new()
            } else {
                match read_link(&mut za, i) {
                    Some(t) => t,
                    None => {
                        tree.skip(LINK_TOO_LONG);
                        continue;
                    }
                }
            };
            MemberKind::Symlink(&target)
        } else if kind_bits == 0 || kind_bits == S_IFREG {
            MemberKind::File
        } else {
            MemberKind::Special
        };
        let default_mode = if matches!(kind, MemberKind::Dir) {
            0o755
        } else {
            0o644
        };
        let added = tree
            .add(Member {
                name: &name,
                kind,
                mode: mode.map_or(default_mode, |m| m & 0o7777),
                size,
                mtime,
                locator: i as u64,
                encrypted,
            })
            .map_err(|f| Stop::Full(f.0))?;
        sink.added(tree, added);
    }
    Ok(())
}

/// A symlink entry's target: its content, at most `PATH_MAX` bytes. `None` when longer. A
/// target that cannot be read is empty; the entry stays listed.
fn read_link(za: &mut ZipArchive<PosReader>, i: usize) -> Option<Vec<u8>> {
    match read_entry(za, i, &|_| Ok(()), &mut |r| read_target(r)) {
        Ok(Ok(target)) => target,
        _ => Some(Vec::new()),
    }
}

/// Opens zip entry `i`, lets `accept` check it (A-5), and lends its decoded bytes to `f`.
/// The crate decodes every method but xz and LZMA, whose dictionaries it would allocate as
/// the entry declares them: those decode here ([`Capped`]). An error in the bytes reads as
/// "archive damaged", or as the memory cap. `Err`: why the entry cannot be read.
pub(crate) fn read_entry<T>(
    za: &mut ZipArchive<PosReader>,
    i: usize,
    accept: &dyn Fn(&ZipFile<'_, PosReader>) -> Result<(), String>,
    f: &mut dyn FnMut(&mut dyn Read) -> T,
) -> Result<T, String> {
    let refused = |e: ZipError| -> String {
        match e {
            ZipError::UnsupportedArchive(m) if m == ZipError::PASSWORD_REQUIRED => {
                ENCRYPTED_MEMBER.into()
            }
            ZipError::UnsupportedArchive(_) | ZipError::CompressionMethodNotSupported(_) => {
                UNSUPPORTED_METHOD.into()
            }
            _ => DAMAGED.into(),
        }
    };
    let raw = za.by_index_raw(i).map_err(refused)?;
    accept(&raw)?;
    if !raw.encrypted()
        && matches!(
            raw.compression(),
            CompressionMethod::Xz | CompressionMethod::Lzma
        )
    {
        let mut r = Capped::new(raw)?;
        return Ok(f(&mut r));
    }
    drop(raw);
    let mut zf = za.by_index(i).map_err(refused)?;
    Ok(f(&mut Decoded(&mut zf)))
}

/// An xz or LZMA entry decoded here, under the caps of A-4: the xz container behind an
/// [`XzCap`], which refuses a block's dictionary above the cap before the decoder allocates
/// it, and the one LZMA dictionary checked before the decoder starts. The CRC-32 is checked
/// at the end, as the crate checks it.
struct Capped<'a> {
    inner: Box<dyn Read + 'a>,
    memory: Rc<Cell<bool>>,
    crc: flate2::Crc,
    want: u32,
}

impl<'a> Capped<'a> {
    fn new(raw: ZipFile<'a, PosReader>) -> Result<Capped<'a>, String> {
        let (size, want, method) = (raw.size(), raw.crc32(), raw.compression());
        let memory = Rc::new(Cell::new(false));
        let mut raw = BufReader::new(raw);
        let inner: Box<dyn Read + 'a> = if method == CompressionMethod::Xz {
            Box::new(lzma_rust2::XzReader::new(
                XzCap::new(raw, memory.clone()),
                false,
            ))
        } else {
            // The zip LZMA header: a version, the properties' size (5), then the
            // properties byte and the dictionary size.
            let mut head = [0u8; 9];
            raw.read_exact(&mut head).map_err(|_| DAMAGED.to_string())?;
            if head[2..4] != [5, 0] {
                return Err(DAMAGED.into());
            }
            let dict = u32::from_le_bytes([head[5], head[6], head[7], head[8]]);
            // lzma-rust2 sizes the dictionary to the declared output at most.
            let need = match size {
                s if s <= u64::MAX / 2 => s.min(dict as u64),
                _ => dict as u64,
            };
            if need > XZ_MEMORY_MAX {
                return Err(NEEDS_MEMORY.into());
            }
            let r = lzma_rust2::LzmaReader::new_with_props(raw, size, head[4], dict, None)
                .map_err(|_| DAMAGED.to_string())?;
            Box::new(r)
        };
        Ok(Capped {
            inner,
            memory,
            crc: flate2::Crc::new(),
            want,
        })
    }
}

impl Read for Capped<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf).map_err(|e| match e.raw_os_error() {
            Some(_) => e,
            None if self.memory.get() => io::Error::other(NEEDS_MEMORY),
            None => io::Error::other(DAMAGED),
        })?;
        if n == 0 && !buf.is_empty() && self.crc.sum() != self.want {
            return Err(io::Error::other(DAMAGED));
        }
        self.crc.update(&buf[..n]);
        Ok(n)
    }
}
