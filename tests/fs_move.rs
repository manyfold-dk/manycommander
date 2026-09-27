//! T4: move (F6) with group commit. A-FS-4 (move), 5, 6, 7, 9, 10 (move), 11, 12 (move),
//! 13 (move), the mount-point skip, batch sizing and the case-only rename.

mod common;

use common::*;
use manycommander::fsops::job::{JobSpec, Outcome, Report, run_guarded};
use manycommander::fsops::question::{Answer, Question};
use manycommander::fsops::sys::Sys;
#[cfg(feature = "failpoints")]
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};

fn mv(sys: &Sys, ui: &mut Script, src: &Path, names: &[&[u8]], dst: &Path) -> Report {
    let names = names
        .iter()
        .map(|n| OsString::from_vec(n.to_vec()))
        .collect();
    run_guarded(
        JobSpec::Move {
            src_dir: src.to_path_buf(),
            names,
            dst: dst.to_path_buf(),
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
                "a temporary file remained: {p:?}"
            );
        }
    }
}

#[cfg(feature = "failpoints")]
/// The sweep tree: multi-chunk files (the read/write fallback uses 1 MiB chunks across
/// filesystems), small files, an empty file, nested directories and a symlink.
fn sweep_tree(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let files: [(&str, usize); 6] = [
        ("a", 1_600_000),
        ("b", 10_000),
        ("c", 0),
        ("sub/d", 2_300_000),
        ("sub/e", 5_000),
        ("sub/deeper/f", 100),
    ];
    std::fs::create_dir_all(root.join("tree/sub/deeper")).unwrap();
    let mut out = BTreeMap::new();
    for (i, (n, len)) in files.iter().enumerate() {
        let data = noise(*len, i as u64 + 11);
        write(&root.join("tree").join(n), &data);
        out.insert(PathBuf::from(n), data);
    }
    symlink("a", root.join("tree/link")).unwrap();
    out
}

#[cfg(feature = "failpoints")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Moved,
    SourceOnly,
    Both,
}

