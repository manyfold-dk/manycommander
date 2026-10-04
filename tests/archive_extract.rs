//! Archive view and extract (P3 3.4, 3.5, T3, and 7z in T8): A-AR-2 and A-AR-3 on the write
//! side (the committed hostile fixtures of `tests/fixtures/archive/make.py`), A-AR-5 (F5: zip
//! and 7z by locator, tar and a solid 7z in one pass), A-AR-6 (the runtime view directory and
//! the view copy) and A-AR-7 (encrypted members).

mod common;

use common::*;
use manycommander::archive::OpenRequest;
use manycommander::archive::detect::Want;
use manycommander::archive::extract::{ArchiveOrigin, ENCRYPTED_MEMBER, UNSUPPORTED_METHOD};
use manycommander::archive::index::Limits;
use manycommander::archive::{
    self, ArchiveIndex, CHANGED_MEMBER, DAMAGED, IndexCache, NEEDS_MEMORY,
};
use manycommander::fsops::copy::{ARCHIVE_ITSELF, LINK_NOT_EXTRACTED, copy_from};
use manycommander::fsops::group::{Group, Root};
use manycommander::fsops::job::{Dest, JobSpec, Outcome, Report, run_guarded};
use manycommander::fsops::origin::SIZE_MISMATCH;
use manycommander::fsops::question::{Answer, Question};
use manycommander::fsops::sys::Sys;
use manycommander::panel::listing::ListingMsg;
use manycommander::provider::{PlaceError, Provider, VPath};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

const BASE: u64 = 1_700_000_000;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/archive")
        .join(name)
}

/// Opens an archive synchronously, as a listing thread does, and returns its complete
/// index.
fn open(path: &Path) -> Arc<ArchiveIndex> {
    let name = path.file_name().unwrap().as_encoded_bytes();
    let req = OpenRequest {
        slot: 0,
        generation: 1,
        archive: path.to_path_buf(),
        want: Want::of_name(name),
        inner: VPath::root(),
        cancel: Arc::new(AtomicBool::new(false)),
        tz: jiff::tz::TimeZone::UTC,
        limits: Limits::default(),
    };
    let got = std::cell::RefCell::new(None);
    archive::open(&req, &IndexCache::default(), &|m| {
        if let ListingMsg::Opened { index, .. } = m {
            *got.borrow_mut() = Some(index);
        }
    });
    let ix = got.into_inner().expect("the archive opens");
    assert!(ix.is_complete(), "{path:?}");
    ix
}

fn os(s: &[u8]) -> OsString {
    OsString::from_vec(s.to_vec())
}

/// One group of the archive: `names` in the inner directory `sub`.
fn group(ix: &Arc<ArchiveIndex>, sub: &[&[u8]], names: &[&[u8]]) -> Group {
    Group {
        root: Root::Archive(ix.clone()),
        sub: sub.iter().map(|c| os(c)).collect(),
        names: names.iter().map(|n| os(n)).collect(),
    }
}

/// Every name at the archive root.
fn everything(ix: &Arc<ArchiveIndex>) -> Group {
    Group {
        root: Root::Archive(ix.clone()),
        sub: Vec::new(),
        names: archive::root_names(ix),
    }
}

/// F5 as the app starts it: a copy job whose groups are in an archive.
fn extract(groups: Vec<Group>, dst: &Path, ui: &mut Script) -> Report {
    run_guarded(
        JobSpec::Copy {
            groups,
            dst: Dest::Local(dst.to_path_buf()),
        },
        &Sys::default(),
        ui,
    )
}

fn issues(r: &Report) -> Vec<(String, String)> {
    r.issues
        .iter()
        .map(|i| {
            let (kind, why) = match &i.outcome {
                Outcome::Skipped(w) => ("skipped", w),
                Outcome::Failed(w) => ("failed", w),
            };
            (
                i.path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                format!("{kind}: {why}"),
            )
        })
        .collect()
}

fn partials(dir: &Path) -> Vec<PathBuf> {
    walk(dir)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .unwrap()
                .as_encoded_bytes()
                .windows(12)
                .any(|w| w == b".mc-partial-")
        })
        .collect()
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            out.push(p.clone());
            if std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir()) {
                out.extend(walk(&p));
            }
        }
    }
    out
}

fn mode(p: &Path) -> u32 {
    std::fs::symlink_metadata(p).unwrap().mode() & 0o7777
}

// ---- generated archives -----------------------------------------------------------------

/// A tar written header by header.
struct TarGen {
    b: tar::Builder<Vec<u8>>,
}

impl TarGen {
    fn new() -> TarGen {
        TarGen {
            b: tar::Builder::new(Vec::new()),
        }
    }

    fn raw(&mut self, name: &[u8], kind: u8, mode: u32, link: &[u8], data: &[u8]) {
        let mut h = tar::Header::new_gnu();
        h.as_old_mut().name[..name.len()].copy_from_slice(name);
        h.as_old_mut().linkname[..link.len()].copy_from_slice(link);
        h.set_entry_type(tar::EntryType::new(kind));
        h.set_mode(mode);
        h.set_mtime(BASE);
        h.set_uid(0);
        h.set_gid(0);
        h.set_size(data.len() as u64);
        h.set_cksum();
        self.b.append(&h, data).unwrap();
    }

    fn file(&mut self, name: &[u8], data: &[u8]) -> &mut Self {
        self.raw(name, b'0', 0o644, b"", data);
        self
    }

    fn dir(&mut self, name: &[u8], mode: u32) -> &mut Self {
        self.raw(name, b'5', mode, b"", b"");
        self
    }

    fn symlink(&mut self, name: &[u8], target: &[u8]) -> &mut Self {
        self.raw(name, b'2', 0o777, target, b"");
        self
    }

    fn finish(self) -> Vec<u8> {
        self.b.into_inner().unwrap()
    }
}

/// `data` compressed as `ext` (`tar` leaves it as it is).
fn compress(ext: &str, data: &[u8]) -> Vec<u8> {
    match ext {
        "tar" => data.to_vec(),
        "tar.gz" => {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            e.write_all(data).unwrap();
            e.finish().unwrap()
        }
        "tar.zst" => zstd::stream::encode_all(data, 3).unwrap(),
        "tar.xz" => {
            let mut w =
                lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1))
                    .unwrap();
            w.write_all(data).unwrap();
            w.finish().unwrap()
        }
        "tar.bz2" => {
            let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
            e.write_all(data).unwrap();
            e.finish().unwrap()
        }
        _ => unreachable!("{ext}"),
    }
}

const TAR_FORMATS: [&str; 5] = ["tar", "tar.gz", "tar.zst", "tar.xz", "tar.bz2"];

/// A zip of `(name, mode, data, deflated)` members.
fn zip_of(members: &[(&str, u32, &[u8], bool)]) -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let dos = zip::DateTime::from_date_and_time(2023, 11, 14, 22, 13, 20).unwrap();
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, mode, data, deflated) in members {
        let method = if *deflated {
            zip::CompressionMethod::Deflated
        } else {
            zip::CompressionMethod::Stored
        };
        let o = SimpleFileOptions::default()
            .unix_permissions(*mode)
            .last_modified_time(dos)
            .compression_method(method);
        if name.ends_with('/') {
            w.add_directory(*name, o).unwrap();
        } else {
            w.start_file(*name, o).unwrap();
            w.write_all(data).unwrap();
        }
    }
    w.finish().unwrap().into_inner()
}

