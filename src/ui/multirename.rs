#![forbid(unsafe_code)]
//! The multi-rename tool (Ctrl+M, P2 6.1): a form that fills the panel area, with the live
//! preview below its fields.
//!
//! The tool holds a copy of what the panel lists about the selection (names, modification
//! times, the other names of their directories), so its preview is computed in memory on
//! every change (P-14) and never makes a syscall (P-1). The search fields are compiled
//! once per edit of them, not per name (P2 6.2). Only the preview rows on screen are drawn;
//! `PgUp`/`PgDn` scroll them.

use super::dialog::Outcome;
use super::form::{Form, FormEvent};
use super::text::{escaped, fit};
use crate::fsops::group::Group;
use crate::rename::{
    self, CaseMode, Counter, Directory, Entry, MAX_DIGITS, Preview, Rules, Search, Status,
};
use crate::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line as TLine, Span};
use ratatui::widgets::Paragraph;
use std::cell::Cell;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::sync::Arc;

/// The tool's fields (P2 6.1).
pub const RENAME_NAME: usize = 0;
pub const RENAME_EXT: usize = 1;
pub const RENAME_SEARCH: usize = 2;
pub const RENAME_REPLACE: usize = 3;
pub const RENAME_REGEX: usize = 4;
pub const RENAME_MATCH_CASE: usize = 5;
pub const RENAME_CASE: usize = 6;
pub const RENAME_START: usize = 7;
pub const RENAME_STEP: usize = 8;
pub const RENAME_DIGITS: usize = 9;
/// The case choice's options, in display order; the first is the default.
pub const CASE_MODES: [(&str, CaseMode); 4] = [
    ("unchanged", CaseMode::Unchanged),
    ("lower", CaseMode::Lower),
    ("upper", CaseMode::Upper),
    ("title", CaseMode::Title),
];

/// The status column's width.
const STATUS_W: usize = 14;

/// The search fields a compiled search came from.
type SearchKey = (Vec<u8>, Vec<u8>, bool, bool);

pub struct RenameTool {
    pub form: Form,
    /// The selected entries, in panel order (I-8).
    pub entries: Vec<Entry>,
    /// Their directories, one per group.
    pub dirs: Vec<Directory>,
    /// What the job gets: one group per directory, names in selection order (P2 2.2).
    pub groups: Vec<Group>,
    pub preview: Preview,
    search: Option<(SearchKey, Result<Arc<Search>, String>)>,
    tz: jiff::tz::TimeZone,
    /// The first preview row on screen.
    pub scroll: usize,
    /// Preview rows at the last draw (the `PgUp`/`PgDn` step).
    page: Cell<usize>,
}

/// A counter field as a whole number.
fn number(form: &Form, field: usize, label: &str) -> Result<u64, String> {
    let text = form.text_of(field);
    std::str::from_utf8(text)
        .ok()
        .map(str::trim)
        .and_then(|t| t.parse::<u64>().ok())
        .ok_or_else(|| format!("{label}: a whole number"))
}

impl RenameTool {
    /// The tool for `entries` (their `dir` indexes `dirs` and `groups`). `undo` is the
    /// number of renames Ctrl+Z would undo, when there is a record (P2 6.5).
    pub fn new(
        entries: Vec<Entry>,
        dirs: Vec<Directory>,
        groups: Vec<Group>,
        tz: jiff::tz::TimeZone,
        undo: Option<usize>,
    ) -> RenameTool {
        let modes = CASE_MODES.map(|(label, _)| label);
        let mut form = Form::new("Multi-rename");
        if let Some(n) = undo {
            let what = if n == 1 { "entry" } else { "entries" };
            form = form.line(format!("Ctrl+Z: undo the last multi-rename ({n} {what})"));
        }
        let form = form
            .text("Name mask", b"[N]")
            .text("Extension mask", b"[E]")
            .text("Search", b"")
            .text("Replace", b"")
            .check("Regex (replace: $1..$9, ${name})", false)
            .check("Match case", false)
            .choice("Case", &modes, 0)
            .text("Counter start", b"1")
            .text("Counter step", b"1")
            .text("Counter digits", b"1")
            .help("[N] name  [E] extension  [N2-5] [N2-] [N-3] characters  [C] counter")
            .help("[P] directory  [Y][M][D] [h][m][s] modified  [[ ]] brackets  PgUp/PgDn")
            .fill();
        let mut t = RenameTool {
            form,
            entries,
            dirs,
            groups,
            preview: Preview::default(),
            search: None,
            tz,
            scroll: 0,
            page: Cell::new(10),
        };
        t.refresh();
        t
    }

    /// Recomputes the preview from the fields (P2 6.4) and clears the error of the last
    /// `Enter`. The search is compiled again only when its fields changed.
    pub fn refresh(&mut self) {
        let f = &self.form;
        let key: SearchKey = (
            f.text_of(RENAME_SEARCH).to_vec(),
            f.text_of(RENAME_REPLACE).to_vec(),
            f.checked(RENAME_REGEX),
            f.checked(RENAME_MATCH_CASE),
        );
        if self.search.as_ref().is_none_or(|(k, _)| *k != key) {
            let s = Search::compile(&key.0, &key.1, key.2, key.3).map(Arc::new);
            self.search = Some((key, s));
        }
        self.preview = match self.rules() {
            Ok(r) => rename::preview(&r, &self.entries, &self.dirs),
            Err(e) => Preview::blocked(self.entries.len(), e),
        };
        self.form.error = None;
    }