#[cfg(feature = "failpoints")]
/// The general A-FS-5 predicate: no content lost or corrupted, no temporary names.
fn states(src: &Path, dst: &Path, orig: &BTreeMap<PathBuf, Vec<u8>>) -> BTreeMap<PathBuf, State> {
    let mut out = BTreeMap::new();
    for (rel, data) in orig {
        let (s, d) = (src.join("tree").join(rel), dst.join("tree").join(rel));
        let (se, de) = (s.exists(), d.exists());
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
    let (sl, dl) = (src.join("tree/link"), dst.join("tree/link"));
    let (se, de) = (sl.symlink_metadata().is_ok(), dl.symlink_metadata().is_ok());
    for l in [&sl, &dl] {
        if l.symlink_metadata().is_ok() {
            assert_eq!(std::fs::read_link(l).unwrap(), Path::new("a"));
        }
    }
    let st = match (se, de) {
        (true, true) => State::Both,
        (true, false) => State::SourceOnly,
        (false, true) => State::Moved,
        (false, false) => panic!("the link is lost"),
    };
    out.insert(PathBuf::from("link"), st);
    no_partials(&[src, dst]);
    out
}

#[test]
fn a_fs_6_same_filesystem_move_keeps_inodes() {
    let t = test_dir("move-afs6");
    std::fs::create_dir_all(t.join("src/d/inner")).unwrap();
    std::fs::create_dir_all(t.join("dst")).unwrap();
    write(&t.join("src/d/inner/f"), b"x");
    std::fs::hard_link(t.join("src/d/inner/f"), t.join("src/d/g")).unwrap();
    let ino = std::fs::metadata(t.join("src/d")).unwrap().ino();
    let fino = std::fs::metadata(t.join("src/d/inner/f")).unwrap().ino();
    let r = mv(
        &Sys::default(),
        &mut Script::silent(),
        &t.join("src"),
        &[b"d"],
        &t.join("dst"),
    );
    assert_eq!((r.done, r.dirs_done, r.failed), (2, 2, 0), "{r:?}");
    assert_eq!(std::fs::metadata(t.join("dst/d")).unwrap().ino(), ino);
    let (f, g) = (
        std::fs::metadata(t.join("dst/d/inner/f")).unwrap(),
        std::fs::metadata(t.join("dst/d/g")).unwrap(),
    );
    assert_eq!((f.ino(), g.ino(), f.nlink()), (fino, fino, 2));
    assert!(!t.join("src/d").exists());
}

#[test]
fn a_fs_4_move_into_descendant_writes_nothing() {
    let t = test_dir("move-afs4");
    std::fs::create_dir_all(t.join("src/a/b")).unwrap();
    write(&t.join("src/a/f"), b"x");
    let before = walk(&t.path);
    let r = mv(
        &Sys::default(),
        &mut Script::silent(),
        &t.join("src"),
        &[b"a"],
        &t.join("src/a/b"),
    );
    assert!(r.refused.is_some());
    assert_eq!(walk(&t.path), before);
}

#[test]
fn a_fs_11_directory_exists_and_type_mismatch() {
    let t = test_dir("move-afs11");
    std::fs::create_dir_all(t.join("src/d/sub")).unwrap();
    std::fs::create_dir_all(t.join("dst/d/sub")).unwrap();
    write(&t.join("src/d/one"), b"1");
    write(&t.join("src/d/sub/two"), b"2");
    write(&t.join("dst/d/keep"), b"k");
    write(&t.join("src/f"), b"file");
    std::fs::create_dir(t.join("dst/f")).unwrap();
    let mut ui = Script::new([Answer::Merge, Answer::Merge, Answer::Skip]);
    let r = mv(
        &Sys::default(),
        &mut ui,
        &t.join("src"),
        &[b"d", b"f"],
        &t.join("dst"),
    );
    assert!(
        matches!(ui.asked[0], Question::DirExists { .. }),
        "{:?}",
        ui.asked
    );
    assert!(matches!(ui.asked[1], Question::DirExists { .. }));
    assert!(matches!(ui.asked[2], Question::TypeMismatch { .. }));
    assert_eq!(std::fs::read(t.join("dst/d/one")).unwrap(), b"1");
    assert_eq!(std::fs::read(t.join("dst/d/sub/two")).unwrap(), b"2");
    assert_eq!(std::fs::read(t.join("dst/d/keep")).unwrap(), b"k");
    assert!(
        !t.join("src/d").exists(),
        "the emptied source directory is removed"
    );
    assert_eq!(std::fs::read(t.join("src/f")).unwrap(), b"file");
    assert!(t.join("dst/f").is_dir());
    assert_eq!(r.skipped, 1);
}

#[test]
fn a_fs_10_hostile_names_survive_move() {
    let long = vec![b'M'; 255];
    let names: Vec<&[u8]> = vec![
        b"new\nline",
        b"-leading",
        b"it's",
        b"bad\xff\xfeutf8",
        &long,
    ];
    for cross in [false, true] {
        let t = test_dir("move-afs10");
        let x;
        let dst = if cross {
            x = match xdev_dir("move-afs10") {
                Some(x) => x,
                None => return,
            };
            x.path.clone()
        } else {
            std::fs::create_dir_all(t.join("dst")).unwrap();
            t.join("dst")
        };
        std::fs::create_dir_all(t.join("src")).unwrap();
        let mut hashes = Vec::new();
        for (i, n) in names.iter().enumerate() {
            let data = noise(2000 + i, i as u64);
            write(&t.join("src").join(common::os(n)), &data);
            hashes.push(data);
        }
        let r = mv(
            &Sys::default(),
            &mut Script::silent(),
            &t.join("src"),
            &names,
            &dst,
        );
        assert_eq!((r.done, r.failed), (5, 0), "cross={cross}: {r:?}");
        for (n, data) in names.iter().zip(&hashes) {
            assert_eq!(&std::fs::read(dst.join(common::os(n))).unwrap(), data);
            assert!(!t.join("src").join(common::os(n)).exists());
        }
        no_partials(&[&dst]);
    }
}

#[test]
fn a_fs_7_move_between_unprivileged_subvolumes() {
    if !require_btrfs() {
        return;
    }
    let t = test_dir("move-afs7");
    subvolume_create(&t.join("A"));
    subvolume_create(&t.join("B"));
    std::fs::create_dir_all(t.join("A/tree/dir")).unwrap();
    write(&t.join("A/tree/file"), &noise(100_000, 1));
    write(&t.join("A/tree/dir/inner"), &noise(1000, 2));
    symlink("file", t.join("A/tree/link")).unwrap();
    subvolume_create(&t.join("A/tree/nested"));
    write(&t.join("A/tree/nested/n"), &noise(5000, 3));
    let a_dev = std::fs::metadata(t.join("A/tree")).unwrap().dev();
    let hashes: Vec<_> = ["file", "dir/inner", "nested/n"]
        .iter()
        .map(|f| hash(&t.join("A/tree").join(f)))
        .collect();
    let r = mv(
        &Sys::default(),
        &mut Script::silent(),
        &t.join("A"),
        &[b"tree"],
        &t.join("B"),
    );
    assert_eq!(r.failed, 0, "{r:?}");
    // Subvolumes have their own st_dev and restart inode numbers, so the device shows
    // that the tree was copied into B, which a rename cannot do.
    let b_dev = std::fs::metadata(t.join("B")).unwrap().dev();
    assert_eq!(std::fs::metadata(t.join("B/tree")).unwrap().dev(), b_dev);
    assert_ne!(b_dev, a_dev);
    for (f, h) in ["file", "dir/inner", "nested/n"].iter().zip(hashes) {
        assert_eq!(hash(&t.join("B/tree").join(f)), h, "{f}");
    }
    assert_eq!(
        std::fs::read_link(t.join("B/tree/link")).unwrap(),
        Path::new("file")
    );
    assert!(
        !r.issues
            .iter()
            .any(|i| matches!(&i.outcome, Outcome::Skipped(w) if w == "mount point"))
    );
    // The source is empty, except the nested subvolume root if the kernel refuses to
    // remove it.
    let left: Vec<_> = walk(&t.join("A"));
    assert!(
        left.is_empty() || left == [t.join("A/tree"), t.join("A/tree/nested")],
        "left in source: {left:?}"
    );
}

#[test]
fn batch_limit_flushes_by_count() {
    use manycommander::fsops::mv::BATCH_FILES;
    let Some(x) = xdev_dir("move-batch") else {
        return;
    };
    let t = test_dir("move-batch");
    std::fs::create_dir_all(t.join("tree")).unwrap();
    let n = 2 * BATCH_FILES + 22;
    for i in 0..n {
        write(&t.join(format!("tree/f{i:04}")), &noise(100, i as u64));
    }
    let r = mv(
        &Sys::default(),
        &mut Script::silent(),
        &t.path,
        &[b"tree"],
        &x.path,
    );
    assert_eq!((r.done, r.failed, r.dirs_done), (n as u64, 0, 1), "{r:?}");
    assert!(!t.join("tree").exists());
    assert_eq!(walk(&x.join("tree")).len(), n);
}

#[test]
fn bind_mount_inside_moved_tree_is_skipped() {
    if !in_userns("bind_mount_inside_moved_tree_is_skipped") {
        return;
    }
    let Some(x) = xdev_dir("move-mount") else {
        return;
    };
    let t = test_dir("move-mount");
    std::fs::create_dir_all(t.join("tree/m")).unwrap();
    std::fs::create_dir_all(t.join("other")).unwrap();
    write(&t.join("other/precious"), b"p");
    write(&t.join("tree/f"), b"f");
    bind_mount(&t.join("other"), &t.join("tree/m"));
    let r = mv(
        &Sys::default(),
        &mut Script::silent(),
        &t.path,
        &[b"tree"],
        &x.path,
    );
    let m = r
        .issues
        .iter()
        .find(|i| i.path.ends_with("tree/m"))
        .expect("mount point reported");
    assert_eq!(m.outcome, Outcome::Skipped("mount point".into()));
    assert_eq!(std::fs::read(t.join("other/precious")).unwrap(), b"p");
    assert_eq!(
        std::fs::read(t.join("tree/m/precious")).unwrap(),
        b"p",
        "still mounted, not copy-deleted"
    );
    assert!(!x.join("tree/m/precious").exists());
    assert_eq!(std::fs::read(x.join("tree/f")).unwrap(), b"f");
    assert!(r.notes.iter().any(|n| n.contains("kept")), "{:?}", r.notes);
    umount(&t.join("tree/m"));
}

#[test]
fn a_fs_9b_real_writer_during_cross_filesystem_move() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    if !require_btrfs() {
        return;
    }
    let Some(x) = xdev_dir("move-afs9b") else {
        return;
    };
    let t = test_dir("move-afs9b");
    let f = t.join("growing");
    // The writer starts appending once the destination's temporary file exists, so every
    // append lands after the engine recorded S0. A run where the writer got no append in
    // before the move returned proves nothing and is repeated.
    for attempt in 0..5 {
        write(&f, &noise(256 << 20, attempt));
        let stop = Arc::new(AtomicBool::new(false));
        let writes = Arc::new(AtomicU64::new(0));
        let writer = {
            let (f, stop, writes, dst) = (f.clone(), stop.clone(), writes.clone(), x.path.clone());
            std::thread::spawn(move || {
                use std::io::Write;
                let partial = || {
                    std::fs::read_dir(&dst).unwrap().flatten().any(|e| {
                        e.file_name()
                            .as_bytes()
                            .windows(12)
                            .any(|w| w == b".mc-partial-")
                    })
                };
                while !partial() {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    std::thread::yield_now();
                }
                let mut h = std::fs::OpenOptions::new().append(true).open(&f).unwrap();
                while !stop.load(Ordering::SeqCst) {
                    h.write_all(b"+").unwrap();
                    writes.fetch_add(1, Ordering::SeqCst);
                }
            })
        };
        let r = mv(
            &Sys::default(),
            &mut Script::silent(),
            &t.path,
            &[b"growing"],
            &x.path,
        );
        let during = writes.load(Ordering::SeqCst);
        stop.store(true, Ordering::SeqCst);
        writer.join().unwrap();
        if during == 0 {
            eprintln!("attempt {attempt}: the writer got no append in; repeating");
            let _ = std::fs::remove_file(x.join("growing"));
            continue;
        }
        assert_eq!(r.failed, 1, "{r:?}");
        match &r.issues[0].outcome {
            // Caught before the commit: nothing committed.
            Outcome::Failed(w) if w.contains("source changed during move") => {
                assert!(!x.join("growing").exists(), "nothing was committed");
            }
            // Caught at the flush: both kept.
            Outcome::Failed(w) if w.contains("kept both") => {}
            o => panic!("{o:?}"),
        }
        assert!(f.exists(), "the source is kept");
        no_partials(&[&x.path]);
        return;
    }
    panic!("the writer never overlapped the copy in five attempts");
}

