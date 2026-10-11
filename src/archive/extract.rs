#![forbid(unsafe_code)]
//! Extraction (F5, TDR-3 3.5) and member reads (F3, F4 and the quick view, TDR-3 3.4).
//!
//! Extraction is a copy job with an [`ArchiveOrigin`] source (TDR-3 2.3): its plan comes from
//! the index, so nothing is read from the archive before the destination checks, and every
//! byte goes through the local engine's temporary file and commit (A-2, I-2, I-3). A zip is
//! walked in tree order and each member opened by its locator, the `zip` crate's entry
//! index, so Retry reopens it; so is a 7z whose blocks hold one member each. A tar, plain or
//! compressed, is read in one pass in stream order (`tar::pass`): the engine creates the
//! directories first, takes each member as the stream reaches it, and stops after the last
//! one. A solid 7z is read in one pass too (`sevenz::pass`): each needed block decodes once,
//! in block order.
//!
//! On the write side: modes are masked to `0o777` and ownership is not restored; device,
//! FIFO and socket members are skipped as "special file"; a symlink member becomes a symlink
//! and is never traversed (A-2, A-3). A hard-link member is linked to the destination inode
//! the job extracted for the member it names, after an identity check, or skipped (A-3). An
//! encrypted member is skipped as "encrypted" (A-AR-7). The header at a member's locator must
//! be the one the index stored, or the member fails with "archive changed" (A-5).
//!
//! A member read for F3, F4 and the quick view (`member_reader`) is an owned reader: the
//! decoder runs on a thread of its own and hands the bytes over a bounded channel, because
//! the crates' member readers borrow the archive reader. Dropping the reader stops that
//! thread. Every read of the archive is positioned (TDR-3 3.2).
//!
//! A zip member is found through the central directory, a 7z member through the header.
//! Every member read of an archive, and every F5 job out of it, shares one parse of them
//! ([`Parses`], TDR-3 2.6): the first read makes it, the later ones take a clone.

use super::detect::Format;
use super::index::{ENCRYPTED, Limits, NO_TIME, NodeId, NodeKind, Tree};
use super::zip::NotRead;
use super::{
    ArchiveIndex, CHANGED, CHANGED_MEMBER, DAMAGED, PosReader, S_IFMT, S_IFREG, STILL_READING,
    lock, sevenz, tar,
};
use crate::fsops::copy::{ARCHIVE_ITSELF, Dir, Flow, LINK_NOT_EXTRACTED, copy_from};
use crate::fsops::group::{Group, NOT_LOCAL, OpenGroup, Opened, Root, validate};
use crate::fsops::job::{JobVerb, Report};
use crate::fsops::origin::{EachMember, Order, Origin, OriginDir, OriginFile, Removed, key};
use crate::fsops::plan::{Node, Note, Plan, Refusal, Totals, Verb};
use crate::fsops::question::{Interaction, Phase, Progress, Reporter};
use crate::fsops::sys::{Meta, Snapshot, Sys, Ts};
use crate::fsops::walk::{EntryError, errno_text};
use crate::provider::{StatKey, VPath, synthetic_id};
use rustix::fd::AsFd;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};

/// An encrypted zip entry: listed, never extracted or viewed (A-AR-7).
pub const ENCRYPTED_MEMBER: &str = "encrypted";
/// A member the `zip` crate cannot decode.
pub const UNSUPPORTED_METHOD: &str = "unsupported compression method";
/// Nothing is written into an archive and nothing is removed from one (A-2).
pub const READ_ONLY: &str = "archives are read-only";

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

