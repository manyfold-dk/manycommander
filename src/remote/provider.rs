#![forbid(unsafe_code)]
//! A server as a place (P3 2.1, 5.4): the [`RemoteProvider`] wraps an open [`Session`] and
//! the [`Target`] it was opened for.
//!
//! **Listing.** A listing sends `OPENDIR`, then `READDIR` until `SSH_FX_EOF`, then `CLOSE`.
//! OpenSSH returns up to 100 names per reply, with attributes, so a listing needs no
//! request per entry. Each reply becomes one `Listing(Batch)` (P-27): a directory with
//! 10,000 entries arrives in 101 batches. `.` and `..` are dropped; a name with `/` or NUL,
//! or one that is not a single component, is skipped and counted
//! ([`ListingMsg::Unshown`]). `longname` is ignored. An entry without a mode is "unknown
//! type". Symlink targets are classified after `Done` by a second pass of pipelined
//! `STAT`s, as M1 3.1 does locally; the free space comes from `statvfs@openssh.com` when
//! the server has it. A listing stops at [`MAX_ENTRIES`]. `sftp://host` and `/~` resolve
//! through the `home-directory` extension, else through `REALPATH(".")`.
//!
//! **Reading.** [`open_checked`] reads a regular file for F3, F4, the quick view and a
//! download: `LSTAT` first, and only a regular file is opened (R-3); the open handle is
//! `FSTAT`ed and must still be a regular file (of the planned size, for a download); the
//! bytes come through the pipelined [`FileReader`]; and at the end another `FSTAT` must
//! still show the planned size and mtime, else the read fails with "source changed"
//! (P3 5.5). A swap between the `LSTAT` and the `OPEN` goes undetected (R-3); a swap to a
//! FIFO blocks the server, and a cancel then ends the session after the drain window
//! (P3 2.5).
//!
//! Every call here runs on a listing thread or the job worker; the UI thread only reads a
//! session's lost flag (P3 2.5).

use super::proto::{self, Attrs, Packet};
use super::session::{FileReader, Session, SftpError};
use super::url::RemoteDir;
use crate::fsops::plan::valid_component;
use crate::fsops::sys::{FsIdentity, Kind, Meta, Ts};
use crate::panel::Listing;
use crate::panel::entry::{Entry, LinkKind, NOTIME};
use crate::panel::listing::ListingMsg;
use crate::panel::sort::SortSpec;
use crate::provider::{Caps, PlaceError, Provider, Target, VPath, synthetic_id};
use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fmt;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// A listing stops at this many entries (P3 5.4).
pub const MAX_ENTRIES: usize = 1_000_000;

/// `STAT`s of the symlink pass kept outstanding (P3 5.4).
const LINK_WINDOW: usize = 64;

/// What a panel says when its session was lost (P3 5.5, 5.7).
pub const LOST_PANEL: &str = "connection lost -- Ctrl+R reconnects";

/// What a download or a view says when the file changed while it was read (P3 5.5).
pub const SOURCE_CHANGED: &str = "source changed";

/// What a remote read says when the entry is not a regular file (R-3).
pub const NOT_A_FILE: &str = "not a regular file";

const S_IFMT: u32 = 0o170_000;
const S_IFREG: u32 = 0o100_000;
const S_IFDIR: u32 = 0o040_000;

/// An open session as a place (P3 2.1): what a remote panel, a remote group and a remote
/// preview hold. The pool keeps one `Arc` of each; a session is in use while anything else
/// holds one (P3 5.7).
pub struct RemoteProvider {
    session: Session,
    target: Target,
    /// Opens of file content (`open_read`), for A-QV-6: browsing reads nothing.
    reads: AtomicU64,
}

impl fmt::Debug for RemoteProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RemoteProvider({}, {})",
            self.target.address(),
            self.session.number()
        )
    }
}

