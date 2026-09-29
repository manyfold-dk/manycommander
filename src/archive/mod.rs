#![forbid(unsafe_code)]
//! Archives browsed as read-only directories (P3 3).
//!
//! An archive opens on a listing thread ([`open`]): the file is opened through the M1 4.3
//! `O_PATH` sequence, so a FIFO or a device is never opened (I-10); its `StatKey` looks the
//! index up in the [`IndexCache`], and a miss detects the format by its magic
//! ([`detect`]) and scans it. The scan streams: the rows of the directory the panel shows
//! arrive in batches as they are found ([`Sink`]), the footer shows how much of the file
//! was read, and `Esc` cancels between blocks of at most 1 MiB of decompressed data
//! (P3 3.3). Entering a subdirectory during the scan moves the scan's watch there
//! ([`ArchiveIndex::watch`]). When the scan ends the index is complete and cached, and every
//! listing is built from memory on a listing thread ([`relist`]).
//!
//! The index keeps the archive's read fd, opened at scan time, so a replaced archive name
//! never redirects a later read. Every read is positioned ([`PosReader`]): `pread` on the
//! one open file description, which never moves a shared offset (P3 3.2). Nothing here
//! calls a crate's `extract` or `unpack`, and no member name is ever joined onto a
//! filesystem path (A-1).
//!
//! Scans run under `catch_unwind` on listing threads (NFR-REL): a Rust panic in a parser
//! fails that load. A memory fault in libzstd ends the process, the risk D-1 accepts.

pub mod detect;
pub mod extract;
pub mod index;
pub mod sevenz;
pub mod tar;
pub mod zip;

use crate::fsops::sys::{Meta, Sys};
use crate::fsops::walk::{EntryError, errno_text, open_for_read};
use crate::panel::Listing;
use crate::panel::entry::{EKind, Entry, LinkKind};
use crate::panel::listing::{BATCH, FIRST_BATCH, ListingMsg};
use crate::panel::sort::SortSpec;
use crate::provider::{Caps, PlaceError, Provider, StatKey, VPath, synthetic_id};
use detect::{Format, NOT_SUPPORTED, Want, detect, is_tar_header};
use index::{Added, ENCRYPTED, Limits, NodeId, NodeKind, Tree};
use rustix::fd::AsFd;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// A truncated stream, a CRC error or a decoder error (A-5).
pub const DAMAGED: &str = "archive damaged";
/// The header at a member's locator is not the one the index stored (A-5).
pub const CHANGED_MEMBER: &str = "archive changed";
/// A zstd frame or an xz block whose window exceeds the cap (A-4).
pub const NEEDS_MEMORY: &str = "archive needs too much memory to decode";
/// A member of an archive is not opened as an archive (P3 3.1).
pub const NESTED: &str = "nested archives are not supported; extract it first";
/// Something that needs the complete index while the scan still runs.
pub const STILL_READING: &str = "the archive is still being read";
/// A refresh saw another `StatKey` for the archive's name (P3 3.2).
pub const CHANGED: &str = "the archive changed on disk; Ctrl+R re-reads";
/// `Ctrl+R` saw another key and reads the archive again (P3 2.4).
pub const REREADING: &str = "the archive changed on disk; reading it again";
/// A directory of a history place that the archive no longer holds.
pub const NOT_IN_ARCHIVE: &str = "not in the archive";

/// The zstd window cap (A-4): 2^27 bytes, a 128 MiB window, libzstd's own default limit.
pub const ZSTD_WINDOW_LOG_MAX: u32 = 27;
/// The xz memory cap (A-4): an LZMA2 dictionary above it is refused before it is allocated.
pub const XZ_MEMORY_MAX: u64 = 128 << 20;

/// At most this many indexes stay cached (P3 2.6).
pub const MAX_CACHED: usize = 4;
/// The cached indexes hold at most this many bytes together (P3 2.6).
pub const MAX_CACHE_BYTES: usize = 128 << 20;

/// The progress text of a scan in the footer (P3 3.3): "reading archive: 48 of 98 MB".
pub fn progress_text(read: u64, total: u64) -> String {
    let mb = |b: u64| b.div_ceil(1 << 20);
    format!(
        "reading archive: {} of {} MB",
        mb(read).min(mb(total)),
        mb(total)
    )
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A member at least this large takes a while to inflate: the rows found so far go out
/// before it ([`Sink::before_skip`]).
const SKIP_FLUSH: u64 = 256 << 10;

/// The panel slot, load generation and inner directory a running scan serves (P3 3.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Watch {
    pub slot: usize,
    pub generation: u64,
    pub inner: VPath,
}

/// How a scan ended.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// "archive damaged", the memory cap, or the index bound that stopped the listing.
    pub error: Option<String>,
    pub elapsed: Duration,
}

/// One archive's index (P3 3.2): its tree once the scan completes, the held read fd, its
/// cache key, and the state of the scan that fills it.
pub struct ArchiveIndex {
    /// The archive file as it was opened.
    pub archive: PathBuf,
    pub key: StatKey,
    pub format: Format,
    id: u64,
    file: Arc<File>,
    cancel: Arc<AtomicBool>,
    read: Arc<AtomicU64>,
    tree: OnceLock<Tree>,
    outcome: OnceLock<Outcome>,
    watch: Mutex<Option<Watch>>,
    watch_version: AtomicU64,
    /// A 7z with a block of several members (P3 3.5): extraction runs in one pass.
    solid: AtomicBool,
}

