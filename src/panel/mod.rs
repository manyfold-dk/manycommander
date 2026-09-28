#![forbid(unsafe_code)]
//! Panels (design section 5): listing, sort, selection, cursor, per-panel watcher.
//!
//! A panel holds its directory, the entries (compact, stored once), the sort, the marks
//! (flag bits, carried across a refresh by name), a cursor by name so it survives a
//! refresh, the hidden toggle, the load generation and its directory history. A panel
//! never touches the filesystem: loads are [`listing::ListRequest`]s that the runtime
//! runs on listing threads, and late results are dropped by generation.

pub mod entry;
pub mod listing;
pub mod sort;
pub mod tabs;
pub mod watch;

use crate::fsops::group::Group;
use entry::{EKind, Entry, LinkKind, MARKED, SIZED};
use listing::{Alive, ListRequest};
use sort::{Keys, SortKey, SortSpec};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// One directory's entries, sorted and filtered.
#[derive(Default, Clone)]
pub struct Listing {
    pub entries: Vec<Entry>,
    pub names: Vec<u8>,
    keys: Keys,
    /// The sort permutation over `entries`.
    order: Vec<u32>,
    /// `order` without hidden entries (when they are hidden).
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

    fn resort(&mut self, spec: SortSpec, show_hidden: bool) {
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
        self.refilter(show_hidden);
        self.dirty = false;
    }

    fn refilter(&mut self, show_hidden: bool) {
        self.visible.clear();
        let entries = &self.entries;
        self.visible.extend(
            self.order
                .iter()
                .copied()
                .filter(|&i| show_hidden || !entries[i as usize].hidden()),
        );
    }

    pub(crate) fn find(&self, name: &[u8]) -> Option<u32> {
        (0..self.entries.len() as u32).find(|&i| self.name(i) == name)
    }
}

/// A row of the panel: the parent entry `..`, or a listed entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Parent,
    Entry(u32),
}

#[derive(Default)]
pub struct History {
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
}

impl History {
    const CAP: usize = 100;

