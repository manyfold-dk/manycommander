#![forbid(unsafe_code)]
//! Renames and new directories on a server (TDR-3 5.6): F6 between two panels on one session,
//! Shift+F6, and F7.
//!
//! **A move on the server** (R-2) `LSTAT`s the new name first and raises TDR-1's question when
//! it is taken ("file exists", "directory exists", "type mismatch"); then it sends
//! `SSH_FXP_RENAME`. OpenSSH's `sftp-server` refuses a rename onto every existing name, a
//! file, an empty directory and a symlink alike; another server may replace, so a name that
//! appears between the `LSTAT` and the rename can be lost there (the F1 help states that
//! race). A rename that fails raises the question when an `LSTAT` now finds the name, and
//! the error question otherwise. Overwrite replaces only through
//! `posix-rename@openssh.com`, an atomic `rename(2)`; a server without it refuses with "the
//! server cannot replace a file atomically". Merge moves each entry of the directory with
//! the same rules, then removes the emptied source directory. Its listing stops at the
//! caps of a panel's (TDR-3 5.4); what it did not list stays, and the report says so. A merge
//! nested [`MAX_DEPTH`] levels deep fails with the reason. Nothing is copied: a rename on
//! one server moves the entry itself, whatever it holds.
//!
//! **F7** `LSTAT`s each component of the name and makes the missing ones with `MKDIR` (the
//! server's umask applies, as TDR-1's `0777` does). An existing name is reported, and the
//! cursor moves to it (TDR-1 4.9).
//!
//! Every request that changes the server waits for its reply even after a cancel
//! ([`Session::call_firm`]); a lost session fails what is left with "connection lost"
//! (I-7). Failpoints: `remote.lstat`, `remote.rename`, `remote.replace`, `remote.rmdir`,
//! `remote.mkdir`.

use super::proto::{Attrs, status};
use super::provider::{CAPPED, RemoteProvider, join, meta_of, read_names, shown, valid_name};
use super::session::{Session, SftpError};
use super::tree::{INVALID_NAME, LOST, Limits, MAX_DEPTH};
use super::{Ask, ServerErrors, step};
use crate::fsops::copy::{Decision, Flow, Transfer};
use crate::fsops::group::{Group, Root, validate};
use crate::fsops::job::{JobVerb, Report};
use crate::fsops::mkdir::components;
use crate::fsops::question::{Conflict, Interaction, Reporter, conflict};
use crate::fsops::sys::{Kind, Meta, Sys};
use crate::provider::{VPath, synthetic_id};
use crate::ui::text::escaped;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// What a copy or a move between two sessions, or between an archive and a server, says
/// (TDR-3 2.4).
pub const OTHER_SESSION: &str = "copy through a local directory";

/// What F5 within one session says: SFTP has no copy on the server (TDR-3 2.4).
pub const NO_SERVER_COPY: &str = "no copy on the server; copy through a local directory";

/// The rename engine of one job.
struct Mover<'a, 'u> {
    t: Transfer<'a, 'u>,
    s: &'a Session,
    remote: &'a RemoteProvider,
    cancel: Arc<AtomicBool>,
    posix_rename: bool,
    lost: bool,
    errors: ServerErrors,
    /// Merges in progress: each one recurses into the next.
    depth: u32,
}

impl Mover<'_, '_> {
    fn shown(&self, p: &[u8]) -> PathBuf {
        shown(self.remote.target(), p)
    }

    fn lstat(&mut self, path: &[u8]) -> Result<Option<Attrs>, SftpError> {
        let sys = self.t.sys;
        super::lstat(
            sys,
            "remote.lstat",
            self.s,
            path,
            &self.cancel,
            &mut self.lost,
        )
    }

    fn ask_server(&mut self, path: &std::path::Path, op: &'static str, e: &SftpError) -> Ask {
        self.errors
            .ask(&mut self.t.rep, &mut self.lost, path, op, e)
    }

