#![forbid(unsafe_code)]
//! Per-panel tabs (TDR-1 5, 8). A hidden tab is neither watched (NFR-RES) nor
//! kept in memory: it releases its listing and keeps its directory, sort, cursor name and
//! marks, and it reloads when it is shown again. Memory then stays bounded by the two
//! visible panels (P-6), whatever the number of tabs.

use super::{Listing, Panel, Place, Source};
use std::collections::HashSet;

impl Panel {
    /// The tab goes into the background. A navigation in flight counts as done: the tab
    /// shows its place when it comes back, so the place it left goes into the history now.
    /// A results tab keeps its entries: nothing could list them again (TDR-2 2.4); a running
    /// search keeps adding to them, and a re-stat in flight is dropped. An archive tab
    /// releases its index and keeps the place that reopens it through the cache (TDR-3 2.2); a
    /// scan in flight stops. A remote tab releases its session and keeps the place that
    /// reopens it through the pool (TDR-3 2.2, 5.7).
    pub fn release(&mut self) {
        if let Some(l) = &self.loading {
            l.stop_scan(None);
        }
        let left = self
            .loading
            .as_mut()
            .and_then(|l| Some((l.stash.take()?, l.prev.take()?)));
        if let Some((record, prev)) = left {
            self.record_left(record, prev);
        }
        self.stale = false;
        let reopen = match (self.remote(), self.archive_place()) {
            (Some(v), _) => Some(v.place()),
            (None, Some((archive, inner))) => Some(Place::Archive {
                archive: archive.to_path_buf(),
                key: self
                    .archive()
                    .map_or(crate::provider::StatKey::default(), |v| v.key),
                inner: inner.clone(),
            }),
            (None, None) => None,
        };
        if let Some(place) = reopen {
            self.remember_cursor();
            self.reopen = Some(place);
            self.source = Source::Dir;
            self.list = Listing::default();
            self.loading = None;
            self.generation += 1;
            self.place_generation = self.generation;
            self.free = None;
            self.unshown = (0, false);
            self.released = true;
            return;
        }
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
        // Late results of a load in flight are dropped by generation, a directory size too.
        self.generation += 1;
        self.place_generation = self.generation;
        self.free = None;
        self.released = true;
    }
}
