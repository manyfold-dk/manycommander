#![forbid(unsafe_code)]
//! Trash (F8): the freedesktop.org Trash specification 1.0, compatible with GIO
//! (design 4.10).
//!
//! Trash directories are opened, not resolved: every trash directory and its `files/`
//! and `info/` are opened `O_DIRECTORY | O_NOFOLLOW` and checked with `fstat`. Trash never
//! copies across filesystems, and a trash failure never deletes (I-6): the only permanent
//! delete reachable from here is the typed `delete` confirmation for that one entry.

use super::copy::{Dir, Flow, Transfer, Unlink};
use super::delete::confirm_and_remove;
use super::group::{Group, Source};
use super::job::{JobVerb, Report};
use super::plan::Node;
use super::question::{Answer, Interaction, Question, Reporter};
use super::sys::{Kind, Sys, fd_path, uid};
use super::walk::{EntryError, errno_text};
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use rustix::io::Errno;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

const INFO_SUFFIX: &str = ".trashinfo";
const NAME_MAX: usize = 255;

/// `$XDG_DATA_HOME` when it is set to an absolute path, else `$HOME/.local/share`.
pub fn data_home(xdg: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    if let Some(x) = xdg.filter(|x| x.as_bytes().first() == Some(&b'/')) {
        return Some(PathBuf::from(x));
    }
    home.filter(|h| !h.is_empty())
        .map(|h| Path::new(h).join(".local/share"))
}

/// Percent-encodes a path byte-wise as GIO does (`g_uri_escape_string` with `/` allowed):
/// unreserved ASCII and `/` stay literal; every other byte, including non-UTF-8 bytes,
/// becomes `%XX`.
pub fn encode_path(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len());
    for &c in b {
        if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

/// Decodes a `Path=` value back to bytes (the inverse of [`encode_path`]).
pub fn decode_path(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(out)
}

/// The trash name for attempt `k`: the basename, then `N.2`, `N.3`, ... shortened by bytes
/// so that `N.trashinfo` fits in `NAME_MAX`.
pub fn candidate(name: &[u8], k: u32) -> Vec<u8> {
    let suffix = if k == 1 {
        Vec::new()
    } else {
        format!(".{k}").into_bytes()
    };
    let max = NAME_MAX - INFO_SUFFIX.len() - suffix.len();
    let mut v = name[..name.len().min(max)].to_vec();
    v.extend_from_slice(&suffix);
    v
}

/// An opened trash directory.
struct Can {
    /// The canonical path of the trash directory.
    root: PathBuf,
    files: OwnedFd,
    info: OwnedFd,
    /// `Some(top)` for a top-directory trash: `Path` is then relative to `top`.
    top: Option<PathBuf>,
}

/// Opens (creating 0700 when `create`) a trash directory component. A symlink or a
/// non-directory is refused; `owner` demands the user's ownership.
fn open_part(
    sys: &Sys,
    parent: BorrowedFd,
    name: &OsStr,
    create: bool,
    owner: Option<u32>,
) -> Result<OwnedFd, String> {
    let shown = Path::new(name).display().to_string();
    for _ in 0..2 {
        match sys.open_dir("trash.open", parent, name) {
            Ok(fd) => {
                let m = sys
                    .stat_fd(fd.as_fd())
                    .map_err(|e| format!("{shown}: {}", errno_text(e)))?;
                if m.kind != Kind::Dir {
                    return Err(format!("{shown} is not a directory"));
                }
                if let Some(u) = owner
                    && m.uid != u
                {
                    return Err(format!("{shown} is not owned by you"));
                }
                return Ok(fd);
            }
            Err(Errno::NOENT) if create => match sys.mkdir("trash.mkdir", parent, name, 0o700) {
                Ok(()) | Err(Errno::EXIST) => continue,
                Err(e) => return Err(format!("cannot create {shown}: {}", errno_text(e))),
            },
            Err(Errno::LOOP | Errno::NOTDIR) => {
                return Err(format!("{shown} is a symlink or not a directory"));
            }
            Err(e) => return Err(format!("{shown}: {}", errno_text(e))),
        }
    }
    Err(format!("{shown} could not be opened"))
}

fn open_can(sys: &Sys, trash: OwnedFd, top: Option<PathBuf>) -> Result<Can, String> {
    let root = fd_path(trash.as_fd()).map_err(errno_text)?;
    let files = open_part(sys, trash.as_fd(), OsStr::new("files"), true, None)?;
    let info = open_part(sys, trash.as_fd(), OsStr::new("info"), true, None)?;
    Ok(Can {
        root,
        files,
        info,
        top,
    })
}

/// The home trash, `$XDG_DATA_HOME/Trash`, created 0700 when missing.
fn home_can(sys: &Sys, data_home: &Path) -> Result<Can, String> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(data_home)
        .map_err(|e| format!("{}: {e}", data_home.display()))?;
    let parent = sys
        .open_root(data_home)
        .map_err(|e| format!("{}: {}", data_home.display(), errno_text(e)))?;
    let trash = open_part(sys, parent.as_fd(), OsStr::new("Trash"), true, Some(uid()))
        .map_err(|e| format!("home trash: {e}"))?;
    open_can(sys, trash, None)
}

