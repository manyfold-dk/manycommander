#![forbid(unsafe_code)]
//! Jobs: what the UI asks for, what the worker reports (design 4.4, I-7).
//!
//! The UI builds a [`JobSpec`] from state it already has and does no filesystem work. The
//! worker resolves the paths once, at job start, and runs the verb's phases. Every run goes
//! through [`run_guarded`]: a panic in the engine becomes a failed report, never a dead app
//! (NFR-REL).

use super::attr::{AttrChange, ModeChange};
use super::group::{Group, Root};
use super::link::LinkKind;
use super::question::Interaction;
use super::rename::RenamedDir;
use super::sys::{Sys, Ts};
use crate::provider::VPath;
use crate::remote::RemoteProvider;
use std::ffi::OsString;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Where a copy or a move goes (P3 2.2).
#[derive(Clone)]
pub enum Dest {
    /// What the user confirmed: an existing directory to copy into, or, for a single source
    /// name in total, a new path.
    Local(PathBuf),
    /// A directory on a server (an upload, phase 3b).
    Remote {
        session: Arc<RemoteProvider>,
        dir: VPath,
    },
}

/// What a copy or move to a server says until uploads exist (P3 5.6).
pub const NO_UPLOAD: &str = "a server is not a destination yet";

impl fmt::Debug for Dest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Dest::Local(p) => f.debug_tuple("Local").field(p).finish(),
            Dest::Remote { dir, .. } => f.debug_struct("Remote").field("dir", dir).finish(),
        }
    }
}

/// Two remote destinations are equal when they name one directory of the same session.
impl PartialEq for Dest {
    fn eq(&self, other: &Dest) -> bool {
        match (self, other) {
            (Dest::Local(a), Dest::Local(b)) => a == b,
            (Dest::Remote { session: a, dir: x }, Dest::Remote { session: b, dir: y }) => {
                Arc::ptr_eq(a, b) && x == y
            }
            _ => false,
        }
    }
}

impl Eq for Dest {}

impl From<PathBuf> for Dest {
    fn from(p: PathBuf) -> Dest {
        Dest::Local(p)
    }
}

impl From<&Path> for Dest {
    fn from(p: &Path) -> Dest {
        Dest::Local(p.to_path_buf())
    }
}

impl From<&str> for Dest {
    fn from(p: &str) -> Dest {
        Dest::Local(PathBuf::from(p))
    }
}

/// A file-operation request. The verbs that act on a selection take groups (P2 2.2): a
/// directory panel produces one group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobSpec {
    /// F5. `dst` is what the user confirmed: an existing directory to copy into, or, for a
    /// single source name in total, a new path (P3 2.2: or a directory on a server).
    Copy { groups: Vec<Group>, dst: Dest },
    /// F6, and Shift+F6 as one group with one name, with the same destination rules as
    /// copy.
    Move { groups: Vec<Group>, dst: Dest },
    /// F7. `name` may contain `/` and creates missing parents.
    Mkdir { dir: PathBuf, name: OsString },
    /// F8.
    Trash { groups: Vec<Group> },
    /// Shift+F8.
    Delete { groups: Vec<Group> },
    /// Alt+L (P2 8.1). `dst` as for copy: an existing directory to link into, or, for a
    /// single source name in total, the new link's path.
    Link {
        groups: Vec<Group>,
        dst: PathBuf,
        kind: LinkKind,
    },
    /// Alt+A (P2 8.2). `None` leaves that attribute unchanged.
    Attr {
        groups: Vec<Group>,
        mode: Option<ModeChange>,
        mtime: Option<Ts>,
        recursive: bool,
    },
    /// Ctrl+M (P2 6.3): per group, `(old name, new name)` in selection order; the old names
    /// are the group's names.
    Rename {
        groups: Vec<Group>,
        renames: Vec<Vec<(OsString, OsString)>>,
    },
    /// Ctrl+Z in the multi-rename dialog (P2 6.5): the renames of an earlier rename job,
    /// reversed where the entries still have the recorded identities.
    UndoRename { record: Vec<RenamedDir> },
}