impl RemoteProvider {
    pub fn new(session: Session, target: Target) -> RemoteProvider {
        RemoteProvider {
            session,
            target,
            reads: AtomicU64::new(0),
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The server as the user typed it (P3 5.1).
    pub fn target(&self) -> &Target {
        &self.target
    }

    /// The session number: it tells sessions apart (P3 2.4) and keys synthetic identities.
    pub fn id(&self) -> u64 {
        self.session.number()
    }

    /// Why the session ended, or `None` while it is usable. Reads a flag; no I/O.
    pub fn lost(&self) -> Option<String> {
        self.session.lost()
    }

    /// How many times file content was opened through [`Provider::open_read`].
    pub fn reads(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }

    /// `sftp://[user@]host[:port]` followed by `dir`: a panel's title (escaped by the
    /// caller, as any name).
    pub fn location(&self, dir: &VPath) -> Vec<u8> {
        location(&self.target, dir)
    }
}

/// `sftp://[user@]host[:port]/dir`, the bytes as they are: a title, escaped by the caller.
pub fn location(t: &Target, dir: &VPath) -> Vec<u8> {
    let mut v = t.address().into_bytes();
    v.extend_from_slice(&dir.to_bytes());
    v
}

/// `dir/name` as the server takes it.
pub fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(dir.len() + name.len() + 1);
    v.extend_from_slice(dir);
    if !v.ends_with(b"/") {
        v.push(b'/');
    }
    v.extend_from_slice(name);
    v
}

/// The metadata of a server entry (P3 5.4): the mode with its type bits (an entry without
/// a mode is of unknown type), size, uid, and times in whole seconds; `id` is synthetic
/// (P3 2.1).
pub fn meta_of(a: &Attrs, id: FsIdentity) -> Meta {
    let kind = a.perms.map_or(Kind::Unknown, Kind::from_mode);
    let (at, mt) = a.times.unwrap_or((0, 0));
    let ts = |s: u32| Ts {
        sec: i64::from(s),
        nsec: 0,
    };
    Meta {
        kind,
        perm: a.perms.unwrap_or(0) & 0o7777,
        uid: a.uid_gid.map_or(0, |(u, _)| u),
        nlink: 1,
        size: a.size.unwrap_or(0),
        blocks: 0,
        id,
        atime: ts(at),
        mtime: ts(mt),
        ctime: ts(mt),
        mount_root: false,
        automount: false,
    }
}

/// Whether a server name can be listed (P3 5.4): a single component, without `/` or NUL,
/// that the compact entry storage can hold.
pub fn valid_name(name: &[u8]) -> bool {
    name.len() <= u16::MAX as usize && valid_component(OsStr::from_bytes(name))
}

/// The directory a remote place names, resolved on the server: the login directory for
/// `sftp://host` and `/~/...` (P3 5.4).
pub fn resolve(s: &Session, dir: &RemoteDir, cancel: &AtomicBool) -> Result<VPath, SftpError> {
    match dir {
        RemoteDir::Absolute(p) => Ok(p.clone()),
        RemoteDir::Home(rel) => {
            let home = s.home(cancel)?;
            let mut p = VPath::parse(&home).map_err(|_| {
                SftpError::Local("the server's login directory is not a usable path".into())
            })?;
            for c in rel.components() {
                p = p
                    .join(c)
                    .map_err(|_| SftpError::Local("not a usable path".into()))?;
            }
            Ok(p)
        }
    }
}

/// A remote listing for a panel (P3 5.4), run on a listing thread.
#[derive(Clone)]
pub struct ListRequest {
    pub slot: usize,
    pub generation: u64,
    pub remote: Arc<RemoteProvider>,
    /// What to list; the login directory resolves on the server.
    pub dir: RemoteDir,
    /// The panel's local directory (P3 2.2): `Done` and `Failed` carry it back.
    pub local: PathBuf,
    /// A refresh: the whole listing goes out as one sorted `Listing` (P-1). `None`: a
    /// navigation, one batch per `READDIR` reply.
    pub sort: Option<SortSpec>,
    /// Set when the panel leaves the load (`Esc`, another navigation).
    pub cancel: Arc<AtomicBool>,
}

impl fmt::Debug for ListRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ListRequest")
            .field("slot", &self.slot)
            .field("generation", &self.generation)
            .field("remote", &self.remote)
            .field("dir", &self.dir)
            .field("sort", &self.sort)
            .finish_non_exhaustive()
    }
}

impl PartialEq for ListRequest {
    fn eq(&self, o: &ListRequest) -> bool {
        self.slot == o.slot
            && self.generation == o.generation
            && self.dir == o.dir
            && Arc::ptr_eq(&self.cancel, &o.cancel)
    }
}

impl Eq for ListRequest {}

/// What one directory's read left for after `Done`.
struct Listed {
    /// `(entry index, name)` of each symlink, for the second pass.
    links: Vec<(u32, Vec<u8>)>,
    invalid: u64,
    capped: bool,
}

