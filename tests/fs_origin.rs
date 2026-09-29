//! Phase 3 T1: the copy engine's source side (P3 2.3) and the verbs by place (P3 2.4).
//!
//! A-SRC-1: the local path is the M1 and P2 suites' (they run unchanged through
//! `LocalOrigin`); here, the engine refuses what is not local yet before any write, and an
//! origin read in one pass gets its directories first and its members in stream order.
//! A-SRC-2: an in-memory origin whose files are `Stream`s, as an archive's or a server's
//! are: bytes, symlinks, modes and times; "file exists" commits the kept temporary file
//! without reading the stream again; a stream that is longer or shorter than it declared
//! fails with "size mismatch" and never writes past the declared size; failpoints and
//! cancel leave no temporary name and no partial destination.
//! A-SRC-3: every refused combination of P3 2.4 is refused before any work, with its
//! message, in the app.

mod common;

use common::*;
use manycommander::archive::ArchiveIndex;
use manycommander::fsops::copy::{Flow, copy_from};
use manycommander::fsops::group::{Group, NOT_LOCAL, OpenGroup, Opened, Root};
use manycommander::fsops::job::{Dest, JobSpec, JobVerb, NO_UPLOAD, Outcome, Report, run_guarded};
use manycommander::fsops::origin::{
    EachMember, Order, Origin, OriginDir, OriginFile, Removed, SIZE_MISMATCH,
};
use manycommander::fsops::plan::{Node, Note, Plan, Refusal, Totals, Verb};
use manycommander::fsops::question::{Answer, Question, Reporter};
use manycommander::fsops::sys::{Kind, Meta, Snapshot, Sys, Ts};
use manycommander::fsops::walk::EntryError;
use manycommander::panel::listing::ListingMsg;
use manycommander::provider::{Caps, PlaceError, Provider, VPath, synthetic_id};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

// ---- an in-memory stream origin -------------------------------------------------------------

/// One entry of the in-memory tree.
#[derive(Clone)]
enum Mem {
    Dir(Vec<(OsString, Mem)>),
    /// `declared` is what the "header" says; `data` is what the stream gives, or an endless
    /// run of bytes.
    File {
        data: Arc<Vec<u8>>,
        declared: u64,
        endless: bool,
        perm: u32,
    },
    Link(OsString),
}

fn file(data: &[u8]) -> Mem {
    Mem::File {
        data: Arc::new(data.to_vec()),
        declared: data.len() as u64,
        endless: false,
        perm: 0o644,
    }
}

fn file_mode(data: &[u8], perm: u32) -> Mem {
    Mem::File {
        data: Arc::new(data.to_vec()),
        declared: data.len() as u64,
        endless: false,
        perm,
    }
}

/// A member whose stream gives `data` but whose header says `declared`.
fn lying(data: &[u8], declared: u64) -> Mem {
    Mem::File {
        data: Arc::new(data.to_vec()),
        declared,
        endless: false,
        perm: 0o644,
    }
}

/// A member that never ends behind a small declared size (a decompression bomb, A-4).
fn endless(declared: u64) -> Mem {
    Mem::File {
        data: Arc::new(Vec::new()),
        declared,
        endless: true,
        perm: 0o644,
    }
}

fn dir(entries: Vec<(&[u8], Mem)>) -> Mem {
    Mem::Dir(
        entries
            .into_iter()
            .map(|(n, m)| (OsString::from_vec(n.to_vec()), m))
            .collect(),
    )
}

fn link(target: &[u8]) -> Mem {
    Mem::Link(OsString::from_vec(target.to_vec()))
}

/// The mtime every in-memory entry has.
const MTIME: Ts = Ts {
    sec: 1_700_000_000,
    nsec: 0,
};

/// A stream that counts the bytes it gives.
struct Counting {
    data: Arc<Vec<u8>>,
    pos: usize,
    endless: bool,
    read: Arc<AtomicU64>,
}

impl Read for Counting {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = if self.endless {
            buf.fill(b'z');
            buf.len()
        } else {
            let n = buf.len().min(self.data.len() - self.pos);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            n
        };
        self.read.fetch_add(n as u64, Ordering::SeqCst);
        Ok(n)
    }
}

/// A place held in memory: tree order, every file a `Stream` (P3 2.3), identities
/// synthetic (P3 2.1). It never removes a source.
struct MemOrigin {
    root: Mem,
    /// Bytes its streams gave.
    read: Arc<AtomicU64>,
    order: Order,
    /// The files its last scan planned, by key, for a one pass.
    planned: RefCell<HashMap<u64, Mem>>,
    /// The keys a one pass gave, in its order.
    passed: RefCell<Vec<u64>>,
}

#[derive(Clone)]
struct MemDir {
    at: Vec<OsString>,
    path: PathBuf,
    id: (u64, u64),
}

impl OriginDir for MemDir {
    fn path(&self) -> &Path {
        &self.path
    }

    fn id(&self) -> (u64, u64) {
        self.id
    }
}

impl MemOrigin {
    fn new(root: Mem) -> MemOrigin {
        MemOrigin {
            root,
            read: Arc::new(AtomicU64::new(0)),
            order: Order::Tree,
            planned: RefCell::default(),
            passed: RefCell::default(),
        }
    }

    fn read(&self) -> u64 {
        self.read.load(Ordering::SeqCst)
    }

    fn find(&self, at: &[OsString]) -> Option<&Mem> {
        let mut m = &self.root;
        for c in at {
            let Mem::Dir(children) = m else {
                return None;
            };
            m = &children.iter().find(|(n, _)| n == c)?.1;
        }
        Some(m)
    }

    fn meta(m: &Mem, n: u64) -> Meta {
        let (kind, perm, size) = match m {
            Mem::Dir(_) => (Kind::Dir, 0o755, 0),
            Mem::File { declared, perm, .. } => (Kind::File, *perm, *declared),
            Mem::Link(t) => (Kind::Symlink, 0o777, t.len() as u64),
        };
        Meta {
            kind,
            perm,
            nlink: 1,
            size,
            id: synthetic_id(1, n),
            atime: MTIME,
            mtime: MTIME,
            ctime: MTIME,
            ..Meta::default()
        }
    }

