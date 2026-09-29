#![forbid(unsafe_code)]
//! The preview thread (P3 4.4, 2.5): one thread, the latest request wins.
//!
//! The UI hands over the latest request ([`Worker::submit`]); an older one that has not
//! started is replaced. The thread opens the entry (V-2): a local file through the M1 4.3
//! `O_PATH` sequence, so a symlink target, a FIFO or a device is never opened (I-10); a
//! member of an archive through [`Provider::open_read`](crate::provider::Provider). It
//! reads at most 64 MB, detects the format by its magic bytes, checks the header against
//! V-2's bounds, and only then decodes with `image::Limits`, applies the EXIF orientation,
//! takes the first frame of an animation, fits the image into the pane without upscaling
//! and encodes it for the protocol ([`gfx::prepare_oriented`]). A newer request makes the
//! thread drop its work between reads. Everything else gets the card with its reason,
//! never a panic: the work runs under `catch_unwind` on a thread whose name starts with
//! `list`, so a panic is only logged (NFR-REL).
//!
//! A read that never returns (a FUSE file) holds the thread. When a newer request has
//! waited [`ABANDON_AFTER`] for a thread busy with an older generation, the UI abandons it
//! ([`Worker::replace`]): a new thread serves the request, the old one exits when its read
//! returns and its late answer is dropped by generation, and it counts toward
//! `MAX_ABANDONED` until then.

