#![forbid(unsafe_code)]
//! The terminal probe (P3 4.2, V-6): once at startup, after raw mode and before the input
//! thread exists, one write of every query and one read loop with a 100 ms deadline.
//!
//! | Query | The reply means |
//! |---|---|
//! | kitty graphics `ESC _G i=<id>,s=1,v=1,a=q,t=d,f=24;AAAA ESC \` | `ESC _G i=<id>;OK ESC \`: kitty graphics |
//! | `CSI 16 t` | `CSI 6 ; <height> ; <width> t`: the cell size in pixels |
//! | `CSI ? u` | `CSI ? <flags> u`: the kitty keyboard protocol |
//! | `CSI c` (DA1), last | the end of the replies; a parameter `4` means sixel |
//!
//! A terminal answers in order, so outside tmux the read ends at the DA1 reply. Inside
//! tmux the graphics query goes through tmux's passthrough (`ESC Ptmux;` with doubled
//! escapes), and its reply can come after tmux answers DA1 itself, so the read goes on
//! until the graphics reply or the deadline. Every byte that is not a reply, such as a key
//! typed during the probe, is discarded (V-6). The probe never changes a terminal or tmux
//! setting (V-3), and environment variables never enable a protocol on their own: they only
//! say whether the graphics query is wrapped for tmux.

use super::Protocol;
use crate::config::ProtocolSetting;
use crate::theme::Depth;
use rustix::fd::BorrowedFd;
use std::ffi::OsStr;
use std::time::{Duration, Instant};

/// The probe's read deadline (V-6).
pub const DEADLINE: Duration = Duration::from_millis(100);

/// Replies longer than this are garbage; the parser drops them.
const MAX_PENDING: usize = 64 << 10;

/// What the terminal answered (P3 4.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Probed {
    /// A graphics reply arrived, `OK` or not.
    pub graphics_reply: bool,
    /// The graphics reply was `OK` for our id: kitty graphics.
    pub graphics: bool,
    /// DA1 lists `4`.
    pub sixel: bool,
    /// The kitty keyboard protocol answered (`CSI ? <flags> u`).
    pub keyboard: bool,
    /// The cell size in pixels, `(width, height)`: from `CSI 16 t`, else from the
    /// `TIOCGWINSZ` pixel fields.
    pub cell: Option<(u16, u16)>,
    /// The DA1 reply arrived.
    pub da1: bool,
    /// The graphics query went through tmux's passthrough.
    pub tmux: bool,
    /// Bytes that were not replies (keys typed during the probe): discarded (V-6).
    pub discarded: usize,
    /// How long the read loop ran.
    pub elapsed: Duration,
}

/// Whether manycommander runs inside tmux: `TMUX` set, `TERM` starting with `tmux`, or
/// `TERM_PROGRAM` equal to `tmux`. It only decides that the graphics query and the kitty
/// transmits go through tmux's passthrough; manycommander never runs `tmux` (V-3).
pub fn in_tmux(term: Option<&OsStr>, term_program: Option<&OsStr>, tmux: Option<&OsStr>) -> bool {
    tmux.is_some_and(|t| !t.is_empty())
        || term.is_some_and(|t| t.as_encoded_bytes().starts_with(b"tmux"))
        || term_program.is_some_and(|t| t == "tmux")
}

/// [`in_tmux`] from the process environment.
pub fn in_tmux_env() -> bool {
    in_tmux(
        std::env::var_os("TERM").as_deref(),
        std::env::var_os("TERM_PROGRAM").as_deref(),
        std::env::var_os("TMUX").as_deref(),
    )
}

/// Wraps one escape sequence in tmux's passthrough: `ESC Ptmux;`, the sequence with every
/// `ESC` doubled, `ESC \`.
pub fn passthrough(seq: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1bPtmux;");
    for &b in seq {
        if b == 0x1b {
            out.push(0x1b);
        }
        out.push(b);
    }
    out.extend_from_slice(b"\x1b\\");
}

