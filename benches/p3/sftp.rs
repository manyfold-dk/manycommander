//! P-26, P-27, P-5b and the small-file trees: SFTP against `sshd -i` through a
//! `ProxyCommand` (no listening port, no host) and against `sftp-server` on pipes, with and
//! without the latency helper, beside `sftp` over the same transport.
//!
//! The `sshd -i` environment ([`env`]) lives in `p3/ssh`: a scratch host key and client key,
//! `AuthorizedKeysFile` and `UserKnownHostsFile` there, and an `ssh_config` that `ssh -F`
//! reads instead of the user's; `BatchMode yes`, so nothing ever prompts. Every `sshd`,
//! `ssh`, `sftp` and `sftp-server` runs with `RLIMIT_CORE` 0 (inherited, and set again
//! with `ulimit -c 0` in each `ProxyCommand` and server command) and with stderr on a pipe
//! or `/dev/null`, never a file: sshd's pre-authentication child has a file-size limit of
//! 0 and dies writing to one.
//!
//! Subcommands:
//!   p3-sftp-env                              writes the environment, prints the config path
//!   p3-sftp-get VIA RUNS                     P-26 download of the 1 GiB file
//!   p3-sftp-put VIA RUNS                     P-26 upload of the 1 GiB file
//!   p3-sftp-list VIA RUNS                    P-27: the 10,000-entry listing
//!   p3-sftp-tree VIA NAME RUNS               the small-file tree NAME, down and up
//!   p3-sftp-sweep VIA MIB WINDOW:CHUNK...    upload and download of MIB MiB per setting
//!
//! VIA: `pipes` (`sftp-server` on pipes; `sftp -D`), `ssh` (`ssh -F` to `sshd -i`),
//! `pipes-rtt30` (`sftp-server` behind the latency helper, 15 ms each way), `ssh-rtt30`.

use super::pty::{ALT_Q, CTRL_Q, DOWN, END, ENTER, HOME, Opts, Pty, Term, log_file};
use super::{log_lines, median, ms, no_core_dumps, p3};
use manycommander::fsops::group::{Group, Root};
use manycommander::fsops::job::{Dest, JobSpec, Report, run_guarded};
use manycommander::fsops::sys::Sys;
use manycommander::panel::listing::ListingMsg;
use manycommander::provider::{Target, VPath};
use manycommander::remote::provider::{self as rp, ListRequest, RemoteProvider};
use manycommander::remote::session::Sizes;
use manycommander::remote::transport::{self, SshCommand};
use manycommander::remote::url::RemoteDir;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const SFTP_SERVER: &str = "/usr/lib/ssh/sftp-server";
const HOST: &str = "mc-bench";
const HOST_RTT: &str = "mc-bench-rtt30";

pub fn main(sub: &str, a: &[&str]) {
    no_core_dumps();
    match (sub, a) {
        ("p3-sftp-env", []) => println!("{}", env().display()),
        ("p3-sftp-get", [via, runs]) => transfer(via, runs.parse().unwrap(), false),
        ("p3-sftp-put", [via, runs]) => transfer(via, runs.parse().unwrap(), true),
        ("p3-sftp-list", [via, runs]) => list(via, runs.parse().unwrap()),
        ("p3-sftp-tree", [via, name, runs]) => tree(via, name, runs.parse().unwrap()),
        ("p3-sftp-sweep", [via, mib, settings @ ..]) => sweep(via, mib.parse().unwrap(), settings),
        _ => panic!("unknown sftp subcommand {sub} {a:?}"),
    }
}

/// The fixture root: `remote/` is what the server serves, `local/` and `up/` receive.
fn root() -> PathBuf {
    p3().join("sftp")
}

fn ssh_dir() -> PathBuf {
    p3().join("ssh")
}

fn config() -> PathBuf {
    ssh_dir().join("ssh_config")
}

/// `config.toml` text for a pty session that connects through the bench's `ssh_config`.
pub fn ssh_setting(cfg: &str) -> String {
    format!("[sftp]\nssh = [\"ssh\", \"-F\", {cfg:?}]\n")
}