/// An archive's index as the copy engine's source (TDR-3 2.3, 3.5).
pub struct ArchiveOrigin<'x> {
    sys: &'x Sys,
    ix: &'x Arc<ArchiveIndex>,
    tree: &'x Tree,
    /// The zip's central directory: the shared parse (TDR-3 2.6), taken when the job opens
    /// its first member and again when the archive's key changes, with a reader of its own,
    /// and the key it was taken at.
    zip: RefCell<Option<Taken<zip::ZipArchive<PosReader>>>>,
    /// The 7z header (the shared parse), taken as the zip's is, with its key.
    seven: RefCell<Option<Taken<Arc<sevenz_rust2::Archive>>>>,
    /// The job's reader of the 7z blocks.
    blocks: RefCell<Option<sevenz::Src>>,
    /// The 7z blocks this job decoded (A-AR-5: each at most once in a solid archive).
    decodes: AtomicU64,
    /// The furthest position of the archive file this job read: a tar's pass and the 7z
    /// blocks. Zip members read through the shared parse's reader, which it does not count.
    read: Arc<AtomicU64>,
    /// The time of members that carry none (TDR-3 3.2): the extraction's own.
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
            seven: RefCell::new(None),
            blocks: RefCell::new(None),
            decodes: AtomicU64::new(0),
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

    /// How many 7z blocks this job decoded (A-AR-5: a solid block once per job).
    pub fn blocks_decoded(&self) -> u64 {
        self.decodes.load(Ordering::SeqCst)
    }

    /// A node's metadata in a plan: its synthetic identity (TDR-3 2.1), the mode masked to
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
                    // Sizes are the archive's word: a sum past `u64::MAX` stays there, and
                    // the free-space check then asks (A-4).
                    totals.bytes = totals.bytes.saturating_add(n.size);
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
            NodeKind::Symlink => {
                totals.symlinks += 1;
                // A 7z symlink whose target could not be read has none (TDR-3 3.2).
                if self.tree.link_target(id).is_none() {
                    node.note = Some(if n.flags & ENCRYPTED != 0 {
                        Note::Skip(ENCRYPTED_MEMBER.into())
                    } else {
                        let why = self.ix.outcome().and_then(|o| o.error.clone());
                        Note::Fail(why.unwrap_or_else(|| DAMAGED.into()))
                    });
                }
            }
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

    /// Every group must be a directory of this archive (TDR-3 2.2): `sub` is resolved in the
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

    /// The plan from the index (TDR-3 3.5): nothing is read from the archive. A top-level
    /// destination that is the archive itself fails at once (TDR-3 3.5).
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

    /// Zip, and a 7z of one member per block, in tree order, by locator; tar and a solid 7z
    /// in one pass (TDR-3 3.5).
    fn order(&self) -> Order {
        match self.ix.format {
            Format::Zip => Order::Tree,
            Format::SevenZ if !self.ix.solid() => Order::Tree,
            _ => Order::Stream,
        }
    }

    /// A zip or 7z member by its locator (TDR-3 3.5): each attempt opens it again, so Retry
    /// works. A tar member, and a member of a solid 7z, comes only through [`Origin::pass`].
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
        let expect = Expect::of(self.tree, id);
        let declared = node.meta.size;
        // The archive's key before each member (TDR-3 3.2): when it is not the key the job's
        // parse was taken at, the job lets that parse go and takes the parse again, which
        // parses the bytes as they are now or refuses an archive whose size changed.
        let parser = self.ix.parser();
        let now = parser.key(self.sys);
        if self.ix.format == Format::SevenZ {
            let mut seven = self.seven.borrow_mut();
            let cancel = self.sys.cancel_flag();
            if seven.as_ref().is_none_or(|(_, key)| *key != now) {
                *seven = None;
                match parser.seven(self.sys, cancel) {
                    Ok(taken) => *seven = Some(taken),
                    Err(e) => return Some(Err(e)),
                }
            }
            let (archive, _) = seven.as_ref()?;
            let mut blocks = self.blocks.borrow_mut();
            let src = blocks.get_or_insert_with(|| parser.src(cancel, Some(&self.read)));
            return Some(sevenz::read_member(
                archive,
                src,
                n.locator,
                &expect,
                Some(&self.decodes),
                &mut |r| read(r, declared),
            ));
        }
        let mut zip = self.zip.borrow_mut();
        if zip.as_ref().is_none_or(|(_, key)| *key != now) {
            *zip = None;
            match parser.zip(self.sys) {
                Ok(taken) => *zip = Some(taken),
                Err(e) => return Some(Err(e)),
            }
        }
        let (za, _) = zip.as_mut()?;
        Some(zip_member(za, n.locator as usize, &expect, &mut |r| {
            read(r, declared)
        }))
    }

    /// The one pass (TDR-3 3.5): the wanted members by their locators, a tar in stream order,
    /// a solid 7z block by block.
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
        if self.ix.format == Format::SevenZ {
            let parser = self.ix.parser();
            let (archive, key) = parser.seven(self.sys, cancel)?;
            let mut src = parser.src(cancel, Some(&self.read));
            // Before each block (TDR-3 3.5): the pass plans its blocks from the parse it took,
            // so an archive whose key changed fails the members of every block not reached.
            let fresh = || {
                self.sys.hit("archive.block").map_err(errno_text)?;
                match parser.key(self.sys) == key {
                    true => Ok(()),
                    false => Err(CHANGED.to_string()),
                }
            };
            return sevenz::pass(
                &mut src,
                &archive,
                &by_locator,
                cancel,
                &self.decodes,
                &fresh,
                each,
            );
        }
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

    /// A hard-link member's target node (A-3): resolved only inside the index (TDR-3 3.2).
    fn link_target(&self, node: &Node) -> Option<u64> {
        let id = Self::id(node);
        if self.tree.node(id).kind != NodeKind::HardLink {
            return None;
        }
        self.tree.hard_target(id).map(|t| t as u64)
    }

    /// The archive file itself (TDR-3 3.5).
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

    /// The target from the index, byte-identical (TDR-3 3.2).
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

    /// 2 s for zip's DOS times, 1 s for tar (the TDR-1 4.5 amendment of TDR-3 1.4), 100 ns for
    /// 7z's Windows file times.
    fn mtime_resolution(&self, _: &ArchiveDir) -> i128 {
        match self.ix.format {
            Format::Zip => 2_000_000_000,
            Format::SevenZ => 100,
            _ => 1_000_000_000,
        }
    }
}

