#![forbid(unsafe_code)]
//! An SFTP session (P3 2.5, 5.3): one `ssh` child, one reader thread, one stderr thread.
//!
//! The reader thread routes each reply by request id into its caller's reply slot. Callers
//! write their requests under one mutex and wait on their slots, re-checking their cancel
//! flag every [`POLL`], so no manycommander thread blocks in the kernel on an SFTP request:
//! the network I/O happens in the `ssh` child. The request pipe is non-blocking for the
//! same reason; a writer that the pipe holds back re-checks too.
//!
//! **Cancel and the stuck-session rule.** A cancelled caller sends nothing more and drains
//! the replies to its outstanding requests, dropping them by id; a handle among them (an
//! `OPEN` or `OPENDIR` that completed anyway) is closed, not dropped. OpenSSH's
//! `sftp-server` serves one request at a time, and an `OPEN` that met a FIFO blocks it and
//! every later request of the session. So when no reply to the caller's requests arrives
//! for [`DRAIN`] (2 s) while one is outstanding, the session is stuck: it is marked
//! unusable, the child's process group is killed and reaped, and the caller gets
//! "connection lost". A healthy server answers the requests in flight, and the session
//! stays usable after a cancel.
//!
//! **Session loss.** Stdout EOF, a decode error, a reply to an unknown request, a write
//! that fails, or the stuck rule mark the session lost: every outstanding request fails
//! with "connection lost", the child is reaped, and the loss callback
//! (`Remote(SessionLost)` in the app) gets the reason and ssh's last stderr line. A session
//! closed on purpose reports nothing.
//!
//! **Requests that change the server** (P3 5.6) go through [`Session::call_firm`] (the
//! [`Firm`] calls): a cancel does not drop their replies, so the caller always learns what
//! the server did (a temporary name it created, a commit that completed); after a cancel the
//! server gets the drain window to answer, and a silent one ends the session as above.
//!
//! **Pipelining.** [`FileReader`] and [`Writes`] keep a window of `READ` or `WRITE`
//! requests outstanding ([`WINDOW`] of [`CHUNK`] bytes; the request size rises to the
//! `limits@openssh.com` lengths, at most [`CHUNK_MAX`], in whole pages). Replies may arrive
//! in any order; a short read re-requests the missing range, and `SSH_FX_EOF` ends the file.
//!
//! **Batches.** The server executes a session's requests as if one at a time, in the order
//! they were sent, though it may answer them in any order (draft-ietf-secsh-filexfer-02,
//! section 7; OpenSSH's `sftp-server` serves one request at a time, P3 2.5). So a request
//! sent behind others without waiting for their replies sees their effects: a [`Batch`] of
//! requests costs one round trip, and its replies are matched by id. A file transfer uses
//! this for the requests that follow its data: a download's final `FSTAT` and `CLOSE` go out
//! right behind its last `READ` ([`Session::reader_exact`]), an upload's `FSETSTAT`, `fsync`
//! and `CLOSE` right behind its last `WRITE` ([`Writes`]).

use super::proto::{self, Attrs, Extensions, Limits, Name, Packet, Payload, StatVfs, ext, status};
use super::transport::{Peer, Stderr};
use crate::provider::{Caps, PlaceError};
use rustix::fd::OwnedFd;
use rustix::io::Errno;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// How often a waiting caller re-checks its cancel flag and the session (P3 2.5).
pub const POLL: Duration = Duration::from_millis(50);

/// The drain window (P3 2.5): a cancelled caller whose outstanding requests get no reply
/// for this long ends the session.
pub const DRAIN: Duration = Duration::from_secs(2);

/// How long a closing session waits for its child after closing ssh's stdin (P3 5.2).
pub const CLOSE_GRACE: Duration = Duration::from_millis(500);

/// Requests outstanding per transfer (P3 5.3), tuned in T10: 128 requests of [`CHUNK_MAX`]
/// keep 15.5 MiB in flight, about what sftp(1) keeps with OpenSSH's limits (64 requests of
/// 255 KiB), so a link with a large bandwidth-delay product stays full. At a 30 ms round
/// trip, 64 requests of 64 KiB (8 MiB in flight) took 1.05x and 1.54x of sftp's time for a
/// download and an upload; 128 of 124 KiB took 0.74x and 1.09x.
pub const WINDOW: usize = 128;

/// The request size before `limits@openssh.com` raises it: sftp(1)'s default.
pub const CHUNK: u32 = 32 * 1024;

/// The largest request size (P3 5.3), tuned in T10. It is a multiple of the page size, so
/// the server's writes of an upload and ours of a download start on page boundaries: the
/// 255 KiB that OpenSSH announces is not, and unaligned `pwrite`s made an upload over pipes
/// 1.4 to 1.7x slower (btrfs). And a `DATA` reply's frame (the data and 9 bytes) stays below
/// the 128 KiB mmap threshold that `fsops::sys::tune_allocator` fixes, so no reply maps and
/// unmaps fresh pages: 255 KiB replies made a download 1.45x slower than sftp's.
pub const CHUNK_MAX: u32 = 124 * 1024;

/// The size asked for the two pipes to the peer (the unprivileged maximum by default).
const PIPE_SIZE: usize = 1 << 20;

/// What a failed call reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SftpError {
    /// The server answered with a status other than `SSH_FX_OK`.
    Status { code: u32, message: String },
    /// The caller cancelled; its outstanding replies were drained, and the session is
    /// usable.
    Cancelled,
    /// The session ended (P3 2.5, 5.3).
    Lost,
    /// The local side of a transfer failed (the source of an upload).
    Local(String),
}

impl fmt::Display for SftpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SftpError::Status { code, message } => {
                // The server's own text only for a plain failure: the other codes say
                // more than most servers' messages.
                if *code == status::FAILURE && !message.is_empty() {
                    f.write_str(message)
                } else {
                    f.write_str(proto::status_text(*code))
                }
            }
            SftpError::Cancelled => f.write_str("cancelled"),
            SftpError::Lost => f.write_str("connection lost"),
            SftpError::Local(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for SftpError {}

impl From<SftpError> for PlaceError {
    fn from(e: SftpError) -> PlaceError {
        match e {
            SftpError::Status {
                code: status::NO_SUCH_FILE,
                ..
            } => PlaceError::NotFound,
            SftpError::Status {
                code: status::PERMISSION_DENIED,
                ..
            } => PlaceError::Os(Errno::ACCESS),
            e @ SftpError::Status { .. } => PlaceError::Refused(e.to_string()),
            SftpError::Cancelled => PlaceError::Cancelled,
            SftpError::Lost => PlaceError::Lost,
            SftpError::Local(e) => PlaceError::Refused(e),
        }
    }
}

/// Why a session ended, as the loss callback reports it (P3 5.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lost {
    pub reason: String,
    /// ssh's last stderr line, when it wrote one.
    pub stderr: Option<String>,
}

impl Lost {
    /// ssh's last line when there is one, else the reason.
    pub fn message(&self) -> &str {
        self.stderr.as_deref().unwrap_or(&self.reason)
    }
}

/// Called once, from the thread that noticed, when a session is lost.
pub type OnLost = Box<dyn FnOnce(Lost) + Send>;

/// Request counts for `--log` (NFR-OBS) and the frame bound of A-SF-1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub requests: u64,
    pub replies: u64,
    /// The largest frame buffer the reader held: a reply never makes it hold more than
    /// the frame itself (P3 5.3).
    pub largest_frame: usize,
}

/// The request sizes and window of a transfer (P3 5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sizes {
    pub read: u32,
    pub write: u32,
    pub window: usize,
}

impl Default for Sizes {
    fn default() -> Sizes {
        Sizes {
            read: CHUNK,
            write: CHUNK,
            window: WINDOW,
        }
    }
}

impl Sizes {
    /// sftp(1)'s default request size raised to what `limits@openssh.com` announces, at
    /// most [`CHUNK_MAX`] and in whole pages. A write leaves room in the server's packet for
    /// the request's header and handle.
    pub fn from_limits(l: &Limits) -> Sizes {
        let max = CHUNK_MAX as u64;
        let pick = |x: u64| {
            if x == 0 {
                CHUNK
            } else {
                whole_pages(x.min(max))
            }
        };
        let mut write = pick(l.write);
        if l.packet > 1024 {
            write = write.min(whole_pages((l.packet - 1024).min(max)));
        }
        Sizes {
            read: pick(l.read),
            write,
            window: WINDOW,
        }
    }
}

/// `n` rounded down to a multiple of 4 KiB; below 4 KiB, `n` itself (at least 1).
fn whole_pages(n: u64) -> u32 {
    let n = n.min(u64::from(u32::MAX)) as u32;
    if n >= 4096 { n & !4095 } else { n.max(1) }
}

/// The two pipes to the peer, the peer itself and its stderr (P3 5.2).
pub struct Link {
    /// The session number, for thread names and the log.
    pub n: u64,
    /// Replies: ssh's stdout.
    pub input: OwnedFd,
    /// Requests: ssh's stdin.
    pub output: OwnedFd,
    pub peer: Box<dyn Peer>,
    pub stderr: Option<Arc<Stderr>>,
}

/// What `SSH_FXP_VERSION` said, and any bytes read past it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hello {
    pub version: u32,
    pub extensions: Extensions,
    pub leftover: Vec<u8>,
}

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

/// A number for the next session's threads and log lines.
pub fn next_number() -> u64 {
    NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
}

