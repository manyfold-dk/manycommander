#![forbid(unsafe_code)]
//! The preview thread (TDR-3 4.4, 2.5): one thread, the latest request wins.
//!
//! The UI hands over the latest request ([`Worker::submit`]); an older one that has not
//! started is replaced. The thread opens the entry (V-2): a local file through the TDR-1 4.3
//! `O_PATH` sequence, so a symlink target, a FIFO or a device is never opened (I-10); a
//! member of an archive through [`Provider::open_read`](crate::provider::Provider). It
//! reads at most 64 MB, detects the format by its magic bytes, checks the header against
//! V-2's bounds (the header parse itself runs under V-2's byte limit, never inflates a PNG
//! colour profile, and never reads a WebP chunk that reaches past the end of the file), and
//! only then decodes with `image::Limits`, applies the EXIF orientation, takes the first
//! frame of an animation, fits the image into the pane without upscaling and encodes it for
//! the protocol ([`gfx::prepare_oriented`]). A newer request makes the thread drop its work
//! between reads. Everything else gets the card with its reason, never a panic: the work
//! runs under `catch_unwind` on a thread whose name starts with `list`, so a panic is only
//! logged (NFR-REL).
//!
//! A read that never returns (a FUSE file) holds the thread. When a newer request has
//! waited [`ABANDON_AFTER`] for a thread busy with an older generation, the UI abandons it
//! ([`Worker::replace`]): a new thread serves the request, the old one exits when its read
//! returns and its late answer is dropped by generation, and it counts toward
//! `MAX_ABANDONED` until then.

use super::cache::{Cache, Key, Source};
use super::card::{Card, Head, head_of, mode_text};
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

/// What a preview could not do (TDR-3 4.6).
const CANNOT_READ: &str = "cannot read the image";
const CANNOT_DECODE: &str = "cannot decode the image";
const SIZE_MISMATCH: &str = "size mismatch";
/// A JPEG that ends before its end-of-image marker.
pub const TRUNCATED: &str = "image truncated";
/// A FIFO, socket or device: described, never opened (TDR-1 4.6).
pub const SPECIAL: &str = "special file";

