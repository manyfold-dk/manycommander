#![forbid(unsafe_code)]
//! Case-folding and fuzzy name matching for the quick filter (P2 4).
//!
//! A [`Pattern`] is the filter text as case-folded characters with one bit per character, so
//! every matcher here is bit-parallel and allocates nothing per name (P-12):
//!
//! - [`Pattern::contained_in`]: an exact substring, folding case beyond ASCII (Shift-And).
//! - [`Pattern::prefix_of`]: the name starts with the pattern, folding case.
//! - [`Pattern::distance`]: the fewest edits that turn the pattern into some substring of
//!   the name. An edit is a substitution, an insertion, a deletion or a swap of two adjacent
//!   characters (the optimal string alignment distance), so a wrong, extra, missing or
//!   transposed letter each costs one. This is Myers' bit-vector search with Hyyrö's
//!   transposition term: one pass over the name whatever the edit budget. A pattern of at
//!   most [`SHORT`] characters may not drop one of its own characters (an extra letter
//!   typed): its remaining characters would match any short piece of a name. Such a
//!   candidate is confirmed by a small table without that edit.
//!
//! Case folding is Unicode's simple lowercase: a character whose lowercase is more than one
//! character is compared as it is. Names are bytes; an invalid UTF-8 byte is U+FFFD.

/// The longest pattern, in characters, the bit-parallel matchers take.
pub const MAX_CHARS: usize = 64;

/// The fuzzy tier's edit budget for a pattern of `chars` characters: none below four
/// characters, where one edit matches too much; one up to eight; two from nine.
pub fn budget(chars: usize) -> u8 {
    match chars {
        0..=3 => 0,
        4..=8 => 1,
        _ => 2,
    }
}

/// Patterns of at most this many characters never drop one of their characters in
/// [`Pattern::distance`].
pub const SHORT: usize = 5;

/// `c`'s simple lowercase: its lowercase when that is one character, else `c`.
pub fn fold(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    let mut l = c.to_lowercase();
    match (l.next(), l.next()) {
        (Some(x), None) => x,
        _ => c,
    }
}

/// The characters of `name`, each invalid byte as U+FFFD. Allocates nothing.
fn chars(name: &[u8]) -> impl Iterator<Item = char> + '_ {
    name.utf8_chunks().flat_map(|c| {
        c.valid()
            .chars()
            .chain(c.invalid().iter().map(|_| char::REPLACEMENT_CHARACTER))
    })
}

/// The filter text as case-folded characters, at most [`MAX_CHARS`], one bit each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    /// The number of characters.
    len: usize,
    /// For each ASCII byte, the bits of the pattern characters it matches, in either case.
    ascii: Box<[u64; 128]>,
    /// The pattern's folded non-ASCII characters and their bits.
    other: Vec<(char, u64)>,
}

impl Pattern {
    /// The pattern of `text`; `None` when it is empty or longer than [`MAX_CHARS`].
    pub fn new(text: &[u8]) -> Option<Pattern> {
        let mut p = Pattern {
            len: 0,
            ascii: Box::new([0; 128]),
            other: Vec::new(),
        };
        for c in chars(text) {
            if p.len == MAX_CHARS {
                return None;
            }
            let bit = 1u64 << p.len;
            let f = fold(c);
            if f.is_ascii() {
                p.ascii[f as usize] |= bit;
                p.ascii[f.to_ascii_uppercase() as usize] |= bit;
            } else {
                match p.other.iter_mut().find(|(o, _)| *o == f) {
                    Some((_, m)) => *m |= bit,
                    None => p.other.push((f, bit)),
                }
            }
            p.len += 1;
        }
        (p.len > 0).then_some(p)
    }

    /// The number of characters.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Always false: an empty text has no pattern.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The bits of the pattern characters that `c` matches, folding case.
    fn bits(&self, c: char) -> u64 {
        if c.is_ascii() {
            return self.ascii[c as usize];
        }
        let f = fold(c);
        if f.is_ascii() {
            // A character whose lowercase is ASCII, such as the Kelvin sign.
            return self.ascii[f as usize];
        }
        self.other
            .iter()
            .find(|(o, _)| *o == f)
            .map_or(0, |&(_, m)| m)
    }

    /// Calls `step` with the bits of each character of `name` until it returns true;
    /// returns whether it did. An ASCII name is read byte by byte.
    fn scan(&self, name: &[u8], mut step: impl FnMut(u64) -> bool) -> bool {
        if name.is_ascii() {
            name.iter().any(|&b| step(self.ascii[b as usize]))
        } else {
            chars(name).any(|c| step(self.bits(c)))
        }
    }

    /// Whether `name` contains the pattern, folding case.
    pub fn contained_in(&self, name: &[u8]) -> bool {
        let top = 1u64 << (self.len - 1);
        let mut d = 0u64;
        self.scan(name, |eq| {
            d = ((d << 1) | 1) & eq;
            d & top != 0
        })
    }

