#![forbid(unsafe_code)]
//! Remote panels (P3 2.2, 2.4, 5.1, 5.4, 5.5, 5.7): `cd sftp://...` opens a server
//! directory in the active tab, through the open session for its target or through a
//! connect in the terminal hand-off (P3 5.2). The address grammar and `sftp.ssh` are checked
//! here, before anything is spawned.
//!
//! Inside a remote panel, `Enter` on a directory, `..`, `Backspace` and a relative `cd`
//! navigate on the server; `..` at `/` returns the panel to its local directory (P3 2.2).
//! F3, F4 and `Enter` on a file view a copy in the runtime view directory (P3 5.5); F5
//! downloads into the other panel's directory; `Space` sizes a directory by a walk on the
//! server; `Alt+Q` previews a file (V-5). History places, hidden tabs and bookmarks reopen
//! a place by its target: the pool's session when it is open, else a new connect (P3 5.7).
//!
//! Phase 3b (P3 5.6): F5 and F6 into a remote panel upload; F6 out of one downloads and
//! keeps the remote sources (R-4); F6 between two panels on one session renames on the
//! server; F7, Shift+F6 and Shift+F8 act on the server; F8 is refused (R-5). The confirm
//! dialog of every move across hosts says "best-effort" before the job starts (R-4). After
//! the editor exits, an edited F4 copy of a remote file raises the write-back question:
//! upload it (replacing through R-2), save it under `name (1)` when the server's file
//! changed since the download, or keep the local copy and say where it is.
//!
//! A lost session keeps the panel's rows and says "connection lost -- Ctrl+R reconnects";
//! the verbs that need the server are refused until `Ctrl+R` reconnects. manycommander
//! never reconnects on its own. Like the rest of `App` this makes no I/O: the listing
//! threads and the job worker call the session, the UI thread only reads its lost flag.

use super::event::Effect;
use super::{App, MAX_ABANDONED, TOO_MANY_BLOCKED, count_text};
use crate::fsops::group::{Group, Root};
use crate::fsops::job::{Dest, JobSpec};
use crate::fsops::question::suggest_rename;
use crate::panel::entry::{EKind, LinkKind};
use crate::panel::listing::Alive;
use crate::panel::{Place, Record, RemoteView, Row, join_lexical};
use crate::provider::Target;
use crate::provider::VPath;
use crate::remote::provider::{LOST_PANEL, NOT_A_FILE};
use crate::remote::transport::SshCommand;
use crate::remote::tree::{REMOTE_KEPT, SizeRequest};
use crate::remote::url::{self, Address, RemoteDir};
use crate::remote::{RemoteMsg, RemoteProvider};
use crate::ui::dialog::{Dialog, Purpose, size_phrase};
use crate::ui::text::escaped;
use crate::viewtemp::{ASK_ABOVE, kept_remote_text};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// A connect in flight (P3 5.2): what opens when the hand-off returns a session.
#[derive(Clone, Debug)]
pub struct PendingConnect {
    /// The tab that asked.
    pub slot: usize,
    pub target: Target,
    pub dir: RemoteDir,
    pub cursor: Option<Vec<u8>>,
    pub record: Record,
}

impl App {
    /// `cd sftp://...` (P3 5.1).
    pub(super) fn connect(&mut self, text: &[u8]) -> Vec<Effect> {
        let addr = match url::parse(text) {
            Ok(a) => a,
            Err(e) => {
                self.warn(e);
                return Vec::new();
            }
        };
        let side = self.active;
        self.open_remote(side, addr.target, addr.dir, None, Record::New)
    }

    /// Opens a server place in the active tab of `side` (P3 5.1, 5.7): through the open
    /// session for `target`, else through a connect in the terminal hand-off. Before a
    /// connect the pool makes room, and the target and `sftp.ssh` are checked, so nothing is
    /// spawned for an address or a setting that is refused.
    pub(super) fn open_remote(
        &mut self,
        side: usize,
        target: Target,
        dir: RemoteDir,
        cursor_to: Option<Vec<u8>>,
        record: Record,
    ) -> Vec<Effect> {
        if let Some(remote) = self.pool.get(&target) {
            return self.show_remote(side, remote, dir, cursor_to, record);
        }
        if let Err(e) = url::check_target(&target) {
            self.warn(e);
            return Vec::new();
        }
        let cmd = match SshCommand::from_setting(self.config.sftp.ssh.as_deref()) {
            Ok(c) => c,
            Err(e) => {
                self.warn(e);
                return Vec::new();
            }
        };
        if let Err(why) = self.pool.make_room() {
            self.warn(why);
            return Vec::new();
        }
        self.pending = Some(PendingConnect {
            slot: self.sides[side].panel().slot,
            target: target.clone(),
            dir: dir.clone(),
            cursor: cursor_to,
            record,
        });
        vec![Effect::Connect(Address { target, dir }, cmd)]
    }

