//! P2 T3: change attributes (P2 8.2). A-AT-1: the mode grammar; octal and symbolic modes on
//! files and directories, recursive; a symlink in the tree keeps its target's mode, and a
//! time-only change sets the link's own mtime; a mount point inside the tree (under
//! `unshare -rm`) is skipped; mtime set exactly on files and directories. A-AT-2: recursive
//! `a-rx` then `u+rx`: removing does not prevent the traversal of a directory's children,
//! adding makes a mode 000 directory readable before its children are processed, and a
//! failed final change reports the intermediate mode.

mod common;

use common::*;
use manycommander::fsops::attr::ModeChange;
use manycommander::fsops::group::Group;
use manycommander::fsops::job::{JobSpec, Outcome, Report, run_guarded};
use manycommander::fsops::sys::{Sys, Ts};
use std::ffi::OsString;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;

fn attr(
    ui: &mut Script,
    sys: &Sys,
    dir: &Path,
    names: &[&str],
    mode: Option<&str>,
    mtime: Option<Ts>,
    recursive: bool,
) -> Report {
    let spec = JobSpec::Attr {
        groups: vec![Group::new(dir, names.iter().map(OsString::from).collect())],
        mode: mode.map(|m| ModeChange::parse(m.as_bytes()).unwrap()),
        mtime,
        recursive,
    };
    run_guarded(spec, sys, ui)
}

fn chmod(ui: &mut Script, dir: &Path, names: &[&str], mode: &str, recursive: bool) -> Report {
    attr(ui, &Sys::default(), dir, names, Some(mode), None, recursive)
}

fn mode(p: &Path) -> u32 {
    std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777
}

fn set_mode(p: &Path, m: u32) {
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
}

fn mtime(p: &Path) -> Ts {
    let m = std::fs::symlink_metadata(p).unwrap();
    Ts {
        sec: m.mtime(),
        nsec: m.mtime_nsec() as u32,
    }
}

/// `tree/{f 0644, x 0755, d/{g 0600, e/}}` below `work`, and `outside/target` (0600) with a
/// symlink `tree/link` to it.
fn tree(t: &TestDir) {
    std::fs::create_dir_all(t.join("work/tree/d/e")).unwrap();
    std::fs::create_dir_all(t.join("outside")).unwrap();
    for (p, m) in [
        ("work/tree/f", 0o644),
        ("work/tree/x", 0o755),
        ("work/tree/d/g", 0o600),
    ] {
        write(&t.join(p), p.as_bytes());
        set_mode(&t.join(p), m);
    }
    for d in ["work/tree", "work/tree/d", "work/tree/d/e"] {
        set_mode(&t.join(d), 0o755);
    }
    write(&t.join("outside/target"), b"t");
    set_mode(&t.join("outside/target"), 0o600);
    symlink(t.join("outside/target"), t.join("work/tree/link")).unwrap();
}

#[test]
fn a_at_1_grammar() {
    let apply =
        |m: &str, old: u32, dir: bool| ModeChange::parse(m.as_bytes()).unwrap().apply(old, dir);
    // Octal: exactly those bits.
    assert_eq!(apply("644", 0o4777, false), 0o644);
    assert_eq!(apply("0644", 0o777, false), 0o644);
    assert_eq!(apply("4755", 0o600, false), 0o4755);
    assert_eq!(apply("7", 0o755, false), 0o007);
    // Symbolic.
    assert_eq!(apply("u+x,g-w,o=r", 0o666, false), 0o744);
    assert_eq!(apply("a-x", 0o777, true), 0o666);
    assert_eq!(
        apply("+x", 0o600, false),
        0o711,
        "no class: a, without the umask"
    );
    assert_eq!(apply("+X", 0o600, true), 0o711, "X on a directory");
    assert_eq!(apply("+X", 0o700, false), 0o711, "X on an executable file");
    assert_eq!(apply("+X", 0o600, false), 0o600, "X on a plain file");
    assert_eq!(apply("u+s", 0o755, false), 0o4755);
    assert_eq!(apply("g+s", 0o755, true), 0o2755);
    assert_eq!(apply("+t", 0o777, true), 0o1777);
    assert_eq!(
        apply("u=rwx,go=", 0o2775, true),
        0o700,
        "= clears the class's s"
    );
    for bad in [
        "", "8", "12345", "X", "u", "u+q", "q+x", "u+x,", "g=u", "rwx",
    ] {
        assert!(ModeChange::parse(bad.as_bytes()).is_err(), "{bad:?}");
    }
}

