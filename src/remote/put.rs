#![forbid(unsafe_code)]
//! Uploads (P3 5.6): F5 and F6 from a local directory to a server, and the F4 write-back.
//! This is phase 3's only new destination engine (P3 2.3). Its source side is the local
//! engine's: the groups open as M1 and P2 open them, the scan is M1's, and each regular file
//! is opened through the `O_PATH` sequence of M1 4.3, so I-5 holds on the local side.
//! Questions, standing answers, progress, cancel and the report are M1's ([`Transfer`]).
//!
//! **Files (R-1).** A file is written to `.<name>.mc-partial-<random>` in the destination
//! directory, opened `CREAT` + `EXCL` + `WRITE` with mode `0600`, with a pipelined window of
//! `WRITE`s; its mode (setuid and setgid cleared) and times (whole seconds) are set with
//! `FSETSTAT`; a move syncs it with `fsync@openssh.com` when the server has it; then it is
//! closed. It commits with `hardlink@openssh.com(temporary, final)`, which fails when the
//! final name exists, and the temporary name is removed. `SSH_FXP_RENAME` never commits an
//! upload: protocol version 3 leaves open whether it replaces. A commit that fails raises
//! "file exists" when an `LSTAT` finds the final name (version 3 reports "exists" and other
//! failures alike as `SSH_FX_FAILURE`); as for a local source in M1 4.7, the temporary file
//! is removed first, and after the answer the file is uploaded again.
//!
//! **Round trips.** A file takes three: the `OPEN`; one batch of the `WRITE`s, the
//! `FSETSTAT`, the `fsync` and the `CLOSE` ([`Writes`](super::session::Writes)); one batch
//! of the hard link and the `REMOVE` of the temporary name. The server executes a session's
//! requests in the order sent (the session's "Batches"), so no byte lands after the times
//! are set, the `CLOSE` closes the complete file, and the `REMOVE` takes the temporary name
//! after the link, whether the link succeeded or failed. Every reply is checked; a failed
//! `CLOSE` is seen before the commit goes out. Direct-write mode takes two (the `OPEN` of
//! the final name, then the batch), an overwrite three (the batch is followed by
//! `posix-rename@openssh.com` alone, which consumes the temporary name).
//!
//! **Direct-write mode.** A server without hard links (the extension missing, or refused
//! as unsupported or not permitted, which is how `link(2)` fails on a filesystem without
//! hard links) gets M1's direct-write mode: the final name is created `CREAT` + `EXCL` and
//! written directly. It is visible while it is written, is removed on failure or cancel,
//! and counts as committed only after its last byte and its metadata are written (the M1
//! I-2 exception; the report says so).
//!
//! **Overwrite (R-2).** Only after the "file exists" answer, and only through
//! `posix-rename@openssh.com` of a complete temporary file over the final name: an atomic
//! `rename(2)`. A server without it refuses the overwrite with [`NO_ATOMIC_REPLACE`].
//!
//! **Symlinks** are created as symlinks under the final name (`SYMLINK`, with OpenSSH's
//! argument order on the wire), because a new name cannot show partial content; a failure
//! `LSTAT`s the name to raise "file exists". **Directories** are made with `MKDIR` and mode
//! `0700`; the merge questions are M1's; their final mode and times follow in post-order.
//! Special files are skipped.
//!
//! **Cancel and errors.** The `WRITE` window stops at a cancel and its replies are drained;
//! a cancel before the file's `CLOSE` went out stops the sending there. Every request that
//! changes the server waits for its reply even after a cancel ([`Session::call_firm`], a
//! batch's replies), so no name this job made is forgotten: a cancel seen while the batch's
//! replies arrive discards the file once they are in. On any failure or cancel the
//! temporary name (or, in direct-write mode, the final name) is removed, together with the
//! `CLOSE` of a handle still open.
//!
//! **A lost session** names the path that may hold partial data in the report, and every
//! later entry fails with "connection lost" (I-7). At the `OPEN`: the name it may have
//! created. In the batch of the data: the temporary name (hard-link mode; the final name is
//! untouched), or the final name in direct-write mode, unless every `WRITE`, the `FSETSTAT`
//! and a move's `fsync` were acknowledged: then only the `CLOSE` is unconfirmed, and the
//! file counts as committed for a copy (not for a move, whose source stays). In the commit:
//! without the link's reply its outcome is unknown, so the entry fails with
//! [`LOST_AT_COMMIT`] (the final name holds nothing new or the complete file) and the
//! temporary name is named; with the link acknowledged the file is committed, and a
//! temporary name whose `REMOVE` was not acknowledged is named as not removed.
//!
//! **A move (F6, R-4)** is best-effort, and its confirm dialog says so before the job. The
//! local source of an upload is unlinked only after the upload was committed (and synced
//! with `fsync@openssh.com` when the server has it; otherwise the report says "not synced
//! on the server"), with the M1 4.8 check of the source against `S0`, in batches like M1's
//! group commit. A source directory is removed after its children, when it is empty.
//!
//! Every request runs on the job worker; every step that changes the server has a failpoint
//! (`put.open`, `put.write`, `put.setstat`, `put.fsync`, `put.close`, `put.link`,
//! `put.replace`, `put.remove`, `put.mkdir`, `put.symlink`, `put.lstat`) for the A-SF-9
//! sweep.

use super::proto::{self, Attrs, Packet, ext, open, status};
use super::provider::{RemoteProvider, join, location, meta_of};
use super::session::{Session, SftpError};
use super::step;
use super::tree::LOST;
use crate::fsops::copy::{Decision, Dir, Flow, Transfer, partial_name_with};
use crate::fsops::group::{Group, Opened};
use crate::fsops::job::{JobVerb, Report};
use crate::fsops::origin::{LocalOrigin, Origin, Removed};
use crate::fsops::plan::{Node, Refusal, Scan, Verb, scan_all};
use crate::fsops::question::{Answer, Conflict, Interaction, Question, Reporter, conflict};
use crate::fsops::sys::{Kind, Meta, Snapshot, Sys, Ts, random_u64};
use crate::fsops::walk::{EntryError, open_for_read};
use crate::provider::{Caps, Target, VPath, synthetic_id};
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use rustix::io::Errno;
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What an overwrite says on a server without `posix-rename@openssh.com` (R-2).
pub const NO_ATOMIC_REPLACE: &str = "the server cannot replace a file atomically";

