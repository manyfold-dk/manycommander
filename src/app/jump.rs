#![forbid(unsafe_code)]
//! The directories dialog, `z` and frecency visits (P2 3). Like every dialog they make no
//! filesystem syscall (P-1): the store thread reads `dirs.tsv`, runs zoxide and writes
//! `hotlist.toml`; the app keeps the session's visits as deltas that the runtime merges
//! into `dirs.tsv` on exit.

use super::App;
use super::event::Effect;
use crate::dirs::{Store, Zoxide, now};
use crate::ui::dialog::Dialog;
use crate::ui::dirs::{DirsAction, DirsDialog};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

impl App {
    /// The effect that loads `dirs.tsv`, once per session (P2 3.2): the runtime asks after
    /// the first full frame (P-2); `Ctrl+D` and `z` ask when they come first.
    pub fn dirs_wanted(&mut self) -> Vec<Effect> {
        if self.dirs.requested {
            return Vec::new();
        }
        self.dirs.requested = true;
        vec![Effect::LoadDirs]
    }

    /// The first `Ctrl+D` or `z` of a session reads zoxide's ranking, when enabled
    /// (P2 3.3).
    fn zoxide_wanted(&mut self) -> Vec<Effect> {
        if self.dirs.zoxide != Zoxide::Idle {
            return Vec::new();
        }
        self.dirs.zoxide = Zoxide::Asked;
        vec![Effect::LoadZoxide]
    }

    /// `Ctrl+D`, or `z` without keywords: the directories dialog (P2 3.1).
    pub(super) fn dirs_dialog(&mut self, filter: &[u8]) -> Vec<Effect> {
        let mut fx = self.dirs_wanted();
        fx.extend(self.zoxide_wanted());
        let mut d = DirsDialog::new(filter, &self.home);
        self.fill_dirs(&mut d);
        self.dialog = Some(Dialog::Dirs(d));
        fx
    }

    /// Sets the dialog's lists from the current bookmarks, store and zoxide ranking. A
    /// bookmarked directory is listed once, as the bookmark.
    fn fill_dirs(&self, d: &mut DirsDialog) {
        let bookmarks = &self.dirs.hotlist.dirs;
        let marked: HashSet<&Path> = bookmarks.iter().map(PathBuf::as_path).collect();
        let frequent = self
            .dirs
            .ranked(&self.panel().dir, now())
            .into_iter()
            .map(|(p, _)| p)
            .filter(|p| !marked.contains(p.as_path()))
            .collect();
        d.set(bookmarks, frequent, self.dirs.pending());
    }

    /// Refreshes an open directories dialog, with a note below its list.
    fn refresh_dirs_dialog(&mut self, note: Option<(String, bool)>) {
        if let Some(Dialog::Dirs(mut d)) = self.dialog.take() {
            self.fill_dirs(&mut d);
            if note.is_some() {
                d.note = note;
            }
            self.dialog = Some(Dialog::Dirs(d));
        }
    }

    pub(super) fn on_dirs_action(&mut self, a: DirsAction) -> Vec<Effect> {
        let show = |app: &App, p: &Path| match &app.dialog {
            Some(Dialog::Dirs(d)) => d.display(p),
            _ => p.display().to_string(),
        };
        match a {
            DirsAction::Go(p) => {
                self.dialog = None;
                let side = self.active;
                self.load(side, p, None, false)
            }
            DirsAction::AddCurrent => {
                let dir = self.panel().dir.clone();
                let (note, fx) = match self.dirs.hotlist.add(&dir) {
                    Ok(true) => (
                        (format!("bookmarked {}", show(self, &dir)), false),
                        vec![Effect::SaveHotlist(self.dirs.hotlist.dirs.clone())],
                    ),
                    Ok(false) => (
                        (format!("{} is already a bookmark", show(self, &dir)), false),
                        Vec::new(),
                    ),
                    Err(e) => ((e, true), Vec::new()),
                };
                self.refresh_dirs_dialog(Some(note));
                fx
            }
            DirsAction::RemoveBookmark(p) => {
                let (note, fx) = match self.dirs.hotlist.remove(&p) {
                    Ok(_) => (
                        (format!("removed the bookmark {}", show(self, &p)), false),
                        vec![Effect::SaveHotlist(self.dirs.hotlist.dirs.clone())],
                    ),
                    Err(e) => ((e, true), Vec::new()),
                };
                self.refresh_dirs_dialog(Some(note));
                fx
            }
            DirsAction::Forget(p) => {
                self.dirs.forget(&p);
                let note = (format!("forgot {}", show(self, &p)), false);
                self.refresh_dirs_dialog(Some(note));
                Vec::new()
            }
        }
    }

    /// `dirs.tsv` arrived from the store thread.
    pub(super) fn on_dirs_loaded(&mut self, store: Store) -> Vec<Effect> {
        self.dirs.loaded(store);
        self.refresh_dirs_dialog(None);
        self.resolve_z()
    }

    /// zoxide's ranking arrived from the store thread.
    pub(super) fn on_zoxide_loaded(&mut self, list: Vec<(PathBuf, f64)>) -> Vec<Effect> {
        self.dirs.zoxide_loaded(list);
        self.refresh_dirs_dialog(None);
        self.resolve_z()
    }

    /// `z <keywords>` (P2 3.4); `z` alone opens the dialog. While the store or zoxide's
    /// first ranking is still loading, the jump waits for it.
    pub(super) fn z(&mut self, keywords: Vec<u8>) -> Vec<Effect> {
        if keywords.is_empty() {
            return self.dirs_dialog(b"");
        }
        let mut fx = self.dirs_wanted();
        fx.extend(self.zoxide_wanted());
        if self.dirs.pending() {
            self.pending_z = Some((keywords, self.active));
            self.say("z: loading...");
            return fx;
        }
        let side = self.active;
        fx.extend(self.z_now(&keywords, side));
        fx
    }

    fn z_now(&mut self, keywords: &[u8], side: usize) -> Vec<Effect> {
        let here = self.sides[side].panel().dir.clone();
        match self.dirs.best(keywords, &here, now()) {
            Some(dir) => {
                self.status = None;
                self.load(side, dir, None, false)
            }
            None => {
                self.warn("z: no match");
                Vec::new()
            }
        }
    }

    fn resolve_z(&mut self) -> Vec<Effect> {
        if self.dirs.pending() {
            return Vec::new();
        }
        match self.pending_z.take() {
            Some((k, side)) => self.z_now(&k, side),
            None => Vec::new(),
        }
    }

    /// A navigation of `slot` started: it records a visit when it completes if the user
    /// asked for it (`visit`); the app's own loads (start, restore, the fallback to an
    /// ancestor) do not.
    pub(super) fn mark_visit(&mut self, slot: usize, visit: bool) {
        self.visiting.retain(|&s| s != slot);
        if visit {
            self.visiting.push(slot);
        }
    }

    /// A navigation of `slot` completed in `dir` (P2 3.3).
    pub(super) fn visited(&mut self, slot: usize, dir: &Path) {
        if let Some(i) = self.visiting.iter().position(|&s| s == slot) {
            self.visiting.swap_remove(i);
            self.dirs.visit(dir, now());
        }
    }
}
