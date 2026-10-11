#![forbid(unsafe_code)]
//! App state, the event loop's `update(event) -> effects`, and the view (TDR-1 3.1).
//!
//! The UI thread owns all `App` state and makes no filesystem syscalls (P-1): anything
//! that touches the filesystem is an [`Effect`] the runtime performs on another thread.

mod actions;
pub mod archives;
mod compare;
pub mod event;
pub mod forms;
pub mod handoff;
pub mod jobs;
pub mod jump;
pub mod keys;
mod loads;
pub mod multirename;
pub mod quick;
pub mod remote;
pub mod runtime;
pub mod search;
pub mod signals;
pub mod state;
mod tabs;
pub mod term;
mod verbs;

use crate::cmdline::{self, Line};
use crate::compare::Mode;
use crate::config::{Config, ZoxideMode};
use crate::dirs::Dirs;
use crate::find::Search;
use crate::fsops::group::Group;
use crate::fsops::job::{Dest, JobVerb};
use crate::fsops::question::Progress;
use crate::fsops::rename::RenamedDir;
use crate::panel::listing::Alive;
use crate::panel::{LoadKind, Panel};
use crate::theme::{Depth, Palette, Theme};
use crate::ui::dialog::{Dialog, one_line};
use event::{Effect, Event, Sig};
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

/// At most four abandoned listing threads may exist (TDR-1 3.1).
pub const MAX_ABANDONED: usize = 4;

/// What a load or a refresh says when [`MAX_ABANDONED`] threads are blocked (TDR-1 3.1).
pub const TOO_MANY_BLOCKED: &str = "too many directory loads are blocked; wait for one to return";

/// What `Ctrl+R` says in a results tab whose cancelled search has not stopped (TDR-2 E-28).
pub const SEARCH_STOPPING: &str =
    "the search is still stopping; press Ctrl+R again when it has stopped";

/// One side of the screen: its tabs (TDR-1 5) and the active one.
pub struct Side {
    pub tabs: Vec<Panel>,
    pub active: usize,
}

impl Side {
    pub fn panel(&self) -> &Panel {
        &self.tabs[self.active]
    }

    pub fn panel_mut(&mut self) -> &mut Panel {
        &mut self.tabs[self.active]
    }
}

/// A running job as the UI sees it.
pub struct JobUi {
    pub verb: JobVerb,
    pub progress: Option<Progress>,
    pub started: Instant,
    pub cancel_requested: bool,
    pub cancel_at: Option<Instant>,
    /// The side the job's source was on; F7 moves that panel's cursor.
    pub side: usize,
}

/// A view copy being prepared as the UI sees it (TDR-3 3.4): not a job, so it runs while a job
/// runs; `Esc` cancels it.
pub struct ViewUi {
    /// Messages of any other preparation are dropped.
    pub id: u64,
    /// The entry's name, for the status row.
    pub name: Vec<u8>,
    /// F4: the editor, else the pager.
    pub edit: bool,
    pub done: u64,
    pub total: u64,
    pub cancel: Arc<AtomicBool>,
    pub alive: Alive,
    /// The hand-off's working directory: the panel's local directory.
    pub cwd: PathBuf,
    /// What a blocked preparation counts as (TDR-1 3.1): the archive.
    pub blocked: PathBuf,
    /// The server file of a remote copy, a `Dest::Remote` (the write-back, TDR-3 5.6).
    pub origin: Option<Dest>,
}

/// A walk that sizes a directory (`Space`): `Esc` and the next `Space` stop it. A local
/// walk whose thread still runs counts as abandoned (TDR-1 3.1); a walk on a server does
/// not (TDR-3 2.4).
struct SizeWalk {
    cancel: Arc<AtomicBool>,
    alive: Alive,
}

/// A running compare as the UI sees it (TDR-2 7).
pub struct CompareUi {
    /// Events of any other compare are dropped.
    pub id: u64,
    pub mode: Mode,
    pub progress: Option<crate::compare::Progress>,
}

#[derive(Clone, Debug)]
pub struct Status {
    pub text: String,
    pub error: bool,
}

