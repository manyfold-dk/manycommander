//! P2 T2: copy fidelity (P2 9). A-SP-1 (files with holes keep them through copy and move
//! across btrfs and tmpfs; the `SEEK_DATA` fallback), A-HL-1 (hard links inside the copied
//! set stay links through copy and cross-filesystem move; deferred unlinks and source
//! directories), A-HL-2 (a replaced first destination; a source rewritten with its mtime
//! restored), A-HL-3 (`linkat` fallbacks and their report note), and a failpoint sweep over
//! a cross-filesystem move of a tree with holes and hard links (I-1).

mod common;

use common::*;
use manycommander::fsops::group::Group;
use manycommander::fsops::job::{JobSpec, Outcome, Report, run_guarded};
use manycommander::fsops::question::{Answer, Question};
use manycommander::fsops::sys::Sys;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};

const K: u64 = 1024;

fn names(n: &[&str]) -> Vec<OsString> {
    n.iter().map(OsString::from).collect()
}

fn copy(sys: &Sys, ui: &mut Script, src: &Path, n: &[&str], dst: &Path) -> Report {
    run_guarded(
        JobSpec::Copy {
            groups: vec![Group::new(src, names(n))],
            dst: dst.into(),
        },
        sys,
        ui,
    )
}

fn mv(sys: &Sys, ui: &mut Script, src: &Path, n: &[&str], dst: &Path) -> Report {
    run_guarded(
        JobSpec::Move {
            groups: vec![Group::new(src, names(n))],
            dst: dst.into(),
        },
        sys,
        ui,
    )
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            out.push(p.clone());
            if std::fs::symlink_metadata(&p)
                .map(|m| m.is_dir())
                .unwrap_or(false)
            {
                out.extend(walk(&p));
            }
        }
    }
    out.sort();
    out
}

fn no_partials(dirs: &[&Path]) {
    for d in dirs {
        for p in walk(d) {
            let n = p.file_name().unwrap().as_bytes();
            assert!(
                !n.windows(12).any(|w| w == b".mc-partial-"),
                "a temporary name remained: {p:?}"
            );
        }
    }
}

fn meta(p: &Path) -> std::fs::Metadata {
    std::fs::symlink_metadata(p).unwrap()
}

/// Allocated bytes (`st_blocks x 512`).
fn alloc(p: &Path) -> u64 {
    meta(p).blocks() * 512
}

fn exists(p: &Path) -> bool {
    p.symlink_metadata().is_ok()
}

// ---- sparse files (P2 9.1) --------------------------------------------------------------

/// A file of `size` bytes with noise at the `(offset, len)` ranges and holes elsewhere.
fn sparse(p: &Path, data: &[(u64, u64)], size: u64, seed: u64) {
    let f = std::fs::File::create(p).unwrap();
    for (i, (off, len)) in data.iter().enumerate() {
        f.write_all_at(&noise(*len as usize, seed + i as u64), *off)
            .unwrap();
    }
    f.set_len(size).unwrap();
}

/// The A-SP-1 files in `dir`: holes at the start, in the middle and at the end, and a file
/// that is one hole. Every boundary is page-aligned, so both btrfs and tmpfs keep the holes.
const SPARSE: [&str; 4] = ["start", "middle", "end", "hole"];

fn sparse_set(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    sparse(&dir.join("start"), &[(1024 * K, 256 * K)], 1280 * K, 1);
    sparse(
        &dir.join("middle"),
        &[(0, 256 * K), (2304 * K, 256 * K)],
        2560 * K,
        2,
    );
    sparse(&dir.join("end"), &[(0, 256 * K)], 2048 * K, 3);
    sparse(&dir.join("hole"), &[], 4096 * K, 4);
}

/// What a copy of a sparse file must keep: content, size, and at most its allocation.
struct Want {
    hash: blake3::Hash,
    size: u64,
    alloc: u64,
}

fn wants(dir: &Path) -> Vec<Want> {
    SPARSE
        .iter()
        .map(|n| {
            let p = dir.join(n);
            let w = Want {
                hash: hash(&p),
                size: meta(&p).len(),
                alloc: alloc(&p),
            };
            assert!(
                w.alloc < w.size,
                "the fixture {p:?} has no holes on this filesystem"
            );
            w
        })
        .collect()
}

/// Identical hash and size; allocation at most the source's plus 64 KiB (A-SP-1).
fn check_sparse(dir: &Path, want: &[Want], ctx: &str) {
    for (n, w) in SPARSE.iter().zip(want) {
        let p = dir.join(n);
        assert_eq!(hash(&p), w.hash, "{ctx}: {n} content");
        assert_eq!(meta(&p).len(), w.size, "{ctx}: {n} size");
        assert!(
            alloc(&p) <= w.alloc + 64 * K,
            "{ctx}: {n} allocates {} bytes, the source {}",
            alloc(&p),
            w.alloc
        );
    }
}

