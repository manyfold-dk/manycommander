#![forbid(unsafe_code)]
//! Panels (TDR-1 5): listing, sort, selection, cursor, per-panel watcher.
//!
//! A panel holds its directory, the entries (compact, stored once), the sort, the marks
//! (flag bits, carried across a refresh by name), a cursor by name so it survives a
//! refresh, the hidden toggle, the quick filter (TDR-2 4), the load generation and its
//! directory history. A panel never touches the filesystem: loads are
//! [`listing::ListRequest`]s that the runtime runs on listing threads, and late results are
//! dropped by generation.
//!
//! I-8: the rows on screen are what a verb acts on. [`Panel::marked`] and
//! [`Panel::marked_bytes`] count only the visible marks, and [`Panel::selection`] returns
//! the visible marked entries, else the entry under the cursor. A mark on an entry that
//! the hidden toggle or the filter hides keeps its flag and counts again once visible.
//!
//! A panel's [`Source`] is a directory or a search's results (TDR-2 2.4). A results panel's
//! `dir` is the search root and its entry names are paths relative to it, stored in the
//! same arena, so sorting, marks, the filter and rendering work unchanged. It has no `..`
//! row and no watch; a refresh is a re-stat ([`Panel::restat`], TDR-2 5.5). Its history keeps
//! [`Place`]s: directories, and at most [`History::RESULTS`] results places with their
//! entries.
//!
//! An archive or remote panel (TDR-3 2.2) keeps a local `dir`: the directory that holds the
//! archive, or the one the tab showed before it connected. Its history places name what to
//! reopen, not what is open: an archive's path, cache key and inner directory, or a
//! server's address and directory. The history never holds an index or a session.
//!
//! Opening an archive is a navigation like any other (TDR-3 3.3): the rows of the directory
//! the scan shows arrive in batches, `Esc` returns to the previous place and stops the
//! scan, and entering a subdirectory during the scan keeps the scan and its cancel flag.
//! A hidden archive tab releases its index and reopens it through the cache when shown.
//!
//! A remote directory is a navigation too (TDR-3 5.4): its rows arrive one batch per
//! `READDIR` reply, `Esc` stops the listing, and a refresh keeps the rows until the new
//! listing is complete. A hidden remote tab releases its session and reopens its place
//! through the pool when shown (TDR-3 5.7). A lost session keeps the rows and says so.

pub mod entry;
pub mod fuzzy;
pub mod listing;
pub mod sort;
pub mod tabs;
pub mod watch;

mod cursor;
mod filter;
mod history;
mod marks;
mod source;

pub use filter::{Filter, contains_nocase, glob_match, glob_match_nocase, is_glob};
pub use history::{History, Place, Record, Stashed};
pub use source::{ArchiveView, RemoteView, Source};

use crate::find::Search;
use crate::provider::VPath;
use entry::{EKind, Entry, LinkKind, MARKED, SIZED};
use listing::{Alive, ListRequest};
use sort::{Keys, SortKey, SortSpec};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// One directory's entries, sorted and filtered.
#[derive(Default, Clone)]
pub struct Listing {
    pub entries: Vec<Entry>,
    pub names: Vec<u8>,
    keys: Keys,
    /// The sort permutation over `entries`.
    order: Vec<u32>,
    /// `order` without the entries the hidden toggle or the filter hides.
    pub visible: Vec<u32>,
    /// The edits the filter's fuzzy tier needed for the visible rows; 0 when they match
    /// the filter as typed (TDR-2 4).
    pub fuzzy: u8,
    dirty: bool,
    /// The sort order `order` was built for, while no entry changed since.
    sorted_by: Option<SortSpec>,
}

impl std::fmt::Debug for Listing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Listing")
            .field("entries", &self.entries.len())
            .field("sorted_by", &self.sorted_by)
            .finish()
    }
}

impl Listing {
    /// A complete listing with its collation keys built and its order sorted by `spec`,
    /// made on a listing thread for a refresh (P-1): the UI thread swaps it in and only
    /// computes the visible rows. Its visible rows are empty until then.
    pub fn sorted(entries: Vec<Entry>, names: Vec<u8>, spec: SortSpec) -> Listing {
        let mut l = Listing {
            entries,
            names,
            ..Listing::default()
        };
        l.keys.update(&l.entries, &l.names);
        l.order.extend(0..l.entries.len() as u32);
        sort::sort(&mut l.order, &l.entries, &l.names, &l.keys, spec);
        l.sorted_by = Some(spec);
        l
    }

