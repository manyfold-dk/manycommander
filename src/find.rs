#![forbid(unsafe_code)]
//! Find files (P2 5): the name and content matchers, the parallel search engine and the
//! re-stat of a results tab.
//!
//! **Engine (P2 5.3).** A search runs on a pool of `min(8, available_parallelism)` threads
//! that share a last-in-first-out stack of work items. An item is a directory to open below
//! its parent's fd, or, for a content search, a regular file whose name matched. The
//! parent's fd is shared by `Arc` together with its relative path: it closes when its last
//! queued child has been opened, and because the stack is last-in-first-out the open
//! directory fds stay bounded by about `workers x depth` (NFR-RES). A directory is opened
//! with `O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC` and its fd `statx`ed; an identity
//! `(st_dev, st_ino)` seen before (a bind-mount loop) is not read again. Every directory is
//! `statx`ed before it is opened (`descend`): one the kernel reports as an automount trigger
//! (`STATX_ATTR_AUTOMOUNT`) is never opened, whatever "Stay on this filesystem" says,
//! because `openat` has no `O_NO_AUTOMOUNT` and the open would mount it. A search never
//! triggers an automount; it can still match the trigger by name. With "Stay on this
//! filesystem" a directory whose `mnt_id` differs from the root's is not opened either, so
//! the search never opens another filesystem. Entries come from `getdents64` in 64 KiB
//! batches; `.` and `..` are never matched or queued; `d_type` decides directory versus
//! other, and only `DT_UNKNOWN` costs a `statx`. Every `statx` of the walk uses
//! `AT_SYMLINK_NOFOLLOW | AT_NO_AUTOMOUNT`, and no symlink is ever followed (I-5). A
//! directory or file that cannot be opened or read counts in the search's error total, and
//! the search goes on.
//!
//! **A search never waits on a mount it does not enter.** A `statx` of an entry the walk
//! has not entered (before the open, for a `DT_UNKNOWN` type, for a result's columns) also
//! passes `AT_STATX_DONT_SYNC` ([`Sys::stat_at_cached`]): the kernel answers from the
//! attributes it holds. Without it, the `statx` of a mount point asks that mount's server
//! or daemon for fresh attributes once its attribute cache has expired, and a stalled one
//! (a stopped FUSE daemon, A-FD-7) holds the worker even where "Stay on this filesystem"
//! would never open the mount. The name is still looked up in the parent's filesystem,
//! which the search has entered. The file type, `mnt_id` and automount attribute that
//! decide the open are exact without a refresh. A result's size and times can be as old as
//! its filesystem's attribute cache (on a local filesystem they are current); `Ctrl+R`
//! asks for fresh ones. Only the `statx` of an opened directory's fd (its identity) asks
//! for fresh attributes: the search has entered that filesystem.
//!
//! **Content (P2 5.2).** A literal byte string, ASCII-folded in needle and data unless
//! "Match case". Only regular files whose name matches are read, through the M1 4.3 `O_PATH`
//! sequence, so a FIFO or a device is never opened (I-10). Each worker reads through its
//! own 256 KiB buffer; consecutive chunks overlap by `needle.len() - 1` bytes, so a match
//! across a chunk boundary is found. A sparse file (fewer allocated blocks than its size
//! needs) is read by its data segments (`SEEK_DATA`/`SEEK_HOLE`, with the P2 9.1 error
//! rules): its holes are not read, so a needle that needs hole bytes is not found there.
//!
//! **Results.** Name matches (with content: regular files whose content matched) are sent to
//! the UI as panel entries whose names are paths relative to the root (`a/b/name`, P2 2.4),
//! in batches of at most 4096 (a worker's first batch at most 256), and after at most
//! [`FLUSH_EVERY`] when fewer arrive. A worker also sends the results it holds before it
//! opens a directory, and before a content read longer than one chunk (E-30): either can
//! wait in the kernel on a stalled filesystem, and the results found so far are then on
//! screen. That is at most one send per directory, and none while the worker holds no
//! results. (Results found in the directory a worker is reading can still wait with it,
//! when a lookup or a file open inside an entered filesystem stalls.) `Done` comes last,
//! with the totals. Workers check the stop flag between entries and between content
//! chunks, so a responsive filesystem stops a cancelled search within 100 ms; a worker
//! blocked in the kernel keeps its search [`Search::alive`], which the app's
//! abandoned-search limit counts (P2 2.3). The search stops at [`MAX_RESULTS`] and says
//! so. Every thread runs under `catch_unwind`: a panic ends the search with an error
//! message and the app stays up (NFR-REL).
//!
//! **Re-stat (P2 5.5).** [`restat`] refreshes a results tab on a listing thread: each
//! result's directory is opened once with the P2 2.2 component walk (the root, then
//! `O_NOFOLLOW` per component), and the leaf is `statx`ed without following it. Results
//! that no longer exist, or whose walk meets a symlink, are dropped. The fresh results go
//! to the tab at once, sorted there for the tab's order, so the UI thread only swaps them
//! in (P-1).

use crate::fsops::sys::{Kind, Meta, Sys};
use crate::fsops::walk::{EntryError, errno_text, open_dir_nofollow, open_for_read};
use crate::panel::Listing;
use crate::panel::entry::Entry;
use crate::panel::listing::{Alive, BATCH, FIRST_BATCH, ListingMsg};
use crate::panel::sort::SortSpec;
use crate::panel::{contains_nocase, glob_match, glob_match_nocase};
use memchr::memmem;
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use rustix::fs::{FileType, RawDir};
use rustix::io::Errno;
use std::collections::HashSet;
use std::ffi::{CStr, CString, OsStr};
use std::fmt;
use std::os::unix::ffi::OsStrExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// At most this many worker threads per search (P2 5.3).
pub const MAX_WORKERS: usize = 8;
/// A content search reads files in chunks of this size (P2 5.3).
pub const CHUNK: usize = 256 << 10;
/// The search stops at this many results and says so (P2 5.3).
pub const MAX_RESULTS: u64 = 1_000_000;
/// A worker sends the results it holds after at most this long, so the first rows show at
/// once even when the batch is not full (P-10).
pub const FLUSH_EVERY: Duration = Duration::from_millis(20);
/// The `getdents64` buffer of a worker.
const DIRENT_BUF: usize = 64 << 10;
/// An idle worker looks at the stop flag at least this often.
const IDLE_WAIT: Duration = Duration::from_millis(20);
/// Items a worker keeps before sharing them with idle workers, while it reads a large
/// directory.
const SHARE_EVERY: usize = 64;
/// Shards of the visited set.
const SHARDS: usize = 16;

