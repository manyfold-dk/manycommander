#![forbid(unsafe_code)]
//! Widgets: panels, dialogs, progress, function-key bar (design section 3). `draw` renders
//! the whole frame from `App` state; it makes no filesystem syscalls.

pub mod dialog;
pub mod dirs;
pub mod form;
pub mod help;
pub mod multirename;
pub mod panel;
pub mod tabs;
pub mod text;

use crate::app::App;
use crate::cmdline;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::os::unix::ffi::OsStrExt;
use text::{escaped, fit, fit_left};

const FKEYS: [(&str, &str); 10] = [
    ("1", "Help"),
    ("2", ""),
    ("3", "View"),
    ("4", "Edit"),
    ("5", "Copy"),
    ("6", "Move"),
    ("7", "Mkdir"),
    ("8", "Trash"),
    ("9", ""),
    ("10", "Quit"),
];

pub fn draw(app: &mut App, f: &mut Frame) {
    let area = f.area();
    f.render_widget(
        ratatui::widgets::Block::default().style(app.theme.background),
        area,
    );
    let status_row = app.job.is_some()
        || app.view.is_some()
        || app.status.is_some()
        || app.search.is_some()
        || app.filter_line.is_some()
        || app.compare.is_some();
    let rows = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(status_row as u16),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);
    let halves =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(rows[0]);
    app.page = (rows[0].height as usize).saturating_sub(3).max(1);
    for s in 0..2 {
        let active = s == app.active;
        let bar = tabs::bar(
            &app.sides[s],
            &app.theme,
            halves[s].width.saturating_sub(2) as usize,
        );
        let th = app.theme.clone();
        let tz = app.tz.clone();
        panel::draw(
            app.sides[s].panel_mut(),
            f,
            halves[s],
            active,
            &th,
            &tz,
            bar,
        );
    }
    let mut cursor = None;
    if status_row {
        cursor = draw_status(app, f, rows[1]);
    }
    let cursor = cursor.or(draw_cmdline(app, f, rows[2]));
    draw_fkeys(app, f, rows[3]);
    let mut dcursor = None;
    if let Some(d) = &app.dialog {
        dcursor = dialog::draw(d, f, rows[0], &app.theme, &app.tz);
    }
    if let Some(c) = dcursor.or(if app.dialog.is_none() { cursor } else { None }) {
        f.set_cursor_position(c);
    }
}

/// The status row: quick search, the filter line, a view copy's, a job's or a compare's
/// progress, or the last status message. Returns the cursor position while the filter line is open.
fn draw_status(app: &App, f: &mut Frame, r: Rect) -> Option<(u16, u16)> {
    let w = r.width as usize;
    if app.search.is_none()
        && let Some(l) = &app.filter_line
    {
        let (prompt, pw) = ("Filter: ", 8);
        let (shown, cur) = line_view(l, w.saturating_sub(pw + 1));
        let line = Line::from(vec![
            Span::styled(prompt, app.theme.prompt),
            Span::styled(shown, app.theme.normal),
        ]);
        f.render_widget(Paragraph::new(line), r);
        return Some((r.x + (pw + cur).min(w.saturating_sub(1)) as u16, r.y));
    }
    let (text, style) = if let Some(s) = &app.search {
        (format!("Quick search: {}", escaped(s)), app.theme.prompt)
    } else if let Some(v) = app.view_line() {
        (v, app.theme.warning)
    } else if let Some(j) = app.job_line() {
        (j, app.theme.warning)
    } else if let Some(c) = app.compare_line() {
        (c, app.theme.warning)
    } else if let Some(s) = &app.status {
        (
            s.text.clone(),
            if s.error {
                app.theme.error
            } else {
                app.theme.normal
            },
        )
    } else {
        return None;
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(fit(&text, w).0, style))),
        r,
    );
    None
}

/// The text of `l` shown in `room` columns, scrolled so the cursor stays visible, and the
/// cursor's column in it.
fn line_view(l: &cmdline::Line, room: usize) -> (String, usize) {
    let bytes = l.bytes();
    let before = escaped(&bytes[..l.cursor()]);
    let bw = unicode_width::UnicodeWidthStr::width(before.as_str());
    if bw < room {
        (fit(&escaped(bytes), room).0, bw)
    } else {
        let tail = fit_left(&before, room.saturating_sub(1));
        let tw = unicode_width::UnicodeWidthStr::width(tail.as_str());
        (tail, tw)
    }
}

/// The command line: `<dir>$ <text>`. Returns the cursor position.
fn draw_cmdline(app: &App, f: &mut Frame, r: Rect) -> Option<(u16, u16)> {
    let w = r.width as usize;
    let dir = escaped(app.panel().dir.as_os_str().as_bytes());
    let prompt = format!("{}$ ", fit_left(&dir, w / 3));
    let pw = prompt.chars().count();
    let (shown, cur) = line_view(&app.line, w.saturating_sub(pw + 1));
    let line = Line::from(vec![
        Span::styled(prompt, app.theme.prompt),
        Span::styled(shown, app.theme.normal),
    ]);
    f.render_widget(Paragraph::new(line).style(app.theme.background), r);
    Some((r.x + (pw + cur).min(w.saturating_sub(1)) as u16, r.y))
}

fn draw_fkeys(app: &App, f: &mut Frame, r: Rect) {
    let w = r.width as usize;
    let slot = (w / 10).max(3);
    let mut spans = Vec::new();
    for (i, (n, label)) in FKEYS.iter().enumerate() {
        let width = if i == 9 {
            w.saturating_sub(slot * 9)
        } else {
            slot
        };
        if width == 0 {
            break;
        }
        let nw = n.len().min(width);
        spans.push(Span::styled(n[..nw].to_string(), app.theme.fkey_number));
        let lw = width - nw;
        let (l, lwid) = fit(label, lw);
        spans.push(Span::styled(
            format!("{l}{}", " ".repeat(lw - lwid)),
            app.theme.fkey_label,
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), r);
}