#[test]
fn a_sp_1_sparse_files_keep_their_holes() {
    if !require_btrfs() {
        return;
    }
    let Some(x) = xdev_dir("fid-sp1") else {
        return;
    };
    let t = test_dir("fid-sp1");
    let cases = [
        ("btrfs -> tmpfs", t.join("a"), x.join("a")),
        ("tmpfs -> btrfs", x.join("b"), t.join("b")),
        ("btrfs -> btrfs", t.join("c"), t.join("c-dst")),
    ];
    for (ctx, src, dst) in cases {
        let (from, copied, moved) = (src.join("set"), dst.join("copy"), dst.join("moved"));
        sparse_set(&from);
        std::fs::create_dir_all(&copied).unwrap();
        std::fs::create_dir_all(&moved).unwrap();
        let want = wants(&from);

        let r = copy(
            &Sys::default(),
            &mut Script::silent(),
            &from,
            &SPARSE,
            &copied,
        );
        assert_eq!((r.done, r.failed, r.skipped), (4, 0, 0), "{ctx}: {r:?}");
        check_sparse(&copied, &want, &format!("{ctx} copy"));
        check_sparse(&from, &want, &format!("{ctx} copy source"));

        // Within btrfs a move is a rename; across filesystems it takes the copy path.
        let r = mv(
            &Sys::default(),
            &mut Script::silent(),
            &from,
            &SPARSE,
            &moved,
        );
        assert_eq!((r.done, r.failed, r.skipped), (4, 0, 0), "{ctx}: {r:?}");
        check_sparse(&moved, &want, &format!("{ctx} move"));
        for n in SPARSE {
            assert!(!exists(&from.join(n)), "{ctx}: {n} is gone from the source");
        }
        no_partials(&[&copied, &moved]);
    }
}

// ---- hard links (P2 9.2) ----------------------------------------------------------------

/// The A-HL-1 tree below `root/tree`: a pair in two directories (`p1/a`, `p2/b`), a pair in
/// one directory (`same/c1`, `same/c2`), a triple across two directories (`t/x`, `t/y`,
/// `u/z`), a file with its second link outside the selection (`solo`, `root/outside/solo`)
/// and a plain file. Returns every name below `tree` with its content.
fn link_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    for d in [
        "tree/p1",
        "tree/p2",
        "tree/same",
        "tree/t",
        "tree/u",
        "outside",
    ] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let tree = root.join("tree");
    let mut out = BTreeMap::new();
    let mut file = |rel: &str, len: usize, seed: u64, links: &[&str]| {
        let data = noise(len, seed);
        write(&tree.join(rel), &data);
        for l in links {
            std::fs::hard_link(tree.join(rel), tree.join(l)).unwrap();
        }
        for n in std::iter::once(&rel).chain(links) {
            if !n.starts_with("../") {
                out.insert(PathBuf::from(n), data.clone());
            }
        }
    };
    file("p1/a", 50_000, 1, &["p2/b"]);
    file("same/c1", 30_000, 2, &["same/c2"]);
    file("t/x", 70_000, 3, &["t/y", "u/z"]);
    file("solo", 10_000, 4, &["../outside/solo"]);
    file("plain", 5_000, 5, &[]);
    out
}

fn ino(p: &Path) -> u64 {
    meta(p).ino()
}

/// Contents equal the source's; the pairs and the triple are links again; `solo` and
/// `plain` are single files (A-HL-1).
fn check_link_tree(dst: &Path, orig: &BTreeMap<PathBuf, Vec<u8>>, ctx: &str) {
    let d = |r: &str| dst.join("tree").join(r);
    for (rel, data) in orig {
        assert_eq!(
            &std::fs::read(dst.join("tree").join(rel)).unwrap(),
            data,
            "{ctx}: {rel:?}"
        );
    }
    for (group, n) in [
        (&["p1/a", "p2/b"][..], 2),
        (&["same/c1", "same/c2"][..], 2),
        (&["t/x", "t/y", "u/z"][..], 3),
    ] {
        let first = ino(&d(group[0]));
        for r in group {
            assert_eq!(
                ino(&d(r)),
                first,
                "{ctx}: {r} shares the inode of {}",
                group[0]
            );
            assert_eq!(meta(&d(r)).nlink(), n, "{ctx}: {r} link count");
        }
    }
    assert_eq!(
        meta(&d("solo")).nlink(),
        1,
        "{ctx}: solo is copied on its own"
    );
    assert_eq!(meta(&d("plain")).nlink(), 1, "{ctx}");
    let distinct: std::collections::HashSet<u64> = ["p1/a", "same/c1", "t/x", "solo", "plain"]
        .iter()
        .map(|r| ino(&d(r)))
        .collect();
    assert_eq!(distinct.len(), 5, "{ctx}: one inode per source inode");
}

#[test]
fn a_hl_1_copy_keeps_hard_links() {
    let t = test_dir("fid-hl1-copy");
    let orig = link_tree(&t.path);
    let mut cases = vec![("btrfs", t.join("dst"))];
    let x = xdev_dir("fid-hl1-copy");
    if let Some(x) = &x {
        cases.push(("tmpfs", x.join("dst")));
    }
    for (ctx, dst) in cases {
        std::fs::create_dir_all(&dst).unwrap();
        let r = copy(
            &Sys::default(),
            &mut Script::silent(),
            &t.path,
            &["tree"],
            &dst,
        );
        assert_eq!(
            (r.done, r.dirs_done, r.failed, r.skipped),
            (9, 6, 0, 0),
            "{ctx}: {r:?}"
        );
        assert!(r.notes.is_empty(), "{ctx}: {:?}", r.notes);
        check_link_tree(&dst, &orig, ctx);
        no_partials(&[&dst]);
    }
    // The source is untouched, and its outside link still shares the inode.
    assert_eq!(meta(&t.join("tree/solo")).nlink(), 2);
    assert_eq!(meta(&t.join("tree/t/x")).nlink(), 3);
}