    pub fn append(&mut self, mut entries: Vec<Entry>, names: &[u8]) {
        let base = self.names.len() as u32;
        self.names.extend_from_slice(names);
        for e in &mut entries {
            e.name_off += base;
        }
        self.entries.extend(entries);
        self.dirty = true;
        self.sorted_by = None;
    }

    pub fn name(&self, i: u32) -> &[u8] {
        self.entries[i as usize].name(&self.names)
    }

    fn resort(&mut self, spec: SortSpec, show_hidden: bool, filter: &Filter) {
        self.keys.update(&self.entries, &self.names);
        self.order.clear();
        self.order.extend(0..self.entries.len() as u32);
        sort::sort(
            &mut self.order,
            &self.entries,
            &self.names,
            &self.keys,
            spec,
        );
        self.dirty = false;
        self.refilter(show_hidden, filter);
        self.sorted_by = Some(spec);
    }

    /// Recomputes `visible` from the sort order, without sorting again (P-12). When no
    /// entry matches the filter as typed, the fuzzy tier shows the entries that need the
    /// fewest edits within the filter's budget ([`Filter::distance`]). Not while entries
    /// wait to be sorted in: `order` lacks them, and one of them may match exactly; the
    /// next re-sort decides.
    fn refilter(&mut self, show_hidden: bool, filter: &Filter) {
        self.visible.clear();
        self.fuzzy = 0;
        let (entries, names) = (&self.entries, &self.names);
        let shown = |i: u32| show_hidden || !entries[i as usize].hidden();
        self.visible.extend(
            self.order
                .iter()
                .copied()
                .filter(|&i| shown(i) && filter.matches(entries[i as usize].name(names))),
        );
        if !self.visible.is_empty() || filter.budget() == 0 || self.dirty {
            return;
        }
        let mut best = filter.budget();
        for &i in &self.order {
            if !shown(i) {
                continue;
            }
            if let Some(d) = filter.distance(entries[i as usize].name(names), best) {
                // Not a match as typed, whatever the folding found: an ASCII text compares
                // bytes above, and characters here (the Kelvin sign is a `k`).
                let d = d.max(1);
                if d < best {
                    self.visible.clear();
                    best = d;
                }
                self.visible.push(i);
            }
        }
        if !self.visible.is_empty() {
            self.fuzzy = best;
        }
    }

    /// The index of the entry named `name`.
    pub fn find(&self, name: &[u8]) -> Option<u32> {
        (0..self.entries.len() as u32).find(|&i| self.name(i) == name)
    }

    /// Drops what a re-sort recomputes, for a listing kept in history (P-6b).
    fn shrink(&mut self) {
        self.keys = Keys::default();
        self.order = Vec::new();
        self.visible = Vec::new();
        self.dirty = true;
        self.sorted_by = None;
    }
}

/// A row of the panel: the parent entry `..`, or a listed entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Parent,
    Entry(u32),
}

/// What a load was for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadKind {
    /// Rows appear as batches arrive; `Esc` returns to the previous directory.
    Navigate,
    /// The current rows stay until the new listing is complete.
    Refresh,
}

pub struct Loading {
    pub kind: LoadKind,
    pub started: Instant,
    pub alive: Alive,
    /// What Esc or a failure returns to.
    prev: Option<Prev>,
    staging: Listing,
    /// Where the place this navigation left goes in the history when it completes (TDR-2
    /// 2.4): a failed or cancelled navigation returns to that place and records nothing,
    /// and a results place's entries are never held twice.
    stash: Option<Record>,
    /// A re-stat: the number of results its request copied. Results appended after it
    /// are not in the re-stat and are kept when it completes (TDR-2 E-28).
    copied: Option<usize>,
    /// The archive a navigation opens, and the directory it shows first, until its index
    /// arrives (TDR-3 3.3).
    archive: Option<(PathBuf, VPath)>,
    /// The scan this load waits for: set when the panel leaves it (P-20).
    cancel: Option<Arc<AtomicBool>>,
    /// A remote listing's cancel flag: set when the panel leaves the load (TDR-3 5.4).
    stop: Option<Arc<AtomicBool>>,
}

