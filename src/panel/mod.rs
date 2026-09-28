#![forbid(unsafe_code)]
//! Panels (design section 5): listing, sort, selection, cursor, per-panel watcher.
//!
//! A panel holds its directory, the entries (compact, stored once), the sort, the marks
//! (flag bits, carried across a refresh by name), a cursor by name so it survives a
//! refresh, the hidden toggle, the quick filter (P2 4), the load generation and its
//! directory history. A panel never touches the filesystem: loads are
//! [`listing::ListRequest`]s that the runtime runs on listing threads, and late results are
//! dropped by generation.
//!
//! I-8: the rows on screen are what a verb acts on. [`Panel::marked`] and
//! [`Panel::marked_bytes`] count only the visible marks, and [`Panel::selection`] returns
//! the visible marked entries, else the entry under the cursor. A mark on an entry that
//! the hidden toggle or the filter hides keeps its flag and counts again once visible.
//!
//! A panel's [`Source`] is a directory or a search's results (P2 2.4). A results panel's
//! `dir` is the search root and its entry names are paths relative to it, stored in the
//! same arena, so sorting, marks, the filter and rendering work unchanged. It has no `..`
//! row and no watch; a refresh is a re-stat ([`Panel::restat`], P2 5.5). Its history keeps
//! [`Place`]s: directories, and at most [`History::RESULTS`] results places with their
//! entries.

pub mod entry;
pub mod listing;
pub mod sort;
pub mod tabs;
pub mod watch;

use crate::find::{RestatRequest, Search};
use crate::fsops::group::Group;
use entry::{EKind, Entry, LinkKind, MARKED, SIZED};
use listing::{Alive, ListRequest};
use sort::{Keys, SortKey, SortSpec};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
    dirty: bool,
}

impl Listing {
    pub fn append(&mut self, mut entries: Vec<Entry>, names: &[u8]) {
        let base = self.names.len() as u32;
        self.names.extend_from_slice(names);
        for e in &mut entries {
            e.name_off += base;
        }
        self.entries.extend(entries);
        self.dirty = true;
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
        self.refilter(show_hidden, filter);
        self.dirty = false;
    }

    /// Recomputes `visible` from the sort order, without sorting again (P-12).
    fn refilter(&mut self, show_hidden: bool, filter: &Filter) {
        self.visible.clear();
        let (entries, names) = (&self.entries, &self.names);
        self.visible.extend(self.order.iter().copied().filter(|&i| {
            let e = &entries[i as usize];
            (show_hidden || !e.hidden()) && filter.matches(e.name(names))
        }));
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
    }
}

/// A row of the panel: the parent entry `..`, or a listed entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Parent,
    Entry(u32),
}

/// Where a panel's entries come from (P2 2.4).
#[derive(Clone, Debug, Default)]
pub enum Source {
    /// A directory (M1).
    #[default]
    Dir,
    /// A search's results below its root, which is the panel's `dir`.
    Results(Arc<Search>),
}

/// A place in a panel's history (P2 2.4).
pub enum Place {
    Dir(PathBuf),
    Results(Box<Stashed>),
}

/// A results place as history keeps it: the search, its entries (re-stated when shown
/// again), the cursor and the filter.
pub struct Stashed {
    pub search: Arc<Search>,
    list: Listing,
    cursor: Option<Vec<u8>>,
    filter: Filter,
}

/// Where a navigation puts the place it leaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Record {
    /// Nowhere: start, restored tabs, a fallback to an ancestor.
    No,
    /// A new navigation: onto the back stack, and the forward stack is cleared.
    New,
    /// History back: onto the forward stack.
    Back,
    /// History forward: onto the back stack; the forward stack stays.
    Forward,
}

#[derive(Default)]
pub struct History {
    back: Vec<Place>,
    forward: Vec<Place>,
}

impl History {
    const CAP: usize = 100;
    /// Only this many results places stay in a panel's history; older ones are dropped
    /// (P2 2.4, P-6).
    pub const RESULTS: usize = 3;

    fn push(&mut self, from: Place) {
        let same = matches!(
            (self.back.last(), &from),
            (Some(Place::Dir(a)), Place::Dir(b)) if a == b
        );
        if !same {
            self.back.push(from);
        }
        self.forward.clear();
        self.trim();
    }

