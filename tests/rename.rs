//! T7: multi-rename (P2 6). A-MR-1 (the mask engine and the preview checks), A-MR-2 (swaps,
//! cycles, chains, a chain blocked by an outside entry, a mix), A-MR-3 (targets held
//! outside the set, invalid and duplicate names refused before any write), A-MR-4 (failpoints
//! at every rename step, error and cancel), A-MR-5 (undo), A-MR-6 (a case-insensitive tmpfs
//! directory under `unshare -rm`), and the tool in the app: `Ctrl+M` in a directory panel and
//! in a results tab, the preview, errors that block `Enter`, and `Ctrl+Z`.

mod common;

use common::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jiff::tz::TimeZone;
use manycommander::app::App;
use manycommander::app::event::{Effect, Event, JobEvent};
use manycommander::config::Config;
use manycommander::find;
use manycommander::fsops::group::Group;
use manycommander::fsops::job::{JobSpec, JobVerb, Outcome, Report, run_guarded};
use manycommander::fsops::sys::{Sys, Ts};
use manycommander::panel::listing;
use manycommander::rename::{
    CaseMode, Counter, Directory, Entry, Item, Preview, Problem, Rules, Search, Settings, Status,
    preview, split_ext,
};
use manycommander::theme::Depth;
use manycommander::ui::dialog::Dialog;
use manycommander::ui::multirename::RenameTool;
use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

// ---- A-MR-1: the mask engine ------------------------------------------------------------

/// 2026-01-02 03:04:05 UTC.
fn t0() -> Ts {
    let ts = jiff::civil::date(2026, 1, 2)
        .at(3, 4, 5, 0)
        .to_zoned(TimeZone::UTC)
        .unwrap()
        .timestamp();
    Ts {
        sec: ts.as_second(),
        nsec: 0,
    }
}

fn settings(f: impl FnOnce(&mut Settings)) -> Settings {
    let mut s = Settings::default();
    f(&mut s);
    s
}

fn rules(f: impl FnOnce(&mut Settings)) -> Rules {
    settings(f).compile(&TimeZone::UTC).unwrap()
}

fn compile_error(f: impl FnOnce(&mut Settings)) -> String {
    settings(f).compile(&TimeZone::UTC).unwrap_err()
}

fn apply_at(r: &Rules, name: &[u8], index: usize) -> Vec<u8> {
    r.new_name(&Item {
        name,
        parent: b"photos",
        mtime: t0(),
        index,
    })
    .unwrap()
}

fn apply(r: &Rules, name: &str) -> String {
    String::from_utf8(apply_at(r, name.as_bytes(), 0)).unwrap()
}

/// The new name of `name` with these two masks.
fn masks(name_mask: &str, ext_mask: &str, name: &str) -> String {
    let r = rules(|s| {
        s.name_mask = name_mask.into();
        s.ext_mask = ext_mask.into();
    });
    apply(&r, name)
}

#[test]
fn a_mr_1_name_and_extension_placeholders_with_clamped_ranges() {
    assert_eq!(split_ext(b"photo.jpg"), (&b"photo"[..], &b"jpg"[..]));
    assert_eq!(split_ext(b".bashrc"), (&b".bashrc"[..], &b""[..]));
    assert_eq!(split_ext(b"a."), (&b"a."[..], &b""[..]));
    assert_eq!(split_ext(b"x.tar.gz"), (&b"x.tar"[..], &b"gz"[..]));
    assert_eq!(masks("[N]", "", "photo.jpg"), "photo");
    assert_eq!(masks("[E]", "", "photo.jpg"), "jpg");
    assert_eq!(masks("[N2]", "", "abcdef"), "b");
    assert_eq!(masks("[N2-5]", "", "abcdef"), "bcde");
    assert_eq!(masks("[N2-]", "", "abcdef"), "bcdef");
    assert_eq!(masks("[N-3]", "", "abcdef"), "def");
    // Ranges clamp to the text (P2 6.2).
    assert_eq!(masks("[N2-5]", "", "ab"), "b");
    assert_eq!(masks("x[N4]", "", "ab"), "x");
    assert_eq!(masks("[N-3]", "", "ab"), "ab");
    assert_eq!(masks("x[N3-]", "", "ab"), "x");
    assert_eq!(masks("[N1-1]", "", "ab"), "a");
    assert_eq!(masks("x", "[E1-3]", "a.jpeg"), "x.jpe");
    assert_eq!(masks("x", "[E2]", "a.jpeg"), "x.p");
    assert_eq!(masks("x", "[E-2]", "a.jpeg"), "x.eg");
    assert_eq!(masks("x", "[E2-]", "a.jpeg"), "x.peg");
    assert_eq!(
        masks("x", "[E9]", "a.jpeg"),
        "x",
        "an empty extension has no dot"
    );
    // Characters are Unicode scalar values.
    assert_eq!(masks("[N2]", "", "äöü.txt"), "ö");
    assert_eq!(masks("[N-2]", "[E]", "a\u{1F600}é.txt"), "\u{1F600}é.txt");
    // Mixed text, brackets and several placeholders.
    assert_eq!(masks("[[[N]]]_[E]", "[E]", "photo.jpg"), "[photo]_jpg.jpg");
    assert_eq!(masks("]][N2-3][[", "", "abcd"), "]bc[");
    // An empty extension mask means no extension and no dot.
    assert_eq!(masks("[N]", "", "photo.jpg"), "photo");
    assert_eq!(masks("[N]", "[E]", "README"), "README");
    // The default masks give every name back unchanged.
    let id = rules(|_| {});
    for n in [
        "photo.jpg",
        ".bashrc",
        "a.",
        "...",
        "a..b",
        "x.tar.gz",
        "noext",
        "..x",
    ] {
        assert_eq!(apply(&id, n), n);
    }
}

#[test]
fn a_mr_1_counter_parent_and_dates() {
    let c = |start, step, digits, index| {
        let r = rules(|s| {
            s.name_mask = b"[N]_[C]".to_vec();
            s.counter = Counter {
                start,
                step,
                digits,
            };
        });
        String::from_utf8(apply_at(&r, b"f.txt", index)).unwrap()
    };
    assert_eq!(c(1, 1, 1, 0), "f_1.txt");
    assert_eq!(c(1, 1, 1, 9), "f_10.txt", "a counter outgrows its digits");
    assert_eq!(c(7, 5, 3, 2), "f_017.txt");
    assert_eq!(c(0, 0, 2, 5), "f_00.txt");
    assert_eq!(
        c(u64::MAX, u64::MAX, 1, 2),
        format!("f_{}.txt", u64::MAX as u128 * 3),
        "no overflow"
    );
    assert_eq!(masks("[P]-[N]", "[E]", "a.txt"), "photos-a.txt");
    // The modification time in the local time zone, zero-padded.
    assert_eq!(
        masks("[Y]-[M]-[D] [h]:[m]:[s]", "", "x"),
        "2026-01-02 03:04:05"
    );
    let utc = rules(|s| s.name_mask = b"[D] [h]".to_vec());
    let r = settings(|s| s.name_mask = b"[D] [h]".to_vec())
        .compile(&TimeZone::fixed(jiff::tz::offset(-4)))
        .unwrap();
    let item = |r: &Rules| {
        String::from_utf8(
            r.new_name(&Item {
                name: b"x",
                parent: b"",
                mtime: t0(),
                index: 0,
            })
            .unwrap(),
        )
        .unwrap()
    };
    assert_eq!(item(&utc), "02 03");
    assert_eq!(item(&r), "01 23", "four hours west, the day before");
    // A time outside the calendar is a row error, not a panic.
    let r = rules(|s| s.name_mask = b"[Y]".to_vec());
    let far = Item {
        name: b"x",
        parent: b"",
        mtime: Ts {
            sec: i64::MAX,
            nsec: 0,
        },
        index: 0,
    };
    assert_eq!(r.new_name(&far), Err(Problem::Time));
}

#[test]
fn a_mr_1_unknown_and_malformed_placeholders_are_mask_errors() {
    for bad in [
        "[X]", "[n]", "[N0]", "[N5-2]", "[N-]", "[N-0]", "[N2-0]", "[N2--3]", "[Nx]", "[C2]",
        "[P1]", "[E-]", "[N", "a]b", "[]", "[N,2]", "[ N]",
    ] {
        let e = compile_error(|s| s.name_mask = bad.into());
        assert!(e.starts_with("Name mask: "), "{bad}: {e}");
        let e = compile_error(|s| s.ext_mask = bad.into());
        assert!(e.starts_with("Extension mask: "), "{bad}: {e}");
    }
    assert!(compile_error(|s| s.name_mask = b"[X]".to_vec()).contains("unknown placeholder"));
    let e = compile_error(|s| s.counter.digits = 0);
    assert!(e.starts_with("Counter digits"), "{e}");
    assert!(compile_error(|s| s.counter.digits = 21).starts_with("Counter digits"));
}

