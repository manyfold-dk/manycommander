//! The image quick view (P3 4, T4): A-QV-1 (the probe from canned replies and on a pty),
//! A-QV-2 (the pipeline: debounce, generations, the cache, V-2's limits, abandonment),
//! A-QV-3 (`TestBackend` snapshots), A-QV-4 (a pty acting as a kitty-capable terminal),
//! A-QV-5 (V-3: `tmux` is never run inside tmux), A-QV-6 for archive members (V-5), and the
//! quick-view half of A-KM-1.

mod common;

use common::tui::*;
use common::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use manycommander::app::App;
use manycommander::app::event::{Effect, Event};
use manycommander::archive::{self, IndexCache};
use manycommander::config::{Config, ProtocolSetting};
use manycommander::fsops::sys::{FsIdentity, Kind, Meta, Ts};
use manycommander::panel::entry::Entry;
use manycommander::panel::listing::{self, Alive, ListingMsg};
use manycommander::preview::card::{Card, Head, text_head};
use manycommander::preview::gfx::{self, Body};
use manycommander::preview::probe::{self, Parser, Probed};
use manycommander::preview::worker::{self, Worker};
use manycommander::preview::{self as pv, Msg, Pane, Protocol, Request, Shown, Subject};
use manycommander::provider::{Caps, PlaceError, Provider, VPath};
use manycommander::theme::Depth;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rustix::fd::AsFd;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(10);
const NONE: KeyModifiers = KeyModifiers::NONE;
const CTRL: KeyModifiers = KeyModifiers::CONTROL;
const ALT: KeyModifiers = KeyModifiers::ALT;

// ---- helpers ------------------------------------------------------------------------------

/// A `w` x `h` image in four quadrants: red, green, blue, white.
fn quadrants(w: u32, h: u32) -> image::DynamicImage {
    image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |x, y| {
        match (x < w / 2, y < h / 2) {
            (true, true) => image::Rgb([255, 0, 0]),
            (false, true) => image::Rgb([0, 255, 0]),
            (true, false) => image::Rgb([0, 0, 255]),
            (false, false) => image::Rgb([255, 255, 255]),
        }
    }))
}

fn encode(img: &image::DynamicImage, f: image::ImageFormat) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, f).unwrap();
    out.into_inner()
}

fn png(w: u32, h: u32) -> Vec<u8> {
    encode(&quadrants(w, h), image::ImageFormat::Png)
}

fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut crc = flate2::Crc::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc.sum().to_be_bytes());
}

/// A PNG whose header declares `w` x `h` RGBA pixels, with a few bytes of data: the header
/// check must reject it before any decode (V-2).
fn png_header(w: u32, h: u32) -> Vec<u8> {
    png_header_depth(w, h, 8)
}

/// An RGBA PNG header with `depth` bits a channel and a token image stream.
fn png_header_depth(w: u32, h: u32, depth: u8) -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[depth, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    z.write_all(&[0u8; 64]).unwrap();
    chunk(&mut out, b"IDAT", &z.finish().unwrap());
    chunk(&mut out, b"IEND", b"");
    out
}

fn pane() -> Pane {
    Pane {
        cols: 40,
        rows: 20,
        cell: Some((10, 20)),
    }
}

fn local(dir: &Path, name: &str, protocol: Protocol, generation: u64) -> Request {
    Request {
        generation,
        subject: Subject::Local {
            dir: dir.to_path_buf(),
            name: OsString::from(name),
        },
        pane: pane(),
        protocol,
    }
}

fn card_of(m: &Msg) -> &Card {
    match m {
        Msg::Card { card, .. } | Msg::Ready { card, .. } => card,
    }
}

fn app(left: &Path, right: &Path, depth: Depth) -> App {
    App::new(
        left.to_path_buf(),
        right.to_path_buf(),
        right.to_path_buf(),
        Config::default(),
        None,
        depth,
        jiff::tz::TimeZone::UTC,
    )
}

/// Performs listing and archive effects synchronously; returns the others.
fn run(a: &mut App, cache: &IndexCache, fx: Vec<Effect>) -> Vec<Effect> {
    let mut rest = Vec::new();
    for e in fx {
        let msgs = std::cell::RefCell::new(Vec::new());
        let send = |m| msgs.borrow_mut().push(m);
        match e {
            Effect::List(req, alive) => {
                listing::guarded(&req, &send, listing::list);
                alive.finish();
            }
            Effect::OpenArchive(req, alive) => {
                archive::open(&req, cache, &send);
                alive.finish();
            }
            Effect::Relist(req, alive) => {
                archive::relist(&req, &send);
                alive.finish();
            }
            other => rest.push(other),
        }
        for m in msgs.into_inner() {
            let more = a.update(Event::Listing(m));
            rest.extend(run(a, cache, more));
        }
    }
    a.panel_mut().ensure_sorted();
    rest
}

fn press(a: &mut App, code: KeyCode, m: KeyModifiers) -> Vec<Effect> {
    a.update(Event::Key(KeyEvent::new(code, m), Instant::now()))
}

fn previews(fx: &[Effect]) -> Vec<Request> {
    fx.iter()
        .filter_map(|e| match e {
            Effect::Preview(r) => Some(r.clone()),
            _ => None,
        })
        .collect()
}

fn render(a: &mut App, w: u16, h: u16) -> (String, ratatui::buffer::Buffer) {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| manycommander::ui::draw(a, f)).unwrap();
    let buf = term.backend().buffer().clone();
    let mut out = String::new();
    for y in 0..h {
        for x in 0..w {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    // The runtime syncs after each frame: the pane may have changed.
    a.quick_sync();
    (out, buf)
}

fn meta(kind: Kind, perm: u32, size: u64, mtime: i64) -> Meta {
    let ts = Ts {
        sec: mtime,
        nsec: 0,
    };
    Meta {
        kind,
        perm,
        uid: 1000,
        nlink: 1,
        size,
        blocks: size.div_ceil(512),
        id: FsIdentity::default(),
        atime: ts,
        mtime: ts,
        ctime: ts,
        mount_root: false,
        automount: false,
    }
}

/// A listing that no directory holds, for snapshots with fixed paths.
fn synthetic(a: &mut App, side: usize, entries: &[(&[u8], Kind, u32, u64)]) {
    let slot = a.sides[side].panel().slot;
    let dir = a.sides[side].panel().dir.clone();
    let req = a.sides[side]
        .panel_mut()
        .navigate(dir.clone(), None, Alive::running());
    let mut names = Vec::new();
    let mut es = Vec::new();
    for (i, (n, k, p, s)) in entries.iter().enumerate() {
        es.push(Entry::new(
            &mut names,
            n,
            &meta(*k, *p, *s, 1_790_000_000 + i as i64 * 86_400),
        ));
    }
    a.update(Event::Listing(ListingMsg::Batch {
        slot,
        generation: req.generation,
        entries: es,
        names,
    }));
    a.update(Event::Listing(ListingMsg::Done {
        slot,
        generation: req.generation,
        dir,
        elapsed: Duration::ZERO,
    }));
    a.sides[side].panel_mut().ensure_sorted();
}

// ---- A-QV-1: the probe ----------------------------------------------------------------------

/// Canned replies, each with the protocol of P3 4.3 it picks. Environment variables alone
/// never enable a protocol: only the replies do.
#[test]
fn a_qv_1_canned_replies_pick_the_protocol() {
    let tc = Depth::TrueColor;
    /// `(case, inside tmux, reply chunks, protocol, keyboard protocol)`.
    type Case<'a> = (&'a str, bool, &'a [&'a [u8]], Protocol, bool);
    let cases: &[Case] = &[
        (
            "kitty OK, a cell size and DA1",
            false,
            &[b"\x1b_Gi=5;OK\x1b\\\x1b[6;20;10t\x1b[?1u\x1b[?62;22c"],
            Protocol::Kitty,
            true,
        ),
        (
            "foot: DA1 with 4 and a cell size",
            false,
            &[b"\x1b[6;17;8t\x1b[?62;4;22c"],
            Protocol::Sixel,
            false,
        ),
        (
            "DA1 only",
            false,
            &[b"\x1b[?62;22c"],
            Protocol::Halfblocks,
            false,
        ),
        (
            "the keyboard protocol without graphics",
            false,
            &[b"\x1b[?15u\x1b[?62;22c"],
            Protocol::Halfblocks,
            true,
        ),
        (
            "tmux: its DA1 first, the wrapped graphics reply after",
            true,
            &[
                b"\x1b[6;20;10t\x1b[?62;22c",
                b"\x1bPtmux;\x1b\x1b_Gi=5;OK\x1b\x1b\\\x1b\\",
            ],
            Protocol::KittyTmux,
            false,
        ),
        (
            "tmux: a plain graphics reply after its DA1",
            true,
            &[b"\x1b[6;20;10t\x1b[?62;4c", b"\x1b_Gi=5;OK\x1b\\"],
            Protocol::KittyTmux,
            false,
        ),
        (
            "garbage and keys interleaved",
            false,
            &[
                b"ls\r\x1b[A",
                b"\x1b_Gi=5;O",
                b"K\x1b\\q\x1b[6;20",
                b";10t\x1bx\x1b[?62c",
            ],
            Protocol::Kitty,
            false,
        ),
    ];
    for (name, tmux, chunks, want, keyboard) in cases {
        let mut p = Parser::new(5, *tmux);
        for c in *chunks {
            assert!(!p.done() || !*tmux, "{name}: done too early");
            p.feed(c);
        }
        assert!(p.done(), "{name}: the read ends at the last reply");
        let got = p.result();
        assert_eq!(
            probe::choose(&got, ProtocolSetting::Auto, tc),
            *want,
            "{name}: {got:?}"
        );
        assert_eq!(got.keyboard, *keyboard, "{name}");
    }
    // Keys typed during the probe are discarded, not replayed.
    let mut p = Parser::new(5, false);
    p.feed(b"abc\x1b[?62c");
    assert!(p.result().discarded >= 3);
    // Environment alone: tmux, a cell size from TIOCGWINSZ, no reply.
    let env_only = Probed {
        tmux: true,
        cell: Some((10, 20)),
        ..Probed::default()
    };
    assert_eq!(
        probe::choose(&env_only, ProtocolSetting::Auto, tc),
        Protocol::Halfblocks
    );
    // The configuration overrides the probe (P3 4.3).
    let kitty = Parser::new(5, false);
    let mut k = kitty;
    k.feed(b"\x1b_Gi=5;OK\x1b\\\x1b[6;20;10t\x1b[?62;4c");
    let got = k.result();
    for (setting, want) in [
        (ProtocolSetting::Sixel, Protocol::Sixel),
        (ProtocolSetting::Halfblocks, Protocol::Halfblocks),
        (ProtocolSetting::Off, Protocol::Off),
    ] {
        assert_eq!(probe::choose(&got, setting, tc), want);
    }
    // NO_COLOR or no truecolor: the card only (NFR-TERM).
    assert_eq!(
        probe::choose(&got, ProtocolSetting::Auto, Depth::NoColor),
        Protocol::Off
    );
}

