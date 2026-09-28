//! Fixtures of the phase 2 benchmarks (P2 11), shared by `benches/phase2.rs` and
//! `benches/driver.rs`. Each is created once below `<bench dir>/p2` and reused; a marker in
//! `p2/markers` records a complete one, so an interrupted creation starts again.
//! `scripts/bench/run.sh` removes `p2` after a run unless `MC_BENCH_KEEP=1`.

use std::fs::{self, File};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// `MC_BENCH_DIR`, or `target/bench`.
pub fn base() -> PathBuf {
    std::env::var_os("MC_BENCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bench"))
}

/// Where the phase 2 fixtures live.
pub fn p2() -> PathBuf {
    base().join("p2")
}

/// `p2/<name>`, made by `make` unless its marker says it is complete.
fn once(name: &str, make: impl FnOnce(&Path)) -> PathBuf {
    let path = p2().join(name);
    let marker = p2().join("markers").join(name);
    if !marker.exists() {
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&path);
        fs::create_dir_all(p2()).unwrap();
        make(&path);
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(&marker, b"").unwrap();
    }
    path
}

/// P-10, P-6b: a tree with exactly `n` entries below its root. Three levels of `f`
/// directories each (`f` = 10 from 10k entries on, else 2), the files spread over the
/// leaves; every 100th file is `report_NNNNNN.pdf`, the others `doc_NNNNNN.txt`, so the
/// pattern `report` matches 1 % of the files.
pub fn tree(n: usize) -> PathBuf {
    once(&format!("tree{n}"), |root| {
        let f = if n >= 10_000 { 10 } else { 2 };
        fs::create_dir_all(root).unwrap();
        let mut leaves = Vec::new();
        let mut count = 0;
        for a in 0..f {
            let da = root.join(format!("t{a}"));
            fs::create_dir(&da).unwrap();
            count += 1;
            for b in 0..f {
                let db = da.join(format!("m{b}"));
                fs::create_dir(&db).unwrap();
                count += 1;
                for c in 0..f {
                    let dc = db.join(format!("l{c}"));
                    fs::create_dir(&dc).unwrap();
                    count += 1;
                    leaves.push(dc);
                }
            }
        }
        for i in 0..n - count {
            let name = if i % 100 == 0 {
                format!("report_{i:06}.pdf")
            } else {
                format!("doc_{i:06}.txt")
            };
            File::create(leaves[i % leaves.len()].join(name)).unwrap();
        }
    })
}

/// The needle of the content search (P-11): no word of the text contains `-` or `zqxj`.
pub const NEEDLE: &str = "zqxj-needle-kvbw";

const WORDS: &[&str] = &[
    "the",
    "of",
    "and",
    "to",
    "in",
    "is",
    "that",
    "for",
    "it",
    "as",
    "was",
    "with",
    "be",
    "by",
    "on",
    "not",
    "he",
    "this",
    "are",
    "or",
    "his",
    "from",
    "at",
    "which",
    "but",
    "have",
    "an",
    "had",
    "they",
    "you",
    "were",
    "their",
    "one",
    "all",
    "we",
    "can",
    "her",
    "has",
    "there",
    "been",
    "if",
    "more",
    "when",
    "will",
    "would",
    "who",
    "so",
    "no",
    "file",
    "manager",
    "panel",
    "directory",
    "copy",
    "move",
    "search",
    "result",
    "window",
    "terminal",
    "keyboard",
    "system",
    "theme",
    "frame",
    "listing",
    "entry",
];

/// `len` bytes of lowercase words, spaces and newlines, from a fixed seed.
fn words(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 16);
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut on_line = 0;
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(WORDS[(x % WORDS.len() as u64) as usize].as_bytes());
        on_line += 1;
        out.push(if on_line % 12 == 0 { b'\n' } else { b' ' });
    }
    out.truncate(len);
    out
}

/// P-11: `files` text files of `bytes / files` bytes each, 100 per directory; every 100th
/// holds [`NEEDLE`] in its middle.
pub fn text_tree(files: usize, bytes: usize) -> PathBuf {
    once(&format!("text{files}"), |root| {
        let size = bytes / files;
        let pool = words((4 << 20).max(size * 4));
        let mut buf = vec![0u8; size];
        for k in 0..files {
            let dir = root.join(format!("d{:03}", k / 100));
            if k % 100 == 0 {
                fs::create_dir_all(&dir).unwrap();
            }
            let off = (k * 7919 * 13) % (pool.len() - size);
            buf.copy_from_slice(&pool[off..off + size]);
            if k % 100 == 0 {
                let at = size / 2;
                buf[at..at + NEEDLE.len()].copy_from_slice(NEEDLE.as_bytes());
            }
            fs::write(dir.join(format!("f{k:05}.txt")), &buf).unwrap();
        }
    })
}

