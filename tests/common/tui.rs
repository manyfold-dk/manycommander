//! Drives the manycommander binary on a pty and reads its screen through `vt100`.
#![allow(dead_code)]

use expectrl::Session;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

pub struct Tui {
    s: expectrl::session::OsSession,
    pub parser: vt100::Parser,
    pub raw: Vec<u8>,
    answered_da1: bool,
    answered_dsr: usize,
    /// Answer the kitty keyboard-protocol query as a terminal that supports it.
    pub kitty: bool,
}

pub const F10: &[u8] = b"\x1b[21~";
pub const F5: &[u8] = b"\x1b[15~";
pub const F3: &[u8] = b"\x1b[13~";
pub const SHIFT_F8: &[u8] = b"\x1b[19;2~";
pub const ESC: &[u8] = b"\x1b";
pub const ENTER: &[u8] = b"\r";
pub const DOWN: &[u8] = b"\x1b[B";
pub const ALT_ENTER: &[u8] = b"\x1b\r";

impl Tui {
    /// Starts the binary with `args`, `HOME` at `home`, in a `cols`x`rows` terminal.
    pub fn spawn(args: &[&str], home: &Path, env: &[(&str, &str)], cols: u16, rows: u16) -> Tui {
        Tui::spawn_opts(args, home, env, cols, rows, false)
    }

    /// `kitty`: the terminal reports kitty keyboard-protocol support.
    pub fn spawn_opts(
        args: &[&str],
        home: &Path,
        env: &[(&str, &str)],
        cols: u16,
        rows: u16,
        kitty: bool,
    ) -> Tui {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_manycommander"));
        cmd.args(args)
            .env_clear()
            .env("HOME", home)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("TERM", "xterm-256color")
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("SHELL", "/bin/sh")
            .current_dir(home);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut s = Session::spawn(cmd).expect("spawn manycommander on a pty");
        let _ = s.get_process_mut().set_window_size(cols, rows);
        let mut t = Tui {
            s,
            parser: vt100::Parser::new(rows, cols, 0),
            raw: Vec::new(),
            answered_da1: false,
            answered_dsr: 0,
            kitty: false,
        };
        t.kitty = kitty;
        t
    }

    pub fn pid(&self) -> i32 {
        self.s.get_process().pid().as_raw()
    }

    /// Reads what is available and feeds the screen model.
    pub fn pump(&mut self) {
        let mut buf = [0u8; 65536];
        loop {
            match self.s.try_read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    self.raw.extend_from_slice(&buf[..n]);
                    self.parser.process(&buf[..n]);
                }
                Err(_) => break,
            }
        }
        // Answer the primary device attributes query and cursor position reports, as a
        // terminal would.
        if !self.answered_da1 && self.raw.windows(3).any(|w| w == b"\x1b[c") {
            self.answered_da1 = true;
            if self.kitty {
                self.send(b"\x1b[?0u");
            }
            self.send(b"\x1b[?62;22c");
        }
        let dsr = self.raw.windows(4).filter(|w| *w == b"\x1b[6n").count();
        while self.answered_dsr < dsr {
            self.answered_dsr += 1;
            let (r, c) = self.parser.screen().cursor_position();
            let reply = format!("\x1b[{};{}R", r + 1, c + 1);
            self.send(reply.as_bytes());
        }
    }

    pub fn screen(&self) -> String {
        self.parser.screen().contents()
    }

    pub fn wait_for(&mut self, what: &str, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            self.pump();
            if self.screen().contains(what) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    pub fn wait_until(&mut self, timeout: Duration, mut f: impl FnMut(&mut Tui) -> bool) -> bool {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            self.pump();
            if f(self) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    pub fn send(&mut self, b: &[u8]) {
        use std::io::Write;
        self.s.write_all(b).unwrap();
        self.s.flush().unwrap();
    }

    /// Sends keys one event at a time, so escape sequences are not merged.
    pub fn keys(&mut self, seq: &[&[u8]]) {
        for k in seq {
            self.send(k);
            std::thread::sleep(Duration::from_millis(30));
            self.pump();
        }
    }

    pub fn signal(&self, sig: rustix::process::Signal) {
        let pid = rustix::process::Pid::from_raw(self.pid()).unwrap();
        rustix::process::kill_process(pid, sig).unwrap();
    }

    /// The process state letter from `/proc/<pid>/stat` (`T` = stopped).
    pub fn state(&self) -> Option<char> {
        let s = std::fs::read_to_string(format!("/proc/{}/stat", self.pid())).ok()?;
        let after = s.rsplit_once(')')?.1;
        after.trim_start().chars().next()
    }

    /// Waits for the process to exit; returns its exit code (or `128 + signal`).
    pub fn wait_exit(&mut self, timeout: Duration) -> Option<i32> {
        use expectrl::process::unix::WaitStatus;
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            self.pump();
            match self.s.get_process().status() {
                Ok(WaitStatus::Exited(_, c)) => {
                    self.pump();
                    return Some(c);
                }
                Ok(WaitStatus::Signaled(_, s, _)) => return Some(128 + s as i32),
                _ => {}
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }

    /// Whether the raw output after the last `\x1b[?1049h` leaves the alternate screen.
    pub fn restored(&self) -> bool {
        let enter = find_last(&self.raw, b"\x1b[?1049h");
        let leave = find_last(&self.raw, b"\x1b[?1049l");
        matches!((enter, leave), (Some(e), Some(l)) if l > e)
    }
}

pub fn find_last(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self
            .s
            .get_process_mut()
            .kill(expectrl::process::unix::Signal::SIGKILL);
    }
}
