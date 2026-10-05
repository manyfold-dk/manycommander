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

/// The extensions of the files F3 opens in their application instead of the pager (M1 6,
/// amendment of 2026-10-05): pictures, documents, audio, video and web pages.
const IN_APPLICATION: &[&[u8]] = &[
    // Pictures.
    b"png", b"jpg", b"jpeg", b"gif", b"webp", b"bmp", b"tif", b"tiff", b"avif", b"heic", b"heif",
    b"ico", b"svg", b"jxl", // Documents.
    b"pdf", b"epub", b"djvu", b"odt", b"ods", b"odp", b"docx", b"xlsx", b"pptx", b"doc", b"xls",
    b"ppt", b"rtf", // Audio.
    b"mp3", b"flac", b"ogg", b"oga", b"opus", b"m4a", b"wav", b"aac", // Video.
    b"mp4", b"m4v", b"mkv", b"webm", b"mov", b"avi", b"mpg", b"mpeg", b"wmv", b"ogv",
    // Web pages: the opener takes them to the default browser.
    b"html", b"htm", b"xhtml",
];

/// Whether F3 opens a file named `name` in its application ([`open`]) instead of the
/// pager: its extension, ignoring case, is one of [`IN_APPLICATION`]. Only the name
/// decides, so the UI thread reads nothing.
pub fn in_application(name: &[u8]) -> bool {
    let Some(dot) = name.iter().rposition(|&b| b == b'.') else {
        return false;
    };
    let ext = name[dot + 1..].to_ascii_lowercase();
    dot > 0 && IN_APPLICATION.contains(&&ext[..])
}

/// The program that opens a file in its application (M1 6, amendment of 2026-10-05):
/// `gio open` when a `gio` is on `path` (the `PATH` value), else `xdg-open`. Outside a
/// desktop environment it knows, `xdg-open` types a file by its content and runs the
/// handler's `Exec` line itself, so a terminal program (`Terminal=true`) starts without a
/// terminal, shows nothing and never ends. `gio` types by name first and starts such a
/// handler in a terminal.
pub fn opener(path: Option<&OsStr>) -> &'static [&'static str] {
    use std::os::unix::fs::PermissionsExt;
    let gio = path.is_some_and(|p| {
        std::env::split_paths(p).any(|d| {
            d.is_absolute()
                && std::fs::metadata(d.join("gio"))
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    });
    if gio { &["gio", "open"] } else { &["xdg-open"] }
}

/// `setsid -f gio open <path>` (or `xdg-open`, see [`opener`]) with stdio on /dev/null; a
/// helper thread reaps the short-lived `setsid`, so no zombie remains. The opener is looked
/// up on `PATH` once, at the first open.
pub fn open(path: &std::path::Path) -> Result<(), String> {
    static OPENER: std::sync::OnceLock<&'static [&'static str]> = std::sync::OnceLock::new();
    let argv = OPENER.get_or_init(|| opener(std::env::var_os("PATH").as_deref()));
    let mut child = Command::new("setsid")
        .arg("-f")
        .args(argv.iter())
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{}: {e}", argv[0]))?;
    let _ = std::thread::Builder::new()
        .name("reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bin(dir: &std::path::Path, name: &str, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// The opener (M1 6, amendment of 2026-10-05): `gio open` when an executable `gio` is
    /// in an absolute `PATH` directory, else `xdg-open`.
    #[test]
    fn opener_prefers_gio_on_path() {
        let t = std::env::temp_dir().join(format!("mc-opener-{}", std::process::id()));
        let (with, without, noexec) = (t.join("with"), t.join("without"), t.join("noexec"));
        bin(&with, "gio", 0o755);
        bin(&without, "xdg-open", 0o755);
        bin(&noexec, "gio", 0o644);
        std::fs::create_dir_all(with.join("gio-dir/gio")).unwrap();
        let path = |dirs: &[&std::path::Path]| std::env::join_paths(dirs).unwrap();
        assert_eq!(opener(Some(&path(&[&without, &with]))), ["gio", "open"]);
        assert_eq!(opener(Some(&path(&[&without]))), ["xdg-open"]);
        assert_eq!(
            opener(Some(&path(&[&noexec]))),
            ["xdg-open"],
            "not executable"
        );
        assert_eq!(
            opener(Some(&path(&[&with.join("gio-dir")]))),
            ["xdg-open"],
            "a directory named gio"
        );
        assert_eq!(opener(Some(OsStr::new("with"))), ["xdg-open"], "relative");
        assert_eq!(opener(None), ["xdg-open"]);
        let _ = std::fs::remove_dir_all(&t);
    }
}