    fn node(&self, name: OsString, m: &Mem, next: &mut u64, totals: &mut Totals) -> Node {
        *next += 1;
        let meta = MemOrigin::meta(m, *next);
        let mut children = Vec::new();
        match m {
            Mem::Dir(entries) => {
                totals.dirs += 1;
                let mut sorted: Vec<_> = entries.iter().collect();
                sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
                for (n, c) in sorted {
                    children.push(self.node(n.clone(), c, next, totals));
                }
            }
            Mem::File { declared, .. } => {
                totals.files += 1;
                totals.bytes += declared;
                self.planned.borrow_mut().insert(*next, m.clone());
            }
            Mem::Link(_) => totals.symlinks += 1,
        }
        Node {
            name,
            meta,
            children,
            note: None,
        }
    }

    fn at(dir: &MemDir, node: &Node) -> Vec<OsString> {
        let mut at = dir.at.clone();
        at.push(node.name.clone());
        at
    }
}

impl Origin for MemOrigin {
    type Dir = MemDir;

    fn open_groups(&self, _verb: JobVerb, groups: &[Group]) -> Result<Opened<MemDir>, Box<Report>> {
        let mut sources = Vec::new();
        let mut failed = Vec::new();
        for (i, g) in groups.iter().enumerate() {
            match self.find(&g.sub) {
                Some(Mem::Dir(_)) => sources.push(OpenGroup {
                    dir: MemDir {
                        at: g.sub.clone(),
                        path: g.dir_path(),
                        id: synthetic_id(1, 1_000 + i as u64).inode(),
                    },
                    names: g.names.clone(),
                    group: i,
                }),
                _ => failed.extend(
                    g.names
                        .iter()
                        .map(|n| (g.dir_path().join(n), "disappeared".to_string())),
                ),
            }
        }
        Ok(Opened { sources, failed })
    }

    fn scan(
        &self,
        _verb: Verb,
        sources: &[OpenGroup<MemDir>],
        _dst: &manycommander::fsops::copy::Dir,
        _targets: &[&[OsString]],
        _rep: &mut Reporter,
    ) -> Result<Vec<Plan>, Refusal> {
        let mut next = 0;
        let mut plans = Vec::new();
        for s in sources {
            let mut totals = Totals::default();
            let mut roots = Vec::new();
            for name in &s.names {
                let mut at = s.dir.at.clone();
                at.push(name.clone());
                match self.find(&at) {
                    Some(m) => roots.push(self.node(name.clone(), m, &mut next, &mut totals)),
                    None => roots.push(Node {
                        name: name.clone(),
                        meta: Meta::default(),
                        children: Vec::new(),
                        note: Some(Note::Failed(EntryError::Disappeared)),
                    }),
                }
            }
            plans.push(Plan {
                roots,
                totals,
                src_dirs: HashSet::new(),
                links: HashMap::new(),
            });
        }
        Ok(plans)
    }

    fn order(&self) -> Order {
        self.order
    }

    /// The wanted members in descending key order: deeper and later members first, so the
    /// engine cannot rely on tree order.
    fn pass(
        &self,
        wanted: &HashSet<u64>,
        _cancel: &Arc<AtomicBool>,
        each: &mut EachMember<'_>,
    ) -> Result<(), String> {
        let mut keys: Vec<u64> = wanted.iter().copied().collect();
        keys.sort_unstable_by(|a, b| b.cmp(a));
        for k in keys {
            self.passed.borrow_mut().push(k);
            let Some(Mem::File { data, endless, .. }) = self.planned.borrow().get(&k).cloned()
            else {
                return Err("not in the stream".into());
            };
            let mut r = Counting {
                data,
                pos: 0,
                endless,
                read: self.read.clone(),
            };
            if each(k, Ok(&mut r)) == Flow::Stop {
                return Ok(());
            }
        }
        Ok(())
    }

    fn open_dir(&self, dir: &MemDir, node: &Node) -> Result<MemDir, EntryError> {
        match self.find(&MemOrigin::at(dir, node)) {
            Some(Mem::Dir(_)) => Ok(MemDir {
                at: MemOrigin::at(dir, node),
                path: dir.path.join(&node.name),
                id: node.meta.id.inode(),
            }),
            _ => Err(EntryError::TypeChanged),
        }
    }

    fn open(
        &self,
        dir: &MemDir,
        node: &Node,
        _cancel: &AtomicBool,
    ) -> Result<OriginFile, EntryError> {
        match self.find(&MemOrigin::at(dir, node)) {
            Some(Mem::File {
                data,
                declared,
                endless,
                ..
            }) => Ok(OriginFile::Stream {
                reader: Box::new(Counting {
                    data: data.clone(),
                    pos: 0,
                    endless: *endless,
                    read: self.read.clone(),
                }),
                declared: *declared,
            }),
            _ => Err(EntryError::TypeChanged),
        }
    }

    fn read_link(&self, dir: &MemDir, node: &Node) -> Result<(OsString, Meta), EntryError> {
        match self.find(&MemOrigin::at(dir, node)) {
            Some(Mem::Link(t)) => Ok((t.clone(), node.meta)),
            _ => Err(EntryError::TypeChanged),
        }
    }

    fn remove(&self, _: &MemDir, _: &OsStr, _: &Snapshot) -> Removed {
        Removed::Kept("the source is kept".into())
    }

    fn remove_dir(&self, _: &MemDir, _: &OsStr, _: (u64, u64)) -> Removed {
        Removed::Kept("the source is kept".into())
    }

    fn mtime_resolution(&self, _: &MemDir) -> i128 {
        1_000_000_000
    }
}

/// A place that holds nothing, for roots, destinations and panel sources that are never
/// read in these tests.
struct NoPlace;

impl Provider for NoPlace {
    fn caps(&self) -> Caps {
        Caps::default()
    }
    fn list(
        &self,
        _: &VPath,
        _: &mut dyn FnMut(ListingMsg),
        _: &AtomicBool,
    ) -> Result<(), PlaceError> {
        Err(PlaceError::NotFound)
    }
    fn lstat(&self, _: &VPath) -> Result<Meta, PlaceError> {
        Err(PlaceError::NotFound)
    }
    fn open_read(&self, _: &VPath, _: &AtomicBool) -> Result<Box<dyn Read + Send>, PlaceError> {
        Err(PlaceError::NotFound)
    }
}

fn no_place() -> Arc<dyn Provider> {
    Arc::new(NoPlace)
}

