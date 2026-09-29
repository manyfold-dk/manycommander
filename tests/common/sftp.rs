//! SFTP test helpers (P3 8.4): `sftp-server` on plain pipes, a scripted server on the codec,
//! a latency helper that forwards the pipes with an injected delay, and process hygiene for
//! the tests that spawn `ssh` and `sshd`.
//!
//! Nothing here contacts a host or a port. Every test that spawns a server first sets
//! `RLIMIT_CORE` to 0 for itself, so no child it spawns (and no child of those) can leave
//! a core dump when a test kills it, and `sftp-server` starts through
//! `sh -c 'ulimit -c 0; exec ...'` as well ([`sftp_server_command`]).
#![allow(dead_code)]

use super::skip;
use manycommander::remote::proto::{self, Packet};
use manycommander::remote::session::OnLost;
use manycommander::remote::transport::{self, ChildPeer, Peer};
use manycommander::remote::{Lost, Session};
use rustix::fd::OwnedFd;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const SFTP_SERVER: &str = "/usr/lib/ssh/sftp-server";

/// No core dumps from this test process or anything it spawns (`sshd`, `ssh`,
/// `sftp-server`): an expected kill must never reach the desktop as a crash report.
pub fn no_core_dumps() {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
    let max = getrlimit(Resource::Core).maximum;
    setrlimit(
        Resource::Core,
        Rlimit {
            current: Some(0),
            maximum: max,
        },
    )
    .expect("RLIMIT_CORE 0");
}

/// Whether `sftp-server` is installed; skips the test otherwise.
pub fn have_sftp_server() -> bool {
    if !Path::new(SFTP_SERVER).exists() {
        skip("sftp-server is not installed");
        return false;
    }
    true
}

/// A loss callback and what it received.
pub fn lost_channel() -> (OnLost, Receiver<Lost>) {
    let (tx, rx) = channel();
    let f: OnLost = Box::new(move |l| {
        let _ = tx.send(l);
    });
    (f, rx)
}

/// `sftp-server -e -d <dir> <extra>` started as `sh -c 'ulimit -c 0; exec ...'`: the shell
/// execs into the server, so the process is the server, and it never dumps core. `-e`: its
/// log goes to stderr.
pub fn sftp_server_command(dir: &Path, extra: &[&str]) -> Command {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg("ulimit -c 0; exec \"$0\" \"$@\"")
        .arg(SFTP_SERVER)
        .arg("-e")
        .arg("-d")
        .arg(dir)
        .args(extra);
    cmd
}

/// `sftp-server -e -d <dir>` on plain pipes (no sshd). stdin stays open for the session's
/// life: `sftp-server` exits on stdin EOF without flushing replies it still owes.
pub fn sftp_server(dir: &Path) -> (Session, Receiver<Lost>) {
    no_core_dumps();
    let (on_lost, rx) = lost_channel();
    let cmd = sftp_server_command(dir, &[]);
    let s = transport::start_plain(cmd, Some(on_lost)).expect("sftp-server session");
    (s, rx)
}

