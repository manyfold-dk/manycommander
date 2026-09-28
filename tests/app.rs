//! T7-T8 without a terminal: the App state machine driven with real listing threads
//! replaced by synchronous calls, the theme loader test double, and panel snapshots.

mod common;

use common::*;
use manycommander::app::App;
use manycommander::app::event::{Effect, Event};
use manycommander::config::Config;
use manycommander::fsops::sys::{FsIdentity, Kind, Meta, Ts};
use manycommander::panel::entry::Entry;
use manycommander::panel::listing::{self, ListingMsg};
use manycommander::theme::{Depth, Palette};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

/// Performs effects synchronously: listings run on this thread, their messages go back
/// into the app. Returns effects that were not performed.
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

fn cursor_name(app: &App) -> Vec<u8> {
    app.panel().current_name().unwrap_or(b"..").to_vec()
}

#[test]
fn theme_is_never_loaded_on_the_ui_thread() {
    let t = test_dir("app-theme");
    let path = t.join("colors.toml");
    std::fs::write(&path, "accent = \"#010203\"\n").unwrap();
    let mut a = app(&t.path, &t.path);
    // The UI thread only asks for a load.
    let fx = a.update(Event::ReloadTheme);
    assert_eq!(fx, vec![Effect::LoadTheme]);
    let ui = std::thread::current().id();
    let ran_on = std::sync::Arc::new(std::sync::Mutex::new(None));
    let r = ran_on.clone();
    let loader: manycommander::app::runtime::Loader = std::sync::Arc::new(move |p: &Path| {
        *r.lock().unwrap() = Some(std::thread::current().id());
        Palette::load(p)
    });
    let (tx, rx) = std::sync::mpsc::channel();
    manycommander::app::runtime::spawn_theme_load(loader, path, tx);
    let ev = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let on = ran_on.lock().unwrap().expect("the loader ran");
    assert_ne!(on, ui, "theme::load ran on the UI thread");
    a.redraw = false;
    a.update(ev);
    assert!(a.redraw, "a changed palette redraws fully");
    assert!(a.palette.is_some());
}

#[test]
fn a_ui_2_refresh_keeps_the_cursor_on_its_name() {
    let t = test_dir("app-refresh");
    for n in ["a", "b", "c", "d"] {
        write(&t.join(n), n.as_bytes());
    }
    let mut a = app(&t.path, &t.path);
    let fx = a.start();
    run(&mut a, fx);
    a.panel_mut().ensure_sorted();
    a.panel_mut().cursor_to_name(b"c");
    // External changes: a new name before the cursor, one removed after.
    write(&t.join("a0"), b"x");
    std::fs::remove_file(t.join("d")).unwrap();
    let fx = key_ctrl(&mut a, 'r');
    run(&mut a, fx);
    a.panel_mut().ensure_sorted();
    assert_eq!(cursor_name(&a), b"c");
    let names: Vec<_> = a
        .panel()
        .list
        .visible
        .iter()
        .map(|&i| a.panel().list.name(i).to_vec())
        .collect();
    assert_eq!(
        names,
        [b"a".to_vec(), b"a0".to_vec(), b"b".to_vec(), b"c".to_vec()]
    );
}

fn key_ctrl(app: &mut App, c: char) -> Vec<Effect> {
    app.update(Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::CONTROL,
        ),
        std::time::Instant::now(),
    ))
}

#[test]
fn panel_watcher_reports_changes_within_a_second() {
    let t = test_dir("app-watch");
    let (tx, rx) = std::sync::mpsc::channel();
    let w = manycommander::panel::watch::PanelWatcher::spawn(move |slot| {
        let _ = tx.send(slot);
    })
    .unwrap();
    w.set(7, Some(t.path.clone()));
    std::thread::sleep(Duration::from_millis(100));
    let start = std::time::Instant::now();
    write(&t.join("new"), b"x");
    assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), 7);
    assert!(start.elapsed() < Duration::from_secs(1));
    std::fs::remove_file(t.join("new")).unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), 7);
}

