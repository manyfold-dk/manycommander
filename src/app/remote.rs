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
//! A lost session keeps the panel's rows and says "connection lost -- Ctrl+R reconnects";
//! the verbs that need the server are refused until `Ctrl+R` reconnects. manycommander
//! never reconnects on its own. Like the rest of `App` this makes no I/O: the listing
//! threads and the job worker call the session, the UI thread only reads its lost flag.

use super::event::Effect;
use super::{App, MAX_ABANDONED, TOO_MANY_BLOCKED, count_text};
use crate::fsops::group::{Group, Root};
use crate::panel::entry::{EKind, LinkKind};
use crate::panel::listing::Alive;
use crate::panel::{Place, Record, RemoteView, Row, join_lexical};
use crate::provider::Target;
use crate::remote::provider::{LOST_PANEL, NOT_A_FILE};
use crate::remote::transport::SshCommand;
use crate::remote::tree::SizeRequest;
use crate::remote::url::{self, Address, RemoteDir};
use crate::remote::{RemoteMsg, RemoteProvider};
use crate::ui::dialog::{Dialog, Purpose, human_size};
use crate::viewtemp::ASK_ABOVE;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
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
                        human_size(size)
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

    /// F5 in a remote panel (P3 5.5): the confirm dialog; the job downloads into the other
    /// panel's directory through the local engine.
    pub(super) fn remote_copy(&mut self) -> Vec<Effect> {
        let p = self.panel();
        let Some(v) = p.remote().cloned() else {
            return Vec::new();
        };
        let names = p.selection();
        if names.is_empty() || v.home {
            return Vec::new();
        }
        let links = p
            .selection_kinds()
            .iter()
            .filter(|k| **k == EKind::Symlink)
            .count();
        if self.remote_lost() {
            return Vec::new();
        }
        let groups = vec![Group {
            root: Root::Remote(v.session.clone()),
            sub: v.dir.components().to_vec(),
            names,
        }];
        let mut lines = vec![format!("Download {} to:", count_text(&groups))];
        if links > 0 {
            lines.push(format!("{links} symbolic link(s) are copied as links."));
        }
        let mut dst = self.other().dir.as_os_str().as_bytes().to_vec();
        if !dst.ends_with(b"/") {
            dst.push(b'/');
        }
        let dir = self.panel().dir.clone();
        self.dialog = Some(Dialog::input(
            "Download",
            lines,
            &dst,
            Purpose::Copy { dir, groups },
        ));
        Vec::new()
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
