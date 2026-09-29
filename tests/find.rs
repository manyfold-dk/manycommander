//! T6: find (P2 5). A-FD-1 (name search: substring and glob, the case rules, hidden
//! entries, every kind of entry, `.` and `..`, symlinks and symlink loops, a bind mount
//! inside the tree and a bind-mount cycle), A-FD-2 (content: literal and case-folded, a
//! chunk boundary, symlinks and FIFOs never read, an unreadable file counted, the holes of a
//! sparse file not read), A-FD-3 (the results tab: escaped names, grouped F5 and F8, Enter
//! and `Alt+Left`, `Ctrl+R`, the refused keys) and A-FD-4 (`Esc` stops a search within
//! 100 ms and keeps its results). The app-level tests run the effects synchronously, as
//! `tests/app.rs` does.

mod common;

use common::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use manycommander::app::App;
use manycommander::app::event::{Effect, Event, JobEvent};
use manycommander::config::Config;
use manycommander::find::{self, CHUNK, FindMsg, FindSpec, Search, Stats};
use manycommander::fsops::group::Group;
use manycommander::fsops::job::{JobSpec, Report, run_guarded};
use manycommander::fsops::question::{Answer, Question};
use manycommander::fsops::sys::{FsIdentity, Kind, Meta, Sys, Ts};
use manycommander::fsops::trash::trash_groups_with;
use manycommander::panel::entry::{EKind, Entry};
use manycommander::panel::listing;
use manycommander::theme::Depth;
use manycommander::ui::dialog::Dialog;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::ffi::OsString;
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---- the engine -------------------------------------------------------------------------

fn spec(root: &Path, name: &str) -> FindSpec {
    FindSpec {
        root: root.to_path_buf(),
        name: name.as_bytes().to_vec(),
        content: None,
        hidden: true,
        stay_on_fs: true,
        match_case: false,
    }
}

fn content(root: &Path, name: &str, text: &[u8], match_case: bool) -> FindSpec {
    FindSpec {
        content: Some(text.to_vec()),
        match_case,
        ..spec(root, name)
    }
}

/// Runs a search to its end on this thread: every result (relative name as bytes, and
/// kind), sorted by name, and the totals.
fn search_raw(spec: FindSpec) -> (Vec<(Vec<u8>, EKind)>, Stats) {
    let s = Search::new(1, spec);
    let out = Mutex::new((Vec::new(), None));
    find::guarded(&s, &|m| {
        let mut o = out.lock().unwrap();
        match m {
            FindMsg::Batch { entries, names, .. } => {
                assert!(o.1.is_none(), "no batch after Done");
                for e in &entries {
                    o.0.push((e.name(&names).to_vec(), e.kind));
                }
            }
            FindMsg::Done { stats, .. } => o.1 = Some(stats),
        }
    });
    assert!(!s.alive.is_running(), "the threads returned");
    let (mut v, stats) = out.into_inner().unwrap();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    let stats = stats.expect("Done came last");
    assert_eq!(stats.results, v.len() as u64, "{stats:?}");
    assert_eq!(
        s.stats(),
        Some(&stats),
        "the totals are recorded in the search"
    );
    (v, stats)
}

/// [`search_raw`] with the names shown lossily.
fn search_all(spec: FindSpec) -> (Vec<(String, EKind)>, Stats) {
    let (v, stats) = search_raw(spec);
    let v = v
        .into_iter()
        .map(|(n, k)| (String::from_utf8_lossy(&n).into_owned(), k))
        .collect();
    (v, stats)
}

/// The sorted relative names a search finds.
fn found(spec: FindSpec) -> Vec<String> {
    search_all(spec).0.into_iter().map(|(n, _)| n).collect()
}

/// `found` on a thread, failing when it does not finish within 10 s (a FIFO opened for
/// reading would block forever).
fn found_within(spec: FindSpec) -> Vec<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(found(spec));
    });
    rx.recv_timeout(Duration::from_secs(10))
        .expect("the search finished")
}

fn mkdirs(t: &TestDir, dirs: &[&str]) {
    for d in dirs {
        std::fs::create_dir_all(t.join(d)).unwrap();
    }
}

fn files(t: &TestDir, files: &[(&str, &[u8])]) {
    for (f, c) in files {
        write(&t.join(f), c);
    }
}

fn mkfifo(p: &Path) {
    rustix::fs::mknodat(
        rustix::fs::CWD,
        p,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o644),
        0,
    )
    .unwrap();
}

#[test]
fn a_fd_1_substring_and_glob_with_the_case_rules() {
    let t = test_dir("find-names");
    mkdirs(&t, &["src/deep", "docs"]);
    files(
        &t,
        &[
            ("Readme.MD", b""),
            ("src/main.rs", b""),
            ("src/Lib.RS", b""),
            ("src/deep/mod.rs", b""),
            ("docs/readme.txt", b""),
        ],
    );
    write(&t.join("docs").join(os(b"bad\xffname")), b"");
    let r = &t.path;
    assert_eq!(found(spec(r, "readme")), ["Readme.MD", "docs/readme.txt"]);
    let case = |name: &str| FindSpec {
        match_case: true,
        ..spec(r, name)
    };
    assert_eq!(found(case("readme")), ["docs/readme.txt"]);
    assert_eq!(found(case("Readme")), ["Readme.MD"]);
    assert_eq!(
        found(spec(r, "*.rs")),
        ["src/Lib.RS", "src/deep/mod.rs", "src/main.rs"],
        "a glob matches the whole name, folding ASCII case"
    );
    assert_eq!(found(case("*.rs")), ["src/deep/mod.rs", "src/main.rs"]);
    assert_eq!(found(spec(r, "m*")), ["src/deep/mod.rs", "src/main.rs"]);
    assert_eq!(found(spec(r, "?ib.rs")), ["src/Lib.RS"]);
    assert_eq!(found(spec(r, "[d-e]*")), ["docs", "src/deep"]);
    assert_eq!(found(spec(r, "E.m")), ["Readme.MD"]);
    // Names are bytes: invalid UTF-8 matches byte by byte.
    let (raw, _) = search_raw(FindSpec {
        name: b"\xffn".to_vec(),
        ..spec(r, "")
    });
    assert_eq!(raw, [(b"docs/bad\xffname".to_vec(), EKind::File)]);
    // An empty name matches everything below the root.
    assert_eq!(found(spec(r, "")).len(), 9);
}