impl Loading {
    fn new(kind: LoadKind, alive: Alive) -> Loading {
        Loading {
            kind,
            started: Instant::now(),
            alive,
            prev: None,
            staging: Listing::default(),
            stash: None,
            copied: None,
            archive: None,
            cancel: None,
            stop: None,
        }
    }

    /// Stops the scan this load waits for, unless `keep` carries it on, and a remote
    /// listing, which nothing carries on.
    fn stop_scan(&self, keep: Option<&Arc<AtomicBool>>) {
        if let Some(c) = &self.cancel
            && !keep.is_some_and(|k| Arc::ptr_eq(k, c))
        {
            c.store(true, Ordering::SeqCst);
        }
        if let Some(s) = &self.stop {
            s.store(true, Ordering::SeqCst);
        }
    }
}

/// What a navigation opens besides its place (TDR-3 3.3): the archive whose index is on its
/// way, and the scan it waits for; or a remote listing's cancel flag (TDR-3 5.4).
#[derive(Default)]
struct Opening {
    archive: Option<(PathBuf, VPath)>,
    cancel: Option<Arc<AtomicBool>>,
    stop: Option<Arc<AtomicBool>>,
}

/// The place a navigation left: its source, directory, listing, cursor name and filter.
/// The panel has not changed directory until the load completes, so a return restores the
/// filter.
struct Prev {
    source: Source,
    dir: PathBuf,
    list: Listing,
    cursor: Option<Vec<u8>>,
    filter: Filter,
}

pub struct Panel {
    /// A directory, or a search's results (TDR-2 2.4).
    pub source: Source,
    /// The directory, or a results panel's search root.
    pub dir: PathBuf,
    pub list: Listing,
    pub sort: SortSpec,
    pub show_hidden: bool,
    /// Row index (row 0 is `..` when the directory has a parent).
    pub cursor: usize,
    /// First visible row.
    pub top: usize,
    cursor_name: Option<Vec<u8>>,
    pub generation: u64,
    /// The generation of the navigation that brought the panel to the place on screen; a
    /// refresh keeps the place, so a directory size asked for since then still applies.
    place_generation: u64,
    pub loading: Option<Loading>,
    /// The watcher saw a change while a refresh was in flight: that refresh may have read
    /// the directory before it, so one more follows its completion.
    pub stale: bool,
    pub message: Option<String>,
    pub free: Option<(u64, u64)>,
    pub history: History,
    /// The quick filter (TDR-2 4); cleared when the panel changes directory.
    pub filter: Filter,
    /// The visible marked entries and their bytes (I-8).
    pub marked: usize,
    pub marked_bytes: u64,
    /// Set once the first listing completed (for the first-full-frame timestamp).
    pub loaded_once: bool,
    /// When the listing was last sorted; during a load, re-sorts are spaced out.
    sorted_at: Option<Instant>,
    /// Marks of a released (hidden) tab, applied again when it is listed.
    pub(crate) saved_marks: HashSet<Vec<u8>>,
    /// The tab released its listing and must reload when shown.
    pub released: bool,
    /// A released archive or remote tab's place: showing the tab reopens it (TDR-3 2.2).
    pub reopen: Option<Place>,
    /// What a remote listing did not show (TDR-3 5.4): names with `/` or NUL from the server,
    /// and whether it stopped at the entry cap.
    pub unshown: (u64, bool),
    pub slot: usize,
}

impl Panel {
    pub fn new(slot: usize, dir: PathBuf) -> Panel {
        Panel {
            source: Source::Dir,
            dir,
            list: Listing::default(),
            sort: SortSpec::default(),
            show_hidden: true,
            cursor: 0,
            top: 0,
            cursor_name: None,
            generation: 0,
            place_generation: 0,
            loading: None,
            stale: false,
            message: None,
            free: None,
            history: History::default(),
            filter: Filter::default(),
            marked: 0,
            marked_bytes: 0,
            loaded_once: false,
            sorted_at: None,
            saved_marks: HashSet::new(),
            released: false,
            reopen: None,
            unshown: (0, false),
            slot,
        }
    }

