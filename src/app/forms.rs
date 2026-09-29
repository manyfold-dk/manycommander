#![forbid(unsafe_code)]
//! The link, attributes, compare and find forms (P2 8.1, 8.2, 7, 5.1): built from panel
//! state, checked on every change, turned into a job, a compare or a search when submitted.
//! Like every dialog they make no filesystem syscall (P-1): the attributes preview uses the
//! metadata the panel already lists, and a compare request copies the panels' visible
//! entries.

use super::event::Effect;
use super::jobs::{At, compare_refusal};
use super::search::{FIND_CASE, FIND_DIR, FIND_HIDDEN, FIND_NAME, FIND_STAY, FIND_TEXT};
use super::{App, CompareUi};
use crate::compare::{self, Mode, Request};
use crate::find::FindSpec;
use crate::fsops::attr::{GRAMMAR, ModeChange, perm_text};
use crate::fsops::group::Group;
use crate::fsops::job::JobSpec;
use crate::fsops::link::LinkKind;
use crate::fsops::sys::Ts;
use crate::panel::entry::EKind;
use crate::panel::{Panel, join_lexical};
use crate::ui::dialog::{Dialog, FormPurpose, Listed};
use crate::ui::form::Form;
use crate::ui::text::escaped;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// The link form's fields.
pub const LINK_DST: usize = 0;
pub const LINK_KIND: usize = 1;
/// The link choice's options, in display order; the first is the default (P2 8.1).
pub const LINK_KINDS: [(&str, LinkKind); 3] = [
    ("symbolic, relative", LinkKind::Relative),
    ("symbolic, absolute", LinkKind::Absolute),
    ("hard", LinkKind::Hard),
];

/// The attributes form's fields.
pub const ATTR_MODE: usize = 0;
pub const ATTR_TIME: usize = 1;
pub const ATTR_RECURSIVE: usize = 2;

/// The compare form's fields (P2 7).
pub const COMPARE_MODE: usize = 0;
pub const COMPARE_DIRS: usize = 1;
/// The compare choice's options, in display order; the first is the default.
pub const COMPARE_MODES: [(&str, Mode); 2] = [
    ("by date and size", Mode::DateSize),
    ("by content", Mode::Content),
];

impl App {
    /// Alt+L: the link form (P2 8.1). The destination is, for one entry,
    /// `<other panel>/<name>`, for several the other panel's directory.
    pub(super) fn link_form(&mut self) -> Vec<Effect> {
        let p = self.panel();
        let groups = p.selection_groups();
        if groups.is_empty() {
            return Vec::new();
        }
        let dir = p.dir.clone();
        let total: usize = groups.iter().map(|g| g.names.len()).sum();
        let mut dst = self.other().dir.as_os_str().as_bytes().to_vec();
        if !dst.ends_with(b"/") {
            dst.push(b'/');
        }
        if total == 1 {
            dst.extend_from_slice(groups[0].names[0].as_bytes());
        }
        let kinds = LINK_KINDS.map(|(label, _)| label);
        let form = Form::new("Create link")
            .line(format!("Link {} as:", super::count_text(&groups)))
            .text("Destination", &dst)
            .choice("Type", &kinds, 0)
            .help("A link never replaces an existing entry.");
        self.dialog = Some(Dialog::Form {
            form,
            purpose: FormPurpose::Link { dir, groups },
        });
        Vec::new()
    }

