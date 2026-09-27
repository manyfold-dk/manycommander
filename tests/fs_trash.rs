//! T5: trash (F8). A-TR-1 (automated part), A-TR-2, A-TR-4, A-TR-5, A-FS-10 (trash), the
//! top-directory methods of A-TR-3 (automated part). The data home (`XDG_DATA_HOME`)
//! always points at a test directory.

mod common;

use common::*;
use manycommander::fsops::job::{Outcome, Report};
use manycommander::fsops::question::{Answer, Question};
use manycommander::fsops::sys::Sys;
use manycommander::fsops::trash::{decode_path, trash_job_with};
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

fn trash(ui: &mut Script, dir: &Path, names: &[&[u8]], data: &Path) -> Report {
    let names: Vec<OsString> = names
        .iter()
        .map(|n| OsString::from_vec(n.to_vec()))
        .collect();
    trash_job_with(&Sys::default(), ui, dir, &names, Some(data))
}

/// `(Path bytes, DeletionDate)` of a `.trashinfo` file, checking its shape.
fn read_info(p: &Path) -> (Vec<u8>, String) {
    let s = std::fs::read_to_string(p).unwrap();
    let mut lines = s.lines();
    assert_eq!(lines.next(), Some("[Trash Info]"));
    let path = lines.next().unwrap().strip_prefix("Path=").expect("Path=");
    let date = lines
        .next()
        .unwrap()
        .strip_prefix("DeletionDate=")
        .expect("DeletionDate=");
    assert!(lines.next().is_none());
    let d = date.as_bytes();
    assert_eq!(d.len(), 19, "{date}");
    for (i, c) in d.iter().enumerate() {
        match i {
            4 | 7 => assert_eq!(*c, b'-'),
            10 => assert_eq!(*c, b'T'),
            13 | 16 => assert_eq!(*c, b':'),
            _ => assert!(c.is_ascii_digit(), "{date}"),
        }
    }
    assert!(path.bytes().all(|c| c.is_ascii_graphic()), "{path}");
    (decode_path(path).unwrap(), date.to_string())
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap()
}

#[test]
fn a_tr_1_home_trash_info_and_collisions() {
    let t = test_dir("trash-afs1");
    let data = t.join("data");
    std::fs::create_dir_all(t.join("work")).unwrap();
    let odd: &[u8] = b"new\nline \xff name";
    for n in [&b"f"[..], odd, b"c", b"d"] {
        write(&t.join("work").join(os(n)), n);
    }
    // Collisions: an info file only, and a files entry only.
    std::fs::create_dir_all(data.join("Trash/info")).unwrap();
    std::fs::create_dir_all(data.join("Trash/files")).unwrap();
    write(&data.join("Trash/info/c.trashinfo"), b"x");
    write(&data.join("Trash/files/d"), b"x");
    let mut ui = Script::silent();
    let r = trash(&mut ui, &t.join("work"), &[b"f", odd, b"c", b"d"], &data);
    assert_eq!((r.done, r.failed, r.skipped), (4, 0, 0), "{r:?}");
    assert!(ui.asked.is_empty());
    let work = canonical(&t.join("work"));
    for (n, trash_name) in [
        (&b"f"[..], &b"f"[..]),
        (odd, odd),
        (b"c", b"c.2"),
        (b"d", b"d.2"),
    ] {
        assert!(!t.join("work").join(os(n)).exists());
        let files = data.join("Trash/files").join(os(trash_name));
        assert_eq!(std::fs::read(&files).unwrap(), n);
        let mut info = trash_name.to_vec();
        info.extend_from_slice(b".trashinfo");
        let (path, _) = read_info(&data.join("Trash/info").join(os(&info)));
        assert_eq!(
            path,
            work.join(os(n)).as_os_str().as_bytes(),
            "absolute original path"
        );
    }
    for d in ["Trash", "Trash/files", "Trash/info"] {
        let m = std::fs::metadata(data.join(d)).unwrap();
        assert!(m.is_dir());
    }
}

