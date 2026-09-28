//! P2 3: the directory hotlist, the frecency store, the zoxide import, the directories
//! dialog and `z` (A-DJ-1 to A-DJ-4). Every file lives under the test directory, passed in
//! explicitly; no test reads the user's configuration or runs the user's zoxide.

mod common;

use common::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use manycommander::app::App;
use manycommander::app::event::{Effect, Event};
use manycommander::config::{Config, ZoxideMode};
use manycommander::dirs::{
    Deltas, Dirs, Frecency, HOTLIST_BROKEN, HOTLIST_UNREAD, Hotlist, LOCK_WAIT, MAX_AGE, Matcher,
    Paths, Reply, Request, Store, StoreThread, Zoxide, find_program, lock_path, lowered, now,
    parse_zoxide, save_merged, score, zoxide_query,
};
use manycommander::panel::listing;
use manycommander::theme::Depth;
use manycommander::ui::dialog::Dialog;
use manycommander::ui::dirs::DirRow;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;

fn config(zoxide: ZoxideMode) -> Config {
    let mut c = Config::default();
    c.jump.zoxide = zoxide;
    c
}

fn app_with(left: &Path, right: &Path, zoxide: ZoxideMode) -> App {
    App::new(
        left.to_path_buf(),
        right.to_path_buf(),
        right.to_path_buf(),
        config(zoxide),
        None,
        Depth::NoColor,
        jiff::tz::TimeZone::UTC,
    )
}

fn app(left: &Path, right: &Path) -> App {
    app_with(left, right, ZoxideMode::Off)
}

/// Performs listing effects synchronously; returns the effects it did not perform.
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
    rest
}

fn started(left: &Path, right: &Path) -> App {
    let mut a = app(left, right);
    let fx = a.start();
    let rest = run(&mut a, fx);
    assert!(
        rest.iter().all(|e| matches!(e, Effect::Watch { .. })),
        "{rest:?}"
    );
    a
}

fn key(a: &mut App, code: KeyCode, m: KeyModifiers) -> Vec<Effect> {
    a.update(Event::Key(KeyEvent::new(code, m), Instant::now()))
}

fn press(a: &mut App, code: KeyCode) -> Vec<Effect> {
    key(a, code, KeyModifiers::NONE)
}

fn ctrl_d(a: &mut App) -> Vec<Effect> {
    key(a, KeyCode::Char('d'), KeyModifiers::CONTROL)
}

/// Types `text` on the command line and runs it; performs the listings it starts.
fn line(a: &mut App, text: &str) -> Vec<Effect> {
    for c in text.chars() {
        press(a, KeyCode::Char(c));
    }
    let fx = press(a, KeyCode::Enter);
    run(a, fx)
}

fn store(entries: &[(&Path, f64, i64)]) -> Store {
    let mut s = Store::default();
    for (p, rank, last) in entries {
        s.entries.insert(
            p.to_path_buf(),
            Frecency {
                rank: *rank,
                last: *last,
            },
        );
    }
    s
}

/// The dialog's rows: `*` and the path for a bookmark, two spaces for a frequent one.
fn rows(a: &App) -> Vec<String> {
    let Some(Dialog::Dirs(d)) = &a.dialog else {
        panic!("no directories dialog");
    };
    (0..d.len())
        .map(|i| match d.row(i).unwrap() {
            DirRow::Bookmark(p) => format!("* {}", p.display()),
            DirRow::Frequent(p) => format!("  {}", p.display()),
        })
        .collect()
}

fn dialog_note(a: &App) -> Option<(String, bool)> {
    match &a.dialog {
        Some(Dialog::Dirs(d)) => d.note.clone(),
        _ => panic!("no directories dialog"),
    }
}

fn status(a: &App) -> Option<(String, bool)> {
    a.status.as_ref().map(|s| (s.text.clone(), s.error))
}

// ---- A-DJ-1: bookmarks ------------------------------------------------------------------

/// A-DJ-1: bookmarks persist and reload in insertion order, including a non-UTF-8 path; the
/// file is replaced atomically (a new inode, no temporary file left behind, a reader of the
/// old file keeps its content).
#[test]
fn a_dj_1_hotlist_persists_and_is_replaced_atomically() {
    let t = test_dir("dirs-hotlist");
    let path = t.join("config/manycommander/hotlist.toml");
    let utf8 = PathBuf::from("/home/u/Documents/notes");
    let raw = PathBuf::from(os(b"/srv/caf\xe9/new\nline"));
    let mut h = Hotlist::default();
    assert_eq!(h.add(&utf8), Ok(true));
    assert_eq!(h.add(&raw), Ok(true));
    assert_eq!(h.add(&utf8), Ok(false), "already a bookmark");
    Hotlist::save(&h.dirs, &path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("path = \"/home/u/Documents/notes\""),
        "{text}"
    );
    assert!(
        text.contains("path = [47, 115, 114, 118"),
        "bytes as an array: {text}"
    );
    let (back, report) = Hotlist::load(&path);
    assert_eq!(report, None);
    assert_eq!(back.dirs, vec![utf8.clone(), raw.clone()]);

    // Replace: a new file renamed over the old one.
    let before = std::fs::metadata(&path).unwrap().ino();
    let mut old = std::fs::File::open(&path).unwrap();
    assert_eq!(h.remove(&utf8), Ok(true));
    assert_eq!(h.remove(&utf8), Ok(false));
    assert_eq!(h.add(Path::new("/etc")), Ok(true));
    Hotlist::save(&h.dirs, &path).unwrap();
    assert_ne!(
        std::fs::metadata(&path).unwrap().ino(),
        before,
        "renamed over"
    );
    let mut was = String::new();
    old.read_to_string(&mut was).unwrap();
    assert_eq!(was, text, "the old file was not rewritten in place");
    let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        names,
        [OsString::from("hotlist.toml")],
        "no temporary file left"
    );
    assert_eq!(
        Hotlist::load(&path).0.dirs,
        vec![raw, PathBuf::from("/etc")],
        "insertion order"
    );
    // A missing file is no bookmarks.
    assert_eq!(
        Hotlist::load(&t.join("none.toml")),
        (Hotlist::default(), None)
    );
}

