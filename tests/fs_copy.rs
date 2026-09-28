//! T3: copy (F5) and make directory (F7). A-FS-1, 2, 3, 4, 8, 10 (copy), 12 (copy),
//! 13 (copy), A-P-8, traversal errnos and an injected panic.

mod common;

use common::*;
use manycommander::fsops::group::Group;
use manycommander::fsops::job::{JobSpec, Outcome, Report, run_guarded};
use manycommander::fsops::question::{Answer, PROGRESS_INTERVAL, Question};
use manycommander::fsops::sys::Sys;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

fn copy(sys: &Sys, ui: &mut Script, src: &Path, names: &[&[u8]], dst: &Path) -> Report {
    let names = names
        .iter()
        .map(|n| OsString::from_vec(n.to_vec()))
        .collect();
    run_guarded(
        JobSpec::Copy {
            groups: vec![Group::new(src, names)],
            dst: dst.to_path_buf(),
        },
        sys,
        ui,
    )
}

fn set_times(p: &Path, sec: i64, nsec: i64) {
    let ts = rustix::fs::Timespec {
        tv_sec: sec,
        tv_nsec: nsec as _,
    };
    rustix::fs::utimensat(
        rustix::fs::CWD,
        p,
        &rustix::fs::Timestamps {
            last_access: ts,
            last_modification: ts,
        },
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .unwrap();
}

fn mtime_ns(p: &Path) -> (i64, i64) {
    let m = std::fs::symlink_metadata(p).unwrap();
    (m.mtime(), m.mtime_nsec())
}

fn partials(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in walk(dir) {
        if e.file_name()
            .unwrap()
            .as_bytes()
            .windows(12)
            .any(|w| w == b".mc-partial-")
        {
            out.push(e);
        }
    }
    out
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
    out
}

/// The A-FS-1 tree: regular files, an empty directory, a read-only directory, relative and
/// absolute symlinks, a broken symlink and a FIFO, with nanosecond mtimes.
fn a_fs_1_tree(root: &Path) {
    let t = root.join("tree");
    std::fs::create_dir_all(t.join("empty")).unwrap();
    std::fs::create_dir_all(t.join("ro")).unwrap();
    write(&t.join("f1"), &noise(100_000, 1));
    write(&t.join("f2"), b"");
    write(&t.join("big"), &noise(3 << 20, 2));
    write(&t.join("ro/g"), &noise(5000, 3));
    std::fs::set_permissions(t.join("f1"), std::fs::Permissions::from_mode(0o640)).unwrap();
    std::fs::set_permissions(t.join("big"), std::fs::Permissions::from_mode(0o755)).unwrap();
    symlink("f1", t.join("rel")).unwrap();
    symlink(t.join("f1"), t.join("abs")).unwrap();
    symlink("does-not-exist", t.join("broken")).unwrap();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        t.join("fifo"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o644),
        0,
    )
    .unwrap();
    for (i, n) in ["f1", "f2", "big", "ro/g", "empty", "rel"]
        .iter()
        .enumerate()
    {
        set_times(&t.join(n), 1_600_000_000 + i as i64, 123_456_789 + i as i64);
    }
    set_times(&t.join("ro"), 1_500_000_000, 987_654_321);
    std::fs::set_permissions(t.join("ro"), std::fs::Permissions::from_mode(0o555)).unwrap();
    set_times(&t, 1_400_000_000, 111_111_111);
}

