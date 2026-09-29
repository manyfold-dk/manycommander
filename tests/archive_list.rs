//! Archive index and listing (P3 3.1-3.3, T2, and 7z in T8): A-AR-1 (every format against
//! `bsdtar`'s mtree output), A-AR-4 (navigation), the listing halves of A-AR-2 and A-AR-3
//! (the committed hostile fixtures, made by `tests/fixtures/archive/make.py`), and the index
//! half of A-RES-1 (the cache bounds and the entry cap).

mod common;

use common::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use manycommander::app::App;
use manycommander::app::event::{Effect, Event};
use manycommander::archive::detect::Want;
use manycommander::archive::index::{
    self as ix, ENCRYPTED, IMPLICIT, Limits, NodeKind, SPARSE, UNSAFE_PATH,
};
use manycommander::archive::{self, ArchiveIndex, DAMAGED, IndexCache, NEEDS_MEMORY, OpenRequest};
use manycommander::config::Config;
use manycommander::panel::listing::{self, Alive, ListingMsg};
use manycommander::panel::{Row, Source};
use manycommander::provider::VPath;
use manycommander::theme::Depth;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const BASE: u64 = 1_700_000_000;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/archive")
        .join(name)
}

/// What one synchronous open sent.
struct Opened {
    index: Option<Arc<ArchiveIndex>>,
    msgs: Vec<ListingMsg>,
}

impl Opened {
    fn index(&self) -> &Arc<ArchiveIndex> {
        self.index.as_ref().expect("the archive opened")
    }

    fn failed(&self) -> Option<String> {
        self.msgs.iter().find_map(|m| match m {
            ListingMsg::Failed { error, .. } => Some(error.clone()),
            _ => None,
        })
    }

    fn error(&self) -> Option<String> {
        self.index().outcome().and_then(|o| o.error.clone())
    }

    /// The names the scan sent for the root, as the panel received them.
    fn batch_names(&self) -> Vec<Vec<u8>> {
        let mut v = Vec::new();
        for m in &self.msgs {
            if let ListingMsg::Batch { entries, names, .. } = m {
                v.extend(entries.iter().map(|e| e.name(names).to_vec()));
            }
        }
        v.sort();
        v
    }
}

fn request(path: &Path, want: Want, limits: Limits) -> OpenRequest {
    OpenRequest {
        slot: 7,
        generation: 1,
        archive: path.to_path_buf(),
        want,
        inner: VPath::root(),
        cancel: Arc::new(AtomicBool::new(false)),
        tz: jiff::tz::TimeZone::UTC,
        limits,
    }
}

fn open_req(req: &OpenRequest, cache: &IndexCache) -> Opened {
    let msgs = RefCell::new(Vec::new());
    let index = RefCell::new(None);
    archive::open(req, cache, &|m| {
        if let ListingMsg::Opened { index: i, .. } = &m {
            *index.borrow_mut() = Some(i.clone());
        }
        msgs.borrow_mut().push(m);
    });
    Opened {
        index: index.into_inner(),
        msgs: msgs.into_inner(),
    }
}

fn open_with(path: &Path, cache: &IndexCache, limits: Limits) -> Opened {
    let name = path.file_name().unwrap().as_encoded_bytes();
    open_req(&request(path, Want::of_name(name), limits), cache)
}

fn open(path: &Path) -> Opened {
    open_with(path, &IndexCache::default(), Limits::default())
}

/// One indexed member, as the differential compares it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Mine {
    kind: NodeKind,
    size: u64,
    mode: u32,
    mtime: Option<i64>,
    link: Option<Vec<u8>>,
}

/// Every node of a complete index that stands for a member, by its path.
fn rows(ix: &ArchiveIndex) -> BTreeMap<Vec<u8>, Mine> {
    let t = ix.tree().expect("complete");
    let mut out = BTreeMap::new();
    for id in 1..t.len() as u32 {
        let n = t.node(id);
        if n.flags & IMPLICIT != 0 {
            continue;
        }
        let path = t.path_of(id).to_bytes()[1..].to_vec();
        out.insert(
            path,
            Mine {
                kind: n.kind,
                size: t.meta(id).size,
                mode: n.mode as u32,
                mtime: (n.flags & ix::NO_TIME == 0).then_some(n.mtime),
                link: match n.kind {
                    NodeKind::Symlink => t.link_target(id).map(<[u8]>::to_vec),
                    NodeKind::HardLink => t.hard_target(id).map(|h| t.path_of(h).to_bytes()),
                    _ => None,
                },
            },
        );
    }
    out
}

fn get<'a>(rows: &'a BTreeMap<Vec<u8>, Mine>, path: &str) -> &'a Mine {
    rows.get(path.as_bytes()).unwrap_or_else(|| {
        panic!(
            "{path} is not listed: {:?}",
            rows.keys()
                .map(|k| String::from_utf8_lossy(k).into_owned())
                .collect::<Vec<_>>()
        )
    })
}

fn has_tool(tool: &str) -> bool {
    let ok = Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        skip(&format!("{tool} is not installed"));
    }
    ok
}

// ---- generated fixtures (A-AR-1) --------------------------------------------------------

/// A tar written header by header, so names the `tar` crate would refuse (a leading `/`,
/// invalid UTF-8, a newline) go in as bytes.
struct TarGen {
    b: tar::Builder<Vec<u8>>,
}

impl TarGen {
    fn new() -> TarGen {
        TarGen {
            b: tar::Builder::new(Vec::new()),
        }
    }

    fn long(&mut self, kind: u8, name: &[u8]) {
        let mut h = tar::Header::new_gnu();
        h.as_old_mut().name[..13].copy_from_slice(b"././@LongLink");
        h.set_entry_type(tar::EntryType::new(kind));
        h.set_mode(0o644);
        h.set_size(name.len() as u64 + 1);
        h.set_cksum();
        let mut data = name.to_vec();
        data.push(0);
        self.b.append(&h, &data[..]).unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn raw(&mut self, name: &[u8], kind: u8, mode: u32, mtime: u64, link: &[u8], data: &[u8]) {
        let mut h = tar::Header::new_gnu();
        if name.len() > 100 {
            self.long(b'L', name);
        }
        let n = name.len().min(100);
        h.as_old_mut().name[..n].copy_from_slice(&name[..n]);
        if link.len() > 100 {
            self.long(b'K', link);
        }
        let l = link.len().min(100);
        h.as_old_mut().linkname[..l].copy_from_slice(&link[..l]);
        h.set_entry_type(tar::EntryType::new(kind));
        h.set_mode(mode);
        h.set_mtime(mtime);
        h.set_uid(0);
        h.set_gid(0);
        h.set_size(data.len() as u64);
        h.set_cksum();
        self.b.append(&h, data).unwrap();
    }

    fn file(&mut self, name: &[u8], mode: u32, mtime: u64, data: &[u8]) {
        self.raw(name, b'0', mode, mtime, b"", data);
    }

    fn dir(&mut self, name: &[u8], mode: u32, mtime: u64) {
        self.raw(name, b'5', mode, mtime, b"", b"");
    }

    /// A member whose name comes from a pax `path` record.
    fn pax_file(&mut self, name: &[u8], mtime: u64, data: &[u8]) {
        self.b.append_pax_extensions([("path", name)]).unwrap();
        self.raw(b"pax-placeholder", b'0', 0o644, mtime, b"", data);
    }

    fn finish(self) -> Vec<u8> {
        self.b.into_inner().unwrap()
    }
}

const LEAD: &[u8] = b"/abs/file";

/// The A-AR-1 tar: 10k files, deep and implicit trees, duplicates, `./` and `/` prefixes,
/// a newline and invalid UTF-8, pax and GNU long names, symlinks and hard links.
fn a_ar_1_tar() -> Vec<u8> {
    let mut t = TarGen::new();
    t.dir(b"big/", 0o755, BASE);
    for d in 0..100u64 {
        if d % 2 == 0 {
            t.dir(format!("big/d{d:02}/").as_bytes(), 0o750, BASE + d);
        }
        for f in 0..100u64 {
            let i = d * 100 + f;
            let data = vec![b'x'; (i % 7) as usize];
            t.file(
                format!("big/d{d:02}/f{i:04}").as_bytes(),
                if i % 3 == 0 { 0o600 } else { 0o644 },
                BASE + i,
                &data,
            );
        }
    }
    let deep: Vec<String> = (b'a'..=b'z').map(|c| (c as char).to_string()).collect();
    t.file(
        format!("deep/{}/leaf", deep.join("/")).as_bytes(),
        0o644,
        BASE,
        b"deep",
    );
    t.file(b"imp/x/y", 0o644, BASE, b"implicit parents");
    t.file(b"dup", 0o644, BASE, b"1");
    t.file(b"dup", 0o640, BASE + 7, b"55555");
    t.dir(b"dupdir/", 0o700, BASE);
    t.dir(b"dupdir/", 0o750, BASE + 3);
    t.file(b"./dot/file", 0o644, BASE, b"dot");
    t.file(LEAD, 0o644, BASE, b"abs");
    t.file(b"nl\nname", 0o644, BASE, b"newline");
    t.file(b"bad\xffname", 0o644, BASE, b"invalid utf-8");
    let pax = format!("pax/{}/leaf", "p".repeat(150));
    t.pax_file(pax.as_bytes(), BASE + 11, b"pax long name");
    let gnu = format!("gnu/{}/leaf", "g".repeat(150));
    t.file(gnu.as_bytes(), 0o644, BASE + 12, b"gnu long name");
    t.raw(b"links/rel", b'2', 0o777, BASE, b"../big/d00/f0000", b"");
    t.raw(b"links/abs", b'2', 0o777, BASE, b"/etc/hostname", b"");
    let far = format!("../{}", "t".repeat(150));
    t.raw(b"links/longlink", b'2', 0o777, BASE, far.as_bytes(), b"");
    t.raw(b"links/hard", b'1', 0o644, BASE, b"big/d00/f0001", b"");
    t.file(b"exec", 0o755, BASE, b"#!/bin/sh\n");
    t.file(b"setuid", 0o4755, BASE, b"#!/bin/sh\n");
    t.finish()
}

/// `data` compressed as two members (gzip), frames (zstd), streams (xz, bzip2), split at a
/// 512-byte boundary near the middle (A-AR-1).
fn compress(ext: &str, data: &[u8]) -> Vec<u8> {
    let half = (data.len() / 2) & !511;
    let mut out = Vec::new();
    for part in [&data[..half], &data[half..]] {
        match ext {
            "gz" => {
                let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                e.write_all(part).unwrap();
                out.extend(e.finish().unwrap());
            }
            "zst" => out.extend(zstd::stream::encode_all(part, 3).unwrap()),
            "xz" => {
                let mut w =
                    lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1))
                        .unwrap();
                w.write_all(part).unwrap();
                out.extend(w.finish().unwrap());
            }
            "bz2" => {
                let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
                e.write_all(part).unwrap();
                out.extend(e.finish().unwrap());
            }
            _ => unreachable!("{ext}"),
        }
    }
    out
}