enum Slot {
    Waiting,
    Done(Packet),
    /// Cancelled: the reply is dropped when it arrives.
    Dropped,
}

#[derive(Default)]
struct Table {
    slots: HashMap<u32, Slot>,
    lost: Option<String>,
    /// Handles in replies nobody reads (a cancelled `OPEN` or `OPENDIR`): the drain that
    /// dropped them closes them, so a cancel leaves no handle open on the server (P3 5.5).
    orphans: Vec<Vec<u8>>,
}

impl Table {
    /// Drops a reply nobody reads; a handle in it is kept for the drain to close.
    fn discard(&mut self, p: Packet) {
        if let Packet::Handle { handle, .. } = p {
            self.orphans.push(handle);
        }
    }
}

/// The cancel flag of the requests that close orphaned handles: a cancelled caller's, so a
/// request pipe that stays full for [`DRAIN`] ends the session (P3 2.5).
static CLOSING: AtomicBool = AtomicBool::new(true);

struct Inner {
    n: u64,
    version: u32,
    extensions: Extensions,
    /// The request pipe, non-blocking; `None` once the session ended.
    out: Mutex<Option<OwnedFd>>,
    table: Mutex<Table>,
    cv: Condvar,
    next_id: AtomicU32,
    peer: Mutex<Option<Box<dyn Peer>>>,
    pid: Option<u32>,
    stderr: Option<Arc<Stderr>>,
    on_lost: Mutex<Option<OnLost>>,
    requests: AtomicU64,
    replies: AtomicU64,
    largest: AtomicUsize,
    sizes: OnceLock<Sizes>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// An open SFTP session. Clones share it; when the last clone is dropped, the session
/// closes (ssh's stdin closes, and a helper thread reaps the child).
#[derive(Clone)]
pub struct Session {
    h: Arc<Handle>,
}

struct Handle {
    inner: Arc<Inner>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.inner.close(false);
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Session({})", self.h.inner.n)
    }
}

