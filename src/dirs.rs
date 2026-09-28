#![forbid(unsafe_code)]
//! The directory hotlist (bookmarks) and the frecency jump (P2 3).
//!
//! - [`Hotlist`]: bookmarks in the order the user added them, in
//!   `$XDG_CONFIG_HOME/manycommander/hotlist.toml`. The small file loads on the boot thread
//!   and is written atomically on the directory-store thread after every change. A file
//!   that does not parse is reported once and never overwritten (P2 3.2).
//! - [`Store`]: frecency entries in `$XDG_STATE_HOME/manycommander/dirs.tsv`, following
//!   zoxide's model (P2 3.3). It loads on the directory-store thread after the first frame
//!   (P-2). A session records its visits as [`Deltas`]; on exit [`save_merged`] applies
//!   them to the file under an exclusive `flock` on `dirs.tsv.lock`, so two instances that
//!   exit together keep each other's visits.
//! - [`Matcher`]: zoxide's keyword rules, without allocation per candidate (P-15).
//! - zoxide: [`zoxide_query`] runs `zoxide query --list --score` by argv with a timeout
//!   (NFR-SEC). manycommander never writes to zoxide's database.
//! - [`Dirs`]: the UI thread's view of all of it. It makes no syscalls; the runtime
//!   performs the file work on the [`StoreThread`].

use crate::app::state::Bytes;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// zoxide's `_ZO_MAXAGE` default: the sum of ranks above which the store ages (P2 3.3).
pub const MAX_AGE: f64 = 10_000.0;
/// How long a session waits for `dirs.tsv.lock` on exit (P2 3.3).
pub const LOCK_WAIT: Duration = Duration::from_secs(2);
/// How long `zoxide query` may run (P2 3.3).
pub const ZOXIDE_TIMEOUT: Duration = Duration::from_secs(1);
/// The first line of `dirs.tsv` (P2 3.2).
pub const HEADER: &str = "# manycommander dirs v1";
/// Why adding or removing a bookmark fails while `hotlist.toml` does not parse (P2 3.2).
pub const HOTLIST_BROKEN: &str = "hotlist.toml does not parse; fix or remove it";
/// Why it fails when the boot thread did not read `hotlist.toml` in time.
pub const HOTLIST_UNREAD: &str =
    "hotlist.toml was not read at startup; restart to change bookmarks";

const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;

/// Seconds since the epoch, for visits and scores.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `$XDG_CONFIG_HOME/manycommander/hotlist.toml`, or under `$HOME/.config`.
pub fn hotlist_path(xdg_config: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    Some(crate::config::Config::path(xdg_config, home)?.with_file_name("hotlist.toml"))
}

/// `$XDG_STATE_HOME/manycommander/dirs.tsv`, or under `$HOME/.local/state`.
pub fn store_path(xdg_state: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    Some(crate::app::state::path(xdg_state, home)?.with_file_name("dirs.tsv"))
}

/// The lock beside the store: a separate file, because the atomic rename replaces
/// `dirs.tsv` itself (P2 3.3).
pub fn lock_path(store: &Path) -> PathBuf {
    let mut s = store.as_os_str().to_owned();
    s.push(".lock");
    PathBuf::from(s)
}

