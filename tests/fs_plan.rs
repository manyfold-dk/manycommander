//! T2: the plan phase. Planner-level A-FS-3, A-FS-4 and A-FS-13, plus the per-verb identity
//! boundaries of design 4.2 and the cycle check of 4.6.

mod common;

use common::*;
use manycommander::fsops::plan::{Node, Note, Plan, Refusal, Scan, Verb, scan};
use manycommander::fsops::question::Reporter;
use manycommander::fsops::sys::{Sys, fd};
use std::ffi::OsString;
use std::os::unix::fs::symlink;
use std::path::Path;

fn run(
    sys: &Sys,
    verb: Verb,
    src: &Path,
    names: &[&str],
    dst: Option<&Path>,
) -> Result<Plan, Refusal> {
    let names: Vec<OsString> = names.iter().map(OsString::from).collect();
    let srcfd = sys.open_root(src).unwrap();
    let dstfd = dst.map(|d| sys.open_root(d).unwrap());
    let mut ui = Script::silent();
    let mut rep = Reporter::new(&mut ui);
    let s = Scan {
        sys,
        verb,
        src: fd(&srcfd),
        src_path: src,
        names: &names,
        dst: dstfd.as_ref().map(|d| (fd(d), names.as_slice())),
    };
    scan(&s, &mut rep)
}

fn find<'a>(nodes: &'a [Node], name: &str) -> &'a Node {
    nodes
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("{name} not in plan"))
}