impl JobSpec {
    pub fn verb(&self) -> JobVerb {
        match self {
            JobSpec::Copy { .. } => JobVerb::Copy,
            JobSpec::Move { .. } => JobVerb::Move,
            JobSpec::Mkdir { .. } => JobVerb::Mkdir,
            JobSpec::Trash { .. } => JobVerb::Trash,
            JobSpec::Delete { .. } => JobVerb::Delete,
            JobSpec::Link { .. } => JobVerb::Link,
            JobSpec::Attr { .. } => JobVerb::Attr,
            JobSpec::Rename { .. } => JobVerb::Rename,
            JobSpec::UndoRename { .. } => JobVerb::UndoRename,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum JobVerb {
    #[default]
    Copy,
    Move,
    Mkdir,
    Trash,
    Delete,
    Link,
    Attr,
    Rename,
    UndoRename,
}

impl JobVerb {
    pub fn name(self) -> &'static str {
        match self {
            JobVerb::Copy => "copy",
            JobVerb::Move => "move",
            JobVerb::Mkdir => "make directory",
            JobVerb::Trash => "trash",
            JobVerb::Delete => "delete",
            JobVerb::Link => "link",
            JobVerb::Attr => "change attributes",
            JobVerb::Rename => "rename",
            JobVerb::UndoRename => "undo rename",
        }
    }

    /// The past participle for "N copied".
    fn done_word(self) -> &'static str {
        match self {
            JobVerb::Copy => "copied",
            JobVerb::Move => "moved",
            JobVerb::Mkdir => "created",
            JobVerb::Trash => "moved to trash",
            JobVerb::Delete => "deleted",
            JobVerb::Link => "linked",
            JobVerb::Attr => "changed",
            JobVerb::Rename => "renamed",
            JobVerb::UndoRename => "renamed back",
        }
    }

    /// What the entries that were never reached are, after a cancel.
    fn left_word(self) -> &'static str {
        match self {
            JobVerb::Copy => "not copied",
            JobVerb::Move => "still at source",
            JobVerb::Mkdir => "not created",
            JobVerb::Trash => "not trashed",
            JobVerb::Delete => "not deleted",
            JobVerb::Link => "not linked",
            JobVerb::Attr => "not changed",
            JobVerb::Rename => "not renamed",
            JobVerb::UndoRename => "not renamed back",
        }
    }
}

/// How an entry ended when it did not end as done.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Skipped(String),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub path: PathBuf,
    pub outcome: Outcome,
}

/// The job report (I-7): every entry ends as done, skipped with a reason, or failed with
/// the OS error; the report states the state the job left.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub verb: JobVerb,
    /// Non-directory entries in the plan (files, symlinks, special files).
    pub planned: u64,
    /// Non-directory entries done.
    pub done: u64,
    pub dirs_done: u64,
    /// Skipped entries (each reported once; a skipped directory is one entry).
    pub skipped: u64,
    /// Failed entries (each reported once).
    pub failed: u64,
    /// Entries that were already as asked, so nothing changed (P2 8.2); not counted as done.
    pub unchanged: u64,
    /// Planned non-directory entries that ended (done, or inside something skipped or
    /// failed). `planned - settled` were never reached.
    pub settled: u64,
    /// Skipped and failed entries, in the order they happened.
    pub issues: Vec<Issue>,
    /// Facts the user needs beyond per-entry outcomes: kept source directories, the
    /// intermediate path of a failed case-only rename.
    pub notes: Vec<String>,
    pub cancelled: bool,
    /// The job was refused before any write.
    pub refused: Option<String>,
    /// The name the panel cursor should move to (F7).
    pub focus: Option<OsString>,
    /// The renames a rename job performed, with identities, whatever its outcome: the undo
    /// record (P2 6.5).
    pub renamed: Vec<RenamedDir>,
}

impl Report {
    pub fn new(verb: JobVerb) -> Report {
        Report {
            verb,
            ..Report::default()
        }
    }

    pub fn refused(verb: JobVerb, why: impl fmt::Display) -> Report {
        Report {
            verb,
            refused: Some(why.to_string()),
            ..Report::default()
        }
    }

    pub fn skip(&mut self, path: PathBuf, why: impl Into<String>) {
        self.skipped += 1;
        self.issues.push(Issue {
            path,
            outcome: Outcome::Skipped(why.into()),
        });
    }

    pub fn fail(&mut self, path: PathBuf, why: impl Into<String>) {
        self.failed += 1;
        self.issues.push(Issue {
            path,
            outcome: Outcome::Failed(why.into()),
        });
    }

    /// Entries never reached (a cancel, or a job that stopped on an error).
    pub fn remaining(&self) -> u64 {
        self.planned.saturating_sub(self.settled)
    }

    /// Whether the user needs to see the list: anything skipped, failed, kept or refused.
    pub fn needs_attention(&self) -> bool {
        !self.issues.is_empty() || !self.notes.is_empty() || self.refused.is_some()
    }

