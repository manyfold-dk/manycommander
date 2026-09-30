#![forbid(unsafe_code)]
//! Dialogs (design 4.4, 4.5): confirmations, input prompts, forms (P2 2.1), the worker's
//! questions with both sides' metadata, the typed `delete` confirmation, the job report,
//! the help overlay, the directories dialog (P2 3.1) and the multi-rename tool (P2 6.1).
//! Each dialog owns its state; `handle` turns a key into an outcome.

use super::dirs::{DirsAction, DirsDialog};
use super::form::{Form, FormEvent};
use super::multirename::RenameTool;
use super::text::{escaped, fit};
use crate::cmdline::Line;
use crate::fsops::group::Group;
use crate::fsops::job::{Dest, Outcome as EntryOutcome, Report};
use crate::fsops::question::{Answer, Choice, Question, Side, suggest_rename};
use crate::fsops::sys::Kind;
use crate::fsops::walk::errno_text;
use crate::panel::entry::EKind;
use crate::theme::Theme;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line as TLine, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

/// What an input or confirmation is for; the app acts on it when the dialog completes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Purpose {
    QuitWithJob,
    CancelJob,
    /// F5. `dir` is the panel's directory, which a relative destination resolves against.
    Copy {
        dir: PathBuf,
        groups: Vec<Group>,
    },
    /// F6, as copy.
    Move {
        dir: PathBuf,
        groups: Vec<Group>,
    },
    /// Shift+F6: one name in its directory, as a group (a result's own directory in a
    /// results tab, P2 5.4).
    Rename {
        group: Group,
    },
    Mkdir {
        dir: PathBuf,
    },
    Trash {
        groups: Vec<Group>,
    },
    Delete {
        groups: Vec<Group>,
    },
    EditNew {
        dir: PathBuf,
    },
    /// F3, F4 or `Enter` on an archive member above the size that asks first (P3 3.4).
    ViewLarge {
        edit: bool,
        name: Vec<u8>,
        path: crate::provider::VPath,
        size: u64,
    },
    MarkGlob,
    UnmarkGlob,
    /// F5 or F6 to a server, or F6 between two panels on one session (P3 5.6): `at` is the
    /// other panel's directory there, which a relative path resolves against.
    ToServer {
        groups: Vec<Group>,
        at: Dest,
        moving: bool,
    },
    /// F7 on a server (P3 5.6): `at` is the panel's directory there.
    MkdirRemote {
        at: Dest,
    },
    /// The F4 write-back question (P3 5.6): the edited view copy and the server file it came
    /// from; `changed`: that file's size or mtime changed since the download.
    WriteBack {
        copy: PathBuf,
        at: Dest,
        changed: bool,
    },
}

/// What a form is for (P2 2.1): the app checks the form on every change and acts on it
/// when it is submitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormPurpose {
    /// Alt+L (P2 8.1). `dir` is the panel's directory, which a relative destination
    /// resolves against.
    Link { dir: PathBuf, groups: Vec<Group> },
    /// Alt+A (P2 8.2). `first` is the first selected entry as the panel lists it, for the
    /// preview line.
    Attr {
        groups: Vec<Group>,
        first: Option<Listed>,
    },
    /// Shift+F2 (P2 7): the two panels on screen.
    Compare,
    /// Alt+F7 (P2 5.1): a search of the active panel's directory.
    Find,
}

/// An entry as the panel lists it: what a form previews without a syscall (P-1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub name: Vec<u8>,
    pub kind: EKind,
    /// Permission bits including setuid, setgid and sticky.
    pub perm: u32,
}

