#![forbid(unsafe_code)]
//! App state, the event loop's `update(event) -> effects`, and the view (design 3.1).
//!
//! The UI thread owns all `App` state and makes no filesystem syscalls (P-1): anything
//! that touches the filesystem is an [`Effect`] the runtime performs on another thread.

pub mod archives;
pub mod event;
pub mod forms;
pub mod handoff;
pub mod jobs;
pub mod jump;
pub mod keys;
pub mod multirename;
pub mod runtime;
pub mod search;
pub mod signals;
pub mod state;
pub mod term;

use crate::cmdline::handoff::{Handoff, editors, pager, program_argv};
use crate::cmdline::{self, Command, Line, ProcessEnv, quote};
use crate::compare::{CompareMsg, Marks, Mode};
use crate::config::{Config, ZoxideMode};
use crate::dirs::Dirs;
use crate::find::Search;
use crate::fsops::group::Group;
use crate::fsops::job::{Dest, JobSpec, JobVerb, Report};
use crate::fsops::question::{Phase, Progress};
use crate::fsops::rename::RenamedDir;
use crate::panel::entry::EKind;
use crate::panel::listing::{Alive, ListingMsg};
use crate::panel::{Panel, Record, Row, join_lexical};
use crate::theme::{Depth, Palette, Theme};
use crate::ui::dialog::{Dialog, Outcome, Purpose, edit};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use event::{Effect, Event, JobEvent, Sig};
use keys::Action;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// At most four abandoned listing threads may exist (design 3.1).
pub const MAX_ABANDONED: usize = 4;

/// What a load or a refresh says when [`MAX_ABANDONED`] threads are blocked (design 3.1).
pub const TOO_MANY_BLOCKED: &str = "too many directory loads are blocked; wait for one to return";

/// What `Ctrl+R` says in a results tab whose cancelled search has not stopped (E-28).
pub const SEARCH_STOPPING: &str =
    "the search is still stopping; press Ctrl+R again when it has stopped";

/// One side of the screen: its tabs (M2) and the active one.
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

