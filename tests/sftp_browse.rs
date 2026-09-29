//! SFTP 3a in the app (P3 5.4, 5.5, 5.7): A-SF-2 (listings through the provider and the
//! app: a batch per `READDIR` reply, byte-exact names, unsafe names skipped and counted, the
//! symlink pass, the login directory, free space), A-SF-3 (downloads through the local
//! engine: byte-identical files, trees, symlinks, special files, "file exists", cancel with
//! the drain window, a FIFO swap that wedges the server), A-SF-4 (session loss mid-listing
//! and mid-download, `Ctrl+R`), the remote half of A-QV-6 (V-5) and the session half of
//! A-RES-1 (the pool).
//!
//! Most tests run OpenSSH's `sftp-server` on plain pipes or a scripted server on the codec;
//! nothing contacts a host or a port. Every `sftp-server` runs with `RLIMIT_CORE` 0 (set in
//! the test process and again in `sh -c 'ulimit -c 0; exec ...'`), logs to stderr, and is
//! killed and reaped by the test that started it.

mod common;

use common::sftp::{
    Server, have_sftp_server, lost_channel, no_core_dumps, scripted, sftp_server_with_latency,
};
use common::{Script, noise, test_dir, write};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use manycommander::app::App;
use manycommander::app::event::{Effect, Event};
use manycommander::config::Config;
use manycommander::fsops::group::{Group, Root};
use manycommander::fsops::job::{JobSpec, Outcome, Report, run_guarded};
use manycommander::fsops::question::{Answer, Question};
use manycommander::fsops::sys::Sys;
use manycommander::panel::entry::LinkKind;
use manycommander::panel::listing::{self, ListingMsg};
use manycommander::provider::{Provider, Target, VPath};
use manycommander::remote::proto::{self, Attrs, Name, Packet, ext, status};
use manycommander::remote::provider::{self as rp, LOST_PANEL, ListRequest, RemoteProvider};
use manycommander::remote::url::RemoteDir;
use manycommander::remote::{Lost, RemoteMsg, Session, transport, tree};
use manycommander::theme::Depth;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(20);
const NONE: KeyModifiers = KeyModifiers::NONE;
const CTRL: KeyModifiers = KeyModifiers::CONTROL;
const ALT: KeyModifiers = KeyModifiers::ALT;

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

/// `sftp-server -e -d <dir>` on plain pipes, started as `sh -c 'ulimit -c 0; exec ...'`:
/// no core dump whatever happens to it. stdin stays open for the session's life.
fn server(dir: &Path) -> (Session, Receiver<Lost>) {
    common::sftp::sftp_server(dir)
}

/// A provider over `sftp-server` for the server `srv`.
fn remote(dir: &Path) -> (Arc<RemoteProvider>, Receiver<Lost>) {
    let (s, lost) = server(dir);
    (Arc::new(RemoteProvider::new(s, target("srv"))), lost)
}

/// A session is closed and its child reaped.
fn close(s: &Session) {
    let pid = s.pid();
    s.close_wait();
    if let Some(pid) = pid {
        assert!(
            common::sftp::gone_within(pid, T),
            "the child {pid} was not reaped"
        );
    }
}

/// Kills a session's child as a crash would: `SIGKILL` to its process group.
fn kill_server(s: &Session) {
    let pid = s.pid().unwrap() as i32;
    let g = rustix::process::Pid::from_raw(pid).unwrap();
    let _ = rustix::process::kill_process_group(g, rustix::process::Signal::KILL);
}

/// A panel listing request for `dir`.
fn list_req(r: &Arc<RemoteProvider>, dir: RemoteDir, sort: bool) -> ListRequest {
    ListRequest {
        slot: 0,
        generation: 1,
        remote: r.clone(),
        dir,
        local: PathBuf::from("/"),
        sort: sort.then(Default::default),
        cancel: Arc::new(AtomicBool::new(false)),
    }
}