/// A-DJ-1: a `hotlist.toml` that does not parse is reported once, adding and removing fail,
/// and the file is never written.
#[test]
fn a_dj_1_broken_hotlist_is_never_overwritten() {
    let t = test_dir("dirs-broken");
    for (i, text) in [
        &b"[[dir]]\npath = \"/ok\"\n[[dir\n"[..],
        b"[[dir]]\npath = \"relative/dir\"\n",
        b"[[dir]]\npath = [47, 300]\n",
        b"\xff\xfe",
    ]
    .iter()
    .enumerate()
    {
        let path = t.join(format!("hotlist-{i}.toml"));
        std::fs::write(&path, text).unwrap();
        let (mut h, report) = Hotlist::load(&path);
        let report = report.expect("reported");
        assert!(report.contains(&format!("hotlist-{i}.toml")), "{report}");
        assert!(h.dirs.is_empty());
        assert_eq!(h.add(Path::new("/x")), Err(HOTLIST_BROKEN.to_string()));
        assert_eq!(h.remove(Path::new("/ok")), Err(HOTLIST_BROKEN.to_string()));
        assert_eq!(std::fs::read(&path).unwrap(), *text);
    }
    // Through the app: Insert in the dialog explains and asks for no save.
    std::fs::create_dir(t.join("here")).unwrap();
    let mut a = started(&t.join("here"), &t.path);
    let path = t.join("hotlist-0.toml");
    a.dirs.hotlist = Hotlist::load(&path).0;
    ctrl_d(&mut a);
    let fx = press(&mut a, KeyCode::Insert);
    assert!(fx.is_empty(), "{fx:?}");
    assert_eq!(dialog_note(&a), Some((HOTLIST_BROKEN.to_string(), true)));
    // A hotlist the boot thread never read is read-only too.
    let mut h = Hotlist::unread();
    assert_eq!(h.add(Path::new("/x")), Err(HOTLIST_UNREAD.to_string()));
}

/// A-DJ-1 through the app: `Insert` bookmarks the active panel's directory (a non-UTF-8
/// one here) and asks for a save; `Delete` removes the bookmark; the saved file reloads.
#[test]
fn a_dj_1_bookmarks_through_the_dialog() {
    let t = test_dir("dirs-dialog-bm");
    let odd = t.join(os(b"caf\xe9 dir"));
    std::fs::create_dir(&odd).unwrap();
    let mut a = started(&odd, &t.path);
    let fx = ctrl_d(&mut a);
    assert_eq!(fx, vec![Effect::LoadDirs], "zoxide is off");
    assert!(rows(&a).is_empty());
    let fx = press(&mut a, KeyCode::Insert);
    assert_eq!(fx, vec![Effect::SaveHotlist(vec![odd.clone()])]);
    assert_eq!(rows(&a), vec![format!("* {}", odd.display())]);
    assert!(dialog_note(&a).unwrap().0.starts_with("bookmarked "));
    // The runtime's store thread performs the save; here it runs directly.
    let file = t.join("hotlist.toml");
    Hotlist::save(&a.dirs.hotlist.dirs, &file).unwrap();
    assert_eq!(Hotlist::load(&file).0.dirs, vec![odd.clone()]);
    // Insert again: already there, no save.
    assert!(press(&mut a, KeyCode::Insert).is_empty());
    assert!(
        dialog_note(&a)
            .unwrap()
            .0
            .ends_with("is already a bookmark")
    );
    let fx = press(&mut a, KeyCode::Delete);
    assert_eq!(fx, vec![Effect::SaveHotlist(Vec::new())]);
    assert!(rows(&a).is_empty());
    assert!(a.dirs.hotlist.dirs.is_empty());
    press(&mut a, KeyCode::Esc);
    assert!(a.dialog.is_none());
}

// ---- A-DJ-2: ranking ---------------------------------------------------------------------

