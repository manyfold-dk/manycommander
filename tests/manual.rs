//! T14: the design's manual checks, run by a session on the real desktop and recorded in
//! the plan's execution record. Every test is `#[ignore]`d and also refuses to run without
//! `MC_MANUAL=1`, because some of them change desktop state (a theme switch, a hook, loop
//! devices). Run one with:
//!
//! ```text
//! MC_MANUAL=1 cargo test --test manual -- --ignored --nocapture --test-threads=1 <name>
//! ```
//!
//! `MC_BIN` selects the binary (default: the test build).

mod common;

use common::tui::*;
use common::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

const T: Duration = Duration::from_secs(15);

fn guard() {
    assert_eq!(
        std::env::var("MC_MANUAL").as_deref(),
        Ok("1"),
        "manual checks change desktop state; set MC_MANUAL=1 to run them"
    );
}

fn evidence(id: &str, text: &str) {
    println!("EVIDENCE {id}: {text}");
}

fn sh(cmd: &str) -> (i32, String) {
    let o = Command::new("sh").arg("-c").arg(cmd).output().unwrap();
    let mut s = String::from_utf8_lossy(&o.stdout).to_string();
    s.push_str(&String::from_utf8_lossy(&o.stderr));
    (o.status.code().unwrap_or(-1), s)
}

// ---- A-UI-1 ---------------------------------------------------------------------------------

#[test]
#[ignore]
fn a_ui_1_stuck_fuse_mount() {
    guard();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/fixtures/stall-fuse.sh");
    let me = std::env::current_exe().unwrap();
    // Re-enter this test inside the fixture, which exports MC_STALL_DIR.
    if std::env::var_os("MC_STALL_DIR").is_none() {
        let st = Command::new(&script)
            .arg(&me)
            .args([
                "--exact",
                "a_ui_1_stuck_fuse_mount",
                "--ignored",
                "--nocapture",
            ])
            .status()
            .unwrap();
        assert!(st.success());
        return;
    }
    let base = PathBuf::from(std::env::var_os("MC_STALL_DIR").unwrap());
    let home = test_dir("manual-aui1");
    let mut t = Tui::spawn(&[base.to_str().unwrap()], &home.path, &[], 120, 30);
    assert!(t.wait_for("10Quit", T));
    assert!(t.wait_for("stuck", T), "{}", t.screen());
    // Rows: .., fine, src, stuck.
    t.keys(&[DOWN, DOWN, DOWN, ENTER]);
    assert!(t.wait_for("(loading)", T), "{}", t.screen());
    std::thread::sleep(Duration::from_millis(500));
    t.pump();
    assert!(
        t.screen().contains("(loading)"),
        "still loading after 500 ms"
    );
    let esc = Instant::now();
    t.send(ESC);
    let back = t.wait_until(Duration::from_secs(2), |t| {
        !t.screen().contains("(loading)") && t.screen().contains("stuck")
    });
    let esc_ms = esc.elapsed().as_secs_f64() * 1000.0;
    assert!(back);
    evidence(
        "A-UI-1",
        &format!(
            "Esc returned the panel to its previous directory in {esc_ms:.0} ms (screen poll every 10 ms)"
        ),
    );
    // The same directory again: refused while the first load is stuck.
    t.keys(&[ENTER]);
    assert!(t.wait_for("still blocked", T), "{}", t.screen());
    evidence(
        "A-UI-1",
        "a second load of the stuck directory was refused: \"previous load of this directory is still blocked\"",
    );
    // Another directory still loads.
    t.keys(&[b"\x1b[A", b"\x1b[A", ENTER]);
    assert!(t.wait_for(" ok ", T), "{}", t.screen());
    evidence(
        "A-UI-1",
        "another directory (fine/) loaded while the stuck load was pending",
    );
    t.keys(&[F10]);
    assert_eq!(t.wait_exit(T), Some(0));
    evidence(
        "A-UI-1",
        "F10 quit with a load still blocked in the kernel; exit 0, terminal restored",
    );
    assert!(esc_ms <= 100.0, "Esc took {esc_ms:.0} ms");
}

// ---- A-UI-3 ---------------------------------------------------------------------------------

/// Kills (SIGKILL) the child of `parent` named `name`, never any other process.
fn kill_child_of(parent: i32, name: &str) -> bool {
    let (rc, out) = sh(&format!("pgrep -P {parent} -x {name}"));
    if rc != 0 {
        return false;
    }
    let pid: i32 = out.lines().next().unwrap().trim().parse().unwrap();
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(pid).unwrap(),
        rustix::process::Signal::KILL,
    )
    .is_ok()
}

fn has_child(parent: i32, name: &str) -> bool {
    sh(&format!("pgrep -P {parent} -x {name}")).0 == 0
}