/// V-6 without a terminal: the query goes out in one piece; without a reply the probe
/// returns at its deadline; with replies waiting it returns at once.
#[test]
fn a_qv_1_the_probe_returns_at_the_deadline() {
    let (in_r, in_w) = rustix::pipe::pipe().unwrap();
    let (out_r, out_w) = rustix::pipe::pipe().unwrap();
    let t = Instant::now();
    let got = probe::run(in_r.as_fd(), out_w.as_fd(), false, 9, probe::DEADLINE).unwrap();
    let took = t.elapsed();
    assert!(
        took >= Duration::from_millis(95) && took < Duration::from_millis(400),
        "{took:?}"
    );
    assert!(!got.da1 && !got.graphics);
    let mut q = vec![0u8; 256];
    let n = rustix::io::read(&out_r, &mut q).unwrap();
    assert_eq!(&q[..n], &probe::query(false, 9)[..]);
    // Replies already waiting: no wait at all.
    rustix::io::write(&in_w, b"\x1b_Gi=9;OK\x1b\\\x1b[?62;22c").unwrap();
    let t = Instant::now();
    let got = probe::run(in_r.as_fd(), out_w.as_fd(), false, 9, probe::DEADLINE).unwrap();
    assert!(got.graphics && got.da1);
    assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
}

fn log_line<'a>(log: &'a str, marker: &str) -> Option<&'a str> {
    log.lines().find(|l| l.contains(marker))
}

fn field(line: &str, key: &str) -> Option<String> {
    let k = format!("{key}=");
    let v = line.split(&k).nth(1)?;
    Some(v.split_whitespace().next()?.trim_matches('"').to_string())
}