fn check_a_fs_1(src: &Path, dst: &Path, r: &Report) {
    let (s, d) = (src.join("tree"), dst.join("tree"));
    for f in ["f1", "f2", "big", "ro/g"] {
        assert_eq!(hash(&s.join(f)), hash(&d.join(f)), "{f} content");
        let (sm, dm) = (
            std::fs::metadata(s.join(f)).unwrap(),
            std::fs::metadata(d.join(f)).unwrap(),
        );
        assert_eq!(sm.mode() & 0o7777, dm.mode() & 0o7777, "{f} mode");
        assert_eq!(mtime_ns(&s.join(f)), mtime_ns(&d.join(f)), "{f} mtime");
    }
    for l in ["rel", "abs", "broken"] {
        let m = std::fs::symlink_metadata(d.join(l)).unwrap();
        assert!(m.file_type().is_symlink(), "{l} is a link");
        assert_eq!(
            std::fs::read_link(s.join(l)).unwrap(),
            std::fs::read_link(d.join(l)).unwrap()
        );
    }
    assert!(std::fs::symlink_metadata(d.join("empty")).unwrap().is_dir());
    assert_eq!(mtime_ns(&s.join("empty")), mtime_ns(&d.join("empty")));
    assert_eq!(
        std::fs::metadata(d.join("ro")).unwrap().mode() & 0o7777,
        0o555
    );
    assert_eq!(mtime_ns(&s.join("ro")), mtime_ns(&d.join("ro")));
    assert_eq!(
        mtime_ns(&s),
        mtime_ns(&d),
        "the tree's own mtime, set in post-order"
    );
    assert!(
        std::fs::symlink_metadata(d.join("fifo")).is_err(),
        "the FIFO is not copied"
    );
    let fifo: Vec<_> = r
        .issues
        .iter()
        .filter(|i| i.path.ends_with("fifo"))
        .collect();
    assert_eq!(fifo.len(), 1);
    assert_eq!(fifo[0].outcome, Outcome::Skipped("special file".into()));
    assert_eq!(r.done, 7, "{r:?}");
    assert_eq!(r.skipped, 1);
    assert_eq!(r.failed, 0);
    assert!(!r.cancelled);
    assert!(partials(dst).is_empty());
}

fn a_fs_1(src_root: &Path, dst_root: &Path) {
    a_fs_1_tree(src_root);
    std::fs::create_dir_all(dst_root).unwrap();
    let mut ui = Script::silent();
    let r = copy(&Sys::default(), &mut ui, src_root, &[b"tree"], dst_root);
    assert!(ui.asked.is_empty(), "{:?}", ui.asked);
    check_a_fs_1(src_root, dst_root, &r);
}

#[test]
fn a_fs_1_copy_tree_btrfs() {
    let t = test_dir("copy-afs1");
    a_fs_1(&t.join("src"), &t.join("dst"));
}

#[test]
fn a_fs_1_copy_tree_xdev_dir() {
    let Some(x) = xdev_dir("copy-afs1") else {
        return;
    };
    a_fs_1(&x.join("src"), &x.join("dst"));
}

#[test]
fn a_fs_1_copy_tree_across_filesystems() {
    let Some(x) = xdev_dir("copy-afs1-cross") else {
        return;
    };
    let t = test_dir("copy-afs1-cross");
    a_fs_1(&t.join("src"), &x.join("dst"));
    let t2 = test_dir("copy-afs1-cross-back");
    a_fs_1(&x.join("src"), &t2.join("dst"));
}

#[test]
fn a_fs_2_overwrite_keeps_other_hard_link() {
    let t = test_dir("copy-afs2");
    std::fs::create_dir_all(t.join("src")).unwrap();
    std::fs::create_dir_all(t.join("dst")).unwrap();
    write(&t.join("src/f"), b"new content");
    write(&t.join("other"), b"old content");
    std::fs::hard_link(t.join("other"), t.join("dst/f")).unwrap();
    let mut ui = Script::new([Answer::Overwrite]);
    let r = copy(
        &Sys::default(),
        &mut ui,
        &t.join("src"),
        &[b"f"],
        &t.join("dst"),
    );
    assert!(matches!(ui.asked[0], Question::FileExists { .. }));
    assert_eq!(r.done, 1);
    assert_eq!(std::fs::read(t.join("dst/f")).unwrap(), b"new content");
    assert_eq!(std::fs::read(t.join("other")).unwrap(), b"old content");
}

#[test]
fn a_fs_3_same_inode_is_refused() {
    let t = test_dir("copy-afs3");
    std::fs::create_dir_all(t.join("src/d")).unwrap();
    std::fs::create_dir_all(t.join("dst/d")).unwrap();
    let content = noise(10_000, 4);
    write(&t.join("src/d/f"), &content);
    write(&t.join("src/top"), &content);
    std::fs::hard_link(t.join("src/d/f"), t.join("dst/d/f")).unwrap();
    std::fs::hard_link(t.join("src/top"), t.join("dst/top")).unwrap();
    // Nested, reached through Merge: the engine's own check.
    let mut ui = Script::new([Answer::Merge]);
    let r = copy(
        &Sys::default(),
        &mut ui,
        &t.join("src"),
        &[b"d", b"top"],
        &t.join("dst"),
    );
    assert_eq!(r.skipped, 2, "{r:?}");
    assert_eq!(std::fs::read(t.join("src/d/f")).unwrap(), content);
    assert_eq!(std::fs::read(t.join("src/top")).unwrap(), content);
    // The same path.
    let r = copy(
        &Sys::default(),
        &mut Script::silent(),
        &t.join("src"),
        &[b"top"],
        &t.join("src"),
    );
    assert_eq!(r.skipped, 1);
    assert_eq!(std::fs::read(t.join("src/top")).unwrap(), content);
}