/// One group in the in-memory place: `names` in the directory `sub`.
fn group(sub: &[&[u8]], names: &[&[u8]]) -> Group {
    Group {
        root: Root::Remote(no_place()),
        sub: sub.iter().map(|c| OsString::from_vec(c.to_vec())).collect(),
        names: names
            .iter()
            .map(|n| OsString::from_vec(n.to_vec()))
            .collect(),
    }
}

fn copy(o: &MemOrigin, sys: &Sys, ui: &mut Script, g: Group, dst: &Path) -> Report {
    copy_from(sys, ui, o, &[g], dst)
}

fn partials(dir: &Path) -> Vec<PathBuf> {
    walk(dir)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .unwrap()
                .as_bytes()
                .windows(12)
                .any(|w| w == b".mc-partial-")
        })
        .collect()
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            out.push(p.clone());
            if std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir()) {
                out.extend(walk(&p));
            }
        }
    }
    out
}

fn failed_with(r: &Report, why: &str) -> usize {
    r.issues
        .iter()
        .filter(|i| matches!(&i.outcome, Outcome::Failed(w) if w == why))
        .count()
}

const MIB: usize = 1 << 20;

/// The A-SRC-2 tree: empty, one-byte, exactly one buffer, several buffers and a
/// pre-checked size, an odd name, a subdirectory, and symlinks (one dangling).
fn tree() -> Mem {
    dir(vec![(
        b"t",
        dir(vec![
            (b"empty", file(b"")),
            (b"one", file_mode(b"1", 0o640)),
            (b"chunk", file(&noise(MIB, 1))),
            (b"big", file(&noise(2 * MIB + 3, 2))),
            (b"odd\nname\xff", file(b"odd one")),
            (
                b"sub",
                dir(vec![
                    (b"deep", file(&noise(5000, 3))),
                    (b"lnk", link(b"../one")),
                ]),
            ),
            (b"dangling", link(b"nowhere/x")),
        ]),
    )])
}

/// Every file of the tree below `t` at `dst`, byte-identical, with its mode and mtime;
/// symlinks as links with their targets.
fn check_tree(o: &MemOrigin, dst: &Path) {
    fn visit(o: &MemOrigin, at: &mut Vec<OsString>, path: &Path) {
        match o.find(at).unwrap() {
            Mem::Dir(children) => {
                assert!(
                    std::fs::symlink_metadata(path).unwrap().is_dir(),
                    "{path:?}"
                );
                for (n, _) in children {
                    at.push(n.clone());
                    visit(o, at, &path.join(n));
                    at.pop();
                }
            }
            Mem::File { data, perm, .. } => {
                assert_eq!(std::fs::read(path).unwrap(), **data, "{path:?}");
                let m = std::fs::symlink_metadata(path).unwrap();
                assert_eq!(m.mode() & 0o7777, *perm, "{path:?}");
                assert_eq!((m.mtime(), m.mtime_nsec()), (MTIME.sec, 0), "{path:?}");
            }
            Mem::Link(t) => {
                assert_eq!(std::fs::read_link(path).unwrap().as_os_str(), t, "{path:?}");
            }
        }
    }
    visit(o, &mut vec![OsString::from("t")], &dst.join("t"));
}

fn tree_bytes() -> u64 {
    (1 + MIB + 2 * MIB + 3 + 7 + 5000) as u64
}

// ---- A-SRC-2 -----------------------------------------------------------------------------

/// Files byte-identical, symlinks as links, modes and times applied; every stream read
/// once, to its end and no further.
#[test]
fn a_src_2_a_stream_origin_copies_files_and_links() {
    let t = test_dir("origin-copy");
    let o = MemOrigin::new(tree());
    let r = copy(
        &o,
        &Sys::default(),
        &mut Script::silent(),
        group(&[], &[b"t"]),
        &t.path,
    );
    assert!(r.issues.is_empty() && r.refused.is_none(), "{r:?}");
    assert_eq!((r.done, r.dirs_done), (8, 2), "{r:?}");
    check_tree(&o, &t.path);
    assert_eq!(o.read(), tree_bytes(), "each stream once");
    assert!(partials(&t.path).is_empty());
}

/// "File exists" at the commit of a stream keeps its temporary file across the question:
/// Overwrite and Rename commit it, Skip and Cancel remove it, and the stream is read once
/// (the M1 4.7 amendment). A large member is asked about before it is read.
#[test]
fn a_src_2_file_exists_commits_the_kept_temporary_file() {
    let fresh = noise(2 * MIB + 3, 2);
    for (name, size) in [(&b"one"[..], 1u64), (b"big", fresh.len() as u64)] {
        for answer in ["overwrite", "rename", "rename twice", "skip", "cancel"] {
            let ctx = format!("{} {answer}", String::from_utf8_lossy(name));
            let t = test_dir("origin-exists");
            write(&t.join(os(name)), b"old!");
            write(
                &t.join(format!("{} (1)", String::from_utf8_lossy(name))),
                b"taken",
            );
            let o = MemOrigin::new(tree());
            let n = |s: &str| OsString::from(s);
            let base = String::from_utf8_lossy(name).into_owned();
            let answers = match answer {
                "overwrite" => vec![Answer::Overwrite],
                "rename" => vec![Answer::Rename(n(&format!("{base} (2)")))],
                // The new name is taken too: asked again, still without a second read.
                "rename twice" => vec![
                    Answer::Rename(n(&format!("{base} (1)"))),
                    Answer::Rename(n(&format!("{base} (2)"))),
                ],
                "skip" => vec![Answer::Skip],
                _ => vec![Answer::Cancel],
            };
            let asked = answers.len();
            let mut ui = Script::new(answers);
            let r = copy(
                &o,
                &Sys::default(),
                &mut ui,
                group(&[b"t"], &[name]),
                &t.path,
            );
            assert_eq!(ui.asked.len(), asked, "{ctx}: {:?}", ui.asked);
            assert!(
                ui.asked
                    .iter()
                    .all(|q| matches!(q, Question::FileExists { .. })),
                "{ctx}: {:?}",
                ui.asked
            );
            let content = |s: &str| std::fs::read(t.join(s)).unwrap();
            let want: Vec<u8> = if size == 1 {
                b"1".to_vec()
            } else {
                fresh.clone()
            };
            match answer {
                "overwrite" => assert_eq!(content(&base), want, "{ctx}"),
                "rename" | "rename twice" => {
                    assert_eq!(content(&base), b"old!", "{ctx}");
                    assert_eq!(content(&format!("{base} (2)")), want, "{ctx}");
                    assert_eq!(content(&format!("{base} (1)")), b"taken", "{ctx}");
                }
                "skip" => {
                    assert_eq!(content(&base), b"old!", "{ctx}");
                    assert_eq!(r.skipped, 1, "{ctx}: {r:?}");
                }
                _ => {
                    assert_eq!(content(&base), b"old!", "{ctx}");
                    assert!(r.cancelled, "{ctx}: {r:?}");
                }
            }
            // A small member was read before the commit found the name; a large one only
            // after the answer, and not at all when the answer is Skip or Cancel.
            let read = if size == 1 || matches!(answer, "overwrite" | "rename" | "rename twice") {
                size
            } else {
                0
            };
            assert_eq!(o.read(), read, "{ctx}: the stream is never read twice");
            assert!(partials(&t.path).is_empty(), "{ctx}");
        }
    }
}

