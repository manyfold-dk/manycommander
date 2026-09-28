#![forbid(unsafe_code)]
//! Compare directories (P2 7).
//!
//! The UI thread copies each panel's visible entries (I-8) into a [`Request`]; everything
//! else runs on the compare thread, so the UI never spends the compare's time (P-1, P-13).
//! The thread opens each directory once and reads its filesystem type with one `fstatfs`
//! for the mtime resolution (M1 4.5). By date and size ([`compare_meta`]) it reads no file.
//! By content ([`compare_content`]) it reads every same-named pair of regular files of equal
//! size in 1 MiB chunks through the M1 4.3 open sequence: it never follows a symlink and
//! never opens a FIFO or a device (I-10). It sends back the entry indices to mark on each
//! side and a summary; the UI applies them only while both panels still show the listings
//! the request was made from. A new compare cancels the one before it; the thread checks
//! its cancel flag between pairs and between chunks. It runs under `catch_unwind`, so a
//! panic ends the compare with an error and the app stays up (NFR-REL).

use crate::fsops::copy::{dst_is_older, mtime_resolution};
use crate::fsops::sys::{Sys, Ts};
use crate::fsops::walk::{EntryError, errno_text, open_for_read};
use crate::panel::entry::EKind;
use rustix::fd::{AsFd, BorrowedFd};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt;
use std::os::unix::ffi::OsStrExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Bytes read per file and step of a content compare.
pub const CHUNK: usize = 1 << 20;
/// Progress events at most every this often (<= 15 Hz, P-8).
pub const PROGRESS_EVERY: Duration = Duration::from_millis(67);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// By date and size: reads no file.
    #[default]
    DateSize,
    /// By content: reads same-named regular files of equal size.
    Content,
}

/// One visible entry, as the panel listed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Item {
    /// The entry's index in the panel's listing: what a mark names.
    pub index: u32,
    name_off: u32,
    name_len: u32,
    pub kind: EKind,
    pub size: u64,
    pub mtime: Ts,
}

/// One panel's part of a compare: its directory, its visible entries, and the listing they
/// came from (the panel's slot and listing generation).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Side {
    pub dir: PathBuf,
    pub slot: usize,
    pub generation: u64,
    pub items: Vec<Item>,
    names: Vec<u8>,
}

impl Side {
    pub fn new(dir: PathBuf, slot: usize, generation: u64) -> Side {
        Side {
            dir,
            slot,
            generation,
            ..Side::default()
        }
    }

    /// Reserves room for `items` entries with `name_bytes` bytes of names in total.
    pub fn reserve(&mut self, items: usize, name_bytes: usize) {
        self.items.reserve(items);
        self.names.reserve(name_bytes);
    }

    pub fn push(&mut self, index: u32, name: &[u8], kind: EKind, size: u64, mtime: Ts) {
        let name_off = self.names.len() as u32;
        self.names.extend_from_slice(name);
        self.items.push(Item {
            index,
            name_off,
            name_len: name.len() as u32,
            kind,
            size,
            mtime,
        });
    }

    pub fn name(&self, it: &Item) -> &[u8] {
        &self.names[it.name_off as usize..(it.name_off + it.name_len) as usize]
    }
}

/// What the UI hands the compare thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Identifies the compare; the UI drops the events of an earlier one.
    pub id: u64,
    pub mode: Mode,
    /// Directories that exist on one side only are marked too.
    pub include_dirs: bool,
    pub left: Side,
    pub right: Side,
}

/// The counts behind the status row (P2 7).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub mode: Mode,
    pub left_newer: usize,
    pub left_only: usize,
    pub right_newer: usize,
    pub right_only: usize,
    /// Pairs of regular files that differ in size (marked on both sides).
    pub size_differ: usize,
    /// Pairs of equal size whose bytes differ (content; marked on both sides).
    pub content_differ: usize,
    /// Pairs that could not be read (content): not marked.
    pub unreadable: usize,
}

impl fmt::Display for Summary {
    /// `left: 3 newer, 5 only here; right: 1 newer, 2 only here; 1 differ in size`. By
    /// content no side is newer: pairs are judged by their bytes, and the text ends with
    /// `; N differ in content`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.mode {
            Mode::DateSize => write!(
                f,
                "left: {} newer, {} only here; right: {} newer, {} only here; {} differ in size",
                self.left_newer,
                self.left_only,
                self.right_newer,
                self.right_only,
                self.size_differ
            )?,
            Mode::Content => write!(
                f,
                "left: {} only here; right: {} only here; {} differ in size; {} differ in content",
                self.left_only, self.right_only, self.size_differ, self.content_differ
            )?,
        }
        if self.unreadable > 0 {
            write!(f, "; {} could not be read", self.unreadable)?;
        }
        Ok(())
    }
}