/// Types a command line and runs it.
pub fn run_line(p: &mut Pty, text: &str) {
    p.send(text.as_bytes());
    p.idle(Duration::from_millis(100));
    p.keys(&[ENTER], Duration::from_millis(50));
}

fn keygen(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("pub"));
    let st = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(path)
        .env_remove("SSH_AUTH_SOCK")
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success());
}

fn public_key(path: &Path) -> String {
    let k = std::fs::read_to_string(path.with_extension("pub")).unwrap();
    k.split_whitespace().take(2).collect::<Vec<_>>().join(" ")
}

/// Writes the `sshd -i` environment (again on every run: the `ProxyCommand` of the latency
/// host names this driver's path). Returns the `ssh_config` path.
pub fn env() -> PathBuf {
    let s = ssh_dir();
    std::fs::create_dir_all(&s).unwrap();
    let p = |n: &str| s.join(n);
    keygen(&p("hostkey"));
    keygen(&p("id"));
    std::fs::copy(p("id.pub"), p("authorized_keys")).unwrap();
    std::fs::write(
        p("sshd_config"),
        format!(
            "HostKey {hk}\nAuthorizedKeysFile {ak}\nPidFile none\nUsePAM no\nStrictModes no\n\
             PasswordAuthentication no\nKbdInteractiveAuthentication no\nLogLevel ERROR\n\
             Subsystem sftp {SFTP_SERVER}\n",
            hk = p("hostkey").display(),
            ak = p("authorized_keys").display(),
        ),
    )
    .unwrap();
    std::fs::write(
        p("known_hosts"),
        format!("{HOST},{HOST_RTT} {}\n", public_key(&p("hostkey"))),
    )
    .unwrap();
    let sshd = format!("/usr/bin/sshd -i -e -f {}", p("sshd_config").display());
    let me = std::env::current_exe().unwrap();
    std::fs::write(
        config(),
        format!(
            "Host {HOST}\n\
             \x20 ProxyCommand /bin/sh -c 'ulimit -c 0; exec {sshd} 2>/dev/null'\n\
             Host {HOST_RTT}\n\
             \x20 ProxyCommand /bin/sh -c 'ulimit -c 0; exec {me} delay-pipe 15 {sshd} 2>/dev/null'\n\
             Host {HOST} {HOST_RTT}\n\
             \x20 IdentityFile {id}\n\
             \x20 IdentitiesOnly yes\n\
             \x20 IdentityAgent none\n\
             \x20 UserKnownHostsFile {kh}\n\
             \x20 GlobalKnownHostsFile /dev/null\n\
             \x20 StrictHostKeyChecking yes\n\
             \x20 BatchMode yes\n\
             \x20 CheckHostIP no\n\
             \x20 UpdateHostKeys no\n\
             \x20 ControlMaster no\n\
             \x20 ControlPath none\n\
             \x20 Compression no\n\
             \x20 LogLevel ERROR\n",
            me = me.display(),
            id = p("id").display(),
            kh = p("known_hosts").display(),
        ),
    )
    .unwrap();
    config()
}

fn target(host: &str) -> Target {
    Target {
        user: None,
        host: host.into(),
        port: None,
    }
}

/// The server command on pipes: `sftp-server -e`, as `sh -c 'ulimit -c 0; exec ...'`.
fn server_cmd(extra: &[&str]) -> Command {
    let mut c = Command::new("/bin/sh");
    c.arg("-c")
        .arg("ulimit -c 0; exec \"$0\" \"$@\"")
        .arg(SFTP_SERVER)
        .arg("-e")
        .args(extra);
    c
}

