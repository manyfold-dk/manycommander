#![forbid(unsafe_code)]
//! The copy engine's source side (P3 2.3).
//!
//! [`Transfer`](super::copy::Transfer) reads its sources through an [`Origin`]; the
//! destination side stays as it is: temporary file, commit, questions, progress, cancel and
//! group commit. The name keeps the trait apart from the panel's `Source` (P2 2.4) and from
//! the `Provider` of a non-local place (P3 2.1).
//!
//! [`LocalOrigin`] is today's code behind the trait: the group open (P2 2.2), the scan and
//! its pre-flight checks (M1 4.4, 4.6), the `O_PATH` read open (M1 4.3), and a move's source
//! removal after the group commit (M1 4.8, P2 9.2). Its files are [`OriginFile::Local`], so
//! `copy_file_range`, sparse files and the hard-link map (P2 9) stay local-only. Every other
//! origin gives [`OriginFile::Stream`]s: read into the job buffer, never past their
//! declared size (A-4), and never read twice.
//!
//! An origin whose reader borrows its own state, an archive member's decoder, lends the
//! bytes for one attempt instead ([`Origin::lend`]). An origin of [`Order::Stream`] is read
//! in one pass ([`Origin::pass`], P3 3.5): the engine creates the directories first, then
//! takes each wanted member as the stream reaches it.

use super::copy::{Dir, Flow, mtime_resolution};
use super::group::{Group, OpenGroup, Opened};
use super::job::{JobVerb, Report};
use super::plan::{Node, Plan, Refusal, Scan, Verb, scan_all};
use super::question::Reporter;
use super::sys::{Kind, Meta, Snapshot, Sys};
use super::walk::{EntryError, errno_text, open_child_dir, open_for_read};
use rustix::fd::OwnedFd;
use rustix::io::Errno;
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// What a `Stream` member fails with when it produces more or fewer bytes than it declared
/// (A-4).
pub const SIZE_MISMATCH: &str = "size mismatch";

/// What an origin that is not read in one pass says when asked for one.
pub const NO_PASS: &str = "this source is not read in one pass";

/// A member of a one-pass stream that an attempt already read from: a stream cannot be
/// read again (P3 2.3), so Retry fails it.
pub const READ_ONCE: &str = "the archive is read in one pass; this member cannot be read again";

/// What a one pass calls for each wanted member it reaches ([`Origin::pass`]): the member's
/// [`key`] and its bytes, or why they cannot be read.
pub type EachMember<'a> = dyn FnMut(u64, Result<&mut dyn Read, String>) -> Flow + 'a;

/// The key of a planned node: `st_ino` of its identity, synthetic for a non-local origin
/// (P3 2.1). [`Origin::pass`] and [`Origin::link_target`] name members by it.
pub fn key(node: &Node) -> u64 {
    node.meta.id.ino
}

/// A source directory as an origin reached it: an open directory fd for a local origin
/// (M1 4.3), a directory of the place for the others. The engine hands it back to the
/// origin to reach the directory's entries.
pub trait OriginDir: Clone {
    /// The display path, for progress and the report.
    fn path(&self) -> &Path;
    /// `(st_dev, st_ino)` of the directory: its local identity, or a synthetic one that
    /// never equals a local one (P3 2.1).
    fn id(&self) -> (u64, u64);
}

impl OriginDir for Dir {
    fn path(&self) -> &Path {
        &self.path
    }

    fn id(&self) -> (u64, u64) {
        self.meta.id.inode()
    }
}

/// How the engine walks an origin's plan (P3 2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    /// Tree order, each file opened on its own: local, zip, 7z, SFTP.
    Tree,
    /// One pass in stream order (tar, P3 3.5): the engine creates the directories first,
    /// then takes each member as [`Origin::pass`] reaches it.
    Stream,
}

/// A regular file opened for reading (P3 2.3).
pub enum OriginFile {
    /// A local file (M1 4.3): `copy_file_range`, the sparse walk (P2 9.1) and the hard-link
    /// map (P2 9.2). `meta` is its `S0` (M1 4.7 step 1).
    Local { fd: OwnedFd, meta: Meta },
    /// Bytes from an archive member or a server. They are read into the job buffer and
    /// written to the temporary file; there is no `copy_file_range`, no hole detection and
    /// no hard-link map. Reading stops at the first byte past `declared`, which is never
    /// written, and a stream that ends short fails too, both with [`SIZE_MISMATCH`] (A-4).
    /// A stream is never read twice: "file exists" at its commit keeps the temporary file
    /// across the question (the M1 4.7 amendment, P3 1.4).
    Stream {
        reader: Box<dyn Read + Send>,
        declared: u64,
    },
}