impl Session {
    /// Starts the session's reader thread on a link that has seen `SSH_FXP_VERSION`.
    pub fn start(link: Link, hello: Hello, on_lost: Option<OnLost>) -> std::io::Result<Session> {
        // Requests go out non-blocking: a writer the pipe holds back re-checks its cancel
        // flag (P3 2.5). Replies are read blocking by the reader thread.
        set_nonblocking(&link.output, true)?;
        set_nonblocking(&link.input, false)?;
        // Room for a few 256 KiB packets in each pipe, when the pipes allow it: fewer
        // wakeups per packet. The system limit may refuse; the default size then stays.
        for fd in [&link.input, &link.output] {
            let _ = rustix::pipe::fcntl_setpipe_size(fd, PIPE_SIZE);
        }
        let pid = link.peer.pid();
        let inner = Arc::new(Inner {
            n: link.n,
            version: hello.version,
            extensions: hello.extensions,
            out: Mutex::new(Some(link.output)),
            table: Mutex::new(Table::default()),
            cv: Condvar::new(),
            next_id: AtomicU32::new(1),
            peer: Mutex::new(Some(link.peer)),
            pid,
            stderr: link.stderr,
            on_lost: Mutex::new(on_lost),
            requests: AtomicU64::new(0),
            replies: AtomicU64::new(0),
            largest: AtomicUsize::new(0),
            sizes: OnceLock::new(),
        });
        let r = inner.clone();
        let input = std::fs::File::from(link.input);
        let leftover = hello.leftover;
        // A `list-` name: the panic hook leaves a panic here to this `catch_unwind`, which
        // loses the session and keeps the app up (NFR-REL).
        std::thread::Builder::new()
            .name(format!("list-sftp-{}", link.n))
            .spawn(move || {
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    read_loop(&r, input, leftover)
                }));
                let (reason, eof) = res.unwrap_or_else(|_| ("internal error".into(), false));
                r.lose(reason, !eof);
            })?;
        Ok(Session {
            h: Arc::new(Handle { inner }),
        })
    }

    /// A session that never had a connection: every request fails with "connection lost"
    /// at once, and no thread runs for it. For the tests of places, roots and destinations
    /// that are never read.
    #[doc(hidden)]
    pub fn detached() -> Session {
        let inner = Arc::new(Inner {
            n: next_number(),
            version: proto::VERSION,
            extensions: Extensions::new(),
            out: Mutex::new(None),
            table: Mutex::new(Table {
                lost: Some("not connected".into()),
                ..Table::default()
            }),
            cv: Condvar::new(),
            next_id: AtomicU32::new(1),
            peer: Mutex::new(None),
            pid: None,
            stderr: None,
            on_lost: Mutex::new(None),
            requests: AtomicU64::new(0),
            replies: AtomicU64::new(0),
            largest: AtomicUsize::new(0),
            sizes: OnceLock::new(),
        });
        Session {
            h: Arc::new(Handle { inner }),
        }
    }

    fn i(&self) -> &Inner {
        &self.h.inner
    }

    /// The session number (thread names, the log).
    pub fn number(&self) -> u64 {
        self.i().n
    }

    pub fn version(&self) -> u32 {
        self.i().version
    }

    /// The extensions `SSH_FXP_VERSION` announced.
    pub fn extensions(&self) -> &Extensions {
        &self.i().extensions
    }

    /// Whether `SSH_FXP_VERSION` announced the extension with that version.
    pub fn has(&self, e: (&[u8], &[u8])) -> bool {
        self.i()
            .extensions
            .iter()
            .any(|(k, v)| k.as_slice() == e.0 && v.as_slice() == e.1)
    }

    /// What the server allows, from its extensions (P3 2.1, 5.6).
    pub fn caps(&self) -> Caps {
        Caps {
            write: true,
            hard_link: self.has(ext::HARDLINK),
            posix_rename: self.has(ext::POSIX_RENAME),
            fsync: self.has(ext::FSYNC),
            statvfs: self.has(ext::STATVFS),
            random_access: false,
        }
    }

    /// Why the session ended, or `None` while it is usable. Reads a flag; no I/O.
    pub fn lost(&self) -> Option<String> {
        lock(&self.i().table).lost.clone()
    }

    /// The peer's process id (ssh, or `sftp-server` in the tests).
    pub fn pid(&self) -> Option<u32> {
        self.i().pid
    }

    pub fn stats(&self) -> Stats {
        let i = self.i();
        Stats {
            requests: i.requests.load(Ordering::Relaxed),
            replies: i.replies.load(Ordering::Relaxed),
            largest_frame: i.largest.load(Ordering::Relaxed),
        }
    }

    /// ssh's last stderr line.
    pub fn stderr_line(&self) -> Option<String> {
        self.i().stderr.as_ref().and_then(|s| s.last_line())
    }

    /// Closes the session and waits for the child (up to [`CLOSE_GRACE`], then a kill):
    /// manycommander's exit. Dropping the last clone closes without waiting.
    pub fn close_wait(&self) {
        self.h.inner.close(true);
    }

    // ---- requests and replies ---------------------------------------------------------

    /// Sends the request `build` makes for its id and returns the id; the reply waits in
    /// the id's slot for [`Session::wait`].
    pub fn send(
        &self,
        cancel: &AtomicBool,
        build: impl FnOnce(u32) -> Packet,
    ) -> Result<u32, SftpError> {
        let id = self.register()?;
        let bytes = build(id).encode();
        if let Err(e) = self.write(&[&bytes], cancel) {
            self.unregister(id);
            return Err(e);
        }
        Ok(id)
    }

    /// Sends a request whose reply nobody reads (a `CLOSE` after a cancel): it is dropped
    /// by id when it arrives.
    pub fn send_forget(&self, cancel: &AtomicBool, build: impl FnOnce(u32) -> Packet) {
        if let Ok(id) = self.send(cancel, build) {
            let mut t = lock(&self.i().table);
            match t.slots.get(&id) {
                Some(Slot::Done(_)) => {
                    if let Some(Slot::Done(p)) = t.slots.remove(&id) {
                        t.discard(p);
                    }
                }
                Some(Slot::Waiting) => {
                    t.slots.insert(id, Slot::Dropped);
                }
                _ => {}
            }
        }
    }

    /// `CLOSE`s the handles of replies a drain dropped (P3 5.5); their replies are dropped.
    fn close_orphans(&self) {
        let orphans = std::mem::take(&mut lock(&self.i().table).orphans);
        for handle in orphans {
            self.send_forget(&CLOSING, |id| Packet::Close { id, handle });
        }
    }

    fn register(&self) -> Result<u32, SftpError> {
        let mut t = lock(&self.i().table);
        if t.lost.is_some() {
            return Err(SftpError::Lost);
        }
        let id = loop {
            let id = self.i().next_id.fetch_add(1, Ordering::Relaxed);
            if !t.slots.contains_key(&id) {
                break id;
            }
        };
        t.slots.insert(id, Slot::Waiting);
        Ok(id)
    }

    fn unregister(&self, id: u32) {
        lock(&self.i().table).slots.remove(&id);
    }

    /// Writes one request, in parts, under the writer lock.
    fn write(&self, parts: &[&[u8]], cancel: &AtomicBool) -> Result<(), SftpError> {
        let r = {
            let out = lock(&self.i().out);
            match out.as_ref() {
                None => Err(WriteFail::Lost),
                Some(fd) => write_parts(self.i(), fd, parts, cancel),
            }
        };
        match r {
            Ok(()) => {
                self.i().requests.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(WriteFail::Lost) => Err(SftpError::Lost),
            Err(WriteFail::Stuck) => {
                self.i()
                    .lose("the server stopped reading requests".into(), true);
                Err(SftpError::Lost)
            }
            Err(WriteFail::Io(e)) => {
                self.i().lose(format!("cannot send a request: {e}"), true);
                Err(SftpError::Lost)
            }
        }
    }

    /// Waits for the reply to `id`, re-checking `cancel` every [`POLL`]. On cancel the
    /// slot stays: the caller drains it ([`Session::drain`]).
    pub fn wait(&self, id: u32, cancel: &AtomicBool) -> Result<Packet, SftpError> {
        let i = self.i();
        let mut t = lock(&i.table);
        loop {
            if let Some(Slot::Done(_)) = t.slots.get(&id) {
                let Some(Slot::Done(p)) = t.slots.remove(&id) else {
                    unreachable!()
                };
                return Ok(p);
            }
            if t.lost.is_some() || !t.slots.contains_key(&id) {
                return Err(SftpError::Lost);
            }
            if cancel.load(Ordering::SeqCst) {
                return Err(SftpError::Cancelled);
            }
            t =
                i.cv.wait_timeout(t, POLL)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
        }
    }

    /// Drops the replies to `ids` and waits until they have all arrived (P3 2.5). A handle
    /// among them (a cancelled `OPEN` or `OPENDIR`) is closed, not dropped (P3 5.5). When
    /// none of them arrives for [`DRAIN`] while one is outstanding, the session is stuck:
    /// it is ended, and the result is `Lost`.
    pub fn drain(&self, ids: &[u32]) -> Result<(), SftpError> {
        let i = self.i();
        let mut t = lock(&i.table);
        for id in ids {
            match t.slots.get(id) {
                Some(Slot::Done(_)) => {
                    if let Some(Slot::Done(p)) = t.slots.remove(id) {
                        t.discard(p);
                    }
                }
                Some(Slot::Waiting) => {
                    t.slots.insert(*id, Slot::Dropped);
                }
                _ => {}
            }
        }
        let mut left = usize::MAX;
        let mut progress = Instant::now();
        loop {
            if t.lost.is_some() {
                return Err(SftpError::Lost);
            }
            let n = ids.iter().filter(|id| t.slots.contains_key(id)).count();
            if n == 0 {
                drop(t);
                self.close_orphans();
                return Ok(());
            }
            if n < left {
                left = n;
                progress = Instant::now();
            } else if progress.elapsed() >= DRAIN {
                drop(t);
                tracing::warn!(
                    session = i.n,
                    outstanding = n,
                    "sftp: no reply for 2 s after a cancel"
                );
                i.lose("the server stopped answering".into(), true);
                return Err(SftpError::Lost);
            }
            t =
                i.cv.wait_timeout(t, POLL)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
        }
    }

    /// One request and its reply. A cancel drains the reply and returns `Cancelled`, or
    /// `Lost` when the server stayed silent through the drain window.
    pub fn call(
        &self,
        cancel: &AtomicBool,
        build: impl FnOnce(u32) -> Packet,
    ) -> Result<Packet, SftpError> {
        let id = self.send(cancel, build)?;
        match self.wait(id, cancel) {
            Err(SftpError::Cancelled) => {
                self.drain(&[id])?;
                Err(SftpError::Cancelled)
            }
            r => r,
        }
    }

    /// One request whose reply matters even after a cancel (P3 2.5, 5.6): a request that
    /// changes the server (`OPEN` with `CREAT`, `CLOSE`, `SETSTAT`, `HARDLINK`, `REMOVE`,
    /// ...). A cancel does not drop the reply: the caller learns what the server did, so a
    /// temporary name it created is never forgotten, and a commit that completed is never
    /// reported as undone. After a cancel the server gets [`DRAIN`] to answer; a server that
    /// stays silent that long is stuck, and the session ends with `Lost`.
    pub fn call_firm(
        &self,
        cancel: &AtomicBool,
        build: impl FnOnce(u32) -> Packet,
    ) -> Result<Packet, SftpError> {
        let id = self.send(cancel, build)?;
        let i = self.i();
        let mut t = lock(&i.table);
        let mut since: Option<Instant> = None;
        loop {
            if let Some(Slot::Done(_)) = t.slots.get(&id) {
                let Some(Slot::Done(p)) = t.slots.remove(&id) else {
                    unreachable!()
                };
                return Ok(p);
            }
            if t.lost.is_some() || !t.slots.contains_key(&id) {
                return Err(SftpError::Lost);
            }
            if cancel.load(Ordering::SeqCst) {
                let at = *since.get_or_insert_with(Instant::now);
                if at.elapsed() >= DRAIN {
                    t.slots.insert(id, Slot::Dropped);
                    drop(t);
                    tracing::warn!(session = i.n, "sftp: no reply for 2 s after a cancel");
                    i.lose("the server stopped answering".into(), true);
                    return Err(SftpError::Lost);
                }
            }
            t =
                i.cv.wait_timeout(t, POLL)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
        }
    }

    /// The requests that change the server, each through [`Session::call_firm`] (P3 5.6).
    pub fn firm<'s>(&'s self, cancel: &'s AtomicBool) -> Firm<'s> {
        Firm { s: self, cancel }
    }

    /// Requests sent without waiting, whose replies are awaited together ([`Batch`]).
    pub fn batch<'s>(&'s self, cancel: &'s AtomicBool) -> Batch<'s> {
        Batch {
            s: self,
            cancel,
            ids: Vec::new(),
        }
    }

    /// Waits for the replies to `ids` and hands each to `got` with its index as it arrives,
    /// in any order. As [`Session::call_firm`] waits for one reply, a cancel drops none of
    /// them: the caller learns what the server did. After a cancel, a server that answers
    /// none of them for [`DRAIN`] is stuck, and the session ends (P3 2.5). Returns whether
    /// the cancel flag was set; `Lost` when the session ended first, after every reply that
    /// had arrived was handed over.
    fn gather(
        &self,
        ids: &[u32],
        cancel: &AtomicBool,
        got: &mut dyn FnMut(usize, Packet),
    ) -> Result<bool, SftpError> {
        let i = self.i();
        let mut left: Vec<usize> = (0..ids.len()).collect();
        let mut ready: Vec<(usize, Packet)> = Vec::new();
        // Since the cancel was seen, or since the last reply after it.
        let mut since: Option<Instant> = None;
        loop {
            let (mut lost, mut stuck) = (false, false);
            {
                let mut t = lock(&i.table);
                loop {
                    left.retain(|&k| {
                        if let Some(Slot::Done(_)) = t.slots.get(&ids[k])
                            && let Some(Slot::Done(p)) = t.slots.remove(&ids[k])
                        {
                            ready.push((k, p));
                            return false;
                        }
                        true
                    });
                    if !ready.is_empty() || left.is_empty() {
                        break;
                    }
                    if t.lost.is_some() || left.iter().any(|&k| !t.slots.contains_key(&ids[k])) {
                        lost = true;
                        break;
                    }
                    if cancel.load(Ordering::SeqCst)
                        && since.get_or_insert_with(Instant::now).elapsed() >= DRAIN
                    {
                        for &k in &left {
                            t.slots.insert(ids[k], Slot::Dropped);
                        }
                        stuck = true;
                        break;
                    }
                    t =
                        i.cv.wait_timeout(t, POLL)
                            .unwrap_or_else(|e| e.into_inner())
                            .0;
                }
            }
            if !ready.is_empty() && since.is_some() {
                since = Some(Instant::now());
            }
            for (k, p) in ready.drain(..) {
                got(k, p);
            }
            if stuck {
                tracing::warn!(
                    session = i.n,
                    outstanding = left.len(),
                    "sftp: no reply for 2 s after a cancel"
                );
                i.lose("the server stopped answering".into(), true);
                return Err(SftpError::Lost);
            }
            if lost {
                return Err(SftpError::Lost);
            }
            if left.is_empty() {
                return Ok(cancel.load(Ordering::SeqCst));
            }
        }
    }

    /// A reply of a batch as a status: `Lost` when it never arrived.
    pub fn outcome(&self, p: Option<Packet>) -> Result<(), SftpError> {
        match p {
            Some(p) => self.status(p),
            None => Err(SftpError::Lost),
        }
    }

    /// A reply of the wrong type: the server is confused, so the session ends (E-19).
    pub(crate) fn unexpected(&self, p: &Packet) -> SftpError {
        self.i().lose(
            format!("protocol error: unexpected reply type {}", p.kind()),
            true,
        );
        SftpError::Lost
    }

    pub(crate) fn status(&self, p: Packet) -> Result<(), SftpError> {
        match p {
            Packet::Status {
                code: status::OK, ..
            } => Ok(()),
            Packet::Status { code, message, .. } => Err(status_error(code, &message)),
            p => Err(self.unexpected(&p)),
        }
    }

    fn handle(&self, p: Packet) -> Result<Vec<u8>, SftpError> {
        match p {
            Packet::Handle { handle, .. } => Ok(handle),
            Packet::Status { code, message, .. } => Err(status_error(code, &message)),
            p => Err(self.unexpected(&p)),
        }
    }

    pub(crate) fn attrs(&self, p: Packet) -> Result<Attrs, SftpError> {
        match p {
            Packet::Attrs { attrs, .. } => Ok(attrs),
            Packet::Status { code, message, .. } => Err(status_error(code, &message)),
            p => Err(self.unexpected(&p)),
        }
    }

    /// The single name of a `REALPATH`, `READLINK` or `home-directory` reply.
    fn one_name(&self, p: Packet) -> Result<Vec<u8>, SftpError> {
        match p {
            Packet::Name { mut names, .. } if names.len() == 1 => {
                Ok(std::mem::take(&mut names[0].filename))
            }
            Packet::Status { code, message, .. } => Err(status_error(code, &message)),
            p => Err(self.unexpected(&p)),
        }
    }

    fn ext_reply(&self, p: Packet) -> Result<Vec<u8>, SftpError> {
        match p {
            Packet::ExtendedReply { data, .. } => Ok(data),
            Packet::Status { code, message, .. } => Err(status_error(code, &message)),
            p => Err(self.unexpected(&p)),
        }
    }

    /// An extension request with string arguments; refused without a request when the
    /// server did not announce the extension.
    fn extended(
        &self,
        e: (&[u8], &[u8]),
        args: &[&[u8]],
        cancel: &AtomicBool,
    ) -> Result<Packet, SftpError> {
        if !self.has(e) {
            return Err(SftpError::Status {
                code: status::OP_UNSUPPORTED,
                message: String::new(),
            });
        }
        let data = proto::ext_args(args);
        self.call(cancel, |id| Packet::Extended {
            id,
            name: e.0.to_vec(),
            data,
        })
    }

    // ---- operations -------------------------------------------------------------------

    /// `REALPATH`: the canonical absolute form of `path`.
    pub fn realpath(&self, path: &[u8], cancel: &AtomicBool) -> Result<Vec<u8>, SftpError> {
        let p = self.call(cancel, |id| Packet::Realpath {
            id,
            path: path.to_vec(),
        })?;
        self.one_name(p)
    }

    /// The login directory (P3 5.4): the `home-directory` extension with an empty user
    /// name (the login user), answered with `SSH_FXP_NAME`; else `REALPATH(".")`. OpenSSH
    /// before 9.9 announces the extension but answers the empty name with `SSH_FX_FAILURE`,
    /// so a status reply falls back as well.
    pub fn home(&self, cancel: &AtomicBool) -> Result<Vec<u8>, SftpError> {
        if self.has(ext::HOME_DIRECTORY) {
            match self
                .extended(ext::HOME_DIRECTORY, &[b""], cancel)
                .and_then(|p| self.one_name(p))
            {
                Ok(home) => return Ok(home),
                Err(SftpError::Status { .. }) => {}
                Err(e) => return Err(e),
            }
        }
        self.realpath(b".", cancel)
    }

    /// `LSTAT`: the entry itself, never a symlink's target.
    pub fn lstat(&self, path: &[u8], cancel: &AtomicBool) -> Result<Attrs, SftpError> {
        let p = self.call(cancel, |id| Packet::Lstat {
            id,
            path: path.to_vec(),
        })?;
        self.attrs(p)
    }

    /// `STAT`: follows symlinks (the symlink pass of a listing, P3 5.4).
    pub fn stat(&self, path: &[u8], cancel: &AtomicBool) -> Result<Attrs, SftpError> {
        let p = self.call(cancel, |id| Packet::Stat {
            id,
            path: path.to_vec(),
        })?;
        self.attrs(p)
    }

    pub fn fstat(&self, handle: &[u8], cancel: &AtomicBool) -> Result<Attrs, SftpError> {
        let p = self.call(cancel, |id| Packet::Fstat {
            id,
            handle: handle.to_vec(),
        })?;
        self.attrs(p)
    }

    pub fn setstat(&self, path: &[u8], attrs: Attrs, cancel: &AtomicBool) -> Result<(), SftpError> {
        let p = self.call(cancel, |id| Packet::Setstat {
            id,
            path: path.to_vec(),
            attrs,
        })?;
        self.status(p)
    }

    pub fn fsetstat(
        &self,
        handle: &[u8],
        attrs: Attrs,
        cancel: &AtomicBool,
    ) -> Result<(), SftpError> {
        let p = self.call(cancel, |id| Packet::Fsetstat {
            id,
            handle: handle.to_vec(),
            attrs,
        })?;
        self.status(p)
    }

    /// `OPEN` with `SSH_FXF_*` flags; the handle.
    pub fn open(
        &self,
        path: &[u8],
        pflags: u32,
        attrs: Attrs,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, SftpError> {
        let p = self.call(cancel, |id| Packet::Open {
            id,
            path: path.to_vec(),
            pflags,
            attrs,
        })?;
        self.handle(p)
    }

    pub fn opendir(&self, path: &[u8], cancel: &AtomicBool) -> Result<Vec<u8>, SftpError> {
        let p = self.call(cancel, |id| Packet::Opendir {
            id,
            path: path.to_vec(),
        })?;
        self.handle(p)
    }

    /// One `READDIR` reply: its names (OpenSSH sends up to 100, `.` and `..` among them),
    /// or `None` at `SSH_FX_EOF` (P3 5.4).
    pub fn readdir(
        &self,
        handle: &[u8],
        cancel: &AtomicBool,
    ) -> Result<Option<Vec<Name>>, SftpError> {
        let p = self.call(cancel, |id| Packet::Readdir {
            id,
            handle: handle.to_vec(),
        })?;
        match p {
            Packet::Name { names, .. } => Ok(Some(names)),
            Packet::Status {
                code: status::EOF, ..
            } => Ok(None),
            Packet::Status { code, message, .. } => Err(status_error(code, &message)),
            p => Err(self.unexpected(&p)),
        }
    }

    /// `CLOSE` of a file or directory handle.
    pub fn close_handle(&self, handle: &[u8], cancel: &AtomicBool) -> Result<(), SftpError> {
        let p = self.call(cancel, |id| Packet::Close {
            id,
            handle: handle.to_vec(),
        })?;
        self.status(p)
    }

    pub fn remove(&self, path: &[u8], cancel: &AtomicBool) -> Result<(), SftpError> {
        let p = self.call(cancel, |id| Packet::Remove {
            id,
            path: path.to_vec(),
        })?;
        self.status(p)
    }

    pub fn mkdir(&self, path: &[u8], attrs: Attrs, cancel: &AtomicBool) -> Result<(), SftpError> {
        let p = self.call(cancel, |id| Packet::Mkdir {
            id,
            path: path.to_vec(),
            attrs,
        })?;
        self.status(p)
    }

    pub fn rmdir(&self, path: &[u8], cancel: &AtomicBool) -> Result<(), SftpError> {
        let p = self.call(cancel, |id| Packet::Rmdir {
            id,
            path: path.to_vec(),
        })?;
        self.status(p)
    }

    /// `SSH_FXP_RENAME`; protocol version 3 leaves open whether it replaces (R-1, R-2).
    pub fn rename(&self, from: &[u8], to: &[u8], cancel: &AtomicBool) -> Result<(), SftpError> {
        let p = self.call(cancel, |id| Packet::Rename {
            id,
            from: from.to_vec(),
            to: to.to_vec(),
        })?;
        self.status(p)
    }

    pub fn readlink(&self, path: &[u8], cancel: &AtomicBool) -> Result<Vec<u8>, SftpError> {
        let p = self.call(cancel, |id| Packet::Readlink {
            id,
            path: path.to_vec(),
        })?;
        self.one_name(p)
    }

    /// Creates the symlink `link` pointing at `target` (OpenSSH's argument order on the
    /// wire, P3 5.3).
    pub fn symlink(
        &self,
        link: &[u8],
        target: &[u8],
        cancel: &AtomicBool,
    ) -> Result<(), SftpError> {
        let p = self.call(cancel, |id| Packet::Symlink {
            id,
            link: link.to_vec(),
            target: target.to_vec(),
        })?;
        self.status(p)
    }

    /// `posix-rename@openssh.com`: an atomic `rename(2)` that replaces (R-2).
    pub fn posix_rename(
        &self,
        from: &[u8],
        to: &[u8],
        cancel: &AtomicBool,
    ) -> Result<(), SftpError> {
        let p = self.extended(ext::POSIX_RENAME, &[from, to], cancel)?;
        self.status(p)
    }

    /// `hardlink@openssh.com`: `link(2)`, which never replaces (R-1).
    pub fn hardlink(&self, from: &[u8], to: &[u8], cancel: &AtomicBool) -> Result<(), SftpError> {
        let p = self.extended(ext::HARDLINK, &[from, to], cancel)?;
        self.status(p)
    }

    /// `fsync@openssh.com` on an open handle (R-4).
    pub fn fsync(&self, handle: &[u8], cancel: &AtomicBool) -> Result<(), SftpError> {
        let p = self.extended(ext::FSYNC, &[handle], cancel)?;
        self.status(p)
    }

    /// `statvfs@openssh.com` (P3 5.4).
    pub fn statvfs(&self, path: &[u8], cancel: &AtomicBool) -> Result<StatVfs, SftpError> {
        let p = self.extended(ext::STATVFS, &[path], cancel)?;
        let data = self.ext_reply(p)?;
        StatVfs::parse(&data).map_err(|e| {
            self.i().lose(format!("protocol error: {e}"), true);
            SftpError::Lost
        })
    }

    /// The transfer sizes: sftp(1)'s defaults, raised once by `limits@openssh.com` when
    /// the server has it (P3 5.3). Asked on the first transfer, never on the UI thread.
    pub fn sizes(&self, cancel: &AtomicBool) -> Sizes {
        if let Some(s) = self.i().sizes.get() {
            return *s;
        }
        let s = match self.extended(ext::LIMITS, &[], cancel) {
            Ok(p) => match self.ext_reply(p).map(|d| Limits::parse(&d)) {
                Ok(Ok(l)) => Sizes::from_limits(&l),
                _ => Sizes::default(),
            },
            Err(SftpError::Cancelled) => return Sizes::default(),
            Err(_) => Sizes::default(),
        };
        let _ = self.i().sizes.set(s);
        s
    }

    /// Sets the transfer sizes (tests, and T10's tuning), before the first transfer.
    pub fn set_sizes(&self, s: Sizes) -> bool {
        self.i().sizes.set(s).is_ok()
    }

    /// A pipelined reader of an open file handle (P3 5.3). `size_hint` (the `FSTAT` size)
    /// keeps the window from requesting far past the end; the reader reads until
    /// `SSH_FX_EOF`, and [`FileReader::finish`] sends the final `FSTAT` and the `CLOSE`.
    pub fn reader(
        &self,
        handle: Vec<u8>,
        size_hint: Option<u64>,
        cancel: Arc<AtomicBool>,
    ) -> FileReader {
        self.reader_from(handle, 0, size_hint, cancel)
    }

    /// [`Session::reader`] from `offset` on: the rest of a file after a short read that the
    /// first handle could not ask again (P3 5.5).
    pub fn reader_from(
        &self,
        handle: Vec<u8>,
        offset: u64,
        size_hint: Option<u64>,
        cancel: Arc<AtomicBool>,
    ) -> FileReader {
        let sizes = self.sizes(&cancel);
        FileReader {
            s: self.clone(),
            handle,
            chunk: sizes.read.max(1),
            window: sizes.window.max(1),
            limit: size_hint.unwrap_or(u64::MAX),
            next: offset,
            pos: offset,
            inflight: VecDeque::new(),
            cur: None,
            cur_at: 0,
            eof: false,
            closed: false,
            cancel,
            exact: false,
            tail: None,
            stop: None,
        }
    }

    /// A reader of a file whose size is known (a download, P3 5.5), in batch mode: its
    /// `READ`s end exactly at `size`; right behind the last one go a one-byte `READ` at
    /// `size`, which must meet the end, the final `FSTAT` and the `CLOSE`, without waiting.
    /// A file that fits in the window costs one round trip, the `FSTAT` sent before
    /// [`FileReader::prime`] included. A short read after the `CLOSE` went out ends the
    /// reader with [`Stop::Gap`]; data at `size` with [`Stop::Longer`].
    pub fn reader_exact(&self, handle: Vec<u8>, size: u64, cancel: Arc<AtomicBool>) -> FileReader {
        let mut r = self.reader_from(handle, 0, Some(size), cancel);
        r.exact = true;
        r
    }

    /// The `WRITE`s of an upload into the open `handle` from `offset` ([`Writes`]).
    pub fn writes<'s>(
        &'s self,
        handle: &'s [u8],
        offset: u64,
        cancel: &'s AtomicBool,
    ) -> Writes<'s> {
        let sizes = self.sizes(cancel);
        Writes {
            s: self,
            handle,
            cancel,
            chunk: sizes.write.max(1) as usize,
            window: sizes.window.max(1),
            off: offset,
            acked: 0,
            inflight: VecDeque::new(),
            tail: Vec::new(),
        }
    }

    /// Writes everything `src` yields into the open `handle` from `offset`, with a window
    /// of `WRITE` requests (P3 5.3), and returns the byte count. `progress` gets the
    /// acknowledged bytes. A cancel or an error drains the outstanding replies.
    pub fn write_from(
        &self,
        handle: &[u8],
        offset: u64,
        src: &mut dyn Read,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<u64, SftpError> {
        self.write_from_with(handle, offset, src, cancel, progress, &mut || Ok(()))
    }

    /// [`Session::write_from`], with `before` called before each `WRITE` is sent: an error
    /// from it ends the transfer as a failed `WRITE` would.
    pub fn write_from_with(
        &self,
        handle: &[u8],
        offset: u64,
        src: &mut dyn Read,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64),
        before: &mut dyn FnMut() -> Result<(), SftpError>,
    ) -> Result<u64, SftpError> {
        let mut w = self.writes(handle, offset, cancel);
        w.send_data(src, progress, before)?;
        w.settle(progress)
    }
}

