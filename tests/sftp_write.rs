//! SFTP 3b (P3 5.6): A-SF-7 (uploads: the hard-link commit, "file exists", the atomic
//! overwrite and its refusal, the direct-write mode and its lost session, temporary names
//! on errors and cancels, symlinks uploaded as symlinks in the right direction), A-SF-8 (F7,
//! Shift+F6, F6 within one session, Shift+F8 beside a symlink to the outside, the F8 and
//! cross-session refusals, a read-only server), A-SF-9 (moves across hosts: the
//! best-effort dialog, the local sources removed only after the commit, "not synced on
//! the server", a download move that keeps its remote sources, and the failpoint sweep)
//! and A-SF-10 (the F4 write-back question).
//!
//! Every test runs OpenSSH's `sftp-server` on plain pipes, some behind a tap that records
//! the requests or drops the link; `-P` denies requests and hides the extensions it names
//! from `SSH_FXP_VERSION`, and `-R` makes the server read-only. Nothing contacts a host or a
//! port. Every `sftp-server` runs with `RLIMIT_CORE` 0 (set in the test process and again
//! in `sh -c 'ulimit -c 0; exec ...'`) and is killed and reaped by the test that started it.

mod common;

use common::sftp::{have_sftp_server, lost_channel, no_core_dumps, sftp_server_command};
use common::{Script, noise, test_dir, write};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use manycommander::app::App;
use manycommander::app::event::{Effect, Event, JobEvent};
use manycommander::config::Config;
use manycommander::fsops::group::{Group, Root};
use manycommander::fsops::job::{Dest, JobSpec, JobVerb, Outcome, Report, run_guarded};
use manycommander::fsops::question::{Answer, Question};
use manycommander::fsops::sys::Sys;
use manycommander::panel::listing;
use manycommander::provider::{Target, VPath};
use manycommander::remote::proto::{self, Packet};
use manycommander::remote::provider::{self as rp, RemoteProvider};
use manycommander::remote::put::{DIRECT_WRITE, MAY_BE_PARTIAL, NO_ATOMIC_REPLACE, NOT_SYNCED};
use manycommander::remote::tree::REMOTE_KEPT;
use manycommander::remote::{Lost, transport};
use manycommander::theme::Depth;
use manycommander::ui::dialog::Dialog;
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(20);
const NONE: KeyModifiers = KeyModifiers::NONE;
const SHIFT: KeyModifiers = KeyModifiers::SHIFT;

// ---- helpers ------------------------------------------------------------------------------

fn target(host: &str) -> Target {
    Target {
        user: None,
        host: host.into(),
        port: None,
    }
}

fn bytes(p: &Path) -> Vec<u8> {
    p.as_os_str().as_bytes().to_vec()
}

fn vpath(p: &Path) -> VPath {
    VPath::parse(&bytes(p)).unwrap()
}

/// `sftp-server -e -d <dir> <extra>` on plain pipes for the server `host`.
fn server_as(dir: &Path, extra: &[&str], host: &str) -> (Arc<RemoteProvider>, Receiver<Lost>) {
    no_core_dumps();
    let (on_lost, rx) = lost_channel();
    let s = transport::start_plain(sftp_server_command(dir, extra), Some(on_lost))
        .expect("sftp-server session");
    (Arc::new(RemoteProvider::new(s, target(host))), rx)
}

fn server(dir: &Path, extra: &[&str]) -> (Arc<RemoteProvider>, Receiver<Lost>) {
    server_as(dir, extra, "srv")
}

/// The session closes and its child is reaped.
fn close(r: &Arc<RemoteProvider>) {
    let pid = r.session().pid();
    r.session().close_wait();
    if let Some(pid) = pid {
        assert!(
            common::sftp::gone_within(pid, T),
            "the child {pid} was not reaped"
        );
    }
}

/// Kills a session's child as a crash would: `SIGKILL` to its process group.
fn kill(pid: u32) {
    if let Some(g) = rustix::process::Pid::from_raw(pid as i32) {
        let _ = rustix::process::kill_process_group(g, rustix::process::Signal::KILL);
    }
}

/// The requests a tapped session sent, in order; a `WRITE` without its data.
type Log = Arc<Mutex<Vec<Packet>>>;

/// `sftp-server` behind a tap: each request is logged, then `pass` decides whether it goes
/// on. `false` drops the link: the server is killed before it sees the request, and the
/// session reads EOF, as when a connection breaks.
fn tapped(
    dir: &Path,
    extra: &[&str],
    mut pass: impl FnMut(&Packet) -> bool + Send + 'static,
) -> (Arc<RemoteProvider>, Receiver<Lost>, Log) {
    use std::io::Write;
    use std::os::unix::process::CommandExt;
    no_core_dumps();
    let mut child = sftp_server_command(dir, extra)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .expect("sftp-server");
    let pid = child.id();
    let mut server_in = std::fs::File::from(rustix::fd::OwnedFd::from(child.stdin.take().unwrap()));
    let mut server_out =
        std::fs::File::from(rustix::fd::OwnedFd::from(child.stdout.take().unwrap()));
    let (req_r, req_w) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).unwrap();
    let (rep_r, rep_w) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).unwrap();
    let mut requests = std::fs::File::from(req_r);
    let mut replies = std::fs::File::from(rep_w);
    let log: Log = Arc::default();
    let l = log.clone();
    std::thread::spawn(move || {
        let mut first = true;
        while let Ok(Some(body)) = proto::read_frame(&mut requests) {
            // The first frame is `SSH_FXP_INIT`, which carries no request id.
            if !first && let Ok(p) = Packet::decode(body.clone()) {
                let p = match p {
                    Packet::Write {
                        id,
                        handle,
                        offset,
                        data,
                    } => Packet::Write {
                        id,
                        handle,
                        offset: offset + data.len() as u64,
                        data: Vec::new().into(),
                    },
                    p => p,
                };
                let go = pass(&p);
                l.lock().unwrap().push(p);
                if !go {
                    kill(pid);
                    return;
                }
            }
            first = false;
            let len = (body.len() as u32).to_be_bytes();
            if server_in.write_all(&len).is_err() || server_in.write_all(&body).is_err() {
                return;
            }
        }
    });
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut server_out, &mut replies);
    });
    let (on_lost, rx) = lost_channel();
    let s = transport::start_pipes(
        rep_r,
        req_w,
        Box::new(transport::ChildPeer::new(child)),
        Some(on_lost),
    )
    .expect("tapped session");
    (Arc::new(RemoteProvider::new(s, target("srv"))), rx, log)
}

/// The extension requests of a log by name, and the plain requests by type.
fn count(log: &Log, what: &str) -> usize {
    log.lock()
        .unwrap()
        .iter()
        .filter(|p| match p {
            Packet::Extended { name, .. } => name.as_slice() == what.as_bytes(),
            Packet::Rename { .. } => what == "rename",
            Packet::Remove { .. } => what == "remove",
            Packet::Open { .. } => what == "open",
            Packet::Write { .. } => what == "write",
            Packet::Lstat { .. } => what == "lstat",
            Packet::Symlink { .. } => what == "symlink",
            Packet::Mkdir { .. } => what == "mkdir",
            Packet::Rmdir { .. } => what == "rmdir",
            _ => false,
        })
        .count()
}

/// No `WRITE` reaches the server between a handle's `FSETSTAT` and its `CLOSE`: no byte
/// lands after a file's times were set, which would give it the time of that write. The
/// server may reuse a closed handle's string. Returns the number of `FSETSTAT`s.
fn no_write_after_times(log: &Log) -> usize {
    let mut timed: Vec<Vec<u8>> = Vec::new();
    let mut n = 0;
    for p in log.lock().unwrap().iter() {
        match p {
            Packet::Fsetstat { handle, .. } => {
                timed.push(handle.clone());
                n += 1;
            }
            Packet::Close { handle, .. } => timed.retain(|h| h != handle),
            Packet::Write { handle, .. } => {
                assert!(!timed.contains(handle), "a WRITE after its FSETSTAT: {p:?}");
            }
            _ => {}
        }
    }
    n
}

fn partials(dir: &Path) -> Vec<PathBuf> {
    walk(dir)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .unwrap()
                .as_bytes()
                .windows(12)
                .any(|w| w == b".mc-partial-")
        })
        .collect()
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            out.push(p.clone());
            if e.file_type().unwrap().is_dir() {
                out.extend(walk(&p));
            }
        }
    }
    out.sort();
    out
}

/// `(file name, reason)` of each issue.
fn issues(r: &Report) -> Vec<(String, String)> {
    r.issues
        .iter()
        .map(|i| {
            let name = i.path.file_name().unwrap_or_default();
            let why = match &i.outcome {
                Outcome::Skipped(w) | Outcome::Failed(w) => w.clone(),
            };
            (name.to_string_lossy().into_owned(), why)
        })
        .collect()
}

/// Sets a mode and an mtime (whole seconds) of a file or directory.
fn stamp(p: &Path, mode: u32, mtime: i64) {
    stamp_ns(p, mode, mtime, 0);
}

/// [`stamp`] with a fraction of a second, which SFTP version 3 cannot carry.
fn stamp_ns(p: &Path, mode: u32, mtime: i64, nsec: i64) {
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
    let ts = rustix::fs::Timespec {
        tv_sec: mtime,
        tv_nsec: nsec,
    };
    let t = rustix::fs::Timestamps {
        last_access: ts,
        last_modification: ts,
    };
    rustix::fs::utimensat(rustix::fs::CWD, p, &t, rustix::fs::AtFlags::empty()).unwrap();
}

/// A job's groups: `names` in the local directory `src`.
fn local(src: &Path, names: &[&str]) -> Vec<Group> {
    vec![Group::new(src, names.iter().map(OsString::from).collect())]
}

fn to(r: &Arc<RemoteProvider>, dir: &Path) -> Dest {
    Dest::Remote {
        session: r.clone(),
        dir: vpath(dir),
    }
}

/// F5 (or F6 with `moving`) of `names` in `src` to `dst` on the server.
fn upload(
    r: &Arc<RemoteProvider>,
    src: &Path,
    names: &[&str],
    dst: &Path,
    moving: bool,
    sys: &Sys,
    ui: &mut Script,
) -> Report {
    let groups = local(src, names);
    let dst = to(r, dst);
    let spec = if moving {
        JobSpec::Move { groups, dst }
    } else {
        JobSpec::Copy { groups, dst }
    };
    run_guarded(spec, sys, ui)
}

/// The group of `names` in the server directory `dir`.
fn remote_group(r: &Arc<RemoteProvider>, dir: &Path, names: &[&str]) -> Vec<Group> {
    vec![Group {
        root: Root::Remote(r.clone()),
        sub: vpath(dir).components().to_vec(),
        names: names.iter().map(OsString::from).collect(),
    }]
}

