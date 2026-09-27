#![forbid(unsafe_code)]
//! The events that reach the UI thread over its one channel (design 3.1), and the effects
//! `App::update` asks the runtime to perform. The UI thread makes no filesystem syscalls:
//! everything that touches the filesystem is an effect that runs on another thread.

use crate::fsops::job::{JobSpec, Report};
use crate::fsops::question::{Answer, Progress, Question};
use crate::panel::listing::{Alive, ListRequest, ListingMsg};
use crate::theme::Palette;
use crossterm::event::KeyEvent;
use std::path::PathBuf;
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
    Key(KeyEvent),
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
    /// A handed-off child (command line, F3, F4) ended; the terminal is ours again.
    ChildDone {
        status: String,
        output: Option<Vec<u8>>,
    },
    /// Only while a spinner or a progress dialog is visible (P-5).
    Tick,
}

/// Work the runtime performs for `App::update`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    LoadTheme,
    List(ListRequest, Alive),
    /// Watch `dir` for the panel slot, or stop watching it (`None`).
    Watch {
        slot: usize,
        dir: Option<PathBuf>,
    },
    StartJob(JobSpec),
    CancelJob,
    /// Stop the process after restoring the terminal (`SIGTSTP`).
    SuspendSelf,
    /// Hand the terminal to a child (design section 6).
    Run(crate::cmdline::handoff::Handoff),
    /// `setsid -f xdg-open <path>` with stdio on /dev/null.
    Open(PathBuf),
    /// Compute a directory's size on a listing thread.
    DirSize {
        slot: usize,
        generation: u64,
        dir: PathBuf,
        name: std::ffi::OsString,
    },
    Quit,
}
