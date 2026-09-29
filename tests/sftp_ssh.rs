//! SFTP through the real `ssh` (P3 5.2): A-SF-5 and A-SF-6, end to end in a pty.
//!
//! No root and no listening port: a generated `ssh_config` runs `sshd -i` as the
//! `ProxyCommand`, with a scratch host key, a scratch client key in `AuthorizedKeysFile`,
//! `UserKnownHostsFile` in the test directory, `IdentitiesOnly` and no agent. ssh reads only
//! that file (`-F`), so the user's `~/.ssh` is never read or written, and no host is ever
//! contacted. `sshd` logs to stderr (`-e`), never to a file, and every process the tests
//! spawn runs with `RLIMIT_CORE` 0 (inherited from the test process, and set again in the
//! `ProxyCommand`), so an expected kill never leaves a core dump. Each test kills and
//! checks every process it started.

mod common;

use common::sftp::{Tracker, no_core_dumps, proc_stat};
use common::tui::{DOWN, ENTER, ESC, F3, F10, Tui};
use common::{TestDir, skip, test_dir};
use manycommander::remote::transport::FIXED;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const T: Duration = Duration::from_secs(20);

fn ready(t: &mut Tui) {
    assert!(
        t.wait_for("10Quit", T),
        "no function-key bar:\n{}",
        t.screen()
    );
}

fn have_tools() -> bool {
    for p in [
        "/usr/bin/sshd",
        "/usr/bin/ssh",
        "/usr/bin/ssh-keygen",
        "/usr/bin/setsid",
    ] {
        if !Path::new(p).exists() {
            skip(&format!("{p} is not installed"));
            return false;
        }
    }
    true
}