use super::cache::{Cache, Key, Source};
use super::card::{Card, Head, mode_text, text_head};
use super::gfx::{self, Prepared, swaps};
use super::{
    ABANDON_AFTER, MAX_DECODED, MAX_DIM, MAX_FILE, Msg, Protocol, Request, Subject, TEXT_HEAD,
    TOO_LARGE, TOO_MUCH_MEMORY, TOO_WIDE,
};
use crate::fsops::sys::{Kind, Sys, fd};
use crate::fsops::walk::{errno_text, open_for_read};
use crate::panel::listing::Alive;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits, metadata::Orientation};
use std::io::{Cursor, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Instant;

/// What a preview could not do (P3 4.6).
const CANNOT_READ: &str = "cannot read the image";
const CANNOT_DECODE: &str = "cannot decode the image";
const SIZE_MISMATCH: &str = "size mismatch";
/// A JPEG that ends before its end-of-image marker.
pub const TRUNCATED: &str = "image truncated";
/// A FIFO, socket or device: described, never opened (M1 4.6).
pub const SPECIAL: &str = "special file";

/// The formats the quick view decodes (P3 2.7).
pub fn supported(f: ImageFormat) -> bool {
    matches!(
        f,
        ImageFormat::Png
            | ImageFormat::Jpeg
            | ImageFormat::Gif
            | ImageFormat::WebP
            | ImageFormat::Bmp
    )
}

/// Counters for tests and `--log`.
#[derive(Debug, Default)]
pub struct Stats {
    /// Images decoded.
    pub decodes: AtomicU64,
    /// Local files opened for reading.
    pub opens: AtomicU64,
    /// Non-local files opened through their provider.
    pub place_reads: AtomicU64,
    /// Answers served from the cache.
    pub hits: AtomicU64,
}

struct Slot {
    /// The request waiting for the thread.
    next: Option<Request>,
    /// When `next` was handed over.
    since: Instant,
    /// The generation the thread works on, and what it reads.
    busy: Option<(u64, PathBuf)>,
    /// The newest generation handed over: older work stops between reads.
    latest: u64,
    /// The thread was abandoned or the app ends: it takes no further request.
    stop: bool,
}

struct Shared {
    slot: Mutex<Slot>,
    cv: Condvar,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            slot: Mutex::new(Slot {
                next: None,
                since: Instant::now(),
                busy: None,
                latest: 0,
                stop: false,
            }),
            cv: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn superseded(&self, generation: u64) -> bool {
        let s = self.lock();
        s.stop || s.latest > generation
    }
}

type Send_ = Arc<dyn Fn(Msg) + Send + Sync>;

/// What every preview thread shares: the cache survives an abandoned thread.
#[derive(Clone)]
struct Ctx {
    send: Send_,
    cache: Arc<Mutex<Cache>>,
    stats: Arc<Stats>,
}

/// The UI's handle on the preview thread.
pub struct Worker {
    shared: Arc<Shared>,
    alive: Alive,
    ctx: Ctx,
}

impl Worker {
    /// Starts the preview thread; its answers go to `send`.
    pub fn spawn(send: impl Fn(Msg) + Send + Sync + 'static) -> std::io::Result<Worker> {
        let ctx = Ctx {
            send: Arc::new(send),
            cache: Arc::new(Mutex::new(Cache::default())),
            stats: Arc::new(Stats::default()),
        };
        let shared = Arc::new(Shared::new());
        let alive = Alive::running();
        start(shared.clone(), alive.clone(), ctx.clone())?;
        Ok(Worker { shared, alive, ctx })
    }

    /// Hands `req` to the thread (P3 4.4, step 2); a request that has not started is
    /// replaced.
    pub fn submit(&self, req: Request) {
        let mut s = self.shared.lock();
        s.latest = s.latest.max(req.generation);
        s.next = Some(req);
        s.since = Instant::now();
        drop(s);
        self.shared.cv.notify_all();
    }

    /// When the waiting request will have waited [`ABANDON_AFTER`] for a thread busy with an
    /// older generation (P3 2.5); `None` while nothing waits that way.
    pub fn abandon_due(&self) -> Option<Instant> {
        let s = self.shared.lock();
        match (&s.busy, &s.next) {
            (Some((b, _)), Some(n)) if *b < n.generation => Some(s.since + ABANDON_AFTER),
            _ => None,
        }
    }

    /// Abandons the busy thread and starts a new one with the waiting request. Returns the
    /// abandoned thread's liveness and what it reads, for `MAX_ABANDONED` (P3 2.5).
    pub fn replace(&mut self) -> std::io::Result<(Alive, PathBuf)> {
        let (next, blocked) = {
            let mut s = self.shared.lock();
            s.stop = true;
            (s.next.take(), s.busy.as_ref().map(|b| b.1.clone()))
        };
        self.shared.cv.notify_all();
        let shared = Arc::new(Shared::new());
        if let Some(r) = next {
            let mut s = shared.lock();
            s.latest = r.generation;
            s.next = Some(r);
        }
        let alive = Alive::running();
        start(shared.clone(), alive.clone(), self.ctx.clone())?;
        let old = std::mem::replace(&mut self.alive, alive);
        self.shared = shared;
        Ok((old, blocked.unwrap_or_default()))
    }

    /// The current thread's liveness.
    pub fn alive(&self) -> &Alive {
        &self.alive
    }

    pub fn stats(&self) -> &Stats {
        &self.ctx.stats
    }

    /// Prepared images in the cache.
    pub fn cached(&self) -> usize {
        self.ctx.cache.lock().map(|c| c.len()).unwrap_or(0)
    }

    /// Asks the thread to end once it is idle (at exit).
    pub fn stop(&self) {
        self.shared.lock().stop = true;
        self.shared.cv.notify_all();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn start(shared: Arc<Shared>, alive: Alive, ctx: Ctx) -> std::io::Result<()> {
    let a = alive.clone();
    let r = std::thread::Builder::new()
        .name("list-preview".into())
        .spawn(move || {
            run(&shared, &ctx);
            a.finish();
        });
    if r.is_err() {
        alive.finish();
    }
    r.map(drop)
}

fn run(shared: &Shared, ctx: &Ctx) {
    loop {
        let req = {
            let mut s = shared.lock();
            loop {
                if s.stop {
                    return;
                }
                if let Some(r) = s.next.take() {
                    s.busy = Some((r.generation, r.subject.blocked_path()));
                    break r;
                }
                s = shared.cv.wait(s).unwrap_or_else(|e| e.into_inner());
            }
        };
        let generation = req.generation;
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            process(&req, ctx, &|| shared.superseded(generation))
        }));
        shared.lock().busy = None;
        match out {
            Ok(Some(m)) => (ctx.send)(m),
            Ok(None) => {}
            Err(_) => (ctx.send)(Msg::Card {
                generation,
                card: Card {
                    name: subject_name(&req.subject),
                    kind: "unknown",
                    ..Card::default()
                }
                .with_reason("internal error while preparing the preview"),
            }),
        }
    }
}

