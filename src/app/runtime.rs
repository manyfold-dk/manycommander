#![forbid(unsafe_code)]
//! The runtime: startup, the event loop and the effect executor (design 3.1).
//!
//! Order matters: signal handlers are registered before any thread starts; the terminal
//! probe (keyboard protocol, graphics, cell size; P3 4.2) runs before the input thread
//! starts. The loop blocks on its channel; it waits with a deadline only while a load, a
//! search or a job is in progress (a tick), while a preview debounce is pending, or while a
//! preview request waits behind a stuck preview thread (P-5). The frecency store loads
//! after the first full frame (P2 3.2, P-2) and is merged into `dirs.tsv` after the
//! terminal is restored, like `state.toml`. Around each frame the graphics layer transmits,
//! places and deletes the quick view's images (P3 4.5). An SFTP connect runs on this thread
//! inside the terminal hand-off (P3 5.2); remote listings run on listing threads (P3 5.4);
//! every open session in the app's pool is closed at exit (P3 5.7).

use super::event::{Effect, Event};
use super::term::{Input, TermState, enter, install_panic_hook, leave};
use super::{App, handoff, jobs, signals};
use crate::compare::{self, CompareMsg};
use crate::config::Config;
use crate::dirs::{self, Hotlist, Reply, Request, StoreThread};
use crate::find::{self, FindMsg};
use crate::panel::listing::{self, Alive, ListingMsg};
use crate::panel::watch::PanelWatcher;
use crate::preview::probe::{self, Probed};
use crate::preview::worker::Worker;
use crate::remote::session::OnLost;
use crate::remote::{Lost, RemoteMsg};
use crate::theme::watch::Target;
use crate::theme::{Depth, Palette};
use crate::viewtemp::ViewMsg;
use ratatui::Terminal;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const USAGE: &str = "usage: manycommander [--log FILE] [--theme-file PATH] [--no-theme-watch] [--exit-after-first-frame] [LEFT [RIGHT]]";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub log: Option<PathBuf>,
    pub theme_file: Option<PathBuf>,
    pub no_theme_watch: bool,
    pub exit_after_first_frame: bool,
    pub left: Option<PathBuf>,
    pub right: Option<PathBuf>,
    pub help: bool,
    pub version: bool,
}

