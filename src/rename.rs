#![forbid(unsafe_code)]
//! Multi-rename masks and the preview checks (P2 6.2, 6.4), as pure functions.
//!
//! The dialog compiles its fields into [`Rules`] once per edit (the masks parsed, the
//! search pattern compiled) and computes a [`Preview`] of every selected entry on every
//! keystroke (P-14). Nothing here touches the filesystem: an entry carries what the panel
//! lists (name, modification time) and its directory what the panel knows about it.
//!
//! Names are bytes. A character is a Unicode scalar value where the name is valid UTF-8
//! and one byte where it is not, so invalid names survive byte-exactly. The order of
//! application is: masks, then search and replace on the whole new name, then the case
//! mode, which treats the name part and the extension part separately.

use crate::fsops::sys::Ts;
use crate::ui::text::escaped;
use regex::bytes::{Regex, RegexBuilder};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::sync::Arc;

/// The longest name one path component may have (`NAME_MAX`).
pub const NAME_MAX: usize = 255;
/// The compiled-size limit of a search pattern (P2 6.2, NFR-SEC).
pub const REGEX_SIZE_LIMIT: usize = 1 << 20;
/// The widest counter.
pub const MAX_DIGITS: usize = 20;

/// Splits a name by the M1 extension rule: the extension is what follows the last `.`
/// that is not the first byte. A name that ends in that `.` has no extension and keeps
/// its dot, so the default masks `[N]` and `[E]` give every name back unchanged.
pub fn split_ext(name: &[u8]) -> (&[u8], &[u8]) {
    match name.iter().rposition(|&c| c == b'.') {
        Some(i) if i > 0 && i + 1 < name.len() => (&name[..i], &name[i + 1..]),
        _ => (name, &[]),
    }
}

/// A character range of a name or an extension (1-based, clamped to the text).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Range {
    All,
    /// Characters `from` to `to` (inclusive); `None` is to the end.
    Span(usize, Option<usize>),
    /// The last `n` characters.
    Last(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DatePart {
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Text(Vec<u8>),
    Name(Range),
    Ext(Range),
    Counter,
    Parent,
    Date(DatePart),
}

/// A parsed mask (P2 6.2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mask {
    tokens: Vec<Token>,
}

fn number(b: &[u8]) -> Option<usize> {
    if b.is_empty() || !b.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(b.iter().fold(0usize, |n, &d| {
        n.saturating_mul(10).saturating_add((d - b'0') as usize)
    }))
}

/// The range part of `[N...]` or `[E...]`: empty, `2`, `2-5`, `2-` or `-3`.
fn range(spec: &[u8]) -> Result<Range, &'static str> {
    const ZERO: &str = "characters count from 1";
    if spec.is_empty() {
        return Ok(Range::All);
    }
    let (from, to) = match spec.iter().position(|&c| c == b'-') {
        Some(i) => (&spec[..i], Some(&spec[i + 1..])),
        None => (spec, None),
    };
    match (from, to) {
        (f, None) => {
            let n = number(f).ok_or("malformed range")?;
            if n == 0 {
                return Err(ZERO);
            }
            Ok(Range::Span(n, Some(n)))
        }
        (b"", Some(t)) => {
            let n = number(t).ok_or("malformed range")?;
            if n == 0 {
                return Err(ZERO);
            }
            Ok(Range::Last(n))
        }
        (f, Some(b"")) => {
            let n = number(f).ok_or("malformed range")?;
            if n == 0 {
                return Err(ZERO);
            }
            Ok(Range::Span(n, None))
        }
        (f, Some(t)) => {
            let (a, b) = (
                number(f).ok_or("malformed range")?,
                number(t).ok_or("malformed range")?,
            );
            if a == 0 || b == 0 {
                return Err(ZERO);
            }
            if b < a {
                return Err("the range ends before it starts");
            }
            Ok(Range::Span(a, Some(b)))
        }
    }
}