fn path_of(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(OsString::from_vec(bytes))
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Writes `data` to `path` atomically: a temporary file in the same directory, fsync,
/// rename over `path`, fsync the directory. A failure leaves `path` as it was.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut name = OsString::from(".");
    name.push(path.file_name().unwrap_or(OsStr::new("file")));
    name.push(format!(
        ".{}.{}",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = dir.join(name);
    let written = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written?;
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

// ---- hotlist ------------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct HotlistFile {
    #[serde(default)]
    dir: Vec<HotlistDir>,
}

#[derive(Serialize, Deserialize)]
struct HotlistDir {
    path: Bytes,
}

/// The bookmarks (P2 3.1, 3.2), in the order the user added them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hotlist {
    pub dirs: Vec<PathBuf>,
    /// Set when the file could not be used: adding and removing then fail with this text,
    /// and the file is never written.
    pub refusal: Option<String>,
}

impl Hotlist {
    /// Parses `hotlist.toml`: `[[dir]] path = ...`, a path being a TOML string or a byte
    /// array (as in `state.toml`). A relative path makes the file not parse.
    pub fn parse(text: &str) -> Result<Vec<PathBuf>, String> {
        let f: HotlistFile = toml::from_str(text).map_err(|e| e.to_string())?;
        f.dir
            .into_iter()
            .enumerate()
            .map(|(i, d)| match d.path.bytes() {
                b if b.first() == Some(&b'/') => Ok(path_of(b.to_vec())),
                _ => Err(format!("dir {}: the path is not absolute", i + 1)),
            })
            .collect()
    }

    pub fn to_toml(dirs: &[PathBuf]) -> String {
        let f = HotlistFile {
            dir: dirs
                .iter()
                .map(|d| HotlistDir {
                    path: Bytes::of(d.as_os_str().as_bytes()),
                })
                .collect(),
        };
        format!(
            "# manycommander bookmarks (Ctrl+D). \
             A file that does not parse is never overwritten.\n{}",
            toml::to_string(&f).unwrap_or_default()
        )
    }

    /// Reads the file. A missing file is no bookmarks. A file that cannot be read or does
    /// not parse gives no bookmarks, a refusal, and the text to report once.
    pub fn load(path: &Path) -> (Hotlist, Option<String>) {
        let parsed = match std::fs::read(path) {
            Ok(b) => String::from_utf8(b)
                .map_err(|e| e.to_string())
                .and_then(|t| Hotlist::parse(&t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.to_string()),
        };
        match parsed {
            Ok(dirs) => (
                Hotlist {
                    dirs,
                    refusal: None,
                },
                None,
            ),
            Err(e) => (
                Hotlist {
                    dirs: Vec::new(),
                    refusal: Some(HOTLIST_BROKEN.into()),
                },
                Some(format!("{}: {e}", path.display())),
            ),
        }
    }

    /// The bookmarks when the boot thread did not answer: none, and read-only, because the
    /// file on disk may hold bookmarks that a write would lose.
    pub fn unread() -> Hotlist {
        Hotlist {
            dirs: Vec::new(),
            refusal: Some(HOTLIST_UNREAD.into()),
        }
    }

    /// Writes the file atomically (P2 3.2).
    pub fn save(dirs: &[PathBuf], path: &Path) -> std::io::Result<()> {
        write_atomic(path, Hotlist::to_toml(dirs).as_bytes())
    }

    /// Adds `dir` at the end. `Ok(false)`: it already is a bookmark.
    pub fn add(&mut self, dir: &Path) -> Result<bool, String> {
        if let Some(r) = &self.refusal {
            return Err(r.clone());
        }
        if self.dirs.iter().any(|d| d == dir) {
            return Ok(false);
        }
        self.dirs.push(dir.to_path_buf());
        Ok(true)
    }

    /// Removes `dir`. `Ok(false)`: it was not a bookmark.
    pub fn remove(&mut self, dir: &Path) -> Result<bool, String> {
        if let Some(r) = &self.refusal {
            return Err(r.clone());
        }
        let n = self.dirs.len();
        self.dirs.retain(|d| d != dir);
        Ok(self.dirs.len() != n)
    }
}

// ---- frecency store -----------------------------------------------------------------------

/// One directory's frecency: its rank and last visit (epoch seconds).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frecency {
    pub rank: f64,
    pub last: i64,
}

/// Score = `rank x w(age)`: 4 within an hour, 2 within a day, 0.5 within a week, 0.25
/// otherwise (P2 3.3, as zoxide).
pub fn score(f: Frecency, now: i64) -> f64 {
    let age = now.saturating_sub(f.last);
    let w = if age < HOUR {
        4.0
    } else if age < DAY {
        2.0
    } else if age < WEEK {
        0.5
    } else {
        0.25
    };
    f.rank * w
}

/// The frecency entries of `dirs.tsv`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Store {
    pub entries: HashMap<PathBuf, Frecency>,
}

/// Escapes a path for `dirs.tsv`: `\`, bytes below 0x20, 0x7f and invalid UTF-8 become
/// `\xNN` (P2 3.2), so a line holds exactly three tab-separated fields.
pub fn escape_path(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len());
    for chunk in b.utf8_chunks() {
        for c in chunk.valid().chars() {
            if c == '\\' || (c as u32) < 0x20 || c == '\x7f' {
                out.push_str(&format!("\\x{:02x}", c as u32));
            } else {
                out.push(c);
            }
        }
        for x in chunk.invalid() {
            out.push_str(&format!("\\x{x:02x}"));
        }
    }
    out
}

