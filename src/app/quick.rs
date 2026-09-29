#![forbid(unsafe_code)]
//! The quick view (P3 4.1): `Ctrl+Q` turns the inactive side into a view of the active
//! panel's cursor entry, and `Ctrl+Q` again turns it back. `Tab` swaps sides as always, so
//! the view stays on the inactive side; the hidden panel keeps its directory, listing and
//! watch, and verbs still use it as the other panel.
//!
//! Every change of the entry under the cursor, or of the pane, starts a generation and the
//! debounce (P3 4.4): the request goes out when the cursor has rested for 100 ms, as an
//! event-loop deadline ([`App::quick_due`]), so rapid navigation never starts a decode. A
//! remote file or a member of a compressed tar is previewed only on `Alt+Q` (V-5). Until
//! the preview thread answers, the pane shows the card from the listing's row. Like the rest
//! of `App` this makes no filesystem syscall (P-1).

use super::event::Effect;
use super::{App, MAX_ABANDONED};
use crate::archive as arch;
use crate::panel::entry::EKind;
use crate::panel::listing::Alive;
use crate::preview::card::Card;
use crate::preview::{
    BLOCKED, DEBOUNCE, MEMBER_ON_KEY, Msg, Pane, Protocol, REMOTE_ON_KEY, Request, Shown, Subject,
    SubjectKey,
};
use crate::provider::Provider;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

impl App {
    /// `Ctrl+Q` (P3 4.1): the view on or off. Answers for the old view are dropped.
    pub(super) fn toggle_quick(&mut self) -> Vec<Effect> {
        let q = &mut self.quick;
        q.on = !q.on;
        q.generation += 1;
        q.subject = None;
        q.keyed_pane = None;
        q.shown = None;
        q.due = None;
        q.requested = false;
        q.drawn = None;
        q.blocked = false;
        q.pane = None;
        Vec::new()
    }

    /// `Alt+Q` (P3 6): previews the entry under the cursor now, also a remote file or a
    /// compressed-tar member (V-5); nothing while the view is off.
    pub(super) fn quick_load(&mut self) -> Vec<Effect> {
        if !self.quick.on {
            return Vec::new();
        }
        self.quick_sync();
        if self.panel().remote().is_some() {
            // Remote previews come with the remote panel (T6).
            self.warn(super::jobs::NOT_YET);
            return Vec::new();
        }
        match self.quick_request(true) {
            Some(r) => vec![Effect::Preview(r)],
            None => Vec::new(),
        }
    }

    /// What the view follows: the active panel's cursor entry.
    fn quick_key(&self) -> Option<SubjectKey> {
        let p = self.panel();
        let (i, e) = p.current_entry()?;
        let mut place = Vec::new();
        match p.archive() {
            Some(v) => {
                place.extend_from_slice(format!("{}:", v.index.id()).as_bytes());
                place.extend_from_slice(&arch::title(&v.archive, &v.inner));
            }
            None if p.remote().is_some() => place.extend_from_slice(b"sftp:"),
            None => place.extend_from_slice(p.dir.as_os_str().as_bytes()),
        }
        Some(SubjectKey {
            place,
            name: p.list.name(i).to_vec(),
            size: e.size,
            mtime: (e.mtime, e.mtime_ns),
            kind: e.kind,
        })
    }

    /// Whether cursor rest previews the entry (V-5): local entries, and regular-file members
    /// of random-access archives (zip, 7z, plain tar).
    fn quick_on_rest(&self) -> bool {
        let p = self.panel();
        let Some((_, e)) = p.current_entry() else {
            return false;
        };
        if p.remote().is_some() {
            return false;
        }
        match p.archive() {
            Some(v) => e.kind == EKind::File && v.index.caps().random_access,
            None => true,
        }
    }

    /// Starts a new generation when the entry or the pane changed (P3 4.4, step 1). Runs at
    /// the end of every update and after every frame.
    pub fn quick_sync(&mut self) {
        if !self.quick.on {
            return;
        }
        let key = self.quick_key();
        let pane = self.quick.pane;
        if key == self.quick.subject && pane == self.quick.keyed_pane {
            return;
        }
        let rest = self.quick_on_rest();
        let q = &mut self.quick;
        q.generation += 1;
        q.subject = key;
        q.keyed_pane = pane;
        q.shown = None;
        q.requested = false;
        q.blocked = false;
        q.due = (rest && q.subject.is_some() && pane.is_some()).then(|| Instant::now() + DEBOUNCE);
    }

    /// The debounce deadline the event loop waits for (P-5: only while one is pending).
    pub fn quick_due(&self) -> Option<Instant> {
        if self.quick.on && !self.quick.requested {
            self.quick.due
        } else {
            None
        }
    }

