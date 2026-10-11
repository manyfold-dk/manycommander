#![forbid(unsafe_code)]
//! Job wiring (TDR-1 3.1, 4.4): the channel-backed [`Interaction`] and the worker thread.
//! One job runs at a time. The worker runs under `catch_unwind` (`run_guarded`), so a
//! panic ends as a failed report.
//!
//! The app's side of a job (TDR-1 4.4): starting it, its progress, questions and report, and
//! its status row. Quitting while it runs asks first, then cancels the job and quits when the
//! worker returns; a second request or a signal does so without asking (E-23).
//!
//! The verbs by place (TDR-3 2.4): [`refusal`] refuses a verb that an archive or a server
//! does not allow, before any work, with a status-line message, as TDR-2's "not in search
//! results".

use super::event::{Effect, Event, JobEvent};
use super::keys::Action;
use super::{App, JobUi};
use crate::fsops::job::{JobSpec, JobVerb, Report, run_guarded};
use crate::fsops::question::{Answer, Interaction, Phase, Progress, Question};
use crate::fsops::sys::Sys;
use crate::panel::{Panel, Source};
use crate::ui::dialog::{Dialog, Purpose};
use std::os::unix::ffi::OsStrExt;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

/// Where a verb acts, as the table of TDR-3 2.4 sees a panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
    /// A local directory (TDR-1).
    Local,
    /// A results tab (TDR-2 5); its entries are local.
    Results,
    /// An archive (TDR-3 3).
    Archive,
    /// A server; the value tells sessions apart (the session's number), so a copy within
    /// one session and one across sessions differ (TDR-3 2.4).
    Remote(u64),
}

impl At {
    pub fn of(p: &Panel) -> At {
        match &p.source {
            Source::Dir => At::Local,
            Source::Results(_) => At::Results,
            Source::Archive(_) => At::Archive,
            Source::Remote(v) => At::Remote(v.session.id()),
        }
    }

    fn local(self) -> bool {
        matches!(self, At::Local | At::Results)
    }
}

/// An archive is never written (A-2).
pub const READ_ONLY: &str = crate::archive::extract::READ_ONLY;
/// F8 on a server (R-5).
pub const NO_REMOTE_TRASH: &str = crate::remote::delete::NO_TRASH;
/// F5 within one session (TDR-3 2.4).
pub const NO_SERVER_COPY: &str = crate::remote::rename::NO_SERVER_COPY;
/// A copy or move between two sessions, or between an archive and a server (TDR-3 2.4).
pub const THROUGH_LOCAL: &str = crate::remote::rename::OTHER_SESSION;
/// A verb that needs local files, in an archive (TDR-3 2.4).
pub const NOT_IN_ARCHIVE: &str = "not in an archive";
/// A verb that needs local files, on a server (TDR-3 2.4).
pub const NOT_ON_SERVER: &str = "not on a server";

/// The refusal of `a` in the active panel `here`, with `there` the other panel, the copy
/// and move destination (TDR-3 2.4). `Some` is the status-line message; the verb does nothing.
/// Local directories and results tabs are never refused here: a results tab's own
/// refusals are TDR-2's.
pub fn refusal(a: Action, here: At, there: At) -> Option<&'static str> {
    match a {
        Action::Copy | Action::Move => transfer_refusal(a == Action::Move, here, there),
        // A link is made in the other panel and points into this one (TDR-2 8.1).
        Action::Link => match (here, there) {
            (At::Archive, _) => Some(NOT_IN_ARCHIVE),
            (At::Remote(_), _) => Some(NOT_ON_SERVER),
            (_, At::Archive) => Some(READ_ONLY),
            (_, At::Remote(_)) => Some(NOT_ON_SERVER),
            _ => None,
        },
        _ => match here {
            At::Local | At::Results => None,
            At::Archive => match a {
                Action::Mkdir
                | Action::Rename
                | Action::Trash
                | Action::Delete
                | Action::EditNew
                | Action::Attributes
                | Action::MultiRename => Some(READ_ONLY),
                Action::Find | Action::InsertPath => Some(NOT_IN_ARCHIVE),
                // A member of an archive is not opened as an archive (TDR-3 3.1).
                Action::OpenArchive => Some(crate::archive::NESTED),
                // Browsing, and F3, F4 and `Enter` on a member, are allowed.
                _ => None,
            },
            At::Remote(_) => match a {
                Action::Trash => Some(NO_REMOTE_TRASH),
                Action::EditNew
                | Action::Attributes
                | Action::MultiRename
                | Action::Find
                | Action::OpenArchive => Some(NOT_ON_SERVER),
                // Browsing, F3, F4 and `Enter` on a file (TDR-3 5.4, 5.5); F7, Shift+F6 and
                // Shift+F8 (TDR-3 5.6).
                _ => None,
            },
        },
    }
}

