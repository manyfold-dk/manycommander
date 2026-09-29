#![forbid(unsafe_code)]
//! Copy (F5, design 4.7) and the transfer engine that cross-filesystem move builds on.
//!
//! Per regular file: open the source safely (4.3) and record `S0`; write a temporary file
//! `.<name>.mc-partial-<random>` in the destination directory; copy with
//! `copy_file_range`; apply mode and times; commit with `RENAME_NOREPLACE` (or `linkat`, or
//! the direct-write mode on a filesystem that supports neither). The destination never
//! shows a partial file (I-2) and is never replaced without an answer (I-3).
//!
//! Copy fidelity (P2 9): a file with holes is copied segment by segment and keeps its holes;
//! a regular file with several names in the copied set becomes one inode with those names at
//! the destination, linked to the first destination the job committed for it.
//!
//! The source side is an [`Origin`] (P3 2.3): the engine walks each group's plan and asks
//! the origin to descend, open and read. A local origin's files keep all of the above; a
//! `Stream` file is read into the job buffer, never past its declared size (A-4), and its
//! complete temporary file waits across "file exists" instead of being read again.
//!
//! A non-local origin's plan is checked against the destination's free space before any
//! write (A-4). An origin of [`Order::Stream`] (a tar) is copied in one pass
//! ([`copy_from`], P3 3.5): the directories first, in tree order, then each member as the
//! stream reaches it, into its directory reached again through an LRU of directory fds, then
//! the hard links from the destination inodes the job extracted (A-3), and the directory
//! modes and times in post-order. A destination entry that is the archive being extracted is
//! never replaced.

use super::group::Group;
use super::job::{JobVerb, Report};
use super::origin::{
    LocalOrigin, Order, Origin, OriginDir, OriginFile, READ_ONCE, Removed, SIZE_MISMATCH, key,
};
use super::plan::{Node, Note, Plan, Refusal, Totals, Verb};
use super::question::{
    Answer, Conflict, Phase, Progress, Question, Reporter, Side, conflict, is_conflict_errno,
};
use super::sys::{Kind, Meta, Sys, Ts, magic, random_u64};
use super::walk::{EntryError, open_for_read};
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use rustix::io::Errno;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Bytes requested per `copy_file_range` call (design 4.7 step 3).
pub const CHUNK: usize = 16 << 20;
/// Buffer of the `read`/`write` fallback.
const BUF: usize = 1 << 20;
/// The marker in temporary names. Documented; never deleted automatically after a crash.
pub const PARTIAL_MARK: &str = ".mc-partial-";
const NAME_MAX: usize = 255;

/// An open directory of a job, with its identity and display path.
#[derive(Clone)]
pub struct Dir {
    pub fd: Arc<OwnedFd>,
    pub meta: Meta,
    pub path: PathBuf,
}

impl Dir {
    pub fn open_root(sys: &Sys, path: &Path) -> Result<Dir, EntryError> {
        let fd = sys
            .open_root(path)
            .map_err(|e| EntryError::os("open directory", e))?;
        let meta = sys
            .stat_fd(fd.as_fd())
            .map_err(|e| EntryError::os("stat directory", e))?;
        Ok(Dir {
            fd: Arc::new(fd),
            meta,
            path: path.to_path_buf(),
        })
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

/// Whether the job goes on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

/// Standing answers ("... all") of one job.
#[derive(Default)]
pub(crate) struct Policy {
    /// `Some(false)`: overwrite all; `Some(true)`: overwrite all older.
    pub overwrite: Option<bool>,
    pub skip_exists: bool,
    pub merge_all: bool,
    pub skip_mismatch: bool,
    pub skip_errnos: HashSet<Errno>,
}

pub(crate) enum Decision {
    Overwrite,
    Merge,
    Skip(String),
    Rename(OsString),
    Cancel,
}

/// Why one attempt at an entry did not complete.
#[derive(Debug)]
pub(crate) enum Fail {
    /// Something is at the destination name now; re-check and ask.
    Exists,
    /// Try the entry again from the start (the commit mode changed).
    Again,
    Cancelled,
    Entry(EntryError),
    Os(&'static str, Errno),
    /// The move's change check failed (design 4.8 step 2): nothing was committed.
    SourceChanged,
    /// A `Stream` failed on its read side: a read or decoder error, or [`SIZE_MISMATCH`]
    /// (P3 2.3, A-4). The entry fails with the text; there is no retry, because a stream is
    /// never read twice.
    Stream(String),
    /// The entry is skipped with the text: a hard link whose member the job did not extract
    /// (A-3).
    Skip(String),
    /// The destination entry is the archive being extracted: never replaced (P3 3.5).
    Protected,
}

/// What an entry whose destination is the archive being extracted fails with (P3 3.5).
pub const ARCHIVE_ITSELF: &str = "is the archive being extracted";

/// What a hard-link member fails with when the job did not extract the member it links to
/// (A-3).
pub const LINK_NOT_EXTRACTED: &str = "hard link to a member not extracted";

/// A destination the job extracted for a member that a hard-link member links to (A-3,
/// P3 3.5): its directory fd, name and identity.
#[derive(Clone)]
pub(crate) struct Extracted {
    dir: Arc<OwnedFd>,
    name: OsString,
    id: (u64, u64),
}

impl From<EntryError> for Fail {
    /// An OS error raises the error question; "type changed" and "disappeared" fail the
    /// entry directly.
    fn from(e: EntryError) -> Fail {
        match e {
            EntryError::Os { op, errno } => Fail::Os(op, errno),
            e => Fail::Entry(e),
        }
    }
}

/// Unlinks a name on drop unless disarmed: temporary files and direct-write destinations
/// never outlive a failure, a cancel or a panic (design 4.7 step 6).
pub(crate) struct Unlink<'a> {
    sys: &'a Sys,
    dir: BorrowedFd<'a>,
    name: OsString,
    armed: bool,
}

impl<'a> Unlink<'a> {
    pub fn new(sys: &'a Sys, dir: BorrowedFd<'a>, name: OsString) -> Self {
        Unlink {
            sys,
            dir,
            name,
            armed: true,
        }
    }

    pub fn disarm(&mut self) {
        self.armed = false;
    }

    /// The name the guard removes.
    pub(crate) fn name(&self) -> &OsStr {
        &self.name
    }
}

impl Drop for Unlink<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.sys.unlink("cleanup.unlink", self.dir, &self.name);
        }
    }
}

/// `.<name>.mc-partial-<16 hex>`, with the name shortened by bytes so the whole fits in
/// `NAME_MAX`.
pub fn partial_name(name: &OsStr) -> OsString {
    partial_name_with(name, random_u64())
}

/// [`partial_name`] with a given 64-bit tag.
pub fn partial_name_with(name: &OsStr, tag: u64) -> OsString {
    let suffix = format!("{PARTIAL_MARK}{tag:016x}");
    let room = NAME_MAX - 1 - suffix.len();
    let b = name.as_bytes();
    let mut out = Vec::with_capacity(NAME_MAX);
    out.push(b'.');
    out.extend_from_slice(&b[..b.len().min(room)]);
    out.extend_from_slice(suffix.as_bytes());
    OsString::from_vec(out)
}

/// Below this size, a file is written without checking its destination name first: a
/// conflict then shows at the `RENAME_NOREPLACE` commit, which is atomic either way (I-3),
/// and the check and the question follow. Saves a lookup per small file (P-7).
const PRECHECK_BYTES: u64 = 1 << 20;

/// The mtime resolution of a filesystem in nanoseconds, from `f_type` (design 4.5). Compare
/// (P2 7) uses it too.
pub(crate) fn mtime_resolution(f_type: i64) -> i128 {
    match f_type {
        magic::VFAT => 2_000_000_000,
        magic::EXFAT => 10_000_000,
        _ => 1,
    }
}

/// "Overwrite all older": the destination is strictly older at the coarser resolution.
pub(crate) fn dst_is_older(src: Ts, dst: Ts, resolution: i128) -> bool {
    dst.as_nanos().div_euclid(resolution) < src.as_nanos().div_euclid(resolution)
}

/// The destination that later in-set names of a multi-linked source inode are linked to
/// (P2 9.2).
#[derive(Clone)]
struct FirstDest {
    dir: Arc<OwnedFd>,
    name: OsString,
    /// The rename domain of `dir`: a link cannot leave it.
    domain: (u64, u64),
    /// `(st_dev, st_ino)` of the committed destination.
    id: (u64, u64),
    /// The source's size, mtime and ctime when it was copied.
    size: u64,
    mtime: Ts,
    ctime: Ts,
}

/// The hard-link state of one job (P2 9.2). Only regular files with at least two in-set
/// names take part; every other file is copied as in M1.
#[derive(Default)]
pub(crate) struct Links {
    /// Per such source inode `(st_dev, st_ino)`: its in-set names not yet settled
    /// (committed, skipped or failed). An inode leaves the map when its last name settles.
    unsettled: HashMap<(u64, u64), u32>,
    /// Per such inode: its latest destination whose data the job copied.
    first: HashMap<(u64, u64), FirstDest>,
    /// Inodes whose last in-set name settled since the last flush of a move.
    pub(crate) ready: Vec<(u64, u64)>,
    /// In-set names copied as separate files instead of linked (the report note, I-7).
    fallbacks: u64,
}

impl Links {
    /// The in-set name counts of a job: the sum over its groups' plans, restricted to the
    /// inodes with at least two in-set names.
    pub(crate) fn from_plans(plans: &[Plan]) -> Links {
        let mut unsettled: HashMap<(u64, u64), u32> = HashMap::new();
        for p in plans {
            for (k, n) in &p.links {
                *unsettled.entry(*k).or_insert(0) += n;
            }
        }
        unsettled.retain(|_, n| *n > 1);
        Links {
            unsettled,
            ..Links::default()
        }
    }

    /// The inode of `node` when it is an unsettled in-set name of a multi-linked regular
    /// file. Uses the plan's metadata only: no syscall (P-7).
    pub(crate) fn key(&self, node: &Node) -> Option<(u64, u64)> {
        if node.meta.nlink < 2 || node.meta.kind != Kind::File || self.unsettled.is_empty() {
            return None;
        }
        let k = node.meta.id.inode();
        self.unsettled.contains_key(&k).then_some(k)
    }

    /// One in-set name of `node`'s inode ended: committed, skipped or failed.
    pub(crate) fn settle(&mut self, node: &Node) {
        let Some(k) = self.key(node) else {
            return;
        };
        let n = self.unsettled.get_mut(&k).expect("key() checked it");
        *n -= 1;
        if *n == 0 {
            self.unsettled.remove(&k);
            self.first.remove(&k);
            self.ready.push(k);
        }
    }

