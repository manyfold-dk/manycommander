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
    let (dir, meta) = open_dir_nofollow(sys, step, parent, name)?;
    if meta.id != *expected {
        return Err(EntryError::TypeChanged);
    }
    Ok((dir, meta))
}

/// Opens `name` below `parent` as a directory with `O_DIRECTORY | O_NOFOLLOW` and `statx`es
/// the new fd. A symlink or a non-directory fails with "type changed". The group walk
/// (P2 2.2) uses it for `sub` components, which have no planned identity.
pub fn open_dir_nofollow(
    sys: &Sys,
    step: &'static str,
    parent: BorrowedFd,
    name: &OsStr,
) -> std::result::Result<(OwnedFd, Meta), EntryError> {
    let dir = match sys.open_dir(step, parent, name) {
        Ok(d) => d,
        Err(Errno::LOOP | Errno::NOTDIR) => return Err(EntryError::TypeChanged),
        Err(e) => return Err(EntryError::os("open directory", e)),
    };
    let meta = sys
        .stat_fd(fd(&dir))
        .map_err(|e| EntryError::os("stat directory", e))?;
    if meta.kind != Kind::Dir {
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

/// How many ancestors [`ancestors`] walks up before it gives up.
pub const MAX_DEPTH: usize = 8192;

/// The identities of `dir` and every ancestor up to the root, found by walking `..`.
///
/// `PATH_MAX` does not bound the depth: directories created relative to an open one can
/// nest deeper than any path names. A chain that has not reached the root after
/// [`MAX_DEPTH`] ancestors fails with `ELOOP` rather than end short: a caller that looks
/// for a directory among the ancestors (I-4) must never get an incomplete chain.
pub fn ancestors(sys: &Sys, dir: BorrowedFd) -> Result<Vec<FsIdentity>> {
    let mut out = vec![sys.stat_fd(dir)?.id];
    let mut cur = sys.open_dir("walk.parent", dir, OsStr::new(".."))?;
    for _ in 0..MAX_DEPTH {
        let id = sys.stat_fd(fd(&cur))?.id;
        if out.last().map(|l| l.inode()) == Some(id.inode()) {
            // `..` of the root is the root itself.
            return Ok(out);
        }
        out.push(id);
        cur = sys.open_dir("walk.parent", fd(&cur), OsStr::new(".."))?;
    }
    Err(Errno::LOOP)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    /// A directory `depth` levels below `top`, made relative to its parent's fd, so its
    /// path may be longer than `PATH_MAX`.
    fn nest(sys: &Sys, top: &Path, depth: usize) -> OwnedFd {
        let mut cur = sys.open_root(top).unwrap();
        for _ in 0..depth {
            match sys.mkdir("test.mkdir", fd(&cur), OsStr::new("d"), 0o700) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(e) => panic!("mkdir: {e}"),
            }
            cur = sys
                .open_dir("test.open", fd(&cur), OsStr::new("d"))
                .unwrap();
        }
        cur
    }

    /// Removes a tree made by [`nest`] without a path longer than `PATH_MAX` and without an
    /// fd per level: each level is renamed up to `top`, then the flat directories go.
    fn unnest(sys: &Sys, top: &Path) {
        let Ok(t) = sys.open_root(top) else {
            return;
        };
        let mut cur = sys.open_root(top).unwrap();
        let mut flat = Vec::new();
        let mut n = 0u64;
        while let Ok(child) = sys.open_dir("test.open", fd(&cur), OsStr::new("d")) {
            let name = OsString::from(format!("flat-{n}"));
            sys.rename(
                "test.flatten",
                fd(&cur),
                OsStr::new("d"),
                fd(&t),
                &name,
                true,
            )
            .unwrap();
            flat.push(name);
            n += 1;
            cur = child;
        }
        for name in flat {
            let _ = sys.rmdir("test.rmdir", fd(&t), &name);
        }
        let _ = std::fs::remove_dir(top);
    }

    fn top(name: &str) -> PathBuf {
        let p = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/target/test-tmp"))
            .join(format!("{name}-{}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn ancestors_reach_the_root() {
        let sys = Sys::default();
        let t = top("walk-ancestors");
        let deep = nest(&sys, &t, 3);
        let chain = ancestors(&sys, fd(&deep)).unwrap();
        let root = sys.stat_path(Path::new("/")).unwrap().id;
        assert_eq!(chain.last().map(|l| l.inode()), Some(root.inode()));
        let tid = sys.stat_path(&t).unwrap().id;
        assert!(chain.iter().any(|a| a.inode() == tid.inode()));
        drop(deep);
        unnest(&sys, &t);
    }

    /// A destination nested deeper than `MAX_DEPTH` fails instead of giving a chain that
    /// misses the source above it (I-4).
    #[test]
    fn ancestors_deeper_than_the_limit_fail() {
        let sys = Sys::default();
        let t = top("walk-ancestors-deep");
        let deep = nest(&sys, &t, MAX_DEPTH + 8);
        let chain = ancestors(&sys, fd(&deep)).map(|c| c.len());
        assert_eq!(chain, Err(Errno::LOOP), "a chain that ends short");
        drop(deep);
        unnest(&sys, &t);
    }
}
