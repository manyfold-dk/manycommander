#![forbid(unsafe_code)]
//! The transport (P3 5.2): the user's own `ssh`, unmodified (R-6).
//!
//! **Argv.** `ssh` is spawned by argv, never through a shell: the program, then the fixed
//! options, then the other arguments of `sftp.ssh`, then `[-l user] [-p port] -s -- host
//! sftp`. ssh applies the first `-o` value it sees for a keyword, and a command-line option
//! beats `ssh_config`, so the fixed options win. ssh's flags `-A`, `-X`, `-Y`, `-t` and
//! `-e` override an `-o` wherever they stand, so an `sftp.ssh` argument that is or starts
//! with `-o`, or holds one of those flags, is rejected with the argument named. Nothing
//! manycommander passes relaxes host-key checking or authentication, and it never answers
//! an ssh prompt.
//!
//! **Process group.** ssh runs in its own process group (`process_group(0)`), so a
//! `Ctrl+C` typed into a later hand-off's child (the pager of an F3) never reaches it.
//!
//! **The connect hand-off** ([`connect`], steps 2 to 5 of P3 5.2) runs on the UI thread with
//! the terminal handed off: ssh's group becomes the terminal's foreground group, so ssh can
//! prompt on `/dev/tty` and `Ctrl+C` or `Ctrl+Z` reach only ssh. The UI thread polls ssh's
//! stdout for `SSH_FXP_VERSION` every 50 ms and between polls checks ssh's group with
//! `waitid` (`WEXITED | WSTOPPED | WNOHANG`, and `WNOWAIT` so the child is reaped in one
//! place), because a stopped ssh does not close its stdout. On `VERSION` it takes the
//! foreground back with `SIGTTOU` blocked on the calling thread: a background group that
//! calls `tcsetpgrp` is otherwise stopped. When ssh exits or stops first, its group is
//! killed with `SIGKILL` (a stopped process does not act on `SIGTERM`) and reaped, the
//! foreground comes back, and the caller shows the failure with ssh's last message.
//!
//! **Stderr.** One thread per session reads ssh's stderr: to the terminal during the
//! connect, then into a 4 KiB tail whose last line explains a lost session.

use super::proto::{self, Packet};
use super::session::{self, Hello, Link, OnLost, Session, set_nonblocking};
use super::url;
use crate::provider::Target;
use rustix::fd::{BorrowedFd, OwnedFd};
use rustix::io::Errno;
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// The options manycommander fixes, directly after the program so they win (P3 5.2, R-6).
/// The first four are the ones `sftp(1)` passes to `ssh`; the others keep a user's
/// `RequestTTY`, `RemoteCommand` or escape character away from an SFTP channel.
pub const FIXED: [&str; 8] = [
    "-oForwardAgent=no",
    "-oForwardX11=no",
    "-oClearAllForwardings=yes",
    "-oPermitLocalCommand=no",
    "-oRequestTTY=no",
    "-oRemoteCommand=none",
    "-e",
    "none",
];

/// ssh's flags that override the fixed options wherever they stand; `o` is `-o` itself.
const OVERRIDING: &[u8] = b"oAXYte";

/// ssh's options that take an argument (its `getopt` string).
const WITH_ARG: &[u8] = b"bceilmopBDEFIJLOPQRSWw";

/// How long a failed connect waits for ssh's last messages to reach the terminal.
const STDERR_SETTLE: Duration = Duration::from_millis(300);

/// How often the connect polls for `SSH_FXP_VERSION` between `waitid` checks (P3 5.2).
const VERSION_POLL: Duration = Duration::from_millis(50);

/// `sftp.ssh` checked: a program and its own arguments (P3 5.2). The default is `ssh`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SshCommand {
    program: OsString,
    args: Vec<OsString>,
}

impl SshCommand {
    /// Checks the `sftp.ssh` setting; the error names the refused argument.
    pub fn from_setting(setting: Option<&[String]>) -> Result<SshCommand, String> {
        let words: Vec<&str> = match setting {
            None => vec!["ssh"],
            Some(v) => v.iter().map(String::as_str).collect(),
        };
        let Some((program, args)) = words.split_first() else {
            return Err("sftp.ssh is empty".into());
        };
        if program.is_empty() || program.starts_with('-') {
            return Err(format!("sftp.ssh: {program:?} is not a program"));
        }
        check_args(args)?;
        Ok(SshCommand {
            program: (*program).into(),
            args: args.iter().map(OsString::from).collect(),
        })
    }