#[test]
fn deleting_the_current_directory_moves_to_its_parent() {
    let t = test_dir("app-deleted");
    std::fs::create_dir_all(t.join("gone/deeper")).unwrap();
    let mut a = app(&t.join("gone/deeper"), &t.path);
    let fx = a.start();
    run(&mut a, fx);
    std::fs::remove_dir(t.join("gone/deeper")).unwrap();
    std::fs::remove_dir(t.join("gone")).unwrap();
    let slot = a.panel().slot;
    let fx = a.update(Event::DirChanged { slot });
    run(&mut a, fx);
    assert_eq!(a.panel().dir, t.path, "the nearest existing ancestor");
    assert!(a.panel().loading.is_none());
}

#[test]
fn listing_panic_becomes_listing_failed() {
    let t = test_dir("app-panic");
    std::fs::create_dir(t.join("sub")).unwrap();
    let mut a = app(&t.path, &t.path);
    let fx = a.start();
    run(&mut a, fx);
    // Enter `sub`, with a lister that panics.
    let req = a
        .panel_mut()
        .navigate(t.join("sub"), None, listing::Alive::running());
    let msgs = std::cell::RefCell::new(Vec::new());
    listing::guarded(&req, &|m| msgs.borrow_mut().push(m), |_, _| {
        panic!("injected")
    });
    let msgs = msgs.into_inner();
    assert!(matches!(msgs[..], [ListingMsg::Failed { .. }]));
    for m in msgs {
        a.update(Event::Listing(m));
    }
    assert_eq!(a.panel().dir, t.path, "back where it came from");
    assert!(
        a.panel()
            .message
            .as_deref()
            .unwrap_or("")
            .contains("internal error")
    );
}

fn press(a: &mut App, code: crossterm::event::KeyCode) -> Vec<Effect> {
    a.update(Event::Key(
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE),
        std::time::Instant::now(),
    ))
}

#[test]
fn escape_during_a_load_returns_and_blocked_loads_are_limited() {
    use crossterm::event::KeyCode;
    let t = test_dir("app-esc");
    for n in ["one", "two", "three", "four", "five"] {
        std::fs::create_dir(t.join(n)).unwrap();
    }
    let mut a = app(&t.path, &t.path);
    let fx = a.start();
    run(&mut a, fx);
    // Four loads whose threads never return (their effects are not performed): Esc
    // returns to the directory at once each time.
    for n in ["one", "two", "three", "four"] {
        a.panel_mut().ensure_sorted();
        a.panel_mut().cursor_to_name(n.as_bytes());
        let fx = press(&mut a, KeyCode::Enter);
        assert!(matches!(fx[..], [Effect::List(..)]), "{n}: {fx:?}");
        assert!(a.panel().is_loading());
        press(&mut a, KeyCode::Esc);
        assert_eq!(a.panel().dir, t.path, "{n}");
        assert!(a.panel().loading.is_none());
        a.panel_mut().ensure_sorted();
        assert_eq!(cursor_name(&a), n.as_bytes(), "the cursor is back on {n}");
    }
    // A second load of a directory whose first load is stuck is refused.
    a.panel_mut().cursor_to_name(b"one");
    assert!(press(&mut a, KeyCode::Enter).is_empty());
    assert!(
        a.status.as_ref().unwrap().text.contains("still blocked"),
        "{:?}",
        a.status
    );
    // At most four abandoned loads: a fifth directory is refused too.
    a.panel_mut().cursor_to_name(b"five");
    assert!(press(&mut a, KeyCode::Enter).is_empty());
    assert!(
        a.status.as_ref().unwrap().text.contains("too many"),
        "{:?}",
        a.status
    );
}

// ---- snapshots ------------------------------------------------------------------------------

fn meta(kind: Kind, perm: u32, size: u64, mtime: i64) -> Meta {
    Meta {
        kind,
        perm,
        uid: 1000,
        nlink: 1,
        size,
        blocks: size.div_ceil(512),
        id: FsIdentity::default(),
        atime: Ts {
            sec: mtime,
            nsec: 0,
        },
        mtime: Ts {
            sec: mtime,
            nsec: 0,
        },
        ctime: Ts {
            sec: mtime,
            nsec: 0,
        },
        mount_root: false,
    }
}