    fn push(&mut self, from: PathBuf) {
        if self.back.last() != Some(&from) {
            self.back.push(from);
            if self.back.len() > Self::CAP {
                self.back.remove(0);
            }
        }
        self.forward.clear();
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
    /// The directory and listing to return to (Esc, failure).
    prev: Option<(PathBuf, Listing, Option<Vec<u8>>)>,
    staging: Listing,
}

pub struct Panel {
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
            marked: 0,
            marked_bytes: 0,
            loaded_once: false,
            sorted_at: None,
            saved_marks: HashSet::new(),
            released: false,
            slot,
        }
    }

    pub fn has_parent(&self) -> bool {
        self.dir.parent().is_some()
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
        self.navigate_full(dir, cursor_to, alive, fallback, !fallback)
    }

    /// `record`: push the directory left behind onto the history.
    pub fn navigate_full(
        &mut self,
        dir: PathBuf,
        cursor_to: Option<Vec<u8>>,
        alive: Alive,
        fallback: bool,
        record: bool,
    ) -> ListRequest {
        // What Esc or a failure returns to: the listing on screen, or, when a navigation
        // is still in flight, the one that navigation would have returned to.
        let prev = match self.loading.take() {
            Some(Loading { prev: Some(p), .. }) => p,
            _ => (
                self.dir.clone(),
                std::mem::take(&mut self.list),
                self.cursor_name.clone(),
            ),
        };
        if self.loaded_once && record && dir != prev.0 {
            self.history.push(prev.0.clone());
        }
        self.dir = dir;
        self.list = Listing::default();
        self.generation += 1;
        self.loading = Some(Loading {
            kind: LoadKind::Navigate,
            started: Instant::now(),
            alive,
            prev: Some(prev),
            staging: Listing::default(),
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
            self.list = Listing::default();
            self.loading = Some(Loading {
                kind: LoadKind::Navigate,
                started: Instant::now(),
                alive,
                prev,
                staging: Listing::default(),
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
        });
        self.req(false)
    }

    /// `Esc` during a load: back to the previous directory at once. Returns the abandoned
    /// load's liveness, so the caller can count stuck threads.
    pub fn cancel_load(&mut self) -> Option<(PathBuf, Alive)> {
        let l = self.loading.take()?;
        self.generation += 1;
        let abandoned = (self.dir.clone(), l.alive.clone());
        if let Some((dir, list, name)) = l.prev {
            self.dir = dir;
            self.list = list;
            self.cursor_name = name;
            self.recount_marks();
            self.restore_cursor();
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
        self.recount_marks();
        self.ensure_sorted();
        self.restore_cursor();
        changed.then(|| self.dir.clone())
    }

    /// The load failed: a navigation returns to where it came from.
    pub fn on_failed(&mut self, generation: u64, error: String) {
        if generation != self.generation {
            return;
        }
        let Some(l) = self.loading.take() else { return };
        if let Some((dir, list, name)) = l.prev {
            self.message = Some(format!("{}: {error}", self.dir.display()));
            self.dir = dir;
            self.list = list;
            self.cursor_name = name;
            self.recount_marks();
            self.restore_cursor();
        } else {
            self.message = Some(error);
        }
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
    /// directory is still loading, batches are sorted in at most every 150 ms, so a
    /// 100k-entry load does not re-sort on every batch (the loop ticks while loading).
    pub fn ensure_sorted(&mut self) {
        if !self.list.dirty {
            return;
        }
        if self.is_loading()
            && self
                .sorted_at
                .is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(150))
        {
            return;
        }
        self.force_sort();
    }

    fn force_sort(&mut self) {
        self.list.resort(self.sort, self.show_hidden);
        self.sorted_at = Some(Instant::now());
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
        self.list.refilter(self.show_hidden);
        self.restore_cursor();
    }

    pub(crate) fn remember_cursor(&mut self) {
        self.cursor_name = match self.current() {
            Some(Row::Entry(i)) => Some(self.list.name(i).to_vec()),
            Some(Row::Parent) => Some(b"..".to_vec()),
            None => self.cursor_name.take(),
        };
    }

    fn restore_cursor(&mut self) {
        let rows = self.rows();
        if let Some(name) = &self.cursor_name {
            let p = self.has_parent() as usize;
            if name == b".." {
                self.cursor = 0;
            } else if let Some(pos) = self
                .list
                .visible
                .iter()
                .position(|&i| self.list.name(i) == name.as_slice())
            {
                self.cursor = p + pos;
            }
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

    fn recount_marks(&mut self) {
        self.marked = 0;
        self.marked_bytes = 0;
        for e in &self.list.entries {
            if e.marked() {
                self.marked += 1;
                self.marked_bytes += e.size;
            }
        }
    }

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

    /// The names a verb acts on: the marked entries, or the entry under the cursor.
    pub fn selection(&self) -> Vec<OsString> {
        if self.marked > 0 {
            return self
                .list
                .visible
                .iter()
                .filter(|&&i| self.list.entries[i as usize].marked())
                .map(|&i| OsStr::from_bytes(self.list.name(i)).to_owned())
                .collect();
        }
        self.current_name()
            .map(|n| vec![OsStr::from_bytes(n).to_owned()])
            .unwrap_or_default()
    }

    /// The selection as job groups (P2 2.2), empty when nothing is selected. A directory
    /// panel's selection is one group in its directory. A name with `/` (a results tab's
    /// relative path, P2 2.4) is split at its last `/`, and names that share a directory
    /// form one group.
    pub fn selection_groups(&self) -> Vec<Group> {
        Group::from_relative(&self.dir, self.selection())
    }

    /// The selected entries' kinds, for confirmations ("N symbolic links are copied as
    /// links").
    pub fn selection_kinds(&self) -> Vec<EKind> {
        let sel = self.selection();
        sel.iter()
            .filter_map(|n| self.list.find(n.as_bytes()))
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

    /// Back in history. Returns the directory to load.
    pub fn history_back(&mut self) -> Option<PathBuf> {
        let d = self.history.back.pop()?;
        self.history.forward.push(self.dir.clone());
        Some(d)
    }

    pub fn history_forward(&mut self) -> Option<PathBuf> {
        let d = self.history.forward.pop()?;
        self.history.back.push(self.dir.clone());
        Some(d)
    }

    pub fn dir_name(&self) -> Option<Vec<u8>> {
        self.dir.file_name().map(|n| n.as_bytes().to_vec())
    }
}

/// `*`, `?` and `[...]` matching on bytes (mark by glob).
pub fn glob_match(p: &[u8], s: &[u8]) -> bool {
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
                    if let Some((matched, next)) = class(&p[pi..], s[si])
                        && matched
                    {
                        pi += next;
                        si += 1;
                        continue;
                    }
                }
                c if c == s[si] => {
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