fn put(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

// ---- A-AR-5 ---------------------------------------------------------------------------------

/// Zip members open by locator in any order: bytes identical, modes masked to `0o777`,
/// the DOS mtime applied; directories get their modes.
#[test]
fn a_ar_5_zip_members_by_locator_in_any_order() {
    let t = test_dir("x-zip-order");
    let a = noise(300_000, 1);
    let b = noise(70_000, 2);
    let bytes = zip_of(&[
        ("d/", 0o750, b"", false),
        ("d/a", 0o4755, &a, true),
        ("d/b", 0o640, &b, false),
        ("c", 0o600, b"cc", true),
    ]);
    let ix = open(&put(&t.path, "x.zip", &bytes));
    let dst = t.join("dst");
    std::fs::create_dir(&dst).unwrap();
    let r = extract(
        vec![
            group(&ix, &[b"d"], &[b"b", b"a"]),
            group(&ix, &[], &[b"c", b"d"]),
        ],
        &dst,
        &mut Script::silent(),
    );
    assert!(r.issues.is_empty(), "{r:?}");
    assert_eq!(std::fs::read(dst.join("b")).unwrap(), b);
    assert_eq!(std::fs::read(dst.join("a")).unwrap(), a);
    assert_eq!(std::fs::read(dst.join("c")).unwrap(), b"cc");
    assert_eq!(std::fs::read(dst.join("d/a")).unwrap(), a);
    assert_eq!(mode(&dst.join("a")), 0o755, "no setuid (A-3)");
    assert_eq!(mode(&dst.join("b")), 0o640);
    assert_eq!(mode(&dst.join("d")), 0o750);
    let m = std::fs::metadata(dst.join("c")).unwrap();
    assert_eq!(m.mtime(), BASE as i64, "the DOS time, read as UTC");
    assert!(partials(&dst).is_empty());
}

/// A tar, plain and compressed, is read once in stream order and nothing after the last
/// selected member; the directories come first, and their modes follow in post-order.
#[test]
fn a_ar_5_tar_one_pass_stops_after_the_last_selected_member() {
    let t = test_dir("x-tar-pass");
    let big = noise(3 << 20, 3);
    let mut g = TarGen::new();
    // The file comes before its directory member, which is read-only.
    g.file(b"d/f", b"first")
        .dir(b"d/", 0o555)
        .file(b"e/g", b"second")
        .symlink(b"e/l", b"g")
        .file(b"big", &big)
        .file(b"last", b"last");
    let plain = g.finish();
    for ext in TAR_FORMATS {
        let x = t.join(ext);
        std::fs::create_dir(&x).unwrap();
        let path = put(&x, &format!("a.{ext}"), &compress(ext, &plain));
        let ix = open(&path);
        let dst = x.join("dst");
        std::fs::create_dir(&dst).unwrap();
        let sys = Sys::default();
        let o = ArchiveOrigin::new(&sys, &ix).unwrap();
        let r = copy_from(
            &sys,
            &mut Script::silent(),
            &o,
            &[group(&ix, &[], &[b"d", b"e"])],
            &dst,
        );
        assert!(r.issues.is_empty() && !r.cancelled, "{ext}: {r:?}");
        assert_eq!((r.done, r.dirs_done), (3, 2), "{ext}");
        assert_eq!(std::fs::read(dst.join("d/f")).unwrap(), b"first", "{ext}");
        assert_eq!(std::fs::read(dst.join("e/g")).unwrap(), b"second");
        assert_eq!(std::fs::read_link(dst.join("e/l")).unwrap(), Path::new("g"));
        assert_eq!(mode(&dst.join("d")), 0o555, "{ext}: post-order mode");
        let len = std::fs::metadata(&path).unwrap().len();
        assert!(
            o.bytes_read() < len / 2,
            "{ext}: read {} of {len}: the pass stops before the big member",
            o.bytes_read()
        );
        std::fs::set_permissions(dst.join("d"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// Duplicates: the last of each is extracted and nothing says "archive changed" (A-5).
#[test]
fn a_ar_5_duplicates_extract_the_last() {
    let t = test_dir("x-dups");
    let mut g = TarGen::new();
    g.file(b"x", b"old")
        .file(b"y", b"y")
        .file(b"x", b"newer")
        .dir(b"d/", 0o700)
        .file(b"d/z", b"1")
        .dir(b"d/", 0o751)
        .file(b"d/z", b"22");
    let plain = g.finish();
    for ext in ["tar", "tar.zst"] {
        let x = t.join(ext);
        std::fs::create_dir(&x).unwrap();
        let ix = open(&put(&x, &format!("a.{ext}"), &compress(ext, &plain)));
        let dst = x.join("dst");
        std::fs::create_dir(&dst).unwrap();
        let r = extract(vec![everything(&ix)], &dst, &mut Script::silent());
        assert!(r.issues.is_empty(), "{ext}: {r:?}");
        assert_eq!(std::fs::read(dst.join("x")).unwrap(), b"newer");
        assert_eq!(std::fs::read(dst.join("d/z")).unwrap(), b"22");
        assert_eq!(mode(&dst.join("d")), 0o751);
    }
}

/// A header changed at a selected locator after the scan fails that member with "archive
/// changed" (A-5): the archive is rewritten in place, so the index's held fd reads the new
/// bytes. Tar through the pass, zip through its central directory.
#[test]
fn a_ar_5_a_changed_header_fails_with_archive_changed() {
    let t = test_dir("x-changed");
    let mut g = TarGen::new();
    g.file(b"keep", b"keep").file(b"xname", b"payload");
    let path = put(&t.path, "a.tar", &g.finish());
    let ix = open(&path);
    // The second header's name becomes "yname", with its checksum fixed.
    let mut bytes = std::fs::read(&path).unwrap();
    let h = 1024;
    assert_eq!(&bytes[h..h + 5], b"xname");
    bytes[h] = b'y';
    let sum: u64 = bytes[h..h + 512]
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                32
            } else {
                b as u64
            }
        })
        .sum();
    bytes[h + 148..h + 156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
    let dst = t.join("dst");
    std::fs::create_dir(&dst).unwrap();
    let r = extract(vec![everything(&ix)], &dst, &mut Script::silent());
    assert_eq!(
        issues(&r),
        [("xname".to_string(), format!("failed: {CHANGED_MEMBER}"))]
    );
    assert_eq!(std::fs::read(dst.join("keep")).unwrap(), b"keep");
    assert!(!dst.join("xname").exists() && !dst.join("yname").exists());

    // A zip whose central directory names another member at the same index.
    let zpath = put(&t.path, "a.zip", &zip_of(&[("zname", 0o644, b"zz", false)]));
    let zix = open(&zpath);
    let mut z = std::fs::read(&zpath).unwrap();
    let cd = z.windows(4).rposition(|w| w == b"PK\x01\x02").unwrap();
    assert_eq!(&z[cd + 46..cd + 51], b"zname");
    z[cd + 46] = b'q';
    std::fs::OpenOptions::new()
        .write(true)
        .open(&zpath)
        .unwrap()
        .write_all(&z)
        .unwrap();
    let r = extract(vec![everything(&zix)], &dst, &mut Script::silent());
    assert_eq!(
        issues(&r),
        [("zname".to_string(), format!("failed: {CHANGED_MEMBER}"))]
    );
    assert!(partials(&dst).is_empty());
}

/// Hard links as links within the job (A-3): made from the destination inode the job
/// extracted; a link whose member is not extracted, or whose target text leaves the archive
/// or is absolute, is skipped.
#[test]
fn a_ar_5_hard_links_are_links_within_the_job() {
    let t = test_dir("x-hard");
    let ix = open(&fixture("hardlinks.tar"));
    let dst = t.join("dst");
    std::fs::create_dir(&dst).unwrap();
    let r = extract(vec![everything(&ix)], &dst, &mut Script::silent());
    let mut got = issues(&r);
    got.sort();
    let skip = format!("skipped: {LINK_NOT_EXTRACTED}");
    assert_eq!(
        got,
        [
            ("hl-abs".to_string(), skip.clone()),
            ("hl-missing".to_string(), skip.clone()),
            ("hl-up".to_string(), skip.clone()),
        ]
    );
    let a = std::fs::metadata(dst.join("target")).unwrap();
    let b = std::fs::metadata(dst.join("hl-ok")).unwrap();
    assert_eq!((a.ino(), a.nlink()), (b.ino(), 2), "one inode, two names");
    assert_eq!(std::fs::read(dst.join("hl-ok")).unwrap(), b"the target\n");
    // A link whose member is not in the job is skipped, and the member stays unlinked.
    let dst2 = t.join("dst2");
    std::fs::create_dir(&dst2).unwrap();
    let r = extract(
        vec![group(&ix, &[], &[b"hl-ok"])],
        &dst2,
        &mut Script::silent(),
    );
    assert_eq!(issues(&r), [("hl-ok".to_string(), skip)]);
    assert!(walk(&dst2).is_empty());
}

/// "File exists" (M1 4.5) for zip and tar members: Skip keeps the old file, Overwrite
/// replaces it atomically, Rename writes the new name; no temporary name remains.
#[test]
fn a_ar_5_file_exists_skip_overwrite_rename() {
    let t = test_dir("x-exists");
    let mut g = TarGen::new();
    g.file(b"s", b"new s")
        .file(b"o", b"new o")
        .file(b"r", b"new r");
    let tar = put(&t.path, "a.tar.gz", &compress("tar.gz", &g.finish()));
    let zip = put(
        &t.path,
        "a.zip",
        &zip_of(&[
            ("s", 0o644, b"new s", true),
            ("o", 0o644, b"new o", false),
            ("r", 0o644, b"new r", true),
        ]),
    );
    for path in [tar, zip] {
        let ix = open(&path);
        let dst = t.join(format!(
            "dst-{}",
            path.extension().unwrap().to_string_lossy()
        ));
        std::fs::create_dir(&dst).unwrap();
        for n in ["s", "o", "r"] {
            write(&dst.join(n), b"old");
        }
        let mut ui = Script::new([Answer::Skip, Answer::Overwrite, Answer::Rename("r2".into())]);
        let r = extract(vec![group(&ix, &[], &[b"s", b"o", b"r"])], &dst, &mut ui);
        assert_eq!(ui.asked.len(), 3, "{path:?}: {:?}", ui.asked);
        assert!(
            ui.asked
                .iter()
                .all(|q| matches!(q, Question::FileExists { .. })),
            "{:?}",
            ui.asked
        );
        assert_eq!((r.done, r.skipped), (2, 1), "{path:?}: {r:?}");
        assert_eq!(std::fs::read(dst.join("s")).unwrap(), b"old");
        assert_eq!(std::fs::read(dst.join("o")).unwrap(), b"new o");
        assert_eq!(std::fs::read(dst.join("r")).unwrap(), b"old");
        assert_eq!(std::fs::read(dst.join("r2")).unwrap(), b"new r");
        assert!(partials(&dst).is_empty(), "{path:?}");
    }
}

/// The free-space question (A-4, P3 3.5): a member whose pax size declares 1 PiB, more than
/// any destination has, is asked about before any write. Cancel writes nothing; Continue
/// reaches the end of the stream, which fails the member with "archive damaged" and leaves
/// no partial file.
#[test]
fn a_ar_5_the_free_space_question_comes_before_any_write() {
    let t = test_dir("x-space");
    let mut b = tar::Builder::new(Vec::new());
    b.append_pax_extensions([("size", &b"1125899906842624"[..])])
        .unwrap();
    let mut h = tar::Header::new_ustar();
    h.set_path("huge").unwrap();
    h.set_size(4);
    h.set_mode(0o644);
    h.set_cksum();
    b.append(&h, &b"tiny"[..]).unwrap();
    let path = put(&t.path, "a.tar", &b.into_inner().unwrap());
    let ix = open(&path);
    assert_eq!(
        ix.outcome().and_then(|o| o.error.as_deref()),
        Some(DAMAGED),
        "the scan cannot skip 1 PiB of data"
    );
    let dst = t.join("dst");
    std::fs::create_dir(&dst).unwrap();
    let mut ui = Script::new([Answer::Cancel]);
    let r = extract(vec![everything(&ix)], &dst, &mut ui);
    let [Question::FreeSpace { need, .. }] = &ui.asked[..] else {
        panic!("{:?}", ui.asked);
    };
    assert_eq!(*need, 1 << 50);
    assert!(r.cancelled && r.done == 0, "{r:?}");
    assert!(walk(&dst).is_empty());
    let mut ui = Script::new([Answer::Continue]);
    let r = extract(vec![everything(&ix)], &dst, &mut ui);
    assert_eq!(
        issues(&r),
        [("huge".to_string(), format!("failed: {DAMAGED}"))]
    );
    assert!(walk(&dst).is_empty());
}

/// A destination entry that is the archive itself is never replaced, and no question offers
/// Overwrite (P3 3.5): at the top level (the plan's check) and below it (the commit's).
#[test]
fn a_ar_5_the_archive_itself_is_never_replaced() {
    let t = test_dir("x-itself");
    let mut g = TarGen::new();
    g.file(b"self.tar", b"not the archive")
        .file(b"sub/self.tar", b"not the archive either")
        .file(b"other", b"other");
    let bytes = g.finish();
    let path = put(&t.path, "self.tar", &bytes);
    std::fs::create_dir(t.join("sub")).unwrap();
    std::fs::hard_link(&path, t.join("sub/self.tar")).unwrap();
    let ix = open(&path);
    let mut ui = Script::new([Answer::Merge]);
    let r = extract(vec![everything(&ix)], &t.path, &mut ui);
    let mut got = issues(&r);
    got.sort();
    let fail = format!("failed: {ARCHIVE_ITSELF}");
    assert_eq!(
        got,
        [
            ("self.tar".to_string(), fail.clone()),
            ("self.tar".to_string(), fail)
        ]
    );
    assert!(
        matches!(ui.asked[..], [Question::DirExists { .. }]),
        "only the merge of sub/ is asked; no Overwrite is offered: {:?}",
        ui.asked
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "the archive is intact"
    );
    assert_eq!(std::fs::read(t.join("other")).unwrap(), b"other");
    // The same for a zip in its own directory.
    let zbytes = zip_of(&[("z.zip", 0o644, b"a member", false)]);
    let zpath = put(&t.path, "z.zip", &zbytes);
    let zix = open(&zpath);
    let r = extract(vec![everything(&zix)], &t.path, &mut Script::silent());
    assert_eq!(
        issues(&r),
        [("z.zip".to_string(), format!("failed: {ARCHIVE_ITSELF}"))]
    );
    assert_eq!(std::fs::read(&zpath).unwrap(), zbytes);
    assert!(partials(&t.path).is_empty());
}

/// Reads of the same archive while an extraction runs return correct bytes: every read is
/// positioned on the one held fd (P3 3.2, A-AR-5).
#[test]
fn a_ar_5_member_reads_during_an_extraction_return_correct_bytes() {
    let t = test_dir("x-concurrent");
    let big = noise(6 << 20, 9);
    let mut g = TarGen::new();
    let small: Vec<Vec<u8>> = (0..8)
        .map(|i| noise(40_000 + i * 1000, 20 + i as u64))
        .collect();
    for (i, d) in small.iter().enumerate() {
        g.file(format!("s{i}").as_bytes(), d);
    }
    g.file(b"big", &big);
    let plain = g.finish();
    for ext in ["tar", "tar.zst"] {
        let x = t.join(ext);
        std::fs::create_dir(&x).unwrap();
        let ix = open(&put(&x, &format!("a.{ext}"), &compress(ext, &plain)));
        let dst = x.join("dst");
        std::fs::create_dir(&dst).unwrap();
        let job = {
            let ix = ix.clone();
            let dst = dst.clone();
            std::thread::spawn(move || {
                extract(
                    vec![group(&ix, &[], &[b"big"])],
                    &dst,
                    &mut Script::silent(),
                )
            })
        };
        for round in 0..3 {
            for (i, d) in small.iter().enumerate() {
                let p = VPath::parse(format!("s{i}").as_bytes()).unwrap();
                let mut r = ix.open_read(&p, &Arc::new(AtomicBool::new(false))).unwrap();
                let mut got = Vec::new();
                r.read_to_end(&mut got).unwrap();
                assert_eq!(&got, d, "{ext} round {round}: s{i}");
            }
        }
        let r = job.join().unwrap();
        assert!(r.issues.is_empty(), "{ext}: {r:?}");
        assert_eq!(std::fs::read(dst.join("big")).unwrap(), big, "{ext}");
    }
}

/// An encrypted zip entry is listed, and F5 and a member read refuse it with "encrypted"
/// (A-AR-7).
#[test]
fn a_ar_7_encrypted_members_are_listed_and_refused() {
    let t = test_dir("x-encrypted");
    let ix = open(&fixture("encrypted.zip"));
    assert_eq!(archive::root_names(&ix), [OsString::from("secret.txt")]);
    let r = extract(vec![everything(&ix)], &t.path, &mut Script::silent());
    assert_eq!(
        issues(&r),
        [(
            "secret.txt".to_string(),
            format!("skipped: {ENCRYPTED_MEMBER}")
        )]
    );
    assert!(walk(&t.path).is_empty());
    let p = VPath::parse(b"secret.txt").unwrap();
    assert_eq!(
        ix.open_read(&p, &Arc::new(AtomicBool::new(false))).err(),
        Some(PlaceError::Refused(ENCRYPTED_MEMBER.into()))
    );
    assert_eq!(
        archive::view_target(&ix, &VPath::root(), b"secret.txt"),
        Err(ENCRYPTED_MEMBER)
    );
}

// ---- A-AR-2: hostile fixtures on the write side ------------------------------------------

/// A recursive listing below `root`: kind, mode, size and link target by path.
fn listing(root: &Path) -> BTreeMap<PathBuf, String> {
    let mut out = BTreeMap::new();
    for p in walk(root) {
        let m = std::fs::symlink_metadata(&p).unwrap();
        let ft = m.file_type();
        let kind = if ft.is_symlink() {
            format!("link -> {:?}", std::fs::read_link(&p).unwrap())
        } else if ft.is_dir() {
            "dir".into()
        } else if ft.is_file() {
            format!("file {}", m.len())
        } else {
            "special".into()
        };
        out.insert(p, format!("{kind} {:o} {}", m.mode() & 0o7777, m.mtime()));
    }
    out
}

/// F5 of everything a hostile fixture lists into an empty directory: the listing of the
/// directory around it changes only inside the destination; no special file, no mode above
/// `0o777`, no symlink followed (A-AR-2).
fn hostile(name: &str, pick: Option<&[&[u8]]>) -> (PathBuf, Report, TestDir) {
    let t = test_dir(&format!("x-hostile-{name}"));
    let work = t.join("work");
    std::fs::create_dir_all(work.join("outside")).unwrap();
    write(&work.join("outside/keep"), b"outside");
    std::fs::set_permissions(work.join("outside"), std::fs::Permissions::from_mode(0o750)).unwrap();
    let path = work.join(name);
    std::fs::copy(fixture(name), &path).unwrap();
    let dst = work.join("dst");
    std::fs::create_dir(&dst).unwrap();
    let ix = open(&path);
    let groups = match pick {
        Some(names) => vec![group(&ix, &[], names)],
        None => vec![everything(&ix)],
    };
    let before = listing(&work);
    let r = extract(groups, &dst, &mut Script::silent());
    let after = listing(&work);
    for (p, v) in &after {
        if !p.starts_with(&dst) {
            assert_eq!(before.get(p), Some(v), "{name}: {p:?} changed outside");
        }
    }
    for (p, v) in &before {
        assert!(after.contains_key(p), "{name}: {p:?} ({v}) is gone");
    }
    for p in walk(&dst) {
        let m = std::fs::symlink_metadata(&p).unwrap();
        let ft = m.file_type();
        assert!(
            ft.is_dir() || ft.is_file() || ft.is_symlink(),
            "{name}: special file {p:?}"
        );
        if !ft.is_symlink() {
            assert_eq!(m.mode() & 0o7000, 0, "{name}: {p:?} has a privilege bit");
        }
    }
    assert!(partials(&dst).is_empty(), "{name}");
    (dst, r, t)
}

#[test]
fn a_ar_2_traversal_names_stay_inside() {
    let (dst, r, _t) = hostile("traversal.tar", None);
    assert!(r.issues.is_empty(), "{r:?}");
    let mut got: Vec<_> = walk(&dst)
        .into_iter()
        .filter(|p| p.is_file())
        .map(|p| p.strip_prefix(&dst).unwrap().to_path_buf())
        .collect();
    got.sort();
    assert_eq!(
        got,
        [
            Path::new("abs/file"),
            Path::new("dot/ok"),
            Path::new("ok/file")
        ]
    );
    let (dst, r, _t) = hostile("nul-name.tar", None);
    assert!(r.issues.is_empty(), "{r:?}");
    assert_eq!(std::fs::read(dst.join("ok")).unwrap(), b"ok\n");
}

/// A member below a symlink member (the CVE-2025-29787 shape) is not in the index, so
/// nothing is written through the link; the link itself is extracted as a link.
#[test]
fn a_ar_2_nothing_is_written_through_a_symlink_member() {
    for name in ["symlink-write-through.tar", "symlink-write-through.zip"] {
        let (dst, r, t) = hostile(name, None);
        assert!(r.issues.is_empty(), "{name}: {r:?}");
        assert_eq!(
            std::fs::read_link(dst.join("link")).unwrap(),
            Path::new("../outside"),
            "{name}"
        );
        assert_eq!(std::fs::read(dst.join("fine")).unwrap(), b"fine\n");
        assert!(!t.join("work/outside/pwned").exists(), "{name}");
    }
}

/// A symlink then a directory member of the same name (the RUSTSEC-2026-0067 shape): the
/// directory is made in the destination and its mode applied there, never through the
/// link.
#[test]
fn a_ar_2_a_directory_mode_never_reaches_a_symlink_target() {
    let (dst, r, t) = hostile("symlink-chmod.tar", None);
    assert!(r.issues.is_empty(), "{r:?}");
    let m = std::fs::symlink_metadata(dst.join("d")).unwrap();
    assert!(m.is_dir(), "the last member wins");
    assert_eq!(m.mode() & 0o7777, 0o777);
    assert_eq!(mode(&t.join("work/outside")), 0o750, "untouched");
}

/// A pax size over a smaller ustar size (the RUSTSEC-2026-0068 shape): the member gets the
/// pax size's data, and the header hidden in it is not a member.
#[test]
fn a_ar_2_the_pax_size_is_honoured_on_extraction() {
    let (dst, r, _t) = hostile("pax-size.tar", None);
    assert!(r.issues.is_empty(), "{r:?}");
    assert_eq!(std::fs::metadata(dst.join("a")).unwrap().len(), 1024);
    assert_eq!(
        std::fs::read(dst.join("b")).unwrap(),
        b"after the pax member\n"
    );
    assert!(!dst.join("smuggled").exists());
}

/// Hard links to an absolute path, one that leaves the archive and one to a missing member
/// are skipped; the one to an extracted member is a link.
#[test]
fn a_ar_2_hard_links_never_leave_the_destination() {
    let (dst, r, _t) = hostile("hardlinks.tar", None);
    assert_eq!(r.skipped, 3, "{r:?}");
    for n in ["hl-abs", "hl-up", "hl-missing"] {
        assert!(std::fs::symlink_metadata(dst.join(n)).is_err(), "{n}");
    }
    assert_eq!(std::fs::metadata(dst.join("hl-ok")).unwrap().nlink(), 2);
}

/// Device and FIFO members are skipped as "special file"; setuid and setgid bits go.
#[test]
fn a_ar_2_special_members_and_privilege_bits() {
    let (dst, r, _t) = hostile("special.tar", None);
    let mut got = issues(&r);
    got.sort();
    let special = "skipped: special file".to_string();
    assert_eq!(
        got,
        [
            ("dev-block".to_string(), special.clone()),
            ("dev-null".to_string(), special.clone()),
            ("fifo".to_string(), special),
        ]
    );
    assert_eq!(mode(&dst.join("setuid")), 0o755);
    assert_eq!(mode(&dst.join("setgid-dir")), 0o755);
}

/// Two central directory entries over one local entry: both are extracted, with the
/// shared data, inside the destination.
#[test]
fn a_ar_2_an_overlapping_zip_stays_inside() {
    let (dst, r, _t) = hostile("overlap.zip", None);
    assert!(r.issues.is_empty(), "{r:?}");
    let want = b"shared data\n".repeat(100);
    assert_eq!(std::fs::read(dst.join("a")).unwrap(), want);
    assert_eq!(std::fs::read(dst.join("b")).unwrap(), want);
}

/// A member that inflates far beyond its declared size fails with "size mismatch" and
/// leaves nothing (A-4); the member after it is extracted. A tar member of 1 GiB of zeros
/// before the selected one is decoded and discarded by the one pass.
#[test]
fn a_ar_2_bombs_stop_at_the_declared_size() {
    let (dst, r, _t) = hostile("bomb.zip", None);
    assert_eq!(
        issues(&r),
        [("bomb".to_string(), format!("failed: {SIZE_MISMATCH}"))]
    );
    assert!(!dst.join("bomb").exists());
    assert_eq!(std::fs::read(dst.join("small")).unwrap(), b"small\n");
    let (dst, r, _t) = hostile("bomb.tar.zst", Some(&[b"after"]));
    assert!(r.issues.is_empty(), "{r:?}");
    assert_eq!(
        std::fs::read(dst.join("after")).unwrap(),
        b"after the zeros\n"
    );
}

/// Runs the calling test's body alone in a new process of this test binary, so the peak
/// memory it measures is its own and not a parallel test's. In the parent it runs exactly
/// `test_name` there, asserts that it passed, and returns `false`: the caller returns. In
/// the child it returns `true`: the caller runs the body.
fn alone(test_name: &str) -> bool {
    if std::env::var_os("MC_ALONE").is_some() {
        return true;
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env("MC_ALONE", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("test result: ok. 1 passed"),
        "{test_name} alone:\n{stdout}\n{stderr}"
    );
    false
}

/// What `f` returns, and how much it raised this process's peak resident memory
/// (`VmHWM`), in bytes. The peak is first reset to the current size where the kernel
/// allows it (`clear_refs`).
fn peak_growth<T>(f: impl FnOnce() -> T) -> (T, u64) {
    let hwm = || {
        let s = std::fs::read_to_string("/proc/self/status").unwrap();
        let kb = s.lines().find_map(|l| l.strip_prefix("VmHWM:")).unwrap();
        kb.trim()
            .trim_end_matches("kB")
            .trim()
            .parse::<u64>()
            .unwrap()
            << 10
    };
    let _ = std::fs::write("/proc/self/clear_refs", "5");
    let before = hwm();
    let r = f();
    (r, hwm().saturating_sub(before))
}

/// A pax header whose two `size` records disagree (0, then 32 MiB) over an empty member,
/// then a GNU long name that declares 32 MiB (`pax-size-bomb.tar.zst`). The tar crate skips
/// by the first size and reads the long name next; a header guard that followed the last
/// size let it read the whole name into memory (review finding B1). The scan stops with
/// "archive damaged" at the pax header, at once and in bounded memory (A-4, E-6).
#[test]
fn a_ar_2_disagreeing_pax_sizes_stop_the_scan() {
    if !alone("a_ar_2_disagreeing_pax_sizes_stop_the_scan") {
        return;
    }
    let started = std::time::Instant::now();
    let (ix, grew) = peak_growth(|| open(&fixture("pax-size-bomb.tar.zst")));
    let took = started.elapsed();
    eprintln!(
        "pax-size-bomb scan: {took:?}, peak memory +{} KiB",
        grew >> 10
    );
    assert_eq!(ix.outcome().and_then(|o| o.error.as_deref()), Some(DAMAGED));
    assert!(archive::root_names(&ix).is_empty(), "nothing is listed");
    assert!(grew < 16 << 20, "peak memory grew by {} MiB", grew >> 20);
    assert!(took < std::time::Duration::from_secs(10), "{took:?}");
}

/// The same header in the extraction pass: an archive listed whole is rewritten in place
/// with the crafted bytes (the index's held fd reads them, as in A-5). The pass stops with
/// "archive damaged" at the first header, in bounded memory; no member is written.
#[test]
fn a_ar_2_disagreeing_pax_sizes_stop_the_pass() {
    if !alone("a_ar_2_disagreeing_pax_sizes_stop_the_pass") {
        return;
    }
    let t = test_dir("x-pax-size-bomb");
    // The same first member under one pax size; the noise keeps the compressed archive
    // longer than the crafted one, so the pass reads all of the crafted stream.
    let mut b = tar::Builder::new(Vec::new());
    b.append_pax_extensions([("size", &b"0"[..])]).unwrap();
    let mut g = TarGen { b };
    g.file(b"decoy", b"")
        .file(b"noise", &noise(256 << 10, 7))
        .file(b"tail", b"after the long name\n");
    let path = put(&t.path, "a.tar.zst", &compress("tar.zst", &g.finish()));
    let ix = open(&path);
    assert_eq!(ix.outcome().and_then(|o| o.error.as_deref()), None);
    assert_eq!(archive::root_names(&ix).len(), 3);
    let crafted = std::fs::read(fixture("pax-size-bomb.tar.zst")).unwrap();
    assert!(crafted.len() as u64 <= std::fs::metadata(&path).unwrap().len());
    std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&path)
        .unwrap()
        .write_all(&crafted)
        .unwrap();
    let dst = t.join("dst");
    std::fs::create_dir(&dst).unwrap();
    let started = std::time::Instant::now();
    let (r, grew) = peak_growth(|| extract(vec![everything(&ix)], &dst, &mut Script::silent()));
    let took = started.elapsed();
    eprintln!(
        "pax-size-bomb pass: {took:?}, peak memory +{} KiB",
        grew >> 10
    );
    assert_eq!((r.done, r.failed), (0, 3), "{r:?}");
    for i in &r.issues {
        assert_eq!(i.outcome, Outcome::Failed(DAMAGED.into()), "{:?}", i.path);
    }
    assert!(walk(&dst).is_empty(), "{:?}", walk(&dst));
    assert!(grew < 16 << 20, "peak memory grew by {} MiB", grew >> 20);
    assert!(took < std::time::Duration::from_secs(10), "{took:?}");
}

// ---- A-AR-3: damage -----------------------------------------------------------------------

/// The content `make.py` gives member `i` of the truncated fixtures.
fn truncated_member(i: usize) -> Vec<u8> {
    (0..400)
        .map(|k| format!("member {i} line {k}\n"))
        .collect::<String>()
        .into_bytes()
}

/// A truncated stream: the members before the damage are extracted whole; the member the
/// damage cuts fails with "archive damaged" and leaves no partial file.
#[test]
fn a_ar_3_truncated_streams_leave_no_partial_file() {
    for name in [
        "truncated.tar",
        "truncated.tar.gz",
        "truncated.tar.zst",
        "truncated.tar.xz",
        "truncated.tar.bz2",
    ] {
        let t = test_dir(&format!("x-trunc-{name}"));
        let ix = open(&fixture(name));
        assert_eq!(ix.outcome().and_then(|o| o.error.as_deref()), Some(DAMAGED));
        let r = extract(vec![everything(&ix)], &t.path, &mut Script::silent());
        assert!(r.failed >= 1, "{name}: {r:?}");
        for i in &r.issues {
            assert_eq!(
                i.outcome,
                Outcome::Failed(DAMAGED.into()),
                "{name}: {:?}",
                i.path
            );
        }
        for p in walk(&t.path) {
            let n = p.file_name().unwrap().to_string_lossy().into_owned();
            let i: usize = n[1..3].parse().unwrap();
            assert_eq!(
                std::fs::read(&p).unwrap(),
                truncated_member(i),
                "{name}: {n}"
            );
        }
        assert!(r.done >= 1, "{name}: {r:?}");
        assert!(partials(&t.path).is_empty(), "{name}");
    }
}

/// A zip CRC error fails with "archive damaged"; a member shorter than its header fails
/// with "size mismatch"; neither leaves a file.
#[test]
fn a_ar_3_zip_damage_leaves_no_partial_file() {
    let t = test_dir("x-zipdamage");
    let data = noise(5000, 4);
    let mut z = zip_of(&[("crc", 0o644, &data, false), ("ok", 0o644, b"fine", false)]);
    // A byte of the stored data, after the 30-byte local header and the 3-byte name.
    z[33 + 100] ^= 0xff;
    let ix = open(&put(&t.path, "crc.zip", &z));
    let dst = t.join("a");
    std::fs::create_dir(&dst).unwrap();
    let r = extract(vec![everything(&ix)], &dst, &mut Script::silent());
    assert_eq!(
        issues(&r),
        [("crc".to_string(), format!("failed: {DAMAGED}"))]
    );
    assert_eq!(std::fs::read(dst.join("ok")).unwrap(), b"fine");
    assert!(!dst.join("crc").exists() && partials(&dst).is_empty());

    let mut z = zip_of(&[("short", 0o644, b"abc", true)]);
    // The uncompressed size in the local header and in the central directory says 10000.
    z[22..26].copy_from_slice(&10_000u32.to_le_bytes());
    let cd = z.windows(4).rposition(|w| w == b"PK\x01\x02").unwrap();
    z[cd + 24..cd + 28].copy_from_slice(&10_000u32.to_le_bytes());
    let ix = open(&put(&t.path, "short.zip", &z));
    let dst = t.join("b");
    std::fs::create_dir(&dst).unwrap();
    let r = extract(vec![everything(&ix)], &dst, &mut Script::silent());
    assert_eq!(
        issues(&r),
        [("short".to_string(), format!("failed: {SIZE_MISMATCH}"))]
    );
    assert!(walk(&dst).is_empty());
}

// ---- 7z (T8) -------------------------------------------------------------------------------

/// The members of `solid.7z`, as `make.py` writes them.
fn solid_members() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("d/e/deep.txt", b"deep in the tree\n".repeat(40)),
        ("d/f.txt", b"f\n".repeat(300)),
        (
            "\u{fc}\u{f1}\u{ef}/\u{e7}a.txt",
            "\u{e7}a va\n".repeat(50).into_bytes(),
        ),
        ("empty", Vec::new()),
        ("a.txt", b"member a\n".repeat(100)),
        ("b.txt", b"member b\n".repeat(200)),
        ("c.txt", (0..=255u8).collect::<Vec<u8>>().repeat(16)),
    ]
}

/// The regular members of `shapes.7z`, one block each, as `make.py` writes them.
fn shapes_members() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("dir/deflated", b"deflated member\n".repeat(30)),
        ("dir/bzip2ed", b"bzip2 member\n".repeat(30)),
        ("stored", b"stored member\n".to_vec()),
        ("lzma2ed", b"lzma2 member\n".repeat(30)),
        ("lzmaed", b"lzma member\n".repeat(30)),
    ]
}

