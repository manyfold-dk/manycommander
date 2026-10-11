#![forbid(unsafe_code)]
//! Name display (design 3.2): names are shown lossily. Control characters are escaped
//! (a newline shows as `\n`), bidirectional controls and the line and paragraph separators
//! as `\u{202e}`, and invalid UTF-8 bytes as `\xNN`, in a distinct style.

use ratatui::style::Style;
use ratatui::text::Span;
use unicode_width::UnicodeWidthChar;

/// Characters that rearrange the row they are drawn in. The bidirectional formatting
/// characters (the embeddings, overrides and isolates, and the marks) reorder the text around
/// them: a name such as `invoice<U+202E>fdp.exe` would read as `invoiceexe.pdf`. The line and
/// paragraph separators count as one column, and a terminal may start a new line on them.
fn rearranges(c: char) -> bool {
    matches!(
        c,
        '\u{61c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// Whether `c` is shown escaped: a control character (C0, DEL, C1), or one that rearranges
/// the row.
pub fn is_escaped(c: char) -> bool {
    c.is_control() || rearranges(c)
}

fn escape_char(c: char) -> Option<String> {
    match c {
        '\n' => Some("\\n".into()),
        '\t' => Some("\\t".into()),
        '\r' => Some("\\r".into()),
        c if c.is_control() => Some(format!("\\x{:02x}", c as u32)),
        c if rearranges(c) => Some(c.escape_unicode().to_string()),
        _ => None,
    }
}

/// The columns an invalid UTF-8 byte takes when drawn, as `\xNN`.
pub const INVALID_BYTE_WIDTH: usize = 4;

/// The columns `c` takes when drawn: its escape's length when it is escaped, else its
/// display width.
pub fn drawn_width(c: char) -> usize {
    escape_char(c).map_or_else(|| c.width().unwrap_or(0), |e| e.len())
}

/// The extension of a file named `name`, lowercased: what follows its last dot. A name
/// whose only dot is its first byte (`.md`, a hidden file) has none.
pub fn extension(name: &[u8]) -> Option<Vec<u8>> {
    let dot = name.iter().rposition(|&b| b == b'.')?;
    (dot > 0).then(|| name[dot + 1..].to_ascii_lowercase())
}

/// The name as styled spans, cut to `max` columns (a cut shows as `~`). Returns the spans
/// and their width.
pub fn name_spans(name: &[u8], base: Style, esc: Style, max: usize) -> (Vec<Span<'static>>, usize) {
    // Each span with whether it is one escape.
    let mut spans: Vec<(Span<'static>, bool)> = Vec::new();
    let mut width = 0;
    let mut plain = String::new();
    let mut cut = false;
    let flush = |plain: &mut String, spans: &mut Vec<(Span<'static>, bool)>| {
        if !plain.is_empty() {
            spans.push((Span::styled(std::mem::take(plain), base), false));
        }
    };
    'outer: for chunk in name.utf8_chunks() {
        for c in chunk.valid().chars() {
            if let Some(e) = escape_char(c) {
                if width + e.len() > max {
                    cut = true;
                    break 'outer;
                }
                flush(&mut plain, &mut spans);
                width += e.len();
                spans.push((Span::styled(e, esc), true));
            } else {
                let w = c.width().unwrap_or(0);
                if width + w > max {
                    cut = true;
                    break 'outer;
                }
                width += w;
                plain.push(c);
            }
        }
        for b in chunk.invalid() {
            if width + INVALID_BYTE_WIDTH > max {
                cut = true;
                break 'outer;
            }
            flush(&mut plain, &mut spans);
            width += INVALID_BYTE_WIDTH;
            spans.push((Span::styled(format!("\\x{b:02x}"), esc), true));
        }
    }
    flush(&mut plain, &mut spans);
    if cut && max > 0 {
        // Make room for the cut marker: plain text loses characters from its end, an escape
        // goes whole (half of `\x1b` would read as another byte).
        while width + 1 > max {
            let Some((last, escape)) = spans.last_mut() else {
                break;
            };
            if *escape {
                width -= last.content.len();
                spans.pop();
                continue;
            }
            let mut s = last.content.to_string();
            if let Some(c) = s.pop() {
                width -= c.width().unwrap_or(0);
            }
            if s.is_empty() {
                spans.pop();
            } else {
                last.content = s.into();
            }
        }
        spans.push((Span::styled("~", esc), true));
        width += 1;
    }
    (spans.into_iter().map(|(s, _)| s).collect(), width)
}

/// The whole name as one escaped string (paths in dialogs and titles).
pub fn escaped(name: &[u8]) -> String {
    let mut out = String::new();
    for chunk in name.utf8_chunks() {
        for c in chunk.valid().chars() {
            match escape_char(c) {
                Some(e) => out.push_str(&e),
                None => out.push(c),
            }
        }
        for b in chunk.invalid() {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out
}

/// Cuts `s` to `max` columns (a cut shows as `~`). Returns the text and its width.
pub fn fit(s: &str, max: usize) -> (String, usize) {
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > max {
            while w + 1 > max {
                match out.pop() {
                    Some(p) => w -= p.width().unwrap_or(0),
                    None => break,
                }
            }
            if max > 0 {
                out.push('~');
                w += 1;
            }
            return (out, w);
        }
        w += cw;
        out.push(c);
    }
    (out, w)
}

/// Cuts from the left, keeping the end (`~/long/path` shows its last components).
pub fn fit_left(s: &str, max: usize) -> String {
    let total: usize = s.chars().map(|c| c.width().unwrap_or(0)).sum();
    if total <= max {
        return s.to_string();
    }
    if max == 0 {
        // Not even the cut marker fits.
        return String::new();
    }
    let mut w = 0;
    let mut rev = Vec::new();
    for c in s.chars().rev() {
        let cw = c.width().unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        w += cw;
        rev.push(c);
    }
    let mut out = String::from("~");
    out.extend(rev.into_iter().rev());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes() {
        assert_eq!(escaped(b"a\nb"), "a\\nb");
        assert_eq!(escaped(b"bad\xff"), "bad\\xff");
        assert_eq!(escaped("é\u{1b}".as_bytes()), "é\\x1b");
        let (s, w) = name_spans(b"new\nline", Style::new(), Style::new(), 100);
        assert_eq!(w, 9);
        assert_eq!(s.len(), 3);
        let (s, w) = name_spans(b"abcdefgh", Style::new(), Style::new(), 5);
        assert_eq!(w, 5);
        let text: String = s.iter().map(|x| x.content.to_string()).collect();
        assert_eq!(text, "abcd~");
        assert_eq!(fit("abcdef", 4).0, "abc~");
        assert_eq!(fit_left("/home/user/dir", 8), "~ser/dir");
    }

    /// A bidirectional control would reorder what follows it: `invoice<U+202E>fdp.exe` would
    /// read as a PDF. A line or paragraph separator would break the row.
    #[test]
    fn bidi_controls_and_separators_are_escaped() {
        assert_eq!(
            escaped("invoice\u{202e}fdp.exe".as_bytes()),
            "invoice\\u{202e}fdp.exe"
        );
        // The line and paragraph separators count as one column, and a terminal may start a
        // new line on them.
        for c in [
            '\u{61c}', '\u{200e}', '\u{200f}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}',
            '\u{202e}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}', '\u{2028}', '\u{2029}',
        ] {
            let s = c.to_string();
            assert_eq!(
                escaped(s.as_bytes()),
                c.escape_unicode().to_string(),
                "{c:?}"
            );
            assert!(is_escaped(c), "{c:?}");
        }
        assert!(is_escaped('\u{1b}') && is_escaped('\u{85}'));
        assert!(!is_escaped('a') && !is_escaped('\u{5d0}'));
        // Right-to-left letters are shown as they are.
        assert_eq!(escaped("\u{5d0}\u{5d1}".as_bytes()), "\u{5d0}\u{5d1}");
        let (s, w) = name_spans(
            "a\u{202e}b".as_bytes(),
            Style::new(),
            Style::new().bold(),
            100,
        );
        let text: Vec<&str> = s.iter().map(|x| x.content.as_ref()).collect();
        assert_eq!(text, ["a", "\\u{202e}", "b"]);
        assert_eq!(s[1].style, Style::new().bold(), "in the escape style");
        assert_eq!(w, 10);
    }

    /// A cut never splits an escape: half of `\x1b` would read as another byte.
    #[test]
    fn a_cut_drops_a_whole_escape() {
        let (s, w) = name_spans(b"ab\x1bcdef", Style::new(), Style::new(), 6);
        let text: String = s.iter().map(|x| x.content.to_string()).collect();
        assert_eq!((text.as_str(), w), ("ab~", 3));
        let (s, w) = name_spans(b"a\xffbcd", Style::new(), Style::new(), 5);
        let text: String = s.iter().map(|x| x.content.to_string()).collect();
        assert_eq!((text.as_str(), w), ("a~", 2));
        // A plain span still loses only its last characters.
        let (s, w) = name_spans("ab日本語".as_bytes(), Style::new(), Style::new(), 6);
        let text: String = s.iter().map(|x| x.content.to_string()).collect();
        assert_eq!((text.as_str(), w), ("ab日~", 5));
    }

    #[test]
    fn nothing_fits_in_no_columns() {
        assert_eq!(fit_left("/home/user/dir", 0), "");
        assert_eq!(fit_left("", 0), "");
        assert_eq!(fit_left("/home/user/dir", 1), "~");
        assert_eq!(fit("abc", 0), (String::new(), 0));
        let (s, w) = name_spans(b"abc", Style::new(), Style::new(), 0);
        assert!(s.is_empty() && w == 0);
    }

    #[test]
    fn extensions() {
        assert_eq!(extension(b"Photo.JPG").as_deref(), Some(&b"jpg"[..]));
        assert_eq!(extension(b"a.tar.gz").as_deref(), Some(&b"gz"[..]));
        assert_eq!(extension(b"trailing.").as_deref(), Some(&b""[..]));
        assert_eq!(extension(b".md"), None, "a hidden file named md");
        assert_eq!(extension(b"noext"), None);
    }

    /// The columns a character of a text head takes when drawn.
    #[test]
    fn drawn_widths() {
        assert_eq!(drawn_width('a'), 1);
        assert_eq!(drawn_width('日'), 2);
        assert_eq!(drawn_width('\u{1b}'), 4);
        assert_eq!(drawn_width('\n'), 2);
        assert_eq!(drawn_width('\u{202e}'), 8);
        assert_eq!(INVALID_BYTE_WIDTH, escaped(b"\xff").len());
    }
}