/// Runs a listing and returns its messages with their arrival times.
fn list_msgs(req: &ListRequest) -> Vec<(Instant, ListingMsg)> {
    let msgs = Mutex::new(Vec::new());
    rp::list(req, &|m| msgs.lock().unwrap().push((Instant::now(), m)));
    msgs.into_inner().unwrap()
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

/// Performs listings, remote listings and remote size walks synchronously; returns the
/// other effects.
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
            Effect::RemoteSize(req) => tree::run_size(&req, &send),
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

/// Types `line` and runs it.
fn line(a: &mut App, text: &[u8]) -> Vec<Effect> {
    a.line.set(text);
    press(a, KeyCode::Enter, NONE)
}

fn status(a: &App) -> String {
    a.status
        .as_ref()
        .map(|s| s.text.clone())
        .unwrap_or_default()
}

/// The names the active panel lists, sorted.
fn names(a: &App) -> Vec<Vec<u8>> {
    let p = a.panel();
    let mut v: Vec<Vec<u8>> = (0..p.list.entries.len() as u32)
        .map(|i| p.list.name(i).to_vec())
        .collect();
    v.sort();
    v
}

/// An app whose pool holds `r` (for the server `srv`), with a local directory on both
/// sides, started.
fn app_with(r: &Arc<RemoteProvider>, local: &Path) -> App {
    let mut a = app(local, local);
    let fx = a.start();
    run(&mut a, fx);
    a.pool.insert(r.clone());
    a
}

/// `cd sftp://srv<dir>` in the active panel, run to completion.
fn cd_remote(a: &mut App, dir: &Path) {
    let mut t = b"cd sftp://srv".to_vec();
    t.extend_from_slice(&bytes(dir));
    let fx = line(a, &t);
    let rest = run(a, fx);
    assert!(
        rest.iter()
            .all(|e| matches!(e, Effect::Watch { dir: None, .. })),
        "{rest:?}"
    );
}

fn render(a: &mut App, w: u16, h: u16) -> String {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    term.draw(|f| manycommander::ui::draw(a, f)).unwrap();
    let buf = term.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..h {
        for x in 0..w {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
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
    out
}

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

fn group(r: &Arc<RemoteProvider>, dir: &Path, names: &[&str]) -> Group {
    Group {
        root: Root::Remote(r.clone()),
        sub: vpath(dir).components().to_vec(),
        names: names.iter().map(OsString::from).collect(),
    }
}

/// A download of `names` in `dir` into `dst` with the job's own worker entry point.
fn download(
    r: &Arc<RemoteProvider>,
    dir: &Path,
    names: &[&str],
    dst: &Path,
    ui: &mut Script,
) -> Report {
    run_guarded(
        JobSpec::Copy {
            groups: vec![group(r, dir, names)],
            dst: dst.to_path_buf().into(),
        },
        &Sys::default(),
        ui,
    )
}

fn mkfifo(p: &Path) {
    let st = Command::new("mkfifo").arg(p).status().unwrap();
    assert!(st.success());
}

// ---- a scripted server over an in-memory tree ---------------------------------------------

/// The requests a scripted server saw: `("lstat" | "opendir", path)` in arrival order.
type RequestLog = Vec<(&'static str, Vec<u8>)>;

/// An entry of the scripted tree.
#[derive(Clone)]
enum Obj {
    Dir,
    File(Vec<u8>),
    Link(Vec<u8>),
}

/// The scripted server's tree: paths to entries, and names a directory lists that are
/// not in the tree (unsafe names a hostile server sends).
#[derive(Clone, Default)]
struct Tree {
    objs: BTreeMap<Vec<u8>, (Obj, u32, u32)>,
    extra: BTreeMap<Vec<u8>, Vec<Vec<u8>>>,
    /// The `REALPATH(".")` answer.
    home: Vec<u8>,
    /// The `home-directory` answer, when the server announces the extension.
    ext_home: Vec<u8>,
    /// Files whose second `FSTAT` shows a later mtime: written while they were read.
    touched: Vec<Vec<u8>>,
    /// Files whose `FSTAT` shows another size than their `LSTAT`.
    grown: Vec<Vec<u8>>,
    /// Every `LSTAT` and `OPENDIR` path in arrival order, and the most directory handles
    /// open at once.
    log: Arc<Mutex<RequestLog>>,
    max_dirs: Arc<std::sync::atomic::AtomicUsize>,
}

impl Tree {
    fn dir(&mut self, p: &str) -> &mut Tree {
        self.objs
            .insert(p.as_bytes().to_vec(), (Obj::Dir, 0o040_755, 1_700_000_000));
        self
    }
    fn file(&mut self, p: &str, data: &[u8], mode: u32) -> &mut Tree {
        self.objs.insert(
            p.as_bytes().to_vec(),
            (Obj::File(data.to_vec()), 0o100_000 | mode, 1_700_000_100),
        );
        self
    }
    fn link(&mut self, p: &str, t: &str) -> &mut Tree {
        self.objs.insert(
            p.as_bytes().to_vec(),
            (Obj::Link(t.as_bytes().to_vec()), 0o120_777, 1_700_000_200),
        );
        self
    }

    fn attrs(&self, p: &[u8]) -> Option<Attrs> {
        let (o, mode, mtime) = self.objs.get(p)?;
        let size = match o {
            Obj::File(d) => d.len() as u64,
            Obj::Link(t) => t.len() as u64,
            Obj::Dir => 4096,
        };
        Some(Attrs {
            size: Some(size),
            uid_gid: Some((1000, 1000)),
            perms: Some(*mode),
            times: Some((*mtime, *mtime)),
            extended: Vec::new(),
        })
    }

    /// The names in directory `p`: `.`, `..`, its children, then the extra names.
    fn children(&self, p: &[u8]) -> Vec<Name> {
        let mut prefix = p.to_vec();
        if !prefix.ends_with(b"/") {
            prefix.push(b'/');
        }
        let mut v: Vec<Name> = [&b"."[..], b".."]
            .iter()
            .map(|n| Name {
                filename: n.to_vec(),
                longname: Vec::new(),
                attrs: self.attrs(p).unwrap_or_default(),
            })
            .collect();
        for k in self.objs.keys() {
            if let Some(rest) = k.strip_prefix(prefix.as_slice())
                && !rest.is_empty()
                && !rest.contains(&b'/')
            {
                v.push(Name {
                    filename: rest.to_vec(),
                    longname: Vec::new(),
                    attrs: self.attrs(k).unwrap(),
                });
            }
        }
        for n in self.extra.get(p).into_iter().flatten() {
            v.push(Name {
                filename: n.clone(),
                longname: Vec::new(),
                attrs: Attrs {
                    size: Some(1),
                    perms: Some(0o100_644),
                    times: Some((1, 1)),
                    ..Attrs::default()
                },
            });
        }
        v
    }

    /// Follows symlinks for `STAT`.
    fn follow(&self, p: &[u8]) -> Option<Vec<u8>> {
        let mut p = p.to_vec();
        for _ in 0..8 {
            match self.objs.get(&p)? {
                (Obj::Link(t), ..) => {
                    p = if t.starts_with(b"/") {
                        t.clone()
                    } else {
                        let dir = &p[..p.iter().rposition(|&c| c == b'/').unwrap_or(0)];
                        [dir, b"/", t].concat()
                    };
                }
                _ => return Some(p),
            }
        }
        None
    }
}

fn st(id: u32, code: u32) -> Packet {
    Packet::Status {
        id,
        code,
        message: Vec::new(),
        lang: Vec::new(),
    }
}

/// Serves `tree` until the session closes: every request of a listing, a scan and a
/// download. `READ`s are answered in reverse order of arrival once 8 are queued or no
/// request came for 20 ms, with every third reply cut to half (P3 5.3). `exts`: the
/// extensions `SSH_FXP_VERSION` announces.
fn serve_tree(mut srv: Server, tree: Tree, exts: &[(&[u8], &[u8])]) {
    srv.hello(exts);
    enum H {
        Dir(Vec<Name>, bool),
        File(Vec<u8>),
    }
    let mut fstats: Vec<(Vec<u8>, u32)> = Vec::new();
    let mut handles: Vec<Option<H>> = Vec::new();
    let mut queued: Vec<(u32, usize, u64, u32)> = Vec::new();
    let mut n = 0u64;
    let handle_of = |h: &[u8]| -> usize { u32::from_be_bytes(h.try_into().unwrap()) as usize };
    loop {
        let p = match srv.request_within(Duration::from_millis(20)) {
            Some(Some(p)) => Some(p),
            Some(None) => return,
            None => None,
        };
        let mut flush = p.is_none() && !queued.is_empty();
        match p {
            None => {}
            Some(Packet::Read {
                id,
                handle,
                offset,
                len,
            }) => {
                queued.push((id, handle_of(&handle), offset, len));
                flush = queued.len() >= 8;
            }
            Some(Packet::Lstat { id, path }) => {
                tree.log.lock().unwrap().push(("lstat", path.clone()));
                let r = match tree.attrs(&path) {
                    Some(attrs) => Packet::Attrs { id, attrs },
                    None => st(id, status::NO_SUCH_FILE),
                };
                srv.reply(&r);
            }
            Some(Packet::Stat { id, path }) => {
                let r = match tree.follow(&path).and_then(|p| tree.attrs(&p)) {
                    Some(attrs) => Packet::Attrs { id, attrs },
                    None => st(id, status::NO_SUCH_FILE),
                };
                srv.reply(&r);
            }
            Some(Packet::Opendir { id, path }) => {
                tree.log.lock().unwrap().push(("opendir", path.clone()));
                let r = match tree.objs.get(&path) {
                    Some((Obj::Dir, ..)) => {
                        handles.push(Some(H::Dir(tree.children(&path), false)));
                        let open = handles
                            .iter()
                            .filter(|h| matches!(h, Some(H::Dir(..))))
                            .count();
                        tree.max_dirs.fetch_max(open, Ordering::SeqCst);
                        Packet::Handle {
                            id,
                            handle: ((handles.len() - 1) as u32).to_be_bytes().to_vec(),
                        }
                    }
                    Some(_) => st(id, status::FAILURE),
                    None => st(id, status::NO_SUCH_FILE),
                };
                srv.reply(&r);
            }
            Some(Packet::Readdir { id, handle }) => {
                let r = match &mut handles[handle_of(&handle)] {
                    Some(H::Dir(names, done)) if !*done && !names.is_empty() => {
                        let k = names.len().min(100);
                        let batch: Vec<Name> = names.drain(..k).collect();
                        Packet::Name { id, names: batch }
                    }
                    Some(H::Dir(_, done)) => {
                        *done = true;
                        st(id, status::EOF)
                    }
                    _ => st(id, status::FAILURE),
                };
                srv.reply(&r);
            }
            Some(Packet::Open { id, path, .. }) => {
                let r = match tree.objs.get(&path) {
                    Some((Obj::File(d), ..)) => {
                        handles.push(Some(H::File(d.clone())));
                        fstats.resize(handles.len(), (Vec::new(), 0));
                        fstats[handles.len() - 1] = (path.clone(), 0);
                        Packet::Handle {
                            id,
                            handle: ((handles.len() - 1) as u32).to_be_bytes().to_vec(),
                        }
                    }
                    Some(_) => st(id, status::FAILURE),
                    None => st(id, status::NO_SUCH_FILE),
                };
                srv.reply(&r);
            }
            Some(Packet::Fstat { id, handle }) => {
                let k = handle_of(&handle);
                let r = match &handles[k] {
                    Some(H::File(d)) => {
                        let (path, n) = &mut fstats[k];
                        *n += 1;
                        let later = u32::from(*n > 1 && tree.touched.contains(path));
                        let more = u64::from(tree.grown.contains(path));
                        Packet::Attrs {
                            id,
                            attrs: Attrs {
                                size: Some(d.len() as u64 + more),
                                uid_gid: Some((1000, 1000)),
                                perms: Some(0o100_644),
                                times: Some((1_700_000_100, 1_700_000_100 + later)),
                                extended: Vec::new(),
                            },
                        }
                    }
                    _ => st(id, status::FAILURE),
                };
                srv.reply(&r);
            }
            Some(Packet::Close { id, handle }) => {
                handles[handle_of(&handle)] = None;
                srv.reply(&st(id, status::OK));
            }
            Some(Packet::Readlink { id, path }) => {
                let r = match tree.objs.get(&path) {
                    Some((Obj::Link(t), ..)) => Packet::Name {
                        id,
                        names: vec![Name {
                            filename: t.clone(),
                            ..Name::default()
                        }],
                    },
                    _ => st(id, status::FAILURE),
                };
                srv.reply(&r);
            }
            Some(Packet::Realpath { id, .. }) => {
                srv.reply(&Packet::Name {
                    id,
                    names: vec![Name {
                        filename: tree.home.clone(),
                        ..Name::default()
                    }],
                });
            }
            Some(Packet::Extended { id, name, data }) if name == b"home-directory" => {
                // One string argument, empty for the login user, answered with NAME.
                assert_eq!(data, [0, 0, 0, 0]);
                srv.reply(&Packet::Name {
                    id,
                    names: vec![Name {
                        filename: tree.ext_home.clone(),
                        ..Name::default()
                    }],
                });
            }
            Some(Packet::Extended { id, .. }) => {
                srv.reply(&st(id, status::OP_UNSUPPORTED));
            }
            Some(other) => panic!("unexpected {other:?}"),
        }
        if flush {
            for (id, h, offset, len) in queued.drain(..).rev() {
                n += 1;
                let Some(Some(H::File(content))) = handles.get(h) else {
                    srv.reply(&st(id, status::FAILURE));
                    continue;
                };
                let reply = if offset as usize >= content.len() {
                    st(id, status::EOF)
                } else {
                    let end = (offset as usize + len as usize).min(content.len());
                    let mut cut = end;
                    if n.is_multiple_of(3) && end - offset as usize > 1 {
                        cut = offset as usize + (end - offset as usize) / 2;
                    }
                    Packet::Data {
                        id,
                        data: content[offset as usize..cut].to_vec().into(),
                    }
                };
                srv.reply(&reply);
            }
        }
    }
}

/// A provider over the scripted `tree` for the server `srv`.
fn scripted_remote(
    tree: Tree,
    exts: &'static [(&'static [u8], &'static [u8])],
) -> (Arc<RemoteProvider>, std::thread::JoinHandle<()>) {
    let (s, _lost, h) = scripted(move |srv| serve_tree(srv, tree, exts));
    (Arc::new(RemoteProvider::new(s, target("srv"))), h)
}

// ---- A-SF-2: listings -----------------------------------------------------------------------

/// 10,000 files arrive as 101 batches, one per `READDIR` reply (`.` and `..` dropped), the
/// first long before the last; `Done` carries the local directory back; the free space
/// comes from `statvfs@openssh.com`; no request per entry.
#[test]
fn a_sf_2_ten_thousand_files_stream_one_batch_per_reply() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("browse-10k");
    let big = d.join("big");
    std::fs::create_dir(&big).unwrap();
    for i in 0..10_000 {
        std::fs::File::create(big.join(format!("f{i:05}"))).unwrap();
    }
    let (r, _lost) = remote(&d.path);
    let before = r.session().stats();
    let started = Instant::now();
    let msgs = list_msgs(&list_req(&r, RemoteDir::Absolute(vpath(&big)), false));
    let batches: Vec<&(Instant, ListingMsg)> = msgs
        .iter()
        .filter(|(_, m)| matches!(m, ListingMsg::Batch { .. }))
        .collect();
    assert_eq!(batches.len(), 101, "one batch per READDIR reply");
    let total: usize = batches
        .iter()
        .map(|(_, m)| match m {
            ListingMsg::Batch { entries, .. } => entries.len(),
            _ => 0,
        })
        .sum();
    assert_eq!(total, 10_000);
    let done_at = msgs
        .iter()
        .find_map(|(t, m)| match m {
            ListingMsg::Done { dir, .. } => {
                assert_eq!(dir, Path::new("/"), "Done carries the local directory");
                Some(*t)
            }
            _ => None,
        })
        .expect("Done");
    let first = batches[0].0;
    eprintln!(
        "10,000 entries over sftp-server on pipes: first rows {:.1} ms, complete {:.1} ms",
        (first - started).as_secs_f64() * 1000.0,
        (done_at - started).as_secs_f64() * 1000.0
    );
    assert!(first < done_at);
    assert!(
        msgs.iter()
            .any(|(_, m)| matches!(m, ListingMsg::FreeSpace { free, .. } if *free > 0)),
        "free space from statvfs"
    );
    assert!(msgs.iter().any(|(_, m)| matches!(
        m,
        ListingMsg::Unshown {
            invalid: 0,
            capped: false,
            ..
        }
    )));
    // OPENDIR, 102 READDIRs, CLOSE and statvfs: no request per entry.
    let requests = r.session().stats().requests - before.requests;
    assert_eq!(requests, 105, "{requests}");
    close(r.session());
}

/// Names with a newline, invalid UTF-8 and 255 bytes arrive byte-exact and are shown
/// escaped; a refresh sends one sorted listing; the symlink pass classifies links to a
/// directory, to a file and to nothing.
#[test]
fn a_sf_2_names_are_byte_exact_and_symlinks_are_classified() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("browse-names");
    let dir = d.join("n");
    std::fs::create_dir(&dir).unwrap();
    let long = vec![b'l'; 255];
    let odd: [&[u8]; 3] = [b"new\nline", b"bad\xff\xfeutf", &long];
    for n in odd {
        std::fs::write(dir.join(OsString::from_vec(n.to_vec())), b"x").unwrap();
    }
    std::fs::create_dir(dir.join("sub")).unwrap();
    std::os::unix::fs::symlink("sub", dir.join("to-dir")).unwrap();
    std::os::unix::fs::symlink(
        OsString::from_vec(b"new\nline".to_vec()),
        dir.join("to-file"),
    )
    .unwrap();
    std::os::unix::fs::symlink("nowhere", dir.join("dangling")).unwrap();
    let (r, _lost) = remote(&d.path);
    let mut a = app_with(&r, &d.path);
    cd_remote(&mut a, &dir);
    assert!(a.panel().remote().is_some(), "{}", status(&a));
    let got = names(&a);
    for n in odd {
        assert!(
            got.contains(&n.to_vec()),
            "{:?}",
            String::from_utf8_lossy(n)
        );
    }
    // The symlink pass (P3 5.4).
    let kind = |a: &App, n: &[u8]| {
        let p = a.panel();
        let i = p.list.find(n).unwrap();
        p.list.entries[i as usize].link
    };
    assert_eq!(kind(&a, b"to-dir"), LinkKind::Dir);
    assert_eq!(kind(&a, b"to-file"), LinkKind::File);
    assert_eq!(kind(&a, b"dangling"), LinkKind::Broken);
    // The title and the escaped names on screen.
    let screen = render(&mut a, 200, 30);
    let title = format!("sftp://srv{}", dir.display());
    assert!(screen.contains(&title), "{screen}");
    assert!(screen.contains("new\\nline"), "{screen}");
    assert!(screen.contains("bad\\xff\\xfeutf"), "{screen}");
    // A refresh sends one sorted listing and keeps the rows until it is complete.
    let msgs = list_msgs(&list_req(&r, RemoteDir::Absolute(vpath(&dir)), true));
    assert_eq!(
        msgs.iter()
            .filter(|(_, m)| matches!(m, ListingMsg::Batch { .. }))
            .count(),
        0
    );
    assert!(msgs.iter().any(
        |(_, m)| matches!(m, ListingMsg::Listing { listing, .. } if listing.entries.len() == 7)
    ));
    let fx = press(&mut a, KeyCode::Char('r'), CTRL);
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::ListRemote(req, _) if req.sort.is_some()))
    );
    run(&mut a, fx);
    assert_eq!(names(&a).len(), 7);
    close(r.session());
}