#[test]
fn case_rename_second_step_failure_names_intermediate_path() {
    // Needs a case-insensitive filesystem to arise from a plan; the mechanism is tested
    // directly. The second step fails here because the target exists.
    let t = test_dir("move-case");
    write(&t.join("Foo"), b"x");
    write(&t.join("foo"), b"other");
    let sys = Sys::default();
    let dir = manycommander::fsops::copy::Dir::open_root(&sys, &t.path).unwrap();
    let err = manycommander::fsops::mv::case_rename(&sys, &dir, "Foo".as_ref(), "foo".as_ref())
        .unwrap_err();
    assert!(err.contains(".mc-case-"), "{err}");
    let inter = walk(&t.path)
        .into_iter()
        .find(|p| {
            p.file_name()
                .unwrap()
                .as_bytes()
                .windows(9)
                .any(|w| w == b".mc-case-")
        })
        .unwrap();
    assert!(err.contains(&inter.display().to_string()));
    assert_eq!(std::fs::read(inter).unwrap(), b"x");
    assert_eq!(std::fs::read(t.join("foo")).unwrap(), b"other");
}

#[cfg(feature = "failpoints")]
mod failpoints {
    use super::*;
    use manycommander::fsops::failpoints::{Action, Failpoints, Trigger};
    use rustix::io::Errno;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    fn sys_with(fp: &Arc<Failpoints>) -> Sys {
        Sys::with_failpoints(Arc::new(AtomicBool::new(false)), fp.clone())
    }