pub fn parse_args(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<Options, String> {
    let mut o = Options::default();
    let mut it = args.into_iter().skip(1);
    let mut positional = Vec::new();
    while let Some(a) = it.next() {
        match a.to_str() {
            Some("--log") => o.log = Some(it.next().ok_or("--log needs a file")?.into()),
            Some("--theme-file") => {
                o.theme_file = Some(it.next().ok_or("--theme-file needs a path")?.into())
            }
            Some("--no-theme-watch") => o.no_theme_watch = true,
            Some("--exit-after-first-frame") => o.exit_after_first_frame = true,
            Some("-h" | "--help") => o.help = true,
            Some("-V" | "--version") => o.version = true,
            Some("--") => positional.extend(it.by_ref()),
            Some(s) if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ => positional.push(a),
        }
    }
    if positional.len() > 2 {
        return Err("at most two directories".into());
    }
    let mut p = positional.into_iter();
    o.left = p.next().map(PathBuf::from);
    o.right = p.next().map(PathBuf::from);
    Ok(o)
}

/// Palette loading; injectable so a test can prove it never runs on the UI thread.
pub type Loader = Arc<dyn Fn(&Path) -> Result<Palette, String> + Send + Sync>;

pub fn default_loader() -> Loader {
    Arc::new(|p: &Path| Palette::load(p))
}

/// Loads the palette on a listing thread and sends `ThemeLoaded` (design 7.2).
pub fn spawn_theme_load(loader: Loader, path: PathBuf, tx: Sender<Event>) {
    let requested = Instant::now();
    let _ = std::thread::Builder::new()
        .name("list-theme".into())
        .spawn(move || {
            let palette = loader(&path);
            let _ = tx.send(Event::ThemeLoaded { palette, requested });
        });
}

fn theme_target(o: &Options) -> Option<Target> {
    let file = o.theme_file.clone().or_else(|| {
        std::env::var_os("MANYCOMMANDER_THEME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    });
    match file {
        Some(path) => Some(Target::File { path }),
        None => Target::omarchy_default(std::env::var_os("HOME").as_deref()),
    }
}

fn init_log(path: &Path) -> std::io::Result<()> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let _ = tracing_subscriber::fmt()
        .with_writer(Mutex::new(f))
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .with_thread_names(true)
        .try_init();
    Ok(())
}

struct Boot {
    state: Option<super::state::State>,
    /// `hotlist.toml` (P2 3.2) and the text to report once when it does not parse.
    hotlist: (Hotlist, Option<String>),
    config: Config,
    config_error: Option<String>,
    palette: Option<Palette>,
    tz: jiff::tz::TimeZone,
    cwd: PathBuf,
}

fn state_path() -> Option<PathBuf> {
    super::state::path(
        std::env::var_os("XDG_STATE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// Startup reads (state, config, palette, time zone) run on a listing thread; the UI thread
/// waits for them briefly and falls back to defaults.
fn boot(target: Option<PathBuf>) -> Receiver<Boot> {
    let (tx, rx) = channel();
    let _ = std::thread::Builder::new()
        .name("list-boot".into())
        .spawn(move || {
            let (config, config_error) = match Config::path(
                std::env::var_os("XDG_CONFIG_HOME").as_deref(),
                std::env::var_os("HOME").as_deref(),
            ) {
                Some(p) => Config::load(&p),
                None => (Config::default(), None),
            };
            let palette = target.and_then(|p| Palette::load(&p).ok());
            let tz = jiff::tz::TimeZone::system();
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
            let state = state_path().and_then(|p| super::state::State::load(&p));
            let hotlist = dirs::Paths::from_env()
                .hotlist
                .map(|p| Hotlist::load(&p))
                .unwrap_or_default();
            let _ = tx.send(Boot {
                state,
                hotlist,
                config,
                config_error,
                palette,
                tz,
                cwd,
            });
        });
    rx
}

struct Ctx {
    tx: Sender<Event>,
    watcher: Option<PanelWatcher>,
    cancel: Option<Arc<AtomicBool>>,
    /// The running compare's cancel flag (P2 7).
    compare_cancel: Option<Arc<AtomicBool>>,
    loader: Loader,
    palette_path: Option<PathBuf>,
    /// The directory-store thread (P2 2.3), started by its first request.
    store: Option<StoreThread>,
    dirs_paths: dirs::Paths,
    /// The archive index cache (P3 2.6), shared by the listing threads.
    archives: Arc<crate::archive::IndexCache>,
    /// The runtime view directory (P3 3.4), made on the first view.
    views: crate::viewtemp::Roots,
    /// The preview thread (P3 4.4), started by the first request.
    preview: Option<Worker>,
}

impl Ctx {
    /// Hands `r` to the directory-store thread, starting it first.
    fn store_request(&mut self, r: Request) {
        if self.store.is_none() {
            let tx = self.tx.clone();
            match StoreThread::spawn(self.dirs_paths.clone(), move |reply| {
                let _ = tx.send(match reply {
                    Reply::Loaded(s) => Event::DirsLoaded(s),
                    Reply::Zoxide(v) => Event::ZoxideLoaded(v),
                    Reply::Failed(e) => Event::Status(e),
                });
            }) {
                Ok(t) => self.store = Some(t),
                Err(e) => {
                    // Nobody waits forever: an empty answer and the reason.
                    let _ = self.tx.send(Event::Status(format!(
                        "cannot start the directory store: {e}"
                    )));
                    let _ = self.tx.send(match r {
                        Request::Load => Event::DirsLoaded(dirs::Store::default()),
                        Request::Zoxide => Event::ZoxideLoaded(Vec::new()),
                        Request::SaveHotlist(_) => return,
                    });
                    return;
                }
            }
        }
        if let Some(t) = &self.store {
            t.request(r);
        }
    }
}

impl Ctx {
    fn execute(&mut self, app: &mut App, fx: Vec<Effect>, input: &Input, state: &TermState) {
        for e in fx {
            match e {
                Effect::LoadTheme => {
                    if let Some(p) = &self.palette_path {
                        spawn_theme_load(self.loader.clone(), p.clone(), self.tx.clone());
                    }
                }
                Effect::List(req, alive) => {
                    let tx = self.tx.clone();
                    listing::spawn(req, alive, move |m| {
                        let _ = tx.send(Event::Listing(m));
                    });
                }
                Effect::OpenArchive(req, alive) => {
                    let tx = self.tx.clone();
                    let cache = self.archives.clone();
                    spawn_listing(req.slot, alive, move || {
                        let send = |m| {
                            let _ = tx.send(Event::Listing(m));
                        };
                        crate::archive::guarded(
                            req.slot,
                            req.generation,
                            &req.archive,
                            &send,
                            || crate::archive::open(&req, &cache, &send),
                        );
                    });
                }
                Effect::Relist(req, alive) => {
                    let tx = self.tx.clone();
                    spawn_listing(req.slot, alive, move || {
                        let send = |m| {
                            let _ = tx.send(Event::Listing(m));
                        };
                        let archive = req.index.archive.clone();
                        crate::archive::guarded(req.slot, req.generation, &archive, &send, || {
                            crate::archive::relist(&req, &send)
                        });
                    });
                }
                Effect::ArchiveSize(req) => {
                    let tx = self.tx.clone();
                    spawn_listing(req.slot, Alive::running(), move || {
                        let send = |m| {
                            let _ = tx.send(Event::Listing(m));
                        };
                        let archive = req.index.archive.clone();
                        crate::archive::guarded(req.slot, req.generation, &archive, &send, || {
                            crate::archive::size(&req, &send)
                        });
                    });
                }
                Effect::PrepareView(req, alive) => {
                    let tx = self.tx.clone();
                    let roots = self.views.clone();
                    let id = req.id;
                    let started = spawn_view("list-view", alive, move || {
                        let send = |m| {
                            let _ = tx.send(Event::View(m));
                        };
                        let progress = |done, total| send(ViewMsg::Progress { id, done, total });
                        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            crate::viewtemp::prepare(&roots, &req, &progress)
                        }));
                        send(match r {
                            Ok(Ok(file)) => ViewMsg::Ready { id, file },
                            Ok(Err(error)) => ViewMsg::Failed { id, error },
                            Err(_) => ViewMsg::Failed {
                                id,
                                error: "internal error while preparing the view".into(),
                            },
                        });
                    });
                    if let Err(e) = started {
                        let _ = self.tx.send(Event::View(ViewMsg::Failed {
                            id,
                            error: format!("cannot start the view: {e}"),
                        }));
                    }
                }
                Effect::CheckView(file) => {
                    let tx = self.tx.clone();
                    let roots = self.views.clone();
                    let _ = spawn_view("list-viewcheck", Alive::running(), move || {
                        let (kept, error) = match crate::viewtemp::check(&roots, &file) {
                            Ok(kept) => (kept, None),
                            Err(e) => (None, Some(e)),
                        };
                        let remote = file.remote;
                        let _ = tx.send(Event::View(ViewMsg::Checked {
                            kept,
                            error,
                            remote,
                        }));
                    });
                }
                Effect::CheckEdited(file, at) => {
                    let tx = self.tx.clone();
                    let roots = self.views.clone();
                    let _ = spawn_view("list-viewcheck", Alive::running(), move || {
                        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            crate::viewtemp::check_edited(&roots, &file, at)
                        }));
                        let m = r.unwrap_or_else(|_| ViewMsg::Checked {
                            kept: None,
                            error: Some("internal error while checking the view copy".into()),
                            remote: true,
                        });
                        let _ = tx.send(Event::View(m));
                    });
                }
                Effect::Find(search) => {
                    let tx = self.tx.clone();
                    if let Err(e) = find::spawn(search.clone(), move |m| {
                        let _ = tx.send(Event::Find(m));
                    }) {
                        search.alive.finish();
                        let _ = self.tx.send(Event::Find(FindMsg::Done {
                            id: search.id,
                            stats: find::Stats {
                                error: Some(format!("cannot start the search: {e}")),
                                ..find::Stats::default()
                            },
                        }));
                    }
                }
                Effect::Restat(req, alive) => {
                    let tx = self.tx.clone();
                    find::spawn_restat(req, alive, move |m| {
                        let _ = tx.send(Event::Listing(m));
                    });
                }
                Effect::Watch { slot, dir } => {
                    if let Some(w) = &self.watcher {
                        w.set(slot, dir);
                    }
                }
                Effect::StartJob(spec) => {
                    let cancel = Arc::new(AtomicBool::new(false));
                    self.cancel = Some(cancel.clone());
                    tracing::info!(?spec, "job start");
                    if let Err(e) = jobs::spawn(spec, self.tx.clone(), cancel) {
                        let _ = self.tx.send(Event::Job(super::event::JobEvent::Done(
                            crate::fsops::job::Report::refused(
                                crate::fsops::job::JobVerb::Copy,
                                format!("cannot start the worker: {e}"),
                            ),
                        )));
                    }
                }
                Effect::CancelJob => {
                    if let Some(c) = &self.cancel {
                        c.store(true, Ordering::SeqCst);
                    }
                }
                Effect::Compare(req) => {
                    // At most one compare runs (P2 2.3): a new one cancels the earlier.
                    if let Some(c) = self.compare_cancel.take() {
                        c.store(true, Ordering::SeqCst);
                    }
                    let cancel = Arc::new(AtomicBool::new(false));
                    self.compare_cancel = Some(cancel.clone());
                    let id = req.id;
                    let tx = self.tx.clone();
                    if let Err(e) = compare::spawn(req, cancel, move |m| {
                        let _ = tx.send(Event::Compare(m));
                    }) {
                        let _ = self.tx.send(Event::Compare(CompareMsg::Done {
                            id,
                            error: Some(format!("cannot start the compare thread: {e}")),
                        }));
                    }
                }
                Effect::CancelCompare => {
                    if let Some(c) = self.compare_cancel.take() {
                        c.store(true, Ordering::SeqCst);
                    }
                }
                Effect::LoadDirs => self.store_request(Request::Load),
                Effect::LoadZoxide => self.store_request(Request::Zoxide),
                Effect::SaveHotlist(d) => self.store_request(Request::SaveHotlist(d)),
                Effect::SuspendSelf => {
                    handoff::suspend_self(input, state);
                    app.redraw = true;
                }
                Effect::Run(h) => {
                    let status = handoff::run(&h, input, state);
                    app.redraw = true;
                    let _ = self.tx.send(Event::ChildDone {
                        status,
                        output: None,
                    });
                }
                Effect::Open(path) => {
                    if let Err(e) = handoff::open(&path) {
                        app.warn(e);
                    }
                }
                Effect::DirSize {
                    slot,
                    generation,
                    dir,
                    name,
                } => {
                    let tx = self.tx.clone();
                    let _ = std::thread::Builder::new()
                        .name("list-size".into())
                        .spawn(move || {
                            let never = AtomicBool::new(false);
                            let bytes = listing::dir_size(&dir, &name, &never);
                            let _ = tx.send(Event::Listing(ListingMsg::DirSize {
                                slot,
                                generation,
                                name,
                                bytes,
                            }));
                        });
                }
                Effect::Preview(req) => {
                    if self.preview.is_none() {
                        let tx = self.tx.clone();
                        match Worker::spawn(move |m| {
                            let _ = tx.send(Event::Preview(m));
                        }) {
                            Ok(w) => self.preview = Some(w),
                            Err(e) => {
                                app.warn(format!("cannot start the preview thread: {e}"));
                                continue;
                            }
                        }
                    }
                    if let Some(w) = &self.preview {
                        tracing::debug!(generation = req.generation, subject = ?req.subject, "preview request");
                        w.submit(req);
                    }
                }
                Effect::Connect(addr, cmd) => {
                    let address = addr.target.address();
                    let tx = self.tx.clone();
                    let lost_address = address.clone();
                    let on_lost: OnLost = Box::new(move |l: Lost| {
                        let _ = tx.send(Event::Remote(RemoteMsg::Lost {
                            address: lost_address,
                            message: l.message().to_owned(),
                        }));
                    });
                    let r = handoff::connect(&cmd, &addr, input, state, on_lost);
                    app.redraw = true;
                    let _ = self.tx.send(Event::Remote(match r {
                        Ok(session) => RemoteMsg::Connected {
                            target: addr.target,
                            session,
                        },
                        Err(message) => RemoteMsg::Failed { address, message },
                    }));
                }
                Effect::ListRemote(req, alive) => {
                    let tx = self.tx.clone();
                    spawn_listing(req.slot, alive, move || {
                        crate::remote::provider::list(&req, &|m| {
                            let _ = tx.send(Event::Listing(m));
                        });
                    });
                }
                Effect::RemoteSize(req) => {
                    let tx = self.tx.clone();
                    spawn_listing(req.slot, Alive::running(), move || {
                        crate::remote::tree::run_size(&req, &|m| {
                            let _ = tx.send(Event::Listing(m));
                        });
                    });
                }
                Effect::Quit => app.quit = true,
            }
        }
    }

    /// When the loop must wake without an event: the preview debounce, and a request that
    /// waits behind a stuck preview thread (P3 2.5, 4.4).
    fn deadline(&self, app: &App) -> Option<Instant> {
        let abandon = if app.quick.blocked {
            None
        } else {
            self.preview.as_ref().and_then(Worker::abandon_due)
        };
        [app.quick_due(), abandon].into_iter().flatten().min()
    }

    /// A request that waited [`crate::preview::ABANDON_AFTER`] for a thread busy with an
    /// older generation abandons that thread (P3 2.5), unless `MAX_ABANDONED` threads are
    /// blocked already: then the view says previews are blocked.
    fn check_preview(&mut self, app: &mut App) {
        let Some(w) = self.preview.as_mut() else {
            return;
        };
        if app.quick.blocked || !w.abandon_due().is_some_and(|d| d <= Instant::now()) {
            return;
        }
        if app.at_abandon_cap() {
            app.preview_blocked();
            return;
        }
        match w.replace() {
            Ok((alive, path)) => {
                tracing::warn!(path = %path.display(), "preview thread abandoned");
                app.preview_abandoned(path, alive);
            }
            Err(e) => app.warn(format!("cannot start the preview thread: {e}")),
        }
    }
}