fn synthetic(a: &mut App, side: usize, entries: &[(&[u8], Kind, u32, u64)]) {
    let slot = a.sides[side].panel().slot;
    let dir = a.sides[side].panel().dir.clone();
    let alive = listing::Alive::running();
    let req = a.sides[side].panel_mut().navigate(dir.clone(), None, alive);
    let mut names = Vec::new();
    let mut es = Vec::new();
    for (i, (n, k, p, s)) in entries.iter().enumerate() {
        es.push(Entry::new(
            &mut names,
            n,
            &meta(*k, *p, *s, 1_790_000_000 + i as i64 * 86_400),
        ));
    }
    a.update(Event::Listing(ListingMsg::Batch {
        slot,
        generation: req.generation,
        entries: es,
        names,
    }));
    a.update(Event::Listing(ListingMsg::Done {
        slot,
        generation: req.generation,
        dir,
        elapsed: Duration::ZERO,
    }));
    a.update(Event::Listing(ListingMsg::FreeSpace {
        slot,
        generation: req.generation,
        free: 123 << 30,
        total: 500 << 30,
    }));
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

fn snapshot_app() -> App {
    let mut a = app(
        Path::new("/snap/left"),
        Path::new("/snap/right/with a long name"),
    );
    let long = [b'L'; 255];
    let left: Vec<(&[u8], Kind, u32, u64)> = vec![
        (b"src", Kind::Dir, 0o755, 0),
        (b".config", Kind::Dir, 0o700, 0),
        (b"file10.txt", Kind::File, 0o644, 1234),
        (b"file2.txt", Kind::File, 0o644, 99_999),
        (b"run.sh", Kind::File, 0o755, 512),
        (b"new\nline", Kind::File, 0o644, 1),
        (b"-leading", Kind::File, 0o644, 2),
        (b"it's", Kind::File, 0o644, 3),
        (b"bad\xff\xfeutf8", Kind::File, 0o644, 4),
        (&long, Kind::File, 0o644, 5),
        (b"link", Kind::Symlink, 0o777, 8),
        (b"big.iso", Kind::File, 0o644, 4_700_000_000),
    ];
    synthetic(&mut a, 0, &left);
    synthetic(&mut a, 1, &[(b"only", Kind::File, 0o600, 7)]);
    a.panel_mut().cursor_to_name(b"file2.txt");
    a.panel_mut().toggle_mark(false);
    a.panel_mut().cursor_to_name(b"run.sh");
    a
}

/// Design 7.1: the active cursor row is `background` on `accent` across the whole row. A
/// Line's style does not override its spans' own colours, and the text-only snapshots
/// cannot see colours, so this checks every cell of the bar.
#[test]
fn active_cursor_row_uses_the_cursor_colours_in_every_cell() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/theme/tokyo-night.toml"
    ))
    .unwrap();
    let mut a = App::new(
        PathBuf::from("/snap/left"),
        PathBuf::from("/snap/right"),
        PathBuf::from("/snap/right"),
        Config::default(),
        Some(Palette::parse(&text).unwrap()),
        Depth::TrueColor,
        jiff::tz::TimeZone::UTC,
    );
    let left: Vec<(&[u8], Kind, u32, u64)> = vec![
        (b"src", Kind::Dir, 0o755, 0),
        (b"notes.txt", Kind::File, 0o644, 99),
        (b"run.sh", Kind::File, 0o755, 512),
    ];
    synthetic(&mut a, 0, &left);
    synthetic(&mut a, 1, &[(b"only", Kind::File, 0o600, 7)]);
    let (w, h) = (100, 20);
    let want = a.theme.cursor_active;
    for name in ["src", "run.sh"] {
        a.panel_mut().cursor_to_name(name.as_bytes());
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| manycommander::ui::draw(&mut a, f)).unwrap();
        let buf = term.backend().buffer();
        let bar: Vec<_> = (0..h)
            .flat_map(|y| (0..w / 2).map(move |x| (x, y)))
            .map(|p| &buf[p])
            .filter(|c| Some(c.bg) == want.bg)
            .collect();
        assert!(bar.len() > 30, "no cursor bar for {name}");
        for c in bar {
            assert_eq!(
                Some(c.fg),
                want.fg,
                "{:?} on the {name} cursor row",
                c.symbol()
            );
        }
    }
}