/// A move's removal of one source entry after the destination batch is synced (M1 4.8
/// steps 5 and 6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Removed {
    /// Removed now, or gone already: the committed destination holds the content.
    Done,
    /// Kept before any removal was tried (it changed, or the origin keeps its sources,
    /// R-4); the text is the report's reason.
    Kept(String),
    /// Kept because the removal itself failed; the text is the report's reason.
    Failed(String),
}

/// The source side of the copy engine (P3 2.3).
///
/// The engine walks each group's plan and hands the origin the directory it is in and the
/// planned [`Node`]. `open_groups` and `scan` run before any write; `remove`,
/// `remove_linked` and `remove_dir` run only in a move, after the batch's `syncfs`.
pub trait Origin {
    /// A source directory as this origin reaches it.
    type Dir: OriginDir;

    /// Opens the job's groups (P2 2.2), each group's directory once. `Err` is the final
    /// report of a refused job; a group that cannot be opened fails its names in
    /// [`Opened::failed`], and the other groups go on.
    fn open_groups(
        &self,
        verb: JobVerb,
        groups: &[Group],
    ) -> Result<Opened<Self::Dir>, Box<Report>>;

    /// Plans the opened groups into the existing [`Plan`] of [`Node`]s, one plan per group,
    /// with the pre-flight checks over their union (M1 4.6, P2 2.2). `targets` are, per
    /// group, the names its entries get in `dst`. A non-local origin runs no
    /// destination-inside-source or same-file check: its identities are synthetic.
    fn scan(
        &self,
        verb: Verb,
        sources: &[OpenGroup<Self::Dir>],
        dst: &Dir,
        targets: &[&[OsString]],
        rep: &mut Reporter,
    ) -> Result<Vec<Plan>, Refusal>;

    /// Tree order (local, zip, 7z, SFTP) or one pass in stream order (tar).
    fn order(&self) -> Order {
        Order::Tree
    }

    /// Lends a planned regular file's bytes to `read` for one attempt (P3 2.3): an origin
    /// whose reader borrows its own state (an archive member's decoder) cannot give it away
    /// as an [`OriginFile`]. `read` gets the reader and the declared size. `None`: the origin
    /// opens its files with [`Origin::open`]. `Some(Err(why))`: the file cannot be read, and
    /// the entry fails with `why` without a retry question (E-3). Each call opens the file
    /// again, so Retry works for an origin that can reopen by locator (zip).
    fn lend<T>(
        &self,
        dir: &Self::Dir,
        node: &Node,
        cancel: &AtomicBool,
        read: &mut dyn FnMut(&mut dyn Read, u64) -> T,
    ) -> Option<Result<T, String>> {
        let _ = (dir, node, cancel, read);
        None
    }

    /// The one pass of an origin of [`Order::Stream`] (P3 3.5): reads the stream once, in
    /// stream order, and calls `each` with the [`key`] of every `wanted` member it reaches,
    /// with its bytes or with why they cannot be read ("archive changed", A-5). It stops
    /// after the last wanted member, or when `each` returns [`Flow::Stop`]. `Err`: the stream
    /// ended or broke before every wanted member was reached ("archive damaged"); the engine
    /// fails the members it did not reach with it.
    fn pass(
        &self,
        wanted: &HashSet<u64>,
        cancel: &Arc<AtomicBool>,
        each: &mut EachMember<'_>,
    ) -> Result<(), String> {
        let _ = (wanted, cancel, each);
        Err(NO_PASS.into())
    }

    /// For a hard-link member (A-3), the [`key`] of the member it links to; `None` for every
    /// other node. The engine links it to the destination it extracted for that member, or
    /// skips it.
    fn link_target(&self, node: &Node) -> Option<u64> {
        let _ = node;
        None
    }

    /// `(st_dev, st_ino)` of a local file that the job never replaces: the archive being
    /// extracted (P3 3.5).
    fn protected(&self) -> Option<(u64, u64)> {
        None
    }

    /// Descends into a planned directory of `dir`. An OS error raises the error question;
    /// "type changed" and "disappeared" fail the directory.
    fn open_dir(&self, dir: &Self::Dir, node: &Node) -> Result<Self::Dir, EntryError>;

