#![forbid(unsafe_code)]
//! The signal thread (design 3.1, section 6). `signal-hook`'s handler only writes to a
//! self-pipe; this thread turns signals into events. Registration happens before any other
//! thread starts, as the crate requires to avoid a registration race.

use super::event::{Event, Sig};
use signal_hook::consts::{SIGCONT, SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGTSTP, SIGUSR1, SIGWINCH};
use signal_hook::iterator::Signals;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

/// A handed-off child runs in manycommander's process group, so the terminal's `Ctrl+C`,
/// `Ctrl+\\` and `Ctrl+Z` reach both. While a child runs, those signals are the child's.
pub static CHILD_RUNNING: AtomicBool = AtomicBool::new(false);

/// Registers the handlers. Call first in `main`.
pub fn register() -> std::io::Result<Signals> {
    Signals::new([
        SIGUSR1, SIGTERM, SIGHUP, SIGINT, SIGQUIT, SIGTSTP, SIGCONT, SIGWINCH,
    ])
}

pub fn spawn(mut signals: Signals, tx: Sender<Event>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("signals".into())
        .spawn(move || {
            for s in signals.forever() {
                if matches!(s, SIGINT | SIGQUIT | SIGTSTP) && CHILD_RUNNING.load(Ordering::SeqCst) {
                    continue;
                }
                // The window changed size (a terminal going fullscreen). crossterm notices
                // SIGWINCH only when the input thread reads, which it does on key presses,
                // so the resize is posted from here and redraws at once.
                if s == SIGWINCH {
                    let (w, h) = crossterm::terminal::size().unwrap_or((0, 0));
                    if tx.send(Event::Resize(w, h)).is_err() {
                        return;
                    }
                    continue;
                }
                let sig = match s {
                    SIGUSR1 => Sig::ReloadTheme,
                    SIGTSTP => Sig::Suspend,
                    SIGCONT => Sig::Resume,
                    _ => Sig::Quit,
                };
                if tx.send(Event::Signal(sig)).is_err() {
                    return;
                }
            }
        })?;
    Ok(())
}

/// Stops the process the way the default `SIGTSTP` action would. Returns after `SIGCONT`.
pub fn stop_self() {
    let _ = signal_hook::low_level::emulate_default_handler(SIGTSTP);
}