#[test]
fn a_mr_1_literal_search_and_replace() {
    let lit = |pattern: &str, with: &str, match_case: bool, name: &str| {
        let r = rules(|s| {
            s.search = pattern.into();
            s.replace = with.into();
            s.match_case = match_case;
        });
        apply(&r, name)
    };
    assert_eq!(lit("x", "_", false, "aXbxc"), "a_b_c", "ASCII case folded");
    assert_eq!(lit("x", "_", true, "aXbxc"), "aXb_c");
    assert_eq!(lit("aa", "b", false, "aaaa"), "bb", "non-overlapping");
    assert_eq!(lit("aa", "b", false, "aaa"), "ba", "left to right");
    assert_eq!(
        lit(".TXT", ".md", false, "a.txt"),
        "a.md",
        "the whole new name"
    );
    assert_eq!(lit("$1", "[x]", false, "a$1"), "a[x]", "no expansion");
    assert_eq!(
        lit("ä", "a", false, "ÄäÄ"),
        "ÄaÄ",
        "only ASCII letters fold"
    );
    assert_eq!(
        lit("", "x", false, "abc"),
        "abc",
        "an empty search replaces nothing"
    );
    // Masks first, then search and replace, then the case.
    let r = rules(|s| {
        s.name_mask = b"[N]-X".to_vec();
        s.search = b"x".to_vec();
        s.replace = b"y".to_vec();
        s.case = CaseMode::Upper;
    });
    assert_eq!(apply(&r, "name.md"), "NAME-Y.MD");
    assert_eq!(
        apply(&r, "name.txt"),
        "NAME-Y.TYT",
        "the search covers the extension"
    );
}

#[test]
fn a_mr_1_regex_search_with_groups_and_case_folding() {
    let re = |pattern: &str, with: &str, match_case: bool, name: &str| {
        let r = rules(|s| {
            s.search = pattern.into();
            s.replace = with.into();
            s.regex = true;
            s.match_case = match_case;
        });
        apply(&r, name)
    };
    assert_eq!(
        re(r"IMG_(\d+)", "photo-$1", true, "IMG_1234.jpg"),
        "photo-1234.jpg"
    );
    assert_eq!(
        re(r"(?P<y>\d{4})-(\d\d)", "${2}_${y}", true, "2026-09 trip"),
        "09_2026 trip"
    );
    assert_eq!(re(r"(\d)", "$1a", true, "x7"), "x7a", "one digit after $");
    assert_eq!(re(r"\d", "$$", true, "a1b2"), "a$b$");
    assert_eq!(
        re(r"(a)|(b)", "[$2]", true, "ab"),
        "[][b]",
        "an unmatched group"
    );
    assert_eq!(re("jpg$", "png", false, "img.JPG"), "img.png");
    assert_eq!(re("jpg$", "png", true, "img.JPG"), "img.JPG");
    assert_eq!(re("é", "e", false, "CAFÉ"), "CAFe", "Unicode case folding");
    assert_eq!(re(r"\s+", "_", true, "a  b\tc"), "a_b_c");
    assert_eq!(re("^", "x", true, "ab"), "xab", "an empty match");
    // Errors: the pattern, the replacement, the size limit.
    let err = |pattern: &str, with: &str| {
        compile_error(|s| {
            s.search = pattern.into();
            s.replace = with.into();
            s.regex = true;
        })
    };
    let e = err("(", "");
    assert!(e.starts_with("Search: "), "{e}");
    assert!(!e.contains('\n'), "one line: {e}");
    assert!(err(r"(\d)", "$2").starts_with("Replace: "));
    assert!(err(r"(\d)", "${name}").starts_with("Replace: "));
    let e = err(r"\w{100}", "");
    assert!(e.contains("1 MiB"), "{e}");
    let bad = Search::compile(b"\xff", b"", true, false).unwrap_err();
    assert!(bad.contains("UTF-8"), "{bad}");
    // A literal search never parses the pattern.
    assert_eq!(
        apply(
            &rules(|s| {
                s.search = b"(".to_vec();
                s.replace = b"[".to_vec();
            }),
            "a(b"
        ),
        "a[b"
    );
}

#[test]
fn a_mr_1_case_modes() {
    let case = |mode, name: &str| apply(&rules(|s| s.case = mode), name);
    assert_eq!(case(CaseMode::Lower, "MiXeD.TXT"), "mixed.txt");
    assert_eq!(case(CaseMode::Upper, "MiXeD.txt"), "MIXED.TXT");
    assert_eq!(case(CaseMode::Lower, "ÄBC.ÖÖ"), "äbc.öö");
    assert_eq!(case(CaseMode::Upper, "straße"), "STRASSE");
    // Review finding B3: the name part and the extension are mapped separately, so a
    // capital sigma ending the name part is a final sigma (E-34).
    assert_eq!(case(CaseMode::Lower, "AΣ.BΣ"), "aς.bς");
    assert_eq!(case(CaseMode::Upper, "aς.bς"), "AΣ.BΣ");
    assert_eq!(
        case(CaseMode::Lower, "ΑΣ."),
        "ας.",
        "a trailing dot has no extension"
    );
    // Title: words of the name part, the extension lowercased.
    assert_eq!(case(CaseMode::Title, "my photo.JPG"), "My Photo.jpg");
    assert_eq!(
        case(CaseMode::Title, "report_final.v2.TXT"),
        "Report_Final.V2.txt"
    );
    assert_eq!(case(CaseMode::Title, ".bashrc"), ".Bashrc");
    assert_eq!(
        case(CaseMode::Title, "hello wORLD-foo_bar"),
        "Hello World-Foo_Bar"
    );
    assert_eq!(case(CaseMode::Title, "2nd place"), "2nd Place");
    assert_eq!(case(CaseMode::Title, "élan VITAL.Ogg"), "Élan Vital.ogg");
    assert_eq!(case(CaseMode::Unchanged, "MiXeD.TXT"), "MiXeD.TXT");
    // Invalid bytes stay as they are.
    let r = rules(|s| s.case = CaseMode::Lower);
    assert_eq!(apply_at(&r, b"AB\xffCD.T\xfeX", 0), b"ab\xffcd.t\xfex");
    let r = rules(|s| s.case = CaseMode::Title);
    assert_eq!(apply_at(&r, b"\xffab cd", 0), b"\xffab Cd");
}

#[test]
fn a_mr_1_invalid_utf8_survives_byte_exactly() {
    let name = b"a\xff\xfeb.t\xffx";
    let id = rules(|_| {});
    assert_eq!(apply_at(&id, name, 0), name);
    let r = rules(|s| {
        s.name_mask = b"[N2-3]".to_vec();
        s.ext_mask = b"[E2]".to_vec();
    });
    assert_eq!(
        apply_at(&r, name, 0),
        b"\xff\xfe.\xff",
        "one byte, one character"
    );
    let r = rules(|s| s.name_mask = b"[N-1]".to_vec());
    assert_eq!(apply_at(&r, b"\xc3", 0), b"\xc3", "a truncated sequence");
    let r = rules(|s| {
        s.search = b"b".to_vec();
        s.replace = b"\xfd".to_vec();
    });
    assert_eq!(apply_at(&r, name, 0), b"a\xff\xfe\xfd.t\xffx");
}

fn entries(names: &[(&[u8], usize)]) -> Vec<Entry> {
    names
        .iter()
        .map(|(n, dir)| Entry {
            name: n.to_vec(),
            mtime: t0(),
            dir: *dir,
        })
        .collect()
}

fn dirs(n: usize, others: &[&[u8]]) -> Vec<Directory> {
    (0..n)
        .map(|i| Directory {
            name: format!("d{i}").into_bytes(),
            others: if i == 0 {
                others.iter().map(|o| o.to_vec()).collect()
            } else {
                HashSet::new()
            },
        })
        .collect()
}

fn statuses(p: &Preview) -> Vec<Status> {
    p.status.clone()
}

