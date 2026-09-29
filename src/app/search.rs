#![forbid(unsafe_code)]
//! Find files and the results tab (P2 5): the find form, starting and cancelling searches
//! under the abandoned-search limit (P2 2.3), the search's events, the results tab's keys
//! (P2 5.4) and history moves between directories and results places (P2 2.4). Like the
//! rest of `App` it makes no filesystem syscall (P-1): the search and its re-stats run on
//! other threads, and the UI only reads the search's counters and sets its cancel flag.
//!
//! A search belongs to its results tab. Leaving the tab's results (a navigation, a history
//! move, closing the tab) and starting another search cancel it; the tab keeps what it
//! found and says "(cancelled)".

use super::App;
use super::event::Effect;
use crate::find::{FindMsg, FindSpec, Search};
use crate::panel::entry::EKind;
use crate::panel::{Panel, Place, Record, join_lexical};
use crate::ui::dialog::{Dialog, FormPurpose};
use crate::ui::form::Form;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;

/// The find form's fields (P2 5.1).
pub const FIND_DIR: usize = 0;
pub const FIND_NAME: usize = 1;
pub const FIND_TEXT: usize = 2;
pub const FIND_HIDDEN: usize = 3;
pub const FIND_STAY: usize = 4;
pub const FIND_CASE: usize = 5;

/// At most this many abandoned searches may exist (P2 2.3).
pub const MAX_ABANDONED_SEARCHES: usize = 2;

/// What a results tab says to a key it refuses (P2 5.4).
pub const NOT_IN_RESULTS: &str = "not in search results";

impl App {
    /// Alt+F7: the find form (P2 5.1). It searches the active panel's directory (a results
    /// tab's root) and starts with the panel's hidden toggle.
    pub(super) fn find_form(&mut self) -> Vec<Effect> {
        let p = self.panel();
        let mut form = Form::new("Find files")
            .text("Search in", p.dir.as_os_str().as_bytes())
            .text("Name", b"")
            .text("Containing text", b"")
            .check("Hidden entries", p.show_hidden)
            .check("Stay on this filesystem", true)
            .check("Match case", false)
            .help("Name: part of the name, or a glob (* ? [...]) over the whole name;")
            .help("empty: every name. Text: literal bytes; holes of sparse files are")
            .help("not read.");
        form.focus = FIND_NAME;
        self.dialog = Some(Dialog::Form {
            form,
            purpose: FormPurpose::Find,
        });
        Vec::new()
    }

    /// Starts a search in a new results tab on the active side (P2 5.1). Refused while
    /// [`MAX_ABANDONED_SEARCHES`] cancelled searches are still blocked (P2 2.3). A search
    /// still running is cancelled first; its tab keeps its results.
    pub(super) fn start_find(&mut self, spec: FindSpec) -> Result<Vec<Effect>, String> {
        self.abandoned_finds.retain(|s| s.alive.is_running());
        if self.abandoned_finds.len() >= MAX_ABANDONED_SEARCHES {
            return Err("previous searches are still blocked".into());
        }
        if let Some(s) = self.find.take() {
            self.cancel_search(&s);
        }
        self.next_search += 1;
        let search = Arc::new(Search::new(self.next_search, spec));
        let side = self.active;
        let slot = self.new_slot();
        let cur = self.sides[side].panel();
        let mut p = Panel::results(slot, search.clone());
        p.sort = cur.sort;
        p.show_hidden = cur.show_hidden || search.spec.hidden;
        let mut fx = Vec::new();
        self.hide_tab(side, &mut fx);
        let s = &mut self.sides[side];
        s.tabs.insert(s.active + 1, p);
        s.active += 1;
        self.filter_line = None;
        self.find = Some(search.clone());
        tracing::info!(id = search.id, spec = ?search.spec, "find start");
        fx.push(Effect::Find(search));
        Ok(fx)
    }

    /// Cancels `s` when it still runs. While its threads stay alive (blocked in the
    /// kernel) it counts as abandoned (P2 2.3).
    pub(super) fn cancel_search(&mut self, s: &Arc<Search>) {
        if self.find.as_ref().is_some_and(|f| f.id == s.id) {
            self.find = None;
        }
        if !s.running() {
            return;
        }
        s.cancel();
        if s.alive.is_running() && !self.abandoned_finds.iter().any(|a| a.id == s.id) {
            self.abandoned_finds.push(s.clone());
        }
    }

    /// The results tab on screen on `side` is left: its search stops.
    pub(super) fn leave_results(&mut self, side: usize) {
        if let Some(s) = self.sides[side].panel().search().cloned() {
            self.cancel_search(&s);
        }
    }

