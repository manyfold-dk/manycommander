#![forbid(unsafe_code)]
//! `colors.toml` parsing (design 7.1). Known keys only; unknown keys are ignored, and a
//! value that is not `#rrggbb` counts as missing, so its role falls back.

use std::collections::BTreeMap;
use std::path::Path;

/// The keys Omarchy writes (design section 2).
pub const KNOWN: &[&str] = &[
    "accent",
    "selection",
    "muted",
    "background",
    "dark_background",
    "darker_background",
    "lighter_background",
    "foreground",
    "dark_foreground",
    "light_foreground",
    "bright_foreground",
    "red",
    "yellow",
    "orange",
    "green",
    "cyan",
    "blue",
    "magenta",
    "brown",
    "bright_red",
    "bright_yellow",
    "bright_green",
    "bright_cyan",
    "bright_blue",
    "bright_magenta",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn parse(s: &str) -> Option<Rgb> {
        let h = s.strip_prefix('#')?;
        if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        Some(Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }
}

/// A parsed palette: `mode` and the known colour keys that were present and valid.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Palette {
    pub mode: Option<String>,
    colors: BTreeMap<&'static str, Rgb>,
}

impl Palette {
    pub fn parse(text: &str) -> Result<Palette, String> {
        let table: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
        let mut p = Palette {
            mode: table
                .get("mode")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            colors: BTreeMap::new(),
        };
        for key in KNOWN {
            if let Some(rgb) = table
                .get(*key)
                .and_then(|v| v.as_str())
                .and_then(Rgb::parse)
            {
                p.colors.insert(key, rgb);
            }
        }
        Ok(p)
    }

    /// Reads and parses a palette file. It touches the filesystem, so the app calls it on
    /// a listing thread only (P-1).
    pub fn load(path: &Path) -> Result<Palette, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Palette::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn get(&self, key: &str) -> Option<Rgb> {
        self.colors.get(key).copied()
    }

    /// The first key of a fallback chain that is present.
    pub fn chain(&self, keys: &[&str]) -> Option<Rgb> {
        keys.iter().find_map(|k| self.get(k))
    }

    pub fn is_empty(&self) -> bool {
        self.colors.is_empty()
    }

    pub fn len(&self) -> usize {
        self.colors.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_parsing() {
        assert_eq!(Rgb::parse("#7aa2f7"), Some(Rgb(0x7a, 0xa2, 0xf7)));
        assert_eq!(Rgb::parse("7aa2f7"), None);
        assert_eq!(Rgb::parse("#7aa2f"), None);
        assert_eq!(Rgb::parse("#zzzzzz"), None);
    }

    #[test]
    fn unknown_keys_and_bad_values_are_ignored() {
        let p =
            Palette::parse("mode = \"light\"\nred = \"#ff0000\"\nblue = 3\nfoo = \"#000000\"\n")
                .unwrap();
        assert_eq!(p.mode.as_deref(), Some("light"));
        assert_eq!(p.get("red"), Some(Rgb(255, 0, 0)));
        assert_eq!(p.get("blue"), None);
        assert_eq!(p.len(), 1);
        assert_eq!(p.chain(&["accent", "red"]), Some(Rgb(255, 0, 0)));
    }
}
