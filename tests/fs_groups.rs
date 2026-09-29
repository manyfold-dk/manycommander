//! P2 T1: grouped sources (P2 2.2). Standing answers carry across groups for copy and
//! move (also across filesystems), same names from two groups conflict, trash and delete
//! merge groups and count over all of them, the destination-inside-source check uses the
//! union of the scanned directories, a `sub` component that became a symlink fails its
//! group, and invalid components are refused before any write.

mod common;

use common::*;
use manycommander::fsops::group::Group;
use manycommander::fsops::job::{JobSpec, Outcome, Report, run_guarded};
use manycommander::fsops::question::{Answer, Question};
use manycommander::fsops::sys::Sys;
use manycommander::fsops::trash::{decode_path, trash_groups_with};
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

fn group(root: &Path, sub: &[&str], names: &[&str]) -> Group {
    Group {
        root: root.into(),
        sub: sub.iter().map(OsString::from).collect(),
        names: names.iter().map(OsString::from).collect(),
    }
}

fn copy(ui: &mut Script, groups: Vec<Group>, dst: &Path) -> Report {
    let dst = dst.into();
    run_guarded(JobSpec::Copy { groups, dst }, &Sys::default(), ui)
}

fn mv(ui: &mut Script, groups: Vec<Group>, dst: &Path) -> Report {
    let dst = dst.into();
    run_guarded(JobSpec::Move { groups, dst }, &Sys::default(), ui)
}

fn delete(ui: &mut Script, groups: Vec<Group>) -> Report {
    run_guarded(JobSpec::Delete { groups }, &Sys::default(), ui)
}

fn trash(ui: &mut Script, groups: Vec<Group>, data: &Path) -> Report {
    trash_groups_with(&Sys::default(), ui, &groups, Some(data))
}

fn read(p: &Path) -> Vec<u8> {
    std::fs::read(p).unwrap()
}

fn exists(p: &Path) -> bool {
    p.symlink_metadata().is_ok()
}

