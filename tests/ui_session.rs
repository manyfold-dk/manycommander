//! The binary on a pty (expectrl + vt100): start and quit, signals, hand-offs, dialogs,
//! tabs and the session restore.

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
    // The offset is taken before SIGCONT: waiting for the state already reads output, and
    // the re-entry can be in it.
    let resumed = t.raw.len();
    t.signal(rustix::process::Signal::CONT);
    assert!(t.wait_until(T, |t| t.state() != Some('T')));
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
    t.keys(&[DOWN, CTRL_E]);
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
    // The pager marks that it runs before it reads: keys typed earlier could reach
    // manycommander before it parks its input thread, however long a fixed sleep waits.
    let script = h.join("pager.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\n: > \"$OUT.ready\"\nread line\nprintf '%s|%s' \"$line\" \"$1\" > \"$OUT\"\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = h.join("pager.out");
    let started = h.join("pager.out.ready");
    let log = h.join("mc.log");
    let mut t = Tui::spawn(
        &["--log", log.to_str().unwrap()],
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
    // Rows: .., doc.txt, mc.log, pager.sh (names sort naturally; the cursor starts on ..).
    t.keys(&[DOWN, F3]);
    let log_text = || std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        t.wait_until(T, |_| started.exists()),
        "the pager did not start:\n{}\n{}",
        t.screen(),
        log_text()
    );
    t.send(b"typed words\r");
    assert!(
        t.wait_until(T, |_| out.exists()
            && std::fs::read(&out).map(|b| !b.is_empty()).unwrap_or(false)),
        "the pager got no line:\n{}\n{}",
        t.screen(),
        log_text()
    );
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

#[test]
fn tabs_and_restore_across_restarts() {
    let h = test_dir("ui-tabs");
    std::fs::create_dir_all(h.join("one/inner")).unwrap();
    std::fs::create_dir_all(h.join("two")).unwrap();
    let one = h.join("one");
    let log = h.join("tabs.log");
    let mut t = Tui::spawn(
        &[
            "--log",
            log.to_str().unwrap(),
            one.to_str().unwrap(),
            h.path.to_str().unwrap(),
        ],
        &h.path,
        &[],
        120,
        30,
    );
    ready(&mut t);
    assert!(t.wait_for("inner", T));
    // Ctrl+T: a second tab; the tab bar appears.
    t.keys(&[b"\x14"]);
    assert!(t.wait_for("1:one", T), "{}", t.screen());
    assert!(t.screen().contains("2:one"));
    // Into inner in tab 2, then Alt+PgUp back to tab 1 and Alt+2 (legacy) to tab 2.
    t.keys(&[DOWN, ENTER]);
    assert!(t.wait_for("2:inner", T), "{}", t.screen());
    t.keys(&[b"\x1b[5;3~"]);
    assert!(t.wait_for("inner", T));
    t.keys(&[b"\x1b2"]);
    // A command for the history.
    t.keys(&[CTRL_E]);
    for c in b"true" {
        t.keys(&[std::slice::from_ref(c)]);
    }
    t.keys(&[ENTER]);
    assert!(t.wait_until(T, |t| {
        String::from_utf8_lossy(&t.raw).contains("press Enter to return")
    }));
    t.keys(&[ENTER, F10]);
    let code = t.wait_exit(T);
    assert_eq!(
        code,
        Some(0),
        "{}\n{}",
        t.screen(),
        std::fs::read_to_string(&log).unwrap_or_default()
    );
    let state = std::fs::read_to_string(h.join(".local/state/manycommander/state.toml")).unwrap();
    assert!(state.contains("inner"), "{state}");
    // Restart without arguments: two tabs on the left, the second active, history kept.
    let mut t = Tui::spawn(&[], &h.path, &[], 120, 30);
    ready(&mut t);
    assert!(t.wait_for("2:inner", T), "{}", t.screen());
    assert!(t.screen().contains("1:one"));
    t.keys(&[b"\x10"]);
    assert!(
        t.wait_for("$ true", T),
        "Ctrl+P brings back the command:\n{}",
        t.screen()
    );
    // Ctrl+W (line not empty: kills the word); Esc, then Ctrl+W closes the tab.
    t.keys(&[ESC, b"\x17"]);
    std::thread::sleep(Duration::from_millis(200));
    t.pump();
    assert!(!t.screen().contains("2:inner"), "{}", t.screen());
    t.keys(&[F10]);
    assert_eq!(t.wait_exit(T), Some(0));
}

#[test]
fn window_resize_redraws_without_a_key_press() {
    // A terminal going fullscreen only changes the pty size; no key arrives.
    let h = test_dir("ui-resize");
    std::fs::create_dir_all(h.join("somedir")).unwrap();
    let log = h.join("resize.log");
    let mut t = Tui::spawn(&["--log", log.to_str().unwrap()], &h.path, &[], 80, 24);
    ready(&mut t);
    let bottom = |t: &Tui, row: u16| t.parser.screen().contents_between(row, 0, row, 200);
    assert!(bottom(&t, 23).contains("10Quit"));
    t.resize(160, 50);
    assert!(
        t.wait_until(T, |t| bottom(t, 49).contains("10Quit")),
        "the function-key bar did not move to the new last row:\n{}",
        t.screen()
    );
    let top = t.parser.screen().contents_between(0, 0, 0, 160);
    assert!(
        top.trim_end().ends_with('┐'),
        "the right panel reaches column 160: {top:?}"
    );
    // And back: shrinking redraws too.
    t.resize(100, 30);
    assert!(
        t.wait_until(T, |t| bottom(t, 29).contains("10Quit")),
        "{}",
        t.screen()
    );
    // A key right behind a resize is not lost (crossterm's default source dropped the
    // terminal's readiness when SIGWINCH came in the same batch).
    t.resize(120, 40);
    t.send(F10);
    let code = t.wait_exit(T);
    assert_eq!(
        code,
        Some(0),
        "{}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
}

/// F3 on a picture and `Enter` on a text file hand the file to `gio open` (TDR-1 6, amendments
/// of 2026-10-05): recording `gio` and `xdg-open` stubs first on `PATH` see `gio open` with
/// each path and no `xdg-open`, and no pager runs.
#[test]
fn f3_and_enter_open_through_gio() {
    use std::os::unix::fs::PermissionsExt;
    let h = test_dir("ui-f3-open");
    std::fs::create_dir_all(h.join("d")).unwrap();
    std::fs::write(h.join("d/notes.txt"), b"text").unwrap();
    std::fs::write(h.join("d/pic.png"), b"x").unwrap();
    std::fs::create_dir_all(h.join("bin")).unwrap();
    let log = h.join("opened.log");
    for name in ["gio", "xdg-open"] {
        let stub = h.join("bin").join(name);
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"{name} $*\" >> '{}'\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!(
        "{}:{}",
        h.join("bin").display(),
        std::env::var("PATH").unwrap()
    );
    let pager_ran = h.join("pager-ran");
    let pager = format!("touch {}", pager_ran.display());
    let d = h.join("d");
    let mut t = Tui::spawn(
        &[d.to_str().unwrap(), d.to_str().unwrap()],
        &h.path,
        &[("PATH", &path), ("PAGER", &pager)],
        100,
        30,
    );
    assert!(t.wait_for("10Quit", T), "{}", t.screen());
    assert!(t.wait_for("pic", T), "{}", t.screen());
    let lines = || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    // Rows: .., notes.txt, pic.png.
    t.keys(&[DOWN, ENTER]);
    assert!(t.wait_until(T, |_| lines().len() == 1), "{}", t.screen());
    t.keys(&[DOWN, F3]);
    assert!(t.wait_until(T, |_| lines().len() == 2), "{}", t.screen());
    assert_eq!(
        lines(),
        [
            format!("gio open {}", d.join("notes.txt").display()),
            format!("gio open {}", d.join("pic.png").display()),
        ]
    );
    assert!(!pager_ran.exists(), "no pager for a picture");
    t.keys(&[F10]);
    assert_eq!(t.wait_exit(T), Some(0), "{}", t.screen());
}

/// Runs `cmd` from the command line and waits until the shell has run its first command,
/// which creates `started` in the panel's directory.
fn run_command(t: &mut Tui, cmd: &[u8], started: &std::path::Path) {
    t.keys(&[CTRL_E]);
    for c in cmd {
        t.keys(&[std::slice::from_ref(c)]);
    }
    t.keys(&[ENTER]);
    assert!(
        t.wait_until(T, |_| started.exists()),
        "the command did not start:\n{}",
        t.screen()
    );
}

/// Whether the child stopped and manycommander continued it, as `--log` records it.
fn continued(log: &std::path::Path) -> bool {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .contains("hand-off child stopped: continued")
}

/// A `Ctrl+Z` typed while a command-line child runs reaches the whole foreground group: the
/// shell and its command stop, and manycommander drops its own copy (TDR-1 6). manycommander
/// continues them at once, so the command finishes and the hand-off ends as usual instead of
/// waiting for a `SIGCONT` from elsewhere.
#[test]
fn ctrl_z_during_a_hand_off_does_not_freeze_it() {
    let h = test_dir("ui-handoff-tstp");
    write(&h.join("afile.txt"), b"x");
    let log = h.join("mc.log");
    let mut t = Tui::spawn_job(&["--log", log.to_str().unwrap()], &h.path, &[], 100, 30);
    ready(&mut t);
    assert!(t.wait_for("afile.txt", T), "{}", t.screen());
    run_command(
        &mut t,
        b": > started; sleep 2; echo done",
        &h.join("started"),
    );
    // Ctrl+Z on the terminal, now in its normal mode: the kernel sends SIGTSTP.
    t.send(b"\x1a");
    assert!(
        t.wait_until(T, |_| continued(&log)),
        "the child was not continued:\n{}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
    assert!(
        t.wait_until(T, |t| String::from_utf8_lossy(&t.raw)
            .contains("[exit 0] press Enter to return")),
        "the hand-off did not end:\n{}",
        t.screen()
    );
    t.keys(&[ENTER]);
    assert!(t.wait_for("10Quit", T), "{}", t.screen());
    t.keys(&[F10]);
    assert_eq!(t.wait_exit(T), Some(0), "{}", t.screen());
}

/// A child that suspends itself as an editor's `:suspend` does (it sends `SIGTSTP` to its
/// process group, manycommander's) is continued, and goes on to the end.
#[test]
fn a_child_that_suspends_itself_is_continued() {
    let h = test_dir("ui-handoff-suspend");
    write(&h.join("afile.txt"), b"x");
    let log = h.join("mc.log");
    let mut t = Tui::spawn_job(&["--log", log.to_str().unwrap()], &h.path, &[], 100, 30);
    ready(&mut t);
    assert!(t.wait_for("afile.txt", T), "{}", t.screen());
    run_command(
        &mut t,
        b": > started; kill -TSTP 0; : > resumed",
        &h.join("started"),
    );
    assert!(
        t.wait_until(T, |_| h.join("resumed").exists()),
        "the child was not continued:\n{}\n{}",
        t.screen(),
        std::fs::read_to_string(&log).unwrap_or_default()
    );
    assert!(
        continued(&log),
        "{}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
    assert!(
        t.wait_until(T, |t| String::from_utf8_lossy(&t.raw)
            .contains("[exit 0] press Enter to return")),
        "{}",
        t.screen()
    );
    t.keys(&[ENTER]);
    assert!(
        t.wait_for("10Quit", T),
        "manycommander kept running:\n{}",
        t.screen()
    );
    t.keys(&[F10]);
    assert_eq!(t.wait_exit(T), Some(0), "{}", t.screen());
}

/// The pids of `pid`'s children.
fn children(pid: i32) -> Vec<i32> {
    std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|p| p.parse().ok())
        .collect()
}

/// Whether process `pid` has ended (gone, or a zombie its new parent has not reaped yet).
fn ended(pid: i32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| {
            s.rsplit_once(')')
                .map(|(_, a)| a.trim_start().starts_with('Z'))
        })
        .unwrap_or(true)
}

/// The terminal goes away while a command-line child runs (its window is closed): the pty
/// hangs up, and the child dies of the `SIGHUP`. manycommander, waiting for a key under
/// `press Enter to return`, stops waiting and ends on its own `SIGHUP`, instead of reading
/// the hung-up terminal's end-of-file at full speed forever.
#[test]
fn a_hang_up_during_a_hand_off_ends_manycommander() {
    let h = test_dir("ui-handoff-hup");
    write(&h.join("afile.txt"), b"x");
    let log = h.join("mc.log");
    let mut t = Tui::spawn_job(&["--log", log.to_str().unwrap()], &h.path, &[], 100, 30);
    ready(&mut t);
    assert!(t.wait_for("afile.txt", T), "{}", t.screen());
    run_command(&mut t, b": > started; sleep 30", &h.join("started"));
    let mc = children(t.pid());
    assert_eq!(mc.len(), 1, "the shell runs manycommander: {mc:?}");
    let mc = mc[0];
    // The terminal's side of the pty closes, and the shell is killed.
    drop(t);
    let end = std::time::Instant::now() + T;
    while !ended(mc) && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    if ended(mc) {
        return;
    }
    // The state of each thread (`R`: running), for the report.
    let threads: Vec<String> = std::fs::read_dir(format!("/proc/{mc}/task"))
        .map(|d| {
            d.flatten()
                .filter_map(|t| std::fs::read_to_string(t.path().join("stat")).ok())
                .map(|s| s.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
                .collect()
        })
        .unwrap_or_default();
    let _ = rustix::process::kill_process(
        rustix::process::Pid::from_raw(mc).unwrap(),
        rustix::process::Signal::KILL,
    );
    panic!(
        "manycommander kept running after the hang-up: {threads:?}\n{}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
}

/// manycommander cannot write its screen (here its output is `/dev/full`): it reports the
/// error and exits with raw mode, which it had already turned on, turned off again.
#[test]
fn a_failed_start_leaves_the_terminal_as_it_was() {
    let h = test_dir("ui-enter-fails");
    // The probe's queries go to the output too, so no answer is awaited.
    let silent = Term {
        silent: true,
        ..Term::default()
    };
    let mut t = Tui::spawn_sh("exec \"$@\" > /dev/full", &[], &h.path, &[], silent);
    assert_eq!(t.wait_exit(T), Some(1), "{}", t.screen());
    assert!(t.screen().contains("manycommander: "), "{}", t.screen());
    assert!(t.canonical(), "raw mode is still on");
}