fn placeholder(inner: &[u8]) -> Result<Token, String> {
    let bad = |why: &str| {
        format!(
            "[{}]: {why}",
            String::from_utf8_lossy(inner).escape_default()
        )
    };
    let t = match inner {
        b"C" => Token::Counter,
        b"P" => Token::Parent,
        b"Y" => Token::Date(DatePart::Year),
        b"M" => Token::Date(DatePart::Month),
        b"D" => Token::Date(DatePart::Day),
        b"h" => Token::Date(DatePart::Hour),
        b"m" => Token::Date(DatePart::Minute),
        b"s" => Token::Date(DatePart::Second),
        [b'N', rest @ ..] => Token::Name(range(rest).map_err(bad)?),
        [b'E', rest @ ..] => Token::Ext(range(rest).map_err(bad)?),
        _ => return Err(bad("unknown placeholder")),
    };
    Ok(t)
}

impl Mask {
    /// Parses a mask. An unknown or malformed placeholder, an unclosed `[` and a lone `]`
    /// are errors; `[[` and `]]` are the literal brackets.
    pub fn parse(text: &[u8]) -> Result<Mask, String> {
        let mut tokens = Vec::new();
        let mut lit = Vec::new();
        let mut i = 0;
        while i < text.len() {
            match text[i] {
                b'[' if text.get(i + 1) == Some(&b'[') => {
                    lit.push(b'[');
                    i += 2;
                }
                b']' if text.get(i + 1) == Some(&b']') => {
                    lit.push(b']');
                    i += 2;
                }
                b']' => return Err("a lone ] (write ]] for a literal ])".into()),
                b'[' => {
                    let Some(len) = text[i + 1..].iter().position(|&c| c == b']') else {
                        return Err("a [ without its ] (write [[ for a literal [)".into());
                    };
                    let t = placeholder(&text[i + 1..i + 1 + len])?;
                    if !lit.is_empty() {
                        tokens.push(Token::Text(std::mem::take(&mut lit)));
                    }
                    tokens.push(t);
                    i += len + 2;
                }
                c => {
                    lit.push(c);
                    i += 1;
                }
            }
        }
        if !lit.is_empty() {
            tokens.push(Token::Text(lit));
        }
        Ok(Mask { tokens })
    }

    fn uses_dates(&self) -> bool {
        self.tokens.iter().any(|t| matches!(t, Token::Date(_)))
    }
}

/// The byte offset where each character of `s` starts, then `s.len()`.
fn char_bounds(s: &[u8], out: &mut Vec<usize>) {
    out.clear();
    let mut at = 0;
    for chunk in s.utf8_chunks() {
        let valid = chunk.valid();
        out.extend(valid.char_indices().map(|(i, _)| at + i));
        at += valid.len();
        for _ in chunk.invalid() {
            out.push(at);
            at += 1;
        }
    }
    out.push(s.len());
}

/// The characters of `s` that `r` selects, clamped to the text.
fn slice<'a>(s: &'a [u8], r: Range, bounds: &mut Vec<usize>) -> &'a [u8] {
    if r == Range::All {
        return s;
    }
    char_bounds(s, bounds);
    let n = bounds.len() - 1;
    let (from, to) = match r {
        Range::All => (0, n),
        Range::Span(a, b) => (a - 1, b.map_or(n, |b| b.min(n))),
        Range::Last(k) => (n.saturating_sub(k), n),
    };
    if from >= to {
        return &[];
    }
    &s[bounds[from]..bounds[to]]
}

/// How the case mode changes the new name (P2 6.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaseMode {
    #[default]
    Unchanged,
    Lower,
    Upper,
    /// Words of the name part capitalised, the extension lowercased.
    Title,
}

/// The `[C]` counter: `start + index x step`, zero-padded to `digits`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counter {
    pub start: u64,
    pub step: u64,
    pub digits: usize,
}

impl Default for Counter {
    fn default() -> Self {
        Counter {
            start: 1,
            step: 1,
            digits: 1,
        }
    }
}

/// One piece of a regex replacement: literal bytes or a capture group's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    Text(Vec<u8>),
    Group(usize),
}

/// The compiled search and replace (P2 6.2).
#[derive(Clone, Debug)]
pub enum Search {
    /// No search text: nothing is replaced.
    None,
    /// Every non-overlapping occurrence, left to right; `fold` compares ASCII letters
    /// case-insensitively.
    Literal {
        needle: Vec<u8>,
        with: Vec<u8>,
        fold: bool,
    },
    /// `regex::bytes`; the replacement's `$1`..`$9` and `${name}` expand.
    Regex {
        re: Regex,
        with: Vec<Piece>,
        groups: bool,
    },
}