/// A session over `via`, as the app holds one.
fn connect(via: &str) -> Arc<RemoteProvider> {
    let cmd = match via {
        "pipes" => server_cmd(&[]),
        "pipes-rtt30" => {
            let mut c = Command::new(std::env::current_exe().unwrap());
            c.args([
                "delay-pipe",
                "15",
                "/bin/sh",
                "-c",
                "ulimit -c 0; exec \"$0\" -e",
            ])
            .arg(SFTP_SERVER);
            c
        }
        "ssh" | "ssh-rtt30" => {
            let setting: Vec<String> =
                vec!["ssh".into(), "-F".into(), config().display().to_string()];
            SshCommand::from_setting(Some(&setting))
                .unwrap()
                .command(&target(if via == "ssh" { HOST } else { HOST_RTT }))
                .unwrap()
        }
        _ => panic!("unknown transport {via}"),
    };
    let s = transport::start_plain(cmd, None).unwrap_or_else(|e| panic!("{via}: {e}"));
    Arc::new(RemoteProvider::new(s, target("bench")))
}

/// `sftp` over the same transport, in batch mode with `commands`; its run time.
fn sftp(via: &str, commands: &str, flags: &[&str]) -> Duration {
    let me = std::env::current_exe().unwrap();
    let mut c = Command::new("sftp");
    // Options before the destination: sftp stops reading options there.
    c.arg("-q").args(flags).arg("-b").arg("-");
    match via {
        "pipes" => {
            c.arg("-D").arg(format!("{SFTP_SERVER} -e"));
        }
        "pipes-rtt30" => {
            c.arg("-D")
                .arg(format!("{} delay-pipe 15 {SFTP_SERVER} -e", me.display()));
        }
        "ssh" => {
            c.arg("-F").arg(config()).arg(HOST);
        }
        "ssh-rtt30" => {
            c.arg("-F").arg(config()).arg(HOST_RTT);
        }
        _ => panic!("unknown transport {via}"),
    }
    c.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let t = Instant::now();
    let mut child = c.spawn().expect("sftp");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(commands.as_bytes())
        .unwrap();
    let st = child.wait().unwrap();
    assert!(st.success(), "sftp {via} {commands:?}: {st}");
    t.elapsed()
}

fn vpath(p: &Path) -> VPath {
    VPath::parse(p.as_os_str().as_bytes()).unwrap()
}

fn sync() {
    let _ = Command::new("sync").status();
}

fn fresh(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
}

/// Our download of `names` in the server directory `dir` into `dst`, with its own session:
/// connect, the copy job, close.
fn ours_get(
    via: &str,
    dir: &Path,
    names: &[&str],
    dst: &Path,
    sizes: Option<Sizes>,
) -> (Duration, Report, u64) {
    let t = Instant::now();
    let r = connect(via);
    if let Some(s) = sizes {
        r.session().set_sizes(s);
    }
    let rep = run_guarded(
        JobSpec::Copy {
            groups: vec![Group {
                root: Root::Remote(r.clone()),
                sub: vpath(dir).components().to_vec(),
                names: names.iter().map(OsString::from).collect(),
            }],
            dst: Dest::Local(dst.to_path_buf()),
        },
        &Sys::default(),
        &mut crate::Silent,
    );
    let requests = r.session().stats().requests;
    r.session().close_wait();
    (t.elapsed(), rep, requests)
}

/// Our upload of `names` in the local directory `dir` into the server directory `dst`.
fn ours_put(
    via: &str,
    dir: &Path,
    names: &[&str],
    dst: &Path,
    sizes: Option<Sizes>,
) -> (Duration, Report, u64) {
    let t = Instant::now();
    let r = connect(via);
    if let Some(s) = sizes {
        r.session().set_sizes(s);
    }
    let rep = run_guarded(
        JobSpec::Copy {
            groups: vec![Group::new(dir, names.iter().map(OsString::from).collect())],
            dst: Dest::Remote {
                session: r.clone(),
                dir: vpath(dst),
            },
        },
        &Sys::default(),
        &mut crate::Silent,
    );
    let requests = r.session().stats().requests;
    r.session().close_wait();
    (t.elapsed(), rep, requests)
}

fn check(rep: &Report) {
    assert!(
        rep.failed == 0 && rep.refused.is_none() && rep.skipped == 0,
        "{rep:?}"
    );
}