/// Whether process `pid` still exists (a zombie counts: it is not reaped).
pub fn exists(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Waits up to `t` for `pid` to be gone.
pub fn gone_within(pid: u32, t: Duration) -> bool {
    let end = Instant::now() + t;
    while Instant::now() < end {
        if !exists(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    !exists(pid)
}

fn pipe() -> (File, File) {
    let (r, w) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).unwrap();
    (File::from(r), File::from(w))
}

fn pipe_fds() -> (OwnedFd, OwnedFd) {
    rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).unwrap()
}

// ---- the scripted server --------------------------------------------------------------

/// The server ends of a scripted session's pipes.
pub struct Server {
    rx: File,
    tx: Arc<Mutex<Option<File>>>,
}

impl Server {
    /// The next request, or `None` when the session closed its end.
    pub fn request(&mut self) -> Option<Packet> {
        let body = proto::read_frame(&mut self.rx).ok()??;
        Some(Packet::decode(body).expect("the client sent a malformed packet"))
    }

    /// The next request if one arrives within `t`: `None` on a timeout, `Some(None)` when
    /// the session closed its end.
    pub fn request_within(&mut self, t: Duration) -> Option<Option<Packet>> {
        let mut fds = [rustix::event::PollFd::new(
            &self.rx,
            rustix::event::PollFlags::IN,
        )];
        let ts = rustix::event::Timespec {
            tv_sec: t.as_secs() as _,
            tv_nsec: t.subsec_nanos() as _,
        };
        match rustix::event::poll(&mut fds, Some(&ts)) {
            Ok(0) => None,
            _ => Some(self.request()),
        }
    }

    /// Writes `b` as it is; `false` when the session killed its peer.
    pub fn raw(&mut self, b: &[u8]) -> bool {
        let mut g = self.tx.lock().unwrap();
        match g.as_mut() {
            Some(f) => f.write_all(b).is_ok(),
            None => false,
        }
    }

    pub fn reply(&mut self, p: &Packet) -> bool {
        self.raw(&p.encode())
    }

    /// Answers `SSH_FXP_INIT` with `SSH_FXP_VERSION` and these extensions; returns the INIT
    /// frame exactly as it arrived.
    pub fn hello(&mut self, exts: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut len = [0u8; 4];
        self.rx.read_exact(&mut len).unwrap();
        let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
        self.rx.read_exact(&mut body).unwrap();
        let mut frame = len.to_vec();
        frame.extend_from_slice(&body);
        let v = Packet::Version {
            version: 3,
            extensions: exts.iter().map(|(k, v)| (k.to_vec(), v.to_vec())).collect(),
        };
        self.reply(&v);
        frame
    }

    /// Closes the reply pipe, as a server that exits does.
    pub fn hang_up(&mut self) {
        self.tx.lock().unwrap().take();
    }
}

/// A scripted session's peer: a kill closes the reply pipe, as a killed ssh's stdout
/// closes, and waiting means the script has returned.
struct ScriptedPeer {
    tx: Arc<Mutex<Option<File>>>,
    done: Arc<AtomicBool>,
}

impl Peer for ScriptedPeer {
    fn kill(&mut self) {
        self.tx.lock().unwrap().take();
    }

    fn wait(&mut self, grace: Duration) -> bool {
        let end = Instant::now() + grace;
        while !self.done.load(Ordering::SeqCst) {
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }
}

/// A session against `script`, which runs on its own thread with the server ends of the
/// pipes and must begin with [`Server::hello`].
pub fn scripted(
    script: impl FnOnce(Server) + Send + 'static,
) -> (Session, Receiver<Lost>, JoinHandle<()>) {
    let (req_r, req_w) = pipe_fds();
    let (rep_r, rep_w) = pipe_fds();
    let tx = Arc::new(Mutex::new(Some(File::from(rep_w))));
    let done = Arc::new(AtomicBool::new(false));
    let server = Server {
        rx: File::from(req_r),
        tx: tx.clone(),
    };
    let d = done.clone();
    let h = std::thread::spawn(move || {
        script(server);
        d.store(true, Ordering::SeqCst);
    });
    let (on_lost, rx) = lost_channel();
    let s = transport::start_pipes(
        rep_r,
        req_w,
        Box::new(ScriptedPeer { tx, done }),
        Some(on_lost),
    )
    .expect("scripted session");
    (s, rx, h)
}

// ---- the latency helper ---------------------------------------------------------------

/// Forwards `from` to `to`, each chunk `delay` after it arrived: one direction of a link
/// with latency. Ends when either side closes.
fn delayed(mut from: File, mut to: File, delay: Duration) {
    let (tx, rx) = channel::<(Instant, Vec<u8>)>();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            match from.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if tx.send((Instant::now(), buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
            }
        }
    });
    std::thread::spawn(move || {
        for (at, b) in rx {
            let due = at + delay;
            let now = Instant::now();
            if due > now {
                std::thread::sleep(due - now);
            }
            if to.write_all(&b).is_err() {
                return;
            }
        }
    });
}

