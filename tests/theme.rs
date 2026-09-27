//! T6: theme and config. A-TH-2 (replay of the theme-set event sequence against the real
//! watcher, an `IN_CREATE` variant, next-theme noise, re-arming) and A-TH-3 (fixtures).

mod common;

use common::*;
use manycommander::config::Config;
use manycommander::theme::watch::{Target, spawn};
use manycommander::theme::{Depth, Palette, Theme};
use ratatui::style::Color;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/theme")
        .join(name)
}

/// The app side of a reload: load, and count effective changes and failed loads.
struct Loader {
    path: std::path::PathBuf,
    current: Palette,
    changes: usize,
    failures: usize,
    empties: usize,
}

impl Loader {
    fn reload(&mut self) {
        match Palette::load(&self.path) {
            Ok(p) if p.is_empty() => self.empties += 1,
            Ok(p) => {
                if p != self.current {
                    self.changes += 1;
                    self.current = p;
                }
            }
            // The current palette stays.
            Err(_) => self.failures += 1,
        }
    }
}

/// Drains notifications until `quiet` passes without one.
fn settle(rx: &mpsc::Receiver<()>, l: &mut Loader, quiet: Duration) -> usize {
    let mut n = 0;
    while rx.recv_timeout(quiet).is_ok() {
        n += 1;
        l.reload();
    }
    n
}

fn state(t: &TestDir) -> std::path::PathBuf {
    let current = t.join("state/omarchy/current");
    std::fs::create_dir_all(current.join("theme")).unwrap();
    std::fs::copy(
        fixture("tokyo-night.toml"),
        current.join("theme/colors.toml"),
    )
    .unwrap();
    std::fs::write(current.join("theme.name"), "tokyo-night\n").unwrap();
    current
}

