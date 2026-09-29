//! Phase 3 fixtures (P3 7.1), made once below `p3` and reused; a marker in `p3/markers`
//! records a complete one, so an interrupted creation starts again.
//!
//! - `pkg10k`: a package-shaped tree of exactly 10,000 entries (800 directories up to six
//!   levels deep, 9,200 files whose sizes follow a log-normal distribution around 4 KiB,
//!   two thirds text and one third binary-like), 155 MB as tar; and `pkg92`: 92 entries
//!   (10 directories, 82 files around 1 MiB with a heavy tail, mostly binary-like), 345 MB
//!   as tar. The research's two shapes. Each is archived with the system tools: `bsdtar`
//!   (tar, and 7z for `pkg10k`), `gzip`, `zstd`, `xz -T0`, `bzip2` and Info-ZIP `zip`, at
//!   their default levels.
//! - `flat10k`: a `.tar.zst` whose `big/` holds 10,000 files (P-21).
//! - `idx100k`: a `.tar.zst` of the M1 100k-entry directory (P-6c).
//! - `photos`: four 12 MP JPEGs (ImageMagick's plasma fractal at quality 90, about 3 MB
//!   each, as a phone photo is) for P-23; `burst`: 200 JPEGs of 0.75 to 12 MP (reflinked
//!   copies of ten generated ones, each its own file to the preview cache) for P-24.
//! - `sftp`: a 1 GiB file of random bytes, a directory of 10,000 empty files, and small
//!   trees of 4 KiB files (P-26, P-27).
//!
//! Content is deterministic: fixed seeds, and every entry's mtime is 2026-01-01T00:00Z.

use super::p3;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

/// 2026-01-01T00:00:00Z.
const T0: &str = "@1767225600";

/// `p3/<name>`, made by `make` unless its marker says it is complete.
pub fn once(name: &str, make: impl FnOnce(&Path)) -> PathBuf {
    let path = p3().join(name);
    let marker = p3().join("markers").join(name);
    if !marker.exists() {
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&path);
        fs::create_dir_all(p3()).unwrap();
        let t = Instant::now();
        eprintln!("fixture: {}", path.display());
        make(&path);
        eprintln!(
            "fixture: {} in {:.1} s",
            path.display(),
            t.elapsed().as_secs_f64()
        );
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(&marker, b"").unwrap();
    }
    path
}

pub fn main(kind: &str) {
    match kind {
        "pkg10k" | "pkg92" => println!("{}", archives(kind).display()),
        "flat10k" => println!("{}", flat10k().display()),
        "idx100k" => println!("{}", idx100k().display()),
        "photos" => println!("{}", photos().display()),
        "burst" => println!("{}", burst().display()),
        "sftp" => println!("{}", sftp_tree().display()),
        _ => panic!("unknown phase 3 fixture {kind}"),
    }
}

/// A xorshift generator: the fixtures are the same on every run.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    pub fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A standard normal sample (Box-Muller).
    pub fn normal(&mut self) -> f64 {
        let u = self.unit().max(1e-12);
        let v = self.unit();
        (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
    }

    pub fn fill(&mut self, buf: &mut [u8]) {
        for c in buf.chunks_mut(8) {
            let x = self.next().to_le_bytes();
            c.copy_from_slice(&x[..c.len()]);
        }
    }
}

const WORDS: &[&str] = &[
    "the", "of", "and", "to", "in", "is", "that", "for", "it", "as", "with", "be", "on", "not",
    "this", "are", "or", "from", "at", "which", "return", "self", "import", "function", "const",
    "value", "error", "result", "string", "buffer", "index", "length", "config", "module", "class",
    "struct", "impl", "public", "static", "void", "int", "size", "data", "file", "path", "name",
    "type", "list", "map", "key", "none", "true", "false", "if", "else", "while", "let", "match",
    "case", "break", "include", "define", "package", "version", "license", "author",
];

/// `len` bytes of words, spaces and newlines.
fn text_pool(len: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng::new(seed);
    let mut out = Vec::with_capacity(len + 16);
    let mut on_line = 0;
    while out.len() < len {
        out.extend_from_slice(WORDS[r.below(WORDS.len() as u64) as usize].as_bytes());
        on_line += 1;
        let b = match r.below(20) {
            0 => b'\n',
            1 => b'(',
            2 => b';',
            _ if on_line % 11 == 0 => b'\n',
            _ => b' ',
        };
        out.push(b);
    }
    out.truncate(len);
    out
}