    /// Alt+A: the attributes form (P2 8.2). The mode field starts empty (unchanged); the
    /// current octal mode of a single selected entry is shown next to it as a hint, so an
    /// untouched field never applies a mode recursively.
    pub(super) fn attr_form(&mut self) -> Vec<Effect> {
        let p = self.panel();
        let groups = p.selection_groups();
        if groups.is_empty() {
            return Vec::new();
        }
        let sel = p.selection();
        let first = sel.first().and_then(|n| {
            let i = p.list.find(n.as_bytes())?;
            let e = p.list.entries[i as usize];
            Some(Listed {
                name: n.as_bytes().to_vec(),
                kind: e.kind,
                perm: e.perm as u32,
            })
        });
        let label = match &first {
            Some(f) if sel.len() == 1 && f.kind != EKind::Symlink => {
                format!("Mode (now {:04o})", f.perm)
            }
            _ => "Mode".to_string(),
        };
        let form = Form::new("Change attributes")
            .line(format!("Change {}", super::count_text(&groups)))
            .text(&label, b"")
            .text("Modification time", b"")
            .check(
                "Recursive: also everything below selected directories",
                false,
            )
            .help(format!("Mode: {GRAMMAR}."))
            .help("No class means a, without the umask. X: execute where a directory or")
            .help("already executable. Time: YYYY-MM-DD HH:MM[:SS] (local) or now.")
            .help("Empty: unchanged. Symbolic links keep their mode.");
        self.dialog = Some(Dialog::Form {
            form,
            purpose: FormPurpose::Attr { groups, first },
        });
        self.check_form();
        Vec::new()
    }

    /// Shift+F2: the compare form (P2 7). It needs two directory panels; a results tab
    /// refuses the key itself (P2 5.4). An archive or a server compares by date and size
    /// only (P3 2.4).
    pub(super) fn compare_form(&mut self) -> Vec<Effect> {
        if self.sides.iter().any(|s| s.panel().is_results()) {
            self.warn("compare needs two directory panels");
            return Vec::new();
        }
        let modes = COMPARE_MODES.map(|(label, _)| label);
        let form = Form::new("Compare directories")
            .line("Mark the entries that differ between the two panels.")
            .choice("Compare", &modes, 0)
            .check("include directories", true)
            .help("Existing marks are replaced. Hidden and filtered-out entries")
            .help("do not take part.");
        self.dialog = Some(Dialog::Form {
            form,
            purpose: FormPurpose::Compare,
        });
        Vec::new()
    }

    /// The compare the submitted form asks for: a copy of both panels' visible entries
    /// (I-8), cheap enough for the UI thread (P-13). `Err` keeps the form open.
    fn compare_request(&mut self, mode: Mode, include_dirs: bool) -> Result<Request, String> {
        let [l, r] = [0, 1].map(|s| At::of(self.sides[s].panel()));
        if let Some(why) = compare_refusal(mode == Mode::Content, l, r) {
            return Err(why.into());
        }
        if self
            .sides
            .iter()
            .any(|s| s.panel().listing_generation().is_none())
        {
            return Err("a panel is still loading; compare when it is done".into());
        }
        self.next_compare += 1;
        Ok(Request {
            id: self.next_compare,
            mode,
            include_dirs,
            left: compare_side(self.sides[0].panel()),
            right: compare_side(self.sides[1].panel()),
        })
    }

    /// Re-checks the open form after a change: the preview line, and the error of the last
    /// `Enter` is cleared.
    pub(super) fn check_form(&mut self) {
        if let Some(Dialog::Rename(t)) = self.dialog.as_mut() {
            t.refresh();
            return;
        }
        let Some(Dialog::Form { form, purpose }) = self.dialog.as_mut() else {
            return;
        };
        form.error = None;
        if let FormPurpose::Attr { first, .. } = purpose {
            form.status = attr_preview(first.as_ref(), form.text_of(ATTR_MODE));
        }
    }