pub enum Dialog {
    Confirm {
        title: String,
        lines: Vec<String>,
        yes: &'static str,
        focus_yes: bool,
        purpose: Purpose,
    },
    /// A question with more answers than yes and no, such as the F4 write-back (P3 5.6):
    /// `Esc` gives the last button.
    Choose {
        title: String,
        lines: Vec<String>,
        buttons: Vec<String>,
        focus: usize,
        purpose: Purpose,
    },
    Input {
        title: String,
        lines: Vec<String>,
        line: Line,
        purpose: Purpose,
    },
    Form {
        form: Form,
        purpose: FormPurpose,
    },
    Question {
        q: Question,
        focus: usize,
        reply: Option<Sender<Answer>>,
        /// The Rename answer's name, while it is being edited.
        rename: Option<Line>,
        /// The typed `delete` confirmation.
        typed: Line,
    },
    Report {
        report: Report,
        scroll: usize,
    },
    Message {
        title: String,
        lines: Vec<String>,
        error: bool,
    },
    Help {
        scroll: usize,
    },
    /// `Ctrl+D`: go to a bookmark or a frequent directory (P2 3.1).
    Dirs(DirsDialog),
    /// `Ctrl+M`: the multi-rename tool (P2 6.1).
    Rename(Box<RenameTool>),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Keep the dialog open.
    Stay,
    Close,
    /// Input or confirmation completed.
    Done(Purpose, Vec<u8>),
    /// A form field changed: the app re-checks the form (preview, errors).
    FormChanged,
    /// `Enter` in a form: the app acts on it, or keeps it open with an error.
    FormSubmit,
    /// A key in the directories dialog that the app acts on (P2 3.1).
    Dirs(DirsAction),
    /// `Ctrl+Z` in the multi-rename tool: undo the last multi-rename (P2 6.5).
    Undo,
    /// A [`Dialog::Choose`] answered with the button of that index.
    Chosen(Purpose, usize),
}

impl Dialog {
    pub fn question(q: Question, reply: Sender<Answer>) -> Dialog {
        let focus = q
            .choices()
            .iter()
            .position(|c| *c == q.default_choice())
            .unwrap_or(0);
        Dialog::Question {
            q,
            focus,
            reply: Some(reply),
            rename: None,
            typed: Line::default(),
        }
    }

    pub fn input(
        title: impl Into<String>,
        lines: Vec<String>,
        initial: &[u8],
        purpose: Purpose,
    ) -> Dialog {
        let mut line = Line::default();
        line.set(initial);
        Dialog::Input {
            title: title.into(),
            lines,
            line,
            purpose,
        }
    }

    pub fn confirm(
        title: impl Into<String>,
        lines: Vec<String>,
        yes: &'static str,
        purpose: Purpose,
    ) -> Dialog {
        Dialog::Confirm {
            title: title.into(),
            lines,
            yes,
            focus_yes: true,
            purpose,
        }
    }

    pub fn choose(
        title: impl Into<String>,
        lines: Vec<String>,
        buttons: Vec<String>,
        purpose: Purpose,
    ) -> Dialog {
        Dialog::Choose {
            title: title.into(),
            lines,
            buttons,
            focus: 0,
            purpose,
        }
    }

    /// Whether this dialog blocks the worker (a question waits for its answer).
    pub fn is_question(&self) -> bool {
        matches!(self, Dialog::Question { .. })
    }

    /// Paste into an input field.
    pub fn paste(&mut self, s: &str) {
        match self {
            Dialog::Input { line, .. } => line.insert_bytes(s.replace('\n', " ").as_bytes()),
            Dialog::Form { form, .. } => {
                form.paste(s);
            }
            Dialog::Dirs(d) => d.paste(s),
            Dialog::Rename(t) => t.paste(s),
            Dialog::Question {
                rename: Some(l), ..
            } => l.insert_bytes(s.as_bytes()),
            Dialog::Question {
                q: Question::ConfirmDelete { .. },
                typed,
                ..
            } => typed.insert_bytes(s.as_bytes()),
            _ => {}
        }
    }