/// Reverses [`escape_path`]; `None` for a `\` that does not start `\xNN`.
pub fn unescape_path(s: &[u8]) -> Option<Vec<u8>> {
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'\\' {
            if s.get(i + 1) != Some(&b'x') {
                return None;
            }
            let hi = hex(*s.get(i + 2)?)?;
            let lo = hex(*s.get(i + 3)?)?;
            out.push(hi << 4 | lo);
            i += 4;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    Some(out)
}

fn parse_line(line: &[u8]) -> Option<(PathBuf, Frecency)> {
    let mut it = line.splitn(3, |&b| b == b'\t');
    let rank: f64 = std::str::from_utf8(it.next()?).ok()?.parse().ok()?;
    let last: i64 = std::str::from_utf8(it.next()?).ok()?.parse().ok()?;
    let path = unescape_path(it.next()?)?;
    (rank.is_finite() && rank > 0.0 && path.first() == Some(&b'/'))
        .then(|| (path_of(path), Frecency { rank, last }))
}

impl Store {
    /// Parses `dirs.tsv`: `rank<TAB>last<TAB>path` per line. Comment lines (the header) and
    /// lines that do not parse are skipped; a path listed twice adds up.
    pub fn parse(data: &[u8]) -> Store {
        let mut s = Store::default();
        for line in data.split(|&b| b == b'\n') {
            if line.is_empty() || line[0] == b'#' {
                continue;
            }
            if let Some((p, f)) = parse_line(line) {
                s.add(p, f);
            }
        }
        s
    }

    fn add(&mut self, p: PathBuf, f: Frecency) {
        let e = self.entries.entry(p).or_insert(Frecency {
            rank: 0.0,
            last: f.last,
        });
        e.rank += f.rank;
        e.last = e.last.max(f.last);
    }

    /// The file's text: the header, then the entries by rank, best first. Ranks are written
    /// as shortest round-trip decimals.
    pub fn to_tsv(&self) -> Vec<u8> {
        let mut v: Vec<(&PathBuf, &Frecency)> = self.entries.iter().collect();
        v.sort_by(|a, b| {
            b.1.rank
                .total_cmp(&a.1.rank)
                .then_with(|| a.0.as_os_str().as_bytes().cmp(b.0.as_os_str().as_bytes()))
        });
        let mut out = format!("{HEADER}\n");
        for (p, f) in v {
            out.push_str(&format!(
                "{}\t{}\t{}\n",
                f.rank,
                f.last,
                escape_path(p.as_os_str().as_bytes())
            ));
        }
        out.into_bytes()
    }

    /// Reads `dirs.tsv`; a missing file is an empty store.
    pub fn load(path: &Path) -> std::io::Result<Store> {
        match std::fs::read(path) {
            Ok(b) => Ok(Store::parse(&b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Store::default()),
            Err(e) => Err(e),
        }
    }

    /// Applies a session's deltas: a forgotten directory is removed first, then visits add
    /// their ranks and the later `last` wins.
    pub fn apply(&mut self, deltas: &Deltas) {
        for (p, d) in &deltas.map {
            if d.forget {
                self.entries.remove(p);
            }
            if d.rank > 0.0 {
                self.add(
                    p.clone(),
                    Frecency {
                        rank: d.rank,
                        last: d.last,
                    },
                );
            }
        }
    }

    /// Ages as zoxide does: when the sum of ranks exceeds [`MAX_AGE`], every rank is
    /// multiplied once by `0.9 x MAX_AGE / sum` and entries below 1 are dropped.
    pub fn age(&mut self) {
        let sum: f64 = self.entries.values().map(|f| f.rank).sum();
        if sum > MAX_AGE {
            let factor = 0.9 * MAX_AGE / sum;
            for f in self.entries.values_mut() {
                f.rank *= factor;
            }
            self.entries.retain(|_, f| f.rank >= 1.0);
        }
    }
}

/// A session's change to one directory's entry.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Delta {
    /// The entry was dropped (the directory is gone, or the user forgot it); visits after
    /// that count again.
    pub forget: bool,
    /// Visits since the session started (or since the forget).
    pub rank: f64,
    pub last: i64,
}

/// What a session changes in `dirs.tsv`; applied under the lock on exit (P2 3.3).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Deltas {
    pub map: HashMap<PathBuf, Delta>,
}