    /// Every in-set name in a planned subtree ended (a skipped or failed directory, a
    /// subtree moved by one rename).
    pub(crate) fn settle_tree(&mut self, node: &Node) {
        if self.unsettled.is_empty() {
            return;
        }
        match node.meta.kind {
            Kind::Dir => node.children.iter().for_each(|c| self.settle_tree(c)),
            _ => self.settle(node),
        }
    }
}

/// The engine of copy and cross-filesystem move, and the question, progress and report
/// state that the other verbs share. `D` is a source directory as the job's origin reaches
/// it (P3 2.3); a local job's is an open directory fd.
pub struct Transfer<'a, 'u, D = Dir> {
    pub sys: &'a Sys,
    pub rep: Reporter<'u>,
    pub report: Report,
    pub(crate) policy: Policy,
    /// Destination domains that support neither `RENAME_NOREPLACE` nor hard links.
    pub(crate) direct: HashSet<(u64, u64)>,
    buf: Vec<u8>,
    pub files_total: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub bytes_done: u64,
    stopped: bool,
    current: PathBuf,
    /// Cross-filesystem move: change check before commit, sources unlinked by batch.
    pub(crate) moving: bool,
    pub(crate) batch: super::mv::Batch<D>,
    /// Every top-level entry counts once, whatever its kind (trash).
    pub(crate) flat: bool,
    /// `(source st_dev, destination st_dev)` pairs where `copy_file_range` is known not to
    /// work: later files go straight to read/write.
    no_kernel_copy: HashSet<(u64, u64)>,
    /// Temporary names: a random base per job plus a counter (no syscall per file).
    tmp_base: u64,
    tmp_seq: u64,
    /// Hard links within the copied set (P2 9.2).
    pub(crate) links: Links,
    /// A local file never replaced: the archive being extracted (P3 3.5).
    pub(crate) protect: Option<(u64, u64)>,
    /// The members that hard-link members of the plan link to (A-3), by [`key`].
    pub(crate) link_targets: HashSet<u64>,
    /// The destinations extracted for those members.
    extracted: HashMap<u64, Extracted>,
    /// The identity of the file the last stream attempt wrote, for a link target.
    written: Option<(u64, u64)>,
}

impl<'a, 'u, D: OriginDir> Transfer<'a, 'u, D> {
    pub fn new(sys: &'a Sys, rep: Reporter<'u>, report: Report) -> Self {
        Transfer {
            sys,
            rep,
            report,
            policy: Policy::default(),
            direct: HashSet::new(),
            buf: Vec::new(),
            files_total: 0,
            bytes_total: 0,
            files_done: 0,
            bytes_done: 0,
            stopped: false,
            current: PathBuf::new(),
            moving: false,
            batch: super::mv::Batch::default(),
            flat: false,
            no_kernel_copy: HashSet::new(),
            tmp_base: random_u64(),
            tmp_seq: 0,
            links: Links::default(),
            protect: None,
            link_targets: HashSet::new(),
            extracted: HashMap::new(),
            written: None,
        }
    }

    pub fn set_totals(&mut self, plan: &Plan) {
        self.set_sum(plan.totals);
    }

    /// Sets the progress totals and `planned` of the job: the totals of every group's plan
    /// (P2 2.2).
    pub fn set_sum(&mut self, totals: Totals) {
        self.files_total = totals.entries();
        self.bytes_total = totals.bytes;
        self.report.planned = totals.entries();
    }

    /// The path progress shows as the current entry (links and attributes, P2 8).
    pub(crate) fn set_current(&mut self, path: PathBuf) {
        self.current = path;
    }

    pub(crate) fn tick(&mut self) {
        let p = Progress {
            phase: Phase::Executing,
            files_done: self.files_done,
            files_total: self.files_total,
            bytes_done: self.bytes_done,
            bytes_total: self.bytes_total,
            current: self.current.clone(),
        };
        self.rep.progress(|| p);
    }

    pub fn cancelled(&mut self) -> bool {
        if self.sys.cancelled() {
            self.report.cancelled = true;
        }
        self.report.cancelled
    }

    pub fn stop(&mut self) -> Flow {
        self.stopped = true;
        self.report.cancelled = true;
        Flow::Stop
    }

    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// Ends the job without a cancel (a `syncfs` failure, design 4.8 step 5.2).
    pub(crate) fn halt(&mut self) -> Flow {
        self.stopped = true;
        Flow::Stop
    }

    /// Accounts `n` planned entries as ended (progress and the report's `settled`).
    pub(crate) fn settle(&mut self, n: u64) {
        self.files_done += n;
        self.report.settled += n;
    }

    /// Counts one entry as ended: a non-directory entry, or any entry in flat mode.
    pub(crate) fn entry_processed(&mut self, node: &Node) {
        if self.flat || node.meta.kind != Kind::Dir {
            self.settle(1);
        }
        self.links.settle(node);
    }

    /// The report note for in-set names that were copied as separate files (P2 9.2), so
    /// the report states the structure the job left (I-7).
    pub(crate) fn link_note(&mut self) {
        match self.links.fallbacks {
            0 => {}
            1 => self
                .report
                .notes
                .push("1 hard link was copied as a separate file".into()),
            n => self
                .report
                .notes
                .push(format!("{n} hard links were copied as separate files")),
        }
    }

    pub(crate) fn done(&mut self, node: &Node) {
        if node.meta.kind == Kind::Dir && !self.flat {
            self.report.dirs_done += 1;
        } else {
            self.report.done += 1;
        }
        self.entry_processed(node);
        self.tick();
    }

    /// Ends a skipped or failed entry; a directory takes its planned subtree with it.
    fn end_subtree(&mut self, node: &Node) {
        if node.meta.kind == Kind::Dir && !self.flat {
            let (entries, bytes) = subtree_counts(node);
            self.settle(entries);
            self.bytes_done += bytes;
            self.links.settle_tree(node);
        } else {
            if node.meta.kind == Kind::File {
                self.bytes_done += node.meta.size;
            }
            self.entry_processed(node);
        }
    }

    pub(crate) fn skip(&mut self, node: &Node, path: PathBuf, why: impl Into<String>) {
        self.end_subtree(node);
        self.report.skip(path, why);
        self.tick();
    }

    pub(crate) fn fail(&mut self, node: &Node, path: PathBuf, why: impl Into<String>) {
        self.end_subtree(node);
        self.report.fail(path, why);
        self.tick();
    }

    // ---- questions ----------------------------------------------------------------------

    fn f_type(&self, dir: &Dir) -> i64 {
        self.sys.fstatfs(dir.fd()).map(|s| s.f_type).unwrap_or(0)
    }

    /// "File exists" (design 4.5), with the standing answers applied first. `src_res` is the
    /// source side's mtime resolution, asked only for "Overwrite all older".
    pub(crate) fn decide_exists(
        &mut self,
        src: &Meta,
        src_res: &dyn Fn() -> i128,
        dst: &Meta,
        dst_dir: &Dir,
        dpath: &Path,
    ) -> Decision {
        let sys = self.sys;
        let older = || {
            let ft = |d: &Dir| sys.fstatfs(d.fd()).map(|s| s.f_type).unwrap_or(0);
            let res = src_res().max(mtime_resolution(ft(dst_dir)));
            if dst_is_older(src.mtime, dst.mtime, res) {
                Decision::Overwrite
            } else {
                Decision::Skip("the destination is not older".into())
            }
        };
        match self.policy.overwrite {
            Some(false) => return Decision::Overwrite,
            Some(true) => return older(),
            None => {}
        }
        if self.policy.skip_exists {
            return Decision::Skip("the destination exists".into());
        }
        let q = Question::FileExists {
            path: dpath.to_path_buf(),
            src: Side::of(src),
            dst: Side::of(dst),
            dst_is_symlink: dst.kind == Kind::Symlink,
        };
        match self.rep.ask(q) {
            Answer::Overwrite => Decision::Overwrite,
            Answer::OverwriteAll => {
                self.policy.overwrite = Some(false);
                Decision::Overwrite
            }
            Answer::OverwriteAllOlder => {
                self.policy.overwrite = Some(true);
                older()
            }
            Answer::SkipAll => {
                self.policy.skip_exists = true;
                Decision::Skip("the destination exists".into())
            }
            Answer::Rename(n) => rename_decision(n),
            Answer::Cancel => Decision::Cancel,
            _ => Decision::Skip("the destination exists".into()),
        }
    }

    /// "Directory exists".
    pub(crate) fn decide_dir_exists(&mut self, src: &Meta, dst: &Meta, dpath: &Path) -> Decision {
        if self.policy.merge_all {
            return Decision::Merge;
        }
        let q = Question::DirExists {
            path: dpath.to_path_buf(),
            src: Side::of(src),
            dst: Side::of(dst),
        };
        match self.rep.ask(q) {
            Answer::Merge => Decision::Merge,
            Answer::MergeAll => {
                self.policy.merge_all = true;
                Decision::Merge
            }
            Answer::Rename(n) => rename_decision(n),
            Answer::Cancel => Decision::Cancel,
            _ => Decision::Skip("the destination directory exists".into()),
        }
    }

    /// "Type mismatch": a tree never replaces a file, and a file never replaces a tree.
    pub(crate) fn decide_mismatch(&mut self, src: &Meta, dst: &Meta, dpath: &Path) -> Decision {
        let why = || {
            if src.kind == Kind::Dir {
                "the destination exists and is not a directory".to_string()
            } else {
                "the destination exists and is a directory".to_string()
            }
        };
        if self.policy.skip_mismatch {
            return Decision::Skip(why());
        }
        let q = Question::TypeMismatch {
            path: dpath.to_path_buf(),
            src: Side::of(src),
            dst: Side::of(dst),
        };
        match self.rep.ask(q) {
            Answer::SkipAll => {
                self.policy.skip_mismatch = true;
                Decision::Skip(why())
            }
            Answer::Rename(n) => rename_decision(n),
            Answer::Cancel => Decision::Cancel,
            _ => Decision::Skip(why()),
        }
    }

    /// "Error on entry". `Some(true)`: retry; `Some(false)`: the entry fails; `None`: cancel.
    pub(crate) fn decide_error(
        &mut self,
        path: &Path,
        op: &'static str,
        errno: Errno,
    ) -> Option<bool> {
        if self.policy.skip_errnos.contains(&errno) {
            return Some(false);
        }
        let q = Question::Error {
            path: path.to_path_buf(),
            op,
            errno,
        };
        match self.rep.ask(q) {
            Answer::Retry => Some(true),
            Answer::SkipAllErrno => {
                self.policy.skip_errnos.insert(errno);
                Some(false)
            }
            Answer::Cancel => None,
            _ => Some(false),
        }
    }