/// Everything below `dir`, relative to it, sorted.
fn listing(dir: &Path) -> Vec<PathBuf> {
    fn go(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                out.push(p.strip_prefix(base).unwrap().to_path_buf());
                if p.symlink_metadata().map(|m| m.is_dir()).unwrap_or(false) {
                    go(base, &p, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    go(dir, dir, &mut out);
    out.sort();
    out
}

fn file_exists_paths(ui: &Script) -> Vec<PathBuf> {
    ui.asked
        .iter()
        .map(|q| match q {
            Question::FileExists { path, .. } => path.clone(),
            q => panic!("unexpected question {q:?}"),
        })
        .collect()
}

/// `src/f1`, `src/f2` and `src/d/f3`; `dst/f1` and `dst/f3` exist with old content.
fn conflicts(src: &Path, dst: &Path) {
    std::fs::create_dir_all(src.join("d")).unwrap();
    std::fs::create_dir_all(dst).unwrap();
    write(&src.join("f1"), b"new1");
    write(&src.join("f2"), b"new2");
    write(&src.join("d/f3"), b"new3");
    write(&dst.join("f1"), b"old1");
    write(&dst.join("f3"), b"old3");
}

fn conflict_groups(src: &Path) -> Vec<Group> {
    vec![group(src, &[], &["f1", "f2"]), group(src, &["d"], &["f3"])]
}

#[test]
fn copy_standing_answers_carry_across_groups() {
    for answer in [Answer::OverwriteAll, Answer::SkipAll] {
        let t = test_dir("groups-copy-standing");
        let (src, dst) = (t.join("src"), t.join("dst"));
        conflicts(&src, &dst);
        let mut ui = Script::new([answer.clone()]);
        let r = copy(&mut ui, conflict_groups(&src), &dst);
        // One question, in group 1; group 2's conflict follows the standing answer.
        assert_eq!(file_exists_paths(&ui), vec![dst.join("f1")], "{answer:?}");
        assert_eq!(r.planned, 3, "{r:?}");
        assert_eq!(read(&dst.join("f2")), b"new2");
        if answer == Answer::OverwriteAll {
            assert_eq!((r.done, r.skipped, r.failed), (3, 0, 0), "{r:?}");
            assert_eq!(read(&dst.join("f1")), b"new1");
            assert_eq!(read(&dst.join("f3")), b"new3");
        } else {
            assert_eq!((r.done, r.skipped, r.failed), (1, 2, 0), "{r:?}");
            assert_eq!(read(&dst.join("f1")), b"old1");
            assert_eq!(read(&dst.join("f3")), b"old3");
            assert_eq!(r.issues[1].path, src.join("d/f3"));
        }
        assert_eq!(r.remaining(), 0);
    }
}

#[test]
fn copy_merge_all_carries_across_groups() {
    let t = test_dir("groups-copy-merge");
    let (src, dst) = (t.join("src"), t.join("dst"));
    std::fs::create_dir_all(src.join("a")).unwrap();
    std::fs::create_dir_all(src.join("s/b")).unwrap();
    write(&src.join("a/x"), b"ax");
    write(&src.join("s/b/y"), b"by");
    std::fs::create_dir_all(dst.join("a")).unwrap();
    std::fs::create_dir_all(dst.join("b")).unwrap();
    write(&dst.join("b/kept"), b"k");
    let mut ui = Script::new([Answer::MergeAll]);
    let r = copy(
        &mut ui,
        vec![group(&src, &[], &["a"]), group(&src, &["s"], &["b"])],
        &dst,
    );
    assert_eq!(ui.asked.len(), 1, "{:?}", ui.asked);
    assert!(matches!(&ui.asked[0], Question::DirExists { path, .. } if *path == dst.join("a")));
    assert_eq!((r.done, r.failed, r.skipped), (2, 0, 0), "{r:?}");
    assert_eq!(read(&dst.join("a/x")), b"ax");
    assert_eq!(read(&dst.join("b/y")), b"by");
    assert_eq!(read(&dst.join("b/kept")), b"k");
}

#[test]
fn move_standing_answers_carry_across_groups() {
    for xdev in [false, true] {
        for answer in [Answer::OverwriteAll, Answer::SkipAll] {
            let t = test_dir("groups-move-standing");
            let x = if xdev {
                match xdev_dir("groups-move-standing") {
                    Some(x) => Some(x),
                    None => return,
                }
            } else {
                None
            };
            let src = t.join("src");
            let dst = x.as_ref().map(|x| x.join("dst")).unwrap_or(t.join("dst"));
            conflicts(&src, &dst);
            let mut ui = Script::new([answer.clone()]);
            let r = mv(&mut ui, conflict_groups(&src), &dst);
            let ctx = format!("xdev={xdev} {answer:?}: {r:?}");
            assert_eq!(file_exists_paths(&ui), vec![dst.join("f1")], "{ctx}");
            assert_eq!(read(&dst.join("f2")), b"new2", "{ctx}");
            assert!(!exists(&src.join("f2")), "{ctx}");
            if answer == Answer::OverwriteAll {
                assert_eq!((r.done, r.skipped, r.failed), (3, 0, 0), "{ctx}");
                assert_eq!(read(&dst.join("f1")), b"new1", "{ctx}");
                assert_eq!(read(&dst.join("f3")), b"new3", "{ctx}");
                assert!(
                    !exists(&src.join("f1")) && !exists(&src.join("d/f3")),
                    "{ctx}"
                );
            } else {
                assert_eq!((r.done, r.skipped, r.failed), (1, 2, 0), "{ctx}");
                assert_eq!(read(&dst.join("f1")), b"old1", "{ctx}");
                assert_eq!(read(&dst.join("f3")), b"old3", "{ctx}");
                // A skipped entry stays at its source.
                assert_eq!(read(&src.join("f1")), b"new1", "{ctx}");
                assert_eq!(read(&src.join("d/f3")), b"new3", "{ctx}");
            }
            assert_eq!(r.remaining(), 0, "{ctx}");
        }
    }
}

#[test]
fn same_names_from_two_groups_raise_file_exists_for_the_second() {
    for moving in [false, true] {
        let t = test_dir("groups-same-name");
        let (src, dst) = (t.join("src"), t.join("dst"));
        std::fs::create_dir_all(src.join("a")).unwrap();
        std::fs::create_dir_all(src.join("b")).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        write(&src.join("a/x"), b"A");
        write(&src.join("b/x"), b"B");
        let groups = vec![group(&src, &["a"], &["x"]), group(&src, &["b"], &["x"])];
        let mut ui = Script::new([Answer::Skip]);
        let r = if moving {
            mv(&mut ui, groups, &dst)
        } else {
            copy(&mut ui, groups, &dst)
        };
        assert_eq!(
            file_exists_paths(&ui),
            vec![dst.join("x")],
            "moving={moving}"
        );
        assert_eq!((r.done, r.skipped), (1, 1), "{r:?}");
        assert_eq!(r.issues[0].path, src.join("b/x"));
        assert_eq!(read(&dst.join("x")), b"A");
        assert_eq!(read(&src.join("b/x")), b"B");
        assert_eq!(exists(&src.join("a/x")), !moving);
    }
}

#[test]
fn two_group_trash_merges_groups_that_reach_one_directory() {
    let t = test_dir("groups-trash");
    let data = t.join("data");
    let work = t.join("work");
    std::fs::create_dir_all(work.join("sub")).unwrap();
    for (p, c) in [("a", "a"), ("c", "c"), ("sub/b", "b")] {
        write(&work.join(p), c.as_bytes());
    }
    // The third group reaches `work` through the component walk from `t`: it is merged
    // into the first, so `a` is trashed once and not reported as disappeared.
    let groups = vec![
        group(&work, &[], &["a"]),
        group(&work, &["sub"], &["b"]),
        group(&t.path, &["work"], &["a", "c"]),
    ];
    let mut ui = Script::silent();
    let r = trash(&mut ui, groups, &data);
    assert!(ui.asked.is_empty(), "{:?}", ui.asked);
    assert_eq!(
        (r.planned, r.done, r.failed, r.skipped),
        (3, 3, 0, 0),
        "{r:?}"
    );
    assert_eq!(r.remaining(), 0);
    for (n, c) in [("a", "a"), ("b", "b"), ("c", "c")] {
        assert_eq!(read(&data.join("Trash/files").join(n)), c.as_bytes());
    }
    assert_eq!(listing(&work), vec![PathBuf::from("sub")]);
    let info = std::fs::read_to_string(data.join("Trash/info/b.trashinfo")).unwrap();
    let path = info.lines().find_map(|l| l.strip_prefix("Path=")).unwrap();
    let canon = std::fs::canonicalize(&work).unwrap();
    assert_eq!(
        decode_path(path).unwrap(),
        canon.join("sub/b").as_os_str().as_bytes()
    );
}

#[test]
fn two_group_delete_asks_once_with_totals_over_both() {
    let t = test_dir("groups-delete");
    let work = t.join("work");
    std::fs::create_dir_all(work.join("sub/dir")).unwrap();
    write(&work.join("a"), b"123");
    write(&work.join("sub/b"), b"12345");
    write(&work.join("sub/dir/c"), b"1234567");
    let groups = || {
        vec![
            group(&work, &[], &["a"]),
            group(&work, &["sub"], &["b", "dir"]),
            // Merged into the first group; `a` is not counted twice.
            group(&t.path, &["work"], &["a"]),
        ]
    };
    let asked = |ui: &Script| {
        assert_eq!(ui.asked.len(), 1, "{:?}", ui.asked);
        assert_eq!(
            ui.asked[0],
            Question::ConfirmDelete {
                files: 3,
                dirs: 1,
                bytes: 15,
                single: None,
            }
        );
    };
    // Declined: one question, nothing deleted.
    let mut ui = Script::new([Answer::Cancel]);
    let r = delete(&mut ui, groups());
    asked(&ui);
    assert!(r.cancelled && r.done == 0, "{r:?}");
    assert_eq!(listing(&work).len(), 5);
    // Confirmed once for both groups.
    let mut ui = Script::new([Answer::Confirm]);
    let r = delete(&mut ui, groups());
    asked(&ui);
    assert_eq!((r.done, r.dirs_done, r.failed), (3, 1, 0), "{r:?}");
    assert_eq!(listing(&work), vec![PathBuf::from("sub")]);
}

#[test]
fn destination_inside_a_second_groups_source_is_refused_before_any_write() {
    let t = test_dir("groups-inside");
    let src = t.join("src");
    std::fs::create_dir_all(src.join("g/d/inner/deeper")).unwrap();
    write(&src.join("f"), b"f");
    write(&src.join("g/d/inner/x"), b"x");
    let before = listing(&src);
    for dst in [src.join("g/d/inner"), src.join("g/d/inner/deeper")] {
        let groups = || vec![group(&src, &[], &["f"]), group(&src, &["g"], &["d"])];
        for moving in [false, true] {
            let mut ui = Script::silent();
            let r = if moving {
                mv(&mut ui, groups(), &dst)
            } else {
                copy(&mut ui, groups(), &dst)
            };
            let why = r.refused.as_deref().unwrap_or_default();
            assert!(why.contains("inside the source"), "{r:?}");
            assert!(ui.asked.is_empty());
            assert_eq!(listing(&src), before, "nothing was written");
        }
    }
    // The first group alone may go there: the refusal came from the second group.
    let r = copy(
        &mut Script::silent(),
        vec![group(&src, &[], &["f"])],
        &src.join("g/d/inner"),
    );
    assert_eq!((r.done, r.refused.as_deref()), (1, None), "{r:?}");
}

#[test]
fn copy_into_a_sibling_of_a_selected_file_is_allowed() {
    for moving in [false, true] {
        let t = test_dir("groups-sibling");
        let src = t.join("src");
        std::fs::create_dir_all(src.join("a/sib")).unwrap();
        write(&src.join("a/f"), b"f");
        write(&src.join("x"), b"x");
        // Both groups' own directories (`src/a` and `src`) are ancestors of the
        // destination; neither is a selected or scanned directory.
        let groups = vec![group(&src, &["a"], &["f"]), group(&src, &[], &["x"])];
        let dst = src.join("a/sib");
        let r = if moving {
            mv(&mut Script::silent(), groups, &dst)
        } else {
            copy(&mut Script::silent(), groups, &dst)
        };
        assert_eq!((r.done, r.refused.as_deref()), (2, None), "{r:?}");
        assert_eq!(read(&dst.join("f")), b"f");
        assert_eq!(read(&dst.join("x")), b"x");
        assert_eq!(exists(&src.join("a/f")), !moving);
    }
}

/// `root/a/b` is a symlink to `other`, which holds `f`; `root/ok` is a plain file.
fn symlinked_sub(t: &TestDir) -> (PathBuf, Vec<Group>) {
    let root = t.join("root");
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(t.join("other")).unwrap();
    write(&t.join("other/f"), b"precious");
    write(&root.join("ok"), b"ok");
    symlink(t.join("other"), root.join("a/b")).unwrap();
    let groups = vec![
        group(&root, &["a", "b"], &["f"]),
        group(&root, &[], &["ok"]),
    ];
    (root, groups)
}

fn assert_group_failed(r: &Report, root: &Path) {
    assert_eq!(r.failed, 1, "{r:?}");
    assert_eq!(r.issues[0].path, root.join("a/b/f"));
    assert_eq!(r.issues[0].outcome, Outcome::Failed("type changed".into()));
}

#[test]
fn sub_component_that_is_a_symlink_fails_its_group() {
    for verb in ["copy", "move", "trash", "delete"] {
        let t = test_dir("groups-symlink-sub");
        let (root, groups) = symlinked_sub(&t);
        let dst = t.join("dst");
        std::fs::create_dir_all(&dst).unwrap();
        let mut ui = Script::new([Answer::Confirm]);
        let r = match verb {
            "copy" => copy(&mut ui, groups, &dst),
            "move" => mv(&mut ui, groups, &dst),
            "trash" => trash(&mut ui, groups, &t.join("data")),
            _ => delete(&mut ui, groups),
        };
        assert_group_failed(&r, &root);
        // The other group went ahead; nothing was done through the symlink.
        assert_eq!(r.done, 1, "{verb}: {r:?}");
        assert_eq!(read(&t.join("other/f")), b"precious", "{verb}");
        assert!(!exists(&dst.join("f")), "{verb}");
        assert!(!exists(&t.join("data/Trash/files/f")), "{verb}");
        assert_eq!(exists(&root.join("ok")), verb == "copy", "{verb}");
        if verb == "delete" {
            // One confirmation, counting only the group that opened.
            assert_eq!(
                ui.asked,
                vec![Question::ConfirmDelete {
                    files: 1,
                    dirs: 0,
                    bytes: 2,
                    single: None,
                }]
            );
        } else {
            assert!(ui.asked.is_empty(), "{verb}: {:?}", ui.asked);
        }
    }
}

/// Review finding A2 (E-1, I-7): a job refused after its groups were opened still reports
/// every name of a group that could not be opened as failed ("type changed" through the
/// symlinked `sub`): a missing destination for two names (copy, move, link), a destination
/// inside a source (the scan's refusal), two entries given one new name (multi-rename).
#[test]
fn refusals_after_the_groups_opened_still_report_the_failed_groups() {
    for verb in ["copy", "move", "link", "copy-inside", "rename"] {
        let t = test_dir("groups-refused-failed");
        let (root, mut groups) = symlinked_sub(&t);
        let missing = t.join("missing");
        let spec = match verb {
            "copy" => JobSpec::Copy {
                groups,
                dst: missing.clone().into(),
            },
            "move" => JobSpec::Move {
                groups,
                dst: missing.clone().into(),
            },
            "link" => JobSpec::Link {
                groups,
                dst: missing.clone(),
                kind: manycommander::fsops::link::LinkKind::Relative,
            },
            "copy-inside" => {
                std::fs::create_dir(root.join("d")).unwrap();
                groups[1].names = vec!["d".into()];
                JobSpec::Copy {
                    groups,
                    dst: root.join("d").into(),
                }
            }
            _ => {
                write(&root.join("x"), b"x");
                groups[1].names = vec!["ok".into(), "x".into()];
                JobSpec::Rename {
                    groups,
                    renames: vec![
                        vec![("f".into(), "g".into())],
                        vec![("ok".into(), "same".into()), ("x".into(), "same".into())],
                    ],
                }
            }
        };
        let r = run_guarded(spec, &Sys::default(), &mut Script::new([]));
        assert!(r.refused.is_some(), "{verb}: {r:?}");
        assert_group_failed(&r, &root);
        assert_eq!(read(&t.join("other/f")), b"precious", "{verb}");
        assert!(!exists(&missing), "{verb}");
        assert!(exists(&root.join("ok")), "{verb}");
    }
}

#[test]
fn invalid_components_are_refused_before_any_write() {
    let t = test_dir("groups-invalid");
    let src = t.join("src");
    std::fs::create_dir_all(src.join("d")).unwrap();
    write(&src.join("f"), b"f");
    write(&src.join("d/g"), b"g");
    let dst = t.join("dst");
    std::fs::create_dir_all(&dst).unwrap();
    let data = t.join("data");
    let before = listing(&t.path);
    let bad = [
        group(&src, &[".."], &["f"]),
        group(&src, &["."], &["f"]),
        group(&src, &[""], &["f"]),
        group(&src, &["d/.."], &["f"]),
        group(&src, &["d"], &[".."]),
        group(&src, &["d"], &["."]),
        group(&src, &["d"], &[""]),
        group(&src, &[], &["d/g"]),
        group(&src, &[], &["../src/f"]),
    ];
    for b in bad {
        // A valid group first: nothing of it is written either.
        let groups = || vec![group(&src, &[], &["f"]), b.clone()];
        let mut ui = Script::silent();
        let reports = [
            copy(&mut ui, groups(), &dst),
            mv(&mut ui, groups(), &dst),
            trash(&mut ui, groups(), &data),
            delete(&mut ui, groups()),
        ];
        for r in reports {
            let why = r.refused.as_deref().unwrap_or_default();
            assert!(why.contains("not a single path component"), "{b:?}: {r:?}");
        }
        assert!(ui.asked.is_empty(), "{b:?}: {:?}", ui.asked);
        assert_eq!(listing(&t.path), before, "{b:?}: nothing was written");
    }
}

#[test]
fn a_root_that_cannot_be_opened_refuses_the_job_as_in_m1() {
    let t = test_dir("groups-root");
    std::fs::create_dir_all(t.join("src")).unwrap();
    write(&t.join("src/f"), b"f");
    let groups = vec![
        group(&t.join("src"), &[], &["f"]),
        group(&t.join("missing"), &[], &["g"]),
    ];
    let r = copy(&mut Script::silent(), groups, &t.join("src"));
    let why = r.refused.as_deref().unwrap_or_default();
    assert!(
        why.starts_with(&t.join("missing").display().to_string()),
        "{r:?}"
    );
}

#[cfg(feature = "failpoints")]
#[test]
fn sub_component_swapped_for_a_symlink_during_the_walk_fails_its_group() {
    use manycommander::fsops::failpoints::{Action, Failpoints, Trigger};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    let t = test_dir("groups-swap");
    let root = t.join("root");
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(t.join("other")).unwrap();
    write(&root.join("a/f"), b"real");
    write(&t.join("other/f"), b"precious");
    // After the root is open and before the walk opens `a`, `a` becomes a symlink.
    let fp = Failpoints::new();
    let (a, moved, other) = (root.join("a"), t.join("a.moved"), t.join("other"));
    fp.arm(
        "group.walk",
        Trigger::Nth(1),
        Action::Call(Arc::new(move || {
            std::fs::rename(&a, &moved).unwrap();
            symlink(&other, &a).unwrap();
        })),
    );
    let sys = Sys::with_failpoints(Arc::new(AtomicBool::new(false)), fp.clone());
    let mut ui = Script::new([Answer::Confirm]);
    let spec = JobSpec::Delete {
        groups: vec![group(&root, &["a"], &["f"])],
    };
    let r = run_guarded(spec, &sys, &mut ui);
    assert_eq!(fp.hits("group.walk"), 1);
    assert_eq!(r.failed, 1, "{r:?}");
    assert_eq!(r.issues[0].path, root.join("a/f"));
    assert_eq!(r.issues[0].outcome, Outcome::Failed("type changed".into()));
    // Nothing opened, so nothing was confirmed or deleted.
    assert!(ui.asked.is_empty(), "{:?}", ui.asked);
    assert_eq!(read(&t.join("other/f")), b"precious");
    assert_eq!(read(&t.join("a.moved/f")), b"real");
}