    /// Opens a planned regular file of `dir` for reading. `cancel` is the job's flag, for
    /// an origin that waits on something other than a syscall.
    fn open(
        &self,
        dir: &Self::Dir,
        node: &Node,
        cancel: &AtomicBool,
    ) -> Result<OriginFile, EntryError>;

    /// A planned symlink's target, byte-identical, with the metadata it was read with.
    fn read_link(&self, dir: &Self::Dir, node: &Node) -> Result<(OsString, Meta), EntryError>;

    /// A move's removal of `name` in `dir`, after the destination batch is synced: only
    /// when it still matches `planned` (`S0`, M1 4.8 step 5.3). An origin that cannot
    /// identify its sources keeps them (R-4).
    fn remove(&self, dir: &Self::Dir, name: &OsStr, planned: &Snapshot) -> Removed;

    /// A move's removal of the committed names of one multi-linked source inode (P2 9.2),
    /// each `(dir, name, S0, the link count at its copy)`: every name is checked before any
    /// is removed. Only a local origin plans hard links; the default removes each name on
    /// its own.
    fn remove_linked(&self, names: &[(&Self::Dir, &OsStr, &Snapshot, u32)]) -> Vec<Removed> {
        names
            .iter()
            .map(|(dir, name, planned, _)| self.remove(dir, name, planned))
            .collect()
    }

    /// A move's removal of a finished source directory `name` in `parent`, if it is still
    /// the directory `id` the job emptied (M1 4.8 step 6).
    fn remove_dir(&self, parent: &Self::Dir, name: &OsStr, id: (u64, u64)) -> Removed;

    /// The mtime resolution of the source side in nanoseconds, for "Overwrite all older"
    /// (M1 4.5, and its P3 1.4 amendment for archives and servers).
    fn mtime_resolution(&self, dir: &Self::Dir) -> i128;

    /// A local origin: the same-file check applies and the hard-link map is kept.
    fn is_local(&self) -> bool {
        false
    }

    /// The local directory behind `dir`, for the local-only mechanisms: the hard-link map
    /// (P2 9.2) and a symlink's change check before a move commits (M1 4.8 step 2).
    fn local(dir: &Self::Dir) -> Option<&Dir> {
        let _ = dir;
        None
    }
}

/// The local filesystem as an origin: today's engine behind the trait (P3 2.3).
pub struct LocalOrigin<'a> {
    sys: &'a Sys,
}

impl<'a> LocalOrigin<'a> {
    pub fn new(sys: &'a Sys) -> Self {
        LocalOrigin { sys }
    }

    /// `unlinkat` of a checked source name.
    fn unlink(&self, dir: &Dir, name: &OsStr) -> Removed {
        match self.sys.unlink("move.unlink", dir.fd(), name) {
            Ok(()) => Removed::Done,
            Err(e) => Removed::Failed(format!(
                "unlink source: {}; the destination is committed, both kept",
                errno_text(e)
            )),
        }
    }
}

