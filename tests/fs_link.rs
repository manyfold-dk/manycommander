//! P2 T3: create links (P2 8.1). A-LK-1: symbolic links, absolute and relative, for one and
//! several entries (also from a group below its root); the relative link resolves to the
//! source from its directory; an existing name is never replaced ("link exists" answered
//! Skip, Skip all and Rename). A-LK-2: a hard link shares the inode; a symlink is
//! hard-linked as the link itself; a directory is refused; `EXDEV` is reported.

mod common;

use common::*;
use manycommander::fsops::group::Group;
use manycommander::fsops::job::{JobSpec, Outcome, Report, run_guarded};
use manycommander::fsops::link::LinkKind;
use manycommander::fsops::question::{Answer, Question};
use manycommander::fsops::sys::Sys;
use std::ffi::OsString;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};

fn link(ui: &mut Script, groups: Vec<Group>, dst: &Path, kind: LinkKind) -> Report {
    let dst = dst.to_path_buf();
    run_guarded(JobSpec::Link { groups, dst, kind }, &Sys::default(), ui)
}

fn group(root: &Path, sub: &[&str], names: &[&str]) -> Group {
    Group {
        root: root.into(),
        sub: sub.iter().map(OsString::from).collect(),
        names: names.iter().map(OsString::from).collect(),
    }
}

/// Where a symlink leads, resolved from its own directory.
fn resolves_to(link: &Path) -> PathBuf {
    let target = std::fs::read_link(link).unwrap();
    std::fs::canonicalize(link.parent().unwrap().join(target)).unwrap()
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap()
}

fn tree(t: &TestDir) {
    std::fs::create_dir_all(t.join("src/sub")).unwrap();
    std::fs::create_dir_all(t.join("dst/deeper")).unwrap();
    write(&t.join("src/a"), b"a");
    write(&t.join("src/b"), b"b");
    write(&t.join("src/sub/c"), b"c");
}

#[test]
fn a_lk_1_one_entry_relative_and_absolute() {
    let t = test_dir("link-one");
    tree(&t);
    // One entry to a new name: the relative target is lexical from the link's directory.
    let r = link(
        &mut Script::silent(),
        vec![Group::new(t.join("src/sub"), vec!["c".into()])],
        &t.join("dst/deeper/lc"),
        LinkKind::Relative,
    );
    assert_eq!((r.done, r.skipped, r.failed), (1, 0, 0), "{r:?}");
    let lc = t.join("dst/deeper/lc");
    assert_eq!(
        std::fs::read_link(&lc).unwrap(),
        Path::new("../../src/sub/c")
    );
    assert_eq!(resolves_to(&lc), canon(&t.join("src/sub/c")));
    assert_eq!(std::fs::read(&lc).unwrap(), b"c");
    // Absolute: the source's lexical absolute path.
    let r = link(
        &mut Script::silent(),
        vec![Group::new(t.join("src"), vec!["a".into()])],
        &t.join("dst/deeper"),
        LinkKind::Absolute,
    );
    assert_eq!(r.done, 1, "{r:?}");
    let la = t.join("dst/deeper/a");
    assert_eq!(std::fs::read_link(&la).unwrap(), t.join("src/a"));
    assert!(std::fs::read_link(&la).unwrap().is_absolute());
    assert_eq!(std::fs::read(&la).unwrap(), b"a");
    assert_eq!(r.summary(), "link: 1 linked");
}

