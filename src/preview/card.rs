#![forbid(unsafe_code)]
//! The info card (P3 4.6): for everything that is not a previewable image. The name
//! (escaped as in M1 3.2), kind, size, mtime, mode, numeric owner, a symlink's target (the
//! link is never followed, V-2), an image's pixel size, and the reason when there is no
//! image. A regular file without a NUL byte in its first 8 KiB also shows its first lines:
//! at most 64 KiB, read on the preview thread, with control characters escaped and tabs
//! expanded. A directory's card computes no size.
//!
//! The UI builds a card from the listing's row at once ([`Card::of_entry`]); the preview
//! thread's card replaces it with what only a read can tell ([`Card::of_meta`]).

use super::{BINARY_SNIFF, TEXT_HEAD};
use crate::fsops::sys::{Kind, Meta};
use crate::panel::entry::{EKind, Entry};
use crate::theme::Theme;
use crate::ui::dialog::human_size;
use crate::ui::text::{fit, name_spans};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

/// At most this many lines of the text head are kept: more than any pane shows.
const HEAD_LINES: usize = 512;
/// Tabs expand to this width.
const TAB: usize = 8;

/// What the card says about the file's content.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Head {
    /// Not read (yet), or not a regular file.
    #[default]
    None,
    /// The first lines, tabs expanded; control characters are escaped when drawn.
    Text(Vec<Vec<u8>>),
    /// A NUL byte in the first 8 KiB.
    Binary,
}

/// The info card (P3 4.6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Card {
    pub name: Vec<u8>,
    pub kind: &'static str,
    /// `None` for a directory: the card computes no size.
    pub size: Option<u64>,
    pub mtime: Option<i64>,
    /// `ls`-style mode, as `-rw-r--r--`.
    pub mode: Option<String>,
    pub uid: Option<u32>,
    /// A symlink's target, never followed.
    pub target: Option<Vec<u8>>,
    /// An image's pixel size.
    pub pixels: Option<(u32, u32)>,
    /// Why there is no image.
    pub reason: Option<String>,
    pub head: Head,
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::File => "regular file",
        Kind::Dir => "directory",
        Kind::Symlink => "symbolic link",
        Kind::Fifo => "FIFO",
        Kind::Socket => "socket",
        Kind::BlockDevice => "block device",
        Kind::CharDevice => "character device",
        Kind::Unknown => "unknown",
    }
}

fn kind_char(k: Kind) -> char {
    match k {
        Kind::File => '-',
        Kind::Dir => 'd',
        Kind::Symlink => 'l',
        Kind::Fifo => 'p',
        Kind::Socket => 's',
        Kind::BlockDevice => 'b',
        Kind::CharDevice => 'c',
        Kind::Unknown => '?',
    }
}

/// `ls`-style mode with the setuid, setgid and sticky bits.
pub fn mode_text(kind: char, perm: u32) -> String {
    let mut s = String::with_capacity(10);
    s.push(kind);
    let bits = [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ];
    for (i, (bit, c)) in bits.iter().enumerate() {
        let on = perm & bit != 0;
        let special = match i {
            2 => perm & 0o4000 != 0,
            5 => perm & 0o2000 != 0,
            8 => perm & 0o1000 != 0,
            _ => false,
        };
        s.push(match (on, special, i) {
            (true, true, 8) => 't',
            (false, true, 8) => 'T',
            (true, true, _) => 's',
            (false, true, _) => 'S',
            (true, false, _) => *c,
            (false, false, _) => '-',
        });
    }
    s
}

impl Card {
    /// A card from the listing's row, without any read: what the UI shows at once, and
    /// what stays for an entry the view does not read (V-5).
    pub fn of_entry(name: &[u8], e: &Entry) -> Card {
        let (kind, c) = match e.kind {
            EKind::File => ("regular file", '-'),
            EKind::Dir => ("directory", 'd'),
            EKind::Symlink => ("symbolic link", 'l'),
            EKind::Special => ("special file", '?'),
        };
        Card {
            name: name.to_vec(),
            kind,
            size: (e.kind != EKind::Dir).then_some(e.size),
            mtime: (e.flags & crate::panel::entry::NOTIME == 0).then_some(e.mtime),
            mode: Some(mode_text(c, e.perm as u32)),
            ..Card::default()
        }
    }

    /// A card from a `statx` of the entry (the preview thread's, V-2).
    pub fn of_meta(name: &[u8], m: &Meta) -> Card {
        Card {
            name: name.to_vec(),
            kind: kind_name(m.kind),
            size: (m.kind != Kind::Dir).then_some(m.size),
            mtime: Some(m.mtime.sec),
            mode: Some(mode_text(kind_char(m.kind), m.perm)),
            uid: Some(m.uid),
            ..Card::default()
        }
    }

    pub fn with_reason(mut self, why: impl Into<String>) -> Card {
        self.reason = Some(why.into());
        self
    }

