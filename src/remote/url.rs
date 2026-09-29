#![forbid(unsafe_code)]
//! The `sftp://` address grammar (P3 5.1): `sftp://[user@]host[:port][/path]`.
//!
//! The grammar is strict because `ssh_config` can expand the host and the user into
//! `ProxyCommand` and `Match exec` shell commands (the CVE-2023-51385 shape): a user or a
//! host holds only letters, digits, `.`, `_` and `-`, and never starts with `-`, so it can
//! never be read as an option or reach a shell as anything but a word. The transport puts
//! `--` before the host as well (P3 5.2). A query, fragment or parameter is refused. The
//! path is percent-decoded to bytes component by component; `.` is dropped, `..` removes
//! the previous component and is never sent, and every other component passes
//! `valid_component`.
//!
//! A connection is keyed by `(user, host, port)` as typed: the [`Target`].

use crate::fsops::plan::valid_component;
use crate::provider::{Target, VPath};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

/// The longest user name accepted.
pub const MAX_USER: usize = 64;
/// The longest host accepted (a DNS name's limit).
pub const MAX_HOST: usize = 253;

/// The refusal text of the design (P3 5.1).
pub const REFUSED: &str = "not a supported sftp:// address";

/// Where a remote place starts.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RemoteDir {
    /// Relative to the login directory: no path, `/~` or `/~/...`. It resolves through the
    /// `home-directory` extension or `REALPATH(".")` (P3 5.4).
    Home(VPath),
    /// An absolute directory on the server.
    Absolute(VPath),
}

/// A parsed `sftp://` address.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Address {
    pub target: Target,
    pub dir: RemoteDir,
}

/// Whether `text` is meant as an `sftp://` address (the `cd` argument).
pub fn is_sftp(text: &[u8]) -> bool {
    text.starts_with(b"sftp://")
}

fn refused(why: &str) -> String {
    format!("{REFUSED}: {why}")
}

/// Parses `sftp://[user@]host[:port][/path]`.
pub fn parse(text: &[u8]) -> Result<Address, String> {
    let rest = text
        .strip_prefix(b"sftp://")
        .ok_or_else(|| refused("it does not start with sftp://"))?;
    if rest.iter().any(|&c| c == b'?' || c == b'#') {
        return Err(refused("a query or fragment"));
    }
    let (authority, path) = match rest.iter().position(|&c| c == b'/') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    if authority.contains(&b';') {
        return Err(refused("a parameter"));
    }
    let (user, hostport) = match authority.iter().position(|&c| c == b'@') {
        Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
        None => (None, authority),
    };
    let user = match user {
        Some(u) if valid_user(u) => Some(ascii(u)),
        Some(_) => return Err(refused("the user")),
        None => None,
    };
    let (host, port) = split_port(hostport).ok_or_else(|| refused("the port"))?;
    if !valid_host(host) {
        return Err(refused("the host"));
    }
    let port = match port {
        None => None,
        Some(p) => Some(parse_port(p).ok_or_else(|| refused("the port"))?),
    };
    let target = Target {
        user,
        host: ascii(host),
        port,
    };
    let dir = match path {
        None => RemoteDir::Home(VPath::root()),
        Some(p) => parse_path(p)?,
    };
    Ok(Address { target, dir })
}

/// Splits `host[:port]`, with a bracketed IPv6 literal kept whole. `None`: a `:` with
/// nothing usable after it.
fn split_port(hp: &[u8]) -> Option<(&[u8], Option<&[u8]>)> {
    if hp.first() == Some(&b'[') {
        let end = hp.iter().position(|&c| c == b']')?;
        let (host, after) = hp.split_at(end + 1);
        return match after {
            [] => Some((host, None)),
            [b':', p @ ..] => Some((host, Some(p))),
            _ => None,
        };
    }
    match hp.iter().position(|&c| c == b':') {
        Some(i) => Some((&hp[..i], Some(&hp[i + 1..]))),
        None => Some((hp, None)),
    }
}

fn parse_port(p: &[u8]) -> Option<u16> {
    if p.is_empty() || p.len() > 5 || !p.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let n: u32 = std::str::from_utf8(p).ok()?.parse().ok()?;
    (1..=65535).contains(&n).then_some(n as u16)
}

fn ascii(b: &[u8]) -> String {
    // Only called on bytes the grammar checked: ASCII.
    String::from_utf8_lossy(b).into_owned()
}

fn word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-')
}

/// `[A-Za-z0-9._][A-Za-z0-9._-]*`, at most [`MAX_USER`] bytes.
pub fn valid_user(u: &[u8]) -> bool {
    !u.is_empty() && u.len() <= MAX_USER && u[0] != b'-' && u.iter().all(|&c| word_byte(c))
}