#[test]
fn a_tr_2_symlink_is_trashed_not_its_target() {
    let t = test_dir("trash-atr2");
    let data = t.join("data");
    std::fs::create_dir_all(t.join("work/targetdir")).unwrap();
    write(&t.join("work/targetdir/inside"), b"kept");
    symlink("targetdir", t.join("work/link")).unwrap();
    let r = trash(&mut Script::silent(), &t.join("work"), &[b"link"], &data);
    assert_eq!(r.done, 1);
    assert!(t.join("work/link").symlink_metadata().is_err());
    assert!(
        data.join("Trash/files/link")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read(t.join("work/targetdir/inside")).unwrap(),
        b"kept"
    );
    for d in ["Trash", "Trash/files", "Trash/info"] {
        let mode = std::fs::metadata(data.join(d))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "{d} is created 0700");
    }
}

#[test]
fn a_tr_5_symlinked_trash_parts_are_refused() {
    for part in ["Trash", "Trash/files", "Trash/info"] {
        let t = test_dir("trash-atr5");
        let data = t.join("data");
        std::fs::create_dir_all(t.join("work")).unwrap();
        std::fs::create_dir_all(t.join("elsewhere")).unwrap();
        write(&t.join("work/f"), b"f");
        std::fs::create_dir_all(data.join("Trash")).unwrap();
        let p = data.join(part);
        let _ = std::fs::remove_dir(&p);
        symlink(t.join("elsewhere"), &p).unwrap();
        let mut ui = Script::new([Answer::Skip]);
        let r = trash(&mut ui, &t.join("work"), &[b"f"], &data);
        assert!(
            matches!(ui.asked[..], [Question::TrashUnavailable { .. }]),
            "{part}: {:?}",
            ui.asked
        );
        assert_eq!(r.skipped, 1, "{part}");
        assert_eq!(
            std::fs::read(t.join("work/f")).unwrap(),
            b"f",
            "{part}: nothing moved"
        );
        assert_eq!(
            std::fs::read_dir(t.join("elsewhere")).unwrap().count(),
            0,
            "{part}"
        );
    }
}

#[test]
fn a_fs_10_hostile_names_round_trip_through_trash() {
    let t = test_dir("trash-afs10");
    let data = t.join("data");
    std::fs::create_dir_all(t.join("work")).unwrap();
    let long = vec![b'T'; 255];
    let names: Vec<&[u8]> = vec![
        b"new\nline",
        b"-leading",
        b"it's",
        b"bad\xff\xfeutf8",
        &long,
    ];
    for n in &names {
        write(&t.join("work").join(os(n)), n);
    }
    let r = trash(&mut Script::silent(), &t.join("work"), &names, &data);
    assert_eq!((r.done, r.failed), (5, 0), "{r:?}");
    let work = canonical(&t.join("work"));
    let mut seen = 0;
    for e in std::fs::read_dir(data.join("Trash/info"))
        .unwrap()
        .flatten()
    {
        let (path, _) = read_info(&e.path());
        let orig = path.strip_prefix(work.as_os_str().as_bytes()).unwrap()[1..].to_vec();
        assert!(names.contains(&orig.as_slice()), "{orig:?}");
        let info_name = e.file_name().into_vec();
        let trash_name = &info_name[..info_name.len() - ".trashinfo".len()];
        assert_eq!(
            std::fs::read(data.join("Trash/files").join(os(trash_name))).unwrap(),
            orig
        );
        if orig == long {
            assert!(
                trash_name.len() < 255,
                "the 255-byte name is shortened in the trash"
            );
            assert!(info_name.len() <= 255);
        } else {
            assert_eq!(trash_name, orig.as_slice());
        }
        seen += 1;
    }
    assert_eq!(seen, 5);
}

#[test]
fn entry_inside_trash_is_refused() {
    let t = test_dir("trash-inside");
    let data = t.join("data");
    std::fs::create_dir_all(data.join("Trash/files")).unwrap();
    write(&data.join("Trash/files/old"), b"o");
    let r = trash(
        &mut Script::silent(),
        &data.join("Trash/files"),
        &[b"old"],
        &data,
    );
    assert_eq!(
        r.issues[0].outcome,
        Outcome::Skipped("already in trash".into())
    );
    assert!(data.join("Trash/files/old").exists());
}