#[test]
fn a_mr_1_preview_errors() {
    use Problem::*;
    let es = entries(&[(b"a.txt", 0), (b"b.txt", 0), (b"c.md", 1)]);
    let ds = dirs(2, &[b"taken.txt"]);
    let pv = |f: fn(&mut Settings)| preview(&rules(f), &es, &ds);
    // The defaults change nothing.
    let p = pv(|_| {});
    assert_eq!(statuses(&p), [Status::Unchanged; 3]);
    assert_eq!((p.changed, p.errors, p.error.clone()), (0, 0, None));
    // A counter makes every name distinct.
    let p = pv(|s| s.name_mask = b"n[C]".to_vec());
    assert_eq!(statuses(&p), [Status::Ok; 3]);
    assert_eq!(p.new_name(0), b"n1.txt");
    assert_eq!(p.new_name(2), b"n3.md");
    assert_eq!(p.changed, 3);
    // Duplicates are per directory: the entry in the other directory is fine.
    let p = pv(|s| s.name_mask = b"n".to_vec());
    assert_eq!(
        statuses(&p),
        [
            Status::Error(Duplicate),
            Status::Error(Duplicate),
            Status::Ok
        ]
    );
    assert_eq!(
        p.error.as_deref(),
        Some("another selected entry gets the same name: \"a.txt\"")
    );
    // An unchanged entry keeps its name, so another entry cannot take it.
    let p = pv(|s| {
        s.search = b"b.".to_vec();
        s.replace = b"a.".to_vec();
    });
    assert_eq!(
        statuses(&p),
        [
            Status::Error(Duplicate),
            Status::Error(Duplicate),
            Status::Unchanged
        ]
    );
    // A listed entry outside the selection (advisory).
    let p = pv(|s| {
        s.search = b"a".to_vec();
        s.replace = b"taken".to_vec();
    });
    assert_eq!(
        statuses(&p),
        [Status::Error(Exists), Status::Unchanged, Status::Unchanged]
    );
    // Names that cannot be names.
    for (mask, ext, problem) in [
        ("", "", Empty),
        (".", "", Dots),
        ("..", "", Dots),
        ("a/b", "[E]", Slash),
    ] {
        let r = rules(|s| {
            s.name_mask = mask.into();
            s.ext_mask = ext.into();
        });
        let p = preview(&r, &es[..1], &ds);
        assert_eq!(statuses(&p), [Status::Error(problem)], "{mask:?}");
        assert!(p.error.is_some());
    }
    let r = rules(|s| {
        s.search = b"a".to_vec();
        s.replace = b"\0".to_vec();
    });
    assert_eq!(statuses(&preview(&r, &es[..1], &ds)), [Status::Error(Nul)]);
    let long = entries(&[(&[b'x'; 200], 0)]);
    let r = rules(|s| s.name_mask = b"[N][N-56]".to_vec());
    assert_eq!(preview(&r, &long, &ds).new_name(0).len(), 256);
    assert_eq!(statuses(&preview(&r, &long, &ds)), [Status::Error(TooLong)]);
    let r = rules(|s| s.name_mask = b"[N][N-55]".to_vec());
    assert_eq!(
        statuses(&preview(&r, &long, &ds)),
        [Status::Ok],
        "255 bytes"
    );
    // A field error blocks every row.
    let p = Preview::blocked(3, "Name mask: x".into());
    assert_eq!(statuses(&p), [Status::Blocked; 3]);
    assert_eq!(p.error.as_deref(), Some("Name mask: x"));
}

// ---- the job ----------------------------------------------------------------------------

/// What a directory holds: name bytes -> `file:<content>`, `dir` or `link:<target>`.
fn state(dir: &Path) -> BTreeMap<Vec<u8>, String> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        let m = std::fs::symlink_metadata(&p).unwrap();
        let v = if m.is_dir() {
            "dir".to_string()
        } else if m.file_type().is_symlink() {
            format!("link:{}", std::fs::read_link(&p).unwrap().display())
        } else {
            format!(
                "file:{}",
                String::from_utf8_lossy(&std::fs::read(&p).unwrap())
            )
        };
        out.insert(e.file_name().as_bytes().to_vec(), v);
    }
    out
}

fn expect(pairs: &[(&str, &str)]) -> BTreeMap<Vec<u8>, String> {
    pairs
        .iter()
        .map(|(n, v)| (n.as_bytes().to_vec(), v.to_string()))
        .collect()
}

fn temps(dir: &Path) -> Vec<String> {
    state(dir)
        .into_keys()
        .filter(|n| n.starts_with(b".mc-rename-"))
        .map(|n| String::from_utf8_lossy(&n).into_owned())
        .collect()
}

/// Files named and filled with their own names.
fn files(dir: &Path, names: &[&str]) {
    for n in names {
        write(&dir.join(n), n.as_bytes());
    }
}

fn pairs(p: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    p.iter()
        .map(|(o, n)| (OsString::from(o), OsString::from(n)))
        .collect()
}

fn spec(dir: &Path, p: &[(&str, &str)]) -> JobSpec {
    let renames = pairs(p);
    JobSpec::Rename {
        groups: vec![Group::new(
            dir,
            renames.iter().map(|(o, _)| o.clone()).collect(),
        )],
        renames: vec![renames],
    }
}

fn run_job(sys: &Sys, spec: JobSpec) -> Report {
    let mut ui = Script::silent();
    let r = run_guarded(spec, sys, &mut ui);
    assert!(ui.asked.is_empty(), "a rename asks nothing: {:?}", ui.asked);
    r
}

fn rename(dir: &Path, p: &[(&str, &str)]) -> Report {
    run_job(&Sys::default(), spec(dir, p))
}

fn undo(r: &Report) -> Report {
    run_job(
        &Sys::default(),
        JobSpec::UndoRename {
            record: r.renamed.clone(),
        },
    )
}

fn ino(p: &Path) -> (u64, u64) {
    let m = std::fs::symlink_metadata(p).unwrap();
    (m.dev(), m.ino())
}

fn issues(r: &Report) -> Vec<(String, Outcome)> {
    r.issues
        .iter()
        .map(|i| {
            (
                i.path.file_name().unwrap().to_string_lossy().into_owned(),
                i.outcome.clone(),
            )
        })
        .collect()
}

fn skipped(why: &str) -> Outcome {
    Outcome::Skipped(why.into())
}

#[test]
fn a_mr_2_swap_three_cycle_and_chain() {
    let t = test_dir("rename-amr2");
    files(&t.path, &["a", "b"]);
    let (ia, ib) = (ino(&t.join("a")), ino(&t.join("b")));
    let r = rename(&t.path, &[("a", "b"), ("b", "a")]);
    assert_eq!((r.done, r.skipped, r.failed), (2, 0, 0), "{r:?}");
    assert_eq!(state(&t.path), expect(&[("a", "file:b"), ("b", "file:a")]));
    assert_eq!(r.summary(), "rename: 2 renamed");
    // The undo record: the directory and every rename with its inode.
    let [d] = &r.renamed[..] else {
        panic!("{:?}", r.renamed)
    };
    assert_eq!(d.dir_path(), t.path);
    assert_eq!(d.dir, ino(&t.path));
    let mut got: Vec<_> = d
        .entries
        .iter()
        .map(|e| (e.old.clone(), e.new.clone(), e.id))
        .collect();
    got.sort();
    assert_eq!(
        got,
        [("a".into(), "b".into(), ia), ("b".into(), "a".into(), ib)]
    );
    std::fs::remove_file(t.join("a")).unwrap();
    std::fs::remove_file(t.join("b")).unwrap();

    files(&t.path, &["x", "y", "z"]);
    let r = rename(&t.path, &[("x", "y"), ("y", "z"), ("z", "x")]);
    assert_eq!(r.done, 3, "{r:?}");
    assert_eq!(
        state(&t.path),
        expect(&[("x", "file:z"), ("y", "file:x"), ("z", "file:y")])
    );
    assert!(temps(&t.path).is_empty());
    for n in ["x", "y", "z"] {
        std::fs::remove_file(t.join(n)).unwrap();
    }

    // A chain resolves in order: c first, then b, then a.
    files(&t.path, &["a", "b", "c"]);
    let r = rename(&t.path, &[("a", "b"), ("b", "c"), ("c", "d")]);
    assert_eq!(r.done, 3, "{r:?}");
    assert_eq!(
        state(&t.path),
        expect(&[("b", "file:a"), ("c", "file:b"), ("d", "file:c")])
    );
    assert!(temps(&t.path).is_empty());
}