    /// The whole argv for `target` (P3 5.2). The target is checked against the address
    /// grammar again first, so nothing is ever spawned for a host that could be an option.
    pub fn argv(&self, target: &Target) -> Result<Vec<OsString>, String> {
        url::check_target(target)?;
        let mut v = Vec::with_capacity(self.args.len() + 16);
        v.push(self.program.clone());
        v.extend(FIXED.iter().map(OsString::from));
        v.extend(self.args.iter().cloned());
        if let Some(u) = &target.user {
            v.push("-l".into());
            v.push(u.into());
        }
        if let Some(p) = target.port {
            v.push("-p".into());
            v.push(p.to_string().into());
        }
        v.push("-s".into());
        v.push("--".into());
        v.push(url::ssh_host(target).into());
        v.push("sftp".into());
        Ok(v)
    }

    /// The command for `target`: argv, three pipes, its own process group.
    pub fn command(&self, target: &Target) -> Result<Command, String> {
        let argv = self.argv(target)?;
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]);
        Ok(c)
    }

    pub fn program(&self) -> &OsString {
        &self.program
    }
}

/// Walks the arguments as ssh's `getopt` does: an option cluster such as `-vA` holds
/// `-A`, and the rest of a cluster after an option that takes an argument is that
/// argument. A word that is no option would end ssh's options and take the host's place,
/// so it is refused too.
fn check_args(args: &[&str]) -> Result<(), String> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        let b = a.as_bytes();
        if b.len() < 2 || b[0] != b'-' || a == "--" {
            return Err(format!(
                "sftp.ssh: {a:?} is not allowed: only options may follow the program"
            ));
        }
        let mut j = 1;
        while j < b.len() {
            let c = b[j];
            if OVERRIDING.contains(&c) {
                return Err(format!(
                    "sftp.ssh: {a} is not allowed: manycommander's fixed ssh options must win"
                ));
            }
            if WITH_ARG.contains(&c) {
                if j + 1 == b.len() {
                    i += 1;
                    if i == args.len() {
                        return Err(format!("sftp.ssh: {a} needs an argument"));
                    }
                }
                break;
            }
            j += 1;
        }
        i += 1;
    }
    Ok(())
}

/// The other end of a session's pipes: ssh, `sftp-server` in the tests, or a scripted
/// server.
pub trait Peer: Send {
    /// Ends the peer at once (`SIGKILL` to ssh's process group) and reaps it.
    fn kill(&mut self);
    /// Waits up to `grace` for the peer to exit; `true` once it is reaped.
    fn wait(&mut self, grace: Duration) -> bool;
    /// The process id, when the peer is a process.
    fn pid(&self) -> Option<u32> {
        None
    }
}

/// A child process in its own process group.
pub struct ChildPeer {
    child: Child,
    reaped: bool,
}

impl ChildPeer {
    pub fn new(child: Child) -> ChildPeer {
        ChildPeer {
            child,
            reaped: false,
        }
    }

    fn group(&self) -> Option<Pid> {
        Pid::from_raw(self.child.id() as i32)
    }
}

impl Peer for ChildPeer {
    fn kill(&mut self) {
        if self.reaped {
            return;
        }
        // The group first, while the unreaped child still holds its id, then the reap.
        if let Some(g) = self.group() {
            let _ = rustix::process::kill_process_group(g, Signal::KILL);
        }
        let _ = self.child.wait();
        self.reaped = true;
    }

