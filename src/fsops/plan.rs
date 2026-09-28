#![forbid(unsafe_code)]
//! The plan phase (design 4.4, 4.6): scan the source trees with `statx`, build the entry
//! tree and the byte totals, and run the pre-flight checks before any write. A grouped job
//! (P2 2.2) scans every group's directory; its checks use the union over the groups.

use super::identity::{Relation, relation};
use super::question::{Phase, Progress, Reporter};
use super::sys::{Kind, Meta, Sys, fd};
use super::walk::{EntryError, ancestors, open_child_dir};
use rustix::fd::{AsRawFd, BorrowedFd};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

/// The verbs that scan (F8 trash does not: its counts come from panel state).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    Copy,
    Move,
    Delete,
}

/// What the plan already knows about an entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    /// A different `mnt_id` than its parent; move and delete skip it (design 4.2).
    MountPoint,
    /// The `(st_dev, st_ino)` of a directory repeats on the current path: a bind-mount loop.
    Cycle,
    /// Source and destination are the same inode (design 4.6).
    SameFile,
    /// A move whose destination is the source itself under a name that differs only in
    /// case, on a case-insensitive filesystem (design 4.8).
    CaseRename,
    /// The scan could not read the entry.
    Failed(EntryError),
}

impl Note {
    pub fn reason(&self) -> String {
        match self {
            Note::MountPoint => "mount point".into(),
            Note::Cycle => "directory cycle (bind-mount loop)".into(),
            Note::SameFile => "source and destination are the same file".into(),
            Note::CaseRename => "case-only rename".into(),
            Note::Failed(e) => e.to_string(),
        }
    }
}

#[derive(Debug)]
pub struct Node {
    pub name: OsString,
    pub meta: Meta,
    /// Children of a scanned directory, sorted by name bytes.
    pub children: Vec<Node>,
    pub note: Option<Note>,
}

impl Node {
    pub(crate) fn new(name: OsString, meta: Meta) -> Node {
        Node {
            name,
            meta,
            children: Vec::new(),
            note: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub files: u64,
    pub dirs: u64,
    pub symlinks: u64,
    pub specials: u64,
    /// Bytes of regular files.
    pub bytes: u64,
}

impl Totals {
    /// Entries that count toward progress: everything but directories.
    pub fn entries(&self) -> u64 {
        self.files + self.symlinks + self.specials
    }
}

impl std::ops::Add for Totals {
    type Output = Totals;

    fn add(self, o: Totals) -> Totals {
        Totals {
            files: self.files + o.files,
            dirs: self.dirs + o.dirs,
            symlinks: self.symlinks + o.symlinks,
            specials: self.specials + o.specials,
            bytes: self.bytes + o.bytes,
        }
    }
}

impl std::iter::Sum for Totals {
    fn sum<I: Iterator<Item = Totals>>(it: I) -> Totals {
        it.fold(Totals::default(), |a, b| a + b)
    }
}

#[derive(Debug)]
pub struct Plan {
    pub roots: Vec<Node>,
    pub totals: Totals,
    /// `(st_dev, st_ino)` of every scanned source directory.
    pub src_dirs: HashSet<(u64, u64)>,
}

/// Why a job was refused before any write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The destination is a source directory or lies inside one (I-4).
    DestInsideSource,
    Cancelled,
    /// The source or destination directory itself could not be read.
    Root(EntryError),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::DestInsideSource => {
                f.write_str("the destination is inside the source; nothing was written")
            }
            Refusal::Cancelled => f.write_str("cancelled before any write"),
            Refusal::Root(e) => write!(f, "{e}"),
        }
    }
}

/// The input of a scan.
pub struct Scan<'a> {
    pub sys: &'a Sys,
    pub verb: Verb,
    /// The source directory, opened at job start.
    pub src: BorrowedFd<'a>,
    /// The display path of `src`, for progress.
    pub src_path: &'a std::path::Path,
    /// The selected names in `src`.
    pub names: &'a [OsString],
    /// The destination directory and, per selected name, the target name in it.
    pub dst: Option<(BorrowedFd<'a>, &'a [OsString])>,
}

struct Scanner<'a, 'r, 'u> {
    s: &'a Scan<'a>,
    rep: &'r mut Reporter<'u>,
    totals: Totals,
    /// The totals of the groups scanned before this one, for progress.
    before: Totals,
    src_dirs: HashSet<(u64, u64)>,
    current: PathBuf,
}

