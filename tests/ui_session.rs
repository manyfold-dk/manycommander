//! T7-T10: the binary on a pty (expectrl + vt100).

mod common;

use common::tui::*;
use common::*;
use std::time::Duration;

const T: Duration = Duration::from_secs(10);

fn ready(t: &mut Tui) {
    assert!(
        t.wait_for("10Quit", T),
        "no function-key bar:\n{}",
        t.screen()
    );
}

#[test]
fn start_and_quit_with_f10() {
    let h = test_dir("ui-quit");
    std::fs::create_dir_all(h.join("somedir")).unwrap();
    write(&h.join("afile.txt"), b"x");
    let mut t = Tui::spawn(&[], &h.path, &[], 100, 30);
    ready(&mut t);
    assert!(t.wait_for("somedir", T), "{}", t.screen());
    t.send(F10);
    assert_eq!(t.wait_exit(T), Some(0));
    assert!(t.restored(), "the terminal is restored");
}

#[test]
fn sigtstp_stops_and_sigcont_resumes() {
    let h = test_dir("ui-tstp");
    let mut t = Tui::spawn(&[], &h.path, &[], 100, 30);
    ready(&mut t);
    let before = t.raw.len();
    t.signal(rustix::process::Signal::TSTP);
    assert!(
        t.wait_until(T, |t| t.state() == Some('T')),
        "stopped: {:?}",
        t.state()
    );
    t.pump();
    assert!(
        find_last(&t.raw[before..], b"\x1b[?1049l").is_some(),
        "the terminal was restored before stopping"
    );
    t.signal(rustix::process::Signal::CONT);
    assert!(t.wait_until(T, |t| t.state() != Some('T')));
    let resumed = t.raw.len();
    assert!(
        t.wait_until(T, |t| find_last(&t.raw[resumed..], b"\x1b[?1049h")
            .is_some()),
        "the terminal was taken back"
    );
    assert!(t.wait_for("10Quit", T));
    t.send(F10);
    assert_eq!(t.wait_exit(T), Some(0));
    assert!(t.restored());
}

#[test]
fn sigterm_exits_with_the_terminal_restored() {
    let h = test_dir("ui-term");
    let mut t = Tui::spawn(&[], &h.path, &[], 100, 30);
    ready(&mut t);
    t.signal(rustix::process::Signal::TERM);
    assert_eq!(t.wait_exit(T), Some(0));
    assert!(t.restored());
}

#[test]
fn f5_directory_with_file_exists_answered_skip() {
    let h = test_dir("ui-f5");
    std::fs::create_dir_all(h.join("src/d")).unwrap();
    std::fs::create_dir_all(h.join("dst/d")).unwrap();
    write(&h.join("src/d/f"), b"new");
    write(&h.join("src/d/g"), b"g");
    write(&h.join("dst/d/f"), b"old");
    let (l, r) = (h.join("src"), h.join("dst"));
    let mut t = Tui::spawn(
        &[l.to_str().unwrap(), r.to_str().unwrap()],
        &h.path,
        &[],
        100,
        30,
    );
    ready(&mut t);
    assert!(t.wait_for(" d ", T));
    t.keys(&[DOWN, F5]);
    assert!(t.wait_for("Copy", T), "{}", t.screen());
    t.keys(&[ENTER]);
    assert!(t.wait_for("Directory exists", T), "{}", t.screen());
    t.keys(&[ENTER]); // Merge (default)
    assert!(t.wait_for("File exists", T), "{}", t.screen());
    t.keys(&[ENTER]); // Skip (default)
    assert!(t.wait_for("Report", T), "{}", t.screen());
    assert!(t.screen().contains("skipped"), "{}", t.screen());
    assert!(t.screen().contains("1 copied"), "{}", t.screen());
    assert_eq!(std::fs::read(h.join("dst/d/f")).unwrap(), b"old");
    assert_eq!(std::fs::read(h.join("dst/d/g")).unwrap(), b"g");
    t.keys(&[ENTER, F10]);
    assert_eq!(t.wait_exit(T), Some(0));
}

#[test]
fn shift_f8_without_typing_delete_deletes_nothing() {
    let h = test_dir("ui-del");
    std::fs::create_dir_all(h.join("work/tree/sub")).unwrap();
    write(&h.join("work/tree/sub/f"), b"f");
    let w = h.join("work");
    let mut t = Tui::spawn(&[w.to_str().unwrap()], &h.path, &[], 100, 30);
    ready(&mut t);
    assert!(t.wait_for("tree", T));
    t.keys(&[DOWN, SHIFT_F8]);
    assert!(t.wait_for("Delete permanently", T), "{}", t.screen());
    t.keys(&[ENTER]);
    assert!(t.wait_for("Type delete", T), "{}", t.screen());
    // Enter without the word does nothing; a different word does nothing either.
    t.keys(&[ENTER, b"d", b"e", b"l", ENTER]);
    assert!(t.screen().contains("Type delete"), "{}", t.screen());
    assert!(h.join("work/tree/sub/f").exists());
    t.keys(&[ESC]);
    assert!(t.wait_for("nothing was deleted", T), "{}", t.screen());
    assert!(h.join("work/tree/sub/f").exists());
    t.keys(&[ENTER, F10]);
    assert_eq!(t.wait_exit(T), Some(0));
}