    /// An entry moved as a whole: a file or a symlink counts one, a directory counts as a
    /// directory.
    fn moved(&mut self, kind: Kind) {
        if kind == Kind::Dir {
            self.t.report.dirs_done += 1;
        } else {
            self.t.report.done += 1;
            self.t.settle(1);
        }
        self.t.tick();
    }

    fn ended(&mut self, kind: Kind) {
        if kind != Kind::Dir {
            self.t.settle(1);
        }
    }

    fn skip(&mut self, kind: Kind, path: PathBuf, why: impl Into<String>) {
        self.ended(kind);
        self.t.report.skip(path, why);
        self.t.tick();
    }

    fn fail(&mut self, kind: Kind, path: PathBuf, why: impl Into<String>) {
        self.ended(kind);
        self.t.report.fail(path, why);
        self.t.tick();
    }

    /// Moves `from` (of metadata `m`) to `dir/target` (R-2, TDR-3 5.6).
    fn entry(&mut self, from: &[u8], m: &Meta, dir: &[u8], mut target: OsString) -> Flow {
        let src_shown = self.shown(from);
        loop {
            if self.t.stopped() || self.t.cancelled() {
                return self.t.stop();
            }
            if self.lost {
                self.fail(m.kind, src_shown, LOST);
                return Flow::Continue;
            }
            let to = join(dir, target.as_bytes());
            let shown = self.shown(&to);
            if to == from {
                self.skip(
                    m.kind,
                    src_shown,
                    "source and destination are the same file",
                );
                return Flow::Continue;
            }
            // A directory into itself: the server's `rename(2)` refuses it too, but a
            // lexical check says why.
            if m.kind == Kind::Dir && to.starts_with(from) && to.get(from.len()) == Some(&b'/') {
                self.fail(
                    m.kind,
                    src_shown,
                    "the destination is inside the source; nothing was moved",
                );
                return Flow::Continue;
            }
            self.t.set_current(src_shown.clone());
            let taken = match self.lstat(&to) {
                Ok(a) => a,
                Err(e) => match self.ask_server(&shown, "stat destination", &e) {
                    Ask::Retry => continue,
                    Ask::Fail(why) => {
                        self.fail(m.kind, src_shown, why);
                        return Flow::Continue;
                    }
                    Ask::Stop => return self.t.stop(),
                },
            };
            if let Some(a) = taken {
                let dm = meta_of(&a, synthetic_id(self.remote.id(), 0));
                let d = match conflict(m.kind, dm.kind) {
                    Conflict::FileExists => {
                        let res = || 1_000_000_000;
                        self.t.decide_exists_res(m, &dm, &res, &shown)
                    }
                    Conflict::DirExists => self.t.decide_dir_exists(m, &dm, &shown),
                    Conflict::TypeMismatch => self.t.decide_mismatch(m, &dm, &shown),
                };
                match d {
                    Decision::Overwrite => {
                        if !self.posix_rename {
                            self.fail(m.kind, src_shown, super::put::NO_ATOMIC_REPLACE);
                            return Flow::Continue;
                        }
                        let r = step(self.t.sys, "remote.replace")
                            .and_then(|()| self.s.firm(&self.cancel).posix_rename(from, &to));
                        match r {
                            Ok(()) => {
                                self.moved(m.kind);
                                return Flow::Continue;
                            }
                            Err(e) => match self.ask_server(&shown, "replace", &e) {
                                Ask::Retry => continue,
                                Ask::Fail(why) => {
                                    self.fail(m.kind, src_shown, why);
                                    return Flow::Continue;
                                }
                                Ask::Stop => return self.t.stop(),
                            },
                        }
                    }
                    Decision::Merge => return self.merge(from, &to),
                    Decision::Rename(n) => {
                        target = n;
                        continue;
                    }
                    Decision::Skip(why) => {
                        self.skip(m.kind, src_shown, why);
                        return Flow::Continue;
                    }
                    Decision::Cancel => return self.t.stop(),
                }
            }
            let r = step(self.t.sys, "remote.rename")
                .and_then(|()| self.s.firm(&self.cancel).rename(from, &to));
            let e = match r {
                Ok(()) => {
                    self.moved(m.kind);
                    return Flow::Continue;
                }
                Err(e @ SftpError::Status { .. }) => match self.lstat(&to) {
                    // The name appeared since the `LSTAT`: the question, on the next turn.
                    Ok(Some(_)) => continue,
                    Ok(None) => e,
                    Err(e) => e,
                },
                Err(e) => e,
            };
            match self.ask_server(&src_shown, "rename", &e) {
                Ask::Retry => {}
                Ask::Fail(why) => {
                    self.fail(m.kind, src_shown, why);
                    return Flow::Continue;
                }
                Ask::Stop => return self.t.stop(),
            }
        }
    }