/// F5 and F6 from `here` into `there` (the first table of TDR-3 2.4).
fn transfer_refusal(moving: bool, here: At, there: At) -> Option<&'static str> {
    match (here, there) {
        (h, t) if h.local() && t.local() => None,
        // Nothing is written into an archive.
        (_, At::Archive) => Some(READ_ONLY),
        // Upload, and a best-effort move that deletes the local source after it (R-4).
        (h, At::Remote(_)) if h.local() => None,
        // Extract; a move out of an archive would delete from it.
        (At::Archive, t) if t.local() => moving.then_some(READ_ONLY),
        (At::Archive, _) => Some(THROUGH_LOCAL),
        // Download, and a move that keeps the remote sources (R-4).
        (At::Remote(_), t) if t.local() => None,
        // A rename on the server; there is no copy on the server.
        (At::Remote(a), At::Remote(b)) if a == b => (!moving).then_some(NO_SERVER_COPY),
        _ => Some(THROUGH_LOCAL),
    }
}

/// Compare by content reads files, so it needs two local panels; by date and size works on
/// any listing (TDR-3 2.4).
pub fn compare_refusal(content: bool, left: At, right: At) -> Option<&'static str> {
    if !content {
        return None;
    }
    [left, right].into_iter().find_map(|at| match at {
        At::Archive => Some(NOT_IN_ARCHIVE),
        At::Remote(_) => Some(NOT_ON_SERVER),
        At::Local | At::Results => None,
    })
}

/// The worker's side of the UI: questions block until the dialog answers.
pub struct ChannelUi {
    pub tx: Sender<Event>,
}

impl Interaction for ChannelUi {
    fn ask(&mut self, q: Question) -> Answer {
        let (rtx, rrx) = channel();
        if self.tx.send(Event::Job(JobEvent::Ask(q, rtx))).is_err() {
            return Answer::Cancel;
        }
        rrx.recv().unwrap_or(Answer::Cancel)
    }

    fn progress(&mut self, p: Progress) {
        let _ = self.tx.send(Event::Job(JobEvent::Progress(p)));
    }
}

/// Starts the worker for `spec`; `cancel` is the job's flag.
pub fn spawn(spec: JobSpec, tx: Sender<Event>, cancel: Arc<AtomicBool>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("job".into())
        // Traversal recurses once per directory level.
        .stack_size(64 << 20)
        .spawn(move || {
            let sys = Sys::new(cancel);
            let mut ui = ChannelUi { tx: tx.clone() };
            let report = run_guarded(spec, &sys, &mut ui);
            let _ = tx.send(Event::Job(JobEvent::Done(report)));
        })?;
    Ok(())
}

impl App {
    pub(super) fn request_quit(&mut self, forced: bool) -> Vec<Effect> {
        if self.job.is_none() {
            self.quit = true;
            return vec![Effect::Quit];
        }
        if forced || self.quit_after_job {
            // A second request while the cancel is pending, or a signal: cancel and quit
            // as soon as the worker returns.
            self.quit_after_job = true;
            if let Some(j) = self.job.as_mut() {
                j.cancel_requested = true;
                j.cancel_at.get_or_insert_with(Instant::now);
            }
            if let Some(d) = self.dialog.as_mut() {
                d.abandon();
            }
            self.dialog = None;
            return vec![Effect::CancelJob];
        }
        let verb = self.job.as_ref().map(|j| j.verb.name()).unwrap_or("job");
        self.dialog = Some(Dialog::confirm(
            "Quit",
            vec![
                format!("A {verb} job is running."),
                "Cancel it and quit?".into(),
            ],
            "Cancel job and quit",
            Purpose::QuitWithJob,
        ));
        Vec::new()
    }