/// A pty run (A-QV-1): the probe is one contiguous query; a terminal that answers nothing
/// costs the 100 ms deadline once; keys typed during the probe are discarded, and keys after
/// it reach the app. A terminal that names itself kitty in the environment but answers only
/// DA1 gets halfblocks.
#[test]
fn a_qv_1_a_silent_terminal_on_a_pty() {
    let h = test_dir("qv1-silent");
    let log = h.join("silent.log");
    let mut t = Tui::spawn_term(
        &["--log", log.to_str().unwrap()],
        &h.path,
        &[("COLORTERM", "truecolor")],
        100,
        30,
        false,
        Term {
            silent: true,
            ..Term::default()
        },
    );
    // Keys typed while the probe reads.
    t.send(b"zqx");
    assert!(t.wait_for("10Quit", T), "{}", t.screen());
    std::thread::sleep(Duration::from_millis(200));
    t.pump();
    assert!(!t.screen().contains("zqx"), "{}", t.screen());
    let q = b"\x1b[16t\x1b[?u\x1b[c";
    let n = t.raw.windows(q.len()).filter(|w| w == q).count();
    assert_eq!(n, 1, "the probe's queries go out once");
    let at = t.raw.windows(q.len()).position(|w| w == q).unwrap();
    let head = &t.raw[..at];
    assert!(
        String::from_utf8_lossy(head).ends_with(",s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\"),
        "the graphics query comes right before, in the same write"
    );
    // After the deadline, keys reach the app.
    t.send(F10);
    assert_eq!(t.wait_exit(T), Some(0));
    let text = std::fs::read_to_string(&log).unwrap();
    let line = log_line(&text, "terminal probe").expect("the probe is logged");
    let us: u64 = field(line, "probe_us").unwrap().parse().unwrap();
    assert!((95_000..400_000).contains(&us), "{line}");
    assert_eq!(
        field(line, "protocol").as_deref(),
        Some("halfblocks"),
        "{line}"
    );
    let discarded: usize = field(line, "discarded").unwrap().parse().unwrap();
    assert!(discarded >= 3, "{line}");

    // The environment says kitty; the terminal answers DA1 only.
    let log2 = h.join("envonly.log");
    let mut t = Tui::spawn_term(
        &["--log", log2.to_str().unwrap()],
        &h.path,
        &[
            ("COLORTERM", "truecolor"),
            ("TERM", "xterm-kitty"),
            ("TERM_PROGRAM", "ghostty"),
            ("KITTY_WINDOW_ID", "1"),
        ],
        100,
        30,
        false,
        Term::default(),
    );
    assert!(t.wait_for("10Quit", T), "{}", t.screen());
    t.send(F10);
    assert_eq!(t.wait_exit(T), Some(0));
    let text = std::fs::read_to_string(&log2).unwrap();
    let line = log_line(&text, "terminal probe").unwrap();
    assert_eq!(
        field(line, "protocol").as_deref(),
        Some("halfblocks"),
        "{line}"
    );
    let us: u64 = field(line, "probe_us").unwrap().parse().unwrap();
    assert!(us < 90_000, "an answered probe ends at DA1: {line}");
}

// ---- A-QV-2: the pipeline --------------------------------------------------------------------

/// A burst of 30 moves 20 ms apart requests nothing; the rest after it requests once.
#[test]
fn a_qv_2_a_burst_of_moves_requests_once() {
    let d = test_dir("qv2-burst");
    for i in 0..40 {
        write(&d.join(format!("f{i:02}.txt")), b"x");
    }
    let cache = IndexCache::default();
    let mut a = app(&d.path, &d.path, Depth::TrueColor);
    a.set_graphics(Protocol::Halfblocks, None);
    let fx = a.start();
    run(&mut a, &cache, fx);
    assert!(previews(&press(&mut a, KeyCode::Char('q'), CTRL)).is_empty());
    assert!(a.quick.on);
    render(&mut a, 100, 30);
    press(&mut a, KeyCode::Down, NONE);
    let mut sent = 0;
    for _ in 0..30 {
        sent += previews(&press(&mut a, KeyCode::Down, NONE)).len();
        std::thread::sleep(Duration::from_millis(20));
        sent += previews(&a.update(Event::Tick)).len();
        render(&mut a, 100, 30);
    }
    assert_eq!(sent, 0, "no request during the burst");
    let due = a.quick_due().expect("a deadline is pending");
    assert!(due > Instant::now() - Duration::from_millis(1));
    std::thread::sleep(Duration::from_millis(120));
    let r = previews(&a.update(Event::Tick));
    assert_eq!(r.len(), 1, "one request after the burst");
    assert_eq!(r[0].generation, a.quick.generation);
    assert!(a.quick_due().is_none(), "no deadline once requested (P-5)");
    assert!(previews(&a.update(Event::Tick)).is_empty());
    // Ctrl+Q again: off, and answers for it are dropped.
    let g = a.quick.generation;
    press(&mut a, KeyCode::Char('q'), CTRL);
    assert!(!a.quick.on && a.quick.generation > g);
}

/// Stale generations are dropped; the current one is kept.
#[test]
fn a_qv_2_stale_generations_are_dropped() {
    let d = test_dir("qv2-stale");
    write(&d.join("a.txt"), b"a");
    write(&d.join("b.txt"), b"b");
    let cache = IndexCache::default();
    let mut a = app(&d.path, &d.path, Depth::TrueColor);
    let fx = a.start();
    run(&mut a, &cache, fx);
    press(&mut a, KeyCode::Char('q'), CTRL);
    press(&mut a, KeyCode::Down, NONE);
    render(&mut a, 100, 30);
    let old = a.quick.generation;
    press(&mut a, KeyCode::Down, NONE);
    let card = |n: &str| Card {
        name: n.as_bytes().to_vec(),
        kind: "regular file",
        ..Card::default()
    };
    a.update(Event::Preview(Msg::Card {
        generation: old,
        card: card("stale"),
    }));
    assert!(a.quick.shown.is_none(), "a stale answer is dropped");
    a.update(Event::Preview(Msg::Card {
        generation: a.quick.generation,
        card: card("current"),
    }));
    assert!(matches!(&a.quick.shown, Some(Shown::Card(c)) if c.name == b"current"));
}

fn recv_gen(rx: &Receiver<Msg>, generation: u64) -> Msg {
    let end = Instant::now() + T;
    while Instant::now() < end {
        if let Ok(m) = rx.recv_timeout(Duration::from_millis(100))
            && m.generation() == generation
        {
            return m;
        }
    }
    panic!("no answer for generation {generation}");
}

/// A cache hit decodes nothing and keeps the image id, so a stored kitty image is placed
/// again without a transmit.
#[test]
fn a_qv_2_a_cache_hit_decodes_nothing() {
    let d = test_dir("qv2-cache");
    write(&d.join("a.png"), &png(64, 48));
    let (tx, rx) = channel();
    let w = Worker::spawn(move |m| {
        let _ = tx.send(m);
    })
    .unwrap();
    w.submit(local(&d.path, "a.png", Protocol::Kitty, 1));
    let Msg::Ready {
        image: first, card, ..
    } = recv_gen(&rx, 1)
    else {
        panic!("an image")
    };
    assert_eq!(card.pixels, Some((64, 48)));
    assert!(matches!(first.body, Body::Kitty { .. }));
    assert_eq!(w.stats().decodes.load(Ordering::Relaxed), 1);
    w.submit(local(&d.path, "a.png", Protocol::Kitty, 2));
    let Msg::Ready { image: again, .. } = recv_gen(&rx, 2) else {
        panic!("an image")
    };
    assert_eq!(again.id, first.id, "the same prepared image");
    assert_eq!(
        w.stats().decodes.load(Ordering::Relaxed),
        1,
        "no second decode"
    );
    assert_eq!(w.stats().hits.load(Ordering::Relaxed), 1);
    // Another protocol or pane is another key.
    w.submit(local(&d.path, "a.png", Protocol::Halfblocks, 3));
    assert!(matches!(recv_gen(&rx, 3), Msg::Ready { .. }));
    assert_eq!(w.stats().decodes.load(Ordering::Relaxed), 2);
    // A changed file (size, mtime) is another key.
    write(&d.join("a.png"), &png(32, 24));
    w.submit(local(&d.path, "a.png", Protocol::Kitty, 4));
    let Msg::Ready { card, .. } = recv_gen(&rx, 4) else {
        panic!()
    };
    assert_eq!(card.pixels, Some((32, 24)));
}

/// V-2 and P3 4.6: each limit gives the card with its reason, before any decode where the
/// header says so; nothing panics; a FIFO and a symlink are never opened.
#[test]
fn a_qv_2_limits_give_the_card_with_its_reason() {
    let d = test_dir("qv2-limits");
    write(&d.join("huge.png"), &png_header(20000, 20000));
    write(&d.join("gib.png"), &png_header(16384, 16384));
    // 200 MB as RGBA8 (within the limit), 400 MB in the decoder's own 16-bit buffer.
    write(&d.join("deep.png"), &png_header_depth(10000, 5000, 16));
    let jpg = encode(&quadrants(300, 200), image::ImageFormat::Jpeg);
    write(&d.join("cut.jpg"), &jpg[..jpg.len() / 3]);
    write(&d.join("ok.png"), &png(20, 10));
    std::os::unix::fs::symlink("ok.png", d.join("link.png")).unwrap();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        d.join("x.png"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o644),
        0,
    )
    .unwrap();
    // 100 MB: a PNG header, then a hole.
    let big = std::fs::File::create(d.join("big.png")).unwrap();
    (&big).write_all(&png(8, 8)).unwrap();
    big.set_len(100 << 20).unwrap();
    write(&d.join("text.txt"), b"one\ttwo\n\x1b[1mbold\x07\n");
    write(&d.join("bin.dat"), b"\x7fELF\0\0\0\x01");

    let case = |name: &str| worker::process_once(&local(&d.path, name, Protocol::Kitty, 7));
    let (m, s) = case("huge.png");
    let c = card_of(m.as_ref().unwrap());
    assert_eq!(c.reason.as_deref(), Some(pv::TOO_WIDE));
    assert_eq!(c.pixels, Some((20000, 20000)));
    assert_eq!(s.decodes.load(Ordering::Relaxed), 0);
    let (m, s) = case("gib.png");
    assert_eq!(
        card_of(m.as_ref().unwrap()).reason.as_deref(),
        Some(pv::TOO_MUCH_MEMORY)
    );
    assert_eq!(
        s.decodes.load(Ordering::Relaxed),
        0,
        "rejected before any decode"
    );
    let (m, s) = case("deep.png");
    assert_eq!(
        card_of(m.as_ref().unwrap()).reason.as_deref(),
        Some(pv::TOO_MUCH_MEMORY),
        "the decoder's 16-bit buffer counts"
    );
    assert_eq!(s.decodes.load(Ordering::Relaxed), 0);
    let (m, s) = case("cut.jpg");
    let c = card_of(m.as_ref().unwrap());
    assert!(matches!(m, Some(Msg::Card { .. })), "{c:?}");
    assert_eq!(c.reason.as_deref(), Some(worker::TRUNCATED), "{c:?}");
    assert_eq!(s.decodes.load(Ordering::Relaxed), 0);
    // The whole JPEG, and one with data after its end (a motion photo), are complete.
    assert!(worker::jpeg_complete(&jpg));
    let mut motion = jpg.clone();
    motion.extend_from_slice(b"\0\0\0\x18ftypmp42 trailing video");
    assert!(worker::jpeg_complete(&motion));
    for cut in [2, 20, jpg.len() - 2, jpg.len() - 1] {
        assert!(!worker::jpeg_complete(&jpg[..cut]), "cut at {cut}");
    }
    let (m, s) = case("x.png");
    let c = card_of(m.as_ref().unwrap());
    assert_eq!(
        (c.kind, c.reason.as_deref()),
        ("FIFO", Some(worker::SPECIAL))
    );
    assert_eq!(s.opens.load(Ordering::Relaxed), 0, "a FIFO is never opened");
    let (m, s) = case("big.png");
    let c = card_of(m.as_ref().unwrap());
    assert_eq!(c.reason.as_deref(), Some(pv::TOO_LARGE));
    assert_eq!(c.pixels, Some((8, 8)), "the header still names the size");
    assert_eq!(s.decodes.load(Ordering::Relaxed), 0);
    let (m, s) = case("link.png");
    let c = card_of(m.as_ref().unwrap());
    assert_eq!(c.kind, "symbolic link");
    assert_eq!(c.target.as_deref(), Some(&b"ok.png"[..]));
    assert_eq!(
        s.opens.load(Ordering::Relaxed),
        0,
        "a symlink is never followed"
    );
    let (m, _) = case("text.txt");
    let c = card_of(m.as_ref().unwrap());
    assert_eq!(
        c.head,
        Head::Text(vec![b"one     two".to_vec(), b"\x1b[1mbold\x07".to_vec()])
    );
    let (m, _) = case("bin.dat");
    assert_eq!(card_of(m.as_ref().unwrap()).head, Head::Binary);
    let (m, s) = case("ok.png");
    assert!(matches!(m, Some(Msg::Ready { .. })));
    assert_eq!(s.decodes.load(Ordering::Relaxed), 1);
    let (m, _) = case("gone.png");
    assert!(card_of(m.as_ref().unwrap()).reason.is_some());
}