    fn rules(&self) -> Result<Rules, String> {
        let f = &self.form;
        let search = match &self.search {
            Some((_, Ok(s))) => s.clone(),
            Some((_, Err(e))) => return Err(e.clone()),
            None => Arc::new(Search::None),
        };
        let digits = number(f, RENAME_DIGITS, "Counter digits")?;
        let counter = Counter {
            start: number(f, RENAME_START, "Counter start")?,
            step: number(f, RENAME_STEP, "Counter step")?,
            digits: usize::try_from(digits).unwrap_or(MAX_DIGITS + 1),
        };
        let case = CASE_MODES
            .get(f.chosen(RENAME_CASE))
            .map(|(_, c)| *c)
            .unwrap_or_default();
        Rules::new(
            f.text_of(RENAME_NAME),
            f.text_of(RENAME_EXT),
            search,
            case,
            counter,
            &self.tz,
        )
    }

    /// Why `Enter` cannot run the rename: the first error, or nothing to rename.
    pub fn blocked(&self) -> Option<String> {
        if let Some(e) = &self.preview.error {
            return Some(e.clone());
        }
        (self.preview.changed == 0).then(|| "nothing to rename: every name stays".into())
    }

    /// The job's renames: per group, `(old name, new name)` in selection order.
    pub fn renames(&self) -> Vec<Vec<(OsString, OsString)>> {
        let mut out = vec![Vec::new(); self.groups.len()];
        for (i, e) in self.entries.iter().enumerate() {
            out[e.dir].push((
                OsString::from_vec(e.name.clone()),
                OsString::from_vec(self.preview.new_name(i).to_vec()),
            ));
        }
        out
    }

    /// `Ctrl+Z` asks the app for the undo (P2 6.5); `PgUp`/`PgDn` scroll the preview; the
    /// other keys go to the form.
    pub fn handle(&mut self, k: KeyEvent) -> Outcome {
        if k.code == KeyCode::Char('z') && k.modifiers.contains(KeyModifiers::CONTROL) {
            return Outcome::Undo;
        }
        match self.form.handle(k) {
            FormEvent::Changed => Outcome::FormChanged,
            FormEvent::Submit => Outcome::FormSubmit,
            FormEvent::Close => Outcome::Close,
            FormEvent::Stay => Outcome::Stay,
            FormEvent::Unhandled => {
                let page = self.page.get().max(1);
                let last = self.preview.len().saturating_sub(page);
                match k.code {
                    KeyCode::PageDown => self.scroll = (self.scroll + page).min(last),
                    KeyCode::PageUp => self.scroll = self.scroll.min(last).saturating_sub(page),
                    _ => {}
                }
                Outcome::Stay
            }
        }
    }

    pub fn paste(&mut self, s: &str) {
        self.form.paste(s);
    }

    /// Draws the tool over `area`; returns the cursor of a focused text field.
    pub fn draw(&self, f: &mut Frame, area: Rect, th: &Theme) -> Option<(u16, u16)> {
        let drawn = self.form.draw(f, area, th);
        if let Some(body) = drawn.body {
            self.draw_preview(f, body, th);
        }
        drawn.cursor
    }

    /// The preview: a summary (or the first error) and the rows on screen, `old -> new`
    /// with a status.
    fn draw_preview(&self, f: &mut Frame, r: Rect, th: &Theme) {
        let w = r.width as usize;
        let p = &self.preview;
        let page = (r.height as usize).saturating_sub(1);
        self.page.set(page);
        let start = self.scroll.min(p.len().saturating_sub(page));
        let mut t: Vec<TLine> = Vec::with_capacity(page + 1);
        let head = match &p.error {
            Some(e) => Span::styled(fit(e, w).0, th.error),
            None => {
                let unchanged = p.len() - p.changed - p.errors;
                let mut s = format!("{} selected: {} renamed", p.len(), p.changed);
                if unchanged > 0 {
                    s += &format!(", {unchanged} unchanged");
                }
                if p.len() > page {
                    s += &format!("   rows {}-{}", start + 1, (start + page).min(p.len()));
                }
                Span::styled(fit(&s, w).0, th.metadata)
            }
        };
        t.push(TLine::from(head));
        let names_w = w.saturating_sub(STATUS_W + 1 + 4);
        let old_w = names_w / 2;
        let new_w = names_w - old_w;
        let cell = |text: &str, width: usize, style: Style| {
            let (s, used) = fit(text, width);
            Span::styled(
                format!("{s}{}", " ".repeat(width.saturating_sub(used))),
                style,
            )
        };
        for i in start..(start + page).min(p.len()) {
            let e = &self.entries[i];
            let (status, style) = match p.status[i] {
                Status::Unchanged => ("unchanged", th.metadata),
                Status::Ok => ("ok", th.dialog),
                Status::Error(problem) => (problem.short(), th.error),
                Status::Blocked => ("", th.metadata),
            };
            let new = escaped(p.new_name(i));
            t.push(TLine::from(vec![
                cell(&escaped(&e.name), old_w, th.dialog),
                Span::styled(" -> ", th.metadata),
                cell(&new, new_w, th.dialog),
                Span::styled(" ", th.dialog),
                cell(status, STATUS_W, style),
            ]));
        }
        f.render_widget(Paragraph::new(t).style(th.dialog), r);
    }
}
