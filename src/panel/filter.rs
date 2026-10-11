#![forbid(unsafe_code)]
//! The quick filter (TDR-2 4) and the byte matchers behind it. Find (TDR-2 5.2) uses the
//! same matchers, and mark by glob (TDR-1 8) uses the case-sensitive glob. A filter without
//! `*`, `?` or a closed `[...]` matches a substring and ignores case; with one of them it is
//! a glob over the whole name that ignores ASCII case. No matcher allocates per name (P-12).

use super::fuzzy;

/// The quick filter (TDR-2 4). Without `*`, `?` or a `[` closed by a later `]` it matches as a
/// case-insensitive substring of the name; with one of them it is an ASCII case-insensitive
/// glob over the whole name (the mark-glob matcher). The empty filter matches everything.
///
/// A substring filter also has a fuzzy tier ([`fuzzy`]): when no name contains the text,
/// the listing shows the names that need the fewest edits (a wrong, extra, missing or
/// swapped letter) within [`Filter::budget`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    text: Vec<u8>,
    /// `text` ASCII-lowercased, so a match folds only the name.
    folded: Vec<u8>,
    glob: bool,
    /// `text` holds a non-ASCII character: the exact match folds characters, not bytes.
    unicode: bool,
    /// The substring filter as a case-folding pattern: the exact match of a non-ASCII text,
    /// the prefix check and the fuzzy tier. `None` for a glob, and for a text longer than
    /// [`fuzzy::MAX_CHARS`], which then matches ASCII-folded and without a fuzzy tier.
    pattern: Option<fuzzy::Pattern>,
}

impl Filter {
    pub fn new(text: &[u8]) -> Filter {
        let glob = is_glob(text);
        Filter {
            text: text.to_vec(),
            folded: text.to_ascii_lowercase(),
            glob,
            unicode: !text.is_ascii(),
            pattern: if glob {
                None
            } else {
                fuzzy::Pattern::new(text)
            },
        }
    }