#[test]
fn panel_snapshot_80x24() {
    let mut a = snapshot_app();
    insta::assert_snapshot!(render(&mut a, 80, 24));
}

#[test]
fn panel_snapshot_200x60() {
    let mut a = snapshot_app();
    insta::assert_snapshot!(render(&mut a, 200, 60));
}

#[test]
fn tiny_terminal_does_not_panic() {
    let mut a = snapshot_app();
    for (w, h) in [(10, 5), (20, 6), (40, 8), (1, 1), (79, 23)] {
        render(&mut a, w, h);
    }
}

#[test]
fn question_dialog_snapshots() {
    use manycommander::fsops::question::{Question, Side};
    use manycommander::ui::dialog::Dialog;
    let side = |kind, size, sec| Side {
        kind,
        size,
        mtime: Ts { sec, nsec: 0 },
        readonly: false,
    };
    let p = PathBuf::from("/snap/dst/report.pdf");
    let qs = vec![
        (
            "file_exists",
            Question::FileExists {
                path: p.clone(),
                src: side(Kind::File, 2048, 1_790_000_000),
                dst: Side {
                    readonly: true,
                    ..side(Kind::Symlink, 12, 1_780_000_000)
                },
                dst_is_symlink: true,
            },
        ),
        (
            "dir_exists",
            Question::DirExists {
                path: PathBuf::from("/snap/dst/photos"),
                src: side(Kind::Dir, 0, 1_790_000_000),
                dst: side(Kind::Dir, 0, 1_780_000_000),
            },
        ),
        (
            "type_mismatch",
            Question::TypeMismatch {
                path: p.clone(),
                src: side(Kind::Dir, 0, 1_790_000_000),
                dst: side(Kind::File, 5, 1_780_000_000),
            },
        ),
        (
            "error",
            Question::Error {
                path: p.clone(),
                op: "write",
                errno: rustix::io::Errno::NOSPC,
            },
        ),
        (
            "trash_unavailable",
            Question::TrashUnavailable {
                path: PathBuf::from("/mnt/usb/old"),
                reason: ".Trash-1000 is not a directory".into(),
            },
        ),
        (
            "link_exists",
            Question::LinkExists {
                path: PathBuf::from("/snap/dst/report.pdf"),
                existing: Some(side(Kind::File, 2048, 1_780_000_000)),
            },
        ),
        (
            "confirm_delete",
            Question::ConfirmDelete {
                files: 812,
                dirs: 40,
                bytes: 3 << 30,
                single: None,
            },
        ),
    ];
    for (name, q) in qs {
        let mut a = snapshot_app();
        let (tx, _rx) = std::sync::mpsc::channel();
        a.dialog = Some(Dialog::question(q, tx));
        insta::assert_snapshot!(format!("question_{name}"), render(&mut a, 80, 24));
    }
}

// ---- M2: tabs and restore (design 11.5) --------------------------------------------------