    pub fn handle(&mut self, k: KeyEvent) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match self {
            Dialog::Confirm {
                focus_yes, purpose, ..
            } => match k.code {
                KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                    *focus_yes = !*focus_yes;
                    Outcome::Stay
                }
                KeyCode::Enter if *focus_yes => Outcome::Done(purpose.clone(), Vec::new()),
                KeyCode::Char('y') => Outcome::Done(purpose.clone(), Vec::new()),
                KeyCode::Enter | KeyCode::Esc | KeyCode::Char('n') | KeyCode::F(10) => {
                    Outcome::Close
                }
                _ => Outcome::Stay,
            },
            Dialog::Choose {
                buttons,
                focus,
                purpose,
                ..
            } => {
                let n = buttons.len().max(1);
                match k.code {
                    KeyCode::Left | KeyCode::BackTab => {
                        *focus = (*focus + n - 1) % n;
                        Outcome::Stay
                    }
                    KeyCode::Right | KeyCode::Tab => {
                        *focus = (*focus + 1) % n;
                        Outcome::Stay
                    }
                    KeyCode::Enter => Outcome::Chosen(purpose.clone(), *focus),
                    KeyCode::Esc | KeyCode::F(10) => Outcome::Chosen(purpose.clone(), n - 1),
                    _ => Outcome::Stay,
                }
            }
            Dialog::Input { line, purpose, .. } => match k.code {
                KeyCode::Enter => Outcome::Done(purpose.clone(), line.bytes().to_vec()),
                KeyCode::Esc | KeyCode::F(10) => Outcome::Close,
                _ => {
                    edit(line, k, ctrl);
                    Outcome::Stay
                }
            },
            Dialog::Dirs(d) => d.handle(k),
            Dialog::Rename(t) => t.handle(k),
            Dialog::Form { form, .. } => match form.handle(k) {
                FormEvent::Changed => Outcome::FormChanged,
                FormEvent::Submit => Outcome::FormSubmit,
                FormEvent::Close => Outcome::Close,
                FormEvent::Stay | FormEvent::Unhandled => Outcome::Stay,
            },
            Dialog::Question {
                q,
                focus,
                reply,
                rename,
                typed,
            } => {
                let choices = q.choices();
                if let Some(l) = rename {
                    match k.code {
                        KeyCode::Enter => {
                            let name = OsStr::from_bytes(l.bytes()).to_owned();
                            send(reply, Answer::Rename(name));
                            return Outcome::Close;
                        }
                        KeyCode::Esc => *rename = None,
                        _ => {
                            edit(l, k, ctrl);
                        }
                    }
                    return Outcome::Stay;
                }
                if let Question::ConfirmDelete { .. } = q {
                    // The user must type `delete`; there is no default-Enter path.
                    return match k.code {
                        KeyCode::Enter if typed.bytes() == b"delete" => {
                            send(reply, Answer::Confirm);
                            Outcome::Close
                        }
                        KeyCode::Enter => Outcome::Stay,
                        KeyCode::Esc | KeyCode::F(10) => {
                            send(reply, Answer::Cancel);
                            Outcome::Close
                        }
                        _ => {
                            edit(typed, k, ctrl);
                            Outcome::Stay
                        }
                    };
                }
                match k.code {
                    KeyCode::Left | KeyCode::BackTab => {
                        *focus = (*focus + choices.len() - 1) % choices.len();
                        Outcome::Stay
                    }
                    KeyCode::Right | KeyCode::Tab => {
                        *focus = (*focus + 1) % choices.len();
                        Outcome::Stay
                    }
                    KeyCode::Esc => {
                        // Esc is the safe answer: Skip where offered, else cancel.
                        let a = if choices.contains(&Choice::Skip) {
                            Answer::Skip
                        } else {
                            Answer::Cancel
                        };
                        send(reply, a);
                        Outcome::Close
                    }
                    KeyCode::Enter => {
                        let c = choices[*focus];
                        if c == Choice::Rename {
                            let base = match q {
                                Question::FileExists { path, .. }
                                | Question::DirExists { path, .. }
                                | Question::TypeMismatch { path, .. }
                                | Question::LinkExists { path, .. } => {
                                    path.file_name().map(|n| n.to_owned())
                                }
                                _ => None,
                            }
                            .unwrap_or_default();
                            let mut l = Line::default();
                            l.set(suggest_rename(&base, 1).as_bytes());
                            *rename = Some(l);
                            return Outcome::Stay;
                        }
                        send(reply, answer(c));
                        Outcome::Close
                    }
                    _ => Outcome::Stay,
                }
            }
            Dialog::Report { scroll, .. } | Dialog::Help { scroll } => match k.code {
                KeyCode::Up => {
                    *scroll = scroll.saturating_sub(1);
                    Outcome::Stay
                }
                KeyCode::Down => {
                    *scroll += 1;
                    Outcome::Stay
                }
                KeyCode::PageUp => {
                    *scroll = scroll.saturating_sub(10);
                    Outcome::Stay
                }
                KeyCode::PageDown => {
                    *scroll += 10;
                    Outcome::Stay
                }
                KeyCode::Esc
                | KeyCode::Enter
                | KeyCode::F(1)
                | KeyCode::F(10)
                | KeyCode::Char('q') => Outcome::Close,
                _ => Outcome::Stay,
            },
            Dialog::Message { .. } => match k.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::F(10) => Outcome::Close,
                _ => Outcome::Stay,
            },
        }
    }

    /// A question dialog closed without an answer (the app is quitting): the worker gets
    /// Cancel, so it never blocks forever.
    pub fn abandon(&mut self) {
        if let Dialog::Question { reply, .. } = self {
            send(reply, Answer::Cancel);
        }
    }
}