    /// A results panel for `search` (TDR-2 2.4): its `dir` is the search root; results arrive
    /// through [`Panel::append_results`].
    pub fn results(slot: usize, search: Arc<Search>) -> Panel {
        let mut p = Panel::new(slot, search.spec.root.clone());
        p.source = Source::Results(search);
        p.loaded_once = true;
        p
    }

    /// Whether row 0 is `..`: a directory with a parent. A results panel has no `..` row.
    /// An archive or remote panel always has one: at the archive root or at `/` on the
    /// server, `..` returns to the panel's local `dir` (TDR-3 2.2).
    pub fn has_parent(&self) -> bool {
        match self.source {
            Source::Dir => self.dir.parent().is_some(),
            Source::Results(_) => false,
            Source::Archive(_) | Source::Remote(_) => true,
        }
    }

    pub fn rows(&self) -> usize {
        self.has_parent() as usize + self.list.visible.len()
    }

    pub fn row(&self, r: usize) -> Option<Row> {
        let p = self.has_parent() as usize;
        if r < p {
            return Some(Row::Parent);
        }
        self.list.visible.get(r - p).map(|&i| Row::Entry(i))
    }

    pub fn current(&self) -> Option<Row> {
        self.row(self.cursor)
    }

    pub fn current_entry(&self) -> Option<(u32, &Entry)> {
        match self.current()? {
            Row::Entry(i) => Some((i, &self.list.entries[i as usize])),
            Row::Parent => None,
        }
    }

    pub fn current_name(&self) -> Option<&[u8]> {
        self.current_entry().map(|(i, _)| self.list.name(i))
    }

    /// The generation of the listing on screen while no load replaces it. Compare marks
    /// apply only to the listing they were computed from (TDR-2 7): every load, and a
    /// released tab, starts a new generation.
    pub fn listing_generation(&self) -> Option<u64> {
        self.loading.is_none().then_some(self.generation)
    }

    pub fn is_loading(&self) -> bool {
        self.loading
            .as_ref()
            .is_some_and(|l| l.kind == LoadKind::Navigate)
    }

    // ---- loads ---------------------------------------------------------------------------

    fn req(&self, fallback: bool) -> ListRequest {
        ListRequest {
            slot: self.slot,
            generation: self.generation,
            dir: self.dir.clone(),
            ancestor_fallback: fallback,
            sort: None,
        }
    }

    /// Starts a load of `dir`; the cursor goes to `cursor_to` when it is listed.
    pub fn navigate(
        &mut self,
        dir: PathBuf,
        cursor_to: Option<Vec<u8>>,
        alive: Alive,
    ) -> ListRequest {
        self.navigate_with(dir, cursor_to, alive, false)
    }

    /// `fallback`: list the nearest existing ancestor if `dir` is gone.
    pub fn navigate_with(
        &mut self,
        dir: PathBuf,
        cursor_to: Option<Vec<u8>>,
        alive: Alive,
        fallback: bool,
    ) -> ListRequest {
        let record = if fallback { Record::No } else { Record::New };
        self.navigate_full(dir, cursor_to, alive, fallback, record)
    }

    /// `record`: where the place left behind goes in the history once the navigation
    /// completes; a failure or Esc returns to it instead, and records nothing.
    pub fn navigate_full(
        &mut self,
        dir: PathBuf,
        cursor_to: Option<Vec<u8>>,
        alive: Alive,
        fallback: bool,
        record: Record,
    ) -> ListRequest {
        self.begin(
            dir,
            Source::Dir,
            cursor_to,
            alive,
            record,
            Opening::default(),
        );
        self.req(fallback)
    }