/// The one-line reason a pattern does not compile.
fn regex_error(e: regex::Error) -> String {
    match e {
        regex::Error::CompiledTooBig(_) => "the compiled pattern is larger than 1 MiB".into(),
        regex::Error::Syntax(s) => s
            .lines()
            .rev()
            .find_map(|l| l.trim().strip_prefix("error: "))
            .or_else(|| s.lines().last())
            .unwrap_or("the pattern does not parse")
            .to_string(),
        e => e.to_string(),
    }
}

/// Parses a regex replacement: `$1`..`$9` (one digit), `${name}` or `${12}`, `$$` for a
/// literal `$`; any other `$` is literal. A group the pattern does not have is an error.
fn template(with: &[u8], re: &Regex) -> Result<Vec<Piece>, String> {
    let group = |n: usize| {
        if n < re.captures_len() {
            Ok(Piece::Group(n))
        } else {
            Err(format!("the pattern has no group {n}"))
        }
    };
    let mut out = Vec::new();
    let mut lit = Vec::new();
    let mut i = 0;
    while i < with.len() {
        let c = with[i];
        let next = with.get(i + 1).copied();
        let piece = match (c, next) {
            (b'$', Some(b'$')) => {
                lit.push(b'$');
                i += 2;
                continue;
            }
            (b'$', Some(d @ b'1'..=b'9')) => {
                i += 2;
                group((d - b'0') as usize)?
            }
            (b'$', Some(b'{')) => {
                let Some(len) = with[i + 2..].iter().position(|&c| c == b'}') else {
                    return Err("a ${ without its }".into());
                };
                let name = &with[i + 2..i + 2 + len];
                i += len + 3;
                match number(name) {
                    Some(n) => group(n)?,
                    None => {
                        let idx = re
                            .capture_names()
                            .position(|n| n.is_some_and(|n| n.as_bytes() == name));
                        match idx {
                            Some(n) => Piece::Group(n),
                            None => {
                                return Err(format!(
                                    "the pattern has no group named {}",
                                    String::from_utf8_lossy(name)
                                ));
                            }
                        }
                    }
                }
            }
            _ => {
                lit.push(c);
                i += 1;
                continue;
            }
        };
        if !lit.is_empty() {
            out.push(Piece::Text(std::mem::take(&mut lit)));
        }
        out.push(piece);
    }
    if !lit.is_empty() {
        out.push(Piece::Text(lit));
    }
    Ok(out)
}

impl Search {
    /// Compiles the search and replace fields. The dialog does this once per edit of
    /// them, not per name (P2 6.2). `Err` is the message the preview shows.
    pub fn compile(
        pattern: &[u8],
        with: &[u8],
        regex: bool,
        match_case: bool,
    ) -> Result<Search, String> {
        if pattern.is_empty() {
            return Ok(Search::None);
        }
        if !regex {
            return Ok(Search::Literal {
                needle: pattern.to_vec(),
                with: with.to_vec(),
                fold: !match_case,
            });
        }
        let p = std::str::from_utf8(pattern)
            .map_err(|_| "Search: the pattern is not valid UTF-8".to_string())?;
        let re = RegexBuilder::new(p)
            .size_limit(REGEX_SIZE_LIMIT)
            .case_insensitive(!match_case)
            .build()
            .map_err(|e| format!("Search: {}", regex_error(e)))?;
        let with = template(with, &re).map_err(|e| format!("Replace: {e}"))?;
        let groups = with.iter().any(|p| matches!(p, Piece::Group(_)));
        Ok(Search::Regex { re, with, groups })
    }

