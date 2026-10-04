#![forbid(unsafe_code)]
//! The Markdown view of the info card (P3 4.6, amendment of 2026-10-05). The preview thread
//! parses a Markdown file's text head once ([`parse`]) into blocks of styled text with their
//! prefixes; the card wraps them at the pane's width when it draws ([`lines`]). Headings,
//! emphasis, code, lists, block quotes, rules, links, images and tables are shown; HTML shows
//! as its text. Control characters are escaped as in names (M1 3.2).

use crate::theme::Theme;
use crate::ui::text::escaped;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// How a piece of text looks; resolved against the theme when it is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Text,
    Strong,
    Emph,
    StrongEmph,
    Strike,
    Code,
    /// A level 1 heading: bold and underlined.
    Title,
    Heading,
    Link,
    /// Bullets, quote bars, rules, link addresses, image text, table bars.
    Meta,
}

/// One block of the rendered text: the prefix of its first line, the prefix of the lines it
/// wraps to, and its text. A code line is cut instead of wrapped; a rule fills the width.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub first: String,
    pub rest: String,
    pub spans: Vec<(Class, String)>,
    pub wrap: bool,
    pub rule: bool,
}

impl Block {
    fn new(first: String, rest: String) -> Block {
        Block {
            first,
            rest,
            spans: Vec::new(),
            wrap: true,
            rule: false,
        }
    }

    fn blank() -> Block {
        Block::new(String::new(), String::new())
    }
}

/// Where the parser is in the document's structure.
#[derive(Default)]
struct State {
    blocks: Vec<Block>,
    open: Option<Block>,
    /// One entry per open list: the next number of an ordered list, `None` for bullets.
    lists: Vec<Option<u64>>,
    /// The marker the next line of the current item starts with, until it is used.
    marker: Option<String>,
    quotes: usize,
    strong: usize,
    emph: usize,
    strike: usize,
    /// The level of the open heading, 0 outside one.
    heading: u8,
    /// The addresses of the open links, shown after their text.
    links: Vec<String>,
    image: usize,
    code: bool,
    table: Option<Vec<Vec<String>>>,
    cell: Option<String>,
}

impl State {
    /// The prefix of lines inside the current quotes and list items.
    fn indent(&self) -> String {
        let mut s = "│ ".repeat(self.quotes);
        for l in &self.lists {
            let w = l.map_or(2, |n| n.to_string().len() + 2);
            s.push_str(&" ".repeat(w));
        }
        s
    }

    /// Opens a block unless one is open; the first block of a list item takes its marker.
    fn block(&mut self) -> &mut Block {
        if self.open.is_none() {
            let rest = self.indent();
            let first = match self.marker.take() {
                Some(m) => {
                    let w = self
                        .lists
                        .last()
                        .map_or(2, |l| l.map_or(2, |n| n.to_string().len() + 2));
                    let base = &rest[..rest.len() - w.min(rest.len())];
                    format!("{base}{m}")
                }
                None => rest.clone(),
            };
            self.open = Some(Block::new(first, rest));
        }
        self.open.as_mut().expect("opened above")
    }

    fn close(&mut self) {
        if let Some(b) = self.open.take() {
            self.blocks.push(b);
        }
    }

    /// A blank line between top-level blocks; none inside a list.
    fn gap(&mut self) {
        self.close();
        if self.lists.is_empty()
            && self
                .blocks
                .last()
                .is_some_and(|b| !b.spans.is_empty() || b.rule)
        {
            self.blocks.push(Block::blank());
        }
    }

    fn class(&self) -> Class {
        if self.code {
            Class::Code
        } else if self.heading == 1 {
            Class::Title
        } else if self.heading > 0 {
            Class::Heading
        } else if self.image > 0 {
            Class::Meta
        } else if !self.links.is_empty() {
            Class::Link
        } else if self.strike > 0 {
            Class::Strike
        } else {
            match (self.strong > 0, self.emph > 0) {
                (true, true) => Class::StrongEmph,
                (true, false) => Class::Strong,
                (false, true) => Class::Emph,
                (false, false) => Class::Text,
            }
        }
    }

    fn text(&mut self, t: &str) {
        let t = clean(t);
        if let Some(c) = self.cell.as_mut() {
            c.push_str(&t);
            return;
        }
        let class = self.class();
        let b = self.block();
        match b.spans.last_mut() {
            Some((c, s)) if *c == class => s.push_str(&t),
            _ => b.spans.push((class, t)),
        }
    }

