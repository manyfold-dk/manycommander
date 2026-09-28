#![forbid(unsafe_code)]
//! The command line (design section 6): a single-line editor above the function-key bar,
//! byte-oriented shell quoting for inserted names, `cd` with limited expansion, and `z`
//! (P2 3.4).
//!
//! The line holds bytes, not a `String`: an inserted name may carry newlines or invalid
//! UTF-8 inside its quotes, and the shell must receive exactly those bytes.

pub mod handoff;

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

/// Wraps `name` in single quotes; each `'` becomes `'\''`. Newlines and invalid UTF-8 pass
/// through unchanged inside the quotes, so the shell receives exactly one argument.
pub fn quote(name: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len() + 2);
    out.push(b'\'');
    for &c in name {
        if c == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(c);
        }
    }
    out.push(b'\'');
    out
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    buf: Vec<u8>,
    /// Byte offset of the cursor.
    pos: usize,
}

/// The length of the UTF-8 character starting at `b[i]`, or 1 for an invalid byte.
fn char_len(b: &[u8], i: usize) -> usize {
    let n = match b[i] {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    };
    if i + n <= b.len() && std::str::from_utf8(&b[i..i + n]).is_ok() {
        n
    } else {
        1
    }
}

fn prev_boundary(b: &[u8], pos: usize) -> usize {
    let mut i = 0;
    let mut last = 0;
    while i < pos {
        last = i;
        i += char_len(b, i);
    }
    last
}

impl Line {
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.buf
    }

    pub fn cursor(&self) -> usize {
        self.pos
    }

    pub fn set(&mut self, text: &[u8]) {
        self.buf = text.to_vec();
        self.pos = self.buf.len();
    }

    pub fn take(&mut self) -> Vec<u8> {
        self.pos = 0;
        std::mem::take(&mut self.buf)
    }

    pub fn clear(&mut self) {
        self.buf.clear();
        self.pos = 0;
    }

    pub fn insert_char(&mut self, c: char) {
        let mut tmp = [0u8; 4];
        self.insert_bytes(c.encode_utf8(&mut tmp).as_bytes());
    }

    pub fn insert_bytes(&mut self, b: &[u8]) {
        self.buf.splice(self.pos..self.pos, b.iter().copied());
        self.pos += b.len();
    }

    pub fn backspace(&mut self) {
        if self.pos > 0 {
            let p = prev_boundary(&self.buf, self.pos);
            self.buf.drain(p..self.pos);
            self.pos = p;
        }
    }

    pub fn delete(&mut self) {
        if self.pos < self.buf.len() {
            let n = char_len(&self.buf, self.pos);
            self.buf.drain(self.pos..self.pos + n);
        }
    }

    pub fn left(&mut self) {
        if self.pos > 0 {
            self.pos = prev_boundary(&self.buf, self.pos);
        }
    }

    pub fn right(&mut self) {
        if self.pos < self.buf.len() {
            self.pos += char_len(&self.buf, self.pos);
        }
    }

    pub fn home(&mut self) {
        self.pos = 0;
    }

    pub fn end(&mut self) {
        self.pos = self.buf.len();
    }

    pub fn kill_start(&mut self) {
        self.buf.drain(..self.pos);
        self.pos = 0;
    }

    pub fn kill_end(&mut self) {
        self.buf.truncate(self.pos);
    }

    /// Deletes the word before the cursor (and the spaces after it).
    pub fn kill_word(&mut self) {
        let mut p = self.pos;
        while p > 0 && self.buf[p - 1] == b' ' {
            p -= 1;
        }
        while p > 0 && self.buf[p - 1] != b' ' {
            p -= 1;
        }
        self.buf.drain(p..self.pos);
        self.pos = p;
    }
}

/// In-session command history, browsed with Ctrl+P / Ctrl+N.
#[derive(Clone, Debug, Default)]
pub struct History {
    pub items: Vec<Vec<u8>>,
    browse: Option<usize>,
    draft: Vec<u8>,
}

