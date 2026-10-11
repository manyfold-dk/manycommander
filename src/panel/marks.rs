#![forbid(unsafe_code)]
//! Marks and the selection (TDR-1 5, I-8). A mark is a flag on its entry. The counts and the
//! selection take only the visible marks: a verb acts on the visible marked entries, else on
//! the entry under the cursor. A results tab's selection leaves out the results below another
//! selected result and forms job groups by directory (TDR-2 2.2).

use super::entry::{EKind, MARKED};
use super::{Panel, Row, glob_match};
use crate::fsops::group::Group;
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

impl Panel {
    /// Counts the visible marks (I-8): a mark on a hidden or filtered-out entry keeps its
    /// flag but does not count.
    pub(super) fn recount_marks(&mut self) {
        self.marked = 0;
        self.marked_bytes = 0;
        for &i in &self.list.visible {
            let e = &self.list.entries[i as usize];
            if e.marked() {
                self.marked += 1;
                self.marked_bytes = self.marked_bytes.saturating_add(e.size);
            }
        }
    }

    /// Marks exactly the entries `indices` (compare, TDR-2 7): every earlier mark goes, also
    /// on invisible entries and those a released tab saved. Indices out of range are
    /// ignored.
    pub fn replace_marks(&mut self, indices: &[u32]) {
        for e in &mut self.list.entries {
            e.flags &= !MARKED;
        }
        self.saved_marks.clear();
        for &i in indices {
            if let Some(e) = self.list.entries.get_mut(i as usize) {
                e.flags |= MARKED;
            }
        }
        self.recount_marks();
    }

    /// Only for visible entries: the counts are of visible marks (I-8).
    pub(super) fn set_mark(&mut self, i: u32, on: bool) {
        let e = &mut self.list.entries[i as usize];
        if e.marked() == on {
            return;
        }
        let size = e.size;
        if on {
            e.flags |= MARKED;
            self.marked += 1;
            // Archive and server sizes are declared, not measured: the sum saturates.
            self.marked_bytes = self.marked_bytes.saturating_add(size);
        } else {
            e.flags &= !MARKED;
            self.marked -= 1;
            if self.marked_bytes == u64::MAX {
                // A saturated sum cannot be undone by a subtraction.
                self.recount_marks();
            } else {
                self.marked_bytes -= size;
            }
        }
    }

    /// Insert / Shift+Down: toggle the mark under the cursor, move down.
    pub fn toggle_mark(&mut self, advance: bool) {
        if let Some(Row::Entry(i)) = self.current() {
            let on = !self.list.entries[i as usize].marked();
            self.set_mark(i, on);
        }
        if advance {
            self.move_cursor(1);
        }
    }

    pub fn mark_all(&mut self) {
        let vis = self.list.visible.clone();
        for i in vis {
            self.set_mark(i, true);
        }
    }

    pub fn invert_marks(&mut self) {
        let vis = self.list.visible.clone();
        for i in vis {
            let on = !self.list.entries[i as usize].marked();
            self.set_mark(i, on);
        }
    }

    /// Marks (or unmarks) every visible entry whose name matches `pattern`.
    pub fn mark_glob(&mut self, pattern: &[u8], on: bool) {
        let vis = self.list.visible.clone();
        for i in vis {
            if glob_match(pattern, self.list.name(i)) {
                self.set_mark(i, on);
            }
        }
    }

    /// The entries a verb acts on (I-8): the visible marked entries; when no visible entry
    /// is marked, the entry under the cursor; nothing on `..`.
    fn selected(&self) -> Vec<u32> {
        if self.marked > 0 {
            return self
                .list
                .visible
                .iter()
                .copied()
                .filter(|&i| self.list.entries[i as usize].marked())
                .collect();
        }
        self.current_entry()
            .map(|(i, _)| vec![i])
            .unwrap_or_default()
    }

    /// The names a verb acts on (I-8), see [`Panel::selected`].
    pub fn selection(&self) -> Vec<OsString> {
        self.selected()
            .into_iter()
            .map(|i| OsStr::from_bytes(self.list.name(i)).to_owned())
            .collect()
    }

    /// The selection as job groups (TDR-2 2.2), empty when nothing is selected. A directory
    /// panel's selection is one group in its directory. A name with `/` (a results tab's
    /// relative path, TDR-2 2.4) is split at its last `/`, and names that share a directory
    /// form one group. A result below another selected result (`a/sub/x` with `a`) is left
    /// out: it goes with its ancestor.
    pub fn selection_groups(&self) -> Vec<Group> {
        Group::from_relative(&self.dir, drop_nested(self.selection()))
    }

    /// The listing indices a verb acts on (I-8), in display order, without the results
    /// below another selected result (TDR-2 2.2, as [`Panel::selection_groups`]): what the
    /// multi-rename tool lists (TDR-2 6.1).
    pub fn selection_indices(&self) -> Vec<u32> {
        let sel = self.selected();
        if !sel.iter().any(|&i| self.list.name(i).contains(&b'/')) {
            return sel;
        }
        let all: HashSet<&[u8]> = sel.iter().map(|&i| self.list.name(i)).collect();
        sel.iter()
            .copied()
            .filter(|&i| !nested(self.list.name(i), &all))
            .collect()
    }

    /// The selected entries' kinds, for confirmations ("N symbolic links are copied as
    /// links").
    pub fn selection_kinds(&self) -> Vec<EKind> {
        self.selected()
            .into_iter()
            .map(|i| self.list.entries[i as usize].kind)
            .collect()
    }
}

/// Leaves out the names below another name of the list: `a/sub/x` goes when `a` is there.
/// A directory panel's names have no `/`, so they all stay.
fn drop_nested(names: Vec<OsString>) -> Vec<OsString> {
    if !names.iter().any(|n| n.as_bytes().contains(&b'/')) {
        return names;
    }
    let all: HashSet<&[u8]> = names.iter().map(|n| n.as_bytes()).collect();
    let keep: Vec<bool> = names.iter().map(|n| !nested(n.as_bytes(), &all)).collect();
    names
        .into_iter()
        .zip(keep)
        .filter_map(|(n, k)| k.then_some(n))
        .collect()
}

/// Whether a directory above `name` (a prefix ending before one of its `/`) is in `all`.
fn nested(name: &[u8], all: &HashSet<&[u8]>) -> bool {
    name.iter()
        .enumerate()
        .any(|(i, &c)| c == b'/' && all.contains(&name[..i]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_results_go_with_their_selected_ancestor() {
        let os = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            drop_nested(os(&["a", "a/sub/x", "ab/y", "b/c", "b/c/d", "a/z"])),
            os(&["a", "ab/y", "b/c"])
        );
        assert_eq!(drop_nested(os(&["x", "y"])), os(&["x", "y"]));
        assert_eq!(drop_nested(os(&["a/b", "a/c"])), os(&["a/b", "a/c"]));
    }
}
