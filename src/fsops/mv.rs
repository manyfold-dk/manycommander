#![forbid(unsafe_code)]
//! Move and rename (F6, Shift+F6; design 4.8).
//!
//! Every entry first tries `renameat2(..., RENAME_NOREPLACE)` and lets the kernel decide.
//! `EXDEV` leads to the cross-filesystem path: copy (the transfer engine, with the change
//! check before commit), then unlink the source only after a **group commit** has made
//! the batch durable. I-1: at every instant, every source file's complete content exists
//! in at least one committed location.

use super::copy::{Decision, Dir, Flow, Transfer, subtree_counts};
use super::job::{JobVerb, Report};
use super::plan::{Node, Note, Refusal, Scan, Verb, scan};
use super::question::{Interaction, Phase, Progress, Reporter, is_conflict_errno};
use super::sys::{Kind, Snapshot, Sys, random_u64};
use super::walk::{EntryError, errno_text, open_child_dir};
use rustix::fd::{AsFd, OwnedFd};
use rustix::io::Errno;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A batch is flushed when it holds this many committed entries (design 4.8 step 5; 256
/// after the T12 measurements: 64 made small-file moves 2.2x slower than `mv`).
pub const BATCH_FILES: usize = 256;
/// ... or this many bytes.
pub const BATCH_BYTES: u64 = 256 << 20;

/// A committed destination whose source waits for the flush.
pub(crate) struct Pending {
    src: Arc<OwnedFd>,
    path: PathBuf,
    name: OsString,
    snap: Snapshot,
    dst: Arc<OwnedFd>,
    domain: (u64, u64),
}

#[derive(Default)]
pub struct Batch {
    entries: Vec<Pending>,
    bytes: u64,
    /// Destination directories this job created: the flush syncs their filesystem even
    /// when no file is pending, so a directory-only tree is durable before its sources go.
    sync: Vec<(Arc<OwnedFd>, (u64, u64))>,
}

impl Batch {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.sync.is_empty()
    }

    pub(crate) fn sync_dir(&mut self, d: &Dir) {
        let dom = d.meta.id.domain();
        if !self.sync.iter().any(|(_, x)| *x == dom) {
            self.sync.push((d.fd.clone(), dom));
        }
    }
}

impl Transfer<'_, '_> {
    /// Appends a committed entry to the batch (design 4.8 step 4).
    pub(crate) fn queue(&mut self, src: &Dir, node: &Node, snap: Snapshot, dst: &Dir) {
        if node.meta.kind == Kind::File {
            self.batch.bytes += snap.size;
        }
        self.batch.entries.push(Pending {
            src: src.fd.clone(),
            path: src.path.join(&node.name),
            name: node.name.clone(),
            snap,
            dst: dst.fd.clone(),
            domain: dst.meta.id.domain(),
        });
    }

    pub(crate) fn flush_if_full(&mut self) -> Flow {
        if self.batch.entries.len() >= BATCH_FILES || self.batch.bytes >= BATCH_BYTES {
            self.flush()
        } else {
            Flow::Continue
        }
    }