impl History {
    pub const CAP: usize = 500;

    pub fn push(&mut self, cmd: &[u8]) {
        if cmd.is_empty() {
            return;
        }
        self.items.retain(|c| c != cmd);
        self.items.push(cmd.to_vec());
        if self.items.len() > Self::CAP {
            self.items.remove(0);
        }
        self.browse = None;
    }

    pub fn prev(&mut self, line: &mut Line) {
        if self.items.is_empty() {
            return;
        }
        let i = match self.browse {
            None => {
                self.draft = line.bytes().to_vec();
                self.items.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.browse = Some(i);
        line.set(&self.items[i]);
    }

    pub fn next(&mut self, line: &mut Line) {
        match self.browse {
            None => {}
            Some(i) if i + 1 < self.items.len() => {
                self.browse = Some(i + 1);
                line.set(&self.items[i + 1]);
            }
            Some(_) => {
                self.browse = None;
                let d = std::mem::take(&mut self.draft);
                line.set(&d);
            }
        }
    }

    pub fn reset(&mut self) {
        self.browse = None;
    }
}

/// What `Enter` on the line does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// `cd <path>`: the panel changes directory internally.
    Cd(PathBuf),
    /// `z <keywords>` (P2 3.4): the panel goes to the best frecency match; `z` alone
    /// opens the directories dialog. The keywords are taken as typed, without expansion.
    Z(Vec<u8>),
    /// Anything else: `[$SHELL, "-c", text]`.
    Shell(Vec<u8>),
}

/// Environment lookups for expansion, injectable for tests.
pub trait Env {
    fn var(&self, name: &str) -> Option<OsString>;
}

pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn var(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name)
    }
}

/// Parses the line. `cd` expands only a leading `~` and `$VAR` / `${VAR}`, and removes
/// quotes; there is no command substitution and no globbing. `z` never reaches the shell.
pub fn parse(text: &[u8], env: &dyn Env) -> Command {
    let t = trim(text);
    if t == b"z" || t.starts_with(b"z ") || t.starts_with(b"z\t") {
        return Command::Z(trim(&t[1..]).to_vec());
    }
    let is_cd = t == b"cd" || t.starts_with(b"cd ") || t.starts_with(b"cd\t");
    if !is_cd {
        return Command::Shell(text.to_vec());
    }
    let arg = trim(&t[2..]);
    if arg.is_empty() || arg == b"~" {
        return Command::Cd(
            env.var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/")),
        );
    }
    Command::Cd(PathBuf::from(OsString::from_vec(expand(arg, env))))
}

fn trim(b: &[u8]) -> &[u8] {
    let s = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    let e = b
        .iter()
        .rposition(|c| !c.is_ascii_whitespace())
        .map(|i| i + 1)
        .unwrap_or(s);
    &b[s..e.max(s)]
}