    /// `text` with the replacements, into `out` (cleared first).
    fn apply(&self, text: &[u8], out: &mut Vec<u8>) {
        out.clear();
        match self {
            Search::None => out.extend_from_slice(text),
            Search::Literal { needle, with, fold } => {
                let n = needle.len();
                let (mut i, mut last) = (0, 0);
                while i + n <= text.len() {
                    let hay = &text[i..i + n];
                    let hit = if *fold {
                        hay.eq_ignore_ascii_case(needle)
                    } else {
                        hay == needle.as_slice()
                    };
                    if hit {
                        out.extend_from_slice(&text[last..i]);
                        out.extend_from_slice(with);
                        i += n;
                        last = i;
                    } else {
                        i += 1;
                    }
                }
                out.extend_from_slice(&text[last..]);
            }
            Search::Regex { re, with, groups } => {
                let mut last = 0;
                if *groups {
                    for caps in re.captures_iter(text) {
                        let m = caps.get(0).expect("group 0 always matches");
                        out.extend_from_slice(&text[last..m.start()]);
                        for p in with {
                            match p {
                                Piece::Text(t) => out.extend_from_slice(t),
                                Piece::Group(g) => {
                                    if let Some(c) = caps.get(*g) {
                                        out.extend_from_slice(c.as_bytes());
                                    }
                                }
                            }
                        }
                        last = m.end();
                    }
                } else {
                    for m in re.find_iter(text) {
                        out.extend_from_slice(&text[last..m.start()]);
                        for p in with {
                            if let Piece::Text(t) = p {
                                out.extend_from_slice(t);
                            }
                        }
                        last = m.end();
                    }
                }
                out.extend_from_slice(&text[last..]);
            }
        }
    }
}

/// Lowercases (`upper == false`) or uppercases the valid UTF-8 runs of `s` into `out`;
/// invalid bytes stay as they are.
fn map_case(s: &[u8], upper: bool, out: &mut Vec<u8>) {
    for chunk in s.utf8_chunks() {
        let v = chunk.valid();
        if v.is_ascii() {
            out.extend(v.bytes().map(|b| {
                if upper {
                    b.to_ascii_uppercase()
                } else {
                    b.to_ascii_lowercase()
                }
            }));
        } else if upper {
            out.extend_from_slice(v.to_uppercase().as_bytes());
        } else {
            out.extend_from_slice(v.to_lowercase().as_bytes());
        }
        out.extend_from_slice(chunk.invalid());
    }
}

/// Title case of a name part: everything lowercased, then the first character of each word
/// uppercased. A word starts at the beginning and after a space, `_`, `-` or `.`; a word
/// that starts with a digit or a symbol keeps its letters lowercase ("2nd").
fn title(s: &[u8], out: &mut Vec<u8>) {
    let mut lower = Vec::with_capacity(s.len());
    map_case(s, false, &mut lower);
    let mut start = true;
    let mut buf = [0u8; 4];
    for chunk in lower.utf8_chunks() {
        for c in chunk.valid().chars() {
            if start {
                for u in c.to_uppercase() {
                    out.extend_from_slice(u.encode_utf8(&mut buf).as_bytes());
                }
            } else {
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            start = matches!(c, ' ' | '_' | '-' | '.');
        }
        for &b in chunk.invalid() {
            out.push(b);
            start = false;
        }
    }
}

/// What makes a new name unusable (P2 6.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    Empty,
    /// `.` or `..`.
    Dots,
    Slash,
    Nul,
    /// Longer than [`NAME_MAX`] bytes.
    TooLong,
    /// Another selected entry of the same directory gets the same name.
    Duplicate,
    /// A listed entry of the directory outside the selection has the name. Advisory: the
    /// job checks again and skips instead of overwriting.
    Exists,
    /// A date placeholder, and the modification time is outside the calendar.
    Time,
}

impl Problem {
    /// The sentence for the error line.
    pub fn text(self) -> &'static str {
        match self {
            Problem::Empty => "the new name is empty",
            Problem::Dots => "the new name is . or ..",
            Problem::Slash => "the new name contains /",
            Problem::Nul => "the new name contains a NUL byte",
            Problem::TooLong => "the new name is longer than 255 bytes",
            Problem::Duplicate => "another selected entry gets the same name",
            Problem::Exists => "an entry outside the selection has this name",
            Problem::Time => "the modification time is out of range",
        }
    }

    /// The preview's status column.
    pub fn short(self) -> &'static str {
        match self {
            Problem::Empty => "empty name",
            Problem::Dots => "not a name",
            Problem::Slash => "contains /",
            Problem::Nul => "contains NUL",
            Problem::TooLong => "too long",
            Problem::Duplicate => "duplicate name",
            Problem::Exists => "name exists",
            Problem::Time => "bad time",
        }
    }
}