    fn wait(&mut self, grace: Duration) -> bool {
        if self.reaped {
            return true;
        }
        let end = Instant::now() + grace;
        loop {
            match self.child.try_wait() {
                Ok(None) => {}
                Ok(Some(_)) | Err(_) => {
                    self.reaped = true;
                    return true;
                }
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn pid(&self) -> Option<u32> {
        Some(self.child.id())
    }
}

/// The size of the stderr tail (P3 2.5).
pub const TAIL: usize = 4096;

#[derive(Default)]
struct ErrState {
    echo: bool,
    tail: VecDeque<u8>,
    eof: bool,
}

/// A session's stderr thread and its tail (P3 2.5, 5.2).
pub struct Stderr {
    state: Mutex<ErrState>,
    cv: Condvar,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Stderr {
    /// Starts the thread on ssh's stderr. With `echo` the bytes also go to the terminal
    /// until [`Stderr::set_echo`] turns that off.
    pub fn spawn(fd: OwnedFd, n: u64, echo: bool) -> std::io::Result<Arc<Stderr>> {
        let s = Arc::new(Stderr {
            state: Mutex::new(ErrState {
                echo,
                ..ErrState::default()
            }),
            cv: Condvar::new(),
        });
        let t = s.clone();
        std::thread::Builder::new()
            .name(format!("list-sftp-err-{n}"))
            .spawn(move || {
                // This thread writes to the terminal while ssh's group owns it; with the
                // terminal's `tostop` set, that write would stop manycommander.
                block_sigttou_here();
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| t.pump(fd)));
                lock(&t.state).eof = true;
                t.cv.notify_all();
            })?;
        Ok(s)
    }

    fn pump(&self, fd: OwnedFd) {
        let mut f = std::fs::File::from(fd);
        let mut buf = [0u8; 4096];
        loop {
            match f.read(&mut buf) {
                Ok(0) => return,
                Ok(n) => self.push(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    }

    fn push(&self, b: &[u8]) {
        let mut s = lock(&self.state);
        if s.echo {
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(b);
            let _ = o.flush();
        }
        s.tail.extend(b);
        let over = s.tail.len().saturating_sub(TAIL);
        s.tail.drain(..over);
    }

    /// Stops copying to the terminal. Takes the lock the thread writes under, so no byte
    /// reaches the terminal after this returns.
    pub fn set_echo(&self, on: bool) {
        lock(&self.state).echo = on;
    }

    /// Waits up to `timeout` for ssh's stderr to close; `true` when it has.
    pub fn wait_eof(&self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        let mut s = lock(&self.state);
        while !s.eof {
            let now = Instant::now();
            if now >= end {
                return false;
            }
            s = self
                .cv
                .wait_timeout(s, end - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }

    /// The last 4 KiB.
    pub fn tail(&self) -> Vec<u8> {
        lock(&self.state).tail.iter().copied().collect()
    }

    /// The last line with text, control characters replaced: what the status line shows.
    pub fn last_line(&self) -> Option<String> {
        last_line(&self.tail())
    }
}

/// The last line of `b` that holds text.
pub fn last_line(b: &[u8]) -> Option<String> {
    b.split(|&c| c == b'\n' || c == b'\r')
        .map(|l| {
            String::from_utf8_lossy(l)
                .chars()
                .map(|c| if c.is_control() { '?' } else { c })
                .collect::<String>()
        })
        .map(|l| l.trim().to_owned())
        .rfind(|l| !l.is_empty())
}

/// Blocks `SIGTTOU` on the calling thread for good (the stderr thread).
fn block_sigttou_here() {
    use nix::sys::signal::{SigSet, SigmaskHow, Signal as NSignal, pthread_sigmask};
    let mut set = SigSet::empty();
    set.add(NSignal::SIGTTOU);
    let _ = pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&set), None);
}

/// Runs `f` with `SIGTTOU` blocked on the calling thread, so a `tcsetpgrp` from a
/// background process group proceeds instead of stopping the process (P3 5.2 step 4).
/// Neither std nor rustix offers a safe signal-mask call; `nix` does.
pub fn with_sigttou_blocked<T>(f: impl FnOnce() -> T) -> T {
    use nix::sys::signal::{SigSet, SigmaskHow, Signal as NSignal, pthread_sigmask};
    let mut set = SigSet::empty();
    set.add(NSignal::SIGTTOU);
    let mut old = SigSet::empty();
    let blocked = pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&set), Some(&mut old)).is_ok();
    let r = f();
    if blocked {
        let _ = pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&old), None);
    }
    r
}

/// A spawned peer and its three pipes.
struct Spawned {
    child: Child,
    stdin: OwnedFd,
    stdout: OwnedFd,
    stderr: OwnedFd,
}

/// Spawns `cmd` by argv with three pipes, in its own process group.
fn spawn(mut cmd: Command) -> std::io::Result<Spawned> {
    use std::os::unix::process::CommandExt;
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = cmd.spawn()?;
    let (Some(i), Some(o), Some(e)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(std::io::Error::other("the pipes are missing"));
    };
    Ok(Spawned {
        child,
        stdin: i.into(),
        stdout: o.into(),
        stderr: e.into(),
    })
}

/// Whether ssh's process group has a child that exited or stopped (P3 5.2 step 3). The
/// child stays unreaped (`WNOWAIT`): it is reaped where its group is killed.
fn watch(group: Pid) -> Option<String> {
    let opts = WaitIdOptions::EXITED
        | WaitIdOptions::STOPPED
        | WaitIdOptions::NOHANG
        | WaitIdOptions::NOWAIT;
    match rustix::process::waitid(WaitId::Pgid(Some(group)), opts) {
        Ok(None) => None,
        Ok(Some(s)) if s.stopped() => Some("ssh was stopped".into()),
        Ok(Some(s)) => Some(match (s.exit_status(), s.terminating_signal()) {
            (Some(c), _) => format!("ssh exited with status {c}"),
            (None, Some(sig)) => format!("ssh was ended by signal {sig}"),
            _ => "ssh ended".into(),
        }),
        Err(Errno::INTR) => None,
        Err(e) => Some(format!("ssh: {e}")),
    }
}

/// Writes `b` completely to a blocking pipe.
fn write_all(fd: &OwnedFd, mut b: &[u8]) -> Result<(), Errno> {
    while !b.is_empty() {
        match rustix::io::write(fd, b) {
            Ok(n) => b = &b[n..],
            Err(Errno::INTR) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Sends `SSH_FXP_INIT` and waits for `SSH_FXP_VERSION` (P3 5.2 steps 2 and 3): the reply
/// pipe is polled every 50 ms, and between polls `check` says whether the peer ended or
/// stopped. `VERSION` is the one packet read here, before the reader thread exists.
fn hello(
    requests: &OwnedFd,
    replies: &OwnedFd,
    check: &mut dyn FnMut() -> Option<String>,
) -> Result<Hello, String> {
    let init = Packet::Init {
        version: proto::VERSION,
        extensions: Vec::new(),
    };
    if let Err(e) = write_all(requests, &init.encode()) {
        return Err(check().unwrap_or_else(|| format!("cannot send SSH_FXP_INIT: {e}")));
    }
    set_nonblocking(replies, true).map_err(|e| e.to_string())?;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if buf.len() >= 4 {
            let n = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
            if n > proto::MAX_PACKET || n == 0 {
                return Err("the server does not speak SFTP (the first reply is malformed)".into());
            }
            if buf.len() >= 4 + n {
                let leftover = buf.split_off(4 + n);
                let body = buf.split_off(4);
                return match Packet::decode(body) {
                    Ok(Packet::Version {
                        version: proto::VERSION,
                        extensions,
                    }) => Ok(Hello {
                        version: proto::VERSION,
                        extensions,
                        leftover,
                    }),
                    Ok(Packet::Version { version, .. }) => {
                        Err(format!("the server speaks SFTP version {version}, not 3"))
                    }
                    Ok(_) => Err("the server did not answer with SSH_FXP_VERSION".into()),
                    Err(e) => Err(format!("protocol error: {e}")),
                };
            }
        }
        let mut fds = [rustix::event::PollFd::new(
            replies,
            rustix::event::PollFlags::IN,
        )];
        let ts = rustix::event::Timespec {
            tv_sec: 0,
            tv_nsec: VERSION_POLL.as_nanos() as _,
        };
        match rustix::event::poll(&mut fds, Some(&ts)) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(e) => return Err(format!("poll: {e}")),
        }
        match rustix::io::read(replies, &mut chunk) {
            Ok(0) => {
                return Err(check().unwrap_or_else(|| "the connection closed".into()));
            }
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                continue;
            }
            Err(Errno::AGAIN) | Err(Errno::INTR) => {}
            Err(e) => return Err(format!("read: {e}")),
        }
        if let Some(why) = check() {
            return Err(why);
        }
    }
}

/// Connects to `target` inside the terminal hand-off (P3 5.2, steps 2 to 5). The caller has
/// parked the input thread, left the alternate screen and raw mode, and printed "connecting
/// to ..."; `tty` is the controlling terminal. On failure ssh's messages are on the screen
/// and the result is its last line; the caller shows `[connection failed]` under them.
pub fn connect(
    cmd: &SshCommand,
    target: &Target,
    tty: BorrowedFd<'_>,
    on_lost: Option<OnLost>,
) -> Result<Session, String> {
    // Checked before anything is spawned.
    let argv = cmd.argv(target)?;
    let saved = rustix::termios::tcgetattr(tty).ok();
    let mut c = Command::new(&argv[0]);
    c.args(&argv[1..]);
    let sp = spawn(c).map_err(|e| format!("{}: {e}", argv[0].to_string_lossy()))?;
    let n = session::next_number();
    let Some(group) = Pid::from_raw(sp.child.id() as i32) else {
        return Err("ssh has no process id".into());
    };
    let mut peer = ChildPeer::new(sp.child);
    let stderr = match Stderr::spawn(sp.stderr, n, true) {
        Ok(s) => s,
        Err(e) => {
            peer.kill();
            return Err(format!("cannot start the stderr thread: {e}"));
        }
    };
    let ours = rustix::process::getpgrp();
    // Step 2: ssh's group owns the terminal, so it can prompt and gets Ctrl+C and Ctrl+Z.
    let foreground = with_sigttou_blocked(|| rustix::termios::tcsetpgrp(tty, group));
    if let Err(e) = &foreground {
        tracing::warn!("sftp: cannot give the terminal to ssh: {e}");
    }
    // A stop ssh took by touching the terminal before it owned it ends here.
    let _ = rustix::process::kill_process_group(group, Signal::CONT);
    let started = Instant::now();
    let r = hello(&sp.stdin, &sp.stdout, &mut || watch(group));
    tracing::info!(
        session = n,
        ok = r.is_ok(),
        ms = started.elapsed().as_millis() as u64,
        "sftp connect"
    );
    match r {
        Ok(h) => {
            // Step 4: the terminal is manycommander's again, and ssh's stderr goes to the
            // tail only.
            if foreground.is_ok() {
                take_foreground(tty, ours);
            }
            stderr.set_echo(false);
            let link = Link {
                n,
                input: sp.stdout,
                output: sp.stdin,
                peer: Box::new(peer),
                stderr: Some(stderr),
            };
            Session::start(link, h, on_lost).map_err(|e| format!("cannot start the session: {e}"))
        }
        Err(why) => {
            // Step 5: ssh exited or stopped (a refused key, Ctrl+C, Ctrl+Z, a prompt it
            // could not show): its group is killed and reaped, the terminal comes back.
            peer.kill();
            if foreground.is_ok() {
                take_foreground(tty, ours);
            }
            if let Some(t) = &saved {
                let _ = rustix::termios::tcsetattr(tty, rustix::termios::OptionalActions::Now, t);
            }
            stderr.wait_eof(STDERR_SETTLE);
            stderr.set_echo(false);
            Err(stderr.last_line().unwrap_or(why))
        }
    }
}

fn take_foreground(tty: BorrowedFd<'_>, ours: Pid) {
    if let Err(e) = with_sigttou_blocked(|| rustix::termios::tcsetpgrp(tty, ours)) {
        tracing::warn!("sftp: cannot take the terminal back: {e}");
    }
}

/// How long [`start_plain`] and [`start_pipes`] wait for `SSH_FXP_VERSION`.
pub const PLAIN_HELLO: Duration = Duration::from_secs(30);

/// Starts a session over a program that speaks SFTP on its stdin and stdout, without a
/// terminal: `sftp-server` in the tests, `ssh` in a benchmark whose connection needs no
/// prompt. The program runs in its own process group; its stderr goes to the tail.
pub fn start_plain(cmd: Command, on_lost: Option<OnLost>) -> Result<Session, String> {
    let sp = spawn(cmd).map_err(|e| e.to_string())?;
    let n = session::next_number();
    let Some(group) = Pid::from_raw(sp.child.id() as i32) else {
        return Err("no process id".into());
    };
    let mut peer = ChildPeer::new(sp.child);
    let stderr = match Stderr::spawn(sp.stderr, n, false) {
        Ok(s) => s,
        Err(e) => {
            peer.kill();
            return Err(e.to_string());
        }
    };
    let end = Instant::now() + PLAIN_HELLO;
    let r = hello(&sp.stdin, &sp.stdout, &mut || {
        watch(group).or_else(|| (Instant::now() >= end).then(|| "no SSH_FXP_VERSION".into()))
    });
    match r {
        Ok(h) => Session::start(
            Link {
                n,
                input: sp.stdout,
                output: sp.stdin,
                peer: Box::new(peer),
                stderr: Some(stderr),
            },
            h,
            on_lost,
        )
        .map_err(|e| e.to_string()),
        Err(e) => {
            peer.kill();
            Err(stderr.last_line().unwrap_or(e))
        }
    }
}

/// Starts a session over a pair of pipes whose other ends a scripted server holds (the
/// tests' codec server).
pub fn start_pipes(
    replies: OwnedFd,
    requests: OwnedFd,
    mut peer: Box<dyn Peer>,
    on_lost: Option<OnLost>,
) -> Result<Session, String> {
    let end = Instant::now() + PLAIN_HELLO;
    let r = hello(&requests, &replies, &mut || {
        (Instant::now() >= end).then(|| "no SSH_FXP_VERSION".into())
    });
    match r {
        Ok(h) => Session::start(
            Link {
                n: session::next_number(),
                input: replies,
                output: requests,
                peer,
                stderr: None,
            },
            h,
            on_lost,
        )
        .map_err(|e| e.to_string()),
        Err(e) => {
            peer.kill();
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setting(words: &[&str]) -> Result<SshCommand, String> {
        let v: Vec<String> = words.iter().map(|s| s.to_string()).collect();
        SshCommand::from_setting(Some(&v))
    }

    fn target(user: Option<&str>, host: &str, port: Option<u16>) -> Target {
        Target {
            user: user.map(str::to_owned),
            host: host.into(),
            port,
        }
    }

    #[test]
    fn the_fixed_options_follow_the_program_and_the_host_follows_dashdash() {
        let c = setting(&[
            "ssh", "-F", "/cfg", "-i", "/key", "-J", "jump", "-vv", "-4C",
        ])
        .unwrap();
        let argv = c.argv(&target(Some("u"), "h", Some(2222))).unwrap();
        let want: Vec<&str> = ["ssh"]
            .into_iter()
            .chain(FIXED)
            .chain([
                "-F", "/cfg", "-i", "/key", "-J", "jump", "-vv", "-4C", "-l", "u", "-p", "2222",
                "-s", "--", "h", "sftp",
            ])
            .collect();
        assert_eq!(argv, want);
        let argv = SshCommand::from_setting(None)
            .unwrap()
            .argv(&target(None, "[fe80:1]", None))
            .unwrap();
        assert_eq!(argv[0], "ssh");
        assert_eq!(&argv[argv.len() - 4..], ["-s", "--", "fe80:1", "sftp"]);
    }

    #[test]
    fn settings_that_could_override_the_fixed_options_are_rejected_by_name() {
        for (bad, named) in [
            (&["ssh", "-o", "ForwardAgent=yes"][..], "-o"),
            (&["ssh", "-oX=y"], "-oX=y"),
            (&["ssh", "-A"], "-A"),
            (&["ssh", "-X"], "-X"),
            (&["ssh", "-Y"], "-Y"),
            (&["ssh", "-t"], "-t"),
            (&["ssh", "-tt"], "-tt"),
            (&["ssh", "-e", "~"], "-e"),
            (&["ssh", "-vA"], "-vA"),
            (&["ssh", "-4o", "X=y"], "-4o"),
        ] {
            let e = setting(bad).unwrap_err();
            assert!(e.contains(named), "{bad:?}: {e}");
        }
        // Option arguments are not flags: a config file named "-A" is allowed.
        assert!(setting(&["ssh", "-F", "-A"]).is_ok());
        assert!(setting(&["ssh", "-F-oX"]).is_ok());
        assert!(setting(&["ssh", "-iA"]).is_ok());
        // A word that is no option would take the host's place.
        for bad in [
            &["ssh", "host"][..],
            &["ssh", "--"],
            &["ssh", "-"],
            &["ssh", "-F"],
        ] {
            assert!(setting(bad).is_err(), "{bad:?}");
        }
        assert!(setting(&[]).is_err());
        assert!(setting(&["-oX"]).is_err());
        assert!(setting(&["/usr/bin/ssh"]).is_ok());
    }

    #[test]
    fn a_host_that_could_be_an_option_is_never_spawned() {
        let c = SshCommand::from_setting(None).unwrap();
        for host in ["-oProxyCommand=x", "a b", "h%1", "h;x"] {
            assert!(c.argv(&target(None, host, None)).is_err(), "{host}");
        }
        assert!(c.argv(&target(Some("-lroot"), "h", None)).is_err());
    }

    #[test]
    fn the_last_line_with_text_explains_a_failure() {
        assert_eq!(
            last_line(b"warning\r\nHost key verification failed.\r\n\n"),
            Some("Host key verification failed.".into())
        );
        assert_eq!(last_line(b"a\x1b[1mb\n"), Some("a?[1mb".into()));
        assert_eq!(last_line(b"\n \n"), None);
    }
}
