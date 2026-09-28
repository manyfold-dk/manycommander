#![forbid(unsafe_code)]
//! Create links (Alt+L, P2 8.1): symbolic (relative or absolute) or hard.
//!
//! The groups are opened like every job's (P2 2.2), and the destination is resolved once
//! with copy's rules: an existing directory to link into, or, for exactly one source name,
//! the new link's path. A symbolic link's target is lexical: the absolute one is the group's
//! `root` joined with its `sub` and the name, the relative one the lexical path from the
//! link's directory to that source path; no symlink is resolved. A hard link is
//! `linkat(srcdir, name, dstdir, newname, 0)`, which links the entry itself, never a symlink
//! target (I-5). A link never replaces anything: `symlinkat` and `linkat` fail atomically
//! with `EEXIST`, which raises the "link exists" question (no Overwrite). A link is created
//! under its final name directly: a name that did not exist cannot show partial content
//! (I-2 holds trivially).

use super::copy::{Dir, Flow, Transfer, resolve_destination};
use super::group::Group;
use super::job::{JobVerb, Report};
use super::plan::valid_component;
use super::question::{Answer, Interaction, Question, Reporter, Side};
use super::sys::{Kind, Sys};
use super::walk::EntryError;
use rustix::io::Errno;
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

/// The link a form asks for (P2 8.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkKind {
    /// A symbolic link whose target is the lexical relative path from its directory.
    #[default]
    Relative,
    /// A symbolic link whose target is the source's lexical absolute path.
    Absolute,
    /// A hard link: another name of the same inode.
    Hard,
}