/// A hostile server's names with `/` or NUL, or that are not a single component, are
/// skipped and counted in the footer; `.` and `..` are dropped and not counted.
#[test]
fn a_sf_2_unsafe_names_are_skipped_and_counted() {
    let mut t = Tree::default();
    t.dir("/d").file("/d/ok", b"fine", 0o644);
    t.extra.insert(
        b"/d".to_vec(),
        vec![
            b"a/b".to_vec(),
            b"x\0y".to_vec(),
            b"../../etc".to_vec(),
            Vec::new(),
        ],
    );
    let (r, h) = scripted_remote(t, &[]);
    let d = test_dir("browse-unsafe");
    let mut a = app_with(&r, &d.path);
    cd_remote(&mut a, Path::new("/d"));
    assert_eq!(names(&a), vec![b"ok".to_vec()]);
    assert_eq!(a.panel().unshown, (4, false));
    let screen = render(&mut a, 120, 20);
    assert!(
        screen.contains("4 entries with invalid names not shown"),
        "{screen}"
    );
    close(r.session());
    h.join().unwrap();
}

/// `sftp://host` and `/~/...` resolve through the `home-directory` extension when the
/// server announces it (one empty string, answered with `SSH_FXP_NAME`), else through
/// `REALPATH(".")`.
#[test]
fn a_sf_2_the_login_directory_through_the_extension() {
    let mut t = Tree {
        home: b"/wrong".to_vec(),
        ext_home: b"/srv/home".to_vec(),
        ..Tree::default()
    };
    t.dir("/")
        .dir("/srv")
        .dir("/srv/home")
        .file("/srv/home/f", b"1", 0o644);
    let (r, h) = scripted_remote(t, &[ext::HOME_DIRECTORY]);
    let d = test_dir("browse-home-ext");
    let mut a = app_with(&r, &d.path);
    let fx = line(&mut a, b"cd sftp://srv/~");
    run(&mut a, fx);
    assert_eq!(a.panel().location(), b"sftp://srv/srv/home");
    assert_eq!(names(&a), vec![b"f".to_vec()]);
    close(r.session());
    h.join().unwrap();
}

/// `sftp://host` and `/~/...` resolve through `REALPATH(".")` when the server has no
/// `home-directory` extension; the title follows once the server answers, and `..` at `/`
/// returns to the local directory.
#[test]
fn a_sf_2_the_login_directory_and_leaving_the_server() {
    let mut t = Tree {
        home: b"/srv/home".to_vec(),
        ..Tree::default()
    };
    t.dir("/")
        .dir("/srv")
        .dir("/srv/home")
        .file("/srv/home/f", b"1", 0o644);
    t.dir("/srv/home/sub").file("/srv/home/sub/g", b"2", 0o600);
    let (r, h) = scripted_remote(t, &[]);
    let d = test_dir("browse-home");
    let mut a = app_with(&r, &d.path);
    let fx = line(&mut a, b"cd sftp://srv");
    let [Effect::ListRemote(req, _)] = &fx[..] else {
        panic!("{fx:?}")
    };
    assert_eq!(req.dir, RemoteDir::Home(VPath::root()));
    assert_eq!(a.panel().location(), b"sftp://srv/~");
    run(&mut a, fx);
    assert_eq!(a.panel().location(), b"sftp://srv/srv/home");
    assert_eq!(names(&a), vec![b"f".to_vec(), b"sub".to_vec()]);
    // Frecency records local directories only; no watch on a server (P3 5.1, 2.6).
    assert!(a.dirs.deltas.is_empty());
    // A relative cd moves on the server; `..` walks up to `/`, then home again.
    let fx = line(&mut a, b"cd sub");
    run(&mut a, fx);
    assert_eq!(a.panel().location(), b"sftp://srv/srv/home/sub");
    assert_eq!(names(&a), vec![b"g".to_vec()]);
    let fx = line(&mut a, b"cd ../../..");
    run(&mut a, fx);
    assert_eq!(a.panel().location(), b"sftp://srv/", "{}", status(&a));
    // `/~/sub` resolves below the login directory.
    let fx = line(&mut a, b"cd sftp://srv/~/sub");
    run(&mut a, fx);
    assert_eq!(a.panel().location(), b"sftp://srv/srv/home/sub");
    // Alt+Left returns to the places left, by target: the pool's session.
    let fx = press(&mut a, KeyCode::Left, ALT);
    assert!(matches!(&fx[..], [Effect::ListRemote(..)]), "{fx:?}");
    run(&mut a, fx);
    assert_eq!(a.panel().location(), b"sftp://srv/");
    // `..` at `/` returns the panel to its local directory.
    a.panel_mut().cursor_to(0);
    let fx = press(&mut a, KeyCode::Enter, NONE);
    run(&mut a, fx);
    assert!(a.panel().is_directory());
    assert_eq!(a.panel().dir, d.path);
    assert_eq!(a.panel().location(), bytes(&d.path));
    close(r.session());
    h.join().unwrap();
}