#[test]
#[ignore]
fn a_ui_3_handoffs_restore_the_terminal() {
    guard();
    let h = test_dir("manual-aui3");
    write(&h.join("doc.txt"), b"line\n");
    let editor = h.join("ed.sh");
    std::fs::write(&editor, "#!/bin/sh\nexec sleep 300\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bin = std::env::var("MC_BIN").ok();
    let _ = bin;
    let mut t = Tui::spawn(
        &[],
        &h.path,
        &[("PAGER", "less"), ("EDITOR", editor.to_str().unwrap())],
        120,
        30,
    );
    assert!(t.wait_for("doc.txt", T));
    t.keys(&[DOWN]);
    // F3: less, quit normally.
    let mark = t.raw.len();
    t.keys(&[F3]);
    assert!(
        t.wait_until(T, |t| find_last(&t.raw[mark..], b"\x1b[?1049l").is_some()),
        "left the alternate screen"
    );
    assert!(t.wait_for("line", T));
    t.keys(&[b"q"]);
    assert!(t.wait_for("10Quit", T));
    evidence(
        "A-UI-3",
        "F3 ran less on the normal screen and returned to a full redraw after q",
    );
    // F3 again, less killed with SIGKILL.
    t.keys(&[F3]);
    let mc = t.pid();
    assert!(t.wait_until(T, |_| has_child(mc, "less")));
    std::thread::sleep(Duration::from_millis(300));
    assert!(kill_child_of(mc, "less"));
    assert!(t.wait_for("10Quit", T), "{}", t.screen());
    evidence(
        "A-UI-3",
        "F3 with less killed by SIGKILL: terminal restored, UI back",
    );
    // F4: the editor (a sleep) killed with SIGKILL.
    t.keys(&[b"\x1bOS"]);
    // The editor script execs sleep, so the sleep is manycommander's child.
    assert!(t.wait_until(T, |_| has_child(mc, "sleep")));
    std::thread::sleep(Duration::from_millis(200));
    assert!(kill_child_of(mc, "sleep"));
    assert!(t.wait_for("10Quit", T));
    assert!(t.screen().contains("killed by signal 9"), "{}", t.screen());
    evidence(
        "A-UI-3",
        "F4 with the editor killed by SIGKILL: terminal restored, status \"[killed by signal 9]\"",
    );
    // Command line, child killed with SIGKILL.
    for c in b"sleep 301" {
        t.keys(&[std::slice::from_ref(c)]);
    }
    t.keys(&[ENTER]);
    // `sh -c 'sleep 301'` execs the sleep: again a direct child.
    assert!(t.wait_until(T, |_| has_child(mc, "sleep")
        || sh(&format!("pgrep -P {mc} -x sh")).0 == 0));
    std::thread::sleep(Duration::from_millis(200));
    if !kill_child_of(mc, "sleep") {
        let (_, sh_pid) = sh(&format!("pgrep -P {mc} -x sh"));
        let sh_pid: i32 = sh_pid.trim().parse().unwrap();
        assert!(kill_child_of(sh_pid, "sleep"));
    }
    assert!(t.wait_until(T, |t| {
        String::from_utf8_lossy(&t.raw).contains("[killed by signal 9] press Enter to return")
    }));
    t.keys(&[ENTER]);
    assert!(t.wait_for("10Quit", T));
    evidence(
        "A-UI-3",
        "command line with the child killed by SIGKILL: \"[killed by signal 9] press Enter to return\", then restored",
    );
    // SIGTSTP / SIGCONT, then SIGTERM.
    t.signal(rustix::process::Signal::TSTP);
    assert!(t.wait_until(T, |t| t.state() == Some('T')));
    t.signal(rustix::process::Signal::CONT);
    assert!(t.wait_until(T, |t| t.state() != Some('T')));
    assert!(t.wait_for("10Quit", T));
    evidence(
        "A-UI-3",
        "SIGTSTP left the terminal and stopped the process (state T); SIGCONT took it back",
    );
    t.signal(rustix::process::Signal::TERM);
    assert_eq!(t.wait_exit(T), Some(0));
    assert!(t.restored());
    evidence(
        "A-UI-3",
        "SIGTERM: exit 0 with the alternate screen left and the protocol popped",
    );
}

// ---- A-TR-1, A-TR-3 (gio) -----------------------------------------------------------------

/// `gio` in its own D-Bus session with its own runtime directory, so the gvfs daemon it
/// starts sees `data` as XDG_DATA_HOME and never the user's real trash or gvfs.
fn gio(data: &Path, args: &str) -> (i32, String) {
    let run = data.parent().unwrap().join("run");
    std::fs::create_dir_all(&run).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (rc, out) = sh(&format!(
        "env -u DBUS_SESSION_BUS_ADDRESS XDG_RUNTIME_DIR='{}' XDG_DATA_HOME='{}' GVFS_DISABLE_FUSE=1 dbus-run-session -- gio {args} 2>&1",
        run.display(),
        data.display()
    ));
    // Drop the bus's own chatter and gvfs helpers that cannot start in a bare session.
    let out = out
        .lines()
        .filter(|l| {
            !l.starts_with("dbus-daemon[") && !l.contains("A connection to the bus can't be made")
        })
        .collect::<Vec<_>>()
        .join("\n");
    (rc, out)
}

fn trash(dir: &Path, names: &[&[u8]], data: &Path) -> manycommander::fsops::job::Report {
    let names: Vec<std::ffi::OsString> = names.iter().map(|n| os(n).to_owned()).collect();
    manycommander::fsops::trash::trash_job_with(
        &manycommander::fsops::sys::Sys::default(),
        &mut Script::silent(),
        dir,
        &names,
        Some(data),
    )
}

#[test]
#[ignore]
fn a_tr_1_gio_lists_and_restores() {
    guard();
    let t = test_dir("manual-atr1");
    let data = t.join("data");
    std::fs::create_dir_all(t.join("work")).unwrap();
    let odd: &[u8] = b"new\nline \xff name";
    write(&t.join("work").join(os(odd)), b"odd");
    write(&t.join("work/plain"), b"plain");
    let r = trash(&t.join("work"), &[odd, b"plain"], &data);
    assert_eq!(r.done, 2);
    let (rc, list) = gio(&data, "trash --list");
    assert_eq!(rc, 0, "{list}");
    let lines: Vec<&str> = list
        .lines()
        .filter(|l| l.starts_with("trash:///"))
        .collect();
    assert_eq!(lines.len(), 2, "gio lists both: {list}");
    evidence(
        "A-TR-1",
        &format!("gio trash --list shows {} entries", lines.len()),
    );
    let uri = lines
        .iter()
        .find(|l| !l.contains("plain"))
        .unwrap()
        .split('\t')
        .next()
        .unwrap();
    let (rc, out) = gio(&data, &format!("trash --restore '{uri}'"));
    assert_eq!(rc, 0, "{out}");
    assert_eq!(std::fs::read(t.join("work").join(os(odd))).unwrap(), b"odd");
    evidence(
        "A-TR-1",
        "gio trash --restore put the name with a newline and a non-UTF-8 byte back at its original path, content intact",
    );
}

fn udisks(args: &str) -> (i32, String) {
    sh(&format!("udisksctl {args} --no-user-interaction"))
}

/// Attaches an image with udisksctl and returns (loop device, mount point); the desktop
/// automounter may mount it first.
fn attach(img: &Path) -> (String, PathBuf) {
    let (rc, out) = udisks(&format!("loop-setup -f '{}'", img.display()));
    assert_eq!(rc, 0, "{out}");
    let dev = out
        .split(" as ")
        .nth(1)
        .unwrap()
        .trim()
        .trim_end_matches('.')
        .to_string();
    let mut mnt = String::new();
    for _ in 0..30 {
        mnt = sh(&format!("findmnt -n -o TARGET -S {dev}"))
            .1
            .trim()
            .to_string();
        if !mnt.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if mnt.is_empty() {
        let (rc, out) = udisks(&format!("mount -b {dev}"));
        assert_eq!(rc, 0, "{out}");
        mnt = out
            .split(" at ")
            .nth(1)
            .unwrap()
            .trim()
            .trim_end_matches('.')
            .to_string();
    }
    (dev, PathBuf::from(mnt))
}

fn detach(dev: &str) {
    udisks(&format!("unmount -b {dev}"));
    for _ in 0..30 {
        if sh(&format!("losetup {dev}")).0 != 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    udisks(&format!("loop-delete -b {dev}"));
}

fn top_dir_round_trip(label: &str, top: &Path, data: &Path) -> String {
    let dir = top.join(format!("mc-{label}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    write(&dir.join("item"), label.as_bytes());
    let r = trash(&dir, &[b"item"], data);
    assert_eq!(r.done, 1, "{r:?}");
    let uid = rustix::process::getuid().as_raw();
    let method1 = top.join(format!(".Trash/{uid}/info/item.trashinfo"));
    let method2 = top.join(format!(".Trash-{uid}/info/item.trashinfo"));
    let (info, method) = if method1.exists() {
        (method1, 1)
    } else {
        (method2, 2)
    };
    let text = std::fs::read_to_string(&info).unwrap();
    let path_line = text
        .lines()
        .find(|l| l.starts_with("Path="))
        .unwrap()
        .to_string();
    let (rc, list) = gio(data, "trash --list");
    let line = list
        .lines()
        .find(|l| l.contains(&format!("mc-{label}-")))
        .map(str::to_string);
    let restored = match &line {
        Some(l) => {
            let uri = l.split('\t').next().unwrap();
            let (rc2, out) = gio(data, &format!("trash --restore '{uri}'"));
            rc2 == 0
                && std::fs::read(dir.join("item"))
                    .map(|b| b == label.as_bytes())
                    .unwrap_or(false)
                || {
                    println!("restore: {out}");
                    false
                }
        }
        None => false,
    };
    let _ = std::fs::remove_dir_all(&dir);
    format!(
        "{label}: method {method}, {path_line} (relative), gio --list rc {rc} {}, gio --restore {}",
        if line.is_some() {
            "shows it"
        } else {
            "does not show it"
        },
        if restored {
            "restored it"
        } else {
            "did not restore it"
        }
    )
}

#[test]
#[ignore]
fn a_tr_3_top_directory_trashes() {
    guard();
    let t = test_dir("manual-atr3");
    let data = t.join("data");
    // tmpfs: /dev/shm is a tmpfs mount; the trash goes to its top directory. Remove what
    // this test created there afterwards.
    let uid = rustix::process::getuid().as_raw();
    let shm_trash = PathBuf::from(format!("/dev/shm/.Trash-{uid}"));
    let existed = shm_trash.exists();
    let e = top_dir_round_trip("tmpfs", Path::new("/dev/shm"), &data);
    evidence("A-TR-3", &e);
    if !existed {
        remove_tree(&shm_trash);
    }
    // vfat image mounted with the user's uid.
    let img = t.join("vfat.img");
    sh(&format!(
        "truncate -s 64M '{}' && mkfs.vfat -n MCVFAT '{}'",
        img.display(),
        img.display()
    ));
    let (dev, mnt) = attach(&img);
    let e = top_dir_round_trip("vfat", &mnt, &data);
    evidence("A-TR-3", &format!("{e} (mounted at {})", mnt.display()));
    // Method 1 on an ext4 image with a prepared sticky .Trash.
    detach(&dev);
    let img = t.join("ext4.img");
    sh(&format!(
        "truncate -s 64M '{}' && mkfs.ext4 -q -F -L MCEXT4 -E root_owner=$(id -u):$(id -g) '{}'",
        img.display(),
        img.display()
    ));
    let (dev, mnt) = attach(&img);
    std::fs::create_dir(mnt.join(".Trash")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(mnt.join(".Trash"), std::fs::Permissions::from_mode(0o1777)).unwrap();
    let e = top_dir_round_trip("ext4-sticky", &mnt, &data);
    evidence("A-TR-3", &e);
    detach(&dev);
}

// ---- A-TH-1 ---------------------------------------------------------------------------------

fn state_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap()).join(".local/state/omarchy/current")
}

fn current_theme() -> String {
    std::fs::read_to_string(state_dir().join("theme.name"))
        .unwrap()
        .trim()
        .to_string()
}

/// The border colour at the top-left corner of the left (active) panel.
fn border(t: &Tui) -> vt100::Color {
    t.parser.screen().cell(0, 0).unwrap().fgcolor()
}

/// The border colour a theme gives the active panel: its accent (else blue), as RGB.
fn accent(theme: &str) -> vt100::Color {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let user = home.join(format!(".config/omarchy/themes/{theme}/colors.toml"));
    let system = PathBuf::from(format!("/usr/share/omarchy/themes/{theme}/colors.toml"));
    let path = if user.exists() { user } else { system };
    let p = manycommander::theme::Palette::load(&path).unwrap();
    let c = p.chain(&["accent", "blue"]).unwrap();
    vt100::Color::Rgb(c.0, c.1, c.2)
}

/// Switches the theme and returns the ms from the reference event to the new theme's
/// accent on the active panel's border.
fn switch(t: &mut Tui, theme: &str, reference: std::sync::mpsc::Receiver<SystemTime>) -> f64 {
    let want = accent(theme);
    let mut child = Command::new("omarchy-theme-set")
        .arg(theme)
        .spawn()
        .unwrap();
    let give_up = Instant::now() + Duration::from_secs(60);
    let changed_at = loop {
        t.pump();
        if border(t) == want {
            break SystemTime::now();
        }
        assert!(Instant::now() < give_up, "the border never showed {want:?}");
        std::thread::sleep(Duration::from_millis(1));
    };
    let _ = child.wait();
    let ev = reference
        .recv_timeout(Duration::from_secs(30))
        .expect("reference event");
    match changed_at.duration_since(ev) {
        Ok(d) => d.as_secs_f64() * 1000.0,
        Err(_) => panic!("the border changed before the reference event"),
    }
}

/// Records the time of the first `mask` event on `name` in `dir`.
fn watch_event(
    dir: PathBuf,
    name: &'static str,
    mask: inotify::WatchMask,
) -> std::sync::mpsc::Receiver<SystemTime> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut ino = inotify::Inotify::init().unwrap();
    ino.watches().add(&dir, mask).unwrap();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            let evs = ino.read_events_blocking(&mut buf).unwrap();
            for e in evs {
                if e.name.is_some_and(|n| n == name) {
                    let _ = tx.send(SystemTime::now());
                    return;
                }
            }
        }
    });
    rx
}