/// Whether `name` can be one path component: not empty, `.` or `..`, without `/` or NUL,
/// at most [`NAME_MAX`] bytes.
pub fn check_name(name: &[u8]) -> Option<Problem> {
    if name.is_empty() {
        Some(Problem::Empty)
    } else if name == b"." || name == b".." {
        Some(Problem::Dots)
    } else if name.contains(&b'/') {
        Some(Problem::Slash)
    } else if name.contains(&0) {
        Some(Problem::Nul)
    } else if name.len() > NAME_MAX {
        Some(Problem::TooLong)
    } else {
        None
    }
}

/// One entry to rename, as the mask engine sees it.
#[derive(Clone, Copy, Debug)]
pub struct Item<'a> {
    pub name: &'a [u8],
    /// The name of the directory that holds it (`[P]`).
    pub parent: &'a [u8],
    pub mtime: Ts,
    /// Its position in the selection (`[C]`).
    pub index: usize,
}

/// Reusable buffers for [`Rules::apply`], so a preview allocates little per name.
#[derive(Default)]
pub struct Scratch {
    tmp: Vec<u8>,
    bounds: Vec<usize>,
}

/// The dialog's fields, compiled (P2 6.1).
#[derive(Clone, Debug)]
pub struct Rules {
    name: Mask,
    ext: Mask,
    search: Arc<Search>,
    case: CaseMode,
    counter: Counter,
    tz: jiff::tz::TimeZone,
    dates: bool,
}

/// The dialog's fields as typed, for [`Settings::compile`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub name_mask: Vec<u8>,
    pub ext_mask: Vec<u8>,
    pub search: Vec<u8>,
    pub replace: Vec<u8>,
    pub regex: bool,
    pub match_case: bool,
    pub case: CaseMode,
    pub counter: Counter,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            name_mask: b"[N]".to_vec(),
            ext_mask: b"[E]".to_vec(),
            search: Vec::new(),
            replace: Vec::new(),
            regex: false,
            match_case: false,
            case: CaseMode::Unchanged,
            counter: Counter::default(),
        }
    }
}

impl Settings {
    /// Compiles every field; `Err` is the first field error.
    pub fn compile(&self, tz: &jiff::tz::TimeZone) -> Result<Rules, String> {
        let search = Search::compile(&self.search, &self.replace, self.regex, self.match_case)?;
        Rules::new(
            &self.name_mask,
            &self.ext_mask,
            Arc::new(search),
            self.case,
            self.counter,
            tz,
        )
    }
}

impl Rules {
    /// The rules from the masks and an already compiled search (the dialog keeps the
    /// compiled search until its fields change).
    pub fn new(
        name_mask: &[u8],
        ext_mask: &[u8],
        search: Arc<Search>,
        case: CaseMode,
        counter: Counter,
        tz: &jiff::tz::TimeZone,
    ) -> Result<Rules, String> {
        let name = Mask::parse(name_mask).map_err(|e| format!("Name mask: {e}"))?;
        let ext = Mask::parse(ext_mask).map_err(|e| format!("Extension mask: {e}"))?;
        if counter.digits == 0 || counter.digits > MAX_DIGITS {
            return Err(format!("Counter digits: 1 to {MAX_DIGITS}"));
        }
        let dates = name.uses_dates() || ext.uses_dates();
        Ok(Rules {
            name,
            ext,
            search,
            case,
            counter,
            tz: tz.clone(),
            dates,
        })
    }

