#![forbid(unsafe_code)]
//! Archive panels (P3 2.2, 2.4, 3.1, 3.3): opening an archive by name (`Enter`) or by magic
//! (`Alt+O`), navigation inside it, `..` at its root, history places, `Space` from the
//! index, and refreshes that compare the archive's `StatKey` (P3 3.2). Like the rest of
//! `App` it makes no filesystem syscall (P-1): opening, scanning, listing and the key check
//! run on listing threads, and the UI only reads the index in memory.

use super::event::Effect;
use super::{App, MAX_ABANDONED, TOO_MANY_BLOCKED};
use crate::archive::detect::{Format, Want};
use crate::archive::index::Limits;
use crate::archive::{
    self as arch, Check, OpenRequest, RelistRequest, SizeRequest, Watch, enter_target,
};
use crate::panel::entry::EKind;
use crate::panel::listing::Alive;
use crate::panel::{Place, Record, Row, join_lexical};
use crate::provider::VPath;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

impl App {
    /// Opens `archive` in the active tab of `side` and shows `inner` (P3 3.1): through the
    /// index cache, else a scan, subject to the stuck-load limits (M1 3.1, P3 2.5).
    pub(super) fn open_archive(
        &mut self,
        side: usize,
        archive: PathBuf,
        want: Want,
        inner: VPath,
        cursor_to: Option<Vec<u8>>,
        record: Record,
    ) -> Vec<Effect> {
        self.abandoned.retain(|(_, a)| a.is_running());
        if self.abandoned.iter().any(|(d, _)| *d == archive) {
            self.warn(format!(
                "{}: the previous read of this archive is still blocked",
                archive.display()
            ));
            return Vec::new();
        }
        if self.abandoned.len() >= MAX_ABANDONED {
            self.warn(TOO_MANY_BLOCKED);
            return Vec::new();
        }
        let p = self.sides[side].panel_mut();
        if let Some(l) = &p.loading
            && l.alive.is_running()
            && p.is_loading()
        {
            self.abandoned.push((p.blocked_path(), l.alive.clone()));
        }
        if side == self.active {
            self.filter_line = None;
        }
        self.leave_results(side);
        let p = self.sides[side].panel_mut();
        let slot = p.slot;
        let alive = Alive::running();
        let (generation, cancel) = p.navigate_archive(
            archive.clone(),
            inner.clone(),
            cursor_to,
            alive.clone(),
            record,
        );
        // Opening an archive is not a visit of its directory (P2 3.3).
        self.mark_visit(slot, false);
        vec![Effect::OpenArchive(
            OpenRequest {
                slot,
                generation,
                archive,
                want,
                inner,
                cancel,
                tz: self.tz.clone(),
                limits: Limits::default(),
            },
            alive,
        )]
    }

    /// Shows the directory `inner` of the archive on screen (P3 3.3): from memory on a
    /// listing thread once the index is complete; during the scan the scan itself sends
    /// what it knows and appends the rest, and the load keeps the scan's cancel flag.
    pub(super) fn archive_inner(
        &mut self,
        side: usize,
        inner: VPath,
        cursor_to: Option<Vec<u8>>,
        record: Record,
    ) -> Vec<Effect> {
        let Some(view) = self.sides[side].panel().archive().cloned() else {
            return Vec::new();
        };
        if side == self.active {
            self.filter_line = None;
        }
        let p = self.sides[side].panel_mut();
        let slot = p.slot;
        let scanning = !view.index.is_complete();
        let (alive, scan) = match p.scan() {
            Some((alive, cancel)) if scanning => (alive, Some(cancel)),
            _ => {
                if let Some(l) = &p.loading
                    && l.alive.is_running()
                    && p.is_loading()
                {
                    self.abandoned.push((p.blocked_path(), l.alive.clone()));
                }
                (Alive::running(), None)
            }
        };
        let p = self.sides[side].panel_mut();
        let Some(generation) =
            p.navigate_inner(inner.clone(), cursor_to, alive.clone(), scan, record)
        else {
            return Vec::new();
        };
        if scanning
            && view.index.watch(Watch {
                slot,
                generation,
                inner: inner.clone(),
            })
        {
            return Vec::new();
        }
        let dir = self.sides[side].panel().dir.clone();
        vec![Effect::Relist(
            RelistRequest {
                slot,
                generation,
                dir,
                index: view.index,
                inner,
                sort: None,
                check: Check::No,
            },
            alive,
        )]
    }