    /// A tick: the request goes out once the cursor has rested (P3 4.4, step 2).
    pub(super) fn quick_tick(&mut self, now: Instant) -> Vec<Effect> {
        match self.quick_due() {
            Some(d) if d <= now => {}
            _ => return Vec::new(),
        }
        self.quick.due = None;
        match self.quick_request(false) {
            Some(r) => vec![Effect::Preview(r)],
            None => Vec::new(),
        }
    }

    /// The request for the current generation; `explicit` is `Alt+Q`.
    fn quick_request(&mut self, explicit: bool) -> Option<Request> {
        let (cols, rows) = self.quick.pane?;
        let p = self.panel();
        let (i, e) = p.current_entry()?;
        let name = p.list.name(i).to_vec();
        let subject = match p.archive() {
            Some(v) => {
                if e.kind != EKind::File || !(explicit || v.index.caps().random_access) {
                    return None;
                }
                let (path, size) = arch::view_target(&v.index, &v.inner, &name).ok()?;
                let place: Arc<dyn Provider> = v.index.clone();
                Subject::Place {
                    place,
                    place_id: v.index.id(),
                    path,
                    name,
                    size,
                    mtime: e.mtime,
                    perm: e.perm as u32,
                }
            }
            None if p.remote().is_some() => return None,
            None => {
                let path = p.path_of(&name);
                let dir = path.parent()?.to_path_buf();
                let leaf = path.file_name()?.to_owned();
                Subject::Local { dir, name: leaf }
            }
        };
        self.quick.requested = true;
        self.quick.requests += 1;
        Some(Request {
            generation: self.quick.generation,
            subject,
            pane: Pane {
                cols,
                rows,
                cell: self.quick.cell,
            },
            protocol: self.quick.protocol,
        })
    }

    /// An answer of the preview thread: kept when it is for the current generation.
    pub(super) fn on_preview(&mut self, m: Msg) -> Vec<Effect> {
        if !self.quick.on || m.generation() != self.quick.generation {
            return Vec::new();
        }
        self.quick.blocked = false;
        self.quick.shown = Some(match m {
            Msg::Ready { image, card, .. } => Shown::Image { image, card },
            Msg::Card { card, .. } => Shown::Card(card),
        });
        Vec::new()
    }

    /// The card the pane shows when it shows no image: the preview thread's, else one from
    /// the listing's row with the reason the entry is not read (P3 4.6).
    pub fn quick_card(&self) -> Card {
        let p = self.panel();
        let mut c = match (&self.quick.shown, p.current_entry()) {
            (Some(Shown::Image { card, .. } | Shown::Card(card)), _) => card.clone(),
            (None, None) => Card {
                name: b"..".to_vec(),
                kind: "directory",
                ..Card::default()
            },
            (None, Some((i, e))) => {
                let name = p.list.name(i);
                let mut c = Card::of_entry(name, e);
                if p.remote().is_some() && e.kind == EKind::File {
                    c.reason = Some(REMOTE_ON_KEY.into());
                } else if let Some(v) = p.archive()
                    && e.kind == EKind::File
                {
                    if let Err(why) = arch::view_target(&v.index, &v.inner, name) {
                        c.reason = Some(why.into());
                    } else if !v.index.caps().random_access {
                        c.reason = Some(MEMBER_ON_KEY.into());
                    }
                }
                c
            }
        };
        if self.quick.blocked {
            c.reason = Some(BLOCKED.into());
        }
        c
    }

    /// Whether the abandoned-thread limit is reached (M1 3.1, P3 2.5).
    pub fn at_abandon_cap(&mut self) -> bool {
        self.abandoned.retain(|(_, a)| a.is_running());
        self.abandoned.len() >= MAX_ABANDONED
    }

    /// Abandoned threads that still run (M1 3.1, P3 2.5).
    pub fn abandoned_threads(&mut self) -> usize {
        self.abandoned.retain(|(_, a)| a.is_running());
        self.abandoned.len()
    }

    /// The runtime abandoned a preview thread blocked on `path` (P3 2.5): it counts toward
    /// `MAX_ABANDONED` while it runs.
    pub fn preview_abandoned(&mut self, path: PathBuf, alive: Alive) {
        if alive.is_running() {
            self.abandoned.push((path, alive));
        }
    }

    /// At the cap no new preview thread starts: the view says so (P3 2.5).
    pub fn preview_blocked(&mut self) {
        self.quick.blocked = true;
    }

    /// The protocol and cell size of the session (P3 4.2, 4.3).
    pub fn set_graphics(&mut self, protocol: Protocol, cell: Option<(u16, u16)>) {
        self.quick.protocol = protocol;
        self.quick.cell = cell;
    }
}