/// Runs the calling test's body alone in a new process of this test binary, so the peak
/// memory it measures is its own and not a parallel test's. In the parent it runs exactly
/// `test_name` there, asserts that it passed, and returns `false`: the caller returns. In
/// the child it returns `true`: the caller runs the body.
fn alone(test_name: &str) -> bool {
    if std::env::var_os("MC_ALONE").is_some() {
        return true;
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env("MC_ALONE", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("test result: ok. 1 passed"),
        "{test_name} alone:\n{stdout}\n{stderr}"
    );
    false
}

/// What `f` returns, and how much it raised this process's peak resident memory (`VmHWM`)
/// and its peak virtual size (`VmPeak`), in bytes. The resident peak is first reset to the
/// current size where the kernel allows it (`clear_refs`).
fn peak_growth<T>(f: impl FnOnce() -> T) -> (T, u64, u64) {
    let status = |key: &str| {
        let s = std::fs::read_to_string("/proc/self/status").unwrap();
        let kb = s.lines().find_map(|l| l.strip_prefix(key)).unwrap();
        kb.trim()
            .trim_end_matches("kB")
            .trim()
            .parse::<u64>()
            .unwrap()
            << 10
    };
    let _ = std::fs::write("/proc/self/clear_refs", "5");
    let (hwm, peak) = (status("VmHWM:"), status("VmPeak:"));
    let r = f();
    let grew = |key: &str, before: u64| status(key).saturating_sub(before);
    (r, grew("VmHWM:", hwm), grew("VmPeak:", peak))
}

/// A 1 x 1 RGBA PNG whose `iCCP` profile inflates to `mib` MiB of zeros. The zlib stream
/// repeats one fully flushed deflate segment of 1 MiB of zeros, so nothing that large is
/// compressed here; the file is about a thousandth of the profile.
fn iccp_png(mib: usize) -> Vec<u8> {
    use flate2::{Compress, Compression, FlushCompress};
    let mut c = Compress::new(Compression::best(), false);
    let mut seg = Vec::with_capacity(64 << 10);
    c.compress_vec(&vec![0u8; 1 << 20], &mut seg, FlushCompress::Full)
        .unwrap();
    assert_eq!(c.total_in(), 1 << 20);
    let mut end = Vec::with_capacity(64);
    c.compress_vec(&[], &mut end, FlushCompress::Finish)
        .unwrap();
    // A name, its NUL, compression method 0, then the zlib stream; the Adler-32 of n zeros
    // is (n mod 65521) << 16 | 1.
    let mut profile = b"bomb\0\0\x78\xda".to_vec();
    for _ in 0..mib {
        profile.extend_from_slice(&seg);
    }
    profile.extend_from_slice(&end);
    let adler = ((((mib as u64) << 20) % 65521) << 16 | 1) as u32;
    profile.extend_from_slice(&adler.to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
    chunk(&mut out, b"iCCP", &profile);
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    z.write_all(&[0, 255, 0, 0, 255]).unwrap();
    chunk(&mut out, b"IDAT", &z.finish().unwrap());
    chunk(&mut out, b"IEND", b"");
    out
}

/// A 1 x 1 PNG whose `iCCP` profile inflates to 280 MiB of zeros, in a file of about 280 KB
/// (review finding B2). The png crate inflated the profile whole while it read the header,
/// before V-2's checks. The preview has no use for a profile, so the crate now skips it: the
/// image is shown, at once and in bounded memory. A PNG with an ordinary profile still shows.
#[test]
fn a_qv_2_a_colour_profile_is_never_inflated() {
    if !alone("a_qv_2_a_colour_profile_is_never_inflated") {
        return;
    }
    let d = test_dir("qv2-iccp");
    let bomb = iccp_png(280);
    assert!(bomb.len() < 1 << 20, "{}", bomb.len());
    write(&d.join("bomb.png"), &bomb);
    let started = Instant::now();
    let ((m, s), grew, _) =
        peak_growth(|| worker::process_once(&local(&d.path, "bomb.png", Protocol::Kitty, 1)));
    let took = started.elapsed();
    eprintln!("iCCP bomb: {took:?}, peak memory +{} KiB", grew >> 10);
    let c = card_of(m.as_ref().unwrap());
    assert!(matches!(m, Some(Msg::Ready { .. })), "{c:?}");
    assert_eq!((c.pixels, c.reason.as_deref()), (Some((1, 1)), None));
    assert_eq!(s.decodes.load(Ordering::Relaxed), 1);
    assert!(grew < 16 << 20, "peak memory grew by {} MiB", grew >> 20);
    assert!(took < T, "{took:?}");

    // An ordinary profile after IHDR (a PNG of 20 x 10).
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    z.write_all(&noise(3000, 5)).unwrap();
    let mut icc = b"sRGB\0\0".to_vec();
    icc.extend_from_slice(&z.finish().unwrap());
    let plain = png(20, 10);
    let mut with = plain[..33].to_vec();
    chunk(&mut with, b"iCCP", &icc);
    with.extend_from_slice(&plain[33..]);
    write(&d.join("icc.png"), &with);
    let (m, s) = worker::process_once(&local(&d.path, "icc.png", Protocol::Kitty, 2));
    let c = card_of(m.as_ref().unwrap());
    assert!(matches!(m, Some(Msg::Ready { .. })), "{c:?}");
    assert_eq!(c.pixels, Some((20, 10)));
    assert_eq!(s.decodes.load(Ordering::Relaxed), 1);
}

/// A WebP whose EXIF chunk declares 2 GiB in a file of a few hundred bytes (review finding
/// B2). image-webp reads a chunk into a buffer of its declared size, and `image` passes it
/// no limit, so reading the orientation reserved 2 GiB. The chunk is not read now: the
/// image is shown upright, and the peak virtual size stays bounded.
#[test]
fn a_qv_2_a_webp_chunk_past_the_end_is_not_read() {
    if !alone("a_qv_2_a_webp_chunk_past_the_end_is_not_read") {
        return;
    }
    let simple = encode(&quadrants(4, 4), image::ImageFormat::WebP);
    assert_eq!(&simple[12..16], b"VP8L");
    let mut body = b"WEBP".to_vec();
    // VP8X: the EXIF flag, and a canvas of 4 x 4 (each side minus one, in 24 bits).
    body.extend_from_slice(b"VP8X");
    body.extend_from_slice(&10u32.to_le_bytes());
    body.extend_from_slice(&[0x08, 0, 0, 0, 3, 0, 0, 3, 0, 0]);
    body.extend_from_slice(&simple[12..]);
    body.extend_from_slice(b"EXIF");
    body.extend_from_slice(&(2u32 << 30).to_le_bytes());
    body.extend_from_slice(b"II*\0");
    let mut webp = b"RIFF".to_vec();
    webp.extend_from_slice(&(body.len() as u32).to_le_bytes());
    webp.extend_from_slice(&body);
    let d = test_dir("qv2-webp-exif");
    write(&d.join("exif.webp"), &webp);
    let ((m, s), _, virt) =
        peak_growth(|| worker::process_once(&local(&d.path, "exif.webp", Protocol::Kitty, 1)));
    eprintln!("WebP EXIF of 2 GiB: peak virtual size +{} MiB", virt >> 20);
    let c = card_of(m.as_ref().unwrap());
    assert!(matches!(m, Some(Msg::Ready { .. })), "{c:?}");
    assert_eq!(c.pixels, Some((4, 4)));
    assert_eq!(s.decodes.load(Ordering::Relaxed), 1);
    assert!(
        virt < 512 << 20,
        "peak virtual size grew by {} MiB",
        virt >> 20
    );
}

/// A provider whose `open_read` blocks until released, as a FUSE file that never answers.
struct Stuck {
    release: Mutex<Receiver<()>>,
    reads: AtomicU64,
}

impl Provider for Stuck {
    fn caps(&self) -> Caps {
        Caps::default()
    }
    fn list(
        &self,
        _: &VPath,
        _: &mut dyn FnMut(ListingMsg),
        _: &AtomicBool,
    ) -> Result<(), PlaceError> {
        Ok(())
    }
    fn lstat(&self, _: &VPath) -> Result<Meta, PlaceError> {
        Err(PlaceError::NotFound)
    }
    fn open_read(
        &self,
        _: &VPath,
        _: &Arc<AtomicBool>,
    ) -> Result<Box<dyn Read + Send>, PlaceError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let _ = self.release.lock().unwrap().recv();
        Err(PlaceError::NotFound)
    }
}

/// P3 2.5: a read that blocks is abandoned once a newer request has waited 1 s; a new
/// thread serves it; the abandoned thread counts toward `MAX_ABANDONED` until its read
/// returns; at the cap the view says previews are blocked and loads are refused.
#[test]
fn a_qv_2_a_blocked_read_is_abandoned_after_a_second() {
    let d = test_dir("qv2-stuck");
    write(&d.join("ok.png"), &png(20, 10));
    let (release, gate) = channel();
    let stuck = Arc::new(Stuck {
        release: Mutex::new(gate),
        reads: AtomicU64::new(0),
    });
    let (tx, rx) = channel();
    let mut w = Worker::spawn(move |m| {
        let _ = tx.send(m);
    })
    .unwrap();
    let place: Arc<dyn Provider> = stuck.clone();
    w.submit(Request {
        generation: 1,
        subject: Subject::Place {
            place,
            place_id: 99,
            path: VPath::parse(b"fuse/file.png").unwrap(),
            name: b"file.png".to_vec(),
            size: 10,
            mtime: 0,
            perm: 0o644,
        },
        pane: pane(),
        protocol: Protocol::Kitty,
    });
    let end = Instant::now() + T;
    while stuck.reads.load(Ordering::SeqCst) == 0 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(w.abandon_due(), None, "nothing waits yet");
    let submitted = Instant::now();
    w.submit(local(&d.path, "ok.png", Protocol::Kitty, 2));
    let due = w
        .abandon_due()
        .expect("a request waits behind a stuck thread");
    assert!(due >= submitted + pv::ABANDON_AFTER - Duration::from_millis(5));
    assert!(due <= Instant::now() + pv::ABANDON_AFTER);
    std::thread::sleep(due.saturating_duration_since(Instant::now()));
    let (old, blocked) = w.replace().unwrap();
    assert!(old.is_running(), "the abandoned thread is still blocked");
    assert_eq!(blocked, PathBuf::from("/fuse/file.png"));
    assert!(
        matches!(recv_gen(&rx, 2), Msg::Ready { .. }),
        "a new thread serves it"
    );
    assert!(w.alive().is_running());

    // The app counts it; at the cap previews are blocked and loads refused.
    std::fs::create_dir_all(d.join("sub")).unwrap();
    let cache = IndexCache::default();
    let mut a = app(&d.path, &d.path, Depth::TrueColor);
    let fx = a.start();
    run(&mut a, &cache, fx);
    a.preview_abandoned(blocked.clone(), old.clone());
    assert_eq!(a.abandoned_threads(), 1);
    let others: Vec<Alive> = (0..3).map(|_| Alive::running()).collect();
    for o in &others {
        a.preview_abandoned(PathBuf::from("/other"), o.clone());
    }
    assert!(a.at_abandon_cap());
    a.panel_mut().cursor_to_name(b"sub");
    let fx = press(&mut a, KeyCode::Enter, NONE);
    assert!(fx.is_empty(), "{fx:?}");
    assert_eq!(
        a.status.as_ref().map(|s| s.text.as_str()),
        Some(manycommander::app::TOO_MANY_BLOCKED)
    );
    press(&mut a, KeyCode::Char('q'), CTRL);
    press(&mut a, KeyCode::Up, NONE);
    a.preview_blocked();
    assert_eq!(a.quick_card().reason.as_deref(), Some(pv::BLOCKED));
    // The read returns: the old thread exits, its late answer is dropped by generation.
    release.send(()).unwrap();
    let end = Instant::now() + T;
    while old.is_running() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!old.is_running());
    for o in &others {
        o.finish();
    }
    assert_eq!(a.abandoned_threads(), 0);
}