/// What the find form asks for (P2 5.1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FindSpec {
    /// The directory to search, resolved like a panel path (M1 4.3).
    pub root: PathBuf,
    /// Without `*`, `?` or `[`: a substring of the name; with them, a glob over the whole
    /// name. Empty matches every name.
    pub name: Vec<u8>,
    /// A literal byte string the file must contain; `None`: no content search.
    pub content: Option<Vec<u8>>,
    /// Names starting with `.` are matched and descended.
    pub hidden: bool,
    /// Directories on another mount than the root's are not descended.
    pub stay_on_fs: bool,
    /// Without it, name and content match ignoring ASCII case.
    pub match_case: bool,
}

/// The name matcher (P2 5.2). Allocates nothing per name.
#[derive(Clone, Debug)]
pub struct NameMatcher {
    /// The pattern, ASCII-lowercased unless matching case.
    pat: Vec<u8>,
    glob: bool,
    fold: bool,
    finder: Option<memmem::Finder<'static>>,
}

impl NameMatcher {
    pub fn new(pattern: &[u8], match_case: bool) -> NameMatcher {
        let pat = if match_case {
            pattern.to_vec()
        } else {
            pattern.to_ascii_lowercase()
        };
        let glob = pat.iter().any(|c| matches!(c, b'*' | b'?' | b'['));
        let finder = (!glob && match_case && !pat.is_empty())
            .then(|| memmem::Finder::new(&pat).into_owned());
        NameMatcher {
            pat,
            glob,
            fold: !match_case,
            finder,
        }
    }

    pub fn matches(&self, name: &[u8]) -> bool {
        if self.pat.is_empty() {
            return true;
        }
        match (self.glob, self.fold) {
            (true, true) => glob_match_nocase(&self.pat, name),
            (true, false) => glob_match(&self.pat, name),
            (false, true) => contains_nocase(name, &self.pat),
            (false, false) => self.finder.as_ref().is_some_and(|f| f.find(name).is_some()),
        }
    }
}

/// The content matcher (P2 5.2): a literal needle, ASCII-folded unless matching case.
#[derive(Clone, Debug)]
pub struct ContentMatcher {
    finder: memmem::Finder<'static>,
    len: usize,
    fold: bool,
}

/// Why a file's content was not searched to the end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanError {
    /// The search stopped (cancel, result limit).
    Stopped,
    /// A read or seek failed: the file counts as an error.
    Io(Errno),
}

impl ContentMatcher {
    pub fn new(needle: &[u8], match_case: bool) -> ContentMatcher {
        let n = if match_case {
            needle.to_vec()
        } else {
            needle.to_ascii_lowercase()
        };
        ContentMatcher {
            finder: memmem::Finder::new(&n).into_owned(),
            len: n.len(),
            fold: !match_case,
        }
    }

    /// Whether the open regular file `fd` (with metadata `meta`) contains the needle.
    /// `buf` is the worker's buffer; it is grown to [`CHUNK`] plus the overlap. `stop` is
    /// checked before every chunk. A sparse file's holes are not read (P2 5.2).
    pub fn scan(
        &self,
        sys: &Sys,
        fd: BorrowedFd,
        meta: &Meta,
        buf: &mut Vec<u8>,
        stop: &AtomicBool,
    ) -> Result<bool, ScanError> {
        if self.len == 0 {
            return Ok(true);
        }
        let need = CHUNK + self.len;
        if buf.len() < need {
            buf.resize(need, 0);
        }
        if meta.blocks.saturating_mul(512) >= meta.size {
            return self.scan_range(sys, fd, 0, None, buf, stop);
        }
        // Data segments only (P2 9.1 rules): ENXIO ends the data; EINVAL or EOPNOTSUPP on
        // the first SEEK_DATA means the filesystem cannot tell, so the whole file is read.
        let mut off = 0;
        loop {
            let data = match sys.seek_data("find.seek_data", fd, off) {
                Ok(d) => d,
                Err(Errno::NXIO) => return Ok(false),
                Err(Errno::INVAL | Errno::OPNOTSUPP) if off == 0 => {
                    return self.scan_range(sys, fd, 0, None, buf, stop);
                }
                Err(e) => return Err(ScanError::Io(e)),
            };
            let hole = match sys.seek_hole("find.seek_hole", fd, data) {
                Ok(h) => h,
                Err(Errno::NXIO) => return Ok(false),
                Err(e) => return Err(ScanError::Io(e)),
            };
            if hole <= data {
                return Ok(false);
            }
            if self.scan_range(sys, fd, data, Some(hole), buf, stop)? {
                return Ok(true);
            }
            off = hole;
        }
    }

    /// Searches `[start, end)` (to the end of the file for `None`) in chunks that overlap by
    /// `needle.len() - 1` bytes.
    fn scan_range(
        &self,
        sys: &Sys,
        fd: BorrowedFd,
        start: u64,
        end: Option<u64>,
        buf: &mut [u8],
        stop: &AtomicBool,
    ) -> Result<bool, ScanError> {
        let keep = self.len - 1;
        let mut carry = 0;
        let mut off = start;
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err(ScanError::Stopped);
            }
            let want = match end {
                Some(e) => (e.saturating_sub(off)).min(CHUNK as u64) as usize,
                None => CHUNK,
            };
            if want == 0 {
                return Ok(false);
            }
            let n = sys
                .pread("find.read", fd, &mut buf[carry..carry + want], off)
                .map_err(ScanError::Io)?;
            if n == 0 {
                return Ok(false);
            }
            off += n as u64;
            let filled = carry + n;
            if self.fold {
                buf[carry..filled].make_ascii_lowercase();
            }
            if self.finder.find(&buf[..filled]).is_some() {
                return Ok(true);
            }
            let k = keep.min(filled);
            buf.copy_within(filled - k..filled, 0);
            carry = k;
        }
    }
}

/// The totals of a search, sent with `Done`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Directories read.
    pub dirs: u64,
    /// Other entries seen.
    pub files: u64,
    /// Directories and files that could not be opened or read.
    pub errors: u64,
    pub results: u64,
    pub elapsed: Duration,
    /// The user cancelled the search.
    pub cancelled: bool,
    /// The search stopped at [`MAX_RESULTS`].
    pub truncated: bool,
    /// The search failed: the root could not be opened, or a thread panicked.
    pub error: Option<String>,
}