#[test]
fn a_fs_4_copy_into_descendant_writes_nothing() {
    let t = test_dir("copy-afs4");
    std::fs::create_dir_all(t.join("src/a/b")).unwrap();
    write(&t.join("src/a/f"), b"x");
    let before = walk(&t.path);
    let r = copy(
        &Sys::default(),
        &mut Script::silent(),
        &t.join("src"),
        &[b"a"],
        &t.join("src/a/b"),
    );
    assert!(r.refused.is_some());
    assert_eq!(walk(&t.path), before, "nothing was written");
}

#[test]
fn a_fs_10_hostile_names_survive_copy() {
    let t = test_dir("copy-afs10");
    std::fs::create_dir_all(t.join("src")).unwrap();
    std::fs::create_dir_all(t.join("dst")).unwrap();
    let long = vec![b'L'; 255];
    let names: Vec<&[u8]> = vec![
        b"new\nline",
        b"-leading",
        b"it's",
        b"bad\xff\xfeutf8",
        &long,
    ];
    for (i, n) in names.iter().enumerate() {
        write(
            &t.join("src").join(common::os(n)),
            &noise(1000 + i, i as u64),
        );
    }
    std::fs::create_dir(t.join("src").join(common::os(b"dir\nwith\xffname"))).unwrap();
    write(
        &t.join("src")
            .join(common::os(b"dir\nwith\xffname"))
            .join(common::os(&long)),
        b"inner",
    );
    let mut all = names.clone();
    all.push(b"dir\nwith\xffname");
    let r = copy(
        &Sys::default(),
        &mut Script::silent(),
        &t.join("src"),
        &all,
        &t.join("dst"),
    );
    assert_eq!(r.failed, 0, "{r:?}");
    for n in &names {
        let (s, d) = (
            t.join("src").join(common::os(n)),
            t.join("dst").join(common::os(n)),
        );
        assert_eq!(hash(&s), hash(&d));
    }
    let inner = t
        .join("dst")
        .join(common::os(b"dir\nwith\xffname"))
        .join(common::os(&long));
    assert_eq!(std::fs::read(inner).unwrap(), b"inner");
    assert!(partials(&t.join("dst")).is_empty());
}