/// Moves `tree` from `src_root` to `dst` across filesystems and checks A-HL-1.
fn hl1_move(src_root: &Path, dst: &Path, ctx: &str) {
    let orig = link_tree(src_root);
    let r = mv(
        &Sys::default(),
        &mut Script::silent(),
        src_root,
        &["tree"],
        dst,
    );
    assert_eq!(
        (r.done, r.dirs_done, r.failed, r.skipped),
        (9, 6, 0, 0),
        "{ctx}: {r:?}"
    );
    assert!(
        !r.issues
            .iter()
            .any(|i| matches!(&i.outcome, Outcome::Failed(w) if w.contains("source changed"))),
        "{ctx}: {r:?}"
    );
    // No "kept, it still holds entries" for a directory that held a deferred name, and no
    // link fallback.
    assert!(r.notes.is_empty(), "{ctx}: {:?}", r.notes);
    check_link_tree(dst, &orig, ctx);
    assert!(
        !exists(&src_root.join("tree")),
        "{ctx}: every source name and directory is gone: {:?}",
        walk(&src_root.join("tree"))
    );
    let outside = src_root.join("outside/solo");
    assert_eq!(std::fs::read(&outside).unwrap(), orig[Path::new("solo")]);
    assert_eq!(meta(&outside).nlink(), 1, "{ctx}: the outside link remains");
    no_partials(&[dst]);
}

#[test]
fn a_hl_1_cross_filesystem_move_keeps_hard_links() {
    let Some(x) = xdev_dir("fid-hl1-move") else {
        return;
    };
    let t = test_dir("fid-hl1-move");
    std::fs::create_dir_all(t.join("src")).unwrap();
    std::fs::create_dir_all(x.join("dst")).unwrap();
    hl1_move(&t.join("src"), &x.join("dst"), "btrfs -> tmpfs");
    std::fs::create_dir_all(x.join("src")).unwrap();
    std::fs::create_dir_all(t.join("dst")).unwrap();
    hl1_move(&x.join("src"), &t.join("dst"), "tmpfs -> btrfs");
}

#[test]
fn a_skipped_partner_settles_the_deferred_name() {
    // `a` and `b` are one inode; `b` exists at the destination and is skipped. `a` waits
    // for `b` to settle, then goes: the check still sees the link count of its copy.
    let Some(x) = xdev_dir("fid-skip-partner") else {
        return;
    };
    let t = test_dir("fid-skip-partner");
    let data = noise(20_000, 9);
    write(&t.join("a"), &data);
    std::fs::hard_link(t.join("a"), t.join("b")).unwrap();
    write(&x.join("b"), b"existing");
    let mut ui = Script::new([Answer::Skip]);
    let r = mv(&Sys::default(), &mut ui, &t.path, &["a", "b"], &x.path);
    assert!(
        matches!(ui.asked[..], [Question::FileExists { .. }]),
        "{:?}",
        ui.asked
    );
    assert_eq!((r.done, r.skipped, r.failed), (1, 1, 0), "{r:?}");
    assert!(!exists(&t.join("a")), "the deferred name was unlinked");
    assert_eq!(
        std::fs::read(t.join("b")).unwrap(),
        data,
        "the skipped name stays"
    );
    assert_eq!(std::fs::read(x.join("a")).unwrap(), data);
    assert_eq!(std::fs::read(x.join("b")).unwrap(), b"existing");
    no_partials(&[&x.path]);
}

#[test]
fn two_names_of_one_inode_into_one_destination_name() {
    // Two groups select `d1/f` and `d2/f`, one inode. The second raises "file exists" for
    // the first's copy; Overwrite leaves that name with the same inode and no temporary.
    let t = test_dir("fid-two-names");
    std::fs::create_dir_all(t.join("d1")).unwrap();
    std::fs::create_dir_all(t.join("d2")).unwrap();
    std::fs::create_dir_all(t.join("dst")).unwrap();
    let data = noise(8_000, 3);
    write(&t.join("d1/f"), &data);
    std::fs::hard_link(t.join("d1/f"), t.join("d2/f")).unwrap();
    let groups = vec![
        Group::new(t.join("d1"), names(&["f"])),
        Group::new(t.join("d2"), names(&["f"])),
    ];
    let mut ui = Script::new([Answer::Overwrite]);
    let r = run_guarded(
        JobSpec::Copy {
            groups,
            dst: t.join("dst").into(),
        },
        &Sys::default(),
        &mut ui,
    );
    assert!(
        matches!(ui.asked[..], [Question::FileExists { .. }]),
        "{:?}",
        ui.asked
    );
    assert_eq!((r.done, r.failed), (2, 0), "{r:?}");
    assert_eq!(std::fs::read(t.join("dst/f")).unwrap(), data);
    assert_eq!(meta(&t.join("dst/f")).nlink(), 1);
    no_partials(&[&t.join("dst")]);
}