    /// `Enter` in a form: start its job, or keep it open with the error.
    pub(super) fn submit_form(&mut self) -> Vec<Effect> {
        if let Some(Dialog::Rename(_)) = self.dialog {
            return self.submit_rename();
        }
        let Some(Dialog::Form { form, purpose }) = self.dialog.as_ref() else {
            return Vec::new();
        };
        let spec = match purpose {
            FormPurpose::Find => {
                let r = find_spec(&self.panel().dir, form).and_then(|spec| self.start_find(spec));
                return match r {
                    Ok(fx) => {
                        self.dialog = None;
                        fx
                    }
                    Err(e) => {
                        if let Some(Dialog::Form { form, .. }) = self.dialog.as_mut() {
                            form.error = Some(e);
                        }
                        Vec::new()
                    }
                };
            }
            FormPurpose::Link { dir, groups } => link_spec(dir, groups, form),
            FormPurpose::Attr { groups, .. } => attr_spec(groups, form, &self.tz, now()),
            FormPurpose::Compare => {
                let mode = COMPARE_MODES
                    .get(form.chosen(COMPARE_MODE))
                    .map(|(_, m)| *m)
                    .unwrap_or_default();
                let include_dirs = form.checked(COMPARE_DIRS);
                return match self.compare_request(mode, include_dirs) {
                    Ok(req) => {
                        self.dialog = None;
                        // A compare still running is cancelled by the runtime; its late
                        // events no longer match the id.
                        self.compare = Some(CompareUi {
                            id: req.id,
                            mode: req.mode,
                            progress: None,
                        });
                        vec![Effect::Compare(req)]
                    }
                    Err(e) => {
                        if let Some(Dialog::Form { form, .. }) = self.dialog.as_mut() {
                            form.error = Some(e);
                        }
                        Vec::new()
                    }
                };
            }
        };
        match spec {
            Ok(spec) => {
                self.dialog = None;
                self.start_job(spec)
            }
            Err(e) => {
                if let Some(Dialog::Form { form, .. }) = self.dialog.as_mut() {
                    form.error = Some(e);
                }
                Vec::new()
            }
        }
    }
}

/// One panel's visible entries, in display order, for a compare request (P2 7, I-8).
pub fn compare_side(p: &Panel) -> compare::Side {
    let mut s = compare::Side::new(p.dir.clone(), p.slot, p.generation);
    let vis = &p.list.visible;
    s.reserve(vis.len(), p.list.names.len());
    for &i in vis {
        let e = &p.list.entries[i as usize];
        let mtime = Ts {
            sec: e.mtime,
            nsec: e.mtime_ns,
        };
        s.push(i, e.name(&p.list.names), e.kind, e.size, mtime);
    }
    s
}

/// The current time, for `now`.
fn now() -> Ts {
    let t = jiff::Timestamp::now();
    Ts {
        sec: t.as_second(),
        nsec: t.subsec_nanosecond().max(0) as u32,
    }
}

fn blank(b: &[u8]) -> bool {
    b.iter().all(|c| c.is_ascii_whitespace())
}

/// The link job a submitted form asks for. A relative destination resolves against the
/// panel's directory, as for copy.
fn link_spec(dir: &Path, groups: &[Group], form: &Form) -> Result<JobSpec, String> {
    let text = form.text_of(LINK_DST);
    if text.is_empty() {
        return Err("the destination is empty".into());
    }
    let dst = join_lexical(dir, Path::new(OsStr::from_bytes(text)));
    let kind = LINK_KINDS
        .get(form.chosen(LINK_KIND))
        .map(|(_, k)| *k)
        .unwrap_or_default();
    Ok(JobSpec::Link {
        groups: groups.to_vec(),
        dst,
        kind,
    })
}

/// The search a submitted find form asks for (P2 5.1). A relative "Search in" resolves
/// against the panel's directory; an empty "Containing text" searches names only.
fn find_spec(dir: &Path, form: &Form) -> Result<FindSpec, String> {
    let d = form.text_of(FIND_DIR);
    if blank(d) {
        return Err("Search in: the directory is empty".into());
    }
    let text = form.text_of(FIND_TEXT);
    Ok(FindSpec {
        root: join_lexical(dir, Path::new(OsStr::from_bytes(d))),
        name: form.text_of(FIND_NAME).to_vec(),
        content: (!text.is_empty()).then(|| text.to_vec()),
        hidden: form.checked(FIND_HIDDEN),
        stay_on_fs: form.checked(FIND_STAY),
        match_case: form.checked(FIND_CASE),
    })
}

