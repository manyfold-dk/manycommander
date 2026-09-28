//! P2 T4: compare directories (P2 7). A-CD-1 (the date-and-size rules on both sides, the
//! 2 s vfat resolution, directories with and without the option, hidden and filtered
//! entries left out, marks for a listing that changed meanwhile discarded) and A-CD-2
//! (content compare marks same-size differing pairs, leaves identical ones, never opens a
//! FIFO, and cancels).

mod common;

use common::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use manycommander::app::App;
use manycommander::app::event::{Effect, Event};
use manycommander::compare::{
    self, CompareMsg, Marks, Mode, Progress, Request, Side, compare_content, compare_meta,
    resolution,
};
use manycommander::config::Config;
use manycommander::fsops::sys::{Sys, Ts, magic};
use manycommander::panel::entry::EKind;
use manycommander::panel::listing;
use manycommander::theme::Depth;
use rustix::fd::AsFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ---- helpers ------------------------------------------------------------------------------

fn side(entries: &[(&str, EKind, u64, Ts)]) -> Side {
    let mut s = Side::new(PathBuf::from("/nowhere"), 0, 1);
    for (k, (n, kind, size, mtime)) in entries.iter().enumerate() {
        s.push(k as u32, n.as_bytes(), *kind, *size, *mtime);
    }
    s
}

fn at(sec: i64, nsec: u32) -> Ts {
    Ts { sec, nsec }
}

/// Writes `p` with `content` and the modification time `sec`.
fn file(p: &Path, content: &[u8], sec: u64) {
    write(p, content);
    std::fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(sec))
        .unwrap();
}

fn fifo(p: &Path) {
    rustix::fs::mknodat(
        rustix::fs::CWD,
        p,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o644),
        0,
    )
    .unwrap();
}

/// The entries of `dir` as a panel would list them (not following symlinks).
fn listed(dir: &Path) -> Side {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    names.sort();
    let mut s = Side::new(dir.to_path_buf(), 0, 1);
    for (k, n) in names.iter().enumerate() {
        let m = std::fs::symlink_metadata(dir.join(n)).unwrap();
        let t = m.file_type();
        let kind = if t.is_file() {
            EKind::File
        } else if t.is_dir() {
            EKind::Dir
        } else if t.is_symlink() {
            EKind::Symlink
        } else {
            EKind::Special
        };
        let mtime = at(m.mtime(), m.mtime_nsec() as u32);
        s.push(k as u32, n.as_encoded_bytes(), kind, m.len(), mtime);
    }
    s
}

/// The names of `s` at the listing indices `marks`.
fn names(s: &Side, marks: &[u32]) -> Vec<String> {
    let mut v: Vec<String> = marks
        .iter()
        .map(|&i| {
            let it = s.items.iter().find(|it| it.index == i).unwrap();
            String::from_utf8_lossy(s.name(it)).into_owned()
        })
        .collect();
    v.sort();
    v
}

/// Runs a content compare of two listed directories with fresh fds.
fn content(
    l: &Side,
    r: &Side,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Option<Marks> {
    let sys = Sys::default();
    let (lfd, rfd) = (
        sys.open_root(&l.dir).unwrap(),
        sys.open_root(&r.dir).unwrap(),
    );
    compare_content(
        l,
        r,
        true,
        (lfd.as_fd(), rfd.as_fd()),
        &sys,
        cancel,
        progress,
    )
}

/// Runs `req` on its own thread and collects its messages; a compare that blocks (on a
/// FIFO) fails the test instead of hanging it.
fn run_compare(req: Request) -> Vec<CompareMsg> {
    let (tx, rx) = channel();
    compare::spawn(req, std::sync::Arc::new(AtomicBool::new(false)), move |m| {
        let _ = tx.send(m);
    })
    .unwrap();
    let mut out = Vec::new();
    loop {
        let m = rx
            .recv_timeout(Duration::from_secs(20))
            .expect("the compare completes");
        let done = matches!(m, CompareMsg::Done { .. });
        out.push(m);
        if done {
            return out;
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
        Depth::NoColor,
        jiff::tz::TimeZone::UTC,
    )
}

/// Performs listing effects synchronously; returns the others.
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
            other => rest.push(other),
        }
    }
    for s in &mut app.sides {
        s.panel_mut().ensure_sorted();
    }
    rest
}

