#![forbid(unsafe_code)]
//! Loads and refreshes (TDR-1 3.1, 5): navigating a tab to a directory, refreshing a
//! directory, a results tab (TDR-2 5.5), an archive (TDR-3 3.2) or a server directory
//! (TDR-3 5.4), and the listing threads' messages. A load or a size walk that the app stops
//! waiting for while its thread still runs is abandoned, and at [`MAX_ABANDONED`] no new
//! one starts (TDR-1 3.1, TDR-3 2.5). Like the rest of `App` this makes no filesystem
//! syscall (P-1): listing, sizing and `statvfs` run on listing threads.

use super::event::Effect;
use super::{App, MAX_ABANDONED, TOO_MANY_BLOCKED};
use crate::panel::listing::{Alive, ListingMsg};
use crate::panel::{LoadKind, Record};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

impl App {
    /// Loads `dir` into the active tab of `side`, subject to the stuck-load limits.
    pub(super) fn load(
        &mut self,
        side: usize,
        dir: PathBuf,
        cursor_to: Option<Vec<u8>>,
        fallback: bool,
    ) -> Vec<Effect> {
        let record = if fallback { Record::No } else { Record::New };
        self.load_ex(side, dir, cursor_to, fallback, record)
    }

    /// `record`: where the place left goes in the history.
    pub(super) fn load_ex(
        &mut self,
        side: usize,
        dir: PathBuf,
        cursor_to: Option<Vec<u8>>,
        fallback: bool,
        record: Record,
    ) -> Vec<Effect> {
        self.abandoned.retain(|(_, a)| a.is_running());
        if self.abandoned.iter().any(|(d, _)| *d == dir) {
            self.warn(format!(
                "{}: previous load of this directory is still blocked",
                dir.display()
            ));
            return Vec::new();
        }
        if self.at_abandon_cap() {
            self.warn(TOO_MANY_BLOCKED);
            return Vec::new();
        }
        self.abandon_load(side);
        if side == self.active {
            // The filter line edits the filter of the directory on screen.
            self.filter_line = None;
        }
        // Leaving a results tab's results stops its search (TDR-2 5.4).
        self.leave_results(side);
        let p = self.sides[side].panel_mut();
        let alive = Alive::running();
        let req = p.navigate_full(dir, cursor_to, alive.clone(), fallback, record);
        // The user's navigations are visits; the app's own fallback loads are not.
        self.mark_visit(req.slot, !fallback);
        vec![Effect::List(req, alive)]
    }

    /// Refreshes the active tab of `side`: a directory is listed again, a results tab is
    /// re-stated (TDR-2 5.5). A load still in flight is replaced; while its thread runs it
    /// counts as abandoned, and at [`MAX_ABANDONED`] the refresh is refused (TDR-1 3.1).
    pub(super) fn refresh_slot(&mut self, side: usize) -> Vec<Effect> {
        let p = self.sides[side].panel();
        // An archive re-lists from its index after a key check (TDR-3 3.2); a server re-lists
        // through its session (TDR-3 5.4). Its local `dir` is not what it shows.
        if p.archive().is_some() || p.archive_loading() {
            return self.refresh_archive(side, crate::archive::Check::Refresh);
        }
        if p.remote().is_some() {
            return self.refresh_remote(side);
        }
        // A results tab is not re-stated while its search can still add to it, also after
        // a cancel, until its threads have recorded their totals: the re-stat's copy would
        // miss the batches still to come (TDR-2 E-28).
        if p.is_results() && p.search_pending() {
            return Vec::new();
        }
        if !self.replace_load(side) {
            return Vec::new();
        }
        let p = self.sides[side].panel_mut();
        let alive = Alive::running();
        if p.is_results() {
            let req = p.restat(alive.clone());
            return vec![Effect::Restat(req, alive)];
        }
        let req = p.refresh(alive.clone());
        vec![Effect::List(req, alive)]
    }

