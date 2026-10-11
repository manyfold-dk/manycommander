#![forbid(unsafe_code)]
//! The verbs in a local panel (TDR-1 4.4, 6): `Enter` and `..`, F3 and F4 (the pager, the
//! editor, or the file's application), the F5 and F6 confirm dialog, and the answers of the
//! confirm and input dialogs, most of which start a job; and the command line's `Enter`:
//! `cd`, `z` or a shell command in the terminal hand-off. In an archive or a server panel a
//! verb goes to that place's module (TDR-3 2.4). Like the rest of `App` this makes no
//! filesystem syscall (P-1): a job runs on the worker, a program or a shell command in the
//! hand-off.

use super::event::Effect;
use super::{App, count_text, handoff, remote};
use crate::cmdline::handoff::{Handoff, editors, pager, program_argv};
use crate::cmdline::{self, Command, ProcessEnv};
use crate::fsops::job::{Dest, JobSpec};
use crate::panel::entry::EKind;
use crate::panel::{Row, join_lexical};
use crate::ui::dialog::{Dialog, Purpose};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

impl App {
    pub(super) fn enter(&mut self) -> Vec<Effect> {
        if self.panel().archive().is_some() {
            return self.archive_enter();
        }
        if self.panel().remote().is_some() {
            return self.remote_enter();
        }
        let p = self.panel();
        match p.current() {
            Some(Row::Parent) => self.parent(),
            Some(Row::Entry(i)) => {
                let e = p.list.entries[i as usize];
                let name = p.list.name(i).to_vec();
                let path = p.path_of(&name);
                if e.enters() {
                    let side = self.active;
                    self.load(side, path, None, false)
                } else if e.kind == EKind::File
                    && let Some(fx) = self.enter_archive_by_name(&name)
                {
                    // A recognised archive name is browsed (TDR-3 3.1).
                    fx
                } else {
                    vec![Effect::Open(path)]
                }
            }
            None => Vec::new(),
        }
    }

    pub(super) fn parent(&mut self) -> Vec<Effect> {
        if self.panel().archive().is_some() {
            return self.archive_parent();
        }
        if self.panel().remote().is_some() {
            return self.remote_parent();
        }
        let p = self.panel();
        let Some(parent) = p.dir.parent().map(Path::to_path_buf) else {
            return Vec::new();
        };
        let name = p.dir_name();
        let side = self.active;
        self.load(side, parent, name, false)
    }

    pub(super) fn view_edit(&mut self, edit: bool) -> Vec<Effect> {
        if self.panel().archive().is_some() {
            return self.archive_view(edit);
        }
        if self.panel().remote().is_some() {
            return self.remote_view(edit);
        }
        let p = self.panel();
        let Some((i, e)) = p.current_entry() else {
            return Vec::new();
        };
        if e.kind == EKind::Dir {
            return Vec::new();
        }
        let name = p.list.name(i);
        let path = p.path_of(name);
        // F3 on a picture, a document, audio, video or a web page opens it in its
        // application, as Enter does (TDR-1 6): a regular file, or a symlink to one.
        let file = e.kind == EKind::File
            || (e.kind == EKind::Symlink && e.link == crate::panel::entry::LinkKind::File);
        if !edit && file && handoff::in_application(name) {
            return vec![Effect::Open(path)];
        }
        let cwd = p.dir.clone();
        self.program(edit, &path, cwd)
    }

    pub(super) fn program(&mut self, edit: bool, path: &Path, cwd: PathBuf) -> Vec<Effect> {
        let cmds = if edit {
            editors(
                self.config.editor.as_deref(),
                std::env::var_os("EDITOR").as_deref(),
            )
        } else {
            vec![pager(
                self.config.pager.as_deref(),
                std::env::var_os("PAGER").as_deref(),
            )]
        };
        let argvs: Vec<Vec<OsString>> = cmds.iter().filter_map(|c| program_argv(c, path)).collect();
        match argvs.into_iter().next() {
            Some(argv) => vec![Effect::Run(Handoff::Program { argv, cwd })],
            None => {
                self.warn("$PAGER or $EDITOR does not parse");
                Vec::new()
            }
        }
    }

    pub(super) fn copy_move(&mut self, moving: bool) -> Vec<Effect> {
        // F5 out of an archive extracts (F6 is refused before this); out of a server F5
        // downloads and F6 moves or renames; into a server both upload (TDR-3 2.4, 5.6).
        if self.panel().archive().is_some() {
            return self.archive_copy();
        }
        if self.panel().remote().is_some() {
            return self.remote_copy(moving);
        }
        if self.other().remote().is_some() {
            return self.upload(moving);
        }
        let p = self.panel();
        let groups = p.selection_groups();
        if groups.is_empty() {
            return Vec::new();
        }
        let dir = p.dir.clone();
        let links = p
            .selection_kinds()
            .iter()
            .filter(|k| **k == EKind::Symlink)
            .count();
        let mut dst = self.other().dir.as_os_str().as_bytes().to_vec();
        if !dst.ends_with(b"/") {
            dst.push(b'/');
        }
        let verb = if moving { "Move" } else { "Copy" };
        let mut lines = vec![format!("{verb} {} to:", count_text(&groups))];
        if links > 0 && !moving {
            lines.insert(1, format!("{links} symbolic link(s) are copied as links."));
        }
        let purpose = if moving {
            Purpose::Move { dir, groups }
        } else {
            Purpose::Copy { dir, groups }
        };
        self.dialog = Some(Dialog::input(verb, lines, &dst, purpose));
        Vec::new()
    }