/// The upload fixture: files of 0 B, 1 B, 1 MiB + 1 (setuid) and 3 MiB with modes and
/// mtimes, a tree with its own directory modes, and a symlink.
///
/// Every file and directory gets a fixed mode and mtime, so two fixtures made at different
/// times are alike: a move compares its destination with such a twin, because its source
/// is gone. An unstamped file carries the time it was written, so a second boundary between
/// the writes of the two fixtures makes them differ by a second. `sub/deeper/f` keeps a
/// fraction of a second, which SFTP version 3 cannot carry: an upload truncates it, never
/// rounds it up.
fn fixture(src: &Path) {
    std::fs::create_dir_all(src.join("sub/deeper")).unwrap();
    for (k, (name, n, mode)) in [
        ("a", 0usize, 0o640),
        ("b", 1, 0o755),
        ("c", (1 << 20) + 1, 0o4755),
        ("d", 3 << 20, 0o600),
    ]
    .into_iter()
    .enumerate()
    {
        write(&src.join(name), &noise(n, k as u64 + 1));
        stamp(&src.join(name), mode, 1_600_000_000 + k as i64);
    }
    write(&src.join("sub/e"), b"e");
    stamp(&src.join("sub/e"), 0o644, 1_600_000_010);
    write(&src.join("sub/deeper/f"), &noise(70_000, 9));
    stamp_ns(&src.join("sub/deeper/f"), 0o664, 1_600_000_011, 999_999_999);
    std::os::unix::fs::symlink("b", src.join("link")).unwrap();
    stamp(&src.join("sub/deeper"), 0o750, 1_500_000_000);
    stamp(&src.join("sub"), 0o711, 1_500_000_001);
}

const FIXTURE: &[&str] = &["a", "b", "c", "d", "sub", "link"];

/// `dst` holds what `src` held: bytes, symlink targets, and modes (setuid and setgid
/// cleared) and mtimes (the whole seconds SFTP version 3 carries) of files and directories.
fn same_tree(src: &Path, dst: &Path) {
    for p in walk(src) {
        let rel = p.strip_prefix(src).unwrap();
        let q = dst.join(rel);
        let a = std::fs::symlink_metadata(&p).unwrap();
        let b = std::fs::symlink_metadata(&q).unwrap_or_else(|e| panic!("{q:?}: {e}"));
        if a.file_type().is_symlink() {
            assert!(b.file_type().is_symlink(), "{q:?}");
            assert_eq!(
                std::fs::read_link(&p).unwrap(),
                std::fs::read_link(&q).unwrap()
            );
            continue;
        }
        assert_eq!(b.mode() & 0o7777, a.mode() & 0o7777 & !0o6000, "{q:?}");
        assert_eq!(
            b.mtime(),
            a.mtime(),
            "{q:?}: mtime {}.{:09}, source {}.{:09}",
            b.mtime(),
            b.mtime_nsec(),
            a.mtime(),
            a.mtime_nsec()
        );
        if a.is_file() {
            assert!(
                std::fs::read(&p).unwrap() == std::fs::read(&q).unwrap(),
                "{q:?}"
            );
        }
    }
}

fn app(left: &Path, right: &Path) -> App {
    App::new(
        left.to_path_buf(),
        right.to_path_buf(),
        right.to_path_buf(),
        Config::default(),
        None,
        Depth::TrueColor,
        jiff::tz::TimeZone::UTC,
    )
}

/// Performs listings and remote listings synchronously; returns the other effects.
fn run(a: &mut App, fx: Vec<Effect>) -> Vec<Effect> {
    let mut rest = Vec::new();
    for e in fx {
        let msgs = Mutex::new(Vec::new());
        let send = |m| msgs.lock().unwrap().push(m);
        match e {
            Effect::List(req, alive) => {
                listing::guarded(&req, &send, listing::list);
                alive.finish();
            }
            Effect::ListRemote(req, alive) => {
                rp::list(&req, &send);
                alive.finish();
            }
            other => rest.push(other),
        }
        for m in msgs.into_inner().unwrap() {
            let more = a.update(Event::Listing(m));
            rest.extend(run(a, more));
        }
    }
    for s in 0..2 {
        a.sides[s].panel_mut().ensure_sorted();
    }
    rest
}

fn press(a: &mut App, code: KeyCode, m: KeyModifiers) -> Vec<Effect> {
    a.update(Event::Key(KeyEvent::new(code, m), Instant::now()))
}

fn status(a: &App) -> String {
    a.status
        .as_ref()
        .map(|s| s.text.clone())
        .unwrap_or_default()
}

/// Types `text` into the open input dialog, replacing what it holds.
fn type_in(a: &mut App, text: &str) {
    press(a, KeyCode::Char('u'), KeyModifiers::CONTROL);
    for c in text.chars() {
        press(a, KeyCode::Char(c), NONE);
    }
}

/// An app with a local directory on both sides, started, whose pool holds `servers`.
fn app_with(servers: &[&Arc<RemoteProvider>], local: &Path) -> App {
    let mut a = app(local, local);
    let fx = a.start();
    run(&mut a, fx);
    for r in servers {
        a.pool.insert((*r).clone());
    }
    a
}

/// `cd sftp://<host><dir>` in the active panel, run to completion.
fn cd_remote(a: &mut App, host: &str, dir: &Path) {
    let mut t = format!("cd sftp://{host}").into_bytes();
    t.extend_from_slice(&bytes(dir));
    a.line.set(&t);
    let fx = press(a, KeyCode::Enter, NONE);
    let rest = run(a, fx);
    assert!(
        rest.iter()
            .all(|e| matches!(e, Effect::Watch { dir: None, .. })),
        "{rest:?}"
    );
}

/// The job an effect list starts.
fn started(fx: &[Effect]) -> JobSpec {
    match fx {
        [Effect::StartJob(spec)] => spec.clone(),
        _ => panic!("no job: {fx:?}"),
    }
}

/// Runs the job the app started and hands the report back to the app; a report dialog it
/// opens is closed.
fn finish(a: &mut App, spec: JobSpec, ui: &mut Script) -> Report {
    let r = run_guarded(spec, &Sys::default(), ui);
    let fx = a.update(Event::Job(JobEvent::Done(r.clone())));
    run(a, fx);
    if matches!(a.dialog, Some(Dialog::Report { .. })) {
        press(a, KeyCode::Esc, NONE);
    }
    r
}

/// The title and the lines of the open dialog.
fn dialog_text(a: &App) -> (String, Vec<String>) {
    match &a.dialog {
        Some(Dialog::Input { title, lines, .. } | Dialog::Confirm { title, lines, .. }) => {
            (title.clone(), lines.clone())
        }
        Some(Dialog::Choose {
            title,
            lines,
            buttons,
            ..
        }) => {
            let mut l = lines.clone();
            l.extend(buttons.iter().cloned());
            (title.clone(), l)
        }
        _ => panic!("no dialog"),
    }
}

// ---- A-SF-7: uploads ---------------------------------------------------------------------

/// R-1: every file is written under a `.mc-partial-` name opened `CREAT` + `EXCL` +
/// `WRITE`, committed with `hardlink@openssh.com`, and its temporary name removed; no
/// `SSH_FXP_RENAME` commits anything. Files of 0 B, 1 B, 1 MiB + 1 and 3 MiB arrive
/// byte-identical with their modes (setuid cleared) and mtimes, directories with theirs,
/// the symlink as a symlink; no temporary name remains.
#[test]
fn a_sf_7_uploads_commit_through_a_hard_link() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-commit");
    let src = d.join("src");
    let dst = d.join("dst");
    fixture(&src);
    std::fs::create_dir(&dst).unwrap();
    let (r, _lost, log) = tapped(&d.path, &[], |_| true);
    let started = Instant::now();
    let rep = upload(
        &r,
        &src,
        FIXTURE,
        &dst,
        false,
        &Sys::default(),
        &mut Script::silent(),
    );
    let took = started.elapsed();
    assert_eq!((rep.done, rep.failed, rep.skipped), (7, 0, 0), "{rep:?}");
    assert_eq!(rep.dirs_done, 2, "{rep:?}");
    assert!(rep.notes.is_empty(), "{rep:?}");
    same_tree(&src, &dst);
    assert!(partials(&d.path).is_empty(), "{:?}", partials(&d.path));
    assert_eq!(no_write_after_times(&log), 6);
    // Six regular files, each committed through a hard link and its temporary name removed.
    assert_eq!(count(&log, "hardlink@openssh.com"), 6);
    assert_eq!(count(&log, "remove"), 6);
    assert_eq!(count(&log, "rename"), 0);
    assert_eq!(count(&log, "posix-rename@openssh.com"), 0);
    for p in log.lock().unwrap().iter() {
        if let Packet::Open { path, pflags, .. } = p {
            let f = proto::open::WRITE | proto::open::CREAT | proto::open::EXCL;
            assert_eq!(*pflags, f);
            assert!(path.windows(12).any(|w| w == b".mc-partial-"), "{path:?}");
        }
    }
    eprintln!(
        "upload of 4 MiB in 9 entries through sftp-server on pipes: {:.0} ms (debug build)",
        took.as_secs_f64() * 1000.0
    );
    close(&r);
}

/// R-2: an existing name raises "file exists"; Skip leaves it; Rename commits under the new
/// name; Overwrite replaces through `posix-rename@openssh.com`: a second hard link to the
/// old file keeps the old bytes, so the old file was never truncated or written. A small
/// file finds the name at its commit, a large one before its upload.
#[test]
fn a_sf_7_file_exists_and_the_atomic_overwrite() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-exists");
    let src = d.join("src");
    let dst = d.join("dst");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    let big = noise(2 << 20, 4);
    write(&src.join("small"), b"new small");
    write(&src.join("big"), &big);
    for n in ["small", "big"] {
        write(&dst.join(n), b"old");
        std::fs::hard_link(dst.join(n), dst.join(format!("{n}.link"))).unwrap();
    }
    let (r, _lost, log) = tapped(&d.path, &[], |_| true);
    let sys = Sys::default();
    for n in ["small", "big"] {
        let mut ui = Script::new([Answer::Skip]);
        let rep = upload(&r, &src, &[n], &dst, false, &sys, &mut ui);
        assert!(
            matches!(ui.asked[..], [Question::FileExists { .. }]),
            "{n}: {:?}",
            ui.asked
        );
        assert_eq!(rep.skipped, 1, "{rep:?}");
        assert_eq!(std::fs::read(dst.join(n)).unwrap(), b"old");
        let mut ui = Script::new([Answer::Rename(format!("{n} (1)").into())]);
        let rep = upload(&r, &src, &[n], &dst, false, &sys, &mut ui);
        assert_eq!(rep.done, 1, "{rep:?}");
        assert_eq!(
            std::fs::read(dst.join(format!("{n} (1)"))).unwrap(),
            std::fs::read(src.join(n)).unwrap()
        );
        let before = count(&log, "posix-rename@openssh.com");
        let mut ui = Script::new([Answer::Overwrite]);
        let rep = upload(&r, &src, &[n], &dst, false, &sys, &mut ui);
        assert_eq!(rep.done, 1, "{rep:?}");
        assert_eq!(
            std::fs::read(dst.join(n)).unwrap(),
            std::fs::read(src.join(n)).unwrap()
        );
        assert_eq!(
            std::fs::read(dst.join(format!("{n}.link"))).unwrap(),
            b"old"
        );
        assert_eq!(count(&log, "posix-rename@openssh.com"), before + 1);
    }
    assert_eq!(count(&log, "rename"), 0);
    assert!(partials(&d.path).is_empty());
    close(&r);
}