/// The terminal probe (P3 4.2): after raw mode, before the input thread.
fn probe_terminal() -> Probed {
    let tmux = probe::in_tmux_env();
    let tty = match super::term::tty() {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("terminal probe: no terminal: {e}");
            return Probed {
                tmux,
                ..Probed::default()
            };
        }
    };
    let id = crate::preview::gfx::next_id();
    match probe::run(
        rustix::fd::AsFd::as_fd(&tty),
        rustix::stdio::stdout(),
        tmux,
        id,
        probe::DEADLINE,
    ) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("terminal probe failed: {e}");
            Probed {
                tmux,
                ..Probed::default()
            }
        }
    }
}

fn write_out(bytes: &[u8]) -> std::io::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let mut o = std::io::stdout().lock();
    o.write_all(bytes)?;
    o.flush()
}

/// One frame (P3 4.5): images that no longer show are deleted and new ones transmitted
/// before ratatui draws; direct kitty placements and sixels follow the frame. A frame that
/// clears (a full redraw, or a sixel to remove) deletes the stored images first, so the
/// terminal never holds one manycommander forgot; the image is transmitted again.
fn frame(
    app: &mut App,
    terminal: &mut Terminal<super::term::Backend>,
    term: &TermState,
) -> std::io::Result<()> {
    for _ in 0..2 {
        let want = app.quick.image(app.dialog.is_some());
        let mut out = Vec::new();
        let clear = {
            let mut g = term.gfx.lock().unwrap_or_else(|e| e.into_inner());
            let clear = app.redraw || g.needs_clear(want.as_deref());
            if clear {
                g.forget_all(&mut out);
            }
            clear
        };
        write_out(&out)?;
        out.clear();
        if clear {
            terminal.clear()?;
            app.redraw = false;
        }
        term.gfx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .before_draw(want.as_deref(), &mut out);
        write_out(&out)?;
        out.clear();
        terminal.draw(|f| crate::ui::draw(app, f))?;
        let redo = {
            let shown = app.quick.drawn.clone();
            term.gfx
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .after_draw(shown.as_ref().map(|(p, r)| (&**p, *r)), &mut out)
        };
        write_out(&out)?;
        // The pane may have changed in this frame (P3 4.4).
        app.quick_sync();
        if !redo {
            break;
        }
        app.redraw = true;
    }
    Ok(())
}

