#![forbid(unsafe_code)]
//! Listing threads (TDR-1 3.1, section 5).
//!
//! A load opens the directory, reads its entries and `statx`es each one relative to the
//! directory fd without following a symlink or triggering an automount, and with
//! `AT_STATX_DONT_SYNC` ([`Sys::stat_at_cached`]): the mount point of a stalled network or
//! FUSE filesystem then answers from the kernel's cache instead of waiting on its server,
//! so it never blocks its parent's listing. A navigation's entries go out in batches, so rows
//! show before a 100k-entry directory is complete (P-3). A refresh keeps the rows on screen
//! until it is complete, so it sends the whole listing at once, with its collation keys
//! built and sorted for the panel's sort order: the UI thread only swaps it in (P-1).
//! Symlink targets are classified in a second pass, after `Done`, so a stuck target (a dead
//! mount) never delays the listing. Every load runs under `catch_unwind`: a panic becomes
//! `Failed`, and the UI stays up (NFR-REL).

use super::Listing;
use super::entry::{Entry, LinkKind};
use super::sort::SortSpec;
use crate::fsops::sys::{Kind, Sys};
use crate::fsops::walk::EntryError;
use rustix::fd::{AsFd, BorrowedFd};
use rustix::fs::{AtFlags, StatxFlags};
use rustix::io::Errno;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The first batch is small, so rows appear at once (P-3).
pub const FIRST_BATCH: usize = 256;
pub const BATCH: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListRequest {
    pub slot: usize,
    pub generation: u64,
    pub dir: PathBuf,
    /// When `dir` cannot be opened, list its nearest existing ancestor instead (the
    /// current directory was deleted).
    pub ancestor_fallback: bool,
    /// A refresh: the whole listing goes out as one [`ListingMsg::Listing`], sorted by this
    /// order (P-1). `None`: a navigation, in batches (P-3).
    pub sort: Option<SortSpec>,
}

#[derive(Debug)]
pub enum ListingMsg {
    Batch {
        slot: usize,
        generation: u64,
        entries: Vec<Entry>,
        names: Vec<u8>,
    },
    /// A refresh's complete listing, sorted by the request's order, before `Done` (P-1).
    Listing {
        slot: usize,
        generation: u64,
        listing: Box<Listing>,
    },
    /// The directory that was actually listed (an ancestor after a fallback).
    Done {
        slot: usize,
        generation: u64,
        dir: PathBuf,
        elapsed: Duration,
    },
    Failed {
        slot: usize,
        generation: u64,
        dir: PathBuf,
        error: String,
        /// The directory no longer exists.
        gone: bool,
    },
    /// Second pass: `(entry index, target kind)`.
    LinkTargets {
        slot: usize,
        generation: u64,
        kinds: Vec<(u32, LinkKind)>,
    },
    FreeSpace {
        slot: usize,
        generation: u64,
        free: u64,
        total: u64,
    },
    DirSize {
        slot: usize,
        generation: u64,
        name: OsString,
        bytes: Option<u64>,
    },
    /// An archive's index, as soon as its file is open and its format known (TDR-3 3.3): the
    /// panel shows the archive while the scan fills the index.
    Opened {
        slot: usize,
        generation: u64,
        index: Arc<crate::archive::ArchiveIndex>,
    },
    /// A later duplicate replaced a row of the directory a scan shows: the rows sent so
    /// far go, and every row follows again (TDR-3 3.3).
    Reset { slot: usize, generation: u64 },
    /// A refresh or `Ctrl+R` saw another `StatKey` for the archive's name (TDR-3 3.2);
    /// `rescan`: `Ctrl+R`, which reads the archive again.
    Changed {
        slot: usize,
        generation: u64,
        rescan: bool,
    },
    /// The server resolved the directory a remote load lists: the login directory of
    /// `sftp://host` or `/~/...` (TDR-3 5.4).
    Located {
        slot: usize,
        generation: u64,
        dir: crate::provider::VPath,
    },
    /// What a remote listing did not show (TDR-3 5.4), before its `Done`: names with `/` or
    /// NUL from the server, and whether it stopped at the entry or name-byte cap.
    Unshown {
        slot: usize,
        generation: u64,
        invalid: u64,
        capped: bool,
    },
}

