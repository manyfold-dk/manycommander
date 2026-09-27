#![forbid(unsafe_code)]
//! Questions the worker asks the UI, their answers, and the errno mapping (design 4.5).
//!
//! The engine talks to the UI only through [`Interaction`]: tests use a scripted
//! implementation, the app a channel-backed one. The worker blocks in `ask` until the
//! answer arrives.

use super::sys::{Kind, Meta, Ts};
use rustix::io::Errno;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// One side of a conflict, as the dialog shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Side {
    pub kind: Kind,
    pub size: u64,
    pub mtime: Ts,
    pub readonly: bool,
}

impl Side {
    pub fn of(m: &Meta) -> Side {
        Side {
            kind: m.kind,
            size: m.size,
            mtime: m.mtime,
            readonly: m.perm & 0o222 == 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Question {
    /// File over file, or over a symlink ("the link is replaced; its target is not touched").
    FileExists {
        path: PathBuf,
        src: Side,
        dst: Side,
        dst_is_symlink: bool,
    },
    /// Directory over directory.
    DirExists { path: PathBuf, src: Side, dst: Side },
    /// Directory over a non-directory, or the reverse. A tree never replaces a file, and a
    /// file never replaces a tree.
    TypeMismatch { path: PathBuf, src: Side, dst: Side },
    /// Any other error on an entry.
    Error {
        path: PathBuf,
        op: &'static str,
        errno: Errno,
    },
    /// No trash can take this entry (design 4.10 step 5). The only answers are Skip and
    /// "Delete permanently...", which leads to [`Question::ConfirmDelete`] for this entry.
    TrashUnavailable { path: PathBuf, reason: String },
    /// The typed `delete` confirmation (Shift+F8 after the scan, or one entry after a failed
    /// trash). Only [`Answer::Confirm`] deletes; there is no default-Enter path.
    ConfirmDelete {
        files: u64,
        dirs: u64,
        bytes: u64,
        single: Option<PathBuf>,
    },
}

/// The answers. `Rename` carries the new name the user typed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Overwrite,
    OverwriteAll,
    OverwriteAllOlder,
    Skip,
    SkipAll,
    Rename(OsString),
    Merge,
    MergeAll,
    Retry,
    /// "Skip all of this errno".
    SkipAllErrno,
    Cancel,
    DeletePermanently,
    Confirm,
}

/// The answer kinds a dialog offers, without payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Overwrite,
    OverwriteAll,
    OverwriteAllOlder,
    Skip,
    SkipAll,
    Rename,
    Merge,
    MergeAll,
    Retry,
    SkipAllErrno,
    Cancel,
    DeletePermanently,
    Confirm,
}

impl Choice {
    pub fn label(self) -> &'static str {
        match self {
            Choice::Overwrite => "Overwrite",
            Choice::OverwriteAll => "Overwrite all",
            Choice::OverwriteAllOlder => "Overwrite all older",
            Choice::Skip => "Skip",
            Choice::SkipAll => "Skip all",
            Choice::Rename => "Rename",
            Choice::Merge => "Merge",
            Choice::MergeAll => "Merge all",
            Choice::Retry => "Retry",
            Choice::SkipAllErrno => "Skip all of this error",
            Choice::Cancel => "Cancel job",
            Choice::DeletePermanently => "Delete permanently...",
            Choice::Confirm => "Delete",
        }
    }
}

impl Question {
    /// The answers the dialog offers, in display order (design 4.5 table).
    pub fn choices(&self) -> &'static [Choice] {
        use Choice::*;
        match self {
            Question::FileExists { .. } => &[
                Overwrite,
                OverwriteAll,
                OverwriteAllOlder,
                Skip,
                SkipAll,
                Rename,
                Cancel,
            ],
            Question::DirExists { .. } => &[Merge, MergeAll, Skip, Rename, Cancel],
            Question::TypeMismatch { .. } => &[Skip, SkipAll, Rename, Cancel],
            Question::Error { .. } => &[Retry, Skip, SkipAllErrno, Cancel],
            Question::TrashUnavailable { .. } => &[Skip, DeletePermanently],
            Question::ConfirmDelete { .. } => &[Confirm, Cancel],
        }
    }

    /// The focused answer when the dialog opens.
    pub fn default_choice(&self) -> Choice {
        match self {
            Question::FileExists { .. } => Choice::Skip,
            Question::DirExists { .. } => Choice::Merge,
            Question::TypeMismatch { .. } => Choice::Skip,
            Question::Error { errno, .. } if *errno == Errno::NOSPC => Choice::Retry,
            Question::Error { .. } => Choice::Skip,
            Question::TrashUnavailable { .. } => Choice::Skip,
            // No default-Enter path deletes: the typed confirmation needs the word.
            Question::ConfirmDelete { .. } => Choice::Cancel,
        }
    }
}

