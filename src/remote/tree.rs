#![forbid(unsafe_code)]
//! Downloads (F5 remote to local, P3 5.5): a copy job with a [`RemoteOrigin`] source
//! (P3 2.3). The destination side is the local engine's: temporary file, commit with
//! `RENAME_NOREPLACE`, questions, progress, cancel (I-2, I-3, I-5 on the write side).
//!
//! **The scan** [`Walk`] `LSTAT`s each selected name and walks directories with `READDIR`,
//! with up to [`LISTINGS`] directory listings in flight. Every directory is `LSTAT`ed
//! before its `OPENDIR`, and a symlink is never descended (R-3). Symlink targets are read
//! with pipelined `READLINK`s, so a symlink arrives as a symlink with its target
//! byte-identical. A name with `/` or NUL from the server is skipped and reported. SFTP
//! version 3 has no inode numbers: hard links on the server arrive as separate files, and
//! the identities are synthetic (P3 2.1), so the same-file check never applies. A reply of
//! the wrong type ends the session, as it does for every other request (E-19): the entry
//! fails with "protocol error", and the rest with "connection lost".
//!
//! **Each regular file** is `LSTAT`ed again and opened only when it is still a regular
//! file (R-3), then `FSTAT`ed: it must still be a regular file of the planned size. It is
//! read with the pipelined window into the local engine's temporary file, and a final
//! `FSTAT` must still show the planned size and mtime, else the entry fails with "source
//! changed" (P3 5.5). The mode (masked as in M1 4.7) and the times (whole seconds) are
//! applied by the engine.
//!
//! **Cancel** stops the scan or the file: the outstanding replies are drained and dropped
//! by id, and a server that stops answering (an `OPEN` that met a FIFO) loses the session
//! after the 2 s drain window (P3 2.5). Every directory handle the scan holds is closed,
//! and so is one that a drained `OPENDIR` reply carries. **Session loss** fails the
//! remaining entries with "connection lost" (I-7); a file in progress leaves no partial
//! local name.
//!
//! **A move out of a server** (F6, [`download_move`]) is this download with the local
//! group commit of a move (M1 4.8): each batch is synced with `syncfs`. The remote sources
//! are kept: SFTP version 3 cannot identify the file that was read, so `remove` keeps every
//! one of them, and the report says so (R-4).

use super::proto::{self, Packet};
use super::provider::{
    OpenError, RemoteProvider, SOURCE_CHANGED, join, location, meta_of, open_checked, valid_name,
};
use super::session::{Session, SftpError};
use crate::fsops::copy::{Dir, copy_from, prepare, room, tree_walk};
use crate::fsops::group::{Group, NOT_LOCAL, OpenGroup, Opened, Root, validate};
use crate::fsops::job::{JobVerb, Report};
use crate::fsops::origin::{Origin, OriginDir, OriginFile, Removed, key};
use crate::fsops::plan::{Node, Note, Plan, Refusal, Totals, Verb};
use crate::fsops::question::{Interaction, Phase, Progress, Reporter};
use crate::fsops::sys::{Kind, Meta, Snapshot, Sys};
use crate::fsops::walk::EntryError;
use crate::provider::{VPath, synthetic_id};
use crate::ui::text::escaped;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Directory listings in flight during a scan (P3 5.5).
pub const LISTINGS: usize = 8;

/// `LSTAT`s and `READLINK`s kept outstanding during a scan.
const WINDOW: usize = 64;

/// What a download move keeps, as its confirm dialog and its report say (R-4): the origin
/// keeps every remote source whatever asks.
pub const REMOTE_KEPT: &str = "remote sources kept: the server cannot identify them";

/// A name from the server that is not a single component (P3 5.4).
pub const INVALID_NAME: &str = "invalid name from the server";

/// An entry whose server sent no mode (P3 5.4): verbs skip it.
pub const UNKNOWN_TYPE: &str = "unknown type";

/// What an entry fails with after the session ended (P3 5.5).
pub const LOST: &str = "connection lost";

/// What the entry fails with whose reply had the wrong type (E-19).
const PROTOCOL: &str = "protocol error";

const S_IFDIR: u32 = 0o040_000;