/// An `ssh_config` alias or DNS name, `[A-Za-z0-9_][A-Za-z0-9._-]*` of at most
/// [`MAX_HOST`] bytes, or a bracketed IPv6 literal of hex digits, `:` and `.`.
pub fn valid_host(h: &[u8]) -> bool {
    if h.is_empty() || h.len() > MAX_HOST {
        return false;
    }
    if let [b'[', inner @ .., b']'] = h {
        return !inner.is_empty()
            && inner.contains(&b':')
            && inner
                .iter()
                .all(|&c| c.is_ascii_hexdigit() || c == b':' || c == b'.');
    }
    (h[0].is_ascii_alphanumeric() || h[0] == b'_') && h.iter().all(|&c| word_byte(c))
}

/// Checks a [`Target`] built elsewhere (a bookmark, a history place) against the grammar
/// before anything is spawned for it.
pub fn check_target(t: &Target) -> Result<(), String> {
    if let Some(u) = &t.user
        && !valid_user(u.as_bytes())
    {
        return Err(refused("the user"));
    }
    if !valid_host(t.host.as_bytes()) {
        return Err(refused("the host"));
    }
    if t.port == Some(0) {
        return Err(refused("the port"));
    }
    Ok(())
}

/// The host as ssh takes it: an IPv6 literal without its brackets.
pub fn ssh_host(t: &Target) -> &str {
    let h = t.host.as_str();
    h.strip_prefix('[')
        .and_then(|x| x.strip_suffix(']'))
        .unwrap_or(h)
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Percent-decodes one path component. A `%` without two hex digits is refused.
fn decode(seg: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(seg.len());
    let mut i = 0;
    while i < seg.len() {
        if seg[i] == b'%' {
            let h = hex(*seg.get(i + 1)?)?;
            let l = hex(*seg.get(i + 2)?)?;
            out.push(h << 4 | l);
            i += 3;
        } else {
            out.push(seg[i]);
            i += 1;
        }
    }
    Some(out)
}

/// The path after the authority's `/`. `~` as the first component makes it relative to
/// the login directory.
fn parse_path(p: &[u8]) -> Result<RemoteDir, String> {
    let mut segs = p.split(|&c| c == b'/').peekable();
    let home = segs.peek() == Some(&&b"~"[..]);
    if home {
        segs.next();
    }
    let mut parts: Vec<Vec<u8>> = Vec::new();
    for s in segs {
        let c = decode(s).ok_or_else(|| refused("the path"))?;
        match &c[..] {
            b"" | b"." => {}
            b".." => {
                // Above the root there is only the root; above the login directory there
                // is no name to send.
                if parts.pop().is_none() && home {
                    return Err(refused("the path leaves the login directory"));
                }
            }
            _ => {
                if !valid_component(OsStr::from_bytes(&c)) {
                    return Err(refused("the path"));
                }
                parts.push(c);
            }
        }
    }
    let v = VPath::new(
        parts
            .into_iter()
            .map(|c| OsStr::from_bytes(&c).to_owned())
            .collect(),
    )
    .map_err(|_| refused("the path"))?;
    Ok(if home {
        RemoteDir::Home(v)
    } else {
        RemoteDir::Absolute(v)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(parts: &[&[u8]]) -> VPath {
        VPath::new(
            parts
                .iter()
                .map(|p| OsStr::from_bytes(p).to_owned())
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn the_parts_of_an_address() {
        let a = parse(b"sftp://u.s-1@my_host.example:2222/a/b").unwrap();
        assert_eq!(a.target.user.as_deref(), Some("u.s-1"));
        assert_eq!(a.target.host, "my_host.example");
        assert_eq!(a.target.port, Some(2222));
        assert_eq!(a.dir, RemoteDir::Absolute(vp(&[b"a", b"b"])));
        assert_eq!(a.target.address(), "sftp://u.s-1@my_host.example:2222");
        let a = parse(b"sftp://h").unwrap();
        assert_eq!(a.target.user, None);
        assert_eq!(a.target.port, None);
        assert_eq!(a.dir, RemoteDir::Home(VPath::root()));
        assert_eq!(
            parse(b"sftp://h/").unwrap().dir,
            RemoteDir::Absolute(VPath::root())
        );
        assert_eq!(
            parse(b"sftp://h/~").unwrap().dir,
            RemoteDir::Home(VPath::root())
        );
        assert_eq!(
            parse(b"sftp://h/~/x/./y//").unwrap().dir,
            RemoteDir::Home(vp(&[b"x", b"y"]))
        );
        // A `~` that is not the first component is a name.
        assert_eq!(
            parse(b"sftp://h/x/~").unwrap().dir,
            RemoteDir::Absolute(vp(&[b"x", b"~"]))
        );
    }

    #[test]
    fn dot_dot_is_resolved_and_never_sent() {
        assert_eq!(
            parse(b"sftp://h/a/../b/..").unwrap().dir,
            RemoteDir::Absolute(VPath::root())
        );
        assert_eq!(
            parse(b"sftp://h/../../etc").unwrap().dir,
            RemoteDir::Absolute(vp(&[b"etc"]))
        );
        assert_eq!(
            parse(b"sftp://h/~/a/..").unwrap().dir,
            RemoteDir::Home(VPath::root())
        );
        assert!(parse(b"sftp://h/~/..").is_err());
        // A percent-encoded `..` is still `..`.
        assert_eq!(
            parse(b"sftp://h/a/%2e%2E").unwrap().dir,
            RemoteDir::Absolute(VPath::root())
        );
    }

    #[test]
    fn the_path_is_percent_decoded_to_bytes() {
        assert_eq!(
            parse(b"sftp://h/a%20b/%ff%0a").unwrap().dir,
            RemoteDir::Absolute(vp(&[b"a b", b"\xff\n"]))
        );
        // Raw bytes pass as they are; `;` is a name byte in the path.
        assert_eq!(
            parse(b"sftp://h/x;y/\xc3\xa6").unwrap().dir,
            RemoteDir::Absolute(vp(&[b"x;y", b"\xc3\xa6"]))
        );
        for bad in [
            &b"sftp://h/a%2fb"[..],
            b"sftp://h/a%00b",
            b"sftp://h/a%",
            b"sftp://h/a%4",
            b"sftp://h/a%zz",
        ] {
            assert!(parse(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn hosts_and_users_never_become_options_or_shell_words() {
        for bad in [
            &b"sftp://-oProxyCommand=x"[..],
            b"sftp://-h",
            b"sftp://-u@h",
            b"sftp://u@-h",
            b"sftp://h%41",
            b"sftp://h$(id)",
            b"sftp://h;x",
            b"sftp://u;fingerprint=x@h",
            b"sftp://h`x`",
            b"sftp://h x",
            b"sftp://a|b",
            b"sftp://a&b",
            b"sftp://a'b",
            b"sftp://a\"b",
            b"sftp://a*b",
            b"sftp://a\\b",
            b"sftp://.h",
            b"sftp://",
            b"sftp://@h",
            b"sftp://u@",
            b"sftp://u@v@h",
            b"sftp://h:",
            b"sftp://h:0",
            b"sftp://h:65536",
            b"sftp://h:22x",
            b"sftp://h:-1",
            b"sftp://h?x=1",
            b"sftp://h/p#frag",
            b"sftp://[]",
            b"sftp://[zz:1]",
            b"sftp://[1:2]x",
            b"sftp://h\xc3\xa6",
            b"ssh://h",
        ] {
            assert!(parse(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
        let addr = |user: usize, host: usize| {
            let mut a = b"sftp://".to_vec();
            if user > 0 {
                a.extend(std::iter::repeat_n(b'u', user));
                a.push(b'@');
            }
            a.extend(std::iter::repeat_n(b'h', host));
            a
        };
        assert!(parse(&addr(0, 253)).is_ok());
        assert!(parse(&addr(0, 254)).is_err());
        assert!(parse(&addr(64, 1)).is_ok());
        assert!(parse(&addr(65, 1)).is_err());
        assert_eq!(parse(b"sftp://h:65535").unwrap().target.port, Some(65535));
        assert_eq!(parse(b"sftp://_h").unwrap().target.host, "_h");
        assert!(parse(b"sftp://-h").unwrap_err().starts_with(REFUSED));
    }

    #[test]
    fn a_bracketed_literal_keeps_its_brackets_in_the_key_only() {
        let a = parse(b"sftp://u@[fe80:1]:22/x").unwrap();
        assert_eq!(a.target.host, "[fe80:1]");
        assert_eq!(a.target.port, Some(22));
        assert_eq!(ssh_host(&a.target), "fe80:1");
        assert_eq!(ssh_host(&parse(b"sftp://h").unwrap().target), "h");
    }

    #[test]
    fn a_target_from_elsewhere_is_checked_again() {
        let ok = Target {
            user: Some("u".into()),
            host: "h".into(),
            port: Some(22),
        };
        assert!(check_target(&ok).is_ok());
        for bad in [
            Target {
                host: "-oX".into(),
                ..ok.clone()
            },
            Target {
                user: Some("-l".into()),
                ..ok.clone()
            },
            Target {
                port: Some(0),
                ..ok.clone()
            },
            Target {
                host: "a b".into(),
                ..ok.clone()
            },
        ] {
            assert!(check_target(&bad).is_err(), "{bad:?}");
        }
    }
}
