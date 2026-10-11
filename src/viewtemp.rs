#![forbid(unsafe_code)]
//! The runtime view directory and the view copy (TDR-3 3.4): what F3, F4 and `Enter` hand to
//! `$PAGER` or `$EDITOR` for a file that is not on a local disk, an archive member now and a
//! remote file with SFTP (TDR-3 5.5).
//!
//! A copy goes to `$XDG_RUNTIME_DIR/manycommander/view/<random>/<name>`. `manycommander/`
//! and `view/` are created with mode `0700`, opened with `O_DIRECTORY | O_NOFOLLOW` and
//! checked to belong to the user: a symlink or a foreign owner refuses the view. Without
//! `XDG_RUNTIME_DIR` the tree goes into a private directory made the way `mkdtemp(3)` makes
//! one (a random name, `mkdir` with `0700`, a new name on `EEXIST`) in the system temporary
//! directory, never under a predictable name another user could create first. That private
//! directory goes on exit unless it holds an edited copy.
//!
//! The copy ([`prepare`]) runs on a listing thread: it reads the place's `open_read`, stops
//! at the first byte past the declared size (A-4), writes a new file of mode `0600` in the
//! new, empty `<random>` directory, and records the file's identity, size, mtime and ctime.
//! After the hand-off, [`check`] compares the name with the record: another inode at the
//! name (an editor that renames a new file over it) or another size, mtime or ctime is an
//! edit, and the copy is kept; otherwise the directory is removed.

use crate::fsops::origin::SIZE_MISMATCH;
use crate::fsops::sys::{Meta, Sys, Ts, euid, random_u64};
use crate::fsops::walk::errno_text;
use crate::provider::{PlaceError, Provider, VPath};
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use rustix::io::Errno;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A copy larger than this asks first (TDR-3 3.4).
pub const ASK_ABOVE: u64 = 256 << 20;

/// What the status line says when an edited copy of an archive member is kept (TDR-3 3.4).
pub fn kept_text(path: &Path) -> String {
    format!(
        "archives are read-only; your edited copy is at {}",
        crate::ui::text::escaped(path.as_os_str().as_bytes())
    )
}

/// What it says for an edited copy of a remote file that is not uploaded: the answer to
/// the write-back question was to keep it, or the server is gone (TDR-3 5.6).
pub fn kept_remote_text(path: &Path) -> String {
    format!(
        "not uploaded to the server; your edited copy is at {}",
        crate::ui::text::escaped(path.as_os_str().as_bytes())
    )
}

/// The view tree of this process: `.../manycommander/view`, held open, and the private
/// directory that holds it when there is no `XDG_RUNTIME_DIR`. Every copy goes below the
/// held fd, so the checked path is never resolved again.
#[derive(Debug)]
pub struct ViewRoot {
    view: PathBuf,
    fd: OwnedFd,
    private: Option<PathBuf>,
}

/// Creates (or opens) `parent/name` as a directory of the user's own: `mkdirat` with
/// `0700`, then `O_DIRECTORY | O_NOFOLLOW`, then the owner; group and other bits are
/// cleared. A symlink or another owner refuses it.
fn own_dir(sys: &Sys, parent: BorrowedFd, name: &str, shown: &Path) -> Result<OwnedFd, String> {
    let name = OsStr::new(name);
    let refuse = |why: &str| format!("{}: {why}; the view is refused", shown.join(name).display());
    match sys.mkdir("view.mkdir", parent, name, 0o700) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(e) => return Err(refuse(&errno_text(e))),
    }
    let fd = match sys.open_dir("view.open", parent, name) {
        Ok(fd) => fd,
        Err(Errno::LOOP | Errno::NOTDIR) => return Err(refuse("a symlink or not a directory")),
        Err(e) => return Err(refuse(&errno_text(e))),
    };
    let m = sys
        .stat_fd(fd.as_fd())
        .map_err(|e| refuse(&errno_text(e)))?;
    if m.uid != euid() {
        return Err(refuse("owned by another user"));
    }
    if m.perm & 0o7777 != 0o700 {
        sys.fchmod("view.chmod", fd.as_fd(), 0o700)
            .map_err(|e| refuse(&errno_text(e)))?;
    }
    Ok(fd)
}