#[test]
#[ignore]
fn a_th_1_live_theme_switch() {
    guard();
    let original = current_theme();
    let other = if original == "catppuccin" {
        "nord"
    } else {
        "catppuccin"
    };
    let hook_dir =
        PathBuf::from(std::env::var_os("HOME").unwrap()).join(".config/omarchy/hooks/theme-set.d");
    let hook = hook_dir.join("manycommander");
    // The watcher part runs with no hook installed: a copy from an earlier run is set aside
    // (omarchy-hook skips `.sample` files) and put back for the hook part.
    let parked = hook_dir.join("manycommander.sample");
    if hook.exists() {
        std::fs::rename(&hook, &parked).unwrap();
    }
    let home = test_dir("manual-ath1");
    let real_home = std::env::var("HOME").unwrap();
    let env: &[(&str, &str)] = &[("COLORTERM", "truecolor"), ("HOME", &real_home)];

    // Watcher on, no hook: from the mv of current/theme to the new accent on the border.
    let mut t = Tui::spawn(&[home.path.to_str().unwrap()], &home.path, env, 120, 30);
    assert!(t.wait_for("10Quit", T));
    assert!(
        t.wait_until(T, |t| border(t) == accent(&original)),
        "starts with {original}'s accent"
    );
    let mv = watch_event(state_dir(), "theme", inotify::WatchMask::MOVED_TO);
    let ms1 = switch(&mut t, other, mv);
    evidence(
        "A-TH-1",
        &format!(
            "watcher, no hook: {original} -> {other}: {:?} on the border {ms1:.0} ms after the mv of current/theme",
            accent(other)
        ),
    );
    let mv = watch_event(state_dir(), "theme", inotify::WatchMask::MOVED_TO);
    let ms2 = switch(&mut t, &original, mv);
    evidence(
        "A-TH-1",
        &format!(
            "watcher, no hook: {other} -> {original}: {:?} on the border {ms2:.0} ms after the mv",
            accent(&original)
        ),
    );
    t.keys(&[F10]);
    t.wait_exit(T);

    // Hook installed (the authorized copy of contrib/omarchy/theme-set-hook.sh), watcher
    // off: from the hook process start (bash opens the hook file) to the new accent.
    std::fs::create_dir_all(&hook_dir).unwrap();
    let _ = std::fs::remove_file(&parked);
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("contrib/omarchy/theme-set-hook.sh"),
        &hook,
    )
    .unwrap();
    let mut t = Tui::spawn(
        &["--no-theme-watch", home.path.to_str().unwrap()],
        &home.path,
        env,
        120,
        30,
    );
    assert!(t.wait_for("10Quit", T));
    assert!(t.wait_until(T, |t| border(t) == accent(&original)));
    let opened = watch_event(hook_dir.clone(), "manycommander", inotify::WatchMask::OPEN);
    let ms3 = switch(&mut t, other, opened);
    evidence(
        "A-TH-1",
        &format!(
            "hook, --no-theme-watch: {original} -> {other}: accent on the border {ms3:.0} ms after the hook started"
        ),
    );
    let opened = watch_event(hook_dir.clone(), "manycommander", inotify::WatchMask::OPEN);
    let ms4 = switch(&mut t, &original, opened);
    evidence(
        "A-TH-1",
        &format!(
            "hook, --no-theme-watch: {other} -> {original}: accent on the border {ms4:.0} ms after the hook started"
        ),
    );
    t.keys(&[F10]);
    t.wait_exit(T);
    assert_eq!(current_theme(), original, "switched back");
    evidence(
        "A-TH-1",
        &format!(
            "the theme is back to {original}; the hook stays installed at {}",
            hook.display()
        ),
    );
    for ms in [ms1, ms2, ms3, ms4] {
        assert!(ms <= 200.0, "{ms} ms");
    }
}