fn subject_name(s: &Subject) -> Vec<u8> {
    match s {
        Subject::Local { name, .. } => name.clone().into_vec(),
        Subject::Place { name, .. } => name.clone(),
    }
}

/// Prepares one request; `None` when a newer one superseded it.
fn process(req: &Request, ctx: &Ctx, stop: &dyn Fn() -> bool) -> Option<Msg> {
    let started = Instant::now();
    let generation = req.generation;
    let card = |card: Card| Some(Msg::Card { generation, card });
    match &req.subject {
        Subject::Local { dir, name } => {
            let sys = Sys::default();
            let nb = name.as_bytes();
            let base = Card {
                name: nb.to_vec(),
                kind: "unknown",
                ..Card::default()
            };
            let dirfd = match sys.open_root(dir) {
                Ok(f) => f,
                Err(e) => return card(base.with_reason(errno_text(e))),
            };
            // The entry itself (V-2): a symlink is described, never followed, and resting on
            // an automount trigger does not mount it.
            let meta = match sys.stat_at_noauto(fd(&dirfd), name) {
                Ok(m) => m,
                Err(e) => return card(base.with_reason(errno_text(e))),
            };
            let mut c = Card::of_meta(nb, &meta);
            match meta.kind {
                Kind::Symlink => {
                    c.target = sys
                        .readlink("preview.readlink", fd(&dirfd), name)
                        .ok()
                        .map(|t| t.into_vec());
                    return card(c);
                }
                Kind::File => {}
                Kind::Dir => return card(c),
                // A FIFO or a device is never opened (V-2, I-10).
                _ => return card(c.with_reason(SPECIAL)),
            }
            let key = Key {
                source: Source::Local {
                    dev: meta.id.dev,
                    ino: meta.id.ino,
                    mtime: meta.mtime,
                    size: meta.size,
                },
                pane: req.pane,
                protocol: req.protocol,
            };
            if let Some(m) = ctx.hit(&key, generation) {
                return Some(m);
            }
            // The M1 4.3 sequence: O_PATH, fstat, reopen; a FIFO or a device swapped in
            // meanwhile fails with "type changed" and is never opened for I/O.
            let (file, m2) = match open_for_read(&sys, fd(&dirfd), name, Some(meta.id.inode())) {
                Ok(x) => x,
                Err(e) => return card(c.with_reason(e.to_string())),
            };
            ctx.stats.opens.fetch_add(1, Ordering::Relaxed);
            let mut r = std::fs::File::from(file);
            content(req, ctx, c, &mut r, m2.size, None, key, started, stop)
        }
        Subject::Place {
            place,
            path,
            name,
            size,
            mtime,
            perm,
            place_id,
        } => {
            let c = Card {
                name: name.clone(),
                kind: "regular file",
                size: Some(*size),
                mtime: Some(*mtime),
                mode: Some(mode_text('-', *perm)),
                ..Card::default()
            };
            let key = Key {
                source: Source::Place {
                    place: *place_id,
                    path: path.clone(),
                    mtime: *mtime,
                    size: *size,
                },
                pane: req.pane,
                protocol: req.protocol,
            };
            if let Some(m) = ctx.hit(&key, generation) {
                return Some(m);
            }
            let never = AtomicBool::new(false);
            let mut r = match place.open_read(path, &never) {
                Ok(r) => r,
                Err(e) => return card(c.with_reason(e.to_string())),
            };
            ctx.stats.place_reads.fetch_add(1, Ordering::Relaxed);
            content(req, ctx, c, &mut r, *size, Some(*size), key, started, stop)
        }
    }
}

impl Ctx {
    fn hit(&self, key: &Key, generation: u64) -> Option<Msg> {
        if key.protocol == Protocol::Off {
            return None;
        }
        let (image, card) = self.cache.lock().ok()?.get(key)?;
        self.stats.hits.fetch_add(1, Ordering::Relaxed);
        Some(Msg::Ready {
            generation,
            image,
            card,
        })
    }
}