#[test]
fn a_lk_1_several_entries_and_a_group_below_its_root() {
    let t = test_dir("link-several");
    tree(&t);
    // Two groups: two names in the root, one in `sub` below it (a results tab's shape).
    let groups = vec![
        group(&t.path, &["src"], &["a", "b"]),
        group(&t.path, &["src", "sub"], &["c"]),
    ];
    let r = link(
        &mut Script::silent(),
        groups.clone(),
        &t.join("dst/deeper"),
        LinkKind::Relative,
    );
    assert_eq!((r.done, r.planned, r.failed), (3, 3, 0), "{r:?}");
    for (name, src) in [("a", "src/a"), ("b", "src/b"), ("c", "src/sub/c")] {
        let l = t.join("dst/deeper").join(name);
        assert_eq!(resolves_to(&l), canon(&t.join(src)), "{name}");
    }
    assert_eq!(
        std::fs::read_link(t.join("dst/deeper/c")).unwrap(),
        Path::new("../../src/sub/c"),
        "the target keeps the group's sub"
    );
    let r = link(
        &mut Script::silent(),
        groups,
        &t.join("dst"),
        LinkKind::Absolute,
    );
    assert_eq!(r.done, 3, "{r:?}");
    assert_eq!(
        std::fs::read_link(t.join("dst/c")).unwrap(),
        t.join("src/sub/c")
    );
    assert_eq!(
        std::fs::read_link(t.join("dst/a")).unwrap(),
        t.join("src/a")
    );
    // Several names need an existing directory.
    let groups = vec![Group::new(t.join("src"), vec!["a".into(), "b".into()])];
    let r = link(
        &mut Script::silent(),
        groups,
        &t.join("nowhere"),
        LinkKind::Relative,
    );
    assert!(r.refused.is_some(), "{r:?}");
}

#[test]
fn a_lk_1_an_existing_name_is_never_replaced() {
    let t = test_dir("link-exists");
    tree(&t);
    write(&t.join("dst/a"), b"old a");
    symlink("dangling", t.join("dst/b")).unwrap();
    let both = || vec![Group::new(t.join("src"), vec!["a".into(), "b".into()])];
    for kind in [LinkKind::Relative, LinkKind::Absolute, LinkKind::Hard] {
        let mut ui = Script::new([Answer::Skip, Answer::Skip]);
        let r = link(&mut ui, both(), &t.join("dst"), kind);
        assert_eq!((r.done, r.skipped), (0, 2), "{kind:?}: {r:?}");
        match &ui.asked[..] {
            [
                Question::LinkExists {
                    path: pa,
                    existing: Some(ea),
                },
                Question::LinkExists { path: pb, .. },
            ] => {
                assert_eq!(pa, &t.join("dst/a"));
                assert_eq!(ea.size, 5);
                assert_eq!(pb, &t.join("dst/b"));
            }
            q => panic!("{kind:?}: {q:?}"),
        }
        assert_eq!(
            std::fs::read(t.join("dst/a")).unwrap(),
            b"old a",
            "{kind:?}"
        );
        assert_eq!(
            std::fs::read_link(t.join("dst/b")).unwrap(),
            Path::new("dangling"),
            "{kind:?}: a dangling link is not replaced either"
        );
    }
    // Skip all: the second name is not asked about.
    let mut ui = Script::new([Answer::SkipAll]);
    let r = link(&mut ui, both(), &t.join("dst"), LinkKind::Relative);
    assert_eq!((ui.asked.len(), r.skipped), (1, 2), "{r:?}");
    // Rename: the link is made under the new name; the existing entry stays.
    let mut ui = Script::new([Answer::Rename("a2".into()), Answer::Cancel]);
    let r = link(&mut ui, both(), &t.join("dst"), LinkKind::Relative);
    assert_eq!(r.done, 1, "{r:?}");
    assert!(r.cancelled);
    assert_eq!(
        std::fs::read_link(t.join("dst/a2")).unwrap(),
        Path::new("../src/a")
    );
    assert_eq!(std::fs::read(t.join("dst/a")).unwrap(), b"old a");
    assert!(r.summary().contains("1 not linked"), "{}", r.summary());
}