/// Requests sent one after another without waiting for a reply, whose replies are then
/// awaited together: the batch costs one round trip. The server executes them in the
/// order sent, each after the effects of those before it (see the module's "Batches").
pub struct Batch<'s> {
    s: &'s Session,
    cancel: &'s AtomicBool,
    ids: Vec<u32>,
}

impl Batch<'_> {
    /// Sends one request of the batch; returns the index of its reply in [`Replies`].
    pub fn send(&mut self, build: impl FnOnce(u32) -> Packet) -> Result<usize, SftpError> {
        let id = self.s.send(self.cancel, build)?;
        self.ids.push(id);
        Ok(self.ids.len() - 1)
    }

    /// Waits for every reply. A cancel drops none of them, so the caller learns what the
    /// server did; a server that stays silent for [`DRAIN`] after a cancel ends the
    /// session (P3 2.5).
    pub fn collect(self) -> Replies {
        let mut got: Vec<Option<Packet>> = self.ids.iter().map(|_| None).collect();
        let r = self
            .s
            .gather(&self.ids, self.cancel, &mut |k, p| got[k] = Some(p));
        Replies {
            got,
            cancelled: matches!(r, Ok(true)),
            lost: r.is_err(),
        }
    }
}

/// The replies of a [`Batch`] (or of the requests behind an upload's `WRITE`s), by index.
#[derive(Debug, Default)]
pub struct Replies {
    got: Vec<Option<Packet>>,
    /// The cancel flag was set while they arrived.
    pub cancelled: bool,
    /// The session ended before every reply arrived.
    pub lost: bool,
}

