//! T5: permanent delete (Shift+F8). A-DEL-1 (including the bind mount under
//! `unshare -rm`), A-FS-13 (delete), and a read-only directory that is not forced.

mod common;

use common::*;
use manycommander::fsops::job::{JobSpec, Outcome, Report, run_guarded};
use manycommander::fsops::question::{Answer, Question};
use manycommander::fsops::sys::Sys;
use std::ffi::OsString;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

fn delete(sys: &Sys, ui: &mut Script, dir: &Path, names: &[&str]) -> Report {
    run_guarded(
        JobSpec::Delete {
            dir: dir.to_path_buf(),
            names: names.iter().map(OsString::from).collect(),
        },
        sys,
        ui,
    )
}

fn tree(t: &TestDir) {
    std::fs::create_dir_all(t.join("work/tree/a/b")).unwrap();
    write(&t.join("work/tree/a/f"), &noise(3000, 1));
    write(&t.join("work/tree/a/b/g"), &noise(5, 2));
    std::fs::create_dir_all(t.join("outside")).unwrap();
    write(&t.join("outside/precious"), b"p");
    symlink(t.join("outside"), t.join("work/tree/link")).unwrap();
}

#[test]
fn a_del_1_typed_confirmation_and_symlinks() {
    let t = test_dir("delete-adel1");
    tree(&t);
    // Anything but the typed confirmation deletes nothing.
    for answer in [Answer::Cancel, Answer::Skip] {
        let mut ui = Script::new([answer]);
        let r = delete(&Sys::default(), &mut ui, &t.join("work"), &["tree"]);
        match &ui.asked[..] {
            [
                Question::ConfirmDelete {
                    files,
                    dirs,
                    bytes,
                    single: None,
                },
            ] => {
                assert_eq!((*files, *dirs, *bytes), (3, 3, 3005));
            }
            q => panic!("{q:?}"),
        }
        assert!(r.cancelled);
        assert_eq!(r.done, 0);
        assert!(t.join("work/tree/a/b/g").exists());
    }
    let mut ui = Script::new([Answer::Confirm]);
    let r = delete(&Sys::default(), &mut ui, &t.join("work"), &["tree"]);
    assert_eq!((r.done, r.dirs_done, r.failed), (3, 3, 0), "{r:?}");
    assert!(!t.join("work/tree").exists());
    assert_eq!(
        std::fs::read(t.join("outside/precious")).unwrap(),
        b"p",
        "the link's target is intact"
    );
}

#[test]
fn a_del_1_bind_mount_inside_tree_is_skipped() {
    if !in_userns("a_del_1_bind_mount_inside_tree_is_skipped") {
        return;
    }
    let t = test_dir("delete-adel1-bind");
    tree(&t);
    std::fs::create_dir(t.join("work/tree/m")).unwrap();
    bind_mount(&t.join("outside"), &t.join("work/tree/m"));
    let mut ui = Script::new([Answer::Confirm]);
    let r = delete(&Sys::default(), &mut ui, &t.join("work"), &["tree"]);
    let m = r
        .issues
        .iter()
        .find(|i| i.path.ends_with("tree/m"))
        .expect("mount point reported");
    assert_eq!(m.outcome, Outcome::Skipped("mount point".into()));
    assert_eq!(std::fs::read(t.join("outside/precious")).unwrap(), b"p");
    assert!(
        t.join("work/tree/m/precious").exists(),
        "nothing under the mount was deleted"
    );
    assert!(!t.join("work/tree/a").exists());
    umount(&t.join("work/tree/m"));
}

#[test]
fn read_only_directory_is_not_forced() {
    let t = test_dir("delete-ro");
    std::fs::create_dir_all(t.join("work/ro")).unwrap();
    write(&t.join("work/ro/f"), b"f");
    std::fs::set_permissions(t.join("work/ro"), std::fs::Permissions::from_mode(0o555)).unwrap();
    let mut ui = Script::new([Answer::Confirm, Answer::Skip]);
    let r = delete(&Sys::default(), &mut ui, &t.join("work"), &["ro"]);
    assert!(
        matches!(
            ui.asked[1],
            Question::Error {
                errno: rustix::io::Errno::ACCESS,
                ..
            }
        ),
        "{:?}",
        ui.asked
    );
    assert!(t.join("work/ro/f").exists());
    assert_eq!(
        std::fs::metadata(t.join("work/ro"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o555,
        "not chmod-ed"
    );
    assert!(r.failed >= 1);
}

#[cfg(feature = "failpoints")]
#[test]
fn a_fs_13_swapped_directory_during_delete() {
    use manycommander::fsops::failpoints::{Action, Failpoints, Trigger};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let t = test_dir("delete-afs13");
    tree(&t);
    let fp = Failpoints::new();
    let (b, out, moved) = (
        t.join("work/tree/a/b"),
        t.join("outside"),
        t.join("b-moved"),
    );
    // Execution opens `tree` (hit 1), `a` (hit 2), `b` (hit 3).
    fp.arm(
        "walk.openat",
        Trigger::Nth(3),
        Action::Call(Arc::new(move || {
            std::fs::rename(&b, &moved).unwrap();
            symlink(&out, &b).unwrap();
        })),
    );
    let sys = Sys::with_failpoints(Arc::new(AtomicBool::new(false)), fp.clone());
    let mut ui = Script::new([Answer::Confirm]);
    let r = delete(&sys, &mut ui, &t.join("work"), &["tree"]);
    assert_eq!(fp.hits("walk.openat"), 3);
    let b = r
        .issues
        .iter()
        .find(|i| i.path.ends_with("a/b"))
        .expect("b reported");
    assert_eq!(b.outcome, Outcome::Failed("type changed".into()));
    assert_eq!(std::fs::read(t.join("outside/precious")).unwrap(), b"p");
    assert!(t.join("b-moved/g").exists());
}