fn watch(current: &Path) -> (mpsc::Receiver<()>, Loader) {
    let (tx, rx) = mpsc::channel();
    let target = Target::Omarchy {
        current: current.to_path_buf(),
    };
    let path = target.palette_path();
    spawn(target, move || {
        let _ = tx.send(());
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let current = Palette::load(&path).unwrap_or_default();
    (
        rx,
        Loader {
            path,
            current,
            changes: 0,
            failures: 0,
            empties: 0,
        },
    )
}

/// `omarchy-theme-set` as of this writing: fill next-theme, remove theme, move next-theme
/// into place, then rewrite theme.name.
fn theme_set(current: &Path, colors: &Path, name: &str, create_variant: bool) {
    let next = current.join("next-theme");
    std::fs::create_dir_all(&next).unwrap();
    std::fs::copy(colors, next.join("colors.toml")).unwrap();
    std::fs::write(next.join("other.conf"), "x").unwrap();
    std::fs::remove_dir_all(current.join("theme")).unwrap();
    if create_variant {
        std::fs::create_dir(current.join("theme")).unwrap();
        std::fs::copy(colors, current.join("theme/colors.toml")).unwrap();
        std::fs::remove_dir_all(&next).unwrap();
    } else {
        std::fs::rename(&next, current.join("theme")).unwrap();
    }
    std::fs::write(current.join("theme.name"), format!("{name}\n")).unwrap();
}

#[test]
fn a_th_2_theme_set_sequence_yields_one_change() {
    for create_variant in [false, true] {
        let t = test_dir("theme-ath2");
        let current = state(&t);
        let (rx, mut l) = watch(&current);
        let before = l.current.clone();
        theme_set(&current, &fixture("light.toml"), "light", create_variant);
        let notes = settle(&rx, &mut l, Duration::from_millis(300));
        assert!(notes >= 1, "create_variant={create_variant}: no reload");
        assert_eq!(
            l.changes, 1,
            "create_variant={create_variant}: exactly one effective change"
        );
        assert_eq!(
            l.failures, 0,
            "no reload ran against a missing or partial theme"
        );
        assert_eq!(l.empties, 0, "never an empty palette");
        assert_ne!(l.current, before);
        assert_eq!(l.current.mode.as_deref(), Some("light"));
    }
}

#[test]
fn next_theme_events_alone_do_not_reload() {
    let t = test_dir("theme-noise");
    let current = state(&t);
    let (rx, mut l) = watch(&current);
    let next = current.join("next-theme");
    std::fs::create_dir_all(&next).unwrap();
    std::fs::copy(fixture("light.toml"), next.join("colors.toml")).unwrap();
    std::fs::remove_dir_all(&next).unwrap();
    std::fs::write(current.join("unrelated"), "x").unwrap();
    assert_eq!(settle(&rx, &mut l, Duration::from_millis(200)), 0);
}

#[test]
fn watcher_rearms_when_the_state_directory_appears() {
    let t = test_dir("theme-rearm");
    let current = t.join("state/omarchy/current");
    std::fs::create_dir_all(t.join("state")).unwrap();
    let (rx, mut l) = watch(&current);
    std::fs::create_dir_all(current.join("theme")).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    std::fs::copy(
        fixture("tokyo-night.toml"),
        current.join("theme/colors.toml"),
    )
    .unwrap();
    std::fs::write(current.join("theme.name"), "tokyo-night\n").unwrap();
    settle(&rx, &mut l, Duration::from_millis(300));
    assert_eq!(
        l.current,
        Palette::load(&fixture("tokyo-night.toml")).unwrap()
    );
    // Once armed on current/, the normal sequence works.
    theme_set(&current, &fixture("light.toml"), "light", false);
    settle(&rx, &mut l, Duration::from_millis(300));
    assert_eq!(l.current.mode.as_deref(), Some("light"));
}

#[test]
fn theme_file_mode_sees_in_place_edits() {
    let t = test_dir("theme-file");
    let f = t.join("mine.toml");
    std::fs::copy(fixture("tokyo-night.toml"), &f).unwrap();
    let (tx, rx) = mpsc::channel();
    spawn(Target::File { path: f.clone() }, move || {
        let _ = tx.send(());
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(50));
    std::fs::write(&f, std::fs::read(fixture("light.toml")).unwrap()).unwrap();
    assert!(rx.recv_timeout(Duration::from_millis(500)).is_ok());
    assert_eq!(Palette::load(&f).unwrap().mode.as_deref(), Some("light"));
}

#[test]
fn a_th_3_fixtures() {
    let real = Palette::load(&fixture("tokyo-night.toml")).unwrap();
    assert_eq!(real.len(), 25, "orange and brown are known keys");
    assert_eq!(real.mode.as_deref(), Some("dark"));
    let th = Theme::build(Some(&real), Depth::TrueColor, false);
    assert_eq!(th.border_active.fg, Some(Color::Rgb(0x7a, 0xa2, 0xf7)));

    // Without accent, the accent roles fall back to blue.
    let no_accent = Palette::load(&fixture("no-accent.toml")).unwrap();
    let mut blue = no_accent.clone();
    let th = Theme::build(Some(&no_accent), Depth::TrueColor, false);
    assert_eq!(th.border_active.fg, Some(Color::Rgb(0x7a, 0xa2, 0xf7)));
    assert_eq!(blue.chain(&["accent", "blue"]), blue.get("blue"));
    blue = Palette::parse("accent = \"#010101\"\nblue = \"#020202\"").unwrap();
    assert_eq!(blue.chain(&["accent", "blue"]).unwrap().0, 1);

    // A missing file and an unparsable file: ANSI fallback, no crash.
    assert!(Palette::load(&fixture("does-not-exist.toml")).is_err());
    assert!(Palette::load(&fixture("unparsable.toml")).is_err());
    let fallback = Theme::build(None, Depth::TrueColor, false);
    assert_eq!(fallback.border_active.fg, Some(Color::Blue));
    assert_eq!(fallback.directory.fg, Some(Color::White));
}

#[test]
fn config_paint_background_selects_background_role() {
    let t = test_dir("theme-config");
    let p = t.join("config.toml");
    std::fs::write(&p, "paint_background = true\n").unwrap();
    let (c, err) = Config::load(&p);
    assert!(err.is_none());
    let pal = Palette::load(&fixture("tokyo-night.toml")).unwrap();
    let th = Theme::build(Some(&pal), Depth::TrueColor, c.paint_background);
    assert_eq!(th.background.bg, Some(Color::Rgb(0x1a, 0x1b, 0x26)));
    let (d, _) = Config::load(&t.join("missing.toml"));
    assert!(!d.paint_background);
    std::fs::write(&p, "paint_background = [").unwrap();
    let (e, err) = Config::load(&p);
    assert!(err.is_some() && !e.paint_background);
}
