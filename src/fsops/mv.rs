#![forbid(unsafe_code)]
//! Move and rename (F6, Shift+F6; design 4.8).
//!
//! Every entry first tries `renameat2(..., RENAME_NOREPLACE)` and lets the kernel decide.
//! `EXDEV` leads to the cross-filesystem path: copy (the transfer engine, with the change
//! check before commit), then unlink the source only after a **group commit** has made
//! the batch durable. I-1: at every instant, every source file's complete content exists
//! in at least one committed location.
//!
//! Hard links (P2 9.2): unlinking one name of an inode changes its ctime and nlink, so the
//! committed names of a multi-linked source inode wait (deferred) until every in-set name of
//! it has settled, and are then checked together before any of them is unlinked.

use super::copy::{Decision, Dir, Flow, Part, Transfer, prepare, subtree_counts};
use super::group::Group;
use super::job::Report;
use super::plan::{Node, Note, Verb};
use super::question::{Interaction, Phase, Progress, is_conflict_errno};
use super::sys::{Kind, Meta, Snapshot, Sys, random_u64};
use super::walk::{EntryError, errno_text, open_child_dir};
use rustix::fd::{AsFd, OwnedFd};
use rustix::io::Errno;
use std::collections::HashMap;
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
    /// `(st_dev, st_ino)` of the source directory.
    src_dir: (u64, u64),
    path: PathBuf,
    name: OsString,
    snap: Snapshot,
    dst: Arc<OwnedFd>,
    domain: (u64, u64),
    /// An in-set name of a multi-linked source inode (P2 9.2): the inode and its link count
    /// when this name was copied or linked, which completes its `S0`.
    link: Option<((u64, u64), u32)>,
}

/// A source directory whose removal waits for the final flush (P2 9.2).
struct LaterDir {
    parent: Dir,
    name: OsString,
    id: (u64, u64),
    path: PathBuf,
}

#[derive(Default)]
pub struct Batch {
    entries: Vec<Pending>,
    bytes: u64,
    /// Destination directories this job created: the flush syncs their filesystem even
    /// when no file is pending, so a directory-only tree is durable before its sources go.
    sync: Vec<(Arc<OwnedFd>, (u64, u64))>,
    /// Committed names of multi-linked source inodes whose destinations a flush has made
    /// durable, and whose unlink waits for the other in-set names of the inode (P2 9.2).
    deferred: HashMap<(u64, u64), Vec<Pending>>,
    /// Per source directory `(st_dev, st_ino)`: the deferred names and the deferred
    /// directories it holds.
    holds: HashMap<(u64, u64), u32>,
    /// Source directories that held deferred entries when their children were done, in
    /// the order they finished (post-order).
    later: Vec<LaterDir>,
    /// The job end: every inode's in-set names count as settled.
    last: bool,
    /// A `syncfs` failed: the job stopped, and no source is unlinked after it.
    failed: bool,
}

impl Batch {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.sync.is_empty()
    }

    fn hold(&mut self, dir: (u64, u64)) {
        *self.holds.entry(dir).or_insert(0) += 1;
    }

    fn release(&mut self, dir: (u64, u64)) {
        if let Some(n) = self.holds.get_mut(&dir) {
            *n -= 1;
            if *n == 0 {
                self.holds.remove(&dir);
            }
        }
    }

    pub(crate) fn sync_dir(&mut self, d: &Dir) {
        let dom = d.meta.id.domain();
        if !self.sync.iter().any(|(_, x)| *x == dom) {
            self.sync.push((d.fd.clone(), dom));
        }
    }
}