/// One entry the scan found.
struct Item {
    name: OsString,
    meta: Meta,
    note: Option<Note>,
    children: Vec<usize>,
    /// Its path on the server.
    path: Vec<u8>,
}

/// Where a directory listing of the scan is.
enum Stage {
    Lstat,
    Opendir,
    Readdir(Vec<u8>),
}

struct Active {
    item: usize,
    stage: Stage,
}

/// A scan of remote trees (P3 5.5): the selected names `LSTAT`ed, the directories walked
/// with up to [`LISTINGS`] listings in flight, the symlinks read. It builds the plan's
/// nodes; nothing is written anywhere.
pub struct Walk<'a> {
    s: &'a Session,
    place: u64,
    cancel: &'a AtomicBool,
    items: Vec<Item>,
    next: u64,
    /// Directories to list: the item, and whether it was `LSTAT`ed just now.
    dirs: VecDeque<(usize, bool)>,
    links: Vec<usize>,
    /// Symlink targets by the node key.
    pub targets: HashMap<u64, OsString>,
    /// The session ended during the scan.
    pub lost: bool,
    /// Called every 4096 entries with the running totals (progress).
    pub(crate) tick: Option<&'a mut dyn FnMut(Totals)>,
    seen: u64,
    /// Symlink targets are read after the walk (a download); a delete needs none.
    pub read_targets: bool,
}

/// Why a scan stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum Stopped {
    Cancelled,
}

