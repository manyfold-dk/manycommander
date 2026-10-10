#![forbid(unsafe_code)]
//! The signal thread (design 3.1, section 6). `signal-hook`'s handler only writes to a
//! self-pipe; this thread turns signals into events. Registration happens before any other
//! thread starts, as the crate requires to avoid a registration race.

use super::event::{Event, Sig};
use signal_hook::consts::{SIGCONT, SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGTSTP, SIGUSR1, SIGWINCH};
use signal_hook::iterator::SignalsInfo;
use signal_hook::iterator::exfiltrator::WithOrigin;
use signal_hook::low_level::siginfo::Cause;
use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

/// The registered signals, each with its origin as the handler saw it.
pub type Signals = SignalsInfo<WithOrigin>;

/// A handed-off child runs in manycommander's process group, so the terminal's `Ctrl+C`,
/// `Ctrl+\\` and `Ctrl+Z` reach both. While a child runs, those signals are the child's.
/// ssh runs in its own process group (P3 5.2): these keys never reach an open session, and
/// during a connect they reach only ssh, which owns the terminal then; the flag covers the
/// moment before it does.
pub static CHILD_RUNNING: AtomicBool = AtomicBool::new(false);

/// Registers the handlers. Call first in `main`.
pub fn register() -> std::io::Result<Signals> {
    Signals::new([
        SIGUSR1, SIGTERM, SIGHUP, SIGINT, SIGQUIT, SIGTSTP, SIGCONT, SIGWINCH,
    ])
}

/// Whether the signal thread drops a signal instead of acting on it. The terminal sends
/// `SIGINT`, `SIGQUIT` and `SIGTSTP` only outside raw mode, that is during a hand-off, and
/// the kernel is then the sender (`SI_KERNEL`): such a signal is the child's, also when this
/// thread reads it after the child has ended and `CHILD_RUNNING` is false again. The same
/// signals sent with `kill` are dropped only while a child runs.
pub fn dropped(signal: c_int, cause: Cause, child_running: bool) -> bool {
    matches!(signal, SIGINT | SIGQUIT | SIGTSTP) && (child_running || cause == Cause::Kernel)
}

pub fn spawn(mut signals: Signals, tx: Sender<Event>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("signals".into())
        .spawn(move || {
            for o in signals.forever() {
                let s = o.signal;
                if dropped(s, o.cause, CHILD_RUNNING.load(Ordering::SeqCst)) {
                    tracing::debug!(signal = s, cause = ?o.cause, "signal dropped: the child's");
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

#[cfg(test)]
mod tests {
    use super::*;
    use signal_hook::low_level::siginfo::Sent;

    #[test]
    fn terminal_keys_are_the_childs_also_after_the_child_ended() {
        for sig in [SIGINT, SIGQUIT, SIGTSTP] {
            // A key in a hand-off: the kernel sends it to the foreground group. The thread
            // may read it after the child ended (the race that quit manycommander).
            assert!(dropped(sig, Cause::Kernel, false), "{sig}");
            assert!(dropped(sig, Cause::Kernel, true), "{sig}");
            // `kill`: dropped only while a child runs.
            assert!(dropped(sig, Cause::Sent(Sent::User), true), "{sig}");
            assert!(!dropped(sig, Cause::Sent(Sent::User), false), "{sig}");
            assert!(!dropped(sig, Cause::Unknown, false), "{sig}");
        }
        for sig in [SIGTERM, SIGHUP, SIGUSR1, SIGCONT, SIGWINCH] {
            assert!(!dropped(sig, Cause::Kernel, true), "{sig}");
            assert!(!dropped(sig, Cause::Sent(Sent::User), true), "{sig}");
        }
    }
}