    /// Lists `dir` on `remote` in the active tab of `side` (P3 5.4), subject to the
    /// stuck-load limits (M1 3.1). The panel keeps its local directory.
    pub(super) fn show_remote(
        &mut self,
        side: usize,
        remote: Arc<RemoteProvider>,
        dir: RemoteDir,
        cursor_to: Option<Vec<u8>>,
        record: Record,
    ) -> Vec<Effect> {
        self.abandoned.retain(|(_, a)| a.is_running());
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
        let (dir, home) = match dir {
            RemoteDir::Home(d) => (d, true),
            RemoteDir::Absolute(d) => (d, false),
        };
        let view = RemoteView {
            target: remote.target().clone(),
            session: remote,
            dir,
            home,
        };
        let alive = Alive::running();
        let p = self.sides[side].panel_mut();
        let req = p.navigate_remote(view, cursor_to, alive.clone(), record);
        // Frecency records local directories only (P3 5.1).
        self.mark_visit(req.slot, false);
        vec![Effect::ListRemote(req, alive)]
    }

    /// Whether the active panel shows a server whose session was lost; says so.
    fn remote_lost(&mut self) -> bool {
        let lost = self.panel().remote().is_some_and(RemoteView::lost);
        if lost {
            self.warn(LOST_PANEL);
        }
        lost
    }

    /// `Enter` in a remote panel (P3 2.4): `..` goes up, a directory or a symlink that may
    /// lead to one opens on the server, a file is viewed (F3).
    pub(super) fn remote_enter(&mut self) -> Vec<Effect> {
        let side = self.active;
        let p = self.panel();
        let Some(v) = p.remote().cloned() else {
            return Vec::new();
        };
        let (i, e) = match p.current() {
            Some(Row::Parent) => return self.remote_parent(),
            Some(Row::Entry(i)) => (i, p.list.entries[i as usize]),
            None => return Vec::new(),
        };
        let name = p.list.name(i).to_vec();
        let dir_like = e.kind == EKind::Dir
            || (e.kind == EKind::Symlink && e.link != LinkKind::File && e.link != LinkKind::Broken);
        if dir_like {
            if v.home || self.remote_lost() {
                return Vec::new();
            }
            let Ok(dir) = v.dir.join(OsStr::from_bytes(&name)) else {
                return Vec::new();
            };
            return self.show_remote(side, v.session, RemoteDir::Absolute(dir), None, Record::New);
        }
        self.remote_view(false)
    }

    /// `..`, `Backspace` and `Alt+Up` in a remote panel (P3 2.2): the parent directory on
    /// the server, and at `/` the panel's local directory.
    pub(super) fn remote_parent(&mut self) -> Vec<Effect> {
        let side = self.active;
        let Some(v) = self.panel().remote().cloned() else {
            return Vec::new();
        };
        if v.home {
            return Vec::new();
        }
        match v.dir.parent() {
            Some(up) => {
                if self.remote_lost() {
                    return Vec::new();
                }
                let name = v.dir.name().map(|n| n.as_bytes().to_vec());
                self.show_remote(side, v.session, RemoteDir::Absolute(up), name, Record::New)
            }
            None => {
                let dir = self.panel().dir.clone();
                self.load(side, dir, None, false)
            }
        }
    }