    /// `Enter` in an archive panel (P3 2.4): `..` goes up, a directory (or a symlink to one
    /// inside the index) opens; a member with an archive name is not opened as an archive
    /// (P3 3.1); viewing a member arrives with T3.
    pub(super) fn archive_enter(&mut self) -> Vec<Effect> {
        let side = self.active;
        let p = self.panel();
        let Some(view) = p.archive().cloned() else {
            return Vec::new();
        };
        let (i, e) = match p.current() {
            Some(Row::Parent) => return self.archive_parent(),
            Some(Row::Entry(i)) => (i, p.list.entries[i as usize]),
            None => return Vec::new(),
        };
        let name = p.list.name(i).to_vec();
        if matches!(e.kind, EKind::Dir | EKind::Symlink) {
            if !view.index.is_complete() && e.kind == EKind::Dir {
                let Ok(inner) = view.inner.join(OsStr::from_bytes(&name)) else {
                    return Vec::new();
                };
                return self.archive_inner(side, inner, None, Record::New);
            }
            if let Some(target) = enter_target(&view.index, &view.inner, &name, e.kind) {
                return self.archive_inner(side, target, None, Record::New);
            }
            if e.kind == EKind::Symlink && !view.index.is_complete() {
                self.warn(arch::STILL_READING);
                return Vec::new();
            }
        }
        if Format::by_name(&name).is_some() {
            self.warn(arch::NESTED);
        } else {
            self.warn(super::jobs::NOT_YET);
        }
        Vec::new()
    }

    /// `..`, `Backspace` and `Alt+Up` in an archive panel (P3 2.2): the parent directory in
    /// the archive, and at its root the directory that holds the archive, with the cursor on
    /// the archive.
    pub(super) fn archive_parent(&mut self) -> Vec<Effect> {
        let side = self.active;
        let Some(view) = self.panel().archive().cloned() else {
            return Vec::new();
        };
        match view.inner.parent() {
            Some(up) => {
                let name = view.inner.name().map(|n| n.as_bytes().to_vec());
                self.archive_inner(side, up, name, Record::New)
            }
            None => {
                let name = view.archive.file_name().map(|n| n.as_bytes().to_vec());
                let dir = self.panel().dir.clone();
                self.load(side, dir, name, false)
            }
        }
    }

    /// `Alt+O` (P3 3.1, 6): the file under the cursor as an archive; a name that promises a
    /// format opens as that format, any other by its magic.
    pub(super) fn open_as_archive(&mut self) -> Vec<Effect> {
        let p = self.panel();
        let Some((i, e)) = p.current_entry() else {
            return Vec::new();
        };
        if e.kind == EKind::Dir {
            return Vec::new();
        }
        let name = p.list.name(i).to_vec();
        let path = p.path_of(&name);
        let leaf = path
            .file_name()
            .map(|n| n.as_bytes().to_vec())
            .unwrap_or_default();
        let side = self.active;
        self.open_archive(
            side,
            path,
            Want::of_name(&leaf),
            VPath::root(),
            None,
            Record::New,
        )
    }

    /// `Enter` on a file of a directory panel whose name promises an archive (P3 3.1, the
    /// M1 6 amendment of P3 1.4); `None` for any other file (M1: `xdg-open`).
    pub(super) fn enter_archive_by_name(&mut self, name: &[u8]) -> Option<Vec<Effect>> {
        let format = Format::by_name(name)?;
        let path = self.panel().path_of(name);
        let side = self.active;
        Some(self.open_archive(
            side,
            path,
            Want::Name(format),
            VPath::root(),
            None,
            Record::New,
        ))
    }

    /// A history place of an archive (P3 2.2): reopened through the index cache; a changed
    /// archive is read again.
    pub(super) fn open_place(
        &mut self,
        side: usize,
        place: Place,
        cursor_to: Option<Vec<u8>>,
        record: Record,
    ) -> Vec<Effect> {
        let Place::Archive { archive, inner, .. } = place else {
            return Vec::new();
        };
        let leaf = archive
            .file_name()
            .map(|n| n.as_bytes().to_vec())
            .unwrap_or_default();
        self.open_archive(
            side,
            archive,
            Want::of_name(&leaf),
            inner,
            cursor_to,
            record,
        )
    }