impl Replies {
    /// The reply at `k`, once; `None` when it never arrived (the session ended).
    pub fn take(&mut self, k: usize) -> Option<Packet> {
        self.got.get_mut(k).and_then(Option::take)
    }
}

/// The `WRITE`s of one upload with a window of them outstanding (P3 5.3), and the requests
/// that follow them in the same batch (P3 5.6): once the last `WRITE` is sent, the caller
/// sends the file's `FSETSTAT`, `fsync@openssh.com` and `CLOSE` at once with
/// [`Writes::send`], and [`Writes::finish`] waits for every reply. The server executes them
/// after the `WRITE`s, so no byte lands after the times are set, and the `CLOSE` closes the
/// complete file. Dropping it drains what is still outstanding.
pub struct Writes<'s> {
    s: &'s Session,
    handle: &'s [u8],
    cancel: &'s AtomicBool,
    chunk: usize,
    window: usize,
    off: u64,
    acked: u64,
    /// `WRITE`s sent and not answered: `(id, length)`.
    inflight: VecDeque<(u32, u64)>,
    /// The requests sent after the last `WRITE`.
    tail: Vec<u32>,
}

/// What [`Writes::finish`] found.
#[derive(Debug)]
pub struct Written {
    /// The bytes the `WRITE`s acknowledged; else the first failed `WRITE` of the file, or
    /// `Lost` when a `WRITE`'s reply never came.
    pub data: Result<u64, SftpError>,
    /// The replies to the requests sent after the `WRITE`s, by the index
    /// [`Writes::send`] returned.
    pub tail: Replies,
}

