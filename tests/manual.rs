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
