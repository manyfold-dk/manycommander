#![forbid(unsafe_code)]
//! Safe traversal and opening (design 4.3).
//!
//! Every operation is relative to a directory fd that the job opened itself. A child
//! directory is opened with `O_NOFOLLOW` and its identity compared with the plan; a regular
//! file is opened `O_PATH` first, checked with `fstat`, and only then reopened for reading.
//! A directory component swapped for a symlink during the job therefore cannot redirect it
//! (I-5), and a device node or FIFO is never opened for I/O.

use super::sys::{FsIdentity, Kind, Meta, Result, Sys, fd};
use rustix::fd::{BorrowedFd, OwnedFd};
use rustix::io::Errno;
use std::ffi::OsStr;
use std::fmt;

/// Why an entry failed before any write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryError {
    /// The entry is no longer what the plan saw (a symlink now, another inode, another type).
    TypeChanged,
    /// The entry no longer exists.
    Disappeared,
    /// Any other OS error, with the operation that failed.
    Os { op: &'static str, errno: Errno },
}

impl EntryError {
    pub fn os(op: &'static str, errno: Errno) -> EntryError {
        match errno {
            Errno::NOENT => EntryError::Disappeared,
            _ => EntryError::Os { op, errno },
        }
    }

    pub fn errno(&self) -> Option<Errno> {
        match self {
            EntryError::Os { errno, .. } => Some(*errno),
            _ => None,
        }
    }
}

impl fmt::Display for EntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntryError::TypeChanged => f.write_str("type changed"),
            EntryError::Disappeared => f.write_str("disappeared"),
            EntryError::Os { op, errno } => write!(f, "{op}: {}", errno_text(*errno)),
        }
    }
}

/// The OS error text of an errno, e.g. "Permission denied (os error 13)".
pub fn errno_text(e: Errno) -> String {
    std::io::Error::from_raw_os_error(e.raw_os_error()).to_string()
}

/// Descends into `name` below `parent`: `O_DIRECTORY | O_NOFOLLOW`, then `statx` of the new
/// fd compared with the planned identity. A symlink, a non-directory or another inode fails
/// with "type changed".
pub fn open_child_dir(
    sys: &Sys,
    step: &'static str,
    parent: BorrowedFd,
    name: &OsStr,
    expected: &FsIdentity,
) -> std::result::Result<(OwnedFd, Meta), EntryError> {
    let dir = match sys.open_dir(step, parent, name) {
        Ok(d) => d,
        Err(Errno::LOOP | Errno::NOTDIR) => return Err(EntryError::TypeChanged),
        Err(e) => return Err(EntryError::os("open directory", e)),
    };
    let meta = sys
        .stat_fd(fd(&dir))
        .map_err(|e| EntryError::os("stat directory", e))?;
    if meta.kind != Kind::Dir || meta.id != *expected {
        return Err(EntryError::TypeChanged);
    }
    Ok((dir, meta))
}

/// Opens a regular file for reading (design 4.3 steps 1-3). `expected` is the planned
/// `(st_dev, st_ino)`, when there is a plan.
pub fn open_for_read(
    sys: &Sys,
    parent: BorrowedFd,
    name: &OsStr,
    expected: Option<(u64, u64)>,
) -> std::result::Result<(OwnedFd, Meta), EntryError> {
    let opath = match sys.open_path("open.opath", parent, name) {
        Ok(f) => f,
        Err(Errno::LOOP) => return Err(EntryError::TypeChanged),
        Err(e) => return Err(EntryError::os("open", e)),
    };
    let pmeta = sys
        .stat_fd(fd(&opath))
        .map_err(|e| EntryError::os("stat", e))?;
    if pmeta.kind != Kind::File {
        return Err(EntryError::TypeChanged);
    }
    if let Some(inode) = expected
        && pmeta.id.inode() != inode
    {
        return Err(EntryError::TypeChanged);
    }
    let file = sys
        .reopen_read(fd(&opath))
        .map_err(|e| EntryError::os("open", e))?;
    let meta = sys
        .stat_fd(fd(&file))
        .map_err(|e| EntryError::os("stat", e))?;
    if meta.id.inode() != pmeta.id.inode() || meta.kind != Kind::File {
        return Err(EntryError::TypeChanged);
    }
    Ok((file, meta))
}

/// The identities of `dir` and every ancestor up to the root, found by walking `..`.
pub fn ancestors(sys: &Sys, dir: BorrowedFd) -> Result<Vec<FsIdentity>> {
    let mut out = vec![sys.stat_fd(dir)?.id];
    let mut cur = sys.open_dir("walk.parent", dir, OsStr::new(".."))?;
    // PATH_MAX bounds a real directory depth; the limit only guards against a loop.
    for _ in 0..8192 {
        let id = sys.stat_fd(fd(&cur))?.id;
        if out.last().map(|l| l.inode()) == Some(id.inode()) {
            break;
        }
        out.push(id);
        cur = sys.open_dir("walk.parent", fd(&cur), OsStr::new(".."))?;
    }
    Ok(out)
}