#[test]
fn a_lk_2_hard_links() {
    let t = test_dir("link-hard");
    tree(&t);
    std::fs::create_dir(t.join("src/dir")).unwrap();
    symlink("a", t.join("src/l")).unwrap();
    let names = ["a", "l", "dir"].map(OsString::from).to_vec();
    let r = link(
        &mut Script::silent(),
        vec![Group::new(t.join("src"), names)],
        &t.join("dst"),
        LinkKind::Hard,
    );
    assert_eq!((r.done, r.skipped, r.failed), (2, 1, 0), "{r:?}");
    let src = std::fs::metadata(t.join("src/a")).unwrap();
    let dst = std::fs::metadata(t.join("dst/a")).unwrap();
    assert_eq!((dst.dev(), dst.ino()), (src.dev(), src.ino()), "one inode");
    assert_eq!(dst.nlink(), 2);
    // The symlink itself is linked, not its target.
    let ls = std::fs::symlink_metadata(t.join("src/l")).unwrap();
    let ld = std::fs::symlink_metadata(t.join("dst/l")).unwrap();
    assert!(ld.file_type().is_symlink());
    assert_eq!(ld.ino(), ls.ino());
    assert_eq!(ls.nlink(), 2);
    assert_eq!(std::fs::read_link(t.join("dst/l")).unwrap(), Path::new("a"));
    assert_eq!(
        std::fs::metadata(t.join("src/a")).unwrap().nlink(),
        2,
        "a: src and dst only"
    );
    // A directory is refused.
    assert_eq!(r.issues.len(), 1);
    assert_eq!(r.issues[0].path, t.join("src/dir"));
    assert_eq!(
        r.issues[0].outcome,
        Outcome::Skipped("directories cannot be hard-linked".into())
    );
    assert!(!t.join("dst/dir").exists());
}

#[test]
fn a_lk_2_hard_link_across_filesystems_is_reported() {
    let Some(x) = xdev_dir("link-xdev") else {
        return;
    };
    let t = test_dir("link-xdev");
    tree(&t);
    let r = link(
        &mut Script::silent(),
        vec![Group::new(t.join("src"), vec!["a".into()])],
        &x.path,
        LinkKind::Hard,
    );
    assert_eq!((r.done, r.failed), (0, 1), "{r:?}");
    assert_eq!(
        r.issues[0].outcome,
        Outcome::Failed("hard links cannot cross filesystems".into())
    );
    assert!(!x.join("a").exists());
    // A symbolic link across filesystems is fine.
    let r = link(
        &mut Script::silent(),
        vec![Group::new(t.join("src"), vec!["a".into()])],
        &x.path,
        LinkKind::Absolute,
    );
    assert_eq!(r.done, 1, "{r:?}");
    assert_eq!(std::fs::read(x.join("a")).unwrap(), b"a");
}

#[test]
fn a_gone_source_and_a_changed_sub_component_fail() {
    let t = test_dir("link-gone");
    tree(&t);
    std::fs::create_dir(t.join("elsewhere")).unwrap();
    symlink(t.join("elsewhere"), t.join("src/swapped")).unwrap();
    let groups = vec![
        Group::new(t.join("src"), vec!["missing".into(), "a".into()]),
        group(&t.join("src"), &["swapped"], &["x"]),
    ];
    let r = link(
        &mut Script::silent(),
        groups,
        &t.join("dst"),
        LinkKind::Relative,
    );
    assert_eq!((r.done, r.failed), (1, 2), "{r:?}");
    let why = |p: &Path| {
        r.issues
            .iter()
            .find(|i| i.path == p)
            .map(|i| i.outcome.clone())
            .unwrap_or_else(|| panic!("{p:?} not reported: {r:?}"))
    };
    assert_eq!(
        why(&t.join("src/missing")),
        Outcome::Failed("disappeared".into())
    );
    assert_eq!(
        why(&t.join("src/swapped/x")),
        Outcome::Failed("type changed".into())
    );
    assert!(std::fs::symlink_metadata(t.join("dst/missing")).is_err());
}

#[cfg(feature = "failpoints")]
#[test]
fn a_create_error_asks_and_retries() {
    use manycommander::fsops::failpoints::{Action, Failpoints, Trigger};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let t = test_dir("link-retry");
    tree(&t);
    let fp = Failpoints::new();
    fp.arm(
        "link.symlink",
        Trigger::Nth(1),
        Action::Errno(rustix::io::Errno::ACCESS),
    );
    let sys = Sys::with_failpoints(Arc::new(AtomicBool::new(false)), fp.clone());
    let mut ui = Script::new([Answer::Retry]);
    let spec = JobSpec::Link {
        groups: vec![Group::new(t.join("src"), vec!["a".into()])],
        dst: t.join("dst"),
        kind: LinkKind::Relative,
    };
    let r = run_guarded(spec, &sys, &mut ui);
    assert_eq!(fp.hits("link.symlink"), 2);
    assert!(
        matches!(ui.asked[..], [Question::Error { .. }]),
        "{:?}",
        ui.asked
    );
    assert_eq!(r.done, 1, "{r:?}");
}