pub struct App {
    pub sides: [Side; 2],
    pub active: usize,
    next_slot: usize,
    pub line: Line,
    pub history: cmdline::History,
    pub theme: Theme,
    pub palette: Option<Palette>,
    pub depth: Depth,
    pub config: Config,
    pub tz: jiff::tz::TimeZone,
    pub dialog: Option<Dialog>,
    /// Whether the command line has the focus (keys::map): typing goes to it rather than to
    /// the quick filter. `Ctrl+E` gives it; `Esc` and running the line take it back. A line
    /// that holds text always has it.
    pub line_focused: bool,
    /// Quick search prefix while Ctrl+S is active.
    pub search: Option<Vec<u8>>,
    /// The quick filter line while Ctrl+F has it open (TDR-2 4); its text is the active
    /// panel's filter.
    pub filter_line: Option<Line>,
    pub job: Option<JobUi>,
    /// The compare in progress (TDR-2 7).
    pub compare: Option<CompareUi>,
    next_compare: u64,
    pub status: Option<Status>,
    pub quit: bool,
    quit_after_job: bool,
    /// Clear and fully redraw on the next frame (theme reload, resume, resize).
    pub redraw: bool,
    abandoned: Vec<(PathBuf, Alive)>,
    pub home: PathBuf,
    /// Rows of the panel list at the last draw (PgUp/PgDn step).
    pub page: usize,
    /// What the last hand-off (TDR-1 6) reported when it returned, as the status row shows
    /// it: `[exit N]` after a shell command, nothing after a program that succeeded.
    pub last_cmd_status: Option<String>,
    /// Bookmarks, frecency and zoxide (TDR-2 3).
    pub dirs: Dirs,
    /// Panel slots whose navigation in flight records a visit when it completes (TDR-2 3.3).
    visiting: Vec<usize>,
    /// `z <keywords>` waiting for the store or zoxide, and the side it loads (TDR-2 3.4).
    pending_z: Option<(Vec<u8>, usize)>,
    /// The running search (TDR-2 2.3: at most one).
    pub find: Option<Arc<Search>>,
    /// Cancelled searches whose threads may still be blocked (TDR-2 2.3).
    abandoned_finds: Vec<Arc<Search>>,
    next_search: u64,
    /// The renames of the last multi-rename job that performed any, for `Ctrl+Z` in the
    /// tool; dropped when the undo runs and when the app exits (TDR-2 6.5).
    pub rename_undo: Option<Vec<RenamedDir>>,
    /// The view copy being prepared (TDR-3 3.4).
    pub view: Option<ViewUi>,
    /// The view copy handed to the pager or editor, checked when the hand-off returns.
    pub viewing: Option<crate::viewtemp::ViewFile>,
    /// The server file that view copy came from: an edited copy raises the write-back
    /// question (TDR-3 5.6).
    pub viewing_origin: Option<Dest>,
    /// View copies that F3 opened in their application (TDR-1 6): the application
    /// reads them after F3 returns, so they are checked, and removed unless edited, on
    /// exit.
    pub opened: Vec<crate::viewtemp::ViewFile>,
    next_view: u64,
    /// The quick view (TDR-3 4.1).
    pub quick: crate::preview::QuickView,
    /// The open SFTP sessions (TDR-3 5.7).
    pub pool: crate::remote::pool::Pool,
    /// The connect in flight and what it opens (TDR-3 5.2).
    pub pending: Option<remote::PendingConnect>,
    /// The walk that sizes a remote directory (TDR-3 2.4).
    remote_size: Option<SizeWalk>,
    /// The walk that sizes a local directory.
    size_walk: Option<SizeWalk>,
}

