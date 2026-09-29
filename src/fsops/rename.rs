#![forbid(unsafe_code)]
//! Multi-rename (Ctrl+M, P2 6.3) and its undo (P2 6.5): renames inside each selected
//! directory, ordered by what the new names resolve to.
//!
//! The groups are opened like every job's (P2 2.2) and merged by the identity of the
//! directory they reach, so each directory is handled once. Per directory the job `statx`es
//! every old name and every new name (`AT_SYMLINK_NOFOLLOW`). A new name that resolves to
//! another entry of the set makes this entry wait for that one; resolving the name rather
//! than comparing bytes makes a case-insensitive directory work. Among hard links of the
//! inode it resolves to, the holder is the entry whose old name is the new name, exactly
//! or else under case folding ([`folds_equal`]); when no old name of the set matches, a
//! name outside the set holds it. A new name that resolves to the entry itself is a
//! case-only change, which goes through an intermediate name (M1 4.8).
//!
//! Entries whose new names are free, or held by something outside the set, are ready.
//! Every successful rename frees an old name and can make the entry waiting for it ready,
//! so chains resolve in order. An entry whose rename fails or is skipped keeps its old
//! name, and the entries waiting for it are skipped with "the destination exists": they are
//! never passed to cycle breaking. What remains when nothing is ready are cycles (the
//! strongly connected components of the wait graph, in which every entry waits for one
//! other): one member moves to a temporary name `.mc-rename-<16 hex>`, the rest of the
//! cycle follows it, and the temporary name finally goes to its target.
//!
//! Every rename first re-checks the identity of the name it moves ("type changed" when
//! another inode is there) and uses `renameat2(..., RENAME_NOREPLACE)`: `EEXIST` means
//! something outside the set holds the new name, and the entry is skipped (I-3, I-9).
//! After an error or a cancel, a member left under a temporary name goes back to its
//! original name when that is free; otherwise the report names the temporary path (I-9).
//! Cancel is checked between renames. A rename does not fsync (as `mv`, NFR-DUR). Every
//! outcome, completed, cancelled or failed, returns the renames it performed with their
//! identities ([`Report::renamed`]), which is what the undo reverses.

use super::copy::{Dir, Flow};
use super::group::Group;
use super::job::{JobVerb, Report};
use super::plan::valid_component;
use super::question::{Interaction, Phase, Progress, Reporter};
use super::sys::{FsIdentity, Kind, Sys, random_u64};
use super::walk::{EntryError, errno_text};
use rustix::io::Errno;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// The prefix of a temporary name (P2 6.3 step 5).
pub const TEMP_PREFIX: &[u8] = b".mc-rename-";
/// The longest name one path component may have (`NAME_MAX`).
const NAME_MAX: usize = 255;
/// Random temporary names tried before the entry fails.
const TEMP_TRIES: u32 = 16;
const EXISTS: &str = "the destination exists";

/// One rename a job performed (P2 6.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Renamed {
    /// The name before the job.
    pub old: OsString,
    /// The name the job left the entry under: its new name, or a temporary name that the
    /// report states.
    pub new: OsString,
    /// `(st_dev, st_ino)` of the entry.
    pub id: (u64, u64),
}

/// The renames a job performed in one directory (P2 6.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenamedDir {
    /// The directory as the job's group named it (P2 2.2).
    pub root: PathBuf,
    pub sub: Vec<OsString>,
    /// `(st_dev, st_ino)` of the directory the job opened.
    pub dir: (u64, u64),
    pub entries: Vec<Renamed>,
}

impl RenamedDir {
    /// The display path of the directory; a job never opens it (P2 2.2).
    pub fn dir_path(&self) -> PathBuf {
        let mut p = self.root.clone();
        p.extend(&self.sub);
        p
    }
}

/// One requested rename.
struct Want {
    old: OsString,
    new: OsString,
    /// Undo: the inode `old` must still be (P2 6.5).
    expect: Option<(u64, u64)>,
}