impl std::fmt::Debug for ArchiveIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveIndex")
            .field("archive", &self.archive)
            .field("format", &self.format)
            .field("complete", &self.is_complete())
            .finish_non_exhaustive()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl ArchiveIndex {
    fn new(
        archive: PathBuf,
        key: StatKey,
        format: Format,
        file: Arc<File>,
        cancel: Arc<AtomicBool>,
        read: Arc<AtomicU64>,
    ) -> ArchiveIndex {
        ArchiveIndex {
            archive,
            key,
            format,
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            file,
            cancel,
            read,
            tree: OnceLock::new(),
            outcome: OnceLock::new(),
            watch: Mutex::new(None),
            watch_version: AtomicU64::new(0),
            solid: AtomicBool::new(false),
        }
    }

    /// The complete tree; `None` while the scan runs.
    pub fn tree(&self) -> Option<&Tree> {
        self.tree.get()
    }

    pub fn is_complete(&self) -> bool {
        self.tree.get().is_some()
    }

    /// How the scan ended, once it has.
    pub fn outcome(&self) -> Option<&Outcome> {
        self.outcome.get()
    }

    /// Bytes of the archive file read so far, and its size (P3 3.3).
    pub fn progress(&self) -> (u64, u64) {
        (self.read.load(Ordering::Relaxed), self.key.size)
    }

    /// Asks the scan to stop; it checks between blocks of decompressed data (P-20).
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// The archive's held read fd: every read of it is positioned ([`PosReader`]).
    pub fn file(&self) -> &Arc<File> {
        &self.file
    }

    /// The place id of the synthetic identities (P3 2.1).
    pub fn id(&self) -> u64 {
        self.id
    }

    /// A 7z whose blocks hold several members each: extracting from it decodes each block
    /// once per job, in one pass (P3 3.5). Set by the scan.
    pub fn solid(&self) -> bool {
        self.solid.load(Ordering::Relaxed)
    }

    /// The directory that holds the archive: the panel's local directory (P3 2.2).
    pub fn dir(&self) -> PathBuf {
        self.archive
            .parent()
            .map_or_else(|| PathBuf::from("/"), Path::to_path_buf)
    }

    /// While the scan runs, shows `w.inner` to panel `w.slot`: the scan sends what it
    /// knows of that directory and appends the rest, then `Done` (P3 3.3). Returns
    /// `false` once the index is complete: the caller lists it on a listing thread.
    pub fn watch(&self, w: Watch) -> bool {
        let mut slot = lock(&self.watch);
        if self.tree.get().is_some() {
            return false;
        }
        *slot = Some(w);
        self.watch_version.fetch_add(1, Ordering::SeqCst);
        true
    }

    /// Publishes the finished tree; returns the watch the scan must still answer.
    fn complete(&self, tree: Tree, outcome: Outcome) -> Option<Watch> {
        let mut slot = lock(&self.watch);
        let _ = self.outcome.set(outcome);
        let _ = self.tree.set(tree);
        slot.take()
    }

    /// The approximate memory of the index (P3 2.6).
    pub fn bytes(&self) -> usize {
        self.tree.get().map_or(0, Tree::bytes)
    }

    /// An empty, complete index of no file, for the panel's unit tests.
    #[cfg(test)]
    pub(crate) fn detached(archive: PathBuf, key: StatKey) -> ArchiveIndex {
        let file = File::open("/dev/null").expect("/dev/null");
        let ix = ArchiveIndex::new(
            archive,
            key,
            Format::Zip,
            Arc::new(file),
            Arc::new(AtomicBool::new(false)),
            Arc::default(),
        );
        let mut tree = Tree::default();
        tree.finish();
        ix.complete(tree, Outcome::default());
        ix
    }

    fn node_meta(&self, tree: &Tree, id: NodeId) -> Meta {
        let mut m = tree.meta(id);
        m.id = synthetic_id(self.id, id as u64);
        m
    }
}

impl Provider for ArchiveIndex {
    fn caps(&self) -> Caps {
        Caps {
            random_access: self.format.random_access(),
            ..Caps::default()
        }
    }

    fn list(
        &self,
        dir: &VPath,
        out: &mut dyn FnMut(ListingMsg),
        cancel: &AtomicBool,
    ) -> Result<(), PlaceError> {
        let tree = self
            .tree()
            .ok_or_else(|| PlaceError::Refused(STILL_READING.into()))?;
        let d = tree.lookup(dir).ok_or(PlaceError::NotFound)?;
        if tree.node(d).kind != NodeKind::Dir {
            return Err(PlaceError::NotADirectory);
        }
        let mut entries = Vec::new();
        let mut names = Vec::new();
        for k in tree.children(d) {
            if cancel.load(Ordering::Relaxed) {
                return Err(PlaceError::Cancelled);
            }
            entries.push(tree.entry(k, &mut names));
            if entries.len() >= BATCH {
                out(ListingMsg::Batch {
                    slot: 0,
                    generation: 0,
                    entries: std::mem::take(&mut entries),
                    names: std::mem::take(&mut names),
                });
            }
        }
        if !entries.is_empty() {
            out(ListingMsg::Batch {
                slot: 0,
                generation: 0,
                entries,
                names,
            });
        }
        Ok(())
    }

    fn lstat(&self, path: &VPath) -> Result<Meta, PlaceError> {
        let tree = self
            .tree()
            .ok_or_else(|| PlaceError::Refused(STILL_READING.into()))?;
        let id = tree.lookup(path).ok_or(PlaceError::NotFound)?;
        Ok(self.node_meta(tree, id))
    }

    /// A member's bytes for F3, F4 and the quick view (P3 3.4): a regular file, or the
    /// member a hard link names; never a symlink, a directory or a special member. An
    /// encrypted member is refused (A-AR-7). The reader decodes on a thread of its own and
    /// checks the header at the member's locator (A-5); its caller stops at the declared
    /// size (A-4).
    fn open_read(
        &self,
        path: &VPath,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Box<dyn Read + Send>, PlaceError> {
        let tree = self
            .tree()
            .ok_or_else(|| PlaceError::Refused(STILL_READING.into()))?;
        let mut id = tree.lookup(path).ok_or(PlaceError::NotFound)?;
        if tree.node(id).kind == NodeKind::HardLink {
            id = tree.hard_target(id).ok_or_else(|| {
                PlaceError::Refused(crate::fsops::copy::LINK_NOT_EXTRACTED.into())
            })?;
        }
        let n = tree.node(id);
        if n.kind != NodeKind::File {
            return Err(PlaceError::NotAFile);
        }
        if n.flags & ENCRYPTED != 0 {
            return Err(PlaceError::Refused(extract::ENCRYPTED_MEMBER.into()));
        }
        if cancel.load(Ordering::SeqCst) {
            return Err(PlaceError::Cancelled);
        }
        Ok(extract::member_reader(self, tree, id))
    }
}

/// Positioned reads on an archive's held fd (P3 3.2): `pread` at a position of its own,
/// never the file description's offset, so the preview thread, a view preparation and a
/// job can read the same archive at once. Small reads go through a window, so a parser
/// that reads a few bytes at a time costs one `pread` per window.
pub struct PosReader {
    file: Arc<File>,
    pos: u64,
    len: u64,
    buf: Vec<u8>,
    buf_at: u64,
    window: usize,
    read: Option<Arc<AtomicU64>>,
}

impl PosReader {
    /// A reader of the first `len` bytes of `file`.
    pub fn new(file: Arc<File>, len: u64) -> PosReader {
        PosReader {
            file,
            pos: 0,
            len,
            buf: Vec::new(),
            buf_at: 0,
            window: 8192,
            read: None,
        }
    }