// ---- A-SF-3: downloads ----------------------------------------------------------------------

/// Sets a file's mode and mtime (whole seconds).
fn stamp(p: &Path, mode: u32, mtime: i64) {
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
    let t = rustix::fs::Timestamps {
        last_access: rustix::fs::Timespec {
            tv_sec: mtime,
            tv_nsec: 0,
        },
        last_modification: rustix::fs::Timespec {
            tv_sec: mtime,
            tv_nsec: 0,
        },
    };
    rustix::fs::utimensat(rustix::fs::CWD, p, &t, rustix::fs::AtFlags::empty()).unwrap();
}

/// 0 B, 1 B, 1 MiB + 1 and 100 MiB files arrive byte-identical through the local engine
/// with the pipelined window, with their mode (setuid and setgid cleared, M1 4.7) and
/// mtime; no temporary name remains.
#[test]
fn a_sf_3_files_are_byte_identical_with_mode_and_mtime() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("browse-dl");
    let src = d.join("src");
    let dst = d.join("dst");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    let sizes = [0usize, 1, (1 << 20) + 1, 100 << 20];
    let mut names = Vec::new();
    for (k, n) in sizes.iter().enumerate() {
        let name = format!("f{k}");
        write(&src.join(&name), &noise(*n, k as u64 + 1));
        stamp(
            &src.join(&name),
            [0o640, 0o755, 0o4755, 0o600][k],
            1_600_000_000 + k as i64,
        );
        names.push(name);
    }
    let (r, _lost) = remote(&d.path);
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let started = Instant::now();
    let rep = download(&r, &src, &refs, &dst, &mut Script::silent());
    let took = started.elapsed();
    assert_eq!((rep.done, rep.failed, rep.skipped), (4, 0, 0), "{rep:?}");
    for (k, name) in names.iter().enumerate() {
        let a = std::fs::read(src.join(name)).unwrap();
        let b = std::fs::read(dst.join(name)).unwrap();
        assert!(a == b, "{name} differs");
        let m = std::fs::metadata(dst.join(name)).unwrap();
        assert_eq!(m.mode() & 0o7777, [0o640, 0o755, 0o755, 0o600][k], "{name}");
        assert_eq!(m.mtime(), 1_600_000_000 + k as i64, "{name}");
    }
    assert!(partials(&dst).is_empty());
    eprintln!(
        "download of {} MiB through sftp-server on pipes: {:.0} ms (debug build)",
        sizes.iter().sum::<usize>() >> 20,
        took.as_secs_f64() * 1000.0
    );
    close(r.session());
}

/// Replies out of order and short reads from a scripted server give the same bytes; the
/// window is small so the file takes many requests. A symlink arrives with its target.
#[test]
fn a_sf_3_out_of_order_replies_and_short_reads() {
    let content = noise(300_001, 5);
    let mut t = Tree::default();
    t.dir("/").dir("/d").file("/d/f", &content, 0o644);
    t.link("/d/l", "../elsewhere");
    let (r, h) = scripted_remote(t, &[]);
    assert!(
        r.session()
            .set_sizes(manycommander::remote::session::Sizes {
                read: 4096,
                write: 4096,
                window: 8,
            })
    );
    let d = test_dir("browse-ooo");
    let rep = download(&r, Path::new("/"), &["d"], &d.path, &mut Script::silent());
    assert_eq!((rep.done, rep.dirs_done), (2, 1), "{rep:?}");
    assert!(std::fs::read(d.join("d/f")).unwrap() == content);
    assert_eq!(
        std::fs::read_link(d.join("d/l")).unwrap(),
        Path::new("../elsewhere")
    );
    assert!(r.lost().is_none());
    close(r.session());
    h.join().unwrap();
}

/// The scan (P3 5.5, R-3): up to 8 directory listings in flight, and every directory
/// `LSTAT`ed before its `OPENDIR`.
#[test]
fn a_sf_3_the_scan_lists_eight_directories_at_once_after_lstat() {
    let mut t = Tree::default();
    t.dir("/").dir("/top");
    for i in 0..20 {
        t.dir(&format!("/top/d{i:02}"));
        t.dir(&format!("/top/d{i:02}/deep"));
        t.file(&format!("/top/d{i:02}/deep/f"), b"x", 0o644);
    }
    let (log, max) = (t.log.clone(), t.max_dirs.clone());
    let (r, h) = scripted_remote(t, &[]);
    let d = test_dir("browse-scan8");
    let rep = download(&r, Path::new("/"), &["top"], &d.path, &mut Script::silent());
    assert_eq!(
        (rep.done, rep.dirs_done, rep.failed),
        (20, 41, 0),
        "{rep:?}"
    );
    assert_eq!(max.load(Ordering::SeqCst), 8, "listings in flight");
    let log = log.lock().unwrap();
    for (k, (what, path)) in log.iter().enumerate() {
        if *what == "opendir" {
            assert!(
                log[..k].iter().any(|(w, p)| *w == "lstat" && p == path),
                "OPENDIR {} without an LSTAT first",
                String::from_utf8_lossy(path)
            );
        }
    }
    close(r.session());
    h.join().unwrap();
}

/// The `FSTAT` checks (P3 5.5): a file whose open handle shows another size than the plan
/// fails before a byte is read, and one whose mtime changed while it was read fails after
/// the last byte; both with "source changed", and neither leaves a name behind.
#[test]
fn a_sf_3_a_file_that_changes_fails_with_source_changed() {
    let mut t = Tree {
        touched: vec![b"/d/touched".to_vec()],
        grown: vec![b"/d/grown".to_vec()],
        ..Tree::default()
    };
    t.dir("/").dir("/d");
    t.file("/d/touched", &noise(50_000, 1), 0o644);
    t.file("/d/grown", b"small", 0o644);
    t.file("/d/fine", b"fine", 0o644);
    let (r, h) = scripted_remote(t, &[]);
    let d = test_dir("browse-changed");
    let rep = download(
        &r,
        Path::new("/d"),
        &["touched", "grown", "fine"],
        &d.path,
        &mut Script::silent(),
    );
    assert_eq!(rep.done, 1, "{rep:?}");
    let mut got = issues(&rep);
    got.sort();
    assert_eq!(
        got,
        vec![
            ("grown".to_string(), rp::SOURCE_CHANGED.to_string()),
            ("touched".to_string(), rp::SOURCE_CHANGED.to_string()),
        ]
    );
    assert_eq!(std::fs::read(d.join("fine")).unwrap(), b"fine");
    assert!(!d.join("touched").exists() && !d.join("grown").exists());
    assert!(partials(&d.path).is_empty());
    close(r.session());
    h.join().unwrap();
}

/// A tree: directories, files, symlinks as symlinks whose local `readlink` equals the
/// remote target (and never a new file at the path the target names), and a FIFO that is
/// skipped as "special file" and never opened (an open would block `sftp-server`).
#[test]
fn a_sf_3_a_tree_with_symlinks_and_a_fifo() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("browse-tree");
    let src = d.join("tree");
    std::fs::create_dir_all(src.join("a/b/c")).unwrap();
    write(&src.join("top"), b"top");
    write(&src.join("a/one"), &noise(70_000, 2));
    write(&src.join("a/b/two"), b"two");
    write(&src.join("a/b/c/three"), b"");
    std::os::unix::fs::symlink("b/two", src.join("a/rel")).unwrap();
    std::os::unix::fs::symlink("/nonexistent/target", src.join("a/abs")).unwrap();
    mkfifo(&src.join("a/pipe"));
    let dst = d.join("dst");
    std::fs::create_dir(&dst).unwrap();
    let (r, _lost) = remote(&d.path);
    // A job that opened the FIFO would block here: the test fails by timeout instead.
    let (tx, rx) = std::sync::mpsc::channel();
    let (r2, d2, dst2) = (r.clone(), d.path.clone(), dst.clone());
    std::thread::spawn(move || {
        let rep = download(&r2, &d2, &["tree"], &dst2, &mut Script::silent());
        let _ = tx.send(rep);
    });
    let rep = rx.recv_timeout(T).expect("the download finished");
    assert_eq!(
        issues(&rep),
        vec![("pipe".to_string(), "special file".to_string())],
        "{rep:?}"
    );
    assert_eq!(rep.done, 6, "{rep:?}");
    let out = dst.join("tree");
    assert_eq!(std::fs::read(out.join("top")).unwrap(), b"top");
    assert!(std::fs::read(out.join("a/one")).unwrap() == noise(70_000, 2));
    assert_eq!(std::fs::read(out.join("a/b/c/three")).unwrap(), b"");
    for (link, target) in [("a/rel", "b/two"), ("a/abs", "/nonexistent/target")] {
        let m = std::fs::symlink_metadata(out.join(link)).unwrap();
        assert!(m.file_type().is_symlink(), "{link}");
        assert_eq!(
            std::fs::read_link(out.join(link)).unwrap(),
            Path::new(target)
        );
    }
    assert!(!Path::new("/nonexistent/target").exists());
    assert!(!out.join("a/pipe").exists());
    assert!(partials(&dst).is_empty());
    close(r.session());
}

