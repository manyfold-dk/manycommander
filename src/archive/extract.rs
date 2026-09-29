#![forbid(unsafe_code)]
//! Extraction (F5, P3 3.5) and member reads (F3, F4 and the quick view, P3 3.4).
//!
//! Extraction is a copy job with an [`ArchiveOrigin`] source (P3 2.3): its plan comes from
//! the index, so nothing is read from the archive before the destination checks, and every
//! byte goes through the local engine's temporary file and commit (A-2, I-2, I-3). A zip is
//! walked in tree order and each member opened by its locator, the `zip` crate's entry
//! index, so Retry reopens it. A tar, plain or compressed, is read in one pass in stream
//! order (`tar::pass`): the engine creates the directories first, takes each member as the
//! stream reaches it, and stops after the last one.
//!
//! On the write side: modes are masked to `0o777` and ownership is not restored; device,
//! FIFO and socket members are skipped as "special file"; a symlink member becomes a symlink
//! and is never traversed (A-2, A-3). A hard-link member is linked to the destination inode
//! the job extracted for the member it names, after an identity check, or skipped (A-3). An
//! encrypted member is skipped as "encrypted" (A-AR-7). The header at a member's locator must
//! be the one the index stored, or the member fails with "archive changed" (A-5).
//!
//! A member read for F3, F4 and the quick view ([`member_reader`]) is an owned reader: the
//! decoder runs on a thread of its own and hands the bytes over a bounded channel, because
//! the crates' member readers borrow the archive reader. Dropping the reader stops that
//! thread. Every read of the archive is positioned (P3 3.2).

use super::detect::Format;
use super::index::{ENCRYPTED, NO_TIME, NodeId, NodeKind, Tree};
use super::{ArchiveIndex, CHANGED_MEMBER, DAMAGED, PosReader, STILL_READING, tar};
use crate::fsops::copy::{ARCHIVE_ITSELF, Dir, Flow, LINK_NOT_EXTRACTED, copy_from};
use crate::fsops::group::{Group, NOT_LOCAL, OpenGroup, Opened, Root, validate};
use crate::fsops::job::{JobVerb, Report};
use crate::fsops::origin::{EachMember, Order, Origin, OriginDir, OriginFile, Removed, key};
use crate::fsops::plan::{Node, Note, Plan, Refusal, Totals, Verb};
use crate::fsops::question::{Interaction, Phase, Progress, Reporter};
use crate::fsops::sys::{Meta, Snapshot, Sys, Ts};
use crate::fsops::walk::EntryError;
use crate::provider::{VPath, synthetic_id};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

/// An encrypted zip entry: listed, never extracted or viewed (A-AR-7).
pub const ENCRYPTED_MEMBER: &str = "encrypted";
/// A member the `zip` crate cannot decode.
pub const UNSUPPORTED_METHOD: &str = "unsupported compression method";
/// Nothing is written into an archive and nothing is removed from one (A-2).
pub const READ_ONLY: &str = "archives are read-only";

const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;

/// What the index stored for a member that is read (A-5): its path after the A-1 split and
/// its size. The header at the member's locator must name the same path, be a regular file
/// and declare the same size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Expect {
    pub path: VPath,
    pub size: u64,
}

impl Expect {
    fn of(tree: &Tree, id: NodeId) -> Expect {
        Expect {
            path: tree.path_of(id),
            size: tree.node(id).size,
        }
    }

    /// Whether a header's name and size are the stored ones.
    pub(crate) fn matches(&self, name: &[u8], size: u64) -> bool {
        size == self.size && VPath::parse(name).is_ok_and(|p| p == self.path)
    }
}

/// A source directory of an extraction: a directory node of the index.
#[derive(Clone, Debug)]
pub struct ArchiveDir {
    node: NodeId,
    /// `archive.zip:/inner/dir`, for progress and the report.
    path: PathBuf,
    id: (u64, u64),
}

impl OriginDir for ArchiveDir {
    fn path(&self) -> &Path {
        &self.path
    }

    fn id(&self) -> (u64, u64) {
        self.id
    }
}

/// An archive's index as the copy engine's source (P3 2.3, 3.5).
pub struct ArchiveOrigin<'x> {
    sys: &'x Sys,
    ix: &'x Arc<ArchiveIndex>,
    tree: &'x Tree,
    /// The zip's central directory, read once per job when the first member is opened.
    zip: RefCell<Option<zip::ZipArchive<PosReader>>>,
    /// The furthest position of the archive file this job read.
    read: Arc<AtomicU64>,
    /// The time of members that carry none (P3 3.2): the extraction's own.
    now: Ts,
}