#[cfg(feature = "failpoints")]
mod failpoints {
    use super::*;
    use manycommander::fsops::failpoints::{Action, Failpoints, Trigger};
    use rustix::io::Errno;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    fn sys_with(fp: &Arc<Failpoints>) -> Sys {
        Sys::with_failpoints(Arc::new(AtomicBool::new(false)), fp.clone())
    }

    // ---- A-SP-1 ------------------------------------------------------------------------

    #[test]
    fn a_sp_1_seek_data_without_hole_support_takes_the_contiguous_copy() {
        let Some(x) = xdev_dir("fid-sp1-einval") else {
            return;
        };
        let t = test_dir("fid-sp1-einval");
        sparse_set(&t.join("set"));
        let want = hash(&t.join("set/middle"));
        for errno in [Errno::INVAL, Errno::OPNOTSUPP] {
            let dst = x.join(format!("{}", errno.raw_os_error()));
            std::fs::create_dir_all(&dst).unwrap();
            let fp = Failpoints::new();
            fp.arm("copy.seekdata", Trigger::Nth(1), Action::Errno(errno));
            let r = copy(
                &sys_with(&fp),
                &mut Script::silent(),
                &t.join("set"),
                &["middle"],
                &dst,
            );
            assert_eq!((r.done, r.failed), (1, 0), "{errno:?}: {r:?}");
            assert_eq!(
                fp.hits("copy.seekdata"),
                1,
                "the sparse path stopped at its first step"
            );
            assert_eq!(fp.hits("copy.seekhole"), 0);
            assert_eq!(hash(&dst.join("middle")), want, "{errno:?}");
            assert_eq!(meta(&dst.join("middle")).len(), 2560 * K);
            // The contiguous loop wrote the holes as zeros.
            assert!(
                alloc(&dst.join("middle")) >= 2560 * K,
                "{errno:?}: the contiguous copy ran"
            );
        }
    }

    #[test]
    fn only_files_with_holes_take_the_sparse_path() {
        // P-7: a dense file costs no SEEK_DATA; a file with holes walks its segments.
        let t = test_dir("fid-sp-path");
        sparse_set(&t.join("set"));
        write(&t.join("set/dense"), &noise(300_000, 7));
        std::fs::create_dir_all(t.join("dst")).unwrap();
        let fp = Failpoints::new();
        let r = copy(
            &sys_with(&fp),
            &mut Script::silent(),
            &t.join("set"),
            &["dense"],
            &t.join("dst"),
        );
        assert_eq!(r.done, 1);
        assert_eq!(fp.hits("copy.seekdata"), 0, "{:?}", fp.all_hits());
        let fp = Failpoints::new();
        let r = copy(
            &sys_with(&fp),
            &mut Script::silent(),
            &t.join("set"),
            &["hole"],
            &t.join("dst"),
        );
        assert_eq!(r.done, 1);
        // An all-hole file: one SEEK_DATA (ENXIO), no data call, one ftruncate.
        assert_eq!(fp.hits("copy.seekdata"), 1, "{:?}", fp.all_hits());
        assert_eq!(fp.hits("copy.chunk"), 0, "{:?}", fp.all_hits());
        assert_eq!(fp.hits("copy.truncate"), 1);
        assert_eq!(meta(&t.join("dst/hole")).len(), 4096 * K);
        assert_eq!(alloc(&t.join("dst/hole")), 0);
    }

    #[test]
    fn cancel_during_a_sparse_segment_leaves_no_destination() {
        let Some(x) = xdev_dir("fid-sp-cancel") else {
            return;
        };
        let t = test_dir("fid-sp-cancel");
        sparse_set(&t.join("set"));
        let want = hash(&t.join("set/middle"));
        let fp = Failpoints::new();
        // The second data chunk: the first segment is written, the second not.
        fp.arm("copy.chunk", Trigger::Nth(3), Action::Cancel);
        let r = mv(
            &sys_with(&fp),
            &mut Script::silent(),
            &t.join("set"),
            &["middle"],
            &x.path,
        );
        assert!(fp.hits("copy.chunk") >= 3, "{:?}", fp.all_hits());
        assert!(r.cancelled, "{r:?}");
        assert!(!exists(&x.join("middle")));
        assert_eq!(hash(&t.join("set/middle")), want, "the source is kept");
        no_partials(&[&x.path]);
    }

    // ---- A-HL-1: deferral ----------------------------------------------------------------