    /// Where the next read starts.
    pub(crate) fn position(&self) -> u64 {
        self.pos
    }

    /// The size of the read window for small reads.
    pub fn window(mut self, n: usize) -> PosReader {
        self.window = n.max(512);
        self
    }

    /// Records the furthest position read, for the scan's progress (P3 3.3) and for what an
    /// extraction read (A-AR-5).
    pub(crate) fn progress(mut self, read: Arc<AtomicU64>) -> PosReader {
        self.read = Some(read);
        self
    }

    fn pread(&self, buf: &mut [u8], at: u64) -> io::Result<usize> {
        loop {
            match self.file.read_at(buf, at) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                r => return r,
            }
        }
    }
}

impl Read for PosReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || out.is_empty() {
            return Ok(0);
        }
        let want = out
            .len()
            .min((self.len - self.pos).min(usize::MAX as u64) as usize);
        let cached = self.pos >= self.buf_at && self.pos < self.buf_at + self.buf.len() as u64;
        let n = if cached {
            let off = (self.pos - self.buf_at) as usize;
            let n = want.min(self.buf.len() - off);
            out[..n].copy_from_slice(&self.buf[off..off + n]);
            n
        } else if want >= self.window {
            self.pread(&mut out[..want], self.pos)?
        } else {
            let room = self.window.min((self.len - self.pos) as usize);
            self.buf.resize(room, 0);
            let mut buf = std::mem::take(&mut self.buf);
            let got = self.pread(&mut buf[..room], self.pos);
            self.buf = buf;
            let got = got?;
            self.buf.truncate(got);
            self.buf_at = self.pos;
            let n = want.min(got);
            out[..n].copy_from_slice(&self.buf[..n]);
            n
        };
        self.pos += n as u64;
        if let Some(r) = &self.read {
            r.fetch_max(self.pos, Ordering::Relaxed);
        }
        Ok(n)
    }
}

impl Seek for PosReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let base = match to {
            SeekFrom::Start(n) => {
                self.pos = n;
                return Ok(n);
            }
            SeekFrom::End(d) => (self.len, d),
            SeekFrom::Current(d) => (self.pos, d),
        };
        match base.0.checked_add_signed(base.1) {
            Some(p) => {
                self.pos = p;
                Ok(p)
            }
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before the start of the archive",
            )),
        }
    }
}

/// Reads until `buf` is full or the reader ends; returns the bytes read.
fn read_full(r: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(got)
}

/// The index cache (P3 2.6, 3.2): at most [`MAX_CACHED`] indexes and [`MAX_CACHE_BYTES`]
/// together, keyed by `StatKey`, least recently used first out. An index that someone else
/// holds (a panel on screen shows it; a hidden tab releases its reference) is never evicted.
pub struct IndexCache {
    entries: Mutex<Vec<(Arc<ArchiveIndex>, u64)>>,
    tick: AtomicU64,
    scans: AtomicU64,
    max_count: usize,
    max_bytes: usize,
}

impl Default for IndexCache {
    fn default() -> Self {
        IndexCache::new(MAX_CACHED, MAX_CACHE_BYTES)
    }
}

impl IndexCache {
    pub fn new(max_count: usize, max_bytes: usize) -> IndexCache {
        IndexCache {
            entries: Mutex::new(Vec::new()),
            tick: AtomicU64::new(0),
            scans: AtomicU64::new(0),
            max_count,
            max_bytes,
        }
    }

    /// The scans this cache's opens started (A-AR-4: re-entering hits the cache).
    pub fn scans(&self) -> u64 {
        self.scans.load(Ordering::SeqCst)
    }

    /// The cached index of an unchanged archive.
    pub fn get(&self, key: &StatKey) -> Option<Arc<ArchiveIndex>> {
        let mut e = lock(&self.entries);
        let t = self.tick.fetch_add(1, Ordering::Relaxed);
        e.iter_mut()
            .find(|(ix, _)| ix.key == *key)
            .map(|(ix, used)| {
                *used = t;
                ix.clone()
            })
    }

    /// Caches a complete index, then evicts down to the bounds.
    pub fn insert(&self, ix: Arc<ArchiveIndex>) {
        let mut e = lock(&self.entries);
        let t = self.tick.fetch_add(1, Ordering::Relaxed);
        e.retain(|(old, _)| old.key != ix.key);
        e.push((ix, t));
        loop {
            let count = e.len();
            let bytes: usize = e.iter().map(|(ix, _)| ix.bytes()).sum();
            if count <= self.max_count && bytes <= self.max_bytes {
                break;
            }
            // Only this cache holds it: no tab shows it.
            let victim = e
                .iter()
                .enumerate()
                .filter(|(_, (ix, _))| Arc::strong_count(ix) == 1)
                .min_by_key(|(_, (_, used))| *used)
                .map(|(i, _)| i);
            match victim {
                Some(i) => {
                    e.remove(i);
                }
                None => break,
            }
        }
    }

    /// The cached indexes, most recently used last.
    pub fn cached(&self) -> Vec<Arc<ArchiveIndex>> {
        let mut e: Vec<_> = lock(&self.entries).clone();
        e.sort_by_key(|(_, used)| *used);
        e.into_iter().map(|(ix, _)| ix).collect()
    }

    /// The bytes the cached indexes hold.
    pub fn bytes(&self) -> usize {
        lock(&self.entries).iter().map(|(ix, _)| ix.bytes()).sum()
    }
}