/// "file exists": Skip keeps the local file, Overwrite replaces it atomically with the
/// remote bytes, Rename writes the new name.
#[test]
fn a_sf_3_file_exists() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("browse-exists");
    let src = d.join("src");
    let dst = d.join("dst");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    write(&src.join("f"), b"remote");
    write(&dst.join("f"), b"local");
    let (r, _lost) = remote(&d.path);
    let mut ui = Script::new([Answer::Skip]);
    let rep = download(&r, &src, &["f"], &dst, &mut ui);
    assert!(
        matches!(ui.asked[..], [Question::FileExists { .. }]),
        "{:?}",
        ui.asked
    );
    assert_eq!(rep.skipped, 1);
    assert_eq!(std::fs::read(dst.join("f")).unwrap(), b"local");
    let mut ui = Script::new([Answer::Rename("g".into())]);
    download(&r, &src, &["f"], &dst, &mut ui);
    assert_eq!(std::fs::read(dst.join("g")).unwrap(), b"remote");
    let mut ui = Script::new([Answer::Overwrite]);
    let rep = download(&r, &src, &["f"], &dst, &mut ui);
    assert_eq!(rep.done, 1, "{rep:?}");
    assert_eq!(std::fs::read(dst.join("f")).unwrap(), b"remote");
    assert!(partials(&dst).is_empty());
    close(r.session());
}

/// A UI that cancels the job once `at` bytes are done, or calls `hook` then.
struct CancelAt {
    cancel: Arc<AtomicBool>,
    at: u64,
    hook: Option<Box<dyn FnMut() + Send>>,
}

impl manycommander::fsops::question::Interaction for CancelAt {
    fn ask(&mut self, _q: Question) -> Answer {
        Answer::Cancel
    }
    fn progress(&mut self, p: manycommander::fsops::question::Progress) {
        if p.bytes_done >= self.at {
            match self.hook.take() {
                Some(mut f) => f(),
                None => self.cancel.store(true, Ordering::SeqCst),
            }
        }
    }
}

/// Cancel in the middle of a file (P3 5.5): the outstanding replies are drained, no partial
/// name remains, and the session stays usable on a healthy server. A link with 10 ms of
/// latency each way and a small window keep the file in flight long enough.
#[test]
fn a_sf_3_cancel_mid_file_leaves_nothing_and_the_session_stays_usable() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("browse-cancel");
    let src = d.join("src");
    let dst = d.join("dst");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    write(&src.join("big"), &noise(16 << 20, 7));
    let (s, lost) = sftp_server_with_latency(&d.path, Duration::from_millis(10));
    assert!(s.set_sizes(manycommander::remote::session::Sizes {
        read: 32 << 10,
        write: 32 << 10,
        window: 4,
    }));
    let r = Arc::new(RemoteProvider::new(s, target("srv")));
    let cancel = Arc::new(AtomicBool::new(false));
    let mut ui = CancelAt {
        cancel: cancel.clone(),
        at: 1 << 20,
        hook: None,
    };
    let rep = run_guarded(
        JobSpec::Copy {
            groups: vec![group(&r, &src, &["big"])],
            dst: dst.clone().into(),
        },
        &Sys::new(cancel.clone()),
        &mut ui,
    );
    assert!(rep.cancelled, "{rep:?}");
    assert_eq!(rep.done, 0);
    assert!(walk(&dst).is_empty(), "{:?}", walk(&dst));
    assert!(r.lost().is_none(), "the session stays usable");
    assert!(lost.try_recv().is_err());
    let m = r.lstat(&vpath(&src.join("big"))).unwrap();
    assert_eq!(m.size, 16 << 20);
    close(r.session());
}

/// A session over `sftp-server` whose requests pass a tap first: `hook` sees each request
/// before the server does (a scripted swap between the `LSTAT` and the `OPEN`, R-3).
fn tapped_server(
    dir: &Path,
    mut hook: impl FnMut(&Packet) + Send + 'static,
) -> (Arc<RemoteProvider>, Receiver<Lost>) {
    use std::os::unix::process::CommandExt;
    no_core_dumps();
    let mut child = common::sftp::sftp_server_command(dir, &[])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("sftp-server");
    let mut server_in = std::fs::File::from(rustix::fd::OwnedFd::from(child.stdin.take().unwrap()));
    let mut server_out =
        std::fs::File::from(rustix::fd::OwnedFd::from(child.stdout.take().unwrap()));
    let (req_r, req_w) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).unwrap();
    let (rep_r, rep_w) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).unwrap();
    let mut requests = std::fs::File::from(req_r);
    let mut replies = std::fs::File::from(rep_w);
    std::thread::spawn(move || {
        while let Ok(Some(body)) = proto::read_frame(&mut requests) {
            if let Ok(p) = Packet::decode(body.clone()) {
                hook(&p);
            }
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
    (Arc::new(RemoteProvider::new(s, target("srv"))), rx)
}

/// R-3 and P3 2.5: the file is swapped for a FIFO between the download's `LSTAT` and its
/// `OPEN`. `sftp-server` blocks in the open, and every later request waits behind it. A
/// cancel sends nothing more; the server stays silent through the 2 s drain window, so the
/// session ends with "connection lost", its child is killed and reaped, and nothing is left
/// in the destination.
#[test]
fn a_sf_3_a_fifo_swapped_in_before_the_open_wedges_the_server_until_cancel() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("browse-fifo");
    let src = d.join("src");
    let dst = d.join("dst");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    let victim = src.join("victim");
    write(&victim, &noise(1 << 20, 3));
    let opened = Arc::new(AtomicBool::new(false));
    let (o, v) = (opened.clone(), victim.clone());
    let (r, lost) = tapped_server(&d.path, move |p| {
        if let Packet::Open { path, .. } = p
            && path.ends_with(b"/victim")
        {
            std::fs::remove_file(&v).unwrap();
            mkfifo(&v);
            o.store(true, Ordering::SeqCst);
        }
    });
    let pid = r.session().pid().unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel();
    let (r2, src2, dst2, c2) = (r.clone(), src.clone(), dst.clone(), cancel.clone());
    std::thread::spawn(move || {
        let rep = run_guarded(
            JobSpec::Copy {
                groups: vec![group(&r2, &src2, &["victim"])],
                dst: dst2.into(),
            },
            &Sys::new(c2),
            &mut Script::silent(),
        );
        let _ = tx.send(rep);
    });
    let end = Instant::now() + T;
    while !opened.load(Ordering::SeqCst) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(opened.load(Ordering::SeqCst), "the OPEN reached the server");
    // The server is wedged: the job waits.
    assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
    let cancelled = Instant::now();
    cancel.store(true, Ordering::SeqCst);
    let rep = rx.recv_timeout(T).expect("the job ended after the cancel");
    let took = cancelled.elapsed();
    assert!(rep.cancelled, "{rep:?}");
    assert!(
        took >= Duration::from_millis(1900) && took < Duration::from_secs(6),
        "{took:?}"
    );
    let l = lost.recv_timeout(T).expect("the session was lost");
    assert!(l.reason.contains("stopped answering"), "{l:?}");
    assert!(r.lost().is_some());
    assert!(common::sftp::gone_within(pid, T), "the child was reaped");
    assert!(walk(&dst).is_empty(), "{:?}", walk(&dst));
}

// ---- A-SF-4: session loss -------------------------------------------------------------------

/// The server is killed during a refresh of a listed directory: the panel keeps its rows
/// and says "connection lost"; the child is reaped; verbs that need the server are refused;
/// `Ctrl+R` reconnects to the same target and lists the directory again.
#[test]
fn a_sf_4_the_server_killed_mid_listing() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("browse-lost-list");
    let big = d.join("big");
    std::fs::create_dir(&big).unwrap();
    for i in 0..3000 {
        std::fs::File::create(big.join(format!("f{i:04}"))).unwrap();
    }
    // 31 READDIR replies at a 40 ms round trip: over a second per listing.
    let (s, lost) = sftp_server_with_latency(&d.path, Duration::from_millis(20));
    let pid = s.pid().unwrap();
    let r = Arc::new(RemoteProvider::new(s, target("srv")));
    let mut a = app_with(&r, &d.path);
    cd_remote(&mut a, &big);
    assert_eq!(a.panel().list.entries.len(), 3000);
    let fx = press(&mut a, KeyCode::Char('r'), CTRL);
    let r2 = r.clone();
    let killer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        kill_server(r2.session());
    });
    let started = Instant::now();
    run(&mut a, fx);
    killer.join().unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(a.panel().list.entries.len(), 3000, "the rows stay");
    assert_eq!(
        a.panel().message.as_deref(),
        Some("connection lost"),
        "the failed refresh says so"
    );
    let l = lost.recv_timeout(T).expect("the loss is reported");
    a.update(Event::Remote(RemoteMsg::Lost {
        address: "sftp://srv".into(),
        message: l.message().to_owned(),
    }));
    assert_eq!(a.panel().message.as_deref(), Some(LOST_PANEL));
    assert!(status(&a).contains("connection lost"), "{}", status(&a));
    assert!(common::sftp::gone_within(pid, T), "the child was reaped");
    let screen = render(&mut a, 160, 20);
    assert!(screen.contains(LOST_PANEL), "{screen}");
    // Verbs that need the server are refused until a reconnect.
    a.panel_mut().cursor_to_name(b"f0001");
    for (code, m) in [
        (KeyCode::F(5), NONE),
        (KeyCode::F(3), NONE),
        (KeyCode::Enter, NONE),
    ] {
        let fx = press(&mut a, code, m);
        assert!(fx.is_empty(), "{code:?}: {fx:?}");
        assert!(a.dialog.is_none());
        assert_eq!(status(&a), LOST_PANEL, "{code:?}");
    }
    // Ctrl+R reconnects: the pool let the lost session go, so the target connects again,
    // and the panel lists the same directory in place.
    let fx = press(&mut a, KeyCode::Char('r'), CTRL);
    let Some(Effect::Connect(addr, _)) = fx.iter().find(|e| matches!(e, Effect::Connect(..)))
    else {
        panic!("{fx:?}")
    };
    assert_eq!(addr.target, target("srv"));
    assert_eq!(addr.dir, RemoteDir::Absolute(vpath(&big)));
    run(&mut a, fx);
    let (s2, _lost2) = server(&d.path);
    let fx = a.update(Event::Remote(RemoteMsg::Connected {
        target: target("srv"),
        session: s2,
    }));
    run(&mut a, fx);
    let v = a.panel().remote().expect("a remote panel");
    assert!(!v.lost());
    assert!(!Arc::ptr_eq(&v.session, &r));
    assert_eq!(a.panel().list.entries.len(), 3000);
    assert_eq!(a.panel().message, None);
    assert_eq!(a.pool.len(), 1);
    let s2 = v.session.clone();
    a.pool.close_all();
    assert!(s2.lost().is_some());
}