#[test]
fn state_round_trip_through_the_app() {
    use manycommander::app::state::State;
    let t = test_dir("app-state");
    for d in ["a", "b", "c"] {
        std::fs::create_dir(t.join(d)).unwrap();
    }
    let mut a = app(&t.join("a"), &t.join("b"));
    let fx = a.start();
    run(&mut a, fx);
    let fx = key_ctrl(&mut a, 't');
    run(&mut a, fx);
    a.panel_mut()
        .set_sort(manycommander::panel::sort::SortKey::Size);
    // Tab: the right side becomes active.
    let fx = a.update(Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Tab,
            crossterm::event::KeyModifiers::NONE,
        ),
        std::time::Instant::now(),
    ));
    run(&mut a, fx);
    a.history.push(b"ls -l");
    a.history.push(b"printf '%s' 'bad\xff'");
    let s = a.state();
    let path = t.join("state/state.toml");
    s.save(&path).unwrap();
    let back = State::load(&path).unwrap();
    assert_eq!(back, s);
    let mut b = app(&t.path, &t.path);
    b.restore(&back, false, false);
    assert_eq!(b.sides[0].tabs.len(), 2);
    assert_eq!(b.sides[0].active, 1);
    assert_eq!(b.active, 1);
    assert_eq!(
        b.sides[0].tabs[1].sort.key,
        manycommander::panel::sort::SortKey::Size
    );
    assert_eq!(
        b.history.items,
        [b"ls -l".to_vec(), b"printf '%s' 'bad\xff'".to_vec()]
    );
    // A command-line directory wins for its side.
    let mut c2 = app(&t.join("c"), &t.path);
    c2.restore(&back, true, false);
    assert_eq!(c2.sides[0].tabs.len(), 1);
    assert_eq!(c2.sides[0].panel().dir, t.join("c"));
}

#[test]
fn restored_missing_path_falls_back_to_nearest_ancestor() {
    use manycommander::app::state::{Bytes, SideState, State, Tab};
    let t = test_dir("app-restore-missing");
    std::fs::create_dir_all(t.join("kept")).unwrap();
    let gone = t.join("kept/gone/deeper");
    let s = State {
        left: SideState {
            tabs: vec![Tab {
                path: Bytes::of(gone.as_os_str().as_encoded_bytes()),
                sort: Default::default(),
                reverse: false,
                hidden: true,
            }],
            active: 0,
        },
        ..State::default()
    };
    let mut a = app(&t.path, &t.path);
    a.restore(&s, false, false);
    let fx = a.start();
    run(&mut a, fx);
    assert_eq!(a.sides[0].panel().dir, t.join("kept"));
}

#[test]
fn hidden_tab_releases_its_listing_and_keeps_marks() {
    let t = test_dir("app-tab-release");
    for n in ["x", "y", "z"] {
        write(&t.join(n), n.as_bytes());
    }
    let mut a = app(&t.path, &t.path);
    let fx = a.start();
    run(&mut a, fx);
    a.panel_mut().ensure_sorted();
    a.panel_mut().cursor_to_name(b"y");
    a.panel_mut().toggle_mark(false);
    let fx = key_ctrl(&mut a, 't');
    let rest = run(&mut a, fx);
    assert!(
        rest.iter()
            .any(|e| matches!(e, Effect::Watch { dir: None, .. })),
        "the hidden tab is unwatched: {rest:?}"
    );
    assert!(a.sides[0].tabs[0].list.entries.is_empty(), "released");
    assert_eq!(a.sides[0].tabs.len(), 2);
    // Back to the first tab: it reloads with its mark and cursor.
    let fx = a.update(Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::PageUp,
            crossterm::event::KeyModifiers::ALT,
        ),
        std::time::Instant::now(),
    ));
    run(&mut a, fx);
    a.panel_mut().ensure_sorted();
    assert_eq!(a.sides[0].active, 0);
    assert_eq!(a.panel().marked, 1);
    assert_eq!(cursor_name(&a), b"y");
}