/// What a results tab says about its search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Searching,
    Cancelled,
    Done,
    /// Stopped at [`MAX_RESULTS`].
    Truncated,
    Failed(String),
}

/// One search, shared by its results tab and its threads. The threads update the counters;
/// the UI reads them and sets the cancel flag, both without a syscall (P-1).
pub struct Search {
    /// Events of a search carry its id.
    pub id: u64,
    pub spec: FindSpec,
    /// The workers stop: a cancel, the result limit, a panic.
    stop: AtomicBool,
    cancelled: AtomicBool,
    truncated: AtomicBool,
    dirs: AtomicU64,
    files: AtomicU64,
    errors: AtomicU64,
    results: AtomicU64,
    /// The search's threads have not all returned (P2 2.3: a cancelled search that stays
    /// alive is abandoned).
    pub alive: Alive,
    done: OnceLock<Stats>,
}

impl fmt::Debug for Search {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Search")
            .field("id", &self.id)
            .field("spec", &self.spec)
            .field("state", &self.state())
            .finish()
    }
}

impl PartialEq for Search {
    fn eq(&self, o: &Search) -> bool {
        self.id == o.id
    }
}

impl Eq for Search {}

impl Search {
    pub fn new(id: u64, spec: FindSpec) -> Search {
        Search {
            id,
            spec,
            stop: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            truncated: AtomicBool::new(false),
            dirs: AtomicU64::new(0),
            files: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            results: AtomicU64::new(0),
            alive: Alive::running(),
            done: OnceLock::new(),
        }
    }

    /// Cancels the search (`Esc`, a new search, leaving the tab): the workers stop between
    /// entries and chunks; the results found so far stay.
    pub fn cancel(&self) {
        if self.done.get().is_none() {
            self.cancelled.store(true, Ordering::SeqCst);
        }
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Neither finished nor cancelled.
    pub fn running(&self) -> bool {
        self.done.get().is_none() && !self.cancelled.load(Ordering::Relaxed)
    }

    /// The search's threads have recorded their totals: every batch has been sent, and
    /// only `Done` follows (E-28). Until then, a cancelled search can still send batches.
    pub fn finished(&self) -> bool {
        self.done.get().is_some()
    }

    /// Cancelled, and its threads have not recorded their totals yet: batches can still
    /// arrive.
    pub fn stopping(&self) -> bool {
        self.done.get().is_none() && self.cancelled.load(Ordering::Relaxed)
    }

    /// Records the final totals (the first call wins).
    pub fn finish(&self, stats: Stats) {
        let _ = self.done.set(stats);
    }

    pub fn stats(&self) -> Option<&Stats> {
        self.done.get()
    }

    pub fn state(&self) -> State {
        if let Some(s) = self.done.get()
            && let Some(e) = &s.error
        {
            return State::Failed(e.clone());
        }
        if self.cancelled.load(Ordering::Relaxed) {
            State::Cancelled
        } else if self.truncated.load(Ordering::Relaxed) {
            State::Truncated
        } else if self.done.get().is_some() {
            State::Done
        } else {
            State::Searching
        }
    }

    /// The error total: so far, or the final one.
    pub fn errors(&self) -> u64 {
        match self.done.get() {
            Some(s) => s.errors,
            None => self.errors.load(Ordering::Relaxed),
        }
    }

    /// Directories read so far.
    pub fn dirs(&self) -> u64 {
        self.dirs.load(Ordering::Relaxed)
    }

    /// The totals as they stand.
    fn snapshot(&self, elapsed: Duration, error: Option<String>) -> Stats {
        Stats {
            dirs: self.dirs.load(Ordering::SeqCst),
            files: self.files.load(Ordering::SeqCst),
            errors: self.errors.load(Ordering::SeqCst),
            results: self.results.load(Ordering::SeqCst).min(MAX_RESULTS),
            elapsed,
            cancelled: self.cancelled.load(Ordering::SeqCst),
            truncated: self.truncated.load(Ordering::SeqCst),
            error,
        }
    }

    /// The tab title (P2 5.1): `find: <name> "<text>"`, `find: *` for every name.
    pub fn title(&self) -> String {
        let esc = crate::ui::text::escaped;
        let mut t = String::from("find: ");
        if !self.spec.name.is_empty() {
            t += &esc(&self.spec.name);
        } else if self.spec.content.is_none() {
            t.push('*');
        }
        if let Some(c) = &self.spec.content {
            if !self.spec.name.is_empty() {
                t.push(' ');
            }
            t += &format!("\"{}\"", esc(c));
        }
        t
    }
}

/// What the search threads send (P2 2.3).
#[derive(Debug)]
pub enum FindMsg {
    /// Results: entries whose names are paths relative to the root.
    Batch {
        id: u64,
        entries: Vec<Entry>,
        names: Vec<u8>,
    },
    /// Always last.
    Done { id: u64, stats: Stats },
}

// ---- the engine -------------------------------------------------------------------------

/// A directory being read: its fd and its path relative to the root.
struct DirHandle {
    fd: OwnedFd,
    rel: Vec<u8>,
}

enum Item {
    /// A directory the coordinator opened (the root).
    Open(Arc<DirHandle>),
    /// A subdirectory to open below its parent.
    Dir {
        parent: Arc<DirHandle>,
        name: CString,
    },
    /// A regular file whose name matched, to read (content search).
    File {
        parent: Arc<DirHandle>,
        name: CString,
    },
}

#[derive(Default)]
struct Queue {
    items: Vec<Item>,
    /// Workers processing an item: the search ends when none is and the stack is empty.
    busy: usize,
}

/// A worker's own state: its results batch and buffers.
struct Out {
    entries: Vec<Entry>,
    names: Vec<u8>,
    limit: usize,
    /// When the oldest unsent result was found.
    since: Option<Instant>,
    scratch: Vec<u8>,
    children: Vec<Item>,
    files: Vec<Item>,
}

struct Engine<'a> {
    search: &'a Search,
    send: &'a (dyn Fn(FindMsg) + Sync),
    sys: Sys,
    names: NameMatcher,
    content: Option<ContentMatcher>,
    root_mnt: u64,
    visited: Vec<Mutex<HashSet<(u64, u64)>>>,
    queue: Mutex<Queue>,
    cv: Condvar,
    panic: Mutex<Option<String>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The relative path of `name` below the relative directory `rel`.
fn join_rel(out: &mut Vec<u8>, rel: &[u8], name: &[u8]) {
    out.clear();
    out.extend_from_slice(rel);
    if !rel.is_empty() {
        out.push(b'/');
    }
    out.extend_from_slice(name);
}

impl<'a> Engine<'a> {
    fn new(search: &'a Search, send: &'a (dyn Fn(FindMsg) + Sync), root_mnt: u64) -> Self {
        let spec = &search.spec;
        Engine {
            search,
            send,
            sys: Sys::default(),
            names: NameMatcher::new(&spec.name, spec.match_case),
            content: spec
                .content
                .as_ref()
                .filter(|c| !c.is_empty())
                .map(|c| ContentMatcher::new(c, spec.match_case)),
            root_mnt,
            visited: (0..SHARDS).map(|_| Mutex::default()).collect(),
            queue: Mutex::default(),
            cv: Condvar::new(),
            panic: Mutex::new(None),
        }
    }