/// A-DJ-2: the four time weights at their boundaries, and the ranking they produce.
#[test]
fn a_dj_2_time_weights_rank_directories() {
    let n = 1_800_000_000;
    let at = |age: i64| {
        score(
            Frecency {
                rank: 2.0,
                last: n - age,
            },
            n,
        )
    };
    assert_eq!(at(0), 8.0);
    assert_eq!(at(HOUR - 1), 8.0);
    assert_eq!(at(HOUR), 4.0);
    assert_eq!(at(DAY - 1), 4.0);
    assert_eq!(at(DAY), 1.0);
    assert_eq!(at(WEEK - 1), 1.0);
    assert_eq!(at(WEEK), 0.5);
    assert_eq!(at(365 * DAY), 0.5);
    assert_eq!(at(-60), 8.0, "a visit in the future counts as recent");

    let mut d = Dirs::new(false);
    d.loaded(store(&[
        (Path::new("/w/hour"), 1.0, n - 30 * 60), // 4
        (Path::new("/w/day"), 1.5, n - 2 * HOUR), // 3
        (Path::new("/w/week"), 5.0, n - 2 * DAY), // 2.5
        (Path::new("/w/old"), 9.0, n - 30 * DAY), // 2.25
        (Path::new("/w/here"), 100.0, n),         // the active directory
    ]));
    let ranked: Vec<(String, f64)> = d
        .ranked(Path::new("/w/here"), n)
        .into_iter()
        .map(|(p, s)| (p.display().to_string(), s))
        .collect();
    assert_eq!(
        ranked,
        [
            ("/w/hour".to_string(), 4.0),
            ("/w/day".to_string(), 3.0),
            ("/w/week".to_string(), 2.5),
            ("/w/old".to_string(), 2.25),
        ],
        "the active directory is excluded"
    );
    // Visits count at once.
    for _ in 0..3 {
        d.visit(Path::new("/w/old"), n);
    }
    assert_eq!(d.ranked(Path::new("/"), n)[1].0, Path::new("/w/old"));
}

/// A-DJ-2: aging past `MAX_AGE` multiplies every rank once by the zoxide factor
/// `0.9 x MAX_AGE / sum` and drops what falls below 1.
#[test]
fn a_dj_2_aging_uses_the_zoxide_factor() {
    let mut s = store(&[
        (Path::new("/a"), 11_000.0, 1),
        (Path::new("/b"), 998.8, 2),
        (Path::new("/c"), 1.2, 3),
    ]);
    let sum = 11_000.0 + 998.8 + 1.2;
    let factor = 0.9 * MAX_AGE / sum;
    s.age();
    assert_eq!(s.entries.len(), 2, "{s:?}");
    assert_eq!(s.entries[Path::new("/a")].rank, 11_000.0 * factor);
    assert_eq!(s.entries[Path::new("/b")].rank, 998.8 * factor);
    assert_eq!(s.entries[Path::new("/a")].last, 1);
    assert!(
        !s.entries.contains_key(Path::new("/c")),
        "0.9 < 1 is dropped"
    );
    // At or below MAX_AGE nothing changes.
    let mut s = store(&[(Path::new("/a"), 9_999.0, 1), (Path::new("/b"), 1.0, 1)]);
    let before = s.clone();
    s.age();
    assert_eq!(s, before);
}

/// A-DJ-2: keyword order, the last keyword in the last component, ASCII case folding.
#[test]
fn a_dj_2_keywords_match_in_order_and_last_in_the_last_component() {
    let n = now();
    let mut d = Dirs::new(false);
    d.loaded(store(&[
        (Path::new("/home/u/src/foo/bar"), 2.0, n),
        (Path::new("/home/u/bar/foo"), 1.0, n),
        (Path::new("/home/u/Projects/Alpha"), 1.0, n),
        (Path::new("/home/u/other/bar"), 0.5, n),
    ]));
    let best = |f: &str| d.best(f.as_bytes(), Path::new("/"), n);
    assert_eq!(best("foo bar"), Some("/home/u/src/foo/bar".into()));
    assert_eq!(best("bar foo"), Some("/home/u/bar/foo".into()));
    assert_eq!(
        best("bar"),
        Some("/home/u/src/foo/bar".into()),
        "the higher score"
    );
    assert_eq!(best("src"), None, "src is not in the last component");
    assert_eq!(best("src bar"), Some("/home/u/src/foo/bar".into()));
    assert_eq!(best("PROJ alpha"), Some("/home/u/Projects/Alpha".into()));
    assert_eq!(best("alpha proj"), None, "order matters");
    assert_eq!(best(""), Some("/home/u/src/foo/bar".into()));
    assert_eq!(
        d.best(b"bar", Path::new("/home/u/src/foo/bar"), n),
        Some("/home/u/other/bar".into()),
        "the active directory is excluded"
    );
    // A bookmark is the fallback when no frequent directory matches.
    d.hotlist.dirs.push("/srv/www".into());
    assert_eq!(d.best(b"www", Path::new("/"), n), Some("/srv/www".into()));
}

/// A-DJ-2: `z <keywords>` loads the best match into the active panel (a visit), never runs
/// the shell, leaves out the active directory, and says "z: no match".
#[test]
fn a_dj_2_z_loads_the_best_match_and_reports_no_match() {
    let t = test_dir("dirs-z");
    for d in ["here", "a/proj-alpha", "b/proj-beta", "c/other"] {
        std::fs::create_dir_all(t.join(d)).unwrap();
    }
    let n = now();
    let mut a = started(&t.join("here"), &t.path);
    // Before the store is loaded, z waits for it.
    let fx = line(&mut a, "z proj");
    assert_eq!(fx, vec![Effect::LoadDirs]);
    assert_eq!(status(&a), Some(("z: loading...".into(), false)));
    assert_eq!(a.panel().dir, t.join("here"));
    let fx = a.update(Event::DirsLoaded(store(&[
        (&t.join("a/proj-alpha"), 2.0, n),          // 8
        (&t.join("b/proj-beta"), 5.0, n - 2 * DAY), // 2.5
        (&t.join("c/other"), 100.0, n),
    ])));
    run(&mut a, fx);
    assert_eq!(a.panel().dir, t.join("a/proj-alpha"));
    assert_eq!(status(&a), None);
    assert_eq!(
        a.dirs.deltas.map[&t.join("a/proj-alpha")].rank,
        1.0,
        "a visit"
    );

    // The active directory is left out: the next best.
    let fx = line(&mut a, "z proj");
    assert!(
        fx.iter().all(|e| matches!(e, Effect::Watch { .. })),
        "{fx:?}"
    );
    assert_eq!(a.panel().dir, t.join("b/proj-beta"));
    line(&mut a, "z  PROJ-A ");
    assert_eq!(a.panel().dir, t.join("a/proj-alpha"), "ASCII case folding");

    let fx = line(&mut a, "z nothing-like-this");
    assert!(fx.is_empty(), "no shell: {fx:?}");
    assert_eq!(status(&a), Some(("z: no match".into(), true)));
    assert_eq!(a.panel().dir, t.join("a/proj-alpha"));
    // Bare z opens the dialog.
    line(&mut a, "z");
    assert!(matches!(a.dialog, Some(Dialog::Dirs(_))));
    assert!(a.line.is_empty());
}