    pub(super) fn on_job(&mut self, j: JobEvent) -> Vec<Effect> {
        match j {
            JobEvent::Progress(p) => {
                if let Some(job) = self.job.as_mut() {
                    job.progress = Some(p);
                }
                Vec::new()
            }
            JobEvent::Ask(q, reply) => {
                if self.quit_after_job {
                    let _ = reply.send(crate::fsops::question::Answer::Cancel);
                } else {
                    self.dialog = Some(Dialog::question(q, reply));
                }
                Vec::new()
            }
            JobEvent::Done(r) => self.job_done(r),
        }
    }

    fn job_done(&mut self, r: Report) -> Vec<Effect> {
        let side = self.job.take().map(|j| j.side).unwrap_or(self.active);
        if self.dialog.as_ref().is_some_and(|d| d.is_question()) {
            self.dialog = None;
        }
        tracing::info!(summary = %r.summary(), "job done");
        // Every rename job's outcome carries its undo record; the last non-empty one is
        // kept (TDR-2 6.5). An undo job's own record is not.
        if r.verb == JobVerb::Rename && !r.renamed.is_empty() {
            self.rename_undo = Some(r.renamed.clone());
        }
        if self.quit_after_job {
            self.quit = true;
            return vec![Effect::Quit];
        }
        if let Some(name) = &r.focus {
            let n = name.as_bytes().to_vec();
            let p = self.sides[side].panel_mut();
            p.cursor_to_name(&n);
        }
        let summary = r.summary();
        if r.needs_attention() {
            self.dialog = Some(Dialog::report(r));
        } else {
            self.say(summary);
        }
        // The cursor names survive the refresh.
        self.refresh_both()
    }

    pub(super) fn start_job(&mut self, spec: JobSpec) -> Vec<Effect> {
        if self.job.is_some() {
            self.warn("a job is running");
            return Vec::new();
        }
        self.job = Some(JobUi {
            verb: spec.verb(),
            progress: None,
            started: Instant::now(),
            cancel_requested: false,
            cancel_at: None,
            side: self.active,
        });
        vec![Effect::StartJob(spec)]
    }

