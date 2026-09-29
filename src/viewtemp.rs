#![forbid(unsafe_code)]
//! The runtime view directory and the view copy (P3 3.4): what F3, F4 and `Enter` hand to
//! `$PAGER` or `$EDITOR` for a file that is not on a local disk, an archive member now and a
//! remote file with SFTP (P3 5.5).
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
use crate::fsops::sys::{Meta, Sys, Ts, random_u64, uid};
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

/// A copy larger than this asks first (P3 3.4).
pub const ASK_ABOVE: u64 = 256 << 20;

/// What the status line says when an edited copy of an archive member is kept (P3 3.4).
pub fn kept_text(path: &Path) -> String {
    format!(
        "archives are read-only; your edited copy is at {}",
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
    if m.uid != uid() {
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
    /// private directory in `tmp` (P3 3.4).
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
#[derive(Clone, Debug, Default)]
pub struct Roots {
    root: Arc<Mutex<Option<ViewRoot>>>,
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

    /// Runs `f` with the view root, making it first.
    fn with<T>(&self, f: impl FnOnce(&ViewRoot) -> T) -> Result<T, String> {
        let mut g = self.root.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            *g = Some(ViewRoot::create(self.runtime.as_deref(), &self.tmp)?);
        }
        Ok(f(g.as_ref().expect("made above")))
    }

    /// On exit (see [`ViewRoot::remove_private`]).
    pub fn finish(&self) {
        if let Some(r) = self.root.lock().unwrap_or_else(|e| e.into_inner()).take() {
            r.remove_private();
        }
    }
}

/// A view copy to prepare (P3 3.4).
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

/// A prepared view copy and what it was when written (P3 3.4).
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
    /// After the hand-off: `kept` is an edited copy's path.
    Checked {
        kept: Option<PathBuf>,
        error: Option<String>,
    },
}

/// Removes a `<random>` directory and what the copy left in it.
fn discard(sys: &Sys, view: BorrowedFd, dir: &OsStr, dfd: BorrowedFd, name: &OsStr) {
    let _ = sys.unlink("view.unlink", dfd, name);
    let _ = sys.rmdir("view.rmdir", view, dir);
}

/// Prepares a view copy (P3 3.4) on the calling (listing) thread: a new `<random>`
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

/// After the hand-off (P3 3.4): the name in the view directory against the record. Another
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