    /// `cd` in a remote panel (the P3 1.4 amendment of M1 6, P3 5.1): a relative path moves
    /// on the server; `..` above `/` continues in the panel's local directory. `None`: the
    /// path is local (it starts with `/`, `~` or `$`, or is empty).
    pub(super) fn remote_cd(&mut self, raw: &[u8], path: &Path) -> Option<Vec<Effect>> {
        let v = self.panel().remote().cloned()?;
        if raw.is_empty() || matches!(raw[0], b'/' | b'~' | b'$') || path.is_absolute() {
            return None;
        }
        let side = self.active;
        if v.home {
            return Some(Vec::new());
        }
        let mut dir = v.dir.clone();
        let mut comps = path.components();
        while let Some(c) = comps.next() {
            match c {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => match dir.parent() {
                    Some(up) => dir = up,
                    None => {
                        let rest = comps.as_path();
                        let local = join_lexical(&self.panel().dir, rest);
                        return Some(self.load(side, local, None, false));
                    }
                },
                std::path::Component::Normal(n) => match dir.join(n) {
                    Ok(p) => dir = p,
                    Err(_) => return Some(Vec::new()),
                },
                _ => return None,
            }
        }
        if self.remote_lost() {
            return Some(Vec::new());
        }
        Some(self.show_remote(side, v.session, RemoteDir::Absolute(dir), None, Record::New))
    }

    /// F3, F4 and `Enter` on a remote file (P3 5.5): a copy in the runtime view directory,
    /// made on a listing thread, then the pager or the editor. A file above 256 MB asks
    /// first. Only a regular file is read (R-3).
    pub(super) fn remote_view(&mut self, edit: bool) -> Vec<Effect> {
        let p = self.panel();
        let Some(v) = p.remote().cloned() else {
            return Vec::new();
        };
        let Some((i, e)) = p.current_entry() else {
            return Vec::new();
        };
        if e.kind == EKind::Dir {
            return Vec::new();
        }
        let (kind, size) = (e.kind, e.size);
        let name = p.list.name(i).to_vec();
        if kind != EKind::File {
            self.warn(NOT_A_FILE);
            return Vec::new();
        }
        if self.remote_lost() {
            return Vec::new();
        }
        let Ok(path) = v.dir.join(OsStr::from_bytes(&name)) else {
            return Vec::new();
        };
        if size > ASK_ABOVE {
            self.dialog = Some(Dialog::confirm(
                if edit { "Edit" } else { "View" },
                vec![
                    format!(
                        "\"{}\" is {}.",
                        crate::ui::text::escaped(&name),
                        size_phrase(size)
                    ),
                    "Download it into the view directory first?".into(),
                ],
                "Download",
                Purpose::ViewLarge {
                    edit,
                    name,
                    path,
                    size,
                },
            ));
            return Vec::new();
        }
        self.start_view(edit, name, path, size)
    }

    /// The active remote panel's selection as a job group (P3 2.2), after the checks every
    /// verb on the server makes: a resolved login directory and a session that is not lost.
    fn remote_selection(&mut self) -> Option<(RemoteView, Vec<Group>)> {
        let p = self.panel();
        let v = p.remote().cloned()?;
        let names = p.selection();
        if names.is_empty() || v.home || self.remote_lost() {
            return None;
        }
        let groups = vec![Group {
            root: Root::Remote(v.session.clone()),
            sub: v.dir.components().to_vec(),
            names,
        }];
        Some((v, groups))
    }

    /// The other panel's server directory for an upload or a rename on the server, when
    /// it is usable.
    fn other_remote(&mut self) -> Option<RemoteView> {
        let v = self.other().remote().cloned()?;
        if v.home {
            return None;
        }
        if v.lost() {
            self.warn(LOST_PANEL);
            return None;
        }
        Some(v)
    }

    fn links_text(&self) -> Option<String> {
        let links = self
            .panel()
            .selection_kinds()
            .iter()
            .filter(|k| **k == EKind::Symlink)
            .count();
        (links > 0).then(|| format!("{links} symbolic link(s) are copied as links."))
    }

    /// F5 and F6 in a remote panel (P3 5.5, 5.6): into a local directory, a download, and a
    /// move that keeps the remote sources and says so (R-4); into a panel on the same
    /// session, F6 renames on the server.
    pub(super) fn remote_copy(&mut self, moving: bool) -> Vec<Effect> {
        let Some((_, groups)) = self.remote_selection() else {
            return Vec::new();
        };
        let links = self.links_text();
        if self.other().remote().is_some() {
            // F5 and another session are refused before this (P3 2.4).
            let Some(o) = self.other_remote() else {
                return Vec::new();
            };
            let lines = vec![format!(
                "Move {} on {} to:",
                count_text(&groups),
                escaped(o.target.address().as_bytes())
            )];
            let mut dst = o.dir.to_bytes();
            if !dst.ends_with(b"/") {
                dst.push(b'/');
            }
            let at = Dest::Remote {
                session: o.session,
                dir: o.dir,
            };
            let purpose = Purpose::ToServer {
                groups,
                at,
                moving: true,
            };
            self.dialog = Some(Dialog::input("Move", lines, &dst, purpose));
            return Vec::new();
        }
        let verb = if moving { "Move" } else { "Download" };
        let mut lines = vec![format!("{verb} {} to:", count_text(&groups))];
        lines.extend(links);
        if moving {
            lines.push("This move is best-effort: the files are copied and synced here;".into());
            lines.push(REMOTE_KEPT.into());
        }
        let mut dst = self.other().dir.as_os_str().as_bytes().to_vec();
        if !dst.ends_with(b"/") {
            dst.push(b'/');
        }
        let dir = self.panel().dir.clone();
        let purpose = if moving {
            Purpose::Move { dir, groups }
        } else {
            Purpose::Copy { dir, groups }
        };
        self.dialog = Some(Dialog::input(verb, lines, &dst, purpose));
        Vec::new()
    }