/// "Overwrite all older" compares at the source's resolution (1 s here, P3 1.4) and
/// commits the kept file or removes it.
#[test]
fn a_src_2_overwrite_all_older_decides_on_the_kept_temporary_file() {
    let t = test_dir("origin-older");
    let set = |p: &Path, sec: i64| {
        let ts = rustix::fs::Timespec {
            tv_sec: sec,
            tv_nsec: 0,
        };
        rustix::fs::utimensat(
            rustix::fs::CWD,
            p,
            &rustix::fs::Timestamps {
                last_access: ts,
                last_modification: ts,
            },
            rustix::fs::AtFlags::empty(),
        )
        .unwrap();
    };
    // Older by ten seconds, and in the same second (not older).
    write(&t.join("one"), b"old!");
    set(&t.join("one"), MTIME.sec - 10);
    write(&t.join("odd one"), b"new!");
    set(&t.join("odd one"), MTIME.sec);
    let o = MemOrigin::new(dir(vec![(b"one", file(b"1")), (b"odd one", file(b"2"))]));
    let mut ui = Script::new([Answer::OverwriteAllOlder]);
    let r = copy(
        &o,
        &Sys::default(),
        &mut ui,
        group(&[], &[b"one", b"odd one"]),
        &t.path,
    );
    assert_eq!(std::fs::read(t.join("one")).unwrap(), b"1");
    assert_eq!(std::fs::read(t.join("odd one")).unwrap(), b"new!");
    assert_eq!((r.done, r.skipped), (1, 1), "{r:?}");
    assert_eq!(o.read(), 2);
    assert!(partials(&t.path).is_empty());
}

/// A-4: a stream shorter than declared, longer, or endless behind a small declared size
/// fails with "size mismatch", commits nothing, and reads at most one byte past the
/// declared size.
#[test]
fn a_src_2_size_mismatch_commits_nothing() {
    let cases: Vec<(&[u8], Mem, u64)> = vec![
        (b"short", lying(b"12345", 10), 5),
        (b"long", lying(&noise(20, 4), 10), 11),
        (
            b"one-more",
            lying(&noise(MIB + 1, 5), MIB as u64),
            MIB as u64 + 1,
        ),
        (b"none", lying(b"x", 0), 1),
        (b"bomb", endless(3 * MIB as u64 + 5), 3 * MIB as u64 + 6),
    ];
    for (name, member, max_read) in cases {
        let ctx = String::from_utf8_lossy(name).into_owned();
        let t = test_dir("origin-mismatch");
        let o = MemOrigin::new(dir(vec![(name, member), (b"after", file(b"fine"))]));
        let r = copy(
            &o,
            &Sys::default(),
            &mut Script::silent(),
            group(&[], &[name, b"after"]),
            &t.path,
        );
        assert_eq!(failed_with(&r, SIZE_MISMATCH), 1, "{ctx}: {r:?}");
        assert_eq!(r.done, 1, "the next member goes on: {ctx}");
        assert!(!t.join(os(name)).exists(), "{ctx}: nothing committed");
        assert_eq!(std::fs::read(t.join("after")).unwrap(), b"fine");
        assert!(o.read() <= max_read + 4, "{ctx}: read {}", o.read());
        assert!(partials(&t.path).is_empty(), "{ctx}");
    }
}

/// An origin read in one pass (P3 3.5): the directories are made first, the members come in
/// the stream's order (here the reverse of tree order), each read once, and symlinks come
/// from the origin; the result is the tree order's.
#[test]
fn a_one_pass_origin_gets_directories_first_and_members_in_stream_order() {
    let t = test_dir("origin-onepass");
    let mut o = MemOrigin::new(tree());
    o.order = Order::Stream;
    let r = copy(
        &o,
        &Sys::default(),
        &mut Script::silent(),
        group(&[], &[b"t"]),
        &t.path,
    );
    assert!(r.issues.is_empty() && r.refused.is_none(), "{r:?}");
    assert_eq!((r.done, r.dirs_done), (8, 2), "{r:?}");
    check_tree(&o, &t.path);
    assert_eq!(o.read(), tree_bytes(), "each stream once");
    let passed = o.passed.borrow().clone();
    assert_eq!(passed.len(), 6, "the regular files come through the pass");
    assert!(passed.windows(2).all(|w| w[0] > w[1]), "{passed:?}");
    assert!(partials(&t.path).is_empty());
}

/// A group whose directory is not in the place fails its names; the other groups go on.
#[test]
fn a_missing_group_fails_its_names() {
    let t = test_dir("origin-missing");
    let o = MemOrigin::new(tree());
    let r = copy_from(
        &Sys::default(),
        &mut Script::silent(),
        &o,
        &[group(&[b"gone"], &[b"x", b"y"]), group(&[b"t"], &[b"one"])],
        &t.path,
    );
    assert_eq!((r.failed, r.done), (2, 1), "{r:?}");
    assert_eq!(std::fs::read(t.join("one")).unwrap(), b"1");
}