impl ViewRoot {
    /// The view tree under `runtime` (`XDG_RUNTIME_DIR`), or, without it, under a new
    /// private directory in `tmp` (TDR-3 3.4).
    pub fn create(runtime: Option<&Path>, tmp: &Path) -> Result<ViewRoot, String> {
        let sys = Sys::default();
        let (base, fd, private) = match runtime.filter(|r| r.is_absolute()) {
            Some(r) => {
                let fd = sys
                    .open_root(r)
                    .map_err(|e| format!("{}: {}", r.display(), errno_text(e)))?;
                (r.to_path_buf(), fd, None)
            }
            None => {
                let tfd = sys
                    .open_root(tmp)
                    .map_err(|e| format!("{}: {}", tmp.display(), errno_text(e)))?;
                let name = loop {
                    let name = format!("manycommander-{:016x}", random_u64());
                    match sys.mkdir("view.mkdtemp", tfd.as_fd(), OsStr::new(&name), 0o700) {
                        Ok(()) => break name,
                        Err(Errno::EXIST) => continue,
                        Err(e) => return Err(format!("{}: {}", tmp.display(), errno_text(e))),
                    }
                };
                let fd = own_dir(&sys, tfd.as_fd(), &name, tmp)?;
                let private = tmp.join(&name);
                (private.clone(), fd, Some(private))
            }
        };
        let mc = own_dir(&sys, fd.as_fd(), "manycommander", &base)?;
        let base = base.join("manycommander");
        let fd = own_dir(&sys, mc.as_fd(), "view", &base)?;
        Ok(ViewRoot {
            view: base.join("view"),
            fd,
            private,
        })
    }

    /// `.../manycommander/view`.
    pub fn path(&self) -> &Path {
        &self.view
    }

    /// The private directory, when there is no `XDG_RUNTIME_DIR`.
    pub fn private(&self) -> Option<&Path> {
        self.private.as_deref()
    }

    /// On exit: removes the private directory when nothing was kept in it. Only empty
    /// directories go (`rmdir`), so a kept edited copy stays with its path.
    pub fn remove_private(&self) {
        let Some(p) = &self.private else {
            return;
        };
        if let Ok(rd) = std::fs::read_dir(&self.view) {
            for e in rd.flatten() {
                let _ = std::fs::remove_dir(e.path());
            }
        }
        for d in [self.view.clone(), p.join("manycommander"), p.to_path_buf()] {
            if std::fs::remove_dir(&d).is_err() {
                return;
            }
        }
    }
}

/// The view roots of a running app: made on the first view, shared by the view threads.
/// The lock guards only making the root and handing it out: a thread that copies into the
/// root holds its own reference, so a slow download blocks neither another view thread
/// nor the exit.
#[derive(Clone, Debug, Default)]
pub struct Roots {
    root: Arc<Mutex<Option<Arc<ViewRoot>>>>,
    /// `XDG_RUNTIME_DIR` and the system temporary directory, read at startup.
    runtime: Option<PathBuf>,
    tmp: PathBuf,
}

impl Roots {
    pub fn new(runtime: Option<PathBuf>, tmp: PathBuf) -> Roots {
        Roots {
            root: Arc::default(),
            runtime: runtime.filter(|p| !p.as_os_str().is_empty()),
            tmp,
        }
    }

    /// From the environment: `XDG_RUNTIME_DIR` and `TMPDIR` (or `/tmp`).
    pub fn from_env() -> Roots {
        Roots::new(
            std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
            std::env::temp_dir(),
        )
    }

    /// Runs `f` with the view root, making it first. The lock is released before `f` runs.
    fn with<T>(&self, f: impl FnOnce(&ViewRoot) -> T) -> Result<T, String> {
        let root = {
            let mut g = self.root.lock().unwrap_or_else(|e| e.into_inner());
            match g.as_ref() {
                Some(r) => r.clone(),
                None => {
                    let r = Arc::new(ViewRoot::create(self.runtime.as_deref(), &self.tmp)?);
                    *g = Some(r.clone());
                    r
                }
            }
        };
        Ok(f(&root))
    }

