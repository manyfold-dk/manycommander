//! T1: filesystem identity and the failpoint mechanism. Every fixture is built inside the
//! test; no assertion depends on this machine's own mounts.

mod common;

use common::*;
use manycommander::fsops::identity::{Relation, relation};
use manycommander::fsops::sys::{Sys, fd};
use std::ffi::OsStr;

fn identity_at(
    sys: &Sys,
    dir: &std::path::Path,
    name: &str,
) -> manycommander::fsops::sys::FsIdentity {
    let root = sys.open_root(dir).unwrap();
    let child = sys
        .open_dir("walk.openat", fd(&root), OsStr::new(name))
        .unwrap();
    sys.stat_fd(fd(&child)).unwrap().id
}

#[test]
fn directory_and_child_are_same() {
    let t = test_dir("identity-same");
    std::fs::create_dir(t.join("child")).unwrap();
    let sys = Sys::default();
    let parent = sys.stat_path(&t.path).unwrap().id;
    let child = identity_at(&sys, &t.path, "child");
    assert_ne!(parent.mnt_id, 0, "the kernel reports a mount ID");
    assert_eq!(relation(&parent, &child), Relation::Same);
}

#[test]
fn btrfs_subvolume_is_subvolume() {
    if !require_btrfs() {
        return;
    }
    let t = test_dir("identity-subvol");
    subvolume_create(&t.join("sv"));
    let sys = Sys::default();
    let parent = sys.stat_path(&t.path).unwrap().id;
    let child = identity_at(&sys, &t.path, "sv");
    assert_eq!(relation(&parent, &child), Relation::Subvolume);
}

#[test]
fn bind_mount_classifies_as_mount() {
    if !in_userns("bind_mount_classifies_as_mount") {
        return;
    }
    let t = test_dir("identity-bind");
    std::fs::create_dir_all(t.join("src/sub")).unwrap();
    std::fs::create_dir(t.join("mnt")).unwrap();
    bind_mount(&t.join("src/sub"), &t.join("mnt"));
    let sys = Sys::default();
    let parent = sys.stat_path(&t.path).unwrap().id;
    let child = identity_at(&sys, &t.path, "mnt");
    assert_eq!(relation(&parent, &child), Relation::Mount);
    // A bind mount shares st_dev and st_ino with its source; only mnt_id tells them apart.
    let src = identity_at(&sys, &t.join("src"), "sub");
    assert_eq!(src.inode(), child.inode());
    assert_ne!(src.mnt_id, child.mnt_id);
    umount(&t.join("mnt"));
}

#[test]
fn nofile_limit_is_raised_to_hard_limit() {
    let hard = rustix::process::getrlimit(rustix::process::Resource::Nofile).maximum;
    let soft = manycommander::fsops::sys::raise_nofile_limit();
    assert_eq!(soft, hard);
}

#[cfg(feature = "failpoints")]
#[test]
fn failpoint_returns_errno_and_counts_hits() {
    use manycommander::fsops::failpoints::{Action, Failpoints, Trigger};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let t = test_dir("identity-failpoint");
    std::fs::create_dir(t.join("child")).unwrap();
    let fp = Failpoints::new();
    fp.arm(
        "walk.openat",
        Trigger::Nth(2),
        Action::Errno(rustix::io::Errno::MFILE),
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let sys = Sys::with_failpoints(cancel.clone(), fp.clone());
    let root = sys.open_root(&t.path).unwrap();
    assert!(
        sys.open_dir("walk.openat", fd(&root), OsStr::new("child"))
            .is_ok()
    );
    assert_eq!(
        sys.open_dir("walk.openat", fd(&root), OsStr::new("child"))
            .unwrap_err(),
        rustix::io::Errno::MFILE
    );
    assert!(
        sys.open_dir("walk.openat", fd(&root), OsStr::new("child"))
            .is_ok()
    );
    assert_eq!(fp.hits("walk.openat"), 3);

    fp.arm("walk.openat", Trigger::Nth(4), Action::Cancel);
    assert!(!sys.cancelled());
    sys.open_dir("walk.openat", fd(&root), OsStr::new("child"))
        .unwrap();
    assert!(sys.cancelled());
    assert!(cancel.load(std::sync::atomic::Ordering::SeqCst));
}