    /// A remote panel's refresh (TDR-3 5.4): the rows stay until the new listing is complete;
    /// nothing while its session is lost (TDR-3 5.7).
    fn refresh_remote(&mut self, side: usize) -> Vec<Effect> {
        if !self.replace_load(side) {
            return Vec::new();
        }
        let alive = Alive::running();
        match self.sides[side].panel_mut().refresh_remote(alive.clone()) {
            Some(req) => vec![Effect::ListRemote(req, alive)],
            None => Vec::new(),
        }
    }

    pub(super) fn refresh_both(&mut self) -> Vec<Effect> {
        let mut fx = self.refresh_slot(0);
        fx.extend(self.refresh_slot(1));
        fx
    }

    /// Counts a thread the app stops waiting for while it still runs (TDR-1 3.1): a load
    /// left behind, a preview or a size walk. It counts toward [`MAX_ABANDONED`] until it
    /// returns. `path` is what it is blocked on: a load of the same path is refused
    /// meanwhile.
    pub(crate) fn abandon(&mut self, path: PathBuf, alive: Alive) {
        if alive.is_running() && !self.abandoned.iter().any(|(_, a)| *a == alive) {
            self.abandoned.push((path, alive));
        }
    }

    /// The load in flight in the active tab of `side` is left behind, whatever it is (a
    /// navigation, a refresh, a re-stat): another load replaces it, `Esc`, or the tab is
    /// hidden. Its thread is abandoned while it runs.
    pub(super) fn abandon_load(&mut self, side: usize) {
        let p = self.sides[side].panel();
        if let Some(l) = &p.loading {
            let (path, alive) = (p.blocked_path(), l.alive.clone());
            self.abandon(path, alive);
        }
    }

    /// Before a refresh replaces the load in flight in the active tab of `side`: at
    /// [`MAX_ABANDONED`] a running one is not replaced, and the refresh is refused with a
    /// warning (false); otherwise it is abandoned (true).
    fn replace_load(&mut self, side: usize) -> bool {
        let running = self.sides[side]
            .panel()
            .loading
            .as_ref()
            .is_some_and(|l| l.alive.is_running());
        if running && self.at_abandon_cap() {
            self.warn(TOO_MANY_BLOCKED);
            return false;
        }
        self.abandon_load(side);
        true
    }

    /// Whether the abandoned-thread limit is reached (TDR-1 3.1, TDR-3 2.5): no new load, preview
    /// or size walk starts.
    pub fn at_abandon_cap(&mut self) -> bool {
        self.abandoned_threads() >= MAX_ABANDONED
    }

    /// Abandoned threads that still run (TDR-1 3.1, TDR-3 2.5).
    pub fn abandoned_threads(&mut self) -> usize {
        self.abandoned.retain(|(_, a)| a.is_running());
        self.abandoned.len()
    }

    /// Stops the walk that sizes a local directory; a thread that still runs is abandoned.
    /// Returns whether one was still running: `Esc` after a walk ended does what it does
    /// without one.
    pub(super) fn stop_size_walk(&mut self) -> bool {
        let Some(w) = self.size_walk.take() else {
            return false;
        };
        w.cancel.store(true, Ordering::SeqCst);
        let running = w.alive.is_running();
        // A size walk names no path: it never refuses a load of the directory it sizes.
        self.abandon(PathBuf::new(), w.alive);
        running
    }