/// P3 4.4, step 4: the EXIF orientation is applied after the scale, and an animated GIF
/// shows its first frame.
#[test]
fn a_qv_2_orientation_and_the_first_frame() {
    let d = test_dir("qv2-frames");
    // A two-frame GIF: red, then blue.
    let mut gif = Vec::new();
    {
        let mut e = image::codecs::gif::GifEncoder::new(&mut gif);
        for c in [[255u8, 0, 0, 255], [0, 0, 255, 255]] {
            let f = image::Frame::new(image::RgbaImage::from_pixel(8, 8, image::Rgba(c)));
            e.encode_frame(f).unwrap();
        }
    }
    write(&d.join("anim.gif"), &gif);
    let (m, _) = worker::process_once(&local(&d.path, "anim.gif", Protocol::Halfblocks, 1));
    let Some(Msg::Ready { image, .. }) = m else {
        panic!("{m:?}")
    };
    let Body::Halfblocks { cells } = &image.body else {
        panic!()
    };
    assert_eq!(cells[0].top, Some([255, 0, 0]), "the first frame");
    // Orientation: a wide image rotated a quarter turn becomes tall.
    let img = quadrants(40, 20);
    let p = gfx::prepare_oriented(
        &img,
        image::metadata::Orientation::Rotate90,
        pane(),
        Protocol::Kitty,
    )
    .unwrap();
    assert_eq!(p.px, (20, 40));
}

// ---- A-QV-3: snapshots ---------------------------------------------------------------------

fn snapshot_app() -> App {
    let mut a = App::new(
        PathBuf::from("/snap/pictures"),
        PathBuf::from("/snap/other"),
        PathBuf::from("/snap"),
        Config::default(),
        None,
        Depth::TrueColor,
        jiff::tz::TimeZone::UTC,
    );
    a.set_graphics(Protocol::Halfblocks, None);
    synthetic(
        &mut a,
        0,
        &[
            (b"quad.png", Kind::File, 0o644, 1234),
            (b"new\nline\x1b.txt", Kind::File, 0o600, 42),
            (b"notes.txt", Kind::File, 0o644, 80),
            (b"blob.bin", Kind::File, 0o644, 9),
        ],
    );
    synthetic(&mut a, 1, &[(b"kept", Kind::Dir, 0o755, 0)]);
    a
}