/// A directory of the job with its requested renames.
struct Work {
    dir: Dir,
    /// The first group that reached it: its display path.
    group: usize,
    wants: Vec<Want>,
    /// Undo: the directory is no longer the one the rename changed.
    replaced: bool,
}

fn lossy(n: &OsStr) -> String {
    crate::ui::text::escaped(n.as_bytes())
}

/// Ctrl+M (P2 6.3): `renames[g]` are group `g`'s `(old name, new name)` pairs, in
/// selection order; the old names are the group's names. Unchanged pairs are counted and
/// left alone.
pub fn rename_groups(
    sys: &Sys,
    ui: &mut dyn Interaction,
    groups: &[Group],
    renames: &[Vec<(OsString, OsString)>],
) -> Report {
    let verb = JobVerb::Rename;
    let matches = renames.len() == groups.len()
        && groups.iter().zip(renames).all(|(g, r)| {
            g.names.len() == r.len() && g.names.iter().zip(r).all(|(n, (old, _))| n == old)
        });
    if !matches {
        return Report::refused(verb, "the renames do not match the selected names");
    }
    let wants = renames
        .iter()
        .map(|r| {
            r.iter()
                .map(|(old, new)| Want {
                    old: old.clone(),
                    new: new.clone(),
                    expect: None,
                })
                .collect()
        })
        .collect();
    run(sys, ui, verb, groups, wants, vec![None; groups.len()])
}

/// Ctrl+Z in the multi-rename dialog (P2 6.5): renames every entry of `record` back. An
/// entry is renamed back only when its directory and the entry under its new name still
/// have the recorded identities; otherwise it is reported and left alone.
pub fn undo_groups(sys: &Sys, ui: &mut dyn Interaction, record: &[RenamedDir]) -> Report {
    let groups: Vec<Group> = record
        .iter()
        .map(|d| Group {
            root: d.root.clone(),
            sub: d.sub.clone(),
            names: d.entries.iter().map(|e| e.new.clone()).collect(),
        })
        .collect();
    let wants = record
        .iter()
        .map(|d| {
            d.entries
                .iter()
                .map(|e| Want {
                    old: e.new.clone(),
                    new: e.old.clone(),
                    expect: Some(e.id),
                })
                .collect()
        })
        .collect();
    let dirs = record.iter().map(|d| Some(d.dir)).collect();
    run(sys, ui, JobVerb::UndoRename, &groups, wants, dirs)
}

/// Adds `w` to a directory's renames. The same old name twice (one directory reached
/// through two groups) is kept once when it asks for the same new name.
fn add(wants: &mut Vec<Want>, w: Want, dir: &Path) -> Result<(), String> {
    match wants.iter().find(|x| x.old == w.old) {
        Some(x) if x.new == w.new => Ok(()),
        Some(_) => Err(format!(
            "{} is given two new names",
            dir.join(&w.old).display()
        )),
        None => {
            wants.push(w);
            Ok(())
        }
    }
}