impl Scanner<'_, '_, '_> {
    fn tick(&mut self) -> Result<(), Refusal> {
        if self.s.sys.cancelled() {
            return Err(Refusal::Cancelled);
        }
        let t = self.before + self.totals;
        let current = &self.current;
        self.rep.progress(|| Progress {
            phase: Phase::Scanning,
            files_done: 0,
            files_total: t.entries(),
            bytes_done: 0,
            bytes_total: t.bytes,
            current: current.clone(),
        });
        Ok(())
    }

    fn count(&mut self, m: &Meta) {
        match m.kind {
            Kind::File => {
                self.totals.files += 1;
                self.totals.bytes += m.size;
            }
            Kind::Dir => self.totals.dirs += 1,
            Kind::Symlink => self.totals.symlinks += 1,
            _ => self.totals.specials += 1,
        }
    }

    /// Classifies a directory node against its parent and descends when the verb allows.
    fn visit_dir(
        &mut self,
        parent_fd: BorrowedFd,
        parent: &Meta,
        node: &mut Node,
        stack: &mut Vec<(u64, u64)>,
    ) -> Result<(), Refusal> {
        let rel = relation(&parent.id, &node.meta.id);
        if rel == Relation::Mount && self.s.verb != Verb::Copy {
            node.note = Some(Note::MountPoint);
            return Ok(());
        }
        if stack.contains(&node.meta.id.inode()) {
            node.note = Some(Note::Cycle);
            return Ok(());
        }
        self.count(&node.meta);
        self.src_dirs.insert(node.meta.id.inode());
        let dir = match open_child_dir(
            self.s.sys,
            "scan.openat",
            parent_fd,
            &node.name,
            &node.meta.id,
        ) {
            Ok((d, _)) => d,
            Err(e) => {
                node.note = Some(Note::Failed(e));
                return Ok(());
            }
        };
        let names = match self.s.sys.read_dir("scan.readdir", fd(&dir)) {
            Ok(n) => n,
            Err(e) => {
                node.note = Some(Note::Failed(EntryError::os("read directory", e)));
                return Ok(());
            }
        };
        let mut names: Vec<OsString> = names.into_iter().map(|(n, _)| n).collect();
        names.sort_unstable_by(|a, b| a.as_encoded_bytes().cmp(b.as_encoded_bytes()));
        stack.push(node.meta.id.inode());
        for name in names {
            self.current.push(&name);
            self.tick()?;
            let child = self.entry(fd(&dir), &node.meta, name, stack)?;
            node.children.push(child);
            self.current.pop();
        }
        stack.pop();
        Ok(())
    }

    fn entry(
        &mut self,
        dir: BorrowedFd,
        dir_meta: &Meta,
        name: OsString,
        stack: &mut Vec<(u64, u64)>,
    ) -> Result<Node, Refusal> {
        let meta = match self.s.sys.stat_at("scan.stat", dir, &name) {
            Ok(m) => m,
            Err(e) => {
                let mut n = Node::new(name, Meta::default());
                n.note = Some(Note::Failed(EntryError::os("stat", e)));
                return Ok(n);
            }
        };
        let mut node = Node::new(name, meta);
        if meta.kind == Kind::Dir {
            self.visit_dir(dir, dir_meta, &mut node, stack)?;
        } else {
            self.count(&meta);
        }
        Ok(node)
    }
}

/// Whether `dst` or any ancestor of it is in one of `sets` (by `(st_dev, st_ino)`).
fn inside(sys: &Sys, sets: &[&HashSet<(u64, u64)>], dst: BorrowedFd) -> Result<bool, Refusal> {
    let chain = ancestors(sys, dst).map_err(|e| Refusal::Root(EntryError::os("stat", e)))?;
    Ok(chain
        .iter()
        .any(|id| sets.iter().any(|s| s.contains(&id.inode()))))
}

/// Scans the selected entries and runs the pre-flight checks (design 4.6).
pub fn scan(s: &Scan, rep: &mut Reporter) -> Result<Plan, Refusal> {
    let mut plans = scan_all(std::slice::from_ref(s), rep)?;
    Ok(plans.pop().expect("one plan per scan"))
}