impl<'x> ArchiveOrigin<'x> {
    /// The origin of a complete index; `Err` while the scan still runs.
    pub fn new(sys: &'x Sys, ix: &'x Arc<ArchiveIndex>) -> Result<ArchiveOrigin<'x>, String> {
        let tree = ix.tree().ok_or(STILL_READING)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        Ok(ArchiveOrigin {
            sys,
            ix,
            tree,
            zip: RefCell::new(None),
            read: Arc::new(AtomicU64::new(0)),
            now: Ts {
                sec: now.as_secs() as i64,
                nsec: now.subsec_nanos(),
            },
        })
    }

    /// How far into the archive file this job read (A-AR-5: a one pass stops after the last
    /// selected member).
    pub fn bytes_read(&self) -> u64 {
        self.read.load(Ordering::SeqCst)
    }

    /// A node's metadata in a plan: its synthetic identity (P3 2.1), the mode masked to
    /// `0o777` (A-3), and the extraction's time for a member without one.
    fn meta(&self, id: NodeId) -> Meta {
        let n = self.tree.node(id);
        let mut m = self.tree.meta(id);
        m.id = synthetic_id(self.ix.id(), id as u64);
        m.perm &= 0o777;
        if n.flags & NO_TIME != 0 {
            (m.atime, m.mtime, m.ctime) = (self.now, self.now, self.now);
        }
        m
    }

    /// The plan of node `id` and, for a directory, its subtree.
    fn plan(
        &self,
        id: NodeId,
        name: OsString,
        totals: &mut Totals,
        seen: &mut u64,
        rep: &mut Reporter,
    ) -> Result<Node, Refusal> {
        *seen += 1;
        if seen.is_multiple_of(4096) {
            if self.sys.cancelled() {
                return Err(Refusal::Cancelled);
            }
            let t = *totals;
            rep.progress(|| Progress {
                phase: Phase::Scanning,
                files_done: 0,
                files_total: t.entries(),
                bytes_done: 0,
                bytes_total: t.bytes,
                current: self.ix.archive.clone(),
            });
        }
        let n = *self.tree.node(id);
        let mut node = Node::new(name, self.meta(id));
        match n.kind {
            NodeKind::Dir => {
                totals.dirs += 1;
                // A finished tree lists children sorted by name, as a plan holds them.
                for k in self.tree.children(id) {
                    let name = OsStr::from_bytes(self.tree.name(k)).to_owned();
                    node.children.push(self.plan(k, name, totals, seen, rep)?);
                }
            }
            NodeKind::File => {
                totals.files += 1;
                if n.flags & ENCRYPTED != 0 {
                    // Never written: it counts no bytes (A-AR-7).
                    node.meta.size = 0;
                    node.note = Some(Note::Skip(ENCRYPTED_MEMBER.into()));
                } else {
                    totals.bytes += n.size;
                }
            }
            NodeKind::HardLink => {
                // A link writes no data (A-3).
                totals.files += 1;
                node.meta.size = 0;
                if self.tree.hard_target(id).is_none() {
                    node.note = Some(Note::Skip(LINK_NOT_EXTRACTED.into()));
                }
            }
            NodeKind::Symlink => totals.symlinks += 1,
            NodeKind::Special => totals.specials += 1,
        }
        Ok(node)
    }

    /// The node behind a planned [`Node`].
    fn id(node: &Node) -> NodeId {
        key(node) as NodeId
    }
}