    /// "Directory exists", Merge (TDR-1 4.8): each entry of `from` moves into `to` with the
    /// same rules, then `from` goes when it is empty. `from` must still be a directory: a
    /// directory is never entered through a symlink (R-3). A merge nested [`MAX_DEPTH`]
    /// levels deep fails: each one recurses into the next.
    fn merge(&mut self, from: &[u8], to: &[u8]) -> Flow {
        let src_shown = self.shown(from);
        if self.depth >= MAX_DEPTH {
            self.t
                .report
                .fail(src_shown, Limits::default().depth_message());
            return Flow::Continue;
        }
        let (entries, capped) = match self.lstat(from) {
            Ok(Some(a)) if a.is_dir() => match list(self.s, from, &self.cancel) {
                Ok(v) => v,
                Err(e) => {
                    let why = match e {
                        SftpError::Lost => {
                            self.lost = true;
                            LOST.to_string()
                        }
                        SftpError::Cancelled => return self.t.stop(),
                        e => format!("read directory: {e}"),
                    };
                    self.t.report.fail(src_shown, why);
                    return Flow::Continue;
                }
            },
            Ok(_) => {
                self.t.report.fail(src_shown, "type changed");
                return Flow::Continue;
            }
            Err(SftpError::Cancelled) => return self.t.stop(),
            Err(e) => {
                self.t.report.fail(src_shown, e.to_string());
                return Flow::Continue;
            }
        };
        if capped {
            self.t
                .report
                .notes
                .push(format!("{}: {CAPPED}", src_shown.display()));
        }
        self.depth += 1;
        let flow = self.merge_entries(from, to, entries);
        self.depth -= 1;
        if flow == Flow::Stop {
            return Flow::Stop;
        }
        if self.lost {
            return Flow::Continue;
        }
        let r =
            step(self.t.sys, "remote.rmdir").and_then(|()| self.s.firm(&self.cancel).rmdir(from));
        match r {
            Ok(()) => self.t.report.dirs_done += 1,
            Err(SftpError::Lost) => {
                self.lost = true;
                self.t
                    .report
                    .notes
                    .push(format!("{}: {LOST}", src_shown.display()));
            }
            Err(_) => self.t.report.notes.push(format!(
                "{}: kept, it still holds entries that were not moved",
                src_shown.display()
            )),
        }
        Flow::Continue
    }

    /// Moves each listed entry of `from` into `to`.
    fn merge_entries(&mut self, from: &[u8], to: &[u8], entries: Vec<(Vec<u8>, Attrs)>) -> Flow {
        for (name, a) in entries {
            let child = join(from, &name);
            if !valid_name(&name) {
                self.t
                    .report
                    .skip(self.shown(&child), INVALID_NAME.to_string());
                continue;
            }
            let m = meta_of(&a, synthetic_id(self.remote.id(), 0));
            if m.kind != Kind::Dir {
                self.t.report.planned += 1;
            }
            let target = OsString::from_vec(name);
            if self.entry(&child, &m, to, target) == Flow::Stop {
                return Flow::Stop;
            }
        }
        Flow::Continue
    }
}