#[test]
fn a_fd_1_hidden_entries_and_never_dot_or_dotdot() {
    let t = test_dir("find-hidden");
    mkdirs(&t, &[".git/objects", "visible/.cache"]);
    files(
        &t,
        &[
            (".git/config", b""),
            (".hidden", b""),
            ("visible/.x", b""),
            ("visible/y", b""),
            ("visible/.cache/z", b""),
        ],
    );
    let off = FindSpec {
        hidden: false,
        ..spec(&t.path, "")
    };
    assert_eq!(
        found(off),
        ["visible", "visible/y"],
        "hidden names are neither matched nor descended"
    );
    let (all, stats) = search_all(spec(&t.path, ""));
    let names: Vec<&str> = all.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            ".git",
            ".git/config",
            ".git/objects",
            ".hidden",
            "visible",
            "visible/.cache",
            "visible/.cache/z",
            "visible/.x",
            "visible/y",
        ]
    );
    for n in &names {
        assert!(
            n.split('/').all(|c| c != "." && c != ".."),
            "`.` and `..` never appear: {n}"
        );
    }
    assert_eq!(stats.dirs, 5, "each directory read once: {stats:?}");
    assert_eq!(
        found(spec(&t.path, ".")),
        [".git", ".hidden", "visible/.cache", "visible/.x"],
        "the pattern matches the entry's name, not its path"
    );
}

#[test]
fn a_fd_1_files_directories_symlinks_and_special_files_match_by_name() {
    let t = test_dir("find-kinds");
    mkdirs(&t, &["kdir"]);
    files(&t, &[("kfile", b"x"), ("other", b"")]);
    symlink("kfile", t.join("klink")).unwrap();
    symlink("nowhere", t.join("kbroken")).unwrap();
    mkfifo(&t.join("kfifo"));
    let _sock = std::os::unix::net::UnixListener::bind(t.join("ksock")).unwrap();
    let (all, _) = search_all(spec(&t.path, "k"));
    assert_eq!(
        all,
        [
            ("kbroken".to_string(), EKind::Symlink),
            ("kdir".to_string(), EKind::Dir),
            ("kfifo".to_string(), EKind::Special),
            ("kfile".to_string(), EKind::File),
            ("klink".to_string(), EKind::Symlink),
            ("ksock".to_string(), EKind::Special),
        ]
    );
}

#[test]
fn a_fd_1_symlinked_directories_and_symlink_loops_are_not_followed() {
    let t = test_dir("find-symlinks");
    mkdirs(&t, &["real/inner", "outside"]);
    files(&t, &[("real/target.txt", b""), ("outside/target.txt", b"")]);
    symlink("real", t.join("linkdir")).unwrap();
    symlink(".", t.join("loop")).unwrap();
    symlink("..", t.join("real/back")).unwrap();
    symlink(t.join("outside"), t.join("real/out")).unwrap();
    let (all, stats) = search_all(spec(&t.path, ""));
    let names: Vec<&str> = all.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "linkdir",
            "loop",
            "outside",
            "outside/target.txt",
            "real",
            "real/back",
            "real/inner",
            "real/out",
            "real/target.txt",
        ]
    );
    assert_eq!(stats.dirs, 4, "root, outside, real, real/inner: {stats:?}");
    assert_eq!(stats.errors, 0);
    assert_eq!(
        found(spec(&t.join("real"), "target")),
        ["target.txt"],
        "nothing is found through a symlink"
    );
}

#[test]
fn a_fd_1_bind_mount_inside_the_tree_is_not_descended_with_stay_on_fs() {
    if !in_userns("a_fd_1_bind_mount_inside_the_tree_is_not_descended_with_stay_on_fs") {
        return;
    }
    let t = test_dir("find-bind");
    mkdirs(&t, &["outside/deeper", "tree/m"]);
    files(
        &t,
        &[
            ("outside/precious", b""),
            ("outside/deeper/x", b""),
            ("tree/own", b""),
        ],
    );
    bind_mount(&t.join("outside"), &t.join("tree/m"));
    let root = t.join("tree");
    let stay = found(spec(&root, ""));
    let cross = found(FindSpec {
        stay_on_fs: false,
        ..spec(&root, "")
    });
    umount(&t.join("tree/m"));
    assert_eq!(stay, ["m", "own"], "the mount point matches by name only");
    assert_eq!(cross, ["m", "m/deeper", "m/deeper/x", "m/precious", "own"]);
}

#[test]
fn a_fd_1_bind_mount_cycle_visits_each_directory_once() {
    if !in_userns("a_fd_1_bind_mount_cycle_visits_each_directory_once") {
        return;
    }
    let t = test_dir("find-cycle");
    mkdirs(&t, &["tree/a/b", "tree/a/loop", "tree/c"]);
    files(&t, &[("tree/a/b/f", b""), ("tree/c/g", b"")]);
    bind_mount(&t.join("tree"), &t.join("tree/a/loop"));
    let root = t.join("tree");
    let runs: Vec<_> = [true, false]
        .into_iter()
        .map(|stay| {
            search_all(FindSpec {
                stay_on_fs: stay,
                ..spec(&root, "")
            })
        })
        .collect();
    umount(&t.join("tree/a/loop"));
    for (all, stats) in runs {
        let names: Vec<&str> = all.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["a", "a/b", "a/b/f", "a/loop", "c", "c/g"]);
        assert_eq!(stats.dirs, 4, "tree, a, a/b, c, each once: {stats:?}");
    }
}

