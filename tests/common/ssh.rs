//! The `sshd -i` environment of the end-to-end SFTP tests (P3 8.4): a generated
//! `ssh_config` whose `ProxyCommand` runs `sshd -i` with a scratch host key, a scratch client
//! key in `AuthorizedKeysFile` and `UserKnownHostsFile` in the test directory. No root and
//! no listening port; ssh reads only that file (`-F`), so the user's `~/.ssh` is never read
//! or written, and no host is ever contacted. `sshd` logs to stderr (`-e`), and every
//! process runs with `RLIMIT_CORE` 0 (inherited, and set again in the `ProxyCommand`).
#![allow(dead_code)]

use super::sftp::{Tracker, no_core_dumps, proc_stat};
use super::tui::{ENTER, ESC, F10, Tui};
use super::{TestDir, skip, test_dir};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const T: Duration = Duration::from_secs(20);

pub fn ready(t: &mut Tui) {
    assert!(
        t.wait_for("10Quit", T),
        "no function-key bar:\n{}",
        t.screen()
    );
}

pub fn have_tools() -> bool {
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

pub fn user() -> String {
    let out = Command::new("id").arg("-un").output().unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

pub fn keygen(path: &Path) {
    let st = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(path)
        .env_remove("SSH_AUTH_SOCK")
        .status()
        .unwrap();
    assert!(st.success());
}

pub fn public_key(path: &Path) -> String {
    let pubkey = std::fs::read_to_string(path.with_extension("pub")).unwrap();
    pubkey
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

/// A scratch sshd behind a `ProxyCommand`, a client configuration for it, the argv
/// recorder, and a home directory for manycommander.
pub struct Env {
    pub dir: TestDir,
    pub ssh: PathBuf,
    pub home: PathBuf,
    pub config: PathBuf,
    pub known_hosts: PathBuf,
    pub argv_log: PathBuf,
    pub wrapper: PathBuf,
}

impl Env {
    /// `strict`: `StrictHostKeyChecking` for the host aliases; `known`: whether
    /// `known_hosts` starts with the right key.
    pub fn new(name: &str, strict: &str, known: bool) -> Env {
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
    pub fn set_ssh_setting(&self, extra: &[&str]) {
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
    pub fn spawns(&self) -> Vec<Vec<String>> {
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

    pub fn tui(&self, args: &[&str], env: &[(&str, &str)]) -> Tui {
        let mut t = Tui::spawn(args, &self.home, env, 160, 30);
        ready(&mut t);
        t
    }
}

pub fn raw_has(t: &mut Tui, what: &str) -> bool {
    t.wait_until(T, |t| String::from_utf8_lossy(&t.raw).contains(what))
}

/// Types a command line (`Ctrl+E` first) and runs it.
pub fn run_line(t: &mut Tui, text: &str) {
    t.send(b"\x05");
    t.send(text.as_bytes());
    std::thread::sleep(Duration::from_millis(100));
    t.keys(&[ENTER]);
}

/// The terminal's foreground group is manycommander's own.
pub fn foreground_is_ours(t: &Tui) -> bool {
    let p = proc_stat(t.pid()).unwrap();
    p.tpgid == p.pgrp
}

/// The ssh child of manycommander.
pub fn ssh_child(t: &Tui) -> Option<super::sftp::Proc> {
    super::sftp::descendants(t.pid())
        .into_iter()
        .find(|p| p.ppid == t.pid() && p.comm == "ssh")
}

/// Keys reach the TUI: text typed after `Ctrl+E` shows on the command line, and `Esc`
/// clears it.
pub fn keys_reach_the_tui(t: &mut Tui) {
    t.send(b"\x05echo typed-after");
    assert!(t.wait_for("echo typed-after", T), "{}", t.screen());
    t.keys(&[ESC]);
    assert!(
        t.wait_until(T, |t| !t.screen().contains("echo typed-after")),
        "{}",
        t.screen()
    );
}

pub fn quit(t: &mut Tui, tr: &mut Tracker) {
    tr.scan(t.pid());
    t.keys(&[F10]);
    assert_eq!(t.wait_exit(T), Some(0), "{}", t.screen());
    let left = tr.wait_gone(T);
    assert!(left.is_empty(), "processes left behind: {left:?}");
}