/// A directory's names and attributes, and whether its listing stopped at a cap.
type Listed = (Vec<(Vec<u8>, Attrs)>, bool);

/// The names and attributes of the directory `path` on the server, without `.` and `..`;
/// the listing stops at a panel's caps (TDR-3 5.4).
fn list(s: &Session, path: &[u8], cancel: &AtomicBool) -> Result<Listed, SftpError> {
    let mut out = Vec::new();
    let capped = read_names(s, path, cancel, &mut |names| {
        out.extend(names.into_iter().map(|n| (n.filename, n.attrs)));
    })?;
    Ok((out, capped))
}

/// The session of a job's groups, when they all are on one: every group must be a directory
/// of the server `remote` (TDR-3 2.2).
fn same_session(groups: &[Group], remote: &Arc<RemoteProvider>) -> bool {
    groups
        .iter()
        .all(|g| matches!(&g.root, Root::Remote(r) if Arc::ptr_eq(r, remote)))
}

/// F6 between two panels on one session, and Shift+F6 on a server (TDR-3 5.6): `dst` is what
/// the user confirmed, an existing directory on the server to move into or, for a single
/// source name in total, the new path.
pub fn move_on_server(
    sys: &Sys,
    ui: &mut dyn Interaction,
    groups: &[Group],
    remote: &Arc<RemoteProvider>,
    dst: &VPath,
) -> Report {
    let verb = JobVerb::Move;
    if let Err(why) = validate(groups) {
        return Report::refused(verb, why);
    }
    if !same_session(groups, remote) {
        return Report::refused(verb, OTHER_SESSION);
    }
    let s = remote.session();
    let cancel = sys.cancel_flag().clone();
    let t = remote.target();
    if s.lost().is_some() {
        return Report::refused(verb, format!("{}: {LOST}", shown(t, b"").display()));
    }
    let names: Vec<OsString> = groups.iter().flat_map(|g| g.names.clone()).collect();
    // TDR-1's destination rule: an existing directory, or a new path for one name.
    let stat_dir = |p: &VPath| match s.stat(&p.to_bytes(), &cancel) {
        Ok(a) => Ok(a.is_dir()),
        Err(SftpError::Status {
            code: status::NO_SUCH_FILE,
            ..
        }) => Ok(false),
        Err(e) => Err(format!("{}: {e}", shown(t, &p.to_bytes()).display())),
    };
    let (dir, targets) = match stat_dir(dst) {
        Err(why) => return Report::refused(verb, why),
        Ok(true) => (dst.to_bytes(), names.clone()),
        Ok(false) if names.len() == 1 => {
            let (Some(parent), Some(name)) = (dst.parent(), dst.name()) else {
                return Report::refused(verb, "not a valid destination");
            };
            match stat_dir(&parent) {
                Ok(true) => (parent.to_bytes(), vec![name.to_owned()]),
                Ok(false) => {
                    return Report::refused(
                        verb,
                        format!(
                            "{}: no such directory",
                            shown(t, &parent.to_bytes()).display()
                        ),
                    );
                }
                Err(why) => return Report::refused(verb, why),
            }
        }
        Ok(false) => {
            return Report::refused(
                verb,
                format!("{}: no such directory", shown(t, &dst.to_bytes()).display()),
            );
        }
    };
    let mut m = Mover {
        t: Transfer::new(sys, Reporter::new(ui), Report::new(verb)),
        s,
        remote,
        cancel: cancel.clone(),
        posix_rename: s.caps().posix_rename,
        lost: false,
        errors: ServerErrors::default(),
        depth: 0,
    };
    // Each selected name, `LSTAT`ed: its kind decides the question and the count.
    let mut todo = Vec::new();
    let mut targets = targets.into_iter();
    for g in groups {
        let base = VPath::new(g.sub.clone()).unwrap_or_default().to_bytes();
        for n in &g.names {
            let target = targets.next().unwrap_or_else(|| n.clone());
            todo.push((join(&base, n.as_bytes()), target));
        }
    }
    let mut planned = Vec::with_capacity(todo.len());
    for (from, target) in todo {
        let shown_from = m.shown(&from);
        match m.lstat(&from) {
            Ok(Some(a)) => {
                let meta = meta_of(&a, synthetic_id(remote.id(), 0));
                if meta.kind != Kind::Dir {
                    m.t.report.planned += 1;
                }
                planned.push((from, Some(meta), target));
            }
            Ok(None) => {
                m.t.report.planned += 1;
                m.t.settle(1);
                m.t.report.fail(shown_from, "disappeared");
                planned.push((from, None, target));
            }
            Err(SftpError::Cancelled) => {
                m.t.report.cancelled = true;
                return m.t.report;
            }
            Err(e) => {
                m.t.report.planned += 1;
                m.t.settle(1);
                m.t.report.fail(shown_from, format!("stat: {e}"));
                planned.push((from, None, target));
            }
        }
    }
    for (from, meta, target) in planned {
        let Some(meta) = meta else {
            continue;
        };
        if m.entry(&from, &meta, &dir, target) == Flow::Stop {
            break;
        }
    }
    m.t.report
}