    fn error(&self) {
        self.search.errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a directory identity; `false` when it was seen before (P2 5.2).
    fn visit(&self, id: (u64, u64)) -> bool {
        let h = (id.0 ^ id.1.rotate_left(17)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        lock(&self.visited[(h >> 60) as usize % SHARDS]).insert(id)
    }

    /// Hands the collected items to the shared stack: subdirectories first, so the files
    /// (popped first) release their directory's fd early.
    fn share(&self, out: &mut Out) {
        if out.children.is_empty() && out.files.is_empty() {
            return;
        }
        let mut q = lock(&self.queue);
        q.items.append(&mut out.children);
        q.items.append(&mut out.files);
        drop(q);
        self.cv.notify_all();
    }

    fn flush(&self, out: &mut Out) {
        if out.entries.is_empty() {
            return;
        }
        (self.send)(FindMsg::Batch {
            id: self.search.id,
            entries: std::mem::take(&mut out.entries),
            names: std::mem::take(&mut out.names),
        });
        out.limit = BATCH;
        out.since = None;
    }

    fn flush_if_old(&self, out: &mut Out) {
        if out.since.is_some_and(|t| t.elapsed() >= FLUSH_EVERY) {
            self.flush(out);
        }
    }

    /// The next item, or `None` when the search is complete or stopped. A worker that has
    /// to wait sends its results first.
    fn next(&self, out: &mut Out) -> Option<Item> {
        loop {
            let mut q = lock(&self.queue);
            if self.search.stopped() {
                return None;
            }
            if let Some(it) = q.items.pop() {
                q.busy += 1;
                return Some(it);
            }
            if q.busy == 0 {
                drop(q);
                self.cv.notify_all();
                return None;
            }
            if !out.entries.is_empty() {
                drop(q);
                self.flush(out);
                continue;
            }
            let _ = self.cv.wait_timeout(q, IDLE_WAIT);
        }
    }

    fn item_done(&self) {
        let mut q = lock(&self.queue);
        q.busy -= 1;
        let idle = q.busy == 0 && q.items.is_empty();
        drop(q);
        if idle {
            self.cv.notify_all();
        }
    }

    /// One worker thread: takes items until the search is complete or stopped, then sends
    /// what it holds. A panic stops the search with an error (NFR-REL).
    fn worker(&self) {
        let r = catch_unwind(AssertUnwindSafe(|| {
            let mut dirbuf: Vec<u8> = Vec::with_capacity(DIRENT_BUF);
            let mut chunk: Vec<u8> = Vec::new();
            let mut out = Out {
                entries: Vec::new(),
                names: Vec::new(),
                limit: FIRST_BATCH,
                since: None,
                scratch: Vec::new(),
                children: Vec::new(),
                files: Vec::new(),
            };
            while let Some(item) = self.next(&mut out) {
                match item {
                    Item::Open(dir) => self.read_dir(&mut dirbuf, &mut out, dir),
                    Item::Dir { parent, name } => {
                        if let Some(dir) = self.open_child(&mut out, parent, &name) {
                            self.read_dir(&mut dirbuf, &mut out, dir);
                        }
                    }
                    Item::File { parent, name } => {
                        self.read_file(&mut chunk, &mut out, &parent, &name);
                    }
                }
                self.share(&mut out);
                self.flush_if_old(&mut out);
                self.item_done();
            }
            self.flush(&mut out);
        }));
        if let Err(p) = r {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            lock(&self.panic).get_or_insert(format!("internal error while searching: {msg}"));
            self.search.stop.store(true, Ordering::SeqCst);
            self.cv.notify_all();
        }
    }

    /// Opens the subdirectory `name` below `parent` (P2 5.3 step 1). `None`: not descended
    /// (gone, an automount trigger, another filesystem, seen before) or an error (counted).
    /// The results the worker holds are sent first: the lookup and the open can wait in the
    /// kernel on a stalled filesystem, and the results must not wait with them (A-FD-7).
    /// The `statx` before the open takes cached attributes, so a mount that is not entered
    /// is never asked (module doc).
    fn open_child(
        &self,
        out: &mut Out,
        parent: Arc<DirHandle>,
        name: &CStr,
    ) -> Option<Arc<DirHandle>> {
        self.flush(out);
        let os = OsStr::from_bytes(name.to_bytes());
        match self
            .sys
            .stat_at_cached("find.descend", parent.fd.as_fd(), os)
        {
            Ok(m) if !descend(&m, self.search.spec.stay_on_fs, self.root_mnt) => return None,
            Ok(_) => {}
            Err(Errno::NOENT) => return None,
            Err(_) => {
                self.error();
                return None;
            }
        }
        let fd = match self.sys.open_dir("find.open", parent.fd.as_fd(), os) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return None,
            // ELOOP (a symlink now: not followed), EACCES, EMFILE, ...
            Err(_) => {
                self.error();
                return None;
            }
        };
        let mut rel = Vec::with_capacity(parent.rel.len() + 1 + name.to_bytes().len());
        join_rel(&mut rel, &parent.rel, name.to_bytes());
        // The item's reference to the parent goes at once (P2 5.3).
        drop(parent);
        let meta = match self.sys.stat_fd(fd.as_fd()) {
            Ok(m) => m,
            Err(_) => {
                self.error();
                return None;
            }
        };
        if self.search.spec.stay_on_fs && meta.id.mnt_id != self.root_mnt {
            return None;
        }
        if !self.visit(meta.id.inode()) {
            return None;
        }
        Some(Arc::new(DirHandle { fd, rel }))
    }

    /// Reads one directory (P2 5.3 steps 2-4).
    fn read_dir(&self, dirbuf: &mut Vec<u8>, out: &mut Out, dir: Arc<DirHandle>) {
        let spec = &self.search.spec;
        self.search.dirs.fetch_add(1, Ordering::Relaxed);
        let mut files = 0u64;
        let mut seen = 0usize;
        let mut raw = RawDir::new(dir.fd.as_fd(), dirbuf.spare_capacity_mut());
        loop {
            if self.search.stopped() {
                break;
            }
            let e = match raw.next() {
                None => break,
                Some(Ok(e)) => e,
                Some(Err(Errno::INTR)) => continue,
                Some(Err(_)) => {
                    self.error();
                    break;
                }
            };
            let name = e.file_name();
            let nb = name.to_bytes();
            if nb.is_empty() || nb == b"." || nb == b".." {
                continue;
            }
            if !spec.hidden && nb[0] == b'.' {
                continue;
            }
            let kind = match e.file_type() {
                FileType::Directory => Kind::Dir,
                FileType::RegularFile => Kind::File,
                // The type of an inode never changes, so the cached one is exact; the entry
                // can be a mount point that is not entered (module doc).
                FileType::Unknown => {
                    match self.sys.stat_at_cached(
                        "find.kind",
                        dir.fd.as_fd(),
                        OsStr::from_bytes(nb),
                    ) {
                        Ok(m) => m.kind,
                        Err(Errno::NOENT) => continue,
                        Err(_) => {
                            self.error();
                            continue;
                        }
                    }
                }
                // Symlinks and special files: never descended, never read.
                _ => Kind::Unknown,
            };
            if kind == Kind::Dir {
                out.children.push(Item::Dir {
                    parent: dir.clone(),
                    name: name.to_owned(),
                });
            } else {
                files += 1;
            }
            if self.names.matches(nb) {
                if self.content.is_some() {
                    if kind == Kind::File {
                        out.files.push(Item::File {
                            parent: dir.clone(),
                            name: name.to_owned(),
                        });
                    }
                } else {
                    self.stat_result(out, &dir, nb);
                }
            }
            seen += 1;
            if seen.is_multiple_of(SHARE_EVERY) {
                self.share(out);
                if seen.is_multiple_of(1024) {
                    self.flush_if_old(out);
                }
            }
        }
        self.search.files.fetch_add(files, Ordering::Relaxed);
    }

    /// A name match without content: `statx` for the columns (P2 5.3 step 3). Cached
    /// attributes, like every `statx` of an entry the walk has not entered: the result can
    /// be the mount point of a stalled filesystem, which a fresh `statx` would wait on. The
    /// columns are for display, and `Ctrl+R` refreshes them (module doc).
    fn stat_result(&self, out: &mut Out, dir: &DirHandle, name: &[u8]) {
        match self
            .sys
            .stat_at_cached("find.result", dir.fd.as_fd(), OsStr::from_bytes(name))
        {
            Ok(meta) => self.add(out, &dir.rel, name, &meta),
            Err(Errno::NOENT) => {}
            Err(_) => self.error(),
        }
    }

    /// A regular file whose name matched: open it through the M1 4.3 sequence (I-10) and
    /// search its content.
    fn read_file(&self, chunk: &mut Vec<u8>, out: &mut Out, parent: &DirHandle, name: &CStr) {
        let Some(content) = &self.content else { return };
        let os = OsStr::from_bytes(name.to_bytes());
        let (fd, meta) = match open_for_read(&self.sys, parent.fd.as_fd(), os, None) {
            Ok(x) => x,
            // Gone, or no longer a regular file: never opened for reading.
            Err(EntryError::Disappeared | EntryError::TypeChanged) => return,
            Err(_) => {
                self.error();
                return;
            }
        };
        if meta.size > CHUNK as u64 {
            // A long read is coming: the results held so far go first.
            self.flush(out);
        }
        match content.scan(&self.sys, fd.as_fd(), &meta, chunk, &self.search.stop) {
            Ok(true) => self.add(out, &parent.rel, name.to_bytes(), &meta),
            Ok(false) | Err(ScanError::Stopped) => {}
            Err(ScanError::Io(_)) => self.error(),
        }
    }

    /// Appends a result to the worker's batch, within [`MAX_RESULTS`].
    fn add(&self, out: &mut Out, rel: &[u8], name: &[u8], meta: &Meta) {
        join_rel(&mut out.scratch, rel, name);
        if out.scratch.len() > u16::MAX as usize {
            // Longer than a listing entry can hold.
            self.error();
            return;
        }
        if self.search.results.fetch_add(1, Ordering::SeqCst) >= MAX_RESULTS {
            self.search.results.fetch_sub(1, Ordering::SeqCst);
            self.search.truncated.store(true, Ordering::SeqCst);
            self.search.stop.store(true, Ordering::SeqCst);
            return;
        }
        out.entries
            .push(Entry::new(&mut out.names, &out.scratch, meta));
        out.since.get_or_insert_with(Instant::now);
        if out.entries.len() >= out.limit {
            self.flush(out);
        }
    }
}

/// Whether the walk opens a subdirectory, from its `statx` with `AT_NO_AUTOMOUNT` and
/// `AT_STATX_DONT_SYNC` before the open (P2 5.3, E-29; the fields used here are exact from
/// cached attributes): only a directory (a symlink or a file now, replaced since it was
/// listed, is not followed), never an automount trigger (the open would mount it), and with
/// `stay_on_fs` only one on the root's mount.
fn descend(pre: &Meta, stay_on_fs: bool, root_mnt: u64) -> bool {
    pre.kind == Kind::Dir && !pre.automount && (!stay_on_fs || pre.id.mnt_id == root_mnt)
}

/// The number of worker threads: `min(8, available_parallelism)`.
pub fn workers() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(MAX_WORKERS)
}

/// Runs `search` on the calling thread, which opens the root and waits for the pool, and
/// sends its messages; `Done` comes last. The totals are recorded in the search too.
pub fn run(search: &Search, send: &(dyn Fn(FindMsg) + Sync)) {
    let started = Instant::now();
    let sys = Sys::default();
    let spec = &search.spec;
    let done = |error: Option<String>| {
        let stats = search.snapshot(started.elapsed(), error);
        search.finish(stats.clone());
        send(FindMsg::Done {
            id: search.id,
            stats,
        });
    };
    let failed = |e: Errno| Some(format!("{}: {}", spec.root.display(), errno_text(e)));
    let root = match sys.open_root(&spec.root) {
        Ok(fd) => fd,
        Err(e) => return done(failed(e)),
    };
    let meta = match sys.stat_fd(root.as_fd()) {
        Ok(m) => m,
        Err(e) => return done(failed(e)),
    };
    let engine = Engine::new(search, send, meta.id.mnt_id);
    engine.visit(meta.id.inode());
    lock(&engine.queue)
        .items
        .push(Item::Open(Arc::new(DirHandle {
            fd: root,
            rel: Vec::new(),
        })));
    std::thread::scope(|s| {
        let mut spawned = 0;
        for i in 0..workers() {
            let e = &engine;
            if std::thread::Builder::new()
                .name(format!("find-{i}"))
                .spawn_scoped(s, move || e.worker())
                .is_ok()
            {
                spawned += 1;
            }
        }
        if spawned == 0 {
            engine.worker();
        }
    });
    let error = lock(&engine.panic).take();
    done(error);
}

/// Runs [`run`] under `catch_unwind` (a panic sends `Done` with an error), then marks the
/// search's threads as returned.
pub fn guarded(search: &Search, send: &(dyn Fn(FindMsg) + Sync)) {
    if catch_unwind(AssertUnwindSafe(|| run(search, send))).is_err() {
        let stats = Stats {
            error: Some("internal error while searching".into()),
            ..Stats::default()
        };
        search.finish(stats.clone());
        send(FindMsg::Done {
            id: search.id,
            stats,
        });
    }
    search.alive.finish();
}

/// Starts a search on a thread named `find` that runs the pool.
pub fn spawn(
    search: Arc<Search>,
    send: impl Fn(FindMsg) + Send + Sync + 'static,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("find".into())
        .spawn(move || guarded(&search, &send))?;
    Ok(())
}

// ---- re-stat ----------------------------------------------------------------------------

/// A results tab's re-stat (P2 5.5): its results as they stand, for a listing thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestatRequest {
    pub slot: usize,
    pub generation: u64,
    pub root: PathBuf,
    pub entries: Vec<Entry>,
    pub names: Vec<u8>,
    /// The tab's sort order: the fresh results are sorted for it here (P-1).
    pub sort: SortSpec,
}