    #[test]
    fn a_deferred_name_and_its_directory_wait_for_the_partner() {
        // `p1` finishes first; its flush must not unlink `p1/a` (its partner `p2/b` is not
        // settled), and `p1` is not removed then. The second flush (after `p2`) finds both.
        let Some(x) = xdev_dir("fid-deferred") else {
            return;
        };
        let t = test_dir("fid-deferred");
        let orig = link_tree(&t.path);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let fp = Failpoints::new();
        let (a, p1, seen2) = (t.join("tree/p1/a"), t.join("tree/p1"), seen.clone());
        fp.arm(
            "move.syncfs",
            Trigger::Nth(2),
            Action::Call(Arc::new(move || {
                seen2.lock().unwrap().push((exists(&a), exists(&p1)));
            })),
        );
        let r = mv(
            &sys_with(&fp),
            &mut Script::silent(),
            &t.path,
            &["tree"],
            &x.path,
        );
        assert_eq!(*seen.lock().unwrap(), [(true, true)], "{r:?}");
        assert_eq!((r.done, r.failed), (9, 0), "{r:?}");
        assert!(r.notes.is_empty(), "{:?}", r.notes);
        check_link_tree(&x.path, &orig, "deferred");
        assert!(!exists(&t.join("tree")));
    }

    // ---- A-HL-2 --------------------------------------------------------------------------

    /// A stranger replaces `p` atomically.
    fn replace(p: &Path, data: &[u8]) {
        let tmp = p.with_extension("stranger");
        std::fs::write(&tmp, data).unwrap();
        std::fs::rename(&tmp, p).unwrap();
    }

    fn pair(root: &Path) -> Vec<u8> {
        std::fs::create_dir_all(root.join("tree/p1")).unwrap();
        std::fs::create_dir_all(root.join("tree/p2")).unwrap();
        let data = noise(40_000, 21);
        write(&root.join("tree/p1/a"), &data);
        std::fs::hard_link(root.join("tree/p1/a"), root.join("tree/p2/b")).unwrap();
        data
    }

    #[test]
    fn a_hl_2_replaced_first_destination_is_not_linked() {
        // Replaced before the second name is reached (`link.srcstat`), and replaced between
        // the identity check and the link (`link.link`): the second name is copied.
        for step in ["link.srcstat", "link.link"] {
            let t = test_dir("fid-hl2-replaced");
            let data = pair(&t.path);
            std::fs::create_dir_all(t.join("dst")).unwrap();
            let fp = Failpoints::new();
            let first = t.join("dst/tree/p1/a");
            fp.arm(
                step,
                Trigger::Nth(1),
                Action::Call(Arc::new(move || replace(&first, b"stranger"))),
            );
            let r = copy(
                &sys_with(&fp),
                &mut Script::silent(),
                &t.path,
                &["tree"],
                &t.join("dst"),
            );
            assert_eq!(fp.hits(step), 1, "{step}: {:?}", fp.all_hits());
            assert_eq!((r.done, r.failed), (2, 0), "{step}: {r:?}");
            let (a, b) = (t.join("dst/tree/p1/a"), t.join("dst/tree/p2/b"));
            assert_eq!(std::fs::read(&a).unwrap(), b"stranger", "{step}");
            assert_eq!(std::fs::read(&b).unwrap(), data, "{step}: not the stranger");
            assert_ne!(ino(&a), ino(&b), "{step}");
            assert_eq!(meta(&b).nlink(), 1, "{step}");
            assert_eq!(
                r.notes,
                ["1 hard link was copied as a separate file"],
                "{step}"
            );
            no_partials(&[&t.join("dst")]);
        }
    }

    /// Rewrites `p` in place with `data` (same length) and restores its atime and mtime
    /// with `futimens`: only the ctime shows the change.
    fn rewrite_keep_mtime(p: &Path, data: &[u8]) {
        let before = meta(p);
        let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        f.write_all_at(data, 0).unwrap();
        let ts = |sec: i64, nsec: i64| rustix::fs::Timespec {
            tv_sec: sec,
            tv_nsec: nsec as _,
        };
        rustix::fs::futimens(
            &f,
            &rustix::fs::Timestamps {
                last_access: ts(before.atime(), before.atime_nsec()),
                last_modification: ts(before.mtime(), before.mtime_nsec()),
            },
        )
        .unwrap();
        let after = meta(p);
        assert_eq!(
            (after.len(), after.mtime(), after.mtime_nsec()),
            (before.len(), before.mtime(), before.mtime_nsec())
        );
        assert_ne!(
            (after.ctime(), after.ctime_nsec()),
            (before.ctime(), before.ctime_nsec()),
            "the ctime shows the rewrite"
        );
    }

    fn kept(r: &Report, p: &Path) -> bool {
        r.issues.iter().any(|i| {
            i.path == p && i.outcome == Outcome::Failed("source changed; kept both".into())
        })
    }