/// F5 out of an archive (TDR-3 3.5): the groups of one archive into the local `dst`.
pub fn extract(sys: &Sys, ui: &mut dyn Interaction, groups: &[Group], dst: &Path) -> Report {
    let Some(Root::Archive(ix)) = groups.first().map(|g| &g.root) else {
        return Report::refused(JobVerb::Copy, NOT_LOCAL);
    };
    match ArchiveOrigin::new(sys, ix) {
        Ok(o) => copy_from(sys, ui, &o, groups, dst),
        Err(e) => Report::refused(JobVerb::Copy, e),
    }
}

/// The zip's central directory over positioned reads (TDR-3 3.2), under the scan's entry
/// bound ([`super::zip::central`]). The index holds fewer entries than `limits` allow, so a
/// count above them means the archive changed since the scan.
fn open_zip(
    file: Arc<File>,
    len: u64,
    limits: Limits,
    sys: &Sys,
) -> Result<zip::ZipArchive<PosReader>, String> {
    let reader = PosReader::new(file.clone(), len).window(64 << 10);
    super::zip::central(&file, len, reader, limits, sys).map_err(|e| match e {
        NotRead::TooMany => CHANGED.to_string(),
        NotRead::Damaged => DAMAGED.to_string(),
    })
}

/// The parse that member reads of one archive share (TDR-3 2.6): the zip's central
/// directory, or the 7z header. A clone shares it: a zip clone gets a reader of its own on the
/// same held fd, and the 7z header sits behind an `Arc`.
#[derive(Clone)]
enum Parsed {
    Zip(zip::ZipArchive<PosReader>),
    SevenZ(Arc<sevenz_rust2::Archive>),
}