/// Binary-like bytes: 4 KiB blocks of random bytes, of a counter table with noise, of
/// zeros, or of text, in the shares a shared library or a media bundle has.
fn binary(buf: &mut [u8], r: &mut Rng, random_share: f64, text: &[u8]) {
    for block in buf.chunks_mut(4096) {
        let pick = r.unit();
        if pick < random_share {
            r.fill(block);
        } else if pick < random_share + (1.0 - random_share) * 0.5 {
            let mut v = r.next() as u32;
            for c in block.chunks_mut(4) {
                v = v.wrapping_add(1 + (r.below(4) as u32));
                let x = v.to_le_bytes();
                c.copy_from_slice(&x[..c.len()]);
            }
        } else if pick < random_share + (1.0 - random_share) * 0.7 {
            block.fill(0);
        } else {
            let off = r.below((text.len() - block.len()) as u64) as usize;
            block.copy_from_slice(&text[off..off + block.len()]);
        }
    }
}

struct Shape {
    entries: usize,
    dirs: usize,
    /// The tar's size the sizes are scaled to.
    tar_bytes: u64,
    median: f64,
    sigma: f64,
    max_file: u64,
    /// The share of files with text content.
    text: f64,
    /// The share of random 4 KiB blocks in binary-like files.
    random: f64,
    seed: u64,
}

const DIR_NAMES: &[&str] = &[
    "lib", "include", "share", "doc", "src", "bin", "core", "util", "net", "io", "fmt", "tests",
    "data", "assets", "locale", "icons", "plugins", "vendor", "internal", "api",
];
const TEXT_EXT: &[&str] = &[
    ".py", ".js", ".h", ".c", ".txt", ".json", ".md", ".html", ".css", ".xml", ".rs", ".toml",
];
const BIN_EXT: &[&str] = &[
    ".so", ".png", ".bin", ".pyc", ".o", ".dat", ".woff2", ".jpg",
];