    /// Starts a navigation to `source` in the local `dir`. `open.archive`: the archive a
    /// load opens, until its index arrives; `open.cancel`: the scan the load waits for. A
    /// scan the load in flight waited for stops, unless the new load carries it on
    /// (TDR-3 3.3).
    fn begin(
        &mut self,
        dir: PathBuf,
        source: Source,
        cursor_to: Option<Vec<u8>>,
        alive: Alive,
        record: Record,
        open: Opening,
    ) {
        let Opening {
            archive,
            cancel,
            stop,
        } = open;
        // Entering a server lands on another place even in the same local `dir` (TDR-3 2.2).
        let to_place = matches!(source, Source::Remote(_));
        // What Esc or a failure returns to: the listing on screen, or, when a navigation
        // is still in flight, the one that navigation would have returned to.
        let prev = match self.loading.take() {
            Some(l) => {
                l.stop_scan(cancel.as_ref());
                match l.prev {
                    Some(p) => p,
                    None => self.take_prev(),
                }
            }
            None => self.take_prev(),
        };
        let from_results = matches!(prev.source, Source::Results(_));
        // Leaving an archive or a server lands in a directory even when it is the panel's
        // own local `dir` (TDR-3 2.2).
        let from_place = !matches!(prev.source, Source::Dir);
        let elsewhere = dir != prev.dir || from_place || to_place || archive.is_some();
        // A new navigation records a place only when it leaves one: not a reload of the
        // directory on screen, nor the first load of a tab.
        let stash = match record {
            Record::No => None,
            Record::New if !from_results && !(self.loaded_once && elsewhere) => None,
            r => Some(r),
        };
        // A new directory, or leaving a results tab, an archive or a server, drops the
        // filter (TDR-2 4); a reload of the same directory keeps it.
        if elsewhere {
            self.filter = Filter::default();
        }
        self.source = source;
        self.dir = dir;
        self.list = Listing::default();
        self.generation += 1;
        self.place_generation = self.generation;
        self.stale = false;
        self.loading = Some(Loading {
            prev: Some(prev),
            stash,
            archive,
            cancel,
            stop,
            ..Loading::new(LoadKind::Navigate, alive)
        });
        self.message = None;
        self.unshown = (0, false);
        self.sorted_at = None;
        self.cursor = 0;
        self.top = 0;
        self.cursor_name = cursor_to;
        self.marked = 0;
        self.marked_bytes = 0;
    }

    /// The listing on screen, as a navigation leaves it.
    fn take_prev(&mut self) -> Prev {
        Prev {
            source: std::mem::take(&mut self.source),
            dir: self.dir.clone(),
            list: std::mem::take(&mut self.list),
            cursor: self.cursor_name.clone(),
            filter: self.filter.clone(),
        }
    }

    /// Re-reads the directory; the current rows stay until the new listing is complete.
    pub fn refresh(&mut self, alive: Alive) -> ListRequest {
        if self.is_loading() {
            // A navigation already reads the directory: restart it.
            let prev = self.loading.as_mut().and_then(|l| l.prev.take());
            self.generation += 1;
            let stash = self.loading.as_ref().and_then(|l| l.stash);
            self.list = Listing::default();
            self.loading = Some(Loading {
                prev,
                stash,
                ..Loading::new(LoadKind::Navigate, alive)
            });
            return self.req(false);
        }
        self.generation += 1;
        self.loading = Some(Loading::new(LoadKind::Refresh, alive));
        // This refresh reads the directory after every change seen so far.
        self.stale = false;
        // The listing thread sorts the new listing for this order (P-1).
        ListRequest {
            sort: Some(self.sort),
            ..self.req(false)
        }
    }

    /// `Esc` during a load: back to the previous directory at once. Returns the abandoned
    /// load's liveness, so the caller can count stuck threads.
    pub fn cancel_load(&mut self) -> Option<(PathBuf, Alive)> {
        let blocked = self.blocked_path();
        let l = self.loading.take()?;
        l.stop_scan(None);
        self.generation += 1;
        let abandoned = (blocked, l.alive.clone());
        if let Some(p) = l.prev {
            self.go_back(p);
        }
        Some(abandoned)
    }

    pub fn on_batch(&mut self, generation: u64, entries: Vec<Entry>, names: &[u8]) {
        if generation != self.generation {
            return;
        }
        let Some(l) = self.loading.as_mut() else {
            return;
        };
        match l.kind {
            LoadKind::Navigate => self.list.append(entries, names),
            LoadKind::Refresh => l.staging.append(entries, names),
        }
    }