    /// F5 and F6 from a local directory or a results tab into a remote panel (P3 5.6): an
    /// upload; the move is best-effort, and the dialog says so before the job (R-4).
    pub(super) fn upload(&mut self, moving: bool) -> Vec<Effect> {
        let groups = self.panel().selection_groups();
        if groups.is_empty() {
            return Vec::new();
        }
        let Some(o) = self.other_remote() else {
            return Vec::new();
        };
        let verb = if moving { "Move" } else { "Upload" };
        let mut lines = vec![format!(
            "{verb} {} to {}:",
            count_text(&groups),
            escaped(o.target.address().as_bytes())
        )];
        lines.extend(self.links_text());
        if moving {
            lines.push(MOVE_TO_SERVER[0].into());
            lines.push(MOVE_TO_SERVER[1].into());
        }
        let mut dst = o.dir.to_bytes();
        if !dst.ends_with(b"/") {
            dst.push(b'/');
        }
        let at = Dest::Remote {
            session: o.session,
            dir: o.dir,
        };
        let purpose = Purpose::ToServer { groups, at, moving };
        self.dialog = Some(Dialog::input(verb, lines, &dst, purpose));
        Vec::new()
    }

    /// Shift+F6 in a remote panel (P3 5.6): a rename on the server.
    pub(super) fn remote_rename(&mut self) -> Vec<Effect> {
        let p = self.panel();
        let Some(v) = p.remote().cloned() else {
            return Vec::new();
        };
        let Some(name) = p.current_name().map(<[u8]>::to_vec) else {
            return Vec::new();
        };
        if v.home || self.remote_lost() {
            return Vec::new();
        }
        let group = Group {
            root: Root::Remote(v.session.clone()),
            sub: v.dir.components().to_vec(),
            names: vec![OsStr::from_bytes(&name).to_owned()],
        };
        self.dialog = Some(Dialog::input(
            "Rename",
            vec!["New name:".into()],
            &name,
            Purpose::Rename { group },
        ));
        Vec::new()
    }

    /// F7 in a remote panel (P3 5.6).
    pub(super) fn remote_mkdir(&mut self) -> Vec<Effect> {
        let Some(v) = self.panel().remote().cloned() else {
            return Vec::new();
        };
        if v.home || self.remote_lost() {
            return Vec::new();
        }
        let at = Dest::Remote {
            session: v.session,
            dir: v.dir,
        };
        self.dialog = Some(Dialog::input(
            "Make directory",
            vec!["Name (a/b/c creates parents):".into()],
            b"",
            Purpose::MkdirRemote { at },
        ));
        Vec::new()
    }

    /// Shift+F8 in a remote panel (R-5): the M1 confirmation; the job scans and asks for the
    /// typed `delete`.
    pub(super) fn remote_delete(&mut self) -> Vec<Effect> {
        let Some((v, groups)) = self.remote_selection() else {
            return Vec::new();
        };
        let text = count_text(&groups);
        self.dialog = Some(Dialog::confirm(
            "Delete permanently",
            vec![
                format!(
                    "Permanently delete {text} on {}?",
                    escaped(v.target.address().as_bytes())
                ),
                "There is no trash on a server.".into(),
                "The next step counts the files and asks you to type delete.".into(),
            ],
            "Continue",
            Purpose::Delete { groups },
        ));
        Vec::new()
    }