    fn direct_mode(fp: &Failpoints) {
        fp.arm(
            "commit.rename",
            Trigger::Always,
            Action::Errno(Errno::INVAL),
        );
        fp.arm("commit.link", Trigger::Always, Action::Errno(Errno::PERM));
    }

    /// Runs one cross-filesystem move of the sweep tree with `inject` armed.
    fn one(
        direct: bool,
        inject: Option<(&str, u64, Action)>,
    ) -> (
        Report,
        Arc<Failpoints>,
        BTreeMap<PathBuf, State>,
        Vec<Question>,
    ) {
        let t = test_dir("move-sweep");
        let x = xdev_dir("move-sweep").expect("MC_XDEV_DIR");
        let orig = sweep_tree(&t.path);
        let fp = Failpoints::new();
        if direct {
            direct_mode(&fp);
        }
        if let Some((step, n, action)) = &inject {
            fp.arm(step, Trigger::Nth(*n), action.clone());
        }
        let mut ui = Script::new([]);
        ui.fallback = Answer::Skip;
        let r = mv(&sys_with(&fp), &mut ui, &t.path, &[b"tree"], &x.path);
        if let Some((step, n, _)) = &inject {
            assert!(
                fp.hits(step) >= *n,
                "{step} #{n} was not reached: {:?}",
                fp.all_hits()
            );
        }
        let st = states(&t.path, &x.path, &orig);
        (r, fp, st, ui.asked)
    }

