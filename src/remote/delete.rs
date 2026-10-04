#![forbid(unsafe_code)]
//! Shift+F8 on a server (R-5, P3 5.6): there is no remote trash, so F8 is refused, and
//! Shift+F8 keeps M1's typed `delete` confirmation (M1 4.11).
//!
//! The scan is a download's ([`Walk`]): each selected name is `LSTAT`ed, every directory is
//! `LSTAT`ed before its `OPENDIR`, and a symlink is never descended (R-3). The confirmation
//! shows its counts of files, directories and bytes. Then files and symlinks go with
//! `REMOVE` (a symlink is removed as a link, its target untouched) and directories with
//! `RMDIR`, in post-order. A directory is entered again only after an `LSTAT` shows that it
//! still is one; SFTP is path-based, so a swap between that `LSTAT` and a request below it
//! goes undetected (R-3; the F1 help states it). Read-only directories are not changed to
//! force a removal; the entry fails with the server's error.
//!
//! Failpoints: `remote.lstat`, `remote.remove`, `remote.rmdir`.

use super::proto::{Attrs, status};
use super::provider::{RemoteProvider, join, location};
use super::session::{Session, SftpError};
use super::step;
use super::tree::{LOST, Walk};
use crate::fsops::copy::{Flow, Transfer};
use crate::fsops::group::{Group, NOT_LOCAL, Root, validate};
use crate::fsops::job::{JobVerb, Report};
use crate::fsops::plan::{Node, Totals};
use crate::fsops::question::{Answer, Interaction, Phase, Progress, Question, Reporter};
use crate::fsops::sys::{Kind, Sys};
use crate::provider::VPath;
use std::collections::HashSet;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// F8 on a server (R-5).
pub const NO_TRASH: &str = "no trash on the server; Shift+F8 deletes permanently";

const S_IFDIR: u32 = 0o040_000;

/// What the error question decided.
enum Ask {
    Retry,
    Fail(String),
    Stop,
}

struct Deleter<'a, 'u> {
    t: Transfer<'a, 'u>,
    s: &'a Session,
    cancel: Arc<AtomicBool>,
    lost: bool,
    skip_server: HashSet<String>,
}

impl Deleter<'_, '_> {
    fn lstat(&mut self, path: &[u8]) -> Result<Option<Attrs>, SftpError> {
        step(self.t.sys, "remote.lstat")?;
        match self.s.lstat(path, &self.cancel) {
            Ok(a) => Ok(Some(a)),
            Err(SftpError::Status {
                code: status::NO_SUCH_FILE,
                ..
            }) => Ok(None),
            Err(e) => Err(e),
        }
    }

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

    /// Removes one planned entry of the server directory `dir` in post-order.
    fn remove(&mut self, dir: &[u8], shown_dir: &Path, node: &Node) -> Flow {
        if self.t.stopped() || self.t.cancelled() {
            return self.t.stop();
        }
        let path = join(dir, node.name.as_bytes());
        let shown = shown_dir.join(&node.name);
        if let Some(note) = &node.note {
            self.t.noted(node, shown, note);
            return Flow::Continue;
        }
        if self.lost {
            self.t.fail(node, shown, LOST);
            return Flow::Continue;
        }
        self.t.set_current(shown.clone());
        let sys = self.t.sys;
        if node.meta.kind == Kind::Dir {
            // R-3: entered only while it still is a directory, never through a symlink.
            loop {
                match self.lstat(&path) {
                    Ok(Some(a)) if a.kind() == Some(S_IFDIR) => break,
                    Ok(Some(_)) => {
                        self.t.fail(node, shown, "type changed");
                        return Flow::Continue;
                    }
                    Ok(None) => {
                        self.t.fail(node, shown, "disappeared");
                        return Flow::Continue;
                    }
                    Err(e) => match self.ask_server(&shown, "stat", &e) {
                        Ask::Retry => {}
                        Ask::Fail(why) => {
                            self.t.fail(node, shown, why);
                            return Flow::Continue;
                        }
                        Ask::Stop => return self.t.stop(),
                    },
                }
            }
            for child in &node.children {
                if self.remove(&path, &shown, child) == Flow::Stop {
                    return Flow::Stop;
                }
            }
            if self.lost {
                self.t.report.fail(shown, LOST);
                return Flow::Continue;
            }
            loop {
                let r =
                    step(sys, "remote.rmdir").and_then(|()| self.s.firm(&self.cancel).rmdir(&path));
                match r {
                    Ok(()) => {
                        self.t.done(node);
                        return Flow::Continue;
                    }
                    Err(e) => match self.ask_server(&shown, "remove directory", &e) {
                        Ask::Retry => {}
                        // The children are settled; the directory itself is reported.
                        Ask::Fail(why) => {
                            self.t.report.fail(shown, why);
                            return Flow::Continue;
                        }
                        Ask::Stop => return self.t.stop(),
                    },
                }
            }
        }
        loop {
            let r =
                step(sys, "remote.remove").and_then(|()| self.s.firm(&self.cancel).remove(&path));
            match r {
                Ok(()) => {
                    self.t.done(node);
                    return Flow::Continue;
                }
                Err(SftpError::Status {
                    code: status::NO_SUCH_FILE,
                    ..
                }) => {
                    self.t.fail(node, shown, "disappeared");
                    return Flow::Continue;
                }
                Err(e) => match self.ask_server(&shown, "delete", &e) {
                    Ask::Retry => {}
                    Ask::Fail(why) => {
                        self.t.fail(node, shown, why);
                        return Flow::Continue;
                    }
                    Ask::Stop => return self.t.stop(),
                },
            }
        }
    }
}