// ---- A-UI-2 (manual half) -----------------------------------------------------------------

#[test]
#[ignore]
fn a_ui_2_external_changes_show_within_a_second() {
    guard();
    let h = test_dir("manual-aui2");
    std::fs::create_dir_all(h.join("w")).unwrap();
    for n in ["b-file", "c-file", "d-file"] {
        write(&h.join("w").join(n), b"x");
    }
    let w = h.join("w");
    let mut t = Tui::spawn(&[w.to_str().unwrap()], &h.path, &[], 120, 30);
    assert!(t.wait_for("d-file", T));
    // Cursor on c-file (rows: .., b-file, c-file, d-file).
    t.keys(&[DOWN, DOWN]);
    let start = Instant::now();
    write(&h.join("w/a-new"), b"x");
    assert!(
        t.wait_for("a-new", Duration::from_secs(1)),
        "{}",
        t.screen()
    );
    let created = start.elapsed();
    let start = Instant::now();
    std::fs::remove_file(h.join("w/d-file")).unwrap();
    assert!(t.wait_until(Duration::from_secs(1), |t| !t.screen().contains("d-file")));
    let removed = start.elapsed();
    evidence(
        "A-UI-2",
        &format!(
            "touch shown after {} ms, rm shown after {} ms",
            created.as_millis(),
            removed.as_millis()
        ),
    );
    // The cursor stayed on c-file: Insert marks the entry under the cursor.
    t.keys(&[b"\x1b[2~"]);
    std::thread::sleep(Duration::from_millis(100));
    t.pump();
    let marked_row = t
        .screen()
        .lines()
        .find(|l| l.contains("▸"))
        .unwrap_or("")
        .to_string();
    assert!(marked_row.contains("c-file"), "{}", t.screen());
    evidence(
        "A-UI-2",
        "the cursor stayed on c-file across both refreshes",
    );
    t.keys(&[F10]);
    assert_eq!(t.wait_exit(T), Some(0));
}