/// The report note of a move to a server without `fsync@openssh.com` (R-4).
pub const NOT_SYNCED: &str = "not synced on the server: it has no fsync@openssh.com, so a crash there can lose uploads whose local sources are gone";

/// The report note of an upload in direct-write mode (R-1, the M1 I-2 exception).
pub const DIRECT_WRITE: &str = "the server cannot link files: they were written under their final names, visible while they were written";

/// What the report says about a path that may hold partial data after a lost session (R-1).
pub const MAY_BE_PARTIAL: &str = "may hold partial data: the connection was lost";

/// What the report says about a temporary name that could not be removed.
pub const NOT_REMOVED: &str = "temporary file not removed";

/// What an entry fails with when the session was lost during its commit (R-1): the
/// commit's outcome is unknown, but the final name holds either nothing new or the complete
/// file.
pub const LOST_AT_COMMIT: &str =
    "connection lost during the commit: the complete file may or may not be at its name";

/// Below this size, a file is written without checking its destination name first; a
/// conflict then shows at the commit, which never replaces (I-3). M1's rule.
const PRECHECK_BYTES: u64 = 1 << 20;

/// A committed upload is removed from its local source in batches of this many entries or
/// bytes, as M1 4.8 batches a move.
const BATCH_FILES: usize = crate::fsops::mv::BATCH_FILES;
const BATCH_BYTES: u64 = crate::fsops::mv::BATCH_BYTES;

const S_IFDIR: u32 = 0o040_000;

/// A directory on the server as the upload reached it.
#[derive(Clone, Debug)]
struct RDir {
    /// Its path on the server.
    path: Vec<u8>,
    /// `sftp://[user@]host[:port]/dir`, for questions and the report.
    shown: PathBuf,
}

impl RDir {
    fn path_of(&self, name: &OsStr) -> Vec<u8> {
        join(&self.path, name.as_bytes())
    }

    fn shown_of(&self, name: &OsStr) -> PathBuf {
        self.shown.join(name)
    }
}

/// Why one attempt at an entry did not complete.
#[derive(Debug)]
enum PutFail {
    /// Something holds the destination name now: re-check and ask.
    Exists,
    /// The server turned out to have no hard links: again in direct-write mode.
    Again,
    Cancelled,
    /// The local source is no longer what the plan saw.
    Entry(EntryError),
    /// A local OS error: the M1 error question.
    Local(&'static str, Errno),
    /// The server's answer to `op`, or the lost session.
    Server(&'static str, SftpError),
    /// The move's change check failed (M1 4.8 step 2): nothing was committed.
    SourceChanged,
    /// The entry fails with the text, without a question.
    Refused(String),
}

impl From<EntryError> for PutFail {
    fn from(e: EntryError) -> PutFail {
        match e {
            EntryError::Os { op, errno } => PutFail::Local(op, errno),
            e => PutFail::Entry(e),
        }
    }
}

/// What the error question decided.
enum Ask {
    Retry,
    /// The entry fails with the text.
    Fail(String),
    Stop,
}

/// A name this attempt made on the server, which a failure removes (R-1).
struct Made {
    path: Vec<u8>,
    shown: PathBuf,
}

/// How the batch of a file's data, metadata and `CLOSE` ended (P3 5.6).
enum Filled {
    /// Every reply arrived, and every one succeeded.
    Done,
    /// The session ended after every `WRITE`, the metadata and a move's `fsync` were
    /// acknowledged: the file is complete, only its `CLOSE` is unconfirmed.
    Complete,
    /// The attempt failed; `true` when its `CLOSE` went out, so the handle is gone.
    Failed(PutFail, bool),
}

/// A committed upload whose local source waits for the next flush (M1 4.8).
struct Pending {
    dir: Dir,
    name: OsString,
    snap: Snapshot,
    path: PathBuf,
}

/// The local side of a file's bytes: `read(2)` on the `O_PATH`-checked fd, remembering the
/// errno of a failed read for the error question.
struct Source<'a> {
    sys: &'a Sys,
    fd: BorrowedFd<'a>,
    errno: Option<Errno>,
}

impl Read for Source<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.sys.read("put.read", self.fd, buf) {
            Ok(n) => Ok(n),
            Err(e) => {
                self.errno = Some(e);
                Err(std::io::Error::from_raw_os_error(e.raw_os_error()))
            }
        }
    }
}