impl Transfer<'_, '_> {
    /// Appends a committed entry to the batch (design 4.8 step 4). `m` is the source's
    /// metadata the destination corresponds to. Called before the entry settles, so an
    /// in-set name of a multi-linked inode is still recognised.
    pub(crate) fn queue(&mut self, src: &Dir, node: &Node, m: &Meta, dst: &Dir) {
        if node.meta.kind == Kind::File {
            self.batch.bytes += m.size;
        }
        self.batch.entries.push(Pending {
            src: src.fd.clone(),
            src_dir: src.meta.id.inode(),
            path: src.path.join(&node.name),
            name: node.name.clone(),
            snap: m.snapshot(),
            dst: dst.fd.clone(),
            domain: dst.meta.id.domain(),
            link: self.links.key(node).map(|k| (k, m.nlink)),
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
    /// unlink each source whose identity, size, mtime and ctime still equal `S0`. An in-set
    /// name of a multi-linked inode is deferred instead, and the inodes whose in-set names
    /// have all settled are unlinked together (P2 9.2).
    pub(crate) fn flush(&mut self) -> Flow {
        if self.batch.failed {
            return Flow::Stop;
        }
        if self.batch.is_empty() {
            // Nothing to sync: only deferred names can be due.
            let due = !self.batch.deferred.is_empty()
                && (self.batch.last || !self.links.ready.is_empty());
            if !due {
                if self.batch.deferred.is_empty() {
                    // Ready inodes without deferred names were never copied (renamed,
                    // skipped or failed): nothing waits for them.
                    self.links.ready.clear();
                }
                return Flow::Continue;
            }
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
                self.batch.failed = true;
                return self.halt();
            }
        }
        for p in entries {
            if let Some((k, _)) = p.link {
                // Its destination is durable now; its unlink waits (P2 9.2).
                self.batch.hold(p.src_dir);
                self.batch.deferred.entry(k).or_default().push(p);
                continue;
            }
            match sys.stat_at("move.statx", p.src.as_fd(), &p.name) {
                // Linux has no unlink-by-fd: a replacement between the statx and the
                // unlinkat is a documented residual race.
                Ok(m) if m.snapshot() == p.snap => self.unlink_source(p),
                Ok(_) => self.report.fail(p.path, "source changed; kept both"),
                // Already gone: the committed destination holds the content.
                Err(Errno::NOENT) => self.report.done += 1,
                Err(e) => self
                    .report
                    .fail(p.path, format!("stat source: {}; kept both", errno_text(e))),
            }
        }
        let ready: Vec<(u64, u64)> = if self.batch.last {
            self.links.ready.clear();
            self.batch.deferred.keys().copied().collect()
        } else {
            std::mem::take(&mut self.links.ready)
        };
        for k in ready {
            if let Some(names) = self.batch.deferred.remove(&k) {
                self.unlink_links(names);
            }
        }
        Flow::Continue
    }

    /// `unlinkat` of a checked source name.
    fn unlink_source(&mut self, p: Pending) {
        match self.sys.unlink("move.unlink", p.src.as_fd(), &p.name) {
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

    /// Settles the committed names of one multi-linked source inode whose in-set names have
    /// all settled (P2 9.2). Every name is checked with `statx` before any is unlinked, in
    /// full against its `S0`: identity, size, mtime, ctime, and the link count at its copy.
    /// Then the names that match are unlinked one after the other with no further check
    /// (each unlink changes the inode's ctime and nlink); the others are kept. The residual
    /// race is the M1 one between `statx` and `unlinkat`, extended over these consecutive
    /// unlinks. The design takes one `statx` of the inode; a `statx` per name, all before
    /// the first unlink, makes the same comparisons and also checks each name's identity
    /// directly (as M1 does) instead of inferring it from the link count and ctime, and it
    /// reports a name that is gone as gone.
    fn unlink_links(&mut self, names: Vec<Pending>) {
        let sys = self.sys;
        let checks: Vec<_> = names
            .iter()
            .map(|p| sys.stat_at("move.linkstat", p.src.as_fd(), &p.name))
            .collect();
        for (p, c) in names.into_iter().zip(checks) {
            self.batch.release(p.src_dir);
            let nlink = p.link.map_or(0, |(_, n)| n);
            match c {
                Ok(m) if m.snapshot() == p.snap && m.nlink == nlink => self.unlink_source(p),
                Ok(_) => self.report.fail(p.path, "source changed; kept both"),
                Err(Errno::NOENT) => self.report.done += 1,
                Err(e) => self
                    .report
                    .fail(p.path, format!("stat source: {}; kept both", errno_text(e))),
            }
        }
    }

    /// Job end and cancel (design 4.8 step 5, P2 9.2): the final flush. An in-set name that
    /// has not settled by now never will, so every deferred inode is settled with what was
    /// committed of it. Then the source directories that waited for this flush are removed
    /// in the order they finished, if they are empty.
    pub(crate) fn finish_move(&mut self) {
        self.batch.last = true;
        self.flush();
        if self.batch.failed {
            // No source is unlinked after a failed syncfs; the destinations are committed.
            let left: Vec<Pending> = std::mem::take(&mut self.batch.entries)
                .into_iter()
                .chain(
                    std::mem::take(&mut self.batch.deferred)
                        .into_values()
                        .flatten(),
                )
                .collect();
            for p in left {
                self.report.fail(
                    p.path,
                    "the job stopped at a failed syncfs before this entry was settled; both kept",
                );
            }
        }
        for d in std::mem::take(&mut self.batch.later) {
            self.remove_source_dir(&d.parent, &d.name, d.id, &d.path);
        }
    }

    /// After a source directory's children: flush, then `rmdir` it if it is empty
    /// (design 4.8 step 6). A directory that still holds entries stays and is reported. A
    /// directory that still holds a deferred name, or a directory waiting for one, waits
    /// for the final flush (P2 9.2).
    pub(crate) fn finish_source_dir(&mut self, src: &Dir, node: &Node, spath: &Path) -> Flow {
        if self.flush() == Flow::Stop {
            return Flow::Stop;
        }
        let id = node.meta.id.inode();
        if self.batch.holds.contains_key(&id) {
            self.batch.hold(src.meta.id.inode());
            self.batch.later.push(LaterDir {
                parent: src.clone(),
                name: node.name.clone(),
                id,
                path: spath.to_path_buf(),
            });
            return Flow::Continue;
        }
        self.remove_source_dir(src, &node.name, id, spath);
        Flow::Continue
    }

    /// `rmdir` of a finished source directory if it is still the one the job emptied.
    fn remove_source_dir(&mut self, parent: &Dir, name: &OsStr, id: (u64, u64), spath: &Path) {
        // Remove only the directory the job emptied, not a replacement under its name.
        match self.sys.stat_at("move.dirstat", parent.fd(), name) {
            Ok(m) if m.kind == Kind::Dir && m.id.inode() == id => {}
            Ok(_) => {
                self.report.notes.push(format!(
                    "{}: replaced during the move; kept",
                    spath.display()
                ));
                return;
            }
            Err(e) => {
                self.report.notes.push(format!(
                    "{}: source directory not removed: {}",
                    spath.display(),
                    errno_text(e)
                ));
                return;
            }
        }
        match self.sys.rmdir("move.rmdir", parent.fd(), name) {
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
    }

    /// A whole planned subtree moved by one `rename`.
    fn moved(&mut self, node: &Node) {
        let (entries, bytes) = subtree_counts(node);
        self.report.done += entries;
        self.report.dirs_done += count_dirs(node);
        self.settle(entries);
        self.bytes_done += bytes;
        self.links.settle_tree(node);
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
    move_groups(sys, ui, &[Group::new(src_dir, names.to_vec())], dst)
}

/// F6 and Shift+F6 over groups (P2 2.2): plan every group, then move each selected entry
/// with one [`Transfer`]; the final flush also runs after a cancel.
pub fn move_groups(sys: &Sys, ui: &mut dyn Interaction, groups: &[Group], dst: &Path) -> Report {
    let (mut t, dst, parts) = match prepare(sys, ui, Verb::Move, groups, dst) {
        Ok(x) => x,
        Err(r) => return *r,
    };
    'job: for Part { src, plan, targets } in parts {
        for (node, target) in plan.roots.iter().zip(targets) {
            if move_entry(&mut t, &src, node, &dst, target) == Flow::Stop {
                break 'job;
            }
        }
    }
    // Job end and cancel both complete the batch in progress.
    t.finish_move();
    t.link_note();
    t.report
}