// ---- A-DJ-3: the file and its merge --------------------------------------------------------

/// A-DJ-3: two writers whose saves overlap in time both keep their visits.
#[test]
fn a_dj_3_concurrent_writers_keep_every_visit() {
    let t = test_dir("dirs-merge");
    let path = t.join("state/manycommander/dirs.tsv");
    const ROUNDS: usize = 40;
    let writer = |own: &'static str| {
        let path = path.clone();
        std::thread::spawn(move || {
            for i in 0..ROUNDS {
                let mut d = Deltas::default();
                d.visit(Path::new("/shared"), 1_000 + i as i64);
                d.visit(Path::new(own), 2_000 + i as i64);
                save_merged(&path, &d, Duration::from_secs(30)).unwrap();
            }
        })
    };
    let (w1, w2) = (writer("/one"), writer("/two"));
    w1.join().unwrap();
    w2.join().unwrap();
    let s = Store::load(&path).unwrap();
    assert_eq!(s.entries[Path::new("/shared")].rank, 2.0 * ROUNDS as f64);
    assert_eq!(s.entries[Path::new("/one")].rank, ROUNDS as f64);
    assert_eq!(s.entries[Path::new("/two")].rank, ROUNDS as f64);
    assert_eq!(
        s.entries[Path::new("/shared")].last,
        1_000 + ROUNDS as i64 - 1
    );
    assert!(
        std::fs::read(&path)
            .unwrap()
            .starts_with(b"# manycommander dirs v1\n")
    );
}

/// A-DJ-3: a writer waits for the lock another holds, then merges; a writer whose wait
/// runs out saves nothing and says why.
#[test]
fn a_dj_3_the_lock_serialises_and_its_wait_is_bounded() {
    let t = test_dir("dirs-lock");
    let path = t.join("dirs.tsv");
    let mut first = Deltas::default();
    first.visit(Path::new("/before"), 10);
    save_merged(&path, &first, LOCK_WAIT).unwrap();
    let before = std::fs::read(&path).unwrap();

    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path(&path))
        .unwrap();
    rustix::fs::flock(&held, rustix::fs::FlockOperation::LockExclusive).unwrap();
    // Times out: nothing written.
    let mut late = Deltas::default();
    late.visit(Path::new("/late"), 20);
    let start = Instant::now();
    let e = save_merged(&path, &late, Duration::from_millis(150)).unwrap_err();
    assert!(start.elapsed() >= Duration::from_millis(150));
    assert!(
        e.contains("dirs.tsv.lock") && e.contains("not saved"),
        "{e}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    // Waits, then merges once the holder releases.
    let p = path.clone();
    let waiter = std::thread::spawn(move || {
        let mut d = Deltas::default();
        d.visit(Path::new("/waited"), 30);
        save_merged(&p, &d, LOCK_WAIT)
    });
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(std::fs::read(&path).unwrap(), before, "still waiting");
    drop(held);
    waiter.join().unwrap().unwrap();
    let s = Store::load(&path).unwrap();
    assert!(s.entries.contains_key(Path::new("/before")));
    assert!(s.entries.contains_key(Path::new("/waited")));
    assert!(!s.entries.contains_key(Path::new("/late")));
}