/// F7 on a server (TDR-3 5.6, TDR-1 4.9): `name` may hold `/` and makes the missing components
/// below `dir`.
pub fn mkdir(sys: &Sys, remote: &Arc<RemoteProvider>, dir: &VPath, name: &OsStr) -> Report {
    let verb = JobVerb::Mkdir;
    let Some(comps) = components(name) else {
        return Report::refused(verb, "not a valid directory name");
    };
    let s = remote.session();
    let cancel = sys.cancel_flag().clone();
    let t = remote.target();
    let mut report = Report::new(verb);
    report.planned = 1;
    report.focus = Some(comps[0].clone());
    let path = comps
        .iter()
        .fold(dir.to_bytes(), |p, c| join(&p, c.as_bytes()));
    let whole = shown(t, &path);
    let mut at = dir.to_bytes();
    for (i, comp) in comps.iter().enumerate() {
        let last = i + 1 == comps.len();
        at = join(&at, comp.as_bytes());
        let lstat = step(sys, "remote.lstat").and_then(|()| s.lstat(&at, &cancel));
        match lstat {
            Ok(a) if a.is_dir() => {
                if last {
                    report.skip(whole.clone(), "already exists");
                }
                continue;
            }
            Ok(_) => {
                report.fail(
                    whole.clone(),
                    format!("{} exists and is not a directory", escaped(comp.as_bytes())),
                );
                break;
            }
            Err(SftpError::Status {
                code: status::NO_SUCH_FILE,
                ..
            }) => {}
            Err(e) => {
                report.fail(whole.clone(), format!("stat: {e}"));
                break;
            }
        }
        let r =
            step(sys, "remote.mkdir").and_then(|()| s.firm(&cancel).mkdir(&at, Attrs::default()));
        match r {
            Ok(()) => {
                if last {
                    report.done = 1;
                }
            }
            Err(e @ SftpError::Status { .. }) => {
                // The name appeared since the `LSTAT`: reported as TDR-1 reports it.
                let now = s.lstat(&at, &cancel);
                match now {
                    Ok(a) if a.is_dir() && last => {
                        report.skip(whole.clone(), "already exists");
                    }
                    Ok(a) if a.is_dir() => continue,
                    _ => {
                        report.fail(whole.clone(), format!("make directory: {e}"));
                        break;
                    }
                }
            }
            Err(e) => {
                report.fail(whole.clone(), format!("make directory: {e}"));
                break;
            }
        }
    }
    report.settled = 1;
    report
}