    /// Whether `name` starts with the pattern, folding case. Only the bytes that can hold
    /// `len` characters are read: at most four each.
    pub fn prefix_of(&self, name: &[u8]) -> bool {
        let head = &name[..name.len().min(4 * self.len)];
        let mut j = 0;
        let mut ok = true;
        self.scan(head, |eq| {
            ok = eq & (1u64 << j) != 0;
            j += 1;
            !ok || j == self.len
        });
        ok && j == self.len
    }

    /// The fewest edits between the pattern and a substring of `name`, when that is at most
    /// `max`. A pattern of at most [`SHORT`] characters keeps all of its characters: the
    /// edits are a wrong, a missing or a swapped letter.
    pub fn distance(&self, name: &[u8], max: u8) -> Option<u8> {
        let d = self.edits(name);
        if d > u32::from(max) {
            return None;
        }
        let d = if self.len <= SHORT && d > 0 {
            self.edits_keeping_all(name)
        } else {
            d
        };
        (d <= u32::from(max)).then_some(d as u8)
    }

    /// The fewest edits between the pattern and a substring of `name`, each edit costing
    /// one. Myers' search in Hyyrö's formulation with the transposition term; `score` is
    /// the last row of the dynamic-programming table, whose first row is all zeros so a
    /// match may start anywhere in `name`.
    fn edits(&self, name: &[u8]) -> u32 {
        let top = 1u64 << (self.len - 1);
        // The column before the name: row i holds i, so every vertical step is +1.
        let (mut vp, mut vn) = (!0u64, 0u64);
        let (mut d0, mut prev_eq) = (0u64, 0u64);
        let mut score = self.len as u32;
        let mut best = score;
        self.scan(name, |eq| {
            // Rows where the pattern's last two characters swapped match the name's.
            let swap = ((!d0 & eq) << 1) & prev_eq;
            d0 = (((eq & vp).wrapping_add(vp)) ^ vp) | eq | vn | swap;
            let hp = vn | !(d0 | vp);
            let hn = vp & d0;
            if hp & top != 0 {
                score += 1;
            } else if hn & top != 0 {
                score -= 1;
            }
            // The first row is zero in every column: no carry into row 1.
            let x = hp << 1;
            vn = x & d0;
            vp = (hn << 1) | !(x | d0);
            prev_eq = eq;
            best = best.min(score);
            best == 0
        });
        best
    }

