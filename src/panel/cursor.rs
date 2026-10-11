#![forbid(unsafe_code)]
//! The sort, the hidden toggle, the quick filter and the cursor (TDR-1 5, TDR-2 4). A change
//! of the sort, the hidden toggle or the filter recomputes the visible rows from the stored
//! entries (P-4, P-12). The cursor follows its entry by name through every such change.

use super::sort::{SortKey, SortSpec};
use super::{Filter, Panel, Row, split_rel};
use std::time::Instant;

impl Panel {
    /// Re-sorts when entries arrived or changed; keeps the cursor on its name. While a
    /// directory is still loading or a search still runs, batches are sorted in at most
    /// every 150 ms, so a 100k-entry load does not re-sort on every batch (the loop ticks
    /// meanwhile).
    pub fn ensure_sorted(&mut self) {
        if !self.list.dirty {
            return;
        }
        if (self.is_loading() || self.searching())
            && self
                .sorted_at
                .is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(150))
        {
            return;
        }
        self.force_sort();
    }

    pub(super) fn force_sort(&mut self) {
        self.list.resort(self.sort, self.show_hidden, &self.filter);
        self.sorted_at = Some(Instant::now());
        self.recount_marks();
        self.restore_cursor();
    }

    pub fn set_sort(&mut self, key: SortKey) {
        if self.sort.key == key {
            self.sort.reverse = !self.sort.reverse;
        } else {
            self.sort = SortSpec {
                key,
                reverse: false,
            };
        }
        self.remember_cursor();
        self.list.dirty = true;
        self.ensure_sorted();
    }

    pub fn toggle_hidden(&mut self) {
        self.remember_cursor();
        self.show_hidden = !self.show_hidden;
        self.refilter(false);
    }

    /// Sets the quick filter (TDR-2 4) and re-filters at once, from the sort order (P-12). The
    /// cursor goes to the first visible name that starts with the text unless it is on one;
    /// without one, it follows [`Panel::refilter`]. In a results tab, the name is the last
    /// component of the relative path.
    pub fn set_filter(&mut self, text: &[u8]) {
        if self.filter.text() == text {
            return;
        }
        self.remember_cursor();
        self.filter = Filter::new(text);
        let leave_parent = !self.filter.is_empty();
        self.refilter(leave_parent);
        if self.filter.is_empty() || self.filter.is_glob() {
            return;
        }
        let names = |i: u32| split_rel(self.list.name(i)).1;
        if self
            .current_entry()
            .is_some_and(|(i, _)| self.filter.is_prefix_of(names(i)))
        {
            return;
        }
        if let Some(k) = self
            .list
            .visible
            .iter()
            .position(|&i| self.filter.is_prefix_of(names(i)))
        {
            self.cursor_to(self.has_parent() as usize + k);
        }
    }

    /// Recomputes the visible rows after the hidden toggle or the filter changed, and
    /// recounts the visible marks (I-8). The cursor stays on its entry when that entry
    /// stays visible; otherwise it moves to the first visible entry, or to `..` when none
    /// is visible (TDR-2 4). `leave_parent`: a cursor on `..` moves to the first visible
    /// entry too, so a typed filter puts the cursor on its first match.
    fn refilter(&mut self, leave_parent: bool) {
        self.list.refilter(self.show_hidden, &self.filter);
        self.recount_marks();
        let on_parent = self.cursor_name.as_deref() == Some(b"..");
        match self.cursor_row() {
            Some(r) if !(on_parent && leave_parent) => self.cursor = r,
            _ => {
                self.cursor = if self.list.visible.is_empty() {
                    0
                } else {
                    self.has_parent() as usize
                };
                self.remember_cursor();
            }
        }
    }

    pub(crate) fn remember_cursor(&mut self) {
        self.cursor_name = match self.current() {
            Some(Row::Entry(i)) => Some(self.list.name(i).to_vec()),
            Some(Row::Parent) => Some(b"..".to_vec()),
            None => self.cursor_name.take(),
        };
    }

    /// The row of the remembered cursor name, when it is visible.
    fn cursor_row(&self) -> Option<usize> {
        let name = self.cursor_name.as_deref()?;
        if name == b".." {
            return self.has_parent().then_some(0);
        }
        let p = self.has_parent() as usize;
        self.list
            .visible
            .iter()
            .position(|&i| self.list.name(i) == name)
            .map(|pos| p + pos)
    }

    pub(super) fn restore_cursor(&mut self) {
        let rows = self.rows();
        if let Some(r) = self.cursor_row() {
            self.cursor = r;
        } else if self.cursor_name.as_deref() == Some(b"..") {
            self.cursor = 0;
        }
        if self.cursor >= rows {
            self.cursor = rows.saturating_sub(1);
        }
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let rows = self.rows() as isize;
        if rows == 0 {
            return;
        }
        self.cursor = (self.cursor as isize + delta).clamp(0, rows - 1) as usize;
        self.remember_cursor();
    }

    pub fn cursor_to(&mut self, row: usize) {
        self.cursor = row.min(self.rows().saturating_sub(1));
        self.remember_cursor();
    }

    pub fn cursor_to_name(&mut self, name: &[u8]) {
        self.cursor_name = Some(name.to_vec());
        self.restore_cursor();
    }

    /// Keeps the cursor row inside a window of `height` rows.
    pub fn scroll_into_view(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + height {
            self.top = self.cursor + 1 - height;
        }
        let max_top = self.rows().saturating_sub(height);
        self.top = self.top.min(max_top);
    }

    /// The next row after the cursor whose name starts with `prefix` (case-insensitive),
    /// wrapping around.
    pub fn find_prefix(&self, prefix: &[u8], from_next: bool) -> Option<usize> {
        let rows = self.rows();
        let start = self.cursor + from_next as usize;
        (0..rows)
            .map(|k| (start + k) % rows)
            .find(|&r| match self.row(r) {
                Some(Row::Entry(i)) => {
                    let n = self.list.name(i);
                    n.len() >= prefix.len() && n[..prefix.len()].eq_ignore_ascii_case(prefix)
                }
                _ => false,
            })
    }

    /// The name the cursor is on, or was on when the listing went (a released tab).
    pub fn cursor_name(&self) -> Option<&[u8]> {
        self.cursor_name.as_deref()
    }
}
