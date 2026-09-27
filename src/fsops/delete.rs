#![forbid(unsafe_code)]
//! Permanent delete (Shift+F8, design 4.11).
//!
//! Confirm, plan (counts of files, directories and bytes), then the typed `delete`
//! confirmation: only [`Answer::Confirm`] deletes. Traversal follows design 4.3 and the
//! identity table of 4.2: symlinks are unlinked, never followed; a different `mnt_id` is
//! skipped as a mount point; a btrfs subvolume is descended. Files and directories are
//! removed in post-order with `unlinkat`. Read-only directories are not `chmod`ed to force
//! deletion; the entry fails with the OS error.

use super::copy::{Dir, Flow, Transfer};
use super::job::{JobVerb, Report};
use super::plan::{Node, Note, Refusal, Scan, Verb, scan};
use super::question::{Answer, Interaction, Question, Reporter};
use super::sys::{Kind, Sys};
use super::walk::{EntryError, open_child_dir};
use rustix::io::Errno;
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

pub fn delete_job(sys: &Sys, ui: &mut dyn Interaction, dir: &Path, names: &[OsString]) -> Report {
    let verb = JobVerb::Delete;
    let src = match Dir::open_root(sys, dir) {
        Ok(d) => d,
        Err(e) => return Report::refused(verb, format!("{}: {e}", dir.display())),
    };
    let rep = Reporter::new(ui);
    let mut t = Transfer::new(sys, rep, Report::new(verb));
    confirm_and_remove(&mut t, &src, names, false);
    t.report
}

/// Plans `names` in `src`, asks the typed confirmation with the counts, and removes them
/// when it is confirmed. `single` marks the one-entry confirmation after a failed trash.
pub(crate) fn confirm_and_remove(
    t: &mut Transfer,
    src: &Dir,
    names: &[OsString],
    single: bool,
) -> Flow {
    let plan = match scan(
        &Scan {
            sys: t.sys,
            verb: Verb::Delete,
            src: src.fd(),
            src_path: &src.path,
            names,
            dst: None,
        },
        &mut t.rep,
    ) {
        Ok(p) => p,
        Err(Refusal::Cancelled) => return t.stop(),
        Err(e) => {
            t.report.refused = Some(e.to_string());
            return Flow::Stop;
        }
    };
    let q = Question::ConfirmDelete {
        files: plan.totals.entries(),
        dirs: plan.totals.dirs,
        bytes: plan.totals.bytes,
        single: single.then(|| src.path.join(&names[0])),
    };
    if t.rep.ask(q) != Answer::Confirm {
        if single {
            return Flow::Continue;
        }
        t.report.notes.push("nothing was deleted".into());
        t.report.cancelled = true;
        return Flow::Stop;
    }
    if !single {
        t.set_totals(&plan);
    } else {
        t.files_total += plan.totals.entries();
        t.bytes_total += plan.totals.bytes;
    }
    for node in &plan.roots {
        if remove(t, src, node) == Flow::Stop {
            return Flow::Stop;
        }
    }
    Flow::Continue
}

/// Removes one planned entry in post-order.
pub(crate) fn remove(t: &mut Transfer, parent: &Dir, node: &Node) -> Flow {
    if t.stopped() || t.cancelled() {
        return t.stop();
    }
    let sys = t.sys;
    let spath = parent.path.join(&node.name);
    match &node.note {
        None => {}
        Some(Note::Failed(e)) => {
            t.fail(node, spath, e.to_string());
            return Flow::Continue;
        }
        Some(n) => {
            t.skip(node, spath, n.reason());
            return Flow::Continue;
        }
    }
    if node.meta.kind == Kind::Dir {
        let (fd, meta) = loop {
            match open_child_dir(sys, "walk.openat", parent.fd(), &node.name, &node.meta.id) {
                Ok(x) => break x,
                Err(EntryError::Os { op, errno }) => match t.decide_error(&spath, op, errno) {
                    Some(true) => continue,
                    Some(false) => {
                        t.fail(node, spath, EntryError::Os { op, errno }.to_string());
                        return Flow::Continue;
                    }
                    None => return t.stop(),
                },
                Err(e) => {
                    t.fail(node, spath, e.to_string());
                    return Flow::Continue;
                }
            }
        };
        let dir = Dir {
            fd: Arc::new(fd),
            meta,
            path: spath.clone(),
        };
        for child in &node.children {
            if remove(t, &dir, child) == Flow::Stop {
                return Flow::Stop;
            }
        }
        loop {
            match sys.rmdir("delete.rmdir", parent.fd(), &node.name) {
                Ok(()) => {
                    t.done(node);
                    return Flow::Continue;
                }
                Err(Errno::NOTEMPTY | Errno::EXIST) => {
                    t.report.fail(spath, "not removed: it still holds entries");
                    return Flow::Continue;
                }
                Err(e) => match t.decide_error(&spath, "remove directory", e) {
                    Some(true) => continue,
                    Some(false) => {
                        t.report
                            .fail(spath, EntryError::os("remove directory", e).to_string());
                        return Flow::Continue;
                    }
                    None => return t.stop(),
                },
            }
        }
    }
    loop {
        // The plan can be stale: act only on the planned inode.
        match sys.stat_at("delete.stat", parent.fd(), &node.name) {
            Ok(m) if m.id.inode() == node.meta.id.inode() && m.kind == node.meta.kind => {}
            Ok(_) => {
                t.fail(node, spath, EntryError::TypeChanged.to_string());
                return Flow::Continue;
            }
            Err(Errno::NOENT) => {
                t.fail(node, spath, EntryError::Disappeared.to_string());
                return Flow::Continue;
            }
            Err(e) => match t.decide_error(&spath, "stat", e) {
                Some(true) => continue,
                Some(false) => {
                    t.fail(node, spath, EntryError::os("stat", e).to_string());
                    return Flow::Continue;
                }
                None => return t.stop(),
            },
        }
        match sys.unlink("delete.unlink", parent.fd(), &node.name) {
            Ok(()) => {
                t.done(node);
                return Flow::Continue;
            }
            Err(e) => match t.decide_error(&spath, "delete", e) {
                Some(true) => continue,
                Some(false) => {
                    t.fail(node, spath, EntryError::os("delete", e).to_string());
                    return Flow::Continue;
                }
                None => return t.stop(),
            },
        }
    }
}