/// R-2: a server without `posix-rename@openssh.com` refuses the overwrite with "the server
/// cannot replace a file atomically"; the existing file is untouched and nothing is left.
#[test]
fn a_sf_7_without_posix_rename_the_overwrite_is_refused() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-norename");
    let src = d.join("src");
    let dst = d.join("dst");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    write(&src.join("f"), b"new");
    write(&src.join("g"), b"new g");
    write(&dst.join("f"), b"old");
    let (r, _lost) = server(&d.path, &["-P", "posix-rename"]);
    assert!(!r.session().caps().posix_rename);
    let mut ui = Script::new([Answer::OverwriteAll]);
    let rep = upload(&r, &src, &["f", "g"], &dst, false, &Sys::default(), &mut ui);
    assert_eq!((rep.done, rep.failed), (1, 1), "{rep:?}");
    assert_eq!(issues(&rep), [("f".into(), NO_ATOMIC_REPLACE.into())]);
    assert_eq!(std::fs::read(dst.join("f")).unwrap(), b"old");
    assert_eq!(std::fs::read(dst.join("g")).unwrap(), b"new g");
    assert!(partials(&d.path).is_empty());
    close(&r);
}

/// R-1's direct-write mode: a server without `hardlink@openssh.com` gets the final names
/// created `CREAT` + `EXCL` and written directly; the report says so; an existing name
/// still raises "file exists"; nothing is committed through a rename.
#[test]
fn a_sf_7_without_hard_links_files_are_written_directly() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-direct");
    let src = d.join("src");
    let dst = d.join("dst");
    fixture(&src);
    std::fs::create_dir(&dst).unwrap();
    let (r, _lost, log) = tapped(&d.path, &["-P", "hardlink"], |_| true);
    assert!(!r.session().caps().hard_link);
    let rep = upload(
        &r,
        &src,
        FIXTURE,
        &dst,
        false,
        &Sys::default(),
        &mut Script::silent(),
    );
    assert_eq!((rep.done, rep.failed), (7, 0), "{rep:?}");
    assert_eq!(rep.notes, [DIRECT_WRITE.to_string()], "{rep:?}");
    same_tree(&src, &dst);
    assert_eq!(no_write_after_times(&log), 6);
    assert_eq!(count(&log, "hardlink@openssh.com"), 0);
    assert_eq!(count(&log, "rename"), 0);
    for p in log.lock().unwrap().iter() {
        if let Packet::Open { path, .. } = p {
            assert!(!path.windows(12).any(|w| w == b".mc-partial-"), "{path:?}");
        }
    }
    // An existing name: the `EXCL` create fails, and the question follows.
    let mut ui = Script::new([Answer::Skip]);
    let rep = upload(&r, &src, &["b"], &dst, false, &Sys::default(), &mut ui);
    assert!(
        matches!(ui.asked[..], [Question::FileExists { .. }]),
        "{:?}",
        ui.asked
    );
    assert_eq!(rep.skipped, 1);
    assert!(partials(&d.path).is_empty());
    close(&r);
}

/// R-1: a lost session in direct-write mode, in the middle of a file: the report names the
/// final path as possibly partial, the entry and every later one fail with "connection
/// lost" (I-7). The link is dropped at the file's second `WRITE`.
#[test]
fn a_sf_7_a_lost_session_in_direct_write_mode_names_the_final_path() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-direct-lost");
    let src = d.join("src");
    let dst = d.join("dst");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    write(&src.join("big"), &noise(3 << 20, 1));
    write(&src.join("later"), b"later");
    let mut writes = 0;
    let (r, lost, _log) = tapped(&d.path, &["-P", "hardlink"], move |p| {
        if matches!(p, Packet::Write { .. }) {
            writes += 1;
        }
        writes < 2
    });
    let rep = upload(
        &r,
        &src,
        &["big", "later"],
        &dst,
        false,
        &Sys::default(),
        &mut Script::silent(),
    );
    assert_eq!((rep.done, rep.failed), (0, 2), "{rep:?}");
    assert!(
        issues(&rep).iter().all(|(_, why)| why == "connection lost"),
        "{rep:?}"
    );
    let named = format!("sftp://srv{}: {MAY_BE_PARTIAL}", dst.join("big").display());
    assert!(rep.notes.contains(&named), "{:?}", rep.notes);
    assert!(lost.recv_timeout(T).is_ok(), "the loss is reported");
    assert!(!dst.join("later").exists());
    close(&r);
}

/// R-1: a lost session in the hard-link mode, in the middle of a file: the report names the
/// temporary path, the only `.mc-partial-` name left; the final name was never touched.
#[test]
fn a_sf_7_a_lost_session_names_the_temporary_file() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-lost");
    let src = d.join("src");
    let dst = d.join("dst");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    write(&src.join("big"), &noise(3 << 20, 1));
    let mut writes = 0;
    let (r, _lost, _log) = tapped(&d.path, &[], move |p| {
        if matches!(p, Packet::Write { .. }) {
            writes += 1;
        }
        writes < 3
    });
    let rep = upload(
        &r,
        &src,
        &["big"],
        &dst,
        false,
        &Sys::default(),
        &mut Script::silent(),
    );
    assert_eq!(rep.failed, 1, "{rep:?}");
    let left = partials(&d.path);
    assert_eq!(left.len(), 1, "{left:?}");
    let named = format!("sftp://srv{}: {MAY_BE_PARTIAL}", left[0].display());
    assert!(rep.notes.contains(&named), "{:?}", rep.notes);
    assert!(!dst.join("big").exists());
    close(&r);
}

/// Server errors at each step of an upload (`-P` denies a request) fail the entry through
/// the error question and leave no temporary name; a temporary name the server refuses to
/// remove after the commit is named in the report, and the upload counts as done.
#[test]
fn a_sf_7_server_errors_leave_no_temporary_name() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-errors");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    write(&src.join("f"), &noise(100_000, 3));
    write(&src.join("g"), b"g");
    for denied in ["write", "fsetstat", "close", "open"] {
        let dst = d.join(format!("dst-{denied}"));
        std::fs::create_dir(&dst).unwrap();
        let (r, _lost) = server(&d.path, &["-P", denied]);
        let mut ui = Script::new([Answer::SkipAllErrno]);
        let rep = upload(&r, &src, &["f", "g"], &dst, false, &Sys::default(), &mut ui);
        assert_eq!((rep.done, rep.failed), (0, 2), "{denied}: {rep:?}");
        assert!(
            matches!(ui.asked[..], [Question::ServerError { .. }]),
            "{denied}: one question, then Skip all of this error: {:?}",
            ui.asked
        );
        assert!(
            partials(&d.path).is_empty(),
            "{denied}: {:?}",
            partials(&d.path)
        );
        assert!(walk(&dst).is_empty(), "{denied}: {:?}", walk(&dst));
        close(&r);
    }
    // The temporary name cannot be removed after the commit.
    let dst = d.join("dst-remove");
    std::fs::create_dir(&dst).unwrap();
    let (r, _lost) = server(&d.path, &["-P", "remove"]);
    let rep = upload(
        &r,
        &src,
        &["g"],
        &dst,
        false,
        &Sys::default(),
        &mut Script::silent(),
    );
    assert_eq!((rep.done, rep.failed), (1, 0), "{rep:?}");
    assert_eq!(std::fs::read(dst.join("g")).unwrap(), b"g");
    let left = partials(&dst);
    assert_eq!(left.len(), 1, "{left:?}");
    let name = left[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        rep.notes
            .iter()
            .any(|n| n.contains(&name) && n.contains("not removed")),
        "{:?}",
        rep.notes
    );
    close(&r);
}

/// A cancel during an upload stops at once: the file in flight leaves no temporary name,
/// and the session stays usable.
#[test]
fn a_sf_7_cancel_leaves_no_temporary_name() {
    use std::sync::atomic::{AtomicBool, Ordering};
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-cancel");
    let src = d.join("src");
    let dst = d.join("dst");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    write(&src.join("big"), &noise(8 << 20, 1));
    for direct in [false, true] {
        let extra: &[&str] = if direct { &["-P", "hardlink"] } else { &[] };
        let cancel = Arc::new(AtomicBool::new(false));
        let c = cancel.clone();
        let mut writes = 0;
        let (r, _lost, _log) = tapped(&d.path, extra, move |p| {
            if matches!(p, Packet::Write { .. }) {
                writes += 1;
                if writes == 3 {
                    c.store(true, Ordering::SeqCst);
                }
            }
            true
        });
        let sys = Sys::new(cancel);
        let rep = upload(&r, &src, &["big"], &dst, false, &sys, &mut Script::silent());
        assert!(rep.cancelled, "{rep:?}");
        assert!(walk(&dst).is_empty(), "direct={direct}: {:?}", walk(&dst));
        assert!(r.session().lost().is_none(), "the session is usable");
        let rep = upload(
            &r,
            &src,
            &["big"],
            &dst,
            false,
            &Sys::default(),
            &mut Script::silent(),
        );
        assert_eq!(rep.done, 1, "{rep:?}");
        std::fs::remove_file(dst.join("big")).unwrap();
        close(&r);
    }
}