impl Deltas {
    pub fn visit(&mut self, dir: &Path, now: i64) {
        let d = self.map.entry(dir.to_path_buf()).or_default();
        d.rank += 1.0;
        d.last = d.last.max(now);
    }

    pub fn forget(&mut self, dir: &Path) {
        self.map.insert(
            dir.to_path_buf(),
            Delta {
                forget: true,
                rank: 0.0,
                last: 0,
            },
        );
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Applies `deltas` to `dirs.tsv` (P2 3.3): an exclusive `flock` on `dirs.tsv.lock`,
/// waited for at most `wait`; re-read the file; apply; age; write a temporary file, fsync,
/// rename, fsync the directory; release. `Err` says why nothing was saved: the lock stayed
/// taken, or the file could not be read (a file that cannot be read is never replaced).
pub fn save_merged(path: &Path, deltas: &Deltas, wait: Duration) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let lp = lock_path(path);
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lp)
        .map_err(|e| format!("{}: {e}", lp.display()))?;
    let start = Instant::now();
    loop {
        match rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => break,
            Err(e) if e == rustix::io::Errno::WOULDBLOCK || e == rustix::io::Errno::INTR => {
                if start.elapsed() >= wait {
                    return Err(format!(
                        "{} stayed locked for {:.1} s (another manycommander is saving); this \
                         session's directory visits were not saved",
                        lp.display(),
                        wait.as_secs_f64()
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(format!("{}: {e}", lp.display())),
        }
    }
    let mut store = Store::load(path).map_err(|e| format!("{}: {e}", path.display()))?;
    store.apply(deltas);
    store.age();
    write_atomic(path, &store.to_tsv())
        .map_err(|e| format!("could not save {}: {e}", path.display()))?;
    // Closing the lock file releases the lock.
    drop(lock);
    Ok(())
}

// ---- matching -----------------------------------------------------------------------------

/// The keyword rules (P2 3.3, as zoxide): the filter splits on whitespace into keywords,
/// compared ASCII case-insensitively; every keyword occurs in the path in order, and the last
/// one occurs in the last path component. An empty filter matches everything. [`set`]
/// reuses the matcher's buffers, and [`matches`] allocates nothing (P-15).
///
/// [`set`]: Matcher::set
/// [`matches`]: Matcher::matches
#[derive(Clone, Debug, Default)]
pub struct Matcher {
    text: Vec<u8>,
    words: Vec<(usize, usize)>,
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len())
        .rev()
        .find(|&i| &hay[i..i + needle.len()] == needle)
}

impl Matcher {
    pub fn new(filter: &[u8]) -> Matcher {
        let mut m = Matcher::default();
        m.set(filter);
        m
    }

    pub fn set(&mut self, filter: &[u8]) {
        self.text.clear();
        self.text
            .extend(filter.iter().map(|c| c.to_ascii_lowercase()));
        self.words.clear();
        let mut i = 0;
        while i < self.text.len() {
            while i < self.text.len() && self.text[i].is_ascii_whitespace() {
                i += 1;
            }
            let s = i;
            while i < self.text.len() && !self.text[i].is_ascii_whitespace() {
                i += 1;
            }
            if i > s {
                self.words.push((s, i));
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Whether `lower`, a path ASCII-lowercased, matches.
    pub fn matches(&self, lower: &[u8]) -> bool {
        let Some((&(s, e), rest)) = self.words.split_last() else {
            return true;
        };
        let last = &self.text[s..e];
        let Some(i) = rfind(lower, last) else {
            return false;
        };
        if lower[i + last.len()..].contains(&b'/') {
            return false;
        }
        let mut hay = &lower[..i];
        for &(s, e) in rest.iter().rev() {
            match rfind(hay, &self.text[s..e]) {
                Some(j) => hay = &hay[..j],
                None => return false,
            }
        }
        true
    }
}

/// `p`'s bytes, ASCII-lowercased, for [`Matcher::matches`].
pub fn lowered(p: &Path) -> Vec<u8> {
    p.as_os_str().as_bytes().to_ascii_lowercase()
}

// ---- zoxide -------------------------------------------------------------------------------

/// Parses `zoxide query --list --score`: `<score> <path>` per line, the score right-aligned.
/// Lines that do not parse, and relative paths, are skipped.
pub fn parse_zoxide(out: &[u8]) -> Vec<(PathBuf, f64)> {
    out.split(|&b| b == b'\n')
        .filter_map(|line| {
            let start = line.iter().position(|&b| b != b' ')?;
            let line = &line[start..];
            let sp = line.iter().position(|&b| b == b' ')?;
            let score: f64 = std::str::from_utf8(&line[..sp]).ok()?.parse().ok()?;
            let path = &line[sp + 1..];
            (score.is_finite() && score >= 0.0 && path.first() == Some(&b'/'))
                .then(|| (path_of(path.to_vec()), score))
        })
        .collect()
}

/// The first executable `name` in the absolute directories of `search` (a `PATH` value).
pub fn find_program(name: &str, search: Option<&OsStr>) -> Option<PathBuf> {
    search?
        .as_bytes()
        .split(|&b| b == b':')
        .filter(|d| d.first() == Some(&b'/'))
        .map(|d| Path::new(OsStr::from_bytes(d)).join(name))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// Runs `argv query --list --score` (argv, never a shell; NFR-SEC) with its working
/// directory at `/` (zoxide leaves out the working directory) and returns the parsed
/// ranking. A child that has not answered within `timeout` is killed.
pub fn zoxide_query(argv: &[OsString], timeout: Duration) -> Result<Vec<(PathBuf, f64)>, String> {
    let (prog, pre) = argv.split_first().ok_or("zoxide: no program")?;
    let start = Instant::now();
    let mut child = Command::new(prog)
        .args(pre)
        .args(["query", "--list", "--score"])
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("zoxide: {e}"))?;
    let mut out = child.stdout.take().ok_or("zoxide: no output pipe")?;
    let (tx, rx) = channel();
    // A reader thread, so the timeout also covers a child that never closes its output.
    let reader = std::thread::Builder::new()
        .name("list-zoxide".into())
        .spawn(move || {
            let mut buf = Vec::new();
            let r = out.read_to_end(&mut buf).map(|_| buf);
            let _ = tx.send(r);
        });
    if let Err(e) = reader {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("zoxide: {e}"));
    }
    let answer = rx.recv_timeout(timeout.saturating_sub(start.elapsed()));
    // The child closed its output or ran out of time; it may use the rest to exit.
    let exited = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(5)),
            _ => break false,
        }
    };
    if !exited {
        let _ = child.kill();
        let _ = child.wait();
    }
    match answer {
        Ok(Ok(buf)) => Ok(parse_zoxide(&buf)),
        Ok(Err(e)) => Err(format!("zoxide: {e}")),
        Err(_) => Err(format!(
            "zoxide gave no answer within {:.1} s",
            timeout.as_secs_f64()
        )),
    }
}

// ---- the UI thread's view -----------------------------------------------------------------

/// zoxide's ranking, as far as this session has it (P2 3.3).
#[derive(Clone, Debug, PartialEq)]
pub enum Zoxide {
    /// `jump.zoxide = "off"`.
    Off,
    /// Not asked yet: the first `Ctrl+D` (or `z`) asks.
    Idle,
    /// The directory-store thread runs the query.
    Asked,
    Loaded(Vec<(PathBuf, f64)>),
}

/// Bookmarks, frecency and zoxide, as the UI thread holds them. Nothing here makes a
/// syscall: the store thread reads and writes the files.
#[derive(Clone, Debug)]
pub struct Dirs {
    pub hotlist: Hotlist,
    /// `dirs.tsv` with this session's deltas applied; `None` until it is loaded.
    pub store: Option<Store>,
    /// The load was asked for.
    pub requested: bool,
    /// This session's changes, merged into the file on exit.
    pub deltas: Deltas,
    pub zoxide: Zoxide,
}

impl Dirs {
    pub fn new(zoxide: bool) -> Dirs {
        Dirs {
            hotlist: Hotlist::default(),
            store: None,
            requested: false,
            deltas: Deltas::default(),
            zoxide: if zoxide { Zoxide::Idle } else { Zoxide::Off },
        }
    }