impl<'a> Walk<'a> {
    pub fn new(s: &'a Session, place: u64, cancel: &'a AtomicBool) -> Walk<'a> {
        Walk {
            s,
            place,
            cancel,
            items: Vec::new(),
            next: 1,
            dirs: VecDeque::new(),
            links: Vec::new(),
            targets: HashMap::new(),
            lost: false,
            tick: None,
            seen: 0,
            read_targets: true,
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// A new item for a server entry with attributes `a`.
    fn add(&mut self, name: OsString, a: &proto::Attrs, path: Vec<u8>) -> usize {
        let id = synthetic_id(self.place, self.next);
        self.next += 1;
        let meta = meta_of(a, id);
        let i = self.items.len();
        let mut note = None;
        match meta.kind {
            Kind::Dir => self.dirs.push_back((i, false)),
            Kind::Symlink => self.links.push(i),
            Kind::Unknown => note = Some(Note::Skip(UNKNOWN_TYPE.into())),
            _ => {}
        }
        self.items.push(Item {
            name,
            meta,
            note,
            children: Vec::new(),
            path,
        });
        self.seen += 1;
        if self.seen.is_multiple_of(4096) {
            let t = self.totals();
            if let Some(f) = self.tick.as_mut() {
                f(t);
            }
        }
        i
    }

    /// An item that stands for a name the scan could not use.
    fn failed(&mut self, name: OsString, note: Note) -> usize {
        let i = self.items.len();
        self.items.push(Item {
            name,
            meta: Meta::default(),
            note: Some(note),
            children: Vec::new(),
            path: Vec::new(),
        });
        i
    }

    /// The running totals, for progress.
    fn totals(&self) -> Totals {
        let mut t = Totals::default();
        for it in &self.items {
            match it.meta.kind {
                Kind::Dir => t.dirs += 1,
                Kind::File => {
                    t.files += 1;
                    t.bytes += it.meta.size;
                }
                Kind::Symlink => t.symlinks += 1,
                _ => t.specials += 1,
            }
        }
        t
    }

    fn note_lost(&mut self, i: usize) {
        self.items[i].note = Some(Note::Fail(LOST.into()));
        self.items[i].children.clear();
    }

    /// `LSTAT`s the selected `names` in `dir` (pipelined) and returns their items.
    pub fn roots(&mut self, dir: &VPath, names: &[OsString]) -> Result<Vec<usize>, Stopped> {
        let base = dir.to_bytes();
        let mut out = vec![usize::MAX; names.len()];
        let mut pending: VecDeque<(u32, usize)> = VecDeque::new();
        let mut todo = names.iter().enumerate();
        loop {
            if self.cancelled() {
                self.drain(pending.iter().map(|p| p.0).collect());
                return Err(Stopped::Cancelled);
            }
            while !self.lost && pending.len() < WINDOW {
                let Some((k, n)) = todo.next() else {
                    break;
                };
                let path = join(&base, n.as_bytes());
                match self.s.send(self.cancel, |id| Packet::Lstat { id, path }) {
                    Ok(id) => pending.push_back((id, k)),
                    Err(SftpError::Cancelled) => {
                        self.drain(pending.iter().map(|p| p.0).collect());
                        return Err(Stopped::Cancelled);
                    }
                    Err(_) => self.lost = true,
                }
            }
            let Some((id, k)) = pending.pop_front() else {
                break;
            };
            let name = names[k].clone();
            let path = join(&base, name.as_bytes());
            out[k] = match self.s.wait(id, self.cancel) {
                Ok(Packet::Attrs { attrs, .. }) => {
                    let i = self.add(name, &attrs, path);
                    if let Some(d) = self.dirs.back_mut()
                        && d.0 == i
                    {
                        // `LSTAT`ed just now: the listing starts at `OPENDIR` (R-3).
                        d.1 = true;
                    }
                    i
                }
                Ok(Packet::Status {
                    code: proto::status::NO_SUCH_FILE,
                    ..
                }) => self.failed(name, Note::Failed(EntryError::Disappeared)),
                Ok(Packet::Status { code, message, .. }) => self.failed(
                    name,
                    Note::Fail(format!("stat: {}", status_text(code, &message))),
                ),
                Ok(p) => {
                    // E-19: the session ends; what is still outstanding fails with it.
                    self.s.unexpected(&p);
                    self.lost = true;
                    self.failed(name, Note::Fail(PROTOCOL.into()))
                }
                Err(SftpError::Cancelled) => {
                    let mut ids = vec![id];
                    ids.extend(pending.iter().map(|p| p.0));
                    self.drain(ids);
                    return Err(Stopped::Cancelled);
                }
                Err(_) => {
                    self.lost = true;
                    self.failed(name, Note::Fail(LOST.into()))
                }
            };
        }
        // Names never sent after a loss.
        for (k, slot) in out.iter_mut().enumerate() {
            if *slot == usize::MAX {
                *slot = self.failed(names[k].clone(), Note::Fail(LOST.into()));
            }
        }
        Ok(out)
    }

    /// Drops the replies to `ids` (P3 2.5); a server that stays silent loses the session.
    fn drain(&mut self, ids: Vec<u32>) {
        if !ids.is_empty() && self.s.drain(&ids).is_err() {
            self.lost = true;
        }
    }

    /// `CLOSE`s a directory handle the scan holds; nobody reads the reply. Nothing goes
    /// out once the session has ended: the server's handles ended with it.
    fn close(&self, handle: Vec<u8>) {
        self.s
            .send_forget(self.cancel, |id| Packet::Close { id, handle });
    }

    /// Walks every directory found so far and below, with up to [`LISTINGS`] listings in
    /// flight, then reads the symlinks' targets.
    pub fn walk(&mut self) -> Result<(), Stopped> {
        let mut active: HashMap<usize, Active> = HashMap::new();
        let mut fifo: VecDeque<(u32, usize)> = VecDeque::new();
        let mut next_key = 0usize;
        let r = 'run: loop {
            if self.cancelled() {
                break 'run Err(Stopped::Cancelled);
            }
            while !self.lost && active.len() < LISTINGS {
                let Some((i, fresh)) = self.dirs.pop_front() else {
                    break;
                };
                let path = self.items[i].path.clone();
                let (sent, stage) = if fresh {
                    (
                        self.s.send(self.cancel, |id| Packet::Opendir { id, path }),
                        Stage::Opendir,
                    )
                } else {
                    (
                        self.s.send(self.cancel, |id| Packet::Lstat { id, path }),
                        Stage::Lstat,
                    )
                };
                match sent {
                    Ok(id) => {
                        active.insert(next_key, Active { item: i, stage });
                        fifo.push_back((id, next_key));
                        next_key += 1;
                    }
                    Err(SftpError::Cancelled) => break 'run Err(Stopped::Cancelled),
                    Err(_) => {
                        self.lost = true;
                        self.note_lost(i);
                    }
                }
            }
            let Some((id, k)) = fifo.pop_front() else {
                break Ok(());
            };
            let reply = match self.s.wait(id, self.cancel) {
                Ok(p) => p,
                Err(SftpError::Cancelled) => {
                    fifo.push_front((id, k));
                    break 'run Err(Stopped::Cancelled);
                }
                Err(_) => {
                    self.lost = true;
                    for a in active.values() {
                        self.items[a.item].note = Some(Note::Fail(LOST.into()));
                        self.items[a.item].children.clear();
                    }
                    active.clear();
                    fifo.clear();
                    while let Some((i, _)) = self.dirs.pop_front() {
                        self.note_lost(i);
                    }
                    break Ok(());
                }
            };
            let Some(a) = active.get_mut(&k) else {
                continue;
            };
            let item = a.item;
            let path = self.items[item].path.clone();
            // The stage is out of `active` while its reply is handled; every way on puts a
            // held handle back, or closes it (P3 5.5).
            let stage = std::mem::replace(&mut a.stage, Stage::Lstat);
            let next = match (stage, reply) {
                // R-3: only a directory is opened; a swap to anything else fails it.
                (Stage::Lstat, Packet::Attrs { attrs, .. }) => {
                    if attrs.kind() == Some(S_IFDIR) {
                        Some((
                            Stage::Opendir,
                            self.s.send(self.cancel, |id| Packet::Opendir { id, path }),
                        ))
                    } else {
                        self.items[item].note = Some(Note::Failed(EntryError::TypeChanged));
                        None
                    }
                }
                (Stage::Opendir, Packet::Handle { handle, .. }) => {
                    let h = handle.clone();
                    Some((
                        Stage::Readdir(handle),
                        self.s
                            .send(self.cancel, |id| Packet::Readdir { id, handle: h }),
                    ))
                }
                (Stage::Readdir(handle), Packet::Name { names, .. }) => {
                    for n in names {
                        let f = n.filename;
                        if f == b"." || f == b".." {
                            continue;
                        }
                        let c = if valid_name(&f) {
                            let p = join(&path, &f);
                            self.add(OsString::from_vec(f), &n.attrs, p)
                        } else {
                            let shown = OsString::from(escaped(&f));
                            self.failed(shown, Note::Skip(INVALID_NAME.into()))
                        };
                        self.items[item].children.push(c);
                    }
                    let h = handle.clone();
                    Some((
                        Stage::Readdir(handle),
                        self.s
                            .send(self.cancel, |id| Packet::Readdir { id, handle: h }),
                    ))
                }
                (
                    Stage::Readdir(handle),
                    Packet::Status {
                        code: proto::status::EOF,
                        ..
                    },
                ) => {
                    self.close(handle);
                    None
                }
                (stage, Packet::Status { code, message, .. }) => {
                    let what = match stage {
                        Stage::Lstat => "stat",
                        Stage::Opendir => "open directory",
                        Stage::Readdir(handle) => {
                            self.close(handle);
                            "read directory"
                        }
                    };
                    self.items[item].note = Some(if code == proto::status::NO_SUCH_FILE {
                        Note::Failed(EntryError::Disappeared)
                    } else {
                        Note::Fail(format!("{what}: {}", status_text(code, &message)))
                    });
                    None
                }
                (stage, p) => {
                    // E-19: the session ends, after the handles the scan holds are closed.
                    if let Stage::Readdir(handle) = stage {
                        self.close(handle);
                    }
                    for a in active.values() {
                        if let Stage::Readdir(handle) = &a.stage {
                            self.close(handle.clone());
                        }
                    }
                    self.s.unexpected(&p);
                    self.lost = true;
                    self.items[item].note = Some(Note::Fail(PROTOCOL.into()));
                    None
                }
            };
            match next {
                None => {
                    active.remove(&k);
                }
                Some((stage, Ok(id))) => {
                    if let Some(a) = active.get_mut(&k) {
                        a.stage = stage;
                    }
                    fifo.push_back((id, k));
                }
                Some((stage, Err(SftpError::Cancelled))) => {
                    // A handle just received stays held, so the cancel below closes it.
                    if let Some(a) = active.get_mut(&k) {
                        a.stage = stage;
                    }
                    break 'run Err(Stopped::Cancelled);
                }
                Some((_, Err(_))) => {
                    self.lost = true;
                    active.remove(&k);
                    self.note_lost(item);
                }
            }
        };
        if r.is_err() {
            // Cancelled: the outstanding replies are dropped by id (P3 2.5), and a handle
            // among them (an `OPENDIR` in flight) is closed by the session. Then every
            // listing closes the handle it holds (P3 5.5).
            let ids: Vec<u32> = fifo.iter().map(|f| f.0).collect();
            self.drain(ids);
            for a in active.into_values() {
                if let Stage::Readdir(handle) = a.stage {
                    self.close(handle);
                }
            }
            return r;
        }
        // Directories never listed after a loss.
        while let Some((i, _)) = self.dirs.pop_front() {
            self.note_lost(i);
        }
        if !self.read_targets {
            return Ok(());
        }
        self.read_links()
    }

