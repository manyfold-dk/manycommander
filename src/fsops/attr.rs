#![forbid(unsafe_code)]
//! Change attributes (Alt+A, P2 8.2): the mode grammar and the job.
//!
//! **Mode grammar.** Octal: one to four octal digits; it sets exactly those bits (every
//! other bit of `07777` is cleared). Symbolic: comma-separated clauses
//! `[ugoa]*[+-=][rwxXst]*`, applied in order as chmod(1) does, with two differences: a
//! clause without a class means `a` and ignores the umask, and `X` means "execute where the
//! entry is a directory or already has an execute bit". `s` is setuid for `u` and setgid for
//! `g`; `t` is the sticky bit, which belongs to `o` as in chmod(1). `=` clears the class's
//! `rwx` and its `s`/`t` bits, then sets. The result depends on the entry only through `X`.
//!
//! **The job** does not scan first: a directory the change makes traversable could not be
//! scanned before the change, so it traverses while it executes and reports progress as a
//! running count. Each entry is opened `O_PATH | O_NOFOLLOW` and checked with `fstat`; its
//! mode changes through `chmod("/proc/self/fd/<n>")` and its time through `utimensat` on the
//! same path with `UTIME_OMIT` for the access time, so both reach that inode and never a
//! symlink target (I-5). A symlink keeps its mode; a time is set on the link itself with
//! `AT_SYMLINK_NOFOLLOW`, and a mode-only change skips it. Special files are changed the
//! same way and never opened for I/O. Recursion follows the M1 4.2 table as permanent
//! delete does: it descends into subvolumes and skips mount points. A directory first gets
//! the intermediate mode `old | new`, is then reopened `O_RDONLY | O_DIRECTORY |
//! O_NOFOLLOW` (identity compared) to read its entries, and gets its final mode after its
//! children: adding `r` or `x` makes it readable before it is read, and removing them
//! happens only after its children are done.

use super::copy::{Dir, Flow, Transfer};
use super::group::Group;
use super::identity::{Relation, relation};
use super::job::{JobVerb, Report};
use super::question::{Interaction, Reporter};
use super::sys::{Kind, Meta, Sys, Ts, fd};
use super::walk::{EntryError, open_child_dir};
use rustix::fd::OwnedFd;
use rustix::io::Errno;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Every bit a mode change may touch: permissions, setuid, setgid, sticky.
pub const MODE_BITS: u32 = 0o7777;

/// How a clause combines its bits with the mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Add,
    Remove,
    Set,
}

/// One symbolic clause, `[ugoa]*[+-=][rwxXst]*`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clause {
    /// The bits of the clause's classes: `u` = `04700`, `g` = `02070`, `o` = `01007`.
    pub who: u32,
    pub op: Op,
    /// `r`, `w`, `x`, `s`, `t` as bits of every class; masked with `who` when applied.
    pub bits: u32,
    /// `X`: execute where the entry is a directory or already has an execute bit.
    pub exec_if: bool,
}

/// A parsed mode (P2 8.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModeChange {
    /// Exactly these bits.
    Octal(u32),
    /// Clauses applied in order.
    Symbolic(Vec<Clause>),
}

/// The grammar, for form help and errors.
pub const GRAMMAR: &str = "octal (644, 4755) or [ugoa]*[+-=][rwxXst]* clauses (u+x,g-w,o=r)";

impl ModeChange {
    /// Parses the form's text. Surrounding blanks are ignored; anything else outside the
    /// grammar is an error that names it.
    pub fn parse(text: &[u8]) -> Result<ModeChange, String> {
        let s = std::str::from_utf8(text)
            .map_err(|_| "the mode is not ASCII".to_string())?
            .trim();
        if s.is_empty() {
            return Err("the mode is empty".into());
        }
        if s.bytes().all(|c| c.is_ascii_digit()) {
            if s.len() > 4 || s.bytes().any(|c| c > b'7') {
                return Err(format!("{s}: an octal mode is one to four digits 0-7"));
            }
            let v = u32::from_str_radix(s, 8).map_err(|e| e.to_string())?;
            return Ok(ModeChange::Octal(v));
        }
        let mut clauses = Vec::new();
        for c in s.split(',') {
            clauses.push(clause(c).ok_or_else(|| format!("{c:?} is not a mode; use {GRAMMAR}"))?);
        }
        Ok(ModeChange::Symbolic(clauses))
    }