/// The attributes job a submitted form asks for; `Err` blocks `Enter` (P2 8.2).
fn attr_spec(
    groups: &[Group],
    form: &Form,
    tz: &jiff::tz::TimeZone,
    now: Ts,
) -> Result<JobSpec, String> {
    let text = form.text_of(ATTR_MODE);
    let mode = if blank(text) {
        None
    } else {
        Some(ModeChange::parse(text).map_err(|e| format!("Mode: {e}"))?)
    };
    let mtime = parse_mtime(form.text_of(ATTR_TIME), tz, now)
        .map_err(|e| format!("Modification time: {e}"))?;
    if mode.is_none() && mtime.is_none() {
        return Err("set a mode or a modification time".into());
    }
    Ok(JobSpec::Attr {
        groups: groups.to_vec(),
        mode,
        mtime,
        recursive: form.checked(ATTR_RECURSIVE),
    })
}

/// The preview line for the first selected entry: `name: rw-r--r-- -> rwxr-xr-x`.
pub fn attr_preview(first: Option<&Listed>, mode: &[u8]) -> Vec<String> {
    let Some(f) = first else {
        return Vec::new();
    };
    let name = escaped(&f.name);
    if f.kind == EKind::Symlink {
        return vec![format!("{name}: a symbolic link keeps its mode")];
    }
    let old = perm_text(f.perm);
    if blank(mode) {
        return vec![format!("{name}: {old}")];
    }
    match ModeChange::parse(mode) {
        Ok(m) => {
            let new = m.apply(f.perm, f.kind == EKind::Dir);
            vec![format!("{name}: {old} -> {}", perm_text(new))]
        }
        Err(e) => vec![format!("Mode: {e}")],
    }
}

