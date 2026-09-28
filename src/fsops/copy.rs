#![forbid(unsafe_code)]
//! Copy (F5, design 4.7) and the transfer engine that cross-filesystem move builds on.
//!
//! Per regular file: open the source safely (4.3) and record `S0`; write a temporary file
//! `.<name>.mc-partial-<random>` in the destination directory; copy with
//! `copy_file_range`; apply mode and times; commit with `RENAME_NOREPLACE` (or `linkat`, or
//! the direct-write mode on a filesystem that supports neither). The destination never
//! shows a partial file (I-2) and is never replaced without an answer (I-3).

use super::group::{Group, Source};
use super::job::{JobVerb, Report};
use super::plan::{Node, Note, Plan, Refusal, Scan, Totals, Verb, scan_all};
use super::question::{
    Answer, Conflict, Phase, Progress, Question, Reporter, Side, conflict, is_conflict_errno,
};
use super::sys::{Kind, Meta, Snapshot, Sys, Ts, magic, random_u64};
use super::walk::{EntryError, open_child_dir, open_for_read};
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use rustix::io::Errno;
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

/// The mtime resolution of a filesystem in nanoseconds, from `f_type` (design 4.5).
fn mtime_resolution(f_type: i64) -> i128 {
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

pub struct Transfer<'a, 'u> {
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
    pub(crate) batch: super::mv::Batch,
    /// Every top-level entry counts once, whatever its kind (trash).
    pub(crate) flat: bool,
    /// `(source st_dev, destination st_dev)` pairs where `copy_file_range` is known not to
    /// work: later files go straight to read/write.
    no_kernel_copy: HashSet<(u64, u64)>,
    /// Temporary names: a random base per job plus a counter (no syscall per file).
    tmp_base: u64,
    tmp_seq: u64,
}

impl<'a, 'u> Transfer<'a, 'u> {
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

    /// "File exists" (design 4.5), with the standing answers applied first.
    pub(crate) fn decide_exists(
        &mut self,
        src: &Meta,
        src_dir: &Dir,
        dst: &Meta,
        dst_dir: &Dir,
        dpath: &Path,
    ) -> Decision {
        let sys = self.sys;
        let older = || {
            let ft = |d: &Dir| sys.fstatfs(d.fd()).map(|s| s.f_type).unwrap_or(0);
            let res = mtime_resolution(ft(src_dir)).max(mtime_resolution(ft(dst_dir)));
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
    /// `Merge` only for directory over directory.
    pub(crate) fn resolve_conflict(
        &mut self,
        src_meta: &Meta,
        src_dir: &Dir,
        dst_meta: &Meta,
        dst_dir: &Dir,
        dpath: &Path,
    ) -> Decision {
        match conflict(src_meta.kind, dst_meta.kind) {
            Conflict::FileExists => self.decide_exists(src_meta, src_dir, dst_meta, dst_dir, dpath),
            Conflict::DirExists => self.decide_dir_exists(src_meta, dst_meta, dpath),
            Conflict::TypeMismatch => self.decide_mismatch(src_meta, dst_meta, dpath),
        }
    }

    // ---- entries ------------------------------------------------------------------------

    /// Copies one planned entry of `src` to `dst/target`.
    pub fn entry(&mut self, src: &Dir, node: &Node, dst: &Dir, target: OsString) -> Flow {
        if self.stopped || self.cancelled() {
            return self.stop();
        }
        let spath = src.path.join(&node.name);
        if let Some(note) = &node.note {
            match note {
                Note::Failed(e) => self.fail(node, spath, e.to_string()),
                n => self.skip(node, spath, n.reason()),
            }
            return Flow::Continue;
        }
        self.current = spath.clone();
        match node.meta.kind {
            Kind::Dir => self.dir(src, node, dst, target),
            Kind::File | Kind::Symlink => self.file(src, node, dst, target),
            _ => {
                self.skip(node, spath, "special file");
                Flow::Continue
            }
        }
    }

    /// A regular file or a symlink.
    pub(crate) fn file(&mut self, src: &Dir, node: &Node, dst: &Dir, mut target: OsString) -> Flow {
        let spath = src.path.join(&node.name);
        let mut overwrite = false;
        let mut check = node.meta.kind != Kind::File || node.meta.size >= PRECHECK_BYTES;
        loop {
            if self.cancelled() {
                return self.stop();
            }
            let dpath = dst.path.join(&target);
            if !overwrite && check {
                match self.sys.stat_at("copy.dststat", dst.fd(), &target) {
                    Ok(dm) => {
                        if dm.id.inode() == node.meta.id.inode() {
                            self.skip(node, spath, "source and destination are the same file");
                            return Flow::Continue;
                        }
                        match self.resolve_conflict(&node.meta, src, &dm, dst, &dpath) {
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
            match self.transfer(src, node, dst, &target, overwrite) {
                Ok(snap) if self.moving => {
                    // Committed: the source goes with the next flush (design 4.8 step 4).
                    self.queue(src, node, snap, dst);
                    self.entry_processed(node);
                    self.tick();
                    return self.flush_if_full();
                }
                Ok(_) => {
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
    /// commit it.
    fn transfer(
        &mut self,
        src: &Dir,
        node: &Node,
        dst: &Dir,
        target: &OsStr,
        overwrite: bool,
    ) -> Result<Snapshot, Fail> {
        let direct = !overwrite && self.direct.contains(&dst.meta.id.domain());
        if node.meta.kind == Kind::Symlink {
            return self.transfer_symlink(src, node, dst, target, overwrite, direct);
        }
        let sys = self.sys;
        let (fin, m0) = open_for_read(sys, src.fd(), &node.name, Some(node.meta.id.inode()))?;
        if direct {
            // Direct-write mode (I-2 exception): the O_EXCL create takes the place of the
            // commit; the guard removes the name on failure or cancel.
            let fout = match sys.create_excl("commit.direct", dst.fd(), target, 0o600) {
                Ok(f) => f,
                Err(e) if is_conflict_errno(e) => return Err(Fail::Exists),
                Err(e) => return Err(Fail::Os("create", e)),
            };
            let mut guard = Unlink::new(sys, dst.fd(), target.to_owned());
            self.data(
                fin.as_fd(),
                fout.as_fd(),
                m0.size,
                (m0.id.dev, dst.meta.id.dev),
            )?;
            self.metadata(fout.as_fd(), &m0, dst)?;
            // In direct-write mode the check runs after the last byte; a failed check
            // unlinks the destination name through the guard.
            self.change_check(fin.as_fd(), &m0)?;
            guard.disarm();
            return Ok(m0.snapshot());
        }
        let (fout, tmp) = self.create_partial(dst, target)?;
        let mut guard = Unlink::new(sys, dst.fd(), tmp.clone());
        self.data(
            fin.as_fd(),
            fout.as_fd(),
            m0.size,
            (m0.id.dev, dst.meta.id.dev),
        )?;
        self.metadata(fout.as_fd(), &m0, dst)?;
        drop(fout);
        self.change_check(fin.as_fd(), &m0)?;
        self.commit(dst, &tmp, target, overwrite, &mut guard)?;
        Ok(m0.snapshot())
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

    fn transfer_symlink(
        &mut self,
        src: &Dir,
        node: &Node,
        dst: &Dir,
        target: &OsStr,
        overwrite: bool,
        direct: bool,
    ) -> Result<Snapshot, Fail> {
        let sys = self.sys;
        let now = sys
            .stat_at("copy.lstat", src.fd(), &node.name)
            .map_err(|e| EntryError::os("stat", e))?;
        if now.kind != Kind::Symlink || now.id.inode() != node.meta.id.inode() {
            return Err(EntryError::TypeChanged.into());
        }
        let link = match sys.readlink("copy.readlink", src.fd(), &node.name) {
            Ok(l) => l,
            Err(Errno::INVAL) => return Err(EntryError::TypeChanged.into()),
            Err(e) => return Err(EntryError::os("read link", e).into()),
        };
        if self.moving {
            let again = sys
                .stat_at("move.check", src.fd(), &node.name)
                .map_err(|e| Fail::Os("stat source", e))?;
            if again.snapshot() != now.snapshot() {
                return Err(Fail::SourceChanged);
            }
        }
        if direct {
            return match sys.symlink("commit.direct", &link, dst.fd(), target) {
                Ok(()) => Ok(now.snapshot()),
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
        Ok(now.snapshot())
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

    /// Copies the data (design 4.7 step 3).
    pub(crate) fn data(
        &mut self,
        from: BorrowedFd,
        to: BorrowedFd,
        size: u64,
        devs: (u64, u64),
    ) -> Result<(), Fail> {
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
                        let (a, b) = (self.sys.stat_fd(from), self.sys.stat_fd(to));
                        if let (Ok(a), Ok(b)) = (a, b)
                            && a.id.inode() == b.id.inode()
                        {
                            // Never read and write one inode (I-4).
                            return Err(Fail::Entry(EntryError::Os {
                                op: "copy",
                                errno: Errno::INVAL,
                            }));
                        }
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

    /// A directory: create (or merge into) the destination, recurse, then apply the
    /// source's mode and times in post-order to a directory this job created.
    pub(crate) fn dir(&mut self, src: &Dir, node: &Node, dst: &Dir, mut target: OsString) -> Flow {
        let sys = self.sys;
        let spath = src.path.join(&node.name);
        let (sfd, smeta) = loop {
            match open_child_dir(sys, "walk.openat", src.fd(), &node.name, &node.meta.id) {
                Ok(x) => break x,
                Err(EntryError::Os { op, errno }) => match self.decide_error(&spath, op, errno) {
                    Some(true) => continue,
                    Some(false) => {
                        self.fail(node, spath, EntryError::Os { op, errno }.to_string());
                        return Flow::Continue;
                    }
                    None => return self.stop(),
                },
                Err(e) => {
                    self.fail(node, spath, e.to_string());
                    return Flow::Continue;
                }
            }
        };
        let sdir = Dir {
            fd: Arc::new(sfd),
            meta: smeta,
            path: spath.clone(),
        };
        let created = loop {
            if self.cancelled() {
                return self.stop();
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
                            return Flow::Continue;
                        }
                    };
                    match self.resolve_conflict(&node.meta, src, &dm, dst, &dpath) {
                        Decision::Merge => break false,
                        Decision::Rename(n) => target = n,
                        Decision::Skip(why) => {
                            self.skip(node, spath, why);
                            return Flow::Continue;
                        }
                        Decision::Cancel => return self.stop(),
                        Decision::Overwrite => unreachable!("overwrite offered for a directory"),
                    }
                }
                Err(e) => match self.decide_error(&dpath, "make directory", e) {
                    Some(true) => {}
                    Some(false) => {
                        self.fail(node, spath, EntryError::os("make directory", e).to_string());
                        return Flow::Continue;
                    }
                    None => return self.stop(),
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
                return Flow::Continue;
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
                return Flow::Continue;
            }
        };
        let ddir = Dir {
            fd: Arc::new(dfd),
            meta: dmeta,
            path: dst.path.join(&target),
        };
        for child in &node.children {
            if self.entry(&sdir, child, &ddir, child.name.clone()) == Flow::Stop {
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
                return self.flush();
            }
            return self.finish_source_dir(src, node, &spath);
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
pub(crate) struct Part {
    pub src: Dir,
    pub plan: Plan,
    pub targets: Vec<OsString>,
}

/// The shared start of copy and move over groups (P2 2.2): open the groups, resolve the
/// destination once with the M1 rules (an existing directory, or a new path for exactly one
/// source name in total), then plan every group with the checks over their union. One
/// [`Transfer`] serves all groups, so standing answers carry across them. `Err` is the
/// final report (refused, or cancelled during the scan).
pub(crate) fn prepare<'a, 'u>(
    sys: &'a Sys,
    ui: &'u mut dyn super::question::Interaction,
    verb: Verb,
    groups: &[Group],
    dst: &Path,
) -> Result<(Transfer<'a, 'u>, Dir, Vec<Part>), Box<Report>> {
    let jverb = match verb {
        Verb::Move => JobVerb::Move,
        _ => JobVerb::Copy,
    };
    let opened = super::group::open(sys, jverb, groups)?;
    let names: Vec<OsString> = groups.iter().flat_map(|g| g.names.clone()).collect();
    let (dst, targets) =
        resolve_destination(sys, &names, dst).map_err(|e| Box::new(Report::refused(jverb, e)))?;
    // Each group's slice of the targets, by its offset among all names.
    let mut offsets = Vec::with_capacity(groups.len());
    let mut at = 0;
    for g in groups {
        offsets.push(at);
        at += g.names.len();
    }
    let slice = |s: &Source| &targets[offsets[s.group]..offsets[s.group] + s.names.len()];
    let mut rep = Reporter::new(ui);
    let plans = {
        let scans: Vec<Scan> = opened
            .sources
            .iter()
            .map(|s| Scan {
                sys,
                verb,
                src: s.dir.fd(),
                src_path: &s.dir.path,
                names: &s.names,
                dst: Some((dst.fd(), slice(s))),
            })
            .collect();
        scan_all(&scans, &mut rep)
    };
    let plans = match plans {
        Ok(p) => p,
        Err(Refusal::Cancelled) => {
            let mut r = Report::new(jverb);
            opened.report_failed(&mut r);
            r.cancelled = true;
            return Err(Box::new(r));
        }
        Err(e) => return Err(Box::new(Report::refused(jverb, e))),
    };
    let mut t = Transfer::new(sys, rep, Report::new(jverb));
    opened.report_failed(&mut t.report);
    t.set_sum(plans.iter().map(|p| p.totals).sum());
    let parts = plans
        .into_iter()
        .zip(&opened.sources)
        .map(|(plan, s)| Part {
            src: s.dir.clone(),
            targets: slice(s).to_vec(),
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
/// [`Transfer`].
pub fn copy_groups(
    sys: &Sys,
    ui: &mut dyn super::question::Interaction,
    groups: &[Group],
    dst: &Path,
) -> Report {
    let (mut t, dst, parts) = match prepare(sys, ui, Verb::Copy, groups, dst) {
        Ok(x) => x,
        Err(r) => return *r,
    };
    'job: for Part { src, plan, targets } in parts {
        for (node, target) in plan.roots.iter().zip(targets) {
            if t.entry(&src, node, &dst, target) == Flow::Stop {
                break 'job;
            }
        }
    }
    t.report
}
