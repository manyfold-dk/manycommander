#![forbid(unsafe_code)]
//! Listing threads (design 3.1, section 5).
//!
//! A load opens the directory, reads its entries and `statx`es each one relative to the
//! directory fd (`AT_SYMLINK_NOFOLLOW`). A navigation's entries go out in batches, so rows
//! show before a 100k-entry directory is complete (P-3). A refresh keeps the rows on screen
//! until it is complete, so it sends the whole listing at once, with its collation keys
//! built and sorted for the panel's sort order: the UI thread only swaps it in (P-1).
//! Symlink targets are classified in a second pass, after `Done`, so a stuck target (a dead
//! mount) never delays the listing. Every load runs under `catch_unwind`: a panic becomes
//! `Failed`, and the UI stays up (NFR-REL).

use super::Listing;
use super::entry::{EKind, Entry, LinkKind};
use super::sort::SortSpec;
use crate::fsops::sys::{Kind, Sys};
use rustix::fd::{AsFd, BorrowedFd};
use rustix::fs::{AtFlags, StatxFlags};
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
        let meta = match sys.stat_at("list.stat", fd.as_fd(), OsStr::from_bytes(name)) {
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

/// A listing thread's liveness, for the abandoned-thread limit (design 3.1).
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

/// Spawns a listing thread named `list-<slot>`; `alive` turns false when it returns.
pub fn spawn(req: ListRequest, alive: Alive, send: impl Fn(ListingMsg) + Send + 'static) -> Alive {
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
    alive
}

/// Sums the sizes of regular files below `dir/name` (Space on a directory). Symlinks are
/// not followed, mount points are not crossed; `cancel` stops it. A results tab's `name` is
/// a path relative to its root (P2 2.4): it is walked one component at a time with
/// `O_NOFOLLOW` (P2 2.2).
pub fn dir_size(dir: &std::path::Path, name: &OsStr, cancel: &AtomicBool) -> Option<u64> {
    let sys = Sys::default();
    let mut root = sys.open_root(dir).ok()?;
    let b = name.as_bytes();
    let name = match b.iter().rposition(|&c| c == b'/') {
        Some(k) => {
            for c in b[..k].split(|&c| c == b'/') {
                let (fd, _) = crate::fsops::walk::open_dir_nofollow(
                    &sys,
                    "size.walk",
                    root.as_fd(),
                    OsStr::from_bytes(c),
                )
                .ok()?;
                root = fd;
            }
            OsStr::from_bytes(&b[k + 1..])
        }
        None => name,
    };
    let meta = sys.stat_at("size.stat", root.as_fd(), name).ok()?;
    if meta.kind != Kind::Dir {
        return Some(meta.size);
    }
    fn walk(
        sys: &Sys,
        parent: BorrowedFd,
        name: &OsStr,
        mnt: u64,
        cancel: &AtomicBool,
    ) -> Option<u64> {
        if cancel.load(Ordering::SeqCst) {
            return None;
        }
        let fd = sys.open_dir("size.open", parent, name).ok()?;
        let mut total = 0;
        for (n, _) in sys.read_dir("size.readdir", fd.as_fd()).ok()? {
            let Ok(m) = sys.stat_at("size.stat", fd.as_fd(), &n) else {
                continue;
            };
            match m.kind {
                Kind::File => total += m.size,
                Kind::Dir if m.id.mnt_id == mnt => total += walk(sys, fd.as_fd(), &n, mnt, cancel)?,
                _ => {}
            }
        }
        Some(total)
    }
    walk(&sys, root.as_fd(), name, meta.id.mnt_id, cancel)
}

/// Whether a listed entry is a directory for navigation purposes.
pub fn enters(e: &Entry) -> bool {
    e.kind == EKind::Dir || (e.kind == EKind::Symlink && e.link == LinkKind::Dir)
}