impl App {
    pub fn new(
        left: PathBuf,
        right: PathBuf,
        home: PathBuf,
        config: Config,
        palette: Option<Palette>,
        depth: Depth,
        tz: jiff::tz::TimeZone,
    ) -> App {
        let theme = Theme::build(palette.as_ref(), depth, config.paint_background);
        let dirs = Dirs::new(config.jump.zoxide == ZoxideMode::Auto);
        App {
            sides: [
                Side {
                    tabs: vec![Panel::new(0, left)],
                    active: 0,
                },
                Side {
                    tabs: vec![Panel::new(1, right)],
                    active: 0,
                },
            ],
            active: 0,
            next_slot: 2,
            line: Line::default(),
            history: cmdline::History::default(),
            theme,
            palette,
            depth,
            config,
            tz,
            dialog: None,
            line_focused: false,
            search: None,
            filter_line: None,
            job: None,
            compare: None,
            next_compare: 0,
            status: None,
            quit: false,
            quit_after_job: false,
            redraw: false,
            abandoned: Vec::new(),
            home,
            page: 10,
            last_cmd_status: None,
            dirs,
            visiting: Vec::new(),
            pending_z: None,
            find: None,
            abandoned_finds: Vec::new(),
            next_search: 0,
            rename_undo: None,
            view: None,
            viewing: None,
            viewing_origin: None,
            opened: Vec::new(),
            next_view: 0,
            quick: crate::preview::QuickView::default(),
            pool: crate::remote::pool::Pool::default(),
            pending: None,
            remote_size: None,
            size_walk: None,
        }
    }

    /// The effects that load both panels at startup: the visible tab of each side now,
    /// hidden (restored) tabs when they are shown. A path that no longer exists falls
    /// back to its nearest existing ancestor.
    pub fn start(&mut self) -> Vec<Effect> {
        let mut fx = Vec::new();
        for s in 0..2 {
            let dir = self.sides[s].panel().dir.clone();
            fx.extend(self.load(s, dir, None, true));
            let active = self.sides[s].active;
            for (k, p) in self.sides[s].tabs.iter_mut().enumerate() {
                if k != active {
                    p.released = true;
                }
            }
        }
        fx
    }

    pub fn panel(&self) -> &Panel {
        self.sides[self.active].panel()
    }

    pub fn panel_mut(&mut self) -> &mut Panel {
        self.sides[self.active].panel_mut()
    }

    pub fn other(&self) -> &Panel {
        self.sides[1 - self.active].panel()
    }

    /// Allocates a slot id for a new tab's panel.
    pub fn new_slot(&mut self) -> usize {
        self.next_slot += 1;
        self.next_slot - 1
    }

    fn find_slot(&mut self, slot: usize) -> Option<(usize, usize)> {
        for (s, side) in self.sides.iter().enumerate() {
            if let Some(t) = side.tabs.iter().position(|p| p.slot == slot) {
                return Some((s, t));
            }
        }
        None
    }

    fn slot_mut(&mut self, slot: usize) -> Option<&mut Panel> {
        let (s, t) = self.find_slot(slot)?;
        Some(&mut self.sides[s].tabs[t])
    }

    /// Whether the panel with this slot is on screen (NFR-RES: only visible tabs are
    /// watched).
    fn visible(&self, slot: usize) -> bool {
        self.sides.iter().any(|s| s.panel().slot == slot)
    }

    /// A tick runs only while a job, a load or a search is in progress (P-5).
    pub fn needs_tick(&self) -> bool {
        self.job.is_some()
            || self
                .sides
                .iter()
                .any(|s| s.panel().is_loading() || s.panel().searching())
    }

