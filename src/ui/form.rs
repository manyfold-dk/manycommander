#![forbid(unsafe_code)]
//! Forms (P2 2.1): the multi-field dialogs of find, multi-rename, links, attributes and
//! compare.
//!
//! A form is a list of fields: a text line (the command line's [`Line`]), a checkbox, or a
//! choice among labelled options. Lines of text may stand above the fields; status lines
//! (a preview), an error and help lines below them; and below those an optional body area
//! that the form's owner draws itself (the multi-rename preview): [`Form::draw`] returns
//! its rectangle, and [`Form::handle`] returns [`FormEvent::Unhandled`] for the keys the
//! form does not use (`PgUp`, `PgDn`, ...), so the owner can scroll its body. A form that
//! fills its area ([`Form::fill`]) gives the body every row its fields leave.
//!
//! Keys: `Tab` and `Down` move the focus to the next field, `Shift+Tab` (`BackTab`) and
//! `Up` to the previous one; `Space` toggles a focused checkbox; `Left`/`Right` change a
//! focused choice (on a text field they move the cursor); `Enter` submits from any field;
//! `Esc` closes. A focused text field takes the line-editing keys of every dialog field.
//! The form owns its state; its owner builds it, checks it on every change and acts on the
//! submitted values, as with the M1 dialogs.

use super::dialog::{centered, edit, frame_block, line_field};
use super::text::fit;
use crate::cmdline::Line;
use crate::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line as TLine, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

/// One field of a form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Field {
    Text {
        label: String,
        line: Line,
    },
    Check {
        label: String,
        on: bool,
    },
    Choice {
        label: String,
        options: Vec<String>,
        selected: usize,
    },
}

/// A field's value, as [`Form::values`] returns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Text(Vec<u8>),
    Check(bool),
    /// The index of the selected option.
    Choice(usize),
}

/// What a key did to the form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormEvent {
    /// Consumed; no value changed (the focus or a cursor moved).
    Stay,
    /// A field's value changed: the owner re-checks the form.
    Changed,
    /// `Enter`: the owner checks the values and acts, or keeps the form open with an error.
    Submit,
    /// `Esc`: the form closes without acting.
    Close,
    /// Not a form key; the owner may use it (a body's `PgUp`/`PgDn`).
    Unhandled,
}

/// Where [`Form::draw`] put the cursor and the body.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drawn {
    /// The terminal cursor, in a focused text field.
    pub cursor: Option<(u16, u16)>,
    /// The body area the owner draws, when the form has one and it is on screen.
    pub body: Option<Rect>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Form {
    pub title: String,
    /// Lines above the fields.
    pub lines: Vec<String>,
    pub fields: Vec<Field>,
    /// The index of the focused field.
    pub focus: usize,
    /// Lines below the fields that the owner keeps current (a preview).
    pub status: Vec<String>,
    /// Why `Enter` did not submit; the owner sets it and clears it on the next change.
    pub error: Option<String>,
    /// Dimmed lines below the status (the grammar of a field).
    pub help: Vec<String>,
    /// Rows reserved for the owner's body.
    pub body_rows: u16,
    /// The widest the form gets.
    pub max_width: u16,
    /// The form takes the whole area it is drawn in; the body gets the rows left.
    pub fill: bool,
}

const HINT: &str = "Tab: next field   Enter: OK   Esc: cancel";

impl Form {
    pub fn new(title: impl Into<String>) -> Form {
        Form {
            title: title.into(),
            lines: Vec::new(),
            fields: Vec::new(),
            focus: 0,
            status: Vec::new(),
            error: None,
            help: Vec::new(),
            body_rows: 0,
            max_width: 76,
            fill: false,
        }
    }

    /// Adds a line of text above the fields.
    pub fn line(mut self, text: impl Into<String>) -> Form {
        self.lines.push(text.into());
        self
    }

    /// Adds a text field holding `initial`, with its cursor at the end.
    pub fn text(mut self, label: impl Into<String>, initial: &[u8]) -> Form {
        let mut line = Line::default();
        line.set(initial);
        self.fields.push(Field::Text {
            label: label.into(),
            line,
        });
        self
    }

    pub fn check(mut self, label: impl Into<String>, on: bool) -> Form {
        self.fields.push(Field::Check {
            label: label.into(),
            on,
        });
        self
    }

    pub fn choice(mut self, label: impl Into<String>, options: &[&str], selected: usize) -> Form {
        self.fields.push(Field::Choice {
            label: label.into(),
            options: options.iter().map(|o| o.to_string()).collect(),
            selected: selected.min(options.len().saturating_sub(1)),
        });
        self
    }

    /// Adds a dimmed help line below the fields.
    pub fn help(mut self, text: impl Into<String>) -> Form {
        self.help.push(text.into());
        self
    }

