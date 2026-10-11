#![forbid(unsafe_code)]
//! The tab bar (TDR-1 5): a panel with more than one tab shows it above its header. When the
//! labels do not all fit, the bar shows a run of them that holds the active tab: as many
//! before it as fit, then as many after it.

use crate::app::Side;
use crate::theme::Theme;
use ratatui::text::{Line, Span};

pub fn bar(side: &Side, th: &Theme, width: usize) -> Option<Line<'static>> {
    if side.tabs.len() < 2 {
        return None;
    }
    let labels: Vec<String> = side
        .tabs
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let name = match p.search() {
                // A results tab (TDR-2 5.1) is named by its search.
                Some(s) => s.title(),
                None => p
                    .dir
                    .file_name()
                    .map(|n| super::text::escaped(std::os::unix::ffi::OsStrExt::as_bytes(n)))
                    .unwrap_or_else(|| "/".into()),
            };
            format!(" {}:{} ", i + 1, super::text::fit(&name, 16).0)
        })
        .collect();
    let lw = |l: &String| unicode_width::UnicodeWidthStr::width(l.as_str());
    let active = side.active.min(labels.len() - 1);
    // The active label, then the ones before it that fit, then the ones after it.
    let mut used = lw(&labels[active]);
    let mut first = active;
    while first > 0 && used + lw(&labels[first - 1]) <= width {
        first -= 1;
        used += lw(&labels[first]);
    }
    let mut end = active + 1;
    while end < labels.len() && used + lw(&labels[end]) <= width {
        used += lw(&labels[end]);
        end += 1;
    }
    let spans = labels[first..end]
        .iter()
        .enumerate()
        .map(|(k, label)| {
            if first + k == active {
                // Cut only when the active label alone is wider than the bar.
                Span::styled(super::text::fit(label, width).0, th.cursor_active)
            } else {
                Span::styled(label.clone(), th.metadata)
            }
        })
        .collect::<Vec<_>>();
    Some(Line::from(spans))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::Panel;
    use crate::theme::Depth;

    fn side(n: usize, active: usize) -> Side {
        Side {
            tabs: (0..n)
                .map(|i| Panel::new(i, format!("/d/tab{i}").into()))
                .collect(),
            active,
        }
    }

    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.to_string()).collect()
    }

    /// With more tabs than fit, the active tab is always on the bar.
    #[test]
    fn the_active_tab_is_always_shown() {
        let th = Theme::build(None, Depth::NoColor, false);
        // Each label is " k:tabN " (8 or 10 columns).
        for active in 0..12 {
            let s = side(12, active);
            let bar = bar(&s, &th, 30).expect("a bar");
            let t = text(&bar);
            let want = format!(" {}:tab{} ", active + 1, active);
            assert!(t.contains(&want), "{active}: {t:?}");
            assert!(
                unicode_width::UnicodeWidthStr::width(t.as_str()) <= 30,
                "{t:?}"
            );
            let on = bar
                .spans
                .iter()
                .filter(|sp| sp.style == th.cursor_active)
                .count();
            assert_eq!(on, 1, "{active}");
        }
        // From the first tab, the bar shows the tabs that follow.
        let t = text(&bar(&side(12, 0), &th, 30).unwrap());
        assert!(t.starts_with(" 1:tab0  2:tab1 "), "{t:?}");
        // The last tab shows with the ones before it.
        let t = text(&bar(&side(12, 11), &th, 30).unwrap());
        assert!(t.ends_with(" 11:tab10  12:tab11 "), "{t:?}");
        // A bar narrower than the active label cuts that label.
        let t = text(&bar(&side(3, 2), &th, 4).unwrap());
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(t.as_str()),
            4,
            "{t:?}"
        );
    }
}