    #[test]
    fn a_fs_5_failpoint_sweep() {
        if xdev_dir("move-sweep-probe").is_none() {
            return;
        }
        let mut runs = 0;
        let src_root = tmp_root().join(format!("move-sweep-{}", std::process::id()));
        for direct in [false, true] {
            let (r, fp, st, _) = one(direct, None);
            assert!(
                st.values().all(|s| *s == State::Moved),
                "clean run: {st:?} {r:?}"
            );
            let commit_step = if direct {
                "commit.direct"
            } else {
                "commit.rename"
            };
            let steps = [
                "copy.chunk",
                commit_step,
                "move.syncfs",
                "move.statx",
                "move.unlink",
            ];
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
                        let (r, _fp, st, asked) = one(direct, Some((step, n, action)));
                        runs += 1;
                        let ctx =
                            format!("direct={direct} {step} #{n} cancel={cancel}: {st:?} {r:?}");
                        let count = |s: State| st.values().filter(|v| **v == s).count();
                        match (step, cancel) {
                            // A cancel completes the batch in progress: nothing is left in
                            // both places. Cancel at the first chunk leaves every source in
                            // place and no destination for that file.
                            (_, true) => {
                                // A cancel after the job's last checkpoint (the final chunk
                                // of the last file) finds nothing left to stop.
                                assert!(r.cancelled || count(State::Moved) == st.len(), "{ctx}");
                                assert_eq!(count(State::Both), 0, "{ctx}");
                                if step == "copy.chunk" && n == 1 {
                                    assert_eq!(count(State::Moved), 0, "{ctx}");
                                }
                            }
                            ("move.syncfs", false) => {
                                // No source of that batch is unlinked; the job ends. Every
                                // entry the failure names still exists in both places (a
                                // flush may also hold only created directories).
                                assert!(r.notes.iter().any(|n| n.contains("syncfs")), "{ctx}");
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
                            ("move.statx" | "move.unlink", false) => {
                                assert_eq!(count(State::Both), 1, "that entry keeps both: {ctx}");
                                assert_eq!(count(State::SourceOnly), 0, "{ctx}");
                            }
                            // An I/O error while copying or committing: the error question,
                            // then that file stays at the source only.
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

    #[test]
    fn a_fs_9a_snapshot_differs_at_precommit_check() {
        let Some(x) = xdev_dir("move-afs9a") else {
            return;
        };
        let t = test_dir("move-afs9a");
        write(&t.join("f"), &noise(100_000, 1));
        let fp = Failpoints::new();
        let f = t.join("f");
        fp.arm(
            "move.check",
            Trigger::Nth(1),
            Action::Call(Arc::new(move || {
                use std::io::Write;
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&f)
                    .unwrap()
                    .write_all(b"more")
                    .unwrap();
            })),
        );
        let r = mv(
            &sys_with(&fp),
            &mut Script::silent(),
            &t.path,
            &[b"f"],
            &x.path,
        );
        assert_eq!(fp.hits("move.check"), 1);
        assert_eq!(r.failed, 1);
        assert!(t.join("f").exists());
        assert!(!x.join("f").exists(), "no destination is committed");
        no_partials(&[&x.path]);
    }

    #[test]
    fn a_fs_9c_source_replaced_between_commit_and_flush() {
        let Some(x) = xdev_dir("move-afs9c") else {
            return;
        };
        let t = test_dir("move-afs9c");
        write(&t.join("f"), b"original");
        let fp = Failpoints::new();
        let dir = t.path.clone();
        fp.arm(
            "move.syncfs",
            Trigger::Nth(1),
            Action::Call(Arc::new(move || {
                std::fs::write(dir.join("f.new"), b"replacement").unwrap();
                std::fs::rename(dir.join("f.new"), dir.join("f")).unwrap();
            })),
        );
        let r = mv(
            &sys_with(&fp),
            &mut Script::silent(),
            &t.path,
            &[b"f"],
            &x.path,
        );
        assert_eq!(r.failed, 1, "{r:?}");
        assert_eq!(
            r.issues[0].outcome,
            Outcome::Failed("source changed; kept both".into())
        );
        assert_eq!(
            std::fs::read(t.join("f")).unwrap(),
            b"replacement",
            "the new inode is kept"
        );
        assert_eq!(std::fs::read(x.join("f")).unwrap(), b"original");
    }

    #[test]
    fn a_fs_12_move_in_direct_write_mode() {
        let Some(x) = xdev_dir("move-afs12") else {
            return;
        };
        let t = test_dir("move-afs12");
        for n in ["a", "b", "c"] {
            write(&t.join(n), &noise(40_000, n.as_bytes()[0] as u64));
        }
        write(&x.join("b"), b"existing");
        let fp = Failpoints::new();
        direct_mode(&fp);
        // a: the temporary file's commit fails, the file is retried in direct-write mode,
        // and the cancel arrives as its destination name is created.
        fp.arm("commit.direct", Trigger::Nth(1), Action::Cancel);
        let mut ui = Script::new([Answer::Skip]);
        let r = mv(
            &sys_with(&fp),
            &mut ui,
            &t.path,
            &[b"a", b"b", b"c"],
            &x.path,
        );
        assert_eq!(fp.hits("commit.direct"), 1, "direct-write mode ran");
        assert!(r.cancelled);
        assert!(
            t.join("a").exists(),
            "the cancelled file's source is not unlinked"
        );
        assert!(
            !x.join("a").exists(),
            "the cancelled file left no destination name"
        );
        assert_eq!(std::fs::read(x.join("b")).unwrap(), b"existing");
        no_partials(&[&x.path]);

        // Without a cancel, direct-write mode completes and unlinks the sources after the
        // flush; an existing name is not overwritten without an answer.
        let fp = Failpoints::new();
        direct_mode(&fp);
        let mut ui = Script::new([Answer::Skip]);
        let r = mv(
            &sys_with(&fp),
            &mut ui,
            &t.path,
            &[b"a", b"b", b"c"],
            &x.path,
        );
        assert_eq!((r.done, r.skipped), (2, 1), "{r:?}");
        assert!(!t.join("a").exists() && !t.join("c").exists() && t.join("b").exists());
        assert_eq!(std::fs::read(x.join("b")).unwrap(), b"existing");
        assert_eq!(
            std::fs::read(x.join("a")).unwrap(),
            noise(40_000, b'a' as u64)
        );
        assert_eq!(
            std::fs::read(x.join("c")).unwrap(),
            noise(40_000, b'c' as u64)
        );
    }

    #[test]
    fn a_fs_13_swapped_directory_during_move() {
        let Some(x) = xdev_dir("move-afs13") else {
            return;
        };
        let t = test_dir("move-afs13");
        std::fs::create_dir_all(t.join("src/a/b")).unwrap();
        write(&t.join("src/a/b/inner"), b"inner");
        std::fs::create_dir_all(t.join("outside")).unwrap();
        write(&t.join("outside/secret"), b"secret");
        let fp = Failpoints::new();
        let (b, out, moved) = (t.join("src/a/b"), t.join("outside"), t.join("b-moved"));
        fp.arm(
            "walk.openat",
            Trigger::Nth(2),
            Action::Call(Arc::new(move || {
                std::fs::rename(&b, &moved).unwrap();
                symlink(&out, &b).unwrap();
            })),
        );
        let r = mv(
            &sys_with(&fp),
            &mut Script::silent(),
            &t.join("src"),
            &[b"a"],
            &x.path,
        );
        assert_eq!(fp.hits("walk.openat"), 2);
        let b = r
            .issues
            .iter()
            .find(|i| i.path.ends_with("a/b"))
            .expect("b reported");
        assert_eq!(b.outcome, Outcome::Failed("type changed".into()));
        assert_eq!(std::fs::read(t.join("outside/secret")).unwrap(), b"secret");
        assert!(!x.join("a/b/secret").exists());
        assert!(
            t.join("src/a/b")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link is not moved through"
        );
    }

    #[test]
    fn directory_only_tree_is_synced_before_sources_go() {
        // Review finding 1: no file is pending, yet the created directories must be
        // durable before the source directories are removed.
        let Some(x) = xdev_dir("move-dirs-only") else {
            return;
        };
        let t = test_dir("move-dirs-only");
        std::fs::create_dir_all(t.join("tree/a/b")).unwrap();
        std::fs::create_dir_all(t.join("tree/c")).unwrap();
        let fp = Failpoints::new();
        let r = mv(
            &sys_with(&fp),
            &mut Script::silent(),
            &t.path,
            &[b"tree"],
            &x.path,
        );
        assert_eq!((r.dirs_done, r.failed), (4, 0), "{r:?}");
        assert!(fp.hits("move.syncfs") >= 1, "{:?}", fp.all_hits());
        assert!(x.join("tree/a/b").is_dir() && x.join("tree/c").is_dir());
        assert!(!t.join("tree").exists());
        assert!(r.summary().contains("4 directories"), "{}", r.summary());
    }

    #[test]
    fn replaced_source_directory_is_not_removed() {
        // Review finding 4: the rmdir acts on the emptied directory only.
        let Some(x) = xdev_dir("move-dir-replaced") else {
            return;
        };
        let t = test_dir("move-dir-replaced");
        std::fs::create_dir_all(t.join("tree/a")).unwrap();
        let fp = Failpoints::new();
        let (a, aside) = (t.join("tree/a"), t.join("aside"));
        fp.arm(
            "move.dirstat",
            Trigger::Nth(1),
            Action::Call(Arc::new(move || {
                std::fs::rename(&a, &aside).unwrap();
                std::fs::create_dir(&a).unwrap();
            })),
        );
        let r = mv(
            &sys_with(&fp),
            &mut Script::silent(),
            &t.path,
            &[b"tree"],
            &x.path,
        );
        assert!(t.join("tree/a").is_dir(), "the replacement is kept");
        assert!(
            r.notes
                .iter()
                .any(|n| n.contains("replaced during the move")),
            "{r:?}"
        );
    }

    #[test]
    fn directory_metadata_failure_keeps_the_source_directory() {
        // Review finding 6.
        let Some(x) = xdev_dir("move-dir-meta") else {
            return;
        };
        let t = test_dir("move-dir-meta");
        std::fs::create_dir_all(t.join("d")).unwrap();
        write(&t.join("d/f"), b"f");
        let fp = Failpoints::new();
        // chmod #1 is the file, #2 the directory.
        fp.arm("copy.chmod", Trigger::Nth(2), Action::Errno(Errno::PERM));
        let r = mv(
            &sys_with(&fp),
            &mut Script::silent(),
            &t.path,
            &[b"d"],
            &x.path,
        );
        assert_eq!(fp.hits("copy.chmod"), 2);
        assert!(t.join("d").is_dir(), "the source directory is kept: {r:?}");
        assert!(!t.join("d/f").exists(), "the moved file's source is gone");
        assert_eq!(std::fs::read(x.join("d/f")).unwrap(), b"f");
        assert!(r.issues.iter().any(
            |i| matches!(&i.outcome, Outcome::Failed(w) if w.contains("source directory kept"))
        ));
    }

    #[test]
    fn remaining_counts_after_a_skipped_directory() {
        // Review finding 2: a skipped directory takes its subtree, not one entry more.
        let Some(x) = xdev_dir("move-remaining") else {
            return;
        };
        let t = test_dir("move-remaining");
        std::fs::create_dir_all(t.join("a")).unwrap();
        for n in ["a/1", "a/2", "a/3", "b", "c"] {
            write(&t.join(n), n.as_bytes());
        }
        std::fs::create_dir_all(x.join("a")).unwrap();
        let fp = Failpoints::new();
        // The first file opened is b (a is skipped): cancel there.
        fp.arm("open.opath", Trigger::Nth(1), Action::Cancel);
        let mut ui = Script::new([Answer::Skip]);
        let r = mv(
            &sys_with(&fp),
            &mut ui,
            &t.path,
            &[b"a", b"b", b"c"],
            &x.path,
        );
        assert!(r.cancelled);
        assert_eq!((r.planned, r.skipped, r.remaining()), (5, 1, 2), "{r:?}");
        assert!(r.summary().contains("2 still at source"), "{}", r.summary());
    }

    #[test]
    fn case_rename_second_step_eio() {
        let t = test_dir("move-case-eio");
        write(&t.join("Foo"), b"x");
        let fp = Failpoints::new();
        fp.arm("move.case2", Trigger::Nth(1), Action::Errno(Errno::IO));
        let sys = sys_with(&fp);
        let dir = manycommander::fsops::copy::Dir::open_root(&sys, &t.path).unwrap();
        let err = manycommander::fsops::mv::case_rename(&sys, &dir, "Foo".as_ref(), "foo".as_ref())
            .unwrap_err();
        assert!(err.contains("Input/output error"), "{err}");
        let inter = walk(&t.path).pop().unwrap();
        assert!(err.contains(&inter.display().to_string()), "{err}");
        assert_eq!(std::fs::read(inter).unwrap(), b"x");
    }
}