/// A-QV-3: halfblocks of a known image; the colours come from the image.
#[test]
fn a_qv_3_halfblocks_of_a_known_image() {
    let mut a = snapshot_app();
    a.panel_mut().cursor_to_name(b"quad.png");
    press(&mut a, KeyCode::Char('q'), CTRL);
    render(&mut a, 80, 24);
    let (cols, rows) = a.quick.pane.unwrap();
    let p = gfx::prepare(
        &quadrants(16, 16),
        Pane {
            cols,
            rows,
            cell: None,
        },
        Protocol::Halfblocks,
    )
    .unwrap();
    let card = Card {
        name: b"quad.png".to_vec(),
        kind: "regular file",
        pixels: Some((16, 16)),
        ..Card::default()
    };
    a.update(Event::Preview(Msg::Ready {
        generation: a.quick.generation,
        image: Arc::new(p),
        card,
    }));
    let (text, buf) = render(&mut a, 80, 24);
    insta::assert_snapshot!("quick_halfblocks", text);
    // The image sits centred at the top of the right pane: 16 columns, 8 rows.
    let (_, r) = a.quick.drawn.clone().expect("drawn");
    assert_eq!((r.width, r.height), (16, 8));
    assert!(r.x > 40, "on the inactive (right) side: {r:?}");
    let (x0, y0) = (r.x, r.y);
    use ratatui::style::Color;
    assert_eq!(buf[(x0, y0)].fg, Color::Rgb(255, 0, 0));
    assert_eq!(buf[(x0 + 15, y0)].fg, Color::Rgb(0, 255, 0));
    assert_eq!(buf[(x0, y0 + 7)].bg, Color::Rgb(0, 0, 255));
    assert_eq!(buf[(x0 + 15, y0 + 7)].bg, Color::Rgb(255, 255, 255));
    assert!(a.quick.drawn.is_some());

    // V-4: a dialog over the view shows the card, not the image.
    press(&mut a, KeyCode::F(7), NONE);
    let (text, _) = render(&mut a, 80, 24);
    assert!(a.quick.drawn.is_none());
    assert!(!text.contains('▀'), "{text}");
    assert!(
        text.contains("16 x 16 px"),
        "the card under the dialog: {text}"
    );
    press(&mut a, KeyCode::Esc, NONE);
    let (text, _) = render(&mut a, 80, 24);
    assert!(text.contains('▀') && a.quick.drawn.is_some());

    // Tab: the panel under the view becomes active and visible; the view moves.
    press(&mut a, KeyCode::Tab, NONE);
    let (text, _) = render(&mut a, 80, 24);
    let top: String = text.lines().next().unwrap().chars().take(40).collect();
    assert!(top.contains("other panel: ~/pictures"), "{top}");
    assert!(text.contains("kept"), "{text}");
}

/// A-QV-3: the card with an escaped name, a text head with control characters, a binary
/// file.
#[test]
fn a_qv_3_cards() {
    let mut a = snapshot_app();
    a.panel_mut().cursor_to_name(b"new\nline\x1b.txt");
    press(&mut a, KeyCode::Char('q'), CTRL);
    let (text, _) = render(&mut a, 80, 24);
    insta::assert_snapshot!("quick_card_escaped_name", text);

    a.panel_mut().cursor_to_name(b"notes.txt");
    render(&mut a, 80, 24);
    let mut c = Card::of_entry(b"notes.txt", &a.panel().current_entry().unwrap().1.clone());
    c.uid = Some(1000);
    c.head = text_head(b"first line\n\tindented\x1b[31m red\x07\r\nbad \xff byte\n");
    a.update(Event::Preview(Msg::Card {
        generation: a.quick.generation,
        card: c,
    }));
    let (text, _) = render(&mut a, 80, 24);
    insta::assert_snapshot!("quick_card_text_head", text);

    a.panel_mut().cursor_to_name(b"blob.bin");
    render(&mut a, 80, 24);
    let mut c = Card::of_entry(b"blob.bin", &a.panel().current_entry().unwrap().1.clone());
    c.head = Head::Binary;
    a.update(Event::Preview(Msg::Card {
        generation: a.quick.generation,
        card: c,
    }));
    let (text, _) = render(&mut a, 80, 24);
    insta::assert_snapshot!("quick_card_binary", text);
}

// ---- A-QV-4: a kitty terminal on a pty ---------------------------------------------------------

/// The kitty graphics commands in `raw`: `(action, key=value text)` per APC.
fn kitty_cmds(raw: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(k) = raw[i..].windows(3).position(|w| w == b"\x1b_G") {
        let start = i + k + 3;
        let Some(end) = raw[start..].windows(2).position(|w| w == b"\x1b\\") else {
            break;
        };
        let body = &raw[start..start + end];
        let keys = body.split(|&b| b == b';').next().unwrap_or(b"");
        out.push(String::from_utf8_lossy(keys).into_owned());
        i = start + end + 2;
    }
    out
}

fn id_of(cmd: &str) -> Option<u32> {
    cmd.split(',')
        .find_map(|kv| kv.strip_prefix("i=")?.parse().ok())
}

fn transmits(raw: &[u8]) -> Vec<u32> {
    kitty_cmds(raw)
        .iter()
        .filter(|c| c.contains("a=t") || c.contains("a=T"))
        .filter_map(|c| id_of(c))
        .collect()
}

fn with(raw: &[u8], what: &str) -> Vec<u32> {
    kitty_cmds(raw)
        .iter()
        .filter(|c| c.contains(what))
        .filter_map(|c| id_of(c))
        .collect()
}

fn log_values(log: &Path, marker: &str, key: &str) -> Vec<u64> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains(marker))
        .filter_map(|l| field(l, key)?.parse().ok())
        .collect()
}