impl Writes<'_> {
    /// Sends everything `src` yields, keeping a window of `WRITE`s outstanding, and returns
    /// the bytes sent once the last `WRITE` is out; the window's last replies are still
    /// outstanding. `before` runs before each `WRITE` is sent, `progress` gets the
    /// acknowledged bytes. A cancel, an error of the source or of `before`, a failed
    /// `WRITE` or a lost session drains the outstanding replies and returns the error (a
    /// server silent through the drain window turns it into `Lost`, P3 2.5).
    pub fn send_data(
        &mut self,
        src: &mut dyn Read,
        progress: &mut dyn FnMut(u64),
        before: &mut dyn FnMut() -> Result<(), SftpError>,
    ) -> Result<u64, SftpError> {
        let mut buf = vec![0u8; self.chunk];
        let start = self.off;
        let r = loop {
            if self.inflight.len() >= self.window {
                // The window is full: the oldest reply first.
                if let Err(e) = self.wait_one(progress) {
                    break Err(e);
                }
                continue;
            }
            if self.cancel.load(Ordering::SeqCst) {
                break Err(SftpError::Cancelled);
            }
            let n = match read_full(src, &mut buf) {
                Ok(n) => n,
                Err(e) => break Err(SftpError::Local(e.to_string())),
            };
            if n == 0 {
                break Ok(self.off - start);
            }
            if let Err(e) = before() {
                break Err(e);
            }
            let id = match self.s.register() {
                Ok(id) => id,
                Err(e) => break Err(e),
            };
            let header = proto::write_header(id, self.handle, self.off, n);
            if let Err(e) = self.s.write(&[&header, &buf[..n]], self.cancel) {
                self.s.unregister(id);
                break Err(e);
            }
            self.inflight.push_back((id, n as u64));
            self.off += n as u64;
        };
        if r.is_err() {
            self.drain()?;
        }
        r
    }

    /// Waits for the oldest outstanding `WRITE`; a cancel returns `Cancelled` with the
    /// `WRITE` still outstanding.
    fn wait_one(&mut self, progress: &mut dyn FnMut(u64)) -> Result<(), SftpError> {
        let Some((id, n)) = self.inflight.pop_front() else {
            return Ok(());
        };
        match self.s.wait(id, self.cancel) {
            Ok(p) => {
                self.s.status(p)?;
                self.acked += n;
                progress(self.acked);
                Ok(())
            }
            Err(e) => {
                self.inflight.push_front((id, n));
                Err(e)
            }
        }
    }

    /// Waits for the outstanding `WRITE`s one at a time; a cancel or an error drains the
    /// rest ([`Session::write_from`]). Returns the acknowledged bytes.
    fn settle(&mut self, progress: &mut dyn FnMut(u64)) -> Result<u64, SftpError> {
        while !self.inflight.is_empty() {
            if let Err(e) = self.wait_one(progress) {
                self.drain()?;
                return Err(e);
            }
        }
        Ok(self.acked)
    }

    /// Drops the replies to everything outstanding (P3 2.5).
    fn drain(&mut self) -> Result<(), SftpError> {
        let mut ids: Vec<u32> = self.inflight.drain(..).map(|(id, _)| id).collect();
        ids.append(&mut self.tail);
        if ids.is_empty() {
            return Ok(());
        }
        self.s.drain(&ids)
    }

    /// Sends a request behind the `WRITE`s without waiting (the file's metadata, its
    /// `fsync@openssh.com`, its `CLOSE`); returns the index of its reply in
    /// [`Written::tail`].
    pub fn send(&mut self, build: impl FnOnce(u32) -> Packet) -> Result<usize, SftpError> {
        let id = self.s.send(self.cancel, build)?;
        self.tail.push(id);
        Ok(self.tail.len() - 1)
    }

    /// Waits for every outstanding reply, the window's last `WRITE`s and the requests sent
    /// behind them, as one batch: a cancel drops none of them ([`Batch::collect`]).
    /// `progress` gets the acknowledged bytes.
    pub fn finish(mut self, progress: &mut dyn FnMut(u64)) -> Written {
        let writes: Vec<(u32, u64)> = self.inflight.drain(..).collect();
        let tail = std::mem::take(&mut self.tail);
        let nw = writes.len();
        let mut ids: Vec<u32> = writes.iter().map(|w| w.0).collect();
        ids.extend_from_slice(&tail);
        let mut got: Vec<Option<Packet>> = tail.iter().map(|_| None).collect();
        // The first failed `WRITE` by its place in the file, and how many were answered.
        let mut failed: Option<(usize, SftpError)> = None;
        let mut answered = 0;
        let mut acked = self.acked;
        let s = self.s;
        let r = s.gather(&ids, self.cancel, &mut |k, p| {
            if k >= nw {
                got[k - nw] = Some(p);
                return;
            }
            answered += 1;
            match s.status(p) {
                Ok(()) => {
                    acked += writes[k].1;
                    progress(acked);
                }
                Err(e) => {
                    if failed.as_ref().is_none_or(|(j, _)| k < *j) {
                        failed = Some((k, e));
                    }
                }
            }
        });
        self.acked = acked;
        let data = match failed {
            Some((_, e)) => Err(e),
            None if answered < nw => Err(SftpError::Lost),
            None => Ok(acked),
        };
        Written {
            data,
            tail: Replies {
                got,
                cancelled: matches!(r, Ok(true)),
                lost: r.is_err(),
            },
        }
    }
}

impl Drop for Writes<'_> {
    fn drop(&mut self) {
        let _ = self.drain();
    }
}

/// The requests that change the server, each through [`Session::call_firm`]: a cancel
/// never hides what the server did (P3 2.5, 5.6). Uploads, renames, new directories and
/// deletes use these.
pub struct Firm<'s> {
    s: &'s Session,
    cancel: &'s AtomicBool,
}

impl Firm<'_> {
    fn call(&self, build: impl FnOnce(u32) -> Packet) -> Result<Packet, SftpError> {
        self.s.call_firm(self.cancel, build)
    }

    fn status(&self, build: impl FnOnce(u32) -> Packet) -> Result<(), SftpError> {
        let p = self.call(build)?;
        self.s.status(p)
    }

    /// `OPEN` with `SSH_FXF_*` flags; the handle.
    pub fn open(&self, path: &[u8], pflags: u32, attrs: Attrs) -> Result<Vec<u8>, SftpError> {
        let p = self.call(|id| Packet::Open {
            id,
            path: path.to_vec(),
            pflags,
            attrs,
        })?;
        self.s.handle(p)
    }

    pub fn close(&self, handle: &[u8]) -> Result<(), SftpError> {
        self.status(|id| Packet::Close {
            id,
            handle: handle.to_vec(),
        })
    }

    pub fn fsetstat(&self, handle: &[u8], attrs: Attrs) -> Result<(), SftpError> {
        self.status(|id| Packet::Fsetstat {
            id,
            handle: handle.to_vec(),
            attrs,
        })
    }

    pub fn setstat(&self, path: &[u8], attrs: Attrs) -> Result<(), SftpError> {
        self.status(|id| Packet::Setstat {
            id,
            path: path.to_vec(),
            attrs,
        })
    }

    pub fn remove(&self, path: &[u8]) -> Result<(), SftpError> {
        self.status(|id| Packet::Remove {
            id,
            path: path.to_vec(),
        })
    }

    pub fn mkdir(&self, path: &[u8], attrs: Attrs) -> Result<(), SftpError> {
        self.status(|id| Packet::Mkdir {
            id,
            path: path.to_vec(),
            attrs,
        })
    }

    pub fn rmdir(&self, path: &[u8]) -> Result<(), SftpError> {
        self.status(|id| Packet::Rmdir {
            id,
            path: path.to_vec(),
        })
    }

    /// `SSH_FXP_RENAME`; OpenSSH's server refuses an existing target (R-2).
    pub fn rename(&self, from: &[u8], to: &[u8]) -> Result<(), SftpError> {
        self.status(|id| Packet::Rename {
            id,
            from: from.to_vec(),
            to: to.to_vec(),
        })
    }

    /// Creates the symlink `link` pointing at `target` (OpenSSH's argument order on the
    /// wire, P3 5.3).
    pub fn symlink(&self, link: &[u8], target: &[u8]) -> Result<(), SftpError> {
        self.status(|id| Packet::Symlink {
            id,
            link: link.to_vec(),
            target: target.to_vec(),
        })
    }

    /// An extension request with string arguments; refused without a request when the
    /// server did not announce the extension.
    fn extended(&self, e: (&[u8], &[u8]), args: &[&[u8]]) -> Result<(), SftpError> {
        if !self.s.has(e) {
            return Err(SftpError::Status {
                code: status::OP_UNSUPPORTED,
                message: String::new(),
            });
        }
        let data = proto::ext_args(args);
        self.status(|id| Packet::Extended {
            id,
            name: e.0.to_vec(),
            data,
        })
    }

    /// `hardlink@openssh.com`: `link(2)`, which never replaces (R-1).
    pub fn hardlink(&self, from: &[u8], to: &[u8]) -> Result<(), SftpError> {
        self.extended(ext::HARDLINK, &[from, to])
    }

    /// `posix-rename@openssh.com`: an atomic `rename(2)` that replaces (R-2).
    pub fn posix_rename(&self, from: &[u8], to: &[u8]) -> Result<(), SftpError> {
        self.extended(ext::POSIX_RENAME, &[from, to])
    }

    /// `fsync@openssh.com` on an open handle (R-4).
    pub fn fsync(&self, handle: &[u8]) -> Result<(), SftpError> {
        self.extended(ext::FSYNC, &[handle])
    }
}

fn status_error(code: u32, message: &[u8]) -> SftpError {
    SftpError::Status {
        code,
        message: String::from_utf8_lossy(message).into_owned(),
    }
}

