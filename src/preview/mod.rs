#![forbid(unsafe_code)]
//! The image quick view (P3 4): `Ctrl+Q` turns the inactive side into a view of the active
//! panel's cursor entry, an image for a supported picture and the info card for everything
//! else.
//!
//! The pieces, all own code (D-5): the startup [`probe`] of the terminal (P3 4.2), the
//! graphics layer [`gfx`] (kitty graphics, sixel through `icy_sixel`, halfblocks; P3 4.3,
//! 4.7), the preview thread [`worker`] with its [`cache`] (P3 4.4), and the [`card`]
//! (P3 4.6). The UI thread only draws (V-1): reading, decoding, resizing and encoding run on
//! the preview thread, and a request reaches it only after the cursor rests for
//! [`DEBOUNCE`]. [`QuickView`] is the view's state as `App` keeps it.

pub mod cache;
pub mod card;
pub mod gfx;
pub mod probe;
pub mod worker;

use crate::provider::{Provider, VPath};
use card::Card;
use gfx::Prepared;
use ratatui::layout::Rect;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A cursor change starts this timer, and each further change restarts it (P3 4.4).
pub const DEBOUNCE: Duration = Duration::from_millis(100);
/// A newer request that waited this long for a thread busy with a stale generation
/// abandons that thread (P3 2.5).
pub const ABANDON_AFTER: Duration = Duration::from_secs(1);
/// A preview reads at most this much of a file (V-2).
pub const MAX_FILE: u64 = 64 << 20;
/// Images larger than this in either dimension are not decoded (V-2).
pub const MAX_DIM: u32 = 16384;
/// Images whose `width * height * 4` exceeds this are not decoded (V-2).
pub const MAX_DECODED: u64 = 256 << 20;
/// The text head of the card reads at most this much (P3 4.6).
pub const TEXT_HEAD: usize = 64 << 10;
/// A NUL byte in this much of a file's start makes it binary: no text head (P3 4.6).
pub const BINARY_SNIFF: usize = 8 << 10;

/// The card's reasons (P3 4.6).
pub const TOO_WIDE: &str = "image larger than 16384 x 16384 px";
pub const TOO_MUCH_MEMORY: &str = "image needs more than 256 MB decoded";
pub const TOO_LARGE: &str = "file larger than 64 MB";
pub const REMOTE_ON_KEY: &str = "remote file: Alt+Q previews it";
pub const MEMBER_ON_KEY: &str = "compressed archive member: Alt+Q previews it";
pub const BLOCKED: &str = "previews are blocked by a file that does not respond";

/// How images reach the terminal (P3 4.3), chosen once from the probe and
/// `preview.protocol` ([`probe::choose`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Protocol {
    /// Kitty graphics, placed directly at the pane's cell position.
    Kitty,
    /// Kitty graphics through tmux's passthrough, drawn with unicode placeholders.
    KittyTmux,
    /// Sixel, encoded by `icy_sixel` (foot; tmux re-encodes it).
    Sixel,
    /// `▀` cells with 24-bit colours.
    Halfblocks,
    /// No image, only the card: `preview.protocol = "off"`, `NO_COLOR`, no truecolor.
    #[default]
    Off,
}

impl Protocol {
    pub fn name(self) -> &'static str {
        match self {
            Protocol::Kitty => "kitty",
            Protocol::KittyTmux => "kitty (tmux, unicode placeholders)",
            Protocol::Sixel => "sixel",
            Protocol::Halfblocks => "halfblocks",
            Protocol::Off => "off",
        }
    }

    /// Whether the pixels are drawn by the terminal outside ratatui's cells, after the
    /// frame: a direct kitty placement or a sixel.
    pub fn outside_cells(self) -> bool {
        matches!(self, Protocol::Kitty | Protocol::Sixel)
    }
}

/// The pane a preview is prepared for: its size in cells and, when known, the cell size in
/// pixels (P3 4.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Pane {
    pub cols: u16,
    pub rows: u16,
    pub cell: Option<(u16, u16)>,
}

/// What a preview reads (P3 4.4, step 3).
#[derive(Clone)]
pub enum Subject {
    /// A local entry: `name` in `dir`, opened through the M1 4.3 `O_PATH` sequence (V-2).
    Local { dir: PathBuf, name: OsString },
    /// A regular file in a non-local place, read through [`Provider::open_read`]: an
    /// archive member (V-5 decides when), later a remote file.
    Place {
        place: Arc<dyn Provider>,
        /// The place's id, for the cache key (P3 4.4).
        place_id: u64,
        path: VPath,
        name: Vec<u8>,
        size: u64,
        mtime: i64,
        perm: u32,
    },
}

