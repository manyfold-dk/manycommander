#![forbid(unsafe_code)]
//! Make directory (F7, design 4.9).
//!
//! The input may contain `/` and creates missing parents (`a/b/c`). Every component is
//! validated: not empty, `.` or `..`, and no NUL. `mkdirat` uses mode 0777, so the umask
//! applies. Each level is opened with `O_NOFOLLOW`, so a symlinked parent is not followed.

use super::copy::Dir;
use super::job::{JobVerb, Report};
use super::plan::valid_component;
use super::sys::Sys;
use rustix::fd::{AsFd, OwnedFd};
use rustix::io::Errno;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Splits `a/b/c` into validated components; one trailing `/` is allowed.
pub fn components(input: &OsStr) -> Option<Vec<OsString>> {
    let b = input.as_bytes();
    let b = b.strip_suffix(b"/").unwrap_or(b);
    let comps: Vec<OsString> = b
        .split(|&c| c == b'/')
        .map(|c| OsStr::from_bytes(c).to_owned())
        .collect();
    comps.iter().all(|c| valid_component(c)).then_some(comps)
}

pub fn mkdir_job(sys: &Sys, dir: &Path, input: &OsStr) -> Report {
    let verb = JobVerb::Mkdir;
    let Some(comps) = components(input) else {
        return Report::refused(verb, "not a valid directory name");
    };
    let root = match Dir::open_root(sys, dir) {
        Ok(d) => d,
        Err(e) => return Report::refused(verb, format!("{}: {e}", dir.display())),
    };
    let mut report = Report::new(verb);
    report.planned = 1;
    report.focus = Some(comps[0].clone());
    let path = dir.join(OsStr::from_bytes(input.as_bytes()));
    let mut held: Option<OwnedFd> = None;
    for (i, comp) in comps.iter().enumerate() {
        let last = i + 1 == comps.len();
        let cur = held.as_ref().map(|f| f.as_fd()).unwrap_or(root.fd());
        match sys.mkdir("mkdir", cur, comp, 0o777) {
            Ok(()) => {
                if last {
                    report.done = 1;
                }
            }
            Err(Errno::EXIST) if last => {
                report.skip(path.clone(), "already exists");
            }
            Err(Errno::EXIST) => {}
            Err(e) => {
                report.fail(
                    path.clone(),
                    super::walk::EntryError::os("make directory", e).to_string(),
                );
                break;
            }
        }
        if !last {
            match sys.open_dir("mkdir.open", cur, comp) {
                Ok(f) => held = Some(f),
                Err(e) => {
                    let why = if matches!(e, Errno::LOOP | Errno::NOTDIR) {
                        format!(
                            "{} exists and is not a directory",
                            Path::new(comp).display()
                        )
                    } else {
                        super::walk::EntryError::os("open", e).to_string()
                    };
                    report.fail(path.clone(), why);
                    break;
                }
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation() {
        assert_eq!(components(OsStr::new("a/b/c")).unwrap().len(), 3);
        assert_eq!(components(OsStr::new("a/")).unwrap().len(), 1);
        for bad in ["", ".", "..", "a/../b", "/abs", "a//b", "a\0b"] {
            assert!(components(OsStr::new(bad)).is_none(), "{bad:?}");
        }
    }
}