/// 2026-01-01T00:00:00Z.
const T0: u64 = 1_767_225_600;

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

/// P-13: two directories of `n` files each. `a`: `fNNNNNN`, empty, mtimes one second
/// apart. `b`: the same names for the first 95 % (a third each with the same, a newer and
/// an older mtime; every 7th one byte long) and 5 % names of its own.
pub fn compare_pair(n: usize) -> (PathBuf, PathBuf) {
    let a = once(&format!("cmp{n}a"), |d| {
        fs::create_dir_all(d).unwrap();
        for i in 0..n {
            let f = File::create(d.join(format!("f{i:06}"))).unwrap();
            f.set_modified(at(T0 + i as u64)).unwrap();
        }
    });
    let b = once(&format!("cmp{n}b"), |d| {
        fs::create_dir_all(d).unwrap();
        for i in 0..n {
            let name = if i < n * 95 / 100 {
                format!("f{i:06}")
            } else {
                format!("g{i:06}")
            };
            let f = File::create(d.join(name)).unwrap();
            if i % 7 == 0 {
                f.set_len(1).unwrap();
            }
            let t = match i % 3 {
                0 => T0 + i as u64,
                1 => T0 + i as u64 + 10,
                _ => T0 + i as u64 - 10,
            };
            f.set_modified(at(t)).unwrap();
        }
    });
    (a, b)
}

/// P-17: `hl` with `pairs` hard-link pairs (`aN` and `bN` one inode) and `nohl` with the
/// same names as separate files, 100 pairs per directory, `bytes` bytes each.
pub fn hardlinks(pairs: usize, bytes: usize) -> (PathBuf, PathBuf) {
    let data = |k: usize| {
        let mut v = words(bytes);
        v[..8].copy_from_slice(&(k as u64).to_le_bytes());
        v
    };
    let hl = once("hl", |root| {
        for k in 0..pairs {
            let dir = root.join(format!("d{:03}", k / 100));
            if k % 100 == 0 {
                fs::create_dir_all(&dir).unwrap();
            }
            let a = dir.join(format!("a{k:05}"));
            fs::write(&a, data(k)).unwrap();
            fs::hard_link(&a, dir.join(format!("b{k:05}"))).unwrap();
        }
    });
    let nohl = once("nohl", |root| {
        for k in 0..pairs {
            let dir = root.join(format!("d{:03}", k / 100));
            if k % 100 == 0 {
                fs::create_dir_all(&dir).unwrap();
            }
            fs::write(dir.join(format!("a{k:05}")), data(k)).unwrap();
            fs::write(dir.join(format!("b{k:05}")), data(k)).unwrap();
        }
    });
    (hl, nohl)
}

/// P-16: a sparse file of `size` bytes whose data is `segments` runs of 1 MiB, spread
/// evenly; the rest are holes.
pub fn sparse(name: &str, size: u64, segments: u64) -> PathBuf {
    once(name, |p| {
        let f = File::create(p).unwrap();
        f.set_len(size).unwrap();
        let mut data = words(1 << 20);
        for s in 0..segments {
            data[..8].copy_from_slice(&s.to_le_bytes());
            f.write_all_at(&data, s * (size / segments)).unwrap();
        }
        f.sync_all().unwrap();
    })
}

/// P-15: the text of a `dirs.tsv` with `n` frecency entries (P2 3.2), visited over the
/// last 30 days before `now`.
pub fn dirs_tsv(n: usize, now: i64) -> String {
    const TOP: &[&str] = &["src", "work", "notes", "photos", "music", "Documents"];
    const SUB: &[&str] = &[
        "",
        "/src",
        "/docs",
        "/tests/data",
        "/build/out",
        "/assets/img",
    ];
    let mut s = format!("{}\n", manycommander::dirs::HEADER);
    for i in 0..n {
        let rank = 1.0 + (i * 37 % 97) as f64 / 4.0;
        let last = now - (i as i64 * 7919) % (30 * 86_400);
        s += &format!(
            "{rank}\t{last}\t/home/me/{}/project{:04}{}\n",
            TOP[i % TOP.len()],
            i,
            SUB[(i / 7) % SUB.len()]
        );
    }
    s
}