/// A-DJ-3: decimal ranks round-trip exactly; forgotten entries go; lines that do not parse
/// are skipped; a path with a tab, a newline, a backslash and invalid UTF-8 round-trips.
#[test]
fn a_dj_3_decimal_ranks_round_trip_and_bad_lines_are_skipped() {
    let odd = PathBuf::from(os(b"/x/tab\there\nnew\\line\xff"));
    let s = store(&[
        (Path::new("/a"), 0.1 + 0.2, 1_790_000_000),
        (Path::new("/b"), 1_234.567_890_123_4, -5),
        (Path::new("/c"), 1e-7, 0),
        (&odd, 3.0, 7),
    ]);
    let text = s.to_tsv();
    let back = Store::parse(&text);
    assert_eq!(back, s, "{}", String::from_utf8_lossy(&text));
    let shown = String::from_utf8(text).unwrap();
    assert!(
        shown.contains("0.30000000000000004\t1790000000\t/a\n"),
        "{shown}"
    );
    assert!(
        shown.contains("0.0000001\t0\t/c\n"),
        "decimal, not exponent: {shown}"
    );
    assert!(
        shown.contains("/x/tab\\x09here\\x0anew\\x5cline\\xff"),
        "{shown}"
    );

    let messy = b"# manycommander dirs v1\n\
        2.5\t100\t/good\n\
        \n\
        x\t100\t/bad-rank\n\
        1\tsoon\t/bad-last\n\
        1\t100\n\
        1\t100\trelative\n\
        -1\t100\t/negative\n\
        NaN\t100\t/nan\n\
        inf\t100\t/inf\n\
        1\t100\t/bad\\escape\n\
        # a comment\n\
        1.5\t200\t/good\n\
        0.25\t5\t/small";
    let s = Store::parse(messy);
    let mut keys: Vec<_> = s.entries.keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, [PathBuf::from("/good"), PathBuf::from("/small")]);
    assert_eq!(
        s.entries[Path::new("/good")],
        Frecency {
            rank: 4.0,
            last: 200
        },
        "a path listed twice adds up"
    );

    // Deltas: a forget drops the file's entry; visits after it count again.
    let t = test_dir("dirs-forget");
    let path = t.join("dirs.tsv");
    std::fs::write(&path, messy).unwrap();
    let mut d = Deltas::default();
    d.forget(Path::new("/good"));
    d.forget(Path::new("/small"));
    d.visit(Path::new("/small"), 300);
    d.visit(Path::new("/new"), 400);
    save_merged(&path, &d, LOCK_WAIT).unwrap();
    let s = Store::load(&path).unwrap();
    assert!(!s.entries.contains_key(Path::new("/good")));
    assert_eq!(
        s.entries[Path::new("/small")],
        Frecency {
            rank: 1.0,
            last: 300
        }
    );
    assert_eq!(
        s.entries[Path::new("/new")],
        Frecency {
            rank: 1.0,
            last: 400
        }
    );
}

// ---- A-DJ-4: a directory that is gone ----------------------------------------------------

/// A-DJ-4: a frecency entry whose directory is gone fails to load, the panel stays, and
/// the entry is dropped; a bookmark to a gone directory stays.
#[test]
fn a_dj_4_a_gone_directory_fails_and_loses_its_entry() {
    let t = test_dir("dirs-gone");
    std::fs::create_dir_all(t.join("here")).unwrap();
    std::fs::create_dir_all(t.join("other")).unwrap();
    let n = now();
    let mut a = started(&t.join("here"), &t.path);
    a.dirs.hotlist.dirs.push(t.join("gone-bookmark"));
    a.update(Event::DirsLoaded(store(&[
        (&t.join("gone"), 10.0, n),
        (&t.join("other"), 1.0, n),
    ])));
    ctrl_d(&mut a);
    let listed = vec![
        format!("* {}", t.join("gone-bookmark").display()),
        format!("  {}", t.join("gone").display()),
        format!("  {}", t.join("other").display()),
    ];
    assert_eq!(rows(&a), listed);
    press(&mut a, KeyCode::Down);
    let fx = press(&mut a, KeyCode::Enter);
    assert!(a.dialog.is_none());
    run(&mut a, fx);
    assert_eq!(a.panel().dir, t.join("here"), "the panel stays");
    assert!(a.panel().loading.is_none());
    let msg = a.panel().message.clone().unwrap_or_default();
    assert!(msg.contains("gone"), "{msg}");
    let store = a.dirs.store.as_ref().unwrap();
    assert!(!store.entries.contains_key(&t.join("gone")), "dropped");
    assert!(
        a.dirs.deltas.map[&t.join("gone")].forget,
        "dropped on exit too"
    );
    assert!(!a.dirs.deltas.map.contains_key(&t.join("here")), "no visit");

    // The bookmark fails the same way and stays.
    ctrl_d(&mut a);
    assert_eq!(rows(&a), vec![listed[0].clone(), listed[2].clone()]);
    let fx = press(&mut a, KeyCode::Enter);
    run(&mut a, fx);
    assert_eq!(a.panel().dir, t.join("here"));
    assert_eq!(a.dirs.hotlist.dirs, vec![t.join("gone-bookmark")]);
}

// ---- visits --------------------------------------------------------------------------------

/// P2 3.3: every completed navigation the user makes records a visit, including history
/// back and forward; a refresh, the start, and a new tab do not.
#[test]
fn completed_navigations_are_visits() {
    let t = test_dir("dirs-visits");
    std::fs::create_dir_all(t.join("sub/deeper")).unwrap();
    let mut a = started(&t.path, &t.path);
    assert!(a.dirs.deltas.is_empty(), "the start is not a visit");
    let rank = |a: &App, p: &Path| a.dirs.deltas.map.get(p).map_or(0.0, |d| d.rank);
    a.panel_mut().ensure_sorted();
    a.panel_mut().cursor_to_name(b"sub");
    let fx = press(&mut a, KeyCode::Enter);
    run(&mut a, fx);
    assert_eq!(a.panel().dir, t.join("sub"));
    assert_eq!(rank(&a, &t.join("sub")), 1.0);
    let fx = press(&mut a, KeyCode::Backspace);
    run(&mut a, fx);
    assert_eq!(rank(&a, &t.path), 1.0);
    let fx = key(&mut a, KeyCode::Left, KeyModifiers::ALT);
    run(&mut a, fx);
    assert_eq!(a.panel().dir, t.join("sub"));
    assert_eq!(rank(&a, &t.join("sub")), 2.0, "history back");
    let fx = key(&mut a, KeyCode::Right, KeyModifiers::ALT);
    run(&mut a, fx);
    assert_eq!(rank(&a, &t.path), 2.0, "history forward");
    let fx = key(&mut a, KeyCode::Char('r'), KeyModifiers::CONTROL);
    run(&mut a, fx);
    let fx = key(&mut a, KeyCode::Char('t'), KeyModifiers::CONTROL);
    run(&mut a, fx);
    line(&mut a, "cd sub/deeper");
    assert_eq!(rank(&a, &t.join("sub/deeper")), 1.0, "cd");
    assert_eq!(a.dirs.deltas.map.len(), 3, "{:?}", a.dirs.deltas);
    assert_eq!(
        rank(&a, &t.path),
        2.0,
        "a refresh and a new tab are not visits"
    );
    // Visits made before the store loaded are applied to it.
    a.update(Event::DirsLoaded(store(&[(&t.join("sub"), 3.0, 5)])));
    let s = a.dirs.store.as_ref().unwrap();
    assert_eq!(s.entries[&t.join("sub")].rank, 5.0);
    assert!(s.entries[&t.join("sub")].last > 5);
}