    #[test]
    fn a_hl_2_rewrite_before_the_second_name_keeps_the_first() {
        // `p1/a` is committed; then the inode is rewritten with its mtime restored. `p2/b`
        // is copied with the new bytes (not linked to the old ones). At settlement `a` no
        // longer matches its S0 and stays; `b` matches and goes. The new bytes survive at
        // the source.
        let Some(x) = xdev_dir("fid-hl2-rewrite1") else {
            return;
        };
        let t = test_dir("fid-hl2-rewrite1");
        let old = pair(&t.path);
        let new = noise(old.len(), 99);
        let fp = Failpoints::new();
        let (a, new2) = (t.join("tree/p1/a"), new.clone());
        fp.arm(
            "link.srcstat",
            Trigger::Nth(1),
            Action::Call(Arc::new(move || rewrite_keep_mtime(&a, &new2))),
        );
        let r = mv(
            &sys_with(&fp),
            &mut Script::silent(),
            &t.path,
            &["tree"],
            &x.path,
        );
        assert_eq!(fp.hits("link.srcstat"), 1);
        assert!(kept(&r, &t.join("tree/p1/a")), "{r:?}");
        assert_eq!(r.failed, 1, "{r:?}");
        assert_eq!(std::fs::read(t.join("tree/p1/a")).unwrap(), new);
        assert!(!exists(&t.join("tree/p2/b")), "b matched its S0 and went");
        assert_eq!(std::fs::read(x.join("tree/p1/a")).unwrap(), old);
        assert_eq!(std::fs::read(x.join("tree/p2/b")).unwrap(), new);
        assert!(
            r.notes
                .iter()
                .any(|n| n == "1 hard link was copied as a separate file"),
            "{:?}",
            r.notes
        );
        no_partials(&[&x.path]);
    }

    #[test]
    fn a_hl_2_rewrite_before_settlement_keeps_every_name() {
        // Both names are committed (the second as a link); the rewrite comes before the
        // flush that settles the inode. Neither name matches its S0: both stay, with the
        // new bytes.
        let Some(x) = xdev_dir("fid-hl2-rewrite2") else {
            return;
        };
        let t = test_dir("fid-hl2-rewrite2");
        std::fs::create_dir_all(t.join("tree/same")).unwrap();
        let old = noise(30_000, 5);
        write(&t.join("tree/same/c1"), &old);
        std::fs::hard_link(t.join("tree/same/c1"), t.join("tree/same/c2")).unwrap();
        let new = noise(old.len(), 55);
        let fp = Failpoints::new();
        let (c1, new2) = (t.join("tree/same/c1"), new.clone());
        fp.arm(
            "move.syncfs",
            Trigger::Nth(1),
            Action::Call(Arc::new(move || rewrite_keep_mtime(&c1, &new2))),
        );
        let r = mv(
            &sys_with(&fp),
            &mut Script::silent(),
            &t.path,
            &["tree"],
            &x.path,
        );
        assert_eq!(
            fp.hits("link.link"),
            1,
            "c2 was linked: {:?}",
            fp.all_hits()
        );
        for n in ["c1", "c2"] {
            let p = t.join("tree/same").join(n);
            assert!(kept(&r, &p), "{n}: {r:?}");
            assert_eq!(std::fs::read(&p).unwrap(), new, "{n}");
            assert_eq!(std::fs::read(x.join("tree/same").join(n)).unwrap(), old);
        }
        assert_eq!(meta(&t.join("tree/same/c1")).nlink(), 2);
        assert_eq!((r.done, r.failed), (0, 2), "{r:?}");
        no_partials(&[&x.path]);
    }

    #[test]
    fn a_replaced_deferred_name_keeps_the_stranger() {
        // A-FS-9c for a deferred name: `p1/a` is committed and deferred; another inode
        // replaces it at the source before the inode settles. The stranger is kept, and so
        // is `p2/b`, whose inode lost a link meanwhile.
        let Some(x) = xdev_dir("fid-replaced-deferred") else {
            return;
        };
        let t = test_dir("fid-replaced-deferred");
        let data = pair(&t.path);
        let fp = Failpoints::new();
        let a = t.join("tree/p1/a");
        // syncfs #1 is p1's flush; #2 is p2's, before the inode settles.
        fp.arm(
            "move.syncfs",
            Trigger::Nth(2),
            Action::Call(Arc::new(move || replace(&a, b"stranger"))),
        );
        let r = mv(
            &sys_with(&fp),
            &mut Script::silent(),
            &t.path,
            &["tree"],
            &x.path,
        );
        assert!(fp.hits("move.syncfs") >= 2, "{:?}", fp.all_hits());
        assert!(kept(&r, &t.join("tree/p1/a")), "{r:?}");
        assert!(kept(&r, &t.join("tree/p2/b")), "{r:?}");
        assert_eq!(std::fs::read(t.join("tree/p1/a")).unwrap(), b"stranger");
        assert_eq!(std::fs::read(t.join("tree/p2/b")).unwrap(), data);
        assert_eq!(std::fs::read(x.join("tree/p1/a")).unwrap(), data);
        assert_eq!(std::fs::read(x.join("tree/p2/b")).unwrap(), data);
        no_partials(&[&x.path]);
    }

    // ---- A-HL-3 --------------------------------------------------------------------------