fn read_full(src: &mut dyn Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match src.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

pub(crate) fn set_nonblocking(fd: &OwnedFd, on: bool) -> std::io::Result<()> {
    use rustix::fs::OFlags;
    let flags = rustix::fs::fcntl_getfl(fd)?;
    let want = if on {
        flags | OFlags::NONBLOCK
    } else {
        flags & !OFlags::NONBLOCK
    };
    if want != flags {
        rustix::fs::fcntl_setfl(fd, want)?;
    }
    Ok(())
}

enum WriteFail {
    Lost,
    Stuck,
    Io(Errno),
}

/// Writes `parts` to the non-blocking request pipe. While the pipe is full the writer
/// polls every [`POLL`] and re-checks the session; after a cancel, [`DRAIN`] without
/// progress means the session is stuck (P3 2.5).
fn write_parts(
    i: &Inner,
    fd: &OwnedFd,
    parts: &[&[u8]],
    cancel: &AtomicBool,
) -> Result<(), WriteFail> {
    let mut progress = Instant::now();
    for part in parts {
        let mut rest = *part;
        while !rest.is_empty() {
            match rustix::io::write(fd, rest) {
                Ok(n) => {
                    rest = &rest[n..];
                    progress = Instant::now();
                }
                Err(Errno::INTR) => {}
                Err(Errno::AGAIN) => {
                    if lock(&i.table).lost.is_some() {
                        return Err(WriteFail::Lost);
                    }
                    if cancel.load(Ordering::SeqCst) && progress.elapsed() >= DRAIN {
                        return Err(WriteFail::Stuck);
                    }
                    let mut fds = [rustix::event::PollFd::new(
                        fd,
                        rustix::event::PollFlags::OUT,
                    )];
                    let ts = rustix::event::Timespec {
                        tv_sec: 0,
                        tv_nsec: POLL.as_nanos() as _,
                    };
                    let _ = rustix::event::poll(&mut fds, Some(&ts));
                }
                Err(e) => return Err(WriteFail::Io(e)),
            }
        }
    }
    Ok(())
}

/// The reader thread's loop. Returns why it ended, and whether that was an orderly EOF.
fn read_loop(i: &Inner, input: std::fs::File, leftover: Vec<u8>) -> (String, bool) {
    // The file itself when the handshake left nothing over: a `Chain` would zero each
    // frame's buffer before reading into it.
    let mut r: Box<dyn Read> = if leftover.is_empty() {
        Box::new(input)
    } else {
        Box::new(std::io::Cursor::new(leftover).chain(input))
    };
    loop {
        let frame = match proto::read_frame(&mut r) {
            Ok(Some(f)) => f,
            Ok(None) => return ("the connection closed".into(), true),
            Err(e) => return (e.to_string(), false),
        };
        i.largest.fetch_max(frame.capacity(), Ordering::Relaxed);
        let p = match Packet::decode(frame) {
            Ok(p) => p,
            Err(e) => return (format!("protocol error: {e}"), false),
        };
        if let Err(why) = i.deliver(p) {
            return (why, false);
        }
    }
}

impl Inner {
    /// Puts a reply into its caller's slot, or drops it by id after a cancel (a handle in
    /// it waits for the drain to close it). A reply to an id nobody waits for, or a packet
    /// that is no reply, ends the session.
    fn deliver(&self, p: Packet) -> Result<(), String> {
        if !p.is_reply() {
            return Err(format!(
                "protocol error: packet type {} is not a reply",
                p.kind()
            ));
        }
        let id = p.id().unwrap_or_default();
        self.replies.fetch_add(1, Ordering::Relaxed);
        let mut t = lock(&self.table);
        match t.slots.get(&id) {
            Some(Slot::Waiting) => {
                t.slots.insert(id, Slot::Done(p));
            }
            Some(Slot::Dropped) => {
                t.slots.remove(&id);
                t.discard(p);
            }
            _ => {
                return Err(format!(
                    "protocol error: a reply to an unknown request ({id})"
                ));
            }
        }
        drop(t);
        self.cv.notify_all();
        Ok(())
    }

    /// Marks the session lost once: every waiter fails, no request goes out, the child is
    /// reaped (killed at once with `kill`, else after [`CLOSE_GRACE`]), and the loss
    /// callback runs.
    fn lose(&self, reason: String, kill: bool) {
        {
            let mut t = lock(&self.table);
            if t.lost.is_some() {
                return;
            }
            t.lost = Some(reason.clone());
            // A reply that arrived before the end stays for its caller: what the server did
            // is known (a batch's commit, P3 5.6). Every other request fails.
            t.slots.retain(|_, s| matches!(s, Slot::Done(_)));
            // The server's handles end with it.
            t.orphans.clear();
        }
        self.cv.notify_all();
        // A writer the pipe holds back sees the flag within POLL and lets go of the lock.
        *lock(&self.out) = None;
        let peer = lock(&self.peer).take();
        if let Some(mut p) = peer
            && (kill || !p.wait(CLOSE_GRACE))
        {
            p.kill();
        }
        let stderr = self.stderr.as_ref().and_then(|s| {
            s.wait_eof(Duration::from_millis(200));
            s.last_line()
        });
        tracing::info!(
            session = self.n,
            requests = self.requests.load(Ordering::Relaxed),
            replies = self.replies.load(Ordering::Relaxed),
            %reason,
            stderr = stderr.as_deref().unwrap_or(""),
            "sftp session lost"
        );
        let f = lock(&self.on_lost).take();
        if let Some(f) = f {
            f(Lost { reason, stderr });
        }
    }

    /// Ends the session on purpose: no loss is reported, and every waiter fails at once.
    /// Then ssh's stdin closes, and the child gets [`CLOSE_GRACE`] to exit before it is
    /// killed: here with `wait` (manycommander's exit), else on a helper thread, so a
    /// handle dropped on the UI thread never waits for the writer lock or the child.
    fn close(self: &Arc<Self>, wait: bool) {
        {
            let mut t = lock(&self.table);
            if t.lost.is_none() {
                t.lost = Some("closed".into());
            }
            t.slots.clear();
        }
        self.cv.notify_all();
        drop(lock(&self.on_lost).take());
        // Nothing to close: a lost session's child was reaped when it was lost.
        if lock(&self.peer).is_none() {
            *lock(&self.out) = None;
            return;
        }
        if wait {
            self.finish_close(false);
            return;
        }
        let i = self.clone();
        let spawned = std::thread::Builder::new()
            .name("list-sftp-close".into())
            .spawn(move || i.finish_close(false));
        if let Err(e) = spawned {
            // No thread: the child is killed rather than left behind.
            tracing::warn!("sftp: cannot start the closing thread: {e}");
            self.finish_close(true);
        }
    }

    fn finish_close(&self, kill: bool) {
        *lock(&self.out) = None;
        let Some(mut p) = lock(&self.peer).take() else {
            return;
        };
        tracing::info!(
            session = self.n,
            requests = self.requests.load(Ordering::Relaxed),
            replies = self.replies.load(Ordering::Relaxed),
            "sftp session closed"
        );
        if kill || !p.wait(CLOSE_GRACE) {
            p.kill();
        }
    }
}

/// A pipelined reader of one open file (P3 5.3). It keeps up to a window of `READ`
/// requests outstanding and yields the bytes in order. A short read re-requests the
/// missing range first; `SSH_FX_EOF` ends the file. [`FileReader::finish`] sends the final
/// `FSTAT` and the `CLOSE` together and returns their replies. Dropping it drains what is
/// still outstanding and closes the handle.
///
/// In batch mode ([`Session::reader_exact`], a download) the size is known: the `READ`s end
/// exactly at it, and right behind the last one go a one-byte `READ` at the size, which must
/// meet the end, the final `FSTAT` and the `CLOSE`, all without waiting. The server executes
/// them in that order, so the `FSTAT` still sees any change made while the file was read.
/// What cannot be asked again once the `CLOSE` is out ends the reader early with a
/// [`Stop`].
pub struct FileReader {
    s: Session,
    handle: Vec<u8>,
    chunk: u32,
    window: usize,
    /// Requests are not sent ahead from here on (the size hint); one at a time after it.
    /// In batch mode, the size.
    limit: u64,
    /// The next offset to request.
    next: u64,
    /// The offset of the next byte to deliver.
    pos: u64,
    /// `(id, offset, len)` in delivery order.
    inflight: VecDeque<(u32, u64, u32)>,
    cur: Option<Payload>,
    cur_at: usize,
    eof: bool,
    /// The `CLOSE` went out.
    closed: bool,
    cancel: Arc<AtomicBool>,
    /// Batch mode.
    exact: bool,
    /// The ids of the final `FSTAT` and the `CLOSE`, from when they are sent until
    /// [`FileReader::finish`] takes their replies.
    tail: Option<(u32, u32)>,
    stop: Option<Stop>,
}

/// Why a reader in batch mode ended before the end of the file (P3 5.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// A short read after the `CLOSE` had gone out: the bytes from
    /// [`FileReader::position`] on need another handle.
    Gap,
    /// The `READ` at the planned size found data: the file is longer than planned.
    Longer,
}

impl FileReader {
    /// Sends the first window of `READ`s (and in batch mode, when they reach the size, the
    /// requests behind them) without waiting for a reply.
    pub fn prime(&mut self) -> Result<(), SftpError> {
        self.fill()
    }

    fn read_at(&mut self, off: u64, len: u32) -> Result<u32, SftpError> {
        let handle = &self.handle;
        self.s.send(&self.cancel, |id| Packet::Read {
            id,
            handle: handle.clone(),
            offset: off,
            len,
        })
    }

    fn fill(&mut self) -> Result<(), SftpError> {
        if self.exact {
            return self.fill_exact();
        }
        while !self.eof
            && self.inflight.len() < self.window
            && (self.next < self.limit || self.inflight.is_empty())
        {
            let (off, len) = (self.next, self.chunk);
            let id = self.read_at(off, len)?;
            self.inflight.push_back((id, off, len));
            self.next = off + u64::from(len);
        }
        Ok(())
    }

    /// Batch mode: `READ`s up to the size exactly; after the last one, the one-byte `READ`
    /// at the size, the final `FSTAT` and the `CLOSE` (P3 5.5).
    fn fill_exact(&mut self) -> Result<(), SftpError> {
        if self.eof || self.tail.is_some() {
            return Ok(());
        }
        while self.inflight.len() < self.window && self.next < self.limit {
            let off = self.next;
            let len = (self.limit - off).min(u64::from(self.chunk)) as u32;
            let id = self.read_at(off, len)?;
            self.inflight.push_back((id, off, len));
            self.next = off + u64::from(len);
        }
        if self.next < self.limit {
            return Ok(());
        }
        let id = self.read_at(self.limit, 1)?;
        self.inflight.push_back((id, self.limit, 1));
        self.next = self.limit + 1;
        self.send_tail()?;
        Ok(())
    }