// ---- A-FD-7 (phase 2) -----------------------------------------------------------------------

const ALT_F7: &[u8] = b"\x1b[18;3~";
const F1: &[u8] = b"\x1bOP";
const TAB: &[u8] = b"\t";
const UP: &[u8] = b"\x1b[A";
const INSERT: &[u8] = b"\x1b[2~";
/// A-FD-7 "keeps the UI responsive": every key reaches the screen within this.
const KEY_BUDGET_MS: f64 = 200.0;
/// The fixture's mount points: `mktemp -d /tmp/mc-stall.XXXXXX` in `stall-fuse.sh`.
const STALL_PREFIX: &str = "/tmp/mc-stall.";
const HELP_LINE: &str = "KEYS (command line empty)";

/// `rclone mount SRC MNT ...` processes as (pid, SRC, MNT), found by their exact argv in
/// `/proc`. The fixture's rclone runs with `--daemon`, so it is no child of the fixture and
/// the parent-pid rule cannot find it; an exact argv matches no other process.
fn rclone_mounts() -> Vec<(i32, PathBuf, PathBuf)> {
    use std::os::unix::ffi::OsStrExt;
    let mut v = Vec::new();
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return v;
    };
    for e in rd.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(cmd) = std::fs::read(e.path().join("cmdline")) else {
            continue;
        };
        let argv: Vec<&[u8]> = cmd.split(|&b| b == 0).collect();
        if argv.len() >= 4 && argv[0].ends_with(b"rclone") && argv[1] == b"mount" {
            let path = |b: &[u8]| PathBuf::from(std::ffi::OsStr::from_bytes(b));
            v.push((pid, path(argv[2]), path(argv[3])));
        }
    }
    v
}

/// The state letter of `pid` from `/proc/<pid>/stat` (`T` = stopped).
fn proc_state(pid: i32) -> Option<char> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    s.rsplit_once(')')?.1.trim_start().chars().next()
}

/// The search threads of the running process `pid` (`find`, `find-N`), each as
/// `name:wait channel`.
fn find_threads(pid: i32) -> Vec<String> {
    let mut v = Vec::new();
    let rd = std::fs::read_dir(format!("/proc/{pid}/task")).expect("the process runs");
    for e in rd.flatten() {
        let read = |f: &str| {
            std::fs::read_to_string(e.path().join(f))
                .unwrap_or_default()
                .trim()
                .to_string()
        };
        let comm = read("comm");
        if comm == "find" || comm.starts_with("find-") {
            v.push(format!("{comm}:{}", read("wchan")));
        }
    }
    v.sort();
    v
}

/// The fields of the `find done` lines in `log` (`id=.. dirs=.. results=..`), in order.
fn find_done(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split("find done").nth(1).map(|f| f.trim().to_string()))
        .collect()
}

/// An integer field (`dirs=4`) of a `find done` line.
fn done_field(line: &str, name: &str) -> Option<u64> {
    let prefix = format!("{name}=");
    line.split_whitespace()
        .find_map(|kv| kv.strip_prefix(prefix.as_str()))?
        .parse()
        .ok()
}