enum Stop {
    Superseded,
    Io(std::io::Error),
}

/// Reads from `r` into `buf` until it holds `max` bytes or the end; stops between reads of
/// at most 1 MiB when `stop` says so.
fn read_into(
    r: &mut dyn Read,
    buf: &mut Vec<u8>,
    max: usize,
    stop: &dyn Fn() -> bool,
) -> Result<(), Stop> {
    const STEP: usize = 1 << 20;
    while buf.len() < max {
        if stop() {
            return Err(Stop::Superseded);
        }
        let at = buf.len();
        let want = STEP.min(max - at);
        buf.resize(at + want, 0);
        match r.read(&mut buf[at..]) {
            Ok(0) => {
                buf.truncate(at);
                break;
            }
            Ok(n) => buf.truncate(at + n),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => buf.truncate(at),
            Err(e) => {
                buf.truncate(at);
                return Err(Stop::Io(e));
            }
        }
    }
    Ok(())
}

/// The pixel size and EXIF orientation from the header only (V-2: before any decode).
fn header(fmt: ImageFormat, data: &[u8]) -> image::ImageResult<((u32, u32), Orientation)> {
    let mut r = ImageReader::with_format(Cursor::new(data), fmt);
    r.no_limits();
    let mut dec = r.into_decoder()?;
    let dims = dec.dimensions();
    let o = dec.orientation().unwrap_or(Orientation::NoTransforms);
    Ok((dims, o))
}

/// Whether a JPEG stream reaches its end-of-image marker. The segments are walked by their
/// lengths, so the marker of an EXIF thumbnail does not count, and data after the image (a
/// motion photo's video) does not matter. The decoder pads a truncated stream without an
/// error; the card says so instead (P3 4.6).
pub fn jpeg_complete(d: &[u8]) -> bool {
    if !d.starts_with(&[0xFF, 0xD8]) {
        return false;
    }
    let mut i = 2;
    loop {
        // The next marker, after any fill bytes.
        let Some(k) = memchr::memchr(0xFF, &d[i.min(d.len())..]) else {
            return false;
        };
        i += k;
        while d.get(i) == Some(&0xFF) {
            i += 1;
        }
        let Some(&m) = d.get(i) else {
            return false;
        };
        i += 1;
        match m {
            0xD9 => return true,
            0x01 | 0xD0..=0xD7 => {}
            _ => {
                let Some(len) = d
                    .get(i..i + 2)
                    .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
                else {
                    return false;
                };
                if len < 2 {
                    return false;
                }
                i += len;
                if m == 0xDA {
                    // Entropy-coded data up to the next marker that is not a stuffed byte or
                    // a restart.
                    loop {
                        let Some(k) = memchr::memchr(0xFF, &d[i.min(d.len())..]) else {
                            return false;
                        };
                        i += k;
                        match d.get(i + 1) {
                            None => return false,
                            Some(0x00 | 0xD0..=0xD7) => i += 2,
                            Some(0xFF) => i += 1,
                            Some(_) => break,
                        }
                    }
                }
            }
        }
    }
}

/// Decodes under `image::Limits` of V-2's bounds; a decoder that cannot take them is not
/// run. An animation gives its first frame.
fn decode(fmt: ImageFormat, data: &[u8]) -> image::ImageResult<DynamicImage> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIM);
    limits.max_image_height = Some(MAX_DIM);
    limits.max_alloc = Some(MAX_DECODED);
    let mut r = ImageReader::with_format(Cursor::new(data), fmt);
    r.limits(limits);
    let dec = r.into_decoder()?;
    DynamicImage::from_decoder(dec)
}

