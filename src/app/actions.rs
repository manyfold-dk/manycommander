#![forbid(unsafe_code)]
//! What a key does (TDR-1 8, TDR-2 10): an open dialog takes it first, then the quick
//! search and the filter line (TDR-2 4); what is left goes through the keymap
//! ([`keys::map`]) to [`App::act`], which refuses a verb the place does not allow (TDR-3
//! 2.4) and performs the action: cursor moves, marks, the command line's editing keys, and
//! the verbs, tabs, forms and dialogs that other modules carry out.

use super::event::Effect;
use super::keys::{self, Action};
use super::{App, SEARCH_STOPPING, SizeWalk, TOO_MANY_BLOCKED, count_text, jobs, search};
use crate::cmdline::handoff::Handoff;
use crate::cmdline::{Line, quote};
use crate::fsops::group::Group;
use crate::panel::Row;
use crate::panel::entry::EKind;
use crate::panel::listing::{Alive, SizeRequest};
use crate::ui::dialog::{Dialog, Outcome, Purpose, edit};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

impl App {
    pub(super) fn on_key(&mut self, k: KeyEvent) -> Vec<Effect> {
        self.status = None;
        if let Some(d) = self.dialog.as_mut() {
            return match d.handle(k) {
                Outcome::Stay => Vec::new(),
                Outcome::Close => {
                    self.dialog = None;
                    Vec::new()
                }
                Outcome::Done(p, text) => {
                    self.dialog = None;
                    self.dialog_done(p, text)
                }
                Outcome::FormChanged => {
                    self.check_form();
                    Vec::new()
                }
                Outcome::FormSubmit => self.submit_form(),
                Outcome::Dirs(a) => self.on_dirs_action(a),
                Outcome::Undo => self.undo_rename(),
                Outcome::Chosen(p, i) => {
                    self.dialog = None;
                    match p {
                        Purpose::WriteBack { copy, at, changed } => {
                            self.write_back_answer(copy, at, changed, i)
                        }
                        _ => Vec::new(),
                    }
                }
            };
        }
        if let Some(prefix) = self.search.as_mut() {
            match k.code {
                KeyCode::Char('s') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    let pre = prefix.clone();
                    let p = self.panel_mut();
                    if let Some(r) = p.find_prefix(&pre, true) {
                        p.cursor_to(r);
                    }
                    return Vec::new();
                }
                KeyCode::Char(c)
                    if !k
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    let mut b = [0u8; 4];
                    prefix.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                    let pre = prefix.clone();
                    let p = self.panel_mut();
                    if let Some(r) = p.find_prefix(&pre, false) {
                        p.cursor_to(r);
                    }
                    return Vec::new();
                }
                KeyCode::Backspace => {
                    // A whole character goes, and the shorter prefix is searched again.
                    while prefix.pop().is_some_and(|b| b & 0xc0 == 0x80) {}
                    let pre = prefix.clone();
                    let p = self.panel_mut();
                    if let Some(r) = p.find_prefix(&pre, false) {
                        p.cursor_to(r);
                    }
                    return Vec::new();
                }
                KeyCode::Esc | KeyCode::Enter => {
                    self.search = None;
                    return Vec::new();
                }
                _ => self.search = None,
            }
        }
        if self.filter_line.is_some() && self.filter_key(k) {
            return Vec::new();
        }
        let empty = self.line.is_empty();
        if self.line_focused && empty && keys::leaves_empty_line(k) {
            self.line_focused = false;
            if keys::map(k, true) == Action::LineBackspace {
                return Vec::new();
            }
        }
        let a = keys::map(k, self.line_focused || !empty);
        self.act(a)
    }

    /// A key while the filter line is open (TDR-2 4). Returns whether the line took it.
    /// `Ctrl+F` closes the line and keeps the filter; `Esc` clears the filter and closes;
    /// `Ctrl+E` closes it, keeping the filter, and gives the command line the focus;
    /// `Backspace` on an empty line closes it. The line-editing keys edit it and re-filter
    /// at once. `Up`, `Down`, `PgUp` and `PgDn` move the panel cursor with the line open.
    /// Any other key closes the line, keeping the filter, and then acts as usual: `Enter`
    /// opens the entry under the cursor, unless no entry is visible.
    fn filter_key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let nothing_visible = self.panel().list.visible.is_empty();
        let Some(line) = self.filter_line.as_mut() else {
            return false;
        };
        let backspace = k.code == KeyCode::Backspace || (ctrl && k.code == KeyCode::Char('h'));
        match k.code {
            // With nothing to open, Enter would act on `..`.
            KeyCode::Enter if !alt && nothing_visible => {}
            KeyCode::Char('f') if ctrl => {}
            KeyCode::Char('e') if ctrl && !alt => self.line_focused = true,
            _ if backspace && !alt && line.is_empty() => {}
            KeyCode::Esc => self.panel_mut().set_filter(b""),
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown if !alt => {
                return false;
            }
            _ if !alt && edit(line, k, ctrl) => {
                let text = line.bytes().to_vec();
                self.panel_mut().set_filter(&text);
                return true;
            }
            _ => {
                self.filter_line = None;
                return false;
            }
        }
        self.filter_line = None;
        true
    }

    fn act(&mut self, a: Action) -> Vec<Effect> {
        // Verbs by place (TDR-3 2.4): refused before any work.
        if let Some(why) = jobs::refusal(a, jobs::At::of(self.panel()), jobs::At::of(self.other()))
        {
            self.warn(why);
            return Vec::new();
        }
        let page = self.page.max(1) as isize;
        match a {
            Action::None => Vec::new(),
            Action::Up => self.cursor(-1),
            Action::Down => self.cursor(1),
            Action::PageUp => self.cursor(-page),
            Action::PageDown => self.cursor(page),
            Action::First => {
                self.panel_mut().cursor_to(0);
                Vec::new()
            }
            Action::Last => {
                let r = self.panel().rows();
                self.panel_mut().cursor_to(r.saturating_sub(1));
                Vec::new()
            }
            Action::SwitchPanel => {
                self.active = 1 - self.active;
                Vec::new()
            }
            Action::SwapPanels => {
                self.sides.swap(0, 1);
                Vec::new()
            }
            Action::Enter if self.panel().is_results() => self.enter_result(),
            Action::Enter => self.enter(),
            Action::Parent if self.panel().is_results() => self.results_parent(),
            Action::Parent => self.parent(),
            Action::Mkdir | Action::EditNew | Action::Compare if self.panel().is_results() => {
                self.warn(search::NOT_IN_RESULTS);
                Vec::new()
            }
            Action::Find => self.find_form(),
            Action::Escape => {
                let p = self.panel_mut();
                let slot = p.slot;
                // A navigation stops. A refresh keeps the rows on screen and is left alone:
                // one the watcher started while a job writes here would otherwise take the
                // `Esc` that cancels the job.
                let navigating = p.is_loading();
                if let Some((dir, alive)) = navigating.then(|| p.cancel_load()).flatten() {
                    self.mark_visit(slot, false);
                    self.abandon(dir, alive);
                } else if self.cancel_view() || self.cancel_active_search() {
                } else if self.stop_size_walk() || self.stop_remote_size() {
                    self.say("size cancelled");
                } else if self.compare.take().is_some() {
                    self.say("compare cancelled");
                    return vec![Effect::CancelCompare];
                } else if self.job.is_some() {
                    self.dialog = Some(Dialog::confirm(
                        "Cancel job",
                        vec!["Cancel the running job?".into()],
                        "Cancel job",
                        Purpose::CancelJob,
                    ));
                }
                Vec::new()
            }
            Action::MarkAndDown => {
                self.panel_mut().toggle_mark(true);
                Vec::new()
            }
            Action::OpenArchive => self.open_as_archive(),
            Action::QuickView => self.toggle_quick(),
            Action::QuickLoad => self.quick_load(),
            Action::MarkSpace if self.panel().archive().is_some() => self.archive_size(),
            Action::MarkSpace if self.panel().remote().is_some() => self.remote_size(),
            Action::MarkSpace => {
                let p = self.panel();
                let (slot, generation, dir) = (p.slot, p.generation, p.dir.clone());
                // A directory's size in an archive comes from the index, and on a server
                // from a walk there (TDR-3 2.4): no local walk.
                let local = p.archive().is_none() && p.remote().is_none();
                let dir_name = match p.current_entry() {
                    Some((i, e)) if e.kind == EKind::Dir && local => {
                        Some(OsStr::from_bytes(p.list.name(i)).to_owned())
                    }
                    _ => None,
                };
                self.panel_mut().toggle_mark(false);
                let Some(name) = dir_name else {
                    return Vec::new();
                };
                // One walk at a time: the previous one stops, and counts as abandoned while
                // its thread still runs (TDR-1 3.1).
                self.stop_size_walk();
                if self.at_abandon_cap() {
                    self.warn(TOO_MANY_BLOCKED);
                    return Vec::new();
                }
                let cancel = Arc::new(AtomicBool::new(false));
                let alive = Alive::running();
                self.size_walk = Some(SizeWalk {
                    cancel: cancel.clone(),
                    alive: alive.clone(),
                });
                vec![Effect::DirSize(
                    SizeRequest {
                        slot,
                        generation,
                        dir,
                        name,
                        cancel,
                    },
                    alive,
                )]
            }
            Action::MarkAll => {
                self.panel_mut().mark_all();
                Vec::new()
            }
            Action::InvertMarks => {
                self.panel_mut().invert_marks();
                Vec::new()
            }
            Action::MarkGlob => {
                self.dialog = Some(Dialog::input(
                    "Mark",
                    vec!["Mark entries matching:".into()],
                    b"*",
                    Purpose::MarkGlob,
                ));
                Vec::new()
            }
            Action::UnmarkGlob => {
                self.dialog = Some(Dialog::input(
                    "Unmark",
                    vec!["Unmark entries matching:".into()],
                    b"*",
                    Purpose::UnmarkGlob,
                ));
                Vec::new()
            }
            Action::QuickSearch => {
                self.search = Some(Vec::new());
                Vec::new()
            }
            Action::Filter => {
                let mut l = Line::default();
                l.set(self.panel().filter.text());
                self.filter_line = Some(l);
                Vec::new()
            }
            // Type to filter: a new filter from the typed character.
            Action::FilterChar(c) => {
                let mut l = Line::default();
                l.insert_char(c);
                let text = l.bytes().to_vec();
                self.filter_line = Some(l);
                self.panel_mut().set_filter(&text);
                Vec::new()
            }
            Action::FocusLine => {
                self.line_focused = true;
                self.line.end();
                Vec::new()
            }
            Action::Compare => self.compare_form(),
            Action::Directories => self.dirs_dialog(b""),
            Action::ToggleHidden => {
                self.panel_mut().toggle_hidden();
                Vec::new()
            }
            Action::Reread => {
                if self.panel().search().is_some_and(|s| s.stopping()) {
                    self.say(SEARCH_STOPPING);
                }
                // Ctrl+R in an archive reads it again when it changed (TDR-3 2.4).
                let side = self.active;
                if self.panel().archive().is_some() {
                    let mut fx = self.refresh_archive(side, crate::archive::Check::Rescan);
                    fx.extend(self.refresh_slot(1 - side));
                    return fx;
                }
                // Ctrl+R on a lost session reconnects (TDR-3 5.7).
                if let Some(mut fx) = self.remote_reread() {
                    fx.extend(self.refresh_slot(1 - side));
                    return fx;
                }
                self.refresh_both()
            }
            Action::HistoryBack => self.history_move(true),
            Action::HistoryForward => self.history_move(false),
            Action::Sort(key) => {
                self.panel_mut().set_sort(key);
                Vec::new()
            }
            Action::InsertName | Action::InsertPath => {
                let p = self.panel();
                let bytes = match p.current() {
                    Some(Row::Entry(i)) => {
                        let n = p.list.name(i);
                        match (a, p.remote()) {
                            // `Alt+P` on a server: the remote path (TDR-3 5.7).
                            (Action::InsertPath, Some(v)) => {
                                crate::remote::provider::join(&v.dir.to_bytes(), n)
                            }
                            (Action::InsertPath, None) => {
                                p.path_of(n).as_os_str().as_bytes().to_vec()
                            }
                            _ => n.to_vec(),
                        }
                    }
                    _ => return Vec::new(),
                };
                let mut q = quote(&bytes);
                q.push(b' ');
                self.line.insert_bytes(&q);
                Vec::new()
            }
            Action::ShowOutput => vec![Effect::Run(Handoff::ShowScreen)],
            Action::Help => {
                self.dialog = Some(Dialog::help());
                Vec::new()
            }
            Action::View => self.view_edit(false),
            Action::Edit => self.view_edit(true),
            Action::EditNew => {
                let dir = self.panel().dir.clone();
                self.dialog = Some(Dialog::input(
                    "Edit new file",
                    vec!["File name:".into()],
                    b"",
                    Purpose::EditNew { dir },
                ));
                Vec::new()
            }
            Action::Copy | Action::Move => self.copy_move(a == Action::Move),
            Action::Rename if self.panel().remote().is_some() => self.remote_rename(),
            Action::Mkdir if self.panel().remote().is_some() => self.remote_mkdir(),
            Action::Delete if self.panel().remote().is_some() => self.remote_delete(),
            Action::Rename => {
                let p = self.panel();
                let Some(name) = p.current_name() else {
                    return Vec::new();
                };
                // A result is renamed in its own directory (TDR-2 5.4): one group with its
                // `sub`, and the leaf as the name.
                let Some(group) =
                    Group::from_relative(&p.dir, [OsStr::from_bytes(name).to_owned()]).pop()
                else {
                    return Vec::new();
                };
                let leaf = group.names[0].as_bytes().to_vec();
                self.dialog = Some(Dialog::input(
                    "Rename",
                    vec!["New name:".into()],
                    &leaf,
                    Purpose::Rename { group },
                ));
                Vec::new()
            }
            Action::Mkdir => {
                let dir = self.panel().dir.clone();
                self.dialog = Some(Dialog::input(
                    "Make directory",
                    vec!["Name (a/b/c creates parents):".into()],
                    b"",
                    Purpose::Mkdir { dir },
                ));
                Vec::new()
            }
            Action::Trash => {
                let groups = self.panel().selection_groups();
                if groups.is_empty() {
                    return Vec::new();
                }
                let text = count_text(&groups);
                let purpose = Purpose::Trash { groups };
                self.dialog = Some(Dialog::confirm(
                    "Trash",
                    vec![format!("Move {text} to trash?")],
                    "Trash",
                    purpose,
                ));
                Vec::new()
            }
            Action::Delete => {
                let groups = self.panel().selection_groups();
                if groups.is_empty() {
                    return Vec::new();
                }
                let text = count_text(&groups);
                let purpose = Purpose::Delete { groups };
                self.dialog = Some(Dialog::confirm(
                    "Delete permanently",
                    vec![
                        format!("Permanently delete {text}?"),
                        "The next step counts the files and asks you to type delete.".into(),
                    ],
                    "Continue",
                    purpose,
                ));
                Vec::new()
            }
            Action::Quit => self.request_quit(false),
            Action::Link => self.link_form(),
            Action::Attributes => self.attr_form(),
            Action::MultiRename => self.rename_tool(),
            Action::NewTab
            | Action::CloseTab
            | Action::PrevTab
            | Action::NextTab
            | Action::GotoTab(_) => self.tab_action(a),
            // Command line.
            Action::LineChar(c) => {
                self.line.insert_char(c);
                self.history.reset();
                Vec::new()
            }
            Action::LineRun => {
                self.line_focused = false;
                if self.line.is_empty() {
                    return Vec::new();
                }
                self.run_line()
            }
            Action::LineBackspace => {
                self.line.backspace();
                Vec::new()
            }
            Action::LineDelete => {
                self.line.delete();
                Vec::new()
            }
            Action::LineLeft => {
                self.line.left();
                Vec::new()
            }
            Action::LineRight => {
                self.line.right();
                Vec::new()
            }
            Action::LineHome => {
                self.line.home();
                Vec::new()
            }
            Action::LineEnd => {
                self.line.end();
                Vec::new()
            }
            Action::LineKillStart => {
                self.line.kill_start();
                Vec::new()
            }
            Action::LineKillEnd => {
                self.line.kill_end();
                Vec::new()
            }
            Action::LineKillWord => {
                self.line.kill_word();
                Vec::new()
            }
            Action::LineClear => {
                self.line.clear();
                self.line_focused = false;
                self.history.reset();
                Vec::new()
            }
            Action::HistoryPrev => {
                self.history.prev(&mut self.line);
                Vec::new()
            }
            Action::HistoryNext => {
                self.history.next(&mut self.line);
                Vec::new()
            }
        }
    }

    fn cursor(&mut self, d: isize) -> Vec<Effect> {
        self.panel_mut().move_cursor(d);
        Vec::new()
    }
}
