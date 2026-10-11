#![forbid(unsafe_code)]
//! Panel sources other than a local directory (TDR-2 2.4, TDR-3 2.2): a search's results,
//! an archive and a server directory. The panel's `dir` stays local: a results panel's
//! search root, the directory that holds the archive, or the directory the tab showed
//! before it connected. The loads particular to each source are here: a results tab's
//! re-stat (TDR-2 5.5), an archive's open, scan and relist (TDR-3 3.2, 3.3), and a remote
//! listing (TDR-3 5.4, 5.7).

use super::entry::Entry;
use super::listing::Alive;
use super::{Listing, LoadKind, Loading, Opening, Panel, Place, Record};
use crate::archive::{ArchiveIndex, RelistRequest};
use crate::find::{RestatRequest, Search};
use crate::provider::{StatKey, Target, VPath};
use crate::remote::RemoteProvider;
use crate::remote::provider::ListRequest as RemoteListRequest;
use crate::remote::url::RemoteDir;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Where a panel's entries come from (TDR-2 2.4, TDR-3 2.2).
#[derive(Clone, Debug, Default)]
pub enum Source {
    /// A directory (TDR-1).
    #[default]
    Dir,
    /// A search's results below its root, which is the panel's `dir`.
    Results(Arc<Search>),
    /// An archive, browsed as a read-only directory tree (TDR-3 3.3).
    Archive(ArchiveView),
    /// A directory on a server (TDR-3 5.4).
    Remote(RemoteView),
}

/// An archive panel's source (TDR-3 2.2). The panel's `dir` is the directory that holds the
/// archive.
#[derive(Clone)]
pub struct ArchiveView {
    /// The archive's index; complete once its scan ended (TDR-3 3.2).
    pub index: Arc<ArchiveIndex>,
    /// The archive file.
    pub archive: PathBuf,
    /// The index's cache key, which the history place keeps (TDR-3 3.2).
    pub key: StatKey,
    /// The directory shown, below the archive root.
    pub inner: VPath,
}

impl ArchiveView {
    /// The history place of this view: what reopens it, without the index.
    pub fn place(&self) -> Place {
        Place::Archive {
            archive: self.archive.clone(),
            key: self.key,
            inner: self.inner.clone(),
        }
    }
}

impl std::fmt::Debug for ArchiveView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveView")
            .field("archive", &self.archive)
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

/// A remote panel's source (TDR-3 2.2). The panel's `dir` is the local directory the tab
/// showed before it connected.
#[derive(Clone)]
pub struct RemoteView {
    /// The SFTP session, shared with the pool (TDR-3 5.7).
    pub session: Arc<RemoteProvider>,
    /// The server as typed, which the history place keeps to reconnect (TDR-3 5.7).
    pub target: Target,
    /// The absolute directory on the server, or, while `home`, the directory below the
    /// login directory.
    pub dir: VPath,
    /// `dir` is below the login directory, which the server has not resolved yet (TDR-3 5.4).
    pub home: bool,
}

impl RemoteView {
    /// The history place of this view: what reconnects to it, without the session.
    pub fn place(&self) -> Place {
        Place::Remote {
            target: self.target.clone(),
            dir: self.dir.clone(),
            home: self.home,
        }
    }

    /// Where the view is, as a place on the server.
    pub fn remote_dir(&self) -> RemoteDir {
        if self.home {
            RemoteDir::Home(self.dir.clone())
        } else {
            RemoteDir::Absolute(self.dir.clone())
        }
    }

    /// `sftp://[user@]host[:port]/dir` (TDR-3 2.2); `/~/dir` while the login directory is not
    /// resolved.
    pub fn location(&self) -> Vec<u8> {
        if self.home {
            let mut v = self.target.address().into_bytes();
            v.extend_from_slice(b"/~");
            if !self.dir.is_root() {
                v.extend_from_slice(&self.dir.to_bytes());
            }
            return v;
        }
        crate::remote::provider::location(&self.target, &self.dir)
    }

    /// Whether the session ended (a flag, no I/O).
    pub fn lost(&self) -> bool {
        self.session.lost().is_some()
    }
}