/// A symlink is uploaded as a symlink (R-3), in the right direction: the destination name
/// holds a link whose target equals the source's target text, and nothing is made at the
/// path that the target text names (OpenSSH takes `SYMLINK`'s paths in reverse order).
/// An absolute target and a dangling one stay byte-identical.
#[test]
fn a_sf_7_symlinks_are_uploaded_as_symlinks_in_the_right_direction() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-symlink");
    let src = d.join("src");
    let dst = d.join("dst");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    std::os::unix::fs::symlink("target.txt", src.join("rel")).unwrap();
    std::os::unix::fs::symlink("/nonexistent/abs", src.join("abs")).unwrap();
    write(&src.join("target.txt"), b"local target");
    let (r, _lost) = server(&d.path, &[]);
    let rep = upload(
        &r,
        &src,
        &["rel", "abs"],
        &dst,
        false,
        &Sys::default(),
        &mut Script::silent(),
    );
    assert_eq!(rep.done, 2, "{rep:?}");
    assert_eq!(
        std::fs::read_link(dst.join("rel")).unwrap(),
        Path::new("target.txt")
    );
    assert_eq!(
        std::fs::read_link(dst.join("abs")).unwrap(),
        Path::new("/nonexistent/abs")
    );
    assert!(std::fs::symlink_metadata(dst.join("target.txt")).is_err());
    assert_eq!(walk(&dst).len(), 2, "{:?}", walk(&dst));
    // The local links are untouched.
    assert_eq!(
        std::fs::read_link(src.join("rel")).unwrap(),
        Path::new("target.txt")
    );
    // A link whose name is taken raises "file exists"; Overwrite replaces it through
    // `posix-rename@openssh.com`.
    std::os::unix::fs::symlink("old", dst.join("taken")).unwrap();
    std::os::unix::fs::symlink("new", src.join("taken")).unwrap();
    let mut ui = Script::new([Answer::Overwrite]);
    let rep = upload(&r, &src, &["taken"], &dst, false, &Sys::default(), &mut ui);
    assert_eq!(rep.done, 1, "{rep:?}");
    assert!(matches!(ui.asked[..], [Question::FileExists { .. }]));
    assert_eq!(
        std::fs::read_link(dst.join("taken")).unwrap(),
        Path::new("new")
    );
    assert!(partials(&dst).is_empty());
    close(&r);
}

// ---- A-SF-7: round trips per file and the batches' lost sessions ---------------------------

/// A provider over `sftp-server` behind the rounds tap ([`common::sftp::sftp_server_in_rounds`])
/// with sftp(1)'s default request sizes, so no `limits@openssh.com` request.
fn in_rounds(dir: &Path, extra: &[&str]) -> (Arc<RemoteProvider>, common::sftp::Rounds) {
    let (s, _lost, rounds) = common::sftp::sftp_server_in_rounds(dir, extra, |_| {});
    assert!(s.set_sizes(manycommander::remote::session::Sizes::default()));
    (Arc::new(RemoteProvider::new(s, target("srv"))), rounds)
}

/// The rounds of a log as request names, from round `from` on; the log is emptied.
fn take_rounds(rounds: &common::sftp::Rounds) -> Vec<Vec<String>> {
    let mut r = rounds.lock().unwrap();
    let names = r.iter().map(|x| common::sftp::round_names(x)).collect();
    r.clear();
    names
}

fn names(v: &[&[&str]]) -> Vec<Vec<String>> {
    v.iter()
        .map(|r| r.iter().map(|s| s.to_string()).collect())
        .collect()
}

/// P3 5.6: a small file uploads in three round trips: the `OPEN` of the temporary name, one
/// batch of the `WRITE`, the `FSETSTAT` and the `CLOSE`, and one batch of the hard link and
/// the `REMOVE` of the temporary name. A move adds `fsync@openssh.com` to the first batch
/// (R-4); direct-write mode takes two round trips; an overwrite after "file exists" three,
/// its commit `posix-rename@openssh.com` alone. The tap holds every reply until the client
/// has sent nothing for 50 ms, so each round is what the client sent without waiting.
/// Before the batches, an upload took six rounds a file.
#[test]
fn a_sf_7_a_small_file_uploads_in_three_round_trips() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-rounds");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    for (k, n) in ["a", "b", "taken"].iter().enumerate() {
        write(&src.join(n), &noise(4096, k as u64));
    }
    let file: &[&[&str]] = &[
        &["open"],
        &["write", "fsetstat", "close"],
        &["hardlink@openssh.com", "remove"],
    ];
    // A copy of two files.
    let dst = d.join("copy");
    std::fs::create_dir(&dst).unwrap();
    let (r, rounds) = in_rounds(&d.path, &[]);
    let rep = upload(
        &r,
        &src,
        &["a", "b"],
        &dst,
        false,
        &Sys::default(),
        &mut Script::silent(),
    );
    assert_eq!((rep.done, rep.failed), (2, 0), "{rep:?}");
    let mut want = names(&[&["stat"]]);
    want.extend(names(file));
    want.extend(names(file));
    assert_eq!(
        take_rounds(&rounds),
        want,
        "the destination's STAT, then three rounds a file"
    );
    for n in ["a", "b"] {
        assert!(std::fs::read(src.join(n)).unwrap() == std::fs::read(dst.join(n)).unwrap());
    }
    // An overwrite: the commit meets the name, the question, then the upload again, whose
    // commit replaces atomically.
    write(&dst.join("taken"), b"old");
    let mut ui = Script::new([Answer::Overwrite]);
    let rep = upload(&r, &src, &["taken"], &dst, false, &Sys::default(), &mut ui);
    assert_eq!(rep.done, 1, "{rep:?}");
    assert!(matches!(ui.asked[..], [Question::FileExists { .. }]));
    let mut want = names(&[&["stat"]]);
    want.extend(names(file));
    // The commit's `LSTAT` finds the name; the question's own `LSTAT` reads its metadata
    // (M1's conflict flow, unchanged).
    want.extend(names(&[
        &["lstat"],
        &["lstat"],
        &["open"],
        &["write", "fsetstat", "close"],
        &["posix-rename@openssh.com"],
    ]));
    assert_eq!(take_rounds(&rounds), want);
    assert!(std::fs::read(dst.join("taken")).unwrap() == std::fs::read(src.join("taken")).unwrap());
    // A move: the sync joins the first batch, before the commit.
    let moved = d.join("moved");
    std::fs::create_dir(&moved).unwrap();
    let rep = upload(
        &r,
        &src,
        &["a"],
        &moved,
        true,
        &Sys::default(),
        &mut Script::silent(),
    );
    assert_eq!((rep.done, rep.failed), (1, 0), "{rep:?}");
    assert!(!src.join("a").exists());
    assert_eq!(
        take_rounds(&rounds),
        names(&[
            &["stat"],
            &["open"],
            &["write", "fsetstat", "fsync@openssh.com", "close"],
            &["hardlink@openssh.com", "remove"],
        ])
    );
    assert!(partials(&d.path).is_empty());
    close(&r);
    // Direct-write mode: the OPEN of the final name, then the batch.
    let direct = d.join("direct");
    std::fs::create_dir(&direct).unwrap();
    let (r, rounds) = in_rounds(&d.path, &["-P", "hardlink"]);
    let rep = upload(
        &r,
        &src,
        &["b"],
        &direct,
        false,
        &Sys::default(),
        &mut Script::silent(),
    );
    assert_eq!((rep.done, rep.failed), (1, 0), "{rep:?}");
    assert_eq!(
        take_rounds(&rounds),
        names(&[&["stat"], &["open"], &["write", "fsetstat", "close"]])
    );
    close(&r);
}

/// P3 5.6 at a 30 ms round trip (the latency helper, 15 ms each way): each small file of an
/// upload takes about three round trips, not six. Measured per file as the difference
/// between a job of eleven files and a job of one, over ten files, against the round trip
/// of an `LSTAT` on the same session.
#[test]
fn a_sf_7_at_a_30_ms_round_trip_a_small_file_takes_three_round_trips() {
    use common::sftp::sftp_server_with_latency;
    use std::sync::atomic::AtomicBool;
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-rtt30");
    let src = d.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let files: Vec<String> = (0..11).map(|k| format!("f{k:02}")).collect();
    for (k, n) in files.iter().enumerate() {
        write(&src.join(n), &noise(4096, k as u64));
    }
    let (s, _lost) = sftp_server_with_latency(&d.path, Duration::from_millis(15));
    assert!(s.set_sizes(manycommander::remote::session::Sizes::default()));
    let never = AtomicBool::new(false);
    let mut rtt: Vec<Duration> = (0..5)
        .map(|_| {
            let t = Instant::now();
            s.lstat(&bytes(&src), &never).unwrap();
            t.elapsed()
        })
        .collect();
    rtt.sort();
    let rtt = rtt[2];
    let r = Arc::new(RemoteProvider::new(s, target("srv")));
    let refs: Vec<&str> = files.iter().map(String::as_str).collect();
    let mut took = Vec::new();
    for (dst, names) in [("one", &refs[..1]), ("eleven", &refs[..])] {
        let dst = d.join(dst);
        std::fs::create_dir(&dst).unwrap();
        let t = Instant::now();
        let rep = upload(
            &r,
            &src,
            names,
            &dst,
            false,
            &Sys::default(),
            &mut Script::silent(),
        );
        took.push(t.elapsed());
        assert_eq!((rep.done, rep.failed), (names.len() as u64, 0), "{rep:?}");
    }
    let per_file = took[1].saturating_sub(took[0]) / 10;
    let trips = per_file.as_secs_f64() / rtt.as_secs_f64();
    eprintln!(
        "upload at a {rtt:?} round trip: {per_file:?} a file = {trips:.2} round trips \
         (one file {:?}, eleven {:?})",
        took[0], took[1]
    );
    assert!(rtt >= Duration::from_millis(30), "{rtt:?}");
    assert!((2.5..4.5).contains(&trips), "{trips:.2} round trips a file");
    close(&r);
}

/// R-1 with the batch: the `CLOSE` goes out behind the `WRITE` and the `FSETSTAT`, and its
/// reply is checked before the commit. A server that refuses the `CLOSE` (`-P close`)
/// fails the file through the error question ("close: permission denied"); no hard link
/// is ever sent, the temporary name is removed, and the destination stays empty.
#[test]
fn a_sf_7_a_failed_close_prevents_the_commit() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-close-fails");
    let src = d.join("src");
    let dst = d.join("dst");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    write(&src.join("f"), &noise(4096, 1));
    let (r, rounds) = in_rounds(&d.path, &["-P", "close"]);
    let mut ui = Script::new([Answer::Skip]);
    let rep = upload(&r, &src, &["f"], &dst, false, &Sys::default(), &mut ui);
    assert_eq!((rep.done, rep.failed), (0, 1), "{rep:?}");
    assert!(
        matches!(&ui.asked[..], [Question::ServerError { op: "close", message, .. }] if message == "permission denied"),
        "{:?}",
        ui.asked
    );
    assert_eq!(
        take_rounds(&rounds),
        names(&[
            &["stat"],
            &["open"],
            &["write", "fsetstat", "close"],
            // The CLOSE went out, so the handle is gone: only the temporary name is removed.
            &["remove"],
        ])
    );
    assert!(walk(&dst).is_empty(), "{:?}", walk(&dst));
    close(&r);
}