#[test]
fn a_at_1_octal_and_symbolic_recursive() {
    let t = test_dir("attr-recursive");
    tree(&t);
    let work = t.join("work");
    let r = chmod(&mut Script::silent(), &work, &["tree"], "750", true);
    // Files f, x, g; directories tree, d, e; the symlink is skipped.
    assert_eq!((r.done, r.dirs_done, r.failed), (3, 3, 0), "{r:?}");
    for p in ["tree", "tree/f", "tree/x", "tree/d", "tree/d/g", "tree/d/e"] {
        assert_eq!(mode(&work.join(p)), 0o750, "{p}");
    }
    assert_eq!(
        mode(&t.join("outside/target")),
        0o600,
        "the link's target is untouched"
    );
    assert_eq!(r.issues.len(), 1, "{r:?}");
    assert_eq!(r.issues[0].path, work.join("tree/link"));
    assert_eq!(
        r.issues[0].outcome,
        Outcome::Skipped("symbolic links have no mode of their own".into())
    );
    // The same change again changes nothing: every entry is unchanged, none done.
    let r = chmod(&mut Script::new([]), &work, &["tree"], "0750", true);
    assert_eq!((r.done, r.dirs_done, r.unchanged), (0, 0, 6), "{r:?}");
    assert!(r.summary().contains("6 unchanged"), "{}", r.summary());
    // Symbolic, with X resolved per entry.
    set_mode(&work.join("tree/f"), 0o640);
    let r = chmod(&mut Script::silent(), &work, &["tree"], "u=rwX,go=rX", true);
    assert_eq!(r.failed, 0, "{r:?}");
    for (p, m) in [
        ("tree", 0o755),
        ("tree/f", 0o644),
        ("tree/x", 0o755),
        ("tree/d", 0o755),
        ("tree/d/g", 0o755),
        ("tree/d/e", 0o755),
    ] {
        assert_eq!(mode(&work.join(p)), m, "{p}");
    }
    // Without Recursive a directory changes alone.
    let r = chmod(&mut Script::silent(), &work, &["tree"], "go-rx", false);
    assert_eq!((r.done, r.dirs_done), (0, 1), "{r:?}");
    assert_eq!(mode(&work.join("tree")), 0o700);
    assert_eq!(mode(&work.join("tree/d")), 0o755);
    assert_eq!(r.summary(), "change attributes: 0 changed, 1 directory");
}

#[test]
fn a_at_1_symlink_keeps_its_target_and_takes_its_own_time() {
    let t = test_dir("attr-symlink");
    tree(&t);
    let work = t.join("work/tree");
    let target_before = mtime(&t.join("outside/target"));
    let when = Ts {
        sec: 1_600_000_000,
        nsec: 123_456_789,
    };
    // Time only: the link's own mtime, not its target's.
    let r = attr(
        &mut Script::silent(),
        &Sys::default(),
        &work,
        &["link"],
        None,
        Some(when),
        false,
    );
    assert_eq!((r.done, r.failed, r.skipped), (1, 0, 0), "{r:?}");
    assert_eq!(mtime(&work.join("link")), when);
    assert_eq!(mtime(&t.join("outside/target")), target_before);
    assert_eq!(mode(&t.join("outside/target")), 0o600);
    // Mode and time: the mode is ignored for the link, the time set (already equal now).
    let r = attr(
        &mut Script::silent(),
        &Sys::default(),
        &work,
        &["link"],
        Some("777"),
        Some(when),
        false,
    );
    assert_eq!((r.done, r.unchanged), (0, 1), "{r:?}");
    assert_eq!(mode(&t.join("outside/target")), 0o600);
    // Mode only: skipped with the reason.
    let r = chmod(&mut Script::silent(), &work, &["link"], "777", false);
    assert_eq!(r.skipped, 1, "{r:?}");
    assert_eq!(mode(&t.join("outside/target")), 0o600);
}