    /// A completed navigation (P2 3.3): `rank += 1`, `last = now`.
    pub fn visit(&mut self, dir: &Path, now: i64) {
        self.deltas.visit(dir, now);
        if let Some(s) = self.store.as_mut() {
            s.add(
                dir.to_path_buf(),
                Frecency {
                    rank: 1.0,
                    last: now,
                },
            );
        }
    }

    /// Drops `dir`'s frecency entry (a gone directory, or `Delete` in the dialog). zoxide's
    /// entry is hidden for this session; zoxide's database is never changed.
    pub fn forget(&mut self, dir: &Path) {
        self.deltas.forget(dir);
        if let Some(s) = self.store.as_mut() {
            s.entries.remove(dir);
        }
        if let Zoxide::Loaded(v) = &mut self.zoxide {
            v.retain(|(p, _)| p != dir);
        }
    }

    /// `dirs.tsv` arrived; the visits made meanwhile are applied to it.
    pub fn loaded(&mut self, mut store: Store) {
        store.apply(&self.deltas);
        self.store = Some(store);
    }

    /// zoxide's ranking arrived; directories this session forgot stay hidden.
    pub fn zoxide_loaded(&mut self, mut list: Vec<(PathBuf, f64)>) {
        if self.zoxide == Zoxide::Off {
            return;
        }
        list.retain(|(p, _)| !self.deltas.map.get(p).is_some_and(|d| d.forget));
        self.zoxide = Zoxide::Loaded(list);
    }