    /// The new permission bits of an entry whose bits are `old` (P2 8.2). `dir` resolves `X`.
    pub fn apply(&self, old: u32, dir: bool) -> u32 {
        match self {
            ModeChange::Octal(v) => v & MODE_BITS,
            ModeChange::Symbolic(cs) => cs.iter().fold(old & MODE_BITS, |cur, c| {
                let mut v = c.bits & c.who;
                if c.exec_if && (dir || cur & 0o111 != 0) {
                    v |= 0o111 & c.who;
                }
                match c.op {
                    Op::Add => cur | v,
                    Op::Remove => cur & !v,
                    Op::Set => (cur & !c.who) | v,
                }
            }),
        }
    }
}

fn clause(c: &str) -> Option<Clause> {
    let b = c.as_bytes();
    let mut i = 0;
    let mut who = 0;
    while let Some(&x) = b.get(i) {
        who |= match x {
            b'u' => 0o4700,
            b'g' => 0o2070,
            b'o' => 0o1007,
            b'a' => MODE_BITS,
            _ => break,
        };
        i += 1;
    }
    if who == 0 {
        // No class: all of them, without the umask.
        who = MODE_BITS;
    }
    let op = match b.get(i)? {
        b'+' => Op::Add,
        b'-' => Op::Remove,
        b'=' => Op::Set,
        _ => return None,
    };
    let mut bits = 0;
    let mut exec_if = false;
    for &x in &b[i + 1..] {
        match x {
            b'r' => bits |= 0o444,
            b'w' => bits |= 0o222,
            b'x' => bits |= 0o111,
            b's' => bits |= 0o6000,
            b't' => bits |= 0o1000,
            b'X' => exec_if = true,
            _ => return None,
        }
    }
    Some(Clause {
        who,
        op,
        bits,
        exec_if,
    })
}

/// `rwxr-xr-x`, with `s`/`S` and `t`/`T` as ls(1) shows setuid, setgid and sticky.
pub fn perm_text(perm: u32) -> String {
    let mut s = String::with_capacity(9);
    for (shift, special, mark) in [(6, 0o4000, 's'), (3, 0o2000, 's'), (0, 0o1000, 't')] {
        let p = perm >> shift;
        s.push(if p & 4 != 0 { 'r' } else { '-' });
        s.push(if p & 2 != 0 { 'w' } else { '-' });
        let x = p & 1 != 0;
        s.push(match (perm & special != 0, x) {
            (true, true) => mark,
            (true, false) => mark.to_ascii_uppercase(),
            (false, true) => 'x',
            (false, false) => '-',
        });
    }
    s
}

/// `0755 (rwxr-xr-x)`, as the report names a mode.
pub fn mode_text(perm: u32) -> String {
    format!("{perm:04o} ({})", perm_text(perm))
}

/// What a job changes. `None` leaves that attribute as it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttrChange {
    pub mode: Option<ModeChange>,
    pub mtime: Option<Ts>,
    /// Apply to everything below the selected directories.
    pub recursive: bool,
}

/// Alt+A over groups (P2 8.2).
pub fn attr_groups(
    sys: &Sys,
    ui: &mut dyn Interaction,
    groups: &[Group],
    change: &AttrChange,
) -> Report {
    let verb = JobVerb::Attr;
    if change.mode.is_none() && change.mtime.is_none() {
        return Report::refused(verb, "nothing to change");
    }
    let mut opened = match super::group::open(sys, verb, groups) {
        Ok(o) => o,
        Err(r) => return *r,
    };
    // Each directory once, also when two groups reach it (P2 2.2).
    opened.merge();
    let mut w = Walk {
        t: Transfer::new(sys, Reporter::new(ui), Report::new(verb)),
        change,
        stack: Vec::new(),
    };
    opened.report_failed(&mut w.t.report);
    'job: for s in &opened.sources {
        w.stack = vec![s.dir.meta.id.inode()];
        for name in &s.names {
            if w.entry(&s.dir, name, true) == Flow::Stop {
                break 'job;
            }
        }
    }
    w.t.report
}

/// The outcome of one change, after the error question.
enum Step {
    Ok,
    Failed(String),
    Stop,
}

struct Walk<'a, 'u, 'c> {
    t: Transfer<'a, 'u>,
    change: &'c AttrChange,
    /// `(st_dev, st_ino)` of the directories on the current path (design 4.6 cycle check).
    stack: Vec<(u64, u64)>,
}

