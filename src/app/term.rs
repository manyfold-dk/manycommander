#![forbid(unsafe_code)]
//! Terminal setup and teardown, the input thread, and the panic hook (design section 6,
//! NFR-REL).
//!
//! The input thread blocks in `poll` on the tty and a wake pipe, with no timeout, so an
//! idle manycommander has no periodic wakeups (P-5). Suspend asks it to park through the
//! pipe and waits until it has: the terminal is then free for a child, and the reader and
//! the child never both read the pty (A-UI-3).

use super::event::Event;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyEventKind, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute};
use rustix::fd::{AsFd, OwnedFd};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Whether the terminal is in manycommander's mode, whether the keyboard protocol was
/// pushed, and the images the terminal holds (P3 4.7); shared with the panic hook.
#[derive(Default)]
pub struct TermState {
    pub active: AtomicBool,
    pub enhanced: AtomicBool,
    pub gfx: Mutex<crate::preview::gfx::Screen>,
}

/// Raw mode, alternate screen, bracketed paste, hidden cursor, and the keyboard protocol
/// (`DISAMBIGUATE_ESCAPE_CODES` only) when supported.
pub fn enter(state: &TermState) -> io::Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(
        out,
        EnterAlternateScreen,
        EnableBracketedPaste,
        cursor::Hide
    )?;
    if state.enhanced.load(Ordering::SeqCst) {
        execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    state.active.store(true, Ordering::SeqCst);
    Ok(())
}

/// The reverse of [`enter`]; safe to call twice. The images manycommander placed are
/// deleted first (P3 4.5: before a hand-off and before exit); a frame that holds the
/// graphics state (a panic while drawing) skips that.
pub fn leave(state: &TermState) -> io::Result<()> {
    if !state.active.swap(false, Ordering::SeqCst) {
        return Ok(());
    }
    let mut out = io::stdout();
    if let Ok(mut g) = state.gfx.try_lock() {
        let mut b = Vec::new();
        g.forget_all(&mut b);
        if !b.is_empty() {
            let _ = out.write_all(&b);
        }
    }
    if state.enhanced.load(Ordering::SeqCst) {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        out,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        cursor::Show
    );
    let _ = out.flush();
    crossterm::terminal::disable_raw_mode()
}

/// Restores the terminal before the panic message prints (NFR-REL). Panics on job and
/// listing threads are caught there and become failed reports; they are only logged. A
/// panic anywhere else ends the process after the restore.
pub fn install_panic_hook(state: Arc<TermState>) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let name = std::thread::current().name().unwrap_or("").to_owned();
        if name.starts_with("job") || name.starts_with("list") {
            tracing::error!("panic on thread {name}: {info}");
            return;
        }
        let _ = leave(&state);
        default(info);
        if name != "main" {
            std::process::abort();
        }
    }));
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Running,
    PauseRequested,
    Paused,
    Stop,
}

/// The input thread's handle: pause, resume, stop.
pub struct Input {
    shared: Arc<(Mutex<Mode>, Condvar)>,
    wake: OwnedFd,
}

impl Input {
    /// Starts the input thread. Only key presses are forwarded (never releases), so a
    /// binding fires once under the keyboard protocol.
    pub fn start(tx: Sender<Event>) -> io::Result<Input> {
        let (wake_r, wake_w) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)?;
        let shared = Arc::new((Mutex::new(Mode::Running), Condvar::new()));
        let s = shared.clone();
        std::thread::Builder::new()
            .name("input".into())
            .spawn(move || input_loop(s, wake_r, tx))?;
        Ok(Input {
            shared,
            wake: wake_w,
        })
    }

    fn set(&self, m: Mode) {
        *self.shared.0.lock().unwrap() = m;
        self.shared.1.notify_all();
        let _ = rustix::io::write(&self.wake, b"x");
    }

    /// Parks the input thread and waits until it no longer reads the terminal.
    pub fn pause(&self) {
        self.set(Mode::PauseRequested);
        let (lock, cv) = &*self.shared;
        let mut m = lock.lock().unwrap();
        while *m == Mode::PauseRequested {
            let (g, _) = cv.wait_timeout(m, Duration::from_secs(1)).unwrap();
            m = g;
        }
    }

    pub fn resume(&self) {
        self.set(Mode::Running);
    }

    pub fn stop(&self) {
        self.set(Mode::Stop);
    }
}

/// The terminal for reading: stdin when it is one, else `/dev/tty`.
pub(crate) fn tty() -> io::Result<OwnedFd> {
    use rustix::fs::{Mode as FMode, OFlags};
    let stdin = rustix::stdio::stdin();
    if rustix::termios::isatty(stdin) {
        Ok(rustix::io::fcntl_dupfd_cloexec(stdin, 3)?)
    } else {
        Ok(rustix::fs::open(
            "/dev/tty",
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY,
            FMode::empty(),
        )?)
    }
}