/// The top directory of an entry whose parent is `dir`: walk up while the parent is in the
/// entry's domain (`(st_dev, mnt_id)`).
fn find_top(
    sys: &Sys,
    dir: &Dir,
    canon: &Path,
    dom: (u64, u64),
) -> Result<(OwnedFd, PathBuf), String> {
    if dir.meta.id.domain() != dom {
        return Err("the entry is the root of its filesystem or subvolume".into());
    }
    let mut cur = sys
        .open_dir("trash.top", dir.fd(), OsStr::new("."))
        .map_err(errno_text)?;
    let mut cur_ino = dir.meta.id.inode();
    let mut path = canon.to_path_buf();
    loop {
        let up = sys
            .open_dir("trash.top", cur.as_fd(), OsStr::new(".."))
            .map_err(errno_text)?;
        let um = sys.stat_fd(up.as_fd()).map_err(errno_text)?;
        if um.id.inode() == cur_ino || um.id.domain() != dom {
            return Ok((cur, path));
        }
        cur = up;
        cur_ino = um.id.inode();
        path.pop();
    }
}

/// Method 1 (`$top/.Trash/$uid`, when `.Trash` is a sticky directory and not a symlink),
/// then method 2 (`$top/.Trash-$uid`, owned by the user).
fn top_cans(sys: &Sys, dir: &Dir, canon: &Path, dom: (u64, u64)) -> (Vec<Can>, Vec<String>) {
    let mut cans = Vec::new();
    let mut why = Vec::new();
    let (top, top_path) = match find_top(sys, dir, canon, dom) {
        Ok(x) => x,
        Err(e) => return (cans, vec![e]),
    };
    let u = uid();
    let uid_name = OsString::from(u.to_string());
    match sys.open_dir("trash.open", top.as_fd(), OsStr::new(".Trash")) {
        Ok(fd) => match sys.stat_fd(fd.as_fd()) {
            Ok(m) if m.kind == Kind::Dir && m.perm & 0o1000 != 0 => {
                match open_part(sys, fd.as_fd(), &uid_name, true, Some(u))
                    .and_then(|t| open_can(sys, t, Some(top_path.clone())))
                {
                    Ok(c) => cans.push(c),
                    Err(e) => why.push(format!(".Trash/{u}: {e}")),
                }
            }
            Ok(_) => why.push(".Trash has no sticky bit; method 1 skipped".into()),
            Err(e) => why.push(format!(".Trash: {}", errno_text(e))),
        },
        Err(Errno::NOENT) => {}
        Err(Errno::LOOP | Errno::NOTDIR) => {
            why.push(".Trash is a symlink or not a directory; method 1 skipped".into())
        }
        Err(e) => why.push(format!(".Trash: {}", errno_text(e))),
    }
    let m2 = OsString::from(format!(".Trash-{u}"));
    match open_part(sys, top.as_fd(), &m2, true, Some(u))
        .and_then(|t| open_can(sys, t, Some(top_path.clone())))
    {
        Ok(c) => cans.push(c),
        Err(e) => why.push(e),
    }
    (cans, why)
}