    fn meta(&mut self, t: &str) {
        let b = self.block();
        b.spans.push((Class::Meta, t.to_string()));
    }

    /// A code block's text: one block per line, cut instead of wrapped.
    fn code_text(&mut self, t: &str) {
        for line in t.split_inclusive('\n') {
            let line = line.strip_suffix('\n').unwrap_or(line);
            let indent = format!("{}  ", self.indent());
            let mut b = Block::new(indent.clone(), indent);
            b.wrap = false;
            b.spans.push((Class::Code, clean(line)));
            self.blocks.push(b);
        }
    }
}

/// Control characters escaped as in names (M1 3.2); tabs expand to four spaces.
fn clean(t: &str) -> String {
    if !t.chars().any(char::is_control) {
        return t.to_string();
    }
    let mut out = String::with_capacity(t.len());
    for c in t.chars() {
        match c {
            '\t' => out.push_str("    "),
            c if c.is_control() => out.push_str(&escaped(c.to_string().as_bytes())),
            c => out.push(c),
        }
    }
    out
}

/// Parses Markdown into blocks (CommonMark with tables, strikethrough and task lists).
pub fn parse(text: &str) -> Vec<Block> {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut s = State::default();
    for ev in Parser::new_ext(text, opts) {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { level, .. } => {
                    s.gap();
                    let n = match level {
                        HeadingLevel::H1 => 1,
                        HeadingLevel::H2 => 2,
                        HeadingLevel::H3 => 3,
                        HeadingLevel::H4 => 4,
                        HeadingLevel::H5 => 5,
                        HeadingLevel::H6 => 6,
                    };
                    s.heading = n;
                    // H1 and H2 stand alone; deeper headings keep their level visible.
                    if n > 2 {
                        let marks = "#".repeat(usize::from(n));
                        s.block().spans.push((Class::Heading, format!("{marks} ")));
                    }
                }
                Tag::BlockQuote(_) => {
                    s.close();
                    s.quotes += 1;
                }
                Tag::CodeBlock(kind) => {
                    s.gap();
                    s.code = true;
                    if let CodeBlockKind::Fenced(lang) = kind
                        && !lang.is_empty()
                    {
                        // The language, aligned with the code below it.
                        let indent = format!("{}  ", s.indent());
                        let mut b = Block::new(indent.clone(), indent);
                        b.spans.push((Class::Meta, clean(&lang)));
                        s.blocks.push(b);
                    }
                }
                Tag::List(start) => {
                    s.close();
                    s.lists.push(start);
                }
                Tag::Item => {
                    s.close();
                    let m = match s.lists.last_mut() {
                        Some(Some(n)) => {
                            let m = format!("{n}. ");
                            *n += 1;
                            m
                        }
                        _ => "• ".to_string(),
                    };
                    s.marker = Some(m);
                }
                Tag::Emphasis => s.emph += 1,
                Tag::Strong => s.strong += 1,
                Tag::Strikethrough => s.strike += 1,
                Tag::Link { dest_url, .. } => s.links.push(dest_url.to_string()),
                Tag::Image { dest_url, .. } => {
                    s.image += 1;
                    s.meta("[image: ");
                    s.links.push(dest_url.to_string());
                }
                Tag::Table(_) => {
                    s.gap();
                    s.table = Some(Vec::new());
                }
                Tag::TableHead | Tag::TableRow => {
                    if let Some(t) = s.table.as_mut() {
                        t.push(Vec::new());
                    }
                }
                Tag::TableCell => s.cell = Some(String::new()),
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    if s.lists.is_empty() {
                        s.gap();
                    } else {
                        s.close();
                    }
                }
                TagEnd::Heading(_) => {
                    s.heading = 0;
                    s.gap();
                }
                TagEnd::BlockQuote(_) => {
                    s.close();
                    s.quotes -= 1;
                    if s.quotes == 0 {
                        s.gap();
                    }
                }
                TagEnd::CodeBlock => {
                    s.code = false;
                    s.gap();
                }
                TagEnd::List(_) => {
                    s.close();
                    s.lists.pop();
                    if s.lists.is_empty() {
                        s.gap();
                    }
                }
                TagEnd::Item => s.close(),
                TagEnd::Emphasis => s.emph -= 1,
                TagEnd::Strong => s.strong -= 1,
                TagEnd::Strikethrough => s.strike -= 1,
                TagEnd::Link => {
                    let url = s.links.pop().unwrap_or_default();
                    if !url.is_empty() && !url.starts_with('#') {
                        s.meta(&format!(" ({})", clean(&url)));
                    }
                }
                TagEnd::Image => {
                    s.links.pop();
                    s.image -= 1;
                    s.meta("]");
                }
                TagEnd::TableCell => {
                    if let (Some(c), Some(t)) = (s.cell.take(), s.table.as_mut())
                        && let Some(row) = t.last_mut()
                    {
                        row.push(c);
                    }
                }
                TagEnd::Table => {
                    let rows = s.table.take().unwrap_or_default();
                    table(&mut s, rows);
                    s.gap();
                }
                _ => {}
            },
            Event::Text(t) => {
                if s.code {
                    s.code_text(&t);
                } else {
                    s.text(&t);
                }
            }
            Event::Code(t) => {
                if let Some(c) = s.cell.as_mut() {
                    c.push_str(&clean(&t));
                } else {
                    let b = s.block();
                    b.spans.push((Class::Code, clean(&t)));
                }
            }
            Event::Html(t)
            | Event::InlineHtml(t)
            | Event::InlineMath(t)
            | Event::DisplayMath(t) => {
                for (i, line) in t.lines().enumerate() {
                    if i > 0 {
                        s.close();
                    }
                    s.text(line);
                }
            }
            Event::FootnoteReference(t) => s.text(&format!("[{t}]")),
            Event::SoftBreak => s.text(" "),
            Event::HardBreak => s.close(),
            Event::Rule => {
                s.gap();
                let mut b = Block::new(s.indent(), s.indent());
                b.rule = true;
                s.blocks.push(b);
                s.gap();
            }
            Event::TaskListMarker(done) => s.meta(if done { "[x] " } else { "[ ] " }),
        }
    }
    s.close();
    while s
        .blocks
        .last()
        .is_some_and(|b| b.spans.is_empty() && !b.rule)
    {
        s.blocks.pop();
    }
    s.blocks
}

