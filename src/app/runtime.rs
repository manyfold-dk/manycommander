#![forbid(unsafe_code)]
//! The runtime: startup, the event loop and the effect executor (design 3.1).
//!
//! Order matters: signal handlers are registered before any thread starts; keyboard
//! protocol support is queried before the input thread starts. The loop blocks on its
//! channel; a tick runs only while a load or a job is in progress (P-5).

use super::event::{Effect, Event};
use super::term::{Input, TermState, detect_enhancement, enter, install_panic_hook, leave};
use super::{App, handoff, jobs, signals};
use crate::config::Config;
use crate::panel::listing::{self, ListingMsg};
use crate::panel::watch::PanelWatcher;
use crate::theme::watch::Target;
use crate::theme::{Depth, Palette};
use ratatui::Terminal;
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
            let _ = tx.send(Boot {
                state,
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
    loader: Loader,
    palette_path: Option<PathBuf>,
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
                Effect::Quit => app.quit = true,
            }
        }
    }
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
    // The keyboard-protocol query reads the terminal's answer: before the input thread.
    crossterm::terminal::enable_raw_mode()?;
    term.enhanced.store(detect_enhancement(), Ordering::SeqCst);
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
    let (state, config, config_error, palette, tz) = match b {
        Some(b) => (b.state, b.config, b.config_error, b.palette, b.tz),
        None => (None, Config::default(), None, None, jiff::tz::TimeZone::UTC),
    };
    let mut app = App::new(left, right, home, config, palette, Depth::from_env(), tz);
    // Restored tabs (M2); a directory named on the command line wins for its side.
    if let Some(s) = &state {
        app.restore(s, opts.left.is_some(), opts.right.is_some());
    }
    if let Some(e) = config_error {
        app.warn(format!("config: {e}"));
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
        loader: default_loader(),
        palette_path,
    };

    let mut terminal = Terminal::new(super::term::Backend::new())?;
    terminal.clear()?;
    let fx = app.start();
    ctx.execute(&mut app, fx, &input, &term);
    let mut first_full = false;
    let result = (|| -> std::io::Result<()> {
        loop {
            let ev = if app.needs_tick() {
                match rx.recv_timeout(Duration::from_millis(100)) {
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
                    tracing::debug!(
                        code = ?k.code,
                        modifiers = ?k.modifiers,
                        action = ?super::keys::map(*k, true),
                        "key"
                    );
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
            if app.quit {
                return Ok(());
            }
            if app.redraw {
                terminal.clear()?;
                app.redraw = false;
            }
            terminal.draw(|f| crate::ui::draw(&mut app, f))?;
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
            }
        }
    })();
    input.stop();
    let _ = leave(&term);
    // The session state for the next start, written atomically after the terminal is
    // restored.
    if let Some(p) = state_path()
        && let Err(e) = app.state().save(&p)
    {
        eprintln!("manycommander: could not save {}: {e}", p.display());
    }
    result.map(|()| 0)
}