    /// The symlinks' targets, with pipelined `READLINK`s.
    fn read_links(&mut self) -> Result<(), Stopped> {
        let links = std::mem::take(&mut self.links);
        let mut pending: VecDeque<(u32, usize)> = VecDeque::new();
        let mut todo = links.into_iter();
        loop {
            if self.cancelled() {
                self.drain(pending.iter().map(|p| p.0).collect());
                return Err(Stopped::Cancelled);
            }
            while !self.lost && pending.len() < WINDOW {
                let Some(i) = todo.next() else {
                    break;
                };
                let path = self.items[i].path.clone();
                match self.s.send(self.cancel, |id| Packet::Readlink { id, path }) {
                    Ok(id) => pending.push_back((id, i)),
                    Err(SftpError::Cancelled) => {
                        self.drain(pending.iter().map(|p| p.0).collect());
                        return Err(Stopped::Cancelled);
                    }
                    Err(_) => {
                        self.lost = true;
                        self.note_lost(i);
                    }
                }
            }
            let Some((id, i)) = pending.pop_front() else {
                break;
            };
            match self.s.wait(id, self.cancel) {
                Ok(Packet::Name { mut names, .. }) if names.len() == 1 => {
                    let t = std::mem::take(&mut names[0].filename);
                    self.targets
                        .insert(self.items[i].meta.id.ino, OsString::from_vec(t));
                }
                Ok(Packet::Status { code, message, .. }) => {
                    self.items[i].note = Some(Note::Fail(format!(
                        "read link: {}",
                        status_text(code, &message)
                    )));
                }
                Ok(p) => {
                    // E-19: the session ends; what is still outstanding fails with it.
                    self.s.unexpected(&p);
                    self.lost = true;
                    self.items[i].note = Some(Note::Fail(PROTOCOL.into()));
                }
                Err(SftpError::Cancelled) => {
                    let mut ids = vec![id];
                    ids.extend(pending.iter().map(|p| p.0));
                    self.drain(ids);
                    return Err(Stopped::Cancelled);
                }
                Err(_) => {
                    self.lost = true;
                    self.note_lost(i);
                }
            }
        }
        // Symlinks never asked after a loss.
        for i in todo {
            self.note_lost(i);
        }
        Ok(())
    }