    /// Handles a conflict at `dst/target` for `src_meta`. Returns the decision, with
    /// `Merge` only for directory over directory. `src_res` as for
    /// [`Transfer::decide_exists`].
    pub(crate) fn resolve_conflict(
        &mut self,
        src_meta: &Meta,
        src_res: &dyn Fn() -> i128,
        dst_meta: &Meta,
        dst_dir: &Dir,
        dpath: &Path,
    ) -> Decision {
        match conflict(src_meta.kind, dst_meta.kind) {
            Conflict::FileExists => self.decide_exists(src_meta, src_res, dst_meta, dst_dir, dpath),
            Conflict::DirExists => self.decide_dir_exists(src_meta, dst_meta, dpath),
            Conflict::TypeMismatch => self.decide_mismatch(src_meta, dst_meta, dpath),
        }
    }

    // ---- entries ------------------------------------------------------------------------

    /// Copies one planned entry of `src`, a directory of origin `o`, to `dst/target`.
    pub fn entry<O: Origin<Dir = D>>(
        &mut self,
        o: &O,
        src: &D,
        node: &Node,
        dst: &Dir,
        target: OsString,
    ) -> Flow {
        if self.stopped || self.cancelled() {
            return self.stop();
        }
        let spath = src.path().join(&node.name);
        if let Some(note) = &node.note {
            self.noted(node, spath, note);
            return Flow::Continue;
        }
        self.current = spath.clone();
        match node.meta.kind {
            Kind::Dir => self.dir(o, src, node, dst, target),
            Kind::File | Kind::Symlink => self.file(o, src, node, dst, target),
            _ => {
                self.skip(node, spath, "special file");
                Flow::Continue
            }
        }
    }

    /// Ends an entry the plan already decided: failed or skipped with the note's reason.
    fn noted(&mut self, node: &Node, spath: PathBuf, note: &Note) {
        if note.is_failure() {
            self.fail(node, spath, note.reason());
        } else {
            self.skip(node, spath, note.reason());
        }
    }

    /// A regular file or a symlink.
    pub(crate) fn file<O: Origin<Dir = D>>(
        &mut self,
        o: &O,
        src: &D,
        node: &Node,
        dst: &Dir,
        mut target: OsString,
    ) -> Flow {
        let spath = src.path().join(&node.name);
        let mut overwrite = false;
        let mut check = node.meta.kind != Kind::File || node.meta.size >= PRECHECK_BYTES;
        // A `Stream`'s complete temporary file, kept across "file exists" and a failed
        // commit (P3 2.3, the M1 4.7 amendment). The guard removes it on Skip, Cancel, a
        // failure or a panic. `written` is its identity when it is a link target (A-3).
        let mut kept: Option<Unlink> = None;
        self.written = None;
        loop {
            if self.cancelled() {
                return self.stop();
            }
            let dpath = dst.path.join(&target);
            if !overwrite && check {
                match self.sys.stat_at("copy.dststat", dst.fd(), &target) {
                    Ok(dm) => {
                        // Only a local source can be the destination (P3 2.1).
                        if o.is_local() && dm.id.inode() == node.meta.id.inode() {
                            self.skip(node, spath, "source and destination are the same file");
                            return Flow::Continue;
                        }
                        // The archive being extracted is never replaced, and Overwrite is
                        // not offered (P3 3.5).
                        if self.protect == Some(dm.id.inode()) {
                            self.fail(node, spath, ARCHIVE_ITSELF);
                            return Flow::Continue;
                        }
                        let res = || o.mtime_resolution(src);
                        match self.resolve_conflict(&node.meta, &res, &dm, dst, &dpath) {
                            Decision::Overwrite => overwrite = true,
                            Decision::Rename(n) => {
                                target = n;
                                continue;
                            }
                            Decision::Skip(why) => {
                                self.skip(node, spath, why);
                                return Flow::Continue;
                            }
                            Decision::Cancel => return self.stop(),
                            Decision::Merge => unreachable!("merge offered for a file"),
                        }
                    }
                    Err(Errno::NOENT) => {}
                    Err(e) => match self.decide_error(&dpath, "stat destination", e) {
                        Some(true) => continue,
                        Some(false) => {
                            self.fail(
                                node,
                                spath,
                                EntryError::os("stat destination", e).to_string(),
                            );
                            return Flow::Continue;
                        }
                        None => return self.stop(),
                    },
                }
            }
            match self.transfer(o, src, node, dst, &target, overwrite, &mut kept) {
                Ok(m) if self.moving => {
                    // Committed: the source goes with the next flush (design 4.8 step 4).
                    self.queue(src, node, &m, dst);
                    self.entry_processed(node);
                    self.tick();
                    return self.flush_if_full(o);
                }
                Ok(_) => {
                    self.record_extracted(node, dst, &target);
                    self.done(node);
                    return Flow::Continue;
                }
                Err(Fail::SourceChanged) => {
                    self.fail(node, spath, "source changed during move; source kept");
                    return Flow::Continue;
                }
                Err(Fail::Exists) => {
                    overwrite = false;
                    check = true;
                }
                Err(Fail::Again) => {}
                Err(Fail::Cancelled) => return self.stop(),
                Err(Fail::Entry(e)) => {
                    self.fail(node, spath, e.to_string());
                    return Flow::Continue;
                }
                Err(Fail::Stream(why)) => {
                    self.fail(node, spath, why);
                    return Flow::Continue;
                }
                Err(Fail::Skip(why)) => {
                    self.skip(node, spath, why);
                    return Flow::Continue;
                }
                Err(Fail::Protected) => {
                    self.fail(node, spath, ARCHIVE_ITSELF);
                    return Flow::Continue;
                }
                Err(Fail::Os(op, errno)) => match self.decide_error(&spath, op, errno) {
                    Some(true) => {}
                    Some(false) => {
                        self.fail(node, spath, EntryError::os(op, errno).to_string());
                        return Flow::Continue;
                    }
                    None => return self.stop(),
                },
            }
        }
    }

