#![forbid(unsafe_code)]
//! Sorting (design section 5): name (default, directories first), extension, size, mtime.
//! Names sort naturally (digit runs compare as numbers) and case-insensitively. A sort
//! order is an index permutation over the stored entries (P-4); the natural comparison
//! runs on precomputed collation keys, so a sort is plain byte comparisons.

use super::entry::Entry;
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SortKey {
    #[default]
    Name,
    Ext,
    Size,
    Mtime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SortSpec {
    pub key: SortKey,
    pub reverse: bool,
}

/// Appends the natural, case-insensitive collation key of `name` to `out`. A digit run
/// becomes `0x01`, its significant length and its significant digits, so numbers sort
/// numerically and before letters. Letters are ASCII-lowercased; other bytes stay.
pub fn collation_key(name: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < name.len() {
        let c = name[i];
        if c.is_ascii_digit() {
            let start = i;
            while i < name.len() && name[i].is_ascii_digit() {
                i += 1;
            }
            let run = &name[start..i];
            let sig = match run.iter().position(|&d| d != b'0') {
                Some(p) => &run[p..],
                None => &run[run.len() - 1..],
            };
            out.push(0x01);
            out.push(sig.len().min(255) as u8);
            out.extend_from_slice(sig);
        } else {
            out.push(if c == 0x01 {
                0x02
            } else {
                c.to_ascii_lowercase()
            });
            i += 1;
        }
    }
}

/// Collation keys of all entries, in one arena.
#[derive(Default, Clone)]
pub struct Keys {
    bytes: Vec<u8>,
    /// `(offset, len)` of name key, then of extension key, per entry.
    idx: Vec<(u32, u16, u32, u16)>,
}

impl Keys {
    /// Extends the cache to cover `entries` (new entries are appended by listing batches).
    pub fn update(&mut self, entries: &[Entry], names: &[u8]) {
        for e in &entries[self.idx.len()..] {
            let off = self.bytes.len() as u32;
            collation_key(e.name(names), &mut self.bytes);
            let len = (self.bytes.len() as u32 - off) as u16;
            let eoff = self.bytes.len() as u32;
            collation_key(e.ext(names), &mut self.bytes);
            let elen = (self.bytes.len() as u32 - eoff) as u16;
            self.idx.push((off, len, eoff, elen));
        }
    }

    pub fn clear(&mut self) {
        self.bytes.clear();
        self.idx.clear();
    }

    fn name(&self, i: usize) -> &[u8] {
        let (o, l, ..) = self.idx[i];
        &self.bytes[o as usize..o as usize + l as usize]
    }

    fn ext(&self, i: usize) -> &[u8] {
        let (_, _, o, l) = self.idx[i];
        &self.bytes[o as usize..o as usize + l as usize]
    }
}

/// Sorts `order` (indices into `entries`): directories first, then by the spec's key,
/// ties broken by the natural name and finally the raw bytes.
pub fn sort(order: &mut [u32], entries: &[Entry], names: &[u8], keys: &Keys, spec: SortSpec) {
    let by_name = |a: usize, b: usize| {
        keys.name(a)
            .cmp(keys.name(b))
            .then_with(|| entries[a].name(names).cmp(entries[b].name(names)))
    };
    order.sort_unstable_by(|&a, &b| {
        let (a, b) = (a as usize, b as usize);
        let (ea, eb) = (&entries[a], &entries[b]);
        match (ea.is_dir_like(), eb.is_dir_like()) {
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
        let o = match spec.key {
            SortKey::Name => by_name(a, b),
            SortKey::Ext => keys.ext(a).cmp(keys.ext(b)).then_with(|| by_name(a, b)),
            SortKey::Size => ea.size.cmp(&eb.size).then_with(|| by_name(a, b)),
            SortKey::Mtime => (ea.mtime, ea.mtime_ns)
                .cmp(&(eb.mtime, eb.mtime_ns))
                .then_with(|| by_name(a, b)),
        };
        if spec.reverse { o.reverse() } else { o }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> Vec<u8> {
        let mut v = Vec::new();
        collation_key(s.as_bytes(), &mut v);
        v
    }

    #[test]
    fn natural_and_case_insensitive() {
        let mut names = vec![
            "file10", "File2", "file1", "a", "B", "file02", "file", "10", "9", "é",
        ];
        names.sort_by(|a, b| key(a).cmp(&key(b)).then(a.cmp(b)));
        assert_eq!(
            names,
            [
                "9", "10", "a", "B", "file", "file1", "File2", "file02", "file10", "é"
            ]
        );
        assert!(
            key("x007") == key("x7"),
            "leading zeros are not significant"
        );
    }
}