/// A package-shaped tree of exactly `s.entries` entries below `root/<name>`.
fn package(root: &Path, name: &str, s: &Shape) {
    let mut r = Rng::new(s.seed);
    let top = root.join(name);
    fs::create_dir_all(&top).unwrap();
    // Directories: each below a random earlier one of depth < 6.
    let mut dirs: Vec<(PathBuf, usize)> = vec![(top.clone(), 0)];
    while dirs.len() < s.dirs {
        let (parent, depth) = loop {
            let k = r.below(dirs.len() as u64) as usize;
            if dirs[k].1 < 6 {
                break dirs[k].clone();
            }
        };
        let base = DIR_NAMES[r.below(DIR_NAMES.len() as u64) as usize];
        let d = parent.join(format!("{base}{}", dirs.len()));
        fs::create_dir(&d).unwrap();
        dirs.push((d, depth + 1));
    }
    let files = s.entries - s.dirs;
    // Sizes: log-normal, clamped, then scaled so the tar has `tar_bytes`.
    let mut sizes: Vec<f64> = (0..files)
        .map(|_| (s.median * (s.sigma * r.normal()).exp()).clamp(0.0, s.max_file as f64))
        .collect();
    let overhead = (s.entries as u64) * 512 + files as u64 * 256 + 10240;
    let want = s.tar_bytes.saturating_sub(overhead) as f64;
    let have: f64 = sizes.iter().sum();
    for x in &mut sizes {
        *x = (*x * want / have).min(s.max_file as f64);
    }
    let text = text_pool(8 << 20, s.seed ^ 0x5555);
    let mut buf = Vec::new();
    for (i, size) in sizes.iter().enumerate() {
        let size = *size as usize;
        let (dir, _) = &dirs[r.below(dirs.len() as u64) as usize];
        let is_text = r.unit() < s.text;
        let ext = if is_text {
            TEXT_EXT[r.below(TEXT_EXT.len() as u64) as usize]
        } else {
            BIN_EXT[r.below(BIN_EXT.len() as u64) as usize]
        };
        let word = WORDS[r.below(WORDS.len() as u64) as usize];
        let path = dir.join(format!("{word}_{i:05}{ext}"));
        buf.resize(size, 0);
        if is_text {
            let mut at = 0;
            while at < size {
                let n = (size - at).min(text.len() / 2);
                let off = r.below((text.len() - n) as u64) as usize;
                buf[at..at + n].copy_from_slice(&text[off..off + n]);
                at += n;
            }
        } else {
            binary(&mut buf, &mut r, s.random, &text);
        }
        fs::write(&path, &buf).unwrap();
        if !is_text && r.below(4) == 0 {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    stamp(&top);
}

/// Every entry below `dir`, and `dir` itself, gets the mtime [`T0`].
fn stamp(dir: &Path) {
    sh(Command::new("find")
        .arg(dir)
        .args(["-exec", "touch", "-h", "-d", T0, "{}", "+"]));
}

fn sh(cmd: &mut Command) {
    let st = cmd
        .stdin(Stdio::null())
        .status()
        .unwrap_or_else(|e| panic!("{cmd:?}: {e}"));
    assert!(st.success(), "{cmd:?}: {st}");
}

/// `tool args < input > output`.
fn pipe_to(tool: &str, args: &[&str], input: &Path, output: &Path) {
    let st = Command::new(tool)
        .args(args)
        .stdin(fs::File::open(input).unwrap())
        .stdout(fs::File::create(output).unwrap())
        .status()
        .unwrap_or_else(|e| panic!("{tool}: {e}"));
    assert!(st.success(), "{tool} {args:?}: {st}");
}

/// The archives of `pkg10k` or `pkg92` in `p3/<kind>/`: `<kind>.tar` and its `.gz`,
/// `.zst`, `.xz` and `.bz2`, `<kind>.zip`, and for `pkg10k` a `.7z`.
pub fn archives(kind: &str) -> PathBuf {
    once(kind, |dir| {
        let shape = if kind == "pkg10k" {
            Shape {
                entries: 10_000,
                dirs: 800,
                tar_bytes: 155_000_000,
                median: 4096.0,
                sigma: 1.6,
                max_file: 8 << 20,
                text: 0.65,
                random: 0.4,
                seed: 10_000,
            }
        } else {
            Shape {
                entries: 92,
                dirs: 10,
                tar_bytes: 345_000_000,
                median: 1024.0 * 1024.0,
                sigma: 1.5,
                max_file: 96 << 20,
                text: 0.2,
                random: 0.6,
                seed: 92,
            }
        };
        let tree = dir.join("tree");
        package(&tree, kind, &shape);
        let tar = dir.join(format!("{kind}.tar"));
        sh(Command::new("bsdtar")
            .arg("-cf")
            .arg(&tar)
            .arg("-C")
            .arg(&tree)
            .arg(kind));
        let with = |ext: &str| dir.join(format!("{kind}.tar.{ext}"));
        pipe_to("gzip", &["-c"], &tar, &with("gz"));
        pipe_to("zstd", &["-q", "-c"], &tar, &with("zst"));
        pipe_to("xz", &["-T0", "-c"], &tar, &with("xz"));
        pipe_to("bzip2", &["-c"], &tar, &with("bz2"));
        sh(Command::new("zip")
            .args(["-r", "-q"])
            .arg(dir.join(format!("{kind}.zip")))
            .arg(kind)
            .current_dir(&tree));
        if kind == "pkg10k" {
            sh(Command::new("bsdtar")
                .args(["--format", "7zip", "-cf"])
                .arg(dir.join(format!("{kind}.7z")))
                .arg("-C")
                .arg(&tree)
                .arg(kind));
        }
    })
}

/// P-21: `flat10k.tar.zst` whose `flat/big/` holds 10,000 empty files, beside `flat/small/`
/// with ten files and `flat/README`.
pub fn flat10k() -> PathBuf {
    let dir = once("flat10k", |dir| {
        let tree = dir.join("tree/flat");
        fs::create_dir_all(tree.join("big")).unwrap();
        fs::create_dir_all(tree.join("small")).unwrap();
        for i in 0..10_000 {
            fs::File::create(tree.join(format!("big/f{i:05}"))).unwrap();
        }
        for i in 0..10 {
            fs::write(tree.join(format!("small/s{i}")), b"small\n").unwrap();
        }
        fs::write(tree.join("README"), b"flat\n").unwrap();
        stamp(&tree);
        let tar = dir.join("flat10k.tar");
        sh(Command::new("bsdtar")
            .arg("-cf")
            .arg(&tar)
            .arg("-C")
            .arg(dir.join("tree"))
            .arg("flat"));
        pipe_to("zstd", &["-q", "-c"], &tar, &dir.join("flat10k.tar.zst"));
        fs::remove_file(&tar).unwrap();
        fs::remove_dir_all(dir.join("tree")).unwrap();
    });
    dir.join("flat10k.tar.zst")
}

/// P-6c: `idx100k.tar.zst`, the M1 fixture `src/many` (100,000 empty files) archived.
pub fn idx100k() -> PathBuf {
    let dir = once("idx100k", |dir| {
        fs::create_dir_all(dir).unwrap();
        let src = crate::common::base().join("src");
        assert!(
            src.join("many").is_dir(),
            "the M1 fixture src/many is missing"
        );
        let tar = dir.join("idx100k.tar");
        sh(Command::new("bsdtar")
            .arg("-cf")
            .arg(&tar)
            .arg("-C")
            .arg(&src)
            .arg("many"));
        pipe_to("zstd", &["-q", "-c"], &tar, &dir.join("idx100k.tar.zst"));
        fs::remove_file(&tar).unwrap();
    });
    dir.join("idx100k.tar.zst")
}

fn plasma(out: &Path, w: u32, h: u32, seed: u32) {
    sh(Command::new("magick")
        .arg("-seed")
        .arg(seed.to_string())
        .arg("-size")
        .arg(format!("{w}x{h}"))
        .arg("plasma:fractal")
        .args(["-quality", "90"])
        .arg(out));
}

/// P-23: `photos/photo1.jpg` .. `photo4.jpg`, 4000 x 3000, and `photos/a.txt`.
pub fn photos() -> PathBuf {
    once("photos", |dir| {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("a.txt"), b"a text file\n").unwrap();
        for k in 1..=4 {
            plasma(&dir.join(format!("photo{k}.jpg")), 4000, 3000, k);
        }
    })
}

/// P-24: `burst/img000.jpg` .. `img199.jpg`.
pub fn burst() -> PathBuf {
    once("burst", |dir| {
        let base = dir.join("base");
        fs::create_dir_all(&base).unwrap();
        let sizes = [
            (1000, 750),
            (1600, 1200),
            (1920, 1080),
            (2000, 1500),
            (2400, 1800),
            (3000, 2000),
            (3264, 2448),
            (4000, 3000),
            (4000, 3000),
            (3000, 4000),
        ];
        for (k, (w, h)) in sizes.iter().enumerate() {
            plasma(&base.join(format!("b{k}.jpg")), *w, *h, 100 + k as u32);
        }
        let imgs = dir.join("imgs");
        fs::create_dir_all(&imgs).unwrap();
        for i in 0..200 {
            sh(Command::new("cp")
                .arg("--reflink=auto")
                .arg(base.join(format!("b{}.jpg", i % sizes.len())))
                .arg(imgs.join(format!("img{i:03}.jpg"))));
        }
    })
}

/// P-26, P-27: `sftp/remote/{big1g, list10k/, small1k/, small200/}`, and empty
/// `sftp/local/`.
pub fn sftp_tree() -> PathBuf {
    once("sftp", |dir| {
        let remote = dir.join("remote");
        fs::create_dir_all(remote.join("list10k")).unwrap();
        fs::create_dir_all(dir.join("local")).unwrap();
        let mut r = Rng::new(26);
        let mut f = fs::File::create(remote.join("big1g")).unwrap();
        let mut buf = vec![0u8; 8 << 20];
        for _ in 0..128 {
            r.fill(&mut buf);
            f.write_all(&buf).unwrap();
        }
        drop(f);
        for i in 0..10_000 {
            fs::File::create(remote.join(format!("list10k/f{i:05}"))).unwrap();
        }
        for (name, n) in [("small1k", 1000), ("small200", 200)] {
            let mut data = vec![0u8; 4096];
            for i in 0..n {
                let d = remote.join(name).join(format!("d{:02}", i / 100));
                fs::create_dir_all(&d).unwrap();
                r.fill(&mut data);
                fs::write(d.join(format!("f{i:04}")), &data).unwrap();
            }
        }
        stamp(&remote);
    })
}