/// Scans the source directories of one job, one [`Scan`] per group, and runs the
/// pre-flight checks over all of them (P2 2.2), returning one plan per scan. Every scan of
/// a job has the same destination. The destination-inside-source check uses the union,
/// across the scans, of the selected directories and of the directories the scans
/// recorded, exactly the M1 set for one scan: a group's own directory is not in it, so
/// copying into a sibling of a selected file stays allowed.
pub fn scan_all(scans: &[Scan], rep: &mut Reporter) -> Result<Vec<Plan>, Refusal> {
    // Top-level entries first, so the cheap destination check can run before the scan.
    let mut tops = Vec::with_capacity(scans.len());
    for s in scans {
        let src_meta = s
            .sys
            .stat_fd(s.src)
            .map_err(|e| Refusal::Root(EntryError::os("stat", e)))?;
        let mut roots = Vec::with_capacity(s.names.len());
        for name in s.names {
            let meta = s.sys.stat_at("scan.stat", s.src, name);
            let mut n = Node::new(name.clone(), meta.unwrap_or_default());
            if let Err(e) = meta {
                n.note = Some(Note::Failed(EntryError::os("stat", e)));
            }
            roots.push(n);
        }
        tops.push((src_meta, roots));
    }
    // The destinations: one fd for a job, however many groups it has.
    let mut dsts: Vec<(&Scan, BorrowedFd)> = Vec::new();
    for s in scans {
        if let Some((d, _)) = s.dst
            && !dsts.iter().any(|(_, x)| x.as_raw_fd() == d.as_raw_fd())
        {
            dsts.push((s, d));
        }
    }
    if !dsts.is_empty() {
        let top: HashSet<(u64, u64)> = tops
            .iter()
            .flat_map(|(_, roots)| roots)
            .filter(|n| n.note.is_none() && n.meta.kind == Kind::Dir)
            .map(|n| n.meta.id.inode())
            .collect();
        for (s, dst) in &dsts {
            if !top.is_empty() && inside(s.sys, &[&top], *dst)? {
                return Err(Refusal::DestInsideSource);
            }
        }
    }

    let mut plans = Vec::with_capacity(scans.len());
    let mut src_metas = Vec::with_capacity(scans.len());
    let mut before = Totals::default();
    for (s, (src_meta, mut roots)) in scans.iter().zip(tops) {
        src_metas.push(src_meta);
        let mut sc = Scanner {
            s,
            rep: &mut *rep,
            totals: Totals::default(),
            before,
            src_dirs: HashSet::new(),
            current: s.src_path.to_path_buf(),
        };
        let mut stack = vec![src_meta.id.inode()];
        for n in &mut roots {
            if n.note.is_some() {
                continue;
            }
            sc.current.push(&n.name);
            sc.tick()?;
            if n.meta.kind == Kind::Dir {
                sc.visit_dir(s.src, &src_meta, n, &mut stack)?;
            } else {
                sc.count(&n.meta);
            }
            sc.current.pop();
        }
        before = before + sc.totals;
        plans.push(Plan {
            roots,
            totals: sc.totals,
            src_dirs: sc.src_dirs,
        });
    }

    // The full set catches a bind mount of a source subdirectory (A-FS-4).
    let sets: Vec<&HashSet<(u64, u64)>> = plans.iter().map(|p| &p.src_dirs).collect();
    for (s, dst) in &dsts {
        if inside(s.sys, &sets, *dst)? {
            return Err(Refusal::DestInsideSource);
        }
    }
    for ((s, src_meta), plan) in scans.iter().zip(&src_metas).zip(&mut plans) {
        let Some((dst, targets)) = s.dst else {
            continue;
        };
        same_files(s, src_meta, dst, targets, &mut plan.roots)?;
    }
    Ok(plans)
}

/// Notes the selected entries whose target is the source itself (design 4.6 "same file",
/// 4.8 case-only rename).
fn same_files(
    s: &Scan,
    src_meta: &Meta,
    dst: BorrowedFd,
    targets: &[OsString],
    roots: &mut [Node],
) -> Result<(), Refusal> {
    let dst_meta = s
        .sys
        .stat_fd(dst)
        .map_err(|e| Refusal::Root(EntryError::os("stat", e)))?;
    let same_dir = dst_meta.id.inode() == src_meta.id.inode();
    let mut listing: Option<Vec<OsString>> = None;
    for (n, target) in roots.iter_mut().zip(targets) {
        if n.note.is_some() {
            continue;
        }
        if let Ok(d) = s.sys.stat_at("scan.stat", dst, target)
            && d.id.inode() == n.meta.id.inode()
        {
            let mut note = Note::SameFile;
            if s.verb == Verb::Move && same_dir && target.as_os_str() != n.name {
                // The lookup of `target` found the source itself. It is a case-only
                // rename only when no entry is literally named `target`; otherwise
                // `target` is a second hard link on a case-sensitive filesystem.
                let names = listing.get_or_insert_with(|| {
                    s.sys
                        .read_dir("scan.readdir", dst)
                        .map(|v| v.into_iter().map(|(n, _)| n).collect())
                        .unwrap_or_default()
                });
                if !names.iter().any(|x| x == target) {
                    note = Note::CaseRename;
                }
            }
            n.note = Some(note);
        }
    }
    Ok(())
}

/// A name that is safe as a single path component: not empty, `.`, `..`, and without `/`
/// or NUL.
pub fn valid_component(name: &OsStr) -> bool {
    let b = name.as_encoded_bytes();
    !b.is_empty() && b != b"." && b != b".." && !b.contains(&b'/') && !b.contains(&0)
}
