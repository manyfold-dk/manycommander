#![forbid(unsafe_code)]
//! Per-panel tabs (M2, design section 8). A hidden tab is neither watched (NFR-RES) nor
//! kept in memory: it releases its listing and keeps its directory, sort, cursor name and
//! marks, and it reloads when it is shown again. Memory then stays bounded by the two
//! visible panels (P-6), whatever the number of tabs.

use super::{Listing, Panel};
use std::collections::HashSet;

impl Panel {
    /// The tab goes into the background. A results tab keeps its entries: nothing could
    /// list them again (P2 2.4); a running search keeps adding to them, and a re-stat in
    /// flight is dropped.
    pub fn release(&mut self) {
        if !self.is_directory() {
            self.remember_cursor();
            if self.loading.take().is_some() {
                self.generation += 1;
            }
            self.released = true;
            return;
        }
        let names = &self.list.names;
        let marked: HashSet<Vec<u8>> = self
            .list
            .entries
            .iter()
            .filter(|e| e.marked())
            .map(|e| e.name(names).to_vec())
            .collect();
        self.saved_marks.extend(marked);
        self.remember_cursor();
        self.list = Listing::default();
        self.loading = None;
        // Late results of a load in flight are dropped by generation.
        self.generation += 1;
        self.free = None;
        self.released = true;
    }
}