    /// Keeps at most [`History::CAP`] back entries and [`History::RESULTS`] results places;
    /// the oldest go first.
    fn trim(&mut self) {
        if self.back.len() > Self::CAP {
            self.back.remove(0);
        }
        while self.results_places() > Self::RESULTS {
            let is_results = |p: &Place| matches!(p, Place::Results(_));
            if let Some(i) = self.back.iter().position(is_results) {
                self.back.remove(i);
            } else if let Some(i) = self.forward.iter().position(is_results) {
                self.forward.remove(i);
            }
        }
    }

    /// The results places held (at most [`History::RESULTS`]).
    pub fn results_places(&self) -> usize {
        self.back
            .iter()
            .chain(&self.forward)
            .filter(|p| matches!(p, Place::Results(_)))
            .count()
    }
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
    /// A results place left by this navigation goes into history when it completes, so
    /// its entries are never held twice (P2 2.4).
    stash: Option<Record>,
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

/// The quick filter (P2 4). Without `*`, `?` or `[` it matches as an ASCII case-insensitive
/// substring of the name; with one of them it is an ASCII case-insensitive glob over the
/// whole name (the mark-glob matcher). The empty filter matches everything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    text: Vec<u8>,
    /// `text` ASCII-lowercased, so a match folds only the name.
    folded: Vec<u8>,
    glob: bool,
}

impl Filter {
    pub fn new(text: &[u8]) -> Filter {
        Filter {
            text: text.to_vec(),
            folded: text.to_ascii_lowercase(),
            glob: text.iter().any(|c| matches!(c, b'*' | b'?' | b'[')),
        }
    }