    /// The plan node of item `i` and its subtree; children sorted by name bytes.
    pub(crate) fn node(&self, i: usize, totals: &mut Totals) -> Node {
        let it = &self.items[i];
        let mut n = Node {
            name: it.name.clone(),
            meta: it.meta,
            children: Vec::new(),
            note: it.note.clone(),
        };
        match it.meta.kind {
            Kind::Dir => {
                totals.dirs += 1;
                if n.note.is_none() {
                    n.children = it.children.iter().map(|&c| self.node(c, totals)).collect();
                    n.children
                        .sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
                }
            }
            Kind::File => {
                totals.files += 1;
                totals.bytes += it.meta.size;
            }
            Kind::Symlink => totals.symlinks += 1,
            _ => totals.specials += 1,
        }
        n
    }

    /// The bytes of the regular files below item `i` (`Space` on a remote directory).
    fn bytes(&self, i: usize) -> u64 {
        let it = &self.items[i];
        match it.meta.kind {
            Kind::File => it.meta.size,
            Kind::Dir => it.children.iter().map(|&c| self.bytes(c)).sum(),
            _ => 0,
        }
    }
}

/// The text of a status reply, as [`SftpError`] shows it.
fn status_text(code: u32, message: &[u8]) -> String {
    SftpError::Status {
        code,
        message: String::from_utf8_lossy(message).into_owned(),
    }
    .to_string()
}