/// Parses the modification time field (P2 8.2): `YYYY-MM-DD HH:MM[:SS]` in `tz`, or `now`.
/// Empty means unchanged (`None`).
pub fn parse_mtime(text: &[u8], tz: &jiff::tz::TimeZone, now: Ts) -> Result<Option<Ts>, String> {
    let s = std::str::from_utf8(text)
        .map_err(|_| "expected YYYY-MM-DD HH:MM[:SS] or now".to_string())?
        .trim();
    if s.is_empty() {
        return Ok(None);
    }
    if s == "now" {
        return Ok(Some(now));
    }
    let bad = || format!("{s:?}: expected YYYY-MM-DD HH:MM[:SS] or now");
    let num = |p: &str, len: usize| -> Option<i64> {
        (p.len() == len && p.bytes().all(|c| c.is_ascii_digit()))
            .then(|| p.parse().ok())
            .flatten()
    };
    let mut parts = s.split_ascii_whitespace();
    let (Some(date), Some(time), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(bad());
    };
    let d: Vec<&str> = date.split('-').collect();
    let t: Vec<&str> = time.split(':').collect();
    if d.len() != 3 || !(2..=3).contains(&t.len()) {
        return Err(bad());
    }
    let field = |p: &str, len| num(p, len).ok_or_else(bad);
    let (y, mo, da) = (field(d[0], 4)?, field(d[1], 2)?, field(d[2], 2)?);
    let (h, mi) = (field(t[0], 2)?, field(t[1], 2)?);
    let sec = if t.len() == 3 { field(t[2], 2)? } else { 0 };
    let dt = jiff::civil::DateTime::new(
        y as i16, mo as i8, da as i8, h as i8, mi as i8, sec as i8, 0,
    )
    .map_err(|e| format!("{s}: {e}"))?;
    let ts = dt
        .to_zoned(tz.clone())
        .map_err(|e| format!("{s}: {e}"))?
        .timestamp();
    Ok(Some(Ts {
        sec: ts.as_second(),
        nsec: ts.subsec_nanosecond().max(0) as u32,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::tz::TimeZone;

    const NOW: Ts = Ts {
        sec: 1_790_000_000,
        nsec: 5,
    };

    #[test]
    fn times_parse_in_the_local_zone() {
        let utc = TimeZone::UTC;
        let at = |s: &str, tz: &TimeZone| parse_mtime(s.as_bytes(), tz, NOW);
        // 2026-09-28 12:34:00 UTC.
        let base = 1_790_598_840;
        assert_eq!(at("2026-09-28 12:34", &utc).unwrap().unwrap().sec, base);
        assert_eq!(
            at(" 2026-09-28  12:34:56 ", &utc).unwrap().unwrap().sec,
            base + 56
        );
        let plus2 = TimeZone::fixed(jiff::tz::offset(2));
        assert_eq!(at("2026-09-28 14:34", &plus2).unwrap().unwrap().sec, base);
        assert_eq!(at("", &utc), Ok(None));
        assert_eq!(at("   ", &utc), Ok(None));
        assert_eq!(at("now", &utc), Ok(Some(NOW)));
        for bad in [
            "2026-09-28",
            "12:34",
            "2026-9-28 12:34",
            "2026-09-28 12:34:5",
            "2026-02-30 00:00",
            "2026-09-28 24:00",
            "2026-09-28T12:34",
            "2026-09-28 12:34 x",
            "yesterday",
            "NOW",
        ] {
            assert!(at(bad, &utc).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn preview_shows_the_first_entry_before_and_after() {
        let file = Listed {
            name: b"run.sh".to_vec(),
            kind: EKind::File,
            perm: 0o644,
        };
        assert_eq!(
            attr_preview(Some(&file), b"u+x,go+X"),
            ["run.sh: rw-r--r-- -> rwxr-xr-x"]
        );
        assert_eq!(attr_preview(Some(&file), b""), ["run.sh: rw-r--r--"]);
        assert!(attr_preview(Some(&file), b"u+q")[0].starts_with("Mode: "));
        let dir = Listed {
            name: b"d".to_vec(),
            kind: EKind::Dir,
            perm: 0o700,
        };
        assert_eq!(
            attr_preview(Some(&dir), b"go+X"),
            ["d: rwx------ -> rwx--x--x"]
        );
        let link = Listed {
            name: b"l".to_vec(),
            kind: EKind::Symlink,
            perm: 0o777,
        };
        assert_eq!(
            attr_preview(Some(&link), b"644"),
            ["l: a symbolic link keeps its mode"]
        );
        assert!(attr_preview(None, b"644").is_empty());
    }

    #[test]
    fn find_spec_reads_the_form() {
        let form = |dir: &str, name: &str, text: &str| {
            Form::new("t")
                .text("Search in", dir.as_bytes())
                .text("Name", name.as_bytes())
                .text("Containing text", text.as_bytes())
                .check("Hidden entries", false)
                .check("Stay on this filesystem", true)
                .check("Match case", true)
        };
        let s = find_spec(Path::new("/p"), &form("sub/../x", "*.rs", "")).unwrap();
        assert_eq!(
            s,
            FindSpec {
                root: "/p/x".into(),
                name: b"*.rs".to_vec(),
                content: None,
                hidden: false,
                stay_on_fs: true,
                match_case: true,
            }
        );
        let s = find_spec(Path::new("/p"), &form("/abs", "", "TODO")).unwrap();
        assert_eq!(s.root, Path::new("/abs"));
        assert_eq!(s.content.as_deref(), Some(&b"TODO"[..]));
        assert!(find_spec(Path::new("/p"), &form(" ", "", "")).is_err());
    }

    #[test]
    fn attr_spec_blocks_bad_input() {
        let g = vec![Group::new("/d", vec!["a".into()])];
        let form = |mode: &str, time: &str| {
            Form::new("t")
                .text("Mode", mode.as_bytes())
                .text("Modification time", time.as_bytes())
                .check("Recursive", true)
        };
        let utc = TimeZone::UTC;
        assert!(
            attr_spec(&g, &form("", ""), &utc, NOW).is_err(),
            "nothing to change"
        );
        assert!(
            attr_spec(&g, &form("9", ""), &utc, NOW)
                .unwrap_err()
                .starts_with("Mode: ")
        );
        assert!(
            attr_spec(&g, &form("", "soon"), &utc, NOW)
                .unwrap_err()
                .starts_with("Modification time: ")
        );
        assert_eq!(
            attr_spec(&g, &form(" ", "now"), &utc, NOW),
            Ok(JobSpec::Attr {
                groups: g.clone(),
                mode: None,
                mtime: Some(NOW),
                recursive: true,
            })
        );
    }
}