#[test]
fn f10_during_a_job_asks_then_cancels() {
    let Some(x) = xdev_dir("ui-f10job") else {
        return;
    };
    let h = test_dir("ui-f10job");
    std::fs::create_dir_all(x.join("many")).unwrap();
    for i in 0..20_000 {
        write(&x.join(format!("many/f{i:05}")), b"0123456789");
    }
    let mut t = Tui::spawn(
        &[x.path.to_str().unwrap(), h.path.to_str().unwrap()],
        &h.path,
        &[],
        100,
        30,
    );
    ready(&mut t);
    assert!(t.wait_for("many", T));
    t.keys(&[DOWN, F5, ENTER]);
    assert!(t.wait_for("copy", T), "{}", t.screen());
    t.keys(&[F10]);
    assert!(t.wait_for("Cancel it and quit?", T), "{}", t.screen());
    t.keys(&[ENTER]);
    assert_eq!(t.wait_exit(Duration::from_secs(30)), Some(0));
    assert!(t.restored());
    let copied = std::fs::read_dir(h.join("many"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert!(copied < 20_000, "the job was cancelled ({copied} copied)");
    let partial = std::fs::read_dir(h.join("many"))
        .map(|d| {
            d.flatten()
                .any(|e| e.file_name().to_string_lossy().contains(".mc-partial-"))
        })
        .unwrap_or(false);
    assert!(!partial);
}

#[test]
fn a_fs_10_command_line_insert_is_one_argument() {
    let h = test_dir("ui-cmdline");
    // A quote, a newline, a leading dash inside, an invalid byte and shell syntax.
    let name: Vec<u8> = b"it's a\n-x \xff $HOME `id`".to_vec();
    write(&h.join(os(&name)), b"x");
    let mut t = Tui::spawn(&[], &h.path, &[], 120, 30);
    ready(&mut t);
    assert!(t.wait_for("it's", T), "{}", t.screen());
    t.keys(&[DOWN]);
    for c in b"printf '%s\\0' ".iter() {
        t.keys(&[std::slice::from_ref(c)]);
    }
    t.keys(&[ALT_ENTER]);
    for c in b"> out.bin".iter() {
        t.keys(&[std::slice::from_ref(c)]);
    }
    t.keys(&[ENTER]);
    assert!(t.wait_until(T, |t| {
        String::from_utf8_lossy(&t.raw).contains("press Enter to return")
    }));
    t.keys(&[ENTER]);
    assert!(t.wait_for("10Quit", T));
    let out = std::fs::read(h.join("out.bin")).unwrap();
    let mut want = name.clone();
    want.push(0);
    assert_eq!(out, want, "exactly one argument, equal to the name");
    t.keys(&[F10]);
    assert_eq!(t.wait_exit(T), Some(0));
}

#[test]
fn pager_child_gets_the_keystrokes() {
    let h = test_dir("ui-pager");
    write(&h.join("doc.txt"), b"hello");
    let script = h.join("pager.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nread line\nprintf '%s|%s' \"$line\" \"$1\" > \"$OUT\"\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = h.join("pager.out");
    let mut t = Tui::spawn(
        &[],
        &h.path,
        &[
            ("PAGER", script.to_str().unwrap()),
            ("OUT", out.to_str().unwrap()),
        ],
        100,
        30,
    );
    ready(&mut t);
    assert!(t.wait_for("doc.txt", T));
    // Rows: .., doc.txt, pager.sh (names sort naturally; the cursor starts on ..).
    t.keys(&[DOWN, F3]);
    std::thread::sleep(Duration::from_millis(300));
    t.send(b"typed words\r");
    assert!(t.wait_until(T, |_| out.exists()
        && std::fs::read(&out).map(|b| !b.is_empty()).unwrap_or(false)));
    let got = std::fs::read_to_string(&out).unwrap();
    assert_eq!(got, format!("typed words|{}", h.join("doc.txt").display()));
    assert!(t.wait_for("10Quit", T));
    std::thread::sleep(Duration::from_millis(200));
    t.pump();
    assert!(
        !t.screen().contains("typed words"),
        "manycommander did not read the child's keys:\n{}",
        t.screen()
    );
    t.keys(&[F10]);
    assert_eq!(t.wait_exit(T), Some(0));
}