/// Opening an archive (P3 3.1): through the cache, or a scan.
#[derive(Clone)]
pub struct OpenRequest {
    pub slot: usize,
    pub generation: u64,
    pub archive: PathBuf,
    pub want: Want,
    /// The directory the panel shows first, below the archive root.
    pub inner: VPath,
    /// The scan's cancel flag, which the panel's load holds (P-20).
    pub cancel: Arc<AtomicBool>,
    /// Zip's DOS times are local times (P3 3.2).
    pub tz: jiff::tz::TimeZone,
    pub limits: Limits,
}

impl std::fmt::Debug for OpenRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenRequest")
            .field("slot", &self.slot)
            .field("generation", &self.generation)
            .field("archive", &self.archive)
            .field("want", &self.want)
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl PartialEq for OpenRequest {
    fn eq(&self, o: &OpenRequest) -> bool {
        self.slot == o.slot
            && self.generation == o.generation
            && self.archive == o.archive
            && self.want == o.want
            && self.inner == o.inner
            && Arc::ptr_eq(&self.cancel, &o.cancel)
    }
}

impl Eq for OpenRequest {}

/// What a re-listing of a complete index checks first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// Navigation inside the index: no check.
    No,
    /// A refresh: another `StatKey` for the archive's name says [`CHANGED`] and keeps the
    /// view on the indexed inode.
    Refresh,
    /// `Ctrl+R`: another key asks the panel to read the archive again (P3 2.4).
    Rescan,
}

/// Listing a directory of a complete index, from memory (P3 3.3).
#[derive(Clone)]
pub struct RelistRequest {
    pub slot: usize,
    pub generation: u64,
    /// The panel's local directory: the one that holds the archive.
    pub dir: PathBuf,
    pub index: Arc<ArchiveIndex>,
    pub inner: VPath,
    /// A refresh: the whole listing at once, sorted for this order (P-1).
    pub sort: Option<SortSpec>,
    pub check: Check,
}

impl std::fmt::Debug for RelistRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelistRequest")
            .field("slot", &self.slot)
            .field("generation", &self.generation)
            .field("archive", &self.index.archive)
            .field("inner", &self.inner)
            .field("check", &self.check)
            .finish_non_exhaustive()
    }
}

impl PartialEq for RelistRequest {
    fn eq(&self, o: &RelistRequest) -> bool {
        self.slot == o.slot
            && self.generation == o.generation
            && self.dir == o.dir
            && Arc::ptr_eq(&self.index, &o.index)
            && self.inner == o.inner
            && self.sort == o.sort
            && self.check == o.check
    }
}

impl Eq for RelistRequest {}

/// How a scan stopped early.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    /// The panel moved on; nothing more is sent.
    Cancelled,
    /// [`DAMAGED`]: the members before the damage stay listed.
    Damaged,
    /// [`NEEDS_MEMORY`].
    Memory,
    /// An index bound (P3 2.6), with its message.
    Full(String),
    /// Nothing can be listed: the load fails with this message.
    Fatal(String),
}

