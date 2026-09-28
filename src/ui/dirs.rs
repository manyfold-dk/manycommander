#![forbid(unsafe_code)]
//! The directories dialog, "Go to directory" (P2 3.1): a filter line over the bookmarks
//! (marked `*`, in the order the user added them) and the frequent directories (ranked by
//! frecency, the active panel's directory left out).
//!
//! The dialog ranks once, when its lists are set, and keeps an ASCII-lowercased copy of
//! every path; a keystroke only re-filters the ranked lists into a reused row vector
//! (P-15). It holds no reference to the store: the app sets its lists again when the store
//! or zoxide's ranking arrives, or after `Insert`/`Delete`.

use super::dialog::{Outcome, centered, edit, frame_block, line_field};
use super::text::{escaped, fit, fit_left};
use crate::cmdline::Line;
use crate::dirs::{Matcher, lowered};
use crate::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::text::{Line as TLine, Span};
use ratatui::widgets::{Clear, Paragraph};
use std::cell::Cell;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use unicode_width::UnicodeWidthStr;

/// What a key in the dialog asks the app to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DirsAction {
    /// `Enter`: load this directory into the active panel.
    Go(PathBuf),
    /// `Insert`: bookmark the active panel's directory.
    AddCurrent,
    /// `Delete` on a bookmark.
    RemoveBookmark(PathBuf),
    /// `Delete` on a frequent directory: forget its frecency entry.
    Forget(PathBuf),
}

/// A row of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirRow<'a> {
    Bookmark(&'a Path),
    Frequent(&'a Path),
}

impl<'a> DirRow<'a> {
    pub fn path(&self) -> &'a Path {
        match self {
            DirRow::Bookmark(p) | DirRow::Frequent(p) => p,
        }
    }
}

struct Candidate {
    path: PathBuf,
    lower: Vec<u8>,
}

fn candidates(paths: impl IntoIterator<Item = PathBuf>) -> Vec<Candidate> {
    paths
        .into_iter()
        .map(|path| Candidate {
            lower: lowered(&path),
            path,
        })
        .collect()
}

const HELP: &str = "Enter: go   Ins: bookmark this directory   Del: remove   Esc: close";

pub struct DirsDialog {
    pub filter: Line,
    /// The filter the rows were computed for.
    filtered: Vec<u8>,
    matcher: Matcher,
    bookmarks: Vec<Candidate>,
    frequent: Vec<Candidate>,
    /// The frecency store or zoxide's ranking is still loading.
    pub loading: bool,
    /// The visible rows: an index below `bookmarks.len()` is a bookmark, the rest are
    /// frequent directories offset by it.
    rows: Vec<u32>,
    pub cursor: usize,
    /// The first row on screen and the list height at the last draw.
    top: Cell<usize>,
    page: Cell<usize>,
    /// The line below the list: what the last `Insert` or `Delete` did (`false`), or why
    /// it failed (`true`).
    pub note: Option<(String, bool)>,
    home: Vec<u8>,
}

impl DirsDialog {
    /// An empty dialog; the app fills it with [`DirsDialog::set`].
    pub fn new(filter: &[u8], home: &Path) -> DirsDialog {
        let mut line = Line::default();
        line.set(filter);
        DirsDialog {
            filter: line,
            filtered: Vec::new(),
            matcher: Matcher::default(),
            bookmarks: Vec::new(),
            frequent: Vec::new(),
            loading: false,
            rows: Vec::new(),
            cursor: 0,
            top: Cell::new(0),
            page: Cell::new(10),
            note: None,
            home: home.as_os_str().as_bytes().to_vec(),
        }
    }

    /// Replaces the lists: `bookmarks` in insertion order, `frequent` best first. The
    /// cursor stays on its directory when that is still listed.
    pub fn set(&mut self, bookmarks: &[PathBuf], frequent: Vec<PathBuf>, loading: bool) {
        let keep = self.selected().map(|r| match r {
            DirRow::Bookmark(p) => (true, p.to_path_buf()),
            DirRow::Frequent(p) => (false, p.to_path_buf()),
        });
        self.bookmarks = candidates(bookmarks.iter().cloned());
        self.frequent = candidates(frequent);
        self.loading = loading;
        self.refilter();
        if let Some((bm, p)) = keep
            && let Some(i) = (0..self.rows.len()).find(|&i| match self.row(i) {
                Some(DirRow::Bookmark(q)) => bm && q == p,
                Some(DirRow::Frequent(q)) => !bm && q == p,
                None => false,
            })
        {
            self.cursor = i;
        }
    }