/// Quote removal plus `~` and `$VAR` expansion (outside single quotes).
fn expand(arg: &[u8], env: &dyn Env) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    if arg == b"~" || arg.starts_with(b"~/") {
        if let Some(h) = env.var("HOME") {
            out.extend_from_slice(h.as_bytes());
        }
        i = 1;
    }
    let mut dq = false;
    while i < arg.len() {
        let c = arg[i];
        match c {
            b'\'' if !dq => {
                let end = arg[i + 1..]
                    .iter()
                    .position(|&x| x == b'\'')
                    .map(|p| i + 1 + p)
                    .unwrap_or(arg.len());
                out.extend_from_slice(&arg[i + 1..end]);
                i = end + 1;
            }
            b'"' => {
                dq = !dq;
                i += 1;
            }
            b'\\' if i + 1 < arg.len() => {
                out.push(arg[i + 1]);
                i += 2;
            }
            b'$' => {
                let (name, used) = if arg.get(i + 1) == Some(&b'{') {
                    match arg[i + 2..].iter().position(|&x| x == b'}') {
                        Some(p) => (&arg[i + 2..i + 2 + p], p + 3),
                        None => (&arg[i..i], 0),
                    }
                } else {
                    let n = arg[i + 1..]
                        .iter()
                        .take_while(|c| c.is_ascii_alphanumeric() || **c == b'_')
                        .count();
                    (&arg[i + 1..i + 1 + n], n + 1)
                };
                if name.is_empty() || used == 0 {
                    out.push(b'$');
                    i += 1;
                } else {
                    if let Some(v) = std::str::from_utf8(name).ok().and_then(|n| env.var(n)) {
                        out.extend_from_slice(v.as_bytes());
                    }
                    i += used;
                }
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// The shell for the command line: `$SHELL`, else `/bin/sh`.
pub fn shell(env: &dyn Env) -> OsString {
    env.var("SHELL")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| OsStr::new("/bin/sh").to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Map(HashMap<&'static str, &'static str>);
    impl Env for Map {
        fn var(&self, n: &str) -> Option<OsString> {
            self.0.get(n).map(OsString::from)
        }
    }

    fn env() -> Map {
        Map(HashMap::from([("HOME", "/home/u"), ("X", "ex")]))
    }

    #[test]
    fn quoting() {
        assert_eq!(quote(b"a b"), b"'a b'");
        assert_eq!(quote(b"it's"), b"'it'\\''s'");
        assert_eq!(quote(b"new\nline\xff"), b"'new\nline\xff'");
    }

    #[test]
    fn cd_parsing() {
        let e = env();
        assert_eq!(parse(b"cd", &e), Command::Cd("/home/u".into()));
        assert_eq!(parse(b"cd ~/x", &e), Command::Cd("/home/u/x".into()));
        assert_eq!(parse(b"cd $X/${X}y", &e), Command::Cd("ex/exy".into()));
        assert_eq!(parse(b"cd 'a b'", &e), Command::Cd("a b".into()));
        assert_eq!(parse(b"cd '$X'", &e), Command::Cd("$X".into()));
        assert_eq!(
            parse(b"cd $(rm -rf /)", &e),
            Command::Cd("$(rm -rf /)".into())
        );
        assert_eq!(parse(b"cd *", &e), Command::Cd("*".into()));
        assert_eq!(parse(b"cdx", &e), Command::Shell(b"cdx".to_vec()));
        assert_eq!(parse(b"ls -l", &e), Command::Shell(b"ls -l".to_vec()));
    }

    #[test]
    fn z_parsing() {
        let e = env();
        assert_eq!(parse(b"z", &e), Command::Z(Vec::new()));
        assert_eq!(parse(b"  z  ", &e), Command::Z(Vec::new()));
        assert_eq!(parse(b"z foo  bar ", &e), Command::Z(b"foo  bar".to_vec()));
        assert_eq!(parse(b"z\t$X", &e), Command::Z(b"$X".to_vec()));
        assert_eq!(parse(b"zz", &e), Command::Shell(b"zz".to_vec()));
        assert_eq!(parse(b"zi foo", &e), Command::Shell(b"zi foo".to_vec()));
    }

    #[test]
    fn editing() {
        let mut l = Line::default();
        for c in "héllo wörld".chars() {
            l.insert_char(c);
        }
        l.backspace();
        assert_eq!(l.bytes(), "héllo wörl".as_bytes());
        l.kill_word();
        assert_eq!(l.bytes(), "héllo ".as_bytes());
        l.left();
        l.left();
        l.insert_bytes(b"\xff");
        assert_eq!(l.bytes(), b"h\xc3\xa9ll\xffo ");
        l.home();
        l.kill_end();
        assert!(l.is_empty());
        let mut h = History::default();
        h.push(b"one");
        h.push(b"two");
        l.set(b"draft");
        h.prev(&mut l);
        assert_eq!(l.bytes(), b"two");
        h.prev(&mut l);
        assert_eq!(l.bytes(), b"one");
        h.next(&mut l);
        h.next(&mut l);
        assert_eq!(l.bytes(), b"draft");
    }
}