/// P2 2.2: a directory panel's selection reaches every selection verb as one group; Shift+F6
/// is a move of one group with one name. The dialogs keep their M1 text.
#[test]
fn selection_reaches_the_job_as_one_group() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use manycommander::app::event::JobEvent;
    use manycommander::fsops::group::Group;
    use manycommander::fsops::job::{JobSpec, Report};
    use std::ffi::OsString;
    let l = test_dir("app-groups-left");
    let r = test_dir("app-groups-right");
    for n in ["a", "b", "c"] {
        write(&l.join(n), n.as_bytes());
    }
    let mut a = app(&l.path, &r.path);
    let fx = a.start();
    run(&mut a, fx);
    a.panel_mut().ensure_sorted();
    for n in [b"a", b"c"] {
        a.panel_mut().cursor_to_name(n);
        a.panel_mut().toggle_mark(false);
    }
    let marked = || Group::new(&l.path, vec![OsString::from("a"), OsString::from("c")]);
    assert_eq!(a.panel().selection_groups(), vec![marked()]);
    let key = |a: &mut App, code, m| {
        a.update(Event::Key(
            KeyEvent::new(code, m),
            std::time::Instant::now(),
        ))
    };
    // Starts the job with `keys`, returns its spec and ends the job.
    let job = |a: &mut App, keys: &[(KeyCode, KeyModifiers)], typed: &str| {
        for (code, m) in keys {
            key(a, *code, *m);
        }
        if !typed.is_empty() {
            a.update(Event::Paste(typed.into()));
        }
        let fx = key(a, KeyCode::Enter, KeyModifiers::NONE);
        let [Effect::StartJob(spec)] = &fx[..] else {
            panic!("{fx:?}");
        };
        let spec = spec.clone();
        let fx = a.update(Event::Job(JobEvent::Done(Report::new(spec.verb()))));
        run(a, fx);
        spec
    };
    let none = KeyModifiers::NONE;
    let shift = KeyModifiers::SHIFT;
    assert_eq!(
        job(&mut a, &[(KeyCode::F(5), none)], ""),
        JobSpec::Copy {
            groups: vec![marked()],
            dst: r.path.clone(),
        }
    );
    assert_eq!(
        job(&mut a, &[(KeyCode::F(6), none)], ""),
        JobSpec::Move {
            groups: vec![marked()],
            dst: r.path.clone(),
        }
    );
    assert_eq!(
        job(&mut a, &[(KeyCode::F(8), none)], ""),
        JobSpec::Trash {
            groups: vec![marked()],
        }
    );
    assert_eq!(
        job(&mut a, &[(KeyCode::F(8), shift)], ""),
        JobSpec::Delete {
            groups: vec![marked()],
        }
    );
    a.panel_mut().ensure_sorted();
    a.panel_mut().cursor_to_name(b"b");
    assert_eq!(
        job(&mut a, &[(KeyCode::F(6), shift)], "2"),
        JobSpec::Move {
            groups: vec![Group::new(&l.path, vec![OsString::from("b")])],
            dst: l.join("b2"),
        }
    );
}