/// Where an upload goes.
enum To<'p> {
    /// What the user confirmed: an existing directory to copy into, or, for a single source
    /// name in total, a new path (M1).
    Resolve(&'p VPath),
    /// Exactly this path: the F4 write-back.
    Exactly(&'p VPath),
}

/// Whole seconds for the protocol's 32-bit times.
fn secs(t: Ts) -> u32 {
    t.sec.clamp(0, i64::from(u32::MAX)) as u32
}

/// The mode (setuid and setgid cleared, M1 4.7 step 4) and the times of a file.
fn file_attrs(m: &Meta) -> Attrs {
    Attrs {
        perms: Some(m.perm & 0o7777 & !0o6000),
        times: Some((secs(m.atime), secs(m.mtime))),
        ..Attrs::default()
    }
}

/// The mode (setuid and setgid cleared, the sticky bit kept) and the times of a
/// directory, applied in post-order (M1 4.7).
fn dir_attrs(m: &Meta) -> Attrs {
    Attrs {
        perms: Some(m.perm & 0o1777),
        times: Some((secs(m.atime), secs(m.mtime))),
        ..Attrs::default()
    }
}

/// The upload engine of one job.
struct Put<'a, 'u> {
    t: Transfer<'a, 'u>,
    s: &'a Session,
    remote: &'a RemoteProvider,
    caps: Caps,
    /// The job's cancel flag, which the reply slots re-check (P3 2.5).
    cancel: Arc<AtomicBool>,
    /// No hard links on this server: direct-write mode (R-1).
    direct: bool,
    /// A file was written in direct-write mode (the report note).
    wrote_direct: bool,
    /// The session ended: every later entry fails with "connection lost" (I-7).
    lost: bool,
    moving: bool,
    batch: Vec<Pending>,
    batch_bytes: u64,
    /// A move committed something (the fsync note).
    committed: bool,
    /// "Skip all of this error" answers to the server's error question.
    skip_server: HashSet<String>,
    tmp_base: u64,
    tmp_seq: u64,
}

impl<'a, 'u> Put<'a, 'u> {
    fn firm(&self) -> super::session::Firm<'_> {
        self.s.firm(&self.cancel)
    }

    fn note(&mut self, shown: &Path, what: impl std::fmt::Display) {
        self.t
            .report
            .notes
            .push(format!("{}: {what}", shown.display()));
    }

    fn next_partial(&mut self, target: &OsStr) -> OsString {
        self.tmp_seq += 1;
        partial_name_with(target, self.tmp_base.wrapping_add(self.tmp_seq))
    }

    /// `LSTAT` of a server path: `None` when nothing is there.
    fn lstat(&mut self, path: &[u8]) -> Result<Option<Attrs>, SftpError> {
        step(self.t.sys, "put.lstat")?;
        match self.s.lstat(path, &self.cancel) {
            Ok(a) => Ok(Some(a)),
            Err(SftpError::Status {
                code: status::NO_SUCH_FILE,
                ..
            }) => Ok(None),
            Err(e) => {
                if e == SftpError::Lost {
                    self.lost = true;
                }
                Err(e)
            }
        }
    }

    /// The server's error question (P3 5.6: M1's questions), or the entry's end without
    /// one: a lost session fails it, a cancel stops the job.
    fn ask_server(&mut self, path: &Path, op: &'static str, e: &SftpError) -> Ask {
        match e {
            SftpError::Lost => {
                self.lost = true;
                Ask::Fail(LOST.into())
            }
            SftpError::Cancelled => Ask::Stop,
            SftpError::Local(m) => Ask::Fail(format!("{op}: {m}")),
            SftpError::Status { .. } => {
                let message = e.to_string();
                let why = format!("{op}: {message}");
                if self.skip_server.contains(&message) {
                    return Ask::Fail(why);
                }
                let q = Question::ServerError {
                    path: path.to_path_buf(),
                    op,
                    message: message.clone(),
                };
                match self.t.rep.ask(q) {
                    Answer::Retry => Ask::Retry,
                    Answer::SkipAllErrno => {
                        self.skip_server.insert(message);
                        Ask::Fail(why)
                    }
                    Answer::Cancel => Ask::Stop,
                    _ => Ask::Fail(why),
                }
            }
        }
    }

    /// The M1 conflict question for a name that exists on the server: a server's mtime has
    /// whole seconds (the P3 1.4 amendment of M1 4.5).
    fn resolve(
        &mut self,
        o: &LocalOrigin,
        src: &Dir,
        sm: &Meta,
        dm: &Meta,
        shown: &Path,
    ) -> Decision {
        let res = || o.mtime_resolution(src).max(1_000_000_000);
        match conflict(sm.kind, dm.kind) {
            Conflict::FileExists => self.t.decide_exists_res(sm, dm, &res, shown),
            Conflict::DirExists => self.t.decide_dir_exists(sm, dm, shown),
            Conflict::TypeMismatch => self.t.decide_mismatch(sm, dm, shown),
        }
    }

    // ---- entries ------------------------------------------------------------------------

    /// Uploads one planned entry of the local directory `src` to `dst/target`.
    fn entry(
        &mut self,
        o: &LocalOrigin,
        src: &Dir,
        node: &Node,
        dst: &RDir,
        target: OsString,
    ) -> Flow {
        if self.t.stopped() || self.t.cancelled() {
            return self.t.stop();
        }
        let spath = src.path.join(&node.name);
        if let Some(note) = &node.note {
            self.t.noted(node, spath, note);
            return Flow::Continue;
        }
        if self.lost {
            self.t.fail(node, spath, LOST);
            return Flow::Continue;
        }
        self.t.set_current(spath.clone());
        match node.meta.kind {
            Kind::Dir => self.dir(o, src, node, dst, target),
            Kind::File | Kind::Symlink => self.file(o, src, node, dst, target),
            _ => {
                self.t.skip(node, spath, "special file");
                Flow::Continue
            }
        }
    }

    /// A regular file or a symlink: the destination check, the attempt, the questions.
    fn file(
        &mut self,
        o: &LocalOrigin,
        src: &Dir,
        node: &Node,
        dst: &RDir,
        mut target: OsString,
    ) -> Flow {
        let spath = src.path.join(&node.name);
        let mut overwrite = false;
        let mut check = node.meta.kind != Kind::File
            || node.meta.size >= PRECHECK_BYTES
            || self.t.policy.overwrite.is_some();
        loop {
            if self.t.cancelled() {
                return self.t.stop();
            }
            let shown = dst.shown_of(&target);
            if !overwrite && check {
                match self.lstat(&dst.path_of(&target)) {
                    Ok(Some(a)) => {
                        let dm = meta_of(&a, synthetic_id(self.remote.id(), 0));
                        match self.resolve(o, src, &node.meta, &dm, &shown) {
                            Decision::Overwrite => overwrite = true,
                            Decision::Rename(n) => {
                                target = n;
                                continue;
                            }
                            Decision::Skip(why) => {
                                self.t.skip(node, spath, why);
                                return Flow::Continue;
                            }
                            Decision::Cancel => return self.t.stop(),
                            Decision::Merge => unreachable!("merge offered for a file"),
                        }
                    }
                    Ok(None) => {}
                    Err(e) => match self.ask_server(&shown, "stat destination", &e) {
                        Ask::Retry => continue,
                        Ask::Fail(why) => {
                            self.t.fail(node, spath, why);
                            return Flow::Continue;
                        }
                        Ask::Stop => return self.t.stop(),
                    },
                }
            }
            // R-2: the only replace on a server is the atomic one.
            if overwrite && !self.caps.posix_rename {
                self.t.fail(node, spath, NO_ATOMIC_REPLACE);
                return Flow::Continue;
            }
            let r = if node.meta.kind == Kind::Symlink {
                self.put_symlink(o, src, node, dst, &target, overwrite)
            } else {
                self.put_file(src, node, dst, &target, overwrite)
            };
            match r {
                Ok(m0) if self.moving => {
                    // Committed: the local source goes with the next flush (M1 4.8).
                    self.committed = true;
                    self.queue(src, node, &m0);
                    self.t.entry_processed(node);
                    self.t.tick();
                    return self.flush_if_full(o);
                }
                Ok(_) => {
                    self.t.done(node);
                    return Flow::Continue;
                }
                Err(PutFail::Exists) => {
                    overwrite = false;
                    check = true;
                }
                Err(PutFail::Again) => {}
                Err(PutFail::Cancelled) => return self.t.stop(),
                Err(PutFail::SourceChanged) => {
                    self.t
                        .fail(node, spath, "source changed during move; source kept");
                    return Flow::Continue;
                }
                Err(PutFail::Entry(e)) => {
                    self.t.fail(node, spath, e.to_string());
                    return Flow::Continue;
                }
                Err(PutFail::Refused(why)) => {
                    self.t.fail(node, spath, why);
                    return Flow::Continue;
                }
                Err(PutFail::Local(op, errno)) => match self.t.decide_error(&spath, op, errno) {
                    Some(true) => {}
                    Some(false) => {
                        self.t
                            .fail(node, spath, EntryError::os(op, errno).to_string());
                        return Flow::Continue;
                    }
                    None => return self.t.stop(),
                },
                Err(PutFail::Server(op, e)) => match self.ask_server(&shown, op, &e) {
                    Ask::Retry => {}
                    Ask::Fail(why) => {
                        self.t.fail(node, spath, why);
                        return Flow::Continue;
                    }
                    Ask::Stop => return self.t.stop(),
                },
            }
        }
    }

    /// One attempt at a regular file (R-1, R-2), in three round trips (P3 5.6): the
    /// temporary file (or, in direct-write mode, the final name) is opened; its data, mode
    /// and times, a move's `fsync@openssh.com` and its `CLOSE` go out as one batch, and
    /// every reply is checked; after the move's change check, the commit goes out as
    /// another batch. Returns `S0`, the source's metadata that the committed file
    /// corresponds to.
    fn put_file(
        &mut self,
        src: &Dir,
        node: &Node,
        dst: &RDir,
        target: &OsStr,
        overwrite: bool,
    ) -> Result<Meta, PutFail> {
        let sys = self.t.sys;
        let (fin, m0) = open_for_read(sys, src.fd(), &node.name, Some(node.meta.id.inode()))?;
        let direct = self.direct && !overwrite;
        let (made, handle) = if direct {
            let x = self.open_final(dst, target)?;
            self.wrote_direct = true;
            x
        } else {
            self.create_temp(dst, target)?
        };
        match self.fill(&fin, &m0, &handle) {
            Filled::Done => {}
            // Direct-write mode: every byte and the metadata were acknowledged before the
            // session ended, so the file counts as committed (R-1, E-25); only its `CLOSE`
            // is unconfirmed, and the session is gone for every later entry.
            Filled::Complete if direct && !self.moving => self.lost = true,
            Filled::Complete => {
                self.discard(None, &made, true);
                return Err(PutFail::Server("close", SftpError::Lost));
            }
            Filled::Failed(e, closed) => {
                let open = (!closed).then_some(&handle[..]);
                self.discard(open, &made, e.is_lost());
                return Err(e);
            }
        }
        // M1 4.8 step 2: before a move commits, the source must still be `S0`. In
        // direct-write mode the check runs after the last byte, and a failed check removes
        // the destination name.
        if self.moving {
            let now = sys.stat_fd(fin.as_fd());
            match now {
                Ok(m) if m.snapshot() == m0.snapshot() => {}
                Ok(_) => {
                    self.discard(None, &made, false);
                    return Err(PutFail::SourceChanged);
                }
                Err(e) => {
                    self.discard(None, &made, false);
                    return Err(PutFail::Local("stat source", e));
                }
            }
        }
        if direct {
            // The `EXCL` create took the place of the commit (M1 4.7 step 5).
            return Ok(m0);
        }
        self.commit(&made, dst, target, overwrite)?;
        Ok(m0)
    }

    /// The data, then, without waiting, the mode and times, a move's `fsync@openssh.com`
    /// (R-4) and the `CLOSE`: one batch behind the last `WRITE`
    /// ([`Writes`](super::session::Writes)). The server executes them in that order, so no
    /// byte lands after the times are set and the `CLOSE` closes the complete file; every
    /// reply is awaited and checked before the commit, a failed `CLOSE` included. A step
    /// whose failpoint fires, or a cancel, stops the sending there; the replies to what
    /// went out are still collected.
    fn fill(&mut self, fin: &OwnedFd, m0: &Meta, handle: &[u8]) -> Filled {
        let sys = self.t.sys;
        let s = self.s;
        let cancel = self.cancel.clone();
        let fsync = self.moving && self.caps.fsync;
        let mut src = Source {
            sys,
            fd: fin.as_fd(),
            errno: None,
        };
        let base = self.t.bytes_done;
        let t = &mut self.t;
        let mut progress = |acked| {
            t.bytes_done = base + acked;
            t.tick();
        };
        let mut w = s.writes(handle, 0, &cancel);
        match w.send_data(&mut src, &mut progress, &mut || step(sys, "put.write")) {
            Ok(_) => {}
            Err(SftpError::Cancelled) => return Filled::Failed(PutFail::Cancelled, false),
            Err(SftpError::Local(_)) if src.errno.is_some() => {
                let errno = src.errno.unwrap_or(Errno::IO);
                return Filled::Failed(PutFail::Local("read", errno), false);
            }
            Err(e) => return Filled::Failed(PutFail::Server("write", e), false),
        }
        let attrs = file_attrs(m0);
        type Build<'h> = Box<dyn FnOnce(u32) -> Packet + 'h>;
        let mut steps: Vec<(&'static str, &'static str, Build)> = vec![(
            "set attributes",
            "put.setstat",
            Box::new(|id| Packet::Fsetstat {
                id,
                handle: handle.to_vec(),
                attrs,
            }),
        )];
        if fsync {
            steps.push((
                "fsync",
                "put.fsync",
                Box::new(|id| Packet::Extended {
                    id,
                    name: ext::FSYNC.0.to_vec(),
                    data: proto::ext_args(&[handle]),
                }),
            ));
        }
        steps.push((
            "close",
            "put.close",
            Box::new(|id| Packet::Close {
                id,
                handle: handle.to_vec(),
            }),
        ));
        // What went out, by its reply's index; the step that did not, and why.
        let mut sent: Vec<(&'static str, usize)> = Vec::new();
        let mut held: Option<PutFail> = None;
        for (op, fp, build) in steps {
            if cancel.load(Ordering::SeqCst) {
                held = Some(PutFail::Cancelled);
                break;
            }
            match step(sys, fp).and_then(|()| w.send(build)) {
                Ok(k) => sent.push((op, k)),
                Err(e) => {
                    held = Some(PutFail::Server(op, e));
                    break;
                }
            }
        }
        let closed = sent.iter().any(|(op, _)| *op == "close");
        let mut got = w.finish(&mut progress);
        let mut results = vec![("write", got.data.map(|_| ()))];
        for (op, k) in sent {
            results.push((op, s.outcome(got.tail.take(k))));
        }
        let ok = |op: &str| results.iter().any(|(o, r)| *o == op && r.is_ok());
        // Every byte, the metadata and a move's sync were acknowledged: only the `CLOSE`
        // can be missing.
        let complete = ok("write") && ok("set attributes") && (!fsync || ok("fsync"));
        let lost = got.tail.lost
            || results.iter().any(|(_, r)| *r == Err(SftpError::Lost))
            || held.as_ref().is_some_and(PutFail::is_lost);
        if lost {
            if complete {
                return Filled::Complete;
            }
            let op = results
                .iter()
                .find(|(_, r)| r.is_err())
                .map_or("close", |(op, _)| op);
            return Filled::Failed(PutFail::Server(op, SftpError::Lost), closed);
        }
        if got.tail.cancelled || matches!(held, Some(PutFail::Cancelled)) {
            return Filled::Failed(PutFail::Cancelled, closed);
        }
        if let Some((op, Err(e))) = results.into_iter().find(|(_, r)| r.is_err()) {
            return Filled::Failed(PutFail::Server(op, e), closed);
        }
        match held {
            Some(e) => Filled::Failed(e, closed),
            None => Filled::Done,
        }
    }

    /// `.<name>.mc-partial-<random>`, opened `CREAT` + `EXCL` + `WRITE` with mode `0600`
    /// (R-1); a taken name gets a new random suffix.
    fn create_temp(&mut self, dst: &RDir, target: &OsStr) -> Result<(Made, Vec<u8>), PutFail> {
        loop {
            if self.t.cancelled() {
                return Err(PutFail::Cancelled);
            }
            let tmp = self.next_partial(target);
            let made = Made {
                path: dst.path_of(&tmp),
                shown: dst.shown_of(&tmp),
            };
            match self.open_excl(&made) {
                Ok(h) => return Ok((made, h)),
                Err(PutFail::Exists) => continue,
                Err(e) => return Err(e),
            }
        }
    }

    /// Direct-write mode: the final name, created `CREAT` + `EXCL` (M1 4.7 step 5).
    fn open_final(&mut self, dst: &RDir, target: &OsStr) -> Result<(Made, Vec<u8>), PutFail> {
        let made = Made {
            path: dst.path_of(target),
            shown: dst.shown_of(target),
        };
        let h = self.open_excl(&made)?;
        Ok((made, h))
    }

    /// `OPEN` with `CREAT` + `EXCL` + `WRITE`; `Exists` when an `LSTAT` finds the name. A
    /// session lost meanwhile may have left the name created: the report names it (R-1).
    fn open_excl(&mut self, made: &Made) -> Result<Vec<u8>, PutFail> {
        let attrs = Attrs {
            perms: Some(0o600),
            ..Attrs::default()
        };
        let flags = open::WRITE | open::CREAT | open::EXCL;
        let r =
            step(self.t.sys, "put.open").and_then(|()| self.firm().open(&made.path, flags, attrs));
        match r {
            Ok(h) => Ok(h),
            Err(e @ SftpError::Status { .. }) => match self.lstat(&made.path) {
                Ok(Some(_)) => Err(PutFail::Exists),
                Ok(None) => Err(PutFail::Server("create", e)),
                Err(e) => Err(PutFail::Server("create", e)),
            },
            Err(SftpError::Lost) => {
                self.lost = true;
                self.note(&made.shown, MAY_BE_PARTIAL);
                Err(PutFail::Server("create", SftpError::Lost))
            }
            Err(e) => Err(PutFail::Server("create", e)),
        }
    }

    /// The commit of a complete temporary file (R-1, R-2). After Overwrite,
    /// `posix-rename@openssh.com` over the final name, which consumes the temporary name.
    /// Otherwise one batch: `hardlink@openssh.com` and the `REMOVE` of the temporary name,
    /// which the server executes after the link, so the name goes whether the link made it
    /// a second name of the committed file or failed. A hard link the server refuses as
    /// unsupported or not permitted turns the job to direct-write mode (M1 4.7 step 5); a
    /// link that fails because the final name exists raises the question, and the file is
    /// uploaded again after the answer (E-25).
    fn commit(
        &mut self,
        made: &Made,
        dst: &RDir,
        target: &OsStr,
        overwrite: bool,
    ) -> Result<(), PutFail> {
        let sys = self.t.sys;
        let fin = dst.path_of(target);
        let (op, link, removed) = if overwrite {
            let r =
                step(sys, "put.replace").and_then(|()| self.firm().posix_rename(&made.path, &fin));
            ("replace", r, None)
        } else {
            let s = self.s;
            let mut b = s.batch(&self.cancel);
            let link = step(sys, "put.link").and_then(|()| {
                if !s.has(ext::HARDLINK) {
                    return Err(SftpError::Status {
                        code: status::OP_UNSUPPORTED,
                        message: String::new(),
                    });
                }
                b.send(|id| Packet::Extended {
                    id,
                    name: ext::HARDLINK.0.to_vec(),
                    data: proto::ext_args(&[&made.path, &fin]),
                })
            });
            let remove = step(sys, "put.remove").and_then(|()| {
                b.send(|id| Packet::Remove {
                    id,
                    path: made.path.clone(),
                })
            });
            let mut got = b.collect();
            let link = link.and_then(|k| s.outcome(got.take(k)));
            let remove = remove.and_then(|k| s.outcome(got.take(k)));
            ("commit", link, Some(remove))
        };
        match link {
            // The rename consumed the temporary name.
            Ok(()) if overwrite => Ok(()),
            Ok(()) => {
                // The temporary name was a second link to the committed file.
                match removed {
                    Some(Err(SftpError::Lost)) => {
                        self.lost = true;
                        self.note(&made.shown, format!("{NOT_REMOVED}: {LOST}"));
                    }
                    Some(Err(e)) => self.note(&made.shown, format!("{NOT_REMOVED}: {e}")),
                    _ => {}
                }
                Ok(())
            }
            Err(SftpError::Lost) => {
                self.lost = true;
                self.note(&made.shown, MAY_BE_PARTIAL);
                Err(PutFail::Refused(LOST_AT_COMMIT.into()))
            }
            Err(e) => {
                match removed {
                    // The `REMOVE` behind the link took the temporary name.
                    Some(r) => self.removed(made, r),
                    None => self.discard(None, made, false),
                }
                if let SftpError::Status {
                    code: status::OP_UNSUPPORTED | status::PERMISSION_DENIED,
                    ..
                } = e
                    && !overwrite
                {
                    // No hard links on this server: this file and every later one of the
                    // job are written directly (M1 4.7 step 5).
                    tracing::info!(
                        session = self.remote.id(),
                        "sftp: hard links refused; direct-write mode"
                    );
                    self.direct = true;
                    return Err(PutFail::Again);
                }
                if self.lost {
                    return Err(PutFail::Server(op, SftpError::Lost));
                }
                match self.lstat(&fin) {
                    Ok(Some(_)) => Err(PutFail::Exists),
                    Ok(None) => Err(PutFail::Server(op, e)),
                    Err(e) => Err(PutFail::Server("stat destination", e)),
                }
            }
        }
    }

    /// After a failure or a cancel: closes `handle` when its `CLOSE` did not go out, and
    /// removes the name the attempt made (R-1), in one batch; the `CLOSE`'s outcome is of
    /// no consequence, the name goes behind it. What cannot be removed is named in the
    /// report; after a lost session, as a path that may hold partial data.
    fn discard(&mut self, handle: Option<&[u8]>, made: &Made, lost: bool) {
        if lost || self.s.lost().is_some() {
            self.lost = true;
            self.note(&made.shown, MAY_BE_PARTIAL);
            return;
        }
        let s = self.s;
        let mut b = s.batch(&self.cancel);
        if let Some(h) = handle {
            let _ = b.send(|id| Packet::Close {
                id,
                handle: h.to_vec(),
            });
        }
        let remove = step(self.t.sys, "put.remove").and_then(|()| {
            b.send(|id| Packet::Remove {
                id,
                path: made.path.clone(),
            })
        });
        let mut got = b.collect();
        let r = remove.and_then(|k| s.outcome(got.take(k)));
        self.removed(made, r);
    }

    /// The report's note for the `REMOVE` of a name an attempt made.
    fn removed(&mut self, made: &Made, r: Result<(), SftpError>) {
        match r {
            Ok(()) => {}
            Err(SftpError::Lost) => {
                self.lost = true;
                self.note(&made.shown, MAY_BE_PARTIAL);
            }
            Err(e) => self.note(&made.shown, format!("{NOT_REMOVED}: {e}")),
        }
    }

    /// A symlink, created as a symlink (R-3): under the final name, which never shows
    /// partial content; after Overwrite, under a temporary name that
    /// `posix-rename@openssh.com` moves over the final name (R-2).
    fn put_symlink(
        &mut self,
        o: &LocalOrigin,
        src: &Dir,
        node: &Node,
        dst: &RDir,
        target: &OsStr,
        overwrite: bool,
    ) -> Result<Meta, PutFail> {
        let sys = self.t.sys;
        let (link, now) = o.read_link(src, node)?;
        if self.moving {
            // M1 4.8 step 2: the link must still be the one that was read.
            let again = sys
                .stat_at("move.check", src.fd(), &node.name)
                .map_err(|e| PutFail::Local("stat source", e))?;
            if again.snapshot() != now.snapshot() {
                return Err(PutFail::SourceChanged);
            }
        }
        let fin = dst.path_of(target);
        if !overwrite {
            let r =
                step(sys, "put.symlink").and_then(|()| self.firm().symlink(&fin, link.as_bytes()));
            return match r {
                Ok(()) => Ok(now),
                Err(e @ SftpError::Status { .. }) => match self.lstat(&fin) {
                    Ok(Some(_)) => Err(PutFail::Exists),
                    Ok(None) => Err(PutFail::Server("create symlink", e)),
                    Err(e) => Err(PutFail::Server("create symlink", e)),
                },
                Err(e) => Err(PutFail::Server("create symlink", e)),
            };
        }
        let made = loop {
            if self.t.cancelled() {
                return Err(PutFail::Cancelled);
            }
            let tmp = self.next_partial(target);
            let path = dst.path_of(&tmp);
            let r =
                step(sys, "put.symlink").and_then(|()| self.firm().symlink(&path, link.as_bytes()));
            match r {
                Ok(()) => {
                    break Made {
                        path,
                        shown: dst.shown_of(&tmp),
                    };
                }
                Err(e @ SftpError::Status { .. }) => match self.lstat(&path) {
                    Ok(Some(_)) => continue,
                    Ok(None) => return Err(PutFail::Server("create symlink", e)),
                    Err(e) => return Err(PutFail::Server("create symlink", e)),
                },
                Err(SftpError::Lost) => {
                    // The temporary link may exist: the report names it (R-1).
                    self.lost = true;
                    self.note(&dst.shown_of(&tmp), MAY_BE_PARTIAL);
                    return Err(PutFail::Server("create symlink", SftpError::Lost));
                }
                Err(e) => return Err(PutFail::Server("create symlink", e)),
            }
        };
        let r = step(sys, "put.replace").and_then(|()| self.firm().posix_rename(&made.path, &fin));
        match r {
            Ok(()) => Ok(now),
            Err(e) => {
                let lost = e == SftpError::Lost;
                self.discard(None, &made, lost);
                Err(PutFail::Server("replace", e))
            }
        }
    }

    /// A directory: `MKDIR` with `0700` (or the merge of M1's "directory exists"), its
    /// children, then its mode and times in post-order when this job made it (M1 4.7). In
    /// a move, the local source directory goes after its children, when it is empty.
    fn dir(
        &mut self,
        o: &LocalOrigin,
        src: &Dir,
        node: &Node,
        dst: &RDir,
        mut target: OsString,
    ) -> Flow {
        let sys = self.t.sys;
        let spath = src.path.join(&node.name);
        let sdir = loop {
            match o.open_dir(src, node) {
                Ok(d) => break d,
                Err(EntryError::Os { op, errno }) => match self.t.decide_error(&spath, op, errno) {
                    Some(true) => continue,
                    Some(false) => {
                        self.t
                            .fail(node, spath, EntryError::Os { op, errno }.to_string());
                        return Flow::Continue;
                    }
                    None => return self.t.stop(),
                },
                Err(e) => {
                    self.t.fail(node, spath, e.to_string());
                    return Flow::Continue;
                }
            }
        };
        let created = loop {
            if self.t.cancelled() {
                return self.t.stop();
            }
            let path = dst.path_of(&target);
            let shown = dst.shown_of(&target);
            let attrs = Attrs {
                perms: Some(0o700),
                ..Attrs::default()
            };
            let r = step(sys, "put.mkdir").and_then(|()| self.firm().mkdir(&path, attrs));
            let e = match r {
                Ok(()) => break true,
                Err(e @ SftpError::Status { .. }) => match self.lstat(&path) {
                    Ok(Some(a)) => {
                        let dm = meta_of(&a, synthetic_id(self.remote.id(), 0));
                        match self.resolve(o, src, &node.meta, &dm, &shown) {
                            Decision::Merge => break false,
                            Decision::Rename(n) => {
                                target = n;
                                continue;
                            }
                            Decision::Skip(why) => {
                                self.t.skip(node, spath, why);
                                return Flow::Continue;
                            }
                            Decision::Cancel => return self.t.stop(),
                            Decision::Overwrite => {
                                unreachable!("overwrite offered for a directory")
                            }
                        }
                    }
                    Ok(None) => e,
                    Err(e) => e,
                },
                Err(e) => e,
            };
            match self.ask_server(&shown, "make directory", &e) {
                Ask::Retry => {}
                Ask::Fail(why) => {
                    self.t.fail(node, spath, why);
                    return Flow::Continue;
                }
                Ask::Stop => return self.t.stop(),
            }
        };
        let ddir = RDir {
            path: dst.path_of(&target),
            shown: dst.shown_of(&target),
        };
        for child in &node.children {
            if self.entry(o, &sdir, child, &ddir, child.name.clone()) == Flow::Stop {
                return Flow::Stop;
            }
        }
        let mut meta_ok = true;
        if created {
            let r = if self.lost {
                Err(SftpError::Lost)
            } else {
                step(sys, "put.setstat")
                    .and_then(|()| self.firm().setstat(&ddir.path, dir_attrs(&node.meta)))
            };
            if let Err(e) = r {
                if e == SftpError::Lost {
                    self.lost = true;
                }
                meta_ok = false;
                let kept = if self.moving {
                    "; source directory kept"
                } else {
                    ""
                };
                self.t
                    .report
                    .fail(spath.clone(), format!("set directory metadata: {e}{kept}"));
            }
        }
        if self.moving {
            // The children's sources go first; then the directory, if it is empty and
            // still the one the job emptied (M1 4.8 step 6). After a metadata failure it
            // stays.
            self.flush(o);
            if meta_ok {
                match o.remove_dir(src, &node.name, node.meta.id.inode()) {
                    Removed::Done | Removed::Retained => self.t.report.dirs_done += 1,
                    Removed::Kept(why) | Removed::Failed(why) => {
                        self.note(&spath, why);
                    }
                }
                self.t.tick();
            }
            return Flow::Continue;
        }
        if meta_ok {
            self.t.done(node);
        }
        Flow::Continue
    }

    // ---- the move's local side (R-4, M1 4.8) -------------------------------------------

    fn queue(&mut self, src: &Dir, node: &Node, m0: &Meta) {
        if node.meta.kind == Kind::File {
            self.batch_bytes += m0.size;
        }
        self.batch.push(Pending {
            dir: src.clone(),
            name: node.name.clone(),
            snap: m0.snapshot(),
            path: src.path.join(&node.name),
        });
    }

    fn flush_if_full(&mut self, o: &LocalOrigin) -> Flow {
        if self.batch.len() >= BATCH_FILES || self.batch_bytes >= BATCH_BYTES {
            self.flush(o);
        }
        Flow::Continue
    }

    /// Removes the local sources of committed uploads, each only if it still matches its
    /// `S0` (M1 4.8 step 5.3). The uploads were synced before their commit when the server
    /// has `fsync@openssh.com`; the rest of a server's durability is out of reach (R-4).
    fn flush(&mut self, o: &LocalOrigin) {
        self.batch_bytes = 0;
        for p in std::mem::take(&mut self.batch) {
            match o.remove(&p.dir, &p.name, &p.snap) {
                Removed::Done | Removed::Retained => self.t.report.done += 1,
                Removed::Kept(why) | Removed::Failed(why) => self.t.report.fail(p.path, why),
            }
        }
        self.t.tick();
    }

    /// The report notes of the whole job.
    fn finish(&mut self) {
        if self.wrote_direct {
            self.t.report.notes.push(DIRECT_WRITE.into());
        }
        if self.moving && self.committed && !self.caps.fsync {
            self.t.report.notes.push(NOT_SYNCED.into());
        }
    }
}

impl PutFail {
    fn is_lost(&self) -> bool {
        matches!(self, PutFail::Server(_, SftpError::Lost))
    }
}

/// `sftp://[user@]host[:port]/dir`, as a path for questions and the report.
fn shown(t: &Target, dir: &VPath) -> PathBuf {
    PathBuf::from(OsString::from_vec(location(t, dir)))
}

/// Where an upload goes, with the M1 rules: an existing directory on the server to copy
/// into (a `STAT`, which follows a symlink as M1 follows one in a panel path), or, for a
/// single source name in total, a new path in an existing directory.
fn destination(
    s: &Session,
    t: &Target,
    to: To,
    names: &[OsString],
    cancel: &AtomicBool,
) -> Result<(RDir, Vec<OsString>), String> {
    let is_dir = |p: &VPath| -> Result<bool, String> {
        match s.stat(&p.to_bytes(), cancel) {
            Ok(a) => Ok(a.kind() == Some(S_IFDIR)),
            Err(SftpError::Status {
                code: status::NO_SUCH_FILE,
                ..
            }) => Ok(false),
            Err(e) => Err(format!("{}: {e}", shown(t, p).display())),
        }
    };
    let dst = match to {
        To::Resolve(dst) => {
            if is_dir(dst)? {
                let dir = RDir {
                    path: dst.to_bytes(),
                    shown: shown(t, dst),
                };
                return Ok((dir, names.to_vec()));
            }
            if names.len() != 1 {
                return Err(format!("{}: no such directory", shown(t, dst).display()));
            }
            dst
        }
        To::Exactly(p) => p,
    };
    let (Some(parent), Some(name)) = (dst.parent(), dst.name()) else {
        return Err(format!(
            "{}: not a valid destination",
            shown(t, dst).display()
        ));
    };
    if !is_dir(&parent)? {
        return Err(format!(
            "{}: no such directory",
            shown(t, &parent).display()
        ));
    }
    let dir = RDir {
        path: parent.to_bytes(),
        shown: shown(t, &parent),
    };
    Ok((dir, vec![name.to_owned()]))
}

/// F5 and F6 from local groups to the server `remote` (P3 5.6): `dst` is what the user
/// confirmed, an existing directory on the server or, for a single source name in total,
/// a new path. `moving`: F6, which removes each local source after its upload was
/// committed (best-effort, R-4).
pub fn upload(
    sys: &Sys,
    ui: &mut dyn Interaction,
    groups: &[Group],
    remote: &Arc<RemoteProvider>,
    dst: &VPath,
    moving: bool,
) -> Report {
    run(sys, ui, groups, remote, To::Resolve(dst), moving, false)
}

/// The F4 write-back (P3 5.6): the edited view copy `copy` goes to the server's `path`. The
/// user's answer to the write-back question is the decision to replace, so an existing file
/// is replaced, through `posix-rename@openssh.com` only (R-2).
pub fn write_back(
    sys: &Sys,
    ui: &mut dyn Interaction,
    copy: &Path,
    remote: &Arc<RemoteProvider>,
    path: &VPath,
) -> Report {
    let (Some(dir), Some(name)) = (copy.parent(), copy.file_name()) else {
        return Report::refused(JobVerb::Copy, "not a file");
    };
    let groups = [Group::new(dir, vec![name.to_owned()])];
    run(sys, ui, &groups, remote, To::Exactly(path), false, true)
}

fn run(
    sys: &Sys,
    ui: &mut dyn Interaction,
    groups: &[Group],
    remote: &Arc<RemoteProvider>,
    to: To,
    moving: bool,
    replace: bool,
) -> Report {
    let verb = if moving { JobVerb::Move } else { JobVerb::Copy };
    let o = LocalOrigin::new(sys);
    let opened: Opened = match o.open_groups(verb, groups) {
        Ok(x) => x,
        Err(r) => return *r,
    };
    let s = remote.session();
    let cancel = sys.cancel_flag().clone();
    let names: Vec<OsString> = groups.iter().flat_map(|g| g.names.clone()).collect();
    let (dst, targets) = match destination(s, remote.target(), to, &names, &cancel) {
        Ok(x) => x,
        Err(why) => return opened.refuse(verb, why),
    };
    // Each group's slice of the targets, by its offset among all names.
    let mut offsets = Vec::with_capacity(groups.len());
    let mut at = 0;
    for g in groups {
        offsets.push(at);
        at += g.names.len();
    }
    let slices: Vec<Vec<OsString>> = opened
        .sources
        .iter()
        .map(|s| targets[offsets[s.group]..offsets[s.group] + s.names.len()].to_vec())
        .collect();
    // The scan of M1 4.4 without a local destination: a server has no identities to
    // compare. A move skips mount points (I-6).
    let scan_verb = if moving { Verb::Move } else { Verb::Copy };
    let mut rep = Reporter::new(ui);
    let plans = {
        let scans: Vec<Scan> = opened
            .sources
            .iter()
            .map(|s| Scan {
                sys,
                verb: scan_verb,
                src: s.dir.fd(),
                src_path: &s.dir.path,
                names: &s.names,
                dst: None,
            })
            .collect();
        scan_all(&scans, &mut rep)
    };
    let plans = match plans {
        Ok(p) => p,
        Err(Refusal::Cancelled) => {
            let mut r = Report::new(verb);
            opened.report_failed(&mut r);
            r.cancelled = true;
            return r;
        }
        Err(e) => return opened.refuse(verb, e),
    };
    let mut t = Transfer::new(sys, rep, Report::new(verb));
    opened.report_failed(&mut t.report);
    t.set_sum(plans.iter().map(|p| p.totals).sum());
    if replace {
        t.policy.overwrite = Some(false);
    }
    let caps = s.caps();
    let mut p = Put {
        t,
        s,
        remote,
        caps,
        cancel,
        direct: !caps.hard_link,
        wrote_direct: false,
        lost: false,
        moving,
        batch: Vec::new(),
        batch_bytes: 0,
        committed: false,
        skip_server: HashSet::new(),
        tmp_base: random_u64(),
        tmp_seq: 0,
    };
    'job: for ((plan, src), targets) in plans.iter().zip(&opened.sources).zip(slices) {
        for (node, target) in plan.roots.iter().zip(targets) {
            if p.entry(&o, &src.dir, node, &dst, target) == Flow::Stop {
                break 'job;
            }
        }
    }
    // Job end and cancel both settle the committed uploads' sources.
    if moving {
        p.flush(&o);
    }
    p.finish();
    p.t.report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_clears_setuid_and_setgid() {
        let m = Meta {
            perm: 0o6755,
            atime: Ts { sec: 5, nsec: 9 },
            mtime: Ts { sec: -3, nsec: 0 },
            ..Meta::default()
        };
        let a = file_attrs(&m);
        assert_eq!(a.perms, Some(0o755));
        assert_eq!(a.times, Some((5, 0)));
        let d = Meta {
            perm: 0o3775,
            ..Meta::default()
        };
        assert_eq!(dir_attrs(&d).perms, Some(0o1775));
        assert_eq!(
            secs(Ts {
                sec: i64::MAX,
                nsec: 0
            }),
            u32::MAX
        );
    }
}