/// Opens the file through the M1 4.3 `O_PATH` sequence (I-10): a FIFO, a device or a
/// symlink is never opened for reading.
fn open_file(archive: &Path) -> Result<(File, Meta), String> {
    let sys = Sys::default();
    let parent = archive.parent().unwrap_or(Path::new("/"));
    let name = archive.file_name().ok_or("not a regular file")?;
    let dir = sys.open_root(parent).map_err(errno_text)?;
    match open_for_read(&sys, dir.as_fd(), name, None) {
        Ok((fd, meta)) => Ok((File::from(fd), meta)),
        Err(EntryError::TypeChanged) => Err("not a regular file".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// The `StatKey` of the archive's name now, without opening it.
fn stat_key(archive: &Path) -> Option<StatKey> {
    let sys = Sys::default();
    let parent = archive.parent()?;
    let dir = sys.open_root(parent).ok()?;
    let m = sys
        .stat_at("archive.stat", dir.as_fd(), archive.file_name()?)
        .ok()?;
    Some(StatKey::of(&m))
}

/// The format of the opened file for `want` (P3 3.1). For a compressed tar the first 512
/// decompressed bytes must form a tar header, unless the decoder already refuses the
/// stream for its window (the scan then says so). A compressed tar's decoder, which counts
/// what it reads of the file in `read`, goes to the scan with those 512 bytes, so the
/// stream's first block is decoded once (P-19).
fn sniff(
    file: &Arc<File>,
    len: u64,
    want: Want,
    read: &Arc<AtomicU64>,
) -> Result<(Format, Option<tar::Started>), String> {
    let mut head = vec![0u8; 512];
    let n =
        read_full(&mut PosReader::new(file.clone(), len), &mut head).map_err(|e| e.to_string())?;
    head.truncate(n);
    let f = detect(&head, want)?;
    if f.compressed() {
        let promised = || match want {
            Want::Name(f) => f.mismatch(),
            Want::Magic => NOT_SUPPORTED.to_string(),
        };
        let base = PosReader::new(file.clone(), len).progress(read.clone());
        let (mut r, memory) = tar::decoder(f, base).map_err(|_| promised())?;
        let mut block = [0u8; 512];
        match read_full(&mut r, &mut block) {
            Ok(512) if is_tar_header(&block) => {
                let started = tar::Started {
                    head: block,
                    decoder: (r, memory),
                };
                return Ok((f, Some(started)));
            }
            // The scan decodes again and reports the window cap.
            Err(e) if tar::is_memory_error(&e, &memory) => {}
            _ => return Err(promised()),
        }
    }
    Ok((f, None))
}

/// Opens an archive on a listing thread (P3 3.1, 3.3): a cached index of the same
/// `StatKey` lists at once; otherwise the format is detected and the archive scanned. Sends
/// `Opened` with the index, then the watched directory's rows, `Done` and the symlink
/// kinds, or `Failed`. A cancelled scan sends nothing more and is not cached.
pub fn open(req: &OpenRequest, cache: &IndexCache, send: &dyn Fn(ListingMsg)) {
    let start = Instant::now();
    let (slot, generation) = (req.slot, req.generation);
    let dir = req
        .archive
        .parent()
        .map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
    let failed = |error: String| {
        send(ListingMsg::Failed {
            slot,
            generation,
            dir: dir.clone(),
            error,
            gone: false,
        })
    };
    let (file, meta) = match open_file(&req.archive) {
        Ok(x) => x,
        Err(e) => return failed(e),
    };
    let key = StatKey::of(&meta);
    if let Some(ix) = cache.get(&key) {
        send(ListingMsg::Opened {
            slot,
            generation,
            index: ix.clone(),
        });
        let at = Place {
            slot,
            generation,
            dir: &dir,
        };
        list_complete(&ix, &req.inner, at, None, send, start);
        return;
    }
    let file = Arc::new(file);
    let read = Arc::new(AtomicU64::new(0));
    let (format, started) = match sniff(&file, meta.size, req.want, &read) {
        Ok(f) => f,
        Err(e) => return failed(e),
    };
    if req.cancel.load(Ordering::SeqCst) {
        return;
    }
    let ix = Arc::new(ArchiveIndex::new(
        req.archive.clone(),
        key,
        format,
        file,
        req.cancel.clone(),
        read,
    ));
    cache.scans.fetch_add(1, Ordering::SeqCst);
    ix.watch(Watch {
        slot,
        generation,
        inner: req.inner.clone(),
    });
    send(ListingMsg::Opened {
        slot,
        generation,
        index: ix.clone(),
    });
    let mut tree = Tree::new(req.limits);
    let mut sink = Sink::new(&ix, send);
    let result = match format {
        Format::Zip => zip::scan(&ix, &mut tree, &mut sink, &req.tz),
        Format::SevenZ => sevenz::scan(&ix, &mut tree, &mut sink),
        f => tar::scan(&ix, f, started, &mut tree, &mut sink),
    };
    let error = match result {
        Ok(()) => None,
        Err(Stop::Cancelled) => return,
        Err(Stop::Fatal(e)) => return failed(e),
        Err(Stop::Damaged) => Some(DAMAGED.to_string()),
        Err(Stop::Memory) => Some(NEEDS_MEMORY.to_string()),
        Err(Stop::Full(m)) => Some(m),
    };
    tree.finish();
    tracing::info!(
        archive = %req.archive.display(),
        format = format.name(),
        nodes = tree.len(),
        ms = start.elapsed().as_secs_f64() * 1000.0,
        "archive scan done"
    );
    let watch = ix.complete(
        tree,
        Outcome {
            error,
            elapsed: start.elapsed(),
        },
    );
    cache.insert(ix.clone());
    if let Some(tree) = ix.tree() {
        sink.finish(tree, watch, start);
    }
}

/// The entries of `dir`, with the symlinks among them by entry index.
fn entries_of(tree: &Tree, dir: NodeId) -> (Vec<Entry>, Vec<u8>, Vec<(u32, NodeId)>) {
    let mut entries = Vec::new();
    let mut names = Vec::new();
    let mut links = Vec::new();
    for k in tree.children(dir) {
        if tree.node(k).kind == NodeKind::Symlink {
            links.push((entries.len() as u32, k));
        }
        entries.push(tree.entry(k, &mut names));
    }
    (entries, names, links)
}

fn send_links(
    tree: &Tree,
    links: &[(u32, NodeId)],
    slot: usize,
    generation: u64,
    send: &dyn Fn(ListingMsg),
) {
    if links.is_empty() {
        return;
    }
    let kinds: Vec<(u32, LinkKind)> = links.iter().map(|&(i, k)| (i, tree.link_kind(k))).collect();
    for chunk in kinds.chunks(BATCH) {
        send(ListingMsg::LinkTargets {
            slot,
            generation,
            kinds: chunk.to_vec(),
        });
    }
}

/// Where a listing goes: the panel slot, the load generation, and the panel's local
/// directory, which `Done` reports. Another path to the same archive inode (a hard link, a
/// symlinked directory) hits the same cached index, so the directory is the request's, not
/// the index's.
#[derive(Clone, Copy)]
struct Place<'a> {
    slot: usize,
    generation: u64,
    dir: &'a Path,
}

/// Lists `inner` of a complete index: batches (a navigation) or one sorted listing (a
/// refresh, P-1), `Done`, then the symlink kinds.
fn list_complete(
    ix: &ArchiveIndex,
    inner: &VPath,
    at: Place<'_>,
    sort: Option<SortSpec>,
    send: &dyn Fn(ListingMsg),
    start: Instant,
) {
    let Some(tree) = ix.tree() else {
        return;
    };
    let (slot, generation) = (at.slot, at.generation);
    let dir = at.dir.to_path_buf();
    let Some(d) = tree
        .lookup(inner)
        .filter(|&d| tree.node(d).kind == NodeKind::Dir)
    else {
        send(ListingMsg::Failed {
            slot,
            generation,
            dir,
            error: NOT_IN_ARCHIVE.into(),
            gone: false,
        });
        return;
    };
    let (entries, names, links) = entries_of(tree, d);
    match sort {
        Some(spec) => send(ListingMsg::Listing {
            slot,
            generation,
            listing: Box::new(Listing::sorted(entries, names, spec)),
        }),
        None => send_batches(entries, &names, slot, generation, send),
    }
    send(ListingMsg::Done {
        slot,
        generation,
        dir,
        elapsed: start.elapsed(),
    });
    send_links(tree, &links, slot, generation, send);
}

/// Sends entries as navigation batches: the first at most [`FIRST_BATCH`] (M1 3.1).
fn send_batches(
    entries: Vec<Entry>,
    names: &[u8],
    slot: usize,
    generation: u64,
    send: &dyn Fn(ListingMsg),
) {
    let mut at = 0;
    let mut limit = FIRST_BATCH;
    while at < entries.len() {
        let end = (at + limit).min(entries.len());
        let mut part = Vec::with_capacity(end - at);
        let mut part_names = Vec::new();
        for e in &entries[at..end] {
            let mut e = *e;
            let n = e.name(names);
            e.name_off = part_names.len() as u32;
            part_names.extend_from_slice(n);
            part.push(e);
        }
        send(ListingMsg::Batch {
            slot,
            generation,
            entries: part,
            names: part_names,
        });
        at = end;
        limit = BATCH;
    }
}

/// Lists a directory of a complete index on a listing thread (P3 3.3). A refresh or
/// `Ctrl+R` first compares the archive name's `StatKey` with the index's (P3 3.2).
pub fn relist(req: &RelistRequest, send: &dyn Fn(ListingMsg)) {
    let start = Instant::now();
    let (slot, generation) = (req.slot, req.generation);
    let changed = match req.check {
        Check::No => false,
        Check::Refresh | Check::Rescan => stat_key(&req.index.archive) != Some(req.index.key),
    };
    if changed && req.check == Check::Rescan {
        send(ListingMsg::Changed {
            slot,
            generation,
            rescan: true,
        });
        return;
    }
    let at = Place {
        slot,
        generation,
        dir: &req.dir,
    };
    list_complete(&req.index, &req.inner, at, req.sort, send, start);
    if changed {
        send(ListingMsg::Changed {
            slot,
            generation,
            rescan: false,
        });
    }
}

/// `Space` on a directory of an archive (P3 2.4).
#[derive(Clone)]
pub struct SizeRequest {
    pub slot: usize,
    pub generation: u64,
    pub index: Arc<ArchiveIndex>,
    pub inner: VPath,
    pub name: OsString,
}

impl std::fmt::Debug for SizeRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SizeRequest")
            .field("slot", &self.slot)
            .field("inner", &self.inner)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl PartialEq for SizeRequest {
    fn eq(&self, o: &SizeRequest) -> bool {
        self.slot == o.slot
            && self.generation == o.generation
            && Arc::ptr_eq(&self.index, &o.index)
            && self.inner == o.inner
            && self.name == o.name
    }
}

impl Eq for SizeRequest {}

/// Answers a [`SizeRequest`] with `DirSize`.
pub fn size(req: &SizeRequest, send: &dyn Fn(ListingMsg)) {
    send(ListingMsg::DirSize {
        slot: req.slot,
        generation: req.generation,
        name: req.name.clone(),
        bytes: dir_size(&req.index, &req.inner, &req.name),
    });
}

/// `Space` in an archive (P3 2.4): a directory's size from the index, at once.
pub fn dir_size(index: &ArchiveIndex, inner: &VPath, name: &OsStr) -> Option<u64> {
    let tree = index.tree()?;
    let id = tree.lookup(&inner.join(name).ok()?)?;
    Some(tree.node(id).size)
}

/// Runs `f` under `catch_unwind` (NFR-REL): a panic in a parser fails the load.
pub fn guarded(
    slot: usize,
    generation: u64,
    archive: &Path,
    send: &dyn Fn(ListingMsg),
    f: impl FnOnce(),
) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
        send(ListingMsg::Failed {
            slot,
            generation,
            dir: archive
                .parent()
                .map_or_else(|| PathBuf::from("/"), Path::to_path_buf),
            error: "internal error while reading the archive".into(),
            gone: false,
        });
    }
}