/// `p3-sftp-get|put VIA RUNS`: P-26 with the 1 GiB file, ours and `sftp` alternating,
/// each run on a fresh session and after `sync`.
fn transfer(via: &str, runs: usize, up: bool) {
    let remote = root().join("remote");
    let dst = root().join(if up { "up" } else { "local" });
    let (mut ours, mut theirs) = (Vec::new(), Vec::new());
    let mut requests = 0;
    for _ in 0..runs {
        fresh(&dst);
        sync();
        let (t, rep, n) = if up {
            ours_put(via, &remote, &["big1g"], &dst, None)
        } else {
            ours_get(via, &remote, &["big1g"], &dst, None)
        };
        check(&rep);
        assert_eq!(std::fs::metadata(dst.join("big1g")).unwrap().len(), 1 << 30);
        ours.push(t.as_secs_f64());
        requests = n;
        fresh(&dst);
        sync();
        let cmd = if up {
            format!(
                "put {} {}\n",
                remote.join("big1g").display(),
                dst.join("big1g").display()
            )
        } else {
            format!(
                "get {} {}\n",
                remote.join("big1g").display(),
                dst.join("big1g").display()
            )
        };
        theirs.push(sftp(via, &cmd, &[]).as_secs_f64());
        assert_eq!(std::fs::metadata(dst.join("big1g")).unwrap().len(), 1 << 30);
    }
    fresh(&dst);
    let (o, t) = (median(&ours), median(&theirs));
    println!(
        "p3-sftp-{} via={via} ours_s={o:.3} sftp_s={t:.3} ratio={:.3} ours_mib_s={:.0} sftp_mib_s={:.0} ours_all={} sftp_all={} requests={requests}",
        if up { "put" } else { "get" },
        o / t,
        1024.0 / o,
        1024.0 / t,
        fmt_list(&ours),
        fmt_list(&theirs)
    );
}