/// Normalises a path lexically: `.` components are dropped and `..` removes the component
/// before it (it stays at the root). No symlink is resolved and nothing is touched.
pub fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() && !p.is_absolute() {
                    out.push("..");
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// The lexical relative path from the directory `from` to `to`, both lexical absolute
/// paths (P2 8.1): `..` for every component of `from` below the common prefix, then the
/// rest of `to`. `.` when they are the same path.
pub fn relative(from: &Path, to: &Path) -> PathBuf {
    let f: Vec<Component> = from.components().collect();
    let t: Vec<Component> = to.components().collect();
    let common = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..f.len() {
        out.push("..");
    }
    for c in &t[common..] {
        out.push(c);
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// A lexical absolute path: relative paths are taken from the working directory.
fn absolute(p: &Path) -> PathBuf {
    lexical(&std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()))
}

/// Alt+L over groups (P2 8.1).
pub fn link_groups(
    sys: &Sys,
    ui: &mut dyn Interaction,
    groups: &[Group],
    dst: &Path,
    kind: LinkKind,
) -> Report {
    let verb = JobVerb::Link;
    let opened = match super::group::open(sys, verb, groups) {
        Ok(o) => o,
        Err(r) => return *r,
    };
    let names: Vec<OsString> = groups.iter().flat_map(|g| g.names.clone()).collect();
    let (dst, targets) = match resolve_destination(sys, &names, dst) {
        Ok(x) => x,
        Err(e) => return Report::refused(verb, e),
    };
    let link_dir = absolute(&dst.path);
    let mut offsets = Vec::with_capacity(groups.len());
    let mut at = 0;
    for g in groups {
        offsets.push(at);
        at += g.names.len();
    }
    // Groups that reach the same directory (a bind mount) are merged: each entry of one
    // directory is linked once, under the first group's path (P2 2.2).
    let mut seen = HashSet::new();
    let mut work = Vec::new();
    for s in &opened.sources {
        for (k, name) in s.names.iter().enumerate() {
            if seen.insert((s.dir.meta.id.inode(), name.clone())) {
                work.push((s, name, targets[offsets[s.group] + k].clone()));
            }
        }
    }
    let mut t = Transfer::new(sys, Reporter::new(ui), Report::new(verb));
    opened.report_failed(&mut t.report);
    t.report.planned = work.len() as u64;
    t.files_total = work.len() as u64;
    for (s, name, target) in work {
        let source = absolute(&groups[s.group].dir_path().join(name));
        let entry = Entry {
            src: &s.dir,
            name,
            source: &source,
        };
        if link_one(&mut t, &entry, &dst, &link_dir, target, kind) == Flow::Stop {
            break;
        }
    }
    t.report
}

/// One source entry of a link job.
struct Entry<'a> {
    /// Its directory, opened by the group walk.
    src: &'a Dir,
    name: &'a OsStr,
    /// Its lexical absolute path.
    source: &'a Path,
}

/// What to do after `EEXIST`.
enum Exists {
    Skip(String),
    Rename(OsString),
    /// The name is free again: try once more.
    Again,
    Cancel,
}

fn link_one(
    t: &mut Transfer,
    e: &Entry,
    dst: &Dir,
    link_dir: &Path,
    mut target: OsString,
    kind: LinkKind,
) -> Flow {
    if t.stopped() || t.cancelled() {
        return t.stop();
    }
    let sys = t.sys;
    let spath = e.src.path.join(e.name);
    t.set_current(spath.clone());
    // The entry must still be there; a directory cannot be hard-linked.
    let meta = loop {
        match sys.stat_at("link.stat", e.src.fd(), e.name) {
            Ok(m) => break m,
            Err(Errno::NOENT) => return end(t, spath, Err(EntryError::Disappeared.to_string())),
            Err(err) => match t.decide_error(&spath, "stat", err) {
                Some(true) => continue,
                Some(false) => return end(t, spath, Err(EntryError::os("stat", err).to_string())),
                None => return t.stop(),
            },
        }
    };
    if kind == LinkKind::Hard && meta.kind == Kind::Dir {
        return skip(t, spath, "directories cannot be hard-linked");
    }
    let link_target = match kind {
        LinkKind::Absolute => e.source.to_path_buf(),
        LinkKind::Relative => relative(link_dir, e.source),
        LinkKind::Hard => PathBuf::new(),
    };
    loop {
        if t.cancelled() {
            return t.stop();
        }
        let dpath = dst.path.join(&target);
        let made = match kind {
            LinkKind::Hard => sys.link("link.hard", e.src.fd(), e.name, dst.fd(), &target),
            _ => sys.symlink("link.symlink", link_target.as_os_str(), dst.fd(), &target),
        };
        match made {
            Ok(()) => return end(t, spath, Ok(())),
            Err(Errno::EXIST) => match link_exists(t, dst, &target, &dpath) {
                Exists::Skip(why) => return skip(t, spath, why),
                Exists::Rename(n) => target = n,
                Exists::Again => {}
                Exists::Cancel => return t.stop(),
            },
            Err(Errno::XDEV) if kind == LinkKind::Hard => {
                return end(t, spath, Err("hard links cannot cross filesystems".into()));
            }
            Err(err) => match t.decide_error(&dpath, "create link", err) {
                Some(true) => {}
                Some(false) => {
                    return end(
                        t,
                        spath,
                        Err(EntryError::os("create link", err).to_string()),
                    );
                }
                None => return t.stop(),
            },
        }
    }
}

/// The "link exists" question (P2 8.1), with "Skip all" standing for the rest of the job.
fn link_exists(t: &mut Transfer, dst: &Dir, target: &OsStr, dpath: &Path) -> Exists {
    const EXISTS: &str = "the destination exists";
    if t.policy.skip_exists {
        return Exists::Skip(EXISTS.into());
    }
    let existing = match t.sys.stat_at("link.dststat", dst.fd(), target) {
        Ok(m) => Some(Side::of(&m)),
        Err(Errno::NOENT) => return Exists::Again,
        Err(_) => None,
    };
    let q = Question::LinkExists {
        path: dpath.to_path_buf(),
        existing,
    };
    match t.rep.ask(q) {
        Answer::SkipAll => {
            t.policy.skip_exists = true;
            Exists::Skip(EXISTS.into())
        }
        Answer::Rename(n) if valid_component(&n) => Exists::Rename(n),
        Answer::Rename(_) => Exists::Skip("the new name is not valid".into()),
        Answer::Cancel => Exists::Cancel,
        _ => Exists::Skip(EXISTS.into()),
    }
}

/// Ends an entry as done (`Ok`) or failed with the reason.
fn end(t: &mut Transfer, path: PathBuf, r: Result<(), String>) -> Flow {
    match r {
        Ok(()) => t.report.done += 1,
        Err(why) => t.report.fail(path, why),
    }
    t.settle(1);
    t.tick();
    Flow::Continue
}

fn skip(t: &mut Transfer, path: PathBuf, why: impl Into<String>) -> Flow {
    t.report.skip(path, why);
    t.settle(1);
    t.tick();
    Flow::Continue
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_are_lexical() {
        let r = |a: &str, b: &str| relative(Path::new(a), Path::new(b));
        assert_eq!(r("/a/b", "/a/b/c/x"), Path::new("c/x"));
        assert_eq!(r("/a/b/c", "/a/x"), Path::new("../../x"));
        assert_eq!(r("/a/b", "/a/b"), Path::new("."));
        assert_eq!(r("/", "/a"), Path::new("a"));
        assert_eq!(r("/x/y", "/a/b"), Path::new("../../a/b"));
        assert_eq!(r("/a/bc", "/a/b/x"), Path::new("../b/x"));
    }

    #[test]
    fn lexical_normalisation() {
        assert_eq!(lexical(Path::new("/a/./b/../c")), Path::new("/a/c"));
        assert_eq!(lexical(Path::new("/../a")), Path::new("/a"));
        assert_eq!(lexical(Path::new("/a/b/")), Path::new("/a/b"));
    }
}