/// A complete index of a one-member tar: an archive view holds a real index (T2).
fn tar_index(dir: &Path) -> Arc<ArchiveIndex> {
    use manycommander::archive::detect::Want;
    use manycommander::archive::index::Limits;
    use manycommander::archive::{self, IndexCache, OpenRequest};
    use manycommander::panel::listing;
    let path = dir.join("a.tar");
    let mut b = tar::Builder::new(std::fs::File::create(&path).unwrap());
    let mut h = tar::Header::new_gnu();
    h.set_size(1);
    h.set_mode(0o644);
    h.set_cksum();
    b.append_data(&mut h, "m", &b"x"[..]).unwrap();
    b.finish().unwrap();
    let req = OpenRequest {
        slot: 0,
        generation: 0,
        archive: path,
        want: Want::Magic,
        inner: VPath::root(),
        cancel: Arc::default(),
        tz: jiff::tz::TimeZone::UTC,
        limits: Limits::default(),
    };
    let got = std::cell::RefCell::new(None);
    archive::open(&req, &IndexCache::default(), &|m| {
        if let listing::ListingMsg::Opened { index, .. } = m {
            *got.borrow_mut() = Some(index);
        }
    });
    let ix = got.into_inner().expect("the tar opens");
    assert!(ix.is_complete());
    ix
}

// ---- A-SRC-1: the engine refuses what is not local yet -----------------------------------------

/// The P2 group open works on local roots only (P3 2.2), and a server is not a destination
/// yet: every verb refuses before anything is opened or written. An archive's groups are
/// extracted by F5 (T3); a move out of an archive is refused as read-only.
#[test]
fn a_src_1_non_local_roots_and_remote_destinations_are_refused() {
    let t = test_dir("origin-refuse");
    write(&t.join("f"), b"f");
    std::fs::create_dir(t.join("dst")).unwrap();
    let x = test_dir("origin-refuse-archive");
    let ix = tar_index(&x.path);
    let archive = || {
        vec![Group {
            root: Root::Archive(ix.clone()),
            sub: vec![],
            names: vec!["m".into()],
        }]
    };
    let remote = || {
        vec![Group {
            root: Root::Remote(no_place()),
            sub: vec![],
            names: vec!["f".into()],
        }]
    };
    let dst: Dest = t.join("dst").into();
    for groups in [archive(), remote()] {
        let specs = [
            JobSpec::Copy {
                groups: groups.clone(),
                dst: dst.clone(),
            },
            JobSpec::Move {
                groups: groups.clone(),
                dst: dst.clone(),
            },
            JobSpec::Trash {
                groups: groups.clone(),
            },
            JobSpec::Delete {
                groups: groups.clone(),
            },
            JobSpec::Link {
                groups: groups.clone(),
                dst: t.join("dst"),
                kind: manycommander::fsops::link::LinkKind::Relative,
            },
            JobSpec::Attr {
                groups: groups.clone(),
                mode: None,
                mtime: Some(MTIME),
                recursive: false,
            },
            JobSpec::Rename {
                groups: groups.clone(),
                renames: vec![vec![(groups[0].names[0].clone(), "g".into())]],
            },
        ];
        for spec in specs {
            let verb = spec.verb();
            let in_archive = matches!(groups[0].root, Root::Archive(_));
            let why = match verb {
                // Extraction (T3) is tested in tests/archive_extract.rs.
                JobVerb::Copy if in_archive => continue,
                JobVerb::Move if in_archive => "archives are read-only",
                _ => NOT_LOCAL,
            };
            let r = run_guarded(spec, &Sys::default(), &mut Script::silent());
            assert_eq!(r.refused.as_deref(), Some(why), "{verb:?}: {r:?}");
        }
    }
    // A mix of a local and a non-local group is refused as a whole.
    let mut mixed = archive();
    mixed.push(Group::new(&t.path, vec!["f".into()]));
    let r = run_guarded(
        JobSpec::Copy {
            groups: mixed,
            dst: dst.clone(),
        },
        &Sys::default(),
        &mut Script::silent(),
    );
    assert_eq!(r.refused.as_deref(), Some(NOT_LOCAL));
    for spec in [
        JobSpec::Copy {
            groups: vec![Group::new(&t.path, vec!["f".into()])],
            dst: Dest::Remote {
                session: no_place(),
                dir: VPath::root(),
            },
        },
        JobSpec::Move {
            groups: vec![Group::new(&t.path, vec!["f".into()])],
            dst: Dest::Remote {
                session: no_place(),
                dir: VPath::root(),
            },
        },
    ] {
        let r = run_guarded(spec, &Sys::default(), &mut Script::silent());
        assert_eq!(r.refused.as_deref(), Some(NO_UPLOAD), "{r:?}");
    }
    assert!(walk(&t.join("dst")).is_empty(), "nothing written");
    assert_eq!(
        std::fs::read(t.join("f")).unwrap(),
        b"f",
        "the source is untouched"
    );
    // Two remote destinations are equal only for one session and one directory.
    let s = no_place();
    let d = |s: &Arc<dyn Provider>, p: &[u8]| Dest::Remote {
        session: s.clone(),
        dir: VPath::parse(p).unwrap(),
    };
    assert_eq!(d(&s, b"/a"), d(&s, b"/a"));
    assert_ne!(d(&s, b"/a"), d(&s, b"/b"));
    assert_ne!(d(&s, b"/a"), d(&no_place(), b"/a"));
    assert_eq!(
        Root::Archive(ix.clone()),
        Root::Archive(ix.clone()),
        "same index"
    );
    assert_ne!(Root::Archive(ix.clone()), Root::Remote(s));
}

// ---- A-SRC-2 with failpoints ------------------------------------------------------------------

#[cfg(feature = "failpoints")]
mod failpoints {
    use super::*;
    use manycommander::fsops::failpoints::{Action, Failpoints, Trigger};
    use rustix::io::Errno;
    use std::sync::Mutex;

    fn sys_with(fp: &Arc<Failpoints>) -> Sys {
        Sys::with_failpoints(Arc::new(AtomicBool::new(false)), fp.clone())
    }

    /// Every file under `t` at `dst` is absent or complete: no partial destination (I-2).
    fn complete_or_absent(o: &MemOrigin, dst: &Path, ctx: &str) {
        fn visit(o: &MemOrigin, at: &mut Vec<OsString>, path: &Path, ctx: &str) {
            let Ok(m) = std::fs::symlink_metadata(path) else {
                return;
            };
            match o.find(at).unwrap() {
                Mem::Dir(children) => {
                    assert!(m.is_dir(), "{ctx}: {path:?}");
                    for (n, _) in children {
                        at.push(n.clone());
                        visit(o, at, &path.join(n), ctx);
                        at.pop();
                    }
                }
                Mem::File { data, .. } => {
                    assert_eq!(std::fs::read(path).unwrap(), **data, "{ctx}: {path:?}")
                }
                Mem::Link(t) => {
                    assert_eq!(std::fs::read_link(path).unwrap().as_os_str(), t, "{ctx}")
                }
            }
        }
        visit(o, &mut vec![OsString::from("t")], &dst.join("t"), ctx);
    }