    /// Whether a load the dialog or `z` waits for is still out.
    pub fn pending(&self) -> bool {
        self.store.is_none() || self.zoxide == Zoxide::Asked
    }

    /// The frequent directories, best first: this store's score and zoxide's, whichever is
    /// larger; `exclude` (the active panel's directory) left out. Ties go by path.
    pub fn ranked(&self, exclude: &Path, now: i64) -> Vec<(PathBuf, f64)> {
        let mut m: HashMap<&Path, f64> = HashMap::new();
        if let Some(s) = &self.store {
            for (p, f) in &s.entries {
                m.insert(p, score(*f, now));
            }
        }
        if let Zoxide::Loaded(v) = &self.zoxide {
            for (p, sc) in v {
                let e = m.entry(p).or_insert(*sc);
                *e = e.max(*sc);
            }
        }
        m.remove(exclude);
        let mut v: Vec<(PathBuf, f64)> = m.into_iter().map(|(p, s)| (p.to_path_buf(), s)).collect();
        v.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then_with(|| a.0.as_os_str().as_bytes().cmp(b.0.as_os_str().as_bytes()))
        });
        v
    }

    /// The directory `z <filter>` goes to (P2 3.4): the best-scoring match, else the first
    /// matching bookmark; never `exclude`.
    pub fn best(&self, filter: &[u8], exclude: &Path, now: i64) -> Option<PathBuf> {
        let m = Matcher::new(filter);
        let ranked = self.ranked(exclude, now);
        let bookmarks = self.hotlist.dirs.iter().filter(|p| p.as_path() != exclude);
        ranked
            .into_iter()
            .map(|(p, _)| p)
            .chain(bookmarks.cloned())
            .find(|p| m.matches(&lowered(p)))
    }
}

// ---- the directory-store thread -----------------------------------------------------------

/// Work for the directory-store thread (P2 2.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Read `dirs.tsv`.
    Load,
    /// Run `zoxide query` when zoxide is on `PATH`.
    Zoxide,
    /// Write `hotlist.toml` atomically.
    SaveHotlist(Vec<PathBuf>),
}

/// What the directory-store thread sends back.
#[derive(Debug)]
pub enum Reply {
    Loaded(Store),
    /// zoxide's ranking; empty when zoxide is missing or failed.
    Zoxide(Vec<(PathBuf, f64)>),
    /// A read, a save or the query failed: shown once as a warning.
    Failed(String),
}

/// Where the directory-store thread reads and writes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Paths {
    pub hotlist: Option<PathBuf>,
    pub store: Option<PathBuf>,
    /// The `PATH` searched for `zoxide`.
    pub search: Option<OsString>,
}

impl Paths {
    pub fn from_env() -> Paths {
        let home = std::env::var_os("HOME");
        Paths {
            hotlist: hotlist_path(
                std::env::var_os("XDG_CONFIG_HOME").as_deref(),
                home.as_deref(),
            ),
            store: store_path(
                std::env::var_os("XDG_STATE_HOME").as_deref(),
                home.as_deref(),
            ),
            search: std::env::var_os("PATH"),
        }
    }
}

