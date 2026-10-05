//! The release binary on a pty, for the phase 3 checks. Unlike the M1 driver's terminal it
//! keeps no copy of the whole output: image transmits run to megabytes each, so the
//! sequences the checks wait for (the probe's queries, cursor reports, kitty placements and
//! sixel ends) are counted as the bytes arrive, each with the wall time it arrived at.
//!
//! Every run has its own state and config directories, its own `HOME`, and a no-op `gio`
//! and `xdg-open` first on `PATH`: a benchmark never reads the user's configuration or
//! state, and never opens anything on the desktop.

use super::{run_dir, wall};
use expectrl::Session;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// How the terminal answers the startup probe (P3 4.2).
#[derive(Clone, Copy, Debug, Default)]
pub struct Term {
    /// Kitty graphics: answers the graphics query with `OK`.
    pub graphics: bool,
    /// Sixel: `4` in DA1.
    pub sixel: bool,
    /// Answers `CSI 16 t` with this cell size in pixels.
    pub cell: Option<(u16, u16)>,
    /// Answers nothing to the probe (cursor reports are still answered).
    pub silent: bool,
}

impl Term {
    /// Ghostty outside tmux: kitty graphics, a cell size, the keyboard protocol.
    pub fn ghostty() -> Term {
        Term {
            graphics: true,
            cell: Some((10, 20)),
            ..Term::default()
        }
    }

    /// foot: sixel in DA1, a cell size, the keyboard protocol.
    pub fn foot() -> Term {
        Term {
            sixel: true,
            cell: Some((10, 20)),
            ..Term::default()
        }
    }

    /// A truecolor terminal without graphics: halfblocks.
    pub fn plain() -> Term {
        Term::default()
    }

    pub fn silent() -> Term {
        Term {
            silent: true,
            ..Term::default()
        }
    }

    pub fn by_name(name: &str) -> Term {
        match name {
            "kitty" | "ghostty" => Term::ghostty(),
            "sixel" | "foot" => Term::foot(),
            "halfblocks" | "plain" => Term::plain(),
            "silent" => Term::silent(),
            _ => panic!("unknown terminal {name}"),
        }
    }
}

/// Counts of the sequences the checks wait for, with the wall time of the last of each.
#[derive(Clone, Debug, Default)]
pub struct Seen {
    /// Wall times of the kitty placements (`a=p`).
    pub placements: Vec<f64>,
    pub transmits: u64,
    /// Wall times of the ends of sixel images.
    pub sixels: Vec<f64>,
    pub bytes: u64,
}

pub struct Pty {
    s: expectrl::session::OsSession,
    pub parser: vt100::Parser,
    /// The first bytes of output: the probe's queries.
    head: Vec<u8>,
    /// The last bytes of the previous read, so a sequence split across reads is found.
    carry: Vec<u8>,
    term: Term,
    da1: bool,
    dsr_seen: usize,
    dsr_answered: usize,
    in_str: Option<Str>,
    pub seen: Seen,
    pub home: PathBuf,
}

/// What a spawn needs besides the arguments.
pub struct Opts<'a> {
    pub term: Term,
    pub cols: u16,
    pub rows: u16,
    /// Extra `config.toml` text (after `[jump] zoxide = "off"`).
    pub config: &'a str,
}

impl Default for Opts<'_> {
    fn default() -> Self {
        Opts {
            term: Term::plain(),
            cols: 160,
            rows: 50,
            config: "",
        }
    }
}

/// The kind of string the output is inside.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Str {
    Apc,
    Sixel,
}

