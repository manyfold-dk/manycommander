//! P-3 and P-4 (A-P-3, A-P-4): listing and sorting 100k entries, time to the first batch,
//! re-sort and filter. `scripts/bench/run.sh` parses criterion's estimates against the
//! targets. Run by `cargo test --all-targets` too, then on a small directory.

use criterion::{Criterion, criterion_group, criterion_main};
use manycommander::panel::Panel;
use manycommander::panel::listing::{self, Alive, ListRequest, ListingMsg};
use manycommander::panel::sort::SortKey;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Benchmarks run with `--bench`; `cargo test` runs them once, on a small fixture.
fn full_run() -> bool {
    std::env::args().any(|a| a == "--bench")
}

fn fixture() -> PathBuf {
    let n = if full_run() { 100_000 } else { 1_000 };
    let base = std::env::var_os("MC_BENCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bench"));
    let dir = base.join(format!("list{n}"));
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

fn load(dir: &Path) -> Panel {
    let mut p = Panel::new(0, dir.to_path_buf());
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

fn benches(c: &mut Criterion) {
    let dir = fixture();
    let mut g = c.benchmark_group("p3");
    g.sample_size(10).measurement_time(Duration::from_secs(10));
    g.bench_function("list_and_sort", |b| {
        b.iter(|| {
            let p = load(&dir);
            assert!(p.list.entries.len() >= 1_000);
            p
        })
    });
    g.bench_function("first_batch", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let req = ListRequest {
                    slot: 0,
                    generation: 1,
                    dir: dir.clone(),
                    ancestor_fallback: false,
                    sort: None,
                };
                let start = Instant::now();
                let first = Cell::new(None);
                listing::list(&req, &|m| {
                    if first.get().is_none() && matches!(m, ListingMsg::Batch { .. }) {
                        first.set(Some(start.elapsed()));
                    }
                });
                total += first.get().unwrap();
            }
            total
        })
    });
    g.finish();

    let mut g = c.benchmark_group("p4");
    g.sample_size(20);
    let mut p = load(&dir);
    g.bench_function("resort", |b| {
        let mut k = 0;
        b.iter(|| {
            k += 1;
            p.set_sort(if k % 2 == 0 {
                SortKey::Name
            } else {
                SortKey::Mtime
            });
        })
    });
    g.bench_function("filter", |b| b.iter(|| p.toggle_hidden()));
    g.finish();
}

criterion_group!(group, benches);
criterion_main!(group);