/// R-1 and E-25 with the batch: the session ends after every `WRITE` and the `FSETSTAT`
/// were answered, before the `CLOSE`'s reply (the tap waits for the server's replies, then
/// drops the link at the `CLOSE`). In direct-write mode the last byte and the metadata are
/// written, so the file counts as committed for a copy, and no path is named; for a move it
/// fails, its local source stays, and the final path is named as possibly partial. In the
/// hard-link mode the temporary name is named, and the final name was never made.
#[test]
fn a_sf_7_a_session_lost_at_the_close_after_the_data_and_the_metadata() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-lost-close");
    for (case, extra, moving) in [
        ("direct", &["-P", "hardlink"][..], false),
        ("direct-move", &["-P", "hardlink"][..], true),
        ("link", &[][..], false),
    ] {
        let src = d.join(format!("src-{case}"));
        let dst = d.join(format!("dst-{case}"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let data = noise(4096, 2);
        write(&src.join("f"), &data);
        write(&src.join("later"), b"later");
        let (r, lost, log) = tapped(&d.path, extra, |p| {
            if matches!(p, Packet::Close { .. }) {
                // The replies to the WRITE and the FSETSTAT reach the session first.
                std::thread::sleep(Duration::from_millis(300));
                return false;
            }
            true
        });
        let rep = upload(
            &r,
            &src,
            &["f", "later"],
            &dst,
            moving,
            &Sys::default(),
            &mut Script::silent(),
        );
        assert!(lost.recv_timeout(T).is_ok(), "{case}: the loss is reported");
        {
            let log = log.lock().unwrap();
            let fsetstat = log
                .iter()
                .position(|p| matches!(p, Packet::Fsetstat { .. }));
            let close = log.iter().position(|p| matches!(p, Packet::Close { .. }));
            assert!(fsetstat.is_some() && close > fsetstat, "{case}: {log:?}");
        }
        let final_named = rep
            .notes
            .iter()
            .any(|n| n.contains("/f: ") && n.contains(MAY_BE_PARTIAL));
        match case {
            "direct" => {
                assert_eq!((rep.done, rep.failed), (1, 1), "{case}: {rep:?}");
                assert_eq!(issues(&rep), [("later".into(), "connection lost".into())]);
                assert!(!final_named, "{case}: {:?}", rep.notes);
                assert!(std::fs::read(dst.join("f")).unwrap() == data);
            }
            "direct-move" => {
                assert_eq!((rep.done, rep.failed), (0, 2), "{case}: {rep:?}");
                assert!(final_named, "{case}: {:?}", rep.notes);
                assert!(src.join("f").exists(), "{case}: the source stays");
            }
            _ => {
                assert_eq!((rep.done, rep.failed), (0, 2), "{case}: {rep:?}");
                let left = partials(&dst);
                assert_eq!(left.len(), 1, "{left:?}");
                let named = format!("sftp://srv{}: {MAY_BE_PARTIAL}", left[0].display());
                assert!(rep.notes.contains(&named), "{case}: {:?}", rep.notes);
                assert!(!dst.join("f").exists());
            }
        }
        close(&r);
    }
}

/// R-1 with the commit batch: the hard link and the `REMOVE` of the temporary name go out
/// together. When the session ends after the link was answered, before the `REMOVE`'s
/// reply, the file is committed and complete, and the report names the temporary name as
/// not removed. When it ends before the link reached the server, the commit's outcome is
/// unknown to the job: the entry fails with "connection lost during the commit", the
/// temporary name is named, and the final name was never made.
#[test]
fn a_sf_7_a_session_lost_in_the_commit_batch() {
    use manycommander::remote::put::{LOST_AT_COMMIT, NOT_REMOVED};
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-lost-commit");
    for at_link in [false, true] {
        let src = d.join(format!("src-{at_link}"));
        let dst = d.join(format!("dst-{at_link}"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let data = noise(4096, 5);
        write(&src.join("f"), &data);
        let (r, _lost, _log) = tapped(&d.path, &[], move |p| match p {
            Packet::Extended { name, .. } if name == b"hardlink@openssh.com" => !at_link,
            Packet::Remove { .. } => {
                // The link's reply reaches the session first.
                std::thread::sleep(Duration::from_millis(300));
                false
            }
            _ => true,
        });
        let rep = upload(
            &r,
            &src,
            &["f"],
            &dst,
            false,
            &Sys::default(),
            &mut Script::silent(),
        );
        let left = partials(&dst);
        assert_eq!(left.len(), 1, "{at_link}: {left:?}");
        let temp = format!("sftp://srv{}", left[0].display());
        if at_link {
            assert_eq!(
                issues(&rep),
                [("f".into(), LOST_AT_COMMIT.into())],
                "{rep:?}"
            );
            assert!(
                rep.notes.contains(&format!("{temp}: {MAY_BE_PARTIAL}")),
                "{:?}",
                rep.notes
            );
            assert!(!dst.join("f").exists());
        } else {
            assert_eq!((rep.done, rep.failed), (1, 0), "{rep:?}");
            assert!(
                rep.notes
                    .contains(&format!("{temp}: {NOT_REMOVED}: connection lost")),
                "{:?}",
                rep.notes
            );
            assert!(std::fs::read(dst.join("f")).unwrap() == data);
        }
        close(&r);
    }
}

// ---- A-SF-8: the other verbs of 3b -------------------------------------------------------

/// F5 into a remote panel uploads into its directory; F7 makes directories on the server
/// (`a/b/c` makes the parents, an existing name is reported and the cursor moves to it).
#[test]
fn a_sf_8_f5_upload_and_f7_through_the_app() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-app-f7");
    let local = d.join("local");
    let srv = d.join("srv");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::create_dir_all(&srv).unwrap();
    write(&local.join("up"), b"uploaded");
    let (r, _lost) = server(&d.path, &[]);
    let mut a = app_with(&[&r], &local);
    press(&mut a, KeyCode::Tab, NONE);
    cd_remote(&mut a, "srv", &srv);
    press(&mut a, KeyCode::Tab, NONE);
    a.panel_mut().cursor_to_name(b"up");
    // F5: the upload dialog, filled with the server directory.
    assert!(press(&mut a, KeyCode::F(5), NONE).is_empty());
    let (title, lines) = dialog_text(&a);
    assert_eq!(title, "Upload");
    assert!(lines[0].contains("sftp://srv"), "{lines:?}");
    let spec = started(&press(&mut a, KeyCode::Enter, NONE));
    let rep = finish(&mut a, spec, &mut Script::silent());
    assert_eq!(rep.done, 1, "{rep:?}");
    assert_eq!(std::fs::read(srv.join("up")).unwrap(), b"uploaded");
    // F7 in the remote panel.
    press(&mut a, KeyCode::Tab, NONE);
    assert!(press(&mut a, KeyCode::F(7), NONE).is_empty());
    type_in(&mut a, "x/y/z");
    let spec = started(&press(&mut a, KeyCode::Enter, NONE));
    assert!(matches!(spec, JobSpec::MkdirRemote { .. }), "{spec:?}");
    let rep = finish(&mut a, spec, &mut Script::silent());
    assert_eq!(rep.done, 1, "{rep:?}");
    assert!(srv.join("x/y/z").is_dir());
    assert_eq!(a.panel().cursor_name(), Some(&b"x"[..]));
    // Again: reported as existing.
    press(&mut a, KeyCode::F(7), NONE);
    type_in(&mut a, "x/y/z");
    let spec = started(&press(&mut a, KeyCode::Enter, NONE));
    let rep = finish(&mut a, spec, &mut Script::silent());
    assert_eq!(issues(&rep), [("z".into(), "already exists".into())]);
    // A component that is a file.
    press(&mut a, KeyCode::F(7), NONE);
    type_in(&mut a, "up/q");
    let spec = started(&press(&mut a, KeyCode::Enter, NONE));
    let rep = finish(&mut a, spec, &mut Script::silent());
    assert_eq!(rep.failed, 1, "{rep:?}");
    assert!(
        issues(&rep)[0].1.contains("exists and is not a directory"),
        "{rep:?}"
    );
    close(&r);
}

/// Shift+F6 and F6 within one session (P3 5.6, R-2): the new name is `LSTAT`ed before the
/// `SSH_FXP_RENAME`; a taken name raises the question: Skip keeps both, Overwrite replaces
/// through `posix-rename@openssh.com`, Merge moves each entry into the existing directory
/// and removes the emptied one. Nothing is copied.
#[test]
fn a_sf_8_renames_and_moves_within_one_session() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-rename");
    let local = d.join("local");
    let one = d.join("one");
    let two = d.join("two");
    for p in [&local, &one, &two] {
        std::fs::create_dir_all(p).unwrap();
    }
    write(&one.join("a"), b"a");
    write(&one.join("b"), b"b");
    std::fs::create_dir_all(one.join("dir/inner")).unwrap();
    write(&one.join("dir/x"), b"x");
    write(&one.join("dir/inner/y"), b"y");
    std::fs::create_dir_all(two.join("dir")).unwrap();
    write(&two.join("dir/keep"), b"keep");
    let (r, _lost, log) = tapped(&d.path, &[], |_| true);
    let mut a = app_with(&[&r], &local);
    cd_remote(&mut a, "srv", &one);
    // Shift+F6 to a new name.
    a.panel_mut().cursor_to_name(b"a");
    press(&mut a, KeyCode::F(6), SHIFT);
    type_in(&mut a, "c");
    let spec = started(&press(&mut a, KeyCode::Enter, NONE));
    let rep = finish(&mut a, spec, &mut Script::silent());
    assert_eq!(rep.done, 1, "{rep:?}");
    assert_eq!(std::fs::read(one.join("c")).unwrap(), b"a");
    assert!(!one.join("a").exists());
    {
        let log = log.lock().unwrap();
        let at = log
            .iter()
            .position(|p| matches!(p, Packet::Rename { .. }))
            .expect("a rename");
        let new = bytes(&one.join("c"));
        assert!(
            matches!(&log[at - 1], Packet::Lstat { path, .. } if *path == new),
            "the new name is checked just before: {:?}",
            log[at - 1]
        );
    }
    // Shift+F6 onto a taken name: Skip, then Overwrite.
    a.panel_mut().cursor_to_name(b"c");
    press(&mut a, KeyCode::F(6), SHIFT);
    type_in(&mut a, "b");
    let spec = started(&press(&mut a, KeyCode::Enter, NONE));
    let mut ui = Script::new([Answer::Skip]);
    let rep = finish(&mut a, spec.clone(), &mut ui);
    assert!(matches!(ui.asked[..], [Question::FileExists { .. }]));
    assert_eq!(rep.skipped, 1);
    assert_eq!(std::fs::read(one.join("b")).unwrap(), b"b");
    let mut ui = Script::new([Answer::Overwrite]);
    let rep = finish(&mut a, spec, &mut ui);
    assert_eq!(rep.done, 1, "{rep:?}");
    assert_eq!(std::fs::read(one.join("b")).unwrap(), b"a");
    assert!(!one.join("c").exists());
    assert_eq!(count(&log, "posix-rename@openssh.com"), 1);
    // F6 into the other panel on the same session: a file, and a directory that merges.
    press(&mut a, KeyCode::Tab, NONE);
    cd_remote(&mut a, "srv", &two);
    press(&mut a, KeyCode::Tab, NONE);
    a.panel_mut().cursor_to_name(b"b");
    press(&mut a, KeyCode::Insert, NONE);
    a.panel_mut().cursor_to_name(b"dir");
    press(&mut a, KeyCode::Insert, NONE);
    assert_eq!(a.panel().marked, 2);
    assert!(press(&mut a, KeyCode::F(6), NONE).is_empty());
    let (title, _) = dialog_text(&a);
    assert_eq!(title, "Move");
    let spec = started(&press(&mut a, KeyCode::Enter, NONE));
    let mut ui = Script::new([Answer::Merge]);
    let rep = finish(&mut a, spec, &mut ui);
    assert!(
        matches!(ui.asked[..], [Question::DirExists { .. }]),
        "{:?}",
        ui.asked
    );
    assert_eq!(rep.failed, 0, "{rep:?}");
    assert_eq!(std::fs::read(two.join("b")).unwrap(), b"a");
    assert_eq!(std::fs::read(two.join("dir/x")).unwrap(), b"x");
    assert_eq!(std::fs::read(two.join("dir/inner/y")).unwrap(), b"y");
    assert_eq!(std::fs::read(two.join("dir/keep")).unwrap(), b"keep");
    assert!(!one.join("dir").exists(), "the emptied directory goes");
    assert!(walk(&one).is_empty(), "{:?}", walk(&one));
    assert_eq!(count(&log, "open"), 0, "nothing was copied");
    close(&r);
}

/// Without `posix-rename@openssh.com`, Overwrite on a rename is refused (R-2) and both
/// names stay.
#[test]
fn a_sf_8_a_rename_over_a_name_without_posix_rename_is_refused() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-rename-refused");
    let one = d.join("one");
    std::fs::create_dir_all(&one).unwrap();
    write(&one.join("a"), b"a");
    write(&one.join("b"), b"b");
    let (r, _lost) = server(&d.path, &["-P", "posix-rename"]);
    let spec = JobSpec::Move {
        groups: remote_group(&r, &one, &["a"]),
        dst: to(&r, &one.join("b")),
    };
    let mut ui = Script::new([Answer::Overwrite]);
    let rep = run_guarded(spec, &Sys::default(), &mut ui);
    assert_eq!(issues(&rep), [("a".into(), NO_ATOMIC_REPLACE.into())]);
    assert_eq!(std::fs::read(one.join("a")).unwrap(), b"a");
    assert_eq!(std::fs::read(one.join("b")).unwrap(), b"b");
    close(&r);
}