fn bsdtar() -> bool {
    let ok = std::process::Command::new("bsdtar")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        skip("bsdtar is not installed");
    }
    ok
}

/// bsdtar's extraction of `archive` into `dst`, with its permissions, for A-AR-5's
/// comparison.
fn bsdtar_x(archive: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    let out = std::process::Command::new("bsdtar")
        .env("LC_ALL", "C.UTF-8")
        .arg("-xpf")
        .arg(archive)
        .arg("-C")
        .arg(dst)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "bsdtar -x: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Everything below `root` by relative path: kind, permission bits, content, symlink
/// target, and the mtime of files and directories.
fn facts(root: &Path) -> BTreeMap<PathBuf, String> {
    let mut out = BTreeMap::new();
    for p in walk(root) {
        let m = std::fs::symlink_metadata(&p).unwrap();
        let ft = m.file_type();
        let what = if ft.is_symlink() {
            format!("link -> {:?}", std::fs::read_link(&p).unwrap())
        } else if ft.is_dir() {
            format!(
                "dir {:o} {}.{:09}",
                m.mode() & 0o777,
                m.mtime(),
                m.mtime_nsec()
            )
        } else if ft.is_file() {
            format!(
                "file {:o} {} {}.{:09}",
                m.mode() & 0o777,
                hash(&p),
                m.mtime(),
                m.mtime_nsec()
            )
        } else {
            "special".into()
        };
        out.insert(p.strip_prefix(root).unwrap().to_path_buf(), what);
    }
    out
}

/// A-AR-5 with T8: a solid 7z is read in one pass that decodes its block once per job,
/// whatever the selection and however many groups it spans; a selection of empty members
/// decodes nothing. The extracted tree equals bsdtar's extraction of the archive.
#[test]
fn a_ar_5_7z_a_solid_block_decodes_once_per_job() {
    let t = test_dir("x-7z-solid");
    let ix = open(&fixture("solid.7z"));
    assert!(ix.solid());
    let sys = Sys::default();
    let jobs: [(&str, Vec<Group>, u64); 3] = [
        ("all", vec![everything(&ix)], 1),
        (
            "groups",
            vec![
                group(&ix, &[b"d"], &[b"f.txt"]),
                group(&ix, &[b"d", b"e"], &[b"deep.txt"]),
                group(&ix, &[], &[b"c.txt", b"a.txt", b"link"]),
            ],
            1,
        ),
        ("empty", vec![group(&ix, &[], &[b"empty", b"link"])], 0),
    ];
    for (label, groups, decodes) in jobs {
        let dst = t.join(label);
        std::fs::create_dir(&dst).unwrap();
        let o = ArchiveOrigin::new(&sys, &ix).unwrap();
        let r = copy_from(&sys, &mut Script::silent(), &o, &groups, &dst);
        assert!(r.issues.is_empty() && !r.cancelled, "{label}: {r:?}");
        assert_eq!(o.blocks_decoded(), decodes, "{label}");
        assert!(partials(&dst).is_empty());
    }
    let all = t.join("all");
    for (name, data) in solid_members() {
        assert_eq!(std::fs::read(all.join(name)).unwrap(), data, "{name}");
    }
    assert_eq!(
        std::fs::read_link(all.join("link")).unwrap(),
        Path::new("d/f.txt")
    );
    assert_eq!(
        std::fs::read(t.join("groups/deep.txt")).unwrap(),
        solid_members()[0].1
    );
    let m = std::fs::metadata(all.join("c.txt")).unwrap();
    assert_eq!(m.mtime(), BASE as i64);
    if bsdtar() {
        bsdtar_x(&fixture("solid.7z"), &t.join("ref"));
        assert_eq!(facts(&all), facts(&t.join("ref")));
    }
}

/// A 7z of one block per member opens each member by its locator, in any order; every coder
/// decodes (Copy, LZMA, LZMA2, deflate, bzip2); modes are masked to `0o777`; a FIFO is
/// skipped as a special file; the symlink comes from the index. The tree equals bsdtar's.
#[test]
fn a_ar_5_7z_members_by_locator_in_any_order() {
    let t = test_dir("x-7z-locator");
    let ix = open(&fixture("shapes.7z"));
    assert!(!ix.solid());
    let sys = Sys::default();
    let dst = t.join("all");
    std::fs::create_dir(&dst).unwrap();
    let o = ArchiveOrigin::new(&sys, &ix).unwrap();
    let r = copy_from(&sys, &mut Script::silent(), &o, &[everything(&ix)], &dst);
    assert_eq!(
        issues(&r),
        [("fifo".to_string(), "skipped: special file".to_string())]
    );
    assert_eq!(
        o.blocks_decoded(),
        5,
        "one block per regular member with data"
    );
    for (name, data) in shapes_members() {
        assert_eq!(std::fs::read(dst.join(name)).unwrap(), data, "{name}");
    }
    assert_eq!(mode(&dst.join("dir/bzip2ed")), 0o755, "no setuid (A-3)");
    assert_eq!(mode(&dst.join("dir")), 0o750);
    assert_eq!(mode(&dst.join("stored")), 0o444);
    assert_eq!(std::fs::read(dst.join("empty")).unwrap(), b"");
    assert_eq!(
        std::fs::read_link(dst.join("link")).unwrap(),
        Path::new("dir/deflated")
    );
    let some = t.join("some");
    std::fs::create_dir(&some).unwrap();
    let o = ArchiveOrigin::new(&sys, &ix).unwrap();
    let r = copy_from(
        &sys,
        &mut Script::silent(),
        &o,
        &[
            group(&ix, &[], &[b"lzmaed", b"stored"]),
            group(&ix, &[b"dir"], &[b"bzip2ed"]),
        ],
        &some,
    );
    assert!(r.issues.is_empty(), "{r:?}");
    assert_eq!(o.blocks_decoded(), 3);
    assert_eq!(
        std::fs::read(some.join("bzip2ed")).unwrap(),
        shapes_members()[1].1
    );
    if bsdtar() {
        let reference = t.join("ref");
        bsdtar_x(&fixture("shapes.7z"), &reference);
        std::fs::remove_file(reference.join("fifo")).unwrap();
        std::fs::set_permissions(
            reference.join("dir/bzip2ed"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert_eq!(facts(&dst), facts(&reference));
    }
}

/// A-AR-5 with 7z, against bsdtar: its archives of a tree with nested directories,
/// symlinks, an empty file and directory, non-ASCII names and modes, solid and stored,
/// extract to the tree bsdtar extracts.
#[test]
fn a_ar_5_7z_extracts_as_bsdtar_does() {
    if !bsdtar() {
        return;
    }
    let t = test_dir("x-7z-bsdtar");
    let src = t.join("src");
    for d in 0..6 {
        for f in 0..20 {
            let p = src.join(format!("t/d{d}/f{f}"));
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            write(&p, &noise(100 * (d * 20 + f) + 1, (d * 20 + f) as u64));
        }
    }
    write(&src.join("big"), &noise(600_000, 77));
    write(&src.join("\u{fc}ber.txt"), b"non-ASCII");
    write(&src.join("empty"), b"");
    std::fs::create_dir_all(src.join("emptydir")).unwrap();
    std::os::unix::fs::symlink("t/d1/f3", src.join("rel")).unwrap();
    std::os::unix::fs::symlink("/etc/hostname", src.join("abs")).unwrap();
    std::fs::set_permissions(src.join("t/d2/f5"), std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(src.join("t/d3"), std::fs::Permissions::from_mode(0o750)).unwrap();
    for (label, options) in [
        ("lzma", None),
        ("lzma2", Some("7zip:compression=lzma2")),
        ("store", Some("7zip:compression=store")),
    ] {
        let path = t.join(format!("{label}.7z"));
        let mut c = std::process::Command::new("bsdtar");
        c.args(["--format", "7zip"]);
        if let Some(o) = options {
            c.args(["--options", o]);
        }
        let out = c
            .arg("-C")
            .arg(&src)
            .arg("-cf")
            .arg(&path)
            .arg(".")
            .output()
            .unwrap();
        assert!(out.status.success(), "{label}");
        let ix = open(&path);
        let dst = t.join(format!("{label}-ours"));
        std::fs::create_dir(&dst).unwrap();
        let r = extract(vec![everything(&ix)], &dst, &mut Script::silent());
        assert!(r.issues.is_empty() && !r.cancelled, "{label}: {r:?}");
        let reference = t.join(format!("{label}-bsdtar"));
        bsdtar_x(&path, &reference);
        assert_eq!(facts(&dst), facts(&reference), "{label}");
    }
}

/// A-4 and A-AR-7 with 7z: a block whose LZMA2 dictionary exceeds the cap is never decoded
/// ("archive needs too much memory to decode"), and a block with an AES coder is listed and
/// refused ("encrypted"), by F5 and by a member read; the other blocks extract. Traversal
/// names stay inside the destination (A-AR-2).
#[test]
fn a_ar_2_7z_memory_cap_encryption_and_traversal() {
    let (dst, r, _t) = hostile("large-dict.7z", None);
    assert_eq!(
        issues(&r),
        [("small".to_string(), format!("failed: {NEEDS_MEMORY}"))]
    );
    assert_eq!(std::fs::read(dst.join("other")).unwrap(), b"other\n");
    assert!(!dst.join("small").exists());
    let ix = open(&fixture("large-dict.7z"));
    let mut rd = ix
        .open_read(
            &VPath::parse(b"small").unwrap(),
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    let err = rd.read_to_end(&mut Vec::new()).unwrap_err();
    assert_eq!(err.to_string(), NEEDS_MEMORY);

    let (dst, r, _t) = hostile("encrypted.7z", None);
    assert_eq!(
        issues(&r),
        [(
            "secret.txt".to_string(),
            format!("skipped: {ENCRYPTED_MEMBER}")
        )]
    );
    assert_eq!(std::fs::read(dst.join("plain.txt")).unwrap(), b"plain\n");
    let ix = open(&fixture("encrypted.7z"));
    assert_eq!(
        ix.open_read(
            &VPath::parse(b"secret.txt").unwrap(),
            &Arc::new(AtomicBool::new(false))
        )
        .err(),
        Some(PlaceError::Refused(ENCRYPTED_MEMBER.into()))
    );

    let (dst, r, _t) = hostile("traversal.7z", None);
    assert!(r.issues.is_empty(), "{r:?}");
    assert_eq!(std::fs::read(dst.join("abs/file")).unwrap(), b"abs\n");
    assert_eq!(std::fs::read(dst.join("ok/file")).unwrap(), b"ok\n");
    assert_eq!(walk(&dst).len(), 4, "{:?}", walk(&dst));
}

/// The coders the reader leaves out (zstd, PPMd; P3 D-6) fail their members with
/// "unsupported compression method", by F5 and by a member read; the next block extracts.
#[test]
fn a_ar_5_7z_coders_left_out_are_unsupported() {
    let (dst, r, _t) = hostile("unsupported.7z", None);
    assert_eq!(
        issues(&r),
        [
            ("ppmd".to_string(), format!("failed: {UNSUPPORTED_METHOD}")),
            ("zstd".to_string(), format!("failed: {UNSUPPORTED_METHOD}")),
        ]
    );
    assert_eq!(std::fs::read(dst.join("plain")).unwrap(), b"plain\n");
    let ix = open(&fixture("unsupported.7z"));
    let mut rd = ix
        .open_read(
            &VPath::parse(b"zstd").unwrap(),
            &Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
    let err = rd.read_to_end(&mut Vec::new()).unwrap_err();
    assert_eq!(err.to_string(), UNSUPPORTED_METHOD);
}

/// Member reads (F3, F4, the quick view): the bytes of any member of a solid block, of an
/// empty member, and of the last member of a block whose file list has an empty file
/// between its members; extraction of that archive gives every member.
#[test]
fn a_ar_5_7z_member_reads_and_an_interleaved_block() {
    let ix = open(&fixture("solid.7z"));
    let read = |ix: &ArchiveIndex, p: &str| {
        let mut r = ix
            .open_read(
                &VPath::parse(p.as_bytes()).unwrap(),
                &Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        let mut got = Vec::new();
        r.read_to_end(&mut got).unwrap();
        got
    };
    for (name, data) in solid_members() {
        assert_eq!(read(&ix, name), data, "{name}");
    }
    let ix = open(&fixture("interleaved.7z"));
    assert_eq!(read(&ix, "second"), b"second member\n");
    let t = test_dir("x-7z-interleaved");
    let r = extract(vec![everything(&ix)], &t.path, &mut Script::silent());
    assert!(r.issues.is_empty(), "{r:?}");
    assert_eq!(std::fs::read(t.join("first")).unwrap(), b"first member\n");
    assert_eq!(std::fs::read(t.join("second")).unwrap(), b"second member\n");
    assert_eq!(std::fs::read(t.join("between")).unwrap(), b"");
}

/// A-AR-3 with 7z: a CRC error fails its member with "archive damaged" and leaves no file;
/// the other members extract. In a block of one member (Copy) the damage stays in it; in a
/// solid LZMA block the members after the damage fail too.
#[test]
fn a_ar_3_7z_damage_leaves_no_partial_file() {
    let t = test_dir("x-7z-damage");
    let mut b = std::fs::read(fixture("shapes.7z")).unwrap();
    let at = b
        .windows(14)
        .position(|w| w == b"stored member\n")
        .expect("the stored member's data");
    b[at + 3] ^= 0x20;
    let ix = open(&put(&t.path, "crc.7z", &b));
    let dst = t.join("a");
    std::fs::create_dir(&dst).unwrap();
    let r = extract(
        vec![group(&ix, &[], &[b"stored", b"lzmaed"])],
        &dst,
        &mut Script::silent(),
    );
    assert_eq!(
        issues(&r),
        [("stored".to_string(), format!("failed: {DAMAGED}"))]
    );
    assert_eq!(
        std::fs::read(dst.join("lzmaed")).unwrap(),
        shapes_members()[4].1
    );
    assert!(!dst.join("stored").exists() && partials(&dst).is_empty());

    let mut b = std::fs::read(fixture("solid.7z")).unwrap();
    // A byte in the middle of the one LZMA block, which starts after the 32-byte signature
    // header.
    let len = b.len();
    b[32 + (len - 32) / 4] ^= 0xff;
    let ix = open(&put(&t.path, "solid-crc.7z", &b));
    let dst = t.join("b");
    std::fs::create_dir(&dst).unwrap();
    // The scan could not read the symlink's target in the broken block either.
    assert_eq!(ix.outcome().and_then(|o| o.error.as_deref()), Some(DAMAGED));
    let mut ui = Script::silent();
    let r = extract(vec![everything(&ix)], &dst, &mut ui);
    assert!(ui.asked.is_empty(), "{:?}", ui.asked);
    assert!(r.failed >= 1, "{r:?}");
    for i in &r.issues {
        assert_eq!(i.outcome, Outcome::Failed(DAMAGED.into()), "{:?}", i.path);
    }
    for (name, data) in solid_members() {
        if let Ok(got) = std::fs::read(dst.join(name)) {
            assert_eq!(got, data, "{name}: extracted whole or not at all");
        }
    }
    assert!(partials(&dst).is_empty());
}

// ---- failpoints ---------------------------------------------------------------------------

#[cfg(feature = "failpoints")]
mod failpoints {
    use super::*;
    use manycommander::fsops::failpoints::{Action, Failpoints, Trigger};
    use manycommander::fsops::origin::READ_ONCE;
    use rustix::io::Errno;

    fn sys_with(fp: &Arc<Failpoints>) -> Sys {
        Sys::with_failpoints(Arc::new(AtomicBool::new(false)), fp.clone())
    }

    fn extract_fp(fp: &Arc<Failpoints>, groups: Vec<Group>, dst: &Path, ui: &mut Script) -> Report {
        run_guarded(
            JobSpec::Copy {
                groups,
                dst: Dest::Local(dst.to_path_buf()),
            },
            &sys_with(fp),
            ui,
        )
    }

    /// Replaces the directory `d` with a symlink to `outside`.
    fn swap_for_link(d: PathBuf, outside: PathBuf) -> Action {
        Action::Call(Arc::new(move || {
            let aside = d.with_extension("aside");
            std::fs::rename(&d, &aside).unwrap();
            std::os::unix::fs::symlink(&outside, &d).unwrap();
        }))
    }

    /// A destination directory replaced by a symlink after the walk created it is never
    /// entered: `O_NOFOLLOW` fails with "type changed", and nothing lands outside (A-2).
    /// Tar re-walks it from the destination root in the one pass; zip opens it right after
    /// the `mkdirat`.
    #[test]
    fn a_ar_2_a_directory_swapped_for_a_symlink_is_never_entered() {
        let t = test_dir("xf-swap");
        let mut g = TarGen::new();
        g.file(b"d/f", b"payload");
        let tar = put(&t.path, "a.tar", &g.finish());
        let zip = put(
            &t.path,
            "a.zip",
            &zip_of(&[("d/f", 0o644, b"payload", false)]),
        );
        for (path, step) in [(tar, "pass.walk"), (zip, "copy.opendst")] {
            let x = t.join(path.extension().unwrap());
            let (dst, outside) = (x.join("dst"), x.join("outside"));
            std::fs::create_dir_all(&dst).unwrap();
            std::fs::create_dir_all(&outside).unwrap();
            let fp = Failpoints::new();
            fp.arm(
                step,
                Trigger::Nth(1),
                swap_for_link(dst.join("d"), outside.clone()),
            );
            let ix = open(&path);
            let r = extract_fp(&fp, vec![everything(&ix)], &dst, &mut Script::silent());
            assert!(fp.hits(step) >= 1, "{step}");
            assert!(r.failed >= 1, "{step}: {r:?}");
            assert!(
                r.issues
                    .iter()
                    .all(|i| i.outcome == Outcome::Failed("type changed".into())),
                "{step}: {r:?}"
            );
            assert!(
                walk(&outside).is_empty(),
                "{step}: written through the link"
            );
            assert!(partials(&dst).is_empty());
        }
    }

    /// A hard link is made from the destination inode after an identity check: a file that
    /// replaced the first destination's name before the link is never linked (A-3).
    #[test]
    fn a_ar_5_a_replaced_link_target_is_never_linked() {
        let t = test_dir("xf-linkswap");
        let ix = open(&fixture("hardlinks.tar"));
        let target = t.join("target");
        let fp = Failpoints::new();
        let tt = target.clone();
        fp.arm(
            "link.open",
            Trigger::Nth(1),
            Action::Call(Arc::new(move || {
                let tmp = tt.with_extension("new");
                std::fs::write(&tmp, b"a replacement").unwrap();
                std::fs::rename(&tmp, &tt).unwrap();
            })),
        );
        let r = extract_fp(
            &fp,
            vec![group(&ix, &[], &[b"target", b"hl-ok"])],
            &t.path,
            &mut Script::silent(),
        );
        assert_eq!(fp.hits("link.open"), 1);
        assert_eq!(
            issues(&r),
            [(
                "hl-ok".to_string(),
                format!("skipped: {LINK_NOT_EXTRACTED}")
            )]
        );
        let m = std::fs::metadata(&target).unwrap();
        assert_eq!(m.nlink(), 1, "the replacement is not linked");
        assert_eq!(std::fs::read(&target).unwrap(), b"a replacement");
        assert!(!t.join("hl-ok").exists());
    }

    /// A tar member whose name appears at its commit: "file exists" keeps the temporary
    /// file across the question, and Overwrite commits it without reading the stream again
    /// (the M1 4.7 amendment, A-AR-5).
    #[test]
    fn a_ar_5_a_name_that_appears_at_the_commit_keeps_the_temporary_file() {
        let t = test_dir("xf-late");
        let mut g = TarGen::new();
        g.file(b"late", b"the member").file(b"next", b"next");
        let ix = open(&put(&t.path, "a.tar.gz", &compress("tar.gz", &g.finish())));
        let dst = t.join("dst");
        std::fs::create_dir(&dst).unwrap();
        let fp = Failpoints::new();
        let intruder = dst.join("late");
        fp.arm(
            "commit.rename",
            Trigger::Nth(1),
            Action::Call(Arc::new(move || {
                std::fs::write(&intruder, b"intruder").unwrap()
            })),
        );
        let mut ui = Script::new([Answer::Overwrite]);
        let r = extract_fp(&fp, vec![everything(&ix)], &dst, &mut ui);
        assert!(r.issues.is_empty(), "{r:?}");
        assert!(
            matches!(ui.asked[..], [Question::FileExists { .. }]),
            "{:?}",
            ui.asked
        );
        assert_eq!(std::fs::read(dst.join("late")).unwrap(), b"the member");
        assert_eq!(std::fs::read(dst.join("next")).unwrap(), b"next");
        // Two reads per member: its bytes and the end; none after the question.
        assert_eq!(fp.hits("copy.chunk"), 4, "each member read once");
        assert!(partials(&dst).is_empty());
    }

    /// Cancel in the middle of a member leaves no partial name (I-2), for zip and tar.
    #[test]
    fn a_ar_5_cancel_mid_member_leaves_no_partial_name() {
        let t = test_dir("xf-cancel");
        let big = noise(3 << 20, 5);
        let mut g = TarGen::new();
        g.file(b"big", &big).file(b"after", b"after");
        let tar = put(&t.path, "a.tar.zst", &compress("tar.zst", &g.finish()));
        let zip = put(
            &t.path,
            "a.zip",
            &zip_of(&[
                ("big", 0o644, &big, true),
                ("after", 0o644, b"after", false),
            ]),
        );
        for path in [tar, zip] {
            let dst = t.join(format!(
                "dst-{}",
                path.extension().unwrap().to_string_lossy()
            ));
            std::fs::create_dir(&dst).unwrap();
            let fp = Failpoints::new();
            fp.arm("copy.chunk", Trigger::Nth(2), Action::Cancel);
            let ix = open(&path);
            let r = extract_fp(&fp, vec![everything(&ix)], &dst, &mut Script::silent());
            assert!(r.cancelled, "{path:?}: {r:?}");
            assert!(!dst.join("big").exists(), "{path:?}");
            assert!(partials(&dst).is_empty(), "{path:?}: {:?}", walk(&dst));
        }
    }

    /// Retry after an error (M1 4.5): a zip member is opened again by its locator; a tar
    /// member that nothing was read from yet is lent again; one whose bytes were read
    /// fails, because a stream is never read twice (P3 2.3).
    #[test]
    fn a_ar_5_retry_reopens_a_zip_member_and_never_rereads_a_stream() {
        let t = test_dir("xf-retry");
        let data = noise(3 << 20, 6);
        let mut g = TarGen::new();
        g.file(b"m", &data);
        let tar = put(&t.path, "a.tar", &g.finish());
        let zip = put(&t.path, "a.zip", &zip_of(&[("m", 0o644, &data, true)]));
        for path in [&tar, &zip] {
            let dst = t.join(format!(
                "create-{}",
                path.extension().unwrap().to_string_lossy()
            ));
            std::fs::create_dir(&dst).unwrap();
            let fp = Failpoints::new();
            fp.arm("copy.tmp", Trigger::Nth(1), Action::Errno(Errno::ACCESS));
            let mut ui = Script::new([Answer::Retry]);
            let r = extract_fp(&fp, vec![everything(&open(path))], &dst, &mut ui);
            assert!(r.issues.is_empty(), "{path:?}: {r:?}");
            assert!(matches!(ui.asked[..], [Question::Error { .. }]));
            assert_eq!(std::fs::read(dst.join("m")).unwrap(), data, "{path:?}");
        }
        for (path, want) in [(&zip, None), (&tar, Some(READ_ONCE))] {
            let dst = t.join(format!(
                "write-{}",
                path.extension().unwrap().to_string_lossy()
            ));
            std::fs::create_dir(&dst).unwrap();
            let fp = Failpoints::new();
            fp.arm("copy.write", Trigger::Nth(2), Action::Errno(Errno::IO));
            let mut ui = Script::new([Answer::Retry]);
            let r = extract_fp(&fp, vec![everything(&open(path))], &dst, &mut ui);
            match want {
                None => {
                    assert!(r.issues.is_empty(), "{r:?}");
                    assert_eq!(std::fs::read(dst.join("m")).unwrap(), data);
                }
                Some(why) => {
                    assert_eq!(issues(&r), [("m".to_string(), format!("failed: {why}"))]);
                    assert!(walk(&dst).is_empty());
                }
            }
            assert!(partials(&dst).is_empty());
        }
    }

    /// 7z (T8): cancel in the middle of a member of a solid block leaves no partial name
    /// (I-2); Retry after a write error opens a member of a one-member block again by its
    /// locator, and fails a member of a solid block whose bytes were read ("read in one
    /// pass", P3 2.3).
    #[test]
    fn a_ar_5_7z_cancel_and_retry() {
        let t = test_dir("xf-7z");
        let fp = Failpoints::new();
        fp.arm("copy.write", Trigger::Nth(1), Action::Errno(Errno::IO));
        let dst = t.join("retry-shapes");
        std::fs::create_dir(&dst).unwrap();
        let ix = open(&fixture("shapes.7z"));
        let mut ui = Script::new([Answer::Retry]);
        let r = extract_fp(&fp, vec![group(&ix, &[], &[b"lzmaed"])], &dst, &mut ui);
        assert!(r.issues.is_empty(), "{r:?}");
        assert!(matches!(ui.asked[..], [Question::Error { .. }]));
        assert_eq!(
            std::fs::read(dst.join("lzmaed")).unwrap(),
            shapes_members()[4].1
        );
        let fp = Failpoints::new();
        fp.arm("copy.write", Trigger::Nth(1), Action::Errno(Errno::IO));
        let dst = t.join("retry-solid");
        std::fs::create_dir(&dst).unwrap();
        let ix = open(&fixture("solid.7z"));
        let mut ui = Script::new([Answer::Retry]);
        let r = extract_fp(&fp, vec![group(&ix, &[], &[b"c.txt"])], &dst, &mut ui);
        assert_eq!(
            issues(&r),
            [("c.txt".to_string(), format!("failed: {READ_ONCE}"))]
        );
        assert!(walk(&dst).is_empty() && partials(&dst).is_empty());
        if !bsdtar() {
            return;
        }
        let src = t.join("src");
        std::fs::create_dir(&src).unwrap();
        write(&src.join("big"), &noise(3 << 20, 5));
        write(&src.join("after"), b"after");
        let path = t.join("big.7z");
        let out = std::process::Command::new("bsdtar")
            .args(["--format", "7zip", "-C"])
            .arg(&src)
            .arg("-cf")
            .arg(&path)
            .args(["big", "after"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let ix = open(&path);
        assert!(ix.solid());
        let dst = t.join("cancel");
        std::fs::create_dir(&dst).unwrap();
        let fp = Failpoints::new();
        fp.arm("copy.chunk", Trigger::Nth(2), Action::Cancel);
        let r = extract_fp(&fp, vec![everything(&ix)], &dst, &mut Script::silent());
        assert!(r.cancelled, "{r:?}");
        assert!(!dst.join("big").exists());
        assert!(partials(&dst).is_empty(), "{:?}", walk(&dst));
    }

    /// A bomb never writes more than its declared size: the temporary file is at most the
    /// declared 1000 bytes at every read, and it is gone after the failure (A-4).
    #[test]
    fn a_ar_2_a_bomb_never_writes_past_its_declared_size() {
        let t = test_dir("xf-bomb");
        let ix = open(&fixture("bomb.zip"));
        let fp = Failpoints::new();
        let dir = t.path.clone();
        let largest = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let l = largest.clone();
        fp.arm(
            "copy.chunk",
            Trigger::Always,
            Action::Call(Arc::new(move || {
                for p in partials(&dir) {
                    let n = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                    l.fetch_max(n, std::sync::atomic::Ordering::SeqCst);
                }
            })),
        );
        let r = extract_fp(
            &fp,
            vec![group(&ix, &[], &[b"bomb"])],
            &t.path,
            &mut Script::silent(),
        );
        assert_eq!(
            issues(&r),
            [("bomb".to_string(), format!("failed: {SIZE_MISMATCH}"))]
        );
        assert!(fp.hits("copy.chunk") >= 1);
        assert!(largest.load(std::sync::atomic::Ordering::SeqCst) <= 1000);
        assert!(walk(&t.path).is_empty());
    }
}

// ---- A-AR-6: the view directory and the view copy -------------------------------------------

mod view {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use manycommander::app::App;
    use manycommander::app::event::{Effect, Event};
    use manycommander::cmdline::handoff::Handoff;
    use manycommander::config::Config;
    use manycommander::panel::listing;
    use manycommander::theme::Depth;
    use manycommander::ui::dialog::{Dialog, Purpose};
    use manycommander::viewtemp::{self, Roots, ViewFile, ViewMsg, ViewRequest, ViewRoot};
    use std::time::Instant;

    fn request(ix: &Arc<ArchiveIndex>, member: &[u8], size: u64) -> ViewRequest {
        let path = VPath::parse(member).unwrap();
        ViewRequest {
            id: 1,
            place: ix.clone(),
            name: path.name().unwrap().to_owned(),
            path,
            size,
            cancel: Arc::new(AtomicBool::new(false)),
            remote: false,
        }
    }

    fn prepare(roots: &Roots, req: &ViewRequest) -> Result<ViewFile, String> {
        viewtemp::prepare(roots, req, &|_, _| {})
    }

    /// The runtime directory is `0700` and the user's; one that exists with looser bits is
    /// tightened.
    #[test]
    fn a_ar_6_the_runtime_view_directory_is_private() {
        let t = test_dir("xv-private");
        let rt = t.join("rt");
        std::fs::create_dir_all(rt.join("manycommander")).unwrap();
        std::fs::set_permissions(
            rt.join("manycommander"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let root = ViewRoot::create(Some(&rt), &t.join("tmp")).unwrap();
        assert_eq!(root.path(), rt.join("manycommander/view"));
        assert_eq!(mode(&rt.join("manycommander")), 0o700);
        assert_eq!(mode(&rt.join("manycommander/view")), 0o700);
        assert!(root.private().is_none());
    }

    /// A symlinked `view/` refuses the view, and nothing is made where it points.
    #[test]
    fn a_ar_6_a_symlinked_view_directory_is_refused() {
        let t = test_dir("xv-symlink");
        let rt = t.join("rt");
        std::fs::create_dir_all(rt.join("manycommander")).unwrap();
        std::fs::create_dir(t.join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(t.join("elsewhere"), rt.join("manycommander/view")).unwrap();
        let e = ViewRoot::create(Some(&rt), &t.path).unwrap_err();
        assert!(e.contains("a symlink or not a directory"), "{e}");
        assert!(walk(&t.join("elsewhere")).is_empty());
        // A symlinked manycommander/ as well.
        let rt2 = t.join("rt2");
        std::fs::create_dir(&rt2).unwrap();
        std::os::unix::fs::symlink(t.join("elsewhere"), rt2.join("manycommander")).unwrap();
        assert!(ViewRoot::create(Some(&rt2), &t.path).is_err());
        assert!(walk(&t.join("elsewhere")).is_empty());
    }

    /// Without `XDG_RUNTIME_DIR` the tree goes into a fresh private `0700` directory in the
    /// temporary directory, never into a predictable `manycommander` that someone created
    /// first (here a symlink to a trap); the private directory goes on exit when empty.
    #[test]
    fn a_ar_6_without_a_runtime_directory_a_fresh_private_one_is_made() {
        let t = test_dir("xv-mkdtemp");
        let tmp = t.join("tmp");
        std::fs::create_dir_all(t.join("trap")).unwrap();
        std::fs::create_dir(&tmp).unwrap();
        std::os::unix::fs::symlink(t.join("trap"), tmp.join("manycommander")).unwrap();
        let root = ViewRoot::create(None, &tmp).unwrap();
        let private = root.private().unwrap().to_path_buf();
        let leaf = private.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            leaf.starts_with("manycommander-") && leaf.len() == 30,
            "{leaf}"
        );
        assert_eq!(private.parent(), Some(tmp.as_path()));
        assert_eq!(mode(&private), 0o700);
        assert_eq!(root.path(), private.join("manycommander/view"));
        assert!(
            walk(&t.join("trap")).is_empty(),
            "the pre-made name is not used"
        );
        let other = ViewRoot::create(None, &tmp).unwrap();
        assert_ne!(other.private(), root.private(), "a new name each time");
        root.remove_private();
        other.remove_private();
        assert!(!private.exists());
        assert_eq!(
            std::fs::read_dir(&tmp).unwrap().count(),
            1,
            "only the trap link is left"
        );
    }

    /// A view directory owned by another user refuses the view: in a user namespace, a
    /// bind mount of a directory whose owner is not mapped stands in for another user's.
    #[test]
    fn a_ar_6_a_foreign_owner_refuses_the_view() {
        if !in_userns("view::a_ar_6_a_foreign_owner_refuses_the_view") {
            return;
        }
        let t = test_dir("xv-foreign");
        let rt = t.join("rt");
        std::fs::create_dir_all(rt.join("manycommander")).unwrap();
        bind_mount(Path::new("/usr/share"), &rt.join("manycommander"));
        let e = ViewRoot::create(Some(&rt), &t.path).unwrap_err();
        umount(&rt.join("manycommander"));
        assert!(e.contains("owned by another user"), "{e}");
    }

    /// The copy is `0600` in a new `0700` directory, byte-identical; an unchanged copy is
    /// removed with its directory; an edited one is kept and its path returned, also when
    /// the editor renamed a new file over it with the same size and times.
    #[test]
    fn a_ar_6_the_copy_is_0600_and_an_edit_is_kept() {
        let t = test_dir("xv-copy");
        let data = noise(200_000, 7);
        let zip = put(
            &t.path,
            "a.zip",
            &zip_of(&[("d/m.txt", 0o755, &data, true)]),
        );
        let mut g = TarGen::new();
        g.file(b"x", b"first").file(b"d/m.txt", &data);
        let tgz = put(&t.path, "a.tar.gz", &compress("tar.gz", &g.finish()));
        let roots = Roots::new(Some(t.join("rt")), t.join("tmp"));
        std::fs::create_dir(t.join("rt")).unwrap();
        for path in [zip, tgz] {
            let ix = open(&path);
            let req = request(&ix, b"d/m.txt", data.len() as u64);
            let f = prepare(&roots, &req).unwrap();
            assert_eq!(f.path(), f.dir.join("m.txt"));
            assert!(f.dir.starts_with(t.join("rt/manycommander/view")));
            assert_eq!(mode(&f.dir), 0o700);
            assert_eq!(mode(&f.path()), 0o600, "{path:?}");
            assert_eq!(std::fs::read(f.path()).unwrap(), data);
            // Unchanged: the copy and its directory go.
            assert_eq!(viewtemp::check(&roots, &f), Ok(None));
            assert!(!f.dir.exists());
            // Edited in place: kept.
            let f = prepare(&roots, &req).unwrap();
            std::fs::OpenOptions::new()
                .append(true)
                .open(f.path())
                .unwrap()
                .write_all(b"edit")
                .unwrap();
            assert_eq!(viewtemp::check(&roots, &f), Ok(Some(f.path())));
            assert!(f.path().exists());
            // Replaced by rename with the same bytes and times: a new inode is an edit.
            let f = prepare(&roots, &req).unwrap();
            let before = std::fs::metadata(f.path()).unwrap();
            let tmp = f.dir.join(".m.txt.swp");
            std::fs::write(&tmp, &data).unwrap();
            let file = std::fs::File::options().write(true).open(&tmp).unwrap();
            file.set_modified(before.modified().unwrap()).unwrap();
            drop(file);
            std::fs::rename(&tmp, f.path()).unwrap();
            let after = std::fs::metadata(f.path()).unwrap();
            assert_eq!((after.len(), after.mtime()), (before.len(), before.mtime()));
            assert_eq!(viewtemp::check(&roots, &f), Ok(Some(f.path())));
        }
    }

    /// A member longer or shorter than it declares is not viewed ("size mismatch"), and
    /// nothing is left behind; an encrypted one is refused before any copy.
    #[test]
    fn a_ar_6_a_bad_member_leaves_no_copy() {
        let t = test_dir("xv-bad");
        let roots = Roots::new(Some(t.path.clone()), t.join("tmp"));
        let ix = open(&fixture("bomb.zip"));
        let e = prepare(&roots, &request(&ix, b"bomb", 1000)).unwrap_err();
        assert_eq!(e, SIZE_MISMATCH);
        let e = prepare(&roots, &request(&ix, b"small", 3)).unwrap_err();
        assert_eq!(e, SIZE_MISMATCH, "shorter than the request's size");
        let ix = open(&fixture("encrypted.zip"));
        let e = prepare(&roots, &request(&ix, b"secret.txt", 12)).unwrap_err();
        assert_eq!(e, ENCRYPTED_MEMBER);
        assert!(walk(&t.join("manycommander/view")).is_empty());
    }

    // -- the app ------------------------------------------------------------------------

    fn app(left: &Path, right: &Path) -> App {
        let config = Config {
            pager: Some("view-pager".into()),
            editor: Some("view-editor --flag".into()),
            ..Config::default()
        };
        App::new(
            left.to_path_buf(),
            right.to_path_buf(),
            right.to_path_buf(),
            config,
            None,
            Depth::NoColor,
            jiff::tz::TimeZone::UTC,
        )
    }

    /// Performs listings, archive opens and view copies synchronously, as the runtime
    /// does on its threads; returns the hand-offs and view checks it did not perform.
    fn run(a: &mut App, roots: &Roots, fx: Vec<Effect>) -> Vec<Effect> {
        let cache = IndexCache::default();
        let mut left = Vec::new();
        for e in fx {
            let msgs = std::cell::RefCell::new(Vec::new());
            let send = |m| msgs.borrow_mut().push(Event::Listing(m));
            match e {
                Effect::List(req, alive) => {
                    listing::guarded(&req, &send, listing::list);
                    alive.finish();
                }
                Effect::OpenArchive(req, alive) => {
                    archive::open(&req, &cache, &send);
                    alive.finish();
                }
                Effect::Relist(req, alive) => {
                    archive::relist(&req, &send);
                    alive.finish();
                }
                Effect::PrepareView(req, alive) => {
                    let m = match viewtemp::prepare(roots, &req, &|_, _| {}) {
                        Ok(file) => ViewMsg::Ready { id: req.id, file },
                        Err(error) => ViewMsg::Failed { id: req.id, error },
                    };
                    alive.finish();
                    msgs.borrow_mut().push(Event::View(m));
                }
                e @ (Effect::Run(_) | Effect::CheckView(_) | Effect::Open(_)) => left.push(e),
                _ => {}
            }
            for m in msgs.into_inner() {
                let more = a.update(m);
                left.extend(run(a, roots, more));
            }
        }
        a.panel_mut().ensure_sorted();
        left
    }

    fn press(a: &mut App, code: KeyCode) -> Vec<Effect> {
        a.update(Event::Key(
            KeyEvent::new(code, KeyModifiers::NONE),
            Instant::now(),
        ))
    }

    fn status(a: &App) -> Option<&str> {
        a.status.as_ref().map(|s| s.text.as_str())
    }

    /// F3 on a picture member (M1 6 amendment of 2026-10-05): the copy is prepared as for the
    /// pager and opened in its application; it stays until exit, when an unchanged copy goes.
    #[test]
    fn f3_on_a_picture_member_opens_the_copy_in_its_application() {
        let t = test_dir("xv-open");
        let roots = Roots::new(Some(t.join("rt")), t.join("tmp"));
        std::fs::create_dir(t.join("rt")).unwrap();
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_ustar();
        h.set_size(4);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, "pic.png", &b"\x89PNG"[..]).unwrap();
        let work = t.join("work");
        std::fs::create_dir(&work).unwrap();
        put(&work, "a.tar", &b.into_inner().unwrap());
        let mut a = app(&work, &t.path);
        let fx = a.start();
        run(&mut a, &roots, fx);
        a.panel_mut().cursor_to_name(b"a.tar");
        let fx = press(&mut a, KeyCode::Enter);
        run(&mut a, &roots, fx);
        a.panel_mut().cursor_to_name(b"pic.png");
        let fx = press(&mut a, KeyCode::F(3));
        assert!(matches!(fx[..], [Effect::PrepareView(..)]), "{fx:?}");
        let left = run(&mut a, &roots, fx);
        let [Effect::Open(copy)] = &left[..] else {
            panic!("{left:?}");
        };
        assert!(
            copy.starts_with(t.join("rt/manycommander/view")),
            "{copy:?}"
        );
        assert_eq!(std::fs::read(copy).unwrap(), b"\x89PNG");
        assert_eq!(a.opened.len(), 1);
        // On exit the unchanged copy goes with its directory.
        let f = a.opened.pop().unwrap();
        assert_eq!(viewtemp::check(&roots, &f), Ok(None));
        assert!(!copy.exists() && !copy.parent().unwrap().exists());
    }

    /// F3, F4 and `Enter` on a member (P3 3.4): the copy is prepared off the UI thread and
    /// handed to the pager or editor by argv; an unchanged copy goes after the hand-off, an
    /// edited one (here replaced by rename) is kept and its path reported; a member above
    /// 256 MB asks first; an encrypted one is refused.
    #[test]
    fn a_ar_6_f3_f4_and_enter_on_members() {
        let t = test_dir("xv-app");
        let roots = Roots::new(Some(t.join("rt")), t.join("tmp"));
        std::fs::create_dir(t.join("rt")).unwrap();
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_ustar();
        h.set_size(5);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, "doc.txt", &b"hello"[..]).unwrap();
        // A member that declares 300 MB (its data is cut: only the question matters).
        b.append_pax_extensions([("size", &b"314572800"[..])])
            .unwrap();
        let mut h = tar::Header::new_ustar();
        h.set_path("huge").unwrap();
        h.set_size(1);
        h.set_mode(0o644);
        h.set_cksum();
        b.append(&h, &b"x"[..]).unwrap();
        let work = t.join("work");
        std::fs::create_dir(&work).unwrap();
        put(&work, "a.tar", &b.into_inner().unwrap());
        let mut a = app(&work, &t.path);
        let fx = a.start();
        run(&mut a, &roots, fx);
        a.panel_mut().cursor_to_name(b"a.tar");
        let fx = press(&mut a, KeyCode::Enter);
        run(&mut a, &roots, fx);
        assert!(a.panel().archive().is_some());

        // F3: the pager gets the copy's path as its own argument.
        a.panel_mut().cursor_to_name(b"doc.txt");
        let fx = press(&mut a, KeyCode::F(3));
        assert!(matches!(fx[..], [Effect::PrepareView(..)]), "{fx:?}");
        assert!(a.view_line().is_some_and(|l| l.contains("doc.txt")));
        let left = run(&mut a, &roots, fx);
        let [Effect::Run(Handoff::Program { argv, cwd })] = &left[..] else {
            panic!("{left:?}");
        };
        assert_eq!(cwd, &work);
        assert_eq!(argv.len(), 2);
        assert_eq!(argv[0], "view-pager");
        let copy = PathBuf::from(&argv[1]);
        assert!(
            copy.starts_with(t.join("rt/manycommander/view")),
            "{copy:?}"
        );
        assert_eq!(std::fs::read(&copy).unwrap(), b"hello");
        assert_eq!(mode(&copy), 0o600);
        assert!(a.view.is_none());
        // The pager returns: the unchanged copy goes.
        let fx = a.update(Event::ChildDone {
            status: String::new(),
            output: None,
        });
        let checks: Vec<_> = run(&mut a, &roots, fx)
            .into_iter()
            .filter_map(|e| match e {
                Effect::CheckView(f) => Some(f),
                _ => None,
            })
            .collect();
        assert_eq!(checks.len(), 1);
        let (kept, error) = match viewtemp::check(&roots, &checks[0]) {
            Ok(k) => (k, None),
            Err(e) => (None, Some(e)),
        };
        assert_eq!((kept.clone(), error.clone()), (None, None));
        a.update(Event::View(ViewMsg::Checked {
            kept,
            error,
            remote: false,
        }));
        assert!(!copy.exists() && !copy.parent().unwrap().exists());

        // F4: the editor replaces the copy by rename; the copy is kept and reported.
        let fx = press(&mut a, KeyCode::F(4));
        let left = run(&mut a, &roots, fx);
        let [Effect::Run(Handoff::Program { argv, .. })] = &left[..] else {
            panic!("{left:?}");
        };
        assert_eq!(argv[..2], ["view-editor", "--flag"]);
        let copy = PathBuf::from(&argv[2]);
        let tmp = copy.with_extension("new");
        std::fs::write(&tmp, b"hello, edited").unwrap();
        std::fs::rename(&tmp, &copy).unwrap();
        let fx = a.update(Event::ChildDone {
            status: String::new(),
            output: None,
        });
        let left = run(&mut a, &roots, fx);
        let Some(Effect::CheckView(f)) =
            left.into_iter().find(|e| matches!(e, Effect::CheckView(_)))
        else {
            panic!("no view check");
        };
        let kept = viewtemp::check(&roots, &f).unwrap();
        assert_eq!(kept.as_deref(), Some(copy.as_path()));
        a.update(Event::View(ViewMsg::Checked {
            kept,
            error: None,
            remote: false,
        }));
        assert_eq!(
            status(&a),
            Some(
                format!(
                    "archives are read-only; your edited copy is at {}",
                    copy.display()
                )
                .as_str()
            )
        );
        assert_eq!(std::fs::read(&copy).unwrap(), b"hello, edited");

        // Enter views a member too.
        let fx = press(&mut a, KeyCode::Enter);
        let [Effect::PrepareView(req, _)] = &fx[..] else {
            panic!("{fx:?}");
        };
        // Esc cancels the preparation: a thread that sees the flag removes what it made.
        press(&mut a, KeyCode::Esc);
        assert!(a.view.is_none());
        assert_eq!(status(&a), Some("view cancelled"));
        assert!(run(&mut a, &roots, fx.clone()).is_empty());
        let view_dir = t.join("rt/manycommander/view");
        let copies = || walk(&view_dir).into_iter().filter(|p| p.is_file()).count();
        assert_eq!(copies(), 1, "only the kept edited copy");
        // A copy that was finished when the cancel came is removed when it arrives.
        let late = ViewRequest {
            cancel: Arc::new(AtomicBool::new(false)),
            ..req.clone()
        };
        let file = viewtemp::prepare(&roots, &late, &|_, _| {}).unwrap();
        let fx = a.update(Event::View(ViewMsg::Ready { id: late.id, file }));
        let [Effect::CheckView(f)] = &fx[..] else {
            panic!("{fx:?}");
        };
        assert_eq!(viewtemp::check(&roots, f), Ok(None));
        assert_eq!(copies(), 1);

        // A member above 256 MB asks first.
        a.panel_mut().cursor_to_name(b"huge");
        assert!(press(&mut a, KeyCode::F(3)).is_empty());
        let Some(Dialog::Confirm { purpose, .. }) = &a.dialog else {
            panic!("the size question");
        };
        assert!(matches!(
            purpose,
            Purpose::ViewLarge {
                size: 314_572_800,
                ..
            }
        ));
        press(&mut a, KeyCode::Esc);
        assert!(a.dialog.is_none() && a.view.is_none());
    }

    /// An encrypted member is refused with "encrypted" before any copy (A-AR-7).
    #[test]
    fn a_ar_7_f3_refuses_an_encrypted_member() {
        let t = test_dir("xv-enc");
        let roots = Roots::new(Some(t.path.clone()), t.join("tmp"));
        std::fs::copy(fixture("encrypted.zip"), t.join("e.zip")).unwrap();
        let mut a = app(&t.path, &t.path);
        let fx = a.start();
        run(&mut a, &roots, fx);
        a.panel_mut().cursor_to_name(b"e.zip");
        let fx = press(&mut a, KeyCode::Enter);
        run(&mut a, &roots, fx);
        a.panel_mut().cursor_to_name(b"secret.txt");
        assert!(press(&mut a, KeyCode::F(3)).is_empty());
        assert_eq!(status(&a), Some(ENCRYPTED_MEMBER));
        assert!(!t.join("manycommander").exists());
    }
}

// ---- the binary on a pty -----------------------------------------------------------------

mod pty {
    use super::*;
    use common::tui::*;
    use std::time::Duration;

    const T: Duration = Duration::from_secs(10);
    const F4: &[u8] = b"\x1bOS";

    fn script(p: &Path, body: &str) {
        std::fs::write(p, body).unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// F3 on a zip member hands a `0600` copy under `$XDG_RUNTIME_DIR` to `$PAGER`, and the
    /// copy is gone after the pager exits; F4 with an editor that changes it keeps it and
    /// says where (P3 3.4, A-AR-6).
    #[test]
    fn a_ar_6_f3_and_f4_through_the_terminal_hand_off() {
        let t = test_dir("xp-view");
        let work = t.join("work");
        let rt = t.join("rt");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&rt).unwrap();
        std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o700)).unwrap();
        put(
            &work,
            "a.zip",
            &zip_of(&[("doc.txt", 0o644, b"hello", true)]),
        );
        let out = t.join("pager.out");
        let pager = t.join("pager.sh");
        script(
            &pager,
            "#!/bin/sh\nstat -c %a \"$1\" > \"$OUT.mode\"\ncat \"$1\" > \"$OUT\"\n",
        );
        let editor = t.join("editor.sh");
        script(&editor, "#!/bin/sh\nprintf ', edited' >> \"$1\"\n");
        let mut ui = Tui::spawn(
            &[work.to_str().unwrap()],
            &t.path,
            &[
                ("XDG_RUNTIME_DIR", rt.to_str().unwrap()),
                ("PAGER", pager.to_str().unwrap()),
                ("EDITOR", editor.to_str().unwrap()),
                ("OUT", out.to_str().unwrap()),
            ],
            100,
            30,
        );
        assert!(ui.wait_for("10Quit", T), "{}", ui.screen());
        assert!(ui.wait_for("a.zip", T), "{}", ui.screen());
        // Rows: .., a.zip. Into the archive once its listing is complete.
        ui.keys(&[DOWN, ENTER]);
        assert!(ui.wait_for("a.zip:/", T), "{}", ui.screen());
        assert!(ui.wait_for("doc.txt", T), "{}", ui.screen());
        ui.keys(&[DOWN, F3]);
        assert!(
            ui.wait_until(T, |_| std::fs::read(&out).is_ok_and(|b| b == b"hello")),
            "{}",
            ui.screen()
        );
        assert!(ui.wait_for("10Quit", T));
        let view = rt.join("manycommander/view");
        assert_eq!(
            std::fs::read_to_string(t.join("pager.out.mode"))
                .unwrap()
                .trim(),
            "600"
        );
        assert_eq!(mode(&rt.join("manycommander")), 0o700);
        assert!(
            ui.wait_until(T, |_| std::fs::read_dir(&view)
                .is_ok_and(|d| d.count() == 0)),
            "the unchanged copy is removed"
        );
        ui.keys(&[F4]);
        assert!(ui.wait_for("your edited copy is at", T), "{}", ui.screen());
        let kept: Vec<_> = walk(&view).into_iter().filter(|p| p.is_file()).collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(std::fs::read(&kept[0]).unwrap(), b"hello, edited");
        ui.keys(&[F10]);
        assert_eq!(ui.wait_exit(T), Some(0));
    }
}