    /// [`Pattern::edits`] without deleting a pattern character, for a pattern of at most
    /// [`SHORT`] characters: the table with one column per character of `name`, rows 0 to
    /// `len`. Row 0 is zero, so a match may start anywhere; a pattern character is never
    /// matched against nothing.
    fn edits_keeping_all(&self, name: &[u8]) -> u32 {
        const NONE: u32 = u32::MAX / 2;
        let m = self.len;
        debug_assert!(m <= SHORT);
        // The columns before the name: row 0 is zero, the others cannot be reached.
        let mut before = [NONE; SHORT + 1];
        before[0] = 0;
        let (mut prev2, mut prev) = (before, before);
        let mut prev_eq = 0u64;
        let mut best = NONE;
        self.scan(name, |eq| {
            let mut cur = before;
            for i in 1..=m {
                let hit = eq >> (i - 1) & 1 == 1;
                // A match or a wrong letter; a letter the pattern is missing.
                let mut v = (prev[i - 1] + u32::from(!hit)).min(prev[i] + 1);
                // Two letters swapped.
                if i >= 2 && prev_eq >> (i - 1) & 1 == 1 && eq >> (i - 2) & 1 == 1 {
                    v = v.min(prev2[i - 2] + 1);
                }
                cur[i] = v;
            }
            best = best.min(cur[m]);
            (prev2, prev, prev_eq) = (prev, cur, eq);
            best == 0
        });
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference: the optimal string alignment table over characters, with a free
    /// start and end in `name`. `drop` is the cost of deleting a pattern character.
    fn reference(pattern: &str, name: &str, drop: u32) -> u32 {
        let p: Vec<char> = pattern.chars().map(fold).collect();
        let t: Vec<char> = name.chars().map(fold).collect();
        let (m, n) = (p.len(), t.len());
        let mut d = vec![vec![0u32; n + 1]; m + 1];
        for (i, row) in d.iter_mut().enumerate() {
            row[0] = i as u32 * drop;
        }
        for i in 1..=m {
            for j in 1..=n {
                let cost = u32::from(p[i - 1] != t[j - 1]);
                let mut v = (d[i - 1][j] + drop)
                    .min(d[i][j - 1] + 1)
                    .min(d[i - 1][j - 1] + cost);
                if i > 1 && j > 1 && p[i - 1] == t[j - 2] && p[i - 2] == t[j - 1] {
                    v = v.min(d[i - 2][j - 2] + 1);
                }
                d[i][j] = v;
            }
        }
        (0..=n).map(|j| d[m][j]).min().unwrap()
    }

    fn naive(pattern: &str, name: &str) -> u32 {
        reference(pattern, name, 1)
    }

    /// The unit-cost search.
    fn edits(pattern: &str, name: &str) -> u32 {
        Pattern::new(pattern.as_bytes())
            .unwrap()
            .edits(name.as_bytes())
    }

    /// What the fuzzy tier sees, with no budget.
    fn dist(pattern: &str, name: &str) -> Option<u32> {
        let p = Pattern::new(pattern.as_bytes()).unwrap();
        p.distance(name.as_bytes(), u8::MAX).map(u32::from)
    }

    #[test]
    fn the_common_typing_mistakes_cost_one_edit() {
        for (typed, name) in [
            ("reamde", "README.md"),  // swapped letters
            ("confg", "config.toml"), // a missing letter
            ("readmee", "README.md"), // an extra letter
            ("mainn", "src/main.rs"), // a wrong letter ('.')
            ("cargp", "Cargo.lock"),  // a wrong letter
            ("teh", "the"),
        ] {
            assert_eq!(dist(typed, name), Some(1), "{typed} in {name}");
        }
        assert_eq!(dist("readme", "README.md"), Some(0));
        assert_eq!(dist("presentaiton", "presentation-final.odp"), Some(1));
        assert_eq!(dist("presentaton", "presentation-final.odp"), Some(1));
        assert_eq!(dist("xyz", "abc"), Some(3));
        // A short pattern keeps its letters: dropping one would match any short piece.
        assert_eq!(edits("fiel", "filter.rs"), 1, "fil");
        assert_eq!(dist("fiel", "filter.rs"), Some(2));
        assert_eq!(dist("fiel", "profile.txt"), Some(1), "swapped");
        assert_eq!(edits("xmain", "main.rs"), 1);
        assert_eq!(
            dist("xmain", "main.rs"),
            Some(5),
            "nothing before `m` to replace"
        );
        assert_eq!(dist("xmain", "src/main.rs"), Some(1), "`/` replaced");
        assert_eq!(
            dist("xmaint", "main.rs"),
            Some(2),
            "six letters may drop one"
        );
        assert_eq!(dist("abcd", "abc"), None, "four letters never fit in three");
    }

    /// The bit-vector search equals the reference table on random strings over a small
    /// alphabet, where near matches and repeated letters are frequent.
    #[test]
    fn distance_equals_the_reference() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let alphabet = ['a', 'b', 'c', 'A', 'é', 'É'];
        let mut word = |max: u64| -> String {
            let n = 1 + next() % max;
            (0..n)
                .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
                .collect()
        };
        for _ in 0..20_000 {
            let (p, t) = (word(9), word(14));
            assert_eq!(edits(&p, &t), naive(&p, &t), "{p:?} in {t:?}");
            // Short patterns: dropping a pattern letter never pays.
            let want = if p.chars().count() <= SHORT {
                Some(reference(&p, &t, 1000)).filter(|&d| d < 1000)
            } else {
                Some(naive(&p, &t))
            };
            assert_eq!(dist(&p, &t), want, "{p:?} in {t:?}");
        }
        // Long patterns use the top bits of the word.
        for _ in 0..500 {
            let p = word(64) + &"ab".repeat(20);
            let p: String = p.chars().take(64).collect();
            let t = word(40) + &p.chars().rev().collect::<String>() + &word(40);
            assert_eq!(edits(&p, &t), naive(&p, &t), "{p:?} in {t:?}");
        }
    }

    #[test]
    fn distance_respects_the_budget() {
        let p = Pattern::new(b"confg").unwrap();
        assert_eq!(p.distance(b"config.toml", 1), Some(1));
        assert_eq!(p.distance(b"config.toml", 0), None);
        assert_eq!(p.distance(b"Cargo.toml", 1), None);
    }

    #[test]
    fn exact_and_prefix_fold_case_beyond_ascii() {
        let p = Pattern::new("æble".as_bytes()).unwrap();
        assert!(p.contained_in("Grønne ÆBLER.txt".as_bytes()));
        assert!(!p.contained_in("Grønne æbeler".as_bytes()));
        assert!(p.prefix_of("ÆBLE-most".as_bytes()));
        assert!(!p.prefix_of("mit æble".as_bytes()));
        assert!(!p.prefix_of("æbl".as_bytes()));
        let p = Pattern::new("ÉTÉ".as_bytes()).unwrap();
        assert!(p.contained_in("l'été 2026".as_bytes()));
        // An invalid byte is U+FFFD and never matches a letter.
        assert!(Pattern::new(b"ab").unwrap().contained_in(b"\xffAB\xfe"));
        assert!(!Pattern::new(b"ab").unwrap().contained_in(b"a\xffb"));
        // The Kelvin sign folds to an ASCII k.
        assert!(
            Pattern::new(b"k")
                .unwrap()
                .contained_in("\u{212a}".as_bytes())
        );
        // A pattern over the cap has no pattern.
        assert!(Pattern::new(&[b'a'; MAX_CHARS]).is_some());
        assert!(Pattern::new(&[b'a'; MAX_CHARS + 1]).is_none());
        assert!(Pattern::new(b"").is_none());
    }

    #[test]
    fn budget_grows_with_the_pattern() {
        assert_eq!(
            [1, 3, 4, 8, 9, 30].map(budget),
            [0, 0, 1, 1, 2, 2],
            "no fuzz for three characters or fewer"
        );
    }
}