#[test]
fn a_fd_2_literal_and_case_folded_content() {
    let t = test_dir("find-content");
    mkdirs(&t, &["Hello"]);
    files(
        &t,
        &[
            ("one.txt", b"Hello World\n"),
            ("two.txt", b"hello world"),
            ("three.bin", b"\x00\x01Hello\xff\xfe"),
            ("other.md", b"Hello"),
            ("none.txt", b"Help"),
            ("Hello/inside.txt", b"HELLO"),
        ],
    );
    let r = &t.path;
    assert_eq!(
        found(content(r, "", b"hello", false)),
        [
            "Hello/inside.txt",
            "one.txt",
            "other.md",
            "three.bin",
            "two.txt"
        ],
        "binary files are searched like text; directories are not read"
    );
    assert_eq!(
        found(content(r, "", b"Hello", true)),
        ["one.txt", "other.md", "three.bin"]
    );
    assert_eq!(
        found(content(r, "*.txt", b"WORLD", false)),
        ["one.txt", "two.txt"],
        "only files whose name matches are read"
    );
    assert_eq!(found(content(r, "", b"\x01Hel", true)), ["three.bin"]);
    assert!(found(content(r, "", b"absent", false)).is_empty());
}

#[test]
fn a_fd_2_a_match_across_a_chunk_boundary_is_found() {
    let t = test_dir("find-boundary");
    let needle = b"BOUNDARY";
    for (name, at) in [
        ("across.bin", CHUNK - 3),
        ("end.bin", 2 * CHUNK - needle.len()),
        ("start2.bin", CHUNK),
        ("before.bin", CHUNK - needle.len()),
    ] {
        let mut data = vec![b'.'; 2 * CHUNK];
        data[at..at + needle.len()].copy_from_slice(needle);
        write(&t.join(name), &data);
    }
    let mut miss = vec![b'.'; 2 * CHUNK];
    miss[CHUNK - 4..CHUNK].copy_from_slice(b"BOUN");
    miss[CHUNK + 10..CHUNK + 14].copy_from_slice(b"DARY");
    write(&t.join("miss.bin"), &miss);
    for case in [true, false] {
        assert_eq!(
            found(content(&t.path, "", needle, case)),
            ["across.bin", "before.bin", "end.bin", "start2.bin"],
            "match_case={case}"
        );
    }
}

#[test]
fn a_fd_2_symlinks_are_not_read_and_a_fifo_is_never_opened() {
    let t = test_dir("find-fifo");
    mkdirs(&t, &["outside", "tree"]);
    files(
        &t,
        &[
            ("outside/secret.txt", b"NEEDLE"),
            ("tree/real.txt", b"a NEEDLE"),
        ],
    );
    symlink(t.join("outside/secret.txt"), t.join("tree/link.txt")).unwrap();
    mkfifo(&t.join("tree/fifo.txt"));
    let found = found_within(content(&t.join("tree"), "", b"NEEDLE", true));
    assert_eq!(found, ["real.txt"]);
}