/// Sends the watched directory's rows while a scan runs (P3 3.3): what is known when the
/// watch moves there, then each new member. The first batch goes out as soon as it holds a
/// row (at most [`FIRST_BATCH`]); later ones hold at most [`BATCH`] and go out at least
/// every 100 ms. A later duplicate that replaces a node of the watched directory makes the
/// panel re-read it (`Reset`, then every row again).
pub(crate) struct Sink<'a> {
    ix: &'a ArchiveIndex,
    send: &'a dyn Fn(ListingMsg),
    version: u64,
    watch: Option<Watch>,
    dir: Option<NodeId>,
    entries: Vec<Entry>,
    names: Vec<u8>,
    links: Vec<(u32, NodeId)>,
    sent: u32,
    first: bool,
    last: Instant,
}

impl<'a> Sink<'a> {
    fn new(ix: &'a ArchiveIndex, send: &'a dyn Fn(ListingMsg)) -> Sink<'a> {
        Sink {
            ix,
            send,
            version: u64::MAX,
            watch: None,
            dir: None,
            entries: Vec::new(),
            names: Vec::new(),
            links: Vec::new(),
            sent: 0,
            first: true,
            last: Instant::now(),
        }
    }

    /// Follows a moved watch; flushes rows that waited long enough.
    pub(crate) fn poll(&mut self, tree: &Tree) {
        let v = self.ix.watch_version.load(Ordering::Acquire);
        if v != self.version {
            self.version = v;
            let w = lock(&self.ix.watch).clone();
            self.switch(tree, w);
        }
        self.maybe_flush();
    }

    fn switch(&mut self, tree: &Tree, w: Option<Watch>) {
        self.entries.clear();
        self.names.clear();
        self.links.clear();
        self.sent = 0;
        self.first = true;
        self.last = Instant::now();
        self.dir = w.as_ref().and_then(|w| Self::resolve(tree, &w.inner));
        self.watch = w;
        if let Some(d) = self.dir {
            self.push_all(tree, d);
        }
    }

    fn resolve(tree: &Tree, inner: &VPath) -> Option<NodeId> {
        tree.lookup(inner)
            .filter(|&d| tree.node(d).kind == NodeKind::Dir)
    }

    fn push_all(&mut self, tree: &Tree, d: NodeId) {
        for k in tree.children(d) {
            self.push(tree, k);
        }
    }

    fn push(&mut self, tree: &Tree, k: NodeId) {
        if tree.node(k).kind == NodeKind::Symlink {
            self.links.push((self.sent + self.entries.len() as u32, k));
        }
        self.entries.push(tree.entry(k, &mut self.names));
        let limit = if self.first { FIRST_BATCH } else { BATCH };
        if self.entries.len() >= limit {
            self.flush();
        }
    }

    /// A new node: a row of the watched directory, or the watched directory itself.
    fn fresh(&mut self, tree: &Tree, id: NodeId) {
        let n = tree.node(id);
        if Some(n.parent) == self.dir {
            self.push(tree, id);
        } else if self.dir.is_none()
            && n.kind == NodeKind::Dir
            && let Some(w) = &self.watch
        {
            self.dir = Self::resolve(tree, &w.inner);
            if let Some(d) = self.dir {
                self.push_all(tree, d);
            }
        }
    }

    /// One member went into the tree, with the directories synthesized for it.
    pub(crate) fn added(&mut self, tree: &Tree, a: Added) {
        if self.watch.is_none() {
            return;
        }
        let known = self.dir.is_some();
        for &id in tree.synthesized() {
            self.fresh(tree, id);
        }
        match a {
            // The watched directory appeared on the way to this member: its rows, this one
            // included, went out with it.
            Added::New(_) if !known && self.dir.is_some() => {}
            Added::New(id) => self.fresh(tree, id),
            Added::Replaced(id) => {
                if Some(tree.node(id).parent) == self.dir
                    && let Some(w) = &self.watch
                {
                    (self.send)(ListingMsg::Reset {
                        slot: w.slot,
                        generation: w.generation,
                    });
                    self.entries.clear();
                    self.names.clear();
                    self.links.clear();
                    self.sent = 0;
                    if let Some(d) = self.dir {
                        self.push_all(tree, d);
                    }
                }
            }
            Added::Skipped => {}
        }
        self.maybe_flush();
    }

    /// Symlink targets arrived after their rows went out (a 7z symlink's target is its
    /// data, read after the header, P3 3.3): the panel re-reads the watched directory when
    /// it holds a symlink, so its rows carry the targets' sizes.
    pub(crate) fn relink(&mut self, tree: &Tree) {
        let (Some(d), Some(w)) = (self.dir, &self.watch) else {
            return;
        };
        if !tree
            .children(d)
            .any(|k| tree.node(k).kind == NodeKind::Symlink)
        {
            return;
        }
        (self.send)(ListingMsg::Reset {
            slot: w.slot,
            generation: w.generation,
        });
        self.entries.clear();
        self.names.clear();
        self.links.clear();
        self.sent = 0;
        self.push_all(tree, d);
    }

    /// The scan is about to inflate `bytes` of member data: rows found so far go out
    /// first, so they never wait behind a long member (P-19: first rows at once).
    pub(crate) fn before_skip(&mut self, bytes: u64) {
        if bytes >= SKIP_FLUSH {
            self.flush();
        }
    }

    /// The first batch goes out as soon as it holds a row, as the M1 listing's small first
    /// batch does: a decoder that takes long per block (a bzip2 block of 900 kB takes 20 to
    /// 30 ms) must not hold the first rows until the next block (P-19). Later rows gather
    /// for up to 100 ms.
    fn maybe_flush(&mut self) {
        let due = self.first || self.last.elapsed() >= Duration::from_millis(100);
        if !self.entries.is_empty() && due {
            self.flush();
        }
    }

    fn flush(&mut self) {
        let Some(w) = &self.watch else {
            self.entries.clear();
            self.names.clear();
            return;
        };
        if self.entries.is_empty() {
            return;
        }
        self.sent += self.entries.len() as u32;
        (self.send)(ListingMsg::Batch {
            slot: w.slot,
            generation: w.generation,
            entries: std::mem::take(&mut self.entries),
            names: std::mem::take(&mut self.names),
        });
        self.first = false;
        self.last = Instant::now();
    }

    /// The scan ended: the last rows, `Done` and the symlink kinds for the watch the
    /// index took at completion.
    fn finish(mut self, tree: &Tree, watch: Option<Watch>, start: Instant) {
        if watch != self.watch {
            self.switch(tree, watch);
        }
        self.flush();
        let Some(w) = self.watch.clone() else {
            return;
        };
        if self.dir.is_none() {
            (self.send)(ListingMsg::Failed {
                slot: w.slot,
                generation: w.generation,
                dir: self.ix.dir(),
                error: NOT_IN_ARCHIVE.into(),
                gone: false,
            });
            return;
        }
        (self.send)(ListingMsg::Done {
            slot: w.slot,
            generation: w.generation,
            dir: self.ix.dir(),
            elapsed: start.elapsed(),
        });
        send_links(tree, &self.links, w.slot, w.generation, self.send);
    }

    fn cancelled(&self) -> bool {
        self.ix.cancel.load(Ordering::Relaxed)
    }
}