    /// On exit (see [`ViewRoot::remove_private`]). A copy still in progress keeps its
    /// directory, so the private directory stays with it.
    pub fn finish(&self) {
        let root = self.root.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(r) = root {
            r.remove_private();
        }
    }
}

/// A view copy to prepare (TDR-3 3.4).
#[derive(Clone)]
pub struct ViewRequest {
    pub id: u64,
    pub place: Arc<dyn Provider>,
    pub path: VPath,
    /// The name the copy gets: a single component.
    pub name: OsString,
    /// The size the place declares.
    pub size: u64,
    pub cancel: Arc<AtomicBool>,
    /// A remote file (TDR-3 5.5), not an archive member: an edited copy says so.
    pub remote: bool,
}

impl std::fmt::Debug for ViewRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewRequest")
            .field("id", &self.id)
            .field("path", &self.path)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl PartialEq for ViewRequest {
    fn eq(&self, o: &ViewRequest) -> bool {
        self.id == o.id && self.path == o.path && Arc::ptr_eq(&self.cancel, &o.cancel)
    }
}

impl Eq for ViewRequest {}

/// A prepared view copy and what it was when written (TDR-3 3.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewFile {
    /// The `<random>` directory.
    pub dir: PathBuf,
    /// Its name in the view directory.
    dname: OsString,
    pub name: OsString,
    pub id: (u64, u64),
    pub size: u64,
    pub mtime: Ts,
    pub ctime: Ts,
    /// A copy of a remote file (TDR-3 5.5).
    pub remote: bool,
    /// The remote file's size and mtime (whole seconds) just before the download: the
    /// write-back question offers "save as" when they changed (TDR-3 5.6). A change during
    /// the download counts too.
    pub stamp: Option<(u64, i64)>,
}

impl ViewFile {
    pub fn path(&self) -> PathBuf {
        self.dir.join(&self.name)
    }

    fn same(&self, m: &Meta) -> bool {
        m.id.inode() == self.id
            && m.size == self.size
            && m.mtime == self.mtime
            && m.ctime == self.ctime
    }
}

/// What a view thread tells the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewMsg {
    Progress {
        id: u64,
        done: u64,
        total: u64,
    },
    Ready {
        id: u64,
        file: ViewFile,
    },
    Failed {
        id: u64,
        error: String,
    },
    /// After the hand-off: `kept` is an edited copy's path; `remote`: of a remote file.
    Checked {
        kept: Option<PathBuf>,
        error: Option<String>,
        remote: bool,
    },
    /// After the hand-off of a remote file's copy that was edited (TDR-3 5.6): the copy, the
    /// server file (a `Dest::Remote`), and whether that file's size or mtime changed since
    /// the download.
    Edited {
        copy: PathBuf,
        at: crate::fsops::job::Dest,
        changed: bool,
    },
}

/// Removes a `<random>` directory and what the copy left in it.
fn discard(sys: &Sys, view: BorrowedFd, dir: &OsStr, dfd: BorrowedFd, name: &OsStr) {
    let _ = sys.unlink("view.unlink", dfd, name);
    let _ = sys.rmdir("view.rmdir", view, dir);
}

/// Prepares a view copy (TDR-3 3.4) on the calling (listing) thread: a new `<random>`
/// directory of mode `0700` under the view root, the file `name` in it created `O_EXCL` with
/// mode `0600`, the place's bytes up to the declared size (a longer or shorter stream is
/// "size mismatch"), and the record. `progress` gets `(done, total)` at most every 100 ms.
/// A failure or a cancel removes what it made.
pub fn prepare(
    roots: &Roots,
    req: &ViewRequest,
    progress: &dyn Fn(u64, u64),
) -> Result<ViewFile, String> {
    roots.with(|root| prepare_in(root, req, progress))?
}

