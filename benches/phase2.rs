//! Phase 2 in-process benchmarks (P2 11): P-12 re-filter, P-13 compare by date and size,
//! P-14 multi-rename preview, P-15 directories dialog, and the UI thread's share of `Ctrl+R`
//! in a results tab (against P-1). `scripts/bench/run.sh` parses criterion's estimates
//! against the targets. Run by `cargo test --all-targets` too, then on small fixtures.

#[allow(dead_code)]
mod common;

use criterion::{Criterion, criterion_group, criterion_main};
use crossterm::event::{KeyCode, KeyEvent};
use manycommander::app::forms::compare_side;
use manycommander::compare::{self, Mode, Request};
use manycommander::dirs::{Dirs, Store};
use manycommander::find::{self, FindMsg, FindSpec, Search};
use manycommander::fsops::group::Group;
use manycommander::fsops::sys::Ts;
use manycommander::panel::Panel;
use manycommander::panel::listing::{self, Alive, ListingMsg};
use manycommander::rename::{Directory, Entry as RenameEntry};
use manycommander::ui::dirs::DirsDialog;
use manycommander::ui::multirename::{
    RENAME_NAME, RENAME_REGEX, RENAME_REPLACE, RENAME_SEARCH, RenameTool,
};
use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Benchmarks run with `--bench`; `cargo test` runs them once, on small fixtures.
fn full_run() -> bool {
    std::env::args().any(|a| a == "--bench")
}

/// Full size, or the small size of a `cargo test` run.
fn size(full: usize, small: usize) -> usize {
    if full_run() { full } else { small }
}

/// The listing bench's fixture (`benches/listing.rs`): `n` empty files `fileN.txt` and a
/// hidden `.complete`.
fn list_fixture(n: usize) -> PathBuf {
    let dir = common::base().join(format!("list{n}"));
    let done = dir.join(".complete");
    if !done.exists() {
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..n {
            let p = dir.join(format!("file{i}.txt"));
            if !p.exists() {
                std::fs::write(&p, b"").unwrap();
            }
        }
        std::fs::write(&done, b"").unwrap();
    }
    dir
}

/// A panel with `dir` listed and sorted, as the UI holds it.
fn load(slot: usize, dir: &Path) -> Panel {
    let mut p = Panel::new(slot, dir.to_path_buf());
    let req = p.navigate(dir.to_path_buf(), None, Alive::running());
    let msgs = RefCell::new(Vec::new());
    listing::list(&req, &|m| msgs.borrow_mut().push(m));
    for m in msgs.into_inner() {
        match m {
            ListingMsg::Batch {
                generation,
                entries,
                names,
                ..
            } => p.on_batch(generation, entries, &names),
            ListingMsg::Done {
                generation, dir, ..
            } => {
                p.on_done(generation, dir);
            }
            _ => {}
        }
    }
    p
}

/// A results panel with every entry of `root` (a find with an empty name), sorted.
fn results(root: &Path) -> Panel {
    let spec = FindSpec {
        root: root.to_path_buf(),
        hidden: true,
        stay_on_fs: true,
        ..FindSpec::default()
    };
    let search = Arc::new(Search::new(1, spec));
    let got = Mutex::new(Vec::new());
    find::run(&search, &|m| {
        if let FindMsg::Batch { entries, names, .. } = m {
            got.lock().unwrap().push((entries, names));
        }
    });
    let mut p = Panel::results(0, search);
    for (entries, names) in got.into_inner().unwrap() {
        p.append_results(entries, &names);
    }
    p.ensure_sorted();
    p
}

/// P-12 (A-QF-3): one keystroke of the quick filter on 100k entries, typed and deleted in
/// turn: a substring that narrows, a first character that keeps every entry, and a glob.
fn p12(c: &mut Criterion) {
    let dir = list_fixture(size(100_000, 1_000));
    let mut p = load(0, &dir);
    assert!(p.list.entries.len() >= 1_000);
    let mut g = c.benchmark_group("p12");
    g.sample_size(20);
    for (id, a, b) in [
        ("substring", &b"file1"[..], &b"file12"[..]),
        ("first_char", b"", b"f"),
        ("glob", b"*1*.txt", b"*12*.txt"),
    ] {
        let mut k = 0u64;
        g.bench_function(id, |bch| {
            bch.iter(|| {
                k += 1;
                p.set_filter(if k.is_multiple_of(2) { a } else { b });
                p.list.visible.len()
            })
        });
    }
    p.set_filter(b"");
    g.finish();
}