    /// `Esc` in a results tab while its search runs (P2 5.3). Returns whether it did.
    pub(super) fn cancel_active_search(&mut self) -> bool {
        match self.panel().search().filter(|s| s.running()).cloned() {
            Some(s) => {
                self.cancel_search(&s);
                self.say("search cancelled");
                true
            }
            None => false,
        }
    }

    /// The panel, on any side and tab, that shows the results of search `id`.
    fn results_panel(&mut self, id: u64) -> Option<&mut Panel> {
        self.sides
            .iter_mut()
            .flat_map(|s| s.tabs.iter_mut())
            .find(|p| p.search().is_some_and(|s| s.id == id))
    }

    /// An event of a search (P2 2.3). Results of a search no tab shows any more are
    /// dropped.
    pub(super) fn on_find(&mut self, m: FindMsg) -> Vec<Effect> {
        match m {
            FindMsg::Batch { id, entries, names } => {
                if let Some(p) = self.results_panel(id) {
                    p.append_results(entries, &names);
                }
            }
            FindMsg::Done { id, stats } => {
                tracing::info!(
                    id,
                    dirs = stats.dirs,
                    results = stats.results,
                    errors = stats.errors,
                    ms = stats.elapsed.as_secs_f64() * 1000.0,
                    "find done"
                );
                if self.find.as_ref().is_some_and(|s| s.id == id) {
                    self.find = None;
                }
                let error = stats.error.clone();
                if let Some(s) = self.results_panel(id).and_then(|p| p.search().cloned()) {
                    s.finish(stats);
                }
                if let Some(e) = error {
                    self.warn(format!("find: {e}"));
                }
            }
        }
        Vec::new()
    }

    /// `Enter` in a results tab (P2 5.4): a directory result opens; any other result is
    /// "Go to file", its directory with the cursor on it. `Alt+Left` returns to the results.
    pub(super) fn enter_result(&mut self) -> Vec<Effect> {
        let p = self.panel();
        let Some((i, e)) = p.current_entry() else {
            return Vec::new();
        };
        let rel = p.list.name(i).to_vec();
        let root = p.dir.clone();
        let side = self.active;
        if e.kind == EKind::Dir {
            let dir = join_lexical(&root, Path::new(OsStr::from_bytes(&rel)));
            return self.load(side, dir, None, false);
        }
        let (dir, leaf) = match rel.iter().rposition(|&c| c == b'/') {
            Some(k) => (&rel[..k], &rel[k + 1..]),
            None => (&rel[..0], &rel[..]),
        };
        let dir = join_lexical(&root, Path::new(OsStr::from_bytes(dir)));
        self.load(side, dir, Some(leaf.to_vec()), false)
    }

    /// `Backspace`, `Alt+Up` in a results tab (P2 5.4): the search root, with the cursor on
    /// the first component of the current result.
    pub(super) fn results_parent(&mut self) -> Vec<Effect> {
        let p = self.panel();
        let first = p
            .current_name()
            .and_then(|n| n.split(|&c| c == b'/').next())
            .map(<[u8]>::to_vec);
        let root = p.dir.clone();
        let side = self.active;
        self.load(side, root, first, false)
    }

    /// `Alt+Left` (`back`) and `Alt+Right`: a directory place loads; a results place shows
    /// its entries again and re-stats them (P2 2.4); an archive place reopens (P3 2.2).
    pub(super) fn history_move(&mut self, back: bool) -> Vec<Effect> {
        let side = self.active;
        let p = self.sides[side].panel_mut();
        let target = if back {
            p.history_back()
        } else {
            p.history_forward()
        };
        let record = if back { Record::Back } else { Record::Forward };
        match target {
            None => Vec::new(),
            Some(Place::Dir(d)) => self.load_ex(side, d, None, false, record),
            // An archive reopens through the index cache (P3 2.2).
            Some(place @ Place::Archive { .. }) => self.open_place(side, place, None, record),
            // A server reuses its open session, or reconnects (P3 2.2, 5.7).
            Some(place @ Place::Remote { .. }) => self.open_remote_place(side, place, None, record),
            Some(Place::Results(s)) => {
                self.leave_results(side);
                self.filter_line = None;
                let p = self.sides[side].panel_mut();
                let (slot, dir, navigating) = (p.slot, p.dir.clone(), p.is_loading());
                if let Some(alive) = p.show_results(*s, record)
                    && navigating
                    && alive.is_running()
                {
                    self.abandoned.push((dir, alive));
                }
                self.mark_visit(slot, false);
                let mut fx = vec![super::Effect::Watch { slot, dir: None }];
                fx.extend(self.refresh_slot(side));
                fx
            }
        }
    }
}