impl Origin for ArchiveOrigin<'_> {
    type Dir = ArchiveDir;

    /// Every group must be a directory of this archive (P3 2.2): `sub` is resolved in the
    /// index. A group whose directory is not there fails its names.
    fn open_groups(
        &self,
        verb: JobVerb,
        groups: &[Group],
    ) -> Result<Opened<ArchiveDir>, Box<Report>> {
        validate(groups).map_err(|why| Box::new(Report::refused(verb, why)))?;
        let ours = |g: &Group| matches!(&g.root, Root::Archive(i) if Arc::ptr_eq(i, self.ix));
        if !groups.iter().all(ours) {
            return Err(Box::new(Report::refused(verb, NOT_LOCAL)));
        }
        let mut sources = Vec::with_capacity(groups.len());
        let mut failed = Vec::new();
        for (i, g) in groups.iter().enumerate() {
            let at = VPath::new(g.sub.clone())
                .ok()
                .and_then(|p| self.tree.lookup(&p));
            match at {
                Some(d) if self.tree.node(d).kind == NodeKind::Dir => sources.push(OpenGroup {
                    dir: ArchiveDir {
                        node: d,
                        path: g.dir_path(),
                        id: synthetic_id(self.ix.id(), d as u64).inode(),
                    },
                    names: g.names.clone(),
                    group: i,
                }),
                other => {
                    let why = match other {
                        Some(_) => "not a directory",
                        None => "disappeared",
                    };
                    let dir = g.dir_path();
                    failed.extend(g.names.iter().map(|n| (dir.join(n), why.to_string())));
                }
            }
        }
        Ok(Opened { sources, failed })
    }

    /// The plan from the index (P3 3.5): nothing is read from the archive. A top-level
    /// destination that is the archive itself fails at once (P3 3.5).
    fn scan(
        &self,
        _verb: Verb,
        sources: &[OpenGroup<ArchiveDir>],
        dst: &Dir,
        targets: &[&[OsString]],
        rep: &mut Reporter,
    ) -> Result<Vec<Plan>, Refusal> {
        let archive = self.protected();
        let mut seen = 0;
        let mut plans = Vec::with_capacity(sources.len());
        for (s, targets) in sources.iter().zip(targets) {
            let mut totals = Totals::default();
            let mut roots = Vec::with_capacity(s.names.len());
            for (name, target) in s.names.iter().zip(targets.iter()) {
                let mut node = match self.tree.child(s.dir.node, name.as_bytes()) {
                    Some(id) => self.plan(id, name.clone(), &mut totals, &mut seen, rep)?,
                    None => {
                        let mut n = Node::new(name.clone(), Meta::default());
                        n.note = Some(Note::Failed(EntryError::Disappeared));
                        n
                    }
                };
                if node.note.is_none()
                    && let Ok(d) = self.sys.stat_at("scan.stat", dst.fd(), target)
                    && Some(d.id.inode()) == archive
                {
                    node.note = Some(Note::Fail(ARCHIVE_ITSELF.into()));
                }
                roots.push(node);
            }
            plans.push(Plan {
                roots,
                totals,
                src_dirs: HashSet::new(),
                links: HashMap::new(),
            });
        }
        Ok(plans)
    }

    /// Zip in tree order, by locator; tar in one pass (P3 3.5).
    fn order(&self) -> Order {
        match self.ix.format {
            Format::Zip => Order::Tree,
            _ => Order::Stream,
        }
    }

    /// A zip member by its locator (P3 3.5): each attempt opens it again, so Retry works. A
    /// tar member comes only through [`Origin::pass`].
    fn lend<T>(
        &self,
        _dir: &ArchiveDir,
        node: &Node,
        _cancel: &AtomicBool,
        read: &mut dyn FnMut(&mut dyn Read, u64) -> T,
    ) -> Option<Result<T, String>> {
        if self.order() == Order::Stream {
            return Some(Err(crate::fsops::origin::NO_PASS.into()));
        }
        let id = Self::id(node);
        let n = self.tree.node(id);
        if n.flags & ENCRYPTED != 0 {
            return Some(Err(ENCRYPTED_MEMBER.into()));
        }
        let mut zip = self.zip.borrow_mut();
        if zip.is_none() {
            match open_zip(self.ix.file().clone(), self.ix.key.size, Some(&self.read)) {
                Ok(z) => *zip = Some(z),
                Err(e) => return Some(Err(e)),
            }
        }
        let za = zip.as_mut()?;
        let expect = Expect::of(self.tree, id);
        let declared = node.meta.size;
        Some(zip_member(za, n.locator as usize, &expect, &mut |r| {
            read(r, declared)
        }))
    }

    /// The tar pass (P3 3.5): the wanted members by their locators.
    fn pass(
        &self,
        wanted: &HashSet<u64>,
        cancel: &Arc<AtomicBool>,
        each: &mut EachMember<'_>,
    ) -> Result<(), String> {
        let by_locator: HashMap<u64, (u64, Expect)> = wanted
            .iter()
            .map(|&k| {
                let id = k as NodeId;
                (self.tree.node(id).locator, (k, Expect::of(self.tree, id)))
            })
            .collect();
        tar::pass(
            self.ix.file().clone(),
            self.ix.key.size,
            self.ix.format,
            &by_locator,
            cancel.clone(),
            Some(self.read.clone()),
            each,
        )
    }

    /// A hard-link member's target node (A-3): resolved only inside the index (P3 3.2).
    fn link_target(&self, node: &Node) -> Option<u64> {
        let id = Self::id(node);
        if self.tree.node(id).kind != NodeKind::HardLink {
            return None;
        }
        self.tree.hard_target(id).map(|t| t as u64)
    }

    /// The archive file itself (P3 3.5).
    fn protected(&self) -> Option<(u64, u64)> {
        Some((self.ix.key.dev, self.ix.key.ino))
    }

    fn open_dir(&self, dir: &ArchiveDir, node: &Node) -> Result<ArchiveDir, EntryError> {
        let id = Self::id(node);
        let n = self.tree.node(id);
        if n.kind != NodeKind::Dir || n.parent != dir.node {
            return Err(EntryError::TypeChanged);
        }
        Ok(ArchiveDir {
            node: id,
            path: dir.path.join(&node.name),
            id: node.meta.id.inode(),
        })
    }

    /// Never reached: members are lent ([`Origin::lend`]) or come through the pass.
    fn open(&self, _: &ArchiveDir, _: &Node, _: &AtomicBool) -> Result<OriginFile, EntryError> {
        Err(EntryError::TypeChanged)
    }

    /// The target from the index, byte-identical (P3 3.2).
    fn read_link(&self, _dir: &ArchiveDir, node: &Node) -> Result<(OsString, Meta), EntryError> {
        let target = self
            .tree
            .link_target(Self::id(node))
            .ok_or(EntryError::TypeChanged)?;
        Ok((OsString::from_vec(target.to_vec()), node.meta))
    }

    fn remove(&self, _: &ArchiveDir, _: &OsStr, _: &Snapshot) -> Removed {
        Removed::Kept(READ_ONLY.into())
    }

    fn remove_dir(&self, _: &ArchiveDir, _: &OsStr, _: (u64, u64)) -> Removed {
        Removed::Kept(READ_ONLY.into())
    }

    /// 2 s for zip's DOS times, 1 s for tar (the M1 4.5 amendment of P3 1.4).
    fn mtime_resolution(&self, _: &ArchiveDir) -> i128 {
        match self.ix.format {
            Format::Zip => 2_000_000_000,
            _ => 1_000_000_000,
        }
    }
}