    /// A refresh's complete listing, sorted on the listing thread (P-1): it waits in the
    /// staging listing for `Done`.
    pub fn on_listing(&mut self, generation: u64, listing: Listing) {
        if generation != self.generation {
            return;
        }
        let Some(l) = self.loading.as_mut() else {
            return;
        };
        match l.kind {
            LoadKind::Refresh => l.staging = listing,
            LoadKind::Navigate => self.list.append(listing.entries, &listing.names),
        }
    }

    /// The listing completed. Returns the directory to watch when it changed.
    pub fn on_done(&mut self, generation: u64, dir: PathBuf) -> Option<PathBuf> {
        if generation != self.generation {
            return None;
        }
        let l = self.loading.take()?;
        // A navigation, another directory (an ancestor after a fallback), the first listing,
        // or a hidden tab shown again (it lost its watch when it was hidden).
        let changed =
            dir != self.dir || !self.loaded_once || self.released || l.kind == LoadKind::Navigate;
        // The place this navigation left goes into the history now (TDR-2 2.4).
        if let (Some(record), Some(prev)) = (l.stash, l.prev) {
            self.record_left(record, prev);
        }
        if l.kind == LoadKind::Refresh {
            let names = &self.list.names;
            let mut marked: HashSet<Vec<u8>> = self
                .list
                .entries
                .iter()
                .filter(|e| e.marked())
                .map(|e| e.name(names).to_vec())
                .collect();
            marked.extend(std::mem::take(&mut self.saved_marks));
            // What the new listing cannot know, carried over by name: a directory's
            // computed size (Space), and the target of a symlink that did not change, which
            // the second pass classifies again; until then a link to a directory would sort
            // and show as a file.
            let known: HashMap<Vec<u8>, Entry> = self
                .list
                .entries
                .iter()
                .filter(|e| {
                    (e.kind == EKind::Dir && e.flags & SIZED != 0)
                        || (e.kind == EKind::Symlink && e.link != LinkKind::Unknown)
                })
                .map(|e| (e.name(names).to_vec(), *e))
                .collect();
            let mut fresh = l.staging;
            // A search's batches that arrived while the re-stat ran (queued before its
            // `Done`) are not in the re-stat's copy: they stay (TDR-2 E-28).
            if let Some(n) = l.copied
                && n < self.list.entries.len()
            {
                let mut names = Vec::new();
                let late: Vec<Entry> = self.list.entries[n..]
                    .iter()
                    .map(|e| {
                        let mut e = *e;
                        let name = e.name(&self.list.names);
                        e.name_off = names.len() as u32;
                        names.extend_from_slice(name);
                        e
                    })
                    .collect();
                fresh.append(late, &names);
            }
            self.list = fresh;
            if !marked.is_empty() || !known.is_empty() {
                let by_size = self.sort.key == SortKey::Size;
                let mut moved = false;
                let names = &self.list.names;
                for e in &mut self.list.entries {
                    let name = e.name(names);
                    if marked.contains(name) {
                        e.flags |= MARKED;
                    }
                    let Some(k) = known.get(name) else { continue };
                    match (e.kind, k.kind) {
                        (EKind::Dir, EKind::Dir) => {
                            e.size = k.size;
                            e.flags |= SIZED;
                            moved |= by_size;
                        }
                        (EKind::Symlink, EKind::Symlink)
                            if (e.mtime, e.mtime_ns, e.size) == (k.mtime, k.mtime_ns, k.size) =>
                        {
                            e.link = k.link;
                            moved |= k.link == LinkKind::Dir;
                        }
                        _ => {}
                    }
                }
                if moved {
                    // Sorted on the listing thread without what was carried over.
                    self.list.dirty = true;
                    self.list.sorted_by = None;
                }
            }
        }
        self.dir = dir;
        self.loaded_once = true;
        self.released = false;
        if self.list.sorted_by != Some(self.sort) {
            // Sorted for an order changed meanwhile.
            self.list.dirty = true;
        }
        if self.list.dirty {
            // Batches, or results added during a re-stat: sorted here.
            self.ensure_sorted();
        } else {
            // Sorted for the order in use, on the listing thread (a refresh, P-1) or during
            // the load: only the visible rows are computed here.
            self.list.refilter(self.show_hidden, &self.filter);
            self.sorted_at = Some(Instant::now());
        }
        self.recount_marks();
        self.restore_cursor();
        changed.then(|| self.dir.clone())
    }