    /// Failpoints and cancel at each chunk, at each step of the write side and at the
    /// commit leave no `.mc-partial-` name and no partial destination.
    #[test]
    fn a_src_2_failpoint_and_cancel_sweep() {
        let clean = {
            let t = test_dir("origin-sweep");
            let fp = Failpoints::new();
            let o = MemOrigin::new(tree());
            let r = copy(
                &o,
                &sys_with(&fp),
                &mut Script::silent(),
                group(&[], &[b"t"]),
                &t.path,
            );
            assert!(r.issues.is_empty(), "{r:?}");
            fp
        };
        let steps = [
            "copy.mkdir",
            "copy.opendst",
            "copy.tmp",
            "copy.chunk",
            "copy.write",
            "copy.chmod",
            "copy.utimes",
            "copy.dststat",
            "commit.rename",
        ];
        let mut runs = 0;
        for step in steps {
            let hits = clean.hits(step);
            assert!(hits > 0, "{step} never reached: {:?}", clean.all_hits());
            for n in 1..=hits {
                for cancel in [true, false] {
                    let t = test_dir("origin-sweep");
                    let fp = Failpoints::new();
                    let action = if cancel {
                        Action::Cancel
                    } else {
                        Action::Errno(Errno::IO)
                    };
                    fp.arm(step, Trigger::Nth(n), action);
                    let o = MemOrigin::new(tree());
                    let mut ui = Script::new([]);
                    ui.fallback = Answer::Skip;
                    let r = copy(&o, &sys_with(&fp), &mut ui, group(&[], &[b"t"]), &t.path);
                    runs += 1;
                    let ctx = format!("{step} #{n} cancel={cancel}: {r:?}");
                    assert!(fp.hits(step) >= n, "{ctx}");
                    assert!(partials(&t.path).is_empty(), "{ctx}");
                    complete_or_absent(&o, &t.path, &ctx);
                    if cancel {
                        assert!(r.cancelled || r.done == 8, "{ctx}");
                    } else {
                        assert!(r.failed >= 1, "{ctx}");
                    }
                    assert!(o.read() <= tree_bytes() + 8, "{ctx}: read {}", o.read());
                }
            }
        }
        assert!(runs > 40, "the sweep ran {runs} injections");
    }

    /// Cancel in the middle of a stream leaves no temporary file and no destination.
    #[test]
    fn cancel_mid_stream_leaves_no_temporary() {
        let t = test_dir("origin-cancel");
        let fp = Failpoints::new();
        fp.arm("copy.chunk", Trigger::Nth(2), Action::Cancel);
        let o = MemOrigin::new(dir(vec![(b"big", file(&noise(3 * MIB, 6)))]));
        let r = copy(
            &o,
            &sys_with(&fp),
            &mut Script::silent(),
            group(&[], &[b"big"]),
            &t.path,
        );
        assert!(r.cancelled, "{r:?}");
        assert!(walk(&t.path).is_empty(), "{:?}", walk(&t.path));
    }

    /// The temporary file never holds more than the declared size, at any chunk (A-4), and
    /// the member fails with "size mismatch".
    #[test]
    fn a_src_2_the_temporary_file_never_exceeds_the_declared_size() {
        for (name, member, declared) in [
            (
                &b"bomb"[..],
                endless(3 * MIB as u64 + 5),
                3 * MIB as u64 + 5,
            ),
            (
                b"one-more",
                lying(&noise(2 * MIB + 1, 7), 2 * MIB as u64),
                2 * MIB as u64,
            ),
        ] {
            let t = test_dir("origin-bound");
            let fp = Failpoints::new();
            let seen = Arc::new(Mutex::new(Vec::new()));
            let (dir_path, s) = (t.path.clone(), seen.clone());
            let measure = move || {
                for p in partials(&dir_path) {
                    s.lock().unwrap().push(std::fs::metadata(p).unwrap().len());
                }
            };
            let measure = Arc::new(measure);
            fp.arm("copy.chunk", Trigger::Always, Action::Call(measure.clone()));
            fp.arm("copy.write", Trigger::Always, Action::Call(measure));
            let o = MemOrigin::new(dir(vec![(name, member)]));
            let r = copy(
                &o,
                &sys_with(&fp),
                &mut Script::silent(),
                group(&[], &[name]),
                &t.path,
            );
            assert_eq!(failed_with(&r, SIZE_MISMATCH), 1, "{r:?}");
            let seen = seen.lock().unwrap();
            assert!(seen.len() >= 3, "measured at the failpoints: {seen:?}");
            assert!(seen.iter().all(|&s| s <= declared), "{seen:?} > {declared}");
            assert!(o.read() <= declared + 1);
            assert!(walk(&t.path).is_empty());
        }
    }

    /// A failed commit keeps the temporary file: Retry commits it without reading the
    /// stream again, and Skip removes it.
    #[test]
    fn a_failed_commit_keeps_the_temporary_file_for_retry() {
        for (answer, committed) in [(Answer::Retry, true), (Answer::Skip, false)] {
            let t = test_dir("origin-retry");
            let fp = Failpoints::new();
            fp.arm("commit.rename", Trigger::Nth(1), Action::Errno(Errno::IO));
            let o = MemOrigin::new(dir(vec![(b"f", file(b"content"))]));
            let mut ui = Script::new([answer]);
            let r = copy(&o, &sys_with(&fp), &mut ui, group(&[], &[b"f"]), &t.path);
            assert!(
                matches!(
                    ui.asked[..],
                    [Question::Error {
                        errno: Errno::IO,
                        ..
                    }]
                ),
                "{:?}",
                ui.asked
            );
            assert_eq!(t.join("f").exists(), committed, "{r:?}");
            if committed {
                assert_eq!(std::fs::read(t.join("f")).unwrap(), b"content");
            }
            assert_eq!(o.read(), 7, "read once");
            assert!(partials(&t.path).is_empty());
        }
    }

