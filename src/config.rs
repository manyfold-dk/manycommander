#![forbid(unsafe_code)]
//! `~/.config/manycommander/config.toml` (or under `$XDG_CONFIG_HOME`). Every key is
//! optional; unknown keys are ignored. An example:
//!
//! ```toml
//! paint_background = false   # true: the panels take the palette's background colour
//! pager = "less -R"          # the pager for F3, over $PAGER
//! editor = "nvim"            # the editor for F4, over $EDITOR
//!
//! [jump]
//! zoxide = "auto"            # "off": the directories dialog never runs zoxide
//!
//! [preview]
//! protocol = "auto"          # the quick view's images: "auto" asks the terminal;
//!                            # "kitty", "sixel", "halfblocks" or "off" decide
//!
//! [sftp]
//! ssh = ["ssh", "-F", "/path/ssh_config"]   # the ssh program and its own arguments
//! ```
//!
//! Without `pager` and `$PAGER`, F3 runs `less`. Without `editor` and `$EDITOR`, F4 runs
//! `nvim` when it is installed, else `vi`; an editor named in either is run as named.
//!
//! `sftp.ssh`: manycommander's fixed ssh options follow the program and win over the rest;
//! an argument that is or starts with `-o`, or holds `-A`, `-X`, `-Y`, `-t` or `-e`, is
//! rejected with the argument named.
//!
//! The settings are specified in TDR-1 7.1 (`paint_background`), P2 3.3 (`jump.zoxide`),
//! P3 4.3 (`preview.protocol`) and P3 5.2 (`sftp.ssh`).
//!
//! A file that does not parse, or holds a value of the wrong kind, is ignored as a whole:
//! every setting is at its default, and the status line says so once, with the line and
//! column of the error.

use serde::Deserialize;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `true`: the panels use the palette's `background` role (TDR-1 7.1).
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
    /// The config in `text`. The error is one line, for the status line: where, then what.
    pub fn parse(text: &str) -> Result<Config, String> {
        toml::from_str(text).map_err(|e| {
            let what = e.message().trim_end().replace('\n', " ");
            match e.span().and_then(|s| text.get(..s.start)) {
                Some(before) => {
                    let line = before.matches('\n').count() + 1;
                    let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
                    format!("line {line}, column {column}: {what}")
                }
                None => what,
            }
        })
    }

    /// Reads the config file. A missing file is the default config; a file that does not
    /// parse is the default config, for every key, plus the error, which the app shows once.
    pub fn load(path: &Path) -> (Config, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(t) => match Config::parse(&t) {
                Ok(c) => (c, None),
                Err(e) => (
                    Config::default(),
                    Some(format!(
                        "{}: {e} (every setting is at its default)",
                        path.display()
                    )),
                ),
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

    /// One bad value: the whole file is ignored, and the message, one line, says where and
    /// that every setting is at its default.
    #[test]
    fn a_bad_value_is_reported_on_one_line() {
        let text = "pager = \"most\"\npaint_background = \"yes\"\n";
        assert_eq!(
            Config::parse(text).unwrap_err(),
            "line 2, column 20: invalid type: string \"yes\", expected a boolean"
        );
        let p = std::env::temp_dir().join(format!("mc-config-{}.toml", std::process::id()));
        std::fs::write(&p, text).unwrap();
        let (c, e) = Config::load(&p);
        let _ = std::fs::remove_file(&p);
        assert_eq!(c, Config::default(), "the pager too is at its default");
        let e = e.unwrap();
        assert!(!e.contains('\n'), "{e}");
        let want = "line 2, column 20: invalid type: string \"yes\", expected a boolean \
                    (every setting is at its default)";
        assert!(e.ends_with(want), "{e}");
        assert_eq!(
            Config::parse("pager = [").unwrap_err().lines().count(),
            1,
            "a syntax error"
        );
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