/// A parse that left the cache ([`Parses::release`]): whoever holds it decides on which
/// thread it is freed.
pub struct Parse(#[allow(dead_code)] Parsed);

/// The cached parse.
struct Cached {
    /// The index whose held fd the parse was made from ([`ArchiveIndex::id`]).
    owner: u64,
    /// That fd's `StatKey` when the parse was made.
    key: StatKey,
    parsed: Parsed,
}

/// The one cached parse (TDR-3 2.6): made by the first member read of the archive a panel
/// shows, and taken by every later member read and F5 job there. It serves only the index
/// whose held fd it was made from, and only while that fd's `StatKey` is the one it had then,
/// so a parse never meets bytes it was not made from; a rescan makes a new index, whose
/// first read parses again. A read that cannot take it frees it before making its own; and
/// it leaves when no panel shows its archive ([`Parses::release`]). Reads that miss at the
/// same moment each make a parse: nothing makes one wait for another's. The lock is never
/// held while a parse is made or freed, so the UI thread never waits on one (P-1).
#[derive(Default)]
pub struct Parses {
    slot: Mutex<Option<Cached>>,
    made: AtomicU64,
}

impl Parses {
    /// The parses made so far: by member reads that found none to share (A-AR-9).
    pub fn made(&self) -> u64 {
        self.made.load(Ordering::SeqCst)
    }

    /// The index whose parse is cached ([`ArchiveIndex::id`]).
    pub fn owner(&self) -> Option<u64> {
        lock(&self.slot).as_ref().map(|c| c.owner)
    }

    /// The cached parse of index `owner` while its fd still has `key`. Otherwise the
    /// cached parse, of another index or stale, leaves and is freed here, outside the lock,
    /// before the caller makes its own: a switch between two archives peaks at the parse
    /// being made, not at that plus the old one. Reads in flight keep the clones they took.
    fn get(&self, owner: u64, key: StatKey) -> Option<Parsed> {
        let old = {
            let mut slot = lock(&self.slot);
            match slot.as_ref() {
                Some(c) if c.owner == owner && c.key == key => return Some(c.parsed.clone()),
                _ => slot.take(),
            }
        };
        drop(old);
        None
    }

    /// Takes out and frees the parse of index `owner`, outside the lock, if it is cached.
    fn forget(&self, owner: u64) {
        let old = {
            let mut slot = lock(&self.slot);
            match slot.as_ref() {
                Some(c) if c.owner == owner => slot.take(),
                _ => None,
            }
        };
        drop(old);
    }

    /// Caches `parsed` in place of the parse cached so far, which is freed outside the lock.
    fn put(&self, owner: u64, key: StatKey, parsed: Parsed) {
        let old = lock(&self.slot).replace(Cached { owner, key, parsed });
        drop(old);
    }

    /// Takes the cached parse out when no index in `shown` (the archives the panels show)
    /// owns it. Reads in flight keep the clones they took.
    pub fn release(&self, shown: &[u64]) -> Option<Parse> {
        let mut slot = lock(&self.slot);
        if slot.as_ref().is_some_and(|c| !shown.contains(&c.owner)) {
            return slot.take().map(|c| Parse(c.parsed));
        }
        None
    }
}

/// What a member read of one index needs to find or make the shared parse (TDR-3 2.6), owned
/// so that a reader thread can take it.
#[derive(Clone)]
pub(crate) struct Parser {
    owner: u64,
    file: Arc<File>,
    /// The archive's size at the scan: every read of the index stops there.
    len: u64,
    /// The index's bounds, which a parse keeps as the scan kept them.
    limits: Limits,
    parses: Arc<Parses>,
}

/// A parse and the `StatKey` the held fd had when it was taken: an F5 job compares it with
/// the key before each member (TDR-3 3.2). `None` when `statx` failed.
pub(crate) type Taken<T> = (T, Option<StatKey>);

impl Parser {
    pub(crate) fn new(
        owner: u64,
        file: Arc<File>,
        len: u64,
        limits: Limits,
        parses: Arc<Parses>,
    ) -> Parser {
        Parser {
            owner,
            file,
            len,
            limits,
            parses,
        }
    }

    /// The `StatKey` of the held fd now; `None` when `statx` fails, and nothing is shared.
    pub(crate) fn key(&self, sys: &Sys) -> Option<StatKey> {
        sys.stat_fd(self.file.as_fd()).ok().map(|m| StatKey::of(&m))
    }

    /// The cached parse when it is this index's and the file behind the fd has the key it
    /// had when the parse was made. An archive whose size is no longer the scan's is refused
    /// with [`CHANGED`]: every read stops at the scan's size, so a parse would see part of
    /// the file. Nothing is then parsed, and this index's parse leaves the cache. Otherwise
    /// `make` parses the bytes as they are now, and its parse is cached when the key did not
    /// change while it ran. A failed or cancelled parse caches nothing.
    fn parse(
        &self,
        sys: &Sys,
        make: impl FnOnce() -> Result<Parsed, String>,
    ) -> Result<Taken<Parsed>, String> {
        let key = self.key(sys);
        if let Some(k) = key {
            if k.size != self.len {
                self.parses.forget(self.owner);
                return Err(CHANGED.into());
            }
            if let Some(p) = self.parses.get(self.owner, k) {
                return Ok((p, key));
            }
        }
        let parsed = make()?;
        self.parses.made.fetch_add(1, Ordering::SeqCst);
        if let Some(k) = key
            && self.key(sys) == Some(k)
        {
            self.parses.put(self.owner, k, parsed.clone());
        }
        Ok((parsed, key))
    }

    /// The zip's central directory, with a reader of its own on the held fd. `sys` consults
    /// the job's failpoints.
    pub(crate) fn zip(&self, sys: &Sys) -> Result<Taken<zip::ZipArchive<PosReader>>, String> {
        let make = || open_zip(self.file.clone(), self.len, self.limits, sys).map(Parsed::Zip);
        match self.parse(sys, make)? {
            (Parsed::Zip(z), key) => Ok((z, key)),
            // An index has one format, and its parse is of that format.
            (Parsed::SevenZ(_), _) => Err(DAMAGED.into()),
        }
    }

    /// The 7z header, walked under the index's bounds as the scan walked it; `cancel` stops
    /// its reading (TDR-3 3.2).
    pub(crate) fn seven(
        &self,
        sys: &Sys,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Taken<Arc<sevenz_rust2::Archive>>, String> {
        let make = || {
            let mut src = self.src(cancel, None);
            sevenz::read_archive(&mut src, self.len, self.limits)
                .map(|a| Parsed::SevenZ(Arc::new(a)))
                .map_err(|f| f.text())
        };
        match self.parse(sys, make)? {
            (Parsed::SevenZ(a), key) => Ok((a, key)),
            (Parsed::Zip(_), _) => Err(DAMAGED.into()),
        }
    }

    /// A 7z reader on the held fd; `read` records how far it read.
    pub(crate) fn src(
        &self,
        cancel: &Arc<AtomicBool>,
        read: Option<&Arc<AtomicU64>>,
    ) -> sevenz::Src {
        sevenz::Src::new(self.file.clone(), self.len, cancel.clone(), read.cloned())
    }
}

/// Opens zip entry `locator`, checks it against what the index stored (A-5), and lends its
/// decoded bytes to `f` (`zip::read_entry`). A CRC or decoder error in the bytes reads as
/// "archive damaged".
fn zip_member<T>(
    za: &mut zip::ZipArchive<PosReader>,
    locator: usize,
    expect: &Expect,
    f: &mut dyn FnMut(&mut dyn Read) -> T,
) -> Result<T, String> {
    let accept = |zf: &zip::read::ZipFile<'_, PosReader>| {
        let bits = zf.unix_mode().map_or(0, |m| m & S_IFMT);
        let file = !zf.name_raw().ends_with(b"/") && (bits == 0 || bits == S_IFREG);
        match file && expect.matches(zf.name_raw(), zf.size()) {
            true => Ok(()),
            false => Err(CHANGED_MEMBER.to_string()),
        }
    };
    super::zip::read_entry(za, locator, &accept, f)
}

/// A member's decoded bytes: an error that is not the OS's (a CRC mismatch, a broken
/// deflate stream, an early end) is "archive damaged" (A-5).
pub(crate) struct Decoded<'r, R: ?Sized>(pub(crate) &'r mut R);