fn prepare_in(
    root: &ViewRoot,
    req: &ViewRequest,
    progress: &dyn Fn(u64, u64),
) -> Result<ViewFile, String> {
    let sys = Sys::new(req.cancel.clone());
    let view = root.fd.as_fd();
    let dname = loop {
        let n = OsString::from(format!("{:016x}", random_u64()));
        match sys.mkdir("view.mkdir", view, &n, 0o700) {
            Ok(()) => break n,
            Err(Errno::EXIST) => continue,
            Err(e) => return Err(format!("{}: {}", root.path().display(), errno_text(e))),
        }
    };
    let made = || root.path().join(&dname);
    let dfd = match sys.open_dir("view.open", view, &dname) {
        Ok(fd) => fd,
        Err(e) => {
            let _ = sys.rmdir("view.rmdir", view, &dname);
            return Err(format!("{}: {}", made().display(), errno_text(e)));
        }
    };
    // What the server file was before it was read, for the write-back question (TDR-3 5.6).
    // Taken after the read, it would hide a change made while the bytes were coming.
    let stamp = match req.remote {
        true => req
            .place
            .lstat(&req.path)
            .ok()
            .map(|m| (m.size, m.mtime.sec)),
        false => None,
    };
    let r = copy_into(&sys, req, dfd.as_fd(), progress);
    match r {
        Ok(file) => Ok(ViewFile {
            dir: made(),
            dname: dname.clone(),
            name: req.name.clone(),
            id: file.id.inode(),
            size: file.size,
            mtime: file.mtime,
            ctime: file.ctime,
            remote: req.remote,
            stamp,
        }),
        Err(e) => {
            discard(&sys, view, &dname, dfd.as_fd(), &req.name);
            Err(e)
        }
    }
}

/// The copy itself: `O_EXCL`, `0600`, the declared size exactly (A-4).
fn copy_into(
    sys: &Sys,
    req: &ViewRequest,
    dir: BorrowedFd,
    progress: &dyn Fn(u64, u64),
) -> Result<Meta, String> {
    let fout = sys
        .create_excl("view.create", dir, &req.name, 0o600)
        .map_err(|e| format!("create the view copy: {}", errno_text(e)))?;
    // `0600` whatever the umask, so an editor that writes in place can write it.
    sys.fchmod("view.chmod", fout.as_fd(), 0o600)
        .map_err(|e| format!("set permissions: {}", errno_text(e)))?;
    let mut reader = req
        .place
        .open_read(&req.path, &req.cancel)
        .map_err(|e| match e {
            PlaceError::Cancelled => "cancelled".to_string(),
            e => e.to_string(),
        })?;
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0u64;
    let mut last = Instant::now();
    progress(0, req.size);
    loop {
        if req.cancel.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        let room = req.size - done;
        let want = room.saturating_add(1).min(buf.len() as u64) as usize;
        let n = loop {
            match reader.read(&mut buf[..want]) {
                Ok(n) => break n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.to_string()),
            }
        };
        if n == 0 {
            if done == req.size {
                break;
            }
            return Err(SIZE_MISMATCH.into());
        }
        if n as u64 > room {
            return Err(SIZE_MISMATCH.into());
        }
        sys.write_all("view.write", fout.as_fd(), &buf[..n])
            .map_err(|e| format!("write: {}", errno_text(e)))?;
        done += n as u64;
        if last.elapsed() >= Duration::from_millis(100) {
            progress(done, req.size);
            last = Instant::now();
        }
    }
    drop(reader);
    sys.stat_fd(fout.as_fd())
        .map_err(|e| format!("stat the view copy: {}", errno_text(e)))
}

/// After the hand-off (TDR-3 3.4): the name in the view directory against the record. Another
/// inode at the name, or another size, mtime or ctime, is an edit: the copy is kept and
/// its path returned. An unchanged copy is removed with its directory; a directory the
/// program left files in stays.
pub fn check(roots: &Roots, file: &ViewFile) -> Result<Option<PathBuf>, String> {
    roots.with(|root| check_in(root, file))?
}