/// Sends every event crossterm can parse now, including ones it buffered. `false`: the
/// terminal or the channel is gone.
///
/// crossterm reads the terminal through its level-triggered `use-dev-tty` source: the
/// default edge-triggered source drops the terminal's readiness when a `SIGWINCH` is
/// reported in the same batch, and the key then waits for the next key. That source
/// never polls with a zero timeout, hence the 1 ms: it only runs after this thread woke.
fn deliver(tx: &Sender<Event>) -> bool {
    loop {
        match crossterm::event::poll(Duration::from_millis(1)) {
            Ok(true) => {}
            Ok(false) => return true,
            Err(e) => {
                tracing::debug!("input: crossterm poll failed: {e}");
                return false;
            }
        }
        let ev = match crossterm::event::read() {
            Ok(e) => e,
            Err(_) => return false,
        };
        let out = match ev {
            crossterm::event::Event::Key(k) if k.kind == KeyEventKind::Press => {
                Event::Key(k, std::time::Instant::now())
            }
            crossterm::event::Event::Key(_) => continue,
            crossterm::event::Event::Resize(w, h) => Event::Resize(w, h),
            crossterm::event::Event::Paste(s) => Event::Paste(s),
            _ => continue,
        };
        if tx.send(out).is_err() {
            return false;
        }
    }
}

fn input_loop(shared: Arc<(Mutex<Mode>, Condvar)>, wake: OwnedFd, tx: Sender<Event>) {
    let Ok(tty) = tty() else { return };
    let mut drain = [0u8; 64];
    loop {
        {
            let (lock, cv) = &*shared;
            let mut m = lock.lock().unwrap();
            loop {
                match *m {
                    Mode::Running => break,
                    Mode::Stop => return,
                    Mode::PauseRequested => {
                        *m = Mode::Paused;
                        cv.notify_all();
                    }
                    Mode::Paused => m = cv.wait(m).unwrap(),
                }
            }
        }
        // Events crossterm already holds (a hand-off's key wait may have read several
        // keys at once) are delivered before the thread blocks on the terminal.
        if !deliver(&tx) {
            tracing::debug!("input: deliver failed; input thread ends");
            return;
        }
        let mut fds = [
            rustix::event::PollFd::new(&tty, rustix::event::PollFlags::IN),
            rustix::event::PollFd::new(&wake, rustix::event::PollFlags::IN),
        ];
        match rustix::event::poll(&mut fds, None) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => {
                tracing::debug!("input: poll failed: {e}");
                return;
            }
        }
        if !fds[1].revents().is_empty() {
            let _ = rustix::io::read(wake.as_fd(), &mut drain);
        }
        // The terminal's bytes are read at the top of the loop, after the pause check.
    }
}

/// The crossterm backend without cursor-position queries. ratatui asks for the cursor
/// position in `Terminal::new` and `Terminal::clear`; crossterm answers by writing a query
/// and reading the reply from the terminal. On the UI thread that makes a second reader of
/// the terminal: a key that arrives with the reply is queued inside crossterm while the input
/// thread sleeps on an empty terminal, and a terminal that never answers stalls the UI
/// thread. The position manycommander last set is all ratatui needs in fullscreen.
pub struct Backend {
    inner: ratatui::backend::CrosstermBackend<std::io::Stdout>,
    cursor: ratatui::layout::Position,
}

impl Backend {
    pub fn new() -> Backend {
        Backend {
            inner: ratatui::backend::CrosstermBackend::new(io::stdout()),
            cursor: ratatui::layout::Position::ORIGIN,
        }
    }
}

impl Default for Backend {
    fn default() -> Self {
        Backend::new()
    }
}

impl ratatui::backend::Backend for Backend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.inner.draw(content)
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<ratatui::layout::Position> {
        Ok(self.cursor)
    }

    fn set_cursor_position<P: Into<ratatui::layout::Position>>(
        &mut self,
        position: P,
    ) -> io::Result<()> {
        self.cursor = position.into();
        self.inner.set_cursor_position(self.cursor)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, t: ratatui::backend::ClearType) -> io::Result<()> {
        self.inner.clear_region(t)
    }

    fn size(&self) -> io::Result<ratatui::layout::Size> {
        self.inner.size()
    }

    fn window_size(&mut self) -> io::Result<ratatui::backend::WindowSize> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        ratatui::backend::Backend::flush(&mut self.inner)
    }
}