/// The entries to mark on each side (their listing indices), and the summary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Marks {
    pub left: Vec<u32>,
    pub right: Vec<u32>,
    pub summary: Summary,
}

/// How far a content compare is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub pairs_done: usize,
    pub pairs_total: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

/// What the compare thread sends (P2 2.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompareMsg {
    Progress {
        id: u64,
        progress: Progress,
    },
    /// The result, for the listings `(slot, generation)` of the request's left and right
    /// side.
    Marks {
        id: u64,
        listings: [(usize, u64); 2],
        marks: Marks,
    },
    /// The compare ended: after `Marks`, when cancelled, or with an error.
    Done {
        id: u64,
        error: Option<String>,
    },
}

/// The coarser mtime resolution of two filesystems, in nanoseconds, from their `f_type`
/// (M1 4.5): 2 s for vfat, 10 ms for exfat, 1 ns otherwise.
pub fn resolution(left_f_type: i64, right_f_type: i64) -> i128 {
    mtime_resolution(left_f_type).max(mtime_resolution(right_f_type))
}

/// Pairs the entries by name: the positions of the left-only and right-only items, and of
/// the pairs `(left, right)`.
fn pair(left: &Side, right: &Side) -> (Vec<usize>, Vec<usize>, Vec<(usize, usize)>) {
    let mut by_name: HashMap<&[u8], usize> = HashMap::with_capacity(right.items.len());
    for (k, it) in right.items.iter().enumerate() {
        by_name.insert(right.name(it), k);
    }
    let mut paired = vec![false; right.items.len()];
    let (mut left_only, mut pairs) = (Vec::new(), Vec::new());
    for (k, it) in left.items.iter().enumerate() {
        match by_name.get(left.name(it)) {
            Some(&r) => {
                paired[r] = true;
                pairs.push((k, r));
            }
            None => left_only.push(k),
        }
    }
    let right_only = (0..right.items.len()).filter(|&k| !paired[k]).collect();
    (left_only, right_only, pairs)
}

/// The rule for names on one side only: marked on that side, directories only with
/// "include directories".
fn mark_unique(
    m: &mut Marks,
    left: &Side,
    right: &Side,
    (left_only, right_only): (&[usize], &[usize]),
    include_dirs: bool,
) {
    let counts = |it: &Item| include_dirs || it.kind != EKind::Dir;
    for it in left_only
        .iter()
        .map(|&k| &left.items[k])
        .filter(|i| counts(i))
    {
        m.left.push(it.index);
        m.summary.left_only += 1;
    }
    for it in right_only
        .iter()
        .map(|&k| &right.items[k])
        .filter(|i| counts(i))
    {
        m.right.push(it.index);
        m.summary.right_only += 1;
    }
}

/// By date and size (P2 7). Reads no file.
///
/// | Case | Marked |
/// |---|---|
/// | Only on one side | on that side (directories only with `include_dirs`) |
/// | Both regular files, one strictly newer at `resolution` | the newer side |
/// | Both regular files, same mtime at `resolution`, different size | both sides |
/// | Same mtime and size, both directories, or not two regular files | neither |
pub fn compare_meta(left: &Side, right: &Side, resolution: i128, include_dirs: bool) -> Marks {
    let res = resolution.max(1);
    let (left_only, right_only, pairs) = pair(left, right);
    let mut m = Marks::default();
    mark_unique(&mut m, left, right, (&left_only, &right_only), include_dirs);
    for (l, r) in pairs {
        let (a, b) = (&left.items[l], &right.items[r]);
        if a.kind != EKind::File || b.kind != EKind::File {
            continue;
        }
        if dst_is_older(a.mtime, b.mtime, res) {
            m.left.push(a.index);
            m.summary.left_newer += 1;
        } else if dst_is_older(b.mtime, a.mtime, res) {
            m.right.push(b.index);
            m.summary.right_newer += 1;
        } else if a.size != b.size {
            m.left.push(a.index);
            m.right.push(b.index);
            m.summary.size_differ += 1;
        }
    }
    m
}

/// Why a pair was not compared to the end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairError {
    Cancelled,
    /// Not a regular file any more (a symlink, a FIFO, a device), gone, or unreadable.
    Entry(EntryError),
}

