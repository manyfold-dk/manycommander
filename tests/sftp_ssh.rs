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

use common::sftp::{Tracker, proc_stat};
use common::ssh::*;
use common::test_dir;
use common::tui::{DOWN, ENTER, ESC, F3};

const TAB: &[u8] = b"\t";
use manycommander::remote::transport::FIXED;
use std::os::unix::fs::PermissionsExt;

#[test]
fn a_sf_5_key_authentication_the_argv_and_the_foreground() {
    if !have_tools() {
        return;
    }
    let e = Env::new("ssh-key", "yes", true);
    let u = user();
    let home = e.home.display().to_string();
    let mut t = e.tui(&[], &[]);
    let mut tr = Tracker::default();
    run_line(&mut t, &format!("cd sftp://{u}@mc-test:2222{home}"));
    assert!(raw_has(
        &mut t,
        &format!("connecting to sftp://{u}@mc-test:2222 ...")
    ));
    assert!(
        t.wait_for(&format!("connected to sftp://{u}@mc-test:2222"), T),
        "{}\n{}",
        t.screen(),
        String::from_utf8_lossy(&t.raw)
    );
    // The remote panel lists the directory on the server.
    assert!(t.wait_for(" docs ", T), "{}", t.screen());
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
    run_line(&mut t, &format!("cd sftp://{u}@mc-test:2222{home}/docs"));
    assert!(t.wait_for(" doc ", T), "{}", t.screen());
    assert!(t.wait_until(T, |t| !t.screen().contains("(loading)")));
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
    let home = e.home.display().to_string();
    let mut t = e.tui(&[], &[]);
    let mut tr = Tracker::default();
    run_line(&mut t, &format!("cd sftp://mc-test{home}"));
    // ssh prompts in the hand-off, on the terminal; the test answers as the user would.
    assert!(
        raw_has(&mut t, "Are you sure you want to continue connecting"),
        "{}",
        String::from_utf8_lossy(&t.raw)
    );
    tr.scan(t.pid());
    t.send(b"yes\r");
    assert!(
        t.wait_for("connected to sftp://mc-test", T),
        "{}",
        t.screen()
    );
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
    run_line(&mut t, &format!("cd sftp://mc-test{home}"));
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
    let home = e.home.display().to_string();
    let mut t = e.tui(
        &[docs.to_str().unwrap(), docs.to_str().unwrap()],
        &[("PAGER", pager.to_str().unwrap())],
    );
    let mut tr = Tracker::default();
    run_line(&mut t, &format!("cd sftp://mc-test{home}"));
    assert!(
        t.wait_for("connected to sftp://mc-test", T),
        "{}",
        t.screen()
    );
    tr.scan(t.pid());
    let ssh = ssh_child(&t).expect("the ssh child");
    // F3 on the local file in the other panel: the pager runs in manycommander's group,
    // and Ctrl+C there goes to that group only.
    assert!(t.wait_for(" doc ", T), "{}", t.screen());
    t.keys(&[TAB, DOWN, F3]);
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
    t.keys(&[TAB]);
    run_line(&mut t, "cd docs");
    assert!(t.wait_for("home/docs", T), "{}", t.screen());
    assert!(t.wait_until(T, |t| !t.screen().contains("(loading)")));
    assert!(!t.screen().contains("connection lost"), "{}", t.screen());
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