/// `Space` on a remote directory (P3 2.4): the size of the regular files below it, by a
/// walk on the server that `cancel` stops. `None` when the walk did not complete.
pub fn size(
    remote: &RemoteProvider,
    dir: &VPath,
    name: &OsStr,
    cancel: &AtomicBool,
) -> Option<u64> {
    let mut w = Walk::new(remote.session(), remote.id(), cancel);
    let roots = w.roots(dir, &[name.to_owned()]).ok()?;
    w.walk().ok()?;
    let failed = |n: &Option<Note>| matches!(n, Some(Note::Fail(_) | Note::Failed(_)));
    if w.lost || w.items.iter().any(|i| failed(&i.note)) {
        return None;
    }
    Some(roots.iter().map(|&r| w.bytes(r)).sum())
}

/// `Space` on a remote directory (P3 2.4): the walk that sizes it, on a listing thread.
#[derive(Clone)]
pub struct SizeRequest {
    pub slot: usize,
    pub generation: u64,
    pub remote: Arc<RemoteProvider>,
    pub dir: VPath,
    pub name: OsString,
    /// `Esc` stops the walk.
    pub cancel: Arc<AtomicBool>,
}

impl std::fmt::Debug for SizeRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SizeRequest")
            .field("slot", &self.slot)
            .field("dir", &self.dir)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl PartialEq for SizeRequest {
    fn eq(&self, o: &SizeRequest) -> bool {
        self.slot == o.slot
            && self.generation == o.generation
            && self.name == o.name
            && Arc::ptr_eq(&self.cancel, &o.cancel)
    }
}

impl Eq for SizeRequest {}

/// Runs a [`SizeRequest`] and sends its `DirSize` (`None` when it did not complete).
pub fn run_size(req: &SizeRequest, send: &dyn Fn(crate::panel::listing::ListingMsg)) {
    let bytes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        size(&req.remote, &req.dir, &req.name, &req.cancel)
    }))
    .unwrap_or(None);
    send(crate::panel::listing::ListingMsg::DirSize {
        slot: req.slot,
        generation: req.generation,
        name: req.name.clone(),
        bytes,
    });
}

/// A source directory of a download: a directory on the server.
#[derive(Clone, Debug)]
pub struct RemoteDir {
    path: VPath,
    /// `sftp://host/dir`, for progress and the report.
    display: PathBuf,
    id: (u64, u64),
}

impl OriginDir for RemoteDir {
    fn path(&self) -> &Path {
        &self.display
    }

    fn id(&self) -> (u64, u64) {
        self.id
    }
}

/// A server as the copy engine's source (P3 2.3, 5.5).
pub struct RemoteOrigin<'x> {
    sys: &'x Sys,
    remote: &'x Arc<RemoteProvider>,
    /// The job's cancel flag, which the reply slots re-check (P3 2.5).
    cancel: Arc<AtomicBool>,
    /// Symlink targets read by the scan, by the node key.
    targets: RefCell<HashMap<u64, OsString>>,
    dirs: Cell<u64>,
}

impl<'x> RemoteOrigin<'x> {
    pub fn new(sys: &'x Sys, remote: &'x Arc<RemoteProvider>) -> RemoteOrigin<'x> {
        RemoteOrigin {
            sys,
            remote,
            cancel: sys.cancel_flag().clone(),
            targets: RefCell::new(HashMap::new()),
            dirs: Cell::new(0),
        }
    }

    fn display(&self, dir: &VPath) -> PathBuf {
        PathBuf::from(OsString::from_vec(location(self.remote.target(), dir)))
    }
}