/// The top-directory trashes of one domain, and why the others were not usable. The key
/// is the entry's domain and whether the source directory is in it: [`find_top`] gives the
/// same answer for every directory of a domain, and a different one for an entry whose
/// directory is outside it (P2 2.2: groups share this cache).
type TopCans = (((u64, u64), bool), Vec<Can>, Vec<String>);

enum TrashFail {
    /// The domain check was defeated (for example by a concurrent mount): try the next
    /// method. A normal failure, not a bug.
    Xdev,
    Os(&'static str, Errno),
}

/// Trashes one entry into `can` (design 4.10, "Trashing one entry"). Returns the name used
/// in the trash, and a warning when the final `fsync` of `files/` failed: the entry is in
/// the trash then, only its durability is uncertain.
fn trash_one(
    sys: &Sys,
    can: &Can,
    parent: BorrowedFd,
    name: &OsStr,
    info_path: &[u8],
    date: &str,
) -> Result<(OsString, Option<String>), TrashFail> {
    for k in 1..=10_000u32 {
        let n = OsString::from_vec(candidate(name.as_bytes(), k));
        let mut info_name = n.clone();
        info_name.push(INFO_SUFFIX);
        // Reserve the name: info/N.trashinfo with O_EXCL.
        let f = match sys.create_excl("trash.reserve", can.info.as_fd(), &info_name, 0o600) {
            Ok(f) => f,
            Err(Errno::EXIST) => continue,
            Err(e) => return Err(TrashFail::Os("create trash info", e)),
        };
        let guard_dir = can.info.as_fd();
        let mut guard = Unlink::new(sys, guard_dir, info_name.clone());
        // A name is free only when files/N does not exist either (GIO's rule).
        match sys.stat_at("trash.stat", can.files.as_fd(), &n) {
            Ok(_) => continue,
            Err(Errno::NOENT) => {}
            Err(e) => return Err(TrashFail::Os("stat trash", e)),
        }
        let body = format!(
            "[Trash Info]\nPath={}\nDeletionDate={date}\n",
            encode_path(info_path)
        );
        sys.write_all("trash.write", f.as_fd(), body.as_bytes())
            .map_err(|e| TrashFail::Os("write trash info", e))?;
        sys.fsync("trash.fsync", f.as_fd())
            .map_err(|e| TrashFail::Os("fsync trash info", e))?;
        drop(f);
        sys.fsync("trash.fsync", can.info.as_fd())
            .map_err(|e| TrashFail::Os("fsync trash info directory", e))?;
        match sys.rename("trash.rename", parent, name, can.files.as_fd(), &n, true) {
            Ok(()) => {
                // Moved: from here on the entry is in the trash, whatever happens.
                guard.disarm();
                let warn = sys
                    .fsync("trash.fsync", can.files.as_fd())
                    .err()
                    .map(|e| format!("fsync of the trash's files/ failed ({})", errno_text(e)));
                return Ok((n, warn));
            }
            Err(Errno::EXIST) => continue,
            Err(Errno::XDEV) => return Err(TrashFail::Xdev),
            Err(e) => return Err(TrashFail::Os("move to trash", e)),
        }
    }
    Err(TrashFail::Os("move to trash", Errno::EXIST))
}

/// F8 with the home trash under `$XDG_DATA_HOME` (or `$HOME/.local/share`).
pub fn trash_job(sys: &Sys, ui: &mut dyn Interaction, dir: &Path, names: &[OsString]) -> Report {
    trash_groups(sys, ui, &[Group::new(dir, names.to_vec())])
}

/// F8 with an explicit data home (tests point it at a test directory).
pub fn trash_job_with(
    sys: &Sys,
    ui: &mut dyn Interaction,
    dir: &Path,
    names: &[OsString],
    data_home: Option<&Path>,
) -> Report {
    trash_groups_with(sys, ui, &[Group::new(dir, names.to_vec())], data_home)
}

/// F8 over groups (P2 2.2) with the home trash under `$XDG_DATA_HOME` (or
/// `$HOME/.local/share`).
pub fn trash_groups(sys: &Sys, ui: &mut dyn Interaction, groups: &[Group]) -> Report {
    let dh = data_home(
        std::env::var_os("XDG_DATA_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    );
    trash_groups_with(sys, ui, groups, dh.as_deref())
}

/// F8 over groups with an explicit data home. Groups that reach the same directory are
/// merged, so each directory is handled once; the trash directories found for one group
/// serve the next.
pub fn trash_groups_with(
    sys: &Sys,
    ui: &mut dyn Interaction,
    groups: &[Group],
    data_home: Option<&Path>,
) -> Report {
    let verb = JobVerb::Trash;
    let mut opened = match super::group::open(sys, verb, groups) {
        Ok(o) => o,
        Err(r) => return *r,
    };
    opened.merge();
    let mut canons = Vec::with_capacity(opened.sources.len());
    for s in &opened.sources {
        match fd_path(s.dir.fd()) {
            Ok(p) => canons.push(p),
            Err(e) => {
                return opened.refuse(verb, format!("{}: {}", s.dir.path.display(), errno_text(e)));
            }
        }
    }
    let total: u64 = opened.sources.iter().map(|s| s.names.len() as u64).sum();
    let mut t = Transfer::new(sys, Reporter::new(ui), Report::new(verb));
    opened.report_failed(&mut t.report);
    t.flat = true;
    t.report.planned = total;
    t.files_total = total;
    let date = jiff::Zoned::now().strftime("%Y-%m-%dT%H:%M:%S").to_string();
    // The home trash's domain: the data home, or its nearest existing ancestor, where it
    // will be created.
    let home_dom = data_home.and_then(|d| {
        d.ancestors()
            .find_map(|a| sys.stat_path(a).ok())
            .map(|m| m.id.domain())
    });
    let mut home: Option<Result<Can, String>> = None;
    let mut tops: Vec<TopCans> = Vec::new();

    'job: for (s, canon) in opened.sources.iter().zip(&canons) {
        let src = &s.dir;
        for name in &s.names {
            if t.stopped() || t.cancelled() {
                t.stop();
                break 'job;
            }
            let spath = src.path.join(name);
            let meta = match sys.stat_at("trash.stat", src.fd(), name) {
                Ok(m) => m,
                Err(e) => {
                    t.report.fail(spath, EntryError::os("stat", e).to_string());
                    t.settle(1);
                    continue;
                }
            };
            let node = Node::new(name.clone(), meta);
            if meta.id.mnt_id != src.meta.id.mnt_id {
                t.skip(&node, spath, "mount point");
                continue;
            }
            let dom = meta.id.domain();
            let p = canon.join(name);
            // The home trash when the entry is in its domain; never a fallthrough to the
            // top-directory methods for such an entry.
            let (cans, mut why): (Vec<&Can>, Vec<String>) = if home_dom == Some(dom) {
                let h = home.get_or_insert_with(|| match data_home {
                    Some(d) => home_can(sys, d),
                    None => Err("neither XDG_DATA_HOME nor HOME is set".into()),
                });
                match h {
                    Ok(c) => (vec![&*c], Vec::new()),
                    Err(e) => (Vec::new(), vec![e.clone()]),
                }
            } else {
                let key = (dom, src.meta.id.domain() == dom);
                if !tops.iter().any(|(k, ..)| *k == key) {
                    let (c, w) = top_cans(sys, src, canon, dom);
                    tops.push((key, c, w));
                }
                let (_, c, w) = tops.iter().find(|(k, ..)| *k == key).unwrap();
                (c.iter().collect(), w.clone())
            };
            if cans.iter().any(|c| p.starts_with(&c.root)) {
                t.skip(&node, spath, "already in trash");
                continue;
            }
            if cans.iter().any(|c| c.root.starts_with(&p)) {
                t.fail(&node, spath, "the trash directory is inside this entry");
                continue;
            }
            let mut trashed = false;
            let mut failed = false;
            let mut stop = false;
            'methods: for c in &cans {
                let info_path: Vec<u8> = match &c.top {
                    None => p.as_os_str().as_bytes().to_vec(),
                    Some(top) => p
                        .strip_prefix(top)
                        .map(|r| r.as_os_str().as_bytes().to_vec())
                        .unwrap_or_else(|_| p.as_os_str().as_bytes().to_vec()),
                };
                loop {
                    match trash_one(sys, c, src.fd(), name, &info_path, &date) {
                        Ok((_, warn)) => {
                            if let Some(w) = warn {
                                t.report.notes.push(format!("{}: {w}", spath.display()));
                            }
                            trashed = true;
                            break 'methods;
                        }
                        Err(TrashFail::Xdev) => {
                            why.push(format!("{}: a different filesystem", c.root.display()));
                            continue 'methods;
                        }
                        Err(TrashFail::Os(op, e)) => match t.decide_error(&spath, op, e) {
                            Some(true) => continue,
                            Some(false) => {
                                t.fail(&node, spath.clone(), EntryError::os(op, e).to_string());
                                failed = true;
                                break 'methods;
                            }
                            None => {
                                stop = true;
                                break 'methods;
                            }
                        },
                    }
                }
            }
            if stop {
                t.stop();
                break 'job;
            }
            if trashed {
                t.done(&node);
                continue;
            }
            if failed {
                continue;
            }
            // No usable trash (design 4.10 step 5): Skip, or the typed confirmation for this
            // one entry. A trash failure never deletes on its own (I-6).
            let reason = if why.is_empty() {
                "no trash directory could be used".to_string()
            } else {
                why.join("; ")
            };
            let q = Question::TrashUnavailable {
                path: spath.clone(),
                reason: reason.clone(),
            };
            match t.rep.ask(q) {
                Answer::DeletePermanently => {
                    // The delete counts its own tree; the trash report counts this one entry.
                    let (done, dirs, settled, files, failed) = (
                        t.report.done,
                        t.report.dirs_done,
                        t.report.settled,
                        t.files_done,
                        t.report.failed,
                    );
                    t.flat = false;
                    let one = [Source {
                        dir: src.clone(),
                        names: vec![name.clone()],
                        group: s.group,
                    }];
                    let flow = confirm_and_remove(&mut t, &one, true);
                    t.flat = true;
                    let deleted = t.report.done + t.report.dirs_done > done + dirs
                        && t.report.failed == failed;
                    t.report.done = done + deleted as u64;
                    t.report.dirs_done = dirs;
                    t.report.settled = settled + 1;
                    t.files_done = files + 1;
                    if flow == Flow::Stop {
                        break 'job;
                    }
                    if deleted {
                        t.report.notes.push(format!(
                            "{}: deleted permanently (no usable trash)",
                            spath.display()
                        ));
                    } else if t.report.failed == failed {
                        t.report
                            .skip(spath, "not deleted: the confirmation was not given");
                    }
                }
                Answer::Cancel => {
                    t.stop();
                    break 'job;
                }
                _ => t.skip(&node, spath, format!("no usable trash: {reason}")),
            }
        }
    }
    t.report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gio_path_encoding_round_trip() {
        assert_eq!(encode_path(b"/home/u/a b.txt"), "/home/u/a%20b.txt");
        assert_eq!(encode_path(b"x\ny"), "x%0Ay");
        assert_eq!(encode_path(b"\xff%"), "%FF%25");
        assert_eq!(encode_path("ø".as_bytes()), "%C3%B8");
        assert_eq!(encode_path(b"it's-~_."), "it%27s-~_.");
        let raw = b"/a/new\nline/bad\xfe\xff/%41/";
        assert_eq!(decode_path(&encode_path(raw)).unwrap(), raw);
    }

    #[test]
    fn candidates_fit_name_max() {
        assert_eq!(candidate(b"f", 1), b"f");
        assert_eq!(candidate(b"f", 2), b"f.2");
        let long = [b'x'; 255];
        for k in [1, 2, 10, 1000] {
            let c = candidate(&long, k);
            assert!(c.len() + INFO_SUFFIX.len() <= NAME_MAX, "{k}");
        }
    }

    #[test]
    fn data_home_resolution() {
        let h = Some(OsStr::new("/home/u"));
        assert_eq!(
            data_home(Some(OsStr::new("/x")), h).unwrap(),
            Path::new("/x")
        );
        assert_eq!(
            data_home(Some(OsStr::new("rel")), h).unwrap(),
            Path::new("/home/u/.local/share")
        );
        assert_eq!(
            data_home(None, h).unwrap(),
            Path::new("/home/u/.local/share")
        );
        assert!(data_home(None, None).is_none());
    }
}