fn send(reply: &mut Option<Sender<Answer>>, a: Answer) {
    if let Some(r) = reply.take() {
        let _ = r.send(a);
    }
}

fn answer(c: Choice) -> Answer {
    match c {
        Choice::Overwrite => Answer::Overwrite,
        Choice::OverwriteAll => Answer::OverwriteAll,
        Choice::OverwriteAllOlder => Answer::OverwriteAllOlder,
        Choice::Skip => Answer::Skip,
        Choice::SkipAll => Answer::SkipAll,
        Choice::Rename => Answer::Rename(OsString::new()),
        Choice::Merge => Answer::Merge,
        Choice::MergeAll => Answer::MergeAll,
        Choice::Retry => Answer::Retry,
        Choice::SkipAllErrno => Answer::SkipAllErrno,
        Choice::Cancel => Answer::Cancel,
        Choice::DeletePermanently => Answer::DeletePermanently,
        Choice::Confirm => Answer::Confirm,
        Choice::Continue => Answer::Continue,
    }
}

/// Line editing inside a dialog field. Returns whether the key was a line-editing key.
pub(crate) fn edit(l: &mut Line, k: KeyEvent, ctrl: bool) -> bool {
    match k.code {
        KeyCode::Char('a') if ctrl => l.home(),
        KeyCode::Char('e') if ctrl => l.end(),
        KeyCode::Char('u') if ctrl => l.kill_start(),
        KeyCode::Char('k') if ctrl => l.kill_end(),
        KeyCode::Char('w') if ctrl => l.kill_word(),
        KeyCode::Char('h') if ctrl => l.backspace(),
        KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => l.insert_char(c),
        KeyCode::Backspace => l.backspace(),
        KeyCode::Delete => l.delete(),
        KeyCode::Left => l.left(),
        KeyCode::Right => l.right(),
        KeyCode::Home => l.home(),
        KeyCode::End => l.end(),
        _ => return false,
    }
    true
}

// ---- rendering ----------------------------------------------------------------------------

pub fn human_size(b: u64) -> String {
    if b < 10_000 {
        return b.to_string();
    }
    let units = ["K", "M", "G", "T", "P"];
    let mut v = b as f64 / 1024.0;
    let mut u = 0;
    while v >= 1000.0 && u + 1 < units.len() {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1}{}", units[u])
}