/// Whether an entry kind of a listing is a directory for `Enter` in an archive, following
/// a symlink inside the index (P3 2.4).
pub fn enter_target(
    index: &ArchiveIndex,
    inner: &VPath,
    name: &[u8],
    kind: EKind,
) -> Option<VPath> {
    let tree = index.tree()?;
    let dir = tree.lookup(inner)?;
    let id = tree.child(dir, name)?;
    let target = match kind {
        EKind::Dir => id,
        EKind::Symlink => tree.follow(id)?,
        _ => return None,
    };
    (tree.node(target).kind == NodeKind::Dir).then(|| tree.path_of(target))
}

/// What F3, F4 and `Enter` on the entry `name` of `inner` read (P3 3.4): a regular file, the
/// member a hard link names, or the regular file a symlink leads to inside the index; its
/// path and declared size. `Err` says why the entry cannot be viewed.
pub fn view_target(
    index: &ArchiveIndex,
    inner: &VPath,
    name: &[u8],
) -> Result<(VPath, u64), &'static str> {
    let tree = index.tree().ok_or(STILL_READING)?;
    let id = tree
        .lookup(inner)
        .and_then(|d| tree.child(d, name))
        .ok_or(NOT_IN_ARCHIVE)?;
    let mut id = match tree.node(id).kind {
        NodeKind::Symlink => tree.follow(id).ok_or("the link leads out of the archive")?,
        _ => id,
    };
    if tree.node(id).kind == NodeKind::HardLink {
        id = tree
            .hard_target(id)
            .ok_or(crate::fsops::copy::LINK_NOT_EXTRACTED)?;
    }
    let n = tree.node(id);
    match n.kind {
        NodeKind::File if n.flags & ENCRYPTED != 0 => Err(extract::ENCRYPTED_MEMBER),
        NodeKind::File => Ok((tree.path_of(id), n.size)),
        NodeKind::Special => Err("special file"),
        _ => Err("not a regular file"),
    }
}

/// The bytes the entry `name` of `inner` declares: a file's size, a directory's total
/// (P3 3.5: the confirm dialog shows the declared total).
pub fn member_bytes(index: &ArchiveIndex, inner: &VPath, name: &OsStr) -> u64 {
    dir_size(index, inner, name).unwrap_or(0)
}

/// The display form of an archive place: `archive.zip:/inner/dir` (P3 2.2).
pub fn title(archive: &Path, inner: &VPath) -> Vec<u8> {
    let mut t = archive.as_os_str().as_bytes().to_vec();
    t.push(b':');
    t.extend_from_slice(&inner.to_bytes());
    t
}