fn user() -> String {
    let out = Command::new("id").arg("-un").output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn keygen(path: &Path) {
    let st = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(path)
        .env_remove("SSH_AUTH_SOCK")
        .status()
        .unwrap();
    assert!(st.success());
}

fn public_key(path: &Path) -> String {
    let pubkey = std::fs::read_to_string(path.with_extension("pub")).unwrap();
    pubkey
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

/// A scratch sshd behind a `ProxyCommand`, a client configuration for it, the argv
/// recorder, and a home directory for manycommander.
struct Env {
    dir: TestDir,
    ssh: PathBuf,
    home: PathBuf,
    config: PathBuf,
    known_hosts: PathBuf,
    argv_log: PathBuf,
    wrapper: PathBuf,
}

impl Env {
    /// `strict`: `StrictHostKeyChecking` for the host aliases; `known`: whether
    /// `known_hosts` starts with the right key.
    fn new(name: &str, strict: &str, known: bool) -> Env {
        no_core_dumps();
        let dir = test_dir(name);
        let ssh = dir.join("ssh");
        let home = dir.join("home");
        std::fs::create_dir_all(&ssh).unwrap();
        std::fs::create_dir_all(home.join("docs")).unwrap();
        std::fs::write(home.join("docs/doc.txt"), b"a local file\n").unwrap();
        let s = |p: &str| ssh.join(p);
        keygen(&s("hostkey"));
        keygen(&s("id"));
        std::fs::copy(s("id.pub"), s("authorized_keys")).unwrap();
        std::fs::write(
            s("sshd_config"),
            format!(
                "HostKey {hk}\nAuthorizedKeysFile {ak}\nPidFile none\nUsePAM no\nStrictModes no\n\
                 PasswordAuthentication yes\nKbdInteractiveAuthentication no\nLogLevel ERROR\n\
                 Subsystem sftp /usr/lib/ssh/sftp-server\n",
                hk = s("hostkey").display(),
                ak = s("authorized_keys").display(),
            ),
        )
        .unwrap();
        let known_hosts = s("known_hosts");
        let line = if known {
            format!("mc-test,mc-pw {}\n", public_key(&s("hostkey")))
        } else {
            String::new()
        };
        std::fs::write(&known_hosts, line).unwrap();
        // `ulimit -c 0` again inside the ProxyCommand: sshd never dumps core, whatever the
        // test process's limit. ssh takes the first value it finds, so `mc-pw` comes first.
        // Its sshd runs in a session of its own: in ssh's process group a Ctrl+Z at the
        // prompt would stop the proxy too, and the SIGCHLD of that stop can reach ssh before
        // its SIGTSTP; ssh's SIGCHLD handler restarts the prompt's read, so ssh itself would
        // not stop (an OpenSSH race, independent of manycommander).
        let config = s("ssh_config");
        std::fs::write(
            &config,
            format!(
                "Host mc-pw\n\
                 \x20 PreferredAuthentications password\n\
                 \x20 PubkeyAuthentication no\n\
                 \x20 NumberOfPasswordPrompts 1\n\
                 \x20 ProxyCommand /bin/sh -c 'ulimit -c 0; exec /usr/bin/setsid /usr/bin/sshd -i -e -f {cfg}'\n\
                 Host mc-test mc-pw\n\
                 \x20 ProxyCommand /bin/sh -c 'ulimit -c 0; exec /usr/bin/sshd -i -e -f {cfg}'\n\
                 \x20 IdentityFile {id}\n\
                 \x20 IdentitiesOnly yes\n\
                 \x20 IdentityAgent none\n\
                 \x20 UserKnownHostsFile {kh}\n\
                 \x20 GlobalKnownHostsFile /dev/null\n\
                 \x20 StrictHostKeyChecking {strict}\n\
                 \x20 CheckHostIP no\n\
                 \x20 UpdateHostKeys no\n\
                 \x20 ControlMaster no\n\
                 \x20 ControlPath none\n\
                 \x20 LogLevel ERROR\n",
                cfg = s("sshd_config").display(),
                id = s("id").display(),
                kh = known_hosts.display(),
            ),
        )
        .unwrap();
        // The wrapper records the argv it was given, one argument per line, then runs ssh.
        let argv_log = s("argv.log");
        let wrapper = s("ssh-wrap");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done >> '{log}'\n\
                 printf '%s\\n' '--end--' >> '{log}'\nexec /usr/bin/ssh \"$@\"\n",
                log = argv_log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = Env {
            dir,
            ssh,
            home,
            config,
            known_hosts,
            argv_log,
            wrapper,
        };
        env.set_ssh_setting(&[]);
        env
    }

    /// `sftp.ssh` = the wrapper, `-F <config>`, then `extra`.
    fn set_ssh_setting(&self, extra: &[&str]) {
        let mut words = vec![
            self.wrapper.display().to_string(),
            "-F".into(),
            self.config.display().to_string(),
        ];
        words.extend(extra.iter().map(|s| s.to_string()));
        let list: Vec<String> = words.iter().map(|w| format!("{w:?}")).collect();
        let cfg = self.home.join(".config/manycommander");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(
            cfg.join("config.toml"),
            format!("[sftp]\nssh = [{}]\n", list.join(", ")),
        )
        .unwrap();
    }

    /// The argv of each ssh the wrapper started.
    fn spawns(&self) -> Vec<Vec<String>> {
        let Ok(text) = std::fs::read_to_string(&self.argv_log) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut cur = Vec::new();
        for l in text.lines() {
            if l == "--end--" {
                out.push(std::mem::take(&mut cur));
            } else {
                cur.push(l.to_owned());
            }
        }
        out
    }

    fn tui(&self, args: &[&str], env: &[(&str, &str)]) -> Tui {
        let mut t = Tui::spawn(args, &self.home, env, 160, 30);
        ready(&mut t);
        t
    }
}

fn raw_has(t: &mut Tui, what: &str) -> bool {
    t.wait_until(T, |t| String::from_utf8_lossy(&t.raw).contains(what))
}

/// Types a command line and runs it.
fn run_line(t: &mut Tui, text: &str) {
    t.send(text.as_bytes());
    std::thread::sleep(Duration::from_millis(100));
    t.keys(&[ENTER]);
}

/// The terminal's foreground group is manycommander's own.
fn foreground_is_ours(t: &Tui) -> bool {
    let p = proc_stat(t.pid()).unwrap();
    p.tpgid == p.pgrp
}

/// The ssh child of manycommander.
fn ssh_child(t: &Tui) -> Option<common::sftp::Proc> {
    common::sftp::descendants(t.pid())
        .into_iter()
        .find(|p| p.ppid == t.pid() && p.comm == "ssh")
}

/// Keys reach the TUI: typed text shows on the command line, and `Esc` clears it.
fn keys_reach_the_tui(t: &mut Tui) {
    t.send(b"echo typed-after");
    assert!(t.wait_for("echo typed-after", T), "{}", t.screen());
    t.keys(&[ESC]);
    assert!(
        t.wait_until(T, |t| !t.screen().contains("echo typed-after")),
        "{}",
        t.screen()
    );
}

fn quit(t: &mut Tui, tr: &mut Tracker) {
    tr.scan(t.pid());
    t.keys(&[F10]);
    assert_eq!(t.wait_exit(T), Some(0), "{}", t.screen());
    let left = tr.wait_gone(T);
    assert!(left.is_empty(), "processes left behind: {left:?}");
}

#[test]
fn a_sf_5_key_authentication_the_argv_and_the_foreground() {
    if !have_tools() {
        return;
    }
    let e = Env::new("ssh-key", "yes", true);
    let u = user();
    let mut t = e.tui(&[], &[]);
    let mut tr = Tracker::default();
    run_line(&mut t, &format!("cd sftp://{u}@mc-test:2222"));
    assert!(raw_has(
        &mut t,
        &format!("connecting to sftp://{u}@mc-test:2222 ...")
    ));
    assert!(
        t.wait_for("connected, home", T),
        "{}\n{}",
        t.screen(),
        String::from_utf8_lossy(&t.raw)
    );
    tr.scan(t.pid());
    // The argv: the program, the fixed options directly after it, the other sftp.ssh
    // arguments, the user and port, then `-s -- host sftp`.
    let spawns = e.spawns();
    assert_eq!(spawns.len(), 1, "{spawns:?}");
    let mut want: Vec<String> = FIXED.iter().map(|s| s.to_string()).collect();
    want.extend([
        "-F".into(),
        e.config.display().to_string(),
        "-l".into(),
        u.clone(),
        "-p".into(),
        "2222".into(),
        "-s".into(),
        "--".into(),
        "mc-test".into(),
        "sftp".into(),
    ]);
    assert_eq!(spawns[0], want);
    // ssh runs in its own process group, and the terminal is manycommander's again.
    let ssh = ssh_child(&t).expect("the ssh child");
    assert_eq!(ssh.pgrp, ssh.pid, "ssh leads its own group");
    assert_ne!(ssh.pgrp, proc_stat(t.pid()).unwrap().pgrp);
    assert!(foreground_is_ours(&t));
    keys_reach_the_tui(&mut t);
    // A second connect to the same target reuses the session.
    run_line(&mut t, &format!("cd sftp://{u}@mc-test:2222/tmp"));
    assert!(t.wait_for("session open, home", T), "{}", t.screen());
    assert_eq!(e.spawns().len(), 1);
    // Addresses that could become options or shell words spawn nothing.
    for bad in [
        "cd sftp://-oProxyCommand=touch%20x",
        "cd sftp://h%41",
        "cd sftp://h;id",
        "cd sftp://h?x",
    ] {
        run_line(&mut t, bad);
        assert!(
            t.wait_for("not a supported sftp:// address", T),
            "{bad}: {}",
            t.screen()
        );
        t.keys(&[ESC]);
    }
    assert_eq!(e.spawns().len(), 1);
    assert!(foreground_is_ours(&t));
    quit(&mut t, &mut tr);
}

#[test]
fn a_sf_5_an_ssh_setting_that_would_override_the_fixed_options_spawns_nothing() {
    if !have_tools() {
        return;
    }
    let e = Env::new("ssh-reject", "yes", true);
    for bad in ["-oX=y", "-A", "-vt"] {
        e.set_ssh_setting(&[bad]);
        let mut t = e.tui(&[], &[]);
        let mut tr = Tracker::default();
        run_line(&mut t, "cd sftp://mc-test");
        assert!(
            t.wait_for(&format!("sftp.ssh: {bad} is not allowed"), T),
            "{}",
            t.screen()
        );
        quit(&mut t, &mut tr);
    }
    assert!(e.spawns().is_empty(), "{:?}", e.spawns());
}

#[test]
fn a_sf_5_an_unknown_host_key_prompts_and_a_changed_one_is_refused() {
    if !have_tools() {
        return;
    }
    let e = Env::new("ssh-hostkey", "ask", false);
    let mut t = e.tui(&[], &[]);
    let mut tr = Tracker::default();
    run_line(&mut t, "cd sftp://mc-test");
    // ssh prompts in the hand-off, on the terminal; the test answers as the user would.
    assert!(
        raw_has(&mut t, "Are you sure you want to continue connecting"),
        "{}",
        String::from_utf8_lossy(&t.raw)
    );
    tr.scan(t.pid());
    t.send(b"yes\r");
    assert!(t.wait_for("connected, home", T), "{}", t.screen());
    assert!(
        std::fs::read_to_string(&e.known_hosts)
            .unwrap()
            .contains("ssh-ed25519")
    );
    assert!(foreground_is_ours(&t));
    quit(&mut t, &mut tr);

    // Another key for the same name: ssh refuses, and manycommander shows ssh's message.
    keygen(&e.ssh.join("otherkey"));
    std::fs::write(
        &e.known_hosts,
        format!("mc-test {}\n", public_key(&e.ssh.join("otherkey"))),
    )
    .unwrap();
    let mut t = e.tui(&[], &[]);
    let mut tr = Tracker::default();
    run_line(&mut t, "cd sftp://mc-test");
    assert!(raw_has(&mut t, "[connection failed] press Enter to return"));
    tr.scan(t.pid());
    let raw = String::from_utf8_lossy(&t.raw).into_owned();
    assert!(
        raw.contains("REMOTE HOST IDENTIFICATION HAS CHANGED"),
        "{raw}"
    );
    assert!(raw.contains("Host key verification failed."), "{raw}");
    t.keys(&[ENTER]);
    assert!(
        t.wait_for("connection failed: Host key verification failed.", T),
        "{}",
        t.screen()
    );
    assert!(foreground_is_ours(&t));
    assert!(ssh_child(&t).is_none());
    keys_reach_the_tui(&mut t);
    quit(&mut t, &mut tr);
}

#[test]
fn a_sf_6_ctrl_c_in_a_local_pager_leaves_the_session_working() {
    if !have_tools() {
        return;
    }
    let e = Env::new("ssh-pager", "yes", true);
    let pager = e.dir.join("pager.sh");
    std::fs::write(&pager, "#!/bin/sh\nexec sleep 60\n").unwrap();
    std::fs::set_permissions(&pager, std::fs::Permissions::from_mode(0o755)).unwrap();
    let docs = e.home.join("docs");
    let mut t = e.tui(
        &[docs.to_str().unwrap()],
        &[("PAGER", pager.to_str().unwrap())],
    );
    let mut tr = Tracker::default();
    run_line(&mut t, "cd sftp://mc-test");
    assert!(t.wait_for("connected, home", T), "{}", t.screen());
    tr.scan(t.pid());
    let ssh = ssh_child(&t).expect("the ssh child");
    // F3 on the local file: the pager runs in manycommander's group, and Ctrl+C there goes
    // to that group only.
    assert!(t.wait_for(" doc ", T), "{}", t.screen());
    t.keys(&[DOWN, F3]);
    assert!(
        t.wait_until(T, |t| common::sftp::descendants(t.pid())
            .iter()
            .any(|p| p.comm == "sleep")),
        "the pager did not start"
    );
    t.send(b"\x03");
    assert!(t.wait_for("10Quit", T), "{}", t.screen());
    assert!(
        t.wait_until(T, |t| !common::sftp::descendants(t.pid())
            .iter()
            .any(|p| p.comm == "sleep")),
        "the pager survived Ctrl+C"
    );
    // ssh is untouched, and the session still answers.
    assert_eq!(
        common::sftp::still_running(std::slice::from_ref(&ssh)).len(),
        1
    );
    run_line(&mut t, "cd sftp://mc-test");
    assert!(t.wait_for("session open, home", T), "{}", t.screen());
    assert_eq!(e.spawns().len(), 1);
    assert!(foreground_is_ours(&t));
    quit(&mut t, &mut tr);
}

/// Connects to the password host and interrupts the prompt with `key`.
fn interrupt_the_password_prompt(name: &str, key: &[u8]) {
    if !have_tools() {
        return;
    }
    let e = Env::new(name, "yes", true);
    let mut t = e.tui(&[], &[]);
    let mut tr = Tracker::default();
    run_line(&mut t, "cd sftp://mc-pw");
    assert!(
        raw_has(&mut t, "password:"),
        "{}",
        String::from_utf8_lossy(&t.raw)
    );
    tr.scan(t.pid());
    let ssh = ssh_child(&t).expect("the ssh child");
    // ssh owns the terminal during the prompt.
    let p = proc_stat(t.pid()).unwrap();
    assert_eq!(p.tpgid, ssh.pgrp);
    // readpassphrase installs its handlers, writes the prompt, then reads: a key typed
    // before the read starts only sets its flag. A person never types that fast; the test
    // waits until ssh blocks in the terminal read.
    assert!(
        t.wait_until(T, |_| std::fs::read_to_string(format!(
            "/proc/{}/wchan",
            ssh.pid
        ))
        .is_ok_and(|w| w == "wait_woken" || w == "n_tty_read")),
        "ssh never waited for the password"
    );
    t.send(key);
    assert!(
        raw_has(&mut t, "[connection failed] press Enter to return"),
        "{}",
        String::from_utf8_lossy(&t.raw)
    );
    // ssh's group was ended and reaped (its sshd, in a session of its own, ends on EOF);
    // the terminal came back.
    assert!(common::sftp::still_running(std::slice::from_ref(&ssh)).is_empty());
    let left = tr.wait_gone(T);
    assert!(left.is_empty(), "{left:?}");
    assert!(foreground_is_ours(&t));
    t.keys(&[ENTER]);
    assert!(
        t.wait_for("sftp://mc-pw: connection failed", T),
        "{}",
        t.screen()
    );
    keys_reach_the_tui(&mut t);
    quit(&mut t, &mut tr);
}

#[test]
fn a_sf_6_ctrl_c_at_a_password_prompt() {
    interrupt_the_password_prompt("ssh-ctrl-c", b"\x03");
}

#[test]
fn a_sf_6_ctrl_z_at_a_password_prompt_is_seen_through_waitid() {
    interrupt_the_password_prompt("ssh-ctrl-z", b"\x1a");
}

/// The transport without a terminal: real ssh through the same argv, a pipelined download.
#[test]
fn real_ssh_without_a_terminal_downloads_byte_exact() {
    if !have_tools() {
        return;
    }
    let e = Env::new("ssh-plain", "yes", true);
    let data = common::noise((2 << 20) + 5, 9);
    let file = e.dir.join("payload");
    std::fs::write(&file, &data).unwrap();
    let words = vec![
        "/usr/bin/ssh".to_owned(),
        "-F".into(),
        e.config.display().to_string(),
    ];
    let cmd = manycommander::remote::transport::SshCommand::from_setting(Some(&words)).unwrap();
    let target = manycommander::provider::Target {
        user: None,
        host: "mc-test".into(),
        port: None,
    };
    let mut c = cmd.command(&target).unwrap();
    c.env_remove("SSH_AUTH_SOCK").env("SHELL", "/bin/sh");
    let (on_lost, lost) = common::sftp::lost_channel();
    let s = manycommander::remote::transport::start_plain(c, Some(on_lost)).unwrap();
    let never = std::sync::atomic::AtomicBool::new(false);
    let h = s
        .open(
            file.to_str().unwrap().as_bytes(),
            manycommander::remote::proto::open::READ,
            Default::default(),
            &never,
        )
        .unwrap();
    let mut r = s.reader(
        h,
        Some(data.len() as u64),
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    let mut got = Vec::new();
    std::io::Read::read_to_end(&mut r, &mut got).unwrap();
    drop(r);
    assert!(got == data);
    let pid = s.pid().unwrap();
    let mut tr = Tracker::default();
    tr.scan(pid as i32);
    s.close_wait();
    assert!(common::sftp::gone_within(pid, T));
    assert!(tr.wait_gone(T).is_empty());
    assert!(lost.try_recv().is_err());
}

/// The app checks the address and `sftp.ssh` before it asks the runtime for anything: a
/// refused address or setting produces no effect at all, so nothing can be spawned.
#[test]
fn a_sf_5_the_app_checks_the_address_and_the_setting_before_any_effect() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use manycommander::app::App;
    use manycommander::app::event::{Effect, Event};
    use manycommander::remote::url::{REFUSED, RemoteDir};
    let d = test_dir("ssh-app");
    let mut a = App::new(
        d.path.clone(),
        d.path.clone(),
        d.path.clone(),
        manycommander::config::Config::default(),
        None,
        manycommander::theme::Depth::NoColor,
        jiff::tz::TimeZone::UTC,
    );
    let run = |a: &mut App, line: &str| {
        a.line.set(line.as_bytes());
        a.update(Event::Key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            std::time::Instant::now(),
        ))
    };
    let status = |a: &App| {
        a.status
            .as_ref()
            .map(|s| s.text.clone())
            .unwrap_or_default()
    };
    let fx = run(&mut a, "cd sftp://u@h:2222/x/%41");
    let [Effect::Connect(addr, cmd)] = fx.as_slice() else {
        panic!("{fx:?}")
    };
    assert_eq!(addr.target.address(), "sftp://u@h:2222");
    assert!(matches!(&addr.dir, RemoteDir::Absolute(p) if p.to_bytes() == b"/x/A"));
    let argv = cmd.argv(&addr.target).unwrap();
    assert_eq!(argv[0], "ssh");
    assert_eq!(&argv[1..=FIXED.len()], FIXED);
    for bad in [
        "cd sftp://-oProxyCommand=x",
        "cd sftp://h%25x",
        "cd sftp://h`id`",
        "cd sftp://u@h/p?q",
        "cd sftp://h/~/..",
    ] {
        let fx = run(&mut a, bad);
        assert!(fx.is_empty(), "{bad}: {fx:?}");
        assert!(status(&a).starts_with(REFUSED), "{bad}: {}", status(&a));
    }
    for (setting, named) in [
        (
            vec!["ssh", "-oStrictHostKeyChecking=no"],
            "-oStrictHostKeyChecking=no",
        ),
        (vec!["ssh", "-A"], "-A"),
        (vec!["ssh", "-e", "~"], "-e"),
        (vec![], "empty"),
    ] {
        a.config.sftp.ssh = Some(setting.iter().map(|s| s.to_string()).collect());
        let fx = run(&mut a, "cd sftp://h");
        assert!(fx.is_empty(), "{setting:?}: {fx:?}");
        assert!(status(&a).contains(named), "{setting:?}: {}", status(&a));
    }
}