impl Origin for RemoteOrigin<'_> {
    type Dir = RemoteDir;

    /// Every group must be a directory of this session (P3 2.2); `sub` is the absolute
    /// directory on the server. Nothing is read here: the scan `LSTAT`s the names.
    fn open_groups(
        &self,
        verb: JobVerb,
        groups: &[Group],
    ) -> Result<Opened<RemoteDir>, Box<Report>> {
        validate(groups).map_err(|why| Box::new(Report::refused(verb, why)))?;
        let ours = |g: &Group| matches!(&g.root, Root::Remote(r) if Arc::ptr_eq(r, self.remote));
        if !groups.iter().all(ours) {
            return Err(Box::new(Report::refused(verb, NOT_LOCAL)));
        }
        let mut sources = Vec::with_capacity(groups.len());
        for (i, g) in groups.iter().enumerate() {
            let path = VPath::new(g.sub.clone())
                .map_err(|_| Box::new(Report::refused(verb, "not a single path component")))?;
            let n = self.dirs.get() + 1;
            self.dirs.set(n);
            sources.push(OpenGroup {
                dir: RemoteDir {
                    display: self.display(&path),
                    path,
                    id: synthetic_id(self.remote.id(), u64::MAX - n).inode(),
                },
                names: g.names.clone(),
                group: i,
            });
        }
        Ok(Opened {
            sources,
            failed: Vec::new(),
        })
    }

    /// The scan of P3 5.5: the names `LSTAT`ed, the directories walked with up to 8
    /// listings in flight, the symlinks read. A session lost meanwhile fails what the scan
    /// did not reach with "connection lost" (I-7).
    fn scan(
        &self,
        _verb: Verb,
        sources: &[OpenGroup<RemoteDir>],
        _dst: &Dir,
        _targets: &[&[OsString]],
        rep: &mut Reporter,
    ) -> Result<Vec<Plan>, Refusal> {
        let current = self.display(&VPath::root());
        let mut tick = |t: Totals| {
            rep.progress(|| Progress {
                phase: Phase::Scanning,
                files_done: 0,
                files_total: t.entries(),
                bytes_done: 0,
                bytes_total: t.bytes,
                current: current.clone(),
            });
        };
        let mut w = Walk::new(self.remote.session(), self.remote.id(), &self.cancel);
        w.tick = Some(&mut tick);
        let mut roots = Vec::with_capacity(sources.len());
        for s in sources {
            roots.push(
                w.roots(&s.dir.path, &s.names)
                    .map_err(|_| Refusal::Cancelled)?,
            );
        }
        w.walk().map_err(|_| Refusal::Cancelled)?;
        if w.lost {
            tracing::warn!(
                session = self.remote.id(),
                "sftp: the session ended during a scan"
            );
        }
        let plans = roots
            .iter()
            .map(|r| {
                let mut totals = Totals::default();
                let roots = r.iter().map(|&i| w.node(i, &mut totals)).collect();
                Plan {
                    roots,
                    totals,
                    src_dirs: HashSet::new(),
                    links: HashMap::new(),
                }
            })
            .collect();
        *self.targets.borrow_mut() = std::mem::take(&mut w.targets);
        Ok(plans)
    }

    /// A regular file, `LSTAT`ed, opened, `FSTAT`ed and read with the pipelined window
    /// (P3 5.5). Each attempt opens it again, so Retry works.
    fn lend<T>(
        &self,
        dir: &RemoteDir,
        node: &Node,
        _cancel: &AtomicBool,
        read: &mut dyn FnMut(&mut dyn Read, u64) -> T,
    ) -> Option<Result<T, String>> {
        let declared = node.meta.size;
        // A cancelled job: the engine sees the flag before it writes anything.
        let cancelled = |read: &mut dyn FnMut(&mut dyn Read, u64) -> T| {
            Some(Ok(read(&mut std::io::empty(), declared)))
        };
        if self.sys.cancelled() {
            return cancelled(read);
        }
        let path = join(&dir.path.to_bytes(), node.name.as_bytes());
        // A server that sent no times planned none (P3 5.4): only the size is checked.
        let mtime =
            (node.meta.mtime.sec > 0).then(|| node.meta.mtime.sec.min(i64::from(u32::MAX)) as u32);
        let s = self.remote.session();
        match open_checked(s, &path, Some((declared, mtime)), &self.cancel) {
            Ok(mut r) => Some(Ok(read(&mut r, declared))),
            Err(_) if self.sys.cancelled() => cancelled(read),
            Err(OpenError::Sftp(SftpError::Lost)) => Some(Err(LOST.into())),
            Err(OpenError::Sftp(SftpError::Status {
                code: proto::status::NO_SUCH_FILE,
                ..
            })) => Some(Err(EntryError::Disappeared.to_string())),
            Err(OpenError::Sftp(e)) => Some(Err(format!("open: {e}"))),
            // R-3: a regular file became something else; it is never opened.
            Err(OpenError::NotAFile) => Some(Err(EntryError::TypeChanged.to_string())),
            Err(OpenError::Changed) => Some(Err(SOURCE_CHANGED.into())),
        }
    }

    fn open_dir(&self, dir: &RemoteDir, node: &Node) -> Result<RemoteDir, EntryError> {
        let path = dir
            .path
            .join(&node.name)
            .map_err(|_| EntryError::TypeChanged)?;
        Ok(RemoteDir {
            display: dir.display.join(&node.name),
            path,
            id: node.meta.id.inode(),
        })
    }

    /// Never reached: files are lent ([`Origin::lend`]).
    fn open(&self, _: &RemoteDir, _: &Node, _: &AtomicBool) -> Result<OriginFile, EntryError> {
        Err(EntryError::TypeChanged)
    }

    /// The target the scan read, byte-identical.
    fn read_link(&self, _dir: &RemoteDir, node: &Node) -> Result<(OsString, Meta), EntryError> {
        let t = self
            .targets
            .borrow()
            .get(&key(node))
            .cloned()
            .ok_or(EntryError::TypeChanged)?;
        Ok((t, node.meta))
    }

    /// Every remote source is kept (R-4): size and a one-second mtime cannot tell the file
    /// that was read from a replacement written in the same second.
    fn remove(&self, _: &RemoteDir, _: &OsStr, _: &Snapshot) -> Removed {
        Removed::Retained
    }

    fn remove_dir(&self, _: &RemoteDir, _: &OsStr, _: (u64, u64)) -> Removed {
        Removed::Retained
    }

    /// Whole seconds (the M1 4.5 amendment of P3 1.4).
    fn mtime_resolution(&self, _: &RemoteDir) -> i128 {
        1_000_000_000
    }
}