#[test]
fn a_mr_2_a_chain_blocked_by_an_outside_entry_is_skipped_whole() {
    let t = test_dir("rename-amr2-blocked");
    files(&t.path, &["a", "b", "c", "x"]);
    let before = state(&t.path);
    let r = rename(&t.path, &[("a", "b"), ("b", "c"), ("c", "x")]);
    assert_eq!((r.done, r.skipped, r.failed), (0, 3, 0), "{r:?}");
    let mut got = issues(&r);
    got.sort_by(|a, b| a.0.cmp(&b.0));
    let exists = skipped("the destination exists");
    assert_eq!(
        got,
        [
            ("a".into(), exists.clone()),
            ("b".into(), exists.clone()),
            ("c".into(), exists)
        ]
    );
    assert_eq!(
        state(&t.path),
        before,
        "the outside file and the chain untouched"
    );
    assert!(r.renamed.is_empty());
    // A swap next to a blocked chain still resolves, and the chain is still skipped whole.
    files(&t.path, &["y", "z"]);
    let r = rename(
        &t.path,
        &[("a", "b"), ("b", "c"), ("c", "x"), ("y", "z"), ("z", "y")],
    );
    assert_eq!((r.done, r.skipped, r.failed), (2, 3, 0), "{r:?}");
    let mut want = before.clone();
    want.insert(b"y".to_vec(), "file:z".into());
    want.insert(b"z".to_vec(), "file:y".into());
    assert_eq!(state(&t.path), want);
    assert!(temps(&t.path).is_empty());
}

#[test]
fn a_mr_2_a_mix_of_every_shape() {
    let t = test_dir("rename-amr2-mix");
    files(&t.path, &["a", "b", "p", "q", "r", "m", "n", "i", "u", "x"]);
    std::fs::create_dir(t.join("D")).unwrap();
    symlink("nowhere", t.join("L")).unwrap();
    write(&t.join(OsStr::from_bytes(b"bad\xff")), b"bad");
    let spec_pairs: Vec<(OsString, OsString)> = [
        (&b"a"[..], &b"b"[..]),
        (b"b", b"a"),
        (b"p", b"q"),
        (b"q", b"r"),
        (b"r", b"p"),
        (b"m", b"n"),
        (b"n", b"o"),
        (b"i", b"j"),
        (b"u", b"u"),
        // A directory and a symlink swap names; the link is renamed, not its target.
        (b"D", b"L"),
        (b"L", b"D"),
        (b"bad\xff", b"ok\xfe"),
        // Blocked by x, outside the set.
        (b"k", b"x"),
    ]
    .iter()
    .map(|(o, n)| {
        (
            OsStr::from_bytes(o).to_owned(),
            OsStr::from_bytes(n).to_owned(),
        )
    })
    .collect();
    files(&t.path, &["k"]);
    let groups = vec![Group::new(
        &t.path,
        spec_pairs.iter().map(|(o, _)| o.clone()).collect(),
    )];
    let r = run_job(
        &Sys::default(),
        JobSpec::Rename {
            groups,
            renames: vec![spec_pairs],
        },
    );
    assert_eq!(
        (r.done, r.unchanged, r.skipped, r.failed),
        (11, 1, 1, 0),
        "{r:?}"
    );
    assert_eq!(r.summary(), "rename: 11 renamed, 1 unchanged, 1 skipped");
    let mut want = expect(&[
        ("a", "file:b"),
        ("b", "file:a"),
        ("p", "file:r"),
        ("q", "file:p"),
        ("r", "file:q"),
        ("n", "file:m"),
        ("o", "file:n"),
        ("j", "file:i"),
        ("u", "file:u"),
        ("L", "dir"),
        ("D", "link:nowhere"),
        ("k", "file:k"),
        ("x", "file:x"),
    ]);
    want.insert(b"ok\xfe".to_vec(), "file:bad".into());
    assert_eq!(state(&t.path), want);
    assert!(temps(&t.path).is_empty());
    assert_eq!(r.renamed[0].entries.len(), 11);
}

#[test]
fn a_mr_3_a_target_held_outside_the_set_is_skipped() {
    let t = test_dir("rename-amr3");
    files(&t.path, &["a", "b", "x"]);
    let r = rename(&t.path, &[("a", "x"), ("b", "c")]);
    assert_eq!((r.done, r.skipped), (1, 1), "{r:?}");
    assert_eq!(
        issues(&r),
        [("a".into(), skipped("the destination exists"))]
    );
    assert_eq!(
        state(&t.path),
        expect(&[("a", "file:a"), ("c", "file:b"), ("x", "file:x")])
    );
}

/// A new name that is a second hard link of the entry resolves to the entry's own inode
/// but is not a case-only change: the rename is skipped and both names stay.
#[test]
fn a_mr_3_a_second_hard_link_is_not_a_case_alias() {
    let t = test_dir("rename-amr3-hardlink");
    files(&t.path, &["a"]);
    std::fs::hard_link(t.join("a"), t.join("b")).unwrap();
    let r = rename(&t.path, &[("a", "b")]);
    assert_eq!((r.done, r.skipped), (0, 1), "{r:?}");
    assert_eq!(
        issues(&r),
        [("a".into(), skipped("the destination exists"))]
    );
    assert_eq!(state(&t.path), expect(&[("a", "file:a"), ("b", "file:a")]));
    assert!(temps(&t.path).is_empty());
}

#[test]
fn a_mr_3_invalid_and_duplicate_names_are_refused_before_any_write() {
    let t = test_dir("rename-amr3-refused");
    files(&t.path, &["a", "b"]);
    let before = state(&t.path);
    let refused = |r: &Report| {
        assert!(r.refused.is_some(), "{r:?}");
        assert_eq!(r.done, 0);
        assert_eq!(state(&t.path), before, "nothing written");
    };
    refused(&rename(&t.path, &[("a", "z"), ("b", "z")]));
    // An unchanged entry keeps its name.
    let r = rename(&t.path, &[("a", "a"), ("b", "a")]);
    refused(&r);
    assert!(
        r.summary().contains("get the new name \"a\""),
        "{}",
        r.summary()
    );
    let long = "n".repeat(256);
    for bad in ["", ".", "..", "x/y", "x\0y", long.as_str()] {
        let r = rename(&t.path, &[("a", "ok"), ("b", bad)]);
        refused(&r);
        assert!(r.summary().contains("not a valid name"), "{}", r.summary());
    }
    let n255 = "n".repeat(255);
    assert_eq!(rename(&t.path, &[("a", n255.as_str())]).done, 1);
    std::fs::rename(t.join(&n255), t.join("a")).unwrap();
    // An invalid old name, and renames that do not match the groups.
    refused(&rename(&t.path, &[("../a", "c")]));
    let r = run_job(
        &Sys::default(),
        JobSpec::Rename {
            groups: vec![Group::new(&t.path, vec!["a".into()])],
            renames: vec![pairs(&[("b", "c")])],
        },
    );
    refused(&r);
    // One entry with two new names through two groups of one directory.
    let r = run_job(
        &Sys::default(),
        JobSpec::Rename {
            groups: vec![
                Group::new(&t.path, vec!["a".into()]),
                Group::new(&t.path, vec!["a".into()]),
            ],
            renames: vec![pairs(&[("a", "c")]), pairs(&[("a", "d")])],
        },
    );
    refused(&r);
}

/// P2 2.2: groups that reach the same directory are merged before a rename, so a swap
/// split across two groups (here a path and a symlink to it) is one cycle.
#[test]
fn groups_of_one_directory_are_merged() {
    let t = test_dir("rename-merge");
    std::fs::create_dir(t.join("d")).unwrap();
    files(&t.join("d"), &["a", "b"]);
    symlink(t.join("d"), t.join("via")).unwrap();
    let r = run_job(
        &Sys::default(),
        JobSpec::Rename {
            groups: vec![
                Group::new(t.join("d"), vec!["a".into()]),
                Group::new(t.join("via"), vec!["b".into()]),
            ],
            renames: vec![pairs(&[("a", "b")]), pairs(&[("b", "a")])],
        },
    );
    assert_eq!((r.done, r.skipped), (2, 0), "{r:?}");
    assert_eq!(
        state(&t.join("d")),
        expect(&[("a", "file:b"), ("b", "file:a")])
    );
    assert_eq!(r.renamed.len(), 1, "one directory");
    assert_eq!(
        r.renamed[0].root,
        t.join("d"),
        "under the first group's path"
    );
    // A group in a subdirectory, reached by the component walk.
    let r = run_job(
        &Sys::default(),
        JobSpec::Rename {
            groups: vec![Group {
                root: t.path.clone(),
                sub: vec!["d".into()],
                names: vec!["a".into()],
            }],
            renames: vec![pairs(&[("a", "c")])],
        },
    );
    assert_eq!(r.done, 1, "{r:?}");
    assert_eq!(r.renamed[0].sub, [OsString::from("d")]);
    assert!(t.join("d/c").exists());
}