/// Reads `len` bytes or up to the end of the file.
fn fill(sys: &Sys, fd: BorrowedFd, buf: &mut [u8]) -> Result<usize, PairError> {
    let mut n = 0;
    while n < buf.len() {
        match sys.read("compare.read", fd, &mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) => return Err(PairError::Entry(EntryError::os("read", e))),
        }
    }
    Ok(n)
}

/// Whether the regular files `left.1` below `left.0` and `right.1` below `right.0` hold the
/// same bytes. Both are opened through the M1 4.3 sequence (`O_PATH | O_NOFOLLOW`, `fstat`,
/// reopen), so a symlink, a FIFO or a device fails with "type changed" and is never opened
/// for reading (I-10). Reads [`CHUNK`] bytes of each at a time into `bufs`, checks `cancel`
/// between chunks and reports the bytes of each chunk to `read`.
pub fn same_content(
    sys: &Sys,
    left: (BorrowedFd, &OsStr),
    right: (BorrowedFd, &OsStr),
    bufs: &mut [Vec<u8>; 2],
    cancel: &AtomicBool,
    read: &mut dyn FnMut(u64),
) -> Result<bool, PairError> {
    let (fa, ma) = open_for_read(sys, left.0, left.1, None).map_err(PairError::Entry)?;
    let (fb, mb) = open_for_read(sys, right.0, right.1, None).map_err(PairError::Entry)?;
    if ma.size != mb.size {
        return Ok(false);
    }
    let [a, b] = bufs;
    a.resize(CHUNK, 0);
    b.resize(CHUNK, 0);
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(PairError::Cancelled);
        }
        let na = fill(sys, fa.as_fd(), a)?;
        let nb = fill(sys, fb.as_fd(), b)?;
        if na != nb || a[..na] != b[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
        read(na as u64);
    }
}

/// By content (P2 7): the date-and-size rule for names on one side only, then every
/// same-named pair of regular files: different sizes differ without reading; equal sizes
/// are read ([`same_content`]). Differing pairs are marked on both sides; a pair that cannot
/// be read is counted, not marked. `dirs` are the two directories, opened once. `progress`
/// gets at most one call per [`PROGRESS_EVERY`]. Returns `None` when cancelled.
pub fn compare_content(
    left: &Side,
    right: &Side,
    include_dirs: bool,
    dirs: (BorrowedFd, BorrowedFd),
    sys: &Sys,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Option<Marks> {
    let (left_only, right_only, pairs) = pair(left, right);
    let mut m = Marks::default();
    m.summary.mode = Mode::Content;
    mark_unique(&mut m, left, right, (&left_only, &right_only), include_dirs);
    let mut to_read = Vec::new();
    for (l, r) in pairs {
        let (a, b) = (&left.items[l], &right.items[r]);
        if a.kind != EKind::File || b.kind != EKind::File {
            continue;
        }
        if a.size != b.size {
            m.left.push(a.index);
            m.right.push(b.index);
            m.summary.size_differ += 1;
        } else {
            to_read.push((a, b));
        }
    }
    let mut p = Progress {
        pairs_total: to_read.len(),
        bytes_total: to_read.iter().map(|(a, _)| a.size).sum(),
        ..Progress::default()
    };
    let mut bufs = [Vec::new(), Vec::new()];
    let mut last = Instant::now();
    for (a, b) in to_read {
        if cancel.load(Ordering::SeqCst) {
            return None;
        }
        let before = p.bytes_done;
        let verdict = same_content(
            sys,
            (dirs.0, OsStr::from_bytes(left.name(a))),
            (dirs.1, OsStr::from_bytes(right.name(b))),
            &mut bufs,
            cancel,
            &mut |n| {
                p.bytes_done += n;
                if last.elapsed() >= PROGRESS_EVERY {
                    last = Instant::now();
                    progress(p);
                }
            },
        );
        match verdict {
            Ok(true) => {}
            Ok(false) => {
                m.left.push(a.index);
                m.right.push(b.index);
                m.summary.content_differ += 1;
            }
            Err(PairError::Cancelled) => return None,
            Err(PairError::Entry(_)) => m.summary.unreadable += 1,
        }
        p.pairs_done += 1;
        p.bytes_done = before + a.size;
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            progress(p);
        }
    }
    Some(m)
}