fn serve(r: Request, paths: &Paths, send: &dyn Fn(Reply)) {
    match r {
        Request::Load => {
            let store = match &paths.store {
                Some(p) => Store::load(p).unwrap_or_else(|e| {
                    send(Reply::Failed(format!("{}: {e}", p.display())));
                    Store::default()
                }),
                None => Store::default(),
            };
            send(Reply::Loaded(store));
        }
        Request::Zoxide => {
            let list = match find_program("zoxide", paths.search.as_deref()) {
                Some(z) => {
                    zoxide_query(&[z.into_os_string()], ZOXIDE_TIMEOUT).unwrap_or_else(|e| {
                        send(Reply::Failed(e));
                        Vec::new()
                    })
                }
                None => Vec::new(),
            };
            send(Reply::Zoxide(list));
        }
        Request::SaveHotlist(dirs) => {
            if let Some(p) = &paths.hotlist
                && let Err(e) = Hotlist::save(&dirs, p)
            {
                send(Reply::Failed(format!(
                    "could not save {}: {e}",
                    p.display()
                )));
            }
        }
    }
}

/// The directory-store thread: it serves requests in order (so the last hotlist save
/// wins) and blocks on its queue while idle (P-5). It is named `list-dirs`, so a panic is
/// logged, not fatal; each request runs under `catch_unwind` (NFR-REL).
pub struct StoreThread {
    tx: Option<Sender<Request>>,
    handle: Option<JoinHandle<()>>,
}

impl StoreThread {
    pub fn spawn(
        paths: Paths,
        send: impl Fn(Reply) + Send + 'static,
    ) -> std::io::Result<StoreThread> {
        let (tx, rx) = channel::<Request>();
        let handle = std::thread::Builder::new()
            .name("list-dirs".into())
            .spawn(move || {
                for r in rx {
                    let lost = matches!(r, Request::Load);
                    let zoxide = matches!(r, Request::Zoxide);
                    if catch_unwind(AssertUnwindSafe(|| serve(r, &paths, &send))).is_err() {
                        send(Reply::Failed(
                            "internal error in the directory store".into(),
                        ));
                        // Whoever waits for the answer gets an empty one.
                        if lost {
                            send(Reply::Loaded(Store::default()));
                        }
                        if zoxide {
                            send(Reply::Zoxide(Vec::new()));
                        }
                    }
                }
            })?;
        Ok(StoreThread {
            tx: Some(tx),
            handle: Some(handle),
        })
    }

    pub fn request(&self, r: Request) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(r);
        }
    }

    /// Finishes the queued work (a hotlist save): closes the queue and waits at most `wait`
    /// for the thread. Returns whether it finished.
    pub fn finish(mut self, wait: Duration) -> bool {
        self.tx = None;
        let start = Instant::now();
        let Some(h) = self.handle.take() else {
            return true;
        };
        while !h.is_finished() {
            if start.elapsed() >= wait {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = h.join();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_round_trips() {
        let raw: &[u8] = b"/a\\b\tc\nd\x7fe\xff\xfe \xc3\xa9";
        let e = escape_path(raw);
        assert_eq!(e, "/a\\x5cb\\x09c\\x0ad\\x7fe\\xff\\xfe \u{e9}");
        assert!(!e.contains('\t') && !e.contains('\n'));
        assert_eq!(unescape_path(e.as_bytes()).unwrap(), raw);
        assert_eq!(unescape_path(b"/x\\X41").as_deref(), None);
        assert_eq!(unescape_path(b"/x\\x4").as_deref(), None);
        assert_eq!(unescape_path(b"/x\\x4G").as_deref(), None);
        assert_eq!(unescape_path(b"/x\\x4A").unwrap(), b"/xJ");
    }

    #[test]
    fn keyword_rules() {
        let m = |f: &str, p: &str| Matcher::new(f.as_bytes()).matches(&lowered(Path::new(p)));
        assert!(m("", "/any/thing"));
        assert!(m("  ", "/any/thing"));
        assert!(m("foo bar", "/home/u/foo/x/bar"));
        assert!(!m("bar foo", "/home/u/foo/x/bar"), "keywords in order");
        assert!(
            !m("foo", "/home/u/foo/x/bar"),
            "last keyword in the last component"
        );
        assert!(m("FOO BaR", "/home/u/Foo/x/BAR"), "ASCII case-insensitive");
        assert!(m("oo ba", "/home/u/foo/bar"));
        assert!(
            !m("bar bar", "/home/u/bar"),
            "each keyword is a separate occurrence"
        );
        assert!(m("bar bar", "/home/bar/bar"));
        assert!(m("u/fo", "/home/u/foo"), "a keyword may span a separator");
        assert!(!m("x", "/"));
    }
}
