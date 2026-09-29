#![forbid(unsafe_code)]
//! The multi-rename tool's wiring (P2 6): `Ctrl+M` builds it from the active panel's
//! selection (I-8; a results tab's selection grouped by directory, P2 5.4), every change
//! recomputes its preview, `Enter` starts the rename job and `Ctrl+Z` the undo of the last
//! one (P2 6.5). Like every dialog it makes no filesystem syscall (P-1): the tool works on
//! what the panel lists.

use super::App;
use super::event::Effect;
use crate::fsops::group::{Group, Root};
use crate::fsops::job::JobSpec;
use crate::fsops::sys::Ts;
use crate::rename::{Directory, Entry};
use crate::ui::dialog::Dialog;
use crate::ui::multirename::RenameTool;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

/// A listed name split at its last `/`: the relative directory (empty in a directory panel)
/// and the name in it (P2 2.4).
fn split_rel(rel: &[u8]) -> (&[u8], &[u8]) {
    match rel.iter().rposition(|&c| c == b'/') {
        Some(i) => (&rel[..i], &rel[i + 1..]),
        None => (&rel[..0], rel),
    }
}

impl App {
    /// `Ctrl+M`: the multi-rename tool for the selection (P2 6.1). One group per directory
    /// of the selection, in the order the directories first appear, as
    /// [`Group::from_relative`] builds them; the entries keep the panel order.
    pub(super) fn rename_tool(&mut self) -> Vec<Effect> {
        let p = self.panel();
        let sel = p.selection_indices();
        if sel.is_empty() {
            return Vec::new();
        }
        let root_name = p
            .dir
            .file_name()
            .map(|n| n.as_bytes().to_vec())
            .unwrap_or_default();
        let mut index: HashMap<&[u8], usize> = HashMap::new();
        let mut groups: Vec<Group> = Vec::new();
        let mut dirs: Vec<Directory> = Vec::new();
        let mut entries: Vec<Entry> = Vec::with_capacity(sel.len());
        for &i in &sel {
            let (dir, leaf) = split_rel(p.list.name(i));
            let g = *index.entry(dir).or_insert_with(|| {
                let sub: Vec<OsString> = if dir.is_empty() {
                    Vec::new()
                } else {
                    dir.split(|&c| c == b'/')
                        .map(|c| OsString::from_vec(c.to_vec()))
                        .collect()
                };
                let name = sub
                    .last()
                    .map(|c| c.as_bytes().to_vec())
                    .unwrap_or_else(|| root_name.clone());
                groups.push(Group {
                    root: Root::Local(p.dir.clone()),
                    sub,
                    names: Vec::new(),
                });
                dirs.push(Directory {
                    name,
                    others: HashSet::new(),
                });
                groups.len() - 1
            });
            groups[g].names.push(OsString::from_vec(leaf.to_vec()));
            let e = &p.list.entries[i as usize];
            entries.push(Entry {
                name: leaf.to_vec(),
                mtime: Ts {
                    sec: e.mtime,
                    nsec: e.mtime_ns,
                },
                dir: g,
            });
        }
        // The other names the panel lists in those directories, hidden and filtered-out
        // ones included: they exist (P2 6.4, advisory).
        let selected: HashSet<u32> = sel.iter().copied().collect();
        for i in 0..p.list.entries.len() as u32 {
            if selected.contains(&i) {
                continue;
            }
            let (dir, leaf) = split_rel(p.list.name(i));
            if let Some(&g) = index.get(dir) {
                dirs[g].others.insert(leaf.to_vec());
            }
        }
        let undo = self
            .rename_undo
            .as_ref()
            .map(|r| r.iter().map(|d| d.entries.len()).sum());
        let tool = RenameTool::new(entries, dirs, groups, self.tz.clone(), undo);
        self.dialog = Some(Dialog::Rename(Box::new(tool)));
        Vec::new()
    }

    /// `Enter` in the tool: the rename job, or the first error and the tool stays open
    /// (P2 6.1, 6.4).
    pub(super) fn submit_rename(&mut self) -> Vec<Effect> {
        let Some(Dialog::Rename(t)) = self.dialog.as_mut() else {
            return Vec::new();
        };
        let blocked = if self.job.is_some() {
            Some("a job is running".to_string())
        } else {
            t.blocked()
        };
        if let Some(e) = blocked {
            t.form.error = Some(e);
            return Vec::new();
        }
        let spec = JobSpec::Rename {
            groups: t.groups.clone(),
            renames: t.renames(),
        };
        self.dialog = None;
        self.start_job(spec)
    }

    /// `Ctrl+Z` in the tool: the undo of the last multi-rename of this session (P2 6.5).
    /// The record is used up by it, whatever the undo job's outcome.
    pub(super) fn undo_rename(&mut self) -> Vec<Effect> {
        let Some(Dialog::Rename(t)) = self.dialog.as_mut() else {
            return Vec::new();
        };
        if self.job.is_some() {
            t.form.error = Some("a job is running".into());
            return Vec::new();
        }
        let Some(record) = self.rename_undo.take() else {
            t.form.error = Some("nothing to undo: no multi-rename in this session".into());
            return Vec::new();
        };
        self.dialog = None;
        self.start_job(JobSpec::UndoRename { record })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_names_split_at_the_last_slash() {
        assert_eq!(split_rel(b"a/b/c.txt"), (&b"a/b"[..], &b"c.txt"[..]));
        assert_eq!(split_rel(b"c"), (&b""[..], &b"c"[..]));
    }
}