/// The formats the quick view decodes (TDR-3 2.7).
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
    /// The request the thread works on.
    busy: Option<Request>,
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

    /// Hands `req` to the thread (TDR-3 4.4, step 2); a request that has not started is
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
    /// older generation (TDR-3 2.5); `None` while nothing waits that way.
    pub fn abandon_due(&self) -> Option<Instant> {
        let s = self.shared.lock();
        match (&s.busy, &s.next) {
            (Some(b), Some(n)) if b.generation < n.generation => Some(s.since + ABANDON_AFTER),
            _ => None,
        }
    }

    /// Abandons the busy thread and starts a new one with the waiting request. Returns the
    /// abandoned thread's liveness and what it reads, for `MAX_ABANDONED` (TDR-3 2.5).
    ///
    /// The stale read may have returned since [`Worker::abandon_due`], and the thread taken
    /// the waiting request. Stopped, it drops that request at its next check without an
    /// answer, so the new thread serves the request again.
    pub fn replace(&mut self) -> std::io::Result<(Alive, PathBuf)> {
        let (next, blocked) = {
            let mut s = self.shared.lock();
            s.stop = true;
            let next = s.next.take().or_else(|| {
                s.busy
                    .as_ref()
                    .filter(|b| b.generation == s.latest)
                    .cloned()
            });
            (next, s.busy.as_ref().map(|b| b.subject.blocked_path()))
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
        self.ctx.cache().len()
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

/// Marks a preview thread finished when it ends, also by a panic outside a request's
/// `catch_unwind` (NFR-REL: the panic is only logged, and the thread must not count as
/// blocked).
struct Finish(Alive);

impl Drop for Finish {
    fn drop(&mut self) {
        self.0.finish();
    }
}

fn start(shared: Arc<Shared>, alive: Alive, ctx: Ctx) -> std::io::Result<()> {
    let a = Finish(alive.clone());
    let r = std::thread::Builder::new()
        .name("list-preview".into())
        .spawn(move || {
            let _finish = a;
            run(&shared, &ctx);
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
                    s.busy = Some(r.clone());
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
            // The TDR-1 4.3 sequence: O_PATH, fstat, reopen; a FIFO or a device swapped in
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
            let never = Arc::new(AtomicBool::new(false));
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
    /// The cache, also after a panic poisoned its lock (as [`Shared::lock`] takes the
    /// slot): ignoring a poisoned cache would decode every image again, and nothing would
    /// say why.
    fn cache(&self) -> MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn hit(&self, key: &Key, generation: u64) -> Option<Msg> {
        if key.protocol == Protocol::Off {
            return None;
        }
        let (image, card) = self.cache().get(key)?;
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

/// V-2's limits for `image`: the decoded bytes, and with `dims` the dimensions.
fn limits(dims: bool) -> Limits {
    let mut limits = Limits::default();
    if dims {
        limits.max_image_width = Some(MAX_DIM);
        limits.max_image_height = Some(MAX_DIM);
    }
    limits.max_alloc = Some(MAX_DECODED);
    limits
}

/// The pixel size and EXIF orientation from the header only (V-2: before any decode), under
/// V-2's byte limit. The dimensions are left to the check after it, so the card still names
/// the pixel size of an image above them (TDR-3 4.6). A PNG's colour profile in `data` is made
/// a chunk the decoders skip first ([`skip_profile`]), for the decode too; a WebP whose EXIF
/// chunk reaches past the end of the file is not read for its orientation
/// ([`webp_exif_inside`]).
fn header(fmt: ImageFormat, data: &mut [u8]) -> image::ImageResult<((u32, u32), Orientation, u64)> {
    if fmt == ImageFormat::Png {
        skip_profile(data);
    }
    let mut r = ImageReader::with_format(Cursor::new(&*data), fmt);
    r.limits(limits(false));
    let mut dec = r.into_decoder()?;
    let dims = dec.dimensions();
    // The decoder's own buffer: a 16-bit RGBA image needs 8 bytes a pixel, twice the RGBA
    // copy the preview makes afterwards.
    let native = dec.total_bytes();
    let o = if fmt == ImageFormat::WebP && !webp_exif_inside(data) {
        Orientation::NoTransforms
    } else {
        dec.orientation().unwrap_or(Orientation::NoTransforms)
    };
    Ok((dims, o, native))
}

/// A private ancillary PNG chunk type that no decoder knows: a colour profile renamed to it
/// is skipped unread.
const SKIPPED_PROFILE: &[u8; 4] = b"icCP";

/// Makes a PNG's colour profiles chunks the decoders skip (V-2). The png crate inflates an
/// `iCCP` profile whole while it reads the header, before any check here, and drops one
/// that passes its byte limit only after inflating that much: a file of 280 KB inflates to
/// 280 MiB. The preview has no use for a profile, so each `iCCP` chunk before the image
/// data is renamed to [`SKIPPED_PROFILE`] with its CRC made to match, and the crate skips
/// it without reading it into memory.
fn skip_profile(d: &mut [u8]) {
    let mut at = 8usize;
    while let Some(h) = at.checked_add(8).and_then(|e| d.get(at..e)) {
        let len = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) as usize;
        let kind = [h[4], h[5], h[6], h[7]];
        if &kind == b"IDAT" {
            return;
        }
        // Where the chunk's CRC starts; it covers the type and the data.
        let crc_at = at.saturating_add(8).saturating_add(len);
        if &kind == b"iCCP" {
            d[at + 4..at + 8].copy_from_slice(SKIPPED_PROFILE);
            if let Some(typed) = d.get(at + 4..crc_at) {
                let mut crc = flate2::Crc::new();
                crc.update(typed);
                let sum = crc.sum().to_be_bytes();
                if let Some(c) = d.get_mut(crc_at..crc_at.saturating_add(4)) {
                    c.copy_from_slice(&sum);
                }
            }
        }
        at = crc_at.saturating_add(4);
    }
}

/// Whether a WebP's EXIF chunk, which the orientation comes from, lies inside the file.
/// image-webp reads a chunk into a buffer of the size the chunk declares (up to 4 GiB)
/// before it reads the bytes, and `image` passes it no limit; so a chunk that reaches past
/// the end is not read, and the image is shown upright.
fn webp_exif_inside(d: &[u8]) -> bool {
    // After the RIFF header, the chunks in order: a tag, a little-endian size, the data,
    // and a pad byte after an odd size.
    let mut at = 12usize;
    while let Some(h) = at.checked_add(8).and_then(|e| d.get(at..e)) {
        let size = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as usize;
        let end = at.saturating_add(8).saturating_add(size);
        if h[..4] == *b"EXIF" {
            return end <= d.len();
        }
        at = end.saturating_add(size & 1);
    }
    true
}

/// Whether a JPEG stream reaches its end-of-image marker. The segments are walked by their
/// lengths, so the marker of an EXIF thumbnail does not count, and data after the image (a
/// motion photo's video) does not matter. The decoder pads a truncated stream without an
/// error; the card says so instead (TDR-3 4.6).
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
    let mut r = ImageReader::with_format(Cursor::new(data), fmt);
    r.limits(limits(true));
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
    // One byte past a declared size shows a file that grew; the bound is computed within
    // V-2's 64 MB, so the largest declared size cannot overflow it.
    let first = TEXT_HEAD.min(declared.map_or(usize::MAX, |d| d.min(MAX_FILE) as usize + 1));
    match read_into(r, &mut data, first, stop) {
        Ok(()) => {}
        Err(Stop::Superseded) => return None,
        Err(Stop::Io(e)) => return done(card.with_reason(e.to_string())),
    }
    let fmt = image::guess_format(&data).ok().filter(|f| supported(*f));
    let Some(fmt) = fmt else {
        card.head = head_of(&card.name, &data);
        return done(card);
    };
    card.head = Head::None;
    if size > MAX_FILE {
        if let Ok(((w, h), o, _)) = header(fmt, &mut data) {
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
    let ((w, h), orientation, native) = match header(fmt, &mut data) {
        Ok(x) => x,
        Err(e) => return done(card.with_reason(format!("{CANNOT_READ}: {e}"))),
    };
    card.pixels = Some(if swaps(orientation) { (h, w) } else { (w, h) });
    if w > MAX_DIM || h > MAX_DIM {
        return done(card.with_reason(TOO_WIDE));
    }
    // V-2: both the decoder's buffer (16-bit images need up to 8 bytes a pixel) and the
    // RGBA copy stay within the byte limit.
    if w as u64 * h as u64 * 4 > MAX_DECODED || native > MAX_DECODED {
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
    ctx.cache().insert(key, prepared.clone(), card.clone());
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::Duration;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mc-worker-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn request(generation: u64, dir: &Path, name: &str, protocol: Protocol) -> Request {
        Request {
            generation,
            subject: Subject::Local {
                dir: dir.to_path_buf(),
                name: name.into(),
            },
            pane: super::super::Pane {
                cols: 8,
                rows: 4,
                cell: None,
            },
            protocol,
        }
    }

    fn ctx(send: impl Fn(Msg) + Send + Sync + 'static) -> Ctx {
        Ctx {
            send: Arc::new(send),
            cache: Arc::new(Mutex::new(Cache::default())),
            stats: Arc::new(Stats::default()),
        }
    }

    /// The stale read returns between `abandon_due` and `replace`, and the old thread takes
    /// the waiting request. `replace` stops that thread, which drops the request at its next
    /// check, so the new thread must serve it: else the pane gets no answer until the
    /// cursor moves.
    #[test]
    fn replace_serves_the_request_the_abandoned_thread_took() {
        let d = temp("replace");
        std::fs::write(d.join("a.txt"), b"hello\n").unwrap();
        let (tx, rx) = mpsc::channel();
        // No thread serves `shared`: its slot is as the race leaves it, the old thread busy
        // with the newest generation and nothing waiting.
        let shared = Arc::new(Shared::new());
        {
            let mut s = shared.lock();
            s.latest = 2;
            s.busy = Some(request(2, &d, "a.txt", Protocol::Off));
        }
        let mut w = Worker {
            shared,
            alive: Alive::running(),
            ctx: ctx(move |m| {
                let _ = tx.send(m);
            }),
        };
        let (_, blocked) = w.replace().unwrap();
        assert_eq!(blocked, d.join("a.txt"));
        let m = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("no answer for the newest generation");
        assert_eq!(m.generation(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A non-local file that declares the largest size reads its text head; the bound is
    /// not computed past `u64`.
    #[test]
    fn the_largest_declared_size_reads_the_text_head() {
        let d = temp("declared");
        let req = request(1, &d, "big.txt", Protocol::Off);
        let key = Key {
            source: Source::Place {
                place: 1,
                path: crate::provider::VPath::root(),
                mtime: 0,
                size: u64::MAX,
            },
            pane: req.pane,
            protocol: req.protocol,
        };
        let c = ctx(|_| {});
        let mut r = Cursor::new(b"hello\n".to_vec());
        let out = content(
            &req,
            &c,
            Card::default(),
            &mut r,
            u64::MAX,
            Some(u64::MAX),
            key,
            Instant::now(),
            &|| false,
        );
        let Some(Msg::Card { card, .. }) = out else {
            panic!("{out:?}");
        };
        assert_eq!(card.head, Head::Text(vec![b"hello".to_vec()]));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A panic on the preview thread outside a request's `catch_unwind` (here in the
    /// answer's send) still marks the thread finished, so it does not count as blocked.
    #[test]
    fn a_panic_outside_a_request_finishes_the_thread() {
        let d = temp("panic");
        std::fs::write(d.join("a.txt"), b"hello\n").unwrap();
        let w = Worker::spawn(|_| panic!("the answer cannot be sent")).unwrap();
        w.submit(request(1, &d, "a.txt", Protocol::Off));
        let end = Instant::now() + Duration::from_secs(5);
        while w.alive().is_running() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !w.alive().is_running(),
            "the thread ended but counts as running"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A panic while the cache is locked poisons its mutex; the cache keeps working.
    #[test]
    fn a_poisoned_cache_still_serves() {
        let d = temp("poison");
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]))
            .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .unwrap();
        let c = ctx(|_| {});
        let cache = c.cache.clone();
        let _ = std::thread::spawn(move || {
            let _g = cache.lock().unwrap();
            panic!("poisons the cache");
        })
        .join();
        assert!(c.cache.is_poisoned());
        let req = request(1, &d, "red.png", Protocol::Halfblocks);
        let key = Key {
            source: Source::Place {
                place: 1,
                path: crate::provider::VPath::root(),
                mtime: 0,
                size: png.len() as u64,
            },
            pane: req.pane,
            protocol: req.protocol,
        };
        let out = content(
            &req,
            &c,
            Card::default(),
            &mut Cursor::new(png.clone()),
            png.len() as u64,
            Some(png.len() as u64),
            key.clone(),
            Instant::now(),
            &|| false,
        );
        assert!(matches!(out, Some(Msg::Ready { .. })), "{out:?}");
        assert!(c.hit(&key, 2).is_some(), "the prepared image is cached");
        assert_eq!(c.stats.hits.load(Ordering::Relaxed), 1);
        let _ = std::fs::remove_dir_all(&d);
    }
}