fn key(a: &mut App, code: KeyCode, m: KeyModifiers) -> Vec<Effect> {
    a.update(Event::Key(
        KeyEvent::new(code, m),
        std::time::Instant::now(),
    ))
}

/// Shift+F2, then `keys` in the form, then Enter: the request it starts.
fn request(a: &mut App, keys: &[KeyCode]) -> Request {
    key(a, KeyCode::F(2), KeyModifiers::SHIFT);
    for k in keys {
        key(a, *k, KeyModifiers::NONE);
    }
    let fx = key(a, KeyCode::Enter, KeyModifiers::NONE);
    let [Effect::Compare(req)] = &fx[..] else {
        panic!("{fx:?}");
    };
    assert!(a.dialog.is_none());
    req.clone()
}

/// The marked names of the panel on `side`, visible or not.
fn marked(a: &App, side: usize) -> Vec<String> {
    let p = a.sides[side].panel();
    let mut v: Vec<String> = p
        .list
        .entries
        .iter()
        .filter(|e| e.marked())
        .map(|e| String::from_utf8_lossy(e.name(&p.list.names)).into_owned())
        .collect();
    v.sort();
    v
}

fn status(a: &App) -> String {
    a.status
        .as_ref()
        .map(|s| s.text.clone())
        .unwrap_or_default()
}

// ---- A-CD-1 ------------------------------------------------------------------------------

/// The rule table of P2 7 on both sides.
#[test]
fn a_cd_1_date_and_size_rules() {
    use EKind::{Dir, File, Special, Symlink};
    let t = at(100, 0);
    let later = at(200, 0);
    let l = side(&[
        ("only_l", File, 1, t),
        ("only_l_dir", Dir, 0, t),
        ("newer_l", File, 5, later),
        ("newer_r", File, 5, t),
        ("size", File, 5, t),
        ("same", File, 5, t),
        ("dirs", Dir, 0, t),
        ("types", File, 5, later),
        ("links", Symlink, 3, later),
        ("fifos", Special, 0, later),
        ("only_l_link", Symlink, 3, t),
    ]);
    let r = side(&[
        ("only_r", File, 1, t),
        ("only_r_dir", Dir, 0, t),
        ("newer_l", File, 5, t),
        ("newer_r", File, 5, later),
        ("size", File, 6, t),
        ("same", File, 5, t),
        ("dirs", Dir, 0, later),
        ("types", Dir, 0, t),
        ("links", Symlink, 9, t),
        ("fifos", Special, 0, t),
    ]);
    let m = compare_meta(&l, &r, 1, true);
    assert_eq!(
        names(&l, &m.left),
        ["newer_l", "only_l", "only_l_dir", "only_l_link", "size"]
    );
    assert_eq!(
        names(&r, &m.right),
        ["newer_r", "only_r", "only_r_dir", "size"]
    );
    assert_eq!(
        m.summary.to_string(),
        "left: 1 newer, 3 only here; right: 1 newer, 2 only here; 1 differ in size"
    );
    // Without "include directories", a directory on one side only is not marked.
    let m = compare_meta(&l, &r, 1, false);
    assert_eq!(
        names(&l, &m.left),
        ["newer_l", "only_l", "only_l_link", "size"]
    );
    assert_eq!(names(&r, &m.right), ["newer_r", "only_r", "size"]);
    assert_eq!(
        m.summary.to_string(),
        "left: 1 newer, 2 only here; right: 1 newer, 1 only here; 1 differ in size"
    );
    // Swapping the sides swaps the result.
    let m = compare_meta(&r, &l, 1, true);
    assert_eq!(names(&l, &m.right).len(), 5);
    assert_eq!(names(&r, &m.left).len(), 4);
}