/// The compare thread's work: opens both directories, reads their filesystem types, runs
/// the compare and sends its messages. `Done` always comes last.
pub fn run(req: &Request, cancel: &AtomicBool, send: &dyn Fn(CompareMsg)) {
    let id = req.id;
    let sys = Sys::default();
    let open = |s: &Side| {
        sys.open_root(&s.dir)
            .map_err(|e| format!("{}: {}", s.dir.display(), errno_text(e)))
    };
    let (l, r) = match (open(&req.left), open(&req.right)) {
        (Ok(l), Ok(r)) => (l, r),
        (Err(e), _) | (_, Err(e)) => {
            send(CompareMsg::Done { id, error: Some(e) });
            return;
        }
    };
    let f_type = |fd: BorrowedFd| sys.fstatfs(fd).map(|s| s.f_type).unwrap_or(0);
    let res = resolution(f_type(l.as_fd()), f_type(r.as_fd()));
    let marks = match req.mode {
        Mode::DateSize => Some(compare_meta(&req.left, &req.right, res, req.include_dirs)),
        Mode::Content => compare_content(
            &req.left,
            &req.right,
            req.include_dirs,
            (l.as_fd(), r.as_fd()),
            &sys,
            cancel,
            &mut |progress| send(CompareMsg::Progress { id, progress }),
        ),
    };
    if let Some(marks) = marks
        && !cancel.load(Ordering::SeqCst)
    {
        send(CompareMsg::Marks {
            id,
            listings: [
                (req.left.slot, req.left.generation),
                (req.right.slot, req.right.generation),
            ],
            marks,
        });
    }
    send(CompareMsg::Done { id, error: None });
}

/// Runs [`run`] under `catch_unwind`; a panic ends the compare with an error.
pub fn guarded(req: &Request, cancel: &AtomicBool, send: &dyn Fn(CompareMsg)) {
    if catch_unwind(AssertUnwindSafe(|| run(req, cancel, send))).is_err() {
        send(CompareMsg::Done {
            id: req.id,
            error: Some("internal error while comparing".into()),
        });
    }
}

/// Starts the compare thread, named `list-compare` (a panic there is logged, not fatal).
pub fn spawn(
    req: Request,
    cancel: Arc<AtomicBool>,
    send: impl Fn(CompareMsg) + Send + 'static,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("list-compare".into())
        .spawn(move || guarded(&req, &cancel, &send))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn side(entries: &[(&str, EKind, u64, i64)]) -> Side {
        let mut s = Side::new(PathBuf::from("/x"), 0, 1);
        for (k, (n, kind, size, sec)) in entries.iter().enumerate() {
            s.push(
                k as u32 * 10,
                n.as_bytes(),
                *kind,
                *size,
                Ts { sec: *sec, nsec: 0 },
            );
        }
        s
    }

    #[test]
    fn names_pair_by_bytes_and_marks_carry_listing_indices() {
        let l = side(&[("a", EKind::File, 1, 5), ("B", EKind::File, 1, 5)]);
        let r = side(&[("b", EKind::File, 1, 5), ("a", EKind::File, 2, 5)]);
        let m = compare_meta(&l, &r, 1, true);
        // "B" and "b" are different names; "a" differs in size.
        assert_eq!(m.left, [10, 0]);
        assert_eq!(m.right, [0, 10]);
        assert_eq!(
            m.summary.to_string(),
            "left: 0 newer, 1 only here; right: 0 newer, 1 only here; 1 differ in size"
        );
    }

    #[test]
    fn summary_text() {
        let s = Summary {
            left_newer: 3,
            left_only: 5,
            right_newer: 1,
            right_only: 2,
            size_differ: 1,
            ..Summary::default()
        };
        assert_eq!(
            s.to_string(),
            "left: 3 newer, 5 only here; right: 1 newer, 2 only here; 1 differ in size"
        );
        let c = Summary {
            mode: Mode::Content,
            content_differ: 4,
            unreadable: 1,
            ..s
        };
        assert_eq!(
            c.to_string(),
            "left: 5 only here; right: 2 only here; 1 differ in size; 4 differ in content; \
             1 could not be read"
        );
    }

    #[test]
    fn resolution_is_the_coarser_one() {
        use crate::fsops::sys::magic;
        assert_eq!(resolution(magic::BTRFS, magic::TMPFS), 1);
        assert_eq!(resolution(magic::BTRFS, magic::VFAT), 2_000_000_000);
        assert_eq!(resolution(magic::EXFAT, magic::TMPFS), 10_000_000);
        assert_eq!(resolution(magic::EXFAT, magic::VFAT), 2_000_000_000);
    }
}
