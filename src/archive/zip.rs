#![forbid(unsafe_code)]
//! Zip listing from the central directory (P3 3.1, 3.2, 3.3).
//!
//! The `zip` crate reads the central directory through a [`PosReader`]; each entry's
//! metadata comes from `by_index_raw`, which reads nothing but the entry's local header.
//! Names are `name_raw`: the central-directory bytes, or the UTF-8 name of a valid Unicode
//! path extra field, which the crate puts in their place; CP437 is not decoded. Times come
//! from the extended-timestamp field when present, else from the DOS time as local time at
//! 2 s resolution. Encrypted entries are flagged: listed, never extracted. A symlink's
//! target is the entry's content, read up to `PATH_MAX` bytes.
//!
//! The crate materializes the whole central directory, so the entry count the end record
//! declares is checked first: an archive that declares more than the index bound is not
//! listed at all (P3 2.6).

use super::index::{DAMAGED_MEMBER, LINK_TOO_LONG, Limits, Member, MemberKind, Tree};
use super::{ArchiveIndex, DAMAGED, PosReader, Sink, Stop};
use crate::fsops::sys::Ts;
use std::io::Read;
use std::os::unix::fs::FileExt;
use zip::ExtraField;

/// A symlink target longer than this is not a target (`PATH_MAX`).
const LINK_MAX: u64 = 4096;

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFLNK: u32 = 0o120000;
const S_IFREG: u32 = 0o100000;

/// The entry count the last end-of-central-directory record declares, zip64-aware; `None`
/// when there is none in the last 64 KiB (the crate then decides).
fn declared_entries(ix: &ArchiveIndex) -> Option<u64> {
    let len = ix.key.size;
    let tail_len = len.min(65_557 + 22) as usize;
    let mut tail = vec![0u8; tail_len];
    let at = len - tail_len as u64;
    let n = ix.file().read_at(&mut tail, at).ok()?;
    tail.truncate(n);
    let p = (0..tail.len().saturating_sub(21)).rev().find(|&p| {
        tail[p..].starts_with(b"PK\x05\x06")
            && p + 22 + u16::from_le_bytes([tail[p + 20], tail[p + 21]]) as usize <= tail.len()
    })?;
    let count = u16::from_le_bytes([tail[p + 10], tail[p + 11]]) as u64;
    let offset = u32::from_le_bytes([tail[p + 16], tail[p + 17], tail[p + 18], tail[p + 19]]);
    if count != 0xffff && offset != 0xffff_ffff {
        return Some(count);
    }
    // Zip64: the locator sits right before the end record.
    let eocd = at + p as u64;
    let mut loc = [0u8; 20];
    ix.file().read_at(&mut loc, eocd.checked_sub(20)?).ok()?;
    if !loc.starts_with(b"PK\x06\x07") {
        return Some(count);
    }
    let rec = u64::from_le_bytes(loc[8..16].try_into().ok()?);
    let mut r = [0u8; 56];
    ix.file().read_at(&mut r, rec).ok()?;
    if !r.starts_with(b"PK\x06\x06") {
        return Some(count);
    }
    Some(count.max(u64::from_le_bytes(r[32..40].try_into().ok()?)))
}

/// The DOS date and time as local time in `tz` (P3 3.2).
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

/// Scans a zip's central directory into `tree` (P3 3.3).
pub(crate) fn scan(
    ix: &ArchiveIndex,
    tree: &mut Tree,
    sink: &mut Sink<'_>,
    tz: &jiff::tz::TimeZone,
) -> Result<(), Stop> {
    let limits: Limits = tree.limits();
    if declared_entries(ix).is_some_and(|n| n > limits.entries as u64) {
        return Err(Stop::Full(limits.entries_message()));
    }
    let reader = PosReader::new(ix.file().clone(), ix.key.size)
        .window(4096)
        .progress(ix.read.clone());
    let mut za = zip::ZipArchive::new(reader).map_err(|_| Stop::Fatal(DAMAGED.into()))?;
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

/// A symlink entry's target: its content, at most [`LINK_MAX`] bytes. `None` when longer.
/// A target that cannot be read is empty; the entry stays listed.
fn read_link(za: &mut zip::ZipArchive<PosReader>, i: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let Ok(f) = za.by_index(i) else {
        return Some(out);
    };
    if f.take(LINK_MAX + 1).read_to_end(&mut out).is_err() {
        return Some(Vec::new());
    }
    (out.len() as u64 <= LINK_MAX).then_some(out)
}
