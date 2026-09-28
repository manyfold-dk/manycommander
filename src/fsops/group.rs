#![forbid(unsafe_code)]
//! Grouped sources (P2 2.2): every verb takes groups, each one directory and names in it.
//!
//! A directory panel's selection is one group. A results tab (P2 5) holds entries from many
//! directories below one root; its selection is one group per relative directory. A job
//! opens each `root` once, like an M1 panel path (design 4.3), then walks `sub` one
//! component at a time with `O_DIRECTORY | O_NOFOLLOW`: a component that is now a symlink
//! fails the whole group with "type changed" (I-5). No job opens a joined path such as
//! `root/a/b`, because that would follow symlinks in `a` and `b`.

use super::copy::Dir;
use super::job::{JobVerb, Report};
use super::plan::valid_component;
use super::sys::Sys;
use super::walk::{EntryError, open_dir_nofollow};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One directory of a job's sources and the selected names in it (P2 2.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// A panel path, resolved once at job start (design 4.3).
    pub root: PathBuf,
    /// The directory below `root` as single components; empty for a directory panel.
    pub sub: Vec<OsString>,
    /// Single components in that directory.
    pub names: Vec<OsString>,
}

impl Group {
    /// A directory panel's group: `names` in `dir` itself.
    pub fn new(dir: impl Into<PathBuf>, names: Vec<OsString>) -> Group {
        Group {
            root: dir.into(),
            sub: Vec::new(),
            names,
        }
    }

    /// The display path of the group's directory: `root` joined with `sub`. For showing
    /// only; a job never opens it (P2 2.2).
    pub fn dir_path(&self) -> PathBuf {
        let mut p = self.root.clone();
        p.extend(&self.sub);
        p
    }

    /// Groups entry names relative to `root` (`a/b/name`, as a results tab holds them) by
    /// their directory (P2 2.4). Each name is split at its last `/`: the part before it
    /// becomes `sub` (split at every `/`), the part after it the name. A name without `/`
    /// belongs to `root` itself. Groups keep the order in which their directory first
    /// appears, and names keep their order within a group. Nothing is normalised: an empty
    /// or `..` component stays, so the job boundary refuses it (P2 2.2).
    pub fn from_relative(root: &Path, names: impl IntoIterator<Item = OsString>) -> Vec<Group> {
        let mut groups: Vec<Group> = Vec::new();
        let mut index: HashMap<Vec<u8>, usize> = HashMap::new();
        for rel in names {
            let b = rel.as_bytes();
            let (dir, leaf) = match b.iter().rposition(|&c| c == b'/') {
                Some(i) => (&b[..i], &b[i + 1..]),
                None => (&b[..0], b),
            };
            let leaf = OsString::from_vec(leaf.to_vec());
            match index.entry(dir.to_vec()) {
                Entry::Occupied(e) => groups[*e.get()].names.push(leaf),
                Entry::Vacant(e) => {
                    let sub = if b.contains(&b'/') {
                        dir.split(|&c| c == b'/')
                            .map(|c| OsString::from_vec(c.to_vec()))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    e.insert(groups.len());
                    groups.push(Group {
                        root: root.to_path_buf(),
                        sub,
                        names: vec![leaf],
                    });
                }
            }
        }
        groups
    }
}

/// The job-boundary check (P2 2.2): every `sub` component and every name must be a single
/// path component (`valid_component`: not empty, `.` or `..`, without `/` or NUL). `Err`
/// says which one is not, and the job is refused before anything is opened or written.
pub fn validate(groups: &[Group]) -> Result<(), String> {
    for g in groups {
        let bad = |what: &str, c: &OsStr| {
            format!(
                "{what} {:?} in {} is not a single path component",
                Path::new(c).display().to_string(),
                g.dir_path().display()
            )
        };
        if let Some(c) = g.sub.iter().find(|c| !valid_component(c)) {
            return Err(bad("directory", c));
        }
        if let Some(n) = g.names.iter().find(|n| !valid_component(n)) {
            return Err(bad("name", n));
        }
    }
    Ok(())
}

/// A group opened at job start.
pub(crate) struct Source {
    /// The group's directory, reached through the component walk; its display path is
    /// `root` joined with `sub`.
    pub dir: Dir,
    pub names: Vec<OsString>,
    /// The index of the (first) group this source came from.
    pub group: usize,
}

/// The opened groups of a job.
pub(crate) struct Opened {
    /// The groups that opened, in group order.
    pub sources: Vec<Source>,
    /// Every name of a group that could not be opened, with the reason.
    failed: Vec<(PathBuf, String)>,
}

impl Opened {
    /// Reports every name of a group that could not be opened as failed (I-7).
    pub fn report_failed(&self, r: &mut Report) {
        for (path, why) in &self.failed {
            r.fail(path.clone(), why.clone());
        }
    }

