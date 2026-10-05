#![forbid(unsafe_code)]
//! The events that reach the UI thread over its one channel (design 3.1), and the effects
//! `App::update` asks the runtime to perform. The UI thread makes no filesystem syscalls:
//! everything that touches the filesystem is an effect that runs on another thread.

use crate::compare::{CompareMsg, Request};
use crate::find::{FindMsg, RestatRequest, Search};
use crate::fsops::job::{JobSpec, Report};
use crate::fsops::question::{Answer, Progress, Question};
use crate::panel::listing::{Alive, ListRequest, ListingMsg};
use crate::theme::Palette;
use crossterm::event::KeyEvent;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Instant;

/// A signal, as the signal thread forwards it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sig {
    /// `SIGUSR1`: reload the theme.
    ReloadTheme,
    /// `SIGTERM`, `SIGHUP`, `SIGINT`: quit through the restore path.
    Quit,
    /// `SIGTSTP`: restore the terminal, then stop.
    Suspend,
    /// `SIGCONT`: take the terminal back and redraw.
    Resume,
}

/// Messages from the file-operation worker.
#[derive(Debug)]
pub enum JobEvent {
    Progress(Progress),
    Ask(Question, Sender<Answer>),
    Done(Report),
}

#[derive(Debug)]
pub enum Event {
    /// A key press and when the input thread read it (key-to-flush latency, P-1).
    Key(KeyEvent, Instant),
    Resize(u16, u16),
    Paste(String),
    Signal(Sig),
    /// The theme watcher saw a theme switch.
    ReloadTheme,
    /// A listing thread read and parsed the palette (`Err`: the current palette stays).
    ThemeLoaded {
        palette: Result<Palette, String>,
        requested: Instant,
    },
    Listing(ListingMsg),
    /// A watched panel directory changed (debounced).
    DirChanged {
        slot: usize,
    },
    Job(JobEvent),
    /// From the compare thread (P2 2.3, 7).
    Compare(CompareMsg),
    /// From a search's threads (P2 2.3, 5.3).
    Find(FindMsg),
    /// From the directory-store thread (P2 2.3, 3.2): `dirs.tsv` as read.
    DirsLoaded(crate::dirs::Store),
    /// From the directory-store thread (P2 3.3): zoxide's ranking, empty when zoxide is
    /// missing or failed.
    ZoxideLoaded(Vec<(PathBuf, f64)>),
    /// A helper thread failed (a hotlist save, a store read): shown as a warning.
    Status(String),
    /// From a view thread (P3 3.4): the progress of a view copy, the copy ready for the
    /// hand-off or why it failed, and after the hand-off whether it was edited.
    View(crate::viewtemp::ViewMsg),
    /// From the preview thread (P3 2.5, 4.4): a prepared image or a card.
    Preview(crate::preview::Msg),
    /// From the SFTP side (P3 2.5): a connect's session or its failure, a lost session.
    Remote(crate::remote::RemoteMsg),
    /// A handed-off child (command line, F3, F4) ended; the terminal is ours again.
    ChildDone {
        status: String,
        output: Option<Vec<u8>>,
    },
    /// Only while a spinner or a progress dialog is visible, or a preview debounce is
    /// pending (P-5).
    Tick,
}

/// Work the runtime performs for `App::update`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    LoadTheme,
    List(ListRequest, Alive),
    /// Start the search's thread pool (P2 5.3); the UI cancels it through its flag.
    Find(Arc<Search>),
    /// Re-stat a results tab on a listing thread (P2 5.5).
    Restat(RestatRequest, Alive),
    /// Watch `dir` for the panel slot, or stop watching it (`None`).
    Watch {
        slot: usize,
        dir: Option<PathBuf>,
    },
    StartJob(JobSpec),
    CancelJob,
    /// Start the compare thread (P2 7); it cancels a compare still running.
    Compare(Request),
    CancelCompare,
    /// Read `dirs.tsv` on the directory-store thread (P2 3.2).
    LoadDirs,
    /// Run `zoxide query --list --score` there (P2 3.3).
    LoadZoxide,
    /// Write `hotlist.toml` there, atomically (P2 3.2).
    SaveHotlist(Vec<PathBuf>),
    /// Stop the process after restoring the terminal (`SIGTSTP`).
    SuspendSelf,
    /// Hand the terminal to a child (design section 6).
    Run(crate::cmdline::handoff::Handoff),
    /// `setsid -f gio open <path>` (or `xdg-open`) with stdio on /dev/null.
    Open(PathBuf),
    /// Open an archive on a listing thread: through the index cache, or a scan that
    /// streams the rows of the directory the panel shows (P3 3.1, 3.3).
    OpenArchive(crate::archive::OpenRequest, Alive),
    /// List a directory of a complete archive index on a listing thread (P3 3.3).
    Relist(crate::archive::RelistRequest, Alive),
    /// A directory's size from an archive index (P3 2.4).
    ArchiveSize(crate::archive::SizeRequest),
    /// Copy a member into the runtime view directory on a listing thread (P3 3.4).
    PrepareView(crate::viewtemp::ViewRequest, Alive),
    /// After the hand-off of a view copy: keep it when it was edited, else remove it
    /// (P3 3.4), on a helper thread.
    CheckView(crate::viewtemp::ViewFile),
    /// After the hand-off of a view copy of a remote file (P3 5.6): as `CheckView`, and for
    /// an edited copy an `LSTAT` of the server file (a `Dest::Remote`), on a helper thread;
    /// the write-back question follows.
    CheckEdited(crate::viewtemp::ViewFile, crate::fsops::job::Dest),
    /// Hand the latest preview request to the preview thread (P3 4.4, step 2).
    Preview(crate::preview::Request),
    /// Connect to an `sftp://` address through the terminal hand-off (P3 5.2); the app
    /// already found no open session for its target.
    Connect(
        crate::remote::url::Address,
        crate::remote::transport::SshCommand,
    ),
    /// List a server directory on a listing thread (P3 5.4).
    ListRemote(crate::remote::provider::ListRequest, Alive),
    /// A remote directory's size by a walk on the server (P3 2.4), on a listing thread.
    RemoteSize(crate::remote::tree::SizeRequest),
    /// Compute a directory's size on a listing thread.
    DirSize {
        slot: usize,
        generation: u64,
        dir: PathBuf,
        name: std::ffi::OsString,
    },
    Quit,
}
