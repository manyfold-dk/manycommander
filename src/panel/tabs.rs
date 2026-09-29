#![forbid(unsafe_code)]
//! Per-panel tabs (M2, design section 8). A hidden tab is neither watched (NFR-RES) nor
//! kept in memory: it releases its listing and keeps its directory, sort, cursor name and
//! marks, and it reloads when it is shown again. Memory then stays bounded by the two
//! visible panels (P-6), whatever the number of tabs.

use super::{Listing, Panel, Place, Source};
use std::collections::HashSet;

impl Panel {
    /// The tab goes into the background. A results tab keeps its entries: nothing could
    /// list them again (P2 2.4); a running search keeps adding to them, and a re-stat in
    /// flight is dropped. An archive tab releases its index and keeps the place that
    /// reopens it through the cache (P3 2.2); a scan in flight stops. A remote tab releases
    /// its session and keeps the place that reopens it through the pool (P3 2.2, 5.7).
    pub fn release(&mut self) {
        if let Some(l) = self.loading.take() {
            l.stop_scan(None);
            self.loading = Some(l);
        }
        if let Some(v) = self.remote() {
            let place = v.place();
            self.remember_cursor();
            self.reopen = Some(place);
            self.source = Source::Dir;
            self.list = Listing::default();
            self.loading = None;
            self.generation += 1;
            self.free = None;
            self.unshown = (0, false);
            self.released = true;
            return;
        }
        if let Some((archive, inner)) = self.archive_place() {
            let place = Place::Archive {
                archive: archive.to_path_buf(),
                key: self
                    .archive()
                    .map_or(crate::provider::StatKey::default(), |v| v.key),
                inner: inner.clone(),
            };
            self.remember_cursor();
            self.reopen = Some(place);
            self.source = Source::Dir;
            self.list = Listing::default();
            self.loading = None;
            self.generation += 1;
            self.free = None;
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
        // Late results of a load in flight are dropped by generation.
        self.generation += 1;
        self.free = None;
        self.released = true;
    }
}