#[test]
fn a_mr_5_undo_restores_and_leaves_replaced_entries_alone() {
    let t = test_dir("rename-amr5");
    files(&t.path, &["a", "b", "c", "d", "e"]);
    let before = state(&t.path);
    let r = rename(
        &t.path,
        &[("a", "b"), ("b", "a"), ("c", "x"), ("d", "c"), ("e", "y")],
    );
    assert_eq!(r.done, 5, "{r:?}");
    let u = undo(&r);
    assert_eq!(u.verb, JobVerb::UndoRename);
    assert_eq!((u.done, u.skipped, u.failed), (5, 0, 0), "{u:?}");
    assert_eq!(u.summary(), "undo rename: 5 renamed back");
    assert_eq!(state(&t.path), before);
    assert!(temps(&t.path).is_empty());

    // An entry replaced since (another inode under its new name) is left alone.
    let r = rename(&t.path, &[("a", "x"), ("b", "y"), ("c", "a")]);
    assert_eq!(r.done, 3, "{r:?}");
    std::fs::remove_file(t.join("y")).unwrap();
    write(&t.join("y"), b"new");
    let u = undo(&r);
    assert_eq!((u.done, u.skipped), (2, 1), "{u:?}");
    assert_eq!(
        issues(&u),
        [("y".into(), skipped("replaced since the rename; left alone"))]
    );
    assert_eq!(
        state(&t.path),
        expect(&[
            ("a", "file:a"),
            ("c", "file:c"),
            ("d", "file:d"),
            ("e", "file:e"),
            ("y", "file:new")
        ])
    );
    // A directory replaced since: every entry in it is left alone.
    std::fs::create_dir(t.join("sub")).unwrap();
    files(&t.join("sub"), &["s"]);
    let r = rename(&t.join("sub"), &[("s", "s2")]);
    std::fs::rename(t.join("sub"), t.join("old-sub")).unwrap();
    std::fs::create_dir(t.join("sub")).unwrap();
    files(&t.join("sub"), &["s2"]);
    let u = undo(&r);
    assert_eq!(u.skipped, 1, "{u:?}");
    assert!(
        matches!(&u.issues[0].outcome, Outcome::Skipped(w) if w.contains("directory was replaced")),
        "{u:?}"
    );
    assert!(t.join("sub/s2").exists() && t.join("old-sub/s2").exists());
}