/// The A-AR-1 zip: 10k entries, deep and implicit trees, a duplicate, `./` and `/`
/// prefixes, a newline, a CP437 byte name, a Unicode path extra field, symlinks, an
/// extended timestamp.
fn a_ar_1_zip() -> Vec<u8> {
    use zip::write::{ExtendedFileOptions, FileOptions, SimpleFileOptions};
    let dos = zip::DateTime::from_date_and_time(2023, 11, 14, 22, 13, 21).unwrap();
    let opts = |mode: u32| {
        SimpleFileOptions::default()
            .unix_permissions(mode)
            .last_modified_time(dos)
    };
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    w.add_directory("big/", opts(0o755)).unwrap();
    for d in 0..100u64 {
        if d % 2 == 0 {
            w.add_directory(format!("big/d{d:02}/"), opts(0o750))
                .unwrap();
        }
        for f in 0..100u64 {
            let i = d * 100 + f;
            let method = if i % 2 == 0 {
                zip::CompressionMethod::Stored
            } else {
                zip::CompressionMethod::Deflated
            };
            w.start_file(
                format!("big/d{d:02}/f{i:04}"),
                opts(if i % 3 == 0 { 0o600 } else { 0o644 }).compression_method(method),
            )
            .unwrap();
            w.write_all(&vec![b'x'; (i % 7) as usize]).unwrap();
        }
    }
    let deep: Vec<String> = (b'a'..=b'z').map(|c| (c as char).to_string()).collect();
    for (name, data) in [
        (format!("deep/{}/leaf", deep.join("/")), &b"deep"[..]),
        ("imp/x/y".into(), b"implicit parents"),
        ("dup1".into(), b"1"),
        ("dup2".into(), b"55555"),
        ("./dot/file".into(), b"dot"),
        ("nl\nname".into(), b"newline"),
        ("badXname".into(), b"a CP437 byte"),
        ("exec".into(), b"#!/bin/sh\n"),
    ] {
        let mode = if name == "exec" { 0o755 } else { 0o644 };
        w.start_file(name, opts(mode)).unwrap();
        w.write_all(data).unwrap();
    }
    // A Unicode path extra field (0x7075) over an ASCII header name; written under an
    // unreserved id and renamed below, because the writer checks the field against a name
    // it does not have yet.
    let header = b"uni-placeholder";
    let mut crc = flate2::Crc::new();
    crc.update(header);
    let mut up = vec![1u8];
    up.extend_from_slice(&crc.sum().to_le_bytes());
    up.extend_from_slice("\u{fc}n\u{ef}c\u{f6}d\u{e9}.txt".as_bytes());
    let mut uopts: FileOptions<ExtendedFileOptions> = FileOptions::default()
        .unix_permissions(0o644)
        .last_modified_time(dos);
    uopts.add_extra_data(UP_STANDIN, &up[..], false).unwrap();
    w.start_file(std::str::from_utf8(header).unwrap(), uopts)
        .unwrap();
    w.write_all(b"unicode path").unwrap();
    // An extended timestamp at an odd second: the index takes it over the DOS time.
    let mut ut = vec![1u8];
    ut.extend_from_slice(&((BASE + 1) as u32).to_le_bytes());
    let mut topts: FileOptions<ExtendedFileOptions> = FileOptions::default()
        .unix_permissions(0o644)
        .last_modified_time(dos);
    topts.add_extra_data(0x5455, &ut[..], false).unwrap();
    topts.add_extra_data(0x5455, &ut[..], true).unwrap();
    w.start_file("ext-time", topts).unwrap();
    w.write_all(b"extended timestamp").unwrap();
    w.add_symlink("links/rel", "../big/d00/f0000", opts(0o777))
        .unwrap();
    w.add_symlink("links/abs", "/etc/hostname", opts(0o777))
        .unwrap();
    let mut bytes = w.finish().unwrap().into_inner();
    let patch = |bytes: &mut Vec<u8>, from: &[u8], to: &[u8]| {
        let mut n = 0;
        let mut i = 0;
        while i + from.len() <= bytes.len() {
            if &bytes[i..i + from.len()] == from {
                bytes[i..i + from.len()].copy_from_slice(to);
                n += 1;
            }
            i += 1;
        }
        n
    };
    assert_eq!(
        patch(&mut bytes, b"dup2", b"dup1"),
        2,
        "local and central names"
    );
    assert_eq!(patch(&mut bytes, b"badXname", b"bad\xffname"), 2);
    // The stand-in id, with the field's length, version and CRC after it, is unique.
    let mut from = UP_STANDIN.to_le_bytes().to_vec();
    from.extend_from_slice(&(up.len() as u16).to_le_bytes());
    from.extend_from_slice(&up[..5]);
    let mut to = 0x7075u16.to_le_bytes().to_vec();
    to.extend_from_slice(&from[2..]);
    assert_eq!(patch(&mut bytes, &from, &to), 2);
    bytes
}

/// An extra field id the writer does not reserve, standing in for the Unicode path field.
const UP_STANDIN: u16 = 0x7a7a;