/// Reads directory `path` with `OPENDIR` and `READDIR`, one batch per reply (or, with
/// `sort`, one sorted listing), then closes the handle. On a cancel it stops and closes
/// the handle.
fn read_dir(
    s: &Session,
    place: u64,
    path: &[u8],
    sort: Option<SortSpec>,
    cancel: &AtomicBool,
    at: (usize, u64),
    out: &mut dyn FnMut(ListingMsg),
) -> Result<Listed, SftpError> {
    let (slot, generation) = at;
    let handle = s.opendir(path, cancel)?;
    let mut entries: Vec<Entry> = Vec::new();
    let mut names: Vec<u8> = Vec::new();
    let mut listed = Listed {
        links: Vec::new(),
        invalid: 0,
        capped: false,
    };
    let mut index: u32 = 0;
    let r = loop {
        let batch = match s.readdir(&handle, cancel) {
            Ok(Some(b)) => b,
            Ok(None) => break Ok(()),
            Err(e) => break Err(e),
        };
        for n in batch {
            let f = n.filename.as_slice();
            if f == b"." || f == b".." {
                continue;
            }
            if !valid_name(f) {
                listed.invalid += 1;
                continue;
            }
            if index as usize >= MAX_ENTRIES {
                listed.capped = true;
                break;
            }
            let meta = meta_of(&n.attrs, synthetic_id(place, u64::from(index)));
            if meta.kind == Kind::Symlink {
                listed.links.push((index, f.to_vec()));
            }
            let mut e = Entry::new(&mut names, f, &meta);
            if n.attrs.times.is_none() {
                e.flags |= NOTIME;
            }
            entries.push(e);
            index += 1;
        }
        if sort.is_none() {
            // One batch per reply (P3 5.4, P-27), also when the reply held only `.` and
            // `..`.
            out(ListingMsg::Batch {
                slot,
                generation,
                entries: std::mem::take(&mut entries),
                names: std::mem::take(&mut names),
            });
        }
        if listed.capped {
            break Ok(());
        }
    };
    // The listing thread closes its handle, also after a cancel (P3 5.4); the reply is
    // dropped by id.
    s.send_forget(cancel, |id| Packet::Close { id, handle });
    r?;
    if let Some(spec) = sort {
        out(ListingMsg::Listing {
            slot,
            generation,
            listing: Box::new(Listing::sorted(entries, names, spec)),
        });
    }
    Ok(listed)
}

/// The second pass (P3 5.4): one pipelined `STAT` per symlink, which follows it. Sends
/// `LinkTargets` in batches; a cancel drains what is outstanding.
fn classify(
    s: &Session,
    dir: &[u8],
    links: &[(u32, Vec<u8>)],
    cancel: &AtomicBool,
    at: (usize, u64),
    out: &mut dyn FnMut(ListingMsg),
) {
    let (slot, generation) = at;
    let mut todo = links.iter();
    let mut pending: VecDeque<(u32, u32)> = VecDeque::new();
    let mut kinds = Vec::new();
    loop {
        while pending.len() < LINK_WINDOW {
            let Some((i, name)) = todo.next() else {
                break;
            };
            let path = join(dir, name);
            match s.send(cancel, |id| Packet::Stat { id, path }) {
                Ok(id) => pending.push_back((id, *i)),
                Err(_) => {
                    let ids: Vec<u32> = pending.iter().map(|(id, _)| *id).collect();
                    let _ = s.drain(&ids);
                    return;
                }
            }
        }
        let Some((id, i)) = pending.pop_front() else {
            break;
        };
        let k = match s.wait(id, cancel) {
            Ok(Packet::Attrs { attrs, .. }) if attrs.kind() == Some(S_IFDIR) => LinkKind::Dir,
            Ok(Packet::Attrs { .. }) => LinkKind::File,
            Ok(_) => LinkKind::Broken,
            Err(_) => {
                let mut ids = vec![id];
                ids.extend(pending.iter().map(|(id, _)| *id));
                let _ = s.drain(&ids);
                return;
            }
        };
        kinds.push((i, k));
        if kinds.len() >= crate::panel::listing::BATCH {
            out(ListingMsg::LinkTargets {
                slot,
                generation,
                kinds: std::mem::take(&mut kinds),
            });
        }
    }
    if !kinds.is_empty() {
        out(ListingMsg::LinkTargets {
            slot,
            generation,
            kinds,
        });
    }
}

/// What a failed listing says in the panel.
fn list_error(e: &SftpError) -> (String, bool) {
    match e {
        SftpError::Status {
            code: proto::status::NO_SUCH_FILE,
            ..
        } => (e.to_string(), true),
        e => (e.to_string(), false),
    }
}