/// The entries of an archive root, by name, for tests and diagnostics.
pub fn root_names(index: &ArchiveIndex) -> Vec<OsString> {
    index
        .tree()
        .map(|t| t.child_names(index::ROOT))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_rounds_up_to_megabytes() {
        assert_eq!(progress_text(0, 98 << 20), "reading archive: 0 of 98 MB");
        assert_eq!(
            progress_text(48 << 20, 98 << 20),
            "reading archive: 48 of 98 MB"
        );
        assert_eq!(progress_text(10, 20), "reading archive: 1 of 1 MB");
    }

    /// Scans `members` into a fresh index whose panel watches `a/b` from the start, and
    /// returns what the scan sent.
    fn scan_watching(members: &[&'static [u8]]) -> Vec<ListingMsg> {
        use index::{Member, MemberKind};
        let ix = ArchiveIndex::new(
            "/x/a.tar".into(),
            StatKey::default(),
            Format::Tar,
            Arc::new(File::open("/dev/null").unwrap()),
            Arc::new(AtomicBool::new(false)),
            Arc::default(),
        );
        assert!(ix.watch(Watch {
            slot: 1,
            generation: 2,
            inner: VPath::parse(b"a/b").unwrap(),
        }));
        let msgs = std::cell::RefCell::new(Vec::new());
        let send = |m| msgs.borrow_mut().push(m);
        let mut sink = Sink::new(&ix, &send);
        let mut tree = Tree::default();
        for &name in members {
            sink.poll(&tree);
            let added = tree
                .add(Member {
                    name,
                    kind: MemberKind::File,
                    mode: 0o644,
                    size: 1,
                    mtime: None,
                    locator: 0,
                    encrypted: false,
                })
                .unwrap();
            sink.added(&tree, added);
        }
        tree.finish();
        let watch = ix.complete(tree, Outcome::default());
        sink.finish(ix.tree().unwrap(), watch, Instant::now());
        msgs.into_inner()
    }

    fn rows(ms: &[ListingMsg]) -> Vec<Vec<u8>> {
        let mut v: Vec<Vec<u8>> = ms
            .iter()
            .filter_map(|m| match m {
                ListingMsg::Batch { entries, names, .. } => Some(
                    entries
                        .iter()
                        .map(|e| e.name(names).to_vec())
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .flatten()
            .collect();
        v.sort();
        v
    }

    /// P3 3.3: a watched directory that appears only during the scan, implicitly, shows
    /// each row once; a later duplicate in it makes the panel re-read it.
    #[test]
    fn the_scan_streams_the_watched_directory_once() {
        let msgs = scan_watching(&[b"x", b"a/b/c", b"a/b/d", b"a/e"]);
        assert_eq!(rows(&msgs), [b"c".to_vec(), b"d".to_vec()]);
        assert!(!msgs.iter().any(|m| matches!(m, ListingMsg::Reset { .. })));
        assert!(matches!(
            msgs.last(),
            Some(ListingMsg::Done {
                slot: 1,
                generation: 2,
                ..
            })
        ));
        let msgs = scan_watching(&[b"a/b/c", b"a/b/d", b"a/b/c"]);
        let reset = msgs
            .iter()
            .position(|m| {
                matches!(
                    m,
                    ListingMsg::Reset {
                        slot: 1,
                        generation: 2
                    }
                )
            })
            .expect("the duplicate re-reads the directory");
        // Rows before the reset go; after it every row comes again, once.
        assert_eq!(rows(&msgs[reset..]), [b"c".to_vec(), b"d".to_vec()]);
    }

    /// P-19: the scan's first row goes out as soon as the member is in the tree, without a
    /// wait; the rows after it gather into later batches.
    #[test]
    fn the_first_row_goes_out_at_once() {
        use index::{Member, MemberKind};
        let ix = ArchiveIndex::new(
            "/x/a.tar.bz2".into(),
            StatKey::default(),
            Format::TarBz2,
            Arc::new(File::open("/dev/null").unwrap()),
            Arc::new(AtomicBool::new(false)),
            Arc::default(),
        );
        assert!(ix.watch(Watch {
            slot: 0,
            generation: 1,
            inner: VPath::root(),
        }));
        let msgs = std::cell::RefCell::new(Vec::new());
        let send = |m| msgs.borrow_mut().push(m);
        let mut sink = Sink::new(&ix, &send);
        let mut tree = Tree::default();
        let batches = |msgs: &std::cell::RefCell<Vec<ListingMsg>>| {
            msgs.borrow()
                .iter()
                .filter(|m| matches!(m, ListingMsg::Batch { .. }))
                .count()
        };
        let add = |tree: &mut Tree, sink: &mut Sink<'_>, name: &'static [u8]| {
            sink.poll(tree);
            let added = tree
                .add(Member {
                    name,
                    kind: MemberKind::File,
                    mode: 0o644,
                    size: 1,
                    mtime: None,
                    locator: 0,
                    encrypted: false,
                })
                .unwrap();
            sink.added(tree, added);
        };
        add(&mut tree, &mut sink, b"a");
        let first = Instant::now();
        assert_eq!(batches(&msgs), 1, "the first row waits for nothing");
        assert_eq!(rows(&msgs.borrow()), [b"a".to_vec()]);
        add(&mut tree, &mut sink, b"b");
        add(&mut tree, &mut sink, b"c");
        if first.elapsed() < Duration::from_millis(100) {
            assert_eq!(batches(&msgs), 1, "later rows gather");
        }
        tree.finish();
        let watch = ix.complete(tree, Outcome::default());
        sink.finish(ix.tree().unwrap(), watch, Instant::now());
        let msgs = msgs.into_inner();
        assert_eq!(rows(&msgs), [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
        assert!(matches!(msgs.last(), Some(ListingMsg::Done { .. })));
    }

    #[test]
    fn a_title_joins_the_archive_and_the_inner_directory() {
        assert_eq!(
            title(Path::new("/x/a.zip"), &VPath::parse(b"in/d").unwrap()),
            b"/x/a.zip:/in/d"
        );
        assert_eq!(title(Path::new("/a.tar"), &VPath::root()), b"/a.tar:/");
    }
}