    /// One attempt at a file or symlink: write it under a temporary name (or directly) and
    /// commit it, or link an earlier destination of the same source inode (P2 9.2) or of the
    /// member a hard-link member names (A-3), or commit the temporary file an earlier attempt
    /// at a `Stream` kept. Returns the source's metadata that the committed destination
    /// corresponds to (`S0`).
    #[allow(clippy::too_many_arguments)]
    fn transfer<'k, O: Origin<Dir = D>>(
        &mut self,
        o: &O,
        src: &D,
        node: &Node,
        dst: &'k Dir,
        target: &OsStr,
        overwrite: bool,
        kept: &mut Option<Unlink<'k>>,
    ) -> Result<Meta, Fail>
    where
        'a: 'k,
    {
        let direct = !overwrite && self.direct.contains(&dst.meta.id.domain());
        if node.meta.kind == Kind::Symlink {
            return self.transfer_symlink(o, src, node, dst, target, overwrite, direct);
        }
        if let Some(k) = o.link_target(node) {
            return self.link_extracted(k, node, dst, target, overwrite);
        }
        if let Some(tmp) = kept.take() {
            return self.commit_kept(tmp, node, dst, target, overwrite, direct, kept);
        }
        let link = self.links.key(node);
        // Whether a destination to link to exists: a data copy of this name then leaves a
        // separate file, which the report counts (P2 9.2).
        let mut separate = false;
        if let Some(k) = link
            && self.links.first.contains_key(&k)
            && let Some(local) = O::local(src)
        {
            if let Some(m) = self.link_existing(k, local, node, dst, target, overwrite)? {
                return Ok(m);
            }
            separate = true;
        }
        // An origin that lends its bytes for the attempt (an archive member, P3 2.3).
        let cancel = self.sys.cancel_flag().clone();
        let lent = o.lend(src, node, &cancel, &mut |reader, declared| {
            self.transfer_stream(reader, declared, node, dst, target, overwrite, direct, kept)
        });
        match lent {
            Some(Ok(r)) => return r,
            Some(Err(why)) => return Err(Fail::Stream(why)),
            None => {}
        }
        match o.open(src, node, self.sys.cancel_flag())? {
            OriginFile::Local { fd, meta } => {
                self.transfer_local(fd, meta, dst, target, overwrite, direct, link, separate)
            }
            OriginFile::Stream {
                mut reader,
                declared,
            } => self.transfer_stream(
                &mut *reader,
                declared,
                node,
                dst,
                target,
                overwrite,
                direct,
                kept,
            ),
        }
    }

    /// A committed file of a member that hard-link members link to (A-3): the destination
    /// is kept, with the identity it was written with, for the links (P3 3.5).
    fn record_extracted(&mut self, node: &Node, dst: &Dir, target: &OsStr) {
        if self.link_targets.is_empty() {
            return;
        }
        let k = key(node);
        if let Some(id) = self.written.take()
            && self.link_targets.contains(&k)
        {
            self.extracted.insert(
                k,
                Extracted {
                    dir: dst.fd.clone(),
                    name: target.to_owned(),
                    id,
                },
            );
        }
    }

    /// A hard-link member (A-3, P3 3.5): the destination the job extracted for the member it
    /// names, opened `O_PATH` by name and checked by identity, is linked under a temporary
    /// name through that fd, so a name that replaced the destination after the check is
    /// never linked; the link commits with the M1 4.7 step 5 rules. Without such a
    /// destination, or when it changed, the member is skipped.
    fn link_extracted(
        &mut self,
        k: u64,
        node: &Node,
        dst: &Dir,
        target: &OsStr,
        overwrite: bool,
    ) -> Result<Meta, Fail> {
        let sys = self.sys;
        let not_extracted = || Fail::Skip(LINK_NOT_EXTRACTED.into());
        let Some(first) = self.extracted.get(&k).cloned() else {
            return Err(not_extracted());
        };
        let Ok(target_fd) = sys.open_path("link.open", first.dir.as_fd(), &first.name) else {
            return Err(not_extracted());
        };
        match sys.stat_fd(target_fd.as_fd()) {
            Ok(m) if m.kind == Kind::File && m.id.inode() == first.id => {}
            _ => return Err(not_extracted()),
        }
        let tmp = loop {
            if self.cancelled() {
                return Err(Fail::Cancelled);
            }
            let tmp = self.next_partial(target);
            match sys.link_fd("link.link", target_fd.as_fd(), dst.fd(), &tmp) {
                Ok(()) => break tmp,
                Err(Errno::EXIST) => continue,
                // No link possible here (`ENOENT`: the inode lost its last name meanwhile).
                Err(e @ (Errno::XDEV | Errno::MLINK | Errno::PERM | Errno::OPNOTSUPP)) => {
                    return Err(Fail::Skip(format!(
                        "hard link not made: {}",
                        crate::fsops::walk::errno_text(e)
                    )));
                }
                Err(Errno::NOENT) => return Err(not_extracted()),
                Err(e) => return Err(Fail::Os("link", e)),
            }
        };
        let mut guard = Unlink::new(sys, dst.fd(), tmp.clone());
        if overwrite
            && let Ok(d) = sys.stat_at("copy.dststat", dst.fd(), target)
            && d.id.inode() == first.id
        {
            // The name already holds this inode; the guard removes the temporary name.
            drop(guard);
            return Ok(node.meta);
        }
        self.commit(dst, &tmp, target, overwrite, &mut guard)?;
        Ok(node.meta)
    }

    /// One attempt at a local file (M1 4.7, P2 9): `fin` is open for reading and `m0` is its
    /// `S0`.
    #[allow(clippy::too_many_arguments)]
    fn transfer_local(
        &mut self,
        fin: OwnedFd,
        m0: Meta,
        dst: &Dir,
        target: &OsStr,
        overwrite: bool,
        direct: bool,
        link: Option<(u64, u64)>,
        separate: bool,
    ) -> Result<Meta, Fail> {
        let sys = self.sys;
        if direct {
            // Direct-write mode (I-2 exception): the O_EXCL create takes the place of the
            // commit; the guard removes the name on failure or cancel.
            let fout = match sys.create_excl("commit.direct", dst.fd(), target, 0o600) {
                Ok(f) => f,
                Err(e) if is_conflict_errno(e) => return Err(Fail::Exists),
                Err(e) => return Err(Fail::Os("create", e)),
            };
            let mut guard = Unlink::new(sys, dst.fd(), target.to_owned());
            self.data(fin.as_fd(), fout.as_fd(), &m0, (m0.id.dev, dst.meta.id.dev))?;
            self.metadata(fout.as_fd(), &m0, dst)?;
            let dst_id = self.link_identity(link, fout.as_fd());
            // In direct-write mode the check runs after the last byte; a failed check
            // unlinks the destination name through the guard.
            self.change_check(fin.as_fd(), &m0)?;
            guard.disarm();
            self.copied(link, dst_id, dst, target, &m0, separate);
            return Ok(m0);
        }
        let (fout, tmp) = self.create_partial(dst, target)?;
        let mut guard = Unlink::new(sys, dst.fd(), tmp.clone());
        self.data(fin.as_fd(), fout.as_fd(), &m0, (m0.id.dev, dst.meta.id.dev))?;
        self.metadata(fout.as_fd(), &m0, dst)?;
        let dst_id = self.link_identity(link, fout.as_fd());
        drop(fout);
        self.change_check(fin.as_fd(), &m0)?;
        self.commit(dst, &tmp, target, overwrite, &mut guard)?;
        self.copied(link, dst_id, dst, target, &m0, separate);
        Ok(m0)
    }

    /// One attempt at a `Stream` file (P3 2.3): read it into the job buffer and write it to
    /// a temporary file, or in direct-write mode to the final name, then apply the planned
    /// mode and times and commit. A commit that does not complete (the name is taken, the
    /// filesystem turns out to need direct write, an OS error) leaves the complete
    /// temporary file in `kept` for the next attempt: the stream is never read again.
    #[allow(clippy::too_many_arguments)]
    fn transfer_stream<'k>(
        &mut self,
        reader: &mut dyn Read,
        declared: u64,
        node: &Node,
        dst: &'k Dir,
        target: &OsStr,
        overwrite: bool,
        direct: bool,
        kept: &mut Option<Unlink<'k>>,
    ) -> Result<Meta, Fail>
    where
        'a: 'k,
    {
        let sys = self.sys;
        let m = node.meta;
        if direct {
            // Direct-write mode (I-2 exception): the O_EXCL create takes the place of the
            // commit; the guard removes the name on failure or cancel.
            let fout = match sys.create_excl("commit.direct", dst.fd(), target, 0o600) {
                Ok(f) => f,
                Err(e) if is_conflict_errno(e) => return Err(Fail::Exists),
                Err(e) => return Err(Fail::Os("create", e)),
            };
            let mut guard = Unlink::new(sys, dst.fd(), target.to_owned());
            self.stream_data(reader, fout.as_fd(), declared)?;
            self.metadata(fout.as_fd(), &m, dst)?;
            self.note_written(node, fout.as_fd());
            guard.disarm();
            return Ok(m);
        }
        let (fout, tmp) = self.create_partial(dst, target)?;
        let mut guard = Unlink::new(sys, dst.fd(), tmp.clone());
        self.stream_data(reader, fout.as_fd(), declared)?;
        self.metadata(fout.as_fd(), &m, dst)?;
        self.note_written(node, fout.as_fd());
        drop(fout);
        match self.commit(dst, &tmp, target, overwrite, &mut guard) {
            Ok(()) => Ok(m),
            Err(e @ (Fail::Exists | Fail::Again | Fail::Os(..))) => {
                *kept = Some(guard);
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    /// The identity of a written member that hard-link members link to (A-3). One `fstat`,
    /// for such members only.
    fn note_written(&mut self, node: &Node, fout: BorrowedFd) {
        if !self.link_targets.is_empty() && self.link_targets.contains(&key(node)) {
            self.written = self.sys.stat_fd(fout).ok().map(|m| m.id.inode());
        }
    }

    /// Reads a `Stream` into the job buffer and writes it to `to` (P3 2.3, A-4). Each read
    /// asks for at most one byte more than `declared` still allows, so the first byte past
    /// `declared` is seen without being written: the entry then fails with
    /// [`SIZE_MISMATCH`], as one that ends short does. Cancel is checked between chunks.
    fn stream_data(
        &mut self,
        reader: &mut dyn Read,
        to: BorrowedFd,
        declared: u64,
    ) -> Result<(), Fail> {
        if self.buf.is_empty() {
            self.buf = vec![0; BUF];
        }
        let mut buf = std::mem::take(&mut self.buf);
        let r = self.stream_chunks(reader, to, declared, &mut buf);
        self.buf = buf;
        r
    }

    fn stream_chunks(
        &mut self,
        reader: &mut dyn Read,
        to: BorrowedFd,
        declared: u64,
        buf: &mut [u8],
    ) -> Result<(), Fail> {
        let read_error = |e: std::io::Error| {
            Fail::Stream(match e.raw_os_error() {
                Some(n) => EntryError::os("read", Errno::from_raw_os_error(n)).to_string(),
                None => e.to_string(),
            })
        };
        let mut written: u64 = 0;
        loop {
            if self.cancelled() {
                return Err(Fail::Cancelled);
            }
            let room = declared - written;
            let want = room.saturating_add(1).min(buf.len() as u64) as usize;
            self.sys
                .hit("copy.chunk")
                .map_err(|e| Fail::Stream(EntryError::os("read", e).to_string()))?;
            let n = loop {
                match reader.read(&mut buf[..want]) {
                    Ok(n) => break n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    // A decoder that watches the job's cancel flag stops with an error.
                    Err(_) if self.cancelled() => return Err(Fail::Cancelled),
                    Err(e) => return Err(read_error(e)),
                }
            };
            if n == 0 {
                if written == declared {
                    return Ok(());
                }
                return Err(Fail::Stream(SIZE_MISMATCH.into()));
            }
            if n as u64 > room {
                return Err(Fail::Stream(SIZE_MISMATCH.into()));
            }
            self.sys
                .write_all("copy.write", to, &buf[..n])
                .map_err(|e| Fail::Os("write", e))?;
            written += n as u64;
            self.bytes_done += n as u64;
            self.tick();
        }
    }

    /// Commits the complete temporary file `tmp` that an earlier attempt at a `Stream` kept
    /// (P3 2.3, the M1 4.7 amendment): with the atomic replace after Overwrite, under the new
    /// name after Rename, and without reading the stream again. A commit that does not
    /// complete keeps it again for the next attempt. In direct-write mode, found at this
    /// file's own commit, the temporary file is copied into the final name, created
    /// `O_EXCL`, and then removed.
    #[allow(clippy::too_many_arguments)]
    fn commit_kept<'k>(
        &mut self,
        mut tmp: Unlink<'k>,
        node: &Node,
        dst: &'k Dir,
        target: &OsStr,
        overwrite: bool,
        direct: bool,
        kept: &mut Option<Unlink<'k>>,
    ) -> Result<Meta, Fail>
    where
        'a: 'k,
    {
        let m = node.meta;
        let r = if direct {
            self.copy_kept(&tmp, &m, dst, target)
        } else {
            let name = tmp.name().to_owned();
            self.commit(dst, &name, target, overwrite, &mut tmp)
        };
        match r {
            // The guard removes the temporary name unless the commit took it.
            Ok(()) => Ok(m),
            Err(e @ (Fail::Exists | Fail::Again | Fail::Os(..))) => {
                *kept = Some(tmp);
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    /// Direct-write mode for a kept temporary file: its bytes go to the final name, created
    /// `O_EXCL` (M1 4.7 step 5), with the planned mode and times. The progress already
    /// counted them when the stream was read.
    fn copy_kept(&mut self, tmp: &Unlink, m: &Meta, dst: &Dir, target: &OsStr) -> Result<(), Fail> {
        let sys = self.sys;
        let fout = match sys.create_excl("commit.direct", dst.fd(), target, 0o600) {
            Ok(f) => f,
            Err(e) if is_conflict_errno(e) => return Err(Fail::Exists),
            Err(e) => return Err(Fail::Os("create", e)),
        };
        let mut guard = Unlink::new(sys, dst.fd(), target.to_owned());
        let (fin, tm) = open_for_read(sys, dst.fd(), tmp.name(), None)?;
        let counted = self.bytes_done;
        let r = self.data(fin.as_fd(), fout.as_fd(), &tm, (tm.id.dev, dst.meta.id.dev));
        self.bytes_done = counted;
        r?;
        self.metadata(fout.as_fd(), m, dst)?;
        guard.disarm();
        Ok(())
    }

    /// The identity of a data copy of an in-set name of a multi-linked inode, which a later
    /// name checks before it links to it (P2 9.2). One `fstat`, for such names only.
    fn link_identity(&self, link: Option<(u64, u64)>, fout: BorrowedFd) -> Option<(u64, u64)> {
        link.and_then(|_| self.sys.stat_fd(fout).ok().map(|m| m.id.inode()))
    }

    /// A committed data copy of an in-set name (P2 9.2): it becomes the destination later
    /// names link to, since it holds the newest content; `separate` counts it in the report
    /// note when a destination to link to existed.
    fn copied(
        &mut self,
        link: Option<(u64, u64)>,
        dst_id: Option<(u64, u64)>,
        dst: &Dir,
        target: &OsStr,
        m0: &Meta,
        separate: bool,
    ) {
        let (Some(k), Some(id)) = (link, dst_id) else {
            return;
        };
        if separate {
            self.links.fallbacks += 1;
        }
        self.links.first.insert(
            k,
            FirstDest {
                dir: dst.fd.clone(),
                name: target.to_owned(),
                domain: dst.meta.id.domain(),
                id,
                size: m0.size,
                mtime: m0.mtime,
                ctime: m0.ctime,
            },
        );
    }

    /// Another in-set name of a multi-linked source inode (P2 9.2): link the destination the
    /// job committed for that inode under a temporary name, and commit it with the M1 4.7
    /// step 5 rules. `None`: copy the data instead, because that destination is gone or
    /// replaced, the source changed since, or `linkat` failed with `EXDEV`, `EMLINK`,
    /// `EPERM` or `EOPNOTSUPP`.
    fn link_existing(
        &mut self,
        k: (u64, u64),
        src: &Dir,
        node: &Node,
        dst: &Dir,
        target: &OsStr,
        overwrite: bool,
    ) -> Result<Option<Meta>, Fail> {
        let Some(first) = self.links.first.get(&k).cloned() else {
            return Ok(None);
        };
        let sys = self.sys;
        let domain = dst.meta.id.domain();
        if first.domain != domain || self.direct.contains(&domain) {
            // A link cannot leave its filesystem, and a direct-write filesystem has none.
            return Ok(None);
        }
        // The source must still hold the content the first destination got. Anything
        // else about it (gone, another inode now) the data path finds and reports.
        let now = match sys.stat_at("link.srcstat", src.fd(), &node.name) {
            Ok(m) if m.kind == Kind::File && m.id.inode() == k => m,
            _ => return Ok(None),
        };
        if (now.size, now.mtime, now.ctime) != (first.size, first.mtime, first.ctime) {
            return Ok(None);
        }
        // The first destination, opened by name and checked by identity. The link is made
        // through this fd, so an entry that replaces the name after the check is never
        // linked; an inode that lost its last name fails with ENOENT.
        let Ok(target_fd) = sys.open_path("link.open", first.dir.as_fd(), &first.name) else {
            return Ok(None);
        };
        match sys.stat_fd(target_fd.as_fd()) {
            Ok(m) if m.kind == Kind::File && m.id.inode() == first.id => {}
            _ => return Ok(None),
        }
        let tmp = loop {
            if self.cancelled() {
                return Err(Fail::Cancelled);
            }
            let tmp = self.next_partial(target);
            match sys.link_fd("link.link", target_fd.as_fd(), dst.fd(), &tmp) {
                Ok(()) => break tmp,
                Err(Errno::EXIST) => continue,
                // No link possible here (`ENOENT`: the inode lost its last name meanwhile).
                Err(Errno::XDEV | Errno::MLINK | Errno::PERM | Errno::OPNOTSUPP | Errno::NOENT) => {
                    return Ok(None);
                }
                Err(e) => return Err(Fail::Os("link", e)),
            }
        };
        let mut guard = Unlink::new(sys, dst.fd(), tmp.clone());
        if overwrite
            && let Ok(d) = sys.stat_at("copy.dststat", dst.fd(), target)
            && d.id.inode() == first.id
        {
            // The name already holds this inode. A rename between two links of one inode
            // does nothing and would leave the temporary name; the guard removes it.
            drop(guard);
            self.bytes_done += now.size;
            return Ok(Some(now));
        }
        self.commit(dst, &tmp, target, overwrite, &mut guard)?;
        self.bytes_done += now.size;
        Ok(Some(now))
    }

    /// Design 4.8 step 2: before a move commits, the source must still match `S0`.
    fn change_check(&mut self, fin: BorrowedFd, m0: &Meta) -> Result<(), Fail> {
        if !self.moving {
            return Ok(());
        }
        self.sys
            .hit("move.check")
            .map_err(|e| Fail::Os("stat source", e))?;
        let now = self
            .sys
            .stat_fd(fin)
            .map_err(|e| Fail::Os("stat source", e))?;
        if now.snapshot() != m0.snapshot() {
            return Err(Fail::SourceChanged);
        }
        Ok(())
    }

    /// A symlink: its target from the origin, byte-identical, then `symlinkat` under a
    /// temporary name and the commit of M1 4.7. Before a local move commits, the link must
    /// still be the one that was read (M1 4.8 step 2).
    #[allow(clippy::too_many_arguments)]
    fn transfer_symlink<O: Origin<Dir = D>>(
        &mut self,
        o: &O,
        src: &D,
        node: &Node,
        dst: &Dir,
        target: &OsStr,
        overwrite: bool,
        direct: bool,
    ) -> Result<Meta, Fail> {
        let sys = self.sys;
        let (link, now) = o.read_link(src, node)?;
        if self.moving
            && let Some(local) = O::local(src)
        {
            let again = sys
                .stat_at("move.check", local.fd(), &node.name)
                .map_err(|e| Fail::Os("stat source", e))?;
            if again.snapshot() != now.snapshot() {
                return Err(Fail::SourceChanged);
            }
        }
        if direct {
            return match sys.symlink("commit.direct", &link, dst.fd(), target) {
                Ok(()) => Ok(now),
                Err(e) if is_conflict_errno(e) => Err(Fail::Exists),
                Err(e) => Err(Fail::Os("create symlink", e)),
            };
        }
        let tmp = loop {
            let tmp = self.next_partial(target);
            match sys.symlink("copy.tmp", &link, dst.fd(), &tmp) {
                Ok(()) => break tmp,
                Err(Errno::EXIST) => continue,
                Err(e) => return Err(Fail::Os("create symlink", e)),
            }
        };
        let mut guard = Unlink::new(sys, dst.fd(), tmp.clone());
        self.commit(dst, &tmp, target, overwrite, &mut guard)?;
        Ok(now)
    }

    fn next_partial(&mut self, target: &OsStr) -> OsString {
        self.tmp_seq += 1;
        partial_name_with(target, self.tmp_base.wrapping_add(self.tmp_seq))
    }

    /// `.<name>.mc-partial-<random>`, `O_CREAT | O_EXCL`, mode 0600 (design 4.7 step 2).
    fn create_partial(&mut self, dst: &Dir, target: &OsStr) -> Result<(OwnedFd, OsString), Fail> {
        loop {
            if self.cancelled() {
                return Err(Fail::Cancelled);
            }
            let tmp = self.next_partial(target);
            match self.sys.create_excl("copy.tmp", dst.fd(), &tmp, 0o600) {
                Ok(f) => return Ok((f, tmp)),
                Err(Errno::EXIST) => continue,
                Err(e) => return Err(Fail::Os("create", e)),
            }
        }
    }

    /// Copies the data (design 4.7 step 3). A file with fewer allocated blocks than its size
    /// needs has holes and takes the sparse path (P2 9.1); the test uses the metadata
    /// already at hand, so a dense file costs no extra syscall.
    pub(crate) fn data(
        &mut self,
        from: BorrowedFd,
        to: BorrowedFd,
        m0: &Meta,
        devs: (u64, u64),
    ) -> Result<(), Fail> {
        let size = m0.size;
        if m0.blocks.saturating_mul(512) < size && self.sparse(from, to, size, devs)? {
            return Ok(());
        }
        let mut copied: u64 = 0;
        let mut kernel = !self.no_kernel_copy.contains(&devs);
        loop {
            if self.cancelled() {
                return Err(Fail::Cancelled);
            }
            let n = if kernel {
                match self.sys.copy_range("copy.chunk", from, to, CHUNK) {
                    // EOF before the snapshot size: the source may report no size (a
                    // pseudo-file), so let read() find the real end.
                    Ok(0) if copied < size => {
                        kernel = false;
                        continue;
                    }
                    Ok(n) => n,
                    Err(Errno::XDEV | Errno::OPNOTSUPP | Errno::NOSYS) => {
                        self.no_kernel_copy.insert(devs);
                        kernel = false;
                        continue;
                    }
                    Err(Errno::INVAL) => {
                        self.refuse_same_inode(from, to)?;
                        kernel = false;
                        continue;
                    }
                    Err(e) => return Err(Fail::Os("copy", e)),
                }
            } else {
                if self.buf.is_empty() {
                    self.buf = vec![0; BUF];
                }
                let mut buf = std::mem::take(&mut self.buf);
                let r = self.sys.read("copy.chunk", from, &mut buf);
                let n = match r {
                    Ok(n) => n,
                    Err(e) => {
                        self.buf = buf;
                        return Err(Fail::Os("read", e));
                    }
                };
                let w = self.sys.write_all("copy.write", to, &buf[..n]);
                self.buf = buf;
                w.map_err(|e| Fail::Os("write", e))?;
                n
            };
            if n == 0 {
                return Ok(());
            }
            copied += n as u64;
            self.bytes_done += n as u64;
            self.tick();
        }
    }

    /// `EINVAL` from `copy_file_range`: the entry fails when source and destination are one
    /// inode, which is never read and written at once (I-4); otherwise the caller falls
    /// back to reading and writing.
    fn refuse_same_inode(&self, from: BorrowedFd, to: BorrowedFd) -> Result<(), Fail> {
        let (a, b) = (self.sys.stat_fd(from), self.sys.stat_fd(to));
        if let (Ok(a), Ok(b)) = (a, b)
            && a.id.inode() == b.id.inode()
        {
            return Err(Fail::Entry(EntryError::Os {
                op: "copy",
                errno: Errno::INVAL,
            }));
        }
        Ok(())
    }

    /// The sparse path (P2 9.1): walk the source's data segments with `SEEK_DATA` and
    /// `SEEK_HOLE`, copy each at its own offset, and set the size with `ftruncate` at the
    /// end, so every skipped range stays a hole (an all-hole file is only the `ftruncate`).
    /// `Ok(false)`: the source filesystem has no hole support (`EINVAL` or `EOPNOTSUPP`
    /// from the first `SEEK_DATA`); nothing was written, and the caller takes the
    /// contiguous loop.
    ///
    /// The walk runs to the end of the data (`ENXIO`), as the contiguous loop runs to EOF;
    /// the size is the larger of `size` (from `S0`) and the end of the last segment, or the
    /// point where the source ended early (a pseudo-file whose size promised more). Hole
    /// bytes count as done when they are skipped.
    fn sparse(
        &mut self,
        from: BorrowedFd,
        to: BorrowedFd,
        size: u64,
        devs: (u64, u64),
    ) -> Result<bool, Fail> {
        let mut kernel = !self.no_kernel_copy.contains(&devs);
        // Where the next SEEK_DATA starts: the end of the last segment.
        let mut off = 0;
        // How far progress counts this file.
        let mut counted = 0;
        let mut first = true;
        let len = loop {
            if self.cancelled() {
                return Err(Fail::Cancelled);
            }
            let data = match self.sys.seek_data("copy.seekdata", from, off) {
                Ok(d) => d,
                Err(Errno::NXIO) => break size.max(off),
                Err(Errno::INVAL | Errno::OPNOTSUPP) if first => return Ok(false),
                Err(e) => return Err(Fail::Os("seek", e)),
            };
            first = false;
            let end = match self.sys.seek_hole("copy.seekhole", from, data) {
                Ok(h) => h,
                // The source shrank below `data` meanwhile: no data there any more.
                Err(Errno::NXIO) => break size.max(off),
                Err(e) => return Err(Fail::Os("seek", e)),
            };
            // The hole before the segment is done without being read; the segment counts
            // its own bytes.
            self.bytes_done += data.saturating_sub(counted);
            match self.segment(from, to, data, end, &mut kernel, devs)? {
                Ok(()) => {
                    off = end;
                    counted = end;
                }
                Err(eof) => {
                    counted = eof;
                    break eof;
                }
            }
        };
        self.sys
            .ftruncate("copy.truncate", to, len)
            .map_err(|e| Fail::Os("truncate", e))?;
        self.bytes_done += len.saturating_sub(counted);
        self.tick();
        Ok(true)
    }

    /// Copies `[start, end)` of a data segment at the same offsets (P2 9.1 step 3), with
    /// the rules of design 4.7 step 3: short counts continue, `EXDEV`, `EOPNOTSUPP` and
    /// `ENOSYS` fall back to `pread`/`pwrite` for this pair of devices, `EINVAL` for this
    /// file unless it would read and write one inode (I-4). Cancel is checked between
    /// chunks. `Ok(Err(at))`: the source ended at `at`, before `end`.
    fn segment(
        &mut self,
        from: BorrowedFd,
        to: BorrowedFd,
        start: u64,
        end: u64,
        kernel: &mut bool,
        devs: (u64, u64),
    ) -> Result<Result<(), u64>, Fail> {
        let sys = self.sys;
        let mut pos = start;
        while pos < end {
            if self.cancelled() {
                return Err(Fail::Cancelled);
            }
            let left = end - pos;
            let n = if *kernel {
                let (mut i, mut o) = (pos, pos);
                let want = left.min(CHUNK as u64) as usize;
                match sys.copy_range_at("copy.chunk", from, &mut i, to, &mut o, want) {
                    // No data where the size promised some (a pseudo-file): let pread find
                    // the real end.
                    Ok(0) => {
                        *kernel = false;
                        continue;
                    }
                    Ok(n) => n,
                    Err(Errno::XDEV | Errno::OPNOTSUPP | Errno::NOSYS) => {
                        self.no_kernel_copy.insert(devs);
                        *kernel = false;
                        continue;
                    }
                    Err(Errno::INVAL) => {
                        self.refuse_same_inode(from, to)?;
                        *kernel = false;
                        continue;
                    }
                    Err(e) => return Err(Fail::Os("copy", e)),
                }
            } else {
                if self.buf.is_empty() {
                    self.buf = vec![0; BUF];
                }
                let mut buf = std::mem::take(&mut self.buf);
                let want = left.min(BUF as u64) as usize;
                let r = match sys.pread("copy.chunk", from, &mut buf[..want], pos) {
                    Ok(0) => Ok(0),
                    Ok(n) => sys
                        .pwrite_all("copy.write", to, &buf[..n], pos)
                        .map(|()| n)
                        .map_err(|e| Fail::Os("write", e)),
                    Err(e) => Err(Fail::Os("read", e)),
                };
                self.buf = buf;
                match r? {
                    0 => return Ok(Err(pos)),
                    n => n,
                }
            };
            pos += n as u64;
            self.bytes_done += n as u64;
            self.tick();
        }
        Ok(Ok(()))
    }

    /// A directory's mode (setuid and setgid cleared, sticky kept) and times, applied in
    /// post-order. vfat and exfat refusals are not errors, as for files.
    fn dir_metadata(&mut self, d: &Dir, m: &Meta) -> Result<(), Errno> {
        let t = self.f_type(d);
        let lax = |e: Errno| {
            matches!(e, Errno::PERM | Errno::OPNOTSUPP) && (t == magic::VFAT || t == magic::EXFAT)
        };
        match self.sys.fchmod("copy.chmod", d.fd(), m.perm & 0o1777) {
            Err(e) if !lax(e) => return Err(e),
            _ => {}
        }
        match self.sys.futimens("copy.utimes", d.fd(), m.atime, m.mtime) {
            Err(e) if !lax(e) => Err(e),
            _ => Ok(()),
        }
    }

    /// Mode with setuid and setgid cleared, then atime and mtime (design 4.7 step 4). On
    /// vfat and exfat a refused mode change is not an error: they keep what they can.
    pub(crate) fn metadata(&mut self, to: BorrowedFd, m: &Meta, dst: &Dir) -> Result<(), Fail> {
        if let Err(e) = self.sys.fchmod("copy.chmod", to, m.perm & !0o6000) {
            let t = self.f_type(dst);
            if !(matches!(e, Errno::PERM | Errno::OPNOTSUPP)
                && (t == magic::VFAT || t == magic::EXFAT))
            {
                return Err(Fail::Os("set permissions", e));
            }
        }
        self.sys
            .futimens("copy.utimes", to, m.atime, m.mtime)
            .map_err(|e| Fail::Os("set times", e))
    }

    /// Commits `tmp` to `target` (design 4.7 step 5).
    pub(crate) fn commit(
        &mut self,
        dst: &Dir,
        tmp: &OsStr,
        target: &OsStr,
        overwrite: bool,
        guard: &mut Unlink,
    ) -> Result<(), Fail> {
        let sys = self.sys;
        if overwrite {
            // The archive being extracted is never replaced (P3 3.5).
            if let Some(p) = self.protect
                && let Ok(d) = sys.stat_at("copy.dststat", dst.fd(), target)
                && d.id.inode() == p
            {
                return Err(Fail::Protected);
            }
            // Atomic replace: the old entry is never truncated or opened for writing.
            return match sys.rename("commit.replace", dst.fd(), tmp, dst.fd(), target, false) {
                Ok(()) => {
                    guard.disarm();
                    Ok(())
                }
                Err(e) if is_conflict_errno(e) => Err(Fail::Exists),
                Err(e) => Err(Fail::Os("rename", e)),
            };
        }
        match sys.rename("commit.rename", dst.fd(), tmp, dst.fd(), target, true) {
            Ok(()) => {
                guard.disarm();
                Ok(())
            }
            Err(Errno::INVAL) => match sys.link("commit.link", dst.fd(), tmp, dst.fd(), target) {
                // The guard unlinks the temporary name; the final link stays.
                Ok(()) => Ok(()),
                Err(e) if is_conflict_errno(e) => Err(Fail::Exists),
                Err(Errno::PERM | Errno::OPNOTSUPP) => {
                    // Neither RENAME_NOREPLACE nor hard links: remember it for this
                    // filesystem, drop the temporary file, and write this and every later
                    // file of the job there directly.
                    self.direct.insert(dst.meta.id.domain());
                    Err(Fail::Again)
                }
                Err(e) => Err(Fail::Os("link", e)),
            },
            Err(e) if is_conflict_errno(e) => Err(Fail::Exists),
            Err(e) => Err(Fail::Os("rename", e)),
        }
    }

    /// Opens a planned source directory and creates (or merges into) its destination
    /// `dst/target`, which it opens: the part of [`Transfer::dir`] before the children,
    /// shared with the one-pass walk (P3 3.5). Returns the source directory, the destination
    /// directory and whether this job created it. `Err` is how the entry ended: skipped,
    /// failed, or the job stopped.
    fn open_dirs<O: Origin<Dir = D>>(
        &mut self,
        o: &O,
        src: &D,
        node: &Node,
        dst: &Dir,
        mut target: OsString,
    ) -> Result<(D, Dir, bool), Flow> {
        let sys = self.sys;
        let spath = src.path().join(&node.name);
        let sdir = loop {
            match o.open_dir(src, node) {
                Ok(d) => break d,
                Err(EntryError::Os { op, errno }) => match self.decide_error(&spath, op, errno) {
                    Some(true) => continue,
                    Some(false) => {
                        self.fail(node, spath, EntryError::Os { op, errno }.to_string());
                        return Err(Flow::Continue);
                    }
                    None => return Err(self.stop()),
                },
                Err(e) => {
                    self.fail(node, spath, e.to_string());
                    return Err(Flow::Continue);
                }
            }
        };
        let created = loop {
            if self.cancelled() {
                return Err(self.stop());
            }
            let dpath = dst.path.join(&target);
            match sys.mkdir("copy.mkdir", dst.fd(), &target, 0o700) {
                Ok(()) => break true,
                Err(Errno::EXIST) => {
                    let dm = match sys.stat_at("copy.dststat", dst.fd(), &target) {
                        Ok(m) => m,
                        Err(Errno::NOENT) => continue,
                        Err(e) => {
                            self.fail(
                                node,
                                spath,
                                EntryError::os("stat destination", e).to_string(),
                            );
                            return Err(Flow::Continue);
                        }
                    };
                    let res = || o.mtime_resolution(src);
                    match self.resolve_conflict(&node.meta, &res, &dm, dst, &dpath) {
                        Decision::Merge => break false,
                        Decision::Rename(n) => target = n,
                        Decision::Skip(why) => {
                            self.skip(node, spath, why);
                            return Err(Flow::Continue);
                        }
                        Decision::Cancel => return Err(self.stop()),
                        Decision::Overwrite => unreachable!("overwrite offered for a directory"),
                    }
                }
                Err(e) => match self.decide_error(&dpath, "make directory", e) {
                    Some(true) => {}
                    Some(false) => {
                        self.fail(node, spath, EntryError::os("make directory", e).to_string());
                        return Err(Flow::Continue);
                    }
                    None => return Err(self.stop()),
                },
            }
        };
        let dfd = match sys.open_dir("copy.opendst", dst.fd(), &target) {
            Ok(f) => f,
            Err(e) => {
                let why = if matches!(e, Errno::LOOP | Errno::NOTDIR) {
                    EntryError::TypeChanged
                } else {
                    EntryError::os("open destination directory", e)
                };
                self.fail(node, spath, why.to_string());
                return Err(Flow::Continue);
            }
        };
        let dmeta = match sys.stat_fd(dfd.as_fd()) {
            Ok(m) => m,
            Err(e) => {
                self.fail(
                    node,
                    spath,
                    EntryError::os("stat destination", e).to_string(),
                );
                return Err(Flow::Continue);
            }
        };
        let ddir = Dir {
            fd: Arc::new(dfd),
            meta: dmeta,
            path: dst.path.join(&target),
        };
        Ok((sdir, ddir, created))
    }

    /// A directory: create (or merge into) the destination, recurse, then apply the
    /// source's mode and times in post-order to a directory this job created.
    pub(crate) fn dir<O: Origin<Dir = D>>(
        &mut self,
        o: &O,
        src: &D,
        node: &Node,
        dst: &Dir,
        target: OsString,
    ) -> Flow {
        let spath = src.path().join(&node.name);
        let (sdir, ddir, created) = match self.open_dirs(o, src, node, dst, target) {
            Ok(x) => x,
            Err(flow) => return flow,
        };
        for child in &node.children {
            if self.entry(o, &sdir, child, &ddir, child.name.clone()) == Flow::Stop {
                return Flow::Stop;
            }
        }
        let mut meta_ok = true;
        if created {
            if self.moving {
                // The flush before the source directory goes must cover this mkdir.
                self.batch.sync_dir(&ddir);
            }
            if let Err(e) = self.dir_metadata(&ddir, &node.meta) {
                meta_ok = false;
                let kept = if self.moving {
                    "; source directory kept"
                } else {
                    ""
                };
                self.report.fail(
                    spath.clone(),
                    format!(
                        "set directory metadata: {}{kept}",
                        crate::fsops::walk::errno_text(e)
                    ),
                );
            }
        }
        if self.moving {
            // The flush covers this directory's children; only then may the source
            // directory go (design 4.8 step 6). After a metadata failure it stays.
            if !meta_ok {
                return self.flush(o);
            }
            return self.finish_source_dir(o, src, node, &spath);
        }
        if meta_ok {
            self.done(node);
        }
        Flow::Continue
    }
}

fn rename_decision(n: OsString) -> Decision {
    if super::plan::valid_component(&n) {
        Decision::Rename(n)
    } else {
        Decision::Skip("the new name is not valid".into())
    }
}

/// `(non-directory entries, bytes)` of a planned subtree, the node itself included.
pub(crate) fn subtree_counts(node: &Node) -> (u64, u64) {
    match node.meta.kind {
        Kind::Dir => node.children.iter().fold((0, 0), |(e, b), c| {
            let (ce, cb) = subtree_counts(c);
            (e + ce, b + cb)
        }),
        Kind::File => (1, node.meta.size),
        _ => (1, 0),
    }
}

/// Where a copy or move goes: into an existing directory, or, for a single source, to a
/// new path. Resolved once, on the worker.
pub(crate) fn resolve_destination(
    sys: &Sys,
    names: &[OsString],
    dst: &Path,
) -> Result<(Dir, Vec<OsString>), String> {
    if let Ok(m) = sys.stat_path(dst)
        && m.kind == Kind::Dir
    {
        let d = Dir::open_root(sys, dst).map_err(|e| format!("{}: {e}", dst.display()))?;
        return Ok((d, names.to_vec()));
    }
    if names.len() != 1 {
        return Err(format!("{}: no such directory", dst.display()));
    }
    let (Some(parent), Some(name)) = (dst.parent(), dst.file_name()) else {
        return Err(format!("{}: not a valid destination", dst.display()));
    };
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let d = Dir::open_root(sys, parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    Ok((d, vec![name.to_owned()]))
}

/// One opened group of a copy or move with its plan and, per selected name, its target
/// name in the destination.
pub(crate) struct Part<D = Dir> {
    pub src: D,
    pub plan: Plan,
    pub targets: Vec<OsString>,
}

/// The shared start of copy and move over groups (P2 2.2): open the groups through the
/// origin, resolve the destination once with the M1 rules (an existing directory, or a new
/// path for exactly one source name in total), then plan every group with the checks over
/// their union. One [`Transfer`] serves all groups, so standing answers carry across them.
/// Only a local origin keeps the hard-link map (P2 9.2, P3 2.3). `Err` is the final report
/// (refused, or cancelled during the scan).
#[allow(clippy::type_complexity)]
pub(crate) fn prepare<'a, 'u, O: Origin>(
    o: &O,
    sys: &'a Sys,
    ui: &'u mut dyn super::question::Interaction,
    verb: Verb,
    groups: &[Group],
    dst: &Path,
) -> Result<(Transfer<'a, 'u, O::Dir>, Dir, Vec<Part<O::Dir>>), Box<Report>> {
    let jverb = match verb {
        Verb::Move => JobVerb::Move,
        _ => JobVerb::Copy,
    };
    let opened = o.open_groups(jverb, groups)?;
    let names: Vec<OsString> = groups.iter().flat_map(|g| g.names.clone()).collect();
    let (dst, targets) =
        resolve_destination(sys, &names, dst).map_err(|e| Box::new(opened.refuse(jverb, e)))?;
    // Each group's slice of the targets, by its offset among all names.
    let mut offsets = Vec::with_capacity(groups.len());
    let mut at = 0;
    for g in groups {
        offsets.push(at);
        at += g.names.len();
    }
    let slices: Vec<&[OsString]> = opened
        .sources
        .iter()
        .map(|s| &targets[offsets[s.group]..offsets[s.group] + s.names.len()])
        .collect();
    let mut rep = Reporter::new(ui);
    let plans = o.scan(verb, &opened.sources, &dst, &slices, &mut rep);
    let plans = match plans {
        Ok(p) => p,
        Err(Refusal::Cancelled) => {
            let mut r = Report::new(jverb);
            opened.report_failed(&mut r);
            r.cancelled = true;
            return Err(Box::new(r));
        }
        Err(e) => return Err(Box::new(opened.refuse(jverb, e))),
    };
    let mut t = Transfer::new(sys, rep, Report::new(jverb));
    opened.report_failed(&mut t.report);
    t.set_sum(plans.iter().map(|p| p.totals).sum());
    if o.is_local() {
        t.links = Links::from_plans(&plans);
    }
    let parts = plans
        .into_iter()
        .zip(opened.sources)
        .zip(slices)
        .map(|((plan, s), targets)| Part {
            src: s.dir,
            targets: targets.to_vec(),
            plan,
        })
        .collect();
    Ok((t, dst, parts))
}

/// F5: plan, then copy each selected entry.
pub fn copy_job(
    sys: &Sys,
    ui: &mut dyn super::question::Interaction,
    src_dir: &Path,
    names: &[OsString],
    dst: &Path,
) -> Report {
    copy_groups(sys, ui, &[Group::new(src_dir, names.to_vec())], dst)
}

/// F5 over groups (P2 2.2): plan every group, then copy each selected entry with one
/// [`Transfer`]. The groups are local (P3 2.2).
pub fn copy_groups(
    sys: &Sys,
    ui: &mut dyn super::question::Interaction,
    groups: &[Group],
    dst: &Path,
) -> Report {
    copy_from(sys, ui, &LocalOrigin::new(sys), groups, dst)
}

/// F5 from any origin (P3 2.3): the origin opens and plans the groups; a non-local origin's
/// declared total is checked against the destination's free space (A-4); then each selected
/// entry is copied with one [`Transfer`] into the local destination, in tree order or in one
/// pass (P3 3.5). Extraction and download are this copy with a non-local origin: they inherit
/// I-2, I-3 and I-5 on the write side, and the questions, progress and cancel.
pub fn copy_from<O: Origin>(
    sys: &Sys,
    ui: &mut dyn super::question::Interaction,
    o: &O,
    groups: &[Group],
    dst: &Path,
) -> Report {
    let (mut t, dst, parts) = match prepare(o, sys, ui, Verb::Copy, groups, dst) {
        Ok(x) => x,
        Err(r) => return *r,
    };
    if !o.is_local() && !room(&mut t, &dst, &parts) {
        return t.report;
    }
    t.protect = o.protected();
    if !o.is_local() {
        for p in &parts {
            for n in &p.plan.roots {
                collect_link_targets(o, n, &mut t.link_targets);
            }
        }
    }
    match o.order() {
        Order::Tree => tree_walk(&mut t, o, &dst, parts),
        Order::Stream => one_pass(&mut t, o, &dst, &parts),
    }
    t.link_note();
    t.report
}

/// Copies each selected entry in tree order.
fn tree_walk<O: Origin>(t: &mut Transfer<O::Dir>, o: &O, dst: &Dir, parts: Vec<Part<O::Dir>>) {
    for Part { src, plan, targets } in parts {
        for (node, target) in plan.roots.iter().zip(targets) {
            if t.entry(o, &src, node, dst, target) == Flow::Stop {
                return;
            }
        }
    }
}

/// The members hard-link members of a planned subtree link to (A-3).
fn collect_link_targets<O: Origin>(o: &O, node: &Node, out: &mut HashSet<u64>) {
    if let Some(k) = o.link_target(node) {
        out.insert(k);
    }
    for c in &node.children {
        collect_link_targets(o, c, out);
    }
}

/// A-4's second check (P3 3.5): the declared total of a non-local origin against the
/// destination's free space (`statvfs`), asked about once, before any write. `false`: the
/// user cancelled, and the report says so. Free space that cannot be read asks nothing.
fn room<D: OriginDir>(t: &mut Transfer<D>, dst: &Dir, parts: &[Part<D>]) -> bool {
    let need: u64 = parts.iter().map(|p| p.plan.totals.bytes).sum();
    let Ok((free, _)) = t.sys.free_space(dst.fd()) else {
        return true;
    };
    if need <= free {
        return true;
    }
    let q = Question::FreeSpace {
        path: dst.path.clone(),
        need,
        free,
    };
    match t.rep.ask(q) {
        Answer::Continue => true,
        _ => {
            t.report.cancelled = true;
            false
        }
    }
}

// ---- one pass (P3 3.5) ---------------------------------------------------------------------

/// At most this many destination directory fds stay open in a one-pass copy (P3 2.6).
pub const PASS_DIRS: usize = 64;

/// A directory's place in [`Pass::dirs`]; 0 is the destination root.
type DirIx = usize;

/// A directory the one-pass walk created or merged into, reached again from the destination
/// root through the component walk.
struct PassDir<'p> {
    parent: DirIx,
    /// Its name in the parent, after a Rename answer.
    name: OsString,
    /// `(st_dev, st_ino)` when the walk created or merged into it.
    id: (u64, u64),
    /// Its plan node and whether this job created it; `None` for the root.
    node: Option<(&'p Node, bool)>,
    spath: PathBuf,
}

/// A member the stream brings, or a hard link that follows the stream.
struct PassMember<'p, D> {
    src: D,
    node: &'p Node,
    dir: DirIx,
    target: OsString,
}

/// What the directory phase of a one-pass copy left for the stream.
struct Pass<'p, D> {
    dirs: Vec<PassDir<'p>>,
    members: HashMap<u64, PassMember<'p, D>>,
    /// The members' keys in tree order, for those the stream never reaches.
    order: Vec<u64>,
    links: Vec<PassMember<'p, D>>,
}

/// The destination directories of a one-pass copy: at most [`PASS_DIRS`] fds open, least
/// recently used out. A directory not open is reached from its parent with `O_DIRECTORY |
/// O_NOFOLLOW` and checked against the identity it had when the walk created it: a directory
/// replaced meanwhile, by a symlink or anything else, fails with "type changed" (A-2).
#[derive(Default)]
struct DirLru {
    open: Vec<(DirIx, Dir, u64)>,
    tick: u64,
}

impl DirLru {
    fn get(
        &mut self,
        sys: &Sys,
        dirs: &[PassDir],
        root: &Dir,
        ix: DirIx,
    ) -> Result<Dir, EntryError> {
        if ix == 0 {
            return Ok(root.clone());
        }
        self.tick += 1;
        if let Some(e) = self.open.iter_mut().find(|e| e.0 == ix) {
            e.2 = self.tick;
            return Ok(e.1.clone());
        }
        let d = &dirs[ix];
        let parent = self.get(sys, dirs, root, d.parent)?;
        let (fd, meta) = super::walk::open_dir_nofollow(sys, "pass.walk", parent.fd(), &d.name)?;
        if meta.id.inode() != d.id {
            return Err(EntryError::TypeChanged);
        }
        let dir = Dir {
            fd: Arc::new(fd),
            meta,
            path: parent.path.join(&d.name),
        };
        if self.open.len() >= PASS_DIRS
            && let Some(i) = (0..self.open.len()).min_by_key(|&i| self.open[i].2)
        {
            self.open.swap_remove(i);
        }
        self.tick += 1;
        self.open.push((ix, dir.clone(), self.tick));
        Ok(dir)
    }
}

impl<D: OriginDir> Transfer<'_, '_, D> {
    /// The directory phase of a one-pass copy (P3 3.5): creates the planned directories in
    /// tree order (M1 4.7: `mkdirat` with `0700`) with the M1 questions, makes symlinks from
    /// the origin at once, and records the members the stream will bring and the hard links
    /// that follow it.
    #[allow(clippy::too_many_arguments)]
    fn skeleton<'p, O: Origin<Dir = D>>(
        &mut self,
        o: &O,
        src: &D,
        node: &'p Node,
        dst: &Dir,
        at: DirIx,
        target: OsString,
        pass: &mut Pass<'p, D>,
    ) -> Flow {
        if self.stopped || self.cancelled() {
            return self.stop();
        }
        let spath = src.path().join(&node.name);
        if let Some(note) = &node.note {
            self.noted(node, spath, note);
            return Flow::Continue;
        }
        self.current = spath.clone();
        match node.meta.kind {
            Kind::Dir => {
                let (sdir, ddir, created) = match self.open_dirs(o, src, node, dst, target) {
                    Ok(x) => x,
                    Err(flow) => return flow,
                };
                let name = ddir.path.file_name().map(OsStr::to_owned);
                pass.dirs.push(PassDir {
                    parent: at,
                    name: name.unwrap_or_default(),
                    id: ddir.meta.id.inode(),
                    node: Some((node, created)),
                    spath,
                });
                let me = pass.dirs.len() - 1;
                for child in &node.children {
                    let name = child.name.clone();
                    if self.skeleton(o, &sdir, child, &ddir, me, name, pass) == Flow::Stop {
                        return Flow::Stop;
                    }
                }
                Flow::Continue
            }
            Kind::File => {
                let m = PassMember {
                    src: src.clone(),
                    node,
                    dir: at,
                    target,
                };
                if o.link_target(node).is_some() {
                    pass.links.push(m);
                } else {
                    pass.order.push(key(node));
                    pass.members.insert(key(node), m);
                }
                Flow::Continue
            }
            Kind::Symlink => self.file(o, src, node, dst, target),
            _ => {
                self.skip(node, spath, "special file");
                Flow::Continue
            }
        }
    }
}

/// A copy in one pass in stream order (P3 3.5): the directories first, in tree order; then
/// the stream once, each wanted member committed into its directory as the stream reaches
/// it, and nothing read after the last one; then the hard links, from the destination
/// inodes the job extracted (A-3); then the directory modes and times in post-order
/// (M1 4.7). Members the stream never reached fail with the reason the stream ended.
fn one_pass<O: Origin>(t: &mut Transfer<O::Dir>, o: &O, dst: &Dir, parts: &[Part<O::Dir>]) {
    let mut pass = Pass {
        dirs: vec![PassDir {
            parent: 0,
            name: OsString::new(),
            id: dst.meta.id.inode(),
            node: None,
            spath: dst.path.clone(),
        }],
        members: HashMap::new(),
        order: Vec::new(),
        links: Vec::new(),
    };
    'plan: for part in parts {
        for (node, target) in part.plan.roots.iter().zip(&part.targets) {
            let target = target.clone();
            if t.skeleton(o, &part.src, node, dst, 0, target, &mut pass) == Flow::Stop {
                break 'plan;
            }
        }
    }
    if t.stopped() {
        return;
    }
    let sys = t.sys;
    let mut lru = DirLru::default();
    if !pass.members.is_empty() {
        let wanted: HashSet<u64> = pass.members.keys().copied().collect();
        let cancel = sys.cancel_flag().clone();
        let (members, dirs) = (&mut pass.members, &pass.dirs);
        let ended = o.pass(&wanted, &cancel, &mut |k, bytes| {
            let Some(m) = members.remove(&k) else {
                return Flow::Continue;
            };
            let spath = m.src.path().join(&m.node.name);
            t.set_current(spath.clone());
            let ddir = match lru.get(sys, dirs, dst, m.dir) {
                Ok(d) => d,
                Err(e) => {
                    t.fail(m.node, spath, e.to_string());
                    return Flow::Continue;
                }
            };
            match bytes {
                Err(why) => {
                    t.fail(m.node, spath, why);
                    Flow::Continue
                }
                Ok(reader) => {
                    let lent = Lent {
                        o,
                        reader: RefCell::new(reader),
                        read: Cell::new(0),
                    };
                    t.file(&lent, &m.src, m.node, &ddir, m.target)
                }
            }
        });
        if t.stopped() || t.cancelled() {
            t.stop();
            return;
        }
        // The stream ended or broke before these members (I-7).
        let why = ended
            .err()
            .unwrap_or_else(|| "not found in the archive".into());
        for k in &pass.order {
            if let Some(m) = pass.members.remove(k) {
                let spath = m.src.path().join(&m.node.name);
                t.fail(m.node, spath, why.clone());
            }
        }
    }
    for m in std::mem::take(&mut pass.links) {
        let spath = m.src.path().join(&m.node.name);
        t.set_current(spath.clone());
        let ddir = match lru.get(sys, &pass.dirs, dst, m.dir) {
            Ok(d) => d,
            Err(e) => {
                t.fail(m.node, spath, e.to_string());
                continue;
            }
        };
        if t.file(o, &m.src, m.node, &ddir, m.target) == Flow::Stop {
            return;
        }
    }
    // Directory modes and times in post-order: a child comes after its parent in `dirs`.
    for ix in (1..pass.dirs.len()).rev() {
        let d = &pass.dirs[ix];
        let Some((node, created)) = d.node else {
            continue;
        };
        if created {
            let applied = lru
                .get(sys, &pass.dirs, dst, ix)
                .map_err(|e| e.to_string())
                .and_then(|dir| {
                    t.dir_metadata(&dir, &node.meta).map_err(|e| {
                        format!(
                            "set directory metadata: {}",
                            crate::fsops::walk::errno_text(e)
                        )
                    })
                });
            if let Err(why) = applied {
                t.report.fail(d.spath.clone(), why);
                continue;
            }
        }
        t.done(node);
    }
}

/// A one-pass origin with the bytes of the member the stream is at (P3 3.5): it lends them
/// to the engine's attempts at that member and delegates everything else. An attempt after
/// one that read bytes fails with [`READ_ONCE`], because a stream cannot be read again
/// (P3 2.3); an attempt after one that failed before reading (the temporary file could not
/// be created) gets them.
struct Lent<'o, 'r, O> {
    o: &'o O,
    reader: RefCell<&'r mut dyn Read>,
    read: Cell<u64>,
}

/// Counts what a lent reader gave.
struct Counted<'c> {
    inner: &'c mut dyn Read,
    read: &'c Cell<u64>,
}

