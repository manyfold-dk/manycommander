#![forbid(unsafe_code)]
//! Session state (M2, design 1.1 and 9): panel paths, tabs and command history, restored
//! on the next start from `~/.local/state/manycommander/state.toml`. The file is written
//! atomically (a temporary file, fsync, rename) when manycommander exits. A path that
//! no longer exists falls back to its nearest existing ancestor when it is loaded.
//!
//! Paths and commands are bytes: a UTF-8 value is stored as a TOML string, anything else
//! as an array of byte values, so every name round-trips exactly.

use crate::panel::sort::SortKey;
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// A byte string that round-trips through TOML.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Bytes {
    Text(String),
    Raw(Vec<u8>),
}

impl Bytes {
    pub fn of(b: &[u8]) -> Bytes {
        match std::str::from_utf8(b) {
            Ok(s) if !s.chars().any(|c| c.is_control()) => Bytes::Text(s.to_owned()),
            _ => Bytes::Raw(b.to_vec()),
        }
    }

    pub fn bytes(&self) -> &[u8] {
        match self {
            Bytes::Text(s) => s.as_bytes(),
            Bytes::Raw(v) => v,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tab {
    pub path: Bytes,
    #[serde(default)]
    pub sort: SortKey,
    #[serde(default)]
    pub reverse: bool,
    #[serde(default = "yes")]
    pub hidden: bool,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SideState {
    pub tabs: Vec<Tab>,
    #[serde(default)]
    pub active: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct State {
    #[serde(default)]
    pub left: SideState,
    #[serde(default)]
    pub right: SideState,
    /// The active side (0: left, 1: right).
    #[serde(default)]
    pub active: usize,
    #[serde(default)]
    pub history: Vec<Bytes>,
}

impl Tab {
    pub fn path(&self) -> PathBuf {
        PathBuf::from(OsString::from_vec(self.path.bytes().to_vec()))
    }
}

/// `$XDG_STATE_HOME/manycommander/state.toml`, or under `$HOME/.local/state`.
pub fn path(xdg: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    let base = match xdg.filter(|x| x.as_bytes().first() == Some(&b'/')) {
        Some(x) => PathBuf::from(x),
        None => Path::new(home.filter(|h| !h.is_empty())?).join(".local/state"),
    };
    Some(base.join("manycommander/state.toml"))
}

impl State {
    pub fn parse(text: &str) -> Result<State, String> {
        let s: State = toml::from_str(text).map_err(|e| e.to_string())?;
        Ok(s.normalized())
    }

    /// Reads the state file; a missing or broken file is no state.
    pub fn load(path: &Path) -> Option<State> {
        let text = std::fs::read_to_string(path).ok()?;
        State::parse(&text).ok()
    }

    fn normalized(mut self) -> State {
        for s in [&mut self.left, &mut self.right] {
            s.tabs.retain(|t| t.path.bytes().first() == Some(&b'/'));
            if s.active >= s.tabs.len() {
                s.active = 0;
            }
        }
        if self.active > 1 {
            self.active = 0;
        }
        self
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }

    /// Writes the state atomically: a temporary file in the same directory, fsync, rename.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".state.toml.{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(self.to_toml().as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    }
}

impl super::App {
    /// The state to save: every tab's directory and view settings, and the history.
    pub fn state(&self) -> State {
        let side = |i: usize| SideState {
            tabs: self.sides[i]
                .tabs
                .iter()
                .map(|p| Tab {
                    path: Bytes::of(p.dir.as_os_str().as_bytes()),
                    sort: p.sort.key,
                    reverse: p.sort.reverse,
                    hidden: p.show_hidden,
                })
                .collect(),
            active: self.sides[i].active,
        };
        State {
            left: side(0),
            right: side(1),
            active: self.active,
            history: self.history.items.iter().map(|c| Bytes::of(c)).collect(),
        }
    }

    /// Replaces the tabs with restored ones. `keep_left` / `keep_right`: the command line
    /// named that side's directory, which wins over the restored tabs. A directory on the
    /// command line also starts on the left side (M1 9, amendment of 2026-10-05); without
    /// one the saved active side returns.
    pub fn restore(&mut self, s: &State, keep_left: bool, keep_right: bool) {
        for (i, (side, keep)) in [(&s.left, keep_left), (&s.right, keep_right)]
            .into_iter()
            .enumerate()
        {
            if keep || side.tabs.is_empty() {
                continue;
            }
            let mut tabs = Vec::new();
            for (k, t) in side.tabs.iter().enumerate() {
                let slot = if k == 0 {
                    self.sides[i].tabs[0].slot
                } else {
                    self.new_slot()
                };
                let mut p = crate::panel::Panel::new(slot, t.path());
                p.sort = crate::panel::sort::SortSpec {
                    key: t.sort,
                    reverse: t.reverse,
                };
                p.show_hidden = t.hidden;
                tabs.push(p);
            }
            self.sides[i].tabs = tabs;
            self.sides[i].active = side.active.min(self.sides[i].tabs.len() - 1);
        }
        self.active = if keep_left || keep_right {
            0
        } else {
            s.active.min(1)
        };
        for c in &s.history {
            self.history.push(c.bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_round_trip() {
        let s = State {
            left: SideState {
                tabs: vec![
                    Tab {
                        path: Bytes::of(b"/home/u/Documents"),
                        sort: SortKey::Size,
                        reverse: true,
                        hidden: false,
                    },
                    Tab {
                        path: Bytes::of(b"/tmp/new\nline \xff"),
                        sort: SortKey::Name,
                        reverse: false,
                        hidden: true,
                    },
                ],
                active: 1,
            },
            right: SideState::default(),
            active: 1,
            history: vec![Bytes::of(b"ls -l"), Bytes::of(b"printf '%s' 'bad\xff'")],
        };
        let text = s.to_toml();
        assert_eq!(State::parse(&text).unwrap(), s, "{text}");
        assert!(matches!(s.left.tabs[1].path, Bytes::Raw(_)));
        assert!(matches!(s.left.tabs[0].path, Bytes::Text(_)));
    }

    #[test]
    fn broken_state_is_normalized() {
        let s =
            State::parse("active = 7\n[left]\nactive = 5\n[[left.tabs]]\npath = \"relative\"\n")
                .unwrap();
        assert_eq!(s.active, 0);
        assert!(s.left.tabs.is_empty());
        assert_eq!(s.left.active, 0);
        assert!(State::parse("not toml [").is_err());
    }
}
