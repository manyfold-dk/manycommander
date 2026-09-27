#![forbid(unsafe_code)]
//! Name display (design 3.2): names are shown lossily. Control characters are escaped
//! (a newline shows as `\n`) and invalid UTF-8 bytes as `\xNN`, in a distinct style.

use ratatui::style::Style;
use ratatui::text::Span;
use unicode_width::UnicodeWidthChar;

fn escape_char(c: char) -> Option<String> {
    match c {
        '\n' => Some("\\n".into()),
        '\t' => Some("\\t".into()),
        '\r' => Some("\\r".into()),
        c if (c as u32) < 0x20 || c as u32 == 0x7f || (0x80..0xa0).contains(&(c as u32)) => {
            Some(format!("\\x{:02x}", c as u32))
        }
        _ => None,
    }
}

/// The name as styled spans, cut to `max` columns (a cut shows as `~`). Returns the spans
/// and their width.
pub fn name_spans(name: &[u8], base: Style, esc: Style, max: usize) -> (Vec<Span<'static>>, usize) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut width = 0;
    let mut plain = String::new();
    let mut cut = false;
    let flush = |plain: &mut String, spans: &mut Vec<Span<'static>>| {
        if !plain.is_empty() {
            spans.push(Span::styled(std::mem::take(plain), base));
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
                spans.push(Span::styled(e, esc));
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
            if width + 4 > max {
                cut = true;
                break 'outer;
            }
            flush(&mut plain, &mut spans);
            width += 4;
            spans.push(Span::styled(format!("\\x{b:02x}"), esc));
        }
    }
    flush(&mut plain, &mut spans);
    if cut && max > 0 {
        // Make room for the cut marker.
        while width + 1 > max {
            let Some(last) = spans.last_mut() else { break };
            let mut s = last.content.to_string();
            match s.pop() {
                Some(c) => width -= c.width().unwrap_or(0).max(if c.is_ascii() { 1 } else { 0 }),
                None => {
                    spans.pop();
                    continue;
                }
            }
            if s.is_empty() {
                spans.pop();
            } else {
                last.content = s.into();
            }
        }
        spans.push(Span::styled("~", esc));
        width += 1;
    }
    (spans, width)
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
}
