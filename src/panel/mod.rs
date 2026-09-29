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
//!
//! An archive or remote panel (P3 2.2) keeps a local `dir`: the directory that holds the
//! archive, or the one the tab showed before it connected. Its history places name what to
//! reopen, not what is open: an archive's path, cache key and inner directory, or a
//! server's address and directory. The history never holds an index or a session.
//!
//! Opening an archive is a navigation like any other (P3 3.3): the rows of the directory
//! the scan shows arrive in batches, `Esc` returns to the previous place and stops the
//! scan, and entering a subdirectory during the scan keeps the scan and its cancel flag.
//! A hidden archive tab releases its index and reopens it through the cache when shown.

pub mod entry;
pub mod listing;
pub mod sort;
pub mod tabs;
pub mod watch;

use crate::archive::{ArchiveIndex, RelistRequest};
use crate::find::{RestatRequest, Search};
use crate::fsops::group::Group;
use crate::provider::{Provider, StatKey, Target, VPath};
use entry::{EKind, Entry, LinkKind, MARKED, SIZED};
use listing::{Alive, ListRequest};
use sort::{Keys, SortKey, SortSpec};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
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
        self.refilter(show_hidden, filter);
        self.dirty = false;
        self.sorted_by = Some(spec);
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
        self.sorted_by = None;
    }
}

/// A row of the panel: the parent entry `..`, or a listed entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Parent,
    Entry(u32),
}

/// Where a panel's entries come from (P2 2.4, P3 2.2).
#[derive(Clone, Debug, Default)]
pub enum Source {
    /// A directory (M1).
    #[default]
    Dir,
    /// A search's results below its root, which is the panel's `dir`.
    Results(Arc<Search>),
    /// An archive, browsed as a read-only directory tree (P3 3.3).
    Archive(ArchiveView),
    /// A directory on a server (P3 5.4).
    Remote(RemoteView),
}

/// An archive panel's source (P3 2.2). The panel's `dir` is the directory that holds the
/// archive.
#[derive(Clone)]
pub struct ArchiveView {
    /// The archive's index; complete once its scan ended (P3 3.2).
    pub index: Arc<ArchiveIndex>,
    /// The archive file.
    pub archive: PathBuf,
    /// The index's cache key, which the history place keeps (P3 3.2).
    pub key: StatKey,
    /// The directory shown, below the archive root.
    pub inner: VPath,
}

impl ArchiveView {
    /// The history place of this view: what reopens it, without the index.
    pub fn place(&self) -> Place {
        Place::Archive {
            archive: self.archive.clone(),
            key: self.key,
            inner: self.inner.clone(),
        }
    }
}

impl std::fmt::Debug for ArchiveView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveView")
            .field("archive", &self.archive)
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

/// A remote panel's source (P3 2.2). The panel's `dir` is the local directory the tab
/// showed before it connected.
#[derive(Clone)]
pub struct RemoteView {
    /// The SFTP session (T6).
    pub session: Arc<dyn Provider>,
    /// The server as typed, which the history place keeps to reconnect (P3 5.7).
    pub target: Target,
    /// The absolute directory on the server.
    pub dir: VPath,
}

impl RemoteView {
    /// The history place of this view: what reconnects to it, without the session.
    pub fn place(&self) -> Place {
        Place::Remote {
            target: self.target.clone(),
            dir: self.dir.clone(),
        }
    }
}

impl std::fmt::Debug for RemoteView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteView")
            .field("target", &self.target)
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

/// A place in a panel's history (P2 2.4, P3 2.2). An archive or remote place names what to
/// reopen, so the history never pins an index or a connection.
pub enum Place {
    Dir(PathBuf),
    Results(Box<Stashed>),
    /// Going back reopens it through the index cache; a changed `key` rescans.
    Archive {
        archive: PathBuf,
        key: StatKey,
        inner: VPath,
    },
    /// Going back reuses the open session for `target`, or reconnects (P3 5.7).
    Remote {
        target: Target,
        dir: VPath,
    },
}