/// Lists a remote panel's directory (P3 5.4) on the calling (listing) thread: the
/// directory resolved (`Located` for the login directory), a batch per `READDIR` reply,
/// what was not shown (`Unshown`), `Done`, the free space, then the symlink pass. A cancel
/// sends nothing more. Runs under `catch_unwind`: a panic fails the load (NFR-REL).
pub fn list(req: &ListRequest, send: &dyn Fn(ListingMsg)) {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| list_inner(req, send)));
    if r.is_err() {
        send(ListingMsg::Failed {
            slot: req.slot,
            generation: req.generation,
            dir: req.local.clone(),
            error: "internal error while listing".into(),
            gone: false,
        });
    }
}

fn list_inner(req: &ListRequest, send: &dyn Fn(ListingMsg)) {
    let start = Instant::now();
    let (slot, generation) = (req.slot, req.generation);
    let s = req.remote.session();
    let cancel = &*req.cancel;
    let fail = |e: SftpError| {
        if matches!(e, SftpError::Cancelled) || cancel.load(Ordering::SeqCst) {
            return;
        }
        let (error, gone) = list_error(&e);
        send(ListingMsg::Failed {
            slot,
            generation,
            dir: req.local.clone(),
            error,
            gone,
        });
    };
    let dir = match resolve(s, &req.dir, cancel) {
        Ok(d) => d,
        Err(e) => return fail(e),
    };
    if matches!(req.dir, RemoteDir::Home(_)) {
        send(ListingMsg::Located {
            slot,
            generation,
            dir: dir.clone(),
        });
    }
    let path = dir.to_bytes();
    let mut out = |m| send(m);
    let listed = match read_dir(
        s,
        req.remote.id(),
        &path,
        req.sort,
        cancel,
        (slot, generation),
        &mut out,
    ) {
        Ok(l) => l,
        Err(e) => return fail(e),
    };
    send(ListingMsg::Unshown {
        slot,
        generation,
        invalid: listed.invalid,
        capped: listed.capped,
    });
    send(ListingMsg::Done {
        slot,
        generation,
        dir: req.local.clone(),
        elapsed: start.elapsed(),
    });
    tracing::debug!(
        session = s.number(),
        requests = s.stats().requests,
        ms = start.elapsed().as_secs_f64() * 1000.0,
        "sftp listing"
    );
    if s.caps().statvfs
        && let Ok(v) = s.statvfs(&path, cancel)
    {
        send(ListingMsg::FreeSpace {
            slot,
            generation,
            free: v.available(),
            total: v.blocks.saturating_mul(v.frsize.max(1)),
        });
    }
    classify(
        s,
        &path,
        &listed.links,
        cancel,
        (slot, generation),
        &mut out,
    );
}

/// Why a remote read could not start.
#[derive(Debug)]
pub(crate) enum OpenError {
    Sftp(SftpError),
    /// Not a regular file at the `LSTAT` or the `FSTAT` (R-3).
    NotAFile,
    /// The open file is not the planned one (P3 5.5).
    Changed,
}

impl From<SftpError> for OpenError {
    fn from(e: SftpError) -> OpenError {
        OpenError::Sftp(e)
    }
}

/// A remote file's bytes through the pipelined [`FileReader`], with the `FSTAT` check at
/// the end (P3 5.5): the file must still have the planned size and mtime, else the read
/// fails with [`SOURCE_CHANGED`]. Dropping it drains what is outstanding and closes the
/// handle.
pub struct Checked {
    inner: FileReader,
    s: Session,
    handle: Vec<u8>,
    size: u64,
    mtime: Option<u32>,
    cancel: Arc<AtomicBool>,
    done: bool,
}

impl Read for Checked {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        let n = self.inner.read(buf)?;
        if n == 0 {
            self.done = true;
            let a = self
                .s
                .fstat(&self.handle, &self.cancel)
                .map_err(io::Error::other)?;
            let mtime = a.times.map(|(_, m)| m);
            if a.size != Some(self.size) || (self.mtime.is_some() && mtime != self.mtime) {
                return Err(io::Error::other(SOURCE_CHANGED));
            }
        }
        Ok(n)
    }
}