    #[test]
    fn a_hl_3_link_failure_copies_the_data() {
        let x = xdev_dir("fid-hl3");
        for errno in [Errno::MLINK, Errno::PERM] {
            let t = test_dir("fid-hl3");
            let orig = link_tree(&t.path);
            std::fs::create_dir_all(t.join("dst")).unwrap();
            let fp = Failpoints::new();
            fp.arm("link.link", Trigger::Always, Action::Errno(errno));
            let r = copy(
                &sys_with(&fp),
                &mut Script::silent(),
                &t.path,
                &["tree"],
                &t.join("dst"),
            );
            // b, c2, y and z would have been links.
            assert_eq!(fp.hits("link.link"), 4, "{errno:?}");
            assert_eq!((r.done, r.failed), (9, 0), "{errno:?}: {r:?}");
            assert_eq!(r.notes, ["4 hard links were copied as separate files"]);
            for (rel, data) in &orig {
                let p = t.join("dst/tree").join(rel);
                assert_eq!(&std::fs::read(&p).unwrap(), data, "{errno:?}: {rel:?}");
                assert_eq!(meta(&p).nlink(), 1, "{errno:?}: {rel:?}");
            }
            no_partials(&[&t.join("dst")]);

            // A cross-filesystem move with the same fallback: every source goes, and the
            // deferred check still matches (the copies do not touch the source inode).
            let Some(x) = &x else {
                continue;
            };
            let dst = x.join(format!("{}", errno.raw_os_error()));
            std::fs::create_dir_all(&dst).unwrap();
            let fp = Failpoints::new();
            fp.arm("link.link", Trigger::Always, Action::Errno(errno));
            let r = mv(
                &sys_with(&fp),
                &mut Script::silent(),
                &t.path,
                &["tree"],
                &dst,
            );
            assert_eq!((r.done, r.failed), (9, 0), "{errno:?}: {r:?}");
            assert_eq!(r.notes, ["4 hard links were copied as separate files"]);
            assert!(!exists(&t.join("tree")), "{errno:?}");
            for (rel, data) in &orig {
                assert_eq!(&std::fs::read(dst.join("tree").join(rel)).unwrap(), data);
            }
        }
    }

    // ---- the fidelity sweep (I-1) ----------------------------------------------------------