/// The server is killed in the middle of a download: the job reports every entry (I-7),
/// the file in progress and the ones not reached fail with "connection lost", no partial
/// name remains, and the child is reaped.
#[test]
fn a_sf_4_the_server_killed_mid_download() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("browse-lost-dl");
    let src = d.join("src");
    let dst = d.join("dst");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    let names = ["a", "b", "c", "d", "e"];
    for (k, n) in names.iter().enumerate() {
        write(&src.join(n), &noise(4 << 20, k as u64));
    }
    let (s, lost) = sftp_server_with_latency(&d.path, Duration::from_millis(5));
    assert!(s.set_sizes(manycommander::remote::session::Sizes {
        read: 32 << 10,
        write: 32 << 10,
        window: 4,
    }));
    let pid = s.pid().unwrap();
    let r = Arc::new(RemoteProvider::new(s, target("srv")));
    let r2 = r.clone();
    let mut ui = CancelAt {
        cancel: Arc::new(AtomicBool::new(false)),
        at: 6 << 20,
        hook: Some(Box::new(move || kill_server(r2.session()))),
    };
    let rep = run_guarded(
        JobSpec::Copy {
            groups: vec![group(&r, &src, &names)],
            dst: dst.clone().into(),
        },
        &Sys::default(),
        &mut ui,
    );
    assert!(!rep.cancelled, "{rep:?}");
    assert_eq!(rep.planned, 5);
    assert_eq!(rep.done + rep.failed, 5, "every entry is reported: {rep:?}");
    assert!(rep.done >= 1 && rep.failed >= 1, "{rep:?}");
    for (_, why) in issues(&rep) {
        assert_eq!(why, "connection lost");
    }
    assert!(partials(&dst).is_empty(), "{:?}", partials(&dst));
    for n in names {
        let p = dst.join(n);
        if p.exists() {
            assert!(std::fs::read(&p).unwrap() == std::fs::read(src.join(n)).unwrap());
        }
    }
    assert!(lost.recv_timeout(T).is_ok());
    assert!(common::sftp::gone_within(pid, T), "the child was reaped");
}

// ---- A-QV-6, the remote half ------------------------------------------------------------------

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |x, _| {
        image::Rgb([(x * 7) as u8, 128, 64])
    }));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// V-5: resting on a remote file reads nothing from it (the provider's read counter), and
/// the card says why; `Alt+Q` loads the entry under the cursor through the provider.
#[test]
fn a_qv_6_remote_files_preview_only_on_alt_q() {
    use manycommander::preview::{self as pv, Msg, Protocol, Subject, worker};
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("browse-qv6");
    let pics = d.join("pics");
    std::fs::create_dir(&pics).unwrap();
    write(&pics.join("img.png"), &png(30, 20));
    let (r, _lost) = remote(&d.path);
    let mut a = app_with(&r, &d.path);
    a.set_graphics(Protocol::Kitty, Some((10, 20)));
    cd_remote(&mut a, &pics);
    press(&mut a, KeyCode::Char('q'), CTRL);
    a.panel_mut().cursor_to_name(b"img.png");
    a.quick_sync();
    render(&mut a, 100, 30);
    a.quick_sync();
    std::thread::sleep(Duration::from_millis(150));
    let fx = a.update(Event::Tick);
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::Preview(_))),
        "cursor rest reads nothing: {fx:?}"
    );
    assert_eq!(a.quick.requests, 0);
    assert_eq!(r.reads(), 0);
    assert_eq!(a.quick_card().reason.as_deref(), Some(pv::REMOTE_ON_KEY));
    let fx = press(&mut a, KeyCode::Char('q'), ALT);
    let reqs: Vec<_> = fx
        .iter()
        .filter_map(|e| match e {
            Effect::Preview(r) => Some(r.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(reqs.len(), 1, "Alt+Q loads it: {fx:?}");
    assert!(matches!(reqs[0].subject, Subject::Place { .. }));
    let (m, stats) = worker::process_once(&reqs[0]);
    let Some(Msg::Ready { card, .. }) = m else {
        panic!("{m:?}")
    };
    assert_eq!(card.pixels, Some((30, 20)));
    assert_eq!(stats.place_reads.load(Ordering::Relaxed), 1);
    assert_eq!(r.reads(), 1);
    close(r.session());
}

// ---- A-RES-1, the session half ----------------------------------------------------------------

/// At most four sessions (P3 5.7): a fifth connect closes the least recently used session
/// that is not in use, and is refused when all four are in use. A session is in use while
/// anything besides the pool holds it.
#[test]
fn a_res_1_four_sessions_least_recently_used_out() {
    use manycommander::remote::pool::FULL;
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("browse-pool");
    let mut a = app(&d.path, &d.path);
    let fx = a.start();
    run(&mut a, fx);
    let mut held = Vec::new();
    let mut pids = Vec::new();
    for k in 1..=4 {
        let (s, _lost) = server(&d.path);
        pids.push(s.pid().unwrap());
        let r = Arc::new(RemoteProvider::new(s, target(&format!("s{k}"))));
        a.pool.insert(r.clone());
        held.push(r);
        std::thread::sleep(Duration::from_millis(2));
    }
    // s1 is the least recently used, but a tab shows it: in use. s2 is idle.
    a.pool.get(&target("s3"));
    a.pool.get(&target("s4"));
    let s2 = held.remove(1).id();
    let fx = line(&mut a, b"cd sftp://s5");
    assert!(
        matches!(&fx[..], [Effect::Connect(addr, _)] if addr.target == target("s5")),
        "{fx:?}"
    );
    assert_eq!(a.pool.len(), 3, "{:?}", a.pool.numbers());
    assert!(!a.pool.numbers().contains(&s2), "the idle session closed");
    assert!(
        common::sftp::gone_within(pids[1], T),
        "its child was reaped"
    );
    // The connect arrives: four again, all in use now.
    let (s5, _lost) = server(&d.path);
    pids.push(s5.pid().unwrap());
    a.update(Event::Remote(RemoteMsg::Connected {
        target: target("s5"),
        session: s5,
    }));
    assert_eq!(a.pool.len(), 4);
    let fx = line(&mut a, b"cd sftp://s6");
    assert!(fx.is_empty(), "{fx:?}");
    assert_eq!(status(&a), FULL);
    assert_eq!(a.pool.len(), 4);
    // A target with an open session never connects again.
    let fx = line(&mut a, b"cd sftp://s1/");
    assert!(
        matches!(&fx[..], [Effect::ListRemote(req, _)] if Arc::ptr_eq(&req.remote, &held[0])),
        "{fx:?}"
    );
    a.pool.close_all();
    for pid in pids {
        assert!(common::sftp::gone_within(pid, T), "{pid}");
    }
}

// ---- F3, F4, F5, Space, tabs and bookmarks in a remote panel ----------------------------------

/// F3 on a remote file (P3 5.5): a copy in the runtime view directory, made through the
/// provider, handed to the pager; an unchanged copy is removed after the hand-off, an
/// edited one is kept and reported (3a uploads nothing). A file above 256 MB asks first.
/// Symlinks and special files are never read (R-3).
#[test]
fn f3_and_f4_on_a_remote_file_go_through_the_view_directory() {
    use manycommander::viewtemp::{self, Roots, ViewMsg};
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("browse-view");
    let dir = d.join("files");
    std::fs::create_dir(&dir).unwrap();
    write(&dir.join("notes.txt"), b"remote notes\n");
    std::os::unix::fs::symlink("notes.txt", dir.join("link")).unwrap();
    // A sparse file above the cap.
    let big = std::fs::File::create(dir.join("big")).unwrap();
    big.set_len(300 << 20).unwrap();
    drop(big);
    let rt = d.join("rt");
    std::fs::create_dir(&rt).unwrap();
    std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o700)).unwrap();
    let roots = Roots::new(Some(rt.clone()), d.join("tmp"));
    let (r, _lost) = remote(&d.path);
    let mut a = app_with(&r, &d.path);
    cd_remote(&mut a, &dir);
    // A symlink is never read for data.
    a.panel_mut().cursor_to_name(b"link");
    assert!(press(&mut a, KeyCode::F(3), NONE).is_empty());
    assert_eq!(status(&a), rp::NOT_A_FILE);
    // Above 256 MB: asked first.
    a.panel_mut().cursor_to_name(b"big");
    assert!(press(&mut a, KeyCode::F(3), NONE).is_empty());
    assert!(matches!(
        a.dialog,
        Some(manycommander::ui::dialog::Dialog::Confirm { .. })
    ));
    press(&mut a, KeyCode::Esc, NONE);
    for edit in [false, true] {
        a.panel_mut().cursor_to_name(b"notes.txt");
        let key = if edit { KeyCode::F(4) } else { KeyCode::F(3) };
        let fx = press(&mut a, key, NONE);
        let [Effect::PrepareView(req, alive)] = &fx[..] else {
            panic!("{fx:?}")
        };
        assert!(req.remote);
        let file = viewtemp::prepare(&roots, req, &|_, _| {}).unwrap();
        alive.finish();
        assert_eq!(std::fs::read(file.path()).unwrap(), b"remote notes\n");
        let m = std::fs::metadata(file.path()).unwrap();
        assert_eq!(m.mode() & 0o777, 0o600);
        let fx = a.update(Event::View(ViewMsg::Ready {
            id: req.id,
            file: file.clone(),
        }));
        assert!(matches!(&fx[..], [Effect::Run(_)]), "{fx:?}");
        if edit {
            // An editor that writes a new file and renames it over the copy.
            let tmp = file.dir.join(".edit");
            write(&tmp, b"edited\n");
            std::fs::rename(&tmp, file.path()).unwrap();
        }
        let fx = a.update(Event::ChildDone {
            status: String::new(),
            output: None,
        });
        // A remote file's copy is checked with its server file (P3 5.6, T7).
        let Some(Effect::CheckEdited(f, at)) =
            fx.iter().find(|e| matches!(e, Effect::CheckEdited(..)))
        else {
            panic!("{fx:?}")
        };
        let m = viewtemp::check_edited(&roots, f, at.clone());
        assert_eq!(matches!(m, ViewMsg::Edited { .. }), edit, "{m:?}");
        let kept = match &m {
            ViewMsg::Edited { copy, changed, .. } => {
                assert!(!changed, "the server file is unchanged");
                Some(copy.clone())
            }
            _ => None,
        };
        a.update(Event::View(m));
        if edit {
            // The write-back question; Esc keeps the local copy and says where.
            assert!(matches!(
                a.dialog,
                Some(manycommander::ui::dialog::Dialog::Choose { .. })
            ));
            press(&mut a, KeyCode::Esc, NONE);
            assert!(a.dialog.is_none());
            assert!(
                status(&a).starts_with("not uploaded to the server; your edited copy is at"),
                "{}",
                status(&a)
            );
            assert_eq!(std::fs::read(kept.unwrap()).unwrap(), b"edited\n");
        } else {
            assert!(!file.dir.exists(), "the copy is removed");
        }
    }
    // The server's file is untouched.
    assert_eq!(
        std::fs::read(dir.join("notes.txt")).unwrap(),
        b"remote notes\n"
    );
    close(r.session());
}