// ---- the dialog ----------------------------------------------------------------------------

/// P2 3.1: bookmarks first in insertion order, then frequent directories by score, the
/// active directory and bookmarked ones left out; "loading..." until the store arrives;
/// the filter narrows both.
#[test]
fn dialog_lists_bookmarks_then_frequent_directories() {
    let t = test_dir("dirs-list");
    std::fs::create_dir_all(t.join("here")).unwrap();
    let n = now();
    let mut a = started(&t.join("here"), &t.path);
    a.dirs.hotlist.dirs = vec!["/z/second".into(), "/a/first".into()];
    ctrl_d(&mut a);
    let Some(Dialog::Dirs(d)) = &a.dialog else {
        panic!()
    };
    assert!(d.loading);
    assert_eq!(rows(&a), ["* /z/second", "* /a/first"]);
    a.update(Event::DirsLoaded(store(&[
        (Path::new("/f/low"), 1.0, n),
        (Path::new("/f/high"), 9.0, n),
        (Path::new("/a/first"), 50.0, n),
        (&t.join("here"), 99.0, n),
    ])));
    let Some(Dialog::Dirs(d)) = &a.dialog else {
        panic!()
    };
    assert!(!d.loading);
    assert_eq!(
        rows(&a),
        ["* /z/second", "* /a/first", "  /f/high", "  /f/low"]
    );
    press(&mut a, KeyCode::Char('f'));
    assert_eq!(rows(&a), ["* /a/first"], "f in the last component");
    press(&mut a, KeyCode::Char('/'));
    assert_eq!(rows(&a), ["  /f/high", "  /f/low"]);
    // Forget a frequent directory: gone from the list and from the file on exit.
    press(&mut a, KeyCode::Down);
    assert!(press(&mut a, KeyCode::Delete).is_empty());
    assert_eq!(rows(&a), ["  /f/high"]);
    assert!(a.dirs.deltas.map[Path::new("/f/low")].forget);
    // Enter goes to the selected directory: a navigation.
    let fx = press(&mut a, KeyCode::Enter);
    assert!(a.dialog.is_none());
    assert!(matches!(&fx[..], [Effect::List(req, _)] if req.dir == Path::new("/f/high")));
}

/// P2 3.3: with `jump.zoxide = "auto"` the first `Ctrl+D` asks for zoxide's ranking once;
/// it merges as the larger score; a forgotten directory stays hidden. With `"off"` it is
/// never asked for. (The app only returns the effect; nothing runs zoxide here.)
#[test]
fn zoxide_is_asked_once_and_merged_by_the_larger_score() {
    let t = test_dir("dirs-zoxide-app");
    let n = now();
    let mut a = app_with(&t.path, &t.path, ZoxideMode::Auto);
    let fx = ctrl_d(&mut a);
    assert_eq!(fx, vec![Effect::LoadDirs, Effect::LoadZoxide]);
    press(&mut a, KeyCode::Esc);
    assert!(ctrl_d(&mut a).is_empty(), "asked once");
    a.update(Event::DirsLoaded(store(&[
        (Path::new("/own"), 1.0, n),    // 4
        (Path::new("/both"), 1.0, n),   // 4, zoxide 10
        (Path::new("/strong"), 5.0, n), // 20, zoxide 1
    ])));
    a.dirs.forget(Path::new("/hidden"));
    let Some(Dialog::Dirs(d)) = &a.dialog else {
        panic!()
    };
    assert!(d.loading, "zoxide is still out");
    a.update(Event::ZoxideLoaded(vec![
        ("/both".into(), 10.0),
        ("/strong".into(), 1.0),
        ("/zonly".into(), 6.0),
        ("/hidden".into(), 99.0),
    ]));
    assert_eq!(
        rows(&a),
        ["  /strong", "  /both", "  /zonly", "  /own"],
        "max(own, zoxide)"
    );
    let ranked = a.dirs.ranked(Path::new("/"), n);
    assert_eq!(ranked[1], (PathBuf::from("/both"), 10.0));

    let mut off = app(&t.path, &t.path);
    assert_eq!(ctrl_d(&mut off), vec![Effect::LoadDirs]);
    off.update(Event::ZoxideLoaded(vec![("/x".into(), 1.0)]));
    assert_eq!(off.dirs.zoxide, Zoxide::Off);
}