/// The probe's queries in the order of the table above, as the one write (V-6).
pub fn query(tmux: bool, id: u32) -> Vec<u8> {
    let graphics = format!("\x1b_Gi={id},s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\");
    let mut q = Vec::with_capacity(96);
    if tmux {
        passthrough(graphics.as_bytes(), &mut q);
    } else {
        q.extend_from_slice(graphics.as_bytes());
    }
    q.extend_from_slice(b"\x1b[16t\x1b[?u\x1b[c");
    q
}

/// Parses the terminal's replies from bytes as they arrive (P3 4.2). A sequence split
/// across reads waits for its rest; everything that is not a reply is discarded.
#[derive(Debug)]
pub struct Parser {
    id: u32,
    tmux: bool,
    pending: Vec<u8>,
    got: Probed,
}

/// One step of the parser: a complete sequence of `n` bytes, or more bytes needed.
enum Step {
    Took(usize),
    More,
}

fn parse_num(p: &[u8]) -> Option<u32> {
    if p.is_empty() || p.len() > 9 || !p.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(p).ok()?.parse().ok()
}

impl Parser {
    /// `id`: the image id of the graphics query; `tmux`: the query went through tmux, so the
    /// read waits for the graphics reply after DA1.
    pub fn new(id: u32, tmux: bool) -> Parser {
        Parser {
            id,
            tmux,
            pending: Vec::new(),
            got: Probed {
                tmux,
                ..Probed::default()
            },
        }
    }

    /// Whether the read can end: at the DA1 reply, and inside tmux also at the graphics
    /// reply.
    pub fn done(&self) -> bool {
        self.got.da1 && (!self.tmux || self.got.graphics_reply)
    }

    /// What arrived so far.
    pub fn result(&self) -> Probed {
        self.got
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        let mut buf = std::mem::take(&mut self.pending);
        buf.extend_from_slice(bytes);
        let mut at = 0;
        while at < buf.len() {
            if buf[at] != 0x1b {
                self.got.discarded += 1;
                at += 1;
                continue;
            }
            match self.sequence(&buf[at..]) {
                Step::Took(n) => at += n,
                Step::More => break,
            }
        }
        buf.drain(..at);
        if buf.len() > MAX_PENDING {
            self.got.discarded += buf.len();
            buf.clear();
        }
        self.pending = buf;
    }

    /// One sequence starting with `ESC`.
    fn sequence(&mut self, s: &[u8]) -> Step {
        let Some(&kind) = s.get(1) else {
            return Step::More;
        };
        match kind {
            b'[' => self.csi(s),
            b'_' => match string_end(s, 2) {
                Some((body, n)) => {
                    self.apc(&body);
                    Step::Took(n)
                }
                None => Step::More,
            },
            b'P' => match dcs_end(s) {
                Some((body, n)) => {
                    // tmux's passthrough wrapper: its body is the terminal's sequence with
                    // doubled escapes, already undone here.
                    if let Some(inner) = body.strip_prefix(b"tmux;") {
                        let mut nested = Parser::new(self.id, self.tmux);
                        nested.feed(inner);
                        let g = nested.got;
                        self.merge(g);
                    }
                    Step::Took(n)
                }
                None => Step::More,
            },
            b']' => match string_end(s, 2) {
                Some((_, n)) => Step::Took(n),
                None => Step::More,
            },
            // Alt+key and anything else: two bytes, discarded.
            _ => {
                self.got.discarded += 2;
                Step::Took(2)
            }
        }
    }

    fn merge(&mut self, g: Probed) {
        self.got.graphics_reply |= g.graphics_reply;
        self.got.graphics |= g.graphics;
        self.got.sixel |= g.sixel;
        self.got.keyboard |= g.keyboard;
        self.got.da1 |= g.da1;
        if self.got.cell.is_none() {
            self.got.cell = g.cell;
        }
        self.got.discarded += g.discarded;
    }

    fn csi(&mut self, s: &[u8]) -> Step {
        let mut i = 2;
        while i < s.len() {
            match s[i] {
                0x20..=0x3f => i += 1,
                0x40..=0x7e => {
                    self.csi_reply(&s[2..i], s[i]);
                    return Step::Took(i + 1);
                }
                // Not a CSI after all: drop the introducer, keep the rest.
                _ => {
                    self.got.discarded += 2;
                    return Step::Took(2);
                }
            }
        }
        Step::More
    }

    fn csi_reply(&mut self, params: &[u8], fin: u8) {
        match (params.first(), fin) {
            (Some(b'?'), b'c') => {
                self.got.da1 = true;
                if params[1..].split(|&b| b == b';').any(|p| p == b"4") {
                    self.got.sixel = true;
                }
            }
            (Some(b'?'), b'u') => self.got.keyboard = true,
            (_, b't') => {
                let mut it = params.split(|&b| b == b';');
                if it.next() == Some(b"6")
                    && let (Some(h), Some(w)) = (it.next(), it.next())
                    && let (Some(h), Some(w)) = (parse_num(h), parse_num(w))
                    && (1..=1024).contains(&w)
                    && (1..=1024).contains(&h)
                {
                    self.got.cell = Some((w as u16, h as u16));
                } else {
                    self.got.discarded += params.len() + 3;
                }
            }
            // A key such as an arrow or a function key.
            _ => self.got.discarded += params.len() + 3,
        }
    }

    /// `G<key=value,...>;<message>`: the graphics reply for our id.
    fn apc(&mut self, body: &[u8]) {
        let Some(g) = body.strip_prefix(b"G") else {
            self.got.discarded += body.len() + 4;
            return;
        };
        let (keys, msg) = match g.iter().position(|&b| b == b';') {
            Some(k) => (&g[..k], &g[k + 1..]),
            None => (g, &b""[..]),
        };
        let ours = keys.split(|&b| b == b',').any(|kv| {
            kv.strip_prefix(b"i=")
                .and_then(parse_num)
                .is_some_and(|i| i == self.id)
        });
        if ours {
            self.got.graphics_reply = true;
            self.got.graphics |= msg == b"OK";
        }
    }
}

/// The body of an APC or OSC string from byte `from` to its `ESC \` or BEL, and the length
/// of the whole sequence.
fn string_end(s: &[u8], from: usize) -> Option<(Vec<u8>, usize)> {
    let mut i = from;
    while i < s.len() {
        match s[i] {
            0x07 => return Some((s[from..i].to_vec(), i + 1)),
            0x1b if s.get(i + 1) == Some(&b'\\') => return Some((s[from..i].to_vec(), i + 2)),
            0x1b if i + 1 == s.len() => return None,
            _ => i += 1,
        }
    }
    None
}

/// A DCS string: `ESC ESC` inside is one `ESC` (tmux's passthrough doubles them), a single
/// `ESC \` ends it.
fn dcs_end(s: &[u8]) -> Option<(Vec<u8>, usize)> {
    let mut body = Vec::new();
    let mut i = 2;
    while i < s.len() {
        if s[i] == 0x1b {
            match s.get(i + 1) {
                Some(0x1b) => {
                    body.push(0x1b);
                    i += 2;
                }
                Some(b'\\') => return Some((body, i + 2)),
                Some(_) => {
                    body.push(0x1b);
                    i += 1;
                }
                None => return None,
            }
        } else {
            body.push(s[i]);
            i += 1;
        }
    }
    None
}

/// The cell size from the `TIOCGWINSZ` pixel fields, when the terminal fills them.
pub fn winsize_cell(tty: BorrowedFd) -> Option<(u16, u16)> {
    let ws = rustix::termios::tcgetwinsize(tty).ok()?;
    cell_of(ws.ws_col, ws.ws_row, ws.ws_xpixel, ws.ws_ypixel)
}

/// The cell size from a window's size in cells and pixels.
pub fn cell_of(cols: u16, rows: u16, xpixel: u16, ypixel: u16) -> Option<(u16, u16)> {
    if cols == 0 || rows == 0 || xpixel == 0 || ypixel == 0 {
        return None;
    }
    let (w, h) = (xpixel / cols, ypixel / rows);
    ((1..=1024).contains(&w) && (1..=1024).contains(&h)).then_some((w, h))
}

/// Runs the probe (V-6): one write of [`query`] to `out`, then reads `input` until
/// [`Parser::done`] or `deadline`. Nothing reads the terminal after it returns except the
/// input thread.
pub fn run(
    input: BorrowedFd,
    out: BorrowedFd,
    tmux: bool,
    id: u32,
    deadline: Duration,
) -> std::io::Result<Probed> {
    let q = query(tmux, id);
    let mut written = 0;
    // One write; a short write only continues it.
    while written < q.len() {
        match rustix::io::write(out, &q[written..]) {
            Ok(n) => written += n,
            Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(e.into()),
        }
    }
    let start = Instant::now();
    let mut parser = Parser::new(id, tmux);
    let mut buf = [0u8; 4096];
    while !parser.done() {
        let left = deadline.saturating_sub(start.elapsed());
        if left.is_zero() {
            break;
        }
        let ts = rustix::event::Timespec {
            tv_sec: left.as_secs() as _,
            tv_nsec: left.subsec_nanos() as _,
        };
        let mut fds = [rustix::event::PollFd::new(
            &input,
            rustix::event::PollFlags::IN,
        )];
        match rustix::event::poll(&mut fds, Some(&ts)) {
            Ok(0) => break,
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => return Err(e.into()),
        }
        match rustix::io::read(input, &mut buf) {
            Ok(0) => break,
            Ok(n) => parser.feed(&buf[..n]),
            Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => {}
            Err(e) => return Err(e.into()),
        }
    }
    let mut got = parser.result();
    got.elapsed = start.elapsed();
    if got.cell.is_none() {
        got.cell = winsize_cell(input);
    }
    Ok(got)
}

/// The protocol for the session (P3 4.3). `preview.protocol` overrides the probe; without
/// truecolor or with `NO_COLOR` only the card is shown (NFR-TERM); without a cell size only
/// halfblocks are used (P3 4.2).
pub fn choose(p: &Probed, setting: ProtocolSetting, depth: Depth) -> Protocol {
    if setting == ProtocolSetting::Off || depth != Depth::TrueColor {
        return Protocol::Off;
    }
    let kitty = if p.tmux {
        Protocol::KittyTmux
    } else {
        Protocol::Kitty
    };
    let sized = p.cell.is_some();
    match setting {
        ProtocolSetting::Off => Protocol::Off,
        ProtocolSetting::Halfblocks => Protocol::Halfblocks,
        ProtocolSetting::Kitty if sized => kitty,
        ProtocolSetting::Sixel if sized => Protocol::Sixel,
        ProtocolSetting::Kitty | ProtocolSetting::Sixel => Protocol::Halfblocks,
        ProtocolSetting::Auto if p.graphics && sized => kitty,
        ProtocolSetting::Auto if p.sixel && sized => Protocol::Sixel,
        ProtocolSetting::Auto => Protocol::Halfblocks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(tmux: bool, chunks: &[&[u8]]) -> Parser {
        let mut p = Parser::new(7, tmux);
        for c in chunks {
            p.feed(c);
        }
        p
    }

    #[test]
    fn the_query_is_one_sequence_of_four() {
        assert_eq!(
            query(false, 7),
            b"\x1b_Gi=7,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[16t\x1b[?u\x1b[c"
        );
        let t = query(true, 7);
        assert!(t.starts_with(b"\x1bPtmux;\x1b\x1b_Gi=7,"), "{t:?}");
        assert!(
            t.ends_with(b"\x1b\x1b\\\x1b\\\x1b[16t\x1b[?u\x1b[c"),
            "{t:?}"
        );
    }

    #[test]
    fn replies_split_anywhere_still_parse() {
        let all: &[u8] = b"\x1b_Gi=7;OK\x1b\\\x1b[6;20;10t\x1b[?1u\x1b[?62;4;22c";
        for cut in 1..all.len() {
            let p = parse(false, &[&all[..cut], &all[cut..]]);
            let g = p.result();
            assert!(p.done(), "cut {cut}");
            assert!(
                g.graphics && g.sixel && g.keyboard && g.da1,
                "cut {cut}: {g:?}"
            );
            assert_eq!(g.cell, Some((10, 20)), "cut {cut}");
        }
    }

    #[test]
    fn keys_and_garbage_are_discarded() {
        let p = parse(false, &[b"ab\x1b[A\x1bx\x1b]0;t\x07\x1b[?62c"]);
        let g = p.result();
        assert!(g.da1 && !g.graphics && !g.keyboard && !g.sixel);
        assert!(g.discarded >= 6, "{g:?}");
    }

    #[test]
    fn another_id_or_an_error_is_not_kitty() {
        let g = parse(false, &[b"\x1b_Gi=8;OK\x1b\\\x1b[?62c"]).result();
        assert!(!g.graphics && !g.graphics_reply);
        let g = parse(false, &[b"\x1b_Gi=7;ENOTSUPPORTED:x\x1b\\\x1b[?62c"]).result();
        assert!(!g.graphics && g.graphics_reply);
    }

    #[test]
    fn inside_tmux_the_read_waits_for_the_graphics_reply() {
        let mut p = parse(true, &[b"\x1b[?1;2;4c"]);
        assert!(!p.done(), "tmux's own DA1 does not end the read");
        // The reply wrapped in passthrough, with doubled escapes.
        p.feed(b"\x1bPtmux;\x1b\x1b_Gi=7;OK\x1b\x1b\\\x1b\\");
        assert!(p.done());
        assert!(p.result().graphics && p.result().sixel);
        let p = parse(true, &[b"\x1b[?62c\x1b_Gi=7;OK\x1b\\"]);
        assert!(p.done() && p.result().graphics, "a plain reply after DA1");
    }

    #[test]
    fn a_cell_size_out_of_range_is_ignored() {
        let g = parse(false, &[b"\x1b[6;0;10t\x1b[6;20;99999t\x1b[?62c"]).result();
        assert_eq!(g.cell, None);
        assert_eq!(cell_of(80, 24, 800, 480), Some((10, 20)));
        assert_eq!(cell_of(80, 24, 0, 0), None);
    }

    #[test]
    fn the_environment_only_decides_tmux() {
        fn o(s: &str) -> Option<&OsStr> {
            Some(OsStr::new(s))
        }
        assert!(in_tmux(o("tmux-256color"), None, None));
        assert!(in_tmux(o("xterm"), o("tmux"), None));
        assert!(in_tmux(o("xterm"), None, o("/tmp/tmux-1/default,1,0")));
        assert!(!in_tmux(o("xterm-ghostty"), o("ghostty"), None));
        assert!(!in_tmux(o("xterm"), None, o("")));
    }

    #[test]
    fn the_protocol_follows_the_replies_and_the_setting() {
        let tc = Depth::TrueColor;
        let mut p = Probed {
            da1: true,
            ..Probed::default()
        };
        assert_eq!(choose(&p, ProtocolSetting::Auto, tc), Protocol::Halfblocks);
        p.graphics = true;
        assert_eq!(
            choose(&p, ProtocolSetting::Auto, tc),
            Protocol::Halfblocks,
            "no cell size"
        );
        p.cell = Some((10, 20));
        assert_eq!(choose(&p, ProtocolSetting::Auto, tc), Protocol::Kitty);
        p.tmux = true;
        assert_eq!(choose(&p, ProtocolSetting::Auto, tc), Protocol::KittyTmux);
        p.graphics = false;
        p.sixel = true;
        assert_eq!(choose(&p, ProtocolSetting::Auto, tc), Protocol::Sixel);
        assert_eq!(
            choose(&p, ProtocolSetting::Halfblocks, tc),
            Protocol::Halfblocks
        );
        assert_eq!(choose(&p, ProtocolSetting::Off, tc), Protocol::Off);
        assert_eq!(
            choose(&p, ProtocolSetting::Auto, Depth::Ansi),
            Protocol::Off
        );
        assert_eq!(
            choose(&p, ProtocolSetting::Kitty, Depth::NoColor),
            Protocol::Off
        );
        assert_eq!(choose(&p, ProtocolSetting::Kitty, tc), Protocol::KittyTmux);
    }
}