impl Read for Counted<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read.set(self.read.get() + n as u64);
        Ok(n)
    }
}

impl<O: Origin> Origin for Lent<'_, '_, O> {
    type Dir = O::Dir;

    fn open_groups(
        &self,
        verb: JobVerb,
        groups: &[Group],
    ) -> Result<super::group::Opened<O::Dir>, Box<Report>> {
        self.o.open_groups(verb, groups)
    }

    fn scan(
        &self,
        verb: Verb,
        sources: &[super::group::OpenGroup<O::Dir>],
        dst: &Dir,
        targets: &[&[OsString]],
        rep: &mut Reporter,
    ) -> Result<Vec<Plan>, Refusal> {
        self.o.scan(verb, sources, dst, targets, rep)
    }

    fn order(&self) -> Order {
        self.o.order()
    }

    fn lend<T>(
        &self,
        _dir: &O::Dir,
        node: &Node,
        _cancel: &AtomicBool,
        read: &mut dyn FnMut(&mut dyn Read, u64) -> T,
    ) -> Option<Result<T, String>> {
        if self.read.get() > 0 {
            return Some(Err(READ_ONCE.into()));
        }
        let mut r = self.reader.borrow_mut();
        let mut counted = Counted {
            inner: &mut **r,
            read: &self.read,
        };
        Some(Ok(read(&mut counted, node.meta.size)))
    }