    /// What the user typed.
    pub fn text(&self) -> &[u8] {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Whether the filter is a glob over the whole name.
    pub fn is_glob(&self) -> bool {
        self.glob
    }

    /// Whether `name` passes the filter. Allocates nothing (P-12).
    pub fn matches(&self, name: &[u8]) -> bool {
        if self.text.is_empty() {
            true
        } else if self.glob {
            glob_match_nocase(&self.folded, name)
        } else {
            contains_nocase(name, &self.folded)
        }
    }
}

pub struct Panel {
    /// A directory, or a search's results (P2 2.4).
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
    pub loading: Option<Loading>,
    pub message: Option<String>,
    pub free: Option<(u64, u64)>,
    pub history: History,
    /// The quick filter (P2 4); cleared when the panel changes directory.
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
            loading: None,
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
            slot,
        }
    }

    /// A results panel for `search` (P2 2.4): its `dir` is the search root; results arrive
    /// through [`Panel::append_results`].
    pub fn results(slot: usize, search: Arc<Search>) -> Panel {
        let mut p = Panel::new(slot, search.spec.root.clone());
        p.source = Source::Results(search);
        p.loaded_once = true;
        p
    }

    /// Whether row 0 is `..`: a directory with a parent. A results panel has no `..` row.
    pub fn has_parent(&self) -> bool {
        self.is_directory() && self.dir.parent().is_some()
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

    /// A directory panel, not a results tab (P2 2.4); compare needs two.
    pub fn is_directory(&self) -> bool {
        matches!(self.source, Source::Dir)
    }

    /// The search of a results panel.
    pub fn search(&self) -> Option<&Arc<Search>> {
        match &self.source {
            Source::Results(s) => Some(s),
            Source::Dir => None,
        }
    }

    /// A results panel whose search is still running: rows are sorted in at most every
    /// 150 ms, and the loop ticks.
    pub fn searching(&self) -> bool {
        self.search().is_some_and(|s| s.running())
    }

    /// The generation of the listing on screen while no load replaces it. Compare marks
    /// apply only to the listing they were computed from (P2 7): every load, and a
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

    /// `record`: where the place left behind goes in the history. A directory goes there
    /// at once; a results place goes there when the navigation completes (a failure or Esc
    /// returns to it instead).
    pub fn navigate_full(
        &mut self,
        dir: PathBuf,
        cursor_to: Option<Vec<u8>>,
        alive: Alive,
        fallback: bool,
        record: Record,
    ) -> ListRequest {
        let here = self.dir.clone();
        // What Esc or a failure returns to: the listing on screen, or, when a navigation
        // is still in flight, the one that navigation would have returned to.
        let prev = match self.loading.take() {
            Some(Loading { prev: Some(p), .. }) => p,
            _ => Prev {
                source: std::mem::take(&mut self.source),
                dir: self.dir.clone(),
                list: std::mem::take(&mut self.list),
                cursor: self.cursor_name.clone(),
                filter: self.filter.clone(),
            },
        };
        let from_results = matches!(prev.source, Source::Results(_));
        let stash = match record {
            Record::No => None,
            _ if from_results => Some(record),
            Record::New => {
                if self.loaded_once && dir != prev.dir {
                    self.history.push(Place::Dir(prev.dir.clone()));
                }
                None
            }
            Record::Back => {
                self.history.forward.push(Place::Dir(here));
                None
            }
            Record::Forward => {
                self.history.back.push(Place::Dir(here));
                self.history.trim();
                None
            }
        };
        // A new directory, or leaving a results tab, drops the filter (P2 4); a reload of
        // the same directory keeps it.
        if dir != prev.dir || from_results {
            self.filter = Filter::default();
        }
        self.source = Source::Dir;
        self.dir = dir;
        self.list = Listing::default();
        self.generation += 1;
        self.loading = Some(Loading {
            kind: LoadKind::Navigate,
            started: Instant::now(),
            alive,
            prev: Some(prev),
            staging: Listing::default(),
            stash,
        });
        self.message = None;
        self.sorted_at = None;
        self.cursor = 0;
        self.top = 0;
        self.cursor_name = cursor_to;
        self.marked = 0;
        self.marked_bytes = 0;
        self.req(fallback)
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
                kind: LoadKind::Navigate,
                started: Instant::now(),
                alive,
                prev,
                staging: Listing::default(),
                stash,
            });
            return self.req(false);
        }
        self.generation += 1;
        self.loading = Some(Loading {
            kind: LoadKind::Refresh,
            started: Instant::now(),
            alive,
            prev: None,
            staging: Listing::default(),
            stash: None,
        });
        self.req(false)
    }

    /// A results panel's refresh: a re-stat of its entries on a listing thread (P2 5.5).
    /// The rows stay until it completes; marks survive by name.
    pub fn restat(&mut self, alive: Alive) -> RestatRequest {
        self.generation += 1;
        self.loading = Some(Loading {
            kind: LoadKind::Refresh,
            started: Instant::now(),
            alive,
            prev: None,
            staging: Listing::default(),
            stash: None,
        });
        RestatRequest {
            slot: self.slot,
            generation: self.generation,
            root: self.dir.clone(),
            entries: self.list.entries.clone(),
            names: self.list.names.clone(),
        }
    }

    /// Results of the panel's running search (P2 5.3); they are sorted in like listing
    /// batches.
    pub fn append_results(&mut self, entries: Vec<Entry>, names: &[u8]) {
        self.list.append(entries, names);
    }

    /// Shows a results place from history (P2 2.4). The place on screen goes to the back
    /// stack (`Record::Forward`) or the forward stack (`Record::Back`); a load in flight is
    /// dropped and its liveness returned, so the caller can count a stuck thread. The
    /// caller then re-stats the results.
    pub fn show_results(&mut self, s: Stashed, record: Record) -> Option<Alive> {
        let abandoned = self.loading.take().map(|l| l.alive);
        // While the source is still the one on screen, so the rows are counted right.
        self.remember_cursor();
        let here = match std::mem::take(&mut self.source) {
            Source::Results(search) => {
                let mut list = std::mem::take(&mut self.list);
                list.shrink();
                Place::Results(Box::new(Stashed {
                    search,
                    list,
                    cursor: self.cursor_name.clone(),
                    filter: std::mem::take(&mut self.filter),
                }))
            }
            Source::Dir => Place::Dir(self.dir.clone()),
        };
        match record {
            Record::Forward => self.history.back.push(here),
            _ => self.history.forward.push(here),
        }
        self.history.trim();
        self.dir = s.search.spec.root.clone();
        self.source = Source::Results(s.search);
        self.list = s.list;
        self.cursor_name = s.cursor;
        self.filter = s.filter;
        self.generation += 1;
        self.message = None;
        self.loaded_once = true;
        self.released = false;
        self.sorted_at = None;
        self.cursor = 0;
        self.top = 0;
        self.force_sort();
        abandoned
    }

    /// `Esc` during a load: back to the previous directory at once. Returns the abandoned
    /// load's liveness, so the caller can count stuck threads.
    pub fn cancel_load(&mut self) -> Option<(PathBuf, Alive)> {
        let l = self.loading.take()?;
        self.generation += 1;
        let abandoned = (self.dir.clone(), l.alive.clone());
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

    /// The listing completed. Returns the directory to watch when it changed.
    pub fn on_done(&mut self, generation: u64, dir: PathBuf) -> Option<PathBuf> {
        if generation != self.generation {
            return None;
        }
        let l = self.loading.take()?;
        let changed = dir != self.dir || !self.loaded_once || l.kind == LoadKind::Navigate;
        // A results place this navigation left goes into history now (P2 2.4).
        if let (Some(record), Some(prev)) = (l.stash, l.prev)
            && let Source::Results(search) = prev.source
        {
            let mut list = prev.list;
            list.shrink();
            let place = Place::Results(Box::new(Stashed {
                search,
                list,
                cursor: prev.cursor,
                filter: prev.filter,
            }));
            match record {
                Record::Back => self.history.forward.push(place),
                Record::Forward => self.history.back.push(place),
                _ => self.history.push(place),
            }
            self.history.trim();
        }
        if l.kind == LoadKind::Refresh {
            let mut marked: HashSet<Vec<u8>> = self
                .list
                .entries
                .iter()
                .filter(|e| e.marked())
                .map(|e| e.name(&self.list.names).to_vec())
                .collect();
            marked.extend(std::mem::take(&mut self.saved_marks));
            self.list = l.staging;
            if !marked.is_empty() {
                let names = &self.list.names;
                for e in &mut self.list.entries {
                    if marked.contains(e.name(names)) {
                        e.flags |= MARKED;
                    }
                }
            }
        }
        self.dir = dir;
        self.loaded_once = true;
        self.released = false;
        self.ensure_sorted();
        self.recount_marks();
        self.restore_cursor();
        changed.then(|| self.dir.clone())
    }

    /// The load failed: a navigation returns to where it came from.
    pub fn on_failed(&mut self, generation: u64, error: String) {
        if generation != self.generation {
            return;
        }
        let Some(l) = self.loading.take() else { return };
        if let Some(p) = l.prev {
            self.message = Some(format!("{}: {error}", self.dir.display()));
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
        // The hidden toggle may have changed during the load.
        self.list.refilter(self.show_hidden, &self.filter);
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
        list.dirty = true;
    }

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

    // ---- sorting and cursor ---------------------------------------------------------------

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

    fn force_sort(&mut self) {
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

    /// Sets the quick filter (P2 4) and re-filters at once, from the sort order (P-12).
    pub fn set_filter(&mut self, text: &[u8]) {
        if self.filter.text() == text {
            return;
        }
        self.remember_cursor();
        self.filter = Filter::new(text);
        let leave_parent = !self.filter.is_empty();
        self.refilter(leave_parent);
    }

    /// Recomputes the visible rows after the hidden toggle or the filter changed, and
    /// recounts the visible marks (I-8). The cursor stays on its entry when that entry
    /// stays visible; otherwise it moves to the first visible entry, or to `..` when none
    /// is visible (P2 4). `leave_parent`: a cursor on `..` moves to the first visible
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

    fn restore_cursor(&mut self) {
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

    // ---- marks ----------------------------------------------------------------------------

    /// Counts the visible marks (I-8): a mark on a hidden or filtered-out entry keeps its
    /// flag but does not count.
    fn recount_marks(&mut self) {
        self.marked = 0;
        self.marked_bytes = 0;
        for &i in &self.list.visible {
            let e = &self.list.entries[i as usize];
            if e.marked() {
                self.marked += 1;
                self.marked_bytes += e.size;
            }
        }
    }

    /// Marks exactly the entries `indices` (compare, P2 7): every earlier mark goes, also
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
    fn set_mark(&mut self, i: u32, on: bool) {
        let e = &mut self.list.entries[i as usize];
        if e.marked() == on {
            return;
        }
        if on {
            e.flags |= MARKED;
            self.marked += 1;
            self.marked_bytes += e.size;
        } else {
            e.flags &= !MARKED;
            self.marked -= 1;
            self.marked_bytes -= e.size;
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

    /// The selection as job groups (P2 2.2), empty when nothing is selected. A directory
    /// panel's selection is one group in its directory. A name with `/` (a results tab's
    /// relative path, P2 2.4) is split at its last `/`, and names that share a directory
    /// form one group. A result below another selected result (`a/sub/x` with `a`) is left
    /// out: it goes with its ancestor.
    pub fn selection_groups(&self) -> Vec<Group> {
        Group::from_relative(&self.dir, drop_nested(self.selection()))
    }

    /// The selected entries' kinds, for confirmations ("N symbolic links are copied as
    /// links").
    pub fn selection_kinds(&self) -> Vec<EKind> {
        self.selected()
            .into_iter()
            .map(|i| self.list.entries[i as usize].kind)
            .collect()
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

    pub fn path_of(&self, name: &[u8]) -> PathBuf {
        self.dir.join(OsStr::from_bytes(name))
    }

    /// Takes the place history goes back to. The caller navigates there with
    /// `Record::Back` (a directory) or shows it with [`Panel::show_results`], which records
    /// the place on screen.
    pub fn history_back(&mut self) -> Option<Place> {
        self.history.back.pop()
    }

    /// Takes the place history goes forward to; see [`Panel::history_back`].
    pub fn history_forward(&mut self) -> Option<Place> {
        self.history.forward.pop()
    }

    pub fn dir_name(&self) -> Option<Vec<u8>> {
        self.dir.file_name().map(|n| n.as_bytes().to_vec())
    }
}

/// Leaves out the names below another name of the list: `a/sub/x` goes when `a` is there.
/// A directory panel's names have no `/`, so they all stay.
fn drop_nested(names: Vec<OsString>) -> Vec<OsString> {
    if !names.iter().any(|n| n.as_bytes().contains(&b'/')) {
        return names;
    }
    let all: HashSet<&[u8]> = names.iter().map(|n| n.as_bytes()).collect();
    let nested = |n: &[u8]| {
        n.iter()
            .enumerate()
            .any(|(i, &c)| c == b'/' && all.contains(&n[..i]))
    };
    let keep: Vec<bool> = names.iter().map(|n| !nested(n.as_bytes())).collect();
    names
        .into_iter()
        .zip(keep)
        .filter_map(|(n, k)| k.then_some(n))
        .collect()
}

/// `*`, `?` and `[...]` matching on bytes (mark by glob).
pub fn glob_match(p: &[u8], s: &[u8]) -> bool {
    glob(p, s, false)
}

/// [`glob_match`] ignoring ASCII case; `p` must be ASCII-lowercase (the quick filter,
/// P2 4). Only the name's bytes are folded, as they are compared.
pub fn glob_match_nocase(p: &[u8], s: &[u8]) -> bool {
    glob(p, s, true)
}

/// Whether `hay` contains `needle`, ignoring ASCII case; `needle` must be ASCII-lowercase.
/// Allocates nothing (P-12).
pub fn contains_nocase(hay: &[u8], needle: &[u8]) -> bool {
    let Some((&first, rest)) = needle.split_first() else {
        return true;
    };
    if needle.len() > hay.len() {
        return false;
    }
    (0..=hay.len() - needle.len()).any(|i| {
        hay[i].to_ascii_lowercase() == first
            && hay[i + 1..i + needle.len()]
                .iter()
                .zip(rest)
                .all(|(a, b)| a.to_ascii_lowercase() == *b)
    })
}

fn glob(p: &[u8], s: &[u8], fold: bool) -> bool {
    let at = |i: usize| {
        if fold {
            s[i].to_ascii_lowercase()
        } else {
            s[i]
        }
    };
    let (mut pi, mut si) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while si < s.len() {
        if pi < p.len() {
            match p[pi] {
                b'*' => {
                    star = Some(pi);
                    mark = si;
                    pi += 1;
                    continue;
                }
                b'?' => {
                    pi += 1;
                    si += 1;
                    continue;
                }
                b'[' => {
                    if let Some((matched, next)) = class(&p[pi..], at(si))
                        && matched
                    {
                        pi += next;
                        si += 1;
                        continue;
                    }
                }
                c if c == at(si) => {
                    pi += 1;
                    si += 1;
                    continue;
                }
                _ => {}
            }
        }
        if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

/// A `[...]` class at the start of `p`: `(matched, length)`.
fn class(p: &[u8], c: u8) -> Option<(bool, usize)> {
    let end = p.iter().skip(1).position(|&x| x == b']')? + 1;
    let body = &p[1..end];
    let (neg, body) = match body.first() {
        Some(b'!' | b'^') => (true, &body[1..]),
        _ => (false, body),
    };
    let mut m = false;
    let mut i = 0;
    while i < body.len() {
        if i + 2 < body.len() && body[i + 1] == b'-' {
            m |= body[i] <= c && c <= body[i + 2];
            i += 3;
        } else {
            m |= body[i] == c;
            i += 1;
        }
    }
    Some((m != neg, end + 1))
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
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match(b"*", b"anything"));
        assert!(glob_match(b"*.rs", b"main.rs"));
        assert!(!glob_match(b"*.rs", b"main.rc"));
        assert!(glob_match(b"a?c", b"abc"));
        assert!(glob_match(b"[a-c]*", b"beta"));
        assert!(!glob_match(b"[!a-c]*", b"beta"));
        assert!(glob_match(b"*a*b*", b"xxaxxbxx"));
    }

    #[test]
    fn substring_filter_folds_ascii_case_only() {
        assert!(contains_nocase(b"ReadMe.MD", b"readme"));
        assert!(contains_nocase(b"ReadMe.MD", b"me.m"));
        assert!(contains_nocase(b"abc", b""));
        assert!(!contains_nocase(b"ab", b"abc"));
        assert!(!contains_nocase(b"ReadMe.MD", b"xyz"));
        assert!(contains_nocase(b"aaab", b"aab"), "a partial match restarts");
        // Non-ASCII bytes compare exactly: no Unicode case folding.
        assert!(!contains_nocase(
            "\u{c9}t\u{e9}".as_bytes(),
            "\u{e9}t\u{e9}".as_bytes()
        ));
        assert!(contains_nocase(b"bad\xff\xfeUTF8", b"\xff\xfeutf"));
        let f = Filter::new(b"TXT");
        assert!(!f.is_glob());
        assert!(f.matches(b"notes.txt") && f.matches(b"TXT") && !f.matches(b"notes.md"));
        assert!(Filter::default().matches(b"anything"));
        assert!(Filter::new(b" ").matches(b"a b") && !Filter::new(b" ").matches(b"ab"));
    }

    #[test]
    fn glob_filter_matches_the_whole_name_ignoring_case() {
        for pat in [&b"*.RS"[..], b"?ain.rs", b"[l-n]*", b"M*"] {
            let f = Filter::new(pat);
            assert!(f.is_glob(), "{pat:?}");
            assert!(f.matches(b"main.rs"), "{pat:?}");
            assert!(f.matches(b"MAIN.RS"), "{pat:?}");
        }
        let f = Filter::new(b"ma*");
        assert!(!f.matches(b"xmain"), "a glob is anchored at both ends");
        assert!(!Filter::new(b"*.rs").matches(b"main.rsx"));
        assert!(Filter::new(b"[!a-c]*").matches(b"Delta"));
        assert!(!Filter::new(b"[!a-c]*").matches(b"Beta"));
        // The mark glob stays case-sensitive.
        assert!(!glob_match(b"*.RS", b"main.rs"));
    }

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

    fn search(id: u64) -> Arc<Search> {
        Arc::new(Search::new(
            id,
            crate::find::FindSpec {
                root: "/r".into(),
                ..Default::default()
            },
        ))
    }

    fn stashed(id: u64) -> Stashed {
        Stashed {
            search: search(id),
            list: Listing::default(),
            cursor: None,
            filter: Filter::default(),
        }
    }

    #[test]
    fn history_keeps_three_results_places() {
        let mut h = History::default();
        h.push(Place::Dir("/a".into()));
        for id in 1..=5 {
            h.push(Place::Results(Box::new(stashed(id))));
            h.push(Place::Dir(format!("/d{id}").into()));
        }
        assert_eq!(h.results_places(), History::RESULTS);
        let ids: Vec<u64> = h
            .back
            .iter()
            .filter_map(|p| match p {
                Place::Results(s) => Some(s.search.id),
                Place::Dir(_) => None,
            })
            .collect();
        assert_eq!(ids, [3, 4, 5], "the oldest go first");
        assert_eq!(h.back.len(), 1 + 3 + 5, "directories stay");
        // Forward places count too.
        h.forward.push(Place::Results(Box::new(stashed(6))));
        h.trim();
        assert_eq!(h.results_places(), History::RESULTS);
    }

    #[test]
    fn a_results_panel_has_no_parent_row_and_is_not_a_directory() {
        let mut p = Panel::results(3, search(1));
        assert!(!p.is_directory() && !p.has_parent());
        assert_eq!(p.dir, Path::new("/r"));
        assert!(p.searching());
        let m = crate::fsops::sys::Meta {
            kind: crate::fsops::sys::Kind::File,
            ..Default::default()
        };
        let mut names = Vec::new();
        let es = vec![
            Entry::new(&mut names, b"x/b", &m),
            Entry::new(&mut names, b"a", &m),
        ];
        p.append_results(es, &names);
        p.force_sort();
        assert_eq!(p.rows(), 2);
        assert_eq!(p.row(0), Some(Row::Entry(1)));
        assert_eq!(p.current_name(), Some(&b"a"[..]));
    }

    #[test]
    fn leaving_a_results_place_stashes_it_when_the_load_completes() {
        let mut p = Panel::results(3, search(9));
        let m = crate::fsops::sys::Meta::default();
        let mut names = Vec::new();
        p.append_results(vec![Entry::new(&mut names, b"d/f", &m)], &names);
        p.force_sort();
        let req = p.navigate_full(
            "/r/d".into(),
            Some(b"f".to_vec()),
            Alive::running(),
            false,
            Record::New,
        );
        assert!(p.is_directory());
        assert_eq!(
            p.history.results_places(),
            0,
            "not before the load completes"
        );
        // Esc returns to the results.
        p.cancel_load();
        assert!(p.search().is_some_and(|s| s.id == 9));
        assert_eq!(p.list.entries.len(), 1);
        let req2 = p.navigate_full("/r/d".into(), None, Alive::running(), false, Record::New);
        assert!(req2.generation > req.generation);
        p.on_done(req2.generation, "/r/d".into());
        assert_eq!(p.history.results_places(), 1);
        let Some(Place::Results(s)) = p.history_back() else {
            panic!("a results place");
        };
        assert!(p.show_results(*s, Record::Back).is_none());
        assert_eq!(p.search().map(|s| s.id), Some(9));
        assert_eq!(p.current_name(), Some(&b"d/f"[..]));
        assert!(matches!(p.history_forward(), Some(Place::Dir(d)) if d == Path::new("/r/d")));
        // One results place replacing another keeps the first one's cursor.
        let mut names = Vec::new();
        p.append_results(vec![Entry::new(&mut names, b"a", &m)], &names);
        p.force_sort();
        p.cursor_to_name(b"d/f");
        p.show_results(stashed(10), Record::Forward);
        assert_eq!(p.search().map(|s| s.id), Some(10));
        let Some(Place::Results(s)) = p.history_back() else {
            panic!("the first results place");
        };
        assert_eq!(s.cursor.as_deref(), Some(&b"d/f"[..]));
    }

    #[test]
    fn lexical_join() {
        assert_eq!(
            join_lexical(Path::new("/a/b"), Path::new("../c")),
            Path::new("/a/c")
        );
        assert_eq!(
            join_lexical(Path::new("/a/b"), Path::new("/x/./y")),
            Path::new("/x/y")
        );
        assert_eq!(
            join_lexical(Path::new("/"), Path::new("..")),
            Path::new("/")
        );
    }
}