    /// The one-line summary, e.g. "move cancelled: 812 moved, 40 still at source".
    pub fn summary(&self) -> String {
        let v = self.verb;
        if let Some(why) = &self.refused {
            return format!("{} refused: {why}", v.name());
        }
        let mut parts = vec![format!("{} {}", self.done, v.done_word())];
        if self.dirs_done > 0 {
            let s = if self.dirs_done == 1 {
                "directory"
            } else {
                "directories"
            };
            parts.push(format!("{} {s}", self.dirs_done));
        }
        if self.unchanged > 0 {
            parts.push(format!("{} unchanged", self.unchanged));
        }
        if self.skipped > 0 {
            parts.push(format!("{} skipped", self.skipped));
        }
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        let left = self.remaining();
        if left > 0 {
            parts.push(format!("{left} {}", v.left_word()));
        }
        let state = if self.cancelled { " cancelled" } else { "" };
        format!("{}{state}: {}", v.name(), parts.join(", "))
    }
}

/// Whether a job's sources are in an archive: extraction (P3 3.5).
fn from_archive(groups: &[Group]) -> bool {
    groups.iter().any(|g| matches!(g.root, Root::Archive(_)))
}

/// Whether a job's sources are on a server: a download (P3 5.5).
fn from_remote(groups: &[Group]) -> bool {
    groups.iter().any(|g| matches!(g.root, Root::Remote(_)))
}

/// What a move out of a server says until phase 3b (P3 5.6, E-4).
pub const NO_REMOTE_MOVE: &str = "not available here yet";

/// Runs a job on the calling (worker) thread.
pub fn run(spec: JobSpec, sys: &Sys, ui: &mut dyn Interaction) -> Report {
    match spec {
        JobSpec::Copy { groups, dst } => match dst {
            Dest::Local(dst) if from_archive(&groups) => {
                crate::archive::extract::extract(sys, ui, &groups, &dst)
            }
            Dest::Local(dst) if from_remote(&groups) => {
                crate::remote::tree::download(sys, ui, &groups, &dst)
            }
            Dest::Local(dst) => super::copy::copy_groups(sys, ui, &groups, &dst),
            Dest::Remote { .. } => Report::refused(JobVerb::Copy, NO_UPLOAD),
        },
        // A move would remove members from the archive (A-2).
        JobSpec::Move { groups, .. } if from_archive(&groups) => {
            Report::refused(JobVerb::Move, crate::archive::extract::READ_ONLY)
        }
        // A move out of a server is phase 3b (P3 5.6).
        JobSpec::Move { groups, .. } if from_remote(&groups) => {
            Report::refused(JobVerb::Move, NO_REMOTE_MOVE)
        }
        JobSpec::Move { groups, dst } => match dst {
            Dest::Local(dst) => super::mv::move_groups(sys, ui, &groups, &dst),
            Dest::Remote { .. } => Report::refused(JobVerb::Move, NO_UPLOAD),
        },
        JobSpec::Mkdir { dir, name } => super::mkdir::mkdir_job(sys, &dir, &name),
        JobSpec::Trash { groups } => super::trash::trash_groups(sys, ui, &groups),
        JobSpec::Delete { groups } => super::delete::delete_groups(sys, ui, &groups),
        JobSpec::Link { groups, dst, kind } => {
            super::link::link_groups(sys, ui, &groups, &dst, kind)
        }
        JobSpec::Attr {
            groups,
            mode,
            mtime,
            recursive,
        } => {
            let change = AttrChange {
                mode,
                mtime,
                recursive,
            };
            super::attr::attr_groups(sys, ui, &groups, &change)
        }
        JobSpec::Rename { groups, renames } => {
            super::rename::rename_groups(sys, ui, &groups, &renames)
        }
        JobSpec::UndoRename { record } => super::rename::undo_groups(sys, ui, &record),
    }
}

/// Runs a job under `catch_unwind`: a panic becomes a failed report (NFR-REL). Temporary
/// files are removed by their guards while the panic unwinds.
pub fn run_guarded(spec: JobSpec, sys: &Sys, ui: &mut dyn Interaction) -> Report {
    let verb = spec.verb();
    match catch_unwind(AssertUnwindSafe(|| run(spec, sys, ui))) {
        Ok(r) => r,
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            let mut r = Report::new(verb);
            r.fail(
                PathBuf::new(),
                format!("internal error, the job stopped: {msg}"),
            );
            r
        }
    }
}