impl std::fmt::Debug for RemoteView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteView")
            .field("target", &self.target)
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl Panel {
    /// A local directory panel: not a results tab (TDR-2 2.4), an archive or a server.
    pub fn is_directory(&self) -> bool {
        matches!(self.source, Source::Dir)
    }

    /// A results tab (TDR-2 5.4).
    pub fn is_results(&self) -> bool {
        matches!(self.source, Source::Results(_))
    }

    /// The search of a results panel.
    pub fn search(&self) -> Option<&Arc<Search>> {
        match &self.source {
            Source::Results(s) => Some(s),
            Source::Dir | Source::Archive(_) | Source::Remote(_) => None,
        }
    }

    /// The archive an archive panel shows (TDR-3 2.2).
    pub fn archive(&self) -> Option<&ArchiveView> {
        match &self.source {
            Source::Archive(v) => Some(v),
            _ => None,
        }
    }

    /// The server directory a remote panel shows (TDR-3 2.2).
    pub fn remote(&self) -> Option<&RemoteView> {
        match &self.source {
            Source::Remote(v) => Some(v),
            _ => None,
        }
    }

    /// A results panel whose search is still running: rows are sorted in at most every
    /// 150 ms, and the loop ticks.
    pub fn searching(&self) -> bool {
        self.search().is_some_and(|s| s.running())
    }

    /// A results panel whose search can still add rows: its threads have not recorded
    /// their totals, whether it runs or was cancelled. No re-stat starts then (TDR-2 E-28).
    pub fn search_pending(&self) -> bool {
        self.search().is_some_and(|s| !s.finished())
    }

    /// Opens `archive` and shows its directory `inner` (TDR-3 3.1, 3.3). The panel's local
    /// `dir` becomes the directory that holds the archive; the index arrives with
    /// `Opened`. Returns the load's generation and the scan's cancel flag.
    pub fn navigate_archive(
        &mut self,
        archive: PathBuf,
        inner: VPath,
        cursor_to: Option<Vec<u8>>,
        alive: Alive,
        record: Record,
    ) -> (u64, Arc<AtomicBool>) {
        let dir = archive
            .parent()
            .map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
        let cancel = Arc::new(AtomicBool::new(false));
        let open = Opening {
            archive: Some((archive, inner)),
            cancel: Some(cancel.clone()),
            stop: None,
        };
        self.begin(dir, Source::Dir, cursor_to, alive, record, open);
        (self.generation, cancel)
    }

    /// Shows the directory `inner` of the archive on screen (TDR-3 3.3). `scan`: the cancel
    /// flag of the scan still filling the index, which this load carries on. Returns the
    /// load's generation; `None` when the panel shows no archive.
    pub fn navigate_inner(
        &mut self,
        inner: VPath,
        cursor_to: Option<Vec<u8>>,
        alive: Alive,
        scan: Option<Arc<AtomicBool>>,
        record: Record,
    ) -> Option<u64> {
        let view = self.archive()?.clone();
        let dir = self.dir.clone();
        let open = Opening {
            archive: None,
            cancel: scan,
            stop: None,
        };
        let source = Source::Archive(ArchiveView { inner, ..view });
        self.begin(dir, source, cursor_to, alive, record, open);
        Some(self.generation)
    }

    /// Shows the server directory `view` names (TDR-3 5.4): the panel's local `dir` stays.
    /// Returns the listing request, whose rows arrive one batch per `READDIR` reply.
    pub fn navigate_remote(
        &mut self,
        view: RemoteView,
        cursor_to: Option<Vec<u8>>,
        alive: Alive,
        record: Record,
    ) -> RemoteListRequest {
        let stop = Arc::new(AtomicBool::new(false));
        let open = Opening {
            stop: Some(stop.clone()),
            ..Opening::default()
        };
        let dir = self.dir.clone();
        let remote = view.session.clone();
        let target = view.remote_dir();
        self.begin(dir, Source::Remote(view), cursor_to, alive, record, open);
        RemoteListRequest {
            slot: self.slot,
            generation: self.generation,
            remote,
            dir: target,
            local: self.dir.clone(),
            sort: None,
            cancel: stop,
        }
    }

    /// A refresh of a remote panel (TDR-3 5.4): the rows stay until the new listing, sorted
    /// on the listing thread, is complete. `None` without a remote view, or while its
    /// session is lost: the rows stay as they are (TDR-3 5.7).
    pub fn refresh_remote(&mut self, alive: Alive) -> Option<RemoteListRequest> {
        let view = self.remote()?.clone();
        if view.lost() {
            return None;
        }
        if self.is_loading() {
            // A navigation already lists the directory: restart it.
            let prev = self.loading.as_mut().and_then(|l| l.prev.take());
            let stash = self.loading.as_ref().and_then(|l| l.stash);
            if let Some(l) = &self.loading {
                l.stop_scan(None);
            }
            let stop = Arc::new(AtomicBool::new(false));
            self.generation += 1;
            self.list = Listing::default();
            self.loading = Some(Loading {
                prev,
                stash,
                stop: Some(stop.clone()),
                ..Loading::new(LoadKind::Navigate, alive)
            });
            return Some(RemoteListRequest {
                slot: self.slot,
                generation: self.generation,
                remote: view.session.clone(),
                dir: view.remote_dir(),
                local: self.dir.clone(),
                sort: None,
                cancel: stop,
            });
        }
        if let Some(l) = &self.loading {
            l.stop_scan(None);
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.generation += 1;
        self.loading = Some(Loading {
            stop: Some(stop.clone()),
            ..Loading::new(LoadKind::Refresh, alive)
        });
        Some(RemoteListRequest {
            slot: self.slot,
            generation: self.generation,
            remote: view.session.clone(),
            dir: view.remote_dir(),
            local: self.dir.clone(),
            sort: Some(self.sort),
            cancel: stop,
        })
    }

    /// The server resolved the login directory a remote load lists (TDR-3 5.4).
    pub fn on_located(&mut self, generation: u64, dir: VPath) {
        if generation != self.generation {
            return;
        }
        if let Source::Remote(v) = &mut self.source {
            v.dir = dir;
            v.home = false;
        }
    }

    /// What a remote listing did not show (TDR-3 5.4).
    pub fn on_unshown(&mut self, generation: u64, invalid: u64, capped: bool) {
        if generation == self.generation {
            self.unshown = (invalid, capped);
        }
    }

    /// The scan the load in flight waits for, and its liveness: a navigation inside the
    /// archive carries them on (TDR-3 3.3).
    pub fn scan(&self) -> Option<(Alive, Arc<AtomicBool>)> {
        let l = self.loading.as_ref()?;
        Some((l.alive.clone(), l.cancel.clone()?))
    }

    /// Whether the load in flight opens or scans an archive.
    pub fn archive_loading(&self) -> bool {
        self.loading
            .as_ref()
            .is_some_and(|l| l.archive.is_some() || l.cancel.is_some())
    }

    /// The archive the panel opens or shows, and the directory in it.
    pub fn archive_place(&self) -> Option<(&Path, &VPath)> {
        if let Some((a, i)) = self.loading.as_ref().and_then(|l| l.archive.as_ref()) {
            return Some((a, i));
        }
        self.archive().map(|v| (v.archive.as_path(), &v.inner))
    }

    /// What the title shows (TDR-3 2.2): `archive.zip:/inner/dir` in an archive,
    /// `sftp://[user@]host[:port]/dir` on a server, else the directory.
    pub fn location(&self) -> Vec<u8> {
        if let Some(v) = self.remote() {
            return v.location();
        }
        match self.archive_place() {
            Some((a, i)) => crate::archive::title(a, i),
            None => self.dir.as_os_str().as_bytes().to_vec(),
        }
    }

    /// What a blocked load counts as in the abandoned-thread limit (TDR-1 3.1): the archive a
    /// scan reads, the server directory a remote listing reads, else the directory.
    pub fn blocked_path(&self) -> PathBuf {
        if let Some(v) = self.remote() {
            return PathBuf::from(OsString::from_vec(v.location()));
        }
        match self.archive_place() {
            Some((a, _)) if self.archive_loading() => a.to_path_buf(),
            _ => self.dir.clone(),
        }
    }

    /// The index of an archive load arrived (TDR-3 3.3): the panel shows the archive now,
    /// while the scan fills it.
    pub fn on_opened(&mut self, generation: u64, index: Arc<ArchiveIndex>) {
        if generation != self.generation {
            return;
        }
        let Some(l) = self.loading.as_mut() else {
            return;
        };
        let Some((archive, inner)) = l.archive.take() else {
            return;
        };
        // The requested path's directory: another path to the same inode shares the index.
        if let Some(d) = archive.parent() {
            self.dir = d.to_path_buf();
        }
        self.source = Source::Archive(ArchiveView {
            key: index.key,
            index,
            archive,
            inner,
        });
    }

    /// A later duplicate replaced a row of the directory the scan shows (TDR-3 3.3): the rows
    /// go, and the scan sends them all again.
    pub fn on_reset(&mut self, generation: u64) {
        if generation != self.generation || !self.is_loading() {
            return;
        }
        self.remember_cursor();
        self.list = Listing::default();
        self.marked = 0;
        self.marked_bytes = 0;
    }

    /// A refresh of an archive panel (TDR-3 3.2): its directory again from the complete
    /// index, sorted on the listing thread; `check` compares the archive's `StatKey`
    /// first. `None` while the scan runs or when the panel shows no archive.
    pub fn refresh_archive(
        &mut self,
        alive: Alive,
        check: crate::archive::Check,
    ) -> Option<RelistRequest> {
        let view = self.archive()?.clone();
        if !view.index.is_complete() || self.archive_loading() {
            return None;
        }
        self.generation += 1;
        self.loading = Some(Loading::new(LoadKind::Refresh, alive));
        Some(RelistRequest {
            slot: self.slot,
            generation: self.generation,
            dir: self.dir.clone(),
            index: view.index,
            inner: view.inner,
            sort: Some(self.sort),
            check,
        })
    }

    /// A results panel's refresh: a re-stat of its entries on a listing thread (TDR-2 5.5).
    /// The rows stay until it completes; marks survive by name. Results that arrive
    /// meanwhile are kept too (TDR-2 E-28).
    pub fn restat(&mut self, alive: Alive) -> RestatRequest {
        self.generation += 1;
        self.loading = Some(Loading {
            copied: Some(self.list.entries.len()),
            ..Loading::new(LoadKind::Refresh, alive)
        });
        RestatRequest {
            slot: self.slot,
            generation: self.generation,
            root: self.dir.clone(),
            entries: self.list.entries.clone(),
            names: self.list.names.clone(),
            sort: self.sort,
        }
    }

    /// Results of the panel's running search (TDR-2 5.3); they are sorted in like listing
    /// batches.
    pub fn append_results(&mut self, entries: Vec<Entry>, names: &[u8]) {
        self.list.append(entries, names);
    }
}
