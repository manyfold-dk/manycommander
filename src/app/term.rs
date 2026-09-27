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

/// Whether the terminal is in manycommander's mode, and whether the keyboard protocol was
/// pushed; shared with the panic hook.
#[derive(Default)]
pub struct TermState {
    pub active: AtomicBool,
    pub enhanced: AtomicBool,
}

/// Queries keyboard-protocol support. Runs before the input thread exists, because the
/// query reads the terminal's answer itself.
pub fn detect_enhancement() -> bool {
    crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false)
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

/// The reverse of [`enter`]; safe to call twice.
pub fn leave(state: &TermState) -> io::Result<()> {
    if !state.active.swap(false, Ordering::SeqCst) {
        return Ok(());
    }
    let mut out = io::stdout();
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

fn tty() -> io::Result<OwnedFd> {
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
        let mut fds = [
            rustix::event::PollFd::new(&tty, rustix::event::PollFlags::IN),
            rustix::event::PollFd::new(&wake, rustix::event::PollFlags::IN),
        ];
        match rustix::event::poll(&mut fds, None) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => return,
        }
        let tty_ready = !fds[0].revents().is_empty();
        let wake_ready = !fds[1].revents().is_empty();
        if wake_ready {
            let _ = rustix::io::read(wake.as_fd(), &mut drain);
            continue;
        }
        if !tty_ready {
            continue;
        }
        // Read every event crossterm can parse now, including ones it buffered.
        loop {
            match crossterm::event::poll(Duration::ZERO) {
                Ok(true) => {}
                Ok(false) => break,
                Err(_) => return,
            }
            let ev = match crossterm::event::read() {
                Ok(e) => e,
                Err(_) => return,
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
                return;
            }
        }
    }
}