/// The coarser resolution of the two filesystems decides "newer" (M1 4.5): vfat's 2 s,
/// exfat's 10 ms.
#[test]
fn a_cd_1_mtime_resolution() {
    let pair = |lt: Ts, ls: u64, rt: Ts, rs: u64| {
        (
            side(&[("a", EKind::File, ls, lt)]),
            side(&[("a", EKind::File, rs, rt)]),
        )
    };
    let vfat = resolution(magic::VFAT, magic::BTRFS);
    assert_eq!(vfat, 2_000_000_000);
    let exfat = resolution(magic::TMPFS, magic::EXFAT);
    let ns = resolution(magic::BTRFS, magic::TMPFS);
    // One second apart inside the same 2 s step: newer at 1 ns, equal on vfat.
    let (l, r) = pair(at(100, 0), 5, at(101, 0), 5);
    let m = compare_meta(&l, &r, ns, true);
    assert_eq!((m.left.len(), m.right.len()), (0, 1));
    assert_eq!(m.summary.right_newer, 1);
    assert_eq!(compare_meta(&l, &r, vfat, true), Marks::default());
    // ... and with different sizes, both sides on vfat.
    let (l, r) = pair(at(100, 0), 5, at(101, 0), 6);
    let m = compare_meta(&l, &r, vfat, true);
    assert_eq!((m.left, m.right), (vec![0], vec![0]));
    assert_eq!(m.summary.size_differ, 1);
    // Across a 2 s step: newer on vfat too.
    let (l, r) = pair(at(103, 0), 5, at(101, 900_000_000), 5);
    let m = compare_meta(&l, &r, vfat, true);
    assert_eq!((m.left, m.right.len()), (vec![0], 0));
    // Nanoseconds: newer at 1 ns, equal on exfat (10 ms).
    let (l, r) = pair(at(100, 5), 5, at(100, 4), 5);
    assert_eq!(compare_meta(&l, &r, ns, true).left, [0]);
    assert_eq!(compare_meta(&l, &r, exfat, true), Marks::default());
    // Before the epoch, steps round down (div_euclid): -2 s and -1 s share a step.
    let (l, r) = pair(at(-2, 0), 5, at(-1, 0), 5);
    assert_eq!(compare_meta(&l, &r, vfat, true), Marks::default());
    let (l, r) = pair(at(-1, 0), 5, at(0, 0), 5);
    assert_eq!(compare_meta(&l, &r, vfat, true).right, [0]);
    let (l, r) = pair(at(-3, 0), 5, at(0, 0), 5);
    assert_eq!(compare_meta(&l, &r, vfat, true).right, [0]);
}