/// Shift+F8 on a server (R-5, R-3): the typed confirmation shows the scan's counts; the
/// tree goes in post-order; a symlink to a directory outside it is removed as a link, and
/// the outside stays intact. F8 is refused with R-5's message; F5 and F6 between two
/// sessions are refused, and F5 within one session.
#[test]
fn a_sf_8_shift_f8_the_symlink_to_the_outside_and_the_refusals() {
    use manycommander::app::jobs::{NO_REMOTE_TRASH, NO_SERVER_COPY, THROUGH_LOCAL};
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-delete");
    let local = d.join("local");
    let srv = d.join("srv");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::create_dir_all(srv.join("t/sub")).unwrap();
    std::fs::create_dir_all(srv.join("outside")).unwrap();
    write(&srv.join("outside/keep"), b"keep");
    write(&srv.join("t/f"), b"12345");
    write(&srv.join("t/sub/g"), b"12");
    std::os::unix::fs::symlink("../outside", srv.join("t/out")).unwrap();
    let (r, _lost) = server(&d.path, &[]);
    let (r2, _lost2) = server_as(&d.path, &[], "srv2");
    let mut a = app_with(&[&r, &r2], &local);
    cd_remote(&mut a, "srv", &srv);
    a.panel_mut().cursor_to_name(b"t");
    // F8: refused (R-5).
    assert!(press(&mut a, KeyCode::F(8), NONE).is_empty());
    assert_eq!(status(&a), NO_REMOTE_TRASH);
    // Between two sessions, and within one: refused.
    press(&mut a, KeyCode::Tab, NONE);
    cd_remote(&mut a, "srv2", &srv);
    press(&mut a, KeyCode::Tab, NONE);
    for k in [KeyCode::F(5), KeyCode::F(6)] {
        assert!(press(&mut a, k, NONE).is_empty());
        assert_eq!(status(&a), THROUGH_LOCAL, "{k:?}");
    }
    press(&mut a, KeyCode::Tab, NONE);
    cd_remote(&mut a, "srv", &srv.join("outside"));
    press(&mut a, KeyCode::Tab, NONE);
    assert!(press(&mut a, KeyCode::F(5), NONE).is_empty());
    assert_eq!(status(&a), NO_SERVER_COPY);
    // Shift+F8.
    a.panel_mut().cursor_to_name(b"t");
    assert!(press(&mut a, KeyCode::F(8), SHIFT).is_empty());
    let (title, lines) = dialog_text(&a);
    assert_eq!(title, "Delete permanently");
    assert!(lines.iter().any(|l| l.contains("no trash")), "{lines:?}");
    let spec = started(&press(&mut a, KeyCode::Enter, NONE));
    let mut ui = Script::new([Answer::Confirm]);
    let rep = finish(&mut a, spec, &mut ui);
    assert_eq!(
        ui.asked,
        [Question::ConfirmDelete {
            files: 3,
            dirs: 2,
            bytes: 7,
            single: None
        }]
    );
    assert_eq!((rep.done, rep.dirs_done, rep.failed), (3, 2, 0), "{rep:?}");
    assert!(!srv.join("t").exists());
    assert_eq!(std::fs::read(srv.join("outside/keep")).unwrap(), b"keep");
    // A typed confirmation that is not given deletes nothing.
    std::fs::create_dir(srv.join("t2")).unwrap();
    let spec = JobSpec::Delete {
        groups: remote_group(&r, &srv, &["t2"]),
    };
    let rep = run_guarded(spec, &Sys::default(), &mut Script::new([Answer::Cancel]));
    assert!(
        rep.cancelled && rep.notes == ["nothing was deleted"],
        "{rep:?}"
    );
    assert!(srv.join("t2").is_dir());
    close(&r);
    close(&r2);
}

/// A read-only server (`sftp-server -R`) fails every write per entry, through the error
/// question, and leaves nothing behind.
#[test]
fn a_sf_8_a_read_only_server_fails_per_entry() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-readonly");
    let src = d.join("src");
    let dst = d.join("dst");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(dst.join("old")).unwrap();
    for n in ["f", "g", "h"] {
        write(&src.join(n), n.as_bytes());
    }
    write(&dst.join("x"), b"x");
    let (r, _lost) = server(&d.path, &["-R"]);
    let mut ui = Script::new([Answer::Skip, Answer::Skip, Answer::Skip]);
    let rep = upload(
        &r,
        &src,
        &["f", "g", "h"],
        &dst,
        false,
        &Sys::default(),
        &mut ui,
    );
    assert_eq!((rep.done, rep.failed), (0, 3), "{rep:?}");
    assert_eq!(ui.asked.len(), 3, "one question per entry: {:?}", ui.asked);
    assert!(
        ui.asked.iter().all(
            |q| matches!(q, Question::ServerError { message, .. } if message == "permission denied")
        ),
        "{:?}",
        ui.asked
    );
    assert_eq!(walk(&dst).len(), 2, "{:?}", walk(&dst));
    // F7, a rename and a delete.
    let rep = run_guarded(
        JobSpec::MkdirRemote {
            dir: to(&r, &dst),
            name: "new".into(),
        },
        &Sys::default(),
        &mut Script::silent(),
    );
    assert_eq!(rep.failed, 1, "{rep:?}");
    let rep = run_guarded(
        JobSpec::Move {
            groups: remote_group(&r, &dst, &["x"]),
            dst: to(&r, &dst.join("y")),
        },
        &Sys::default(),
        &mut Script::new([Answer::Skip]),
    );
    assert_eq!(rep.failed, 1, "{rep:?}");
    let rep = run_guarded(
        JobSpec::Delete {
            groups: remote_group(&r, &dst, &["x", "old"]),
        },
        &Sys::default(),
        &mut Script::new([Answer::Confirm, Answer::SkipAllErrno]),
    );
    assert_eq!((rep.done, rep.failed), (0, 2), "{rep:?}");
    assert!(dst.join("x").exists() && dst.join("old").is_dir());
    close(&r);
}

// ---- A-SF-9: moves across hosts ---------------------------------------------------------

/// R-4: the confirm dialog of a move across hosts says "best-effort" before the job, on a
/// server with `fsync@openssh.com` and on one without; the dialog of a move out of a
/// server also says that the remote sources are kept.
#[test]
fn a_sf_9_the_confirm_dialog_says_best_effort() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-best-effort");
    let local = d.join("local");
    let srv = d.join("srv");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::create_dir_all(&srv).unwrap();
    write(&local.join("f"), b"f");
    write(&srv.join("g"), b"g");
    for extra in [&[][..], &["-P", "fsync"][..]] {
        let (r, _lost) = server(&d.path, extra);
        assert_eq!(r.session().caps().fsync, extra.is_empty());
        let mut a = app_with(&[&r], &local);
        press(&mut a, KeyCode::Tab, NONE);
        cd_remote(&mut a, "srv", &srv);
        press(&mut a, KeyCode::Tab, NONE);
        a.panel_mut().cursor_to_name(b"f");
        assert!(press(&mut a, KeyCode::F(6), NONE).is_empty());
        let (title, lines) = dialog_text(&a);
        assert_eq!(title, "Move");
        assert!(lines.iter().any(|l| l.contains("best-effort")), "{lines:?}");
        press(&mut a, KeyCode::Esc, NONE);
        // Out of the server.
        press(&mut a, KeyCode::Tab, NONE);
        a.panel_mut().cursor_to_name(b"g");
        assert!(press(&mut a, KeyCode::F(6), NONE).is_empty());
        let (title, lines) = dialog_text(&a);
        assert_eq!(title, "Move");
        assert!(lines.iter().any(|l| l.contains("best-effort")), "{lines:?}");
        assert!(lines.iter().any(|l| l == REMOTE_KEPT), "{lines:?}");
        press(&mut a, KeyCode::Esc, NONE);
        close(&r);
    }
}