impl Place {
    /// Whether two places are the same place, so the history does not repeat it. Two
    /// results places never are: each holds its own entries.
    fn same(&self, other: &Place) -> bool {
        match (self, other) {
            (Place::Dir(a), Place::Dir(b)) => a == b,
            (
                Place::Archive {
                    archive: a,
                    key: k,
                    inner: i,
                },
                Place::Archive {
                    archive: b,
                    key: l,
                    inner: j,
                },
            ) => a == b && k == l && i == j,
            (Place::Remote { target: a, dir: x }, Place::Remote { target: b, dir: y }) => {
                a == b && x == y
            }
            _ => false,
        }
    }
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
        let same = self.back.last().is_some_and(|last| last.same(&from));
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
    /// A re-stat: the number of results its request copied. Results appended after it
    /// are not in the re-stat and are kept when it completes (E-28).
    copied: Option<usize>,
    /// The archive a navigation opens, and the directory it shows first, until its index
    /// arrives (P3 3.3).
    archive: Option<(PathBuf, VPath)>,
    /// The scan this load waits for: set when the panel leaves it (P-20).
    cancel: Option<Arc<AtomicBool>>,
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
        }
    }

    /// Stops the scan this load waits for, unless `keep` carries it on.
    fn stop_scan(&self, keep: Option<&Arc<AtomicBool>>) {
        if let Some(c) = &self.cancel
            && !keep.is_some_and(|k| Arc::ptr_eq(k, c))
        {
            c.store(true, Ordering::SeqCst);
        }
    }
}

/// What a navigation opens besides its place (P3 3.3): the archive whose index is on its
/// way, and the scan it waits for.
#[derive(Default)]
struct Opening {
    archive: Option<(PathBuf, VPath)>,
    cancel: Option<Arc<AtomicBool>>,
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
    /// A released archive tab's place: showing the tab reopens it (P3 2.2).
    pub reopen: Option<Place>,
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
            reopen: None,
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
    /// An archive or remote panel always has one: at the archive root or at `/` on the
    /// server, `..` returns to the panel's local `dir` (P3 2.2).
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

    /// A local directory panel: not a results tab (P2 2.4), an archive or a server.
    pub fn is_directory(&self) -> bool {
        matches!(self.source, Source::Dir)
    }

    /// A results tab (P2 5.4).
    pub fn is_results(&self) -> bool {
        matches!(self.source, Source::Results(_))
    }

    /// The search of a results panel.
    pub fn search(&self) -> Option<&Arc<Search>> {
        match &self.source {
            Source::Results(s) => Some(s),
            Source::Dir | Source::Archive(_) | Source::Remote(_) => None,
        }
    }

    /// The archive an archive panel shows (P3 2.2).
    pub fn archive(&self) -> Option<&ArchiveView> {
        match &self.source {
            Source::Archive(v) => Some(v),
            _ => None,
        }
    }

    /// The server directory a remote panel shows (P3 2.2).
    pub fn remote(&self) -> Option<&RemoteView> {
        match &self.source {
            Source::Remote(v) => Some(v),
            _ => None,
        }
    }

    /// The history place of the listing on screen: its directory, archive or server
    /// directory. A results tab's place is stashed with its entries instead
    /// ([`Panel::show_results`]); here it is its search root.
    fn place(&self) -> Place {
        match &self.source {
            Source::Archive(v) => v.place(),
            Source::Remote(v) => v.place(),
            Source::Dir | Source::Results(_) => Place::Dir(self.dir.clone()),
        }
    }

    /// A results panel whose search is still running: rows are sorted in at most every
    /// 150 ms, and the loop ticks.
    pub fn searching(&self) -> bool {
        self.search().is_some_and(|s| s.running())
    }