fn side_line(label: &str, s: &Side, tz: &jiff::tz::TimeZone) -> String {
    let kind = match s.kind {
        Kind::Dir => "directory".to_string(),
        Kind::Symlink => "symlink".to_string(),
        Kind::File => format!("{} bytes", s.size),
        _ => "special file".to_string(),
    };
    let when = jiff::Timestamp::new(s.mtime.sec, s.mtime.nsec as i32)
        .map(|t| {
            t.to_zoned(tz.clone())
                .strftime("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_default();
    let ro = if s.readonly { ", read-only" } else { "" };
    format!("{label}: {kind}, {when}{ro}")
}

fn question_text(q: &Question, tz: &jiff::tz::TimeZone) -> (String, Vec<String>) {
    let p = |p: &PathBuf| escaped(p.as_os_str().as_bytes());
    match q {
        Question::FileExists {
            path,
            src,
            dst,
            dst_is_symlink,
        } => {
            let mut l = vec![
                p(path),
                side_line("new     ", src, tz),
                side_line("existing", dst, tz),
            ];
            if *dst_is_symlink {
                l.push("The link is replaced; its target is not touched.".into());
            }
            ("File exists".into(), l)
        }
        Question::DirExists { path, src, dst } => (
            "Directory exists".into(),
            vec![
                p(path),
                side_line("new     ", src, tz),
                side_line("existing", dst, tz),
            ],
        ),
        Question::TypeMismatch { path, src, dst } => (
            "Type mismatch".into(),
            vec![
                p(path),
                side_line("new     ", src, tz),
                side_line("existing", dst, tz),
                "A directory never replaces a file, and a file never replaces a directory.".into(),
            ],
        ),
        Question::LinkExists { path, existing } => {
            let mut l = vec![p(path)];
            if let Some(e) = existing {
                l.push(side_line("existing", e, tz));
            }
            l.push("A link never replaces an existing entry.".into());
            ("Link exists".into(), l)
        }
        Question::Error { path, op, errno } => (
            "Error".into(),
            vec![p(path), format!("{op}: {}", errno_text(*errno))],
        ),
        Question::ServerError { path, op, message } => (
            "Error on the server".into(),
            vec![p(path), format!("{op}: {}", escaped(message.as_bytes()))],
        ),
        Question::TrashUnavailable { path, reason } => (
            "No usable trash".into(),
            vec![p(path), reason.clone(), "Nothing was deleted.".into()],
        ),
        Question::ConfirmDelete {
            files,
            dirs,
            bytes,
            single,
        } => {
            let mut l = Vec::new();
            if let Some(s) = single {
                l.push(p(s));
            }
            l.push(format!(
                "Permanently delete {files} files, {dirs} directories, {}?",
                human_size(*bytes)
            ));
            l.push("This cannot be undone. Type delete and press Enter.".into());
            ("Delete permanently".into(), l)
        }
        Question::FreeSpace { path, need, free } => (
            "Not enough free space".into(),
            vec![
                p(path),
                format!(
                    "The sources declare {}; the destination has {} free.",
                    human_size(*need),
                    human_size(*free)
                ),
                "Continue anyway?".into(),
            ],
        ),
    }
}

pub(crate) fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

pub(crate) fn frame_block<'a>(title: &'a str, th: &Theme, error: bool) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(if error { th.error } else { th.dialog_border })
        .title(Span::styled(
            format!(" {title} "),
            if error { th.error } else { th.dialog_border },
        ))
        .style(th.dialog)
}

/// Buttons, wrapped onto as many lines as `width` needs.
fn buttons<'a>(labels: &[&'a str], focus: usize, th: &Theme, width: usize) -> Vec<TLine<'a>> {
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    let mut used = 0;
    for (i, l) in labels.iter().enumerate() {
        let text = format!("[ {l} ]");
        let w = text.chars().count() + 1;
        if used > 0 && used + w > width + 1 {
            lines.push(TLine::from(std::mem::take(&mut spans)));
            used = 0;
        }
        let style = if i == focus {
            th.cursor_active
        } else {
            th.dialog
        };
        spans.push(Span::styled(text, style));
        spans.push(Span::raw(" "));
        used += w;
    }
    if !spans.is_empty() {
        lines.push(TLine::from(spans));
    }
    lines
}