#[test]
fn a_tr_3_top_directory_methods() {
    // The automated half of A-TR-3: a separate filesystem (tmpfs mounted in a user
    // namespace) gets `$top/.Trash-$uid` with a relative Path; a prepared sticky `.Trash`
    // selects method 1.
    if !in_userns("a_tr_3_top_directory_methods") {
        return;
    }
    let t = test_dir("trash-atr3");
    let data = t.join("data");
    let m = t.join("m");
    std::fs::create_dir_all(&m).unwrap();
    mount_tmpfs(&m);
    std::fs::create_dir_all(m.join("sub")).unwrap();
    write(&m.join("sub/f"), b"f");
    let uid = rustix::process::getuid().as_raw();
    let r = trash(&mut Script::silent(), &m.join("sub"), &[b"f"], &data);
    assert_eq!(r.done, 1, "{r:?}");
    let can = m.join(format!(".Trash-{uid}"));
    assert_eq!(std::fs::read(can.join("files/f")).unwrap(), b"f");
    let (path, _) = read_info(&can.join("info/f.trashinfo"));
    assert_eq!(path, b"sub/f", "relative to the top directory");
    assert_eq!(
        std::fs::metadata(&can).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(
        !data.join("Trash").exists(),
        "the home trash is not used across filesystems"
    );

    std::fs::create_dir(m.join(".Trash")).unwrap();
    std::fs::set_permissions(m.join(".Trash"), std::fs::Permissions::from_mode(0o1777)).unwrap();
    write(&m.join("sub/g"), b"g");
    let r = trash(&mut Script::silent(), &m.join("sub"), &[b"g"], &data);
    assert_eq!(r.done, 1);
    assert_eq!(
        std::fs::read(m.join(format!(".Trash/{uid}/files/g"))).unwrap(),
        b"g"
    );

    // A .Trash without the sticky bit is skipped; method 2 takes the entry.
    std::fs::set_permissions(m.join(".Trash"), std::fs::Permissions::from_mode(0o777)).unwrap();
    write(&m.join("sub/h"), b"h");
    let r = trash(&mut Script::silent(), &m.join("sub"), &[b"h"], &data);
    assert_eq!(r.done, 1);
    assert_eq!(std::fs::read(can.join("files/h")).unwrap(), b"h");
    umount(&m);
}

#[test]
fn a_tr_4_no_usable_trash_never_deletes_on_its_own() {
    if !in_userns("a_tr_4_no_usable_trash_never_deletes_on_its_own") {
        return;
    }
    let t = test_dir("trash-atr4");
    let data = t.join("data");
    let m = t.join("m");
    std::fs::create_dir_all(&m).unwrap();
    mount_tmpfs(&m);
    let uid = rustix::process::getuid().as_raw();
    // Method 1 absent, method 2 unusable (a regular file where the directory would be).
    write(&m.join(format!(".Trash-{uid}")), b"not a directory");
    std::fs::create_dir_all(m.join("tree/sub")).unwrap();
    write(&m.join("tree/sub/f"), b"f");

    let mut ui = Script::new([Answer::Skip]);
    let r = trash(&mut ui, &m, &[b"tree"], &data);
    match &ui.asked[..] {
        [Question::TrashUnavailable { reason, .. }] => {
            assert!(reason.contains("not a directory"), "{reason}")
        }
        q => panic!("{q:?}"),
    }
    assert_eq!(r.skipped, 1);
    assert!(m.join("tree/sub/f").exists());

    // "Delete permanently..." leads to the typed confirmation; without it nothing goes.
    let mut ui = Script::new([Answer::DeletePermanently, Answer::Cancel]);
    let r = trash(&mut ui, &m, &[b"tree"], &data);
    match &ui.asked[1] {
        Question::ConfirmDelete {
            files,
            dirs,
            single,
            ..
        } => {
            assert_eq!((*files, *dirs), (1, 2));
            assert!(single.as_ref().unwrap().ends_with("tree"));
        }
        q => panic!("{q:?}"),
    }
    assert!(m.join("tree/sub/f").exists());
    assert_eq!(r.done, 0);

    let mut ui = Script::new([Answer::DeletePermanently, Answer::Confirm]);
    let r = trash(&mut ui, &m, &[b"tree"], &data);
    assert!(!m.join("tree").exists());
    assert!(
        r.notes.iter().any(|n| n.contains("deleted permanently")),
        "{r:?}"
    );
    umount(&m);
}