    /// The new name of one entry, into `out` (cleared first): the masks, then search and
    /// replace, then the case mode (P2 6.2). The result is not checked; see
    /// [`check_name`].
    pub fn apply(&self, item: &Item, out: &mut Vec<u8>, s: &mut Scratch) -> Result<(), Problem> {
        let (stem, ext) = split_ext(item.name);
        let date = if self.dates {
            let t = jiff::Timestamp::new(item.mtime.sec, item.mtime.nsec as i32)
                .map_err(|_| Problem::Time)?;
            Some(self.tz.to_datetime(t))
        } else {
            None
        };
        let expand = |mask: &Mask, out: &mut Vec<u8>, bounds: &mut Vec<usize>| {
            for t in &mask.tokens {
                match t {
                    Token::Text(b) => out.extend_from_slice(b),
                    Token::Name(r) => out.extend_from_slice(slice(stem, *r, bounds)),
                    Token::Ext(r) => out.extend_from_slice(slice(ext, *r, bounds)),
                    Token::Parent => out.extend_from_slice(item.parent),
                    Token::Counter => {
                        let c = self.counter;
                        let v = c.start as u128 + item.index as u128 * c.step as u128;
                        let _ = write!(out, "{v:0w$}", w = c.digits);
                    }
                    Token::Date(p) => {
                        let d = date.expect("dates are computed when a mask uses them");
                        let _ = match p {
                            DatePart::Year => write!(out, "{:04}", d.year()),
                            DatePart::Month => write!(out, "{:02}", d.month()),
                            DatePart::Day => write!(out, "{:02}", d.day()),
                            DatePart::Hour => write!(out, "{:02}", d.hour()),
                            DatePart::Minute => write!(out, "{:02}", d.minute()),
                            DatePart::Second => write!(out, "{:02}", d.second()),
                        };
                    }
                }
            }
        };
        let tmp = &mut s.tmp;
        tmp.clear();
        expand(&self.name, tmp, &mut s.bounds);
        let dot = tmp.len();
        tmp.push(b'.');
        expand(&self.ext, tmp, &mut s.bounds);
        if tmp.len() == dot + 1 {
            // An empty extension: no dot.
            tmp.pop();
        }
        self.search.apply(tmp, out);
        if self.case == CaseMode::Unchanged {
            return Ok(());
        }
        // The case mode treats the name part and the extension separately (M1 rule).
        std::mem::swap(tmp, out);
        out.clear();
        let (stem, ext) = split_ext(tmp);
        match self.case {
            CaseMode::Unchanged => unreachable!(),
            CaseMode::Lower => map_case(tmp, false, out),
            CaseMode::Upper => map_case(tmp, true, out),
            CaseMode::Title => {
                title(stem, out);
                if !ext.is_empty() {
                    out.push(b'.');
                    map_case(ext, false, out);
                }
            }
        }
        Ok(())
    }

    /// [`Rules::apply`] into a new vector.
    pub fn new_name(&self, item: &Item) -> Result<Vec<u8>, Problem> {
        let mut out = Vec::new();
        self.apply(item, &mut out, &mut Scratch::default())?;
        Ok(out)
    }
}

/// One selected entry of the dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Its name in its directory.
    pub name: Vec<u8>,
    pub mtime: Ts,
    /// The index of its directory in the dialog's [`Directory`] list.
    pub dir: usize,
}

/// A directory that holds selected entries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Directory {
    /// Its own name (`[P]`).
    pub name: Vec<u8>,
    /// The names the panel lists in it outside the selection (P2 6.4, advisory).
    pub others: HashSet<Vec<u8>>,
}

/// A preview row's state (P2 6.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Unchanged,
    Ok,
    Error(Problem),
    /// A field has an error: there is no new name.
    Blocked,
}

/// The preview of every selected entry (P2 6.4).
#[derive(Clone, Debug, Default)]
pub struct Preview {
    names: Vec<u8>,
    ends: Vec<u32>,
    pub status: Vec<Status>,
    /// The first error, a field's or a row's; it blocks `Enter`.
    pub error: Option<String>,
    /// Rows whose name changes.
    pub changed: usize,
    /// Rows with an error.
    pub errors: usize,
}

