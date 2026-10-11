#![forbid(unsafe_code)]
//! Panel history (TDR-1 5, TDR-2 2.4, TDR-3 2.2). A navigation records the place it leaves
//! when it completes, or when its tab is hidden; a failed or cancelled one records nothing.
//! A results place keeps its entries, and at most [`History::RESULTS`] of them stay. A
//! directory, archive or server place names what to reopen, so the history never pins an
//! index or a session.

use super::listing::Alive;
use super::{Filter, Listing, Panel, Prev, Source};
use crate::find::Search;
use crate::provider::{StatKey, Target, VPath};
use std::path::PathBuf;
use std::sync::Arc;

/// A place in a panel's history (TDR-2 2.4, TDR-3 2.2). An archive or remote place names what to
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
    /// Going back reuses the open session for `target`, or reconnects (TDR-3 5.7). `home`:
    /// `dir` is below the login directory (a place left before the server resolved it).
    Remote {
        target: Target,
        dir: VPath,
        home: bool,
    },
}

impl Place {
    /// A copy of a place that names what to reopen; `None` for a results place, which holds
    /// its entries.
    pub fn reopen_copy(&self) -> Option<Place> {
        match self {
            Place::Dir(d) => Some(Place::Dir(d.clone())),
            Place::Archive {
                archive,
                key,
                inner,
            } => Some(Place::Archive {
                archive: archive.clone(),
                key: *key,
                inner: inner.clone(),
            }),
            Place::Remote { target, dir, home } => Some(Place::Remote {
                target: target.clone(),
                dir: dir.clone(),
                home: *home,
            }),
            Place::Results(_) => None,
        }
    }

    /// Whether two places are the same place, so the history does not repeat it. Two
    /// results places never are: each holds its own entries.
    pub(super) fn same(&self, other: &Place) -> bool {
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
            (
                Place::Remote {
                    target: a,
                    dir: x,
                    home: h,
                },
                Place::Remote {
                    target: b,
                    dir: y,
                    home: g,
                },
            ) => a == b && x == y && h == g,
            _ => false,
        }
    }
}

/// A results place as history keeps it: the search, its entries (re-stated when shown
/// again), the cursor and the filter.
pub struct Stashed {
    pub search: Arc<Search>,
    pub(super) list: Listing,
    pub(super) cursor: Option<Vec<u8>>,
    pub(super) filter: Filter,
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
    pub(super) back: Vec<Place>,
    pub(super) forward: Vec<Place>,
}

impl History {
    const CAP: usize = 100;
    /// Only this many results places stay in a panel's history; older ones are dropped
    /// (TDR-2 2.4, P-6).
    pub const RESULTS: usize = 3;

    pub(super) fn push(&mut self, from: Place) {
        let same = self.back.last().is_some_and(|last| last.same(&from));
        if !same {
            self.back.push(from);
        }
        self.forward.clear();
        self.trim();
    }

    /// Keeps at most [`History::CAP`] back entries and [`History::RESULTS`] results places;
    /// the oldest go first.
    pub(super) fn trim(&mut self) {
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

impl Panel {
    /// Shows a results place from history (TDR-2 2.4). The place on screen goes to the back
    /// stack (`Record::Forward`) or the forward stack (`Record::Back`); a load in flight is
    /// dropped and its liveness returned, so the caller can count a stuck thread. The
    /// caller then re-stats the results.
    pub fn show_results(&mut self, s: Stashed, record: Record) -> Option<Alive> {
        let abandoned = self.loading.take().map(|l| {
            l.stop_scan(None);
            l.alive
        });
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
        self.place_generation = self.generation;
        self.stale = false;
        self.message = None;
        self.loaded_once = true;
        self.released = false;
        self.sorted_at = None;
        self.cursor = 0;
        self.top = 0;
        self.force_sort();
        abandoned
    }

    /// Puts the place a navigation left into the history, as `record` says (TDR-2 2.4): a
    /// results place with its entries, any other as what reopens it.
    pub(super) fn record_left(&mut self, record: Record, prev: Prev) {
        let place = match prev.source {
            Source::Results(search) => {
                let mut list = prev.list;
                list.shrink();
                Place::Results(Box::new(Stashed {
                    search,
                    list,
                    cursor: prev.cursor,
                    filter: prev.filter,
                }))
            }
            Source::Archive(v) => v.place(),
            Source::Remote(v) => v.place(),
            Source::Dir => Place::Dir(prev.dir),
        };
        match record {
            Record::No => return,
            Record::New => self.history.push(place),
            Record::Back => self.history.forward.push(place),
            Record::Forward => self.history.back.push(place),
        }
        self.history.trim();
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

    /// Puts back a place [`Panel::history_back`] (`back`) or [`Panel::history_forward`]
    /// took, when the move to it was refused.
    pub fn history_return(&mut self, back: bool, place: Place) {
        if back {
            self.history.back.push(place);
        } else {
            self.history.forward.push(place);
        }
    }
}