/// Which conflict question an existing destination raises (design 4.5): the errno only
/// says that something is there; the two kinds decide which question it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conflict {
    FileExists,
    DirExists,
    TypeMismatch,
}

pub fn conflict(src: Kind, dst: Kind) -> Conflict {
    match (src == Kind::Dir, dst == Kind::Dir) {
        (true, true) => Conflict::DirExists,
        (false, false) => Conflict::FileExists,
        // A directory over a symlink is a mismatch too: dst is not a directory.
        _ => Conflict::TypeMismatch,
    }
}

/// Whether an errno from a create or rename means "the destination exists"
/// (`EEXIST`, `ENOTEMPTY`, `EISDIR`, `ENOTDIR`); anything else is an error question.
pub fn is_conflict_errno(e: Errno) -> bool {
    matches!(
        e,
        Errno::EXIST | Errno::NOTEMPTY | Errno::ISDIR | Errno::NOTDIR
    )
}

/// `name (1).ext`, the pre-filled Rename answer. A leading dot is part of the stem.
pub fn suggest_rename(name: &std::ffi::OsStr, n: u32) -> OsString {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let b = name.as_bytes();
    let dot = b.iter().rposition(|&c| c == b'.').filter(|&i| i > 0);
    let (stem, ext) = match dot {
        Some(i) => (&b[..i], &b[i..]),
        None => (b, &b[b.len()..]),
    };
    let mut out = stem.to_vec();
    out.extend_from_slice(format!(" ({n})").as_bytes());
    out.extend_from_slice(ext);
    OsString::from_vec(out)
}

/// Job phase shown in the progress dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Scanning,
    Executing,
    Flushing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    pub phase: Phase,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub current: PathBuf,
}

/// The worker's only channel to the UI.
pub trait Interaction {
    fn ask(&mut self, q: Question) -> Answer;
    fn progress(&mut self, p: Progress);
}

/// At most 15 progress messages per second (P-8, A-P-8).
pub const PROGRESS_INTERVAL: Duration = Duration::from_micros(66_667);

/// Wraps an [`Interaction`] and caps progress at [`PROGRESS_INTERVAL`]: two messages are
/// never closer together than that, including the first and the last of a job.
pub struct Reporter<'a> {
    ui: &'a mut dyn Interaction,
    last: Option<Instant>,
}

impl<'a> Reporter<'a> {
    pub fn new(ui: &'a mut dyn Interaction) -> Self {
        Reporter { ui, last: None }
    }

    /// Sends the progress `make` builds, if one is due; `make` runs only then.
    pub fn progress(&mut self, make: impl FnOnce() -> Progress) {
        let now = Instant::now();
        if self
            .last
            .is_none_or(|l| now.duration_since(l) >= PROGRESS_INTERVAL)
        {
            self.last = Some(now);
            self.ui.progress(make());
        }
    }

    pub fn ask(&mut self, q: Question) -> Answer {
        self.ui.ask(q)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn conflict_mapping() {
        assert_eq!(conflict(Kind::File, Kind::File), Conflict::FileExists);
        assert_eq!(conflict(Kind::File, Kind::Symlink), Conflict::FileExists);
        assert_eq!(conflict(Kind::Symlink, Kind::File), Conflict::FileExists);
        assert_eq!(conflict(Kind::Dir, Kind::Dir), Conflict::DirExists);
        assert_eq!(conflict(Kind::Dir, Kind::File), Conflict::TypeMismatch);
        assert_eq!(conflict(Kind::Dir, Kind::Symlink), Conflict::TypeMismatch);
        assert_eq!(conflict(Kind::File, Kind::Dir), Conflict::TypeMismatch);
        assert!(is_conflict_errno(Errno::NOTEMPTY));
        assert!(!is_conflict_errno(Errno::ACCESS));
    }

    #[test]
    fn defaults_follow_the_table() {
        let e = |errno| Question::Error {
            path: PathBuf::new(),
            op: "copy",
            errno,
        };
        assert_eq!(e(Errno::NOSPC).default_choice(), Choice::Retry);
        assert_eq!(e(Errno::ACCESS).default_choice(), Choice::Skip);
        let q = Question::ConfirmDelete {
            files: 1,
            dirs: 0,
            bytes: 0,
            single: None,
        };
        assert_eq!(q.default_choice(), Choice::Cancel);
    }

    #[test]
    fn rename_suggestion() {
        assert_eq!(suggest_rename(OsStr::new("a.txt"), 1), "a (1).txt");
        assert_eq!(suggest_rename(OsStr::new("a"), 2), "a (2)");
        assert_eq!(suggest_rename(OsStr::new(".bashrc"), 1), ".bashrc (1)");
        assert_eq!(suggest_rename(OsStr::new("x.tar.gz"), 1), "x.tar (1).gz");
    }
}