    /// Direct-write mode (M1 4.7 step 5): the first stream finds it at its commit, and its
    /// kept temporary file is copied into the final name; later streams write the final
    /// name directly. Nothing is read twice, and nothing partial remains.
    #[test]
    fn direct_write_mode_takes_the_kept_temporary_file() {
        let t = test_dir("origin-direct");
        let fp = Failpoints::new();
        fp.arm(
            "commit.rename",
            Trigger::Always,
            Action::Errno(Errno::INVAL),
        );
        fp.arm("commit.link", Trigger::Always, Action::Errno(Errno::PERM));
        let o = MemOrigin::new(tree());
        let r = copy(
            &o,
            &sys_with(&fp),
            &mut Script::silent(),
            group(&[], &[b"t"]),
            &t.path,
        );
        assert!(r.issues.is_empty(), "{r:?}");
        check_tree(&o, &t.path);
        assert_eq!(o.read(), tree_bytes(), "each stream once");
        assert!(fp.hits("commit.direct") > 1, "{:?}", fp.all_hits());
        assert!(partials(&t.path).is_empty());
        // A longer stream in direct-write mode leaves no final name either.
        let t = test_dir("origin-direct-long");
        let fp = Failpoints::new();
        fp.arm(
            "commit.rename",
            Trigger::Always,
            Action::Errno(Errno::INVAL),
        );
        fp.arm("commit.link", Trigger::Always, Action::Errno(Errno::PERM));
        let o = MemOrigin::new(dir(vec![
            (b"a", file(b"first")),
            (b"b", lying(b"too long", 3)),
        ]));
        let r = copy(
            &o,
            &sys_with(&fp),
            &mut Script::silent(),
            group(&[], &[b"a", b"b"]),
            &t.path,
        );
        assert_eq!(failed_with(&r, SIZE_MISMATCH), 1, "{r:?}");
        assert_eq!(std::fs::read(t.join("a")).unwrap(), b"first");
        assert!(!t.join("b").exists());
        assert!(partials(&t.path).is_empty());
    }
}

// ---- A-SRC-3 ------------------------------------------------------------------------------

mod app {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use manycommander::app::App;
    use manycommander::app::event::{Effect, Event};
    use manycommander::app::jobs::{
        NO_REMOTE_TRASH, NO_SERVER_COPY, NOT_IN_ARCHIVE, NOT_ON_SERVER, NOT_YET, READ_ONLY,
        THROUGH_LOCAL,
    };
    use manycommander::archive::{self, ArchiveIndex};
    use manycommander::config::Config;
    use manycommander::panel::listing;
    use manycommander::panel::{ArchiveView, RemoteView, Source};
    use manycommander::provider::{StatKey, Target};
    use manycommander::theme::Depth;
    use manycommander::ui::dialog::Dialog;

    fn app(left: &Path, right: &Path) -> App {
        App::new(
            left.to_path_buf(),
            right.to_path_buf(),
            right.to_path_buf(),
            Config::default(),
            None,
            Depth::NoColor,
            jiff::tz::TimeZone::UTC,
        )
    }

    /// Performs the listings synchronously.
    fn run(app: &mut App, fx: Vec<Effect>) {
        for e in fx {
            if let Effect::List(req, alive) = e {
                let msgs = std::cell::RefCell::new(Vec::new());
                listing::guarded(&req, &|m| msgs.borrow_mut().push(m), listing::list);
                alive.finish();
                for m in msgs.into_inner() {
                    let more = app.update(Event::Listing(m));
                    run(app, more);
                }
            }
        }
    }

    fn key(a: &mut App, code: KeyCode, m: KeyModifiers) -> Vec<Effect> {
        a.update(Event::Key(
            KeyEvent::new(code, m),
            std::time::Instant::now(),
        ))
    }

    fn archive(index: &Arc<ArchiveIndex>) -> Source {
        Source::Archive(ArchiveView {
            index: index.clone(),
            archive: "/x/a.zip".into(),
            key: StatKey {
                dev: 1,
                ino: 2,
                size: 3,
                mtime: MTIME,
                ctime: MTIME,
            },
            inner: VPath::root(),
        })
    }

    fn remote(session: &Arc<dyn Provider>) -> Source {
        Source::Remote(RemoteView {
            session: session.clone(),
            target: Target {
                user: None,
                host: "h".into(),
                port: None,
            },
            dir: VPath::root(),
        })
    }

    const NONE: KeyModifiers = KeyModifiers::NONE;
    const SHIFT: KeyModifiers = KeyModifiers::SHIFT;
    const ALT: KeyModifiers = KeyModifiers::ALT;
    const CTRL: KeyModifiers = KeyModifiers::CONTROL;

    /// Each key: its status message, no effect, no dialog, no job.
    fn refused(a: &mut App, keys: &[(KeyCode, KeyModifiers, &str)], ctx: &str) {
        for &(code, m, why) in keys {
            let fx = key(a, code, m);
            let ctx = format!("{ctx}: {code:?} {m:?}");
            assert!(fx.is_empty(), "{ctx}: {fx:?}");
            assert!(a.dialog.is_none(), "{ctx}: a dialog opened");
            assert!(a.job.is_none(), "{ctx}");
            assert_eq!(
                a.status.as_ref().map(|s| s.text.as_str()),
                Some(why),
                "{ctx}"
            );
            assert!(a.status.as_ref().is_some_and(|s| s.error), "{ctx}");
        }
    }

