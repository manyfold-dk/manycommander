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
use super::group::{Group, OpenGroup};
use super::job::{JobVerb, Report};
use super::plan::{Node, Note, Refusal, Scan, Verb, scan_all};
use super::question::{Answer, Interaction, Question, Reporter};
use super::sys::{Kind, Sys};
use super::walk::{EntryError, open_child_dir};
use rustix::io::Errno;
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

pub fn delete_job(sys: &Sys, ui: &mut dyn Interaction, dir: &Path, names: &[OsString]) -> Report {
    delete_groups(sys, ui, &[Group::new(dir, names.to_vec())])
}

/// Shift+F8 over groups (P2 2.2): groups that reach the same directory are merged, every
/// group is planned, and the typed confirmation is asked once, with the totals over all
/// of them.
pub fn delete_groups(sys: &Sys, ui: &mut dyn Interaction, groups: &[Group]) -> Report {
    let verb = JobVerb::Delete;
    let mut opened = match super::group::open(sys, verb, groups) {
        Ok(o) => o,
        Err(r) => return *r,
    };
    opened.merge();
    let rep = Reporter::new(ui);
    let mut t = Transfer::new(sys, rep, Report::new(verb));
    opened.report_failed(&mut t.report);
    // Nothing opened: nothing to confirm.
    if !opened.sources.is_empty() {
        confirm_and_remove(&mut t, &opened.sources, false);
    }
    t.report
}

/// Plans the sources, asks the typed confirmation once with the counts over all of them,
/// and removes them when it is confirmed. `single` marks the one-entry confirmation after
/// a failed trash.
pub(crate) fn confirm_and_remove(t: &mut Transfer, sources: &[OpenGroup], single: bool) -> Flow {
    let sys = t.sys;
    let scans: Vec<Scan> = sources
        .iter()
        .map(|s| Scan {
            sys,
            verb: Verb::Delete,
            src: s.dir.fd(),
            src_path: &s.dir.path,
            names: &s.names,
            dst: None,
        })
        .collect();
    let plans = match scan_all(&scans, &mut t.rep) {
        Ok(p) => p,
        Err(Refusal::Cancelled) => return t.stop(),
        Err(e) => {
            t.report.refused = Some(e.to_string());
            return Flow::Stop;
        }
    };
    let totals: super::plan::Totals = plans.iter().map(|p| p.totals).sum();
    let q = Question::ConfirmDelete {
        files: totals.entries(),
        dirs: totals.dirs,
        bytes: totals.bytes,
        single: single.then(|| sources[0].dir.path.join(&sources[0].names[0])),
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
        t.set_sum(totals);
    } else {
        t.files_total += totals.entries();
        t.bytes_total += totals.bytes;
    }
    for (s, plan) in sources.iter().zip(&plans) {
        for node in &plan.roots {
            if remove(t, &s.dir, node) == Flow::Stop {
                return Flow::Stop;
            }
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
            // Remove only the directory whose children were removed through the held fd.
            match sys.stat_at("delete.stat", parent.fd(), &node.name) {
                Ok(m) if m.kind == Kind::Dir && m.id.inode() == node.meta.id.inode() => {}
                Ok(_) => {
                    t.report.fail(spath, EntryError::TypeChanged.to_string());
                    return Flow::Continue;
                }
                Err(e) => {
                    t.report.fail(spath, EntryError::os("stat", e).to_string());
                    return Flow::Continue;
                }
            }
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