fn fmt_list(v: &[f64]) -> String {
    v.iter()
        .map(|x| format!("{x:.3}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// `p3-sftp-list VIA RUNS`: P-27. The 10,000-entry directory listed as a panel lists it
/// (one batch per `READDIR` reply): first rows, complete, batches and requests; and the
/// round trip of one `LSTAT` on the same session, for the ratio against 103 round trips.
fn list(via: &str, runs: usize) {
    let dir = root().join("remote/list10k");
    let r = connect(via);
    let cancel = AtomicBool::new(false);
    let path = dir.as_os_str().as_bytes();
    let mut rtt = Vec::new();
    for _ in 0..5 {
        let t = Instant::now();
        r.session().lstat(path, &cancel).unwrap();
        rtt.push(ms(t.elapsed()));
    }
    let (mut first, mut all, mut batches, mut rows, mut reqs) = (Vec::new(), Vec::new(), 0, 0, 0);
    for _ in 0..runs {
        let req = ListRequest {
            slot: 0,
            generation: 1,
            remote: r.clone(),
            dir: RemoteDir::Absolute(vpath(&dir)),
            local: PathBuf::from("/"),
            sort: None,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let seen: Mutex<(Option<Duration>, usize, usize)> = Mutex::new((None, 0, 0));
        let before = r.session().stats().requests;
        let t = Instant::now();
        rp::list(&req, &|m| match m {
            ListingMsg::Batch { entries, .. } => {
                let mut s = seen.lock().unwrap();
                if s.0.is_none() {
                    s.0 = Some(t.elapsed());
                }
                s.1 += 1;
                s.2 += entries.len();
            }
            ListingMsg::Failed { error, .. } => panic!("listing failed: {error}"),
            _ => {}
        });
        all.push(ms(t.elapsed()));
        let s = seen.into_inner().unwrap();
        first.push(s.0.map_or(f64::NAN, ms));
        batches = s.1;
        rows = s.2;
        reqs = r.session().stats().requests - before;
    }
    r.session().close_wait();
    let rt = median(&rtt);
    println!(
        "p3-sftp-list via={via} first_ms={:.2} complete_ms={:.1} rtt_ms={rt:.2} round_trips={:.1} ratio_103={:.3} batches={batches} rows={rows} requests={reqs}",
        median(&first),
        median(&all),
        median(&all) / rt,
        median(&all) / (103.0 * rt)
    );
}

/// `p3-sftp-tree VIA NAME RUNS`: the small-file tree NAME down (`get -rp`) and up
/// (`put -rp`), ours and `sftp` alternating, RUNS times; medians. `-p`: `sftp` sets the
/// mode and times too, as ours always does. Each run starts after `sync` on an empty
/// destination.
fn tree(via: &str, name: &str, runs: usize) {
    let remote = root().join("remote");
    let local = root().join("local");
    let up = root().join("up");
    let n = walk_files(&remote.join(name));
    let get_cmd = format!(
        "get -r {} {}\n",
        remote.join(name).display(),
        local.join(name).display()
    );
    let put_cmd = format!(
        "put -r {} {}\n",
        remote.join(name).display(),
        up.join(name).display()
    );
    let (mut og, mut sg, mut op, mut sp) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut get_reqs, mut put_reqs) = (0, 0);
    for _ in 0..runs {
        fresh(&local);
        sync();
        let (t, rep, r) = ours_get(via, &remote, &[name], &local, None);
        check(&rep);
        assert_eq!(walk_files(&local.join(name)), n);
        og.push(t.as_secs_f64());
        get_reqs = r;
        fresh(&local);
        sync();
        sg.push(sftp(via, &get_cmd, &["-p"]).as_secs_f64());
        assert_eq!(walk_files(&local.join(name)), n);
        fresh(&up);
        sync();
        let (t, rep, r) = ours_put(via, &remote, &[name], &up, None);
        check(&rep);
        assert_eq!(walk_files(&up.join(name)), n);
        op.push(t.as_secs_f64());
        put_reqs = r;
        fresh(&up);
        sync();
        sp.push(sftp(via, &put_cmd, &["-p"]).as_secs_f64());
        assert_eq!(walk_files(&up.join(name)), n);
    }
    fresh(&local);
    fresh(&up);
    println!(
        "p3-sftp-tree via={via} tree={name} files={n} runs={runs} get_ours_s={:.3} get_sftp_s={:.3} get_ratio={:.3} put_ours_s={:.3} put_sftp_s={:.3} put_ratio={:.3} get_requests={get_reqs} put_requests={put_reqs} get_all={} put_all={}",
        median(&og),
        median(&sg),
        median(&og) / median(&sg),
        median(&op),
        median(&sp),
        median(&op) / median(&sp),
        fmt_list(&og),
        fmt_list(&op),
    );
}

fn walk_files(d: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(d) else {
        return 0;
    };
    rd.flatten()
        .map(|e| {
            if e.file_type().unwrap().is_dir() {
                walk_files(&e.path())
            } else {
                1
            }
        })
        .sum()
}

/// `p3-sftp-sweep VIA MIB WINDOW:CHUNK...`: the tuning sweep. A file of MIB MiB (the head
/// of the 1 GiB file) up and down with each window and request size, beside `sftp`, five
/// runs each (`MC_SWEEP_RUNS`), alternating. CHUNK 0 keeps the server's
/// `limits@openssh.com` sizes. Below 256 MiB a run stays under the dirty-page limit of the
/// bench machine, so the destination's writeback does not throttle it.
fn sweep(via: &str, mib: u64, settings: &[&str]) {
    let runs: usize = std::env::var("MC_SWEEP_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let remote = root().join("remote");
    let part = root().join("part");
    fresh(&part);
    {
        let mut src = std::fs::File::open(remote.join("big1g")).unwrap();
        let mut out = std::fs::File::create(part.join("payload")).unwrap();
        let mut buf = vec![0u8; 1 << 20];
        for _ in 0..mib {
            src.read_exact(&mut buf).unwrap();
            out.write_all(&buf).unwrap();
        }
    }
    let dst = root().join("local");
    let up = root().join("up");
    let get_cmd = format!(
        "get {} {}\n",
        part.join("payload").display(),
        dst.join("payload").display()
    );
    let put_cmd = format!(
        "put {} {}\n",
        part.join("payload").display(),
        up.join("payload").display()
    );
    for s in settings {
        let (w, c) = s.split_once(':').unwrap();
        let (w, c): (usize, u32) = (w.parse().unwrap(), c.parse().unwrap());
        let sizes = (c > 0).then_some(Sizes {
            read: c,
            write: c,
            window: w,
        });
        // Ours and sftp alternate, so a drift in the machine's state hits both.
        let (mut get, mut put, mut sget, mut sput) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        // `MC_SWEEP_ONLY=get|put` and `MC_SWEEP_OURS=1` narrow a sweep for a profiler.
        let only = std::env::var("MC_SWEEP_ONLY").unwrap_or_default();
        let ours_only = std::env::var("MC_SWEEP_OURS").is_ok_and(|v| v == "1");
        for _ in 0..runs {
            if only != "put" {
                fresh(&dst);
                sync();
                let (t, rep, _) = ours_get(via, &part, &["payload"], &dst, sizes);
                check(&rep);
                get.push(t.as_secs_f64());
                if !ours_only {
                    fresh(&dst);
                    sync();
                    sget.push(sftp(via, &get_cmd, &[]).as_secs_f64());
                }
            }
            if only != "get" {
                fresh(&up);
                sync();
                let (t, rep, _) = ours_put(via, &part, &["payload"], &up, sizes);
                check(&rep);
                put.push(t.as_secs_f64());
                if !ours_only {
                    fresh(&up);
                    sync();
                    sput.push(sftp(via, &put_cmd, &[]).as_secs_f64());
                }
            }
        }
        println!(
            "p3-sftp-sweep via={via} mib={mib} window={w} chunk={c} get_s={:.3} sftp={:.3} ({:.3}x) put_s={:.3} sftp={:.3} ({:.3}x) get_all={} put_all={} sftp_get_all={} sftp_put_all={}",
            median(&get),
            median(&sget),
            median(&get) / median(&sget),
            median(&put),
            median(&sput),
            median(&put) / median(&sput),
            fmt_list(&get),
            fmt_list(&put),
            fmt_list(&sget),
            fmt_list(&sput)
        );
    }
    fresh(&dst);
    fresh(&up);
    let _ = std::fs::remove_dir_all(&part);
}

// ---- the latency helper -----------------------------------------------------------------

/// Forwards `from` to `to`, each chunk `delay` after it arrived; ends when either side
/// closes, and closes `to`.
fn delayed(
    mut from: impl Read + Send + 'static,
    mut to: impl Write + Send + 'static,
    delay: Duration,
) -> std::thread::JoinHandle<()> {
    let (tx, rx) = channel::<(Instant, Vec<u8>)>();
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 256 << 10];
        loop {
            match from.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if tx.send((Instant::now(), buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
            }
        }
    });
    std::thread::spawn(move || {
        for (at, b) in rx {
            let due = at + delay;
            let now = Instant::now();
            if due > now {
                std::thread::sleep(due - now);
            }
            if to.write_all(&b).and_then(|()| to.flush()).is_err() {
                return;
            }
        }
    })
}

/// `delay-pipe MS CMD ARGS...`: runs CMD with its stdin and stdout behind a link that
/// delays each direction by MS (a round trip of twice that). For `sftp -D` and a
/// `ProxyCommand`, and for our sessions. Exits when CMD's output ends, and kills CMD then.
pub fn delay_pipe(ms: u64, cmd: &[&str]) {
    no_core_dumps();
    let delay = Duration::from_millis(ms);
    let mut child = Command::new(cmd[0])
        .args(&cmd[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("delay-pipe: spawn");
    let cin = child.stdin.take().unwrap();
    let cout = child.stdout.take().unwrap();
    let _up = delayed(std::io::stdin(), cin, delay);
    let down = delayed(cout, std::io::stdout(), delay);
    let _ = down.join();
    let _ = child.kill();
    let _ = child.wait();
    std::process::exit(0);
}

// ---- P-5b -------------------------------------------------------------------------------

/// `p3-idle BIN REMOTE ARCHIVE SSHCFG SECS`: P-5b. The left panel opens ARCHIVE and
/// leaves it (a cached index), then connects to REMOTE (an `sftp://` address through `sshd
/// -i`) and rests on a JPEG there; `Ctrl+Q` and `Alt+Q` show it in the quick view (kitty
/// graphics). Then SECS s of idle: manycommander's voluntary context switches and CPU ticks
/// over all its threads, before and after (the A-P-5 method; the ssh child is another
/// process and is not counted).
pub fn idle(bin: &str, remote: &str, archive: &str, cfg: &str, secs: u64) {
    let src = Path::new(archive);
    let name = src.file_name().unwrap().to_string_lossy().into_owned();
    let dir = super::run_dir("idle");
    std::fs::hard_link(src, dir.join(&name)).unwrap();
    let log = log_file("idle");
    let d = dir.to_string_lossy().into_owned();
    let config = ssh_setting(cfg);
    let mut p = Pty::spawn(
        bin,
        &["--log", log.to_str().unwrap(), &d, &d],
        &Opts {
            term: Term::ghostty(),
            cols: super::preview::COLS,
            rows: super::preview::ROWS,
            config: &config,
        },
    );
    p.expect("10Quit", Duration::from_secs(10));
    p.keys(&[END, ENTER], Duration::from_millis(100));
    assert!(
        p.until(Duration::from_secs(30), |_| !log_lines(
            &log,
            "archive scan done"
        )
        .is_empty()),
        "the scan did not end"
    );
    p.idle(Duration::from_millis(300));
    p.keys(&[b"\x7f"], Duration::from_millis(300));
    run_line(&mut p, &format!("cd {remote}"));
    assert!(
        p.wait_for("connected to sftp://", Duration::from_secs(20))
            && p.wait_for("photo1", Duration::from_secs(20)),
        "no remote listing:\n{}",
        p.screen()
    );
    p.keys(&[HOME, DOWN, DOWN], Duration::from_millis(100));
    p.keys(&[CTRL_Q], Duration::from_millis(300));
    let before = p.seen.placements.len();
    p.send(ALT_Q);
    assert!(
        p.until(Duration::from_secs(20), |p| p.seen.placements.len()
            > before),
        "the remote image did not show:\n{}",
        p.screen()
    );
    p.idle(Duration::from_secs(3));
    let ssh = ssh_alive(p.pid());
    let a = crate::proc_counters(p.pid());
    std::thread::sleep(Duration::from_secs(secs));
    let b = crate::proc_counters(p.pid());
    p.pump();
    let still = ssh_alive(p.pid());
    let cached = log_lines(&log, "archive scan done").len();
    println!(
        "p3-idle secs={secs} switches_before={} switches_after={} ticks_before={} ticks_after={} ssh_before={ssh} ssh_after={still} scans={cached} placements={}",
        a.0,
        b.0,
        a.1,
        b.1,
        p.seen.placements.len()
    );
    p.quit();
    drop(p);
    let _ = std::fs::remove_file(&log);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Whether manycommander has a running `ssh` child.
fn ssh_alive(pid: i32) -> bool {
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return false;
    };
    rd.flatten().any(|e| {
        let Ok(s) = std::fs::read_to_string(e.path().join("stat")) else {
            return false;
        };
        let Some((head, after)) = s.rsplit_once(')') else {
            return false;
        };
        let comm = head.split_once('(').map_or("", |x| x.1);
        let f: Vec<&str> = after.split_whitespace().collect();
        comm == "ssh" && f.get(1).and_then(|p| p.parse::<i32>().ok()) == Some(pid) && f[0] != "Z"
    })
}