fn run(
    sys: &Sys,
    ui: &mut dyn Interaction,
    verb: JobVerb,
    groups: &[Group],
    mut wants: Vec<Vec<Want>>,
    dirs: Vec<Option<(u64, u64)>>,
) -> Report {
    // The job refuses invalid new names before it opens anything (P2 6.4, I-9).
    for (g, ws) in groups.iter().zip(&wants) {
        if let Some(w) = ws
            .iter()
            .find(|w| !valid_component(&w.new) || w.new.len() > NAME_MAX)
        {
            return Report::refused(
                verb,
                format!(
                    "the new name \"{}\" of {} is not a valid name",
                    lossy(&w.new),
                    g.dir_path().join(&w.old).display()
                ),
            );
        }
    }
    let opened = match super::group::open(sys, verb, groups) {
        Ok(o) => o,
        Err(r) => return *r,
    };
    // Groups that reach the same directory are merged, so it is handled once (P2 2.2).
    let mut work: Vec<Work> = Vec::new();
    let mut index: HashMap<(u64, u64), usize> = HashMap::new();
    for s in &opened.sources {
        let ws = std::mem::take(&mut wants[s.group]);
        let id = s.dir.meta.id.inode();
        let replaced = dirs[s.group].is_some_and(|d| d != id);
        let k = match index.get(&id) {
            Some(&k) if !replaced => k,
            _ => {
                if !replaced {
                    index.insert(id, work.len());
                }
                work.push(Work {
                    dir: s.dir.clone(),
                    group: s.group,
                    wants: Vec::with_capacity(ws.len()),
                    replaced,
                });
                work.len() - 1
            }
        };
        for w in ws {
            if let Err(why) = add(&mut work[k].wants, w, &s.dir.path) {
                return opened.refuse(verb, why);
            }
        }
    }
    // Two entries of one directory with the same new name: refused before any write. An
    // unchanged entry keeps its name, so it counts.
    for w in work.iter().filter(|w| !w.replaced) {
        let mut seen = HashSet::with_capacity(w.wants.len());
        if let Some(x) = w.wants.iter().find(|x| !seen.insert(x.new.as_bytes())) {
            return opened.refuse(
                verb,
                format!(
                    "two entries of {} get the new name \"{}\"",
                    w.dir.path.display(),
                    lossy(&x.new)
                ),
            );
        }
    }
    let mut job = Job {
        sys,
        rep: Reporter::new(ui),
        report: Report::new(verb),
        current: PathBuf::new(),
    };
    opened.report_failed(&mut job.report);
    job.report.planned = work.iter().map(|w| w.wants.len() as u64).sum();
    for w in work {
        if w.replaced {
            for x in &w.wants {
                job.skip(
                    w.dir.path.join(&x.old),
                    "the directory was replaced since the rename; left alone",
                );
            }
            continue;
        }
        if job.directory(&w.dir, &groups[w.group], w.wants) == Flow::Stop {
            break;
        }
    }
    job.report
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Pending,
    Done,
    /// Skipped or failed; the report says why.
    Ended,
}

/// An entry of one directory that changes its name.
struct Node {
    old: OsString,
    new: OsString,
    /// Where it is now: `old`, `new`, or a temporary name.
    cur: OsString,
    id: FsIdentity,
    /// Its new name resolves to itself: a case-only change (M1 4.8).
    case_only: bool,
    /// The pending entry that holds its new name.
    waits: Option<usize>,
    /// The entries whose new names it holds.
    waiters: Vec<usize>,
    state: State,
}

struct Job<'s, 'u> {
    sys: &'s Sys,
    rep: Reporter<'u>,
    report: Report,
    current: PathBuf,
}

/// Whether two names are the same under the folding of a case-insensitive directory, as
/// far as the job can tell without asking the filesystem: equal after Unicode lowercasing
/// when both are valid UTF-8, the same bytes otherwise. A directory that folds more
/// (`ß` and `ss`, normalization) is handled by identity (P2 6.3 step 3).
pub fn folds_equal(a: &OsStr, b: &OsStr) -> bool {
    if a == b {
        return true;
    }
    match (a.to_str(), b.to_str()) {
        (Some(x), Some(y)) => x.to_lowercase() == y.to_lowercase(),
        _ => false,
    }
}

/// A fresh temporary name.
fn temp_name() -> OsString {
    let mut v = TEMP_PREFIX.to_vec();
    v.extend_from_slice(format!("{:016x}", random_u64()).as_bytes());
    OsString::from_vec(v)
}

/// A member of a cycle, found by following the waits from `start`; `None` when the walk
/// ends at an entry that is not pending (it is ready, or waits for nothing pending).
fn cycle_member(nodes: &[Node], start: usize) -> Option<usize> {
    let mut seen = vec![false; nodes.len()];
    let mut i = start;
    loop {
        if seen[i] {
            return Some(i);
        }
        seen[i] = true;
        match nodes[i].waits {
            Some(j) if nodes[j].state == State::Pending => i = j,
            _ => return None,
        }
    }
}

impl Job<'_, '_> {
    fn tick(&mut self) {
        let r = &self.report;
        let (done, total) = (r.settled, r.planned);
        let current = &self.current;
        self.rep.progress(|| Progress {
            phase: Phase::Executing,
            files_done: done,
            files_total: total,
            bytes_done: 0,
            bytes_total: 0,
            current: current.clone(),
        });
    }

