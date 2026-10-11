#![forbid(unsafe_code)]
//! Hand-offs (design section 6): the command line, F3, F4 and Ctrl+O give the terminal to
//! something else. Programs are spawned by argv; a filename never reaches a shell
//! unquoted (NFR-SEC).

use std::ffi::{OsStr, OsString};
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

/// The command the config sets, else the environment variable's when it is set and not empty.
fn configured(config: Option<&str>, env: Option<&OsStr>) -> Option<String> {
    config.map(str::to_owned).or_else(|| {
        env.and_then(|v| v.to_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    })
}

/// The pager command: config override, `$PAGER`, `less`.
pub fn pager(config: Option<&str>, env: Option<&OsStr>) -> String {
    configured(config, env).unwrap_or_else(|| "less".into())
}

/// The editor command, as a list of one: config override, `$EDITOR`, else the built-in
/// default, `nvim` when an `nvim` is on `PATH` and plain `vi` otherwise ([`default_editor`]).
/// Only that default falls back: an editor the config or `$EDITOR` names that is not
/// installed fails with its own name.
pub fn editors(config: Option<&str>, env: Option<&OsStr>) -> Vec<String> {
    vec![
        configured(config, env)
            .unwrap_or_else(|| default_editor(std::env::var_os("PATH").as_deref()).to_owned()),
    ]
}

/// The built-in editor for `path` (the `PATH` value): `nvim` when it is on it, else `vi`.
pub fn default_editor(path: Option<&OsStr>) -> &'static str {
    if on_path("nvim", path) { "nvim" } else { "vi" }
}

/// Whether an executable file named `name` is in one of the absolute directories of `path`
/// (the `PATH` value). A relative directory would resolve against the panel's directory, so
/// it does not count.
pub fn on_path(name: &str, path: Option<&OsStr>) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_some_and(|p| {
        std::env::split_paths(p).any(|d| {
            d.is_absolute()
                && std::fs::metadata(d.join(name))
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
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
        assert_eq!(pager(None, Some(OsStr::new(""))), "less");
        assert_eq!(pager(Some("most"), Some(OsStr::new("less -R"))), "most");
        assert_eq!(editors(Some("hx"), Some(OsStr::new("vim"))), ["hx"]);
        assert_eq!(editors(None, Some(OsStr::new("vim -u x"))), ["vim -u x"]);
        // An editor that is named is used as named: no fallback to vi.
        assert_eq!(editors(Some("nvim"), None), ["nvim"]);
        assert_eq!(editors(None, Some(OsStr::new("nvim"))), ["nvim"]);
        assert_eq!(
            editors(None, Some(OsStr::new(""))),
            [default_editor(std::env::var_os("PATH").as_deref())]
        );
    }

    /// The built-in editor: `nvim` when an executable `nvim` is in an absolute `PATH`
    /// directory, else plain `vi`.
    #[test]
    fn the_default_editor_is_nvim_when_installed_else_vi() {
        use std::os::unix::fs::PermissionsExt;
        let t = std::env::temp_dir().join(format!("mc-editor-{}", std::process::id()));
        let (with, without, noexec) = (t.join("with"), t.join("without"), t.join("noexec"));
        for (dir, mode) in [(&with, 0o755), (&noexec, 0o644)] {
            std::fs::create_dir_all(dir).unwrap();
            let p = dir.join("nvim");
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        std::fs::create_dir_all(&without).unwrap();
        let path = |dirs: &[&Path]| std::env::join_paths(dirs).unwrap();
        assert_eq!(default_editor(Some(&path(&[&without, &with]))), "nvim");
        assert_eq!(default_editor(Some(&path(&[&without]))), "vi");
        assert_eq!(
            default_editor(Some(&path(&[&noexec]))),
            "vi",
            "not executable"
        );
        assert_eq!(default_editor(Some(OsStr::new("with"))), "vi", "relative");
        assert_eq!(default_editor(None), "vi");
        let _ = std::fs::remove_dir_all(&t);
    }
}