#[test]
fn a_at_1_mtime_is_set_exactly_on_files_and_directories() {
    let t = test_dir("attr-mtime");
    tree(&t);
    let work = t.join("work");
    let when = Ts {
        sec: 1_234_567_890,
        nsec: 987_654_321,
    };
    let before = std::fs::metadata(t.join("work/tree/f")).unwrap();
    let r = attr(
        &mut Script::silent(),
        &Sys::default(),
        &work,
        &["tree"],
        None,
        Some(when),
        true,
    );
    assert_eq!((r.done, r.dirs_done, r.failed), (4, 3, 0), "{r:?}");
    for p in [
        "tree",
        "tree/f",
        "tree/x",
        "tree/d",
        "tree/d/g",
        "tree/d/e",
        "tree/link",
    ] {
        assert_eq!(mtime(&work.join(p)), when, "{p}");
    }
    let after = std::fs::metadata(t.join("work/tree/f")).unwrap();
    assert_eq!(
        (after.atime(), after.atime_nsec()),
        (before.atime(), before.atime_nsec()),
        "the access time is kept"
    );
    assert_eq!(
        mode(&work.join("tree/f")),
        0o644,
        "no mode asked, none changed"
    );
    let target = std::fs::metadata(t.join("outside/target")).unwrap();
    assert_ne!(target.mtime(), when.sec, "the link's target keeps its time");
    // A time before the epoch and one in the far future round-trip too.
    for when in [
        Ts {
            sec: -86_400,
            nsec: 1,
        },
        Ts {
            sec: 4_102_444_800,
            nsec: 0,
        },
    ] {
        let r = attr(
            &mut Script::silent(),
            &Sys::default(),
            &work,
            &["tree"],
            None,
            Some(when),
            false,
        );
        assert_eq!(r.dirs_done, 1, "{r:?}");
        assert_eq!(mtime(&work.join("tree")), when);
    }
}

#[test]
fn a_special_file_is_changed_without_being_opened() {
    let t = test_dir("attr-fifo");
    std::fs::create_dir(t.join("d")).unwrap();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        t.join("d/fifo").as_path(),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o644),
        0,
    )
    .unwrap();
    set_mode(&t.join("d/fifo"), 0o644);
    // Opening the FIFO for reading would block this test forever.
    let r = chmod(&mut Script::silent(), &t.path, &["d"], "go=", true);
    assert_eq!((r.done, r.dirs_done, r.failed), (1, 1, 0), "{r:?}");
    assert_eq!(mode(&t.join("d/fifo")), 0o600);
}

#[test]
fn a_at_1_bind_mount_inside_the_tree_is_skipped() {
    if !in_userns("a_at_1_bind_mount_inside_the_tree_is_skipped") {
        return;
    }
    let t = test_dir("attr-bind");
    tree(&t);
    std::fs::create_dir(t.join("work/tree/m")).unwrap();
    set_mode(&t.join("work/tree/m"), 0o755);
    bind_mount(&t.join("outside"), &t.join("work/tree/m"));
    let r = chmod(
        &mut Script::silent(),
        &t.join("work"),
        &["tree"],
        "a-w",
        true,
    );
    let m = r
        .issues
        .iter()
        .find(|i| i.path.ends_with("tree/m"))
        .expect("mount point reported");
    assert_eq!(m.outcome, Outcome::Skipped("mount point".into()));
    assert_eq!(
        mode(&t.join("outside/target")),
        0o600,
        "nothing under the mount changed"
    );
    assert_eq!(mode(&t.join("outside")), 0o755, "nor the mount root");
    assert_eq!(mode(&t.join("work/tree/d/g")), 0o400);
    umount(&t.join("work/tree/m"));
}

#[test]
fn a_at_2_removing_and_adding_read_and_search() {
    let t = test_dir("attr-order");
    tree(&t);
    let work = t.join("work");
    // Removing r and x from every directory does not stop the traversal: the children
    // change before their directory loses its bits.
    let r = chmod(&mut Script::silent(), &work, &["tree"], "a-rx", true);
    assert_eq!((r.dirs_done, r.done, r.failed), (3, 3, 0), "{r:?}");
    // The directories cannot be searched now, so nothing below `tree` can be checked from
    // here; the next run shows the files' modes.
    assert_eq!(mode(&work.join("tree")), 0o200);
    // Adding u+rx makes each directory readable before its entries are read. The files end
    // at 0700 only if the first run left them at 0200 (f was 0644, g 0600).
    let r = chmod(&mut Script::silent(), &work, &["tree"], "u+rx", true);
    assert_eq!((r.dirs_done, r.done, r.failed), (3, 3, 0), "{r:?}");
    for (p, m) in [
        ("tree", 0o700),
        ("tree/d", 0o700),
        ("tree/d/e", 0o700),
        ("tree/f", 0o700),
        ("tree/x", 0o700),
        ("tree/d/g", 0o700),
    ] {
        assert_eq!(mode(&work.join(p)), m, "{p}");
    }
    // A mode 000 directory with a mode 000 child.
    std::fs::create_dir(work.join("closed")).unwrap();
    write(&work.join("closed/c"), b"c");
    set_mode(&work.join("closed/c"), 0);
    set_mode(&work.join("closed"), 0);
    let r = chmod(&mut Script::silent(), &work, &["closed"], "u+rx", true);
    assert_eq!((r.dirs_done, r.done, r.failed), (1, 1, 0), "{r:?}");
    assert_eq!(mode(&work.join("closed")), 0o500);
    assert_eq!(mode(&work.join("closed/c")), 0o500);
}

