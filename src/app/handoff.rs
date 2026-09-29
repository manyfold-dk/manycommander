#![forbid(unsafe_code)]
//! Suspend and resume (design section 6), shared by the command line, F3, F4, Ctrl+O,
//! `SIGTSTP` and the SFTP connect (P3 5.2): park the input thread, leave the alternate
//! screen, pop the keyboard protocol, disable raw mode; run or stop; then re-enable
//! everything and redraw fully. A child killed by a signal still leads to the restore.

use super::signals::stop_self;
use super::term::{Input, TermState, enter, leave};
use crate::cmdline::handoff::{Handoff, status_text};
use crate::remote::Session;
use crate::remote::session::OnLost;
use crate::remote::transport::{self, SshCommand};
use crate::remote::url::Address;
use crossterm::event::{Event as CEvent, KeyCode, KeyEventKind};
use rustix::fd::AsFd;
use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::process::{Command, Stdio};

pub struct Suspended<'a> {
    input: &'a Input,
    state: &'a TermState,
}

impl<'a> Suspended<'a> {
    /// Gives the terminal away: the input thread is parked before the terminal changes.
    pub fn new(input: &'a Input, state: &'a TermState) -> Suspended<'a> {
        input.pause();
        let _ = leave(state);
        Suspended { input, state }
    }
}

impl Drop for Suspended<'_> {
    fn drop(&mut self) {
        let _ = enter(self.state);
        self.input.resume();
    }
}

/// Waits for Enter (or any key with `any`), in raw mode on the normal screen. The input
/// thread is parked, so this is the terminal's only reader.
fn wait_key(any: bool) {
    let _ = crossterm::terminal::enable_raw_mode();
    loop {
        match crossterm::event::read() {
            Ok(CEvent::Key(k))
                if k.kind == KeyEventKind::Press && (any || k.code == KeyCode::Enter) =>
            {
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    // Raw mode stays on: the resume re-enters it anyway, and a key typed right after this
    // one must not land in the cooked line buffer (echoed, and held until a newline).
}

/// Runs a hand-off with the terminal. Returns the status line for the UI.
pub fn run(h: &Handoff, input: &Input, state: &TermState) -> String {
    let _s = Suspended::new(input, state);
    super::signals::CHILD_RUNNING.store(true, std::sync::atomic::Ordering::SeqCst);
    let _child = ChildGuard;
    match h {
        Handoff::Shell { shell, text, cwd } => {
            let status = Command::new(shell)
                .arg("-c")
                .arg(OsStr::from_bytes(text))
                .current_dir(cwd)
                .status();
            let line = match status {
                Ok(s) => status_text(s),
                Err(e) => format!("[{}: {e}]", shell.to_string_lossy()),
            };
            let mut out = std::io::stdout();
            let _ = write!(out, "\r\n{line} press Enter to return");
            let _ = out.flush();
            wait_key(false);
            let _ = write!(out, "\r\n");
            line
        }
        Handoff::Program { argv, cwd } => {
            let r = Command::new(&argv[0])
                .args(&argv[1..])
                .current_dir(cwd)
                .status();
            match r {
                Ok(s) if s.success() => String::new(),
                Ok(s) => format!("{} {}", argv[0].to_string_lossy(), status_text(s)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && argv[0] == "nvim" => {
                    // The editor default: nvim, then vi.
                    let r = Command::new("vi")
                        .args(&argv[1..])
                        .current_dir(cwd)
                        .status();
                    match r {
                        Ok(s) if s.success() => String::new(),
                        Ok(s) => format!("vi {}", status_text(s)),
                        Err(e) => format!("vi: {e}"),
                    }
                }
                Err(e) => format!("{}: {e}", argv[0].to_string_lossy()),
            }
        }
        Handoff::ShowScreen => {
            wait_key(true);
            String::new()
        }
    }
}

/// Connects to `addr` inside the hand-off (P3 5.2, D-7): the terminal is handed off as for
/// F3, and ssh prompts on it itself, so manycommander never sees a password. On failure
/// ssh's messages stay on the screen with `[connection failed] press Enter to return` under
/// them, and the result is ssh's last line.
pub fn connect(
    cmd: &SshCommand,
    addr: &Address,
    input: &Input,
    state: &TermState,
    on_lost: OnLost,
) -> Result<Session, String> {
    let _s = Suspended::new(input, state);
    // Between the spawn and the moment ssh owns the terminal, a Ctrl+C would still reach
    // manycommander's group; it is ssh's too.
    super::signals::CHILD_RUNNING.store(true, std::sync::atomic::Ordering::SeqCst);
    let _child = ChildGuard;
    let mut out = std::io::stdout();
    let _ = write!(
        out,
        "connecting to {} ... (Ctrl+C cancels)\r\n",
        addr.target.address()
    );
    let _ = out.flush();
    let r = match super::term::tty() {
        Ok(tty) => transport::connect(cmd, &addr.target, tty.as_fd(), Some(on_lost)),
        Err(e) => Err(format!("no terminal: {e}")),
    };
    if r.is_err() {
        let _ = write!(out, "\r\n[connection failed] press Enter to return");
        let _ = out.flush();
        wait_key(false);
        let _ = write!(out, "\r\n");
    }
    r
}

struct ChildGuard;

impl Drop for ChildGuard {
    fn drop(&mut self) {
        super::signals::CHILD_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// `SIGTSTP`: restore the terminal, stop; on `SIGCONT` take it back.
pub fn suspend_self(input: &Input, state: &TermState) {
    let _s = Suspended::new(input, state);
    stop_self();
}

/// `setsid -f xdg-open <path>` with stdio on /dev/null; a helper thread reaps the
/// short-lived `setsid`, so no zombie remains.
pub fn open(path: &std::path::Path) -> Result<(), String> {
    let mut child = Command::new("setsid")
        .arg("-f")
        .arg("xdg-open")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("xdg-open: {e}"))?;
    let _ = std::thread::Builder::new()
        .name("reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(())
}