impl Walk<'_, '_, '_> {
    /// Runs `call` until it succeeds, asking the error question (design 4.5) on failure.
    fn step(
        &mut self,
        path: &Path,
        op: &'static str,
        mut call: impl FnMut() -> Result<(), Errno>,
    ) -> Step {
        loop {
            match call() {
                Ok(()) => return Step::Ok,
                Err(e) => match self.t.decide_error(path, op, e) {
                    Some(true) => {}
                    Some(false) => return Step::Failed(EntryError::os(op, e).to_string()),
                    None => return Step::Stop,
                },
            }
        }
    }

    /// Counts a visited entry for the running progress.
    fn visited(&mut self) -> Flow {
        self.t.files_done += 1;
        self.t.tick();
        Flow::Continue
    }

    fn done(&mut self, dir: bool) -> Flow {
        if dir {
            self.t.report.dirs_done += 1;
        } else {
            self.t.report.done += 1;
        }
        self.visited()
    }

    fn unchanged(&mut self) -> Flow {
        self.t.report.unchanged += 1;
        self.visited()
    }

    fn fail(&mut self, path: PathBuf, why: impl Into<String>) -> Flow {
        self.t.report.fail(path, why);
        self.visited()
    }

    fn skip(&mut self, path: PathBuf, why: impl Into<String>) -> Flow {
        self.t.report.skip(path, why);
        self.visited()
    }

    /// The entry's new permission bits, when the form sets a mode.
    fn new_mode(&self, m: &Meta) -> Option<u32> {
        self.change
            .mode
            .as_ref()
            .map(|c| c.apply(m.perm, m.kind == Kind::Dir))
    }

    /// The time to set, when it differs from the entry's.
    fn new_time(&self, m: &Meta) -> Option<Ts> {
        self.change.mtime.filter(|t| *t != m.mtime)
    }

    /// One entry below `parent`. `top`: a selected entry, never skipped as a mount point.
    fn entry(&mut self, parent: &Dir, name: &OsStr, top: bool) -> Flow {
        if self.t.stopped() || self.t.cancelled() {
            return self.t.stop();
        }
        let sys = self.t.sys;
        let path = parent.path.join(name);
        self.t.set_current(path.clone());
        let opath = loop {
            match sys.open_path("attr.open", parent.fd(), name) {
                Ok(f) => break f,
                Err(Errno::NOENT) => return self.fail(path, EntryError::Disappeared.to_string()),
                Err(e) => match self.t.decide_error(&path, "open", e) {
                    Some(true) => {}
                    Some(false) => return self.fail(path, EntryError::os("open", e).to_string()),
                    None => return self.t.stop(),
                },
            }
        };
        let meta = match sys.stat_fd(fd(&opath)) {
            Ok(m) => m,
            Err(e) => return self.fail(path, EntryError::os("stat", e).to_string()),
        };
        match meta.kind {
            Kind::Symlink => self.symlink(parent, name, path, &meta),
            Kind::Dir if self.change.recursive => {
                if !top && relation(&parent.meta.id, &meta.id) == Relation::Mount {
                    return self.skip(path, "mount point");
                }
                if self.stack.contains(&meta.id.inode()) {
                    return self.skip(path, "directory cycle (bind-mount loop)");
                }
                self.dir(parent, name, path, &opath, &meta)
            }
            _ => self.plain(path, &opath, &meta),
        }
    }

    /// A symlink keeps its mode; a time is set on the link itself (P2 8.2, I-5).
    fn symlink(&mut self, parent: &Dir, name: &OsStr, path: PathBuf, meta: &Meta) -> Flow {
        let Some(t) = self.change.mtime else {
            return self.skip(path, "symbolic links have no mode of their own");
        };
        if t == meta.mtime {
            return self.unchanged();
        }
        let sys = self.t.sys;
        match self.step(&path, "set time", || {
            sys.set_mtime_nofollow("attr.lutimes", parent.fd(), name, t)
        }) {
            Step::Ok => self.done(false),
            Step::Failed(why) => self.fail(path, why),
            Step::Stop => self.t.stop(),
        }
    }

    /// A file, a special file, or a directory without recursion: one change through the
    /// `O_PATH` fd.
    fn plain(&mut self, path: PathBuf, opath: &OwnedFd, meta: &Meta) -> Flow {
        let dir = meta.kind == Kind::Dir;
        let mode = self.new_mode(meta).filter(|m| *m != meta.perm);
        let time = self.new_time(meta);
        if mode.is_none() && time.is_none() {
            return self.unchanged();
        }
        let sys = self.t.sys;
        if let Some(m) = mode {
            match self.step(&path, "change mode", || {
                sys.chmod_fd("attr.chmod", fd(opath), m)
            }) {
                Step::Ok => {}
                Step::Failed(why) => return self.fail(path, why),
                Step::Stop => return self.t.stop(),
            }
        }
        if let Some(t) = time {
            match self.step(&path, "set time", || {
                sys.set_mtime_fd("attr.utimes", fd(opath), t)
            }) {
                Step::Ok => {}
                Step::Failed(why) => return self.fail(path, why),
                Step::Stop => return self.t.stop(),
            }
        }
        self.done(dir)
    }

    /// A directory with recursion (P2 8.2 directory order): the intermediate mode, then
    /// its entries through a reopened fd, then the final mode and the time.
    fn dir(
        &mut self,
        parent: &Dir,
        name: &OsStr,
        path: PathBuf,
        opath: &OwnedFd,
        meta: &Meta,
    ) -> Flow {
        let sys = self.t.sys;
        let old = meta.perm;
        let new = self.new_mode(meta).unwrap_or(old);
        let inter = old | new;
        if inter != old {
            match self.step(&path, "change mode", || {
                sys.chmod_fd("attr.chmod", fd(opath), inter)
            }) {
                Step::Ok => {}
                Step::Failed(why) => {
                    return self.fail(path, format!("{why}; its entries were not visited"));
                }
                Step::Stop => return self.t.stop(),
            }
        }
        // `left`: what the report says about the mode the directory keeps when it stops
        // before its final change.
        let left = |m: u32| {
            if m == old {
                String::new()
            } else {
                format!("; left with mode {}", mode_text(m))
            }
        };
        let read = loop {
            match open_child_dir(sys, "attr.openat", parent.fd(), name, &meta.id) {
                Ok((d, m)) => break Ok((d, m)),
                Err(EntryError::Os { op, errno }) => match self.t.decide_error(&path, op, errno) {
                    Some(true) => {}
                    Some(false) => break Err(EntryError::Os { op, errno }.to_string()),
                    None => return self.cancelled_in(&path, inter, old),
                },
                Err(e) => break Err(e.to_string()),
            }
        };
        let (dfd, dmeta) = match read {
            Ok(x) => x,
            Err(why) => {
                let why = format!("{why}; its entries were not visited{}", left(inter));
                return self.fail(path, why);
            }
        };
        let names = loop {
            match sys.read_dir("attr.readdir", fd(&dfd)) {
                Ok(n) => break Ok(n),
                Err(e) => match self.t.decide_error(&path, "read directory", e) {
                    Some(true) => {}
                    Some(false) => break Err(EntryError::os("read directory", e).to_string()),
                    None => return self.cancelled_in(&path, inter, old),
                },
            }
        };
        let mut names: Vec<OsString> = match names {
            Ok(n) => n.into_iter().map(|(n, _)| n).collect(),
            Err(why) => {
                let why = format!("{why}; its entries were not visited{}", left(inter));
                return self.fail(path, why);
            }
        };
        names.sort_unstable_by(|a, b| a.as_encoded_bytes().cmp(b.as_encoded_bytes()));
        let d = Dir {
            fd: Arc::new(dfd),
            meta: dmeta,
            path: path.clone(),
        };
        self.stack.push(meta.id.inode());
        for n in &names {
            if self.entry(&d, n, false) == Flow::Stop {
                self.stack.pop();
                return self.cancelled_in(&path, inter, old);
            }
        }
        self.stack.pop();
        if new != inter {
            match self.step(&path, "change mode", || {
                sys.chmod_fd("attr.chmod", fd(opath), new)
            }) {
                Step::Ok => {}
                Step::Failed(why) => {
                    return self.fail(path, format!("{why}; left with mode {}", mode_text(inter)));
                }
                Step::Stop => return self.cancelled_in(&path, inter, old),
            }
        }
        let time = self.new_time(meta);
        if let Some(t) = time {
            match self.step(&path, "set time", || {
                sys.set_mtime_fd("attr.utimes", fd(opath), t)
            }) {
                Step::Ok => {}
                Step::Failed(why) => return self.fail(path, why),
                Step::Stop => return self.t.stop(),
            }
        }
        if new == old && time.is_none() {
            self.unchanged()
        } else {
            self.done(true)
        }
    }

    /// The job stops inside a directory that got its intermediate mode: the report states
    /// the mode it keeps (I-7).
    fn cancelled_in(&mut self, path: &Path, inter: u32, old: u32) -> Flow {
        if inter != old {
            self.t.report.notes.push(format!(
                "{}: left with mode {}; the job stopped before its final mode",
                path.display(),
                mode_text(inter)
            ));
        }
        self.t.stop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: &str, old: u32, dir: bool) -> u32 {
        ModeChange::parse(m.as_bytes()).unwrap().apply(old, dir)
    }

    #[test]
    fn octal_sets_exactly_those_bits() {
        assert_eq!(apply("644", 0o7777, false), 0o644);
        assert_eq!(apply("0644", 0o4755, false), 0o644);
        assert_eq!(apply("4755", 0o644, false), 0o4755);
        assert_eq!(apply("7", 0o777, true), 0o007);
        assert_eq!(apply(" 750 ", 0, true), 0o750);
    }

    #[test]
    fn symbolic_clauses_apply_in_order() {
        assert_eq!(apply("u+x,g-w,o=r", 0o666, false), 0o744);
        assert_eq!(apply("a-x", 0o755, true), 0o644);
        assert_eq!(
            apply("+x", 0o644, false),
            0o755,
            "no class is a, without umask"
        );
        assert_eq!(apply("u=rw,u+x", 0o000, false), 0o700);
        assert_eq!(apply("go=", 0o2775, true), 0o700, "= clears setgid with g");
        assert_eq!(apply("o=r", 0o1777, true), 0o774, "= clears sticky with o");
        assert_eq!(apply("a=", 0o7777, true), 0);
        assert_eq!(apply("+", 0o640, false), 0o640);
    }

    #[test]
    fn special_bits() {
        assert_eq!(apply("u+s", 0o755, false), 0o4755);
        assert_eq!(apply("g+s", 0o755, true), 0o2755);
        assert_eq!(apply("+s", 0o755, false), 0o6755);
        assert_eq!(apply("o+s", 0o755, false), 0o755, "s has no bit for o");
        assert_eq!(apply("+t", 0o777, true), 0o1777);
        assert_eq!(apply("o+t", 0o777, true), 0o1777);
        assert_eq!(
            apply("u+t", 0o777, true),
            0o777,
            "t belongs to o, as in chmod(1)"
        );
        assert_eq!(apply("u-s,g-s", 0o6755, false), 0o755);
    }

    #[test]
    fn capital_x_depends_on_the_entry() {
        assert_eq!(apply("+X", 0o644, true), 0o755, "a directory");
        assert_eq!(
            apply("+X", 0o744, false),
            0o755,
            "a file with an execute bit"
        );
        assert_eq!(apply("+X", 0o644, false), 0o644, "a file without one");
        assert_eq!(apply("go+X", 0o700, false), 0o711);
        assert_eq!(
            apply("a-x,+X", 0o755, false),
            0o644,
            "X sees the earlier clause"
        );
        assert_eq!(apply("u=rwX", 0o000, true), 0o700);
    }

    #[test]
    fn errors() {
        for bad in [
            "", " ", "8", "0788", "04755", "12345", "u", "X", "ux", "u+q", "u+x,", ",u+x",
            "u+x,,g+w", "u+-x", "g=u", "0x644", "+x y", "u +x",
        ] {
            assert!(ModeChange::parse(bad.as_bytes()).is_err(), "{bad:?}");
        }
        assert!(ModeChange::parse(b"\xff").is_err());
    }

    #[test]
    fn perm_text_as_ls() {
        assert_eq!(perm_text(0o644), "rw-r--r--");
        assert_eq!(perm_text(0o4755), "rwsr-xr-x");
        assert_eq!(perm_text(0o2644), "rw-r-Sr--");
        assert_eq!(perm_text(0o1777), "rwxrwxrwt");
        assert_eq!(perm_text(0o1776), "rwxrwxrwT");
        assert_eq!(mode_text(0o755), "0755 (rwxr-xr-x)");
    }
}