fn tree(t: &TestDir) {
    std::fs::create_dir_all(t.join("src/a/b")).unwrap();
    write(&t.join("src/a/f1"), b"12345");
    write(&t.join("src/a/b/f2"), b"123");
    symlink("f1", t.join("src/a/link")).unwrap();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        t.join("src/a/fifo"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
    std::fs::create_dir(t.join("dst")).unwrap();
}

#[test]
fn scan_counts_and_orders() {
    let t = test_dir("plan-counts");
    tree(&t);
    let p = run(
        &Sys::default(),
        Verb::Copy,
        &t.join("src"),
        &["a"],
        Some(&t.join("dst")),
    )
    .unwrap();
    assert_eq!(p.totals.files, 2);
    assert_eq!(p.totals.bytes, 8);
    assert_eq!(p.totals.dirs, 2);
    assert_eq!(p.totals.symlinks, 1);
    assert_eq!(p.totals.specials, 1);
    let a = find(&p.roots, "a");
    let names: Vec<_> = a.children.iter().map(|n| n.name.clone()).collect();
    assert_eq!(names, ["b", "f1", "fifo", "link"]);
    assert!(p.src_dirs.contains(&a.meta.id.inode()));
}

#[test]
fn same_file_is_refused_in_plan() {
    // A-FS-3, planner level: a hard link, and the same path.
    let t = test_dir("plan-samefile");
    std::fs::create_dir_all(t.join("src")).unwrap();
    std::fs::create_dir_all(t.join("dst")).unwrap();
    write(&t.join("src/f"), b"data");
    std::fs::hard_link(t.join("src/f"), t.join("dst/f")).unwrap();
    let sys = Sys::default();
    let p = run(
        &sys,
        Verb::Copy,
        &t.join("src"),
        &["f"],
        Some(&t.join("dst")),
    )
    .unwrap();
    assert_eq!(p.roots[0].note, Some(Note::SameFile));
    let p = run(
        &sys,
        Verb::Copy,
        &t.join("src"),
        &["f"],
        Some(&t.join("src")),
    )
    .unwrap();
    assert_eq!(p.roots[0].note, Some(Note::SameFile));
    let p = run(
        &sys,
        Verb::Move,
        &t.join("src"),
        &["f"],
        Some(&t.join("src")),
    )
    .unwrap();
    assert_eq!(p.roots[0].note, Some(Note::SameFile));
}

#[test]
fn destination_inside_source_is_refused() {
    // A-FS-4, planner level: the plain case, the directory itself, and a symlinked
    // destination path that resolves into the source.
    let t = test_dir("plan-inside");
    tree(&t);
    symlink(t.join("src/a/b"), t.join("sneaky")).unwrap();
    let sys = Sys::default();
    for verb in [Verb::Copy, Verb::Move] {
        for dst in [t.join("src/a/b"), t.join("src/a"), t.join("sneaky")] {
            let r = run(&sys, verb, &t.join("src"), &["a"], Some(&dst));
            assert_eq!(
                r.unwrap_err(),
                Refusal::DestInsideSource,
                "{verb:?} into {dst:?}"
            );
        }
    }
    // A sibling is fine.
    assert!(
        run(
            &sys,
            Verb::Copy,
            &t.join("src"),
            &["a"],
            Some(&t.join("dst"))
        )
        .is_ok()
    );
}

#[test]
fn bind_mount_of_source_subdir_as_destination_is_refused() {
    // A-FS-4 through a bind mount: `..` from the destination never reaches the source, the
    // scanned identity set does.
    if !in_userns("bind_mount_of_source_subdir_as_destination_is_refused") {
        return;
    }
    let t = test_dir("plan-inside-bind");
    tree(&t);
    std::fs::create_dir(t.join("mnt")).unwrap();
    bind_mount(&t.join("src/a/b"), &t.join("mnt"));
    let sys = Sys::default();
    for verb in [Verb::Copy, Verb::Move] {
        let r = run(&sys, verb, &t.join("src"), &["a"], Some(&t.join("mnt")));
        assert_eq!(r.unwrap_err(), Refusal::DestInsideSource, "{verb:?}");
    }
    umount(&t.join("mnt"));
}

#[test]
fn bind_mount_inside_tree_per_verb() {
    // Design 4.2: copy descends into a mount, move and delete skip it as a mount point. A
    // bind mount of an ancestor inside the tree is a cycle for copy.
    if !in_userns("bind_mount_inside_tree_per_verb") {
        return;
    }
    let t = test_dir("plan-mount-verbs");
    tree(&t);
    std::fs::create_dir_all(t.join("other/x")).unwrap();
    write(&t.join("other/x/y"), b"y");
    std::fs::create_dir(t.join("src/a/m")).unwrap();
    bind_mount(&t.join("other"), &t.join("src/a/m"));
    std::fs::create_dir(t.join("src/a/loop")).unwrap();
    bind_mount(&t.join("src/a"), &t.join("src/a/loop"));
    let sys = Sys::default();
    let copy = run(
        &sys,
        Verb::Copy,
        &t.join("src"),
        &["a"],
        Some(&t.join("dst")),
    )
    .unwrap();
    let a = find(&copy.roots, "a");
    assert_eq!(find(&a.children, "m").note, None);
    assert_eq!(
        find(&find(&a.children, "m").children, "x").children.len(),
        1
    );
    assert_eq!(find(&a.children, "loop").note, Some(Note::Cycle));
    for verb in [Verb::Move, Verb::Delete] {
        let dst = (verb == Verb::Move).then(|| t.join("dst"));
        let p = run(&sys, verb, &t.join("src"), &["a"], dst.as_deref()).unwrap();
        let a = find(&p.roots, "a");
        assert_eq!(
            find(&a.children, "m").note,
            Some(Note::MountPoint),
            "{verb:?}"
        );
        assert!(find(&a.children, "m").children.is_empty());
    }
    umount(&t.join("src/a/loop"));
    umount(&t.join("src/a/m"));
}

#[cfg(feature = "failpoints")]
#[test]
fn swapped_component_during_walk_is_not_followed() {
    // A-FS-13, planner level: `src/a/b` is replaced by a symlink to a directory outside the
    // tree just before the walk opens it.
    use manycommander::fsops::failpoints::{Action, Failpoints, Trigger};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let t = test_dir("plan-swap");
    tree(&t);
    std::fs::create_dir_all(t.join("outside")).unwrap();
    write(&t.join("outside/secret"), b"s");
    let fp = Failpoints::new();
    let (b, out) = (t.join("src/a/b"), t.join("outside"));
    let moved = t.join("b-moved");
    // Hit 1 opens `a`, hit 2 opens `b`.
    fp.arm(
        "scan.openat",
        Trigger::Nth(2),
        Action::Call(Arc::new(move || {
            std::fs::rename(&b, &moved).unwrap();
            symlink(&out, &b).unwrap();
        })),
    );
    let sys = Sys::with_failpoints(Arc::new(AtomicBool::new(false)), fp.clone());
    let p = run(
        &sys,
        Verb::Copy,
        &t.join("src"),
        &["a"],
        Some(&t.join("dst")),
    )
    .unwrap();
    assert_eq!(fp.hits("scan.openat"), 2);
    let b = find(&find(&p.roots, "a").children, "b");
    assert_eq!(
        b.note,
        Some(Note::Failed(
            manycommander::fsops::walk::EntryError::TypeChanged
        ))
    );
    assert!(
        b.children.is_empty(),
        "the symlinked directory was not read"
    );
}

#[cfg(feature = "failpoints")]
#[test]
fn traversal_errno_fails_the_entry_not_the_scan() {
    use manycommander::fsops::failpoints::{Action, Failpoints, Trigger};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    for errno in [
        rustix::io::Errno::MFILE,
        rustix::io::Errno::NOMEM,
        rustix::io::Errno::NAMETOOLONG,
    ] {
        let t = test_dir("plan-errno");
        tree(&t);
        let fp = Failpoints::new();
        fp.arm("scan.openat", Trigger::Nth(2), Action::Errno(errno));
        let sys = Sys::with_failpoints(Arc::new(AtomicBool::new(false)), fp);
        let p = run(
            &sys,
            Verb::Copy,
            &t.join("src"),
            &["a"],
            Some(&t.join("dst")),
        )
        .unwrap();
        let a = find(&p.roots, "a");
        assert_eq!(
            find(&a.children, "b").note,
            Some(Note::Failed(manycommander::fsops::walk::EntryError::Os {
                op: "open directory",
                errno
            }))
        );
        assert_eq!(find(&a.children, "f1").note, None);
    }
}

#[test]
fn cancel_during_scan() {
    let t = test_dir("plan-cancel");
    tree(&t);
    let sys = Sys::default();
    sys.cancel_flag()
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let r = run(
        &sys,
        Verb::Copy,
        &t.join("src"),
        &["a"],
        Some(&t.join("dst")),
    );
    assert_eq!(r.unwrap_err(), Refusal::Cancelled);
}

#[test]
fn hard_link_differing_in_case_is_same_file_not_case_rename() {
    let t = test_dir("plan-case-hardlink");
    std::fs::create_dir(t.join("d")).unwrap();
    write(&t.join("d/Foo"), b"x");
    std::fs::hard_link(t.join("d/Foo"), t.join("d/foo")).unwrap();
    let names = [OsString::from("Foo")];
    let targets = [OsString::from("foo")];
    let sys = Sys::default();
    let d = sys.open_root(&t.join("d")).unwrap();
    let mut ui = Script::silent();
    let mut rep = Reporter::new(&mut ui);
    let s = Scan {
        sys: &sys,
        verb: Verb::Move,
        src: fd(&d),
        src_path: &t.join("d"),
        names: &names,
        dst: Some((fd(&d), &targets)),
    };
    let p = scan(&s, &mut rep).unwrap();
    assert_eq!(p.roots[0].note, Some(Note::SameFile));
}
