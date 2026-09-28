#![forbid(unsafe_code)]
//! Jobs: what the UI asks for, what the worker reports (design 4.4, I-7).
//!
//! The UI builds a [`JobSpec`] from state it already has and does no filesystem work. The
//! worker resolves the paths once, at job start, and runs the verb's phases. Every run goes
//! through [`run_guarded`]: a panic in the engine becomes a failed report, never a dead app
//! (NFR-REL).

use super::group::Group;
use super::question::Interaction;
use super::sys::Sys;
use std::ffi::OsString;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

/// A file-operation request. The verbs that act on a selection take groups (P2 2.2): a
/// directory panel produces one group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobSpec {
    /// F5. `dst` is what the user confirmed: an existing directory to copy into, or, for a
    /// single source name in total, a new path.
    Copy { groups: Vec<Group>, dst: PathBuf },
    /// F6, and Shift+F6 as one group with one name, with the same destination rules as
    /// copy.
    Move { groups: Vec<Group>, dst: PathBuf },
    /// F7. `name` may contain `/` and creates missing parents.
    Mkdir { dir: PathBuf, name: OsString },
    /// F8.
    Trash { groups: Vec<Group> },
    /// Shift+F8.
    Delete { groups: Vec<Group> },
}

impl JobSpec {
    pub fn verb(&self) -> JobVerb {
        match self {
            JobSpec::Copy { .. } => JobVerb::Copy,
            JobSpec::Move { .. } => JobVerb::Move,
            JobSpec::Mkdir { .. } => JobVerb::Mkdir,
            JobSpec::Trash { .. } => JobVerb::Trash,
            JobSpec::Delete { .. } => JobVerb::Delete,
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
}

impl JobVerb {
    pub fn name(self) -> &'static str {
        match self {
            JobVerb::Copy => "copy",
            JobVerb::Move => "move",
            JobVerb::Mkdir => "make directory",
            JobVerb::Trash => "trash",
            JobVerb::Delete => "delete",
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

/// Runs a job on the calling (worker) thread.
pub fn run(spec: JobSpec, sys: &Sys, ui: &mut dyn Interaction) -> Report {
    match spec {
        JobSpec::Copy { groups, dst } => super::copy::copy_groups(sys, ui, &groups, &dst),
        JobSpec::Move { groups, dst } => super::mv::move_groups(sys, ui, &groups, &dst),
        JobSpec::Mkdir { dir, name } => super::mkdir::mkdir_job(sys, &dir, &name),
        JobSpec::Trash { groups } => super::trash::trash_groups(sys, ui, &groups),
        JobSpec::Delete { groups } => super::delete::delete_groups(sys, ui, &groups),
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