/// The dialog at 80x24: bookmarks marked `*`, `~` for `$HOME`, an escaped name, the help.
#[test]
fn dialog_snapshot() {
    let mut a = App::new(
        PathBuf::from("/snap/left"),
        PathBuf::from("/snap/home"),
        PathBuf::from("/snap/home"),
        config(ZoxideMode::Off),
        None,
        Depth::NoColor,
        jiff::tz::TimeZone::UTC,
    );
    let n = now();
    a.dirs.hotlist.dirs = vec![
        "/snap/home/Documents".into(),
        "/etc".into(),
        PathBuf::from(os(b"/snap/home/bad\xffname\nhere")),
    ];
    a.update(Event::DirsLoaded(store(&[
        (Path::new("/snap/home/src/manycommander"), 9.0, n),
        (Path::new("/snap/home"), 5.0, n),
        (Path::new("/var/log"), 1.0, n),
        (Path::new("/snap/left"), 100.0, n),
    ])));
    ctrl_d(&mut a);
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| manycommander::ui::draw(&mut a, f)).unwrap();
    let buf = term.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..24 {
        for x in 0..80 {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    insta::assert_snapshot!(out);
}

/// P-15 in the small: 5000 entries filtered per keystroke. The benchmark (A-DJ-5) measures
/// it in a release build; this guards against a quadratic filter.
#[test]
fn filtering_5000_entries_per_keystroke_is_fast() {
    let n = now();
    let mut s = Store::default();
    for i in 0..5000 {
        s.entries.insert(
            PathBuf::from(format!("/home/user/projects/group-{}/repo-{i}/src", i % 50)),
            Frecency {
                rank: 1.0 + (i % 97) as f64,
                last: n - (i as i64 % 30) * DAY,
            },
        );
    }
    let t = test_dir("dirs-p15");
    let mut a = app(&t.path, &t.path);
    a.update(Event::DirsLoaded(s));
    let start = Instant::now();
    ctrl_d(&mut a);
    let opened = start.elapsed();
    let mut worst = Duration::ZERO;
    for c in "group-7 repo-1 src".chars() {
        let s = Instant::now();
        press(&mut a, KeyCode::Char(c));
        worst = worst.max(s.elapsed());
    }
    let Some(Dialog::Dirs(d)) = &a.dialog else {
        panic!()
    };
    assert!(!d.is_empty());
    eprintln!("open {opened:?}, worst keystroke {worst:?}");
    assert!(worst < Duration::from_millis(100), "{worst:?}");
    // The matcher allocates nothing per candidate.
    let m = Matcher::new(b"repo src");
    assert!(m.matches(&lowered(Path::new("/x/Repo-1/SRC"))));
}

// ---- zoxide --------------------------------------------------------------------------------

#[test]
fn zoxide_output_parses() {
    let out: &[u8] = b"  12.5 /home/u/src\n   4.0 /home/u/with space \n9999.0 /\n\
        \x20 bad line\n 1.0 relative/path\n\n   x /nan\n 0.3 /home/u/caf\xc3\xa9";
    assert_eq!(
        parse_zoxide(out),
        vec![
            (PathBuf::from("/home/u/src"), 12.5),
            (PathBuf::from("/home/u/with space "), 4.0),
            (PathBuf::from("/"), 9999.0),
            (PathBuf::from("/home/u/café"), 0.3),
        ]
    );
}

/// The zoxide query runs by argv (never a shell), and a child that does not answer within
/// the timeout is killed. A fake zoxide stands in; the user's zoxide never runs here.
#[test]
fn zoxide_query_runs_by_argv_and_times_out() {
    let t = test_dir("dirs-zoxide");
    let fake = t.join("fake-zoxide.sh");
    std::fs::write(
        &fake,
        "[ \"$*\" = \"query --list --score\" ] || exit 3\n\
         [ \"$(pwd)\" = / ] || exit 4\n\
         printf '  12.5 /home/u/src\\n   4.0 /home/u/with space\\n'\n",
    )
    .unwrap();
    let sh = |script: &Path| vec![OsString::from("/bin/sh"), script.as_os_str().to_owned()];
    assert_eq!(
        zoxide_query(&sh(&fake), Duration::from_secs(10)).unwrap(),
        vec![
            (PathBuf::from("/home/u/src"), 12.5),
            (PathBuf::from("/home/u/with space"), 4.0)
        ]
    );
    let slow = t.join("slow-zoxide.sh");
    std::fs::write(&slow, "exec sleep 20\n").unwrap();
    let start = Instant::now();
    let e = zoxide_query(&sh(&slow), Duration::from_millis(300)).unwrap_err();
    assert!(e.contains("no answer"), "{e}");
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "{:?}",
        start.elapsed()
    );
    let e = zoxide_query(
        &[t.join("missing").into_os_string()],
        Duration::from_secs(1),
    );
    assert!(e.is_err());

    // Only an executable file in an absolute PATH entry is found.
    std::fs::create_dir_all(t.join("bin")).unwrap();
    std::fs::create_dir_all(t.join("plain")).unwrap();
    std::fs::write(t.join("plain/zoxide"), "").unwrap();
    std::fs::write(t.join("bin/zoxide"), "").unwrap();
    std::fs::set_permissions(t.join("bin/zoxide"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut search = OsString::from("relative:");
    search.push(t.join("plain"));
    search.push(":");
    search.push(t.join("bin"));
    assert_eq!(
        find_program("zoxide", Some(&search)),
        Some(t.join("bin/zoxide"))
    );
    assert_eq!(
        find_program("zoxide", Some(t.join("plain").as_os_str())),
        None
    );
    assert_eq!(find_program("zoxide", None), None);
}

/// The directory-store thread saves the hotlist, loads the store and, without a `PATH`,
/// answers the zoxide request with nothing; `finish` waits for queued work.
#[test]
fn store_thread_serves_requests_in_order() {
    let t = test_dir("dirs-thread");
    let paths = Paths {
        hotlist: Some(t.join("config/manycommander/hotlist.toml")),
        store: Some(t.join("state/manycommander/dirs.tsv")),
        search: None,
    };
    std::fs::create_dir_all(t.join("state/manycommander")).unwrap();
    std::fs::write(
        paths.store.as_ref().unwrap(),
        "# manycommander dirs v1\n2\t100\t/one\n",
    )
    .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let st = StoreThread::spawn(paths.clone(), move |r| {
        let _ = tx.send(r);
    })
    .unwrap();
    st.request(Request::SaveHotlist(vec!["/b1".into(), "/b2".into()]));
    st.request(Request::Load);
    st.request(Request::Zoxide);
    match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
        Reply::Loaded(s) => assert_eq!(s.entries.len(), 1),
        other => panic!("{other:?}"),
    }
    match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
        Reply::Zoxide(v) => assert!(v.is_empty()),
        other => panic!("{other:?}"),
    }
    st.request(Request::SaveHotlist(vec!["/b3".into()]));
    assert!(st.finish(Duration::from_secs(10)));
    assert_eq!(
        Hotlist::load(paths.hotlist.as_ref().unwrap()).0.dirs,
        vec![PathBuf::from("/b3")],
        "the last save wins"
    );
    // A save that fails is reported.
    let blocked = Paths {
        hotlist: Some(t.join("state/manycommander/dirs.tsv/hotlist.toml")),
        ..Paths::default()
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let st = StoreThread::spawn(blocked, move |r| {
        let _ = tx.send(r);
    })
    .unwrap();
    st.request(Request::SaveHotlist(vec!["/x".into()]));
    match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
        Reply::Failed(e) => assert!(e.contains("could not save"), "{e}"),
        other => panic!("{other:?}"),
    }
    assert!(st.finish(Duration::from_secs(10)));
}

