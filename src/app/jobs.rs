#![forbid(unsafe_code)]
//! Job wiring (design 3.1, 4.4): the channel-backed [`Interaction`] and the worker thread.
//! One job runs at a time. The worker runs under `catch_unwind` (`run_guarded`), so a
//! panic ends as a failed report.
//!
//! The verbs by place (P3 2.4): [`refusal`] refuses a verb that an archive or a server
//! does not allow, before any work, with a status-line message, as P2's "not in search
//! results".

use super::event::{Event, JobEvent};
use super::keys::Action;
use crate::fsops::job::{JobSpec, run_guarded};
use crate::fsops::question::{Answer, Interaction, Progress, Question};
use crate::fsops::sys::Sys;
use crate::panel::{Panel, Source};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Sender, channel};

/// Where a verb acts, as the table of P3 2.4 sees a panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
    /// A local directory (M1).
    Local,
    /// A results tab (P2 5); its entries are local.
    Results,
    /// An archive (P3 3).
    Archive,
    /// A server; the value tells sessions apart (the session's address), so a copy
    /// within one session and one across sessions differ (P3 2.4).
    Remote(usize),
}

impl At {
    pub fn of(p: &Panel) -> At {
        match &p.source {
            Source::Dir => At::Local,
            Source::Results(_) => At::Results,
            Source::Archive(_) => At::Archive,
            Source::Remote(v) => At::Remote(Arc::as_ptr(&v.session).cast::<()>() as usize),
        }
    }

    fn local(self) -> bool {
        matches!(self, At::Local | At::Results)
    }
}

/// An archive is never written (A-2).
pub const READ_ONLY: &str = crate::archive::extract::READ_ONLY;
/// F8 on a server (R-5).
pub const NO_REMOTE_TRASH: &str = "no trash on the server; Shift+F8 deletes permanently";
/// F5 within one session (P3 2.4).
pub const NO_SERVER_COPY: &str = "no copy on the server; copy through a local directory";
/// A copy or move between two sessions, or between an archive and a server (P3 2.4).
pub const THROUGH_LOCAL: &str = "copy through a local directory";
/// A verb that needs local files, in an archive (P3 2.4).
pub const NOT_IN_ARCHIVE: &str = "not in an archive";
/// A verb that needs local files, on a server (P3 2.4).
pub const NOT_ON_SERVER: &str = "not on a server";
/// A verb that the design allows on a server, and that a later phase 3 task brings: SFTP
/// 3a (T6) and 3b (T7).
pub const NOT_YET: &str = "not available here yet";

/// The refusal of `a` in the active panel `here`, with `there` the other panel, the copy
/// and move destination (P3 2.4). `Some` is the status-line message; the verb does nothing.
/// Local directories and results tabs are never refused here: a results tab's own
/// refusals are P2's.
pub fn refusal(a: Action, here: At, there: At) -> Option<&'static str> {
    match a {
        Action::Copy | Action::Move => transfer_refusal(a == Action::Move, here, there),
        // A link is made in the other panel and points into this one (P2 8.1).
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
                // A member of an archive is not opened as an archive (P3 3.1).
                Action::OpenArchive => Some(crate::archive::NESTED),
                // Browsing (T2), and F3, F4 and `Enter` on a member (T3), are allowed.
                _ => None,
            },
            At::Remote(_) => match a {
                Action::Trash => Some(NO_REMOTE_TRASH),
                Action::EditNew
                | Action::Attributes
                | Action::MultiRename
                | Action::Find
                | Action::OpenArchive => Some(NOT_ON_SERVER),
                Action::Mkdir
                | Action::Rename
                | Action::Delete
                | Action::Enter
                | Action::Parent
                | Action::View
                | Action::Edit => Some(NOT_YET),
                _ => None,
            },
        },
    }
}

/// F5 and F6 from `here` into `there` (the first table of P3 2.4).
fn transfer_refusal(moving: bool, here: At, there: At) -> Option<&'static str> {
    match (here, there) {
        (h, t) if h.local() && t.local() => None,
        // Nothing is written into an archive.
        (_, At::Archive) => Some(READ_ONLY),
        // Upload, and a move that deletes the local source after it (3b).
        (h, At::Remote(_)) if h.local() => Some(NOT_YET),
        // Extract (T3); a move out of an archive would delete from it.
        (At::Archive, t) if t.local() => moving.then_some(READ_ONLY),
        (At::Archive, _) => Some(THROUGH_LOCAL),
        // Download (T6), and a move that keeps the remote sources (3b).
        (At::Remote(_), t) if t.local() => Some(NOT_YET),
        // A rename on the server (3b); there is no copy on the server.
        (At::Remote(a), At::Remote(b)) if a == b => {
            Some(if moving { NOT_YET } else { NO_SERVER_COPY })
        }
        _ => Some(THROUGH_LOCAL),
    }
}

/// Compare by content reads files, so it needs two local panels; by date and size works on
/// any listing (P3 2.4).
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

#[cfg(test)]
mod tests {
    use super::*;
    use At::{Archive, Local, Remote, Results};

    /// The first table of P3 2.4: F5 and F6 by where they come from and go to.
    #[test]
    fn copies_and_moves_by_place() {
        let (one, two) = (Remote(1), Remote(2));
        let rows: &[(At, At, Option<&str>, Option<&str>)] = &[
            (Local, Local, None, None),
            (Results, Local, None, None),
            (Local, Results, None, None),
            (Local, Archive, Some(READ_ONLY), Some(READ_ONLY)),
            (Results, Archive, Some(READ_ONLY), Some(READ_ONLY)),
            (Local, one, Some(NOT_YET), Some(NOT_YET)),
            (Results, one, Some(NOT_YET), Some(NOT_YET)),
            (Archive, Local, None, Some(READ_ONLY)),
            (Archive, Results, None, Some(READ_ONLY)),
            (Archive, Archive, Some(READ_ONLY), Some(READ_ONLY)),
            (Archive, one, Some(THROUGH_LOCAL), Some(THROUGH_LOCAL)),
            (one, Local, Some(NOT_YET), Some(NOT_YET)),
            (one, Archive, Some(READ_ONLY), Some(READ_ONLY)),
            (one, one, Some(NO_SERVER_COPY), Some(NOT_YET)),
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

    /// The second table of P3 2.4: verbs in an archive or a remote panel.
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
        // Later phase 3 tasks: 3b on a server, browsing a server, viewing in both.
        for a in [Action::Mkdir, Action::Rename, Action::Delete] {
            assert_eq!(refusal(a, r, Local), Some(NOT_YET), "{a:?}");
        }
        // T3: members are viewed; viewing a remote file arrives with T6.
        for a in [Action::View, Action::Edit] {
            assert_eq!(refusal(a, Archive, Local), None, "{a:?}");
            assert_eq!(refusal(a, r, Local), Some(NOT_YET), "{a:?}");
            assert_eq!(refusal(a, Local, Archive), None, "{a:?}");
        }
        // T2: an archive is browsed.
        for a in [Action::Enter, Action::Parent] {
            assert_eq!(refusal(a, Archive, Local), None, "{a:?}");
            assert_eq!(refusal(a, r, Local), Some(NOT_YET), "{a:?}");
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