    /// The typed server path of a dialog (P3 5.6): absolute, relative to `base`, or an
    /// `sftp://` address of the same server; `..` is refused, as in an address (P3 5.1).
    pub(super) fn server_path(
        &mut self,
        base: &VPath,
        target: &crate::provider::Target,
        text: &[u8],
    ) -> Option<VPath> {
        if url::is_sftp(text) {
            return match url::parse(text) {
                Ok(Address {
                    target: t,
                    dir: RemoteDir::Absolute(p),
                }) if t == *target => Some(p),
                Ok(_) => {
                    self.warn("not on this server; copy through a local directory");
                    None
                }
                Err(e) => {
                    self.warn(e);
                    None
                }
            };
        }
        let rel = match VPath::parse(text) {
            Ok(p) => p,
            Err(_) => {
                self.warn(NOT_A_SERVER_PATH);
                return None;
            }
        };
        if text.starts_with(b"/") {
            return Some(rel);
        }
        let mut p = base.clone();
        for c in rel.components() {
            p = p.join(c).ok()?;
        }
        Some(p)
    }

    /// The write-back question (P3 5.6): the editor changed the view copy of a remote file.
    pub(super) fn write_back_question(&mut self, copy: PathBuf, at: Dest, changed: bool) {
        let Dest::Remote { dir: path, session } = &at else {
            return;
        };
        // A dialog that is open (a job's question waits on it) is never replaced: the copy
        // stays, and the status line says where.
        if self.dialog.is_some() {
            self.warn(kept_remote_text(&copy));
            return;
        }
        let name = path
            .name()
            .map(|n| n.as_bytes().to_vec())
            .unwrap_or_default();
        let place = crate::remote::provider::location(session.target(), path);
        let mut lines = vec![
            format!("\"{}\" was edited.", escaped(&name)),
            format!("Upload it to {}?", escaped(&place)),
        ];
        let mut buttons = vec!["Upload".to_string()];
        if changed {
            lines.push("The file on the server changed since it was downloaded.".into());
            let other = suggest_rename(OsStr::from_bytes(&name), 1);
            buttons.push(format!("Save as \"{}\"", escaped(other.as_bytes())));
        }
        buttons.push("Keep the local copy".into());
        self.dialog = Some(Dialog::choose(
            "Edited copy",
            lines,
            buttons,
            Purpose::WriteBack { copy, at, changed },
        ));
    }

    /// The answer to the write-back question: upload (replacing through R-2), save under
    /// `name (1)`, or keep the local copy and say where it is.
    pub(super) fn write_back_answer(
        &mut self,
        copy: PathBuf,
        at: Dest,
        changed: bool,
        i: usize,
    ) -> Vec<Effect> {
        let Dest::Remote { session, dir: path } = at else {
            return Vec::new();
        };
        let keep = if changed { 2 } else { 1 };
        if i >= keep {
            self.warn(kept_remote_text(&copy));
            return Vec::new();
        }
        if self.job.is_some() {
            self.warn(format!("a job is running; {}", kept_remote_text(&copy)));
            return Vec::new();
        }
        if i == 0 {
            let dst = Dest::Remote { session, dir: path };
            return self.start_job(JobSpec::WriteBack { copy, dst });
        }
        // Save as `name (1)` next to it.
        let (Some(parent), Some(name), Some(dir), Some(file)) =
            (path.parent(), path.name(), copy.parent(), copy.file_name())
        else {
            return Vec::new();
        };
        let Ok(new) = parent.join(&suggest_rename(name, 1)) else {
            return Vec::new();
        };
        let groups = vec![Group::new(dir, vec![file.to_owned()])];
        let dst = Dest::Remote { session, dir: new };
        self.start_job(JobSpec::Copy { groups, dst })
    }