    /// Draws the card into `area`.
    pub fn render(&self, buf: &mut Buffer, area: Rect, th: &Theme, tz: &jiff::tz::TimeZone) {
        let w = area.width as usize;
        if w == 0 || area.height == 0 {
            return;
        }
        let mut lines: Vec<Line<'static>> = Vec::new();
        let (name, _) = name_spans(&self.name, th.directory, th.escaped, w);
        lines.push(Line::from(name));
        let label = |l: &str, v: String| -> Line<'static> {
            let text = fit(&format!("{l:<9}{v}"), w).0;
            Line::from(vec![
                Span::styled(text[..l.len().min(text.len())].to_string(), th.metadata),
                Span::styled(text[l.len().min(text.len())..].to_string(), th.normal),
            ])
        };
        lines.push(label("kind", self.kind.to_string()));
        if let Some(s) = self.size {
            let text = if s < 10_000 {
                format!("{s} bytes")
            } else {
                format!("{} ({s} bytes)", human_size(s))
            };
            lines.push(label("size", text));
        }
        if let Some(t) = self.mtime {
            let s = jiff::Timestamp::new(t, 0)
                .map(|t| {
                    t.to_zoned(tz.clone())
                        .strftime("%Y-%m-%d %H:%M:%S")
                        .to_string()
                })
                .unwrap_or_default();
            lines.push(label("modified", s));
        }
        if let Some(m) = &self.mode {
            lines.push(label("mode", m.clone()));
        }
        if let Some(u) = self.uid {
            lines.push(label("owner", u.to_string()));
        }
        if let Some(t) = &self.target {
            let mut spans = vec![Span::styled("target   ", th.metadata)];
            spans.extend(name_spans(t, th.symlink, th.escaped, w.saturating_sub(9)).0);
            lines.push(Line::from(spans));
        }
        if let Some((pw, ph)) = self.pixels {
            lines.push(label("pixels", format!("{pw} x {ph} px")));
        }
        if let Some(r) = &self.reason {
            lines.push(Line::from(Span::styled(fit(r, w).0, th.warning)));
        }
        match &self.head {
            Head::None => {}
            Head::Binary => lines.push(Line::from(Span::styled("binary file", th.metadata))),
            Head::Text(text) => {
                lines.push(Line::default());
                for l in text {
                    if lines.len() >= area.height as usize {
                        break;
                    }
                    lines.push(Line::from(name_spans(l, th.normal, th.escaped, w).0));
                }
            }
        }
        lines.truncate(area.height as usize);
        Paragraph::new(lines).style(th.background).render(area, buf);
    }
}

/// The text head of `bytes`, the first bytes of a regular file (P3 4.6): binary when the
/// first 8 KiB hold a NUL, else its first lines from at most 64 KiB, tabs expanded. A last
/// line cut by the 64 KiB bound is kept.
pub fn text_head(bytes: &[u8]) -> Head {
    if bytes[..bytes.len().min(BINARY_SNIFF)].contains(&0) {
        return Head::Binary;
    }
    let bytes = &bytes[..bytes.len().min(TEXT_HEAD)];
    let mut lines = Vec::new();
    for raw in bytes.split(|&b| b == b'\n').take(HEAD_LINES) {
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let mut l = Vec::with_capacity(raw.len());
        let mut col = 0;
        for chunk in raw.utf8_chunks() {
            for c in chunk.valid().chars() {
                if c == '\t' {
                    let n = TAB - col % TAB;
                    l.extend(std::iter::repeat_n(b' ', n));
                    col += n;
                } else {
                    let mut b = [0u8; 4];
                    l.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                    col += 1;
                }
            }
            l.extend_from_slice(chunk.invalid());
            col += chunk.invalid().len();
        }
        lines.push(l);
    }
    // A file that ends with a newline has no line after it.
    if bytes.ends_with(b"\n") && lines.last().is_some_and(Vec::is_empty) {
        lines.pop();
    }
    Head::Text(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_read_like_ls() {
        assert_eq!(mode_text('-', 0o644), "-rw-r--r--");
        assert_eq!(mode_text('-', 0o4755), "-rwsr-xr-x");
        assert_eq!(mode_text('d', 0o1777), "drwxrwxrwt");
        assert_eq!(mode_text('-', 0o2640), "-rw-r-S---");
    }

    #[test]
    fn the_text_head_expands_tabs_and_spots_binary() {
        assert_eq!(
            text_head(b"a\tb\r\nxy\tz\n"),
            Head::Text(vec![b"a       b".to_vec(), b"xy      z".to_vec()])
        );
        assert_eq!(text_head(b"PK\x03\x04\0\0"), Head::Binary);
        let mut late = vec![b'a'; BINARY_SNIFF];
        late.push(0);
        assert!(
            matches!(text_head(&late), Head::Text(_)),
            "a NUL after 8 KiB"
        );
        assert_eq!(text_head(b""), Head::Text(vec![vec![]]));
    }
}