#[test]
fn a_fd_2_an_unreadable_file_counts_as_an_error() {
    if rustix::process::geteuid().is_root() {
        skip("running as root: mode 000 does not stop reading");
        return;
    }
    let t = test_dir("find-unreadable");
    files(&t, &[("locked.txt", b"NEEDLE"), ("open.txt", b"NEEDLE")]);
    std::fs::set_permissions(t.join("locked.txt"), std::fs::Permissions::from_mode(0o000)).unwrap();
    mkdirs(&t, &["closed"]);
    files(&t, &[("closed/inner.txt", b"NEEDLE")]);
    std::fs::set_permissions(t.join("closed"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let (all, stats) = search_all(content(&t.path, "", b"NEEDLE", true));
    assert_eq!(all, [("open.txt".to_string(), EKind::File)]);
    assert_eq!(stats.errors, 2, "the file and the directory: {stats:?}");
    std::fs::set_permissions(t.join("closed"), std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_fd_2_a_sparse_files_data_is_found_without_reading_its_holes() {
    let t = test_dir("find-sparse");
    let p = t.join("sparse.bin");
    let at = 3u64 << 30;
    {
        let mut f = std::fs::File::create(&p).unwrap();
        f.set_len(4 << 30).unwrap();
        f.seek(SeekFrom::Start(at)).unwrap();
        f.write_all(b"XNEEDLE and more data").unwrap();
    }
    let m = std::fs::metadata(&p).unwrap();
    if m.blocks() * 512 >= m.size() {
        skip("target/test-tmp does not keep holes");
        return;
    }
    let start = Instant::now();
    assert_eq!(
        found(content(&t.path, "", b"XNEEDLE", true)),
        ["sparse.bin"]
    );
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "4 GiB of holes were not read: {:?}",
        start.elapsed()
    );
    // The byte before the data is a hole: a needle that needs it is not found (the F1 help
    // states it).
    assert!(found(content(&t.path, "", b"\0XNEEDLE", true)).is_empty());
    assert!(start.elapsed() < Duration::from_secs(2));
}

/// A tree whose content search takes seconds: 64 links to one 64 MiB file without the
/// needle, and eight small files with it at the root, which the search reads first.
fn slow_tree(t: &TestDir) {
    mkdirs(t, &["links"]);
    let big = t.join("big.dat");
    {
        let mut f = std::fs::File::create(&big).unwrap();
        let block = vec![b'a'; 1 << 20];
        for _ in 0..64 {
            f.write_all(&block).unwrap();
        }
    }
    for i in 0..64 {
        std::fs::hard_link(&big, t.join(format!("links/l{i}"))).unwrap();
    }
    for i in 0..8 {
        write(&t.join(format!("hit{i}")), b"a NEEDLE here");
    }
}

#[test]
fn a_fd_4_cancel_stops_a_large_search_within_100_ms() {
    let t = test_dir("find-cancel");
    slow_tree(&t);
    let s = Arc::new(Search::new(7, content(&t.path, "", b"NEEDLE", true)));
    let (tx, rx) = std::sync::mpsc::channel();
    find::spawn(s.clone(), move |m| {
        let _ = tx.send(m);
    })
    .unwrap();
    let mut results = 0;
    match rx.recv_timeout(Duration::from_secs(30)).unwrap() {
        FindMsg::Batch { entries, .. } => results += entries.len(),
        FindMsg::Done { stats, .. } => panic!("finished before the cancel: {stats:?}"),
    }
    let cancelled = Instant::now();
    s.cancel();
    let stats = loop {
        match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            FindMsg::Batch { entries, .. } => results += entries.len(),
            FindMsg::Done { stats, .. } => break stats,
        }
    };
    let took = cancelled.elapsed();
    assert!(took < Duration::from_millis(100), "stopped after {took:?}");
    assert!(stats.cancelled && !stats.truncated, "{stats:?}");
    assert!(results >= 1, "the results found before the cancel are kept");
    assert_eq!(stats.results, results as u64);
    for _ in 0..100 {
        if !s.alive.is_running() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!s.alive.is_running(), "every thread returned");
}

// ---- the results tab --------------------------------------------------------------------

fn app(left: &Path, right: &Path) -> App {
    App::new(
        left.to_path_buf(),
        right.to_path_buf(),
        right.to_path_buf(),
        Config::default(),
        None,
        Depth::NoColor,
        jiff::tz::TimeZone::UTC,
    )
}

/// Performs effects synchronously: listings, re-stats and searches run on this thread (a
/// search's pool still runs its workers) and their messages go back into the app. Returns
/// the effects that were not performed.
fn run(app: &mut App, fx: Vec<Effect>) -> Vec<Effect> {
    let mut rest = Vec::new();
    for e in fx {
        match e {
            Effect::List(req, alive) => {
                let msgs = std::cell::RefCell::new(Vec::new());
                listing::guarded(&req, &|m| msgs.borrow_mut().push(m), listing::list);
                alive.finish();
                for m in msgs.into_inner() {
                    let more = app.update(Event::Listing(m));
                    rest.extend(run(app, more));
                }
            }
            Effect::Restat(req, alive) => {
                let msgs = std::cell::RefCell::new(Vec::new());
                find::restat_guarded(&req, &|m| msgs.borrow_mut().push(m));
                alive.finish();
                for m in msgs.into_inner() {
                    let more = app.update(Event::Listing(m));
                    rest.extend(run(app, more));
                }
            }
            Effect::Find(s) => {
                let msgs = Mutex::new(Vec::new());
                find::guarded(&s, &|m| msgs.lock().unwrap().push(m));
                for m in msgs.into_inner().unwrap() {
                    let more = app.update(Event::Find(m));
                    rest.extend(run(app, more));
                }
            }
            other => rest.push(other),
        }
    }
    rest
}

fn press_with(a: &mut App, code: KeyCode, m: KeyModifiers) -> Vec<Effect> {
    a.update(Event::Key(KeyEvent::new(code, m), Instant::now()))
}

fn press(a: &mut App, code: KeyCode) -> Vec<Effect> {
    press_with(a, code, KeyModifiers::NONE)
}

fn typed(a: &mut App, s: &str) {
    for c in s.chars() {
        press(a, KeyCode::Char(c));
    }
}

/// `Alt+F7`, the name, `Tab`, the text, `Enter`: the effects of the submit.
fn find_keys(a: &mut App, name: &str, text: &str) -> Vec<Effect> {
    press_with(a, KeyCode::F(7), KeyModifiers::ALT);
    assert!(matches!(a.dialog, Some(Dialog::Form { .. })));
    typed(a, name);
    press(a, KeyCode::Tab);
    typed(a, text);
    press(a, KeyCode::Enter)
}

/// Starts a search from the form and runs it to its end.
fn find_run(a: &mut App, name: &str, text: &str) {
    let fx = find_keys(a, name, text);
    assert!(a.dialog.is_none(), "the form closed");
    assert!(fx.iter().any(|e| matches!(e, Effect::Find(_))), "{fx:?}");
    let rest = run(a, fx);
    assert!(
        rest.iter()
            .all(|e| matches!(e, Effect::Watch { dir: None, .. })),
        "{rest:?}"
    );
    a.panel_mut().ensure_sorted();
}

fn started(left: &Path, right: &Path) -> App {
    let mut a = app(left, right);
    let fx = a.start();
    run(&mut a, fx);
    a.panel_mut().ensure_sorted();
    a
}

fn visible(a: &App) -> Vec<String> {
    let p = a.panel();
    p.list
        .visible
        .iter()
        .map(|&i| String::from_utf8_lossy(p.list.name(i)).into_owned())
        .collect()
}

fn cursor_name(a: &App) -> String {
    String::from_utf8_lossy(a.panel().current_name().unwrap_or(b"..")).into_owned()
}

fn render(a: &mut App, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
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

fn group(root: &Path, sub: &[&str], names: &[&str]) -> Group {
    Group {
        root: root.into(),
        sub: sub.iter().map(OsString::from).collect(),
        names: names.iter().map(OsString::from).collect(),
    }
}

/// The verb a key starts in the active panel (after its dialog, with `Enter`), ended at
/// once so the next can start.
fn verb(a: &mut App, code: KeyCode, m: KeyModifiers) -> JobSpec {
    press_with(a, code, m);
    assert!(a.dialog.is_some(), "{:?}", a.status);
    let fx = press(a, KeyCode::Enter);
    let [Effect::StartJob(spec)] = &fx[..] else {
        panic!("{fx:?}");
    };
    let spec = spec.clone();
    let fx = a.update(Event::Job(JobEvent::Done(Report::new(spec.verb()))));
    run(a, fx);
    a.panel_mut().ensure_sorted();
    spec
}

fn meta(kind: Kind, size: u64, mtime: i64) -> Meta {
    let ts = Ts {
        sec: mtime,
        nsec: 0,
    };
    Meta {
        kind,
        perm: if kind == Kind::Dir { 0o755 } else { 0o644 },
        uid: 1000,
        nlink: 1,
        size,
        blocks: size.div_ceil(512),
        id: FsIdentity::default(),
        atime: ts,
        mtime: ts,
        ctime: ts,
        mount_root: false,
        automount: false,
    }
}

/// A-FD-3: names with a newline and invalid UTF-8 are shown escaped; the name column
/// shows the relative path and the extension column the leaf's extension; the title names
/// the search and the footer counts results and errors.
#[test]
fn a_fd_3_results_tab_snapshot_with_escaped_names() {
    let mut a = app(Path::new("/snap/root"), Path::new("/snap/other"));
    let fx = find_keys(&mut a, "e", "");
    let Some(Effect::Find(s)) = fx.iter().find(|e| matches!(e, Effect::Find(_))) else {
        panic!("{fx:?}");
    };
    let s = s.clone();
    assert_eq!(s.spec.root, Path::new("/snap/root"));
    let mut names = Vec::new();
    let rows: [(&[u8], Kind, u64); 6] = [
        (b"src/deep", Kind::Dir, 0),
        (b"src/main.rs", Kind::File, 1234),
        (b"docs.d/readme", Kind::File, 99),
        (b"new\nline/file.txt", Kind::File, 5),
        (b"bad\xff\xfe/utf8.txt", Kind::File, 6),
        (b"x/link", Kind::Symlink, 8),
    ];
    let entries: Vec<Entry> = rows
        .iter()
        .enumerate()
        .map(|(i, (n, k, size))| {
            Entry::new(
                &mut names,
                n,
                &meta(*k, *size, 1_790_000_000 + i as i64 * 86_400),
            )
        })
        .collect();
    a.update(Event::Find(FindMsg::Batch {
        id: s.id,
        entries,
        names,
    }));
    a.update(Event::Find(FindMsg::Done {
        id: s.id,
        stats: Stats {
            dirs: 3,
            files: 40,
            errors: 2,
            results: 6,
            ..Stats::default()
        },
    }));
    a.panel_mut().cursor_to_name(b"src/main.rs");
    let screen = render(&mut a, 80, 24);
    assert!(screen.contains("new\\nline/file"), "{screen}");
    assert!(screen.contains("bad\\xff\\xfe/utf8"), "{screen}");
    insta::assert_snapshot!(screen);
    // Wide: the extension column shows the leaf's extension, `docs.d/readme` has none.
    insta::assert_snapshot!("results_tab_wide", render(&mut a, 200, 12));
}

/// A-FD-3: F5 of results from three directories copies all of them into the other panel,
/// as one group per directory; the second of two same-named results raises "file exists".
#[test]
fn a_fd_3_copy_of_results_from_three_directories() {
    let t = test_dir("find-app-copy");
    mkdirs(&t, &["root/a", "root/b", "root/c", "dst"]);
    files(
        &t,
        &[
            ("root/a/x", b"ax"),
            ("root/b/x", b"bx"),
            ("root/c/y", b"cy"),
            ("root/c/z", b"cz"),
        ],
    );
    let root = t.join("root");
    let mut a = started(&root, &t.join("dst"));
    find_run(&mut a, "[xy]", "");
    assert_eq!(visible(&a), ["a/x", "b/x", "c/y"]);
    press_with(&mut a, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert_eq!(a.panel().marked, 3);
    let spec = verb(&mut a, KeyCode::F(5), KeyModifiers::NONE);
    assert_eq!(
        spec,
        JobSpec::Copy {
            groups: vec![
                group(&root, &["a"], &["x"]),
                group(&root, &["b"], &["x"]),
                group(&root, &["c"], &["y"]),
            ],
            dst: t.join("dst").into(),
        }
    );
    let mut ui = Script::new([Answer::Skip]);
    let r = run_guarded(spec, &Sys::default(), &mut ui);
    assert_eq!(std::fs::read(t.join("dst/x")).unwrap(), b"ax");
    assert_eq!(std::fs::read(t.join("dst/y")).unwrap(), b"cy");
    assert!(!t.join("dst/z").exists());
    assert_eq!(ui.asked.len(), 1, "{:?}", ui.asked);
    assert!(
        matches!(&ui.asked[0], Question::FileExists { path, .. } if *path == t.join("dst/x")),
        "{:?}",
        ui.asked
    );
    assert_eq!((r.done, r.skipped), (2, 1), "{r:?}");
}

/// A-FD-3: F8 trashes results from two directories.
#[test]
fn a_fd_3_trash_of_results_from_two_directories() {
    let t = test_dir("find-app-trash");
    mkdirs(&t, &["root/a", "root/b", "data"]);
    files(
        &t,
        &[
            ("root/a/f1", b"1"),
            ("root/b/f2", b"2"),
            ("root/b/keep", b"k"),
        ],
    );
    let root = t.join("root");
    let mut a = started(&root, &root);
    find_run(&mut a, "f", "");
    assert_eq!(visible(&a), ["a/f1", "b/f2"]);
    press_with(&mut a, KeyCode::Char('a'), KeyModifiers::CONTROL);
    let JobSpec::Trash { groups } = verb(&mut a, KeyCode::F(8), KeyModifiers::NONE) else {
        panic!("a trash job");
    };
    assert_eq!(
        groups,
        [group(&root, &["a"], &["f1"]), group(&root, &["b"], &["f2"])]
    );
    let mut ui = Script::silent();
    let r = trash_groups_with(&Sys::default(), &mut ui, &groups, Some(&t.join("data")));
    assert_eq!((r.done, r.failed), (2, 0), "{r:?}");
    assert!(!t.join("root/a/f1").exists() && !t.join("root/b/f2").exists());
    assert!(t.join("root/b/keep").exists());
    assert_eq!(std::fs::read(t.join("data/Trash/files/f1")).unwrap(), b"1");
    assert_eq!(std::fs::read(t.join("data/Trash/files/f2")).unwrap(), b"2");
}

/// A-FD-3, P2 5.4: Enter on a file goes to its directory with the cursor on it, Enter on a
/// directory opens it, Backspace goes to the root; `Alt+Left` returns to the results each
/// time and `Alt+Right` goes forward again.
#[test]
fn a_fd_3_enter_goes_to_the_file_and_alt_left_returns() {
    let t = test_dir("find-app-enter");
    mkdirs(&t, &["root/a/sub", "root/b"]);
    files(
        &t,
        &[("root/a/x", b""), ("root/a/w", b""), ("root/b/y", b"")],
    );
    let root = t.join("root");
    let mut a = started(&root, &root);
    find_run(&mut a, "", "");
    let all = ["a", "a/sub", "b", "a/w", "a/x", "b/y"];
    assert_eq!(visible(&a), all);
    assert_eq!(a.panel().rows(), 6, "no `..` row");
    let alt = KeyModifiers::ALT;
    let back = |a: &mut App| {
        let fx = press_with(a, KeyCode::Left, alt);
        assert!(fx.iter().any(|e| matches!(e, Effect::Restat(..))), "{fx:?}");
        run(a, fx);
        a.panel_mut().ensure_sorted();
        assert!(!a.panel().is_directory(), "back in the results");
        assert_eq!(visible(a), all);
    };

    a.panel_mut().cursor_to_name(b"a/x");
    let fx = press(&mut a, KeyCode::Enter);
    run(&mut a, fx);
    assert!(a.panel().is_directory());
    assert_eq!(a.panel().dir, root.join("a"));
    assert_eq!(cursor_name(&a), "x");
    back(&mut a);
    assert_eq!(cursor_name(&a), "a/x", "the cursor is where it was");
    // Forward to the directory again, and back.
    let fx = press_with(&mut a, KeyCode::Right, alt);
    run(&mut a, fx);
    assert_eq!(a.panel().dir, root.join("a"));
    assert!(a.panel().is_directory());
    back(&mut a);

    a.panel_mut().cursor_to_name(b"a/sub");
    let fx = press(&mut a, KeyCode::Enter);
    run(&mut a, fx);
    assert_eq!(a.panel().dir, root.join("a/sub"));
    back(&mut a);

    a.panel_mut().cursor_to_name(b"b/y");
    let fx = press(&mut a, KeyCode::Backspace);
    run(&mut a, fx);
    assert_eq!(a.panel().dir, root);
    assert_eq!(
        cursor_name(&a),
        "b",
        "the cursor on the result's first component"
    );
    back(&mut a);
    let fx = press_with(&mut a, KeyCode::Up, alt);
    run(&mut a, fx);
    assert_eq!(a.panel().dir, root, "Alt+Up goes to the root too");
    assert_eq!(a.panel().history.results_places(), 1);
}

/// A-FD-3, P2 5.5: Ctrl+R re-stats: vanished results and results whose directory is now a
/// symlink are dropped, the rest get fresh metadata, marks survive by name.
#[test]
fn a_fd_3_ctrl_r_drops_vanished_results() {
    let t = test_dir("find-app-restat");
    mkdirs(&t, &["root/a", "root/b", "root/c"]);
    files(
        &t,
        &[
            ("root/a/x", b"x"),
            ("root/a/y", b"y"),
            ("root/b/z", b"z"),
            ("root/c/w", b"w"),
        ],
    );
    let root = t.join("root");
    let mut a = started(&root, &root);
    find_run(&mut a, "", "");
    assert_eq!(visible(&a), ["a", "b", "c", "a/x", "a/y", "b/z", "c/w"]);
    for n in [&b"a/y"[..], b"b/z", b"c/w"] {
        a.panel_mut().cursor_to_name(n);
        a.panel_mut().toggle_mark(false);
    }
    std::fs::remove_file(t.join("root/a/x")).unwrap();
    write(&t.join("root/a/y"), b"grown");
    std::fs::rename(t.join("root/c"), t.join("c-moved")).unwrap();
    symlink(t.join("c-moved"), t.join("root/c")).unwrap();
    let fx = press_with(&mut a, KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert!(fx.iter().any(|e| matches!(e, Effect::Restat(..))), "{fx:?}");
    run(&mut a, fx);
    a.panel_mut().ensure_sorted();
    assert_eq!(visible(&a), ["a", "b", "a/y", "b/z", "c"]);
    let p = a.panel();
    let size = |n: &[u8]| p.list.entries[p.list.find(n).unwrap() as usize].size;
    assert_eq!(size(b"a/y"), 5, "fresh metadata");
    let kind = p.list.entries[p.list.find(b"c").unwrap() as usize].kind;
    assert_eq!(kind, EKind::Symlink, "the leaf is not followed");
    assert_eq!(p.marked, 2, "marks survive by name");
    assert!(!p.is_directory());
    // A job's end re-stats too.
    std::fs::remove_file(t.join("root/b/z")).unwrap();
    let fx = a.update(Event::Job(JobEvent::Done(Report::new(
        manycommander::fsops::job::JobVerb::Copy,
    ))));
    run(&mut a, fx);
    a.panel_mut().ensure_sorted();
    assert_eq!(visible(&a), ["a", "b", "a/y", "c"]);
}

/// P2 5.4: F7, Shift+F4 and Shift+F2 are refused in a results tab; compare needs two
/// directory panels.
#[test]
fn keys_a_results_tab_refuses() {
    let t = test_dir("find-app-refused");
    mkdirs(&t, &["root/a"]);
    files(&t, &[("root/a/x", b"")]);
    let root = t.join("root");
    let mut a = started(&root, &root);
    find_run(&mut a, "x", "");
    for (code, m) in [
        (KeyCode::F(7), KeyModifiers::NONE),
        (KeyCode::F(4), KeyModifiers::SHIFT),
        (KeyCode::F(2), KeyModifiers::SHIFT),
    ] {
        let fx = press_with(&mut a, code, m);
        assert!(fx.is_empty() && a.dialog.is_none(), "{code:?}");
        assert_eq!(
            a.status.as_ref().map(|s| s.text.as_str()),
            Some("not in search results"),
            "{code:?}"
        );
    }
    press(&mut a, KeyCode::Tab);
    press_with(&mut a, KeyCode::F(2), KeyModifiers::SHIFT);
    assert_eq!(
        a.status.as_ref().map(|s| s.text.as_str()),
        Some("compare needs two directory panels")
    );
}

/// P2 2.2, 5.4: a result below another selected result goes with its ancestor; Shift+F6
/// renames a result in its own directory; Alt+Enter inserts the relative path and Alt+P the
/// full one; F3 opens the full path with the root as the working directory.
#[test]
fn nested_selection_rename_and_paths_in_results() {
    let t = test_dir("find-app-nested");
    mkdirs(&t, &["root/a/sub", "root/b"]);
    files(
        &t,
        &[("root/a/sub/x", b""), ("root/a/y", b""), ("root/b/x", b"")],
    );
    let root = t.join("root");
    let mut a = started(&root, &root);
    find_run(&mut a, "", "");
    for n in [&b"a"[..], b"a/sub/x", b"b/x"] {
        a.panel_mut().cursor_to_name(n);
        a.panel_mut().toggle_mark(false);
    }
    assert_eq!(
        a.panel().selection_groups(),
        [group(&root, &[], &["a"]), group(&root, &["b"], &["x"])]
    );
    press_with(&mut a, KeyCode::Char('a'), KeyModifiers::CONTROL);
    press_with(&mut a, KeyCode::Char('*'), KeyModifiers::ALT);
    assert_eq!(a.panel().marked, 0);

    a.panel_mut().cursor_to_name(b"a/sub/x");
    press_with(&mut a, KeyCode::F(6), KeyModifiers::SHIFT);
    let Some(Dialog::Input { line, .. }) = &a.dialog else {
        panic!("the rename dialog");
    };
    assert_eq!(line.bytes(), b"x", "the leaf");
    typed(&mut a, "2");
    let fx = press(&mut a, KeyCode::Enter);
    assert_eq!(
        fx,
        [Effect::StartJob(JobSpec::Move {
            groups: vec![group(&root, &["a", "sub"], &["x"])],
            dst: root.join("a/sub/x2").into(),
        })]
    );
    let fx = a.update(Event::Job(JobEvent::Done(Report::new(
        manycommander::fsops::job::JobVerb::Move,
    ))));
    run(&mut a, fx);

    a.panel_mut().cursor_to_name(b"a/y");
    press_with(&mut a, KeyCode::Enter, KeyModifiers::ALT);
    assert_eq!(a.line.bytes(), b"'a/y' ");
    press_with(&mut a, KeyCode::Char('p'), KeyModifiers::ALT);
    let mut want = b"'a/y' '".to_vec();
    want.extend_from_slice(root.join("a/y").as_os_str().as_encoded_bytes());
    want.extend_from_slice(b"' ");
    assert_eq!(a.line.bytes(), want);
    let fx = press(&mut a, KeyCode::F(3));
    let [Effect::Run(manycommander::cmdline::handoff::Handoff::Program { argv, cwd })] = &fx[..]
    else {
        panic!("{fx:?}");
    };
    assert_eq!(argv.last().unwrap(), root.join("a/y").as_os_str());
    assert_eq!(cwd, &root);
}

/// P2 2.3: starting a search cancels the running one, whose tab keeps its results and says
/// so; while two cancelled searches are still blocked, a new one is refused.
#[test]
fn abandoned_searches_are_limited_to_two() {
    let t = test_dir("find-app-abandoned");
    mkdirs(&t, &["root"]);
    let root = t.join("root");
    let mut a = started(&root, &root);
    // The Find effects are not performed: the searches stay "alive" as if blocked.
    let search = |fx: &[Effect]| -> Arc<Search> {
        match fx.iter().find(|e| matches!(e, Effect::Find(_))) {
            Some(Effect::Find(s)) => s.clone(),
            _ => panic!("{fx:?}"),
        }
    };
    let s1 = search(&find_keys(&mut a, "one", ""));
    assert!(s1.running());
    let s2 = search(&find_keys(&mut a, "two", ""));
    assert!(!s1.running() && s1.stopped(), "the first was cancelled");
    assert_eq!(s1.state(), find::State::Cancelled);
    let s3 = search(&find_keys(&mut a, "three", ""));
    assert!(!s2.running());
    let fx = find_keys(&mut a, "four", "");
    assert!(fx.is_empty(), "{fx:?}");
    let Some(Dialog::Form { form, .. }) = &a.dialog else {
        panic!("the form stays open");
    };
    assert_eq!(
        form.error.as_deref(),
        Some("previous searches are still blocked")
    );
    // One of them returns: the search starts.
    s1.alive.finish();
    let fx = press(&mut a, KeyCode::Enter);
    let s4 = search(&fx);
    assert!(!s3.running() && s4.running());
    assert_eq!(a.sides[a.active].tabs.len(), 5, "one tab per search");
    // Results tabs are saved as tabs on their root.
    let st = a.state();
    let side = if a.active == 0 { &st.left } else { &st.right };
    assert!(side.tabs.iter().all(|tab| tab.path() == root), "{side:?}");
    assert!(render(&mut a, 100, 20).contains("find: four"));
    // Esc cancels the running search of the tab on screen.
    press(&mut a, KeyCode::Esc);
    assert_eq!(s4.state(), find::State::Cancelled);
    assert!(render(&mut a, 100, 20).contains("0 results (cancelled)"));
}

/// Review finding B1 (E-28): a re-stat never loses a search's late batches. After `Esc`, a
/// cancelled search can still send batches until its threads have recorded their totals:
/// `Ctrl+R` does not re-stat then and says why, and the late batch stays. A re-stat that
/// starts after the threads finished, while their last batch and `Done` are still queued,
/// keeps that batch too.
#[test]
fn a_restat_never_loses_late_search_batches() {
    let t = test_dir("find-app-late");
    mkdirs(&t, &["root"]);
    files(
        &t,
        &[
            ("root/early", b"e"),
            ("root/late", b"l"),
            ("root/later", b"x"),
        ],
    );
    let root = t.join("root");
    let mut a = started(&root, &root);
    let fx = find_keys(&mut a, "", "");
    let Some(Effect::Find(s)) = fx.iter().find(|e| matches!(e, Effect::Find(_))) else {
        panic!("{fx:?}");
    };
    // The search's threads are not run: the test sends their messages.
    let s = s.clone();
    let batch = |name: &str| {
        let mut names = Vec::new();
        let entries = vec![Entry::new(
            &mut names,
            name.as_bytes(),
            &meta(Kind::File, 1, 1_790_000_000),
        )];
        Event::Find(FindMsg::Batch {
            id: s.id,
            entries,
            names,
        })
    };
    a.update(batch("early"));
    press(&mut a, KeyCode::Esc);
    assert!(s.stopping());
    let fx = press_with(&mut a, KeyCode::Char('r'), KeyModifiers::CONTROL);
    let restat = fx.iter().any(|e| matches!(e, Effect::Restat(..)));
    a.update(batch("late"));
    run(&mut a, fx);
    a.panel_mut().ensure_sorted();
    assert_eq!(visible(&a), ["early", "late"], "the late batch stays");
    assert!(!restat, "no re-stat while the search is stopping");
    assert_eq!(
        a.status.as_ref().map(|s| s.text.as_str()),
        Some(manycommander::app::SEARCH_STOPPING)
    );
    // The threads record their totals (the search thread does, before it sends `Done`);
    // their last batch and `Done` are still queued when Ctrl+R re-stats.
    let stats = Stats {
        results: 3,
        cancelled: true,
        ..Stats::default()
    };
    s.finish(stats.clone());
    let fx = press_with(&mut a, KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert!(fx.iter().any(|e| matches!(e, Effect::Restat(..))), "{fx:?}");
    a.update(batch("later"));
    a.update(Event::Find(FindMsg::Done { id: s.id, stats }));
    run(&mut a, fx);
    a.panel_mut().ensure_sorted();
    assert_eq!(visible(&a), ["early", "late", "later"]);
    assert!(a.panel().loading.is_none(), "the re-stat completed");
}

/// P2 2.4, M2: a results tab keeps its entries in the background and re-stats them when it
/// is shown again; a new tab from it opens its root.
#[test]
fn a_hidden_results_tab_keeps_its_results() {
    let t = test_dir("find-app-tabs");
    mkdirs(&t, &["root/a"]);
    files(&t, &[("root/a/x", b""), ("root/a/y", b"")]);
    let root = t.join("root");
    let mut a = started(&root, &root);
    find_run(&mut a, "", "");
    assert_eq!(visible(&a), ["a", "a/x", "a/y"]);
    let fx = press_with(&mut a, KeyCode::Char('t'), KeyModifiers::CONTROL);
    run(&mut a, fx);
    assert!(a.panel().is_directory());
    assert_eq!(a.panel().dir, root);
    std::fs::remove_file(t.join("root/a/y")).unwrap();
    let fx = press_with(&mut a, KeyCode::PageUp, KeyModifiers::ALT);
    assert!(fx.iter().any(|e| matches!(e, Effect::Restat(..))), "{fx:?}");
    run(&mut a, fx);
    a.panel_mut().ensure_sorted();
    assert!(!a.panel().is_directory());
    assert_eq!(visible(&a), ["a", "a/x"]);
}

/// A-FD-4 in the app: `Esc` in the results tab stops the search within 100 ms; the tab
/// keeps its results and says "cancelled".
#[test]
fn a_fd_4_esc_in_the_results_tab_cancels_and_keeps_its_results() {
    let t = test_dir("find-app-esc");
    slow_tree(&t);
    let mut a = started(&t.path, &t.path);
    let fx = find_keys(&mut a, "", "NEEDLE");
    let Some(Effect::Find(s)) = fx.iter().find(|e| matches!(e, Effect::Find(_))) else {
        panic!("{fx:?}");
    };
    let s = s.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    find::spawn(s.clone(), move |m| {
        let _ = tx.send(m);
    })
    .unwrap();
    assert!(a.needs_tick(), "the loop ticks while a search runs");
    while a.panel().list.entries.is_empty() {
        let m = rx.recv_timeout(Duration::from_secs(30)).unwrap();
        assert!(matches!(m, FindMsg::Batch { .. }), "finished too early");
        a.update(Event::Find(m));
    }
    let esc = Instant::now();
    let fx = press(&mut a, KeyCode::Esc);
    assert!(fx.is_empty());
    assert_eq!(s.state(), find::State::Cancelled);
    loop {
        let m = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let done = matches!(m, FindMsg::Done { .. });
        a.update(Event::Find(m));
        if done {
            break;
        }
    }
    let took = esc.elapsed();
    assert!(took < Duration::from_millis(100), "stopped after {took:?}");
    assert!(!a.needs_tick(), "no tick once it stopped (P-5)");
    let n = a.panel().list.entries.len();
    assert!(n >= 1);
    let screen = render(&mut a, 100, 20);
    let want = format!("{n} result");
    assert!(screen.contains(&want), "{screen}");
    assert!(screen.contains("(cancelled)"), "{screen}");
    assert!(screen.contains("find: \"NEEDLE\""), "{screen}");
}