/// Runs `f` on a listing thread named `list-<slot>`; `alive` turns false when it returns
/// (the abandoned-thread limit, M1 3.1, P3 2.5).
fn spawn_listing(slot: usize, alive: Alive, f: impl FnOnce() + Send + 'static) {
    let a = alive.clone();
    let r = std::thread::Builder::new()
        .name(format!("list-{slot}"))
        .spawn(move || {
            f();
            a.finish();
        });
    if r.is_err() {
        alive.finish();
    }
}

/// Runs `f` on a view thread (P3 3.4, a listing thread); `alive` turns false when it
/// returns (the abandoned-thread limit, P3 2.5).
fn spawn_view(name: &str, alive: Alive, f: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    let a = alive.clone();
    let r = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            f();
            a.finish();
        });
    if r.is_err() {
        alive.finish();
    }
    r.map(drop)
}

/// Runs manycommander. `signals` must have been registered before any thread started.
pub fn run(
    opts: Options,
    signals: signal_hook::iterator::Signals,
    start: Instant,
) -> std::io::Result<i32> {
    let (tx, rx) = channel::<Event>();
    signals::spawn(signals, tx.clone())?;
    if let Some(l) = &opts.log {
        init_log(l)?;
    }
    crate::fsops::sys::raise_nofile_limit();
    let target = theme_target(&opts);
    let palette_path = target.as_ref().map(Target::palette_path);
    let boot_rx = boot(palette_path.clone());

    let term = Arc::new(TermState::default());
    install_panic_hook(term.clone());
    // The probe reads the terminal's answers: before the input thread (V-6).
    crossterm::terminal::enable_raw_mode()?;
    let probed = probe_terminal();
    term.enhanced.store(probed.keyboard, Ordering::SeqCst);
    enter(&term)?;
    let input = Input::start(tx.clone())?;

    let b = boot_rx.recv_timeout(Duration::from_millis(500)).ok();
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    let cwd = b
        .as_ref()
        .map(|b| b.cwd.clone())
        .unwrap_or_else(|| PathBuf::from("/"));
    let left = opts
        .left
        .as_ref()
        .map(|p| crate::panel::join_lexical(&cwd, p))
        .unwrap_or(cwd.clone());
    let right = opts
        .right
        .as_ref()
        .map(|p| crate::panel::join_lexical(&cwd, p))
        .unwrap_or(home.clone());
    let (state, config, config_error, palette, tz, (hotlist, hotlist_error)) = match b {
        Some(b) => (
            b.state,
            b.config,
            b.config_error,
            b.palette,
            b.tz,
            b.hotlist,
        ),
        // Without the boot answer the bookmarks stay read-only: a write could lose the
        // ones on disk.
        None => (
            None,
            Config::default(),
            None,
            None,
            jiff::tz::TimeZone::UTC,
            (Hotlist::unread(), None),
        ),
    };
    let mut app = App::new(left, right, home, config, palette, Depth::from_env(), tz);
    // The protocol for the session (P3 4.3): the probe, `preview.protocol`, the colours.
    let protocol = probe::choose(&probed, app.config.preview.protocol, app.depth);
    app.set_graphics(protocol, probed.cell);
    *term.gfx.lock().unwrap_or_else(|e| e.into_inner()) =
        crate::preview::gfx::Screen::new(protocol);
    tracing::info!(
        probe_us = probed.elapsed.as_micros() as u64,
        kitty = probed.graphics,
        sixel = probed.sixel,
        keyboard = probed.keyboard,
        da1 = probed.da1,
        cell = ?probed.cell,
        tmux = probed.tmux,
        discarded = probed.discarded,
        protocol = protocol.name(),
        "terminal probe"
    );
    // Restored tabs (M2); a directory named on the command line wins for its side.
    if let Some(s) = &state {
        app.restore(s, opts.left.is_some(), opts.right.is_some());
    }
    app.dirs.hotlist = hotlist;
    // Reported once (P2 3.2).
    let startup: Vec<String> = [
        config_error.map(|e| format!("config: {e}")),
        hotlist_error.map(|e| format!("hotlist: {e}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !startup.is_empty() {
        app.warn(startup.join("; "));
    }

    if !opts.no_theme_watch
        && let Some(t) = target.clone()
    {
        let ttx = tx.clone();
        let _ = crate::theme::watch::spawn(t, move || {
            let _ = ttx.send(Event::ReloadTheme);
        });
    }
    let wtx = tx.clone();
    let watcher = PanelWatcher::spawn(move |slot| {
        let _ = wtx.send(Event::DirChanged { slot });
    })
    .ok();
    let mut ctx = Ctx {
        tx: tx.clone(),
        watcher,
        cancel: None,
        compare_cancel: None,
        loader: default_loader(),
        palette_path,
        store: None,
        dirs_paths: dirs::Paths::from_env(),
        archives: Arc::new(crate::archive::IndexCache::default()),
        views: crate::viewtemp::Roots::from_env(),
        preview: None,
    };

    let mut terminal = Terminal::new(super::term::Backend::new())?;
    terminal.clear()?;
    let fx = app.start();
    ctx.execute(&mut app, fx, &input, &term);
    let mut first_full = false;
    let result = (|| -> std::io::Result<()> {
        loop {
            let now = Instant::now();
            let tick = app.needs_tick().then(|| now + Duration::from_millis(100));
            let deadline = [tick, ctx.deadline(&app)].into_iter().flatten().min();
            let ev = if let Some(d) = deadline {
                match rx.recv_timeout(d.saturating_duration_since(now)) {
                    Ok(e) => e,
                    Err(RecvTimeoutError::Timeout) => Event::Tick,
                    Err(RecvTimeoutError::Disconnected) => return Ok(()),
                }
            } else {
                match rx.recv() {
                    Ok(e) => e,
                    Err(_) => return Ok(()),
                }
            };
            let mut key_at = None;
            let handle = |ev: Event, app: &mut App, ctx: &mut Ctx, key_at: &mut Option<Instant>| {
                if let Event::Key(k, at) = &ev {
                    // The oldest key of this frame: latency from when the input thread
                    // read it, including any wait in the channel.
                    *key_at = Some(key_at.map_or(*at, |t: Instant| t.min(*at)));
                    // The action is the chord's without the focus on the command line.
                    tracing::debug!(
                        code = ?k.code,
                        modifiers = ?k.modifiers,
                        action = ?super::keys::map(*k, false),
                        "key"
                    );
                }
                if let Event::Resize(..) = &ev
                    && let Ok(ws) = crossterm::terminal::window_size()
                    && let Some(c) = probe::cell_of(ws.columns, ws.rows, ws.width, ws.height)
                {
                    // The cell size is read again on resize (P3 4.2): a font-size change.
                    app.quick.cell = Some(c);
                }
                let fx = app.update(ev);
                ctx.execute(app, fx, &input, &term);
            };
            handle(ev, &mut app, &mut ctx, &mut key_at);
            // Take everything already queued before drawing (listing batches, key repeat).
            for _ in 0..256 {
                match rx.try_recv() {
                    Ok(ev) => handle(ev, &mut app, &mut ctx, &mut key_at),
                    Err(_) => break,
                }
            }
            ctx.check_preview(&mut app);
            if app.quit {
                return Ok(());
            }
            frame(&mut app, &mut terminal, &term)?;
            if let Some(t) = key_at {
                tracing::debug!(key_to_flush_us = t.elapsed().as_micros() as u64, "frame");
            }
            if !first_full && app.sides.iter().all(|s| s.panel().loaded_once) {
                first_full = true;
                tracing::info!(
                    first_full_frame_us = start.elapsed().as_micros() as u64,
                    "first full frame"
                );
                if opts.exit_after_first_frame {
                    return Ok(());
                }
                // The frecency store loads now, not before the first frame (P-2).
                let fx = app.dirs_wanted();
                ctx.execute(&mut app, fx, &input, &term);
            }
        }
    })();
    input.stop();
    let _ = leave(&term);
    // Every session closes: ssh reads EOF and exits, and is reaped (P3 5.2).
    app.pool.close_all();
    // The private view directory goes, unless it holds an edited copy (P3 3.4).
    ctx.views.finish();
    // The session state for the next start, written atomically after the terminal is
    // restored.
    if let Some(p) = state_path()
        && let Err(e) = app.state().save(&p)
    {
        eprintln!("manycommander: could not save {}: {e}", p.display());
    }
    // A bookmark saved just before quitting is written before the process ends.
    if let Some(t) = ctx.store.take()
        && !t.finish(dirs::LOCK_WAIT)
    {
        eprintln!(
            "manycommander: the directory store did not finish; a bookmark change may be lost"
        );
    }
    // This session's visits, merged into dirs.tsv under its lock (P2 3.3).
    if let Some(p) = &ctx.dirs_paths.store
        && !app.dirs.deltas.is_empty()
        && let Err(e) = dirs::save_merged(p, &app.dirs.deltas, dirs::LOCK_WAIT)
    {
        eprintln!("manycommander: {e}");
    }
    result.map(|()| 0)
}