/// F5 in a remote panel opens the download dialog with the other panel's directory; the
/// job it starts downloads through the local engine. F6 and the 3b verbs open their
/// dialogs (T7), F8 is refused with R-5's message; `Space` sizes a directory by a walk on
/// the server; `Alt+P` inserts the remote path.
#[test]
fn f5_space_and_refusals_in_a_remote_panel() {
    use manycommander::app::jobs::NO_REMOTE_TRASH;
    use manycommander::ui::dialog::Dialog;
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("browse-f5");
    let dir = d.join("srv");
    std::fs::create_dir_all(dir.join("sub/deeper")).unwrap();
    write(&dir.join("sub/a"), &noise(1000, 1));
    write(&dir.join("sub/deeper/b"), &noise(234, 2));
    write(&dir.join("x"), b"x");
    let local = d.join("local");
    std::fs::create_dir(&local).unwrap();
    let (r, _lost) = remote(&d.path);
    let mut a = app(&local, &local);
    let fx = a.start();
    run(&mut a, fx);
    a.pool.insert(r.clone());
    cd_remote(&mut a, &dir);
    a.panel_mut().cursor_to_name(b"sub");
    for (code, m, want) in [
        (KeyCode::F(6), NONE, "Move"),
        (KeyCode::F(7), NONE, "Make directory"),
        (KeyCode::F(6), KeyModifiers::SHIFT, "Rename"),
        (KeyCode::F(8), KeyModifiers::SHIFT, "Delete permanently"),
    ] {
        assert!(press(&mut a, code, m).is_empty(), "{code:?}");
        let title = match &a.dialog {
            Some(Dialog::Input { title, .. } | Dialog::Confirm { title, .. }) => title.clone(),
            _ => panic!("{code:?} {m:?}: no dialog"),
        };
        assert_eq!(title, want, "{code:?} {m:?}");
        press(&mut a, KeyCode::Esc, NONE);
        assert!(a.dialog.is_none());
    }
    assert!(press(&mut a, KeyCode::F(8), NONE).is_empty());
    assert_eq!(status(&a), NO_REMOTE_TRASH);
    assert!(a.dialog.is_none());
    // Space: a walk on the server.
    let fx = press(&mut a, KeyCode::Char(' '), NONE);
    assert!(matches!(&fx[..], [Effect::RemoteSize(_)]), "{fx:?}");
    run(&mut a, fx);
    let p = a.panel();
    let i = p.list.find(b"sub").unwrap();
    assert_eq!(p.list.entries[i as usize].size, 1234);
    assert_eq!(p.marked, 1);
    // Alt+P: the path on the server.
    press(&mut a, KeyCode::Char('p'), ALT);
    let mut want = manycommander::cmdline::quote(&bytes(&dir.join("sub")));
    want.push(b' ');
    assert_eq!(a.line.bytes(), &want[..]);
    a.line.clear();
    // F5: the download dialog, then the job.
    let fx = press(&mut a, KeyCode::F(5), NONE);
    assert!(fx.is_empty());
    let Some(Dialog::Input { title, .. }) = &a.dialog else {
        panic!("the download dialog")
    };
    assert_eq!(title, "Download");
    let fx = press(&mut a, KeyCode::Enter, NONE);
    let [Effect::StartJob(spec)] = &fx[..] else {
        panic!("{fx:?}")
    };
    let rep = run_guarded(spec.clone(), &Sys::default(), &mut Script::silent());
    assert_eq!((rep.done, rep.dirs_done, rep.failed), (2, 2, 0), "{rep:?}");
    assert!(std::fs::read(local.join("sub/deeper/b")).unwrap() == noise(234, 2));
    close(r.session());
}

/// `Ctrl+T` on a remote tab shares its session (P3 5.7); a hidden remote tab releases it
/// and reopens its place through the pool when shown; nothing reconnects while the
/// session is open.
#[test]
fn tabs_share_and_release_the_session() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("browse-tabs");
    let dir = d.join("srv");
    std::fs::create_dir(&dir).unwrap();
    write(&dir.join("f"), b"f");
    let (r, _lost) = remote(&d.path);
    let mut a = app_with(&r, &d.path);
    cd_remote(&mut a, &dir);
    let before = Arc::strong_count(&r);
    let fx = press(&mut a, KeyCode::Char('t'), CTRL);
    let Some(Effect::ListRemote(req, _)) = fx.iter().find(|e| matches!(e, Effect::ListRemote(..)))
    else {
        panic!("{fx:?}")
    };
    assert!(
        Arc::ptr_eq(&req.remote, &r),
        "the new tab shares the session"
    );
    run(&mut a, fx);
    assert_eq!(a.sides[0].tabs.len(), 2);
    assert!(
        a.sides[0].tabs[0].reopen.is_some(),
        "the hidden tab keeps its place"
    );
    assert!(
        a.sides[0].tabs[0].remote().is_none(),
        "and holds no session"
    );
    assert_eq!(
        Arc::strong_count(&r),
        before,
        "one tab's hold moved to the other"
    );
    // Back to the first tab: reopened through the pool, no connect.
    let fx = press(&mut a, KeyCode::PageUp, CTRL);
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::Connect(..))),
        "{fx:?}"
    );
    run(&mut a, fx);
    assert!(a.panel().remote().is_some());
    assert_eq!(names(&a), vec![b"f".to_vec()]);
    close(r.session());
}

