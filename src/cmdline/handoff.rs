#![forbid(unsafe_code)]
//! Hand-offs (design section 6): the command line, F3, F4 and Ctrl+O give the terminal to
//! something else. Programs are spawned by argv; a filename never reaches a shell
//! unquoted (NFR-SEC).

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Handoff {
    /// `[$SHELL, "-c", text]` in `cwd`, then `[exit N] press Enter to return`.
    Shell {
        shell: OsString,
        text: Vec<u8>,
        cwd: PathBuf,
    },
    /// F3, F4, Shift+F4: argv from `$PAGER` / `$EDITOR` plus the path.
    Program { argv: Vec<OsString>, cwd: PathBuf },
    /// Ctrl+O: show the terminal's normal screen until a key is pressed.
    ShowScreen,
}

/// Splits `$PAGER` / `$EDITOR` with shell-word rules and appends `path` as its own
/// argument; no shell is involved. `None` when the variable does not parse.
pub fn program_argv(var: &str, path: &Path) -> Option<Vec<OsString>> {
    let words = shell_words::split(var).ok()?;
    if words.is_empty() {
        return None;
    }
    let mut argv: Vec<OsString> = words.into_iter().map(OsString::from).collect();
    argv.push(path.as_os_str().to_owned());
    Some(argv)
}

/// The pager command: config override, `$PAGER`, `less`.
pub fn pager(config: Option<&str>, env: Option<&OsStr>) -> String {
    config
        .map(str::to_owned)
        .or_else(|| {
            env.and_then(|v| v.to_str())
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "less".into())
}

/// The editor commands to try in order: config override or `$EDITOR`, else `nvim`, then
/// `vi`.
pub fn editors(config: Option<&str>, env: Option<&OsStr>) -> Vec<String> {
    if let Some(e) = config.map(str::to_owned).or_else(|| {
        env.and_then(|v| v.to_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    }) {
        return vec![e];
    }
    vec!["nvim".into(), "vi".into()]
}

/// `[exit N]` or `[killed by signal S]`.
pub fn status_text(status: std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(c), _) => format!("[exit {c}]"),
        (None, Some(s)) => format!("[killed by signal {s}]"),
        _ => "[exit ?]".into(),
    }
}

/// A path as the shell would get it, for display.
pub fn display_bytes(b: &[u8]) -> String {
    Path::new(OsStr::from_bytes(b)).display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_splitting() {
        let a = program_argv("less -R", Path::new("/t/a b")).unwrap();
        assert_eq!(a, ["less", "-R", "/t/a b"]);
        let a = program_argv("'my pager' --x", Path::new("-n")).unwrap();
        assert_eq!(a, ["my pager", "--x", "-n"]);
        assert!(program_argv("", Path::new("x")).is_none());
        assert_eq!(pager(None, None), "less");
        assert_eq!(editors(None, Some(OsStr::new(""))), ["nvim", "vi"]);
        assert_eq!(editors(Some("hx"), Some(OsStr::new("vim"))), ["hx"]);
    }
}