impl Preview {
    /// Every row blocked by a field error.
    pub fn blocked(rows: usize, error: String) -> Preview {
        Preview {
            names: Vec::new(),
            ends: vec![0; rows],
            status: vec![Status::Blocked; rows],
            error: Some(error),
            changed: 0,
            errors: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.status.len()
    }

    pub fn is_empty(&self) -> bool {
        self.status.is_empty()
    }

    /// Row `i`'s new name (empty when blocked).
    pub fn new_name(&self, i: usize) -> &[u8] {
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        &self.names[start..self.ends[i] as usize]
    }
}

/// Computes the new name of every entry and checks it (P2 6.4): an unusable name, two
/// entries of one directory with the same new name (unchanged entries included, since
/// they keep theirs), and a new name that a listed entry outside the selection has. The
/// first error, in row order, blocks `Enter`.
pub fn preview(rules: &Rules, entries: &[Entry], dirs: &[Directory]) -> Preview {
    let mut p = Preview {
        names: Vec::with_capacity(entries.iter().map(|e| e.name.len() + 4).sum()),
        ends: Vec::with_capacity(entries.len()),
        status: Vec::with_capacity(entries.len()),
        error: None,
        changed: 0,
        errors: 0,
    };
    let mut s = Scratch::default();
    let mut out = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let item = Item {
            name: &e.name,
            parent: &dirs[e.dir].name,
            mtime: e.mtime,
            index: i,
        };
        let status = match rules.apply(&item, &mut out, &mut s) {
            Err(problem) => {
                out.clear();
                Status::Error(problem)
            }
            Ok(()) if out == e.name => Status::Unchanged,
            Ok(()) => match check_name(&out) {
                Some(problem) => Status::Error(problem),
                None => Status::Ok,
            },
        };
        p.names.extend_from_slice(&out);
        p.ends.push(p.names.len() as u32);
        p.status.push(status);
    }
    // Duplicates per directory, then names held outside the selection.
    let mut seen: HashMap<(usize, &[u8]), usize> = HashMap::with_capacity(entries.len());
    let mut dup = vec![false; entries.len()];
    for (i, e) in entries.iter().enumerate() {
        if matches!(p.status[i], Status::Error(_)) {
            continue;
        }
        let start = if i == 0 { 0 } else { p.ends[i - 1] as usize };
        let name = &p.names[start..p.ends[i] as usize];
        match seen.get(&(e.dir, name)) {
            Some(&first) => {
                dup[first] = true;
                dup[i] = true;
            }
            None => {
                seen.insert((e.dir, name), i);
            }
        }
    }
    for (i, e) in entries.iter().enumerate() {
        if dup[i] {
            p.status[i] = Status::Error(Problem::Duplicate);
        } else if p.status[i] == Status::Ok && dirs[e.dir].others.contains(p.new_name(i)) {
            p.status[i] = Status::Error(Problem::Exists);
        }
        match p.status[i] {
            Status::Ok => p.changed += 1,
            Status::Error(problem) => {
                p.errors += 1;
                if p.error.is_none() {
                    // The reason first: a long name is cut on the error line.
                    p.error = Some(format!("{}: \"{}\"", problem.text(), escaped(&e.name)));
                }
            }
            _ => {}
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_parse_and_reject() {
        assert_eq!(range(b""), Ok(Range::All));
        assert_eq!(range(b"2"), Ok(Range::Span(2, Some(2))));
        assert_eq!(range(b"2-5"), Ok(Range::Span(2, Some(5))));
        assert_eq!(range(b"2-"), Ok(Range::Span(2, None)));
        assert_eq!(range(b"-3"), Ok(Range::Last(3)));
        for bad in [
            &b"0"[..],
            b"-",
            b"x",
            b"1-x",
            b"5-2",
            b"-0",
            b"0-3",
            b"2--3",
            b" 2",
        ] {
            assert!(range(bad).is_err(), "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn char_bounds_count_invalid_bytes_as_characters() {
        let mut b = Vec::new();
        char_bounds("aé\u{1F600}".as_bytes(), &mut b);
        assert_eq!(b, [0, 1, 3, 7]);
        char_bounds(b"a\xff\xfeb", &mut b);
        assert_eq!(b, [0, 1, 2, 3, 4]);
    }

    #[test]
    fn templates_resolve_groups() {
        let re = Regex::new(r"(?P<y>\d{4})-(\d\d)").unwrap();
        assert_eq!(
            template(b"$2/${y}$$x$", &re).unwrap(),
            [
                Piece::Group(2),
                Piece::Text(b"/".to_vec()),
                Piece::Group(1),
                Piece::Text(b"$x$".to_vec()),
            ]
        );
        assert_eq!(template(b"${0}", &re).unwrap(), [Piece::Group(0)]);
        assert!(template(b"$3", &re).is_err());
        assert!(template(b"${nope}", &re).is_err());
        assert!(template(b"${1", &re).is_err());
    }
}