/// The content of a regular file: an image when its magic says so and V-2's bounds hold,
/// else the card with the text head or the reason. `declared`: a non-local file's size,
/// which the read never passes (A-4).
#[allow(clippy::too_many_arguments)]
fn content(
    req: &Request,
    ctx: &Ctx,
    mut card: Card,
    r: &mut dyn Read,
    size: u64,
    declared: Option<u64>,
    key: Key,
    started: Instant,
    stop: &dyn Fn() -> bool,
) -> Option<Msg> {
    let generation = req.generation;
    let done = |card: Card| Some(Msg::Card { generation, card });
    let mut data = Vec::new();
    let first = TEXT_HEAD.min(declared.map_or(usize::MAX, |d| d as usize + 1));
    match read_into(r, &mut data, first, stop) {
        Ok(()) => {}
        Err(Stop::Superseded) => return None,
        Err(Stop::Io(e)) => return done(card.with_reason(e.to_string())),
    }
    let fmt = image::guess_format(&data).ok().filter(|f| supported(*f));
    let Some(fmt) = fmt else {
        card.head = text_head(&data);
        return done(card);
    };
    card.head = Head::None;
    if size > MAX_FILE {
        if let Ok(((w, h), o)) = header(fmt, &data) {
            card.pixels = Some(if swaps(o) { (h, w) } else { (w, h) });
        }
        return done(card.with_reason(TOO_LARGE));
    }
    let cap = declared.map_or(MAX_FILE as usize + 1, |d| (d.min(MAX_FILE) as usize) + 1);
    match read_into(r, &mut data, cap, stop) {
        Ok(()) => {}
        Err(Stop::Superseded) => return None,
        Err(Stop::Io(e)) => return done(card.with_reason(e.to_string())),
    }
    if declared.is_some_and(|d| data.len() as u64 != d) {
        return done(card.with_reason(SIZE_MISMATCH));
    }
    if data.len() as u64 > MAX_FILE {
        return done(card.with_reason(TOO_LARGE));
    }
    let read_ms = started.elapsed().as_secs_f64() * 1000.0;
    // V-2: the header before any decode.
    let ((w, h), orientation) = match header(fmt, &data) {
        Ok(x) => x,
        Err(e) => return done(card.with_reason(format!("{CANNOT_READ}: {e}"))),
    };
    card.pixels = Some(if swaps(orientation) { (h, w) } else { (w, h) });
    if w > MAX_DIM || h > MAX_DIM {
        return done(card.with_reason(TOO_WIDE));
    }
    if w as u64 * h as u64 * 4 > MAX_DECODED {
        return done(card.with_reason(TOO_MUCH_MEMORY));
    }
    if fmt == ImageFormat::Jpeg && !jpeg_complete(&data) {
        return done(card.with_reason(TRUNCATED));
    }
    if req.protocol == Protocol::Off || stop() {
        return if stop() { None } else { done(card) };
    }
    let t = Instant::now();
    ctx.stats.decodes.fetch_add(1, Ordering::Relaxed);
    let img = match decode(fmt, &data) {
        Ok(i) => i,
        Err(e) => return done(card.with_reason(format!("{CANNOT_DECODE}: {e}"))),
    };
    drop(data);
    let decode_ms = t.elapsed().as_secs_f64() * 1000.0;
    if stop() {
        return None;
    }
    let t = Instant::now();
    let Some(prepared) = gfx::prepare_oriented(&img, orientation, req.pane, req.protocol) else {
        return done(card.with_reason("cannot encode the image"));
    };
    drop(img);
    let prepared: Arc<Prepared> = Arc::new(prepared);
    tracing::debug!(
        generation,
        read_ms,
        decode_ms,
        prepare_ms = t.elapsed().as_secs_f64() * 1000.0,
        total_ms = started.elapsed().as_secs_f64() * 1000.0,
        bytes = prepared.bytes(),
        protocol = req.protocol.name(),
        "preview stages"
    );
    if let Ok(mut c) = ctx.cache.lock() {
        c.insert(key, prepared.clone(), card.clone());
    }
    Some(Msg::Ready {
        generation,
        image: prepared,
        card,
    })
}

/// Runs one request on the calling thread with a fresh cache: for tests of the pipeline's
/// limits without a thread. Returns the answer and the counters.
pub fn process_once(req: &Request) -> (Option<Msg>, Arc<Stats>) {
    let ctx = Ctx {
        send: Arc::new(|_| {}),
        cache: Arc::new(Mutex::new(Cache::default())),
        stats: Arc::new(Stats::default()),
    };
    let stats = ctx.stats.clone();
    (process(req, &ctx, &|| false), stats)
}