    /// A results panel whose search can still add rows: its threads have not recorded
    /// their totals, whether it runs or was cancelled. No re-stat starts then (E-28).
    pub fn search_pending(&self) -> bool {
        self.search().is_some_and(|s| !s.finished())
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
    /// (P3 3.3).
    fn begin(
        &mut self,
        dir: PathBuf,
        source: Source,
        cursor_to: Option<Vec<u8>>,
        alive: Alive,
        record: Record,
        open: Opening,
    ) {
        let Opening { archive, cancel } = open;
        // The place on screen, or the directory a navigation in flight loads.
        let here = self.place();
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
        // own local `dir` (P3 2.2).
        let from_place = !matches!(prev.source, Source::Dir);
        let stash = match record {
            Record::No => None,
            _ if from_results => Some(record),
            Record::New => {
                if self.loaded_once && (dir != prev.dir || from_place || archive.is_some()) {
                    let left = match &prev.source {
                        Source::Archive(v) => v.place(),
                        Source::Remote(v) => v.place(),
                        Source::Dir | Source::Results(_) => Place::Dir(prev.dir.clone()),
                    };
                    self.history.push(left);
                }
                None
            }
            Record::Back => {
                self.history.forward.push(here);
                None
            }
            Record::Forward => {
                self.history.back.push(here);
                self.history.trim();
                None
            }
        };
        // A new directory, or leaving a results tab, an archive or a server, drops the
        // filter (P2 4); a reload of the same directory keeps it.
        if dir != prev.dir || from_place || archive.is_some() {
            self.filter = Filter::default();
        }
        self.source = source;
        self.dir = dir;
        self.list = Listing::default();
        self.generation += 1;
        self.loading = Some(Loading {
            prev: Some(prev),
            stash,
            archive,
            cancel,
            ..Loading::new(LoadKind::Navigate, alive)
        });
        self.message = None;
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

    /// Opens `archive` and shows its directory `inner` (P3 3.1, 3.3). The panel's local
    /// `dir` becomes the directory that holds the archive; the index arrives with
    /// `Opened`. Returns the load's generation and the scan's cancel flag.
    pub fn navigate_archive(
        &mut self,
        archive: PathBuf,
        inner: VPath,
        cursor_to: Option<Vec<u8>>,
        alive: Alive,
        record: Record,
    ) -> (u64, Arc<AtomicBool>) {
        let dir = archive
            .parent()
            .map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
        let cancel = Arc::new(AtomicBool::new(false));
        let open = Opening {
            archive: Some((archive, inner)),
            cancel: Some(cancel.clone()),
        };
        self.begin(dir, Source::Dir, cursor_to, alive, record, open);
        (self.generation, cancel)
    }

    /// Shows the directory `inner` of the archive on screen (P3 3.3). `scan`: the cancel
    /// flag of the scan still filling the index, which this load carries on. Returns the
    /// load's generation; `None` when the panel shows no archive.
    pub fn navigate_inner(
        &mut self,
        inner: VPath,
        cursor_to: Option<Vec<u8>>,
        alive: Alive,
        scan: Option<Arc<AtomicBool>>,
        record: Record,
    ) -> Option<u64> {
        let view = self.archive()?.clone();
        let dir = self.dir.clone();
        let open = Opening {
            archive: None,
            cancel: scan,
        };
        let source = Source::Archive(ArchiveView { inner, ..view });
        self.begin(dir, source, cursor_to, alive, record, open);
        Some(self.generation)
    }

    /// The scan the load in flight waits for, and its liveness: a navigation inside the
    /// archive carries them on (P3 3.3).
    pub fn scan(&self) -> Option<(Alive, Arc<AtomicBool>)> {
        let l = self.loading.as_ref()?;
        Some((l.alive.clone(), l.cancel.clone()?))
    }

    /// Whether the load in flight opens or scans an archive.
    pub fn archive_loading(&self) -> bool {
        self.loading
            .as_ref()
            .is_some_and(|l| l.archive.is_some() || l.cancel.is_some())
    }

    /// The archive the panel opens or shows, and the directory in it.
    pub fn archive_place(&self) -> Option<(&Path, &VPath)> {
        if let Some((a, i)) = self.loading.as_ref().and_then(|l| l.archive.as_ref()) {
            return Some((a, i));
        }
        self.archive().map(|v| (v.archive.as_path(), &v.inner))
    }

    /// What the title shows (P3 2.2): `archive.zip:/inner/dir` in an archive, else the
    /// directory.
    pub fn location(&self) -> Vec<u8> {
        match self.archive_place() {
            Some((a, i)) => crate::archive::title(a, i),
            None => self.dir.as_os_str().as_bytes().to_vec(),
        }
    }

    /// What a blocked load counts as in the abandoned-thread limit (M1 3.1): the archive a
    /// scan reads, else the directory.
    pub fn blocked_path(&self) -> PathBuf {
        match self.archive_place() {
            Some((a, _)) if self.archive_loading() => a.to_path_buf(),
            _ => self.dir.clone(),
        }
    }

    /// The index of an archive load arrived (P3 3.3): the panel shows the archive now,
    /// while the scan fills it.
    pub fn on_opened(&mut self, generation: u64, index: Arc<ArchiveIndex>) {
        if generation != self.generation {
            return;
        }
        let Some(l) = self.loading.as_mut() else {
            return;
        };
        let Some((archive, inner)) = l.archive.take() else {
            return;
        };
        // The requested path's directory: another path to the same inode shares the index.
        if let Some(d) = archive.parent() {
            self.dir = d.to_path_buf();
        }
        self.source = Source::Archive(ArchiveView {
            key: index.key,
            index,
            archive,
            inner,
        });
    }

    /// A later duplicate replaced a row of the directory the scan shows (P3 3.3): the rows
    /// go, and the scan sends them all again.
    pub fn on_reset(&mut self, generation: u64) {
        if generation != self.generation || !self.is_loading() {
            return;
        }
        self.remember_cursor();
        self.list = Listing::default();
        self.marked = 0;
        self.marked_bytes = 0;
    }

    /// A refresh of an archive panel (P3 3.2): its directory again from the complete
    /// index, sorted on the listing thread; `check` compares the archive's `StatKey`
    /// first. `None` while the scan runs or when the panel shows no archive.
    pub fn refresh_archive(
        &mut self,
        alive: Alive,
        check: crate::archive::Check,
    ) -> Option<RelistRequest> {
        let view = self.archive()?.clone();
        if !view.index.is_complete() || self.archive_loading() {
            return None;
        }
        self.generation += 1;
        self.loading = Some(Loading::new(LoadKind::Refresh, alive));
        Some(RelistRequest {
            slot: self.slot,
            generation: self.generation,
            dir: self.dir.clone(),
            index: view.index,
            inner: view.inner,
            sort: Some(self.sort),
            check,
        })
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
        // The listing thread sorts the new listing for this order (P-1).
        ListRequest {
            sort: Some(self.sort),
            ..self.req(false)
        }
    }

    /// A results panel's refresh: a re-stat of its entries on a listing thread (P2 5.5).
    /// The rows stay until it completes; marks survive by name. Results that arrive
    /// meanwhile are kept too (E-28).
    pub fn restat(&mut self, alive: Alive) -> RestatRequest {
        self.generation += 1;
        self.loading = Some(Loading {
            copied: Some(self.list.entries.len()),
            ..Loading::new(LoadKind::Refresh, alive)
        });
        RestatRequest {
            slot: self.slot,
            generation: self.generation,
            root: self.dir.clone(),
            entries: self.list.entries.clone(),
            names: self.list.names.clone(),
            sort: self.sort,
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
            Source::Archive(v) => v.place(),
            Source::Remote(v) => v.place(),
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
            let mut fresh = l.staging;
            // A search's batches that arrived while the re-stat ran (queued before its
            // `Done`) are not in the re-stat's copy: they stay (E-28).
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
        let what = match self.archive_place() {
            Some((a, _)) => a.to_path_buf(),
            None => self.dir.clone(),
        };
        let Some(l) = self.loading.take() else { return };
        if let Some(p) = l.prev {
            self.message = Some(format!("{}: {error}", what.display()));
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
        // Only a symlink to a directory moves: it sorts with the directories.
        if kinds.iter().any(|&(_, k)| k == LinkKind::Dir) {
            list.dirty = true;
            list.sorted_by = None;
        }
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

    /// The listing indices a verb acts on (I-8), in display order, without the results
    /// below another selected result (P2 2.2, as [`Panel::selection_groups`]): what the
    /// multi-rename tool lists (P2 6.1).
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

    /// The name the cursor is on, or was on when the listing went (a released tab).
    pub fn cursor_name(&self) -> Option<&[u8]> {
        self.cursor_name.as_deref()
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
                _ => None,
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

    /// A place that holds nothing: the history tests count the references to it.
    struct NoPlace;

    impl Provider for NoPlace {
        fn caps(&self) -> crate::provider::Caps {
            crate::provider::Caps::default()
        }
        fn list(
            &self,
            _: &VPath,
            _: &mut dyn FnMut(listing::ListingMsg),
            _: &std::sync::atomic::AtomicBool,
        ) -> Result<(), crate::provider::PlaceError> {
            Err(crate::provider::PlaceError::NotFound)
        }
        fn lstat(&self, _: &VPath) -> Result<crate::fsops::sys::Meta, crate::provider::PlaceError> {
            Err(crate::provider::PlaceError::NotFound)
        }
        fn open_read(
            &self,
            _: &VPath,
            _: &std::sync::atomic::AtomicBool,
        ) -> Result<Box<dyn std::io::Read + Send>, crate::provider::PlaceError> {
            Err(crate::provider::PlaceError::NotFound)
        }
    }

    fn key() -> StatKey {
        StatKey {
            dev: 1,
            ino: 2,
            size: 3,
            mtime: crate::fsops::sys::Ts { sec: 4, nsec: 5 },
            ctime: crate::fsops::sys::Ts { sec: 6, nsec: 7 },
        }
    }

    fn in_archive(p: &mut Panel, index: &Arc<ArchiveIndex>, inner: &[u8]) {
        p.source = Source::Archive(ArchiveView {
            index: index.clone(),
            archive: "/x/a.zip".into(),
            key: key(),
            inner: VPath::parse(inner).unwrap(),
        });
        p.loaded_once = true;
    }

    fn target() -> Target {
        Target {
            user: Some("u".into()),
            host: "h".into(),
            port: None,
        }
    }

    /// P3 2.2: leaving an archive puts what reopens it into the history, never the index; a
    /// history place of an archive or a server is not repeated; `..` leaves either.
    #[test]
    fn archive_places_name_what_to_reopen() {
        let index = Arc::new(ArchiveIndex::detached("/x/a.zip".into(), key()));
        let mut p = Panel::new(0, "/x".into());
        in_archive(&mut p, &index, b"d/e");
        assert!(p.has_parent(), "`..` leaves the archive");
        assert!(!p.is_directory() && !p.is_results() && p.search().is_none());
        assert_eq!(
            p.archive().map(|v| v.inner.to_bytes()),
            Some(b"/d/e".to_vec())
        );
        p.filter = Filter::new(b"q");
        // Leaving for the archive's own directory is a new place, and drops the filter.
        let req = p.navigate_full(
            "/x".into(),
            Some(b"a.zip".to_vec()),
            Alive::running(),
            false,
            Record::New,
        );
        assert!(p.is_directory() && p.filter.is_empty());
        p.on_done(req.generation, "/x".into());
        assert_eq!(Arc::strong_count(&index), 1, "the history pins no index");
        let Some(Place::Archive {
            archive,
            key: k,
            inner,
        }) = p.history_back()
        else {
            panic!("an archive place");
        };
        assert_eq!(
            (archive, k, inner.to_bytes()),
            (PathBuf::from("/x/a.zip"), key(), b"/d/e".to_vec())
        );

        // The same place twice in a row is kept once.
        for _ in 0..2 {
            in_archive(&mut p, &index, b"d");
            let req = p.navigate_full("/y".into(), None, Alive::running(), false, Record::New);
            p.on_done(req.generation, "/y".into());
        }
        let req = p.navigate_full("/z".into(), None, Alive::running(), false, Record::New);
        p.on_done(req.generation, "/z".into());
        in_archive(&mut p, &index, b"d");
        let req = p.navigate_full("/x".into(), None, Alive::running(), false, Record::New);
        p.on_done(req.generation, "/x".into());
        let places: Vec<bool> = p
            .history
            .back
            .iter()
            .map(|pl| matches!(pl, Place::Archive { .. }))
            .collect();
        assert_eq!(places, [true, false, true], "{places:?}");

        // History back from an archive puts it on the forward stack.
        in_archive(&mut p, &index, b"");
        p.navigate_full("/x".into(), None, Alive::running(), false, Record::Back);
        assert!(
            matches!(p.history_forward(), Some(Place::Archive { inner, .. }) if inner.is_root())
        );
        // A results place shown over an archive stashes the archive's place.
        in_archive(&mut p, &index, b"z");
        p.show_results(stashed(1), Record::Forward);
        assert!(matches!(p.history_back(), Some(Place::Archive { .. })));
        assert_eq!(Arc::strong_count(&index), 1, "nothing holds the index now");
    }

    /// P3 2.2, 5.7: a remote place keeps the server as typed and the directory, never the
    /// session.
    #[test]
    fn remote_places_name_what_to_reconnect() {
        let session: Arc<dyn Provider> = Arc::new(NoPlace);
        let mut p = Panel::new(0, "/home/u".into());
        p.source = Source::Remote(RemoteView {
            session: session.clone(),
            target: target(),
            dir: VPath::parse(b"/srv/www").unwrap(),
        });
        p.loaded_once = true;
        assert!(p.has_parent() && p.remote().is_some());
        let req = p.navigate_full("/home/u".into(), None, Alive::running(), false, Record::New);
        p.on_done(req.generation, "/home/u".into());
        assert_eq!(
            Arc::strong_count(&session),
            1,
            "the history pins no session"
        );
        let Some(Place::Remote { target: t, dir }) = p.history_back() else {
            panic!("a remote place");
        };
        assert_eq!((t, dir.to_bytes()), (target(), b"/srv/www".to_vec()));
        assert!(
            !Place::Remote {
                target: target(),
                dir: VPath::root()
            }
            .same(&Place::Dir("/".into()))
        );
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