    /// Flushes the batch (design 4.8 step 5): `syncfs` every destination filesystem, then
    /// unlink each source whose identity, size, mtime and ctime still equal `S0`.
    pub(crate) fn flush(&mut self) -> Flow {
        if self.batch.is_empty() {
            return Flow::Continue;
        }
        let entries = std::mem::take(&mut self.batch.entries);
        let dirs = std::mem::take(&mut self.batch.sync);
        self.batch.bytes = 0;
        let current = entries.first().map(|p| p.path.clone()).unwrap_or_default();
        let p = Progress {
            phase: Phase::Flushing,
            files_done: self.files_done,
            files_total: self.files_total,
            bytes_done: self.bytes_done,
            bytes_total: self.bytes_total,
            current,
        };
        self.rep.progress(|| p);
        let sys = self.sys;
        let targets = entries
            .iter()
            .map(|p| (p.dst.clone(), p.domain))
            .chain(dirs);
        let mut synced: Vec<(u64, u64)> = Vec::new();
        for (fd, domain) in targets {
            if synced.contains(&domain) {
                continue;
            }
            synced.push(domain);
            if let Err(e) = sys.syncfs("move.syncfs", fd.as_fd()) {
                // Writeback errors are not per entry: keep every source of the batch.
                let why = errno_text(e);
                for p in &entries {
                    self.report.fail(
                        p.path.clone(),
                        format!("syncfs of the destination failed ({why}); source kept"),
                    );
                }
                self.report.notes.push(format!(
                    "syncfs of the destination failed ({why}); the job stopped, and every source of the last batch was kept"
                ));
                return self.halt();
            }
        }
        for p in entries {
            match sys.stat_at("move.statx", p.src.as_fd(), &p.name) {
                Ok(m) if m.snapshot() == p.snap => {
                    // Linux has no unlink-by-fd: a replacement between the statx and the
                    // unlinkat is a documented residual race.
                    match sys.unlink("move.unlink", p.src.as_fd(), &p.name) {
                        Ok(()) => self.report.done += 1,
                        Err(e) => self.report.fail(
                            p.path,
                            format!(
                                "unlink source: {}; the destination is committed, both kept",
                                errno_text(e)
                            ),
                        ),
                    }
                }
                Ok(_) => self.report.fail(p.path, "source changed; kept both"),
                // Already gone: the committed destination holds the content.
                Err(Errno::NOENT) => self.report.done += 1,
                Err(e) => self
                    .report
                    .fail(p.path, format!("stat source: {}; kept both", errno_text(e))),
            }
        }
        Flow::Continue
    }

    /// After a source directory's children: flush, then `rmdir` it if it is empty
    /// (design 4.8 step 6). A directory that still holds entries stays and is reported.
    pub(crate) fn finish_source_dir(&mut self, src: &Dir, node: &Node, spath: &Path) -> Flow {
        if self.flush() == Flow::Stop {
            return Flow::Stop;
        }
        // Remove only the directory the job emptied, not a replacement under its name.
        match self.sys.stat_at("move.dirstat", src.fd(), &node.name) {
            Ok(m) if m.kind == Kind::Dir && m.id.inode() == node.meta.id.inode() => {}
            Ok(_) => {
                self.report.notes.push(format!(
                    "{}: replaced during the move; kept",
                    spath.display()
                ));
                return Flow::Continue;
            }
            Err(e) => {
                self.report.notes.push(format!(
                    "{}: source directory not removed: {}",
                    spath.display(),
                    errno_text(e)
                ));
                return Flow::Continue;
            }
        }
        match self.sys.rmdir("move.rmdir", src.fd(), &node.name) {
            Ok(()) => self.report.dirs_done += 1,
            Err(Errno::NOTEMPTY | Errno::EXIST) => self.report.notes.push(format!(
                "{}: kept, it still holds entries that were not moved",
                spath.display()
            )),
            Err(e) => self.report.notes.push(format!(
                "{}: source directory not removed: {}",
                spath.display(),
                errno_text(e)
            )),
        }
        self.tick();
        Flow::Continue
    }

    /// A whole planned subtree moved by one `rename`.
    fn moved(&mut self, node: &Node) {
        let (entries, bytes) = subtree_counts(node);
        self.report.done += entries;
        self.report.dirs_done += count_dirs(node);
        self.settle(entries);
        self.bytes_done += bytes;
        self.tick();
    }
}

fn count_dirs(node: &Node) -> u64 {
    if node.meta.kind != Kind::Dir {
        return 0;
    }
    1 + node.children.iter().map(count_dirs).sum::<u64>()
}

