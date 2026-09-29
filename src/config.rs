#![forbid(unsafe_code)]
//! `~/.config/manycommander/config.toml`. Every key is optional; unknown keys are ignored.
//!
//! ```toml
//! paint_background = false   # true: panels use the palette's `background` (design 7.1)
//! pager = "less -R"          # overrides $PAGER for F3
//! editor = "nvim"            # overrides $EDITOR for F4
//!
//! [jump]
//! zoxide = "auto"            # "off": do not read zoxide's ranking (P2 3.3)
//!
//! [preview]
//! protocol = "auto"          # "kitty", "sixel", "halfblocks" or "off" (P3 4.3)
//!
//! [sftp]
//! ssh = ["ssh", "-F", "/path/ssh_config"]   # the program and its own arguments (P3 5.2)
//! ```
//!
//! `sftp.ssh`: manycommander's fixed ssh options follow the program and win over the rest;
//! an argument that is or starts with `-o`, or holds `-A`, `-X`, `-Y`, `-t` or `-e`, is
//! rejected with the argument named.

use serde::Deserialize;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Config {
    pub paint_background: bool,
    pub pager: Option<String>,
    pub editor: Option<String>,
    pub jump: Jump,
    pub preview: Preview,
    pub sftp: Sftp,
}

/// The `[sftp]` table (P3 5.2).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Sftp {
    /// `sftp.ssh`: the ssh program and its own arguments; `None` is `["ssh"]`. Checked at
    /// connect time by `remote::transport::SshCommand::from_setting`.
    pub ssh: Option<Vec<String>>,
}

/// The `[preview]` table: the quick view (P3 4).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Preview {
    pub protocol: ProtocolSetting,
}

/// `preview.protocol` (P3 4.3): `"auto"` follows the terminal probe; `"kitty"`, `"sixel"`
/// and `"halfblocks"` override it; `"off"` shows only the info card.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtocolSetting {
    #[default]
    Auto,
    Kitty,
    Sixel,
    Halfblocks,
    Off,
}

/// The `[jump]` table: the directories dialog and `z` (P2 3).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Jump {
    pub zoxide: ZoxideMode,
}

/// `jump.zoxide` (P2 3.3): `"auto"` reads zoxide's ranking when `zoxide` is on `PATH`;
/// `"off"` never runs it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ZoxideMode {
    #[default]
    Auto,
    Off,
}

impl Config {
    pub fn parse(text: &str) -> Result<Config, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    /// Reads the config file. A missing file is the default config; a file that does not
    /// parse is the default config plus the error, which the app shows once.
    pub fn load(path: &Path) -> (Config, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(t) => match Config::parse(&t) {
                Ok(c) => (c, None),
                Err(e) => (Config::default(), Some(format!("{}: {e}", path.display()))),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Config::default(), None),
            Err(e) => (Config::default(), Some(format!("{}: {e}", path.display()))),
        }
    }

    /// `$XDG_CONFIG_HOME/manycommander/config.toml`, or under `$HOME/.config`.
    pub fn path(xdg: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
        let base = match xdg.filter(|x| x.as_encoded_bytes().first() == Some(&b'/')) {
            Some(x) => PathBuf::from(x),
            None => Path::new(home.filter(|h| !h.is_empty())?).join(".config"),
        };
        Some(base.join("manycommander/config.toml"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
        let c =
            Config::parse("paint_background = true\npager = \"less -R\"\nfuture = 1\n").unwrap();
        assert!(c.paint_background);
        assert_eq!(c.pager.as_deref(), Some("less -R"));
        assert!(Config::parse("paint_background = \"yes\"").is_err());
        assert_eq!(c.jump.zoxide, ZoxideMode::Auto);
        let c = Config::parse("[jump]\nzoxide = \"off\"\nfuture = 2\n").unwrap();
        assert_eq!(c.jump.zoxide, ZoxideMode::Off);
        assert!(Config::parse("[jump]\nzoxide = \"sometimes\"").is_err());
        assert_eq!(c.preview.protocol, ProtocolSetting::Auto);
        let c = Config::parse("[preview]\nprotocol = \"sixel\"\n").unwrap();
        assert_eq!(c.preview.protocol, ProtocolSetting::Sixel);
        assert!(Config::parse("[preview]\nprotocol = \"iterm\"").is_err());
        assert_eq!(c.sftp.ssh, None);
        let c = Config::parse("[sftp]\nssh = [\"ssh\", \"-F\", \"/c\"]\n").unwrap();
        assert_eq!(
            c.sftp.ssh.as_deref(),
            Some(&["ssh".to_owned(), "-F".into(), "/c".into()][..])
        );
        assert!(Config::parse("[sftp]\nssh = \"ssh -F x\"").is_err());
    }

    #[test]
    fn path_resolution() {
        let h = Some(OsStr::new("/home/u"));
        assert_eq!(
            Config::path(None, h).unwrap(),
            Path::new("/home/u/.config/manycommander/config.toml")
        );
        assert_eq!(
            Config::path(Some(OsStr::new("/c")), h).unwrap(),
            Path::new("/c/manycommander/config.toml")
        );
    }
}