/// F5 out of an archive (P3 3.5): the groups of one archive into the local `dst`.
pub fn extract(sys: &Sys, ui: &mut dyn Interaction, groups: &[Group], dst: &Path) -> Report {
    let Some(Root::Archive(ix)) = groups.first().map(|g| &g.root) else {
        return Report::refused(JobVerb::Copy, NOT_LOCAL);
    };
    match ArchiveOrigin::new(sys, ix) {
        Ok(o) => copy_from(sys, ui, &o, groups, dst),
        Err(e) => Report::refused(JobVerb::Copy, e),
    }
}

/// The zip's central directory over positioned reads (P3 3.2), with the job's read counter.
fn open_zip(
    file: Arc<File>,
    len: u64,
    read: Option<&Arc<AtomicU64>>,
) -> Result<zip::ZipArchive<PosReader>, String> {
    let mut r = PosReader::new(file, len).window(64 << 10);
    if let Some(p) = read {
        r = r.progress(p.clone());
    }
    zip::ZipArchive::new(r).map_err(|_| DAMAGED.to_string())
}

/// Opens zip entry `locator`, checks it against what the index stored (A-5), and lends its
/// decoded bytes to `f`. A CRC or decoder error in the bytes reads as "archive damaged".
fn zip_member<T>(
    za: &mut zip::ZipArchive<PosReader>,
    locator: usize,
    expect: &Expect,
    f: &mut dyn FnMut(&mut dyn Read) -> T,
) -> Result<T, String> {
    use zip::result::ZipError;
    let mut zf = match za.by_index(locator) {
        Ok(z) => z,
        Err(ZipError::UnsupportedArchive(m)) if m == ZipError::PASSWORD_REQUIRED => {
            return Err(ENCRYPTED_MEMBER.into());
        }
        Err(ZipError::UnsupportedArchive(_) | ZipError::CompressionMethodNotSupported(_)) => {
            return Err(UNSUPPORTED_METHOD.into());
        }
        Err(_) => return Err(DAMAGED.into()),
    };
    let bits = zf.unix_mode().map_or(0, |m| m & S_IFMT);
    let file = !zf.name_raw().ends_with(b"/") && (bits == 0 || bits == S_IFREG);
    if !file || !expect.matches(zf.name_raw(), zf.size()) {
        return Err(CHANGED_MEMBER.into());
    }
    let mut r = Decoded(&mut zf);
    Ok(f(&mut r))
}