/// Lists `req.dir`, sending messages through `send`.
pub fn list(req: &ListRequest, send: &dyn Fn(ListingMsg)) {
    let start = Instant::now();
    let sys = Sys::default();
    let (slot, generation) = (req.slot, req.generation);
    let mut dir = req.dir.clone();
    let fd = loop {
        match sys.open_root(&dir) {
            Ok(fd) => break fd,
            Err(e) if req.ancestor_fallback && dir.pop() => {
                let _ = e;
                continue;
            }
            Err(e) => {
                send(ListingMsg::Failed {
                    slot,
                    generation,
                    dir: dir.clone(),
                    error: crate::fsops::walk::errno_text(e),
                    gone: e == rustix::io::Errno::NOENT,
                });
                return;
            }
        }
    };
    let mut d = match rustix::fs::Dir::read_from(fd.as_fd()) {
        Ok(d) => d,
        Err(e) => {
            send(ListingMsg::Failed {
                slot,
                generation,
                dir,
                error: crate::fsops::walk::errno_text(e),
                gone: false,
            });
            return;
        }
    };
    let mut entries = Vec::with_capacity(FIRST_BATCH);
    let mut names = Vec::new();
    let mut links: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut index: u32 = 0;
    let mut limit = FIRST_BATCH;
    while let Some(e) = d.read() {
        let Ok(e) = e else { continue };
        let name = e.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        let meta = match sys.stat_at_cached("list.stat", fd.as_fd(), OsStr::from_bytes(name)) {
            Ok(m) => m,
            // Gone between readdir and statx: skip it; the watcher refreshes.
            Err(_) => continue,
        };
        if meta.kind == Kind::Symlink {
            links.push((index, name.to_vec()));
        }
        entries.push(Entry::new(&mut names, name, &meta));
        index += 1;
        if req.sort.is_none() && entries.len() >= limit {
            send(ListingMsg::Batch {
                slot,
                generation,
                entries: std::mem::replace(&mut entries, Vec::with_capacity(BATCH)),
                names: std::mem::take(&mut names),
            });
            limit = BATCH;
        }
    }
    match req.sort {
        Some(spec) => send(ListingMsg::Listing {
            slot,
            generation,
            listing: Box::new(Listing::sorted(entries, names, spec)),
        }),
        None if !entries.is_empty() => send(ListingMsg::Batch {
            slot,
            generation,
            entries,
            names,
        }),
        None => {}
    }
    send(ListingMsg::Done {
        slot,
        generation,
        dir: dir.clone(),
        elapsed: start.elapsed(),
    });
    if let Ok((free, total)) = sys.free_space(fd.as_fd()) {
        send(ListingMsg::FreeSpace {
            slot,
            generation,
            free,
            total,
        });
    }
    classify_links(fd.as_fd(), &links, slot, generation, send);
}

/// The second pass: follow each symlink once. A stuck target blocks only this thread.
fn classify_links(
    dir: BorrowedFd,
    links: &[(u32, Vec<u8>)],
    slot: usize,
    generation: u64,
    send: &dyn Fn(ListingMsg),
) {
    let mut kinds = Vec::with_capacity(links.len().min(BATCH));
    for (i, name) in links {
        let k = match rustix::fs::statx(
            dir,
            OsStr::from_bytes(name),
            AtFlags::empty(),
            StatxFlags::TYPE,
        ) {
            Ok(s) if Kind::from_mode(s.stx_mode as u32) == Kind::Dir => LinkKind::Dir,
            Ok(_) => LinkKind::File,
            Err(_) => LinkKind::Broken,
        };
        kinds.push((*i, k));
        if kinds.len() >= BATCH {
            send(ListingMsg::LinkTargets {
                slot,
                generation,
                kinds: std::mem::take(&mut kinds),
            });
        }
    }
    if !kinds.is_empty() {
        send(ListingMsg::LinkTargets {
            slot,
            generation,
            kinds,
        });
    }
}