    /// Merges groups whose opened directories have the same identity (`(st_dev, st_ino)`,
    /// for example one directory seen through a bind mount), so each directory is handled
    /// once (P2 2.2). The first group keeps its display path; a name it already holds is
    /// not added twice.
    pub fn merge(&mut self) {
        let mut index: HashMap<(u64, u64), usize> = HashMap::new();
        let mut out: Vec<Source> = Vec::with_capacity(self.sources.len());
        for s in std::mem::take(&mut self.sources) {
            match index.entry(s.dir.meta.id.inode()) {
                Entry::Occupied(e) => {
                    let into = &mut out[*e.get()];
                    for n in s.names {
                        if !into.names.contains(&n) {
                            into.names.push(n);
                        }
                    }
                }
                Entry::Vacant(e) => {
                    e.insert(out.len());
                    out.push(s);
                }
            }
        }
        self.sources = out;
    }
}

/// Opens the groups of a job (P2 2.2) after the job-boundary check. Each distinct `root` is
/// opened once, like an M1 panel path; a root that cannot be opened refuses the job, as in
/// M1. Then each group's `sub` is walked from its root's fd, one component at a time with
/// `O_DIRECTORY | O_NOFOLLOW`. A component that fails (a symlink or non-directory now:
/// "type changed") fails that whole group: every name of it is reported failed, and the
/// other groups go on. `Err` is the final report of a refused job.
pub(crate) fn open(sys: &Sys, verb: JobVerb, groups: &[Group]) -> Result<Opened, Box<Report>> {
    validate(groups).map_err(|why| Box::new(Report::refused(verb, why)))?;
    let mut roots: HashMap<&Path, Dir> = HashMap::new();
    let mut sources = Vec::with_capacity(groups.len());
    let mut failed = Vec::new();
    for (i, g) in groups.iter().enumerate() {
        let root = match roots.get(g.root.as_path()) {
            Some(d) => d.clone(),
            None => {
                let d = Dir::open_root(sys, &g.root).map_err(|e| {
                    Box::new(Report::refused(verb, format!("{}: {e}", g.root.display())))
                })?;
                roots.insert(&g.root, d.clone());
                d
            }
        };
        match walk(sys, root, &g.sub) {
            Ok(dir) => sources.push(Source {
                dir,
                names: g.names.clone(),
                group: i,
            }),
            Err(e) => {
                let dir = g.dir_path();
                let why = e.to_string();
                failed.extend(g.names.iter().map(|n| (dir.join(n), why.clone())));
            }
        }
    }
    Ok(Opened { sources, failed })
}

/// Walks `sub` below `root` with `O_DIRECTORY | O_NOFOLLOW`, one component at a time.
fn walk(sys: &Sys, root: Dir, sub: &[OsString]) -> Result<Dir, EntryError> {
    let mut dir = root;
    for c in sub {
        let (fd, meta) = open_dir_nofollow(sys, "group.walk", dir.fd(), c)?;
        dir = Dir {
            fd: Arc::new(fd),
            meta,
            path: dir.path.join(c),
        };
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    #[test]
    fn relative_names_group_by_directory_in_first_appearance_order() {
        let names = ["x", "a/b/y", "a/z", "a/b/w", "v"].map(os);
        let g = Group::from_relative(Path::new("/r"), names);
        assert_eq!(
            g,
            vec![
                Group {
                    root: "/r".into(),
                    sub: vec![],
                    names: vec![os("x"), os("v")]
                },
                Group {
                    root: "/r".into(),
                    sub: vec![os("a"), os("b")],
                    names: vec![os("y"), os("w")]
                },
                Group {
                    root: "/r".into(),
                    sub: vec![os("a")],
                    names: vec![os("z")]
                },
            ]
        );
        assert_eq!(g[1].dir_path(), Path::new("/r/a/b"));
        assert!(Group::from_relative(Path::new("/r"), Vec::new()).is_empty());
    }

    #[test]
    fn odd_relative_names_are_kept_for_the_boundary_to_refuse() {
        let g = Group::from_relative(Path::new("/r"), ["a//b", "/c", "d/"].map(os));
        assert_eq!(g[0].sub, vec![os("a"), os("")]);
        assert_eq!(g[1].sub, vec![os("")]);
        assert_eq!(g[2].names, vec![os("")]);
        for one in g {
            assert!(validate(&[one]).is_err());
        }
    }

    #[test]
    fn validation_names_the_bad_component() {
        let ok = Group {
            root: "/r".into(),
            sub: vec![os("a")],
            names: vec![os("b")],
        };
        assert!(validate(std::slice::from_ref(&ok)).is_ok());
        for (sub, name) in [
            ("..", "b"),
            (".", "b"),
            ("a", ".."),
            ("a", "x/y"),
            ("a", ""),
        ] {
            let g = Group {
                root: "/r".into(),
                sub: vec![os(sub)],
                names: vec![os(name)],
            };
            let e = validate(&[ok.clone(), g]).unwrap_err();
            assert!(e.contains("not a single path component"), "{e}");
        }
        let nul = Group::new("/r", vec![OsString::from_vec(b"a\0b".to_vec())]);
        assert!(validate(&[nul]).is_err());
    }
}