impl Origin for LocalOrigin<'_> {
    type Dir = Dir;

    fn open_groups(&self, verb: JobVerb, groups: &[Group]) -> Result<Opened, Box<Report>> {
        super::group::open(self.sys, verb, groups)
    }

    fn scan(
        &self,
        verb: Verb,
        sources: &[OpenGroup],
        dst: &Dir,
        targets: &[&[OsString]],
        rep: &mut Reporter,
    ) -> Result<Vec<Plan>, Refusal> {
        let scans: Vec<Scan> = sources
            .iter()
            .zip(targets)
            .map(|(s, t)| Scan {
                sys: self.sys,
                verb,
                src: s.dir.fd(),
                src_path: &s.dir.path,
                names: &s.names,
                dst: Some((dst.fd(), t)),
            })
            .collect();
        scan_all(&scans, rep)
    }

    /// `O_DIRECTORY | O_NOFOLLOW`, then the identity against the plan (M1 4.3).
    fn open_dir(&self, dir: &Dir, node: &Node) -> Result<Dir, EntryError> {
        let (fd, meta) =
            open_child_dir(self.sys, "walk.openat", dir.fd(), &node.name, &node.meta.id)?;
        Ok(Dir {
            fd: Arc::new(fd),
            meta,
            path: dir.path.join(&node.name),
        })
    }

    /// The `O_PATH` sequence of M1 4.3 against the planned identity; `meta` is `S0`.
    fn open(&self, dir: &Dir, node: &Node, _cancel: &AtomicBool) -> Result<OriginFile, EntryError> {
        let (fd, meta) = open_for_read(self.sys, dir.fd(), &node.name, Some(node.meta.id.inode()))?;
        Ok(OriginFile::Local { fd, meta })
    }

    /// `statx` of the link itself against the plan, then `readlinkat` (M1 4.7).
    fn read_link(&self, dir: &Dir, node: &Node) -> Result<(OsString, Meta), EntryError> {
        let sys = self.sys;
        let now = sys
            .stat_at("copy.lstat", dir.fd(), &node.name)
            .map_err(|e| EntryError::os("stat", e))?;
        if now.kind != Kind::Symlink || now.id.inode() != node.meta.id.inode() {
            return Err(EntryError::TypeChanged);
        }
        match sys.readlink("copy.readlink", dir.fd(), &node.name) {
            Ok(l) => Ok((l, now)),
            Err(Errno::INVAL) => Err(EntryError::TypeChanged),
            Err(e) => Err(EntryError::os("read link", e)),
        }
    }

    /// M1 4.8 step 5.3: `statx` of the name, and `unlinkat` only if identity, size, mtime
    /// and ctime still equal `S0`. Linux has no unlink-by-fd: a replacement between the
    /// `statx` and the `unlinkat` is the documented residual race.
    fn remove(&self, dir: &Dir, name: &OsStr, planned: &Snapshot) -> Removed {
        match self.sys.stat_at("move.statx", dir.fd(), name) {
            Ok(m) if m.snapshot() == *planned => self.unlink(dir, name),
            Ok(_) => Removed::Kept("source changed; kept both".into()),
            // Already gone: the committed destination holds the content.
            Err(Errno::NOENT) => Removed::Done,
            Err(e) => Removed::Kept(format!("stat source: {}; kept both", errno_text(e))),
        }
    }

    /// P2 9.2: every name is checked with `statx` before any is unlinked, in full against
    /// its `S0`: identity, size, mtime, ctime, and the link count at its copy. Then the
    /// names that match are unlinked one after the other with no further check (each
    /// unlink changes the inode's ctime and nlink); the others are kept. The residual race
    /// is the M1 one between `statx` and `unlinkat`, extended over these consecutive
    /// unlinks. The design takes one `statx` of the inode; a `statx` per name, all before
    /// the first unlink, makes the same comparisons and also checks each name's identity
    /// directly (as M1 does) instead of inferring it from the link count and ctime, and it
    /// reports a name that is gone as gone.
    fn remove_linked(&self, names: &[(&Dir, &OsStr, &Snapshot, u32)]) -> Vec<Removed> {
        let checks: Vec<_> = names
            .iter()
            .map(|(dir, name, ..)| self.sys.stat_at("move.linkstat", dir.fd(), name))
            .collect();
        names
            .iter()
            .zip(checks)
            .map(|((dir, name, planned, nlink), c)| match c {
                Ok(m) if m.snapshot() == **planned && m.nlink == *nlink => self.unlink(dir, name),
                Ok(_) => Removed::Kept("source changed; kept both".into()),
                Err(Errno::NOENT) => Removed::Done,
                Err(e) => Removed::Kept(format!("stat source: {}; kept both", errno_text(e))),
            })
            .collect()
    }

    /// `rmdir` of the directory the job emptied, not of a replacement under its name.
    fn remove_dir(&self, parent: &Dir, name: &OsStr, id: (u64, u64)) -> Removed {
        match self.sys.stat_at("move.dirstat", parent.fd(), name) {
            Ok(m) if m.kind == Kind::Dir && m.id.inode() == id => {}
            Ok(_) => return Removed::Kept("replaced during the move; kept".into()),
            Err(e) => {
                return Removed::Kept(format!("source directory not removed: {}", errno_text(e)));
            }
        }
        match self.sys.rmdir("move.rmdir", parent.fd(), name) {
            Ok(()) => Removed::Done,
            Err(Errno::NOTEMPTY | Errno::EXIST) => {
                Removed::Failed("kept, it still holds entries that were not moved".into())
            }
            Err(e) => Removed::Failed(format!("source directory not removed: {}", errno_text(e))),
        }
    }

    /// From the source filesystem's `f_type` (M1 4.5).
    fn mtime_resolution(&self, dir: &Dir) -> i128 {
        mtime_resolution(self.sys.fstatfs(dir.fd()).map(|s| s.f_type).unwrap_or(0))
    }

    fn is_local(&self) -> bool {
        true
    }

    fn local(dir: &Dir) -> Option<&Dir> {
        Some(dir)
    }
}
