#![forbid(unsafe_code)]
//! Panel rendering (design section 5): name, extension, size, mtime and mode columns;
//! only visible rows are rendered (P-1); columns drop below 80x24 without a panic
//! (NFR-TERM).

use super::dialog::human_size;
use super::text::{escaped, fit, fit_left, name_spans};
use crate::panel::entry::{EKind, Entry, LinkKind, SIZED, mode_string};
use crate::panel::{Panel, Row};
use crate::theme::Theme;
use crate::theme::roles::MARK_GLYPH;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use std::os::unix::ffi::OsStrExt;

#[derive(Clone, Copy)]
struct Cols {
    ext: usize,
    size: usize,
    date: usize,
    mode: usize,
}

fn columns(w: usize) -> Cols {
    let c = |ext, size, date, mode| Cols {
        ext,
        size,
        date,
        mode,
    };
    if w >= 90 {
        c(6, 9, 16, 10)
    } else if w >= 60 {
        c(6, 9, 16, 0)
    } else if w >= 36 {
        c(0, 7, 11, 0)
    } else if w >= 20 {
        c(0, 7, 0, 0)
    } else {
        c(0, 0, 0, 0)
    }
}

fn pad(s: &str, w: usize, right: bool) -> String {
    let (t, tw) = fit(s, w);
    let fill = " ".repeat(w - tw);
    if right {
        format!("{fill}{t}")
    } else {
        format!("{t}{fill}")
    }
}

fn size_text(e: &Entry, w: usize) -> String {
    let s = match e.kind {
        EKind::Dir if e.flags & SIZED != 0 => human_size(e.size),
        EKind::Dir => "<DIR>".into(),
        EKind::Symlink if e.link == LinkKind::Dir => "<LNK>".into(),
        _ if w < 9 => human_size(e.size),
        _ if e.size < 1_000_000_000 => e.size.to_string(),
        _ => human_size(e.size),
    };
    pad(&s, w, true)
}

fn date_text(e: &Entry, w: usize, tz: &jiff::tz::TimeZone) -> String {
    let fmt = if w >= 16 {
        "%Y-%m-%d %H:%M"
    } else {
        "%m-%d %H:%M"
    };
    let s = jiff::Timestamp::new(e.mtime, 0)
        .map(|t| t.to_zoned(tz.clone()).strftime(fmt).to_string())
        .unwrap_or_default();
    pad(&s, w, false)
}

fn name_style(e: &Entry, th: &Theme) -> Style {
    if e.marked() {
        return th.marked;
    }
    let s = match e.kind {
        EKind::Dir => th.directory,
        EKind::Symlink if e.link == LinkKind::Broken => th.broken_symlink,
        EKind::Symlink => th.symlink,
        EKind::File if e.exec() => th.executable,
        _ => th.normal,
    };
    if e.hidden() && e.kind != EKind::Dir {
        th.hidden
    } else {
        s
    }
}