    /// The sweep tree: a file with holes, a pair across directories, a pair in one
    /// directory, and a plain file.
    fn sweep_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        for d in ["tree/p1", "tree/p2", "tree/same"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let tree = root.join("tree");
        sparse(
            &tree.join("holes"),
            &[(0, 64 * K), (1024 * K, 64 * K)],
            1536 * K,
            3,
        );
        write(&tree.join("p1/a"), &noise(40_000, 1));
        std::fs::hard_link(tree.join("p1/a"), tree.join("p2/b")).unwrap();
        write(&tree.join("same/c1"), &noise(20_000, 2));
        std::fs::hard_link(tree.join("same/c1"), tree.join("same/c2")).unwrap();
        write(&tree.join("plain"), &noise(3_000, 4));
        ["holes", "p1/a", "p2/b", "same/c1", "same/c2", "plain"]
            .iter()
            .map(|r| (PathBuf::from(r), std::fs::read(tree.join(r)).unwrap()))
            .collect()
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum State {
        Moved,
        SourceOnly,
        Both,
    }

    /// The A-FS-5 general predicate over names: no content lost or corrupted, no temporary
    /// names.
    fn states(
        src: &Path,
        dst: &Path,
        orig: &BTreeMap<PathBuf, Vec<u8>>,
    ) -> BTreeMap<PathBuf, State> {
        let mut out = BTreeMap::new();
        for (rel, data) in orig {
            let (s, d) = (src.join("tree").join(rel), dst.join("tree").join(rel));
            let (se, de) = (exists(&s), exists(&d));
            if se {
                assert_eq!(&std::fs::read(&s).unwrap(), data, "source {rel:?} changed");
            }
            if de {
                assert_eq!(
                    &std::fs::read(&d).unwrap(),
                    data,
                    "destination {rel:?} differs"
                );
            }
            let st = match (se, de) {
                (true, true) => State::Both,
                (true, false) => State::SourceOnly,
                (false, true) => State::Moved,
                (false, false) => panic!("{rel:?} is lost"),
            };
            out.insert(rel.clone(), st);
        }
        no_partials(&[src, dst]);
        out
    }

    /// A file's commit path (M1 4.7 and its amendment): the unnamed temporary file, the named
    /// one (a filesystem without `O_TMPFILE`), or direct write (neither `RENAME_NOREPLACE`
    /// nor hard links, nor `O_TMPFILE`).
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Mode {
        Unnamed,
        Named,
        Direct,
    }

    impl Mode {
        fn arm(self, fp: &Failpoints) {
            if self != Mode::Unnamed {
                fp.arm(
                    "copy.tmpfile",
                    Trigger::Always,
                    Action::Errno(Errno::OPNOTSUPP),
                );
            }
            if self == Mode::Direct {
                fp.arm(
                    "commit.rename",
                    Trigger::Always,
                    Action::Errno(Errno::INVAL),
                );
                fp.arm("commit.link", Trigger::Always, Action::Errno(Errno::PERM));
            }
        }
    }

    fn one(
        mode: Mode,
        inject: Option<(&str, u64, Action)>,
    ) -> (
        Report,
        Arc<Failpoints>,
        BTreeMap<PathBuf, State>,
        Vec<Question>,
        PathBuf,
    ) {
        let t = test_dir("fid-sweep");
        let x = xdev_dir("fid-sweep").expect("MC_XDEV_DIR");
        let orig = sweep_tree(&t.path);
        let fp = Failpoints::new();
        mode.arm(&fp);
        if let Some((step, n, action)) = &inject {
            fp.arm(step, Trigger::Nth(*n), action.clone());
        }
        let mut ui = Script::new([]);
        ui.fallback = Answer::Skip;
        let r = mv(&sys_with(&fp), &mut ui, &t.path, &["tree"], &x.path);
        if let Some((step, n, _)) = &inject {
            assert!(
                fp.hits(step) >= *n,
                "{step} #{n} was not reached: {:?}",
                fp.all_hits()
            );
        }
        let st = states(&t.path, &x.path, &orig);
        if st.values().all(|s| *s == State::Moved) && mode != Mode::Direct && r.notes.is_empty() {
            // Every name moved and no fallback: the pairs are links at the destination.
            let d = |r: &str| x.join("tree").join(r);
            assert_eq!(ino(&d("p1/a")), ino(&d("p2/b")));
            assert_eq!(ino(&d("same/c1")), ino(&d("same/c2")));
        }
        (r, fp, st, ui.asked, t.path.clone())
    }

    #[test]
    fn fidelity_failpoint_sweep() {
        if xdev_dir("fid-sweep-probe").is_none() {
            return;
        }
        let mut runs = 0;
        for mode in [Mode::Unnamed, Mode::Named, Mode::Direct] {
            let (r, fp, st, _, _) = one(mode, None);
            assert!(
                st.values().all(|s| *s == State::Moved),
                "clean run: {st:?} {r:?}"
            );
            assert_eq!(r.failed, 0, "{r:?}");
            // The unnamed file commits each first name with `linkat`; a later name of a
            // hard-linked inode is linked under a named temporary name and renamed.
            let commit_steps: &[&str] = match mode {
                Mode::Unnamed => &["copy.tmpfile", "commit.linkat", "commit.rename"],
                Mode::Named => &["commit.rename"],
                Mode::Direct => &["commit.direct"],
            };
            let mut steps = vec![
                "copy.seekdata",
                "copy.seekhole",
                "copy.chunk",
                "copy.write",
                "copy.truncate",
                "move.syncfs",
                "move.statx",
                "move.linkstat",
                "move.unlink",
                "move.rmdir",
            ];
            steps.extend(commit_steps);
            if mode != Mode::Direct {
                steps.extend(["link.srcstat", "link.open", "link.link"]);
            }
            for step in steps {
                let hits = fp.hits(step);
                assert!(hits > 0, "{step} never reached in a clean run");
                for n in 1..=hits {
                    for cancel in [true, false] {
                        let action = if cancel {
                            Action::Cancel
                        } else {
                            Action::Errno(Errno::IO)
                        };
                        let (r, _fp, st, asked, src_root) = one(mode, Some((step, n, action)));
                        runs += 1;
                        let ctx = format!("{mode:?} {step} #{n} cancel={cancel}: {st:?} {r:?}");
                        let count = |s: State| st.values().filter(|v| **v == s).count();
                        match (step, cancel) {
                            // A cancel completes the batch in progress and settles every
                            // deferred name: nothing is left in both places.
                            (_, true) => {
                                assert_eq!(count(State::Both), 0, "{ctx}");
                            }
                            ("move.syncfs", false) => {
                                // No source is unlinked after the failed syncfs: every
                                // name in both places is one the report names as kept.
                                let kept: Vec<_> = r
                                    .issues
                                    .iter()
                                    .filter(|i| matches!(&i.outcome, Outcome::Failed(w) if w.contains("syncfs")))
                                    .map(|i| i.path.clone())
                                    .collect();
                                for k in &kept {
                                    let rel = k
                                        .strip_prefix(&src_root)
                                        .unwrap()
                                        .strip_prefix("tree")
                                        .unwrap();
                                    assert_eq!(st[rel], State::Both, "{ctx}");
                                }
                                assert_eq!(count(State::Both), kept.len(), "{ctx}");
                            }
                            ("move.statx" | "move.linkstat" | "move.unlink", false) => {
                                assert_eq!(count(State::Both), 1, "that name keeps both: {ctx}");
                                assert_eq!(count(State::SourceOnly), 0, "{ctx}");
                            }
                            ("move.rmdir", false) => {
                                assert!(st.values().all(|s| *s == State::Moved), "{ctx}");
                                assert!(r.notes.iter().any(|n| n.contains("not removed")), "{ctx}");
                            }
                            // The first destination cannot be checked: the name is copied.
                            ("link.srcstat" | "link.open", false) => {
                                assert!(st.values().all(|s| *s == State::Moved), "{ctx}");
                                assert_eq!(
                                    r.notes,
                                    ["1 hard link was copied as a separate file"],
                                    "{ctx}"
                                );
                            }
                            // An I/O error while copying, linking or committing: the error
                            // question, then that name stays at the source only.
                            (_, false) => {
                                assert!(
                                    asked.iter().any(|q| matches!(
                                        q,
                                        Question::Error {
                                            errno: Errno::IO,
                                            ..
                                        }
                                    )),
                                    "{ctx}"
                                );
                                assert_eq!(count(State::Both), 0, "{ctx}");
                                assert_eq!(count(State::SourceOnly), 1, "{ctx}");
                            }
                        }
                    }
                }
            }
        }
        assert!(runs > 50, "the sweep ran {runs} injections");
    }
}