    pub(super) fn on_listing(&mut self, msg: ListingMsg) -> Vec<Effect> {
        let mut fx = Vec::new();
        match msg {
            ListingMsg::Batch {
                slot,
                generation,
                entries,
                names,
            } => {
                if let Some(p) = self.slot_mut(slot) {
                    p.on_batch(generation, entries, &names);
                }
            }
            ListingMsg::Listing {
                slot,
                generation,
                listing,
            } => {
                if let Some(p) = self.slot_mut(slot) {
                    p.on_listing(generation, *listing);
                }
            }
            ListingMsg::Done {
                slot,
                generation,
                dir,
                elapsed,
            } => {
                tracing::debug!(slot, ms = elapsed.as_secs_f64() * 1000.0, "listing done");
                let visible = self.visible(slot);
                let Some(p) = self.slot_mut(slot) else {
                    return fx;
                };
                let current = p.generation == generation && p.loading.is_some();
                // A completed navigation (not a refresh) is a visit (TDR-2 3.3).
                let navigation = current
                    && p.loading
                        .as_ref()
                        .is_some_and(|l| l.kind == LoadKind::Navigate);
                let done = p.on_done(generation, dir);
                let stale = current && std::mem::take(&mut p.stale);
                // A results tab holds no watch (NFR-RES), nor does an archive or a server
                // (TDR-3 2.6).
                let watched = p.is_directory();
                let place = p.archive().is_some() || p.remote().is_some();
                // Frecency records local directories only (TDR-3 5.1).
                if navigation && !place {
                    let listed = p.dir.clone();
                    self.visited(slot, &listed);
                }
                if let Some(d) = done
                    && visible
                    && watched
                {
                    fx.push(Effect::Watch { slot, dir: Some(d) });
                }
                if navigation && place {
                    fx.push(Effect::Watch { slot, dir: None });
                }
                // The watcher saw a change while this refresh ran: one more refresh.
                if stale
                    && visible
                    && watched
                    && let Some((s, _)) = self.find_slot(slot)
                {
                    fx.extend(self.refresh_slot(s));
                }
            }
            ListingMsg::Failed {
                slot,
                generation,
                dir,
                error,
                gone,
            } => {
                let Some((s, t)) = self.find_slot(slot) else {
                    return fx;
                };
                let p = &mut self.sides[s].tabs[t];
                if generation != p.generation {
                    return fx;
                }
                let refreshing = p
                    .loading
                    .as_ref()
                    .is_some_and(|l| l.kind == LoadKind::Refresh);
                // A server directory that went away keeps its rows and says so; the local
                // ancestor fallback is for local directories.
                let local = p.remote().is_none();
                if gone && refreshing && local {
                    // The current directory was deleted: go to the nearest existing
                    // ancestor.
                    p.loading = None;
                    let parent = dir
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| PathBuf::from("/"));
                    let name = p.dir_name();
                    if t == self.sides[s].active {
                        fx.extend(self.load(s, parent, name, true));
                    }
                } else {
                    let navigation = p
                        .loading
                        .as_ref()
                        .is_some_and(|l| l.kind == LoadKind::Navigate);
                    p.on_failed(generation, error);
                    self.mark_visit(slot, false);
                    // A directory that no longer exists loses its frecency entry; a
                    // bookmark is never dropped (TDR-2 3.1).
                    if gone && navigation && local {
                        self.dirs.forget(&dir);
                    }
                }
            }
            ListingMsg::LinkTargets {
                slot,
                generation,
                kinds,
            } => {
                if let Some(p) = self.slot_mut(slot) {
                    p.on_links(generation, &kinds);
                }
            }
            ListingMsg::FreeSpace {
                slot,
                generation,
                free,
                total,
            } => {
                if let Some(p) = self.slot_mut(slot)
                    && p.generation == generation
                {
                    p.free = Some((free, total));
                }
            }
            ListingMsg::DirSize {
                slot,
                generation,
                name,
                bytes,
            } => {
                // A refresh since the request keeps the place: the size still applies to
                // the entry of that name.
                if let Some(p) = self.slot_mut(slot)
                    && p.accepts_size(generation)
                {
                    p.on_dir_size(&name, bytes);
                }
            }
            ListingMsg::Opened {
                slot,
                generation,
                index,
            } => {
                if let Some(p) = self.slot_mut(slot) {
                    p.on_opened(generation, index);
                }
            }
            ListingMsg::Reset { slot, generation } => {
                if let Some(p) = self.slot_mut(slot) {
                    p.on_reset(generation);
                }
            }
            ListingMsg::Changed {
                slot,
                generation,
                rescan,
            } => fx.extend(self.on_archive_changed(slot, generation, rescan)),
            ListingMsg::Located {
                slot,
                generation,
                dir,
            } => {
                if let Some(p) = self.slot_mut(slot) {
                    p.on_located(generation, dir);
                }
            }
            ListingMsg::Unshown {
                slot,
                generation,
                invalid,
                capped,
            } => {
                if let Some(p) = self.slot_mut(slot) {
                    p.on_unshown(generation, invalid, capped);
                }
            }
        }
        fx
    }
}