/// Moves one planned entry: rename first, then merge, replace or the cross-filesystem path.
pub(crate) fn move_entry(
    t: &mut Transfer,
    src: &Dir,
    node: &Node,
    dst: &Dir,
    mut target: OsString,
) -> Flow {
    if t.stopped() {
        return Flow::Stop;
    }
    if t.cancelled() {
        return t.stop();
    }
    let sys = t.sys;
    let spath = src.path.join(&node.name);
    match &node.note {
        None => {}
        Some(Note::CaseRename) => {
            return match case_rename(sys, src, &node.name, &target) {
                Ok(()) => {
                    t.moved(node);
                    Flow::Continue
                }
                Err(msg) => {
                    t.report.notes.push(msg.clone());
                    t.fail(node, spath, msg);
                    Flow::Continue
                }
            };
        }
        Some(Note::Failed(e)) => {
            t.fail(node, spath, e.to_string());
            return Flow::Continue;
        }
        // A mount point is skipped before any rename is attempted.
        Some(n) => {
            t.skip(node, spath, n.reason());
            return Flow::Continue;
        }
    }
    loop {
        if t.cancelled() {
            return t.stop();
        }
        let dpath = dst.path.join(&target);
        let err = match sys.rename("move.rename", src.fd(), &node.name, dst.fd(), &target, true) {
            Ok(()) => {
                t.moved(node);
                return Flow::Continue;
            }
            Err(e) => e,
        };
        match err {
            Errno::XDEV => {
                t.moving = true;
                let flow = t.entry(src, node, dst, target);
                t.moving = false;
                return flow;
            }
            e if is_conflict_errno(e) => {
                let dm = match sys.stat_at("move.dststat", dst.fd(), &target) {
                    Ok(m) => m,
                    Err(Errno::NOENT) => continue,
                    Err(e) => match t.decide_error(&dpath, "stat destination", e) {
                        Some(true) => continue,
                        Some(false) => {
                            t.fail(
                                node,
                                spath,
                                EntryError::os("stat destination", e).to_string(),
                            );
                            return Flow::Continue;
                        }
                        None => return t.stop(),
                    },
                };
                if dm.id.inode() == node.meta.id.inode() {
                    t.skip(node, spath, "source and destination are the same file");
                    return Flow::Continue;
                }
                match t.resolve_conflict(&node.meta, src, &dm, dst, &dpath) {
                    Decision::Overwrite => {
                        match sys.rename(
                            "move.replace",
                            src.fd(),
                            &node.name,
                            dst.fd(),
                            &target,
                            false,
                        ) {
                            Ok(()) => {
                                t.moved(node);
                                return Flow::Continue;
                            }
                            Err(e) if is_conflict_errno(e) => continue,
                            Err(e) => match t.decide_error(&spath, "rename", e) {
                                Some(true) => continue,
                                Some(false) => {
                                    t.fail(node, spath, EntryError::os("rename", e).to_string());
                                    return Flow::Continue;
                                }
                                None => return t.stop(),
                            },
                        }
                    }
                    Decision::Merge => return merge(t, src, node, dst, &target),
                    Decision::Rename(n) => target = n,
                    Decision::Skip(why) => {
                        t.skip(node, spath, why);
                        return Flow::Continue;
                    }
                    Decision::Cancel => return t.stop(),
                }
            }
            // EBUSY and every other error: the error question. Never copy+delete as a
            // fallback for these (I-6).
            e => match t.decide_error(&spath, "rename", e) {
                Some(true) => continue,
                Some(false) => {
                    t.fail(node, spath, EntryError::os("rename", e).to_string());
                    return Flow::Continue;
                }
                None => return t.stop(),
            },
        }
    }
}