/// A-QV-4: the test acts as a kitty-capable terminal. One transmit per image; a delete for
/// each replaced image; no image while a dialog overlaps; deletes before a hand-off and
/// before exit; key-to-frame within P-1 while a preview is pending.
#[test]
fn a_qv_4_a_kitty_terminal_on_a_pty() {
    let h = test_dir("qv4-kitty");
    let pics = h.join("pics");
    std::fs::create_dir_all(&pics).unwrap();
    write(&pics.join("a.png"), &png(60, 40));
    write(&pics.join("b.png"), &png(40, 60));
    // A large photo: its decode is still running while keys arrive.
    let photo = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(2400, 1600, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x ^ y) % 256) as u8])
    }));
    write(
        &pics.join("c.jpg"),
        &encode(&photo, image::ImageFormat::Jpeg),
    );
    write(&pics.join("d.txt"), b"text");
    let log = h.join("kitty.log");
    let pager = h.join("pager.sh");
    std::fs::write(&pager, "#!/bin/sh\nexit 0\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&pager, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut t = Tui::spawn_term(
        &[
            "--log",
            log.to_str().unwrap(),
            pics.to_str().unwrap(),
            h.path.to_str().unwrap(),
        ],
        &h.path,
        &[
            ("COLORTERM", "truecolor"),
            ("PAGER", pager.to_str().unwrap()),
        ],
        120,
        30,
        true,
        Term {
            graphics: true,
            cell: Some((10, 20)),
            ..Term::default()
        },
    );
    assert!(t.wait_for("a.png", T), "{}", t.screen());
    let text = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        field(log_line(&text, "terminal probe").unwrap(), "protocol").as_deref(),
        Some("kitty")
    );
    // Rows: .., a.png, b.png, c.jpg, d.txt.
    t.keys(&[b"\x11", DOWN]);
    assert!(
        t.wait_until(T, |t| !with(&t.raw, "a=p").is_empty()),
        "a.png is placed: {}",
        t.screen()
    );
    let a_id = transmits(&t.raw)[0];
    assert_eq!(with(&t.raw, "a=p"), [a_id]);
    t.keys(&[DOWN]);
    assert!(t.wait_until(T, |t| transmits(&t.raw).len() == 2));
    let b_id = transmits(&t.raw)[1];
    assert!(t.wait_until(T, |t| with(&t.raw, "a=p").contains(&b_id)));
    assert!(with(&t.raw, "d=i,").contains(&a_id), "a's placement went");
    // Back to a.png: stored, so placed again without a transmit.
    t.keys(&[b"\x1b[A"]);
    assert!(t.wait_until(
        T,
        |t| with(&t.raw, "a=p").iter().filter(|&&i| i == a_id).count() == 2
    ));
    assert_eq!(transmits(&t.raw), [a_id, b_id], "one transmit per image");
    assert!(with(&t.raw, "d=i,").contains(&b_id));

    // A dialog over the view: the placement goes, and comes back after it.
    let before = t.raw.len();
    t.keys(&[b"\x1b[18~"]);
    assert!(t.wait_for("Make directory", T), "{}", t.screen());
    assert!(t.wait_until(T, |t| with(&t.raw[before..], "d=i,").contains(&a_id)));
    std::thread::sleep(Duration::from_millis(150));
    t.pump();
    assert!(
        with(&t.raw[before..], "a=p").is_empty(),
        "no image under the dialog"
    );
    let before = t.raw.len();
    t.keys(&[ESC]);
    assert!(t.wait_until(T, |t| with(&t.raw[before..], "a=p").contains(&a_id)));

    // A hand-off (F3 with a pager that exits at once): deletes before leaving the screen,
    // then the full redraw transmits and places again.
    let before = t.raw.len();
    t.keys(&[F3]);
    assert!(t.wait_until(T, |t| with(&t.raw[before..], "a=p").contains(&a_id)));
    let seg = &t.raw[before..];
    let leave = seg
        .windows(8)
        .position(|w| w == b"\x1b[?1049l")
        .expect("left the screen");
    assert!(
        with(&seg[..leave], "d=I,").contains(&a_id),
        "the image is deleted before the hand-off"
    );
    assert!(
        transmits(&seg[leave..]).contains(&a_id),
        "transmitted again after it"
    );

    // P-1 while a preview is pending: rest on the photo, then type on the command line.
    t.keys(&[DOWN, DOWN]);
    let end = Instant::now() + T;
    while !std::fs::read_to_string(&log)
        .unwrap_or_default()
        .contains("c.jpg")
        && Instant::now() < end
    {
        std::thread::sleep(Duration::from_millis(5));
        t.pump();
    }
    let frames_before = log_values(&log, "frame", "key_to_flush_us").len();
    let stages = |log: &Path| {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .matches("preview stages")
            .count()
    };
    let prepared = stages(&log);
    for c in b"abcdef" {
        t.keys(&[std::slice::from_ref(c)]);
    }
    assert_eq!(
        stages(&log),
        prepared,
        "the photo's preview was still pending while the keys arrived"
    );
    t.keys(&[ESC]);
    let lat = log_values(&log, "frame", "key_to_flush_us");
    let typed = &lat[frames_before..];
    assert!(typed.len() >= 6, "{typed:?}");
    let worst = *typed.iter().max().unwrap();
    assert!(
        worst <= 16_000,
        "key-to-frame while a preview is pending: {typed:?} us"
    );

    // Exit: every stored image is deleted before the alternate screen is left.
    let before = t.raw.len();
    t.send(F10);
    assert_eq!(t.wait_exit(T), Some(0));
    let seg = &t.raw[before..];
    let leave = find_last(seg, b"\x1b[?1049l").expect("left the screen");
    let deleted = with(&seg[..leave], "d=I,");
    assert!(deleted.contains(&a_id), "{deleted:?}");
    assert!(t.restored());
}

// ---- A-QV-5: V-3 inside tmux ---------------------------------------------------------------

/// Every `CSI ? <n> h|l` mode the output sets or resets.
fn private_modes(raw: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(k) = raw[i..].windows(3).position(|w| w == b"\x1b[?") {
        let s = i + k + 3;
        let n: Vec<u8> = raw[s..]
            .iter()
            .take_while(|b| b.is_ascii_digit() || **b == b';')
            .copied()
            .collect();
        let after = raw.get(s + n.len()).copied();
        if matches!(after, Some(b'h' | b'l')) {
            for p in n.split(|&b| b == b';') {
                if let Ok(v) = std::str::from_utf8(p).unwrap_or("").parse() {
                    out.push(v);
                }
            }
        }
        i = s;
    }
    out
}