    #[test]
    fn a_src_3_verbs_are_refused_by_place_before_any_work() {
        let l = test_dir("origin-app-left");
        let r = test_dir("origin-app-right");
        write(&l.join("f"), b"f");
        std::fs::create_dir(l.join("d")).unwrap();
        write(&r.join("g"), b"g");
        let mut a = app(&l.path, &r.path);
        let fx = a.start();
        run(&mut a, fx);
        for s in 0..2 {
            a.sides[s].panel_mut().ensure_sorted();
        }
        a.panel_mut().cursor_to_name(b"f");

        // An archive on the active side, a local directory on the other.
        let x = test_dir("origin-app-archive");
        let ix = tar_index(&x.path);
        a.sides[0].panel_mut().source = archive(&ix);
        refused(
            &mut a,
            &[
                (KeyCode::F(6), NONE, READ_ONLY),
                (KeyCode::F(7), NONE, READ_ONLY),
                (KeyCode::F(6), SHIFT, READ_ONLY),
                (KeyCode::F(8), NONE, READ_ONLY),
                (KeyCode::F(8), SHIFT, READ_ONLY),
                (KeyCode::F(4), SHIFT, READ_ONLY),
                (KeyCode::Char('a'), ALT, READ_ONLY),
                (KeyCode::Char('m'), CTRL, READ_ONLY),
                (KeyCode::Char('l'), ALT, NOT_IN_ARCHIVE),
                (KeyCode::F(7), ALT, NOT_IN_ARCHIVE),
            ],
            "archive -> local",
        );
        // Extract and view are allowed (T3): F5 opens the extract dialog; F3, F4 and
        // `Enter` look the entry up in the index (this listing is the local one's, so "f" is
        // not in it) instead of being refused.
        assert!(key(&mut a, KeyCode::F(5), NONE).is_empty());
        let Some(Dialog::Input { title, .. }) = &a.dialog else {
            panic!("F5 in an archive opens the extract dialog");
        };
        assert_eq!(title, "Extract");
        key(&mut a, KeyCode::Esc, NONE);
        for (code, m) in [
            (KeyCode::F(3), NONE),
            (KeyCode::F(4), NONE),
            (KeyCode::Enter, NONE),
        ] {
            assert!(key(&mut a, code, m).is_empty());
            assert_eq!(
                a.status.as_ref().map(|s| s.text.as_str()),
                Some(archive::NOT_IN_ARCHIVE),
                "{code:?}"
            );
        }
        // Compare by content is refused, by date and size is not.
        key(&mut a, KeyCode::F(2), SHIFT);
        key(&mut a, KeyCode::Right, NONE);
        assert!(key(&mut a, KeyCode::Enter, NONE).is_empty());
        let Some(Dialog::Form { form, .. }) = &a.dialog else {
            panic!("the compare form stays open");
        };
        assert_eq!(form.error.as_deref(), Some(NOT_IN_ARCHIVE));
        key(&mut a, KeyCode::Esc, NONE);
        key(&mut a, KeyCode::F(2), SHIFT);
        let fx = key(&mut a, KeyCode::Enter, NONE);
        assert!(matches!(fx[..], [Effect::Compare(_)]), "{fx:?}");
        a.update(Event::Key(
            KeyEvent::new(KeyCode::Esc, NONE),
            std::time::Instant::now(),
        ));

        // A local directory on the active side, the archive on the other: nothing goes
        // into an archive.
        key(&mut a, KeyCode::Tab, NONE);
        a.sides[1].panel_mut().cursor_to_name(b"g");
        refused(
            &mut a,
            &[
                (KeyCode::F(5), NONE, READ_ONLY),
                (KeyCode::F(6), NONE, READ_ONLY),
                (KeyCode::Char('l'), ALT, READ_ONLY),
            ],
            "local -> archive",
        );

        // A server on the active side.
        let one = no_place();
        let two = no_place();
        key(&mut a, KeyCode::Tab, NONE);
        a.sides[0].panel_mut().source = remote(&one);
        a.sides[1].panel_mut().source = Source::Dir;
        refused(
            &mut a,
            &[
                (KeyCode::F(8), NONE, NO_REMOTE_TRASH),
                (KeyCode::F(4), SHIFT, NOT_ON_SERVER),
                (KeyCode::Char('a'), ALT, NOT_ON_SERVER),
                (KeyCode::Char('m'), CTRL, NOT_ON_SERVER),
                (KeyCode::Char('l'), ALT, NOT_ON_SERVER),
                (KeyCode::F(7), ALT, NOT_ON_SERVER),
                // Later phase 3 tasks: download and view (T6), 3b (T7).
                (KeyCode::F(5), NONE, NOT_YET),
                (KeyCode::F(6), NONE, NOT_YET),
                (KeyCode::F(3), NONE, NOT_YET),
                (KeyCode::F(7), NONE, NOT_YET),
                (KeyCode::F(6), SHIFT, NOT_YET),
                (KeyCode::F(8), SHIFT, NOT_YET),
            ],
            "remote -> local",
        );
        a.sides[1].panel_mut().source = remote(&one);
        refused(
            &mut a,
            &[
                (KeyCode::F(5), NONE, NO_SERVER_COPY),
                (KeyCode::F(6), NONE, NOT_YET),
            ],
            "remote -> same session",
        );
        a.sides[1].panel_mut().source = remote(&two);
        refused(
            &mut a,
            &[
                (KeyCode::F(5), NONE, THROUGH_LOCAL),
                (KeyCode::F(6), NONE, THROUGH_LOCAL),
            ],
            "remote -> other session",
        );
        a.sides[1].panel_mut().source = archive(&ix);
        refused(
            &mut a,
            &[
                (KeyCode::F(5), NONE, READ_ONLY),
                (KeyCode::F(6), NONE, READ_ONLY),
            ],
            "remote -> archive",
        );
        a.sides[0].panel_mut().source = archive(&ix);
        a.sides[1].panel_mut().source = remote(&one);
        refused(
            &mut a,
            &[
                (KeyCode::F(5), NONE, THROUGH_LOCAL),
                (KeyCode::F(6), NONE, THROUGH_LOCAL),
            ],
            "archive -> remote",
        );
        // Local to a server: uploads are phase 3b (T7).
        a.sides[0].panel_mut().source = Source::Dir;
        refused(
            &mut a,
            &[
                (KeyCode::F(5), NONE, NOT_YET),
                (KeyCode::F(6), NONE, NOT_YET),
            ],
            "local -> remote",
        );

        // Local to local is untouched: F5 opens its dialog.
        a.sides[1].panel_mut().source = Source::Dir;
        a.status = None;
        assert!(key(&mut a, KeyCode::F(5), NONE).is_empty());
        assert!(a.dialog.is_some() && a.status.is_none());
        key(&mut a, KeyCode::Esc, NONE);
        // Space on a directory in an archive marks it without a local size walk: its size
        // comes from the index (T2).
        a.sides[0].panel_mut().source = archive(&ix);
        a.panel_mut().cursor_to_name(b"d");
        let fx = key(&mut a, KeyCode::Char(' '), NONE);
        assert!(
            fx.iter().all(|e| !matches!(e, Effect::DirSize { .. })),
            "{fx:?}"
        );
        assert!(matches!(fx[..], [Effect::ArchiveSize(_)]), "{fx:?}");
        assert_eq!(a.panel().marked, 1);
        // Ctrl+R re-reads nothing local for it.
        let fx = key(&mut a, KeyCode::Char('r'), CTRL);
        assert!(
            fx.iter()
                .all(|e| !matches!(e, Effect::List(req, _) if req.slot == 0)),
            "{fx:?}"
        );
    }
}