/// "Directory exists", Merge: move each child with the same rules, then `rmdir` the
/// source directory if it is empty.
fn merge(t: &mut Transfer, src: &Dir, node: &Node, dst: &Dir, target: &OsStr) -> Flow {
    let sys = t.sys;
    let spath = src.path.join(&node.name);
    let (sfd, smeta) = match open_child_dir(sys, "walk.openat", src.fd(), &node.name, &node.meta.id)
    {
        Ok(x) => x,
        Err(e) => {
            t.fail(node, spath, e.to_string());
            return Flow::Continue;
        }
    };
    let dfd = match sys.open_dir("move.opendst", dst.fd(), target) {
        Ok(f) => f,
        Err(e) => {
            let why = if matches!(e, Errno::LOOP | Errno::NOTDIR) {
                EntryError::TypeChanged
            } else {
                EntryError::os("open destination directory", e)
            };
            t.fail(node, spath, why.to_string());
            return Flow::Continue;
        }
    };
    let dmeta = match sys.stat_fd(dfd.as_fd()) {
        Ok(m) => m,
        Err(e) => {
            t.fail(
                node,
                spath,
                EntryError::os("stat destination", e).to_string(),
            );
            return Flow::Continue;
        }
    };
    let sdir = Dir {
        fd: Arc::new(sfd),
        meta: smeta,
        path: spath.clone(),
    };
    let ddir = Dir {
        fd: Arc::new(dfd),
        meta: dmeta,
        path: dst.path.join(target),
    };
    for child in &node.children {
        if move_entry(t, &sdir, child, &ddir, child.name.clone()) == Flow::Stop {
            return Flow::Stop;
        }
    }
    t.finish_source_dir(src, node, &spath)
}

/// Case-only rename (`Foo` -> `foo`) on a case-insensitive filesystem: through an
/// intermediate unique name in the same directory. If the second step fails, the message
/// names the intermediate path so the user can find the entry.
pub fn case_rename(sys: &Sys, dir: &Dir, from: &OsStr, to: &OsStr) -> Result<(), String> {
    let inter = {
        let mut v = Vec::new();
        v.push(b'.');
        v.extend_from_slice(&from.as_bytes()[..from.len().min(200)]);
        v.extend_from_slice(format!(".mc-case-{:016x}", random_u64()).as_bytes());
        OsString::from_vec(v)
    };
    sys.rename("move.case1", dir.fd(), from, dir.fd(), &inter, true)
        .map_err(|e| format!("rename: {}", errno_text(e)))?;
    sys.rename("move.case2", dir.fd(), &inter, dir.fd(), to, true)
        .map_err(|e| {
            format!(
                "case-only rename failed at the second step ({}); the entry is now at {}",
                errno_text(e),
                dir.path.join(&inter).display()
            )
        })
}

/// F6: plan, then move each selected entry; the final flush also runs after a cancel.
pub fn move_job(
    sys: &Sys,
    ui: &mut dyn Interaction,
    src_dir: &Path,
    names: &[OsString],
    dst: &Path,
) -> Report {
    let verb = JobVerb::Move;
    let src = match Dir::open_root(sys, src_dir) {
        Ok(d) => d,
        Err(e) => return Report::refused(verb, format!("{}: {e}", src_dir.display())),
    };
    let (dst, targets) = match super::copy::resolve_destination(sys, names, dst) {
        Ok(x) => x,
        Err(e) => return Report::refused(verb, e),
    };
    let mut rep = Reporter::new(ui);
    let plan = match scan(
        &Scan {
            sys,
            verb: Verb::Move,
            src: src.fd(),
            src_path: &src.path,
            names,
            dst: Some((dst.fd(), &targets)),
        },
        &mut rep,
    ) {
        Ok(p) => p,
        Err(Refusal::Cancelled) => {
            let mut r = Report::new(verb);
            r.cancelled = true;
            return r;
        }
        Err(e) => return Report::refused(verb, e),
    };
    let mut t = Transfer::new(sys, rep, Report::new(verb));
    t.set_totals(&plan);
    for (node, target) in plan.roots.iter().zip(targets) {
        if move_entry(&mut t, &src, node, &dst, target) == Flow::Stop {
            break;
        }
    }
    // Job end and cancel both complete the batch in progress.
    t.flush();
    t.report
}