/// A-QV-5: with `TERM=tmux-256color`, `TERM_PROGRAM=tmux` and `TMUX` set for the whole run,
/// and a recording `tmux` stub first on `PATH`, a quick-view session through all three
/// protocols runs `tmux` zero times, and sets no terminal mode beyond M1's.
#[test]
fn a_qv_5_tmux_is_never_run() {
    let h = test_dir("qv5-tmux");
    let stub = h.join("stub");
    std::fs::create_dir_all(&stub).unwrap();
    let record = h.join("tmux-ran");
    std::fs::write(
        stub.join("tmux"),
        format!("#!/bin/sh\necho \"$@\" >> '{}'\n", record.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(stub.join("tmux"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = stub.clone().into_os_string();
    path.push(":");
    path.push(no_desktop_path());
    let pics = h.join("pics");
    std::fs::create_dir_all(&pics).unwrap();
    write(&pics.join("img.png"), &png(40, 40));
    for (setting, seen) in [
        ("kitty", &b"\x1bPtmux;\x1b\x1b_Ga=T,U=1"[..]),
        ("sixel", &b"q\"1;1;"[..]),
        ("halfblocks", "▀".as_bytes()),
    ] {
        let cfg = h.join(format!("cfg-{setting}/manycommander"));
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(
            cfg.join("config.toml"),
            format!("[preview]\nprotocol = \"{setting}\"\n"),
        )
        .unwrap();
        let log = h.join(format!("{setting}.log"));
        let mut t = Tui::spawn_term(
            &["--log", log.to_str().unwrap(), pics.to_str().unwrap()],
            &h.path,
            &[
                ("TERM", "tmux-256color"),
                ("TERM_PROGRAM", "tmux"),
                ("TMUX", "/nonexistent/tmux-test,1,0"),
                ("COLORTERM", "truecolor"),
                ("PATH", path.to_str().unwrap()),
                (
                    "XDG_CONFIG_HOME",
                    h.join(format!("cfg-{setting}")).to_str().unwrap(),
                ),
            ],
            100,
            30,
            true,
            Term {
                graphics: true,
                sixel: true,
                cell: Some((10, 20)),
                ..Term::default()
            },
        );
        assert!(t.wait_for("img.png", T), "{setting}: {}", t.screen());
        t.keys(&[b"\x11", DOWN]);
        assert!(
            t.wait_until(T, |t| t.raw.windows(seen.len()).any(|w| w == seen)),
            "{setting}: the image arrives: {}",
            t.screen()
        );
        t.send(F10);
        assert_eq!(t.wait_exit(T), Some(0), "{setting}");
        let text = std::fs::read_to_string(&log).unwrap();
        let line = log_line(&text, "terminal probe").unwrap();
        assert_eq!(field(line, "tmux").as_deref(), Some("true"), "{line}");
        for m in private_modes(&t.raw) {
            assert!([1049, 2004, 25].contains(&m), "{setting}: mode {m} set");
        }
        let raw = String::from_utf8_lossy(&t.raw);
        assert!(!raw.contains("\x1b[="), "{setting}: no keyboard-mode set");
        assert!(!raw.contains("allow-passthrough"), "{setting}");
    }
    assert!(
        !record.exists(),
        "tmux ran: {:?}",
        std::fs::read_to_string(&record)
    );
}

// ---- A-QV-6: archive members (V-5) ---------------------------------------------------------

fn tar_gz(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (name, data) in members {
        let mut hd = tar::Header::new_gnu();
        hd.set_size(data.len() as u64);
        hd.set_mode(0o644);
        hd.set_mtime(1_700_000_000);
        hd.set_cksum();
        b.append_data(&mut hd, name, *data).unwrap();
    }
    let tar = b.into_inner().unwrap();
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(&tar).unwrap();
    e.finish().unwrap()
}

fn zip_of(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, data) in members {
        w.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

/// V-5: moving over a compressed-tar member requests nothing, so nothing is read from it;
/// `Alt+Q` loads it through the provider. A zip member previews on cursor rest.
#[test]
fn a_qv_6_archive_members() {
    let d = test_dir("qv6-members");
    let img = png(30, 20);
    write(
        &d.join("pics.tar.gz"),
        &tar_gz(&[("img.png", &img), ("note.txt", b"hi")]),
    );
    write(&d.join("pics.zip"), &zip_of(&[("img.png", &img)]));
    for (archive, rest) in [("pics.tar.gz", false), ("pics.zip", true)] {
        let cache = IndexCache::default();
        let mut a = app(&d.path, &d.path, Depth::TrueColor);
        a.set_graphics(Protocol::Kitty, Some((10, 20)));
        let fx = a.start();
        run(&mut a, &cache, fx);
        a.panel_mut().cursor_to_name(archive.as_bytes());
        let fx = press(&mut a, KeyCode::Enter, NONE);
        run(&mut a, &cache, fx);
        assert!(a.panel().archive().is_some(), "{archive} opened");
        press(&mut a, KeyCode::Char('q'), CTRL);
        a.panel_mut().cursor_to_name(b"img.png");
        a.quick_sync();
        render(&mut a, 100, 30);
        std::thread::sleep(Duration::from_millis(120));
        let r = previews(&a.update(Event::Tick));
        if !rest {
            assert!(r.is_empty(), "{archive}: cursor rest reads nothing");
            assert_eq!(a.quick_card().reason.as_deref(), Some(pv::MEMBER_ON_KEY));
            assert_eq!(a.quick.requests, 0);
            let r = previews(&press(&mut a, KeyCode::Char('q'), ALT));
            assert_eq!(r.len(), 1, "{archive}: Alt+Q loads it");
            let (m, s) = worker::process_once(&r[0]);
            assert!(matches!(m, Some(Msg::Ready { .. })), "{m:?}");
            assert_eq!(s.place_reads.load(Ordering::Relaxed), 1);
        } else {
            assert_eq!(r.len(), 1, "{archive}: previewed on rest");
            assert!(matches!(r[0].subject, Subject::Place { .. }));
            let (m, s) = worker::process_once(&r[0]);
            let Some(Msg::Ready { card, .. }) = m else {
                panic!("{m:?}")
            };
            assert_eq!(card.pixels, Some((30, 20)));
            assert_eq!(s.place_reads.load(Ordering::Relaxed), 1);
        }
    }
}

// ---- A-KM-1 ----------------------------------------------------------------------------------

/// A-KM-1: `Ctrl+Q` and `Alt+Q` act with text on the line and leave it unchanged.
#[test]
fn a_km_1_quick_view_keys_leave_the_line() {
    let d = test_dir("qv-km1");
    write(&d.join("a.png"), &png(10, 10));
    let cache = IndexCache::default();
    let mut a = app(&d.path, &d.path, Depth::TrueColor);
    a.set_graphics(Protocol::Halfblocks, None);
    let fx = a.start();
    run(&mut a, &cache, fx);
    a.panel_mut().cursor_to_name(b"a.png");
    press(&mut a, KeyCode::Char('e'), CTRL);
    press(&mut a, KeyCode::Char('l'), NONE);
    press(&mut a, KeyCode::Char('s'), NONE);
    assert_eq!(a.line.bytes(), b"ls");
    press(&mut a, KeyCode::Char('q'), CTRL);
    assert!(a.quick.on);
    assert_eq!(a.line.bytes(), b"ls");
    render(&mut a, 100, 30);
    let r = previews(&press(&mut a, KeyCode::Char('q'), ALT));
    assert_eq!(r.len(), 1);
    assert_eq!(a.line.bytes(), b"ls");
    press(&mut a, KeyCode::Char('q'), CTRL);
    assert!(!a.quick.on);
    assert!(
        previews(&press(&mut a, KeyCode::Char('q'), ALT)).is_empty(),
        "nothing while off"
    );
    assert_eq!(a.line.bytes(), b"ls");
}

// ---- measurements (release build, run by hand) ---------------------------------------------

/// P-23: a 12 MP JPEG in a 100x50-cell pane at 10x20 px, from the request to the prepared
/// image, per protocol, a cache hit, and the stages. `MC_P23_PHOTO` names a real JPEG to
/// use instead of the generated one (scaled to 12 MP). `cargo test --release --test preview
/// -- --ignored --nocapture p_23`.
#[test]
#[ignore]
fn p_23_preview_latency() {
    let d = test_dir("p23");
    let photo = match std::env::var_os("MC_P23_PHOTO") {
        Some(p) => {
            image::open(p)
                .unwrap()
                .resize_exact(4000, 3000, image::imageops::FilterType::Triangle)
        }
        // Smooth gradients with a little noise, as a photo has.
        None => image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(4000, 3000, |x, y| {
            let n = ((x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)) >> 27) as u8;
            image::Rgb([
                (x / 16) as u8 ^ (n & 7),
                (y / 12) as u8 ^ (n & 7),
                ((x + y) / 28) as u8 ^ (n & 7),
            ])
        })),
    };
    let jpeg = encode(&photo, image::ImageFormat::Jpeg);
    println!("P-23 photo: 4000 x 3000, {} bytes as JPEG", jpeg.len());
    write(&d.join("photo.jpg"), &jpeg);
    let big = Pane {
        cols: 100,
        rows: 50,
        cell: Some((10, 20)),
    };
    // The stages, as the preview thread runs them.
    let t = Instant::now();
    let img = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).unwrap();
    let decode = t.elapsed();
    let ((w, h), _) = gfx::fit(4000, 3000, big, false);
    let t = Instant::now();
    let small = gfx::scale(&img, w, h);
    let scale = t.elapsed();
    print!(
        "P-23 stages: decode {:.1} ms, scale to {w} x {h} {:.1} ms",
        decode.as_secs_f64() * 1000.0,
        scale.as_secs_f64() * 1000.0
    );
    for p in [Protocol::Kitty, Protocol::Sixel] {
        let t = Instant::now();
        let _ = gfx::prepare(&small, big, p).unwrap();
        print!(
            ", {} encode {:.1} ms",
            p.name(),
            t.elapsed().as_secs_f64() * 1000.0
        );
    }
    println!();
    let (tx, rx) = channel();
    let w = Worker::spawn(move |m| {
        let _ = tx.send(m);
    })
    .unwrap();
    let mut generation = 0;
    for p in [
        Protocol::Kitty,
        Protocol::Halfblocks,
        Protocol::Sixel,
        Protocol::Kitty,
    ] {
        generation += 1;
        let mut r = local(&d.path, "photo.jpg", p, generation);
        r.pane = big;
        let t = Instant::now();
        w.submit(r);
        let m = recv_gen(&rx, generation);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let bytes = match &m {
            Msg::Ready { image, .. } => image.bytes(),
            _ => 0,
        };
        println!("P-23 {}: {ms:.1} ms, {bytes} bytes", p.name());
    }
}

/// P-2 and P-25 with the probe: start to first full frame on two 1k-entry directories, for a
/// terminal that answers at once (kitty graphics, a cell size, the keyboard protocol) and
/// for one that answers nothing. `cargo test --release --test preview -- --ignored
/// --nocapture p_2`.
#[test]
#[ignore]
fn p_2_first_frame_with_the_probe() {
    let h = test_dir("p2-probe");
    for side in ["l", "r"] {
        let d = h.join(side);
        std::fs::create_dir_all(&d).unwrap();
        for i in 0..1000 {
            write(&d.join(format!("f{i:04}")), b"x");
        }
    }
    let (l, r) = (h.join("l"), h.join("r"));
    for (label, term) in [
        (
            "answering",
            Term {
                graphics: true,
                cell: Some((10, 20)),
                ..Term::default()
            },
        ),
        (
            "silent",
            Term {
                silent: true,
                ..Term::default()
            },
        ),
    ] {
        let mut ms = Vec::new();
        for n in 0..20 {
            let log = h.join(format!("{label}-{n}.log"));
            let mut t = Tui::spawn_term(
                &[
                    "--log",
                    log.to_str().unwrap(),
                    "--exit-after-first-frame",
                    l.to_str().unwrap(),
                    r.to_str().unwrap(),
                ],
                &h.path,
                &[("COLORTERM", "truecolor")],
                160,
                50,
                true,
                term,
            );
            t.wait_exit(T);
            let v = log_values(&log, "first full frame", "first_full_frame_us");
            ms.push(v[0] as f64 / 1000.0);
        }
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "P-2 {label}: median {:.1} ms, max {:.1} ms over {} starts",
            ms[ms.len() / 2],
            ms[ms.len() - 1],
            ms.len()
        );
    }
}