    pub(super) fn dialog_done(&mut self, p: Purpose, text: Vec<u8>) -> Vec<Effect> {
        let typed = PathBuf::from(OsStr::from_bytes(&text));
        match p {
            Purpose::QuitWithJob => {
                self.quit_after_job = true;
                if let Some(j) = self.job.as_mut() {
                    j.cancel_requested = true;
                    j.cancel_at.get_or_insert_with(Instant::now);
                }
                vec![Effect::CancelJob]
            }
            Purpose::CancelJob => {
                if let Some(j) = self.job.as_mut() {
                    j.cancel_requested = true;
                    j.cancel_at.get_or_insert_with(Instant::now);
                }
                vec![Effect::CancelJob]
            }
            Purpose::Copy { dir, groups } => {
                let dst = Dest::Local(join_lexical(&dir, &typed));
                self.start_job(JobSpec::Copy { groups, dst })
            }
            Purpose::Move { dir, groups } => {
                let dst = Dest::Local(join_lexical(&dir, &typed));
                self.start_job(JobSpec::Move { groups, dst })
            }
            Purpose::Rename { group } => {
                if text.is_empty()
                    || text.contains(&b'/')
                    || group.names.first().map(|n| n.as_bytes()) == Some(&text[..])
                {
                    return Vec::new();
                }
                // Shift+F6 is a move of one group with one name (TDR-2 2.2), in the group's
                // own directory; on a server, a rename there (TDR-3 5.6).
                let name = OsStr::from_bytes(&text);
                let dst = match &group.root {
                    crate::fsops::group::Root::Remote(r) => {
                        let at = crate::provider::VPath::new(group.sub.clone())
                            .and_then(|d| d.join(name));
                        let Ok(dir) = at else {
                            self.warn(remote::NOT_A_SERVER_PATH);
                            return Vec::new();
                        };
                        Dest::Remote {
                            session: r.clone(),
                            dir,
                        }
                    }
                    _ => Dest::Local(group.dir_path().join(name)),
                };
                self.start_job(JobSpec::Move {
                    groups: vec![group],
                    dst,
                })
            }
            Purpose::Mkdir { dir } => {
                if text.is_empty() {
                    return Vec::new();
                }
                self.start_job(JobSpec::Mkdir {
                    dir,
                    name: OsStr::from_bytes(&text).to_owned(),
                })
            }
            Purpose::Trash { groups } => self.start_job(JobSpec::Trash { groups }),
            Purpose::Delete { groups } => self.start_job(JobSpec::Delete { groups }),
            Purpose::EditNew { dir } => {
                if text.is_empty() || text.contains(&b'/') {
                    return Vec::new();
                }
                let path = dir.join(OsStr::from_bytes(&text));
                self.program(true, &path, dir)
            }
            Purpose::ViewLarge {
                edit,
                name,
                path,
                size,
            } => self.start_view(edit, name, path, size),
            Purpose::ToServer { groups, at, moving } => {
                let Dest::Remote { session, dir } = at else {
                    return Vec::new();
                };
                let Some(dir) = self.server_path(&dir, session.target(), &text) else {
                    return Vec::new();
                };
                let dst = Dest::Remote { session, dir };
                self.start_job(if moving {
                    JobSpec::Move { groups, dst }
                } else {
                    JobSpec::Copy { groups, dst }
                })
            }
            Purpose::MkdirRemote { at } => {
                if text.is_empty() {
                    return Vec::new();
                }
                self.start_job(JobSpec::MkdirRemote {
                    dir: at,
                    name: OsStr::from_bytes(&text).to_owned(),
                })
            }
            // Answered through `Outcome::Chosen`.
            Purpose::WriteBack { .. } => Vec::new(),
            Purpose::MarkGlob => {
                self.panel_mut().mark_glob(&text, true);
                Vec::new()
            }
            Purpose::UnmarkGlob => {
                self.panel_mut().mark_glob(&text, false);
                Vec::new()
            }
        }
    }

    pub(super) fn run_line(&mut self) -> Vec<Effect> {
        let text = self.line.take();
        self.history.push(&text);
        match cmdline::parse(&text, &ProcessEnv) {
            Command::Cd(p) => {
                if crate::remote::url::is_sftp(p.as_os_str().as_bytes()) {
                    return self.connect(p.as_os_str().as_bytes());
                }
                if let Some(fx) = self.archive_cd(&p) {
                    return fx;
                }
                let raw = remote::cd_arg(&text).unwrap_or_default().to_vec();
                if let Some(fx) = self.remote_cd(&raw, &p) {
                    return fx;
                }
                let dir = join_lexical(&self.panel().dir, &p);
                let side = self.active;
                self.load(side, dir, None, false)
            }
            Command::Z(keywords) => self.z(keywords),
            Command::Shell(t) => vec![Effect::Run(Handoff::Shell {
                shell: cmdline::shell(&ProcessEnv),
                text: t,
                cwd: self.panel().dir.clone(),
            })],
        }
    }
}