/// R-4, upload: every local source goes after its upload was committed (and synced with
/// `fsync@openssh.com`, before the commit); the tree arrives complete; the emptied source
/// directories go. Without the extension the report says "not synced on the server".
#[test]
fn a_sf_9_an_upload_move_removes_the_committed_sources() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-move");
    for fsync in [true, false] {
        let src = d.join(format!("src-{fsync}"));
        let keep = d.join(format!("keep-{fsync}"));
        let dst = d.join(format!("dst-{fsync}"));
        fixture(&src);
        fixture(&keep);
        std::fs::create_dir(&dst).unwrap();
        let extra: &[&str] = if fsync { &[] } else { &["-P", "fsync"] };
        let (r, _lost, log) = tapped(&d.path, extra, |_| true);
        let rep = upload(
            &r,
            &src,
            FIXTURE,
            &dst,
            true,
            &Sys::default(),
            &mut Script::silent(),
        );
        assert_eq!((rep.done, rep.dirs_done, rep.failed), (7, 2, 0), "{rep:?}");
        // The sources are gone: `keep` is their twin, alike because `fixture` stamps every
        // entry.
        same_tree(&keep, &dst);
        assert_eq!(no_write_after_times(&log), 6);
        assert!(walk(&src).is_empty(), "{:?}", walk(&src));
        assert!(src.is_dir(), "the panel's directory itself stays");
        assert!(partials(&d.path).is_empty());
        if fsync {
            assert!(rep.notes.is_empty(), "{rep:?}");
            // Each file is synced before its commit.
            let log = log.lock().unwrap();
            let name = |p: &Packet| match p {
                Packet::Extended { name, .. } => name.clone(),
                _ => Vec::new(),
            };
            let order: Vec<Vec<u8>> = log
                .iter()
                .map(name)
                .filter(|n| n.starts_with(b"fsync") || n.starts_with(b"hardlink"))
                .collect();
            assert_eq!(order.len(), 12, "{order:?}");
            for pair in order.chunks(2) {
                assert!(pair[0].starts_with(b"fsync") && pair[1].starts_with(b"hardlink"));
            }
        } else {
            assert_eq!(rep.notes, [NOT_SYNCED.to_string()], "{rep:?}");
            assert_eq!(count(&log, "fsync@openssh.com"), 0);
        }
        close(&r);
    }
}

/// R-4, upload: a local source that changed after it was read is kept, and its upload is
/// not committed.
#[test]
fn a_sf_9_a_source_that_changes_during_the_move_is_kept() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-move-changed");
    let src = d.join("src");
    let dst = d.join("dst");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    write(&src.join("f"), &noise(2 << 20, 1));
    let f = src.join("f");
    let mut writes = 0;
    let (r, _lost, _log) = tapped(&d.path, &[], move |p| {
        if matches!(p, Packet::Write { .. }) {
            writes += 1;
            if writes == 2 {
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&f)
                    .and_then(|mut h| std::io::Write::write_all(&mut h, b"more"))
                    .unwrap();
            }
        }
        true
    });
    let rep = upload(
        &r,
        &src,
        &["f"],
        &dst,
        true,
        &Sys::default(),
        &mut Script::silent(),
    );
    assert_eq!(
        issues(&rep),
        [("f".into(), "source changed during move; source kept".into())]
    );
    assert!(src.join("f").exists());
    assert!(walk(&dst).is_empty(), "{:?}", walk(&dst));
    close(&r);
}

/// R-4, download: F6 out of a server copies and syncs, keeps every remote source, and says
/// so in the report, which is a copy's.
#[test]
fn a_sf_9_a_download_move_keeps_the_remote_sources() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-dlmove");
    let srv = d.join("srv");
    let dst = d.join("dst");
    fixture(&srv);
    std::fs::create_dir(&dst).unwrap();
    let (r, _lost) = server(&d.path, &[]);
    let spec = JobSpec::Move {
        groups: remote_group(&r, &srv, FIXTURE),
        dst: dst.clone().into(),
    };
    let rep = run_guarded(spec, &Sys::default(), &mut Script::silent());
    assert_eq!(rep.verb, JobVerb::Copy, "{rep:?}");
    assert_eq!((rep.done, rep.dirs_done, rep.failed), (7, 2, 0), "{rep:?}");
    assert_eq!(rep.notes, [REMOTE_KEPT.to_string()]);
    assert!(
        rep.summary().starts_with("copy: 7 copied"),
        "{}",
        rep.summary()
    );
    same_tree(&srv, &dst);
    for n in FIXTURE {
        assert!(std::fs::symlink_metadata(srv.join(n)).is_ok(), "{n} kept");
    }
    close(&r);
}

// ---- A-SF-10: the F4 write-back --------------------------------------------------------

/// P3 5.6: after the editor, an unchanged copy uploads nothing and asks nothing; a changed
/// one asks, and Upload replaces the server's file through `posix-rename@openssh.com` (a
/// second hard link keeps the old bytes); when the server's file changed since the
/// download, the question also offers "save as `name (1)`", which leaves that file as it
/// is; Keep says where the copy is. Without `posix-rename@openssh.com` the upload is
/// refused and the report names the local copy.
#[test]
fn a_sf_10_the_write_back_question() {
    use manycommander::viewtemp::{self, Roots, ViewMsg};
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-back");
    let srv = d.join("srv");
    let local = d.join("local");
    let rt = d.join("rt");
    std::fs::create_dir_all(&srv).unwrap();
    std::fs::create_dir_all(&local).unwrap();
    std::fs::create_dir_all(&rt).unwrap();
    std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o700)).unwrap();
    let roots = Roots::new(Some(rt.clone()), d.join("tmp"));
    write(&srv.join("notes.txt"), b"server notes\n");
    std::fs::hard_link(srv.join("notes.txt"), srv.join("notes.old")).unwrap();

    /// F4 on `name`, the copy prepared, the editor run: `edit` rewrites the copy (by
    /// rename, as many editors do), `server` changes the server's file meanwhile. Returns
    /// the message of the check.
    fn edit_once(
        a: &mut App,
        roots: &Roots,
        name: &[u8],
        edit: Option<&[u8]>,
        server: Option<&Path>,
    ) -> (ViewMsg, PathBuf) {
        a.panel_mut().cursor_to_name(name);
        let fx = press(a, KeyCode::F(4), NONE);
        let [Effect::PrepareView(req, alive)] = &fx[..] else {
            panic!("{fx:?}")
        };
        let file = viewtemp::prepare(roots, req, &|_, _| {}).unwrap();
        alive.finish();
        assert!(file.stamp.is_some());
        let fx = a.update(Event::View(ViewMsg::Ready {
            id: req.id,
            file: file.clone(),
        }));
        assert!(matches!(&fx[..], [Effect::Run(_)]), "{fx:?}");
        if let Some(p) = server {
            write(p, b"changed on the server, longer\n");
        }
        if let Some(text) = edit {
            let tmp = file.dir.join(".edit");
            write(&tmp, text);
            std::fs::rename(&tmp, file.path()).unwrap();
        }
        let fx = a.update(Event::ChildDone {
            status: String::new(),
            output: None,
        });
        // The panels re-read, as after any hand-off.
        let fx = run(a, fx);
        let Some(Effect::CheckEdited(f, at)) =
            fx.iter().find(|e| matches!(e, Effect::CheckEdited(..)))
        else {
            panic!("{fx:?}")
        };
        (viewtemp::check_edited(roots, f, at.clone()), file.path())
    }

    let (r, _lost) = server(&d.path, &[]);
    let mut a = app_with(&[&r], &local);
    cd_remote(&mut a, "srv", &srv);
    // Unchanged: nothing is asked or uploaded, and the copy goes.
    let (m, copy) = edit_once(&mut a, &roots, b"notes.txt", None, None);
    assert!(matches!(m, ViewMsg::Checked { kept: None, .. }), "{m:?}");
    assert!(a.update(Event::View(m)).is_empty());
    assert!(a.dialog.is_none() && a.job.is_none());
    assert!(!copy.exists());
    // Changed: Upload.
    let (m, copy) = edit_once(&mut a, &roots, b"notes.txt", Some(b"edited\n"), None);
    assert!(matches!(m, ViewMsg::Edited { changed: false, .. }), "{m:?}");
    a.update(Event::View(m));
    let (title, lines) = dialog_text(&a);
    assert_eq!(title, "Edited copy");
    assert!(lines.iter().any(|l| l == "Upload"), "{lines:?}");
    assert!(!lines.iter().any(|l| l.starts_with("Save as")), "{lines:?}");
    let spec = started(&press(&mut a, KeyCode::Enter, NONE));
    assert!(matches!(spec, JobSpec::WriteBack { .. }), "{spec:?}");
    let rep = finish(&mut a, spec, &mut Script::silent());
    assert_eq!(rep.done, 1, "{rep:?}");
    assert_eq!(std::fs::read(srv.join("notes.txt")).unwrap(), b"edited\n");
    assert_eq!(
        std::fs::read(srv.join("notes.old")).unwrap(),
        b"server notes\n"
    );
    assert!(copy.exists(), "the edited copy stays where it was");
    // Changed on both sides: "save as" is offered and keeps the server's file.
    let (m, _) = edit_once(
        &mut a,
        &roots,
        b"notes.txt",
        Some(b"mine\n"),
        Some(&srv.join("notes.txt")),
    );
    assert!(matches!(m, ViewMsg::Edited { changed: true, .. }), "{m:?}");
    a.update(Event::View(m));
    let (_, lines) = dialog_text(&a);
    assert!(
        lines.iter().any(|l| l == "Save as \"notes (1).txt\""),
        "{lines:?}"
    );
    press(&mut a, KeyCode::Right, NONE);
    let spec = started(&press(&mut a, KeyCode::Enter, NONE));
    let rep = finish(&mut a, spec, &mut Script::silent());
    assert_eq!(rep.done, 1, "{rep:?}");
    assert_eq!(std::fs::read(srv.join("notes (1).txt")).unwrap(), b"mine\n");
    assert_eq!(
        std::fs::read(srv.join("notes.txt")).unwrap(),
        b"changed on the server, longer\n"
    );
    // Keep: the status line says where the copy is.
    let (m, copy) = edit_once(&mut a, &roots, b"notes.txt", Some(b"kept\n"), None);
    a.update(Event::View(m));
    press(&mut a, KeyCode::Esc, NONE);
    assert!(a.dialog.is_none() && a.job.is_none());
    assert!(
        status(&a).starts_with("not uploaded to the server; your edited copy is at"),
        "{}",
        status(&a)
    );
    assert_eq!(std::fs::read(&copy).unwrap(), b"kept\n");
    close(&r);

    // Without posix-rename: refused, and the report names the local copy.
    let (r, _lost) = server(&d.path, &["-P", "posix-rename"]);
    let rep = run_guarded(
        JobSpec::WriteBack {
            copy: copy.clone(),
            dst: to(&r, &srv.join("notes.txt")),
        },
        &Sys::default(),
        &mut Script::silent(),
    );
    assert_eq!(rep.failed, 1, "{rep:?}");
    assert_eq!(rep.issues[0].path, copy);
    assert_eq!(
        rep.issues[0].outcome,
        Outcome::Failed(NO_ATOMIC_REPLACE.into())
    );
    assert_eq!(
        std::fs::read(srv.join("notes.txt")).unwrap(),
        b"changed on the server, longer\n"
    );
    close(&r);
}

// ---- A-SF-9: the failpoint sweep ----------------------------------------------------------

#[cfg(feature = "failpoints")]
mod sweep {
    use super::*;
    use manycommander::fsops::failpoints::{Action, Failpoints, Trigger};
    use rustix::io::Errno;
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicBool;