#[cfg(feature = "failpoints")]
mod failpoints {
    use super::*;
    use manycommander::fsops::failpoints::{Action, Failpoints, Trigger};
    use manycommander::fsops::question::{Answer, Question};
    use rustix::io::Errno;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    fn sys(fp: &Arc<Failpoints>) -> Sys {
        Sys::with_failpoints(Arc::new(AtomicBool::new(false)), fp.clone())
    }

    /// `d` (0555) holding `files` (0444). With one file, `u=rwx,go=` gives `d` the
    /// intermediate mode 0755 (chmod 1), then `f` 0700 (chmod 2), then `d` its final 0700
    /// (chmod 3).
    fn small(t: &TestDir, files: &[&str]) {
        std::fs::create_dir(t.join("d")).unwrap();
        for f in files {
            write(&t.join("d").join(f), f.as_bytes());
            set_mode(&t.join("d").join(f), 0o444);
        }
        set_mode(&t.join("d"), 0o555);
    }

    #[test]
    fn a_at_2_a_failed_final_change_reports_the_intermediate_mode() {
        let t = test_dir("attr-final-fails");
        small(&t, &["f"]);
        let fp = Failpoints::new();
        fp.arm("attr.chmod", Trigger::Nth(3), Action::Errno(Errno::PERM));
        let mut ui = Script::new([Answer::Skip]);
        let r = attr(
            &mut ui,
            &sys(&fp),
            &t.path,
            &["d"],
            Some("u=rwx,go="),
            None,
            true,
        );
        assert_eq!(fp.hits("attr.chmod"), 3);
        assert!(
            matches!(
                ui.asked[..],
                [Question::Error {
                    errno: Errno::PERM,
                    ..
                }]
            ),
            "{:?}",
            ui.asked
        );
        assert_eq!((r.done, r.dirs_done, r.failed), (1, 0, 1), "{r:?}");
        match &r.issues[0].outcome {
            Outcome::Failed(why) => {
                assert!(why.contains("left with mode 0755 (rwxr-xr-x)"), "{why}")
            }
            o => panic!("{o:?}"),
        }
        assert_eq!(mode(&t.join("d")), 0o755);
        assert_eq!(mode(&t.join("d/f")), 0o700);
    }

    #[test]
    fn eperm_asks_and_retry_succeeds() {
        let t = test_dir("attr-retry");
        small(&t, &["f"]);
        let fp = Failpoints::new();
        fp.arm("attr.chmod", Trigger::Nth(1), Action::Errno(Errno::PERM));
        let mut ui = Script::new([Answer::Retry]);
        let r = attr(
            &mut ui,
            &sys(&fp),
            &t.path,
            &["d"],
            Some("u=rwx,go="),
            None,
            true,
        );
        assert_eq!(ui.asked.len(), 1, "{:?}", ui.asked);
        assert_eq!((r.done, r.dirs_done, r.failed), (1, 1, 0), "{r:?}");
        assert_eq!(mode(&t.join("d")), 0o700);
    }

    #[test]
    fn a_cancel_inside_a_directory_states_the_mode_it_keeps() {
        let t = test_dir("attr-cancel");
        small(&t, &["f", "g"]);
        let fp = Failpoints::new();
        // Opens: d (1), f (2), g (3). The cancel lands while f is changed.
        fp.arm("attr.open", Trigger::Nth(2), Action::Cancel);
        let r = attr(
            &mut Script::silent(),
            &sys(&fp),
            &t.path,
            &["d"],
            Some("u=rwx,go="),
            None,
            true,
        );
        assert!(r.cancelled, "{r:?}");
        assert_eq!(fp.hits("attr.open"), 2);
        assert_eq!(mode(&t.join("d/f")), 0o700);
        assert_eq!(mode(&t.join("d/g")), 0o444, "not reached");
        assert_eq!(mode(&t.join("d")), 0o755);
        assert!(
            r.notes.iter().any(|n| n.contains("left with mode 0755")),
            "{:?}",
            r.notes
        );
        assert!(
            r.summary()
                .starts_with("change attributes cancelled: 1 changed"),
            "{}",
            r.summary()
        );
    }
}