/// A table's rows, its cells padded to their column's width and separated by bars; the
/// header row is bold.
fn table(s: &mut State, rows: Vec<Vec<String>>) {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0; cols];
    for r in &rows {
        for (i, c) in r.iter().enumerate() {
            widths[i] = widths[i].max(width(c));
        }
    }
    for (n, r) in rows.iter().enumerate() {
        let indent = s.indent();
        let mut b = Block::new(indent.clone(), indent);
        b.wrap = false;
        for (i, w) in widths.iter().enumerate() {
            if i > 0 {
                b.spans.push((Class::Meta, " │ ".into()));
            }
            let c = r.get(i).map_or("", String::as_str);
            let pad = " ".repeat(w - width(c));
            b.spans.push((
                if n == 0 { Class::Strong } else { Class::Text },
                format!("{c}{pad}"),
            ));
        }
        s.blocks.push(b);
    }
}

fn width(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

fn style(c: Class, th: &Theme) -> Style {
    match c {
        Class::Text => th.normal,
        Class::Strong => th.normal.add_modifier(Modifier::BOLD),
        Class::Emph => th.normal.add_modifier(Modifier::ITALIC),
        Class::StrongEmph => th.normal.add_modifier(Modifier::BOLD | Modifier::ITALIC),
        Class::Strike => th.normal.add_modifier(Modifier::CROSSED_OUT),
        Class::Code => th.metadata,
        Class::Title => th
            .border_active
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        Class::Heading => th.border_active.add_modifier(Modifier::BOLD),
        Class::Link => th.symlink.add_modifier(Modifier::UNDERLINED),
        Class::Meta => th.metadata,
    }
}

/// The blocks as at most `max` lines of `w` columns: text wraps at spaces (a word longer than
/// the line breaks inside), code lines and table rows are cut, a rule fills the width.
pub fn lines(blocks: &[Block], w: usize, th: &Theme, max: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    for b in blocks {
        if out.len() >= max {
            break;
        }
        let prefix = |first: bool| {
            let p = if first { &b.first } else { &b.rest };
            Span::styled(crate::ui::text::fit(p, w).0, th.metadata)
        };
        if b.rule {
            let room = w.saturating_sub(width(&b.first));
            out.push(Line::from(vec![
                prefix(true),
                Span::styled("─".repeat(room), th.metadata),
            ]));
            continue;
        }
        if b.spans.is_empty() {
            out.push(Line::default());
            continue;
        }
        let room = |first: bool| {
            w.saturating_sub(width(if first { &b.first } else { &b.rest }))
                .max(1)
        };
        if !b.wrap {
            let mut line = vec![prefix(true)];
            let mut left = room(true);
            for (c, t) in &b.spans {
                if left == 0 {
                    break;
                }
                let (t, used) = crate::ui::text::fit(t, left);
                left -= used.min(left);
                line.push(Span::styled(t, style(*c, th)));
            }
            out.push(Line::from(line));
            continue;
        }
        // Greedy wrapping over words that keep their class.
        let mut line = vec![prefix(true)];
        let mut left = room(true);
        let mut at_start = true;
        for (c, t) in &b.spans {
            let st = style(*c, th);
            for (i, word) in t.split(' ').enumerate() {
                let space = i > 0;
                let ww = width(word);
                let need = ww + usize::from(space && !at_start);
                if need > left && !at_start {
                    out.push(Line::from(std::mem::take(&mut line)));
                    if out.len() >= max {
                        return out;
                    }
                    line.push(prefix(false));
                    left = room(false);
                    at_start = true;
                }
                if space && !at_start {
                    line.push(Span::styled(" ", st));
                    left -= 1;
                }
                let mut word = word.to_string();
                // A word longer than a whole line breaks inside.
                while width(&word) > left && at_start && left > 0 {
                    let (head, used) = split_at_width(&word, left);
                    line.push(Span::styled(head, st));
                    out.push(Line::from(std::mem::take(&mut line)));
                    if out.len() >= max {
                        return out;
                    }
                    word = word[used..].to_string();
                    line.push(prefix(false));
                    left = room(false);
                }
                if !word.is_empty() {
                    left -= width(&word).min(left);
                    line.push(Span::styled(word, st));
                    at_start = false;
                }
            }
        }
        out.push(Line::from(line));
    }
    out.truncate(max);
    out
}

/// The longest start of `s` that fits `cols` columns (at least one character), and its length
/// in bytes.
fn split_at_width(s: &str, cols: usize) -> (String, usize) {
    let mut w = 0;
    let mut end = 0;
    for (i, c) in s.char_indices() {
        let cw = c.width().unwrap_or(0);
        if w + cw > cols && end > 0 {
            break;
        }
        w += cw;
        end = i + c.len_utf8();
    }
    (s[..end].to_string(), end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(blocks: &[Block], w: usize) -> Vec<String> {
        let th = Theme::default();
        lines(blocks, w, &th, 100)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn headings_paragraphs_and_lists() {
        let md = "# Title\n\nSome *very* **important** text.\n\n- one\n- two\n  - nested\n\n1. first\n2. second\n\n### Deep\n";
        let b = parse(md);
        assert_eq!(
            plain(&b, 40),
            [
                "Title",
                "",
                "Some very important text.",
                "",
                "• one",
                "• two",
                "  • nested",
                "",
                "1. first",
                "2. second",
                "",
                "### Deep",
            ]
        );
        assert_eq!(b[0].spans, [(Class::Title, "Title".into())]);
        assert_eq!(b[b.len() - 1].spans, [(Class::Heading, "### Deep".into())]);
        assert!(b[2].spans.contains(&(Class::Emph, "very".into())));
        assert!(b[2].spans.contains(&(Class::Strong, "important".into())));
    }

    #[test]
    fn paragraphs_wrap_and_code_is_cut() {
        let md = "one two three four five six\n\n```rust\nfn main() { println!(\"a long line\"); }\n```\n";
        let b = parse(md);
        assert_eq!(
            plain(&b, 12),
            [
                "one two",
                "three four",
                "five six",
                "",
                "  rust",
                "  fn main()~",
            ]
        );
    }

    #[test]
    fn quotes_links_rules_tables_and_tasks() {
        let md = "> quoted\n\nSee [the site](https://example.org) and ![a cat](cat.png).\n\n---\n\n| a | bb |\n|---|---|\n| ccc | d |\n\n- [x] done\n- [ ] open\n";
        let b = parse(md);
        assert_eq!(
            plain(&b, 60),
            [
                "│ quoted",
                "",
                "See the site (https://example.org) and [image: a cat].",
                "",
                "────────────────────────────────────────────────────────────",
                "",
                "a   │ bb",
                "ccc │ d ",
                "",
                "• [x] done",
                "• [ ] open",
            ]
        );
    }

    #[test]
    fn control_characters_are_escaped_and_long_words_break() {
        let b = parse("a\u{1b}b supercalifragilistic\n");
        let out = plain(&b, 8);
        assert!(
            out[0].starts_with("a\\x1bb") || out[0].starts_with("a"),
            "{out:?}"
        );
        assert!(out.iter().all(|l| width(l) <= 8), "{out:?}");
    }
}