impl std::fmt::Debug for Subject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Subject::Local { dir, name } => f
                .debug_struct("Local")
                .field("dir", dir)
                .field("name", name)
                .finish(),
            Subject::Place { place_id, path, .. } => f
                .debug_struct("Place")
                .field("place_id", place_id)
                .field("path", path)
                .finish_non_exhaustive(),
        }
    }
}

impl Subject {
    /// What a blocked read counts as in the abandoned-thread limit (M1 3.1).
    pub fn blocked_path(&self) -> PathBuf {
        match self {
            Subject::Local { dir, name } => dir.join(name),
            Subject::Place { path, .. } => PathBuf::from(path.to_os_string()),
        }
    }
}

/// The latest request the UI hands the preview thread (P3 4.4, step 2).
#[derive(Clone, Debug)]
pub struct Request {
    pub generation: u64,
    pub subject: Subject,
    pub pane: Pane,
    pub protocol: Protocol,
}

impl PartialEq for Request {
    fn eq(&self, o: &Request) -> bool {
        self.generation == o.generation
    }
}

impl Eq for Request {}

/// What the preview thread sends (P3 2.5).
#[derive(Debug)]
pub enum Msg {
    /// A prepared image, with the card the pane shows while a modal overlaps it.
    Ready {
        generation: u64,
        image: Arc<Prepared>,
        card: Card,
    },
    /// Everything that is not a previewable image.
    Card { generation: u64, card: Card },
}

impl Msg {
    pub fn generation(&self) -> u64 {
        match self {
            Msg::Ready { generation, .. } | Msg::Card { generation, .. } => *generation,
        }
    }
}

/// What the view shows for the current generation, once the preview thread answered.
#[derive(Clone, Debug)]
pub enum Shown {
    Image { image: Arc<Prepared>, card: Card },
    Card(Card),
}

/// The entry the view follows, as the UI sees it: a change starts the debounce (P3 4.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubjectKey {
    /// The panel's place: its directory, or the archive index and inner directory.
    pub place: Vec<u8>,
    pub name: Vec<u8>,
    pub size: u64,
    pub mtime: (i64, u32),
    pub kind: crate::panel::entry::EKind,
}

/// The quick view's state in `App` (P3 4.1): on or off, the generation of the entry it
/// follows, the debounce deadline, and what the preview thread answered.
#[derive(Debug, Default)]
pub struct QuickView {
    pub on: bool,
    /// Chosen at startup (P3 4.2, 4.3).
    pub protocol: Protocol,
    /// The cell size in pixels: from the probe, read again on resize (P3 4.2).
    pub cell: Option<(u16, u16)>,
    /// Every change of the entry or the pane starts a new generation; answers for an older
    /// one are dropped.
    pub generation: u64,
    pub subject: Option<SubjectKey>,
    /// The image area of the pane at the last draw, in cells.
    pub pane: Option<(u16, u16)>,
    /// The pane the current generation was started for: a resize starts another.
    pub keyed_pane: Option<(u16, u16)>,
    /// The debounce deadline (P3 4.4): a request goes out when it passes.
    pub due: Option<Instant>,
    /// The current generation was requested.
    pub requested: bool,
    /// The preview thread's answer for the current generation.
    pub shown: Option<Shown>,
    /// The image and area the last frame drew, for the graphics layer (P3 4.5).
    pub drawn: Option<(Arc<Prepared>, Rect)>,
    /// The abandoned-thread limit was reached (P3 2.5).
    pub blocked: bool,
    /// Requests sent, for tests and `--log`.
    pub requests: u64,
}

impl QuickView {
    /// The image the next frame may show: the view is on, the answer is an image for the
    /// pane as it is, and no modal overlaps it (`modal`, V-4).
    pub fn image(&self, modal: bool) -> Option<Arc<Prepared>> {
        if !self.on || modal || self.protocol == Protocol::Off {
            return None;
        }
        match &self.shown {
            Some(Shown::Image { image, .. }) if Some(image.pane) == self.pane => {
                Some(image.clone())
            }
            _ => None,
        }
    }
}