/// The directory part of a relative name (`a/b` of `a/b/c`, empty for `c`) and the leaf.
fn split_rel(name: &[u8]) -> (&[u8], &[u8]) {
    match name.iter().rposition(|&c| c == b'/') {
        Some(i) => (&name[..i], &name[i + 1..]),
        None => (&name[..0], name),
    }
}

/// Re-stats a results tab (P2 5.5), sending the fresh entries as a refresh listing: one
/// `Listing`, sorted for the tab's order, then `Done`. Each result's directory is opened
/// once, by the component walk
/// from the root (`O_NOFOLLOW` per component), and the leaf is `statx`ed with
/// `AT_SYMLINK_NOFOLLOW`. A result that no longer exists, or whose walk meets a symlink or
/// a non-directory, is dropped; one that cannot be checked for another reason keeps its
/// metadata. A root that no longer exists drops every result.
pub fn restat(req: &RestatRequest, send: &dyn Fn(ListingMsg)) {
    let start = Instant::now();
    let sys = Sys::default();
    let (slot, generation) = (req.slot, req.generation);
    let done = || {
        send(ListingMsg::Done {
            slot,
            generation,
            dir: req.root.clone(),
            elapsed: start.elapsed(),
        })
    };
    let root = match sys.open_root(&req.root) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return done(),
        Err(e) => {
            send(ListingMsg::Failed {
                slot,
                generation,
                dir: req.root.clone(),
                error: errno_text(e),
                gone: false,
            });
            return;
        }
    };
    let name = |i: usize| req.entries[i].name(&req.names);
    // Grouped by directory, so each is walked once and the walk reuses the common prefix.
    let mut order: Vec<usize> = (0..req.entries.len()).collect();
    order.sort_by(|&a, &b| split_rel(name(a)).0.cmp(split_rel(name(b)).0));
    let mut entries = Vec::with_capacity(order.len());
    let mut names = Vec::with_capacity(req.names.len());
    // The opened components below the root: `(name, fd)`.
    let mut path: Vec<(&[u8], OwnedFd)> = Vec::new();
    let mut k = 0;
    while k < order.len() {
        let dir = split_rel(name(order[k])).0;
        let end = k + order[k..]
            .iter()
            .take_while(|&&i| split_rel(name(i)).0 == dir)
            .count();
        let comps: Vec<&[u8]> = if dir.is_empty() {
            Vec::new()
        } else {
            dir.split(|&c| c == b'/').collect()
        };
        let common = path
            .iter()
            .zip(&comps)
            .take_while(|(p, c)| p.0 == **c)
            .count();
        path.truncate(common);
        // `Some(drop)`: the walk failed; `drop` when the directory is gone or not one.
        let mut failed = None;
        for &c in &comps[common..] {
            let parent = path.last().map_or(root.as_fd(), |(_, fd)| fd.as_fd());
            match open_dir_nofollow(&sys, "restat.walk", parent, OsStr::from_bytes(c)) {
                Ok((fd, _)) => path.push((c, fd)),
                Err(e) => {
                    failed = Some(matches!(
                        e,
                        EntryError::TypeChanged | EntryError::Disappeared
                    ));
                    break;
                }
            }
        }
        let dirfd = path.last().map_or(root.as_fd(), |(_, fd)| fd.as_fd());
        for &i in &order[k..end] {
            let n = name(i);
            let keep_old = match failed {
                Some(true) => continue,
                Some(false) => true,
                None => match sys.stat_at_noauto(dirfd, OsStr::from_bytes(split_rel(n).1)) {
                    Ok(meta) => {
                        entries.push(Entry::new(&mut names, n, &meta));
                        false
                    }
                    Err(Errno::NOENT | Errno::NOTDIR | Errno::LOOP) => continue,
                    Err(_) => true,
                },
            };
            if keep_old {
                let mut e = req.entries[i];
                e.name_off = names.len() as u32;
                names.extend_from_slice(n);
                entries.push(e);
            }
        }
        k = end;
    }
    send(ListingMsg::Listing {
        slot,
        generation,
        listing: Box::new(Listing::sorted(entries, names, req.sort)),
    });
    done();
}