/// F5 out of a server (P3 5.5): the groups of one session into the local `dst`.
pub fn download(sys: &Sys, ui: &mut dyn Interaction, groups: &[Group], dst: &Path) -> Report {
    let Some(Root::Remote(r)) = groups.first().map(|g| &g.root) else {
        return Report::refused(JobVerb::Copy, NOT_LOCAL);
    };
    let o = RemoteOrigin::new(sys, r);
    copy_from(sys, ui, &o, groups, dst)
}

/// F6 out of a server (R-4, P3 5.6): the download of F5 with a move's local group commit
/// (M1 4.8): the destination batches are synced with `syncfs` as a move's are. Every
/// remote source is kept, because SFTP version 3 cannot identify the file that was read;
/// so nothing moved, and the report is a copy's, with [`REMOTE_KEPT`] as its note.
pub fn download_move(sys: &Sys, ui: &mut dyn Interaction, groups: &[Group], dst: &Path) -> Report {
    let Some(Root::Remote(r)) = groups.first().map(|g| &g.root) else {
        return Report::refused(JobVerb::Move, NOT_LOCAL);
    };
    let o = RemoteOrigin::new(sys, r);
    let (mut t, dst, parts) = match prepare(&o, sys, ui, Verb::Move, groups, dst) {
        Ok(x) => x,
        Err(r) => return *r,
    };
    t.report.verb = JobVerb::Copy;
    t.report.notes.push(REMOTE_KEPT.into());
    if !room(&mut t, &dst, &parts) {
        return t.report;
    }
    t.moving = true;
    tree_walk(&mut t, &o, &dst, parts);
    // Job end and cancel both sync the last batch.
    t.finish_move(&o);
    t.report
}