/// Opens a remote regular file for reading (R-3, P3 5.5): `LSTAT`, and only a regular file
/// is opened; `OPEN`; `FSTAT` of the handle, which must be a regular file, and with
/// `planned` of that size. The reader's end check compares with `planned` (a download's
/// size and, when the plan has one, mtime), else with the `FSTAT` at the open.
pub(crate) fn open_checked(
    s: &Session,
    path: &[u8],
    planned: Option<(u64, Option<u32>)>,
    cancel: &Arc<AtomicBool>,
) -> Result<Checked, OpenError> {
    let a = s.lstat(path, cancel)?;
    if a.perms.map(|p| p & S_IFMT) != Some(S_IFREG) {
        return Err(OpenError::NotAFile);
    }
    let handle = s.open(path, proto::open::READ, Attrs::default(), cancel)?;
    let close = |h: Vec<u8>| s.send_forget(cancel, |id| Packet::Close { id, handle: h });
    let f = match s.fstat(&handle, cancel) {
        Ok(f) => f,
        Err(e) => {
            close(handle);
            return Err(e.into());
        }
    };
    if f.perms.map(|p| p & S_IFMT) != Some(S_IFREG) {
        close(handle);
        return Err(OpenError::NotAFile);
    }
    let (size, mtime) = match planned {
        Some((size, mtime)) => {
            if f.size != Some(size) {
                close(handle);
                return Err(OpenError::Changed);
            }
            (size, mtime)
        }
        None => (f.size.unwrap_or(0), f.times.map(|(_, m)| m)),
    };
    let inner = s.reader(handle.clone(), Some(size), cancel.clone());
    Ok(Checked {
        inner,
        s: s.clone(),
        handle,
        size,
        mtime,
        cancel: cancel.clone(),
        done: false,
    })
}

impl Provider for RemoteProvider {
    fn caps(&self) -> Caps {
        self.session.caps()
    }

    /// One directory (P3 5.4): a batch per `READDIR` reply, then `LinkTargets`. `slot` and
    /// `generation` are the caller's to fill in.
    fn list(
        &self,
        dir: &VPath,
        out: &mut dyn FnMut(ListingMsg),
        cancel: &AtomicBool,
    ) -> Result<(), PlaceError> {
        let path = dir.to_bytes();
        let listed = read_dir(&self.session, self.id(), &path, None, cancel, (0, 0), out)?;
        classify(&self.session, &path, &listed.links, cancel, (0, 0), out);
        Ok(())
    }

    /// `LSTAT`: the entry itself, never a symlink's target.
    fn lstat(&self, path: &VPath) -> Result<Meta, PlaceError> {
        let never = AtomicBool::new(false);
        let a = self.session.lstat(&path.to_bytes(), &never)?;
        Ok(meta_of(&a, synthetic_id(self.id(), 0)))
    }

    /// A regular file's bytes for F3, F4 and the quick view (P3 5.5, V-5): never a
    /// symlink, a directory or a FIFO (R-3).
    fn open_read(
        &self,
        path: &VPath,
        cancel: &Arc<AtomicBool>,
    ) -> Result<Box<dyn Read + Send>, PlaceError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        match open_checked(&self.session, &path.to_bytes(), None, cancel) {
            Ok(r) => Ok(Box::new(r)),
            Err(OpenError::Sftp(e)) => Err(e.into()),
            Err(OpenError::NotAFile) => Err(PlaceError::NotAFile),
            Err(OpenError::Changed) => Err(PlaceError::Damaged(SOURCE_CHANGED.into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_paths() {
        assert_eq!(join(b"/", b"a"), b"/a");
        assert_eq!(join(b"/x", b"a"), b"/x/a");
        for bad in [&b""[..], b".", b"..", b"a/b", b"a\0"] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert!(valid_name(b"new\nline"));
        assert!(valid_name(b"\xff\xfe"));
        let t = Target {
            user: Some("u".into()),
            host: "h".into(),
            port: Some(22),
        };
        assert_eq!(
            location(&t, &VPath::parse(b"/a/b").unwrap()),
            b"sftp://u@h:22/a/b"
        );
        assert_eq!(location(&t, &VPath::root()), b"sftp://u@h:22/");
    }

    #[test]
    fn metadata_from_attributes() {
        let a = Attrs {
            size: Some(5),
            uid_gid: Some((7, 8)),
            perms: Some(0o100_644),
            times: Some((1, 2)),
            extended: Vec::new(),
        };
        let m = meta_of(&a, FsIdentity::default());
        assert_eq!((m.kind, m.perm, m.uid, m.size), (Kind::File, 0o644, 7, 5));
        assert_eq!((m.atime.sec, m.mtime.sec), (1, 2));
        // Without a mode the type is unknown (P3 5.4).
        let m = meta_of(&Attrs::default(), FsIdentity::default());
        assert_eq!(m.kind, Kind::Unknown);
    }
}