    fn link_target(&self, node: &Node) -> Option<u64> {
        self.o.link_target(node)
    }

    fn protected(&self) -> Option<(u64, u64)> {
        self.o.protected()
    }

    fn open_dir(&self, dir: &O::Dir, node: &Node) -> Result<O::Dir, EntryError> {
        self.o.open_dir(dir, node)
    }

    fn open(
        &self,
        dir: &O::Dir,
        node: &Node,
        cancel: &AtomicBool,
    ) -> Result<OriginFile, EntryError> {
        self.o.open(dir, node, cancel)
    }

    fn read_link(&self, dir: &O::Dir, node: &Node) -> Result<(OsString, Meta), EntryError> {
        self.o.read_link(dir, node)
    }

    fn remove(&self, dir: &O::Dir, name: &OsStr, planned: &super::sys::Snapshot) -> Removed {
        self.o.remove(dir, name, planned)
    }

    fn remove_dir(&self, parent: &O::Dir, name: &OsStr, id: (u64, u64)) -> Removed {
        self.o.remove_dir(parent, name, id)
    }

    fn mtime_resolution(&self, dir: &O::Dir) -> i128 {
        self.o.mtime_resolution(dir)
    }

    fn is_local(&self) -> bool {
        self.o.is_local()
    }

    fn local(dir: &O::Dir) -> Option<&Dir> {
        O::local(dir)
    }
}
