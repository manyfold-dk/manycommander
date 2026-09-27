#![forbid(unsafe_code)]
//! The tab bar (M2): a panel with more than one tab shows it above its header.

use crate::app::Side;
use crate::theme::Theme;
use ratatui::text::{Line, Span};

pub fn bar(side: &Side, th: &Theme, width: usize) -> Option<Line<'static>> {
    if side.tabs.len() < 2 {
        return None;
    }
    let mut spans = Vec::new();
    let mut used = 0;
    for (i, p) in side.tabs.iter().enumerate() {
        let name = p
            .dir
            .file_name()
            .map(|n| super::text::escaped(std::os::unix::ffi::OsStrExt::as_bytes(n)))
            .unwrap_or_else(|| "/".into());
        let label = format!(" {}:{} ", i + 1, super::text::fit(&name, 16).0);
        let lw = unicode_width::UnicodeWidthStr::width(label.as_str());
        if used + lw > width {
            break;
        }
        used += lw;
        let style = if i == side.active {
            th.cursor_active
        } else {
            th.metadata
        };
        spans.push(Span::styled(label, style));
    }
    Some(Line::from(spans))
}