#[test]
fn rename_answer_and_symlink_destination() {
    let t = test_dir("copy-rename");
    std::fs::create_dir_all(t.join("src")).unwrap();
    std::fs::create_dir_all(t.join("dst")).unwrap();
    write(&t.join("src/a.txt"), b"src");
    write(&t.join("dst/a.txt"), b"dst");
    write(&t.join("target"), b"target");
    write(&t.join("src/l"), b"file over link");
    symlink(t.join("target"), t.join("dst/l")).unwrap();
    let mut ui = Script::new([Answer::Rename("a (1).txt".into()), Answer::Overwrite]);
    let r = copy(
        &Sys::default(),
        &mut ui,
        &t.join("src"),
        &[b"a.txt", b"l"],
        &t.join("dst"),
    );
    assert_eq!(r.done, 2, "{r:?}");
    assert_eq!(std::fs::read(t.join("dst/a.txt")).unwrap(), b"dst");
    assert_eq!(std::fs::read(t.join("dst/a (1).txt")).unwrap(), b"src");
    match &ui.asked[1] {
        Question::FileExists { dst_is_symlink, .. } => assert!(dst_is_symlink),
        q => panic!("{q:?}"),
    }
    assert!(
        !std::fs::symlink_metadata(t.join("dst/l"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read(t.join("target")).unwrap(),
        b"target",
        "the link's target is untouched"
    );
}

#[test]
fn overwrite_all_older_compares_mtimes() {
    let t = test_dir("copy-older");
    std::fs::create_dir_all(t.join("src")).unwrap();
    std::fs::create_dir_all(t.join("dst")).unwrap();
    for (n, s, d) in [
        ("newer", 2000, 1000),
        ("same", 1000, 1000),
        ("older", 1000, 2000),
    ] {
        write(&t.join("src").join(n), b"src");
        write(&t.join("dst").join(n), b"dst");
        set_times(&t.join("src").join(n), s, 5);
        set_times(&t.join("dst").join(n), d, 5);
    }
    let mut ui = Script::new([Answer::OverwriteAllOlder]);
    let r = copy(
        &Sys::default(),
        &mut ui,
        &t.join("src"),
        &[b"newer", b"older", b"same"],
        &t.join("dst"),
    );
    assert_eq!(ui.asked.len(), 1);
    assert_eq!(r.done, 1);
    assert_eq!(r.skipped, 2);
    assert_eq!(std::fs::read(t.join("dst/newer")).unwrap(), b"src");
    assert_eq!(std::fs::read(t.join("dst/same")).unwrap(), b"dst");
    assert_eq!(std::fs::read(t.join("dst/older")).unwrap(), b"dst");
}

#[test]
fn single_source_to_new_name() {
    let t = test_dir("copy-newname");
    std::fs::create_dir_all(t.join("src")).unwrap();
    write(&t.join("src/f"), b"x");
    let r = copy(
        &Sys::default(),
        &mut Script::silent(),
        &t.join("src"),
        &[b"f"],
        &t.join("src/g"),
    );
    assert_eq!(r.done, 1);
    assert_eq!(std::fs::read(t.join("src/g")).unwrap(), b"x");
    let r = copy(
        &Sys::default(),
        &mut Script::silent(),
        &t.join("src"),
        &[b"f", b"g"],
        &t.join("nowhere"),
    );
    assert!(r.refused.is_some());
}

#[test]
fn a_p_8_progress_is_capped_at_15_hz() {
    let Some(x) = xdev_dir("copy-ap8") else {
        return;
    };
    let t = test_dir("copy-ap8");
    write(&x.join("big"), &noise(96 << 20, 9));
    let mut ui = Script::silent();
    let start = std::time::Instant::now();
    let r = copy(&Sys::default(), &mut ui, &x.path, &[b"big"], &t.path);
    let elapsed = start.elapsed();
    assert_eq!(r.done, 1);
    assert!(!ui.progress.is_empty());
    for w in ui.progress.windows(2) {
        assert!(
            w[1].0.duration_since(w[0].0) >= PROGRESS_INTERVAL,
            "two updates closer than 1/15 s"
        );
    }
    let limit = (elapsed.as_secs_f64() * 15.0).ceil() as usize + 1;
    assert!(
        ui.progress.len() <= limit,
        "{} updates in {elapsed:?}",
        ui.progress.len()
    );
}

#[test]
fn mkdir_creates_parents_and_reports_existing() {
    let t = test_dir("mkdir");
    let sys = Sys::default();
    let r = run_guarded(
        JobSpec::Mkdir {
            dir: t.path.clone(),
            name: "a/b/c".into(),
        },
        &sys,
        &mut Script::silent(),
    );
    assert_eq!(r.done, 1);
    assert_eq!(r.focus.as_deref(), Some(std::ffi::OsStr::new("a")));
    assert!(t.join("a/b/c").is_dir());
    let r = run_guarded(
        JobSpec::Mkdir {
            dir: t.path.clone(),
            name: "a".into(),
        },
        &sys,
        &mut Script::silent(),
    );
    assert_eq!(r.done, 0);
    assert_eq!(r.skipped, 1);
    assert_eq!(r.focus.as_deref(), Some(std::ffi::OsStr::new("a")));
    for bad in ["", ".", "..", "x/../y"] {
        let r = run_guarded(
            JobSpec::Mkdir {
                dir: t.path.clone(),
                name: bad.into(),
            },
            &sys,
            &mut Script::silent(),
        );
        assert!(r.refused.is_some(), "{bad:?}");
    }
    // A symlinked parent is not followed.
    std::fs::create_dir(t.join("elsewhere")).unwrap();
    symlink(t.join("elsewhere"), t.join("link")).unwrap();
    let r = run_guarded(
        JobSpec::Mkdir {
            dir: t.path.clone(),
            name: "link/x".into(),
        },
        &sys,
        &mut Script::silent(),
    );
    assert_eq!(r.failed, 1);
    assert!(!t.join("elsewhere/x").exists());
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

    #[test]
    fn a_fs_8_destination_appears_before_commit() {
        let t = test_dir("copy-afs8");
        std::fs::create_dir_all(t.join("src")).unwrap();
        std::fs::create_dir_all(t.join("dst")).unwrap();
        write(&t.join("src/f"), b"ours");
        let fp = Failpoints::new();
        let intruder = t.join("dst/f");
        fp.arm(
            "commit.rename",
            Trigger::Nth(1),
            Action::Call(Arc::new(move || {
                std::fs::write(&intruder, b"theirs").unwrap()
            })),
        );
        let mut ui = Script::new([Answer::Skip]);
        let r = copy(
            &sys_with(&fp),
            &mut ui,
            &t.join("src"),
            &[b"f"],
            &t.join("dst"),
        );
        assert_eq!(fp.hits("commit.rename"), 1);
        assert!(
            matches!(ui.asked[..], [Question::FileExists { .. }]),
            "{:?}",
            ui.asked
        );
        assert_eq!(r.skipped, 1);
        assert_eq!(std::fs::read(t.join("dst/f")).unwrap(), b"theirs");
        assert!(partials(&t.join("dst")).is_empty());
    }

    #[test]
    fn a_fs_12_direct_write_mode() {
        let t = test_dir("copy-afs12");
        std::fs::create_dir_all(t.join("src")).unwrap();
        std::fs::create_dir_all(t.join("dst")).unwrap();
        for n in ["a", "b", "c"] {
            write(
                &t.join("src").join(n),
                &noise(50_000, n.as_bytes()[0] as u64),
            );
        }
        symlink("a", t.join("src/l")).unwrap();
        write(&t.join("dst/b"), b"existing");
        let fp = Failpoints::new();
        fp.arm(
            "commit.rename",
            Trigger::Always,
            Action::Errno(Errno::INVAL),
        );
        fp.arm("commit.link", Trigger::Always, Action::Errno(Errno::PERM));
        let mut ui = Script::new([Answer::Skip]);
        let r = copy(
            &sys_with(&fp),
            &mut ui,
            &t.join("src"),
            &[b"a", b"b", b"c", b"l"],
            &t.join("dst"),
        );
        assert!(
            fp.hits("commit.direct") >= 3,
            "direct-write mode ran: {:?}",
            fp.all_hits()
        );
        assert_eq!(
            fp.hits("commit.link"),
            1,
            "the mode is remembered after the first failure"
        );
        assert_eq!(r.done, 3, "{r:?}");
        assert_eq!(
            std::fs::read(t.join("dst/b")).unwrap(),
            b"existing",
            "no overwrite without an answer"
        );
        assert_eq!(
            hash(&t.join("dst/a")),
            hash(&t.join("src/a")),
            "the file that took the fallback path"
        );
        assert_eq!(hash(&t.join("dst/c")), hash(&t.join("src/c")));
        assert_eq!(std::fs::read_link(t.join("dst/l")).unwrap(), Path::new("a"));
        assert!(partials(&t.join("dst")).is_empty());

        // Cancel during a direct write leaves no destination name.
        let fp = Failpoints::new();
        fp.arm(
            "commit.rename",
            Trigger::Always,
            Action::Errno(Errno::INVAL),
        );
        fp.arm("commit.link", Trigger::Always, Action::Errno(Errno::PERM));
        // Hits 1-2 copy into the temporary file before the commit fails; hit 3 is the
        // first chunk of the direct write.
        fp.arm("copy.chunk", Trigger::Nth(3), Action::Cancel);
        write(&t.join("src/d"), &noise(50_000, 7));
        let r = copy(
            &sys_with(&fp),
            &mut Script::silent(),
            &t.join("src"),
            &[b"d"],
            &t.join("dst"),
        );
        assert!(r.cancelled);
        assert_eq!(fp.hits("commit.direct"), 1);
        assert!(!t.join("dst/d").exists(), "the cancelled file left no name");
        assert!(partials(&t.join("dst")).is_empty());
    }

    #[test]
    fn a_fs_13_swapped_directory_during_copy() {
        let t = test_dir("copy-afs13");
        std::fs::create_dir_all(t.join("src/a/b")).unwrap();
        write(&t.join("src/a/b/inner"), b"inner");
        std::fs::create_dir_all(t.join("outside")).unwrap();
        write(&t.join("outside/secret"), b"secret");
        std::fs::create_dir_all(t.join("dst")).unwrap();
        let fp = Failpoints::new();
        let (b, out, moved) = (t.join("src/a/b"), t.join("outside"), t.join("b-moved"));
        // Execution opens `a` (hit 1), then `b` (hit 2).
        fp.arm(
            "walk.openat",
            Trigger::Nth(2),
            Action::Call(Arc::new(move || {
                std::fs::rename(&b, &moved).unwrap();
                symlink(&out, &b).unwrap();
            })),
        );
        let r = copy(
            &sys_with(&fp),
            &mut Script::silent(),
            &t.join("src"),
            &[b"a"],
            &t.join("dst"),
        );
        assert_eq!(fp.hits("walk.openat"), 2);
        let b = r
            .issues
            .iter()
            .find(|i| i.path.ends_with("a/b"))
            .expect("b reported");
        assert_eq!(b.outcome, Outcome::Failed("type changed".into()));
        assert!(!t.join("dst/a/b/secret").exists());
        assert_eq!(std::fs::read(t.join("outside/secret")).unwrap(), b"secret");
    }

    #[test]
    fn traversal_errnos_fail_the_entry_and_the_job_continues() {
        for errno in [Errno::MFILE, Errno::NOMEM, Errno::NAMETOOLONG] {
            let t = test_dir("copy-errno");
            std::fs::create_dir_all(t.join("src")).unwrap();
            std::fs::create_dir_all(t.join("dst")).unwrap();
            write(&t.join("src/a"), b"a");
            write(&t.join("src/b"), b"b");
            let fp = Failpoints::new();
            fp.arm("open.opath", Trigger::Nth(1), Action::Errno(errno));
            let mut ui = Script::new([Answer::Skip]);
            let r = copy(
                &sys_with(&fp),
                &mut ui,
                &t.join("src"),
                &[b"a", b"b"],
                &t.join("dst"),
            );
            assert!(matches!(ui.asked[..], [Question::Error { errno: e, .. }] if e == errno));
            assert_eq!((r.done, r.failed), (1, 1), "{errno:?}: {r:?}");
            assert!(t.join("dst/b").exists());
            assert!(!t.join("dst/a").exists());
        }
    }

    #[test]
    fn injected_panic_yields_failed_report() {
        let t = test_dir("copy-panic");
        std::fs::create_dir_all(t.join("src")).unwrap();
        std::fs::create_dir_all(t.join("dst")).unwrap();
        write(&t.join("src/f"), &noise(10_000, 1));
        let fp = Failpoints::new();
        fp.arm(
            "copy.chunk",
            Trigger::Nth(1),
            Action::Call(Arc::new(|| panic!("injected"))),
        );
        let r = copy(
            &sys_with(&fp),
            &mut Script::silent(),
            &t.join("src"),
            &[b"f"],
            &t.join("dst"),
        );
        assert_eq!(r.failed, 1);
        assert!(matches!(&r.issues[0].outcome, Outcome::Failed(m) if m.contains("injected")));
        assert!(
            partials(&t.join("dst")).is_empty(),
            "the guard removed the temporary file"
        );
    }

    #[test]
    fn copy_file_range_fallbacks() {
        for errno in [Errno::XDEV, Errno::OPNOTSUPP, Errno::NOSYS, Errno::INVAL] {
            let t = test_dir("copy-cfr");
            std::fs::create_dir_all(t.join("src")).unwrap();
            std::fs::create_dir_all(t.join("dst")).unwrap();
            write(&t.join("src/f"), &noise(3 << 20, 5));
            let fp = Failpoints::new();
            fp.arm("copy.chunk", Trigger::Nth(1), Action::Errno(errno));
            let r = copy(
                &sys_with(&fp),
                &mut Script::silent(),
                &t.join("src"),
                &[b"f"],
                &t.join("dst"),
            );
            assert_eq!(r.done, 1, "{errno:?}");
            assert_eq!(hash(&t.join("src/f")), hash(&t.join("dst/f")));
        }
    }
}