    /// Progress text for the status row.
    pub fn job_line(&self) -> Option<String> {
        let j = self.job.as_ref()?;
        let mut s = format!("{} ", j.verb.name());
        match &j.progress {
            Some(p) => {
                let phase = match p.phase {
                    Phase::Scanning => "scanning",
                    Phase::Executing => "",
                    Phase::Flushing => "flushing",
                };
                if !phase.is_empty() {
                    s += &format!("({phase}) ");
                }
                if p.files_total > 0 {
                    s += &format!("{}/{} files", p.files_done, p.files_total);
                } else {
                    // A scan's running total, or a job without a scan (TDR-2 8.2) counting
                    // what it has done.
                    s += &format!("{} files", p.files_total.max(p.files_done));
                }
                if p.bytes_total > 0 {
                    let pct = p.bytes_done as f64 * 100.0 / p.bytes_total as f64;
                    s += &format!(", {pct:.0}%");
                }
            }
            None => s += "starting",
        }
        if j.cancel_requested {
            if j.cancel_at
                .is_some_and(|t| t.elapsed() > Duration::from_secs(2))
            {
                s += " -- cancel pending -- the filesystem is not responding";
            } else {
                s += " -- cancelling";
            }
        } else {
            s += "   Esc: cancel";
        }
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use At::{Archive, Local, Remote, Results};

    /// The first table of TDR-3 2.4: F5 and F6 by where they come from and go to.
    #[test]
    fn copies_and_moves_by_place() {
        let (one, two) = (Remote(1), Remote(2));
        let rows: &[(At, At, Option<&str>, Option<&str>)] = &[
            (Local, Local, None, None),
            (Results, Local, None, None),
            (Local, Results, None, None),
            (Local, Archive, Some(READ_ONLY), Some(READ_ONLY)),
            (Results, Archive, Some(READ_ONLY), Some(READ_ONLY)),
            (Local, one, None, None),
            (Results, one, None, None),
            (Archive, Local, None, Some(READ_ONLY)),
            (Archive, Results, None, Some(READ_ONLY)),
            (Archive, Archive, Some(READ_ONLY), Some(READ_ONLY)),
            (Archive, one, Some(THROUGH_LOCAL), Some(THROUGH_LOCAL)),
            (one, Local, None, None),
            (one, Results, None, None),
            (one, Archive, Some(READ_ONLY), Some(READ_ONLY)),
            (one, one, Some(NO_SERVER_COPY), None),
            (one, two, Some(THROUGH_LOCAL), Some(THROUGH_LOCAL)),
        ];
        for &(here, there, copy, mv) in rows {
            assert_eq!(
                refusal(Action::Copy, here, there),
                copy,
                "F5 {here:?} -> {there:?}"
            );
            assert_eq!(
                refusal(Action::Move, here, there),
                mv,
                "F6 {here:?} -> {there:?}"
            );
        }
    }

    /// The second table of TDR-3 2.4: verbs in an archive or a remote panel.
    #[test]
    fn verbs_in_a_panel_by_place() {
        let writes = [
            Action::Mkdir,
            Action::Rename,
            Action::Trash,
            Action::Delete,
            Action::EditNew,
            Action::Attributes,
            Action::MultiRename,
        ];
        for a in writes {
            assert_eq!(refusal(a, Archive, Local), Some(READ_ONLY), "{a:?}");
            assert_eq!(
                refusal(a, Local, Archive),
                None,
                "{a:?} acts here, not there"
            );
        }
        assert_eq!(refusal(Action::Find, Archive, Local), Some(NOT_IN_ARCHIVE));
        assert_eq!(
            refusal(Action::InsertPath, Archive, Local),
            Some(NOT_IN_ARCHIVE)
        );
        assert_eq!(
            refusal(Action::OpenArchive, Archive, Local),
            Some(crate::archive::NESTED)
        );
        assert_eq!(refusal(Action::OpenArchive, Local, Archive), None);
        assert_eq!(refusal(Action::Link, Archive, Local), Some(NOT_IN_ARCHIVE));
        assert_eq!(refusal(Action::Link, Local, Archive), Some(READ_ONLY));
        let r = Remote(1);
        assert_eq!(refusal(Action::Trash, r, Local), Some(NO_REMOTE_TRASH));
        for a in [
            Action::EditNew,
            Action::Attributes,
            Action::MultiRename,
            Action::Find,
            Action::Link,
            Action::OpenArchive,
        ] {
            assert_eq!(refusal(a, r, Local), Some(NOT_ON_SERVER), "{a:?}");
        }
        assert_eq!(refusal(Action::Link, Local, r), Some(NOT_ON_SERVER));
        // F7, Shift+F6 and Shift+F8 on a server; F8 stays refused (R-5).
        for a in [Action::Mkdir, Action::Rename, Action::Delete] {
            assert_eq!(refusal(a, r, Local), None, "{a:?}");
        }
        // Members and remote files are viewed.
        for a in [Action::View, Action::Edit, Action::QuickLoad] {
            assert_eq!(refusal(a, Archive, Local), None, "{a:?}");
            assert_eq!(refusal(a, r, Local), None, "{a:?}");
            assert_eq!(refusal(a, Local, Archive), None, "{a:?}");
        }
        // Archives and servers are browsed.
        for a in [Action::Enter, Action::Parent] {
            assert_eq!(refusal(a, Archive, Local), None, "{a:?}");
            assert_eq!(refusal(a, r, Local), None, "{a:?}");
        }
        // Cursor keys, marks, tabs and the command line are never refused.
        for a in [
            Action::Up,
            Action::MarkSpace,
            Action::MarkAll,
            Action::Reread,
            Action::NewTab,
            Action::Compare,
            Action::LineChar('x'),
        ] {
            assert_eq!(refusal(a, Archive, r), None, "{a:?}");
            assert_eq!(refusal(a, r, Archive), None, "{a:?}");
        }
        // Local directories and results tabs are never refused here.
        for a in writes
            .into_iter()
            .chain([Action::Find, Action::View, Action::Link])
        {
            for (h, t) in [(Local, Local), (Results, Local), (Local, Results)] {
                assert_eq!(refusal(a, h, t), None, "{a:?} {h:?} {t:?}");
            }
        }
    }

    #[test]
    fn compare_by_content_needs_local_panels() {
        assert_eq!(compare_refusal(false, Archive, Remote(1)), None);
        assert_eq!(compare_refusal(true, Local, Results), None);
        assert_eq!(compare_refusal(true, Archive, Local), Some(NOT_IN_ARCHIVE));
        assert_eq!(compare_refusal(true, Local, Remote(3)), Some(NOT_ON_SERVER));
    }
}