    /// A refresh of an archive panel (P3 3.2): the directory again from the index, after a
    /// comparison of the archive's `StatKey`. Nothing while the scan runs.
    pub(super) fn refresh_archive(&mut self, side: usize, check: Check) -> Vec<Effect> {
        let p = self.sides[side].panel_mut();
        if let Some(l) = &p.loading
            && l.alive.is_running()
            && !p.archive_loading()
        {
            let stuck = (p.blocked_path(), l.alive.clone());
            self.abandoned.retain(|(_, a)| a.is_running());
            if self.abandoned.len() >= MAX_ABANDONED {
                self.warn(TOO_MANY_BLOCKED);
                return Vec::new();
            }
            self.abandoned.push(stuck);
        }
        let p = self.sides[side].panel_mut();
        let alive = Alive::running();
        match p.refresh_archive(alive.clone(), check) {
            Some(req) => vec![Effect::Relist(req, alive)],
            None => Vec::new(),
        }
    }

    /// A refresh saw another `StatKey` for the archive's name (P3 3.2). A refresh keeps the
    /// view on the indexed inode and says so; `Ctrl+R` reads the archive again, in place.
    pub(super) fn on_archive_changed(
        &mut self,
        slot: usize,
        generation: u64,
        rescan: bool,
    ) -> Vec<Effect> {
        let Some((s, t)) = self.find_slot(slot) else {
            return Vec::new();
        };
        let p = &mut self.sides[s].tabs[t];
        if p.generation != generation {
            return Vec::new();
        }
        let Some(view) = p.archive().cloned() else {
            return Vec::new();
        };
        if !rescan {
            p.message = Some(arch::CHANGED.into());
            return Vec::new();
        }
        // The pending refresh is replaced by the new read.
        if t != self.sides[s].active {
            return Vec::new();
        }
        let cursor = p.current_name().map(<[u8]>::to_vec);
        let leaf = view
            .archive
            .file_name()
            .map(|n| n.as_bytes().to_vec())
            .unwrap_or_default();
        let fx = self.open_archive(
            s,
            view.archive.clone(),
            Want::of_name(&leaf),
            view.inner.clone(),
            cursor,
            Record::No,
        );
        if !fx.is_empty() {
            self.say(arch::REREADING);
        }
        fx
    }

    /// `Space` on a directory of an archive (P3 2.4): its size from the index.
    pub(super) fn archive_size(&mut self) -> Vec<Effect> {
        let p = self.panel();
        let Some(view) = p.archive().cloned() else {
            return Vec::new();
        };
        let (slot, generation) = (p.slot, p.generation);
        let name = match p.current_entry() {
            Some((i, e)) if e.kind == EKind::Dir => OsStr::from_bytes(p.list.name(i)).to_owned(),
            _ => {
                self.panel_mut().toggle_mark(false);
                return Vec::new();
            }
        };
        self.panel_mut().toggle_mark(false);
        if !view.index.is_complete() {
            self.warn(arch::STILL_READING);
            return Vec::new();
        }
        vec![Effect::ArchiveSize(SizeRequest {
            slot,
            generation,
            index: view.index,
            inner: view.inner,
            name,
        })]
    }

    /// `cd` in an archive panel (the P3 1.4 amendment of M1 6): a relative path moves
    /// inside the archive; `..` above its root continues in the directory that holds the
    /// archive; an absolute path leaves it.
    pub(super) fn archive_cd(&mut self, path: &Path) -> Option<Vec<Effect>> {
        let view = self.panel().archive().cloned()?;
        if path.is_absolute() {
            return None;
        }
        let side = self.active;
        let mut inner = view.inner.clone();
        let mut comps = path.components();
        while let Some(c) = comps.next() {
            match c {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => match inner.parent() {
                    Some(up) => inner = up,
                    None => {
                        let rest = comps.as_path();
                        let dir = join_lexical(&self.panel().dir, rest);
                        return Some(self.load(side, dir, None, false));
                    }
                },
                std::path::Component::Normal(n) => match inner.join(n) {
                    Ok(p) => inner = p,
                    Err(_) => return Some(Vec::new()),
                },
                _ => return None,
            }
        }
        Some(self.archive_inner(side, inner, None, Record::New))
    }
}