// ---- the binary on a pty -----------------------------------------------------------------

/// End to end on a pty: `Ctrl+D` runs a fake `zoxide` found first on `PATH` (by argv, in
/// `/`) and lists its directory; `Insert` bookmarks the panel's directory and the store
/// thread writes `hotlist.toml`; `cd` records a visit that is merged into `dirs.tsv` when
/// the program exits.
#[test]
fn pty_ctrl_d_reads_a_fake_zoxide_and_exit_saves_visits() {
    use common::tui::*;
    const T: Duration = Duration::from_secs(10);
    let t = test_dir("dirs-pty");
    let home = t.join("home");
    std::fs::create_dir_all(home.join("work/project")).unwrap();
    let zdir = t.join("zdir/from-zoxide");
    std::fs::create_dir_all(&zdir).unwrap();
    let bin = t.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let tmp = bin.join("zoxide.tmp");
    std::fs::write(
        &tmp,
        format!(
            "#!/bin/sh\n[ \"$*\" = \"query --list --score\" ] || exit 3\n\
             [ \"$(pwd)\" = / ] || exit 4\nprintf '  42.0 %s\\n' '{}'\n",
            zdir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&tmp, bin.join("zoxide")).unwrap();
    let mut path = bin.clone().into_os_string();
    path.push(":");
    path.push(no_desktop_path());
    let home_s = home.to_str().unwrap();
    let mut tui = Tui::spawn(
        &[home_s, home_s],
        &home,
        &[("PATH", path.to_str().unwrap())],
        110,
        30,
    );
    assert!(tui.wait_for("10Quit", T), "{}", tui.screen());
    assert!(tui.wait_for("work", T), "{}", tui.screen());
    tui.send(b"\x04");
    assert!(tui.wait_for("Go to directory", T), "{}", tui.screen());
    assert!(tui.wait_for("from-zoxide", T), "{}", tui.screen());
    tui.send(b"\x1b[2~");
    assert!(tui.wait_for("bookmarked ~", T), "{}", tui.screen());
    tui.keys(&[ESC]);
    assert!(
        tui.wait_until(T, |t| !t.screen().contains("Go to directory")),
        "{}",
        tui.screen()
    );
    tui.send(b"cd work/project\r");
    assert!(tui.wait_for("project$", T), "{}", tui.screen());
    // A visit counts when the listing completes, not when the prompt changes: wait for
    // the empty directory's footer before quitting (a slow machine lost the visit).
    assert!(tui.wait_for("0 entries", T), "{}", tui.screen());
    tui.send(F10);
    assert_eq!(tui.wait_exit(T), Some(0));
    let hotlist = std::fs::read_to_string(home.join(".config/manycommander/hotlist.toml")).unwrap();
    assert!(
        hotlist.contains(&format!("path = \"{home_s}\"")),
        "{hotlist}"
    );
    let s = Store::load(&home.join(".local/state/manycommander/dirs.tsv")).unwrap();
    assert_eq!(s.entries[&home.join("work/project")].rank, 1.0, "{s:?}");
    assert!(
        !s.entries.contains_key(&zdir),
        "zoxide's database is only read"
    );
}