/// A member's decoded bytes: an error that is not the OS's (a CRC mismatch, a broken
/// deflate stream, an early end) is "archive damaged" (A-5).
struct Decoded<'r, R>(&'r mut R);

impl<R: Read> Read for Decoded<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf).map_err(|e| match e.raw_os_error() {
            Some(_) => e,
            None => io::Error::other(DAMAGED),
        })
    }
}

/// Bytes per message of a member reader.
const PIPE_CHUNK: usize = 256 << 10;

/// The receiving end of a member reader (P3 3.4): chunks from the decoding thread. Dropping
/// it tells that thread to stop.
struct Pipe {
    rx: Receiver<Result<Vec<u8>, String>>,
    chunk: Vec<u8>,
    at: usize,
    closed: Arc<AtomicBool>,
}

impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        while self.at >= self.chunk.len() {
            match self.rx.recv() {
                Ok(Ok(c)) => {
                    self.chunk = c;
                    self.at = 0;
                }
                Ok(Err(why)) => return Err(io::Error::other(why)),
                // The thread finished: the end of the member.
                Err(_) => return Ok(0),
            }
        }
        let n = buf.len().min(self.chunk.len() - self.at);
        buf[..n].copy_from_slice(&self.chunk[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

/// Sends `r` in chunks until it ends, at most `limit` bytes; `false` when the reader is gone.
fn pump(r: &mut dyn Read, tx: &SyncSender<Result<Vec<u8>, String>>, limit: u64) -> bool {
    let mut sent = 0u64;
    loop {
        let want = (limit - sent).min(PIPE_CHUNK as u64) as usize;
        if want == 0 {
            return true;
        }
        let mut buf = vec![0u8; want];
        let n = loop {
            match r.read(&mut buf) {
                Ok(n) => break n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return tx.send(Err(e.to_string())).is_ok(),
            }
        };
        if n == 0 {
            return true;
        }
        buf.truncate(n);
        sent += n as u64;
        if tx.send(Ok(buf)).is_err() {
            return false;
        }
    }
}

/// An owned reader of regular-file node `id` for [`Provider::open_read`](crate::provider::Provider)
/// (P3 3.4): a thread decodes the member (a zip entry by locator, a tar member through the
/// pass) with the A-5 check and hands the bytes over a channel of two chunks. It stops at the
/// first byte past the declared size, which the reader's caller then refuses (A-4), and when
/// the reader is dropped. A panic in a decoder ends the read with an error (NFR-REL).
pub(crate) fn member_reader(ix: &ArchiveIndex, tree: &Tree, id: NodeId) -> Box<dyn Read + Send> {
    let (tx, rx) = sync_channel::<Result<Vec<u8>, String>>(2);
    let closed = Arc::new(AtomicBool::new(false));
    let file = ix.file().clone();
    let (len, format) = (ix.key.size, ix.format);
    let n = *tree.node(id);
    let expect = Expect::of(tree, id);
    let stop = closed.clone();
    let run = move || {
        let limit = expect.size.saturating_add(1);
        let tx2 = tx.clone();
        let body = std::panic::AssertUnwindSafe(move || match format {
            Format::Zip => {
                let r = open_zip(file, len, None).and_then(|mut za| {
                    zip_member(&mut za, n.locator as usize, &expect, &mut |r| {
                        pump(r, &tx, limit)
                    })
                });
                if let Err(e) = r {
                    let _ = tx.send(Err(e));
                }
            }
            _ => {
                let wanted = HashMap::from([(n.locator, (id as u64, expect))]);
                let r = tar::pass(file, len, format, &wanted, stop, None, &mut |_, bytes| {
                    match bytes {
                        Ok(r) => {
                            pump(r, &tx, limit);
                        }
                        Err(why) => {
                            let _ = tx.send(Err(why));
                        }
                    }
                    Flow::Stop
                });
                if let Err(e) = r {
                    let _ = tx.send(Err(e));
                }
            }
        });
        if std::panic::catch_unwind(body).is_err() {
            let _ = tx2.send(Err("internal error while reading the archive".into()));
        }
    };
    let spawned = std::thread::Builder::new()
        .name("list-archive-read".into())
        .spawn(run);
    if let Err(e) = spawned {
        // No thread: the reader fails at once.
        let (tx, rx) = sync_channel(1);
        let _ = tx.send(Err(format!("cannot start the reader: {e}")));
        return Box::new(Pipe {
            rx,
            chunk: Vec::new(),
            at: 0,
            closed,
        });
    }
    Box::new(Pipe {
        rx,
        chunk: Vec::new(),
        at: 0,
        closed,
    })
}