    /// `Space` on a remote directory (P3 2.4): its size by a walk on the server, which
    /// `Esc` stops.
    pub(super) fn remote_size(&mut self) -> Vec<Effect> {
        let p = self.panel();
        let Some(v) = p.remote().cloned() else {
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
        if v.home || v.lost() {
            return Vec::new();
        }
        let cancel = Arc::new(AtomicBool::new(false));
        if let Some(c) = self.remote_size.replace(cancel.clone()) {
            c.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        vec![Effect::RemoteSize(SizeRequest {
            slot,
            generation,
            remote: v.session,
            dir: v.dir,
            name,
            cancel,
        })]
    }

    /// `Ctrl+R` in a remote panel (P3 2.4, 5.7): a lost session reconnects, in place; an
    /// open one re-lists.
    pub(super) fn remote_reread(&mut self) -> Option<Vec<Effect>> {
        let v = self.panel().remote().cloned()?;
        if !v.lost() {
            return None;
        }
        let side = self.active;
        let cursor = self.panel().cursor_name().map(<[u8]>::to_vec);
        let at = v.remote_dir();
        Some(self.open_remote(side, v.target, at, cursor, Record::No))
    }

    /// A history place, a released tab or a bookmark of a server (P3 2.2, 5.7): the pool's
    /// session for its target, else a connect.
    pub(super) fn open_remote_place(
        &mut self,
        side: usize,
        place: Place,
        cursor_to: Option<Vec<u8>>,
        record: Record,
    ) -> Vec<Effect> {
        let Place::Remote { target, dir, home } = place else {
            return Vec::new();
        };
        let dir = if home {
            RemoteDir::Home(dir)
        } else {
            RemoteDir::Absolute(dir)
        };
        self.open_remote(side, target, dir, cursor_to, record)
    }

    /// The bookmark address of the active remote panel (P3 5.1): `Insert` in the
    /// directories dialog.
    pub(super) fn remote_bookmark(&self) -> Option<std::path::PathBuf> {
        let v = self.panel().remote()?;
        (!v.home).then(|| std::path::PathBuf::from(url::format(&v.target, &v.dir)))
    }

    pub(super) fn on_remote(&mut self, m: RemoteMsg) -> Vec<Effect> {
        match m {
            RemoteMsg::Connected { target, session } => {
                self.say(format!("connected to {}", target.address()));
                let remote = Arc::new(RemoteProvider::new(session, target.clone()));
                self.pool.insert(remote.clone());
                let Some(p) = self.pending.take().filter(|p| p.target == target) else {
                    return Vec::new();
                };
                match self.find_slot(p.slot) {
                    Some((s, t)) if t == self.sides[s].active => {
                        self.show_remote(s, remote, p.dir, p.cursor, p.record)
                    }
                    _ => Vec::new(),
                }
            }
            RemoteMsg::Failed { address, message } => {
                self.warn(format!("{address}: connection failed: {message}"));
                // A tab that had released its listing to reconnect shows its local
                // directory again.
                let Some(p) = self.pending.take() else {
                    return Vec::new();
                };
                match self.find_slot(p.slot) {
                    Some((s, t))
                        if t == self.sides[s].active
                            && self.sides[s].panel().is_directory()
                            && self.sides[s].panel().loading.is_none()
                            && self.sides[s].panel().list.entries.is_empty() =>
                    {
                        let dir = self.sides[s].panel().dir.clone();
                        self.load_ex(s, dir, None, true, Record::No)
                    }
                    _ => Vec::new(),
                }
            }
            RemoteMsg::Lost { address, message } => {
                // Every tab on a lost session keeps its rows and says so (P3 5.5).
                for side in &mut self.sides {
                    for p in &mut side.tabs {
                        if p.remote().is_some_and(RemoteView::lost) {
                            p.message = Some(LOST_PANEL.into());
                        }
                    }
                }
                self.warn(format!("{address}: connection lost: {message}"));
                Vec::new()
            }
        }
    }
}

/// What an upload dialog with a path that is not one on the server says.
pub const NOT_A_SERVER_PATH: &str = "not a usable path on the server";

/// The confirm dialog of a move to a server (R-4): best-effort, before the job starts.
pub const MOVE_TO_SERVER: [&str; 2] = [
    "This move is best-effort: a server cannot make it durable.",
    "Each local source goes after its upload is committed.",
];

/// The argument of a `cd` line as typed, before expansion, or `None` for another line:
/// a remote panel tells a local path (`/`, `~`, `$`) from a relative one by it (P3 5.1).
pub fn cd_arg(text: &[u8]) -> Option<&[u8]> {
    let t = text.trim_ascii();
    let rest = t.strip_prefix(b"cd")?;
    if !(rest.is_empty() || rest[0].is_ascii_whitespace()) {
        return None;
    }
    Some(rest.trim_ascii())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cd_argument_as_typed() {
        assert_eq!(cd_arg(b"cd ../x"), Some(&b"../x"[..]));
        assert_eq!(cd_arg(b"  cd\t~/x "), Some(&b"~/x"[..]));
        assert_eq!(cd_arg(b"cd"), Some(&b""[..]));
        assert_eq!(cd_arg(b"cdx"), None);
        assert_eq!(cd_arg(b"ls"), None);
    }
}