impl Pty {
    pub fn spawn(bin: &str, args: &[&str], o: &Opts<'_>) -> Pty {
        let home = run_dir("home");
        let config = home.join(".config/manycommander");
        std::fs::create_dir_all(&config).unwrap();
        // zoxide's ranking is never read: it is the user's state (P2 3.3).
        std::fs::write(
            config.join("config.toml"),
            format!("{}\n[jump]\nzoxide = \"off\"\n", o.config),
        )
        .unwrap();
        let mut cmd = Command::new(bin);
        cmd.args(args)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", stub_path(&home))
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("LANG", "C.UTF-8")
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_RUNTIME_DIR", home.join("run"))
            .env("SHELL", "/bin/sh")
            .current_dir(&home);
        std::fs::create_dir_all(home.join("run")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(home.join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut s = Session::spawn(cmd).expect("spawn manycommander on a pty");
        let _ = s.get_process_mut().set_window_size(o.cols, o.rows);
        let mut p = Pty {
            s,
            parser: vt100::Parser::new(o.rows, o.cols, 0),
            head: Vec::new(),
            carry: Vec::new(),
            term: o.term,
            da1: false,
            dsr_seen: 0,
            dsr_answered: 0,
            in_str: None,
            seen: Seen::default(),
            home,
        };
        if !o.term.silent {
            let end = Instant::now() + Duration::from_secs(5);
            while !p.da1 && Instant::now() < end {
                p.pump();
                std::thread::sleep(Duration::from_micros(200));
            }
        }
        p
    }

    pub fn pid(&self) -> i32 {
        self.s.get_process().pid().as_raw()
    }

    /// Reads what is available, feeds the screen model, counts the sequences, and answers
    /// the probe and cursor reports as a terminal does.
    pub fn pump(&mut self) {
        let mut buf = vec![0u8; 1 << 16];
        loop {
            match self.s.try_read(&mut buf) {
                Ok(0) => break,
                Ok(n) => self.feed(&buf[..n]),
                Err(_) => break,
            }
        }
        if !self.da1 && !self.term.silent && contains(&self.head, b"\x1b[c") {
            self.da1 = true;
            let mut reply = Vec::new();
            if self.term.graphics
                && let Some(id) = graphics_query_id(&self.head)
            {
                reply.extend_from_slice(format!("\x1b_Gi={id};OK\x1b\\").as_bytes());
            }
            if let Some((w, h)) = self.term.cell
                && contains(&self.head, b"\x1b[16t")
            {
                reply.extend_from_slice(format!("\x1b[6;{h};{w}t").as_bytes());
            }
            // The keyboard protocol, as the Omarchy terminals have it.
            reply.extend_from_slice(b"\x1b[?0u");
            reply.extend_from_slice(if self.term.sixel {
                b"\x1b[?62;4;22c"
            } else {
                b"\x1b[?62;22c"
            });
            self.send(&reply);
        }
        while self.dsr_answered < self.dsr_seen {
            self.dsr_answered += 1;
            let (r, c) = self.parser.screen().cursor_position();
            self.send(format!("\x1b[{};{}R", r + 1, c + 1).as_bytes());
        }
    }

    /// Feeds output as a fast terminal reads it: the payload of an APC (kitty graphics) or
    /// DCS (sixel) string is skipped with `memchr` and never reaches the screen model; the
    /// placements, transmits, sixel ends and cursor-report queries are counted on the way.
    fn feed(&mut self, b: &[u8]) {
        let now = wall();
        if self.head.len() < 8192 {
            let k = (8192 - self.head.len()).min(b.len());
            self.head.extend_from_slice(&b[..k]);
        }
        self.seen.bytes += b.len() as u64;
        let mut w = std::mem::take(&mut self.carry);
        w.extend_from_slice(b);
        let mut i = 0;
        // Plain output from `plain` up to the next escape goes to the screen model at once.
        let mut plain = 0;
        while i < w.len() {
            let Some(k) = memchr::memchr(0x1b, &w[i..]) else {
                i = w.len();
                break;
            };
            let at = i + k;
            let rest = &w[at..];
            if self.in_str.is_some() {
                // Inside a string: only its terminator matters.
                if rest.len() < 2 {
                    i = at;
                    break;
                }
                if rest[1] == b'\\' {
                    if self.in_str == Some(Str::Sixel) {
                        self.seen.sixels.push(now);
                    }
                    self.in_str = None;
                    plain = at + 2;
                    i = at + 2;
                } else {
                    i = at + 1;
                }
                continue;
            }
            // A sequence start split across reads waits for the next read, when the rest
            // decides what it is.
            let defer = rest.len() < 2
                || (rest[1] == b'_' && rest.len() < 9)
                || (rest.len() < 4 && b"\x1b[6n".starts_with(rest));
            if defer {
                if at > plain {
                    self.parser.process(&w[plain..at]);
                }
                plain = at;
                i = at;
                break;
            }
            if rest[1] == b'_' || rest[1] == b'P' {
                if at > plain {
                    self.parser.process(&w[plain..at]);
                }
                if rest.starts_with(b"\x1b_Ga=p,i=") {
                    self.seen.placements.push(now);
                } else if rest.starts_with(b"\x1b_Ga=t,") {
                    self.seen.transmits += 1;
                }
                self.in_str = Some(if rest[1] == b'P' {
                    Str::Sixel
                } else {
                    Str::Apc
                });
                i = at + 2;
                plain = i;
                continue;
            }
            if rest.starts_with(b"\x1b[6n") {
                self.dsr_seen += 1;
            }
            i = at + 1;
        }
        if self.in_str.is_none() {
            let end = i.min(w.len());
            if end > plain {
                self.parser.process(&w[plain..end]);
            }
            self.carry = w[end..].to_vec();
        } else {
            self.carry = w[i.min(w.len())..].to_vec();
        }
    }

    pub fn send(&mut self, b: &[u8]) {
        use std::io::Write;
        let _ = self.s.write_all(b);
        let _ = self.s.flush();
    }

    /// Sends keys one at a time, `gap` apart.
    pub fn keys(&mut self, keys: &[&[u8]], gap: Duration) {
        for k in keys {
            self.send(k);
            self.idle(gap);
        }
    }

    /// Pumps for `d`.
    pub fn idle(&mut self, d: Duration) {
        let end = Instant::now() + d;
        loop {
            self.pump();
            if Instant::now() >= end {
                return;
            }
            std::thread::sleep(Duration::from_micros(200));
        }
    }

    pub fn screen(&self) -> String {
        self.parser.screen().contents()
    }

    /// Pumps until `f` holds; `false` after `t`.
    pub fn until(&mut self, t: Duration, mut f: impl FnMut(&mut Pty) -> bool) -> bool {
        let end = Instant::now() + t;
        loop {
            self.pump();
            if f(self) {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_micros(200));
        }
    }

    pub fn wait_for(&mut self, what: &str, t: Duration) -> bool {
        self.until(t, |p| p.screen().contains(what))
    }

    /// Waits for `what`, or panics with the screen.
    pub fn expect(&mut self, what: &str, t: Duration) {
        assert!(
            self.wait_for(what, t),
            "{what:?} did not show:\n{}",
            self.screen()
        );
    }

    /// Halfblock cells on the screen.
    pub fn halfblocks(&self) -> usize {
        self.screen().chars().filter(|&c| c == '▀').count()
    }

    /// F10 and the exit; then the process is killed in any case.
    pub fn quit(&mut self) {
        self.send(F10);
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            self.pump();
            if matches!(
                self.s.get_process().status(),
                Ok(expectrl::process::unix::WaitStatus::Exited(..))
            ) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self
            .s
            .get_process_mut()
            .kill(expectrl::process::unix::Signal::SIGKILL);
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

pub const F10: &[u8] = b"\x1b[21~";
pub const DOWN: &[u8] = b"\x1b[B";
pub const UP: &[u8] = b"\x1b[A";
pub const END: &[u8] = b"\x1b[F";
pub const HOME: &[u8] = b"\x1b[H";
pub const ENTER: &[u8] = b"\r";
pub const BACKSPACE: &[u8] = b"\x7f";
pub const TAB: &[u8] = b"\t";
pub const ESC: &[u8] = b"\x1b[27u";
pub const CTRL_Q: &[u8] = b"\x1b[113;5u";
/// Gives the command line the focus: typing goes to the quick filter without it.
pub const CTRL_E: &[u8] = b"\x1b[101;5u";
pub const ALT_Q: &[u8] = b"\x1b[113;3u";

/// `PATH` with a no-op `gio` and `xdg-open` first.
fn stub_path(home: &Path) -> std::ffi::OsString {
    let dir = home.join("stub");
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["gio", "xdg-open"] {
        let stub = dir.join(name);
        std::fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut p = dir.into_os_string();
    p.push(":");
    p.push(std::env::var_os("PATH").unwrap_or_default());
    p
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// The image id of the probe's kitty graphics query (`_Gi=<id>,`).
fn graphics_query_id(raw: &[u8]) -> Option<u32> {
    let at = raw.windows(4).position(|w| w == b"_Gi=")? + 4;
    let digits: Vec<u8> = raw[at..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .copied()
        .collect();
    std::str::from_utf8(&digits).ok()?.parse().ok()
}

/// A `--log` file of this run, below `p3/run`.
pub fn log_file(tag: &str) -> PathBuf {
    let p = super::p3()
        .join("run")
        .join(format!("{tag}-{}.log", std::process::id()));
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let _ = std::fs::remove_file(&p);
    p
}