    /// What a sweep run injects.
    #[derive(Clone, Copy, Debug)]
    enum Fault {
        Error,
        Cancel,
        /// The server killed before the step's call: a lost session.
        Loss,
    }

    /// A smaller tree: files that take one, three and no `WRITE`, a directory, a symlink.
    fn small(src: &Path) {
        std::fs::create_dir_all(src.join("sub")).unwrap();
        write(&src.join("a"), b"");
        write(&src.join("b"), &noise(70_000, 2));
        write(&src.join("c"), &noise(600_000, 3));
        write(&src.join("sub/e"), b"e");
        std::os::unix::fs::symlink("b", src.join("link")).unwrap();
    }

    const SMALL: &[&str] = &["a", "b", "c", "sub", "link"];

    /// Every regular file and symlink below `src`, by its relative path.
    fn contents(src: &Path) -> BTreeMap<PathBuf, Result<Vec<u8>, PathBuf>> {
        walk(src)
            .into_iter()
            .filter_map(|p| {
                let m = std::fs::symlink_metadata(&p).ok()?;
                let rel = p.strip_prefix(src).unwrap().to_path_buf();
                if m.file_type().is_symlink() {
                    Some((rel, Err(std::fs::read_link(&p).unwrap())))
                } else if m.is_file() {
                    Some((rel, Ok(std::fs::read(&p).unwrap())))
                } else {
                    None
                }
            })
            .collect()
    }

    /// One upload (or upload move) of the small tree with `fault` at the `n`th hit of
    /// `step`, and the A-SF-9 predicate: no crash; no `.mc-partial-` name unless the report
    /// names it; every final name on the server complete (R-1: no partial file in the
    /// hard-link mode); every local source that is gone is complete on the server; every
    /// planned entry reported, unless the job was cancelled (I-7).
    fn one(d: &Path, moving: bool, step: &str, n: u64, fault: Fault) {
        let tag = format!("{}-{step}-{n}-{fault:?}", moving as u8);
        let src = d.join(format!("src-{tag}"));
        let dst = d.join(format!("dst-{tag}"));
        small(&src);
        std::fs::create_dir(&dst).unwrap();
        let before = contents(&src);
        let (r, _lost) = server(d, &[]);
        let fp = Failpoints::new();
        let cancel = Arc::new(AtomicBool::new(false));
        let pid = r.session().pid().unwrap();
        let action = match fault {
            Fault::Error => Action::Errno(Errno::IO),
            Fault::Cancel => Action::Cancel,
            Fault::Loss => Action::Call(Arc::new(move || {
                kill(pid);
                // The session notices before the call is made.
                std::thread::sleep(Duration::from_millis(20));
            })),
        };
        fp.arm(step, Trigger::Nth(n), action);
        let sys = Sys::with_failpoints(cancel, fp.clone());
        let mut ui = Script::new([]);
        ui.fallback = Answer::Skip;
        let rep = upload(&r, &src, SMALL, &dst, moving, &sys, &mut ui);
        let ctx = format!("{tag}: {rep:?}");
        assert!(fp.hits(step) >= n, "{ctx}: the step was not reached");
        assert!(
            !issues(&rep)
                .iter()
                .any(|(_, w)| w.contains("internal error")),
            "{ctx}"
        );
        for p in partials(&dst) {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                rep.notes.iter().any(|n| n.contains(&name)),
                "{ctx}: {p:?} is not in the report"
            );
        }
        for (rel, was) in &before {
            let there = dst.join(rel);
            let gone = std::fs::symlink_metadata(src.join(rel)).is_err();
            match was {
                Ok(data) => {
                    if let Ok(got) = std::fs::read(&there) {
                        assert!(got == *data, "{ctx}: {rel:?} is partial on the server");
                    } else {
                        assert!(!gone, "{ctx}: {rel:?} is gone and not on the server");
                    }
                }
                Err(link) => match std::fs::read_link(&there) {
                    Ok(t) => assert_eq!(t, *link, "{ctx}"),
                    Err(_) => assert!(!gone, "{ctx}: the link {rel:?} is lost"),
                },
            }
        }
        if !rep.cancelled {
            assert_eq!(rep.remaining(), 0, "{ctx}: entries not reported");
        }
        if matches!(fault, Fault::Loss) && step.starts_with("put.") {
            assert!(r.session().lost().is_some(), "{ctx}");
        }
        close(&r);
    }

    /// The steps of an upload, and of a move's local side.
    const STEPS: &[&str] = &[
        "put.open",
        "put.read",
        "put.write",
        "put.setstat",
        "put.fsync",
        "put.close",
        "put.link",
        "put.remove",
        "put.mkdir",
        "put.symlink",
        "put.lstat",
        "move.statx",
        "move.unlink",
        "move.dirstat",
        "move.rmdir",
    ];

    /// A-SF-9: the failpoint sweep over an upload tree and a cross-host move, every step
    /// that the clean run reaches, at its first, a middle and its last hit, with an error, a
    /// cancel and a lost session.
    #[test]
    fn a_sf_9_failpoint_sweep_over_uploads_and_moves() {
        if !have_sftp_server() {
            return;
        }
        let d = test_dir("write-sweep");
        let mut runs = 0;
        for moving in [false, true] {
            let clean = {
                let src = d.join(format!("clean-src-{moving}"));
                let dst = d.join(format!("clean-dst-{moving}"));
                small(&src);
                std::fs::create_dir(&dst).unwrap();
                let (r, _lost) = server(&d.path, &[]);
                let fp = Failpoints::new();
                let sys = Sys::with_failpoints(Arc::new(AtomicBool::new(false)), fp.clone());
                let rep = upload(&r, &src, SMALL, &dst, moving, &sys, &mut Script::silent());
                assert!(rep.issues.is_empty() && rep.notes.is_empty(), "{rep:?}");
                close(&r);
                fp
            };
            for step in STEPS {
                let hits = clean.hits(step);
                if hits == 0 {
                    assert!(
                        !moving && (step.starts_with("move.") || *step == "put.fsync"),
                        "{step} never reached: {:?}",
                        clean.all_hits()
                    );
                    continue;
                }
                let mut at = vec![1, hits.div_ceil(2), hits];
                at.dedup();
                for n in at {
                    for fault in [Fault::Error, Fault::Cancel, Fault::Loss] {
                        one(&d.path, moving, step, n, fault);
                        runs += 1;
                    }
                }
            }
        }
        eprintln!("the sweep ran {runs} injections");
        assert!(runs > 60, "the sweep ran {runs} injections");
    }

    /// A download move under faults on the local side: the remote sources are always kept,
    /// and no temporary name is left in the destination.
    #[test]
    fn a_sf_9_download_moves_keep_their_sources_under_faults() {
        if !have_sftp_server() {
            return;
        }
        let d = test_dir("write-sweep-dl");
        let srv = d.join("srv");
        small(&srv);
        let before = contents(&srv);
        let mut runs = 0;
        for step in ["copy.tmp", "copy.chunk", "commit.rename", "move.syncfs"] {
            for fault in [Fault::Error, Fault::Cancel, Fault::Loss] {
                let dst = d.join(format!("dst-{step}-{fault:?}"));
                std::fs::create_dir(&dst).unwrap();
                let (r, _lost) = server(&d.path, &[]);
                let fp = Failpoints::new();
                let pid = r.session().pid().unwrap();
                let action = match fault {
                    Fault::Error => Action::Errno(Errno::IO),
                    Fault::Cancel => Action::Cancel,
                    Fault::Loss => Action::Call(Arc::new(move || {
                        kill(pid);
                        std::thread::sleep(Duration::from_millis(20));
                    })),
                };
                fp.arm(step, Trigger::Nth(1), action);
                let sys = Sys::with_failpoints(Arc::new(AtomicBool::new(false)), fp.clone());
                let mut ui = Script::new([]);
                ui.fallback = Answer::Skip;
                let rep = run_guarded(
                    JobSpec::Move {
                        groups: remote_group(&r, &srv, SMALL),
                        dst: dst.clone().into(),
                    },
                    &sys,
                    &mut ui,
                );
                let ctx = format!("{step} {fault:?}: {rep:?}");
                assert!(fp.hits(step) >= 1, "{ctx}");
                assert_eq!(contents(&srv), before, "{ctx}: a remote source changed");
                assert!(partials(&dst).is_empty(), "{ctx}");
                close(&r);
                runs += 1;
            }
        }
        assert_eq!(runs, 12);
    }
}

// ---- measurements (release build, run by hand) ---------------------------------------------

/// P-26 by hand: a 256 MiB upload through the app's path (the upload engine) against
/// `sftp -D sftp-server` put. `cargo test --release --test sftp_write -- --ignored
/// --nocapture measure`.
#[test]
#[ignore]
fn measure_upload() {
    use std::io::Write;
    use std::process::Command;
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("write-measure");
    let src = d.join("src");
    std::fs::create_dir(&src).unwrap();
    let n = 256usize << 20;
    write(&src.join("payload"), &noise(n, 1));
    let (r, _lost) = server(&d.path, &[]);
    let mut ours = Duration::MAX;
    for k in 0..3 {
        let dst = d.join(format!("ours{k}"));
        std::fs::create_dir(&dst).unwrap();
        let t = Instant::now();
        let rep = upload(
            &r,
            &src,
            &["payload"],
            &dst,
            false,
            &Sys::default(),
            &mut Script::silent(),
        );
        ours = ours.min(t.elapsed());
        assert_eq!(rep.done, 1, "{rep:?}");
        std::fs::remove_dir_all(&dst).unwrap();
    }
    close(&r);
    let mut theirs = Duration::MAX;
    for _ in 0..3 {
        let out = d.join("theirs");
        let _ = std::fs::remove_file(&out);
        let t = Instant::now();
        let st = Command::new("sftp")
            .arg("-q")
            .arg("-D")
            .arg(common::sftp::SFTP_SERVER)
            .arg("-b")
            .arg("-")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .and_then(|mut c| {
                let mut i = c.stdin.take().unwrap();
                writeln!(i, "put {} {}", src.join("payload").display(), out.display())?;
                drop(i);
                c.wait()
            });
        match st {
            Ok(s) if s.success() => theirs = theirs.min(t.elapsed()),
            other => {
                eprintln!("sftp did not run: {other:?}");
                return;
            }
        }
    }
    let mib = (n >> 20) as f64;
    eprintln!(
        "256 MiB upload: app path {:.0} ms ({:.0} MiB/s), sftp -D {:.0} ms ({:.0} MiB/s), ratio {:.2}",
        ours.as_secs_f64() * 1000.0,
        mib / ours.as_secs_f64(),
        theirs.as_secs_f64() * 1000.0,
        mib / theirs.as_secs_f64(),
        ours.as_secs_f64() / theirs.as_secs_f64()
    );
}