/// What the fixture left behind: rclone processes and `fuse.rclone` mounts under its
/// mount-point prefix. The fixture's trap stops rclone after the unmount; this allows 5 s.
fn stall_leftovers() -> Vec<String> {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let mut left: Vec<String> = rclone_mounts()
            .into_iter()
            .filter(|(_, _, mnt)| mnt.to_string_lossy().starts_with(STALL_PREFIX))
            .map(|(pid, ..)| format!("rclone {pid} in state {:?}", proc_state(pid)))
            .collect();
        let (_, mounts) = sh("findmnt -rn -t fuse.rclone -o TARGET");
        left.extend(
            mounts
                .lines()
                .filter(|l| l.starts_with(STALL_PREFIX))
                .map(|l| format!("mount {l}")),
        );
        if left.is_empty() || Instant::now() >= end {
            return left;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The text of the screen line holding `needle` between box-drawing characters: a panel's
/// footer.
fn snippet(screen: &str, needle: &str) -> Option<String> {
    let line = screen.lines().find(|l| l.contains(needle))?;
    line.split(|c: char| ('\u{2500}'..='\u{257f}').contains(&c))
        .find(|p| p.contains(needle))
        .map(|p| p.trim().to_string())
}

/// The result count of the results footer that shows `state` (`N results (searching)`).
fn results_in(screen: &str, state: &str) -> Option<usize> {
    snippet(screen, state)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Alt+F7 with "Search in" set to `root`, the name `name` and "Stay on this filesystem"
/// set to `stay`, then Enter. Returns when Enter was sent.
fn find_in(t: &mut Tui, root: &Path, name: &str, stay: bool) -> Instant {
    t.keys(&[ALT_F7]);
    assert!(t.wait_for("Find files", T), "{}", t.screen());
    // The focus starts on Name: up to "Search in", then Ctrl+E and Ctrl+U empty it.
    t.keys(&[UP, b"\x05", b"\x15"]);
    t.send(root.to_str().unwrap().as_bytes());
    t.keys(&[TAB]);
    t.send(name.as_bytes());
    // Name -> Containing text -> Hidden entries -> Stay on this filesystem.
    t.keys(&[TAB, TAB, TAB]);
    if !stay {
        t.keys(&[b" "]);
    }
    let want = if stay { "[x] Stay" } else { "[ ] Stay" };
    assert!(t.wait_for(want, T), "{}", t.screen());
    let at = Instant::now();
    t.send(ENTER);
    at
}

/// Sends `key` and returns the ms until `f` holds (the screen is polled every 10 ms), or
/// `None` after 2 s.
fn key_ms(t: &mut Tui, key: &[u8], f: impl FnMut(&mut Tui) -> bool) -> Option<f64> {
    let start = Instant::now();
    t.send(key);
    t.wait_until(Duration::from_secs(2), f)
        .then(|| start.elapsed().as_secs_f64() * 1000.0)
}

/// Key-to-frame latencies in ms (`key_to_flush_us`) that the binary logged after byte
/// `from` of `log`, each with the actions of the frame's keys.
fn frame_latencies(log: &Path, from: u64) -> Vec<(String, f64)> {
    let bytes = std::fs::read(log).unwrap_or_default();
    let text = String::from_utf8_lossy(bytes.get(from as usize..).unwrap_or_default());
    let mut out = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for l in text.lines() {
        if l.contains(" key ") && l.contains("action=") {
            keys.push(l.split("action=").nth(1).unwrap_or("").trim().to_string());
        } else if let Some(v) = l.split("key_to_flush_us=").nth(1) {
            let us: f64 = v
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(f64::NAN);
            out.push((keys.join("+"), us / 1000.0));
            keys.clear();
        }
    }
    out
}

fn log_len(log: &Path) -> u64 {
    std::fs::metadata(log).map(|m| m.len()).unwrap_or(0)
}

/// Keys while a search is blocked, with the results tab active on the left and `fine/`
/// (`..`, `ok`) on the right: Tab and Down move to `ok` in the other panel, Insert marks it
/// ("1 marked"), F1 opens and closes the help, Insert unmarks it, Tab returns. Each key
/// with a visible effect must show within [`KEY_BUDGET_MS`] on the screen, and every key
/// within it in the binary's own key-to-frame log.
fn responsive(t: &mut Tui, log: &Path, case: &str, fail: &mut Vec<String>) {
    let from = log_len(log);
    t.keys(&[TAB, DOWN]);
    let mut shown = Vec::new();
    let mut step =
        |t: &mut Tui, name: &str, key: &[u8], f: &dyn Fn(&str) -> bool| match key_ms(t, key, |t| {
            f(&t.screen())
        }) {
            Some(ms) => {
                shown.push(format!("{name} {ms:.0} ms"));
                if ms > KEY_BUDGET_MS {
                    fail.push(format!("{case}: {name} showed after {ms:.0} ms"));
                }
            }
            None => fail.push(format!("{case}: {name} showed nothing within 2 s")),
        };
    step(t, "Insert (mark)", INSERT, &|s| s.contains("1 marked"));
    step(t, "F1 (help)", F1, &|s| s.contains(HELP_LINE));
    step(t, "F1 (close)", F1, &|s| !s.contains(HELP_LINE));
    assert!(t.wait_for("1 marked", T), "{}", t.screen());
    step(t, "Insert (unmark)", INSERT, &|s| !s.contains("1 marked"));
    t.keys(&[TAB]);
    std::thread::sleep(Duration::from_millis(100));
    let lat = frame_latencies(log, from);
    let max = lat.iter().map(|(_, ms)| *ms).fold(0.0, f64::max);
    if lat.len() < 7 || max.is_nan() || max > KEY_BUDGET_MS {
        fail.push(format!("{case}: logged key-to-frame {lat:?}"));
    }
    let logged: Vec<String> = lat.iter().map(|(k, ms)| format!("{k} {ms:.1}")).collect();
    evidence(
        "A-FD-7",
        &format!(
            "{case}: while the search was blocked, key to screen (pty poll every 10 ms): {}; \
             logged key-to-frame ms: {} (max {max:.1} ms)",
            shown.join(", "),
            logged.join(", ")
        ),
    );
}

#[test]
#[ignore]
fn a_fd_7_search_over_a_stalled_fuse_mount() {
    guard();
    // Re-enter this test inside the fixture, which exports MC_STALL_DIR and MC_STALL_MNT.
    if std::env::var_os("MC_STALL_DIR").is_none() {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/fixtures/stall-fuse.sh");
        let st = Command::new(&script)
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "a_fd_7_search_over_a_stalled_fuse_mount",
                "--ignored",
                "--nocapture",
            ])
            .status()
            .unwrap();
        // The fixture's trap resumes rclone, unmounts and stops rclone, pass or fail.
        let left = stall_leftovers();
        assert!(left.is_empty(), "the fixture left {left:?}");
        evidence(
            "A-FD-7",
            "cleanup: no fuse.rclone mount under the fixture's prefix and no rclone process left",
        );
        assert!(st.success(), "A-FD-7 failed: see the FAIL lines above");
        return;
    }
    let started = Instant::now();
    let base = PathBuf::from(std::env::var_os("MC_STALL_DIR").unwrap());
    let mnt = PathBuf::from(std::env::var_os("MC_STALL_MNT").unwrap());
    let (src, fine) = (base.join("src"), base.join("fine"));
    // The tree: fine/ok, src/inner-ok and stuck/, the mount of src/. A search that reads
    // every directory reads 4; one that leaves out the mount reads 3.
    write(&src.join("inner-ok"), b"x");
    let rclone = rclone_mounts()
        .into_iter()
        .find(|(_, s, m)| *s == src && *m == mnt)
        .map(|(pid, ..)| pid)
        .expect("the fixture's rclone");
    assert_eq!(proc_state(rclone), Some('T'), "the fixture stopped rclone");
    let home = test_dir("manual-afd7");
    let mut fail: Vec<String> = Vec::new();
    // Both panels list directories without the mount point, so no listing waits on it.
    let spawn = |log: &Path| {
        let mut t = Tui::spawn(
            &[
                "--log",
                log.to_str().unwrap(),
                src.to_str().unwrap(),
                fine.to_str().unwrap(),
            ],
            &home.path,
            &[],
            120,
            30,
        );
        assert!(t.wait_for("10Quit", T), "{}", t.screen());
        assert!(t.wait_for("inner-ok", T), "{}", t.screen());
        t
    };

    // Case 1: "Stay on this filesystem" on. The kernel answers a statx of the mount point
    // from its attribute cache only for rclone's --attr-timeout (1 s) after the fixture's
    // last access; later, like on any stalled mount, a statx that asks for the basic fields
    // waits for the stopped rclone. The search starts after that.
    let log1 = home.join("case1.log");
    let mut t = spawn(&log1);
    std::thread::sleep(Duration::from_secs(2).saturating_sub(started.elapsed()));
    let at = find_in(&mut t, &base, "ok", true);
    assert!(t.wait_for("find: ok", T), "{}", t.screen());
    let finished = t.wait_until(Duration::from_secs(5), |t| {
        let s = t.screen();
        s.contains("2 results") && !s.contains("(searching)")
    });
    let took = at.elapsed().as_secs_f64() * 1000.0;
    let screen = t.screen();
    let dirs = find_done(&log1).last().and_then(|l| done_field(l, "dirs"));
    if finished && took <= 1000.0 && dirs == Some(3) && !screen.contains("stuck/") {
        evidence(
            "A-FD-7",
            &format!(
                "case 1 (stay on filesystem): the search completed in {took:.0} ms: {:?}; \
                 3 directories read, the mount not entered",
                snippet(&screen, "2 results").unwrap_or_default()
            ),
        );
    } else {
        let threads = find_threads(t.pid());
        let footer = snippet(&screen, "(searching)")
            .or_else(|| snippet(&screen, " result"))
            .unwrap_or_default();
        let msg = format!(
            "case 1 (stay on filesystem): not completed {took:.0} ms after Enter (dirs read: {dirs:?}): \
             footer {footer:?}; find threads {threads:?}"
        );
        evidence("A-FD-7", &msg);
        fail.push(msg);
    }
    responsive(&mut t, &log1, "case 1", &mut fail);
    if t.screen().contains("(searching)") {
        let n = results_in(&t.screen(), "(searching)");
        match key_ms(&mut t, ESC, |t| t.screen().contains("(cancelled)")) {
            Some(ms) => {
                let kept = results_in(&t.screen(), "(cancelled)");
                evidence(
                    "A-FD-7",
                    &format!(
                        "case 1: Esc showed \"(cancelled)\" after {ms:.0} ms; results {n:?} before, {kept:?} after"
                    ),
                );
                if ms > KEY_BUDGET_MS || kept != n {
                    fail.push(format!(
                        "case 1: Esc after {ms:.0} ms, results {n:?} -> {kept:?}"
                    ));
                }
            }
            None => fail.push("case 1: Esc did not cancel the search".into()),
        }
    }
    let quit = Instant::now();
    t.keys(&[F10]);
    let code = t.wait_exit(T);
    evidence(
        "A-FD-7",
        &format!(
            "case 1: F10 exited with {code:?} after {:.0} ms",
            quit.elapsed().as_secs_f64() * 1000.0
        ),
    );
    if code != Some(0) {
        fail.push(format!("case 1: F10 exit {code:?}"));
    }
    drop(t);

    // Case 2: "Stay on this filesystem" off: a worker waits in the kernel on the mount.
    let log2 = home.join("case2.log");
    let mut t = spawn(&log2);
    let mc = t.pid();
    find_in(&mut t, &base, "ok", false);
    assert!(t.wait_for("find: ok", T), "{}", t.screen());
    let blocked = t.wait_for("(searching)", T) && {
        std::thread::sleep(Duration::from_millis(1500));
        t.pump();
        t.screen().contains("(searching)")
    };
    let n = results_in(&t.screen(), "(searching)");
    let threads = find_threads(mc);
    evidence(
        "A-FD-7",
        &format!(
            "case 2 (all filesystems): 1.5 s after the start still searching: {blocked}; footer {:?}; find threads {threads:?}",
            snippet(&t.screen(), " result").unwrap_or_default()
        ),
    );
    assert!(blocked, "case 2 needs a blocked search: {}", t.screen());
    responsive(&mut t, &log2, "case 2", &mut fail);
    match key_ms(&mut t, ESC, |t| t.screen().contains("(cancelled)")) {
        Some(ms) => {
            let s = t.screen();
            let kept = results_in(&s, "(cancelled)");
            let names = ["fine/ok", "src/inner-ok"]
                .into_iter()
                .filter(|x| s.contains(x))
                .collect::<Vec<_>>();
            evidence(
                "A-FD-7",
                &format!(
                    "case 2: Esc showed {:?} after {ms:.0} ms; results {n:?} before, {kept:?} after, visible {names:?}",
                    snippet(&s, "(cancelled)").unwrap_or_default()
                ),
            );
            if ms > KEY_BUDGET_MS || kept != n || kept.is_none() {
                fail.push(format!(
                    "case 2: Esc after {ms:.0} ms, results {n:?} -> {kept:?}"
                ));
            }
        }
        None => fail.push("case 2: Esc did not cancel the search".into()),
    }
    std::thread::sleep(Duration::from_millis(200));
    let abandoned = find_threads(mc);
    evidence(
        "A-FD-7",
        &format!("case 2: after the cancel the search's threads are {abandoned:?} (abandoned)"),
    );
    if abandoned.is_empty() {
        fail.push("case 2: no search thread stayed blocked after the cancel".into());
    }
    // A second search starts (one abandoned search) and blocks the same way.
    find_in(&mut t, &base, "ok", false);
    let second = t.wait_for("(searching)", T) && {
        std::thread::sleep(Duration::from_millis(1000));
        t.pump();
        t.screen().contains("(searching)")
    };
    let threads2 = find_threads(mc);
    let esc2 = key_ms(&mut t, ESC, |t| t.screen().contains("(cancelled)"));
    evidence(
        "A-FD-7",
        &format!(
            "case 2: a second search started with one abandoned search and was still searching after 1 s: {second}; \
             find threads {threads2:?}; Esc cancelled it after {esc2:.0?} ms"
        ),
    );
    if !second || esc2.is_none_or(|ms| ms > KEY_BUDGET_MS) {
        fail.push(format!("case 2: second search {second}, Esc {esc2:?}"));
    }
    // A third is refused while two cancelled searches are blocked (P2 2.3).
    find_in(&mut t, &base, "ok", false);
    let refused = t.wait_for("previous searches are still blocked", T);
    evidence(
        "A-FD-7",
        &format!(
            "case 2: a third search with two abandoned searches refused with \"previous searches are still blocked\": {refused}"
        ),
    );
    if !refused {
        fail.push(format!("case 2: third search not refused: {}", t.screen()));
    }
    t.keys(&[ESC]);
    assert!(
        t.wait_until(T, |t| !t.screen().contains("Find files")),
        "{}",
        t.screen()
    );
    // rclone resumes: the blocked workers return and their searches end.
    let held = find_threads(mc);
    let done_before = find_done(&log2).len();
    let cont = Instant::now();
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(rclone).unwrap(),
        rustix::process::Signal::CONT,
    )
    .unwrap();
    let returned = t.wait_until(Duration::from_secs(10), |_| {
        find_threads(mc).is_empty() && find_done(&log2).len() >= 2
    });
    let back_ms = cont.elapsed().as_secs_f64() * 1000.0;
    let done = find_done(&log2);
    evidence(
        "A-FD-7",
        &format!(
            "case 2: before SIGCONT to rclone: find threads {held:?}, {done_before} \"find done\" lines; \
             after it every search thread returned and both searches logged their end: {returned} \
             (within {back_ms:.0} ms): {done:?}"
        ),
    );
    if held.is_empty() || done_before != 0 || !returned {
        fail.push(format!(
            "case 2: SIGCONT: threads before {held:?}, after {:?}; find done {done:?}",
            find_threads(mc)
        ));
    }
    // A cancelled worker reads no further entries, so the first search's logged total is
    // what it had found by the cancel; results in the blocked worker's unsent batch were
    // not on screen while it was blocked. Recorded, not asserted: it depends on which
    // worker took the mount point.
    let found = done
        .iter()
        .find(|l| done_field(l, "id") == Some(1))
        .and_then(|l| done_field(l, "results"));
    evidence(
        "A-FD-7",
        &format!(
            "case 2: the first search had found {found:?} results by the cancel; {n:?} were on screen while it was blocked"
        ),
    );
    // A new search is allowed again and now enters the mount: 4 directories.
    let at = find_in(&mut t, &base, "ok", false);
    let ended = t.wait_until(T, |_| find_done(&log2).len() > done.len());
    let took = at.elapsed().as_secs_f64() * 1000.0;
    std::thread::sleep(Duration::from_millis(100));
    t.pump();
    let last = find_done(&log2).last().cloned().unwrap_or_default();
    let s = t.screen();
    let complete = ended
        && !s.contains("(searching)")
        && !s.contains("(cancelled)")
        && done_field(&last, "dirs") == Some(4);
    evidence(
        "A-FD-7",
        &format!(
            "case 2: with rclone running a fourth search completed: {complete} ({took:.0} ms), footer {:?}, logged {last:?}",
            snippet(&s, " result").unwrap_or_default()
        ),
    );
    if !complete {
        fail.push(format!("case 2: search after resume: {last:?}\n{s}"));
    }
    t.keys(&[F10]);
    let code = t.wait_exit(T);
    if code != Some(0) {
        fail.push(format!("case 2: F10 exit {code:?}"));
    }
    let logs = format!(
        "{}{}",
        std::fs::read_to_string(&log1).unwrap_or_default(),
        std::fs::read_to_string(&log2).unwrap_or_default()
    );
    let crashed = logs.contains("panic") || logs.contains("internal error");
    evidence(
        "A-FD-7",
        &format!(
            "case 2: F10 exited with {code:?}; a panic or internal error in the logs: {crashed}"
        ),
    );
    if crashed {
        fail.push("a panic or internal error in the logs".into());
    }
    for f in &fail {
        println!("FAIL A-FD-7: {f}");
    }
    assert!(fail.is_empty(), "{} failed checks", fail.len());
}