    /// Re-filters the lists for the filter line; the cursor goes to the first row.
    fn refilter(&mut self) {
        self.filtered.clear();
        self.filtered.extend_from_slice(self.filter.bytes());
        self.matcher.set(&self.filtered);
        self.rows.clear();
        for (i, c) in self.bookmarks.iter().chain(&self.frequent).enumerate() {
            if self.matcher.matches(&c.lower) {
                self.rows.push(i as u32);
            }
        }
        self.cursor = 0;
        self.top.set(0);
    }

    /// The number of rows the filter lets through.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn row(&self, i: usize) -> Option<DirRow<'_>> {
        let r = *self.rows.get(i)? as usize;
        let nb = self.bookmarks.len();
        Some(if r < nb {
            DirRow::Bookmark(&self.bookmarks[r].path)
        } else {
            DirRow::Frequent(&self.frequent[r - nb].path)
        })
    }

    pub fn selected(&self) -> Option<DirRow<'_>> {
        self.row(self.cursor)
    }

    /// Pastes into the filter (newlines become spaces).
    pub fn paste(&mut self, s: &str) {
        self.filter.insert_bytes(s.replace('\n', " ").as_bytes());
        self.refilter();
    }

    fn move_cursor(&mut self, d: isize) {
        let n = self.rows.len();
        if n > 0 {
            self.cursor = (self.cursor as isize + d).clamp(0, n as isize - 1) as usize;
        }
    }

    pub fn handle(&mut self, k: KeyEvent) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.page.get().max(1) as isize;
        match k.code {
            KeyCode::Esc | KeyCode::F(10) => return Outcome::Close,
            KeyCode::Enter => {
                return match self.selected() {
                    Some(r) => Outcome::Dirs(DirsAction::Go(r.path().to_path_buf())),
                    None => Outcome::Stay,
                };
            }
            KeyCode::Insert => return Outcome::Dirs(DirsAction::AddCurrent),
            KeyCode::Delete => {
                return match self.selected() {
                    Some(DirRow::Bookmark(p)) => {
                        Outcome::Dirs(DirsAction::RemoveBookmark(p.to_path_buf()))
                    }
                    Some(DirRow::Frequent(p)) => Outcome::Dirs(DirsAction::Forget(p.to_path_buf())),
                    None => Outcome::Stay,
                };
            }
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Down => self.move_cursor(1),
            KeyCode::PageUp => self.move_cursor(-page),
            KeyCode::PageDown => self.move_cursor(page),
            _ => {
                if edit(&mut self.filter, k, ctrl) && self.filter.bytes() != self.filtered {
                    self.note = None;
                    self.refilter();
                }
            }
        }
        Outcome::Stay
    }

    /// A path as the list shows it: `~/...` under `$HOME`, escaped (M1 3.2).
    pub fn display(&self, p: &Path) -> String {
        let b = p.as_os_str().as_bytes();
        let h = &self.home;
        if h.len() > 1 && b.starts_with(h) && (b.len() == h.len() || b[h.len()] == b'/') {
            format!("~{}", escaped(&b[h.len()..]))
        } else {
            escaped(b)
        }
    }

    /// Draws the dialog over `area`; returns the cursor position in the filter line.
    pub fn draw(
        &self,
        f: &mut Frame,
        area: ratatui::layout::Rect,
        th: &Theme,
    ) -> Option<(u16, u16)> {
        let width = area.width.saturating_sub(4).clamp(20, 100);
        let height = area.height.saturating_sub(2).clamp(6, 24);
        let r = centered(area, width, height);
        f.render_widget(Clear, r);
        let inner = (r.width as usize).saturating_sub(2);
        let list_h = (r.height as usize).saturating_sub(4);
        self.page.set(list_h.max(1));
        let mut top = self.top.get();
        if self.cursor < top {
            top = self.cursor;
        } else if list_h > 0 && self.cursor >= top + list_h {
            top = self.cursor + 1 - list_h;
        }
        self.top.set(top);

        let mut t: Vec<TLine> = Vec::with_capacity(list_h + 2);
        let (field, cur) = line_field(&self.filter, inner.saturating_sub(2), th);
        let mut spans = vec![Span::styled("> ", th.dialog_border)];
        spans.extend(field.spans);
        t.push(TLine::from(spans));
        for i in top..(top + list_h).min(self.rows.len()) {
            let Some(row) = self.row(i) else { break };
            let mark = match row {
                DirRow::Bookmark(_) => "* ",
                DirRow::Frequent(_) => "  ",
            };
            let text = format!(
                "{mark}{}",
                fit_left(&self.display(row.path()), inner.saturating_sub(2))
            );
            let pad = inner.saturating_sub(text.width());
            let style = if i == self.cursor {
                th.cursor_active
            } else {
                th.dialog
            };
            t.push(TLine::from(Span::styled(
                format!("{text}{}", " ".repeat(pad)),
                style,
            )));
        }
        if t.len() <= list_h {
            if self.loading {
                t.push(TLine::from(Span::styled("  loading...", th.metadata)));
            } else if self.rows.is_empty() {
                t.push(TLine::from(Span::styled("  no match", th.metadata)));
            }
        }
        while t.len() < list_h + 1 {
            t.push(TLine::default());
        }
        t.push(match &self.note {
            Some((text, err)) => TLine::from(Span::styled(
                fit(text, inner).0,
                if *err { th.error } else { th.dialog },
            )),
            None => TLine::from(Span::styled(fit(HELP, inner).0, th.metadata)),
        });
        f.render_widget(
            Paragraph::new(t).block(frame_block("Go to directory", th, false)),
            r,
        );
        (r.height >= 3).then(|| {
            (
                r.x + 1 + 2 + cur.min(inner.saturating_sub(3) as u16),
                r.y + 1,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers as M;

    fn key(a: &mut DirsDialog, code: KeyCode) -> Outcome {
        a.handle(KeyEvent::new(code, M::NONE))
    }

    fn paths(v: &[&str]) -> Vec<PathBuf> {
        v.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn filters_bookmarks_then_frequent_and_keeps_the_cursor() {
        let mut d = DirsDialog::new(b"", Path::new("/home/u"));
        d.set(
            &paths(&["/home/u/b1", "/srv/src"]),
            paths(&["/home/u/src/app", "/home/u/docs"]),
            false,
        );
        assert_eq!(d.len(), 4);
        assert_eq!(d.row(0), Some(DirRow::Bookmark(Path::new("/home/u/b1"))));
        assert_eq!(
            d.row(2),
            Some(DirRow::Frequent(Path::new("/home/u/src/app")))
        );
        for c in "SRC".chars() {
            key(&mut d, KeyCode::Char(c));
        }
        assert_eq!(d.len(), 1, "src only in the last component");
        assert_eq!(d.selected(), Some(DirRow::Bookmark(Path::new("/srv/src"))));
        key(&mut d, KeyCode::Backspace);
        key(&mut d, KeyCode::Backspace);
        key(&mut d, KeyCode::Backspace);
        key(&mut d, KeyCode::Down);
        key(&mut d, KeyCode::Down);
        assert_eq!(d.selected().unwrap().path(), Path::new("/home/u/src/app"));
        // New lists keep the cursor on its directory.
        d.set(
            &paths(&["/home/u/b1"]),
            paths(&["/home/u/docs", "/home/u/src/app"]),
            true,
        );
        assert_eq!(d.selected().unwrap().path(), Path::new("/home/u/src/app"));
        assert_eq!(
            key(&mut d, KeyCode::Enter),
            Outcome::Dirs(DirsAction::Go("/home/u/src/app".into()))
        );
        assert_eq!(
            key(&mut d, KeyCode::Delete),
            Outcome::Dirs(DirsAction::Forget("/home/u/src/app".into()))
        );
        key(&mut d, KeyCode::PageUp);
        assert_eq!(
            key(&mut d, KeyCode::Delete),
            Outcome::Dirs(DirsAction::RemoveBookmark("/home/u/b1".into()))
        );
        assert_eq!(
            key(&mut d, KeyCode::Insert),
            Outcome::Dirs(DirsAction::AddCurrent)
        );
        assert_eq!(key(&mut d, KeyCode::Esc), Outcome::Close);
    }

    #[test]
    fn home_shows_as_tilde() {
        let d = DirsDialog::new(b"", Path::new("/home/u"));
        assert_eq!(d.display(Path::new("/home/u")), "~");
        assert_eq!(d.display(Path::new("/home/u/x\ny")), "~/x\\ny");
        assert_eq!(d.display(Path::new("/home/user2")), "/home/user2");
        let root = DirsDialog::new(b"", Path::new("/"));
        assert_eq!(root.display(Path::new("/etc")), "/etc");
    }
}