/// P-13 (A-CD-3): two 100k-entry listings compared by date and size. `ui_share` is what the
/// UI thread does (copy both panels' visible entries); `compare_thread` is the compare
/// thread's whole run (open both directories, `fstatfs`, compare, send the marks).
fn p13(c: &mut Criterion) {
    let (a, b) = common::compare_pair(size(100_000, 1_000));
    let left = load(0, &a);
    let right = load(1, &b);
    let mut g = c.benchmark_group("p13");
    g.sample_size(20);
    g.bench_function("ui_share", |bch| {
        bch.iter_with_large_drop(|| (compare_side(&left), compare_side(&right)))
    });
    let req = Request {
        id: 1,
        mode: Mode::DateSize,
        include_dirs: false,
        left: compare_side(&left),
        right: compare_side(&right),
    };
    let cancel = AtomicBool::new(false);
    let marks = Mutex::new(0usize);
    compare::run(&req, &cancel, &|m| {
        if let compare::CompareMsg::Marks { marks: mk, .. } = m {
            *marks.lock().unwrap() = mk.left.len() + mk.right.len();
        }
    });
    assert!(*marks.lock().unwrap() > 0, "the fixture differs");
    g.bench_function("compare_thread", |bch| {
        bch.iter(|| compare::run(&req, &cancel, &|_| {}))
    });
    g.finish();
}

fn t0() -> Ts {
    Ts {
        sec: 1_767_323_045,
        nsec: 0,
    }
}

/// The multi-rename tool on `n` selected names in one directory that lists `n` other names.
fn rename_tool(n: usize) -> RenameTool {
    let entries: Vec<RenameEntry> = (0..n)
        .map(|i| RenameEntry {
            name: format!("IMG_{i:05} holiday photo.JPG").into_bytes(),
            mtime: t0(),
            dir: 0,
        })
        .collect();
    let dirs = vec![Directory {
        name: b"pictures".to_vec(),
        others: (0..n)
            .map(|i| format!("other{i}").into_bytes())
            .collect::<HashSet<_>>(),
    }];
    let names = entries
        .iter()
        .map(|e| OsString::from(String::from_utf8(e.name.clone()).unwrap()))
        .collect();
    let groups = vec![Group::new("/pictures", names)];
    RenameTool::new(entries, dirs, groups, jiff::tz::TimeZone::system(), None)
}

fn key(c: KeyCode) -> KeyEvent {
    KeyEvent::from(c)
}

/// The next keystroke of a character typed and deleted in turn: `c`, then Backspace.
fn toggle(k: &mut u64, c: char) -> KeyEvent {
    *k += 1;
    key(if k.is_multiple_of(2) {
        KeyCode::Backspace
    } else {
        KeyCode::Char(c)
    })
}

/// Types `text` into field `field` of the tool, without refreshing.
fn type_into(t: &mut RenameTool, field: usize, text: &str) {
    t.form.focus = field;
    t.paste(text);
}

/// P-14 (A-MR-7): one keystroke in the multi-rename tool with 10k names selected: the
/// form edit, then the preview recomputed and checked (`RenameTool::refresh`, as the app
/// runs it on every change). A character is typed and deleted in turn.
fn p14(c: &mut Criterion) {
    let n = size(10_000, 100);
    let mut g = c.benchmark_group("p14");
    g.sample_size(20);
    // Defaults ([N] and [E]): typing into the name mask renames every entry.
    let mut t = rename_tool(n);
    t.form.focus = RENAME_NAME;
    let mut k = 0u64;
    g.bench_function("name_mask", |b| {
        b.iter(|| {
            t.handle(toggle(&mut k, 'x'));
            t.refresh();
            t.preview.changed
        })
    });
    // A counter, the date, literal search and replace, title case.
    let mut t = rename_tool(n);
    t.form.focus = RENAME_NAME;
    let len = t.form.text_of(RENAME_NAME).len();
    for _ in 0..len {
        t.handle(key(KeyCode::Backspace));
    }
    type_into(&mut t, RENAME_NAME, "[N]_[C] [Y]-[M]-[D]");
    type_into(&mut t, RENAME_SEARCH, "photo");
    type_into(&mut t, RENAME_REPLACE, "pic");
    t.form.focus = RENAME_NAME;
    t.refresh();
    assert_eq!(t.preview.errors, 0);
    let mut k = 0u64;
    g.bench_function("counter_date_search", |b| {
        b.iter(|| {
            t.handle(toggle(&mut k, 'x'));
            t.refresh();
            t.preview.changed
        })
    });
    // A regex with groups; typing into Replace compiles the search again every time.
    let mut t = rename_tool(n);
    type_into(&mut t, RENAME_SEARCH, r"IMG_(\d+) (\w+)");
    type_into(&mut t, RENAME_REPLACE, "${2}_$1");
    t.form.focus = RENAME_REGEX;
    t.handle(key(KeyCode::Char(' ')));
    t.form.focus = RENAME_REPLACE;
    t.refresh();
    assert_eq!(t.preview.errors, 0, "{:?}", t.preview.error);
    assert_eq!(t.preview.changed, n);
    let mut k = 0u64;
    g.bench_function("regex_replace", |b| {
        b.iter(|| {
            t.handle(toggle(&mut k, 'x'));
            t.refresh();
            t.preview.changed
        })
    });
    g.finish();
}