    /// The load failed: a navigation returns to where it came from.
    pub fn on_failed(&mut self, generation: u64, error: String) {
        if generation != self.generation {
            return;
        }
        let what = match (self.remote(), self.archive_place()) {
            (Some(v), _) => crate::ui::text::escaped(&v.location()),
            (None, Some((a, _))) => a.display().to_string(),
            (None, None) => self.dir.display().to_string(),
        };
        let Some(l) = self.loading.take() else { return };
        self.stale = false;
        if let Some(p) = l.prev {
            self.message = Some(format!("{what}: {error}"));
            self.go_back(p);
        } else {
            self.message = Some(error);
        }
    }

    /// Returns to the place a navigation left (Esc, failure), with its filter.
    fn go_back(&mut self, p: Prev) {
        self.source = p.source;
        self.dir = p.dir;
        self.list = p.list;
        self.cursor_name = p.cursor;
        self.filter = p.filter;
        // Late results of the load left behind are dropped, a directory size too.
        self.generation += 1;
        self.place_generation = self.generation;
        // The sort order and the hidden toggle may have changed during the load.
        if self.list.dirty || self.list.sorted_by != Some(self.sort) {
            self.list.resort(self.sort, self.show_hidden, &self.filter);
            self.sorted_at = Some(Instant::now());
        } else {
            self.list.refilter(self.show_hidden, &self.filter);
        }
        self.recount_marks();
        self.restore_cursor();
    }

    pub fn on_links(&mut self, generation: u64, kinds: &[(u32, LinkKind)]) {
        if generation != self.generation {
            return;
        }
        let list = match self.loading.as_mut() {
            Some(l) if l.kind == LoadKind::Refresh => &mut l.staging,
            _ => &mut self.list,
        };
        for &(i, k) in kinds {
            if let Some(e) = list.entries.get_mut(i as usize) {
                e.link = k;
            }
        }
        // Only a symlink to a directory moves: it sorts with the directories.
        if kinds.iter().any(|&(_, k)| k == LinkKind::Dir) {
            list.dirty = true;
            list.sorted_by = None;
        }
    }

    /// Whether a directory size asked for at `generation` is for the place on screen: a
    /// refresh since then keeps the place, a navigation or `Esc` leaves it.
    pub fn accepts_size(&self, generation: u64) -> bool {
        (self.place_generation..=self.generation).contains(&generation)
    }

    /// A directory's computed size (Space), for the entry of that name.
    pub fn on_dir_size(&mut self, name: &OsStr, bytes: Option<u64>) {
        if let Some(i) = self.list.find(name.as_bytes()) {
            let e = &mut self.list.entries[i as usize];
            if let Some(b) = bytes {
                e.size = b;
                e.flags |= SIZED;
            }
            if e.marked() {
                self.recount_marks();
            }
            if self.sort.key == SortKey::Size {
                self.list.dirty = true;
            }
        }
    }

    pub fn path_of(&self, name: &[u8]) -> PathBuf {
        self.dir.join(OsStr::from_bytes(name))
    }

    pub fn dir_name(&self) -> Option<Vec<u8>> {
        self.dir.file_name().map(|n| n.as_bytes().to_vec())
    }
}

/// A listed name split at its last `/`: the relative directory (`a/b` of `a/b/c`; empty in
/// a directory panel) and the name in it (a results tab's relative path, TDR-2 2.4).
pub fn split_rel(name: &[u8]) -> (&[u8], &[u8]) {
    match name.iter().rposition(|&c| c == b'/') {
        Some(i) => (&name[..i], &name[i + 1..]),
        None => (&name[..0], name),
    }
}

/// Joins a user-typed path to `base` lexically: absolute paths replace it; `..` and `.`
/// components are resolved without touching the filesystem.
pub fn join_lexical(base: &Path, p: &Path) -> PathBuf {
    let mut out = if p.is_absolute() {
        PathBuf::from("/")
    } else {
        base.to_path_buf()
    };
    for c in p.components() {
        match c {
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {}
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::Normal(n) => out.push(n),
        }
    }
    out
}

#[cfg(test)]
mod tests;