/// Through the app: Shift+F2 opens the form; the request holds only the visible entries
/// (the hidden toggle and the filter leave entries out, I-8); the marks replace the earlier
/// ones on both sides; the status row carries the summary. Marks for a listing that changed
/// meanwhile, for a cancelled compare, or for a replaced one are not applied.
#[test]
fn a_cd_1_compare_in_the_app() {
    let lt = test_dir("compare-app-left");
    let rt = test_dir("compare-app-right");
    for (n, content, sec) in [
        ("newer", &b"new"[..], 2000),
        ("same", b"same", 1000),
        ("lonly", b"l", 1000),
        (".hid", b"h", 1000),
        ("filtered", b"f", 1000),
        ("sizediff", b"abc", 1000),
    ] {
        file(&lt.join(n), content, sec);
    }
    for (n, content, sec) in [
        ("newer", &b"old"[..], 1000),
        ("same", b"same", 1000),
        ("ronly", b"r", 1000),
        ("filtered", b"f", 1000),
        ("sizediff", b"abcd", 1000),
    ] {
        file(&rt.join(n), content, sec);
    }
    std::fs::create_dir(rt.join("sub")).unwrap();
    let mut a = app(&lt.path, &rt.path);
    let fx = a.start();
    run(&mut a, fx);
    // The left panel hides hidden files and filters out `filtered`; `same` is marked.
    key(&mut a, KeyCode::Char('.'), KeyModifiers::ALT);
    key(&mut a, KeyCode::Char('f'), KeyModifiers::CONTROL);
    for c in "[!f]*".chars() {
        key(&mut a, KeyCode::Char(c), KeyModifiers::NONE);
    }
    key(&mut a, KeyCode::Enter, KeyModifiers::NONE);
    a.panel_mut().cursor_to_name(b"same");
    a.panel_mut().toggle_mark(false);
    assert_eq!(marked(&a, 0), ["same"]);

    let req = request(&mut a, &[]);
    assert_eq!((req.mode, req.include_dirs), (Mode::DateSize, true));
    let side_names = |s: &Side| -> Vec<String> {
        s.items
            .iter()
            .map(|it| String::from_utf8_lossy(s.name(it)).into_owned())
            .collect()
    };
    assert_eq!(
        side_names(&req.left),
        ["lonly", "newer", "same", "sizediff"]
    );
    assert_eq!(
        side_names(&req.right),
        ["sub", "filtered", "newer", "ronly", "same", "sizediff"]
    );
    assert!(
        a.compare_line()
            .is_some_and(|l| l.starts_with("compare by date and size: running")),
        "{:?}",
        a.compare_line()
    );
    for m in run_compare(req.clone()) {
        a.update(Event::Compare(m));
    }
    assert!(a.compare.is_none());
    assert_eq!(marked(&a, 0), ["lonly", "newer", "sizediff"]);
    // `filtered` takes part only on the right, where it is visible: there it is only here.
    assert_eq!(marked(&a, 1), ["filtered", "ronly", "sizediff", "sub"]);
    assert_eq!(
        status(&a),
        "left: 1 newer, 1 only here; right: 0 newer, 3 only here; 1 differ in size"
    );
    assert_eq!(a.sides[0].panel().marked, 3);
    assert_eq!(a.sides[1].panel().marked, 4);

    // The listing changed meanwhile (a refresh starts a new generation): nothing is applied.
    let req2 = request(&mut a, &[KeyCode::Tab, KeyCode::Char(' ')]);
    assert!(!req2.include_dirs);
    let fx = a.update(Event::Key(
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        std::time::Instant::now(),
    ));
    run(&mut a, fx);
    for m in run_compare(req2) {
        a.update(Event::Compare(m));
    }
    assert_eq!(status(&a), "the directories changed; compare again");
    assert!(a.status.as_ref().unwrap().error);
    assert_eq!(marked(&a, 1), ["filtered", "ronly", "sizediff", "sub"]);
    assert!(a.compare.is_none());

    // Esc cancels a running compare; its late result is dropped.
    let req3 = request(&mut a, &[KeyCode::Tab, KeyCode::Char(' ')]);
    let fx = key(&mut a, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(fx, [Effect::CancelCompare]);
    assert_eq!(status(&a), "compare cancelled");
    assert!(a.compare.is_none());
    for m in run_compare(req3) {
        a.update(Event::Compare(m));
    }
    assert_eq!(status(&a), "compare cancelled");
    assert_eq!(marked(&a, 1), ["filtered", "ronly", "sizediff", "sub"]);

    // A new compare replaces a running one: the first one's events are dropped.
    let first = request(&mut a, &[KeyCode::Tab, KeyCode::Char(' ')]);
    let second = request(&mut a, &[]);
    assert_ne!(first.id, second.id);
    for m in run_compare(first) {
        a.update(Event::Compare(m));
    }
    assert!(a.compare.is_some(), "the second compare still runs");
    assert_eq!(marked(&a, 1), ["filtered", "ronly", "sizediff", "sub"]);
    for m in run_compare(second) {
        a.update(Event::Compare(m));
    }
    assert!(a.compare.is_none());
    assert_eq!(marked(&a, 1), ["filtered", "ronly", "sizediff", "sub"]);

    // While a panel loads, the form stays open with a message.
    let alive = listing::Alive::running();
    let _req = a.sides[1].panel_mut().refresh(alive.clone());
    key(&mut a, KeyCode::F(2), KeyModifiers::SHIFT);
    assert!(key(&mut a, KeyCode::Enter, KeyModifiers::NONE).is_empty());
    let Some(manycommander::ui::dialog::Dialog::Form { form, .. }) = &a.dialog else {
        panic!("the form stays open");
    };
    assert!(
        form.error.as_deref().is_some_and(|e| e.contains("loading")),
        "{:?}",
        form.error
    );
    alive.finish();
}

// ---- A-CD-2 ------------------------------------------------------------------------------

/// By content: same-size pairs that differ are marked on both sides, identical ones are
/// not, different sizes differ without being read, and FIFOs, symlinks and directories are
/// never opened for reading (I-10); a FIFO the listing called a regular file is refused by
/// the open sequence and the compare completes.
#[test]
fn a_cd_2_content_compare() {
    let lt = test_dir("compare-content-left");
    let rt = test_dir("compare-content-right");
    let big = noise(3 * compare::CHUNK + 17, 1);
    let mut last = noise(5 * compare::CHUNK / 2, 2);
    for (d, flip) in [(&lt, false), (&rt, true)] {
        write(&d.join("eq"), &big);
        if flip {
            *last.last_mut().unwrap() ^= 1;
        }
        write(&d.join("diff_end"), &last);
        write(
            &d.join("diff_start"),
            if flip { b"xbcdefghij" } else { b"abcdefghij" },
        );
        write(&d.join("empty"), b"");
        fifo(&d.join("fifo"));
        std::os::unix::fs::symlink(if flip { "eq" } else { "empty" }, d.join("link")).unwrap();
        std::fs::create_dir(d.join("dir")).unwrap();
        write(&d.join("dir/inner"), if flip { b"1" } else { b"2" });
        fifo(&d.join("trap"));
    }
    // Different sizes differ without reading: the left one cannot be read at all.
    write(&lt.join("size"), b"12345");
    write(&rt.join("size"), b"123456");
    std::fs::set_permissions(lt.join("size"), std::fs::Permissions::from_mode(0o000)).unwrap();
    let (mut l, mut r) = (listed(&lt.path), listed(&rt.path));
    // `trap` is a FIFO that the listings call a regular file (as if it was replaced after
    // the listing): the O_PATH open and fstat refuse it.
    for s in [&mut l, &mut r] {
        let k = s.items.iter().position(|it| s.name(it) == b"trap").unwrap();
        assert_eq!(s.items[k].kind, EKind::Special);
        s.items[k].kind = EKind::File;
    }
    let never = AtomicBool::new(false);
    let mut calls = 0;
    let m = content(&l, &r, &never, &mut |_| calls += 1).expect("not cancelled");
    assert_eq!(names(&l, &m.left), ["diff_end", "diff_start", "size"]);
    assert_eq!(names(&r, &m.right), ["diff_end", "diff_start", "size"]);
    assert_eq!(
        m.summary.to_string(),
        "left: 0 only here; right: 0 only here; 1 differ in size; 2 differ in content; \
         1 could not be read"
    );
    assert_eq!(m.summary.unreadable, 1, "the disguised FIFO");
    std::fs::set_permissions(lt.join("size"), std::fs::Permissions::from_mode(0o644)).unwrap();

    // The same through the app and the compare thread (by content is the second option).
    let mut a = app(&lt.path, &rt.path);
    let fx = a.start();
    run(&mut a, fx);
    let req = request(&mut a, &[KeyCode::Right]);
    assert_eq!(req.mode, Mode::Content);
    let msgs = run_compare(req);
    assert!(
        matches!(msgs.last(), Some(CompareMsg::Done { error: None, .. })),
        "{msgs:?}"
    );
    for m in msgs {
        a.update(Event::Compare(m));
    }
    assert_eq!(marked(&a, 0), ["diff_end", "diff_start", "size"]);
    assert_eq!(marked(&a, 1), ["diff_end", "diff_start", "size"]);
    assert_eq!(
        status(&a),
        "left: 0 only here; right: 0 only here; 1 differ in size; 2 differ in content"
    );
}

/// Two identical sparse files of 4 GiB: the compare reads zeros for a long time, so a cancel
/// after the first progress stops it well before the end, directly and on the thread.
#[test]
fn a_cd_2_content_compare_cancels() {
    let lt = test_dir("compare-cancel-left");
    let rt = test_dir("compare-cancel-right");
    const SIZE: u64 = 4 << 30;
    for d in [&lt, &rt] {
        let f = std::fs::File::create(d.join("big")).unwrap();
        f.set_len(SIZE).unwrap();
    }
    let (l, r) = (listed(&lt.path), listed(&rt.path));
    let cancel = AtomicBool::new(false);
    let mut seen = Vec::new();
    let start = SystemTime::now();
    let m = content(&l, &r, &cancel, &mut |p| {
        seen.push(p);
        cancel.store(true, Ordering::SeqCst);
    });
    assert_eq!(m, None, "cancelled");
    let p = seen[0];
    assert_eq!((p.pairs_total, p.bytes_total), (1, SIZE));
    assert!(p.bytes_done < SIZE);
    assert_eq!(seen.len(), 1, "progress stops with the cancel");
    assert!(start.elapsed().unwrap() < Duration::from_secs(10));

    // The thread: cancelled after its first progress, it ends with Done and no marks.
    let req = Request {
        id: 7,
        mode: Mode::Content,
        include_dirs: true,
        left: l,
        right: r,
    };
    let flag = std::sync::Arc::new(AtomicBool::new(false));
    let (tx, rx) = channel();
    compare::spawn(req, flag.clone(), move |m| {
        let _ = tx.send(m);
    })
    .unwrap();
    let first = rx.recv_timeout(Duration::from_secs(20)).unwrap();
    assert!(
        matches!(first, CompareMsg::Progress { id: 7, .. }),
        "{first:?}"
    );
    flag.store(true, Ordering::SeqCst);
    loop {
        match rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the thread stops")
        {
            CompareMsg::Progress { .. } => {}
            CompareMsg::Done { id, error } => {
                assert_eq!((id, error), (7, None));
                break;
            }
            m @ CompareMsg::Marks { .. } => panic!("a cancelled compare sent {m:?}"),
        }
    }
}

/// Progress events come at most every 67 ms (<= 15 Hz).
#[test]
fn a_cd_2_progress_is_throttled() {
    let lt = test_dir("compare-rate-left");
    let rt = test_dir("compare-rate-right");
    for d in [&lt, &rt] {
        for k in 0..4 {
            let f = std::fs::File::create(d.join(format!("f{k}"))).unwrap();
            f.set_len(256 << 20).unwrap();
        }
    }
    let (l, r) = (listed(&lt.path), listed(&rt.path));
    let mut at = Vec::new();
    let m = content(&l, &r, &AtomicBool::new(false), &mut |p| {
        at.push((std::time::Instant::now(), p))
    })
    .unwrap();
    assert_eq!(m.summary.content_differ, 0);
    assert!(m.left.is_empty() && m.right.is_empty());
    for w in at.windows(2) {
        assert!(w[1].0 - w[0].0 >= compare::PROGRESS_EVERY);
        assert!(w[1].1.bytes_done >= w[0].1.bytes_done);
    }
}