    /// What the user typed.
    pub fn text(&self) -> &[u8] {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Whether the filter is a glob over the whole name.
    pub fn is_glob(&self) -> bool {
        self.glob
    }

    /// Whether `name` passes the filter as typed, ignoring case. Allocates nothing (P-12).
    /// An ASCII text compares bytes; any other text compares case-folded characters.
    pub fn matches(&self, name: &[u8]) -> bool {
        if self.text.is_empty() {
            true
        } else if self.glob {
            glob_match_nocase(&self.folded, name)
        } else if let Some(p) = self.pattern.as_ref().filter(|_| self.unicode) {
            p.contained_in(name)
        } else {
            contains_nocase(name, &self.folded)
        }
    }

    /// The fuzzy tier's edit budget ([`fuzzy::budget`]); 0 when there is no fuzzy tier.
    pub fn budget(&self) -> u8 {
        self.pattern.as_ref().map_or(0, |p| fuzzy::budget(p.len()))
    }

    /// The fewest edits between the text and a substring of `name`, when at most `max`.
    pub fn distance(&self, name: &[u8], max: u8) -> Option<u8> {
        self.pattern.as_ref()?.distance(name, max)
    }

    /// Whether `name` starts with the text, ignoring case; false for a glob.
    pub fn is_prefix_of(&self, name: &[u8]) -> bool {
        match &self.pattern {
            Some(p) => p.prefix_of(name),
            None => {
                !self.glob
                    && name.len() >= self.folded.len()
                    && name[..self.folded.len()].eq_ignore_ascii_case(&self.folded)
            }
        }
    }
}

/// Whether a name pattern is a glob over the whole name rather than a substring (the quick
/// filter, TDR-2 4; find, TDR-2 5.2): it holds `*`, `?`, or a `[` that a later `]` closes. A
/// lone `[` is a letter.
pub fn is_glob(text: &[u8]) -> bool {
    let class = text
        .iter()
        .position(|&c| c == b'[')
        .is_some_and(|i| text[i + 1..].contains(&b']'));
    class || text.iter().any(|c| matches!(c, b'*' | b'?'))
}

/// `*`, `?` and `[...]` matching on bytes (mark by glob); a `[` that no `]` closes matches
/// itself.
pub fn glob_match(p: &[u8], s: &[u8]) -> bool {
    glob(p, s, false)
}

/// [`glob_match`] ignoring ASCII case; `p` must be ASCII-lowercase (the quick filter,
/// TDR-2 4). Only the name's bytes are folded, as they are compared.
pub fn glob_match_nocase(p: &[u8], s: &[u8]) -> bool {
    glob(p, s, true)
}

/// Whether `hay` contains `needle`, ignoring ASCII case; `needle` must be ASCII-lowercase.
/// Allocates nothing (P-12).
pub fn contains_nocase(hay: &[u8], needle: &[u8]) -> bool {
    let Some((&first, rest)) = needle.split_first() else {
        return true;
    };
    if needle.len() > hay.len() {
        return false;
    }
    (0..=hay.len() - needle.len()).any(|i| {
        hay[i].to_ascii_lowercase() == first
            && hay[i + 1..i + needle.len()]
                .iter()
                .zip(rest)
                .all(|(a, b)| a.to_ascii_lowercase() == *b)
    })
}

fn glob(p: &[u8], s: &[u8], fold: bool) -> bool {
    let at = |i: usize| {
        if fold {
            s[i].to_ascii_lowercase()
        } else {
            s[i]
        }
    };
    let (mut pi, mut si) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while si < s.len() {
        if pi < p.len() {
            match p[pi] {
                b'*' => {
                    star = Some(pi);
                    mark = si;
                    pi += 1;
                    continue;
                }
                b'?' => {
                    pi += 1;
                    si += 1;
                    continue;
                }
                b'[' => match class(&p[pi..], at(si)) {
                    Some((true, next)) => {
                        pi += next;
                        si += 1;
                        continue;
                    }
                    // A `[` that no `]` closes is a letter.
                    None if at(si) == b'[' => {
                        pi += 1;
                        si += 1;
                        continue;
                    }
                    _ => {}
                },
                c if c == at(si) => {
                    pi += 1;
                    si += 1;
                    continue;
                }
                _ => {}
            }
        }
        if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

/// A `[...]` class at the start of `p`: `(matched, length)`.
fn class(p: &[u8], c: u8) -> Option<(bool, usize)> {
    let end = p.iter().skip(1).position(|&x| x == b']')? + 1;
    let body = &p[1..end];
    let (neg, body) = match body.first() {
        Some(b'!' | b'^') => (true, &body[1..]),
        _ => (false, body),
    };
    let mut m = false;
    let mut i = 0;
    while i < body.len() {
        if i + 2 < body.len() && body[i + 1] == b'-' {
            m |= body[i] <= c && c <= body[i + 2];
            i += 3;
        } else {
            m |= body[i] == c;
            i += 1;
        }
    }
    Some((m != neg, end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match(b"*", b"anything"));
        assert!(glob_match(b"*.rs", b"main.rs"));
        assert!(!glob_match(b"*.rs", b"main.rc"));
        assert!(glob_match(b"a?c", b"abc"));
        assert!(glob_match(b"[a-c]*", b"beta"));
        assert!(!glob_match(b"[!a-c]*", b"beta"));
        assert!(glob_match(b"*a*b*", b"xxaxxbxx"));
        // A `[` that no `]` closes is a letter.
        assert!(glob_match(b"*[1*", b"file[1].txt"));
        assert!(!glob_match(b"*[1*", b"file1.txt"));
        assert!(glob_match_nocase(b"file[1?", b"FILE[1x"));
        assert!(Filter::new(b"*[1*").matches(b"File[1].txt"));
    }

    #[test]
    fn substring_filter_folds_ascii_case_only() {
        assert!(contains_nocase(b"ReadMe.MD", b"readme"));
        assert!(contains_nocase(b"ReadMe.MD", b"me.m"));
        assert!(contains_nocase(b"abc", b""));
        assert!(!contains_nocase(b"ab", b"abc"));
        assert!(!contains_nocase(b"ReadMe.MD", b"xyz"));
        assert!(contains_nocase(b"aaab", b"aab"), "a partial match restarts");
        // Non-ASCII bytes compare exactly: no Unicode case folding.
        assert!(!contains_nocase(
            "\u{c9}t\u{e9}".as_bytes(),
            "\u{e9}t\u{e9}".as_bytes()
        ));
        assert!(contains_nocase(b"bad\xff\xfeUTF8", b"\xff\xfeutf"));
        let f = Filter::new(b"TXT");
        assert!(!f.is_glob());
        assert!(f.matches(b"notes.txt") && f.matches(b"TXT") && !f.matches(b"notes.md"));
        assert!(Filter::default().matches(b"anything"));
        assert!(Filter::new(b" ").matches(b"a b") && !Filter::new(b" ").matches(b"ab"));
    }

    #[test]
    fn glob_filter_matches_the_whole_name_ignoring_case() {
        for pat in [&b"*.RS"[..], b"?ain.rs", b"[l-n]*", b"M*"] {
            let f = Filter::new(pat);
            assert!(f.is_glob(), "{pat:?}");
            assert!(f.matches(b"main.rs"), "{pat:?}");
            assert!(f.matches(b"MAIN.RS"), "{pat:?}");
        }
        let f = Filter::new(b"ma*");
        assert!(!f.matches(b"xmain"), "a glob is anchored at both ends");
        assert!(!Filter::new(b"*.rs").matches(b"main.rsx"));
        assert!(Filter::new(b"[!a-c]*").matches(b"Delta"));
        assert!(!Filter::new(b"[!a-c]*").matches(b"Beta"));
        // The mark glob stays case-sensitive.
        assert!(!glob_match(b"*.RS", b"main.rs"));
    }

    #[test]
    fn a_filter_is_a_glob_only_with_a_closed_class_or_a_wildcard() {
        let f = Filter::new(b"[20");
        assert!(!f.is_glob(), "an open `[` is a letter");
        assert!(f.matches(b"[2024] photos"));
        assert!(Filter::new(b"[20]*").is_glob());
        assert!(Filter::new(b"a?").is_glob());
        assert!(!Filter::new(b"a]b[").is_glob());
        // Case folds beyond ASCII for a non-ASCII text.
        assert!(Filter::new("\u{c9}T\u{c9}".as_bytes()).matches("l'\u{e9}t\u{e9}".as_bytes()));
        assert!(Filter::new("\u{e6}ble".as_bytes()).matches("\u{c6}BLE.txt".as_bytes()));
    }
}