/// Shift+F8 over groups on one server (R-5, M1 4.11): the scan, the typed confirmation with
/// its counts, then the removal in post-order.
pub fn delete_groups(sys: &Sys, ui: &mut dyn Interaction, groups: &[Group]) -> Report {
    let verb = JobVerb::Delete;
    if let Err(why) = validate(groups) {
        return Report::refused(verb, why);
    }
    let Some(Root::Remote(remote)) = groups.first().map(|g| &g.root) else {
        return Report::refused(verb, NOT_LOCAL);
    };
    let ours = |g: &Group| matches!(&g.root, Root::Remote(r) if Arc::ptr_eq(r, remote));
    if !groups.iter().all(ours) {
        return Report::refused(verb, NOT_LOCAL);
    }
    let remote: &Arc<RemoteProvider> = remote;
    let s = remote.session();
    let cancel = sys.cancel_flag().clone();
    let shown = |p: &VPath| PathBuf::from(OsString::from_vec(location(remote.target(), p)));
    let dirs: Vec<VPath> = groups
        .iter()
        .map(|g| VPath::new(g.sub.clone()).unwrap_or_default())
        .collect();
    let mut rep = Reporter::new(ui);
    // The scan (R-3).
    let (plans, totals) = {
        let current = dirs.first().map(shown).unwrap_or_default();
        let mut tick = |t: Totals| {
            rep.progress(|| Progress {
                phase: Phase::Scanning,
                files_done: 0,
                files_total: t.entries(),
                bytes_done: 0,
                bytes_total: t.bytes,
                current: current.clone(),
            });
        };
        let mut w = Walk::new(s, remote.id(), &cancel);
        w.tick = Some(&mut tick);
        w.read_targets = false;
        let mut roots = Vec::with_capacity(groups.len());
        for (g, dir) in groups.iter().zip(&dirs) {
            match w.roots(dir, &g.names) {
                Ok(r) => roots.push(r),
                Err(_) => {
                    let mut r = Report::new(verb);
                    r.notes.push("nothing was deleted".into());
                    r.cancelled = true;
                    return r;
                }
            }
        }
        if w.walk().is_err() {
            let mut r = Report::new(verb);
            r.notes.push("nothing was deleted".into());
            r.cancelled = true;
            return r;
        }
        if w.lost {
            let at = dirs.first().map(shown).unwrap_or_default();
            return Report::refused(verb, format!("{}: {LOST}", at.display()));
        }
        let mut totals = Totals::default();
        let plans: Vec<Vec<Node>> = roots
            .iter()
            .map(|r| r.iter().map(|&i| w.node(i, &mut totals)).collect())
            .collect();
        (plans, totals)
    };
    let q = Question::ConfirmDelete {
        files: totals.entries(),
        dirs: totals.dirs,
        bytes: totals.bytes,
        single: None,
    };
    let mut t = Transfer::new(sys, rep, Report::new(verb));
    if t.rep.ask(q) != Answer::Confirm {
        t.report.notes.push("nothing was deleted".into());
        t.report.cancelled = true;
        return t.report;
    }
    t.set_sum(totals);
    let mut d = Deleter {
        t,
        s,
        cancel,
        lost: false,
        skip_server: HashSet::new(),
    };
    'job: for (dir, nodes) in dirs.iter().zip(&plans) {
        let path = dir.to_bytes();
        let at = shown(dir);
        for n in nodes {
            if d.remove(&path, &at, n) == Flow::Stop {
                break 'job;
            }
        }
    }
    d.t.report
}