/// A running compare as the UI sees it (P2 7).
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
    /// Quick search prefix while Ctrl+S is active.
    pub search: Option<Vec<u8>>,
    /// The quick filter line while Ctrl+F has it open (P2 4); its text is the active
    /// panel's filter.
    pub filter_line: Option<Line>,
    pub job: Option<JobUi>,
    /// The compare in progress (P2 7).
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
    /// Directories the job touched, refreshed when it ends.
    pub last_cmd_status: Option<String>,
    /// Bookmarks, frecency and zoxide (P2 3).
    pub dirs: Dirs,
    /// Panel slots whose navigation in flight records a visit when it completes (P2 3.3).
    visiting: Vec<usize>,
    /// `z <keywords>` waiting for the store or zoxide, and the side it loads (P2 3.4).
    pending_z: Option<(Vec<u8>, usize)>,
    /// The running search (P2 2.3: at most one).
    pub find: Option<Arc<Search>>,
    /// Cancelled searches whose threads may still be blocked (P2 2.3).
    abandoned_finds: Vec<Arc<Search>>,
    next_search: u64,
    /// The renames of the last multi-rename job that performed any, for `Ctrl+Z` in the
    /// tool; dropped when the undo runs and when the app exits (P2 6.5).
    pub rename_undo: Option<Vec<RenamedDir>>,
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

    // ---- loads ------------------------------------------------------------------------------

    /// Loads `dir` into the active tab of `side`, subject to the stuck-load limits.
    fn load(
        &mut self,
        side: usize,
        dir: PathBuf,
        cursor_to: Option<Vec<u8>>,
        fallback: bool,
    ) -> Vec<Effect> {
        let record = if fallback { Record::No } else { Record::New };
        self.load_ex(side, dir, cursor_to, fallback, record)
    }

    /// `record`: where the place left goes in the history.
    fn load_ex(
        &mut self,
        side: usize,
        dir: PathBuf,
        cursor_to: Option<Vec<u8>>,
        fallback: bool,
        record: Record,
    ) -> Vec<Effect> {
        self.abandoned.retain(|(_, a)| a.is_running());
        if self.abandoned.iter().any(|(d, _)| *d == dir) {
            self.warn(format!(
                "{}: previous load of this directory is still blocked",
                dir.display()
            ));
            return Vec::new();
        }
        if self.abandoned.len() >= MAX_ABANDONED {
            self.warn(TOO_MANY_BLOCKED);
            return Vec::new();
        }
        let p = self.sides[side].panel_mut();
        if let Some(l) = &p.loading
            && l.alive.is_running()
            && p.is_loading()
        {
            self.abandoned.push((p.blocked_path(), l.alive.clone()));
        }
        if side == self.active {
            // The filter line edits the filter of the directory on screen.
            self.filter_line = None;
        }
        // Leaving a results tab's results stops its search (P2 5.4).
        self.leave_results(side);
        let p = self.sides[side].panel_mut();
        let alive = Alive::running();
        let req = p.navigate_full(dir, cursor_to, alive.clone(), fallback, record);
        // The user's navigations are visits; the app's own fallback loads are not.
        self.mark_visit(req.slot, !fallback);
        vec![Effect::List(req, alive)]
    }

    /// Refreshes the active tab of `side`: a directory is listed again, a results tab is
    /// re-stated (P2 5.5). A load still in flight is replaced; while its thread runs it
    /// counts as abandoned, and at [`MAX_ABANDONED`] the refresh is refused (design 3.1).
    fn refresh_slot(&mut self, side: usize) -> Vec<Effect> {
        let p = self.sides[side].panel();
        // An archive re-lists from its index after a key check (P3 3.2); a server re-lists
        // through its session (T6). Its local `dir` is not what it shows.
        if p.archive().is_some() || p.archive_loading() {
            return self.refresh_archive(side, crate::archive::Check::Refresh);
        }
        if p.remote().is_some() {
            return Vec::new();
        }
        // A results tab is not re-stated while its search can still add to it, also after
        // a cancel, until its threads have recorded their totals: the re-stat's copy would
        // miss the batches still to come (E-28).
        if p.is_results() && p.search_pending() {
            return Vec::new();
        }
        if let Some(l) = &p.loading
            && l.alive.is_running()
        {
            let stuck = (p.dir.clone(), l.alive.clone());
            self.abandoned.retain(|(_, a)| a.is_running());
            if self.abandoned.len() >= MAX_ABANDONED {
                self.warn(TOO_MANY_BLOCKED);
                return Vec::new();
            }
            self.abandoned.push(stuck);
        }
        let p = self.sides[side].panel_mut();
        let alive = Alive::running();
        if p.is_results() {
            let req = p.restat(alive.clone());
            return vec![Effect::Restat(req, alive)];
        }
        let req = p.refresh(alive.clone());
        vec![Effect::List(req, alive)]
    }

    fn refresh_both(&mut self) -> Vec<Effect> {
        let mut fx = self.refresh_slot(0);
        fx.extend(self.refresh_slot(1));
        fx
    }

    fn on_listing(&mut self, msg: ListingMsg) -> Vec<Effect> {
        let mut fx = Vec::new();
        match msg {
            ListingMsg::Batch {
                slot,
                generation,
                entries,
                names,
            } => {
                if let Some(p) = self.slot_mut(slot) {
                    p.on_batch(generation, entries, &names);
                }
            }
            ListingMsg::Listing {
                slot,
                generation,
                listing,
            } => {
                if let Some(p) = self.slot_mut(slot) {
                    p.on_listing(generation, *listing);
                }
            }
            ListingMsg::Done {
                slot,
                generation,
                dir,
                elapsed,
            } => {
                tracing::debug!(slot, ms = elapsed.as_secs_f64() * 1000.0, "listing done");
                let visible = self.visible(slot);
                let Some(p) = self.slot_mut(slot) else {
                    return fx;
                };
                // A completed navigation (not a refresh) is a visit (P2 3.3).
                let navigation = p.generation == generation
                    && p.loading
                        .as_ref()
                        .is_some_and(|l| l.kind == crate::panel::LoadKind::Navigate);
                let done = p.on_done(generation, dir);
                // A results tab holds no watch (NFR-RES), nor does an archive (P3 2.6).
                let watched = p.is_directory();
                let archive = p.archive().is_some();
                if navigation && !archive {
                    let listed = p.dir.clone();
                    self.visited(slot, &listed);
                }
                if let Some(d) = done
                    && visible
                    && watched
                {
                    fx.push(Effect::Watch { slot, dir: Some(d) });
                }
                if navigation && archive {
                    fx.push(Effect::Watch { slot, dir: None });
                }
            }
            ListingMsg::Failed {
                slot,
                generation,
                dir,
                error,
                gone,
            } => {
                let Some((s, t)) = self.find_slot(slot) else {
                    return fx;
                };
                let p = &mut self.sides[s].tabs[t];
                if generation != p.generation {
                    return fx;
                }
                let refreshing = p
                    .loading
                    .as_ref()
                    .is_some_and(|l| l.kind == crate::panel::LoadKind::Refresh);
                if gone && refreshing {
                    // The current directory was deleted: go to the nearest existing
                    // ancestor.
                    p.loading = None;
                    let parent = dir
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| PathBuf::from("/"));
                    let name = p.dir_name();
                    if t == self.sides[s].active {
                        fx.extend(self.load(s, parent, name, true));
                    }
                } else {
                    let navigation = p
                        .loading
                        .as_ref()
                        .is_some_and(|l| l.kind == crate::panel::LoadKind::Navigate);
                    p.on_failed(generation, error);
                    self.mark_visit(slot, false);
                    // A directory that no longer exists loses its frecency entry; a
                    // bookmark is never dropped (P2 3.1).
                    if gone && navigation {
                        self.dirs.forget(&dir);
                    }
                }
            }
            ListingMsg::LinkTargets {
                slot,
                generation,
                kinds,
            } => {
                if let Some(p) = self.slot_mut(slot) {
                    p.on_links(generation, &kinds);
                }
            }
            ListingMsg::FreeSpace {
                slot,
                generation,
                free,
                total,
            } => {
                if let Some(p) = self.slot_mut(slot)
                    && p.generation == generation
                {
                    p.free = Some((free, total));
                }
            }
            ListingMsg::DirSize {
                slot,
                generation,
                name,
                bytes,
            } => {
                if let Some(p) = self.slot_mut(slot)
                    && p.generation == generation
                {
                    p.on_dir_size(&name, bytes);
                }
            }
            ListingMsg::Opened {
                slot,
                generation,
                index,
            } => {
                if let Some(p) = self.slot_mut(slot) {
                    p.on_opened(generation, index);
                }
            }
            ListingMsg::Reset { slot, generation } => {
                if let Some(p) = self.slot_mut(slot) {
                    p.on_reset(generation);
                }
            }
            ListingMsg::Changed {
                slot,
                generation,
                rescan,
            } => fx.extend(self.on_archive_changed(slot, generation, rescan)),
        }
        fx
    }

    // ---- events -------------------------------------------------------------------------------

    pub fn update(&mut self, ev: Event) -> Vec<Effect> {
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
                    l.insert_bytes(s.replace('\n', " ").as_bytes());
                    let text = l.bytes().to_vec();
                    self.panel_mut().set_filter(&text);
                } else {
                    self.line.insert_bytes(s.replace('\n', " ").as_bytes());
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
                Some((s, t))
                    if t == self.sides[s].active && self.sides[s].panel().loading.is_none() =>
                {
                    self.refresh_slot(s)
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
            Event::ChildDone { status, .. } => {
                self.last_cmd_status = Some(status.clone());
                self.redraw = true;
                self.say(status);
                self.refresh_both()
            }
            Event::Tick => Vec::new(),
        }
    }

    fn request_quit(&mut self, forced: bool) -> Vec<Effect> {
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

    fn on_job(&mut self, j: JobEvent) -> Vec<Effect> {
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
        // kept (P2 6.5). An undo job's own record is not.
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
            self.dialog = Some(Dialog::Report {
                report: r,
                scroll: 0,
            });
        } else {
            self.say(summary);
        }
        // The cursor names survive the refresh.
        self.refresh_both()
    }

    fn on_compare(&mut self, m: CompareMsg) -> Vec<Effect> {
        let current = self.compare.as_ref().map(|c| c.id);
        match m {
            CompareMsg::Progress { id, progress } if Some(id) == current => {
                if let Some(c) = self.compare.as_mut() {
                    c.progress = Some(progress);
                }
            }
            CompareMsg::Marks {
                id,
                listings,
                marks,
            } if Some(id) == current => self.apply_compare(listings, marks),
            CompareMsg::Done { id, error } if Some(id) == current => {
                self.compare = None;
                if let Some(e) = error {
                    self.warn(format!("compare: {e}"));
                }
            }
            // A cancelled or replaced compare.
            _ => {}
        }
        Vec::new()
    }

    /// Applies compare marks (P2 7) when both panels still show the listings `(slot,
    /// generation)` the request was made from; otherwise nothing is marked. Existing marks
    /// in both panels are cleared first.
    fn apply_compare(&mut self, listings: [(usize, u64); 2], marks: Marks) {
        let same = (0..2).all(|s| {
            let p = self.sides[s].panel();
            p.slot == listings[s].0 && p.listing_generation() == Some(listings[s].1)
        });
        if !same {
            self.warn("the directories changed; compare again");
            return;
        }
        self.sides[0].panel_mut().replace_marks(&marks.left);
        self.sides[1].panel_mut().replace_marks(&marks.right);
        self.say(marks.summary.to_string());
    }

    /// Progress text of a running compare for the status row.
    pub fn compare_line(&self) -> Option<String> {
        let c = self.compare.as_ref()?;
        let mut s = match c.mode {
            Mode::DateSize => "compare by date and size".to_string(),
            Mode::Content => "compare by content".to_string(),
        };
        match &c.progress {
            Some(p) => {
                s += &format!(": {}/{} pairs", p.pairs_done, p.pairs_total);
                if p.bytes_total > 0 {
                    let pct = p.bytes_done as f64 * 100.0 / p.bytes_total as f64;
                    s += &format!(", {pct:.0}%");
                }
            }
            None => s += ": running",
        }
        s += "   Esc: cancel";
        Some(s)
    }

    fn start_job(&mut self, spec: JobSpec) -> Vec<Effect> {
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

    // ---- keys ---------------------------------------------------------------------------------

    fn on_key(&mut self, k: KeyEvent) -> Vec<Effect> {
        self.status = None;
        if let Some(d) = self.dialog.as_mut() {
            return match d.handle(k) {
                Outcome::Stay => Vec::new(),
                Outcome::Close => {
                    self.dialog = None;
                    Vec::new()
                }
                Outcome::Done(p, text) => {
                    self.dialog = None;
                    self.dialog_done(p, text)
                }
                Outcome::FormChanged => {
                    self.check_form();
                    Vec::new()
                }
                Outcome::FormSubmit => self.submit_form(),
                Outcome::Dirs(a) => self.on_dirs_action(a),
                Outcome::Undo => self.undo_rename(),
            };
        }
        if let Some(prefix) = self.search.as_mut() {
            match k.code {
                KeyCode::Char('s')
                    if k.modifiers
                        .contains(crossterm::event::KeyModifiers::CONTROL) =>
                {
                    let pre = prefix.clone();
                    let p = self.panel_mut();
                    if let Some(r) = p.find_prefix(&pre, true) {
                        p.cursor_to(r);
                    }
                    return Vec::new();
                }
                KeyCode::Char(c)
                    if !k.modifiers.intersects(
                        crossterm::event::KeyModifiers::CONTROL
                            | crossterm::event::KeyModifiers::ALT,
                    ) =>
                {
                    let mut b = [0u8; 4];
                    prefix.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                    let pre = prefix.clone();
                    let p = self.panel_mut();
                    if let Some(r) = p.find_prefix(&pre, false) {
                        p.cursor_to(r);
                    }
                    return Vec::new();
                }
                KeyCode::Backspace => {
                    prefix.pop();
                    return Vec::new();
                }
                KeyCode::Esc | KeyCode::Enter => {
                    self.search = None;
                    return Vec::new();
                }
                _ => self.search = None,
            }
        }
        if self.filter_line.is_some() && self.filter_key(k) {
            return Vec::new();
        }
        let a = keys::map(k, self.line.is_empty());
        self.act(a)
    }

    /// A key while the filter line is open (P2 4). Returns whether the line took it.
    /// `Enter` and `Ctrl+F` close the line and keep the filter; `Esc` clears the filter and
    /// closes; the line-editing keys edit it and re-filter at once. `Up`, `Down`, `PgUp`
    /// and `PgDn` move the panel cursor with the line open. Any other key closes the line,
    /// keeping the filter, and then acts as usual.
    fn filter_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let Some(line) = self.filter_line.as_mut() else {
            return false;
        };
        match k.code {
            KeyCode::Enter if !alt => {}
            KeyCode::Char('f') if ctrl => {}
            KeyCode::Esc => self.panel_mut().set_filter(b""),
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown if !alt => {
                return false;
            }
            _ if !alt && edit(line, k, ctrl) => {
                let text = line.bytes().to_vec();
                self.panel_mut().set_filter(&text);
                return true;
            }
            _ => {
                self.filter_line = None;
                return false;
            }
        }
        self.filter_line = None;
        true
    }

    fn act(&mut self, a: Action) -> Vec<Effect> {
        // Verbs by place (P3 2.4): refused before any work.
        if let Some(why) = jobs::refusal(a, jobs::At::of(self.panel()), jobs::At::of(self.other()))
        {
            self.warn(why);
            return Vec::new();
        }
        let page = self.page.max(1) as isize;
        match a {
            Action::None => Vec::new(),
            Action::Up => self.cursor(-1),
            Action::Down => self.cursor(1),
            Action::PageUp => self.cursor(-page),
            Action::PageDown => self.cursor(page),
            Action::First => {
                self.panel_mut().cursor_to(0);
                Vec::new()
            }
            Action::Last => {
                let r = self.panel().rows();
                self.panel_mut().cursor_to(r.saturating_sub(1));
                Vec::new()
            }
            Action::SwitchPanel => {
                self.active = 1 - self.active;
                Vec::new()
            }
            Action::SwapPanels => {
                self.sides.swap(0, 1);
                Vec::new()
            }
            Action::Enter if self.panel().is_results() => self.enter_result(),
            Action::Enter => self.enter(),
            Action::Parent if self.panel().is_results() => self.results_parent(),
            Action::Parent => self.parent(),
            Action::Mkdir | Action::EditNew | Action::Compare if self.panel().is_results() => {
                self.warn(search::NOT_IN_RESULTS);
                Vec::new()
            }
            Action::Find => self.find_form(),
            Action::Escape => {
                let p = self.panel_mut();
                let slot = p.slot;
                if let Some((dir, alive)) = p.cancel_load() {
                    self.mark_visit(slot, false);
                    if alive.is_running() {
                        self.abandoned.push((dir, alive));
                    }
                } else if self.cancel_active_search() {
                } else if self.compare.take().is_some() {
                    self.say("compare cancelled");
                    return vec![Effect::CancelCompare];
                } else if self.job.is_some() {
                    self.dialog = Some(Dialog::confirm(
                        "Cancel job",
                        vec!["Cancel the running job?".into()],
                        "Cancel job",
                        Purpose::CancelJob,
                    ));
                }
                Vec::new()
            }
            Action::MarkAndDown => {
                self.panel_mut().toggle_mark(true);
                Vec::new()
            }
            Action::OpenArchive => self.open_as_archive(),
            Action::MarkSpace if self.panel().archive().is_some() => self.archive_size(),
            Action::MarkSpace => {
                let p = self.panel();
                let (slot, generation, dir) = (p.slot, p.generation, p.dir.clone());
                // A directory's size in an archive comes from the index, and on a server
                // from a walk there (P3 2.4, T2 and T6): no local walk.
                let local = p.archive().is_none() && p.remote().is_none();
                let dir_name = match p.current_entry() {
                    Some((i, e)) if e.kind == EKind::Dir && local => {
                        Some(OsStr::from_bytes(p.list.name(i)).to_owned())
                    }
                    _ => None,
                };
                self.panel_mut().toggle_mark(false);
                match dir_name {
                    Some(name) => vec![Effect::DirSize {
                        slot,
                        generation,
                        dir,
                        name,
                    }],
                    None => Vec::new(),
                }
            }
            Action::MarkAll => {
                self.panel_mut().mark_all();
                Vec::new()
            }
            Action::InvertMarks => {
                self.panel_mut().invert_marks();
                Vec::new()
            }
            Action::MarkGlob => {
                self.dialog = Some(Dialog::input(
                    "Mark",
                    vec!["Mark entries matching:".into()],
                    b"*",
                    Purpose::MarkGlob,
                ));
                Vec::new()
            }
            Action::UnmarkGlob => {
                self.dialog = Some(Dialog::input(
                    "Unmark",
                    vec!["Unmark entries matching:".into()],
                    b"*",
                    Purpose::UnmarkGlob,
                ));
                Vec::new()
            }
            Action::QuickSearch => {
                self.search = Some(Vec::new());
                Vec::new()
            }
            Action::Filter => {
                let mut l = Line::default();
                l.set(self.panel().filter.text());
                self.filter_line = Some(l);
                Vec::new()
            }
            Action::Compare => self.compare_form(),
            Action::Directories => self.dirs_dialog(b""),
            Action::ToggleHidden => {
                self.panel_mut().toggle_hidden();
                Vec::new()
            }
            Action::Reread => {
                if self.panel().search().is_some_and(|s| s.stopping()) {
                    self.say(SEARCH_STOPPING);
                }
                // Ctrl+R in an archive reads it again when it changed (P3 2.4).
                let side = self.active;
                if self.panel().archive().is_some() {
                    let mut fx = self.refresh_archive(side, crate::archive::Check::Rescan);
                    fx.extend(self.refresh_slot(1 - side));
                    return fx;
                }
                self.refresh_both()
            }
            Action::HistoryBack => self.history_move(true),
            Action::HistoryForward => self.history_move(false),
            Action::Sort(key) => {
                self.panel_mut().set_sort(key);
                Vec::new()
            }
            Action::InsertName | Action::InsertPath => {
                let p = self.panel();
                let bytes = match p.current() {
                    Some(Row::Entry(i)) => {
                        let n = p.list.name(i);
                        if a == Action::InsertPath {
                            p.path_of(n).as_os_str().as_bytes().to_vec()
                        } else {
                            n.to_vec()
                        }
                    }
                    _ => return Vec::new(),
                };
                let mut q = quote(&bytes);
                q.push(b' ');
                self.line.insert_bytes(&q);
                Vec::new()
            }
            Action::ShowOutput => vec![Effect::Run(Handoff::ShowScreen)],
            Action::Help => {
                self.dialog = Some(Dialog::Help { scroll: 0 });
                Vec::new()
            }
            Action::View => self.view_edit(false),
            Action::Edit => self.view_edit(true),
            Action::EditNew => {
                let dir = self.panel().dir.clone();
                self.dialog = Some(Dialog::input(
                    "Edit new file",
                    vec!["File name:".into()],
                    b"",
                    Purpose::EditNew { dir },
                ));
                Vec::new()
            }
            Action::Copy | Action::Move => self.copy_move(a == Action::Move),
            Action::Rename => {
                let p = self.panel();
                let Some(name) = p.current_name() else {
                    return Vec::new();
                };
                // A result is renamed in its own directory (P2 5.4): one group with its
                // `sub`, and the leaf as the name.
                let Some(group) =
                    Group::from_relative(&p.dir, [OsStr::from_bytes(name).to_owned()]).pop()
                else {
                    return Vec::new();
                };
                let leaf = group.names[0].as_bytes().to_vec();
                self.dialog = Some(Dialog::input(
                    "Rename",
                    vec!["New name:".into()],
                    &leaf,
                    Purpose::Rename { group },
                ));
                Vec::new()
            }
            Action::Mkdir => {
                let dir = self.panel().dir.clone();
                self.dialog = Some(Dialog::input(
                    "Make directory",
                    vec!["Name (a/b/c creates parents):".into()],
                    b"",
                    Purpose::Mkdir { dir },
                ));
                Vec::new()
            }
            Action::Trash => {
                let groups = self.panel().selection_groups();
                if groups.is_empty() {
                    return Vec::new();
                }
                let text = count_text(&groups);
                let purpose = Purpose::Trash { groups };
                self.dialog = Some(Dialog::confirm(
                    "Trash",
                    vec![format!("Move {text} to trash?")],
                    "Trash",
                    purpose,
                ));
                Vec::new()
            }
            Action::Delete => {
                let groups = self.panel().selection_groups();
                if groups.is_empty() {
                    return Vec::new();
                }
                let text = count_text(&groups);
                let purpose = Purpose::Delete { groups };
                self.dialog = Some(Dialog::confirm(
                    "Delete permanently",
                    vec![
                        format!("Permanently delete {text}?"),
                        "The next step counts the files and asks you to type delete.".into(),
                    ],
                    "Continue",
                    purpose,
                ));
                Vec::new()
            }
            Action::Quit => self.request_quit(false),
            Action::Link => self.link_form(),
            Action::Attributes => self.attr_form(),
            Action::MultiRename => self.rename_tool(),
            Action::NewTab
            | Action::CloseTab
            | Action::PrevTab
            | Action::NextTab
            | Action::GotoTab(_) => self.tab_action(a),
            // Command line.
            Action::LineChar(c) => {
                self.line.insert_char(c);
                self.history.reset();
                Vec::new()
            }
            Action::LineRun => self.run_line(),
            Action::LineBackspace => {
                self.line.backspace();
                Vec::new()
            }
            Action::LineDelete => {
                self.line.delete();
                Vec::new()
            }
            Action::LineLeft => {
                self.line.left();
                Vec::new()
            }
            Action::LineRight => {
                self.line.right();
                Vec::new()
            }
            Action::LineHome => {
                self.line.home();
                Vec::new()
            }
            Action::LineEnd => {
                self.line.end();
                Vec::new()
            }
            Action::LineKillStart => {
                self.line.kill_start();
                Vec::new()
            }
            Action::LineKillEnd => {
                self.line.kill_end();
                Vec::new()
            }
            Action::LineKillWord => {
                self.line.kill_word();
                Vec::new()
            }
            Action::LineClear => {
                self.line.clear();
                self.history.reset();
                Vec::new()
            }
            Action::HistoryPrev => {
                self.history.prev(&mut self.line);
                Vec::new()
            }
            Action::HistoryNext => {
                self.history.next(&mut self.line);
                Vec::new()
            }
        }
    }

    fn cursor(&mut self, d: isize) -> Vec<Effect> {
        self.panel_mut().move_cursor(d);
        Vec::new()
    }

    fn enter(&mut self) -> Vec<Effect> {
        if self.panel().archive().is_some() {
            return self.archive_enter();
        }
        let p = self.panel();
        match p.current() {
            Some(Row::Parent) => self.parent(),
            Some(Row::Entry(i)) => {
                let e = p.list.entries[i as usize];
                let name = p.list.name(i).to_vec();
                let path = p.path_of(&name);
                if e.kind == EKind::Dir
                    || (e.kind == EKind::Symlink
                        && e.link != crate::panel::entry::LinkKind::File
                        && e.link != crate::panel::entry::LinkKind::Broken)
                {
                    let side = self.active;
                    self.load(side, path, None, false)
                } else if e.kind == EKind::File
                    && let Some(fx) = self.enter_archive_by_name(&name)
                {
                    // A recognised archive name is browsed (P3 3.1).
                    fx
                } else {
                    vec![Effect::Open(path)]
                }
            }
            None => Vec::new(),
        }
    }

    fn parent(&mut self) -> Vec<Effect> {
        if self.panel().archive().is_some() {
            return self.archive_parent();
        }
        let p = self.panel();
        let Some(parent) = p.dir.parent().map(Path::to_path_buf) else {
            return Vec::new();
        };
        let name = p.dir_name();
        let side = self.active;
        self.load(side, parent, name, false)
    }

    fn view_edit(&mut self, edit: bool) -> Vec<Effect> {
        let p = self.panel();
        let Some((i, e)) = p.current_entry() else {
            return Vec::new();
        };
        if e.kind == EKind::Dir {
            return Vec::new();
        }
        let path = p.path_of(p.list.name(i));
        let cwd = p.dir.clone();
        self.program(edit, &path, cwd)
    }

    fn program(&mut self, edit: bool, path: &Path, cwd: PathBuf) -> Vec<Effect> {
        let cmds = if edit {
            editors(
                self.config.editor.as_deref(),
                std::env::var_os("EDITOR").as_deref(),
            )
        } else {
            vec![pager(
                self.config.pager.as_deref(),
                std::env::var_os("PAGER").as_deref(),
            )]
        };
        let argvs: Vec<Vec<OsString>> = cmds.iter().filter_map(|c| program_argv(c, path)).collect();
        match argvs.into_iter().next() {
            Some(argv) => vec![Effect::Run(Handoff::Program { argv, cwd })],
            None => {
                self.warn("$PAGER or $EDITOR does not parse");
                Vec::new()
            }
        }
    }

    fn copy_move(&mut self, moving: bool) -> Vec<Effect> {
        let p = self.panel();
        let groups = p.selection_groups();
        if groups.is_empty() {
            return Vec::new();
        }
        let dir = p.dir.clone();
        let links = p
            .selection_kinds()
            .iter()
            .filter(|k| **k == EKind::Symlink)
            .count();
        let mut dst = self.other().dir.as_os_str().as_bytes().to_vec();
        if !dst.ends_with(b"/") {
            dst.push(b'/');
        }
        let verb = if moving { "Move" } else { "Copy" };
        let mut lines = vec![format!("{verb} {} to:", count_text(&groups))];
        if links > 0 && !moving {
            lines.insert(1, format!("{links} symbolic link(s) are copied as links."));
        }
        let purpose = if moving {
            Purpose::Move { dir, groups }
        } else {
            Purpose::Copy { dir, groups }
        };
        self.dialog = Some(Dialog::input(verb, lines, &dst, purpose));
        Vec::new()
    }

    fn dialog_done(&mut self, p: Purpose, text: Vec<u8>) -> Vec<Effect> {
        let typed = PathBuf::from(OsStr::from_bytes(&text));
        match p {
            Purpose::QuitWithJob => {
                self.quit_after_job = true;
                if let Some(j) = self.job.as_mut() {
                    j.cancel_requested = true;
                    j.cancel_at.get_or_insert_with(Instant::now);
                }
                vec![Effect::CancelJob]
            }
            Purpose::CancelJob => {
                if let Some(j) = self.job.as_mut() {
                    j.cancel_requested = true;
                    j.cancel_at.get_or_insert_with(Instant::now);
                }
                vec![Effect::CancelJob]
            }
            Purpose::Copy { dir, groups } => {
                let dst = Dest::Local(join_lexical(&dir, &typed));
                self.start_job(JobSpec::Copy { groups, dst })
            }
            Purpose::Move { dir, groups } => {
                let dst = Dest::Local(join_lexical(&dir, &typed));
                self.start_job(JobSpec::Move { groups, dst })
            }
            Purpose::Rename { group } => {
                if text.is_empty()
                    || text.contains(&b'/')
                    || group.names.first().map(|n| n.as_bytes()) == Some(&text[..])
                {
                    return Vec::new();
                }
                // Shift+F6 is a move of one group with one name (P2 2.2), in the group's
                // own directory.
                let dst = Dest::Local(group.dir_path().join(OsStr::from_bytes(&text)));
                self.start_job(JobSpec::Move {
                    groups: vec![group],
                    dst,
                })
            }
            Purpose::Mkdir { dir } => {
                if text.is_empty() {
                    return Vec::new();
                }
                self.start_job(JobSpec::Mkdir {
                    dir,
                    name: OsStr::from_bytes(&text).to_owned(),
                })
            }
            Purpose::Trash { groups } => self.start_job(JobSpec::Trash { groups }),
            Purpose::Delete { groups } => self.start_job(JobSpec::Delete { groups }),
            Purpose::EditNew { dir } => {
                if text.is_empty() || text.contains(&b'/') {
                    return Vec::new();
                }
                let path = dir.join(OsStr::from_bytes(&text));
                self.program(true, &path, dir)
            }
            Purpose::MarkGlob => {
                self.panel_mut().mark_glob(&text, true);
                Vec::new()
            }
            Purpose::UnmarkGlob => {
                self.panel_mut().mark_glob(&text, false);
                Vec::new()
            }
        }
    }

    fn run_line(&mut self) -> Vec<Effect> {
        let text = self.line.take();
        self.history.push(&text);
        match cmdline::parse(&text, &ProcessEnv) {
            Command::Cd(p) => {
                if let Some(fx) = self.archive_cd(&p) {
                    return fx;
                }
                let dir = join_lexical(&self.panel().dir, &p);
                let side = self.active;
                self.load(side, dir, None, false)
            }
            Command::Z(keywords) => self.z(keywords),
            Command::Shell(t) => vec![Effect::Run(Handoff::Shell {
                shell: cmdline::shell(&ProcessEnv),
                text: t,
                cwd: self.panel().dir.clone(),
            })],
        }
    }

    // ---- tabs (M2) ------------------------------------------------------------------------

    fn tab_action(&mut self, a: Action) -> Vec<Effect> {
        let side = self.active;
        let mut fx = Vec::new();
        let old_slot = self.sides[side].panel().slot;
        let n = self.sides[side].tabs.len();
        match a {
            Action::NewTab => {
                let slot = self.new_slot();
                let cur = self.sides[side].panel();
                let dir = cur.dir.clone();
                let mut p = Panel::new(slot, dir.clone());
                p.sort = cur.sort;
                p.show_hidden = cur.show_hidden;
                self.hide_tab(side, &mut fx);
                let s = &mut self.sides[side];
                s.tabs.insert(s.active + 1, p);
                s.active += 1;
                fx.extend(self.load(side, dir, None, false));
                // A new tab of the same directory is not a visit.
                self.mark_visit(slot, false);
                return fx;
            }
            Action::CloseTab => {
                if n <= 1 {
                    return fx;
                }
                self.leave_results(side);
                let s = &mut self.sides[side];
                s.tabs.remove(s.active);
                if s.active >= s.tabs.len() {
                    s.active = s.tabs.len() - 1;
                }
                fx.push(Effect::Watch {
                    slot: old_slot,
                    dir: None,
                });
            }
            Action::PrevTab | Action::NextTab | Action::GotoTab(_) => {
                let cur = self.sides[side].active;
                let target = match a {
                    Action::PrevTab => (cur + n - 1) % n,
                    Action::NextTab => (cur + 1) % n,
                    Action::GotoTab(k) => (k as usize - 1).min(n - 1),
                    _ => cur,
                };
                if target == cur {
                    return fx;
                }
                self.hide_tab(side, &mut fx);
                self.sides[side].active = target;
            }
            _ => return fx,
        }
        fx.extend(self.show_tab(side));
        fx
    }

    /// The active tab of `side` goes into the background: no watch, no listing.
    fn hide_tab(&mut self, side: usize, fx: &mut Vec<Effect>) {
        let p = self.sides[side].panel_mut();
        let slot = p.slot;
        if p.is_loading()
            && let Some(l) = &p.loading
            && l.alive.is_running()
        {
            self.abandoned.push((p.blocked_path(), l.alive.clone()));
        }
        let p = self.sides[side].panel_mut();
        p.release();
        fx.push(Effect::Watch { slot, dir: None });
    }

    /// The active tab of `side` comes to the front: reload it; the listing's completion
    /// adds the watch.
    fn show_tab(&mut self, side: usize) -> Vec<Effect> {
        // A released archive tab reopens through the index cache (P3 2.2).
        if let Some(place) = self.sides[side].panel_mut().reopen.take() {
            let cursor = self.sides[side].panel().cursor_name().map(<[u8]>::to_vec);
            let fx = self.open_place(side, place, cursor, Record::No);
            let p = self.sides[side].panel_mut();
            p.released = false;
            return fx;
        }
        let p = self.sides[side].panel();
        if p.loading.is_some() {
            return Vec::new();
        }
        if !p.loaded_once {
            let dir = p.dir.clone();
            return self.load_ex(side, dir, None, true, Record::No);
        }
        self.refresh_slot(side)
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
                    // A scan's running total, or a job without a scan (P2 8.2) counting
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

/// What a confirmation calls the selection: the one entry's name (its path relative to the
/// panel, for a group below it), or "N entries".
fn count_text(groups: &[Group]) -> String {
    let total: usize = groups.iter().map(|g| g.names.len()).sum();
    match groups {
        [g] if total == 1 => {
            let mut rel = Vec::new();
            for c in g.sub.iter().chain(&g.names) {
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