    pub fn say(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            error: false,
        });
    }

    pub fn warn(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            error: true,
        });
    }

    // ---- events -------------------------------------------------------------------------------

    /// Handles one event. The quick view follows the entry under the cursor after every
    /// event, whatever moved it (TDR-3 4.4).
    pub fn update(&mut self, ev: Event) -> Vec<Effect> {
        let fx = self.handle(ev);
        // Whatever put text on the line (typing, Alt+Enter, Alt+P, history, a paste) gave
        // it the focus.
        self.line_focused |= !self.line.is_empty();
        self.quick_sync();
        fx
    }

    fn handle(&mut self, ev: Event) -> Vec<Effect> {
        match ev {
            Event::Key(k, _) => self.on_key(k),
            Event::Resize(..) => {
                self.redraw = true;
                Vec::new()
            }
            Event::Paste(s) => {
                if let Some(d) = self.dialog.as_mut() {
                    d.paste(&s);
                    self.check_form();
                } else if let Some(l) = self.filter_line.as_mut() {
                    l.insert_bytes(one_line(&s).as_bytes());
                    let text = l.bytes().to_vec();
                    self.panel_mut().set_filter(&text);
                } else {
                    self.line.insert_bytes(one_line(&s).as_bytes());
                }
                Vec::new()
            }
            Event::Signal(Sig::Quit) => self.request_quit(true),
            Event::Signal(Sig::Suspend) => vec![Effect::SuspendSelf],
            Event::Signal(Sig::Resume) => {
                self.redraw = true;
                Vec::new()
            }
            Event::Signal(Sig::ReloadTheme) | Event::ReloadTheme => vec![Effect::LoadTheme],
            Event::ThemeLoaded { palette, requested } => {
                match palette {
                    Ok(p) if self.palette.as_ref() != Some(&p) => {
                        self.palette = Some(p);
                        self.theme = Theme::build(
                            self.palette.as_ref(),
                            self.depth,
                            self.config.paint_background,
                        );
                        self.redraw = true;
                        tracing::info!(
                            ms = requested.elapsed().as_secs_f64() * 1000.0,
                            "theme reload applied"
                        );
                    }
                    Ok(_) => tracing::debug!("theme reload: palette unchanged"),
                    // The current palette stays.
                    Err(e) => tracing::warn!("theme reload failed: {e}"),
                }
                Vec::new()
            }
            Event::Listing(m) => self.on_listing(m),
            Event::DirChanged { slot } => match self.find_slot(slot) {
                Some((s, t)) if t == self.sides[s].active => {
                    let p = self.sides[s].panel_mut();
                    match p.loading.as_ref().map(|l| l.kind) {
                        None => self.refresh_slot(s),
                        // The refresh in flight may have read the directory before the
                        // change: one more follows it.
                        Some(LoadKind::Refresh) => {
                            p.stale = true;
                            Vec::new()
                        }
                        // A navigation lists its own directory, and watches it once listed.
                        Some(LoadKind::Navigate) => Vec::new(),
                    }
                }
                _ => Vec::new(),
            },
            Event::Job(j) => self.on_job(j),
            Event::Compare(m) => self.on_compare(m),
            Event::Find(m) => self.on_find(m),
            Event::DirsLoaded(store) => self.on_dirs_loaded(store),
            Event::ZoxideLoaded(list) => self.on_zoxide_loaded(list),
            Event::Status(text) => {
                self.warn(text);
                Vec::new()
            }
            Event::View(m) => self.on_view(m),
            Event::ChildDone { status, .. } => {
                self.last_cmd_status = Some(status.clone());
                self.redraw = true;
                self.say(status);
                let mut fx = self.refresh_both();
                // A view copy is kept when it was edited, and removed otherwise (TDR-3 3.4); an
                // edited copy of a remote file raises the write-back question (TDR-3 5.6).
                let origin = self.viewing_origin.take();
                if let Some(file) = self.viewing.take() {
                    match origin {
                        Some(at) if file.remote => fx.push(Effect::CheckEdited(file, at)),
                        _ => fx.push(Effect::CheckView(file)),
                    }
                }
                fx
            }
            Event::Preview(m) => self.on_preview(m),
            Event::Remote(m) => self.on_remote(m),
            Event::Tick => self.quick_tick(Instant::now()),
        }
    }
}

/// What a confirmation calls the selection: the one entry's name (its path relative to the
/// panel, for a group below it), or "N entries". Only a local group's `sub` is relative to
/// the panel (a results tab); an archive's or a server's `sub` is the panel's own location.
fn count_text(groups: &[Group]) -> String {
    let total: usize = groups.iter().map(|g| g.names.len()).sum();
    match groups {
        [g] if total == 1 => {
            let sub: &[OsString] = if g.root.local().is_some() {
                &g.sub
            } else {
                &[]
            };
            let mut rel = Vec::new();
            for c in sub.iter().chain(&g.names) {
                if !rel.is_empty() {
                    rel.push(b'/');
                }
                rel.extend_from_slice(c.as_bytes());
            }
            format!("\"{}\"", crate::ui::text::escaped(&rel))
        }
        _ => format!("{total} entries"),
    }
}