/// A tar with a GNU sparse member, made by GNU tar (A-AR-1).
fn sparse_tar(dir: &Path) -> Option<PathBuf> {
    let src = dir.join("sparse-src");
    std::fs::create_dir_all(&src).unwrap();
    let f = std::fs::File::create(src.join("holes")).unwrap();
    f.set_len(1 << 20).unwrap();
    use std::os::unix::fs::FileExt;
    f.write_at(b"data", 512 * 1024).unwrap();
    write(&src.join("regular"), b"regular");
    let out = dir.join("sparse.tar");
    let ok = Command::new("tar")
        .args([
            "--sparse",
            "--format=gnu",
            "--mtime=@1700000000",
            "--owner=0",
            "--group=0",
            "-cf",
        ])
        .arg(&out)
        .arg("-C")
        .arg(&src)
        .args(["holes", "regular"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        skip("GNU tar could not make a sparse member");
        return None;
    }
    Some(out)
}

// ---- the bsdtar differential ------------------------------------------------------------

#[derive(Debug, Default)]
struct Theirs {
    kind: String,
    size: Option<u64>,
    mode: Option<u32>,
    time: Option<i64>,
    link: Option<Vec<u8>>,
}

/// mtree's escapes: `\` and three octal digits is a byte.
fn unescape(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'\\'
            && i + 4 <= s.len()
            && s[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            let v = (s[i + 1] - b'0') as u32 * 64
                + (s[i + 2] - b'0') as u32 * 8
                + (s[i + 3] - b'0') as u32;
            out.push(v as u8);
            i += 4;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    out
}

/// `bsdtar`'s mtree listing of `archive` under `TZ=UTC`, by the path A-1 makes of each
/// name; a later duplicate replaces an earlier one, as in the index.
fn mtree(archive: &Path) -> BTreeMap<Vec<u8>, Theirs> {
    let mut at = std::ffi::OsString::from("@");
    at.push(archive);
    let out = Command::new("bsdtar")
        .env("TZ", "UTC")
        .env("LC_ALL", "C.UTF-8")
        .args([
            "-cf",
            "-",
            "--format=mtree",
            "--options=!all,type,size,mode,time,link",
        ])
        .arg(at)
        .output()
        .expect("bsdtar runs");
    assert!(
        out.status.success(),
        "bsdtar failed on {}: {}",
        archive.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let mut map = BTreeMap::new();
    for line in out.stdout.split(|&b| b == b'\n') {
        if line.is_empty() || line[0] == b'#' || line.starts_with(b"/set") {
            continue;
        }
        let mut words = line.split(|&b| b == b' ');
        let name = unescape(words.next().unwrap());
        let Ok(path) = VPath::parse(&name) else {
            panic!("bsdtar lists an unsafe name: {name:?}");
        };
        if path.is_root() {
            continue;
        }
        let mut t = Theirs::default();
        for w in words {
            let (k, v) = match w.iter().position(|&b| b == b'=') {
                Some(i) => (&w[..i], &w[i + 1..]),
                None => continue,
            };
            let v_str = String::from_utf8_lossy(v);
            match k {
                b"type" => t.kind = v_str.into_owned(),
                b"size" => t.size = v_str.parse().ok(),
                b"mode" => t.mode = u32::from_str_radix(&v_str, 8).ok(),
                b"time" => t.time = v_str.split('.').next().and_then(|s| s.parse().ok()),
                b"link" => t.link = Some(unescape(v)),
                _ => {}
            }
        }
        map.insert(path.to_bytes()[1..].to_vec(), t);
    }
    map
}

/// The A-AR-1 differential: every member bsdtar lists matches the index in name bytes,
/// type, size, link target, mode, and mtime at `resolution` seconds; the index lists
/// nothing more. `skips` are names left out with their reason (reported, not matched).
fn differential(
    label: &str,
    ix: &ArchiveIndex,
    archive: &Path,
    resolution: i64,
    skips: &[(&[u8], &str)],
) {
    let theirs = mtree(archive);
    let ours = rows(ix);
    let skipped = |p: &[u8]| skips.iter().find(|(s, _)| *s == p).map(|(_, why)| *why);
    let mut compared = 0;
    for (path, t) in &theirs {
        let shown = String::from_utf8_lossy(path);
        if let Some(why) = skipped(path) {
            eprintln!("SKIP {label}: {shown}: {why}");
            continue;
        }
        let Some(o) = ours.get(path) else {
            panic!("{label}: bsdtar lists {shown:?}, the index does not");
        };
        let kind = match o.kind {
            NodeKind::File | NodeKind::HardLink => "file",
            NodeKind::Dir => "dir",
            NodeKind::Symlink => "link",
            NodeKind::Special => "special",
        };
        assert_eq!(t.kind, kind, "{label}: type of {shown}");
        match o.kind {
            NodeKind::File => assert_eq!(t.size, Some(o.size), "{label}: size of {shown}"),
            // The one field left out: see HARD_SIZE.
            NodeKind::HardLink => eprintln!("SKIP {label}: size of {shown}: {HARD_SIZE}"),
            _ => {}
        }
        if let Some(m) = t.mode {
            assert_eq!(m & 0o7777, o.mode, "{label}: mode of {shown}");
        }
        if let (Some(a), Some(b)) = (t.time, o.mtime) {
            assert_eq!(
                a.div_euclid(resolution),
                b.div_euclid(resolution),
                "{label}: mtime of {shown}"
            );
        }
        if o.kind == NodeKind::Symlink {
            assert_eq!(
                t.link.as_deref(),
                o.link.as_deref(),
                "{label}: link of {shown}"
            );
        }
        compared += 1;
    }
    for path in ours.keys() {
        if skipped(path).is_none() {
            assert!(
                theirs.contains_key(path),
                "{label}: the index lists {:?}, bsdtar does not",
                String::from_utf8_lossy(path)
            );
        }
    }
    assert!(compared > 0, "{label}: nothing compared");
}

/// Hard links: bsdtar's mtree reports the header's size 0; the index shows the target's.
const HARD_SIZE: &str = "hard link: bsdtar reports the header's size 0, the index the target's";

#[test]
fn a_ar_1_every_format_lists_as_bsdtar_does() {
    if !has_tool("bsdtar") {
        return;
    }
    let t = test_dir("ar1-formats");
    let tar = a_ar_1_tar();
    let mut files = vec![("tar", t.join("a.tar"), tar.clone())];
    for ext in ["gz", "zst", "xz", "bz2"] {
        files.push((ext, t.join(format!("a.tar.{ext}")), compress(ext, &tar)));
    }
    for (label, path, bytes) in &files {
        write(path, bytes);
        let o = open(path);
        assert!(o.failed().is_none(), "{label}: {:?}", o.failed());
        let ix = o.index();
        assert!(
            ix.is_complete() && o.error().is_none(),
            "{label}: {:?}",
            o.error()
        );
        let tree = ix.tree().unwrap();
        let r = rows(ix);
        // 10k files, 51 explicit directories in big/.
        assert_eq!(
            r.keys().filter(|k| k.starts_with(b"big/")).count(),
            10_000 + 50,
            "{label}"
        );
        // Implicit directories are synthesized, without a time.
        let deep = tree
            .lookup(&VPath::parse(b"deep/a/b").unwrap())
            .expect("an implicit directory");
        assert_eq!(tree.node(deep).kind, NodeKind::Dir);
        assert_ne!(tree.node(deep).flags & IMPLICIT, 0);
        assert!(tree.lookup(&VPath::parse(b"imp/x/y").unwrap()).is_some());
        // The last duplicate wins.
        assert_eq!(get(&r, "dup").size, 5, "{label}");
        assert_eq!(get(&r, "dup").mode, 0o640, "{label}");
        assert_eq!(get(&r, "dupdir").mode, 0o750, "{label}");
        // `./` and a leading `/` are dropped; the latter is counted.
        assert_eq!(get(&r, "dot/file").size, 3);
        assert_eq!(get(&r, "abs/file").size, 3);
        assert_eq!(tree.stats.leading_slash, 1, "{label}");
        // Names are bytes.
        assert!(r.contains_key(&b"nl\nname"[..]) && r.contains_key(&b"bad\xffname"[..]));
        // Pax and GNU long names fold into their member: no rows of their own.
        let pax = format!("pax/{}/leaf", "p".repeat(150));
        let gnu = format!("gnu/{}/leaf", "g".repeat(150));
        assert_eq!(get(&r, &pax).size, 13);
        assert_eq!(get(&r, &gnu).mtime, Some((BASE + 12) as i64));
        let has = |k: &[u8], w: &[u8]| k.windows(w.len()).any(|x| x == w);
        assert!(
            !r.keys()
                .any(|k| has(k, b"@LongLink") || has(k, b"PaxHeader"))
        );
        assert!(!r.contains_key(&b"pax-placeholder"[..]));
        // Symlinks keep their target text; a hard link names an earlier node.
        assert_eq!(
            get(&r, "links/abs").link.as_deref(),
            Some(&b"/etc/hostname"[..])
        );
        assert_eq!(
            get(&r, "links/longlink").link.as_ref().map(Vec::len),
            Some(153)
        );
        assert_eq!(
            get(&r, "links/hard").link.as_deref(),
            Some(&b"/big/d00/f0001"[..])
        );
        assert_eq!(get(&r, "links/hard").size, 1);
        assert_eq!(get(&r, "setuid").mode, 0o4755);
        assert_eq!(tree.stats.skipped_total(), 0, "{label}");
        differential(label, ix, path, 1, &[]);
    }

    // A GNU sparse member is skipped and left out of the comparison (A-AR-1).
    if let Some(path) = sparse_tar(&t.path) {
        let o = open(&path);
        let ix = o.index();
        let tree = ix.tree().unwrap();
        assert_eq!(tree.stats.skipped(SPARSE), 1);
        assert_eq!(
            tree.stats.skipped_text().as_deref(),
            Some("1 member not shown: sparse member")
        );
        let r = rows(ix);
        assert!(r.contains_key(&b"regular"[..]) && !r.contains_key(&b"holes"[..]));
        differential(
            "sparse",
            ix,
            &path,
            1,
            &[(
                b"holes",
                "sparse member: bsdtar expands it, the index skips it",
            )],
        );
    }
}

#[test]
fn a_ar_1_zip_lists_as_bsdtar_does() {
    if !has_tool("bsdtar") {
        return;
    }
    let t = test_dir("ar1-zip");
    let path = t.join("a.zip");
    write(&path, &a_ar_1_zip());
    let o = open(&path);
    assert!(o.failed().is_none(), "{:?}", o.failed());
    let ix = o.index();
    let r = rows(ix);
    assert_eq!(r.keys().filter(|k| k.starts_with(b"big/")).count(), 10_050);
    let tree = ix.tree().unwrap();
    assert_ne!(
        tree.node(tree.lookup(&VPath::parse(b"deep/a").unwrap()).unwrap())
            .flags
            & IMPLICIT,
        0
    );
    // The duplicate: the last central entry wins.
    assert_eq!(get(&r, "dup1").size, 5);
    assert_eq!(get(&r, "dot/file").size, 3);
    // A valid Unicode path extra field names the entry; the CP437 byte stays a byte.
    let uni = "\u{fc}n\u{ef}c\u{f6}d\u{e9}.txt";
    assert_eq!(get(&r, uni).size, 12);
    assert!(!r.contains_key(&b"uni-placeholder"[..]));
    assert!(r.contains_key(&b"bad\xffname"[..]));
    // DOS time as local time at 2 s resolution; the extended timestamp exactly.
    let dos = jiff::civil::date(2023, 11, 14)
        .at(22, 13, 20, 0)
        .to_zoned(jiff::tz::TimeZone::UTC)
        .unwrap()
        .timestamp()
        .as_second();
    assert_eq!(get(&r, "exec").mtime, Some(dos));
    assert_eq!(get(&r, "ext-time").mtime, Some((BASE + 1) as i64));
    assert_eq!(
        get(&r, "links/rel").link.as_deref(),
        Some(&b"../big/d00/f0000"[..])
    );
    assert_eq!(get(&r, "big").mode, 0o755);
    // bsdtar keeps the CP437 byte as a byte under a UTF-8 locale too, so no name is
    // left out of the comparison.
    differential("zip", ix, &path, 2, &[]);
}

// ---- hostile fixtures (A-AR-2, A-AR-3: the listing halves) -------------------------------

#[test]
fn a_ar_2_hostile_members_are_skipped_or_flagged() {
    let o = open(&fixture("traversal.tar"));
    let tree = o.index().tree().unwrap();
    let r = rows(o.index());
    assert_eq!(
        r.keys().cloned().collect::<Vec<_>>(),
        [&b"abs/file"[..], b"dot/ok", b"ok/file"]
    );
    assert_eq!(tree.stats.skipped(UNSAFE_PATH), 2, "../evil and a/../../b");
    assert_eq!(tree.stats.leading_slash, 1);

    let o = open(&fixture("nul-name.tar"));
    assert_eq!(o.index().tree().unwrap().stats.skipped(UNSAFE_PATH), 1);
    assert_eq!(rows(o.index()).len(), 1);

    // A member below a symlink member (CVE-2025-29787), as tar and as zip.
    for name in ["symlink-write-through.tar", "symlink-write-through.zip"] {
        let o = open(&fixture(name));
        let r = rows(o.index());
        assert_eq!(get(&r, "link").kind, NodeKind::Symlink, "{name}");
        assert_eq!(
            get(&r, "link").link.as_deref(),
            Some(&b"../outside"[..]),
            "{name}"
        );
        assert!(!r.contains_key(&b"link/pwned"[..]), "{name}");
        assert!(r.contains_key(&b"fine"[..]), "{name}");
        assert_eq!(
            o.index().tree().unwrap().stats.skipped(UNSAFE_PATH),
            1,
            "{name}"
        );
    }

    // A symlink, then a directory member of the same name (RUSTSEC-2026-0067): the last
    // member wins, and nothing is followed.
    let o = open(&fixture("symlink-chmod.tar"));
    let d = get(&rows(o.index()), "d").clone();
    assert_eq!((d.kind, d.mode), (NodeKind::Dir, 0o777));

    // A pax size over a different ustar size (RUSTSEC-2026-0068): the pax size wins, and
    // the header hidden in the data is data.
    let o = open(&fixture("pax-size.tar"));
    let r = rows(o.index());
    assert_eq!(r.keys().cloned().collect::<Vec<_>>(), [&b"a"[..], b"b"]);
    assert_eq!(get(&r, "a").size, 1024);
    assert!(o.error().is_none());

    // Hard links resolve only inside the index (A-3).
    let o = open(&fixture("hardlinks.tar"));
    let r = rows(o.index());
    for name in ["hl-abs", "hl-up", "hl-missing"] {
        assert_eq!(get(&r, name).kind, NodeKind::HardLink, "{name}");
        assert_eq!(get(&r, name).link, None, "{name} names no node");
    }
    assert_eq!(get(&r, "hl-ok").link.as_deref(), Some(&b"/target"[..]));

    // Devices and FIFOs are listed as special; privilege bits are listed as stored (the
    // extraction masks them, A-3).
    let o = open(&fixture("special.tar"));
    let r = rows(o.index());
    for name in ["dev-null", "dev-block", "fifo"] {
        assert_eq!(get(&r, name).kind, NodeKind::Special, "{name}");
    }
    assert_eq!(get(&r, "setuid").mode, 0o4755);
    assert_eq!(get(&r, "setgid-dir").mode, 0o2755);

    // Two central entries over one local entry: both listed.
    let o = open(&fixture("overlap.zip"));
    assert_eq!(
        rows(o.index()).keys().cloned().collect::<Vec<_>>(),
        [&b"a"[..], b"b"]
    );

    // A zip bomb lists its declared size; the extraction's output count bounds it (T3).
    let o = open(&fixture("bomb.zip"));
    assert_eq!(get(&rows(o.index()), "bomb").size, 1000);

    // A-AR-7's listing half: an encrypted entry is listed and flagged.
    let o = open(&fixture("encrypted.zip"));
    let tree = o.index().tree().unwrap();
    let id = tree.lookup(&VPath::parse(b"secret.txt").unwrap()).unwrap();
    assert_ne!(tree.node(id).flags & ix::ENCRYPTED, 0);
}

#[test]
fn a_ar_2_decoders_are_bounded() {
    // A zstd frame and an xz block that need more than the 128 MiB cap (A-4).
    for name in ["large-window.tar.zst", "large-dict.tar.xz"] {
        let o = open(&fixture(name));
        assert!(o.failed().is_none(), "{name}: {:?}", o.failed());
        assert_eq!(o.error().as_deref(), Some(NEEDS_MEMORY), "{name}");
    }
    // A GNU long name that declares 64 MiB is not read into memory.
    let start = Instant::now();
    let o = open(&fixture("longname-bomb.tar.zst"));
    assert_eq!(o.error().as_deref(), Some(DAMAGED));
    assert!(start.elapsed() < Duration::from_secs(10));
    // A 1 GiB member of zeros: the scan inflates and discards it, and lists what follows.
    let o = open(&fixture("bomb.tar.zst"));
    let r = rows(o.index());
    assert_eq!(get(&r, "zeros").size, 1 << 30);
    assert!(r.contains_key(&b"after"[..]));
    assert!(o.error().is_none());
}

/// P-20 and A-AR-2: a scan inside a decompression bomb stops soon after its cancel flag,
/// sends nothing more, and is not cached.
#[test]
fn a_ar_2_a_bomb_scan_is_cancellable() {
    let cache = Arc::new(IndexCache::default());
    let req = request(&fixture("bomb.tar.zst"), Want::Magic, Limits::default());
    let cancel = req.cancel.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let c = cache.clone();
    let alive = Alive::running();
    let a = alive.clone();
    std::thread::spawn(move || {
        archive::open(&req, &c, &|m| {
            let _ = tx.send(m);
        });
        a.finish();
    });
    let first = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let ListingMsg::Opened { index, .. } = first else {
        panic!("{first:?}");
    };
    std::thread::sleep(Duration::from_millis(30));
    assert!(!index.is_complete(), "still inflating the zeros");
    let at = Instant::now();
    cancel.store(true, Ordering::SeqCst);
    while alive.is_running() {
        assert!(
            at.elapsed() < Duration::from_secs(2),
            "the scan did not stop"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    eprintln!("bomb scan stopped {:?} after the cancel", at.elapsed());
    assert!(rx.try_iter().all(|m| !matches!(m, ListingMsg::Done { .. })));
    assert!(cache.cached().is_empty(), "a cancelled scan is not cached");
    assert!(!index.is_complete());
}

#[test]
fn a_ar_3_truncated_archives_list_what_precedes_the_damage() {
    for name in [
        "truncated.tar.zst",
        "truncated.tar.xz",
        "truncated.tar.gz",
        "truncated.tar.bz2",
        "truncated.tar",
    ] {
        let o = open(&fixture(name));
        assert!(o.failed().is_none(), "{name}: {:?}", o.failed());
        assert_eq!(o.error().as_deref(), Some(DAMAGED), "{name}");
        let r = rows(o.index());
        assert!(
            !r.is_empty(),
            "{name}: the members before the damage are listed"
        );
        assert!(r.len() < 24, "{name}: {}", r.len());
        assert!(r.contains_key(&b"m00.txt"[..]), "{name}");
        // The scan still ends with Done: the panel shows the partial listing.
        assert!(
            o.msgs.iter().any(|m| matches!(m, ListingMsg::Done { .. })),
            "{name}"
        );
        eprintln!("{name}: {} members before the damage", r.len());
    }
    // A zip without its central directory cannot be listed at all.
    let t = test_dir("ar3-zip");
    let whole = std::fs::read(fixture("overlap.zip")).unwrap();
    write(&t.join("cut.zip"), &whole[..whole.len() / 2]);
    let o = open(&t.join("cut.zip"));
    assert_eq!(o.failed().as_deref(), Some(DAMAGED));
}

// ---- 7z (T8) -----------------------------------------------------------------------------

/// A 7z of the tree below `src`, written by bsdtar with `options` (`7zip:compression=...`).
fn bsdtar_7z(src: &Path, out: &Path, options: Option<&str>) {
    let mut c = Command::new("bsdtar");
    c.env("LC_ALL", "C.UTF-8").args(["--format", "7zip"]);
    if let Some(o) = options {
        c.args(["--options", o]);
    }
    let status = c
        .arg("-C")
        .arg(src)
        .arg("-cf")
        .arg(out)
        .arg(".")
        .output()
        .expect("bsdtar runs");
    assert!(
        status.status.success(),
        "bsdtar: {}",
        String::from_utf8_lossy(&status.stderr)
    );
}

/// Sets a file's modification time.
fn set_mtime(p: &Path, secs: u64) {
    let f = std::fs::File::options().write(true).open(p).unwrap();
    f.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap();
}

/// The A-AR-1 tree for bsdtar's 7z writer: 10k files in 100 directories, a deep tree, an
/// empty directory and an empty file, names with a newline, non-ASCII and invalid UTF-8,
/// symlinks, modes and mtimes. (A file system holds no duplicates, `./` or `/` prefixes;
/// the committed fixtures cover those shapes.)
fn seven_tree(src: &Path) {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    for d in 0..100u64 {
        let dir = src.join(format!("big/d{d:02}"));
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..100u64 {
            let i = d * 100 + f;
            let p = dir.join(format!("f{i:04}"));
            write(&p, &vec![b'x'; (i % 7) as usize]);
            let mode = if i % 3 == 0 { 0o600 } else { 0o644 };
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
            set_mtime(&p, BASE + i);
        }
    }
    let deep: Vec<String> = (b'a'..=b'z').map(|c| (c as char).to_string()).collect();
    let leaf = src.join(format!("deep/{}/leaf", deep.join("/")));
    std::fs::create_dir_all(leaf.parent().unwrap()).unwrap();
    write(&leaf, b"deep");
    std::fs::create_dir_all(src.join("emptydir")).unwrap();
    write(&src.join("empty"), b"");
    write(&src.join("nl\nname"), b"newline");
    write(&src.join("\u{fc}\u{f1}\u{ef}.txt"), b"non-ASCII");
    write(
        &src.join(std::ffi::OsStr::from_bytes(b"bad\xffname")),
        b"invalid utf-8",
    );
    std::fs::create_dir_all(src.join("links")).unwrap();
    std::os::unix::fs::symlink("../big/d00/f0000", src.join("links/rel")).unwrap();
    std::os::unix::fs::symlink("/etc/hostname", src.join("links/abs")).unwrap();
    std::os::unix::fs::symlink("missing", src.join("links/dangling")).unwrap();
    for (name, mode) in [("exec", 0o755), ("setuid", 0o4755)] {
        write(&src.join(name), b"#!/bin/sh\n");
        std::fs::set_permissions(src.join(name), std::fs::Permissions::from_mode(mode)).unwrap();
    }
}

/// A-AR-1 with 7z: bsdtar's archives of one tree, with each of its compressions (solid, and
/// one block per file for `store`), list as bsdtar lists them: names (bsdtar stores the invalid UTF-8 name as it can), types, sizes,
/// modes, mtimes and symlink targets, which the scan reads from the members' data.
#[test]
fn a_ar_1_7z_lists_as_bsdtar_does() {
    if !has_tool("bsdtar") {
        return;
    }
    let t = test_dir("ar1-7z");
    let src = t.join("src");
    seven_tree(&src);
    for (label, options) in [
        ("lzma", None),
        ("lzma2", Some("7zip:compression=lzma2")),
        ("deflate", Some("7zip:compression=deflate")),
        ("bzip2", Some("7zip:compression=bzip2")),
        ("store", Some("7zip:compression=store")),
    ] {
        let path = t.join(format!("{label}.7z"));
        bsdtar_7z(&src, &path, options);
        let at = Instant::now();
        let o = open(&path);
        assert!(o.failed().is_none(), "{label}: {:?}", o.failed());
        let ix = o.index();
        assert!(
            ix.is_complete() && o.error().is_none(),
            "{label}: {:?}",
            o.error()
        );
        eprintln!("7z {label}: listed in {:?}", at.elapsed());
        // bsdtar compresses everything into one block, and stores each file in its own.
        assert_eq!(ix.solid(), label != "store", "{label}");
        let r = rows(ix);
        assert_eq!(
            r.keys().filter(|k| k.starts_with(b"big/")).count(),
            10_000 + 100,
            "{label}"
        );
        assert_eq!(get(&r, "big/d01/f0105").mtime, Some((BASE + 105) as i64));
        assert_eq!(get(&r, "big/d01/f0105").size, 105 % 7);
        assert_eq!(get(&r, "big/d00/f0003").mode, 0o600);
        assert_eq!(get(&r, "setuid").mode, 0o4755);
        assert_eq!(get(&r, "empty").kind, NodeKind::File);
        assert_eq!(get(&r, "emptydir").kind, NodeKind::Dir);
        assert!(r.contains_key(&b"nl\nname"[..]));
        assert_eq!(get(&r, "\u{fc}\u{f1}\u{ef}.txt").size, 9);
        assert_eq!(
            get(&r, "links/rel").link.as_deref(),
            Some(&b"../big/d00/f0000"[..])
        );
        assert_eq!(
            get(&r, "links/abs").link.as_deref(),
            Some(&b"/etc/hostname"[..])
        );
        assert_eq!(get(&r, "links/dangling").kind, NodeKind::Symlink);
        let tree = ix.tree().unwrap();
        assert_eq!(tree.stats.skipped_total(), 0, "{label}");
        differential(label, ix, &path, 1, &[]);
    }
}

/// The committed 7z fixtures list as bsdtar lists them: a solid block with a tree,
/// non-ASCII names, an empty file and a symlink; one block per member with every coder
/// and every kind (a FIFO is "special" to the index).
#[test]
fn a_ar_1_7z_fixtures_list_as_bsdtar_does() {
    let o = open(&fixture("solid.7z"));
    assert!(o.failed().is_none() && o.error().is_none(), "{:?}", o.msgs);
    let ix = o.index();
    assert!(ix.solid());
    let r = rows(ix);
    let names: Vec<String> = r
        .keys()
        .map(|k| String::from_utf8_lossy(k).into())
        .collect();
    assert_eq!(
        names,
        [
            "a.txt",
            "b.txt",
            "c.txt",
            "d",
            "d/e",
            "d/e/deep.txt",
            "d/f.txt",
            "empty",
            "link",
            "\u{fc}\u{f1}\u{ef}",
            "\u{fc}\u{f1}\u{ef}/\u{e7}a.txt",
        ]
    );
    assert_eq!(get(&r, "link").link.as_deref(), Some(&b"d/f.txt"[..]));
    assert_eq!(get(&r, "c.txt").size, 4096);
    assert_eq!(get(&r, "empty").size, 0);
    assert_eq!(get(&r, "d/e/deep.txt").mtime, Some(BASE as i64));
    let o2 = open(&fixture("shapes.7z"));
    let shapes = o2.index();
    assert!(!shapes.solid(), "one block per member");
    let s = rows(shapes);
    assert_eq!(get(&s, "dir").mode, 0o750);
    assert_eq!(get(&s, "dir/bzip2ed").mode, 0o4755);
    assert_eq!(get(&s, "stored").mode, 0o444, "read-only, no Unix mode");
    assert_eq!(get(&s, "fifo").kind, NodeKind::Special);
    assert_eq!(get(&s, "link").link.as_deref(), Some(&b"dir/deflated"[..]));
    assert_eq!(get(&s, "lzma2ed").size, 390);
    if !has_tool("bsdtar") {
        return;
    }
    differential("solid.7z", ix, &fixture("solid.7z"), 1, &[]);
    differential(
        "shapes.7z",
        shapes,
        &fixture("shapes.7z"),
        1,
        &[(b"fifo", "a FIFO: bsdtar says fifo, the index special")],
    );
}

/// A-AR-2 with 7z, the listing half: `../` and absolute names, a name that is not valid
/// UTF-16 (the crate would refuse the whole header; bsdtar skips it too), an encrypted
/// block, a dictionary above the cap (listing decodes no data), a header that declares 16 Mi
/// files (stopped before the crate builds them), an empty file between the members of a
/// block.
#[test]
fn a_ar_2_7z_hostile_members_are_skipped_or_flagged() {
    let o = open(&fixture("traversal.7z"));
    let tree = o.index().tree().unwrap();
    assert_eq!(tree.stats.skipped(UNSAFE_PATH), 2);
    assert_eq!(tree.stats.leading_slash, 1);
    let r = rows(o.index());
    let names: Vec<&[u8]> = r.keys().map(Vec::as_slice).collect();
    assert_eq!(
        names,
        [&b"abs/file"[..], b"ok/file"],
        "abs and ok are implicit"
    );

    let o = open(&fixture("bad-name.7z"));
    assert!(o.failed().is_none(), "{:?}", o.failed());
    let tree = o.index().tree().unwrap();
    assert_eq!(tree.stats.skipped(UNSAFE_PATH), 1);
    assert_eq!(archive::root_names(o.index()), ["ok"]);

    let o = open(&fixture("encrypted.7z"));
    let tree = o.index().tree().unwrap();
    let flag = |n: &[u8]| {
        let id = tree.lookup(&VPath::parse(n).unwrap()).unwrap();
        tree.node(id).flags & ENCRYPTED != 0
    };
    assert!(flag(b"secret.txt") && !flag(b"plain.txt"));

    let o = open(&fixture("large-dict.7z"));
    assert!(o.error().is_none(), "{:?}", o.error());
    assert_eq!(rows(o.index()).len(), 2);

    let at = Instant::now();
    let o = open(&fixture("header-bomb.7z"));
    assert_eq!(
        o.error().as_deref(),
        Some("listing stopped at 1,000,000 entries")
    );
    assert!(rows(o.index()).is_empty());
    assert!(at.elapsed() < Duration::from_secs(10), "{:?}", at.elapsed());

    let o = open(&fixture("interleaved.7z"));
    let r = rows(o.index());
    assert_eq!(get(&r, "first").size, 13);
    assert_eq!(get(&r, "second").size, 14);
    assert_eq!(get(&r, "between").size, 0);
}

/// P-20 with 7z: the rows come from the header at once; reading a symlink target behind a
/// large member decodes the block up to it, and that stops soon after the cancel flag; the
/// scan sends nothing more and is not cached.
#[test]
fn a_ar_2_7z_a_scan_reading_symlink_targets_is_cancellable() {
    if !has_tool("bsdtar") {
        return;
    }
    let t = test_dir("ar2-7z-cancel");
    let src = t.join("src");
    std::fs::create_dir(&src).unwrap();
    // 256 MiB of zeros (a sparse file), then a symlink, in one deflate block.
    std::fs::File::create(src.join("big"))
        .unwrap()
        .set_len(256 << 20)
        .unwrap();
    std::os::unix::fs::symlink("big", src.join("zlink")).unwrap();
    let path = t.join("a.7z");
    let out = Command::new("bsdtar")
        .args([
            "--format",
            "7zip",
            "--options",
            "7zip:compression=deflate",
            "-C",
        ])
        .arg(&src)
        .arg("-cf")
        .arg(&path)
        .args(["big", "zlink"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let cache = Arc::new(IndexCache::default());
    let req = request(&path, Want::Magic, Limits::default());
    let cancel = req.cancel.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let c = cache.clone();
    let alive = Alive::running();
    let a = alive.clone();
    std::thread::spawn(move || {
        archive::open(&req, &c, &|m| {
            let _ = tx.send(m);
        });
        a.finish();
    });
    let first = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let ListingMsg::Opened { index, .. } = first else {
        panic!("{first:?}");
    };
    // Both rows arrive before the block is decoded for the link's target.
    let mut rows = 0;
    while rows < 2 {
        if let ListingMsg::Batch { entries, .. } = rx.recv_timeout(Duration::from_secs(10)).unwrap()
        {
            rows += entries.len();
        }
    }
    assert!(!index.is_complete(), "still decoding the block");
    let at = Instant::now();
    cancel.store(true, Ordering::SeqCst);
    while alive.is_running() {
        assert!(
            at.elapsed() < Duration::from_secs(2),
            "the scan did not stop"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    eprintln!("7z scan stopped {:?} after the cancel", at.elapsed());
    assert!(rx.try_iter().all(|m| !matches!(m, ListingMsg::Done { .. })));
    assert!(cache.cached().is_empty());
}

/// A 7z's signature header with its next header's offset, size and CRC replaced, and the
/// start header's CRC recomputed.
fn reheader(b: &mut [u8], offset: u64, size: u64, crc: u32) {
    b[12..20].copy_from_slice(&offset.to_le_bytes());
    b[20..28].copy_from_slice(&size.to_le_bytes());
    b[28..32].copy_from_slice(&crc.to_le_bytes());
    let mut c = flate2::Crc::new();
    c.update(&b[12..32]);
    let sum = c.sum();
    b[8..12].copy_from_slice(&sum.to_le_bytes());
}

/// A-AR-3 and A-4 with 7z: the header sits at the end, so a truncated archive or a header
/// CRC error lists nothing ("archive damaged"); a header above 64 MiB, or a compressed
/// header whose LZMA2 dictionary exceeds the cap, is refused before anything is allocated;
/// the entry bound refuses a header that declares more files; the name promises the format,
/// and `Alt+O` finds it by its magic.
#[test]
fn a_ar_3_7z_damage_and_bounds() {
    let t = test_dir("ar3-7z");
    let whole = std::fs::read(fixture("solid.7z")).unwrap();
    write(&t.join("cut.7z"), &whole[..whole.len() * 6 / 10]);
    assert_eq!(open(&t.join("cut.7z")).failed().as_deref(), Some(DAMAGED));
    let mut flipped = whole.clone();
    let last = flipped.len() - 3;
    flipped[last] ^= 0x55;
    write(&t.join("crc.7z"), &flipped);
    assert_eq!(open(&t.join("crc.7z")).failed().as_deref(), Some(DAMAGED));
    let mut big = whole.clone();
    reheader(&mut big, 0, 65 << 20, 0);
    write(&t.join("big-header.7z"), &big);
    assert_eq!(
        open(&t.join("big-header.7z")).failed().as_deref(),
        Some(NEEDS_MEMORY)
    );
    let o = open_with(
        &fixture("solid.7z"),
        &IndexCache::default(),
        Limits {
            entries: 5,
            ..Limits::default()
        },
    );
    assert_eq!(o.error().as_deref(), Some("listing stopped at 5 entries"));
    write(
        &t.join("zip.7z"),
        &std::fs::read(fixture("overlap.zip")).unwrap(),
    );
    assert_eq!(
        open(&t.join("zip.7z")).failed().as_deref(),
        Some("not a 7z archive")
    );
    write(&t.join("renamed.bin"), &whole);
    let o = open_req(
        &request(&t.join("renamed.bin"), Want::Magic, Limits::default()),
        &IndexCache::default(),
    );
    assert!(o.failed().is_none(), "{:?}", o.failed());
    assert_eq!(rows(o.index()).len(), 11);
    // A compressed header that declares an LZMA2 dictionary of 512 MiB: refused before
    // the decoder allocates it.
    if !has_tool("bsdtar") {
        return;
    }
    let src = t.join("src");
    std::fs::create_dir(&src).unwrap();
    write(&src.join("f"), b"file");
    let path = t.join("lzma2.7z");
    bsdtar_7z(&src, &path, Some("7zip:compression=lzma2"));
    let mut b = std::fs::read(&path).unwrap();
    let off = u64::from_le_bytes(b[12..20].try_into().unwrap()) as usize;
    let size = u64::from_le_bytes(b[20..28].try_into().unwrap()) as usize;
    let h = 32 + off;
    // The encoded header's folder: one coder, flags 0x21, id 0x21 (LZMA2), one property.
    let at = h
        + b[h..h + size]
            .windows(3)
            .position(|w| w == [0x21, 0x21, 0x01])
            .expect("an LZMA2 header coder")
        + 3;
    b[at] = 34;
    let mut c = flate2::Crc::new();
    c.update(&b[h..h + size]);
    reheader(&mut b, off as u64, size as u64, c.sum());
    write(&path, &b);
    assert_eq!(open(&path).failed().as_deref(), Some(NEEDS_MEMORY));
}

// ---- detection (P3 3.1) -----------------------------------------------------------------

#[test]
fn detection_names_the_promised_format() {
    let t = test_dir("ar-detect");
    write(&t.join("not.zip"), b"plain text, not a zip");
    write(&t.join("not.tar.zst"), b"plain text, not a zstd stream");
    write(&t.join("gz-not-tar.tar.gz"), &compress("gz", &[b'x'; 4096]));
    write(
        &t.join("odd.epub"),
        &std::fs::read(fixture("overlap.zip")).unwrap(),
    );
    write(&t.join("other.txt"), b"text");
    let cases = [
        ("not.zip", Some("not a zip archive")),
        ("not.tar.zst", Some("not a tar.zst archive")),
        ("gz-not-tar.tar.gz", Some("not a tar.gz archive")),
        ("odd.epub", None),
        ("other.txt", Some("not a supported archive")),
    ];
    for (name, want) in cases {
        let o = open(&t.join(name));
        assert_eq!(o.failed().as_deref(), want, "{name}");
    }
    // A FIFO named like an archive is never opened (I-10).
    let fifo = t.join("pipe.zip");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &fifo,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
    let start = Instant::now();
    let o = open(&fifo);
    assert_eq!(o.failed().as_deref(), Some("not a regular file"));
    assert!(start.elapsed() < Duration::from_secs(1));
}

// ---- the index cache and the bounds (A-RES-1, the index half) -----------------------------

fn small_tar(dir: &Path, name: &str, members: usize) -> PathBuf {
    let mut t = TarGen::new();
    for i in 0..members {
        t.file(format!("{name}-{i}").as_bytes(), 0o644, BASE, b"x");
    }
    let path = dir.join(format!("{name}.tar"));
    write(&path, &t.finish());
    path
}

#[test]
fn a_res_1_the_cache_keeps_four_indexes_and_never_evicts_one_on_screen() {
    let t = test_dir("ar-cache");
    let cache = IndexCache::default();
    let paths: Vec<PathBuf> = (0..6)
        .map(|i| small_tar(&t.path, &format!("a{i}"), 3))
        .collect();
    // The first index stays "on screen": this test holds it, as a panel would.
    let shown = open_with(&paths[0], &cache, Limits::default())
        .index
        .unwrap();
    for p in &paths[1..] {
        let o = open_with(p, &cache, Limits::default());
        drop(o);
    }
    let cached = cache.cached();
    assert_eq!(cached.len(), 4);
    assert!(
        cached.iter().any(|c| Arc::ptr_eq(c, &shown)),
        "the least recently used index is on screen and stays"
    );
    // a1 and a2 went: the least recently used of those no tab shows.
    let names: Vec<_> = cached.iter().map(|c| c.archive.clone()).collect();
    assert!(
        !names.contains(&paths[1]) && !names.contains(&paths[2]),
        "{names:?}"
    );
    assert_eq!(cache.scans(), 6);
    // Re-entering a cached archive does not scan it again.
    let again = open_with(&paths[5], &cache, Limits::default());
    assert!(Arc::ptr_eq(again.index(), cached.last().unwrap()));
    assert_eq!(cache.scans(), 6);
    drop(again);

    // The byte bound: a cache of room for about one index keeps the newest one only.
    let cache = IndexCache::new(4, 1);
    for p in &paths[..3] {
        drop(open_with(p, &cache, Limits::default()));
    }
    assert_eq!(cache.cached().len(), 1);
    assert_eq!(cache.cached()[0].archive, paths[2]);
    // An index on screen stays even beyond the bound.
    let held = open_with(&paths[3], &cache, Limits::default())
        .index
        .unwrap();
    drop(open_with(&paths[4], &cache, Limits::default()));
    assert!(cache.cached().iter().any(|c| Arc::ptr_eq(c, &held)));
}

#[test]
fn a_res_1_the_entry_cap_stops_a_listing_with_its_message() {
    let t = test_dir("ar-cap");
    let limits = Limits {
        entries: 50,
        bytes: ix::MAX_INDEX_BYTES,
    };
    let tar = small_tar(&t.path, "many", 80);
    let o = open_with(&tar, &IndexCache::default(), limits);
    assert_eq!(o.error().as_deref(), Some("listing stopped at 50 entries"));
    assert_eq!(
        o.index().tree().unwrap().len(),
        50,
        "the root and 49 members"
    );
    // A zip that declares more entries than the cap is not listed at all: the zip crate
    // would read its whole central directory into memory first.
    let zip = t.join("many.zip");
    let mut w = zip::ZipWriter::new(std::fs::File::create(&zip).unwrap());
    for i in 0..60 {
        w.start_file(format!("f{i}"), zip::write::SimpleFileOptions::default())
            .unwrap();
    }
    w.finish().unwrap();
    let o = open_with(&zip, &IndexCache::default(), limits);
    assert_eq!(o.error().as_deref(), Some("listing stopped at 50 entries"));
    assert!(o.index().tree().unwrap().is_empty());
    assert_eq!(
        Limits::default().entries_message(),
        "listing stopped at 1,000,000 entries"
    );
    assert_eq!(ix::MAX_ENTRIES, 1_000_000);
    // The memory bound of one index.
    let tight = Limits {
        entries: ix::MAX_ENTRIES,
        bytes: 16 << 10,
    };
    let o = open_with(
        &small_tar(&t.path, "wide", 2000),
        &IndexCache::default(),
        tight,
    );
    assert!(
        o.error()
            .is_some_and(|e| e.starts_with("listing stopped: the index reached")),
        "{:?}",
        o.error()
    );
}

// ---- navigation (A-AR-4) ----------------------------------------------------------------

fn app(left: &Path, right: &Path) -> App {
    App::new(
        left.to_path_buf(),
        right.to_path_buf(),
        right.to_path_buf(),
        Config::default(),
        None,
        Depth::NoColor,
        jiff::tz::TimeZone::UTC,
    )
}

/// Performs the effects synchronously, as the runtime does on its threads.
fn run(a: &mut App, cache: &IndexCache, fx: Vec<Effect>) {
    for e in fx {
        let msgs = RefCell::new(Vec::new());
        let send = |m| msgs.borrow_mut().push(m);
        match e {
            Effect::List(req, alive) => {
                listing::guarded(&req, &send, listing::list);
                alive.finish();
            }
            Effect::OpenArchive(req, alive) => {
                archive::open(&req, cache, &send);
                alive.finish();
            }
            Effect::Relist(req, alive) => {
                archive::relist(&req, &send);
                alive.finish();
            }
            Effect::ArchiveSize(req) => archive::size(&req, &send),
            _ => {}
        }
        for m in msgs.into_inner() {
            let more = a.update(Event::Listing(m));
            run(a, cache, more);
        }
    }
    a.panel_mut().ensure_sorted();
}

fn press(a: &mut App, code: KeyCode, m: KeyModifiers) -> Vec<Effect> {
    a.update(Event::Key(KeyEvent::new(code, m), Instant::now()))
}

fn names(a: &App) -> Vec<String> {
    let p = a.panel();
    p.list
        .visible
        .iter()
        .map(|&i| String::from_utf8_lossy(p.list.name(i)).into_owned())
        .collect()
}

fn location(a: &App) -> String {
    String::from_utf8_lossy(&a.panel().location()).into_owned()
}

fn cursor(a: &App) -> String {
    String::from_utf8_lossy(a.panel().current_name().unwrap_or(b"..")).into_owned()
}

fn pkg(dir: &Path, name: &str, extra: &str) -> PathBuf {
    let mut t = TarGen::new();
    t.dir(b"usr/", 0o755, BASE);
    t.file(b"usr/bin/tool", 0o755, BASE, b"#!/bin/sh\n");
    t.file(b"usr/lib/libx.so", 0o644, BASE, &[7u8; 3000]);
    t.raw(b"usr/lib/link", b'2', 0o777, BASE, b"../bin", b"");
    t.file(b"README", 0o644, BASE, b"readme");
    t.file(extra.as_bytes(), 0o644, BASE, b"extra");
    t.file(b"inner.zip", 0o644, BASE, b"not opened");
    let path = dir.join(name);
    write(&path, &compress("zst", &t.finish()));
    path
}

const NONE: KeyModifiers = KeyModifiers::NONE;
const ALT: KeyModifiers = KeyModifiers::ALT;
const CTRL: KeyModifiers = KeyModifiers::CONTROL;

#[test]
fn a_ar_4_navigation() {
    let t = test_dir("ar4-nav");
    let other = test_dir("ar4-other");
    let archive = pkg(&t.path, "pkg.tar.zst", "first");
    write(&t.join("not.zip"), b"no zip");
    write(&t.join("fake.tar.zst"), b"no zstd");
    write(
        &t.join("odd.epub"),
        &std::fs::read(fixture("overlap.zip")).unwrap(),
    );
    write(&t.join("other.txt"), b"text");
    let cache = IndexCache::default();
    let mut a = app(&t.path, &other.path);
    let fx = a.start();
    run(&mut a, &cache, fx);

    // Enter by name browses the archive; the title is `archive:/inner`.
    a.panel_mut().cursor_to_name(b"pkg.tar.zst");
    let fx = press(&mut a, KeyCode::Enter, NONE);
    assert!(matches!(fx[..], [Effect::OpenArchive(..)]), "{fx:?}");
    run(&mut a, &cache, fx);
    assert!(matches!(a.panel().source, Source::Archive(_)));
    assert_eq!(location(&a), format!("{}:/", archive.display()));
    assert_eq!(names(&a), ["usr", "first", "inner.zip", "README"]);
    assert_eq!(
        a.panel().dir,
        t.path,
        "the local directory holds the archive"
    );
    assert_eq!(cache.scans(), 1);
    // The rendered title and footer.
    let screen = draw(&mut a);
    assert!(screen.contains("pkg.tar.zst:/"), "{screen}");
    assert!(screen.contains("4 entries"), "{screen}");

    // Into a directory and back out; Space sizes a directory from the index.
    a.panel_mut().cursor_to_name(b"usr");
    let fx = press(&mut a, KeyCode::Char(' '), NONE);
    run(&mut a, &cache, fx);
    let (_, e) = a.panel().current_entry().unwrap();
    assert!(e.marked() && e.size == 3010, "{e:?}");
    let fx = press(&mut a, KeyCode::Enter, NONE);
    run(&mut a, &cache, fx);
    assert_eq!(location(&a), format!("{}:/usr", archive.display()));
    assert_eq!(names(&a), ["bin", "lib"]);
    // A symlink to a directory inside the index enters its target.
    a.panel_mut().cursor_to_name(b"lib");
    let fx = press(&mut a, KeyCode::Enter, NONE);
    run(&mut a, &cache, fx);
    a.panel_mut().cursor_to_name(b"link");
    let (_, e) = a.panel().current_entry().unwrap();
    assert_eq!(e.link, manycommander::panel::entry::LinkKind::Dir);
    let fx = press(&mut a, KeyCode::Enter, NONE);
    run(&mut a, &cache, fx);
    assert_eq!(location(&a), format!("{}:/usr/bin", archive.display()));
    // Alt+Left and Alt+Right move through the archive's places (through the cache).
    let fx = press(&mut a, KeyCode::Left, ALT);
    run(&mut a, &cache, fx);
    assert_eq!(location(&a), format!("{}:/usr/lib", archive.display()));
    let fx = press(&mut a, KeyCode::Right, ALT);
    run(&mut a, &cache, fx);
    assert_eq!(location(&a), format!("{}:/usr/bin", archive.display()));
    assert_eq!(cache.scans(), 1);
    // `..` goes up, and at the root lands on the archive in its directory.
    let fx = press(&mut a, KeyCode::Backspace, NONE);
    run(&mut a, &cache, fx);
    assert_eq!(location(&a), format!("{}:/usr", archive.display()));
    assert_eq!(cursor(&a), "bin");
    let fx = press(&mut a, KeyCode::Backspace, NONE);
    run(&mut a, &cache, fx);
    assert_eq!(cursor(&a), "usr");
    a.panel_mut().cursor_to(0);
    assert_eq!(a.panel().current(), Some(Row::Parent));
    let fx = press(&mut a, KeyCode::Enter, NONE);
    run(&mut a, &cache, fx);
    assert!(a.panel().is_directory());
    assert_eq!(a.panel().dir, t.path);
    assert_eq!(cursor(&a), "pkg.tar.zst");
    // Re-entering hits the cache.
    let fx = press(&mut a, KeyCode::Enter, NONE);
    run(&mut a, &cache, fx);
    assert_eq!(names(&a), ["usr", "first", "inner.zip", "README"]);
    assert_eq!(cache.scans(), 1);
    // A member with an archive name is not opened; viewing members comes with T3.
    a.panel_mut().cursor_to_name(b"inner.zip");
    assert!(press(&mut a, KeyCode::Enter, NONE).is_empty());
    assert_eq!(
        a.status.as_ref().map(|s| s.text.as_str()),
        Some(archive::NESTED)
    );
    assert!(press(&mut a, KeyCode::Char('o'), ALT).is_empty());
    assert_eq!(
        a.status.as_ref().map(|s| s.text.as_str()),
        Some(archive::NESTED)
    );
    // `cd` inside the archive: relative paths move in it, `..` above the root leaves it.
    for c in "cd usr/lib".chars() {
        press(&mut a, KeyCode::Char(c), NONE);
    }
    let fx = press(&mut a, KeyCode::Enter, NONE);
    run(&mut a, &cache, fx);
    assert_eq!(location(&a), format!("{}:/usr/lib", archive.display()));
    for c in "cd ../../..".chars() {
        press(&mut a, KeyCode::Char(c), NONE);
    }
    let fx = press(&mut a, KeyCode::Enter, NONE);
    run(&mut a, &cache, fx);
    assert!(a.panel().is_directory());
    assert_eq!(a.panel().dir, t.path);

    // A file that is not what its name promises fails, and the panel stays.
    for (name, why) in [
        ("not.zip", "not a zip archive"),
        ("fake.tar.zst", "not a tar.zst archive"),
    ] {
        a.panel_mut().cursor_to_name(name.as_bytes());
        let fx = press(&mut a, KeyCode::Enter, NONE);
        run(&mut a, &cache, fx);
        assert!(a.panel().is_directory(), "{name}");
        assert_eq!(a.panel().dir, t.path);
        assert_eq!(cursor(&a), name);
        let msg = a.panel().message.clone().unwrap_or_default();
        assert!(msg.ends_with(why) && msg.contains(name), "{msg}");
    }
    // Alt+O opens a file of any name by its magic; on a non-archive it says so.
    a.panel_mut().cursor_to_name(b"odd.epub");
    let fx = press(&mut a, KeyCode::Char('o'), ALT);
    run(&mut a, &cache, fx);
    assert_eq!(location(&a), format!("{}:/", t.join("odd.epub").display()));
    assert_eq!(names(&a), ["a", "b"]);
    let fx = press(&mut a, KeyCode::Backspace, NONE);
    run(&mut a, &cache, fx);
    a.panel_mut().cursor_to_name(b"other.txt");
    let fx = press(&mut a, KeyCode::Char('o'), ALT);
    run(&mut a, &cache, fx);
    assert!(a.panel().is_directory());
    assert!(
        a.panel()
            .message
            .as_ref()
            .is_some_and(|m| m.ends_with("not a supported archive"))
    );
    // Alt+O with text on the command line is ignored (P3 6).
    press(&mut a, KeyCode::Char('x'), NONE);
    assert!(press(&mut a, KeyCode::Char('o'), ALT).is_empty());
    assert_eq!(a.line.bytes(), b"x");
    press(&mut a, KeyCode::Esc, NONE);

    // An archive replaced by rename: the view stays on the indexed inode and says the
    // archive changed; Ctrl+R reads it again.
    a.panel_mut().cursor_to_name(b"pkg.tar.zst");
    let fx = press(&mut a, KeyCode::Enter, NONE);
    run(&mut a, &cache, fx);
    let before = a.panel().archive().unwrap().index.clone();
    let next = pkg(&t.path, "pkg.tar.zst.new", "second");
    std::fs::rename(&next, &archive).unwrap();
    let fx = a.update(Event::ChildDone {
        status: "done".into(),
        output: None,
    });
    run(&mut a, &cache, fx);
    assert_eq!(names(&a), ["usr", "first", "inner.zip", "README"]);
    assert!(Arc::ptr_eq(&a.panel().archive().unwrap().index, &before));
    assert_eq!(a.panel().message.as_deref(), Some(archive::CHANGED));
    let scans = cache.scans();
    let fx = press(&mut a, KeyCode::Char('r'), CTRL);
    run(&mut a, &cache, fx);
    assert_eq!(cache.scans(), scans + 1, "Ctrl+R rescans a changed archive");
    assert_eq!(names(&a), ["usr", "inner.zip", "README", "second"]);
    // An unchanged archive: Ctrl+R re-lists from the index and keeps the marks.
    a.panel_mut().cursor_to_name(b"README");
    a.panel_mut().toggle_mark(false);
    let fx = press(&mut a, KeyCode::Char('r'), CTRL);
    run(&mut a, &cache, fx);
    assert_eq!(cache.scans(), scans + 1);
    assert_eq!(a.panel().marked, 1);
}

fn draw(a: &mut App) -> String {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
    term.draw(|f| manycommander::ui::draw(a, f)).unwrap();
    let buf = term.backend().buffer().clone();
    let mut s = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            s.push_str(buf[(x, y)].symbol());
        }
        s.push('\n');
    }
    s
}

/// P-20 in the app: `Esc` during a scan returns the panel at once, stops the scan, and the
/// scan thread ends soon after. Abandoned scans count toward `MAX_ABANDONED` (A-RES-1).
#[test]
fn a_ar_4_esc_during_a_scan_and_blocked_scans() {
    let t = test_dir("ar4-esc");
    let other = test_dir("ar4-esc-other");
    std::fs::copy(fixture("bomb.tar.zst"), t.join("bomb.tar.zst")).unwrap();
    let cache = Arc::new(IndexCache::default());
    let mut a = app(&t.path, &other.path);
    let fx = a.start();
    run(&mut a, &cache, fx);
    a.panel_mut().cursor_to_name(b"bomb.tar.zst");
    let fx = press(&mut a, KeyCode::Enter, NONE);
    let [Effect::OpenArchive(req, alive)] = &fx[..] else {
        panic!("{fx:?}");
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let (req, alive2, c) = (req.clone(), alive.clone(), cache.clone());
    std::thread::spawn(move || {
        archive::open(&req, &c, &|m| {
            let _ = tx.send(m);
        });
        alive2.finish();
    });
    // The index arrives; the title shows the archive while it is read.
    let m = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    a.update(Event::Listing(m));
    assert!(a.panel().archive().is_some());
    assert!(a.panel().is_loading());
    let screen = draw(&mut a);
    assert!(screen.contains("reading archive:"), "{screen}");
    let at = Instant::now();
    press(&mut a, KeyCode::Esc, NONE);
    assert!(a.panel().is_directory() && !a.panel().is_loading());
    assert_eq!(cursor(&a), "bomb.tar.zst");
    assert!(at.elapsed() < Duration::from_millis(100), "P-20");
    while alive.is_running() {
        assert!(
            at.elapsed() < Duration::from_secs(2),
            "the scan did not stop"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    for m in rx.try_iter() {
        a.update(Event::Listing(m));
    }
    assert!(a.panel().is_directory(), "late messages are dropped");

    // Scans that never return count as abandoned; at four, a new scan is refused.
    for i in 0..4 {
        let name = format!("stuck{i}.tar");
        small_tar(&t.path, &format!("stuck{i}"), 1);
        let fx = press(&mut a, KeyCode::Char('r'), CTRL);
        run(&mut a, &cache, fx);
        a.panel_mut().cursor_to_name(name.as_bytes());
        let fx = press(&mut a, KeyCode::Enter, NONE);
        assert!(matches!(fx[..], [Effect::OpenArchive(..)]), "{fx:?}");
        // Never run: its thread stays "blocked". Esc abandons it.
        press(&mut a, KeyCode::Esc, NONE);
    }
    a.panel_mut().cursor_to_name(b"bomb.tar.zst");
    assert!(press(&mut a, KeyCode::Enter, NONE).is_empty());
    assert_eq!(
        a.status.as_ref().map(|s| s.text.as_str()),
        Some(manycommander::app::TOO_MANY_BLOCKED)
    );
}

// ---- timings (P-18, P-19; recorded, T10 benches them) -----------------------------------

#[test]
fn p_18_p_19_timings_are_reported() {
    let t = test_dir("ar-timing");
    let zip = t.join("10k.zip");
    write(&zip, &a_ar_1_zip());
    let start = Instant::now();
    let o = open(&zip);
    let zip_ms = start.elapsed().as_secs_f64() * 1000.0;
    assert!(o.index().is_complete());
    let zst = t.join("10k.tar.zst");
    write(&zst, &compress("zst", &a_ar_1_tar()));
    let first = RefCell::new(None);
    let start = Instant::now();
    let req = request(&zst, Want::Magic, Limits::default());
    archive::open(&req, &IndexCache::default(), &|m| {
        if matches!(m, ListingMsg::Batch { .. }) && first.borrow().is_none() {
            *first.borrow_mut() = Some(start.elapsed());
        }
    });
    let scan_ms = start.elapsed().as_secs_f64() * 1000.0;
    let first_ms = first.into_inner().unwrap().as_secs_f64() * 1000.0;
    // The same process's decompress-only run: the scan's decoder alone (P-19).
    let s = Instant::now();
    let mut d = zstd::stream::read::Decoder::new(std::fs::File::open(&zst).unwrap()).unwrap();
    std::io::copy(&mut d, &mut std::io::sink()).unwrap();
    let decode_ms = s.elapsed().as_secs_f64() * 1000.0;
    let mut zstd_ms = None;
    if Command::new("zstd").arg("--version").output().is_ok() {
        let s = Instant::now();
        let ok = Command::new("zstd")
            .args(["-dc", "-q"])
            .arg(&zst)
            .stdout(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            zstd_ms = Some(s.elapsed().as_secs_f64() * 1000.0);
        }
    }
    eprintln!(
        "TIMING zip 10k listed in {zip_ms:.1} ms; tar.zst 10k: first rows {first_ms:.1} ms, full scan {scan_ms:.1} ms, decompress only {decode_ms:.1} ms, zstd -dc {zstd_ms:?} ms ({} build)",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
}

#[test]
fn listings_arrive_in_batches_with_the_first_small() {
    let t = test_dir("ar-batches");
    let mut g = TarGen::new();
    for i in 0..3000 {
        g.file(format!("f{i:04}").as_bytes(), 0o644, BASE, b"");
    }
    let path = t.join("wide.tar.gz");
    write(&path, &compress("gz", &g.finish()));
    let o = open(&path);
    let sizes: Vec<usize> = o
        .msgs
        .iter()
        .filter_map(|m| match m {
            ListingMsg::Batch { entries, .. } => Some(entries.len()),
            _ => None,
        })
        .collect();
    assert_eq!(sizes.iter().sum::<usize>(), 3000);
    assert!(sizes[0] <= listing::FIRST_BATCH, "{sizes:?}");
    assert!(sizes.iter().all(|&s| s <= listing::BATCH));
    assert_eq!(o.batch_names().len(), 3000);
    // Done follows the rows.
    let done = o
        .msgs
        .iter()
        .position(|m| matches!(m, ListingMsg::Done { .. }))
        .unwrap();
    assert!(
        o.msgs[done + 1..]
            .iter()
            .all(|m| !matches!(m, ListingMsg::Batch { .. }))
    );
}

/// Another path to the same archive inode (a hard link) hits the cached index; the panel
/// keeps its own directory, and `..` returns there.
#[test]
fn a_cache_hit_through_another_path_keeps_the_panel_directory() {
    let t = test_dir("ar-other-path");
    let other = test_dir("ar-other-path-right");
    let first = pkg(&t.path, "one.tar.zst", "x");
    std::fs::create_dir(t.join("sub")).unwrap();
    std::fs::hard_link(&first, t.join("sub/two.tar.zst")).unwrap();
    let cache = IndexCache::default();
    let mut a = app(&t.path, &other.path);
    let fx = a.start();
    run(&mut a, &cache, fx);
    a.panel_mut().cursor_to_name(b"one.tar.zst");
    let fx = press(&mut a, KeyCode::Enter, NONE);
    run(&mut a, &cache, fx);
    assert_eq!(cache.scans(), 1);
    for c in format!("cd {}", t.join("sub").display()).chars() {
        press(&mut a, KeyCode::Char(c), NONE);
    }
    let fx = press(&mut a, KeyCode::Enter, NONE);
    run(&mut a, &cache, fx);
    assert_eq!(a.panel().dir, t.join("sub"));
    a.panel_mut().cursor_to_name(b"two.tar.zst");
    let fx = press(&mut a, KeyCode::Enter, NONE);
    run(&mut a, &cache, fx);
    assert_eq!(cache.scans(), 1, "the same inode: a cache hit");
    assert_eq!(a.panel().dir, t.join("sub"));
    assert_eq!(
        location(&a),
        format!("{}:/", t.join("sub/two.tar.zst").display())
    );
    let fx = press(&mut a, KeyCode::Backspace, NONE);
    run(&mut a, &cache, fx);
    assert_eq!(a.panel().dir, t.join("sub"));
    assert_eq!(cursor(&a), "two.tar.zst");
}

/// The runtime end to end on a pty: `Enter` opens an archive on a listing thread, the
/// title shows `archive:/inner` once the load completes, and `..` leaves it.
#[test]
fn the_binary_browses_an_archive_on_a_pty() {
    use common::tui::*;
    const T: Duration = Duration::from_secs(10);
    let h = test_dir("ar-pty");
    let d = h.join("box");
    std::fs::create_dir(&d).unwrap();
    let mut g = TarGen::new();
    g.file(b"inside/deep.txt", 0o644, BASE, b"deep");
    g.file(b"top.txt", 0o644, BASE, b"top");
    write(&d.join("a.tar.gz"), &compress("gz", &g.finish()));
    let dir = d.to_str().unwrap();
    let mut t = Tui::spawn(&[dir, dir], &h.path, &[], 110, 30);
    assert!(t.wait_for("10Quit", T), "{}", t.screen());
    let loaded = |t: &mut Tui, what: &str| {
        let s = t.screen();
        s.contains(what) && !s.contains("(loading)")
    };
    assert!(t.wait_until(T, |t| loaded(t, "a.tar.gz")), "{}", t.screen());
    t.keys(&[DOWN, ENTER]);
    assert!(
        t.wait_until(T, |t| loaded(t, "a.tar.gz:/ ")
            && t.screen().contains("inside")),
        "{}",
        t.screen()
    );
    t.keys(&[DOWN, ENTER]);
    assert!(
        t.wait_until(T, |t| loaded(t, "a.tar.gz:/inside")
            && t.screen().contains("deep.txt")),
        "{}",
        t.screen()
    );
    t.keys(&[b"\x7f", b"\x7f"]);
    assert!(
        t.wait_until(T, |t| loaded(t, "box ")
            && !t.screen().contains("a.tar.gz:/")),
        "{}",
        t.screen()
    );
    t.send(F10);
    assert_eq!(t.wait_exit(T), Some(0));
    assert!(t.restored());
}