/// Runs [`restat`] under `catch_unwind`; a panic sends `Failed` and the rows stay.
pub fn restat_guarded(req: &RestatRequest, send: &dyn Fn(ListingMsg)) {
    if catch_unwind(AssertUnwindSafe(|| restat(req, send))).is_err() {
        send(ListingMsg::Failed {
            slot: req.slot,
            generation: req.generation,
            dir: req.root.clone(),
            error: "internal error while re-reading the results".into(),
            gone: false,
        });
    }
}

/// Starts a re-stat on a listing thread named `list-<slot>`; `alive` turns false when it
/// returns.
pub fn spawn_restat(req: RestatRequest, alive: Alive, send: impl Fn(ListingMsg) + Send + 'static) {
    let a = alive.clone();
    let r = std::thread::Builder::new()
        .name(format!("list-{}", req.slot))
        .spawn(move || {
            restat_guarded(&req, &send);
            a.finish();
        });
    if r.is_err() {
        alive.finish();
        // Nobody waits forever: the rows stay and the tab says why.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_by_substring_or_glob_with_the_case_rules() {
        let m = NameMatcher::new(b"TXT", false);
        assert!(m.matches(b"notes.txt") && m.matches(b"a.TxT") && !m.matches(b"notes.md"));
        let m = NameMatcher::new(b"TXT", true);
        assert!(m.matches(b"A.TXT") && !m.matches(b"a.txt"));
        let m = NameMatcher::new(b"*.RS", false);
        assert!(m.matches(b"main.rs") && m.matches(b"MAIN.RS") && !m.matches(b"main.rsx"));
        let m = NameMatcher::new(b"*.RS", true);
        assert!(!m.matches(b"main.rs") && m.matches(b"MAIN.RS"));
        let m = NameMatcher::new(b"ma?n*", false);
        assert!(
            m.matches(b"Main.c") && !m.matches(b"xmain"),
            "a glob is anchored"
        );
        assert!(NameMatcher::new(b"", true).matches(b"anything"));
        assert!(NameMatcher::new(b"\xff", true).matches(b"bad\xffname"));
        // Non-ASCII bytes compare exactly.
        assert!(!NameMatcher::new("\u{c9}".as_bytes(), false).matches("\u{e9}".as_bytes()));
    }

    #[test]
    fn titles_name_the_pattern_and_the_text() {
        let s = |name: &[u8], content: Option<&[u8]>| {
            Search::new(
                1,
                FindSpec {
                    name: name.to_vec(),
                    content: content.map(<[u8]>::to_vec),
                    ..FindSpec::default()
                },
            )
            .title()
        };
        assert_eq!(s(b"*.rs", None), "find: *.rs");
        assert_eq!(s(b"", None), "find: *");
        assert_eq!(s(b"x", Some(b"TODO")), "find: x \"TODO\"");
        assert_eq!(s(b"", Some(b"a\nb")), "find: \"a\\nb\"");
    }

    #[test]
    fn states_follow_cancel_limit_and_finish() {
        let s = Search::new(1, FindSpec::default());
        assert_eq!(s.state(), State::Searching);
        assert!(s.running());
        s.truncated.store(true, Ordering::SeqCst);
        assert_eq!(s.state(), State::Truncated);
        assert!(!s.finished() && !s.stopping());
        s.cancel();
        assert_eq!(s.state(), State::Cancelled);
        assert!(!s.running() && s.stopped());
        assert!(
            s.stopping() && !s.finished(),
            "cancelled, batches can still come"
        );
        s.finish(Stats::default());
        assert!(s.finished() && !s.stopping());
        let t = Search::new(2, FindSpec::default());
        t.finish(Stats::default());
        t.cancel();
        assert!(t.finished() && !t.stopping());
        assert_eq!(
            t.state(),
            State::Done,
            "a finished search is not cancelled later"
        );
        let f = Search::new(3, FindSpec::default());
        f.finish(Stats {
            error: Some("boom".into()),
            ..Stats::default()
        });
        assert_eq!(f.state(), State::Failed("boom".into()));
    }

    #[test]
    fn the_visited_set_admits_each_identity_once() {
        let s = Search::new(1, FindSpec::default());
        let send = |_: FindMsg| {};
        let e = Engine::new(&s, &send, 0);
        assert!(e.visit((1, 2)));
        assert!(e.visit((2, 1)));
        assert!(!e.visit((1, 2)));
        for i in 0..1000 {
            assert!(e.visit((7, i)));
        }
        for i in 0..1000 {
            assert!(!e.visit((7, i)));
        }
    }

    #[test]
    fn the_stack_is_last_in_first_out_and_ends_when_idle() {
        let s = Search::new(1, FindSpec::default());
        let send = |_: FindMsg| {};
        let e = Engine::new(&s, &send, 0);
        let fd = |rel: &[u8]| {
            Arc::new(DirHandle {
                fd: rustix::fs::open(
                    "/",
                    rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::empty(),
                )
                .unwrap(),
                rel: rel.to_vec(),
            })
        };
        let mut out = Out {
            entries: Vec::new(),
            names: Vec::new(),
            limit: FIRST_BATCH,
            since: None,
            scratch: Vec::new(),
            children: vec![Item::Open(fd(b"a")), Item::Open(fd(b"b"))],
            files: vec![Item::Open(fd(b"c"))],
        };
        e.share(&mut out);
        let rel = |it: Option<Item>| match it {
            Some(Item::Open(d)) => d.rel.clone(),
            _ => panic!("an open item"),
        };
        // Files go on top of their directory's subdirectories.
        assert_eq!(rel(e.next(&mut out)), b"c");
        assert_eq!(rel(e.next(&mut out)), b"b");
        e.item_done();
        e.item_done();
        assert_eq!(rel(e.next(&mut out)), b"a");
        e.item_done();
        assert!(
            e.next(&mut out).is_none(),
            "empty and nobody busy: complete"
        );
        s.cancel();
        lock(&e.queue).items.push(Item::Open(fd(b"d")));
        assert!(
            e.next(&mut out).is_none(),
            "a stopped search hands out nothing"
        );
    }

    #[test]
    fn the_result_limit_stops_the_search_and_says_so() {
        let s = Search::new(1, FindSpec::default());
        let send = |_: FindMsg| {};
        let e = Engine::new(&s, &send, 0);
        let mut out = Out {
            entries: Vec::new(),
            names: Vec::new(),
            limit: FIRST_BATCH,
            since: None,
            scratch: Vec::new(),
            children: Vec::new(),
            files: Vec::new(),
        };
        s.results.store(MAX_RESULTS - 1, Ordering::SeqCst);
        e.add(&mut out, b"d", b"last", &Meta::default());
        assert_eq!(out.entries.len(), 1);
        assert!(!s.stopped());
        e.add(&mut out, b"d", b"over", &Meta::default());
        assert_eq!(out.entries.len(), 1, "no result past the limit");
        assert!(s.stopped());
        assert_eq!(s.state(), State::Truncated);
        assert_eq!(s.snapshot(Duration::ZERO, None).results, MAX_RESULTS);
        assert!(s.snapshot(Duration::ZERO, None).truncated);
    }

    #[test]
    fn a_panic_ends_the_search_with_an_error() {
        let s = Search::new(
            4,
            FindSpec {
                root: concat!(env!("CARGO_MANIFEST_DIR"), "/src").into(),
                stay_on_fs: true,
                ..FindSpec::default()
            },
        );
        let done = Mutex::new(None);
        guarded(&s, &|m| match m {
            FindMsg::Batch { .. } => panic!("injected"),
            FindMsg::Done { stats, .. } => *done.lock().unwrap() = Some(stats),
        });
        let stats = done.into_inner().unwrap().expect("Done was sent");
        let e = stats.error.expect("an error");
        assert!(e.contains("internal error while searching"), "{e}");
        assert!(matches!(s.state(), State::Failed(_)));
        assert!(!s.alive.is_running());
    }

    /// Review finding B4: the walk never opens an automount trigger, with or without "Stay
    /// on this filesystem"; a different mount is skipped only with it. An end-to-end
    /// automount needs an automount map (autofs) or a network filesystem, which a test here
    /// cannot set up; this checks the decision on the `statx` result.
    #[test]
    fn the_walk_never_opens_an_automount_trigger() {
        let dir = |mnt_id, automount| Meta {
            kind: Kind::Dir,
            id: crate::fsops::sys::FsIdentity {
                dev: 1,
                ino: 2,
                mnt_id,
            },
            automount,
            ..Meta::default()
        };
        for stay in [true, false] {
            assert!(descend(&dir(7, false), stay, 7), "the root's mount");
            assert!(
                !descend(&dir(7, true), stay, 7),
                "a trigger on the root's mount"
            );
            assert!(
                !descend(&dir(9, true), stay, 7),
                "a trigger on another mount"
            );
            for kind in [Kind::Symlink, Kind::File, Kind::Fifo] {
                let m = Meta {
                    kind,
                    ..dir(7, false)
                };
                assert!(!descend(&m, stay, 7), "{kind:?} is not descended");
            }
        }
        assert!(!descend(&dir(9, false), true, 7), "stay on this filesystem");
        assert!(
            descend(&dir(9, false), false, 7),
            "another mount without it"
        );
    }

    /// A-FD-7, defect 2: a worker sends the results it holds before it looks up and opens
    /// a directory, so they are on screen while the lookup or the open waits in the kernel
    /// (a stalled filesystem). Failpoints on the `statx` before the open and on the open
    /// stand in for the wait: each records what the UI has received by then. One worker
    /// runs on this thread, so the order is fixed. The step names also show that the walk's
    /// `statx` calls are the cached ones (`Sys::stat_at_cached`, whose flags
    /// `tests/find.rs` checks).
    #[cfg(feature = "failpoints")]
    #[test]
    fn a_worker_sends_its_results_before_it_opens_a_directory() {
        use crate::fsops::failpoints::{Action, Failpoints, Trigger};
        let dir = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/target/test-tmp"))
            .join(format!("find-flush-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("hit-1"), b"").unwrap();
        std::fs::write(dir.join("sub/hit-2"), b"").unwrap();
        let s = Search::new(
            1,
            FindSpec {
                root: dir.clone(),
                name: b"hit".to_vec(),
                stay_on_fs: true,
                ..FindSpec::default()
            },
        );
        let sent = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let send = {
            let sent = sent.clone();
            move |m: FindMsg| {
                if let FindMsg::Batch { entries, names, .. } = m {
                    let mut v = sent.lock().unwrap();
                    v.extend(entries.iter().map(|e| e.name(&names).to_vec()));
                }
            }
        };
        let root = Sys::default().open_root(&dir).unwrap();
        let meta = Sys::default().stat_fd(root.as_fd()).unwrap();
        let mut e = Engine::new(&s, &send, meta.id.mnt_id);
        let fp = Failpoints::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        for step in ["find.descend", "find.open"] {
            let (sent, seen) = (sent.clone(), seen.clone());
            let record = move || {
                seen.lock()
                    .unwrap()
                    .push((step, sent.lock().unwrap().clone()))
            };
            fp.arm(step, Trigger::Always, Action::Call(Arc::new(record)));
        }
        e.sys = Sys::with_failpoints(Arc::new(AtomicBool::new(false)), fp.clone());
        e.visit(meta.id.inode());
        lock(&e.queue).items.push(Item::Open(Arc::new(DirHandle {
            fd: root,
            rel: Vec::new(),
        })));
        e.worker();
        let _ = std::fs::remove_dir_all(&dir);
        let first = vec![b"hit-1".to_vec()];
        assert_eq!(
            *seen.lock().unwrap(),
            [("find.descend", first.clone()), ("find.open", first)],
            "the root's result was sent before `sub` was looked up and opened"
        );
        let mut all = sent.lock().unwrap().clone();
        all.sort();
        assert_eq!(all, [b"hit-1".to_vec(), b"sub/hit-2".to_vec()]);
        assert_eq!(fp.hits("find.result"), 2, "each result's cached statx");
        assert_eq!(s.snapshot(Duration::ZERO, None).dirs, 2);
    }

    #[test]
    fn relative_names_split_at_the_last_slash() {
        assert_eq!(split_rel(b"a/b/c"), (&b"a/b"[..], &b"c"[..]));
        assert_eq!(split_rel(b"c"), (&b""[..], &b"c"[..]));
        let mut v = Vec::new();
        join_rel(&mut v, b"", b"x");
        assert_eq!(v, b"x");
        join_rel(&mut v, b"a/b", b"x");
        assert_eq!(v, b"a/b/x");
    }
}