pub(crate) fn line_field(l: &Line, width: usize, th: &Theme) -> (TLine<'static>, u16) {
    let text = l.bytes();
    let before = escaped(&text[..l.cursor()]);
    let all = escaped(text);
    let cur = fit(&before, usize::MAX).1 as u16;
    let (shown, _) = fit(&all, width);
    (
        TLine::from(Span::styled(shown, th.dialog)),
        cur.min(width as u16),
    )
}

/// Draws the dialog over `area`; returns where the cursor goes, if an input is focused.
pub fn draw(
    d: &Dialog,
    f: &mut Frame,
    area: Rect,
    th: &Theme,
    tz: &jiff::tz::TimeZone,
) -> Option<(u16, u16)> {
    let width = (area.width.saturating_sub(4)).clamp(20, 76);
    match d {
        Dialog::Confirm {
            title,
            lines,
            yes,
            focus_yes,
            ..
        } => {
            let h = lines.len() as u16 + 4;
            let r = centered(area, width, h);
            f.render_widget(Clear, r);
            let mut t: Vec<TLine> = lines
                .iter()
                .map(|l| TLine::from(fit(l, width.saturating_sub(2) as usize).0))
                .collect();
            t.push(TLine::default());
            t.extend(buttons(
                &[yes, "Cancel"],
                if *focus_yes { 0 } else { 1 },
                th,
                width.saturating_sub(2) as usize,
            ));
            f.render_widget(Paragraph::new(t).block(frame_block(title, th, false)), r);
            None
        }
        Dialog::Choose {
            title,
            lines,
            buttons: labels,
            focus,
            ..
        } => {
            let inner = width.saturating_sub(2) as usize;
            let mut t: Vec<TLine> = lines.iter().map(|l| TLine::from(fit(l, inner).0)).collect();
            t.push(TLine::default());
            let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
            t.extend(buttons(&labels, *focus, th, inner));
            let r = centered(area, width, t.len() as u16 + 2);
            f.render_widget(Clear, r);
            f.render_widget(Paragraph::new(t).block(frame_block(title, th, false)), r);
            None
        }
        Dialog::Input {
            title, lines, line, ..
        } => {
            let h = lines.len() as u16 + 4;
            let r = centered(area, width, h);
            f.render_widget(Clear, r);
            let inner_w = width.saturating_sub(2) as usize;
            let mut t: Vec<TLine> = lines
                .iter()
                .map(|l| TLine::from(fit(l, inner_w).0))
                .collect();
            let (field, cur) = line_field(line, inner_w, th);
            t.push(field);
            t.push(TLine::from(Span::styled(
                "Enter: OK   Esc: cancel",
                th.metadata,
            )));
            f.render_widget(Paragraph::new(t).block(frame_block(title, th, false)), r);
            Some((r.x + 1 + cur, r.y + 1 + lines.len() as u16))
        }
        Dialog::Form { form, .. } => form.draw(f, area, th).cursor,
        Dialog::Dirs(d) => d.draw(f, area, th),
        Dialog::Rename(t) => t.draw(f, area, th),
        Dialog::Question {
            q,
            focus,
            rename,
            typed,
            ..
        } => {
            let (title, lines) = question_text(q, tz);
            let error = matches!(
                q,
                Question::Error { .. }
                    | Question::ServerError { .. }
                    | Question::ConfirmDelete { .. }
            );
            let inner_w = width.saturating_sub(2) as usize;
            let mut t: Vec<TLine> = lines
                .iter()
                .map(|l| TLine::from(fit(l, inner_w).0))
                .collect();
            t.push(TLine::default());
            // The field's row and column offset inside the dialog, when there is one.
            let mut field_at = None;
            if let Some(l) = rename {
                let (field, cur) = line_field(l, inner_w, th);
                field_at = Some((t.len() as u16, cur));
                t.push(field);
                t.push(TLine::from(Span::styled(
                    "New name. Enter: OK   Esc: back",
                    th.metadata,
                )));
            } else if let Question::ConfirmDelete { .. } = q {
                let (field, cur) = line_field(typed, inner_w.saturating_sub(2), th);
                field_at = Some((t.len() as u16, cur + 2));
                let mut spans = vec![Span::styled("> ", th.dialog_border)];
                spans.extend(field.spans);
                t.push(TLine::from(spans));
                t.push(TLine::from(Span::styled("Esc: cancel", th.metadata)));
            } else {
                let labels: Vec<&str> = q.choices().iter().map(|c| c.label()).collect();
                t.extend(buttons(&labels, *focus, th, inner_w));
            }
            let r = centered(area, width, t.len() as u16 + 2);
            f.render_widget(Clear, r);
            f.render_widget(Paragraph::new(t).block(frame_block(&title, th, error)), r);
            field_at.map(|(row, col)| (r.x + 1 + col, r.y + 1 + row))
        }
        Dialog::Report { report, scroll } => {
            let mut lines = vec![report.summary()];
            for n in &report.notes {
                lines.push(n.clone());
            }
            for i in &report.issues {
                let (tag, why) = match &i.outcome {
                    EntryOutcome::Skipped(w) => ("skipped", w),
                    EntryOutcome::Failed(w) => ("failed ", w),
                };
                lines.push(format!(
                    "{tag} {}: {why}",
                    escaped(i.path.as_os_str().as_bytes())
                ));
            }
            let h = (lines.len() as u16 + 3)
                .min(area.height.saturating_sub(2))
                .max(5);
            let r = centered(area, area.width.saturating_sub(4).max(20), h);
            f.render_widget(Clear, r);
            let body = h.saturating_sub(3) as usize;
            let max = lines.len().saturating_sub(body);
            let start = (*scroll).min(max);
            let mut t: Vec<TLine> = lines
                .iter()
                .skip(start)
                .take(body)
                .map(|l| TLine::from(fit(l, r.width.saturating_sub(2) as usize).0))
                .collect();
            t.push(TLine::from(Span::styled(
                "Up/Down: scroll   Enter: close",
                th.metadata,
            )));
            let error = report.failed > 0 || report.refused.is_some();
            f.render_widget(Paragraph::new(t).block(frame_block("Report", th, error)), r);
            None
        }
        Dialog::Message {
            title,
            lines,
            error,
        } => {
            let h = lines.len() as u16 + 3;
            let r = centered(area, width, h);
            f.render_widget(Clear, r);
            let mut t: Vec<TLine> = lines
                .iter()
                .map(|l| TLine::from(fit(l, width.saturating_sub(2) as usize).0))
                .collect();
            t.push(TLine::from(Span::styled("Enter: close", th.metadata)));
            f.render_widget(Paragraph::new(t).block(frame_block(title, th, *error)), r);
            None
        }
        Dialog::Help { scroll } => {
            let r = centered(
                area,
                area.width.saturating_sub(2).max(20),
                area.height.saturating_sub(2).max(5),
            );
            f.render_widget(Clear, r);
            let body = r.height.saturating_sub(3) as usize;
            let max = super::help::TEXT.len().saturating_sub(body);
            let start = (*scroll).min(max);
            let mut t: Vec<TLine> = super::help::TEXT
                .iter()
                .skip(start)
                .take(body)
                .map(|l| TLine::from(fit(l, r.width.saturating_sub(2) as usize).0))
                .collect();
            t.push(TLine::from(Span::styled(
                "Up/Down: scroll   F1/Esc: close",
                th.metadata,
            )));
            f.render_widget(
                Paragraph::new(t).block(frame_block(&super::help::title(), th, false)),
                r,
            );
            None
        }
    }
}

/// The report line for the status row, when the report does not need the list view.
pub fn summary_style(r: &Report, th: &Theme) -> Style {
    if r.failed > 0 || r.refused.is_some() {
        th.error
    } else if r.skipped > 0 || r.cancelled {
        th.warning
    } else {
        th.normal
    }
}