/// A case-insensitive directory: a tmpfs mounted with `casefold` at `t/mnt`, and `ci` in
/// it with `chattr +F`. Only inside `in_userns`. `None` (after `skip`) when the kernel
/// refuses; the caller unmounts `mnt`.
fn casefold_dir(t: &TestDir) -> Option<(PathBuf, PathBuf)> {
    let mnt = t.join("mnt");
    std::fs::create_dir(&mnt).unwrap();
    let out = std::process::Command::new("mount")
        .args(["-t", "tmpfs", "-o", "casefold", "tmpfs"])
        .arg(&mnt)
        .output()
        .unwrap();
    if !out.status.success() {
        skip(&format!(
            "the kernel refuses a casefold tmpfs mount: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
        return None;
    }
    let ci = mnt.join("ci");
    std::fs::create_dir(&ci).unwrap();
    let chattr = std::process::Command::new("chattr")
        .arg("+F")
        .arg(&ci)
        .output();
    let ok = matches!(&chattr, Ok(o) if o.status.success());
    if !ok {
        umount(&mnt);
        skip(&format!(
            "chattr +F on an empty tmpfs directory failed: {chattr:?}"
        ));
        return None;
    }
    Some((mnt, ci))
}

/// A-MR-6: on a case-insensitive directory (tmpfs `casefold`) a case-only rename goes
/// through an intermediate name, and `Foo -> bar`, `Bar -> foo` is a cycle there.
#[test]
fn a_mr_6_case_insensitive_directory() {
    if !in_userns("a_mr_6_case_insensitive_directory") {
        return;
    }
    let t = test_dir("rename-amr6");
    let Some((mnt, ci)) = casefold_dir(&t) else {
        return;
    };
    files(&ci, &["Foo"]);
    assert!(ci.join("FOO").exists(), "the directory folds case");
    let id = ino(&ci.join("Foo"));
    let r = rename(&ci, &[("Foo", "foo")]);
    assert_eq!((r.done, r.skipped, r.failed), (1, 0, 0), "{r:?}");
    assert_eq!(state(&ci), expect(&[("foo", "file:Foo")]));
    assert_eq!(ino(&ci.join("foo")), id, "the same inode");
    // A case-only change of a directory.
    std::fs::create_dir(ci.join("Dir")).unwrap();
    let r = rename(&ci, &[("Dir", "DIR")]);
    assert_eq!(r.done, 1, "{r:?}");
    assert!(state(&ci).contains_key(&b"DIR"[..]));
    std::fs::remove_dir(ci.join("DIR")).unwrap();
    std::fs::remove_file(ci.join("foo")).unwrap();
    // The swap by identity: bar is Bar, foo is Foo.
    files(&ci, &["Foo", "Bar"]);
    let r = rename(&ci, &[("Foo", "bar"), ("Bar", "foo")]);
    assert_eq!((r.done, r.skipped, r.failed), (2, 0, 0), "{r:?}");
    assert_eq!(
        state(&ci),
        expect(&[("bar", "file:Foo"), ("foo", "file:Bar")])
    );
    assert!(temps(&ci).is_empty());
    // Two new names that differ only in case: the second is skipped, nothing overwritten.
    files(&ci, &["x", "y"]);
    let r = rename(&ci, &[("x", "n"), ("y", "N")]);
    assert_eq!((r.done, r.skipped), (1, 1), "{r:?}");
    assert_eq!(state(&ci).len(), 4);
    // The undo on a case-insensitive directory.
    let r = rename(&ci, &[("bar", "Baz")]);
    assert_eq!(undo(&r).done, 1);
    assert!(state(&ci).contains_key(&b"bar"[..]));
    umount(&mnt);
}

/// Review finding A3 (P2 6.3 step 3): on a case-insensitive directory, the holder of a new
/// name among hard links of one inode is the entry whose old name folds to it, not the
/// first link of the set. Scenario A: `Foo` and `Bar` are one inode, `Foo -> bar` and
/// `Bar -> foo` are a cycle. Scenario B: `Foo2` and `Foo` are one inode, `z` another;
/// `z -> foo` waits for `Foo`, which waits for `z` (a cycle), and `Foo2 -> y` is free.
/// Every rename succeeds and no temporary name is left.
#[test]
fn a_mr_6_hard_links_on_a_case_insensitive_directory() {
    if !in_userns("a_mr_6_hard_links_on_a_case_insensitive_directory") {
        return;
    }
    let t = test_dir("rename-amr6-links");
    let Some((mnt, ci)) = casefold_dir(&t) else {
        return;
    };
    let names = |dir: &Path| state(dir).into_keys().collect::<Vec<_>>();
    // Scenario A.
    write(&ci.join("Foo"), b"one");
    std::fs::hard_link(ci.join("Foo"), ci.join("Bar")).unwrap();
    let id = ino(&ci.join("Foo"));
    let r = rename(&ci, &[("Foo", "bar"), ("Bar", "foo")]);
    assert_eq!((r.done, r.skipped, r.failed), (2, 0, 0), "{r:?}");
    assert!(r.notes.is_empty(), "{r:?}");
    assert_eq!(names(&ci), [b"bar".to_vec(), b"foo".to_vec()]);
    assert_eq!((ino(&ci.join("bar")), ino(&ci.join("foo"))), (id, id));
    assert!(temps(&ci).is_empty());
    std::fs::remove_file(ci.join("bar")).unwrap();
    std::fs::remove_file(ci.join("foo")).unwrap();
    // Scenario B, in this insertion order.
    write(&ci.join("Foo2"), b"x");
    std::fs::hard_link(ci.join("Foo2"), ci.join("Foo")).unwrap();
    write(&ci.join("z"), b"z");
    let (x, z) = (ino(&ci.join("Foo")), ino(&ci.join("z")));
    let r = rename(&ci, &[("Foo2", "y"), ("Foo", "z"), ("z", "foo")]);
    assert_eq!((r.done, r.skipped, r.failed), (3, 0, 0), "{r:?}");
    assert!(r.notes.is_empty(), "{r:?}");
    assert_eq!(names(&ci), [b"foo".to_vec(), b"y".to_vec(), b"z".to_vec()]);
    assert_eq!(
        (ino(&ci.join("y")), ino(&ci.join("z")), ino(&ci.join("foo"))),
        (x, x, z)
    );
    assert!(temps(&ci).is_empty());
    umount(&mnt);
}

/// The fold approximation of the rename job: Unicode lowercase on valid UTF-8, the same
/// bytes otherwise.
#[test]
fn names_fold_by_unicode_lowercase_or_bytes() {
    use manycommander::fsops::rename::folds_equal;
    let f = |a: &[u8], b: &[u8]| folds_equal(OsStr::from_bytes(a), OsStr::from_bytes(b));
    assert!(f(b"Foo", b"fOO"));
    assert!(f("\u{c4}BC".as_bytes(), "\u{e4}bc".as_bytes()));
    assert!(!f(b"Foo", b"Foo2"));
    assert!(f(b"a\xffB", b"a\xffB"), "invalid UTF-8: the same bytes");
    assert!(!f(b"a\xffB", b"a\xffb"), "invalid UTF-8 is not folded");
}

#[cfg(feature = "failpoints")]
mod failpoints {
    use super::*;
    use manycommander::fsops::failpoints::{Action, Failpoints, Trigger};
    use rustix::io::Errno;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    fn sys_with(fp: &Arc<Failpoints>) -> Sys {
        Sys::with_failpoints(Arc::new(AtomicBool::new(false)), fp.clone())
    }

    /// A 3-cycle, a swap, a chain and an independent rename.
    const MAP: [(&str, &str); 8] = [
        ("a", "b"),
        ("b", "c"),
        ("c", "a"),
        ("d", "e"),
        ("e", "d"),
        ("f", "g"),
        ("g", "h"),
        ("i", "j"),
    ];

    fn setup(name: &str) -> TestDir {
        let t = test_dir(name);
        files(&t.path, &MAP.map(|(o, _)| o));
        t
    }

    /// I-9: every inode is under its original name, its new name, or a temporary name the
    /// report states; the undo record holds every entry not under its original name.
    fn verify(dir: &Path, r: &Report, what: &str) {
        let now = state(dir);
        assert_eq!(now.len(), MAP.len(), "{what}: {now:?} {r:?}");
        for (old, new) in MAP {
            let content = format!("file:{old}");
            let at: Vec<&Vec<u8>> = now
                .iter()
                .filter(|(_, v)| **v == content)
                .map(|(n, _)| n)
                .collect();
            let [at] = at[..] else {
                panic!("{what}: {old} is at {at:?}")
            };
            let at = String::from_utf8_lossy(at).into_owned();
            let stated = at.starts_with(".mc-rename-") && r.notes.iter().any(|n| n.contains(&at));
            assert!(
                at == old || at == new || stated,
                "{what}: {old} is at {at}: {r:?}"
            );
            let recorded = r
                .renamed
                .iter()
                .flat_map(|d| &d.entries)
                .find(|e| e.old == old);
            if at == old {
                assert!(recorded.is_none(), "{what}: {old} did not move: {r:?}");
            } else {
                let e = recorded.unwrap_or_else(|| panic!("{what}: {old} not recorded: {r:?}"));
                assert_eq!(e.new, OsString::from(&at), "{what}");
                assert_eq!(e.id, ino(&dir.join(&at)), "{what}");
            }
        }
    }

    /// One run with `inject` armed: the report, whether a temporary name was left (and
    /// stated), and how often the recovery ran.
    fn one(inject: &[(&str, Trigger, Action)], what: &str) -> (Report, bool, u64) {
        let t = setup("rename-sweep");
        let fp = Failpoints::new();
        for (step, trigger, action) in inject {
            fp.arm(step, *trigger, action.clone());
        }
        let r = run_job(&sys_with(&fp), spec(&t.path, &MAP));
        verify(&t.path, &r, what);
        let stranded = !temps(&t.path).is_empty();
        // The record is complete: the undo takes every entry back.
        let u = undo(&r);
        assert_eq!(u.failed + u.skipped, 0, "{what}: {u:?}");
        let want: BTreeMap<Vec<u8>, String> = MAP
            .iter()
            .map(|(o, _)| (o.as_bytes().to_vec(), format!("file:{o}")))
            .collect();
        assert_eq!(state(&t.path), want, "{what}: after the undo");
        (r, stranded, fp.hits("rename.back"))
    }

    /// Hits of every step in a run without injections.
    fn baseline() -> std::collections::HashMap<String, u64> {
        let t = setup("rename-sweep-base");
        let fp = Failpoints::new();
        let r = run_job(&sys_with(&fp), spec(&t.path, &MAP));
        assert_eq!(r.done, 8, "{r:?}");
        verify(&t.path, &r, "baseline");
        fp.all_hits()
    }

    #[test]
    fn a_mr_4_failpoint_sweep_error_and_cancel() {
        let hits = baseline();
        assert_eq!(
            hits.get("rename.tmp"),
            Some(&2),
            "one break per cycle: {hits:?}"
        );
        assert_eq!(hits.get("rename.rename"), Some(&8), "{hits:?}");
        let (mut runs, mut stranded, mut recovered) = (0, 0, 0);
        for step in [
            "rename.stat",
            "rename.probe",
            "rename.check",
            "rename.tmp",
            "rename.rename",
        ] {
            for n in 1..=hits[step] {
                for (label, action) in [
                    ("EIO", Action::Errno(Errno::IO)),
                    ("cancel", Action::Cancel),
                ] {
                    let what = format!("{step} #{n} {label}");
                    let (r, left, back) = one(&[(step, Trigger::Nth(n), action.clone())], &what);
                    stranded += left as u32;
                    recovered += back;
                    if label == "cancel" && step == "rename.rename" && n < 8 {
                        assert!(r.cancelled, "{what}: {r:?}");
                        assert!(
                            !r.renamed.is_empty(),
                            "{what}: a cancelled job keeps its record"
                        );
                        assert!(r.remaining() > 0, "{what}: {}", r.summary());
                    }
                    // The same with every recovery failing: the temporary names are stated.
                    let what = format!("{what}, recovery fails");
                    let (_, left, _) = one(
                        &[
                            (step, Trigger::Nth(n), action),
                            ("rename.back", Trigger::Always, Action::Errno(Errno::IO)),
                        ],
                        &what,
                    );
                    stranded += left as u32;
                    runs += 2;
                }
            }
        }
        assert!(runs > 100, "{runs}");
        assert!(recovered > 0, "the recovery ran");
        assert!(stranded > 0, "some runs left a stated temporary name");
    }

    /// The final rename of a cycle's temporary name fails: the member goes back to its
    /// original name when that is free, else the report names the temporary path.
    #[test]
    fn a_mr_4_a_temporary_name_that_cannot_reach_its_target() {
        let t = test_dir("rename-amr4-temp");
        files(&t.path, &["a", "b"]);
        let fp = Failpoints::new();
        // The swap: a moves to a temporary name (tmp #1), b -> a (rename #1), then the
        // temporary name -> b (rename #2) fails.
        fp.arm("rename.rename", Trigger::Nth(2), Action::Errno(Errno::IO));
        let r = run_job(&sys_with(&fp), spec(&t.path, &[("a", "b"), ("b", "a")]));
        assert_eq!((r.done, r.failed), (1, 1), "{r:?}");
        let tmp = temps(&t.path);
        let [tmp] = &tmp[..] else { panic!("{tmp:?}") };
        assert_eq!(state(&t.path)[&b"a"[..]], "file:b");
        assert!(
            r.notes
                .iter()
                .any(|n| n.contains(tmp.as_str()) && n.contains("taken")),
            "{r:?}"
        );
        // Its record takes it back.
        let u = undo(&r);
        assert_eq!(u.done, 2, "{u:?}");
        assert_eq!(state(&t.path), expect(&[("a", "file:a"), ("b", "file:b")]));

        // A cancel right after the temporary name: it goes back to its free original name.
        let fp = Failpoints::new();
        fp.arm("rename.tmp", Trigger::Nth(1), Action::Cancel);
        let r = run_job(&sys_with(&fp), spec(&t.path, &[("a", "b"), ("b", "a")]));
        assert!(r.cancelled, "{r:?}");
        assert_eq!(fp.hits("rename.back"), 1);
        assert_eq!(state(&t.path), expect(&[("a", "file:a"), ("b", "file:b")]));
        assert!(r.renamed.is_empty(), "nothing moved in the end");
        assert_eq!(r.summary(), "rename cancelled: 0 renamed, 2 not renamed");
    }

    /// The hard-link case is told apart by listing the directory, before any rename: no
    /// temporary name is used.
    #[test]
    fn a_second_hard_link_never_takes_the_case_only_path() {
        let t = test_dir("rename-hardlink-fp");
        files(&t.path, &["a"]);
        std::fs::hard_link(t.join("a"), t.join("b")).unwrap();
        let fp = Failpoints::new();
        let r = run_job(&sys_with(&fp), spec(&t.path, &[("a", "b")]));
        assert_eq!(r.skipped, 1, "{r:?}");
        assert_eq!(fp.hits("rename.list"), 1);
        assert_eq!(fp.hits("rename.tmp"), 0);
    }

    /// A temporary name that exists already: another random name is tried (P2 6.3 step 5).
    #[test]
    fn a_taken_temporary_name_is_retried() {
        let t = test_dir("rename-tmp-eexist");
        files(&t.path, &["a", "b"]);
        let fp = Failpoints::new();
        fp.arm("rename.tmp", Trigger::Nth(1), Action::Errno(Errno::EXIST));
        let r = run_job(&sys_with(&fp), spec(&t.path, &[("a", "b"), ("b", "a")]));
        assert_eq!(r.done, 2, "{r:?}");
        assert_eq!(fp.hits("rename.tmp"), 2);
        assert_eq!(state(&t.path), expect(&[("a", "file:b"), ("b", "file:a")]));
    }

    /// A blocked chain is never cycle-broken, and a member whose identity changed fails
    /// with "type changed" and takes the entries waiting for it along.
    #[test]
    fn blocked_chains_are_never_broken_and_identities_are_checked() {
        let t = test_dir("rename-blocked-fp");
        files(&t.path, &["a", "b", "c", "x"]);
        let fp = Failpoints::new();
        let r = run_job(
            &sys_with(&fp),
            spec(&t.path, &[("a", "b"), ("b", "c"), ("c", "x")]),
        );
        assert_eq!(r.skipped, 3, "{r:?}");
        assert_eq!(fp.hits("rename.tmp"), 0, "no cycle breaking");
        assert_eq!(fp.hits("rename.rename"), 1, "only c tries");

        // c is replaced by another inode between the scan and its rename.
        let dir = t.path.clone();
        let fp = Failpoints::new();
        fp.arm(
            "rename.check",
            Trigger::Nth(1),
            Action::Call(Arc::new(move || {
                std::fs::remove_file(dir.join("c")).unwrap();
                write(&dir.join("c"), b"other");
            })),
        );
        std::fs::remove_file(t.join("x")).unwrap();
        let r = run_job(
            &sys_with(&fp),
            spec(&t.path, &[("a", "b"), ("b", "c"), ("c", "x")]),
        );
        assert_eq!(
            issues(&r),
            [
                ("c".into(), Outcome::Failed("type changed".into())),
                ("b".into(), skipped("the destination exists")),
                ("a".into(), skipped("the destination exists")),
            ]
        );
        assert!(!t.join("x").exists(), "the replacement was not renamed");
    }
}

// ---- the tool in the app ----------------------------------------------------------------

fn app(left: &Path, right: &Path) -> App {
    App::new(
        left.to_path_buf(),
        right.to_path_buf(),
        right.to_path_buf(),
        Config::default(),
        None,
        Depth::NoColor,
        TimeZone::UTC,
    )
}

/// Performs effects synchronously (listings, re-stats, searches); returns the rest.
fn run(app: &mut App, fx: Vec<Effect>) -> Vec<Effect> {
    let mut rest = Vec::new();
    for e in fx {
        match e {
            Effect::List(req, alive) => {
                let msgs = std::cell::RefCell::new(Vec::new());
                listing::guarded(&req, &|m| msgs.borrow_mut().push(m), listing::list);
                alive.finish();
                for m in msgs.into_inner() {
                    let more = app.update(Event::Listing(m));
                    rest.extend(run(app, more));
                }
            }
            Effect::Restat(req, alive) => {
                let msgs = std::cell::RefCell::new(Vec::new());
                find::restat_guarded(&req, &|m| msgs.borrow_mut().push(m));
                alive.finish();
                for m in msgs.into_inner() {
                    let more = app.update(Event::Listing(m));
                    rest.extend(run(app, more));
                }
            }
            Effect::Find(s) => {
                let msgs = Mutex::new(Vec::new());
                find::guarded(&s, &|m| msgs.lock().unwrap().push(m));
                for m in msgs.into_inner().unwrap() {
                    let more = app.update(Event::Find(m));
                    rest.extend(run(app, more));
                }
            }
            other => rest.push(other),
        }
    }
    rest
}

fn press_with(a: &mut App, code: KeyCode, m: KeyModifiers) -> Vec<Effect> {
    a.update(Event::Key(KeyEvent::new(code, m), Instant::now()))
}

fn press(a: &mut App, code: KeyCode) -> Vec<Effect> {
    press_with(a, code, KeyModifiers::NONE)
}

fn ctrl(a: &mut App, c: char) -> Vec<Effect> {
    press_with(a, KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn typed(a: &mut App, s: &str) {
    for c in s.chars() {
        press(a, KeyCode::Char(c));
    }
}

fn started(left: &Path, right: &Path) -> App {
    let mut a = app(left, right);
    let fx = a.start();
    run(&mut a, fx);
    a.panel_mut().ensure_sorted();
    a
}

fn tool(a: &App) -> &RenameTool {
    match &a.dialog {
        Some(Dialog::Rename(t)) => t,
        _ => panic!("the multi-rename tool is not open"),
    }
}

fn new_names(a: &App) -> Vec<String> {
    let t = tool(a);
    (0..t.preview.len())
        .map(|i| String::from_utf8_lossy(t.preview.new_name(i)).into_owned())
        .collect()
}

/// Replaces the focused text field's content.
fn set_field(a: &mut App, text: &str) {
    ctrl(a, 'e');
    ctrl(a, 'u');
    typed(a, text);
}

/// Runs the job the effects start on this thread and feeds its report back.
fn finish(a: &mut App, fx: Vec<Effect>) -> (JobSpec, Report) {
    let [Effect::StartJob(spec)] = &fx[..] else {
        panic!("{fx:?}")
    };
    let spec = spec.clone();
    let r = run_job(&Sys::default(), spec.clone());
    let fx = a.update(Event::Job(JobEvent::Done(r.clone())));
    run(a, fx);
    a.panel_mut().ensure_sorted();
    (spec, r)
}

fn group(root: &Path, sub: &[&str], names: &[&str]) -> Group {
    Group {
        root: root.to_path_buf(),
        sub: sub.iter().map(OsString::from).collect(),
        names: names.iter().map(OsString::from).collect(),
    }
}

/// P2 6.1, 6.4, 6.5, 10: `Ctrl+M` opens the tool for the selection, a change recomputes the
/// preview, an error blocks `Enter`, `Enter` starts the job, `Ctrl+Z` in the tool starts
/// the undo of the last multi-rename.
#[test]
fn the_tool_in_a_directory_panel() {
    let l = test_dir("rename-app-dir");
    files(&l.path, &["a.txt", "b.txt", "c.md"]);
    let mut a = started(&l.path, &l.path);
    // With text on the command line Ctrl+M is ignored; Ctrl+Z is ignored outside the tool.
    a.line.set(b"echo");
    assert!(ctrl(&mut a, 'm').is_empty());
    assert!(a.dialog.is_none());
    assert_eq!(a.line.bytes(), b"echo");
    a.line.clear();
    assert!(ctrl(&mut a, 'z').is_empty());
    assert!(a.dialog.is_none());

    for n in [&b"a.txt"[..], b"b.txt"] {
        a.panel_mut().cursor_to_name(n);
        a.panel_mut().toggle_mark(false);
    }
    assert!(ctrl(&mut a, 'm').is_empty());
    let t = tool(&a);
    assert_eq!(t.groups, [group(&l.path, &[], &["a.txt", "b.txt"])]);
    assert_eq!(t.dirs[0].name, l.path.file_name().unwrap().as_bytes());
    assert!(t.dirs[0].others.contains(&b"c.md"[..]));
    assert_eq!(t.preview.status, [Status::Unchanged; 2]);
    // Nothing changes: Enter is blocked.
    assert!(press(&mut a, KeyCode::Enter).is_empty());
    assert!(
        tool(&a)
            .form
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with("nothing to rename")),
        "{:?}",
        tool(&a).form.error
    );
    // Typing recomputes the preview; a duplicate blocks Enter with the first error.
    set_field(&mut a, "x");
    assert_eq!(new_names(&a), ["x.txt", "x.txt"]);
    assert!(
        tool(&a).form.error.is_none(),
        "a change clears the last error"
    );
    assert!(press(&mut a, KeyCode::Enter).is_empty());
    assert_eq!(
        tool(&a).form.error.as_deref(),
        Some("another selected entry gets the same name: \"a.txt\"")
    );
    // A mask error blocks every row.
    set_field(&mut a, "[Q]");
    assert_eq!(tool(&a).preview.status, [Status::Blocked; 2]);
    assert!(press(&mut a, KeyCode::Enter).is_empty());
    assert!(
        tool(&a)
            .form
            .error
            .as_deref()
            .unwrap()
            .starts_with("Name mask: ")
    );
    // A name held by the unselected c.md (advisory).
    set_field(&mut a, "c");
    press(&mut a, KeyCode::Tab);
    set_field(&mut a, "md");
    assert_eq!(
        tool(&a).preview.status,
        [
            Status::Error(Problem::Duplicate),
            Status::Error(Problem::Duplicate)
        ]
    );
    set_field(&mut a, "[E]");
    press(&mut a, KeyCode::BackTab);
    // A counter field that is not a number.
    for _ in 0..7 {
        press(&mut a, KeyCode::Tab);
    }
    set_field(&mut a, "x");
    assert!(
        tool(&a)
            .preview
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with("Counter start")),
        "{:?}",
        tool(&a).preview.error
    );
    set_field(&mut a, "7");
    // The case choice: Tab back to it, Right twice is "upper".
    for _ in 0..1 {
        press(&mut a, KeyCode::BackTab);
    }
    press(&mut a, KeyCode::Right);
    press(&mut a, KeyCode::Right);
    for _ in 0..6 {
        press(&mut a, KeyCode::BackTab);
    }
    set_field(&mut a, "[N]_[C]");
    assert_eq!(new_names(&a), ["A_7.TXT", "B_8.TXT"]);
    let fx = press(&mut a, KeyCode::Enter);
    let (spec, r) = finish(&mut a, fx);
    assert_eq!(
        spec,
        JobSpec::Rename {
            groups: vec![group(&l.path, &[], &["a.txt", "b.txt"])],
            renames: vec![pairs(&[("a.txt", "A_7.TXT"), ("b.txt", "B_8.TXT")])],
        }
    );
    assert!(a.dialog.is_none());
    assert_eq!(r.done, 2);
    assert_eq!(a.rename_undo.as_deref(), Some(&r.renamed[..]));
    assert!(
        a.panel().list.find(b"A_7.TXT").is_some(),
        "the panel refreshed"
    );

    // Ctrl+Z in the tool: the undo of that job.
    a.panel_mut().cursor_to_name(b"c.md");
    ctrl(&mut a, 'm');
    assert_eq!(
        tool(&a).form.lines,
        ["Ctrl+Z: undo the last multi-rename (2 entries)"]
    );
    let fx = ctrl(&mut a, 'z');
    assert_eq!(
        fx,
        [Effect::StartJob(JobSpec::UndoRename {
            record: r.renamed.clone()
        })]
    );
    assert!(a.dialog.is_none());
    assert!(a.rename_undo.is_none(), "the record is used up");
    let (_, u) = finish(&mut a, fx);
    assert_eq!(u.done, 2);
    assert!(a.rename_undo.is_none(), "an undo's own record is not kept");
    assert_eq!(
        state(&l.path),
        expect(&[
            ("a.txt", "file:a.txt"),
            ("b.txt", "file:b.txt"),
            ("c.md", "file:c.md")
        ])
    );
    // Nothing left to undo.
    ctrl(&mut a, 'm');
    assert!(tool(&a).form.lines.is_empty());
    assert!(ctrl(&mut a, 'z').is_empty());
    assert!(
        tool(&a)
            .form
            .error
            .as_deref()
            .unwrap()
            .starts_with("nothing to undo")
    );
    press(&mut a, KeyCode::Esc);
    assert!(a.dialog.is_none());
    // A rename job that renamed nothing keeps no record.
    let fx = a.update(Event::Job(JobEvent::Done(Report::new(JobVerb::Rename))));
    run(&mut a, fx);
    assert!(a.rename_undo.is_none());
}

/// P2 5.4, 6.1: in a results tab, `Ctrl+M` renames the results in their own directories,
/// one group per directory; `[P]` is each result's directory; duplicates are per directory.
#[test]
fn the_tool_in_a_results_tab_is_grouped() {
    let t = test_dir("rename-app-results");
    let root = t.join("root");
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::create_dir_all(root.join("b")).unwrap();
    for f in ["a/x.txt", "a/y.txt", "b/x.txt", "z.txt", "a/other.md"] {
        write(&root.join(f), f.as_bytes());
    }
    let mut a = started(&root, &root);
    press_with(&mut a, KeyCode::F(7), KeyModifiers::ALT);
    typed(&mut a, "*.txt");
    let fx = press(&mut a, KeyCode::Enter);
    run(&mut a, fx);
    a.panel_mut().ensure_sorted();
    assert!(!a.panel().is_directory());
    ctrl(&mut a, 'a');
    ctrl(&mut a, 'm');
    let tl = tool(&a);
    assert_eq!(
        tl.groups,
        [
            group(&root, &["a"], &["x.txt", "y.txt"]),
            group(&root, &["b"], &["x.txt"]),
            group(&root, &[], &["z.txt"]),
        ]
    );
    set_field(&mut a, "n");
    assert_eq!(
        tool(&a).preview.status,
        [
            Status::Error(Problem::Duplicate),
            Status::Error(Problem::Duplicate),
            Status::Ok,
            Status::Ok,
        ],
        "duplicates are per directory"
    );
    set_field(&mut a, "[P]_[N]");
    assert_eq!(
        new_names(&a),
        ["a_x.txt", "a_y.txt", "b_x.txt", "root_z.txt"]
    );
    let fx = press(&mut a, KeyCode::Enter);
    let (spec, r) = finish(&mut a, fx);
    assert_eq!(
        spec,
        JobSpec::Rename {
            groups: vec![
                group(&root, &["a"], &["x.txt", "y.txt"]),
                group(&root, &["b"], &["x.txt"]),
                group(&root, &[], &["z.txt"]),
            ],
            renames: vec![
                pairs(&[("x.txt", "a_x.txt"), ("y.txt", "a_y.txt")]),
                pairs(&[("x.txt", "b_x.txt")]),
                pairs(&[("z.txt", "root_z.txt")]),
            ],
        }
    );
    assert_eq!(r.done, 4, "{r:?}");
    assert!(root.join("a/a_x.txt").exists() && root.join("b/b_x.txt").exists());
    assert!(root.join("root_z.txt").exists());
    assert_eq!(r.renamed.len(), 3);
    assert_eq!(r.renamed[0].sub, [OsString::from("a")]);
}

/// P2 6.4: a directory's other listed names come from the whole listing, hidden ones
/// included; the tool never lists a result below a selected result twice.
#[test]
fn other_names_include_hidden_entries() {
    let l = test_dir("rename-app-hidden");
    files(&l.path, &["a", ".h"]);
    let mut a = started(&l.path, &l.path);
    press_with(&mut a, KeyCode::Char('.'), KeyModifiers::ALT);
    a.panel_mut().ensure_sorted();
    a.panel_mut().cursor_to_name(b"a");
    ctrl(&mut a, 'm');
    assert!(tool(&a).dirs[0].others.contains(&b".h"[..]));
    set_field(&mut a, ".h");
    assert_eq!(tool(&a).preview.status, [Status::Error(Problem::Exists)]);
}

/// The PgUp/PgDn keys scroll the preview; they never leave its rows.
#[test]
fn the_preview_scrolls() {
    let es: Vec<Entry> = (0..50)
        .map(|i| Entry {
            name: format!("f{i:02}").into_bytes(),
            mtime: t0(),
            dir: 0,
        })
        .collect();
    let mut t = RenameTool::new(
        es,
        dirs(1, &[]),
        vec![group(Path::new("/d"), &[], &[])],
        TimeZone::UTC,
        None,
    );
    let key = |t: &mut RenameTool, code| t.handle(KeyEvent::new(code, KeyModifiers::NONE));
    key(&mut t, KeyCode::PageDown);
    assert_eq!(t.scroll, 10);
    for _ in 0..10 {
        key(&mut t, KeyCode::PageDown);
    }
    assert_eq!(t.scroll, 40, "the last page");
    key(&mut t, KeyCode::PageUp);
    assert_eq!(t.scroll, 30);
}