/// Runs `lister` under `catch_unwind`; a panic sends `Failed`.
pub fn guarded(
    req: &ListRequest,
    send: &dyn Fn(ListingMsg),
    lister: impl FnOnce(&ListRequest, &dyn Fn(ListingMsg)),
) {
    if catch_unwind(AssertUnwindSafe(|| lister(req, send))).is_err() {
        send(ListingMsg::Failed {
            slot: req.slot,
            generation: req.generation,
            dir: req.dir.clone(),
            error: "internal error while listing".into(),
            gone: false,
        });
    }
}

/// A listing thread's liveness, for the abandoned-thread limit (TDR-1 3.1).
#[derive(Clone, Default, Debug)]
pub struct Alive(Arc<AtomicBool>);

impl PartialEq for Alive {
    fn eq(&self, o: &Alive) -> bool {
        Arc::ptr_eq(&self.0, &o.0)
    }
}

impl Eq for Alive {}

impl Alive {
    pub fn running() -> Alive {
        Alive(Arc::new(AtomicBool::new(true)))
    }

    pub fn is_running(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub fn finish(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Spawns a listing thread named `list-<slot>`; `alive` turns false when it returns. When
/// the thread cannot start, `alive` turns false at once and the caller says why (a `Failed`
/// for the panel), so nothing waits for a listing that never comes.
pub fn spawn(
    req: ListRequest,
    alive: Alive,
    send: impl Fn(ListingMsg) + Send + 'static,
) -> std::io::Result<()> {
    let a = alive.clone();
    let r = std::thread::Builder::new()
        .name(format!("list-{}", req.slot))
        .spawn(move || {
            guarded(&req, &send, list);
            a.finish();
        });
    if r.is_err() {
        alive.finish();
    }
    r.map(drop)
}

/// `Space` on a local directory: the walk that sizes it, on a listing thread. `Esc` and the
/// next `Space` stop it through `cancel`.
#[derive(Clone)]
pub struct SizeRequest {
    pub slot: usize,
    pub generation: u64,
    pub dir: PathBuf,
    pub name: OsString,
    pub cancel: Arc<AtomicBool>,
}

impl std::fmt::Debug for SizeRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SizeRequest")
            .field("slot", &self.slot)
            .field("generation", &self.generation)
            .field("dir", &self.dir)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl PartialEq for SizeRequest {
    fn eq(&self, o: &SizeRequest) -> bool {
        self.slot == o.slot
            && self.generation == o.generation
            && self.dir == o.dir
            && self.name == o.name
            && Arc::ptr_eq(&self.cancel, &o.cancel)
    }
}

impl Eq for SizeRequest {}

/// What a size walk counted: the bytes of the regular files it could read, and the
/// directories below that it could not open or read, which the total leaves out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SizeTotal {
    pub bytes: u64,
    pub unread: u64,
}

/// Sums the sizes of regular files below `dir/name` (Space on a directory). Symlinks are
/// not followed, mount points are not crossed; `cancel` stops it (`ECANCELED`). A
/// directory below that cannot be read is counted in [`SizeTotal::unread`], and the walk
/// goes on; an error is only what keeps the entry itself from being read. A results tab's
/// `name` is a path relative to its root (TDR-2 2.4): it is walked one component at a time
/// with `O_NOFOLLOW` (TDR-2 2.2).
pub fn dir_size(
    dir: &std::path::Path,
    name: &OsStr,
    cancel: &AtomicBool,
) -> Result<SizeTotal, Errno> {
    let sys = Sys::default();
    let mut root = sys.open_root(dir)?;
    let (sub, name) = super::split_rel(name.as_bytes());
    if !sub.is_empty() {
        for c in sub.split(|&c| c == b'/') {
            let (fd, _) = crate::fsops::walk::open_dir_nofollow(
                &sys,
                "size.walk",
                root.as_fd(),
                OsStr::from_bytes(c),
            )
            .map_err(|e| match e {
                EntryError::TypeChanged => Errno::NOTDIR,
                EntryError::Disappeared => Errno::NOENT,
                EntryError::Os { errno, .. } => errno,
            })?;
            root = fd;
        }
    }
    let name = OsStr::from_bytes(name);
    let meta = sys.stat_at("size.stat", root.as_fd(), name)?;
    if meta.kind != Kind::Dir {
        return Ok(SizeTotal {
            bytes: meta.size,
            unread: 0,
        });
    }
    fn walk(
        sys: &Sys,
        parent: BorrowedFd,
        name: &OsStr,
        mnt: u64,
        cancel: &AtomicBool,
        total: &mut SizeTotal,
    ) -> Result<(), Errno> {
        if cancel.load(Ordering::SeqCst) {
            return Err(Errno::CANCELED);
        }
        let fd = sys.open_dir("size.open", parent, name)?;
        for (n, _) in sys.read_dir("size.readdir", fd.as_fd())? {
            let Ok(m) = sys.stat_at("size.stat", fd.as_fd(), &n) else {
                continue;
            };
            match m.kind {
                Kind::File => total.bytes += m.size,
                Kind::Dir if m.id.mnt_id == mnt => {
                    match walk(sys, fd.as_fd(), &n, mnt, cancel, total) {
                        Err(Errno::CANCELED) => return Err(Errno::CANCELED),
                        Err(_) => total.unread += 1,
                        Ok(()) => {}
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    let mut total = SizeTotal::default();
    walk(&sys, root.as_fd(), name, meta.id.mnt_id, cancel, &mut total)?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn scratch(name: &str) -> PathBuf {
        let d = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/target/test-tmp"))
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn open_up(d: &Path) {
        let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o755));
    }

    /// An unreadable directory below does not cost the whole total: the walk counts what
    /// it can read and how many directories it could not. Cancelling stops it.
    #[test]
    fn a_size_counts_what_it_can_read() {
        let root = scratch("size-unreadable");
        let top = root.join("top");
        std::fs::create_dir_all(top.join("open/deeper")).unwrap();
        std::fs::create_dir_all(top.join("closed")).unwrap();
        std::fs::write(top.join("a"), [0u8; 100]).unwrap();
        std::fs::write(top.join("open/deeper/b"), [0u8; 20]).unwrap();
        std::fs::write(top.join("closed/c"), [0u8; 3]).unwrap();
        std::fs::set_permissions(top.join("closed"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        let never = AtomicBool::new(false);
        let total = dir_size(&root, OsStr::new("top"), &never);
        // Root reads a directory without permission: then nothing is left out.
        let privileged = std::fs::read_dir(top.join("closed")).is_ok();
        open_up(&top.join("closed"));
        let total = total.unwrap();
        if privileged {
            assert_eq!(
                total,
                SizeTotal {
                    bytes: 123,
                    unread: 0
                }
            );
        } else {
            assert_eq!(
                total,
                SizeTotal {
                    bytes: 120,
                    unread: 1
                }
            );
        }
        // A results tab's relative name, and a file.
        let t = dir_size(&root, OsStr::new("top/open"), &never).unwrap();
        assert_eq!(
            t,
            SizeTotal {
                bytes: 20,
                unread: 0
            }
        );
        let t = dir_size(&root, OsStr::new("top/a"), &never).unwrap();
        assert_eq!(t.bytes, 100);
        assert_eq!(
            dir_size(&root, OsStr::new("gone"), &never),
            Err(Errno::NOENT)
        );
        let stop = AtomicBool::new(true);
        assert_eq!(
            dir_size(&root, OsStr::new("top"), &stop),
            Err(Errno::CANCELED)
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