/// `sftp-server -e -d <dir>` behind a link that delays each direction by `delay` (a round
/// trip of twice that). The session's peer is `sftp-server` itself.
pub fn sftp_server_with_latency(dir: &Path, delay: Duration) -> (Session, Receiver<Lost>) {
    use std::os::unix::process::CommandExt;
    no_core_dumps();
    let mut child = sftp_server_command(dir, &[])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .expect("sftp-server");
    let server_in = File::from(OwnedFd::from(child.stdin.take().unwrap()));
    let server_out = File::from(OwnedFd::from(child.stdout.take().unwrap()));
    let (req_r, req_w) = pipe();
    let (rep_r, rep_w) = pipe();
    delayed(req_r, server_in, delay);
    delayed(server_out, rep_w, delay);
    let (on_lost, rx) = lost_channel();
    let s = transport::start_pipes(
        OwnedFd::from(rep_r),
        OwnedFd::from(req_w),
        Box::new(ChildPeer::new(child)),
        Some(on_lost),
    )
    .expect("session over the latency helper");
    (s, rx)
}

// ---- process hygiene ------------------------------------------------------------------

/// A process as `/proc/<pid>/stat` shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proc {
    pub pid: i32,
    pub comm: String,
    pub state: char,
    pub ppid: i32,
    pub pgrp: i32,
    pub tpgid: i32,
    pub start: u64,
}

pub fn proc_stat(pid: i32) -> Option<Proc> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let open = s.find('(')?;
    let (head, after) = s.rsplit_once(')')?;
    let comm = head[open + 1..].to_owned();
    let f: Vec<&str> = after.split_whitespace().collect();
    Some(Proc {
        pid,
        comm,
        state: f.first()?.chars().next()?,
        ppid: f.get(1)?.parse().ok()?,
        pgrp: f.get(2)?.parse().ok()?,
        tpgid: f.get(5)?.parse().ok()?,
        start: f.get(19)?.parse().ok()?,
    })
}

fn all_procs() -> Vec<Proc> {
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter_map(proc_stat)
        .collect()
}

/// Every descendant of `root`.
pub fn descendants(root: i32) -> Vec<Proc> {
    let all = all_procs();
    let mut out: Vec<Proc> = Vec::new();
    let mut frontier = vec![root];
    while let Some(p) = frontier.pop() {
        for c in all.iter().filter(|c| c.ppid == p) {
            if !out.iter().any(|o| o.pid == c.pid) {
                frontier.push(c.pid);
                out.push(c.clone());
            }
        }
    }
    out
}

/// Those of `procs` that still run as the same process (same start time).
pub fn still_running(procs: &[Proc]) -> Vec<Proc> {
    procs
        .iter()
        .filter(|p| proc_stat(p.pid).is_some_and(|q| q.start == p.start && q.state != 'Z'))
        .cloned()
        .collect()
}

/// `SIGKILL` to those of `procs` that still run as the same process: a test never leaves
/// a server behind, and never kills a reused process id.
pub fn kill_all(procs: &[Proc]) {
    for p in still_running(procs) {
        if let Some(pid) = rustix::process::Pid::from_raw(p.pid) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }
}

/// Collects the descendants of `root` whenever [`Tracker::scan`] runs, and kills the ones
/// still running when dropped: children reparented after their parent died are still
/// known by then.
#[derive(Default)]
pub struct Tracker {
    pub seen: Vec<Proc>,
}

impl Tracker {
    pub fn scan(&mut self, root: i32) {
        for p in descendants(root) {
            if !self
                .seen
                .iter()
                .any(|s| s.pid == p.pid && s.start == p.start)
            {
                self.seen.push(p);
            }
        }
    }

    /// Waits up to `t` for every process seen to be gone; returns the ones left.
    pub fn wait_gone(&self, t: Duration) -> Vec<Proc> {
        let end = Instant::now() + t;
        loop {
            let left = still_running(&self.seen);
            if left.is_empty() || Instant::now() >= end {
                return left;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Tracker {
    fn drop(&mut self) {
        kill_all(&self.seen);
    }
}