    fn settle(&mut self) {
        self.report.settled += 1;
        self.tick();
    }

    fn skip(&mut self, path: PathBuf, why: impl Into<String>) {
        self.report.skip(path, why);
        self.settle();
    }

    fn fail(&mut self, path: PathBuf, why: impl Into<String>) {
        self.report.fail(path, why);
        self.settle();
    }

    /// The renames of one directory (P2 6.3 steps 1-7). `Stop` after a cancel.
    fn directory(&mut self, dir: &Dir, group: &Group, wants: Vec<Want>) -> Flow {
        let sys = self.sys;
        // 1-2. Identities; unchanged names drop out.
        let mut nodes: Vec<Node> = Vec::with_capacity(wants.len());
        for w in wants {
            let path = dir.path.join(&w.old);
            if w.old == w.new {
                self.report.unchanged += 1;
                self.settle();
                continue;
            }
            match sys.stat_at("rename.stat", dir.fd(), &w.old) {
                Ok(m) if w.expect.is_some_and(|e| e != m.id.inode()) => {
                    self.skip(path, "replaced since the rename; left alone");
                }
                Ok(m) => nodes.push(Node {
                    cur: w.old.clone(),
                    old: w.old,
                    new: w.new,
                    id: m.id,
                    case_only: false,
                    waits: None,
                    waiters: Vec::new(),
                    state: State::Pending,
                }),
                Err(e) => self.fail(path, EntryError::os("stat", e).to_string()),
            }
        }
        // 3. Dependencies: what each new name resolves to.
        let mut by_id: HashMap<FsIdentity, Vec<usize>> = HashMap::with_capacity(nodes.len());
        for (i, n) in nodes.iter().enumerate() {
            by_id.entry(n.id).or_default().push(i);
        }
        let mut listing: Option<Option<HashSet<OsString>>> = None;
        for i in 0..nodes.len() {
            // Free, or unreadable: the rename itself decides.
            let Ok(m) = sys.stat_at("rename.probe", dir.fd(), &nodes[i].new) else {
                continue;
            };
            // The holder of the new name among the entries of the set with the inode it
            // resolves to: the one whose old name is the new name exactly, else the one
            // whose old name equals it under case folding. Hard links of one inode are told
            // apart by their names, never by their order.
            let same: &[usize] = by_id.get(&m.id).map_or(&[], Vec::as_slice);
            let new = nodes[i].new.as_os_str();
            let holder = same
                .iter()
                .copied()
                .find(|&j| nodes[j].old == new)
                .or_else(|| {
                    same.iter()
                        .copied()
                        .find(|&j| folds_equal(&nodes[j].old, new))
                });
            match holder {
                // Held by another entry of the set: wait for it.
                Some(j) if j != i => {
                    nodes[i].waits = Some(j);
                    nodes[j].waiters.push(i);
                }
                // The entry itself answers to its new name: a case-only change on a
                // case-insensitive directory, unless another name of the directory, a hard
                // link of it outside the set, answers to it (a directory has none). Also
                // when no name of the set matches but the inode is the entry's own: the
                // directory folds more than Unicode lowercase does (`ß` and `ss`, or a
                // normalization difference).
                _ if m.id == nodes[i].id => {
                    let alias = m.kind == Kind::Dir
                        || m.nlink <= 1
                        || !self.listed(dir, &mut listing, new, &nodes[i].old);
                    nodes[i].case_only = alias;
                }
                // Held outside the set: the entry stays ready, and its rename is skipped
                // with "the destination exists" (`RENAME_NOREPLACE`).
                _ => {}
            }
        }
        // 4-5. Ready entries in order, then one cycle at a time.
        let mut ready: VecDeque<usize> = (0..nodes.len())
            .filter(|&i| nodes[i].waits.is_none())
            .collect();
        let mut cancelled = false;
        'work: loop {
            while let Some(i) = ready.pop_front() {
                if nodes[i].state != State::Pending {
                    continue;
                }
                if sys.cancelled() {
                    cancelled = true;
                    break 'work;
                }
                self.step(dir, &mut nodes, i, &mut ready);
            }
            let Some(start) = (0..nodes.len()).find(|&i| nodes[i].state == State::Pending) else {
                break;
            };
            if sys.cancelled() {
                cancelled = true;
                break;
            }
            match cycle_member(&nodes, start) {
                Some(m) => self.break_cycle(dir, &mut nodes, m, &mut ready),
                // Nothing pending holds its new name any more: let the rename decide.
                None => {
                    nodes[start].waits = None;
                    ready.push_back(start);
                }
            }
        }
        // 7. Recovery: members under a temporary name go back when their name is free.
        for n in nodes.iter_mut() {
            if n.state != State::Done && n.cur != n.old {
                self.recover(dir, n);
            }
        }
        let entries: Vec<Renamed> = nodes
            .iter()
            .filter(|n| n.cur != n.old)
            .map(|n| Renamed {
                old: n.old.clone(),
                new: n.cur.clone(),
                id: n.id.inode(),
            })
            .collect();
        if !entries.is_empty() {
            self.report.renamed.push(RenamedDir {
                root: group.root.clone(),
                sub: group.sub.clone(),
                dir: dir.meta.id.inode(),
                entries,
            });
        }
        if cancelled {
            self.report.cancelled = true;
            return Flow::Stop;
        }
        Flow::Continue
    }

    /// Whether an entry of `dir` other than `own` answers to `name`: the same bytes, or
    /// equal under case folding ([`folds_equal`]). The directory is read once. A directory
    /// that cannot be read counts as holding it.
    fn listed(
        &self,
        dir: &Dir,
        listing: &mut Option<Option<HashSet<OsString>>>,
        name: &OsStr,
        own: &OsStr,
    ) -> bool {
        let names = listing.get_or_insert_with(|| {
            self.sys
                .read_dir("rename.list", dir.fd())
                .ok()
                .map(|v| v.into_iter().map(|(n, _)| n).collect())
        });
        names
            .as_ref()
            .is_none_or(|s| s.contains(name) || s.iter().any(|x| x != own && folds_equal(x, name)))
    }

    /// Re-checks that the entry's current name still holds its inode (P2 6.3 step 6).
    fn check(&self, dir: &Dir, n: &Node) -> Result<(), String> {
        match self.sys.stat_at("rename.check", dir.fd(), &n.cur) {
            Ok(m) if m.id.inode() == n.id.inode() => Ok(()),
            Ok(_) => Err(EntryError::TypeChanged.to_string()),
            Err(e) => Err(EntryError::os("stat", e).to_string()),
        }
    }

    /// Moves an entry to a fresh temporary name (`RENAME_NOREPLACE`; another name on
    /// `EEXIST`, as for M1 partial names).
    fn to_temp(&self, dir: &Dir, from: &OsStr) -> Result<OsString, String> {
        for _ in 0..TEMP_TRIES {
            let t = temp_name();
            match self
                .sys
                .rename("rename.tmp", dir.fd(), from, dir.fd(), &t, true)
            {
                Ok(()) => return Ok(t),
                Err(Errno::EXIST) => continue,
                Err(e) => {
                    return Err(format!("rename to a temporary name: {}", errno_text(e)));
                }
            }
        }
        Err("no free temporary name".into())
    }

    /// The entries waiting for `i` may go: its old name is free.
    fn release(nodes: &mut [Node], i: usize, ready: &mut VecDeque<usize>) {
        for w in std::mem::take(&mut nodes[i].waiters) {
            if nodes[w].state == State::Pending && nodes[w].waits == Some(i) {
                nodes[w].waits = None;
                ready.push_back(w);
            }
        }
    }

    /// Ends entry `i` as skipped or failed; it keeps its name, so every entry that waits
    /// for it, directly or through others, is skipped with "the destination exists"
    /// (P2 6.3 step 4). They are never cycle-broken.
    fn end(&mut self, dir: &Dir, nodes: &mut [Node], i: usize, failed: bool, why: String) {
        nodes[i].state = State::Ended;
        let path = dir.path.join(&nodes[i].old);
        if failed {
            self.fail(path, why);
        } else {
            self.skip(path, why);
        }
        let mut stack = vec![i];
        while let Some(k) = stack.pop() {
            for w in nodes[k].waiters.clone() {
                if nodes[w].state == State::Pending && nodes[w].waits == Some(k) {
                    nodes[w].state = State::Ended;
                    self.skip(dir.path.join(&nodes[w].old), EXISTS);
                    stack.push(w);
                }
            }
        }
    }

    /// Renames a ready entry to its new name (P2 6.3 steps 4 and 6); a case-only change
    /// goes through a temporary name first (M1 4.8).
    fn step(&mut self, dir: &Dir, nodes: &mut [Node], i: usize, ready: &mut VecDeque<usize>) {
        self.current = dir.path.join(&nodes[i].old);
        if let Err(why) = self.check(dir, &nodes[i]) {
            return self.end(dir, nodes, i, true, why);
        }
        if nodes[i].case_only && nodes[i].cur == nodes[i].old {
            match self.to_temp(dir, &nodes[i].old) {
                Ok(t) => nodes[i].cur = t,
                Err(why) => return self.end(dir, nodes, i, true, why),
            }
        }
        let (cur, new) = (&nodes[i].cur, &nodes[i].new);
        match self
            .sys
            .rename("rename.rename", dir.fd(), cur, dir.fd(), new, true)
        {
            Ok(()) => {
                nodes[i].cur = nodes[i].new.clone();
                nodes[i].state = State::Done;
                self.report.done += 1;
                self.settle();
                Self::release(nodes, i, ready);
            }
            Err(Errno::EXIST | Errno::NOTEMPTY) => self.end(dir, nodes, i, false, EXISTS.into()),
            Err(e) => self.end(dir, nodes, i, true, EntryError::os("rename", e).to_string()),
        }
    }

    /// Breaks a cycle (P2 6.3 step 5): member `m` moves to a temporary name, which frees
    /// its old name for the member waiting for it; `m` itself renames once its own target
    /// is free.
    fn break_cycle(
        &mut self,
        dir: &Dir,
        nodes: &mut [Node],
        m: usize,
        ready: &mut VecDeque<usize>,
    ) {
        self.current = dir.path.join(&nodes[m].old);
        if nodes[m].cur != nodes[m].old {
            // Cannot happen (nothing waits for an entry under a temporary name); ending it
            // keeps the loop finite, and the recovery takes it back.
            return self.end(dir, nodes, m, true, "internal error: cycle order".into());
        }
        if let Err(why) = self.check(dir, &nodes[m]) {
            return self.end(dir, nodes, m, true, why);
        }
        match self.to_temp(dir, &nodes[m].old) {
            Ok(t) => {
                nodes[m].cur = t;
                Self::release(nodes, m, ready);
            }
            Err(why) => self.end(dir, nodes, m, true, why),
        }
    }

    /// After an error or a cancel: an entry under a temporary name goes back to its
    /// original name if that is free; otherwise the report names the temporary path (I-9).
    fn recover(&mut self, dir: &Dir, n: &mut Node) {
        let left = |why: String| {
            format!(
                "{}: left under the temporary name {} ({why})",
                dir.path.join(&n.old).display(),
                dir.path.join(&n.cur).display()
            )
        };
        if let Err(why) = self.check(dir, n) {
            self.report.notes.push(left(why));
            return;
        }
        match self
            .sys
            .rename("rename.back", dir.fd(), &n.cur, dir.fd(), &n.old, true)
        {
            Ok(()) => n.cur = n.old.clone(),
            Err(Errno::EXIST | Errno::NOTEMPTY) => {
                let note = left("its original name is taken".into());
                self.report.notes.push(note);
            }
            Err(e) => {
                let note = left(errno_text(e));
                self.report.notes.push(note);
            }
        }
    }
}