fn check_in(root: &ViewRoot, file: &ViewFile) -> Result<Option<PathBuf>, String> {
    let sys = Sys::default();
    let view = root.fd.as_fd();
    let dir = match sys.open_dir("view.open", view, &file.dname) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(None),
        Err(e) => return Err(format!("{}: {}", file.dir.display(), errno_text(e))),
    };
    match sys.stat_at("view.check", dir.as_fd(), &file.name) {
        Ok(m) if !file.same(&m) => return Ok(Some(file.path())),
        Ok(_) | Err(Errno::NOENT) => {}
        Err(e) => return Err(format!("{}: {}", file.path().display(), errno_text(e))),
    }
    discard(&sys, view, &file.dname, dir.as_fd(), &file.name);
    Ok(None)
}

/// After the hand-off of a remote file's copy (TDR-3 5.6), on a helper thread: [`check`], and
/// for an edited copy an `LSTAT` of the server file `at` (a `Dest::Remote`), compared with
/// the stamp taken before the download; a stamp that could not be taken counts as a
/// change. A copy that was not edited is removed. When the server file cannot be reached
/// (a lost session), the copy is kept and reported as not uploaded.
pub fn check_edited(roots: &Roots, file: &ViewFile, at: crate::fsops::job::Dest) -> ViewMsg {
    let kept = match check(roots, file) {
        Ok(Some(copy)) => copy,
        Ok(None) => {
            return ViewMsg::Checked {
                kept: None,
                error: None,
                remote: true,
            };
        }
        Err(e) => {
            return ViewMsg::Checked {
                kept: None,
                error: Some(e),
                remote: true,
            };
        }
    };
    let crate::fsops::job::Dest::Remote { session, dir } = &at else {
        return ViewMsg::Checked {
            kept: Some(kept),
            error: None,
            remote: true,
        };
    };
    let now = match session.lstat(dir) {
        Ok(m) => Some((m.size, m.mtime.sec)),
        Err(PlaceError::NotFound) => None,
        Err(_) => {
            return ViewMsg::Checked {
                kept: Some(kept),
                error: None,
                remote: true,
            };
        }
    };
    ViewMsg::Edited {
        copy: kept,
        changed: now.is_none() || now != file.stamp,
        at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsops::sys::Kind;
    use crate::panel::listing::ListingMsg;
    use crate::provider::Caps;
    use std::sync::atomic::AtomicI64;
    use std::sync::mpsc::{Receiver, Sender, channel};

    fn runtime(name: &str) -> PathBuf {
        let p = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/target/test-tmp"))
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn file_meta(size: u64, sec: i64) -> Meta {
        Meta {
            kind: Kind::File,
            size,
            mtime: Ts { sec, nsec: 0 },
            ..Meta::default()
        }
    }

    fn request(place: Arc<dyn Provider>, size: u64) -> ViewRequest {
        ViewRequest {
            id: 1,
            place,
            path: VPath::parse(b"dir/f").unwrap(),
            name: OsString::from("f"),
            size,
            cancel: Arc::new(AtomicBool::new(false)),
            remote: true,
        }
    }

    /// A server file of four bytes whose mtime moves from 100 to 200 once its last byte has
    /// been read: a same-size change during the download.
    struct Changing {
        mtime: Arc<AtomicI64>,
    }

    struct ChangingReader {
        left: &'static [u8],
        mtime: Arc<AtomicI64>,
    }

    impl Read for ChangingReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.left.len().min(buf.len());
            buf[..n].copy_from_slice(&self.left[..n]);
            self.left = &self.left[n..];
            if self.left.is_empty() {
                self.mtime.store(200, Ordering::SeqCst);
            }
            Ok(n)
        }
    }

    impl Provider for Changing {
        fn caps(&self) -> Caps {
            Caps::default()
        }
        fn list(
            &self,
            _: &VPath,
            _: &mut dyn FnMut(ListingMsg),
            _: &AtomicBool,
        ) -> Result<(), PlaceError> {
            Ok(())
        }
        fn lstat(&self, _: &VPath) -> Result<Meta, PlaceError> {
            Ok(file_meta(4, self.mtime.load(Ordering::SeqCst)))
        }
        fn open_read(
            &self,
            _: &VPath,
            _: &Arc<AtomicBool>,
        ) -> Result<Box<dyn Read + Send>, PlaceError> {
            Ok(Box::new(ChangingReader {
                left: b"abcd",
                mtime: self.mtime.clone(),
            }))
        }
    }

    /// The stamp is the server file before the download, so a change while the bytes came
    /// makes the write-back question report it.
    #[test]
    fn the_remote_stamp_is_taken_before_the_download() {
        let rt = runtime("viewtemp-stamp");
        let roots = Roots::new(Some(rt.clone()), rt.clone());
        let mtime = Arc::new(AtomicI64::new(100));
        let place = Arc::new(Changing {
            mtime: mtime.clone(),
        });
        let file = prepare(&roots, &request(place, 4), &|_, _| {}).unwrap();
        assert_eq!(mtime.load(Ordering::SeqCst), 200, "the file changed");
        assert_eq!(file.stamp, Some((4, 100)));
        assert_eq!(std::fs::read(file.path()).unwrap(), b"abcd");
        assert_eq!(check(&roots, &file), Ok(None));
        roots.finish();
        let _ = std::fs::remove_dir_all(&rt);
    }

    /// A server file whose download stops until the test lets it go on.
    struct Stalled {
        started: Mutex<Sender<()>>,
        go: Mutex<Option<Receiver<()>>>,
    }

    struct StalledReader {
        go: Receiver<()>,
        sent: bool,
    }

    impl Read for StalledReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.sent {
                return Ok(0);
            }
            let _ = self.go.recv_timeout(Duration::from_secs(30));
            self.sent = true;
            buf[0] = b'x';
            Ok(1)
        }
    }

    impl Provider for Stalled {
        fn caps(&self) -> Caps {
            Caps::default()
        }
        fn list(
            &self,
            _: &VPath,
            _: &mut dyn FnMut(ListingMsg),
            _: &AtomicBool,
        ) -> Result<(), PlaceError> {
            Ok(())
        }
        fn lstat(&self, _: &VPath) -> Result<Meta, PlaceError> {
            Ok(file_meta(1, 100))
        }
        fn open_read(
            &self,
            _: &VPath,
            _: &Arc<AtomicBool>,
        ) -> Result<Box<dyn Read + Send>, PlaceError> {
            let go = self.go.lock().unwrap().take().expect("one download");
            let _ = self.started.lock().unwrap().send(());
            Ok(Box::new(StalledReader { go, sent: false }))
        }
    }

    /// A download in progress holds no lock: checking another copy and the exit's cleanup
    /// finish while it waits for the server.
    #[test]
    fn a_slow_download_blocks_neither_a_check_nor_the_exit() {
        let rt = runtime("viewtemp-slow");
        let roots = Roots::new(Some(rt.clone()), rt.clone());
        let (started_tx, started) = channel();
        let (go, go_rx) = channel();
        let place = Arc::new(Stalled {
            started: Mutex::new(started_tx),
            go: Mutex::new(Some(go_rx)),
        });
        let req = request(place, 1);
        let r2 = roots.clone();
        let download = std::thread::spawn(move || prepare(&r2, &req, &|_, _| {}));
        started
            .recv_timeout(Duration::from_secs(10))
            .expect("the download started");

        let (done_tx, done) = channel();
        let r3 = roots.clone();
        std::thread::spawn(move || {
            let other = ViewFile {
                dir: PathBuf::from("/nonexistent"),
                dname: OsString::from("0000000000000000"),
                name: OsString::from("g"),
                id: (0, 0),
                size: 0,
                mtime: Ts::default(),
                ctime: Ts::default(),
                remote: false,
                stamp: None,
            };
            let checked = check(&r3, &other);
            r3.finish();
            let _ = done_tx.send(checked);
        });
        let checked = done.recv_timeout(Duration::from_secs(5));
        let _ = go.send(());
        let file = download.join().unwrap();
        assert_eq!(
            checked,
            Ok(Ok(None)),
            "the check and the exit waited for the download"
        );
        let file = file.unwrap();
        assert_eq!(std::fs::read(file.path()).unwrap(), b"x");
        let _ = std::fs::remove_dir_all(&rt);
    }
}