/// P2 8.1, 8.2, 10: `Alt+L` and `Alt+A` open their forms, also with text on the command
/// line; a submit starts the job the form describes; a form error keeps the form open.
#[test]
fn link_and_attribute_forms_start_their_jobs() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use manycommander::app::event::JobEvent;
    use manycommander::fsops::attr::ModeChange;
    use manycommander::fsops::group::Group;
    use manycommander::fsops::job::{JobSpec, Report};
    use manycommander::fsops::link::LinkKind;
    use manycommander::fsops::sys::Ts;
    use manycommander::ui::dialog::Dialog;
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    let l = test_dir("app-forms-left");
    let r = test_dir("app-forms-right");
    for n in ["a", "b"] {
        write(&l.join(n), n.as_bytes());
    }
    std::fs::set_permissions(l.join("a"), std::fs::Permissions::from_mode(0o640)).unwrap();
    let mut a = app(&l.path, &r.path);
    let fx = a.start();
    run(&mut a, fx);
    a.panel_mut().ensure_sorted();
    a.panel_mut().cursor_to_name(b"a");
    let key = |a: &mut App, code, m| {
        a.update(Event::Key(
            KeyEvent::new(code, m),
            std::time::Instant::now(),
        ))
    };
    let none = KeyModifiers::NONE;
    let alt = KeyModifiers::ALT;
    let typed = |a: &mut App, s: &str| {
        for c in s.chars() {
            key(a, KeyCode::Char(c), KeyModifiers::NONE);
        }
    };
    // Submits the open form, returns the job it starts and ends that job.
    let submit = |a: &mut App| {
        let fx = key(a, KeyCode::Enter, KeyModifiers::NONE);
        let [Effect::StartJob(spec)] = &fx[..] else {
            panic!("{fx:?}");
        };
        let spec = spec.clone();
        assert!(a.dialog.is_none());
        let fx = a.update(Event::Job(JobEvent::Done(Report::new(spec.verb()))));
        run(a, fx);
        a.panel_mut().ensure_sorted();
        spec
    };
    let one = || vec![Group::new(&l.path, vec![OsString::from("a")])];
    // Alt+L is always active: text on the command line stays there.
    a.line.set(b"echo");
    key(&mut a, KeyCode::Char('l'), alt);
    assert!(matches!(a.dialog, Some(Dialog::Form { .. })));
    assert_eq!(
        submit(&mut a),
        JobSpec::Link {
            groups: one(),
            dst: r.join("a"),
            kind: LinkKind::Relative,
        }
    );
    assert_eq!(a.line.bytes(), b"echo");
    a.line.clear();
    // The type is the second field: Tab, then Right twice is "hard".
    key(&mut a, KeyCode::Char('l'), alt);
    key(&mut a, KeyCode::Tab, none);
    key(&mut a, KeyCode::Right, none);
    key(&mut a, KeyCode::Right, none);
    let Some(Dialog::Form { form, .. }) = &a.dialog else {
        panic!("no form");
    };
    assert_eq!(form.chosen(1), 2);
    assert_eq!(
        submit(&mut a),
        JobSpec::Link {
            groups: one(),
            dst: r.join("a"),
            kind: LinkKind::Hard,
        }
    );
    // Several entries link into the other panel's directory.
    a.panel_mut().mark_all();
    key(&mut a, KeyCode::Char('l'), alt);
    let JobSpec::Link { groups, dst, .. } = submit(&mut a) else {
        panic!("not a link job");
    };
    assert_eq!(groups[0].names.len(), 2);
    assert_eq!(dst, r.path);
    a.panel_mut().invert_marks();
    // Alt+A: the mode field starts empty (unchanged); the one selected entry's mode is a
    // hint in the label and the preview.
    a.panel_mut().cursor_to_name(b"a");
    key(&mut a, KeyCode::Char('a'), alt);
    let Some(Dialog::Form { form, .. }) = &a.dialog else {
        panic!("no form");
    };
    assert_eq!(form.text_of(0), b"");
    assert_eq!(form.status, ["a: rw-r-----"]);
    typed(&mut a, "u+x");
    let Some(Dialog::Form { form, .. }) = &a.dialog else {
        panic!("no form");
    };
    assert_eq!(form.status, ["a: rw-r----- -> rwxr-----"]);
    key(&mut a, KeyCode::Tab, none);
    typed(&mut a, "2026-09-28 12:34");
    key(&mut a, KeyCode::Tab, none);
    key(&mut a, KeyCode::Char(' '), none);
    assert_eq!(
        submit(&mut a),
        JobSpec::Attr {
            groups: one(),
            mode: Some(ModeChange::parse(b"u+x").unwrap()),
            mtime: Some(Ts {
                sec: 1_790_598_840,
                nsec: 0
            }),
            recursive: true,
        }
    );
    // A bad mode blocks Enter with a message; Esc closes the form.
    key(&mut a, KeyCode::Char('a'), alt);
    key(&mut a, KeyCode::Char('u'), KeyModifiers::CONTROL);
    typed(&mut a, "u+q");
    assert!(key(&mut a, KeyCode::Enter, none).is_empty());
    let Some(Dialog::Form { form, .. }) = &a.dialog else {
        panic!("the form stays open");
    };
    assert!(
        form.error
            .as_deref()
            .is_some_and(|e| e.starts_with("Mode: ")),
        "{:?}",
        form.error
    );
    key(&mut a, KeyCode::Esc, none);
    assert!(a.dialog.is_none());
}

#[test]
fn form_snapshots() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for (name, c) in [("link", 'l'), ("attributes", 'a')] {
        let mut a = snapshot_app();
        a.update(Event::Key(
            KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT),
            std::time::Instant::now(),
        ));
        insta::assert_snapshot!(format!("form_{name}"), render(&mut a, 80, 24));
    }
}