    /// Sends the final `FSTAT` and the `CLOSE`; the handle is gone once the `CLOSE` is out.
    fn send_tail(&mut self) -> Result<(), SftpError> {
        let handle = &self.handle;
        let f = self.s.send(&self.cancel, |id| Packet::Fstat {
            id,
            handle: handle.clone(),
        })?;
        let c = match self.s.send(&self.cancel, |id| Packet::Close {
            id,
            handle: handle.clone(),
        }) {
            Ok(c) => c,
            Err(e) => {
                let _ = self.s.drain(&[f]);
                return Err(e);
            }
        };
        self.closed = true;
        self.tail = Some((f, c));
        Ok(())
    }

    /// The next block of the file in order, kept in the frame it arrived in; `None` at the
    /// end, or at a [`Stop`]. After a cancel nothing more is requested, and what is
    /// outstanding is drained.
    pub fn next_block(&mut self) -> Result<Option<Payload>, SftpError> {
        if self.stop.is_some() || (self.eof && self.inflight.is_empty()) {
            return Ok(None);
        }
        if self.cancel.load(Ordering::SeqCst) {
            return Err(self.fail(SftpError::Cancelled));
        }
        if let Err(e) = self.fill() {
            return Err(self.fail(e));
        }
        let Some((id, off, len)) = self.inflight.pop_front() else {
            return Ok(None);
        };
        debug_assert_eq!(off, self.pos);
        let p = match self.s.wait(id, &self.cancel) {
            Ok(p) => p,
            Err(e) => {
                self.inflight.push_front((id, off, len));
                return Err(self.fail(e));
            }
        };
        match p {
            Packet::Data { data, .. } if !data.is_empty() && data.len() <= len as usize => {
                if self.exact && off >= self.limit {
                    // Data at the planned size: the file is longer than planned.
                    self.stop = Some(Stop::Longer);
                    self.eof = true;
                    return Ok(None);
                }
                let n = data.len() as u32;
                if n < len {
                    if self.closed {
                        // A short read after the `CLOSE` went out: the rest of the range
                        // cannot be asked on this handle. What is still in flight is
                        // dropped when the reader finishes.
                        self.stop = Some(Stop::Gap);
                        self.eof = true;
                    } else {
                        // A short read: the rest of the range comes next, before the
                        // requests already in flight.
                        let (roff, rlen) = (off + u64::from(n), len - n);
                        match self.read_at(roff, rlen) {
                            Ok(rid) => self.inflight.push_front((rid, roff, rlen)),
                            Err(e) => return Err(self.fail(e)),
                        }
                    }
                }
                self.pos += u64::from(n);
                Ok(Some(data))
            }
            Packet::Status {
                code: status::EOF, ..
            } => {
                // Everything from `off` on is past the end; later requests are dropped. In
                // batch mode before the size, the file ended early: the final `FSTAT`
                // decides (P3 5.5).
                self.eof = true;
                self.drain_all()?;
                Ok(None)
            }
            Packet::Status { code, message, .. } => Err(self.fail(status_error(code, &message))),
            p => {
                let e = self.s.unexpected(&p);
                self.inflight.clear();
                Err(e)
            }
        }
    }

    /// The bytes delivered so far (with the offset the reader started at).
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Why a reader in batch mode ended early; `None` at the real end.
    pub fn stop(&self) -> Option<Stop> {
        self.stop
    }

    /// The end of the read (P3 5.5): what is still in flight is dropped; then the final
    /// `FSTAT` and the `CLOSE`, sent now unless batch mode sent them behind the last
    /// `READ`, are awaited together, and a cancel drops neither. Returns the attributes the
    /// `FSTAT` saw and what the `CLOSE` answered.
    pub fn finish(&mut self) -> Result<(Attrs, Result<(), SftpError>), SftpError> {
        self.eof = true;
        self.drain_all()?;
        if self.tail.is_none() {
            if self.closed {
                return Err(SftpError::Local("the file is closed".into()));
            }
            self.send_tail()?;
        }
        let Some((f, c)) = self.tail.take() else {
            return Err(SftpError::Lost);
        };
        let (mut fr, mut cr) = (None, None);
        self.s.gather(&[f, c], &self.cancel, &mut |k, p| {
            if k == 0 {
                fr = Some(p);
            } else {
                cr = Some(p);
            }
        })?;
        let a = self.s.attrs(fr.ok_or(SftpError::Lost)?)?;
        Ok((a, self.s.outcome(cr)))
    }

    fn drain_all(&mut self) -> Result<(), SftpError> {
        if self.inflight.is_empty() {
            return Ok(());
        }
        let ids: Vec<u32> = self.inflight.drain(..).map(|(id, ..)| id).collect();
        self.s.drain(&ids)
    }

    /// After an error or a cancel: nothing more is requested, and the outstanding replies
    /// are drained. A server that stays silent through the drain window turns `e` into
    /// `Lost` (P3 2.5).
    fn fail(&mut self, e: SftpError) -> SftpError {
        self.eof = true;
        match self.drain_all() {
            Err(lost) => lost,
            Ok(()) => e,
        }
    }
}

impl Read for FileReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if let Some(c) = &self.cur {
                let rest = &c[self.cur_at..];
                if !rest.is_empty() {
                    let n = rest.len().min(buf.len());
                    buf[..n].copy_from_slice(&rest[..n]);
                    self.cur_at += n;
                    return Ok(n);
                }
            }
            match self.next_block() {
                Ok(Some(b)) => {
                    self.cur = Some(b);
                    self.cur_at = 0;
                }
                Ok(None) => {
                    self.cur = None;
                    return Ok(0);
                }
                // The error itself travels inside: a caller tells `Cancelled` from
                // `Lost` by downcasting.
                Err(e) => return Err(std::io::Error::other(e)),
            }
        }
    }
}

impl Drop for FileReader {
    fn drop(&mut self) {
        let mut ids: Vec<u32> = self.inflight.drain(..).map(|(id, ..)| id).collect();
        if let Some((f, c)) = self.tail.take() {
            ids.extend([f, c]);
        }
        if !ids.is_empty() {
            let _ = self.s.drain(&ids);
        }
        if !self.closed && self.s.lost().is_none() {
            self.closed = true;
            let handle = std::mem::take(&mut self.handle);
            self.s
                .send_forget(&self.cancel, |id| Packet::Close { id, handle });
        }
    }
}

/// The `SftpError` inside an I/O error from a [`FileReader`].
pub fn sftp_error(e: &std::io::Error) -> Option<&SftpError> {
    e.get_ref().and_then(|x| x.downcast_ref::<SftpError>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_follow_the_servers_limits_up_to_124_kib_in_whole_pages() {
        assert_eq!(Sizes::default().read, 32 * 1024);
        // OpenSSH's 255 KiB: capped, and a whole number of pages.
        let openssh = Limits {
            packet: 256 * 1024,
            read: 256 * 1024 - 1024,
            write: 256 * 1024 - 1024,
            handles: 0,
        };
        let s = Sizes::from_limits(&openssh);
        assert_eq!((s.read, s.write), (CHUNK_MAX, CHUNK_MAX));
        assert_eq!(CHUNK_MAX % 4096, 0);
        assert_eq!(s.window, WINDOW);
        // A reply's frame (9 bytes and the data) stays below the fixed mmap threshold.
        let frame = 9 + s.read as usize;
        assert!(frame < 128 << 10, "{frame}");
        let big = Limits {
            packet: 1 << 30,
            read: 1 << 30,
            write: 1 << 30,
            handles: 0,
        };
        let s = Sizes::from_limits(&big);
        assert_eq!((s.read, s.write), (CHUNK_MAX, CHUNK_MAX));
        // Smaller limits are rounded down to whole pages; tiny ones are kept.
        let odd = Limits {
            packet: 70_000,
            read: 100_000,
            write: 100_000,
            handles: 0,
        };
        let s = Sizes::from_limits(&odd);
        assert_eq!((s.read, s.write), (98_304, 65_536));
        let tiny = Limits {
            packet: 0,
            read: 1000,
            write: 1000,
            handles: 0,
        };
        let s = Sizes::from_limits(&tiny);
        assert_eq!((s.read, s.write), (1000, 1000));
        let s = Sizes::from_limits(&Limits::default());
        assert_eq!((s.read, s.write), (CHUNK, CHUNK));
    }

    #[test]
    fn errors_map_to_place_errors() {
        let e = |code| SftpError::Status {
            code,
            message: "m".into(),
        };
        assert_eq!(
            PlaceError::from(e(status::NO_SUCH_FILE)),
            PlaceError::NotFound
        );
        assert_eq!(
            PlaceError::from(e(status::PERMISSION_DENIED)),
            PlaceError::Os(Errno::ACCESS)
        );
        assert_eq!(
            PlaceError::from(e(status::FAILURE)),
            PlaceError::Refused("m".into())
        );
        assert_eq!(
            PlaceError::from(e(status::OP_UNSUPPORTED)).to_string(),
            "not supported by the server"
        );
        assert_eq!(PlaceError::from(SftpError::Lost), PlaceError::Lost);
        assert_eq!(SftpError::Lost.to_string(), "connection lost");
    }
}