/// A remote panel's bookmark is its address (P3 5.1, hotlist `url`); `Enter` on it opens
/// the place through the pool. `z` never connects.
#[test]
fn bookmarks_hold_server_addresses() {
    use manycommander::dirs::Hotlist;
    use manycommander::ui::dialog::Dialog;
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("browse-bookmarks");
    let dir = d.join("my dir");
    std::fs::create_dir(&dir).unwrap();
    write(&dir.join("f"), b"f");
    let (r, _lost) = remote(&d.path);
    let mut a = app_with(&r, &d.path);
    a.dirs.hotlist = Hotlist::default();
    let mut t = b"cd sftp://srv".to_vec();
    t.extend_from_slice(&bytes(&d.path));
    t.extend_from_slice(b"/my%20dir");
    let fx = line(&mut a, &t);
    run(&mut a, fx);
    assert!(a.panel().remote().is_some(), "{}", status(&a));
    press(&mut a, KeyCode::Char('d'), CTRL);
    let fx = press(&mut a, KeyCode::Insert, NONE);
    let url = format!("sftp://srv{}/my%20dir", d.path.display());
    assert_eq!(
        fx,
        vec![Effect::SaveHotlist(vec![PathBuf::from(url.clone())])],
        "{fx:?}"
    );
    // The file format round-trips the address.
    let text = Hotlist::to_toml(&a.dirs.hotlist.dirs);
    assert!(text.contains(&format!("url = \"{url}\"")), "{text}");
    assert_eq!(Hotlist::parse(&text).unwrap(), a.dirs.hotlist.dirs);
    assert!(Hotlist::parse("[[dir]]\nurl = \"sftp://h?x\"\n").is_err());
    press(&mut a, KeyCode::Esc, NONE);
    // Leave the server, then go back by the bookmark.
    let fx = line(&mut a, &[b"cd ".as_slice(), &bytes(&d.path)].concat());
    run(&mut a, fx);
    assert!(a.panel().is_directory());
    press(&mut a, KeyCode::Char('d'), CTRL);
    let Some(Dialog::Dirs(dd)) = &a.dialog else {
        panic!("the directories dialog")
    };
    assert!(dd.row(0).is_some_and(|r| r.path() == Path::new(&url)));
    let fx = press(&mut a, KeyCode::Enter, NONE);
    assert!(matches!(&fx[..], [Effect::ListRemote(..)]), "{fx:?}");
    run(&mut a, fx);
    assert_eq!(
        a.panel().location(),
        bytes(&dir).iter().fold(b"sftp://srv".to_vec(), |mut v, b| {
            v.push(*b);
            v
        })
    );
    // `z` never connects: the frecency store and local bookmarks only.
    let fx = line(&mut a, b"z dir");
    assert!(
        !fx.iter()
            .any(|e| matches!(e, Effect::Connect(..) | Effect::ListRemote(..))),
        "{fx:?}"
    );
    close(r.session());
}

// ---- end to end: real ssh to `sshd -i` in a pty ------------------------------------------------

/// Through the real `ssh` to `sshd -i` behind a `ProxyCommand` (no network): `cd sftp://`
/// opens a remote panel with the server's rows and title; `Enter` and `Backspace` navigate
/// on the server; F3 views a file through the runtime view directory; F5 downloads a tree
/// into the other panel; F10 closes the session, and no process is left.
#[test]
fn end_to_end_browse_view_and_download_through_ssh() {
    use common::ssh::{Env, T as ST, have_tools, run_line};
    use common::tui::{ENTER, F3, F5, F10};
    if !have_tools() {
        return;
    }
    let e = Env::new("browse-e2e", "yes", true);
    let srv = e.home.join("srv");
    std::fs::create_dir_all(srv.join("sub")).unwrap();
    write(&srv.join("alpha.txt"), b"alpha\n");
    let payload = noise(3 << 20, 11);
    write(&srv.join("sub/beta.bin"), &payload);
    let dl = e.home.join("dl");
    std::fs::create_dir(&dl).unwrap();
    let rt = e.dir.join("rt");
    std::fs::create_dir(&rt).unwrap();
    std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o700)).unwrap();
    let viewed = e.dir.join("viewed");
    let pager = e.dir.join("pager.sh");
    std::fs::write(
        &pager,
        format!("#!/bin/sh\ncat \"$1\" > '{}'\n", viewed.display()),
    )
    .unwrap();
    std::fs::set_permissions(&pager, std::fs::Permissions::from_mode(0o755)).unwrap();
    let dl_s = dl.display().to_string();
    let mut t = e.tui(
        &[&dl_s, &dl_s],
        &[
            ("PAGER", pager.to_str().unwrap()),
            ("XDG_RUNTIME_DIR", rt.to_str().unwrap()),
        ],
    );
    let mut tr = common::sftp::Tracker::default();
    run_line(&mut t, &format!("cd sftp://mc-test{}", srv.display()));
    assert!(
        t.wait_for("connected to sftp://mc-test", ST),
        "{}\n{}",
        t.screen(),
        String::from_utf8_lossy(&t.raw)
    );
    assert!(t.wait_for(" alpha ", ST), "{}", t.screen());
    assert!(t.wait_until(ST, |t| !t.screen().contains("(loading)")));
    tr.scan(t.pid());
    assert!(t.screen().contains("srv"), "{}", t.screen());
    // Rows: `..`, `sub`, `alpha.txt`. Into `sub` and back.
    t.keys(&[common::tui::DOWN, ENTER]);
    assert!(t.wait_for(" beta ", ST), "{}", t.screen());
    t.keys(&[b"\x7f"]);
    assert!(t.wait_for(" alpha ", ST), "{}", t.screen());
    // F3 on `alpha.txt`: a copy through the view directory, then the pager.
    t.keys(&[common::tui::DOWN, F3]);
    assert!(
        t.wait_until(ST, |_| std::fs::read(&viewed)
            .is_ok_and(|b| b == b"alpha\n")),
        "{}",
        t.screen()
    );
    assert!(t.wait_for("10Quit", ST));
    // F5 on `sub`: the download dialog, then the job.
    t.keys(&[b"\x1b[A"]);
    t.keys(&[F5]);
    assert!(t.wait_for("Download", ST), "{}", t.screen());
    t.keys(&[ENTER]);
    assert!(
        t.wait_until(ST, |_| std::fs::read(dl.join("sub/beta.bin"))
            .is_ok_and(|b| b == payload)),
        "{}",
        t.screen()
    );
    assert!(partials(&dl).is_empty());
    // The view copy went after the pager exited.
    let copies: Vec<PathBuf> = walk(&rt).into_iter().filter(|p| p.is_file()).collect();
    assert!(copies.is_empty(), "{copies:?}");
    tr.scan(t.pid());
    t.keys(&[F10]);
    assert_eq!(t.wait_exit(ST), Some(0), "{}", t.screen());
    let left = tr.wait_gone(ST);
    assert!(left.is_empty(), "processes left behind: {left:?}");
}

// ---- measurements (release build, run by hand) ---------------------------------------------

/// P-27 and P-26 by hand: a 10,000-entry listing through `sftp-server` on pipes (first rows
/// and complete), the same behind a 30 ms round trip against its 103 round trips, and a
/// 256 MiB download through the app's path (the copy engine with a remote origin) against
/// `sftp -D sftp-server`. `cargo test --release --test sftp_browse -- --ignored --nocapture
/// measure`.
#[test]
#[ignore]
fn measure_listing_and_download() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("browse-measure");
    let big = d.join("big");
    std::fs::create_dir(&big).unwrap();
    for i in 0..10_000 {
        std::fs::File::create(big.join(format!("f{i:05}"))).unwrap();
    }
    let (r, _lost) = remote(&d.path);
    for _ in 0..3 {
        let started = Instant::now();
        let msgs = list_msgs(&list_req(&r, RemoteDir::Absolute(vpath(&big)), false));
        let first = msgs
            .iter()
            .find(|(_, m)| matches!(m, ListingMsg::Batch { .. }))
            .unwrap()
            .0;
        let done = msgs
            .iter()
            .find(|(_, m)| matches!(m, ListingMsg::Done { .. }))
            .unwrap()
            .0;
        eprintln!(
            "10,000 entries, pipes: first rows {:.2} ms, complete {:.1} ms",
            (first - started).as_secs_f64() * 1000.0,
            (done - started).as_secs_f64() * 1000.0
        );
    }
    close(r.session());
    let (s, _lost) = sftp_server_with_latency(&d.path, Duration::from_millis(15));
    let r = Arc::new(RemoteProvider::new(s, target("srv")));
    let started = Instant::now();
    let msgs = list_msgs(&list_req(&r, RemoteDir::Absolute(vpath(&big)), false));
    let first = msgs
        .iter()
        .find(|(_, m)| matches!(m, ListingMsg::Batch { .. }))
        .unwrap()
        .0;
    let done = msgs
        .iter()
        .find(|(_, m)| matches!(m, ListingMsg::Done { .. }))
        .unwrap()
        .0;
    let rts = 103.0 * 30.0;
    eprintln!(
        "10,000 entries, 30 ms round trip: first rows {:.0} ms, complete {:.0} ms = {:.3}x of 103 round trips",
        (first - started).as_secs_f64() * 1000.0,
        (done - started).as_secs_f64() * 1000.0,
        (done - started).as_secs_f64() * 1000.0 / rts
    );
    close(r.session());

    let n = 256usize << 20;
    let src = d.join("src");
    std::fs::create_dir(&src).unwrap();
    write(&src.join("payload"), &noise(n, 1));
    let (r, _lost) = remote(&d.path);
    let mut ours = Duration::MAX;
    for k in 0..3 {
        let dst = d.join(format!("ours{k}"));
        std::fs::create_dir(&dst).unwrap();
        let t = Instant::now();
        let rep = download(&r, &src, &["payload"], &dst, &mut Script::silent());
        ours = ours.min(t.elapsed());
        assert_eq!(rep.done, 1, "{rep:?}");
        std::fs::remove_dir_all(&dst).unwrap();
    }
    close(r.session());
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
                writeln!(i, "get {} {}", src.join("payload").display(), out.display())?;
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
        "256 MiB download: app path {:.0} ms ({:.0} MiB/s), sftp -D {:.0} ms ({:.0} MiB/s), ratio {:.2}",
        ours.as_secs_f64() * 1000.0,
        mib / ours.as_secs_f64(),
        theirs.as_secs_f64() * 1000.0,
        mib / theirs.as_secs_f64(),
        ours.as_secs_f64() / theirs.as_secs_f64()
    );
}