/// P-15 (A-DJ-5): the directories dialog with 5000 frecency entries. `open` ranks the
/// store and fills the dialog (`Ctrl+D`); `keystroke` re-filters the ranked list for a
/// two-keyword filter, a character typed and deleted in turn.
fn p15(c: &mut Criterion) {
    let now = manycommander::dirs::now();
    let store = Store::parse(common::dirs_tsv(size(5_000, 50), now).as_bytes());
    let mut dirs = Dirs::new(false);
    dirs.loaded(store);
    let bookmarks: Vec<PathBuf> = (0..20)
        .map(|i| PathBuf::from(format!("/home/me/bookmark{i}")))
        .collect();
    let exclude = PathBuf::from("/home/me/src/project0000");
    let home = Path::new("/home/me");
    let mut g = c.benchmark_group("p15");
    g.sample_size(20);
    g.bench_function("open", |b| {
        b.iter_with_large_drop(|| {
            let frequent = dirs
                .ranked(&exclude, now)
                .into_iter()
                .map(|(p, _)| p)
                .collect();
            let mut d = DirsDialog::new(b"", home);
            d.set(&bookmarks, frequent, false);
            d
        })
    });
    let frequent = dirs
        .ranked(&exclude, now)
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    let mut d = DirsDialog::new(b"src m", home);
    d.set(&bookmarks, frequent, false);
    let mut k = 0u64;
    g.bench_function("keystroke", |b| {
        b.iter(|| {
            d.handle(toggle(&mut k, 'o'));
            d.len()
        })
    });
    g.finish();
}

/// Applies a listing's messages to `p` as the UI thread does; returns the time it took.
fn apply(p: &mut Panel, msgs: Vec<ListingMsg>) -> Duration {
    let start = Instant::now();
    for m in msgs {
        match m {
            ListingMsg::Batch {
                generation,
                entries,
                names,
                ..
            } => p.on_batch(generation, entries, &names),
            ListingMsg::Listing {
                generation,
                listing,
                ..
            } => p.on_listing(generation, *listing),
            ListingMsg::Done {
                generation, dir, ..
            } => {
                p.on_done(generation, dir);
            }
            _ => {}
        }
    }
    start.elapsed()
}

/// `Ctrl+R` in a tab of 100k results (P2 5.5), against P-1: `copy` is the UI thread's part
/// when the key arrives (the results copied into the re-stat request); `apply` is its part
/// when the re-stat completes (the fresh entries, sorted on the re-stat thread, swapped in
/// and filtered, the marks and the cursor carried over). `dir_apply` is the same completion
/// for a 100k-entry directory (M1's refresh).
fn restat(c: &mut Criterion) {
    let root = common::tree(size(100_000, 1_000));
    let mut p = results(&root);
    assert!(p.list.entries.len() >= 999);
    let mut g = c.benchmark_group("restat");
    g.sample_size(20);
    g.bench_function("copy", |b| {
        b.iter_with_large_drop(|| p.restat(Alive::running()))
    });
    // Each iteration also re-stats or re-lists 100k entries untimed: a short measurement
    // keeps the run at about a minute.
    g.sample_size(10)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(2));
    g.bench_function("apply", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let req = p.restat(Alive::running());
                let msgs = RefCell::new(Vec::new());
                find::restat(&req, &|m| msgs.borrow_mut().push(m));
                total += apply(&mut p, msgs.into_inner());
            }
            total
        })
    });
    let mut d = load(0, &list_fixture(size(100_000, 1_000)));
    g.bench_function("dir_apply", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let req = d.refresh(Alive::running());
                let msgs = RefCell::new(Vec::new());
                listing::list(&req, &|m| msgs.borrow_mut().push(m));
                total += apply(&mut d, msgs.into_inner());
            }
            total
        })
    });
    g.finish();
}

criterion_group!(group, p12, p13, p14, p15, restat);
criterion_main!(group);