/// Draws one panel. `active`: the panel has the focus.
pub fn draw(
    p: &mut Panel,
    f: &mut Frame,
    area: Rect,
    active: bool,
    th: &Theme,
    tz: &jiff::tz::TimeZone,
    tab_bar: Option<Line<'static>>,
) {
    p.ensure_sorted();
    let border = if active {
        th.border_active
    } else {
        th.border_inactive
    };
    let title_w = area.width.saturating_sub(4) as usize;
    let mut title = fit_left(&escaped(p.dir.as_os_str().as_bytes()), title_w);
    if p.is_loading() {
        title = fit_left(&format!("{title} (loading)"), title_w);
    }
    let footer = footer(p, area.width.saturating_sub(4) as usize);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(border)
        .style(th.background)
        .title(Span::styled(format!(" {title} "), border))
        .title_bottom(Span::styled(format!(" {footer} "), border));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let mut y = inner.y;
    let mut height = inner.height as usize;
    if let Some(bar) = tab_bar {
        f.render_widget(Paragraph::new(bar), Rect { height: 1, ..inner });
        y += 1;
        height = height.saturating_sub(1);
    }
    let w = inner.width as usize;
    let cols = columns(w);
    let fixed = [cols.ext, cols.size, cols.date, cols.mode]
        .iter()
        .filter(|&&c| c > 0)
        .map(|c| c + 1)
        .sum::<usize>();
    let name_w = w.saturating_sub(fixed + 1);
    // Header.
    if height > 0 {
        let mut h = pad("Name", name_w + 1, false);
        if cols.ext > 0 {
            h += &format!(" {}", pad("Ext", cols.ext, false));
        }
        if cols.size > 0 {
            h += &format!(" {}", pad("Size", cols.size, true));
        }
        if cols.date > 0 {
            h += &format!(" {}", pad("Modified", cols.date, false));
        }
        if cols.mode > 0 {
            h += &format!(" {}", pad("Mode", cols.mode, false));
        }
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(fit(&h, w).0, th.metadata))),
            Rect {
                y,
                height: 1,
                ..inner
            },
        );
        y += 1;
        height -= 1;
    }
    p.scroll_into_view(height);
    let mut lines = Vec::with_capacity(height);
    if let Some(msg) = &p.message
        && p.rows() == 0
    {
        lines.push(Line::from(Span::styled(fit(msg, w).0, th.error)));
    }
    for r in p.top..(p.top + height).min(p.rows()) {
        let row = p.row(r).unwrap();
        let cursor = r == p.cursor;
        let mut spans: Vec<Span> = Vec::new();
        let used;
        match row {
            Row::Parent => {
                spans.push(Span::raw(" "));
                spans.push(Span::styled("..", th.directory));
                used = 3;
                let mut rest = " ".repeat(name_w + 1 - used.min(name_w + 1));
                if cols.ext > 0 {
                    rest += &" ".repeat(cols.ext + 1);
                }
                if cols.size > 0 {
                    rest += &format!(" {}", pad("<UP>", cols.size, true));
                }
                spans.push(Span::styled(rest, th.metadata));
            }
            Row::Entry(i) => {
                let e = p.list.entries[i as usize];
                let name = e.name(&p.list.names);
                let ns = name_style(&e, th);
                spans.push(if e.marked() {
                    Span::styled(MARK_GLYPH, th.marked)
                } else {
                    Span::raw(" ")
                });
                // With a separate extension column, the name shows without it.
                let shown = if cols.ext > 0 && !e.ext(&p.list.names).is_empty() {
                    &name[..name.len() - e.ext(&p.list.names).len() - 1]
                } else {
                    name
                };
                let (ns_spans, nw) = name_spans(shown, ns, th.escaped, name_w);
                spans.extend(ns_spans);
                used = 1 + nw;
                spans.push(Span::raw(" ".repeat(name_w + 1 - used)));
                let mut meta = String::new();
                if cols.ext > 0 {
                    meta += &format!(" {}", pad(&escaped(e.ext(&p.list.names)), cols.ext, false));
                }
                if cols.size > 0 {
                    meta += &format!(" {}", size_text(&e, cols.size));
                }
                if cols.date > 0 {
                    meta += &format!(" {}", date_text(&e, cols.date, tz));
                }
                if cols.mode > 0 {
                    meta += &format!(" {}", pad(&mode_string(&e), cols.mode, false));
                }
                spans.push(Span::styled(meta, th.metadata));
            }
        }
        let mut line = Line::from(spans);
        if cursor {
            let cs = if active {
                th.cursor_active
            } else {
                th.cursor_inactive
            };
            // A Line's style does not override its spans' own colours, so the cursor style
            // goes on every span.
            for s in &mut line.spans {
                s.style = s.style.patch(cs);
            }
            // Fill the rest of the row with the cursor colour.
            let lw = line.width();
            if lw < w {
                line.spans.push(Span::styled(" ".repeat(w - lw), cs));
            }
        }
        lines.push(line);
    }
    f.render_widget(
        Paragraph::new(lines).style(th.background),
        Rect {
            y,
            height: height as u16,
            ..inner
        },
    );
}

fn footer(p: &Panel, w: usize) -> String {
    let mut parts = Vec::new();
    if p.marked > 0 {
        parts.push(format!(
            "{} marked, {}",
            p.marked,
            human_size(p.marked_bytes)
        ));
    } else {
        let n = p.list.entries.len();
        parts.push(format!("{n} {}", if n == 1 { "entry" } else { "entries" }));
    }
    if let Some((free, _)) = p.free {
        parts.push(format!("{} free", human_size(free)));
    }
    if let Some(m) = &p.message
        && p.rows() > 0
    {
        parts.push(m.clone());
    }
    fit(&parts.join(", "), w).0
}