    /// Reserves `rows` for a body that the owner draws.
    pub fn body(mut self, rows: u16) -> Form {
        self.body_rows = rows;
        self
    }

    /// Makes the form fill the area it is drawn in, with the body below its fields taking
    /// the rows they leave (the multi-rename tool, P2 6.1).
    pub fn fill(mut self) -> Form {
        self.fill = true;
        self
    }

    /// Lets the form grow to `width` columns (the default is the dialog width).
    pub fn max_width(mut self, width: u16) -> Form {
        self.max_width = width.max(20);
        self
    }

    fn move_focus(&mut self, delta: isize) {
        let n = self.fields.len() as isize;
        if n > 0 {
            self.focus = (self.focus as isize + delta).rem_euclid(n) as usize;
        }
    }

    pub fn handle(&mut self, k: KeyEvent) -> FormEvent {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Enter => return FormEvent::Submit,
            KeyCode::Esc | KeyCode::F(10) => return FormEvent::Close,
            KeyCode::Tab | KeyCode::Down => {
                self.move_focus(1);
                return FormEvent::Stay;
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.move_focus(-1);
                return FormEvent::Stay;
            }
            _ => {}
        }
        let Some(field) = self.fields.get_mut(self.focus) else {
            return FormEvent::Unhandled;
        };
        match field {
            Field::Text { line, .. } => {
                let before = line.bytes().to_vec();
                if !edit(line, k, ctrl) {
                    return FormEvent::Unhandled;
                }
                if line.bytes() == before {
                    FormEvent::Stay
                } else {
                    FormEvent::Changed
                }
            }
            Field::Check { on, .. } => match k.code {
                KeyCode::Char(' ') if !ctrl => {
                    *on = !*on;
                    FormEvent::Changed
                }
                _ => FormEvent::Unhandled,
            },
            Field::Choice {
                options, selected, ..
            } => {
                let n = options.len().max(1);
                match k.code {
                    KeyCode::Left => *selected = (*selected + n - 1) % n,
                    KeyCode::Right => *selected = (*selected + 1) % n,
                    _ => return FormEvent::Unhandled,
                }
                FormEvent::Changed
            }
        }
    }

    /// Pastes into a focused text field (newlines become spaces). Returns whether a value
    /// changed.
    pub fn paste(&mut self, s: &str) -> bool {
        match self.fields.get_mut(self.focus) {
            Some(Field::Text { line, .. }) if !s.is_empty() => {
                line.insert_bytes(s.replace('\n', " ").as_bytes());
                true
            }
            _ => false,
        }
    }

    pub fn values(&self) -> Vec<Value> {
        self.fields
            .iter()
            .map(|f| match f {
                Field::Text { line, .. } => Value::Text(line.bytes().to_vec()),
                Field::Check { on, .. } => Value::Check(*on),
                Field::Choice { selected, .. } => Value::Choice(*selected),
            })
            .collect()
    }

    /// The text of field `i`; empty when it is not a text field.
    pub fn text_of(&self, i: usize) -> &[u8] {
        match self.fields.get(i) {
            Some(Field::Text { line, .. }) => line.bytes(),
            _ => &[],
        }
    }

    /// Whether field `i` is a checked checkbox.
    pub fn checked(&self, i: usize) -> bool {
        matches!(self.fields.get(i), Some(Field::Check { on: true, .. }))
    }

    /// The selected option of field `i`; 0 when it is not a choice.
    pub fn chosen(&self, i: usize) -> usize {
        match self.fields.get(i) {
            Some(Field::Choice { selected, .. }) => *selected,
            _ => 0,
        }
    }

    /// Draws the form centred in `area`, in the dialog style.
    pub fn draw(&self, f: &mut Frame, area: Rect, th: &Theme) -> Drawn {
        let width = if self.fill {
            area.width
        } else {
            area.width
                .saturating_sub(4)
                .clamp(20, self.max_width.max(20))
                .min(area.width)
        };
        let inner = (width as usize).saturating_sub(2);
        let widest = self
            .fields
            .iter()
            .filter_map(|fl| match fl {
                Field::Text { label, .. } | Field::Choice { label, .. } => Some(label.width()),
                Field::Check { .. } => None,
            })
            .max()
            .unwrap_or(0);
        let label_w = (widest + 2).min(inner / 2);
        let mut t: Vec<TLine> = self
            .lines
            .iter()
            .map(|l| TLine::from(Span::styled(fit(l, inner).0, th.dialog)))
            .collect();
        let mut cursor = None;
        for (i, fl) in self.fields.iter().enumerate() {
            let focused = i == self.focus;
            let lstyle = if focused { th.cursor_active } else { th.dialog };
            let label_spans = |label: &str| {
                let (lab, lw) = fit(label, label_w.saturating_sub(1));
                vec![
                    Span::styled(lab, lstyle),
                    Span::styled(" ".repeat(label_w - lw), th.dialog),
                ]
            };
            match fl {
                Field::Text { label, line } => {
                    let mut spans = label_spans(label);
                    let (field, cur) = line_field(line, inner.saturating_sub(label_w), th);
                    if focused {
                        cursor = Some((t.len() as u16, label_w as u16 + cur));
                    }
                    spans.extend(field.spans);
                    t.push(TLine::from(spans));
                }
                Field::Check { label, on } => {
                    let mark = if *on { "[x]" } else { "[ ]" };
                    t.push(TLine::from(vec![
                        Span::styled(mark, lstyle),
                        Span::styled(" ", th.dialog),
                        Span::styled(fit(label, inner.saturating_sub(4)).0, th.dialog),
                    ]));
                }
                Field::Choice {
                    label,
                    options,
                    selected,
                } => {
                    let mut spans = label_spans(label);
                    let mut used = label_w;
                    for (k, o) in options.iter().enumerate() {
                        let text = format!("({}) {o}", if k == *selected { '•' } else { ' ' });
                        let w = text.width();
                        if used > label_w && used + 2 + w > inner {
                            t.push(TLine::from(std::mem::take(&mut spans)));
                            spans.push(Span::styled(" ".repeat(label_w), th.dialog));
                            used = label_w;
                        }
                        if used > label_w {
                            spans.push(Span::styled("  ", th.dialog));
                            used += 2;
                        }
                        let style = if focused && k == *selected {
                            th.cursor_active
                        } else {
                            th.dialog
                        };
                        spans.push(Span::styled(fit(&text, inner - used).0, style));
                        used += w;
                    }
                    t.push(TLine::from(spans));
                }
            }
        }
        for s in &self.status {
            t.push(TLine::from(Span::styled(fit(s, inner).0, th.dialog)));
        }
        if let Some(e) = &self.error {
            t.push(TLine::from(Span::styled(fit(e, inner).0, th.error)));
        }
        for h in &self.help {
            t.push(TLine::from(Span::styled(fit(h, inner).0, th.metadata)));
        }
        let body_at = t.len() as u16;
        // Filling: the borders and the hint line take three rows.
        let body_rows = if self.fill {
            area.height.saturating_sub(body_at + 3)
        } else {
            self.body_rows
        };
        t.extend((0..body_rows).map(|_| TLine::default()));
        t.push(TLine::from(Span::styled(fit(HINT, inner).0, th.metadata)));
        let r = centered(area, width, t.len() as u16 + 2);
        f.render_widget(Clear, r);
        f.render_widget(
            Paragraph::new(t).block(frame_block(&self.title, th, false)),
            r,
        );
        let rows = r.height.saturating_sub(2);
        let cursor = cursor
            .filter(|(row, _)| *row < rows)
            .map(|(row, col)| (r.x + 1 + col, r.y + 1 + row));
        let body = (body_rows > 0 && body_at < rows).then(|| Rect {
            x: r.x + 1,
            y: r.y + 1 + body_at,
            width: r.width.saturating_sub(2),
            height: body_rows.min(rows - body_at),
        });
        Drawn { cursor, body }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Depth;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn sample() -> Form {
        Form::new("Sample")
            .line("Change 2 entries")
            .text("Name", b"ab")
            .check("Recursive", false)
            .choice("Kind", &["one", "two", "three"], 0)
    }

    #[test]
    fn focus_order_wraps_both_ways() {
        let mut f = sample();
        assert_eq!(f.focus, 0);
        for want in [1, 2, 0, 1] {
            assert_eq!(f.handle(key(KeyCode::Tab)), FormEvent::Stay);
            assert_eq!(f.focus, want);
        }
        for want in [0, 2, 1] {
            assert_eq!(
                f.handle(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
                FormEvent::Stay
            );
            assert_eq!(f.focus, want);
        }
        f.handle(key(KeyCode::Down));
        assert_eq!(f.focus, 2);
        f.handle(key(KeyCode::Up));
        assert_eq!(f.focus, 1);
    }

    #[test]
    fn space_toggles_a_checkbox_and_types_into_a_text_field() {
        let mut f = sample();
        assert_eq!(f.handle(key(KeyCode::Char(' '))), FormEvent::Changed);
        assert_eq!(f.text_of(0), b"ab ");
        f.focus = 1;
        assert_eq!(f.handle(key(KeyCode::Char(' '))), FormEvent::Changed);
        assert!(f.checked(1));
        assert_eq!(f.handle(key(KeyCode::Char(' '))), FormEvent::Changed);
        assert!(!f.checked(1));
        f.focus = 2;
        assert_eq!(f.handle(key(KeyCode::Char(' '))), FormEvent::Unhandled);
        assert_eq!(f.chosen(2), 0);
    }

    #[test]
    fn left_and_right_cycle_a_choice_and_move_a_text_cursor() {
        let mut f = sample();
        f.focus = 2;
        assert_eq!(f.handle(key(KeyCode::Right)), FormEvent::Changed);
        assert_eq!(f.chosen(2), 1);
        f.handle(key(KeyCode::Right));
        f.handle(key(KeyCode::Right));
        assert_eq!(f.chosen(2), 0, "wraps forward");
        f.handle(key(KeyCode::Left));
        assert_eq!(f.chosen(2), 2, "wraps backward");
        f.focus = 0;
        assert_eq!(f.handle(key(KeyCode::Left)), FormEvent::Stay);
        assert_eq!(f.handle(key(KeyCode::Char('X'))), FormEvent::Changed);
        assert_eq!(f.text_of(0), b"aXb");
        assert_eq!(
            f.handle(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
            FormEvent::Changed
        );
        assert_eq!(f.text_of(0), b"b", "Ctrl+U kills to the line start");
    }

    #[test]
    fn enter_submits_from_any_field_and_esc_closes() {
        let mut f = sample();
        for i in 0..3 {
            f.focus = i;
            assert_eq!(f.handle(key(KeyCode::Enter)), FormEvent::Submit);
        }
        assert_eq!(f.handle(key(KeyCode::Esc)), FormEvent::Close);
        assert_eq!(f.handle(key(KeyCode::PageDown)), FormEvent::Unhandled);
        assert_eq!(f.handle(key(KeyCode::PageUp)), FormEvent::Unhandled);
    }

    #[test]
    fn submitted_values_are_typed() {
        let mut f = sample();
        f.paste("c\nd");
        f.handle(key(KeyCode::Tab));
        f.handle(key(KeyCode::Char(' ')));
        f.handle(key(KeyCode::Tab));
        f.handle(key(KeyCode::Left));
        assert_eq!(
            f.values(),
            vec![
                Value::Text(b"abc d".to_vec()),
                Value::Check(true),
                Value::Choice(2),
            ]
        );
    }

    #[test]
    fn a_filling_form_gives_its_body_the_rows_left() {
        let th = Theme::build(None, Depth::NoColor, false);
        let f = sample().fill();
        let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
        let mut drawn = Drawn::default();
        term.draw(|fr| {
            let area = Rect::new(2, 1, 50, 18);
            drawn = f.draw(fr, area, &th);
        })
        .unwrap();
        let body = drawn.body.expect("a body");
        // Border, line, three fields, then the body; the hint and the border below it.
        assert_eq!(body, Rect::new(3, 1 + 1 + 4, 48, 18 - 4 - 3));
        let buf = term.backend().buffer().clone();
        assert_eq!(
            buf[(2, 1)].symbol(),
            "┌",
            "the frame starts at the area's corner"
        );
        assert_eq!(buf[(51, 18)].symbol(), "┘", "and ends at its far corner");
    }

    #[test]
    fn drawing_places_the_cursor_and_the_body() {
        let th = Theme::build(None, Depth::NoColor, false);
        let mut f = sample().body(3);
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut drawn = Drawn::default();
        term.draw(|fr| drawn = f.draw(fr, fr.area(), &th)).unwrap();
        let (x, y) = drawn.cursor.expect("a focused text field has the cursor");
        let buf = term.backend().buffer().clone();
        let row: String = (0..80).map(|c| buf[(c, y)].symbol().to_string()).collect();
        assert!(row.contains("Name"), "{row}");
        assert_eq!(buf[(x - 1, y)].symbol(), "b", "the cursor follows the text");
        let body = drawn.body.expect("the body is on screen");
        assert_eq!(body.height, 3);
        assert!(body.y > y);
        f.focus = 1;
        term.draw(|fr| drawn = f.draw(fr, fr.area(), &th)).unwrap();
        assert_eq!(drawn.cursor, None, "a checkbox has no text cursor");
        let text: String = {
            let buf = term.backend().buffer();
            (0..24)
                .flat_map(|y| (0..80).map(move |x| (x, y)))
                .map(|p| buf[p].symbol().to_string())
                .collect()
        };
        assert!(text.contains("(•) one"), "{text}");
        assert!(text.contains("[ ] Recursive"), "{text}");
        // Tiny terminals clip the form; nothing panics and the cursor stays inside.
        f.focus = 0;
        f.paste("a long name that is wider than a tiny terminal");
        for (w, h) in [(1, 1), (10, 5), (30, 6), (24, 12)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|fr| drawn = f.draw(fr, fr.area(), &th)).unwrap();
            if let Some((x, y)) = drawn.cursor {
                assert!(x < w && y < h, "{w}x{h}: {x},{y}");
            }
        }
    }
}