impl<R: Read + ?Sized> Read for Decoded<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf).map_err(|e| match e.raw_os_error() {
            Some(_) => e,
            None => io::Error::other(DAMAGED),
        })
    }
}

/// Bytes per message of a member reader.
const PIPE_CHUNK: usize = 256 << 10;

/// The receiving end of a member reader (TDR-3 3.4): chunks from the decoding thread. Dropping
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
/// (TDR-3 3.4): a thread decodes the member (a zip or 7z entry by locator through the shared
/// parse of TDR-3 2.6, a tar member through the pass) with the A-5 check and hands the bytes
/// over a channel of two chunks. It stops at the first byte past the declared size, which
/// the reader's caller then refuses (A-4), and when the reader is dropped. A panic in a
/// decoder ends the read with an error (NFR-REL).
pub(crate) fn member_reader(ix: &ArchiveIndex, tree: &Tree, id: NodeId) -> Box<dyn Read + Send> {
    let (tx, rx) = sync_channel::<Result<Vec<u8>, String>>(2);
    let closed = Arc::new(AtomicBool::new(false));
    let file = ix.file().clone();
    let (len, format) = (ix.key.size, ix.format);
    let parser = ix.parser();
    let n = *tree.node(id);
    let expect = Expect::of(tree, id);
    let stop = closed.clone();
    let run = move || {
        let limit = expect.size.saturating_add(1);
        let tx2 = tx.clone();
        let body = std::panic::AssertUnwindSafe(move || {
            let r = match format {
                Format::Zip => parser.zip(&Sys::default()).and_then(|(mut za, _)| {
                    zip_member(&mut za, n.locator as usize, &expect, &mut |r| {
                        pump(r, &tx, limit);
                    })
                }),
                Format::SevenZ => {
                    // Dropping the reader raises `stop`, which ends the header's reading and
                    // the block's decoding.
                    parser.seven(&Sys::default(), &stop).and_then(|(a, _)| {
                        let mut src = parser.src(&stop, None);
                        sevenz::read_member(&a, &mut src, n.locator, &expect, None, &mut |r| {
                            pump(r, &tx, limit);
                        })
                    })
                }
                _ => {
                    let wanted = HashMap::from([(n.locator, (id as u64, expect))]);
                    tar::pass(file, len, format, &wanted, stop, None, &mut |_, bytes| {
                        match bytes {
                            Ok(r) => {
                                pump(r, &tx, limit);
                            }
                            Err(why) => {
                                let _ = tx.send(Err(why));
                            }
                        }
                        Flow::Stop
                    })
                }
            };
            if let Err(e) = r {
                let _ = tx.send(Err(e));
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
