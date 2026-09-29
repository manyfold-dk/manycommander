//! The SFTP codec and session (P3 5.3, 2.5): A-SF-1 against a scripted server on the codec,
//! and the protocol half of A-SF-2 against OpenSSH's `sftp-server` on plain pipes (no
//! sshd, no network). Also the pipelined windows, cancel with the drain window, the stuck-
//! session rule and session loss.

mod common;

use common::sftp::{
    Server, have_sftp_server, no_core_dumps, scripted, sftp_server, sftp_server_with_latency,
};
use common::{noise, skip, test_dir};
use manycommander::remote::proto::{
    self, Attrs, DecodeError, Limits, Name, Packet, StatVfs, ext, fxp, open, status,
};
use manycommander::remote::session::{DRAIN, Sizes, sftp_error};
use manycommander::remote::{Session, SftpError};
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(10);

fn never() -> AtomicBool {
    AtomicBool::new(false)
}

fn bytes(p: &std::path::Path) -> Vec<u8> {
    p.as_os_str().as_bytes().to_vec()
}

/// A session is closed and its child reaped.
fn close(s: Session) {
    let pid = s.pid();
    s.close_wait();
    if let Some(pid) = pid {
        assert!(
            common::sftp::gone_within(pid, T),
            "the child {pid} was not reaped"
        );
    }
}

// ---- A-SF-1: the codec ------------------------------------------------------------------

fn one_of_each() -> Vec<Packet> {
    let attrs = Attrs {
        size: Some(1 << 40),
        uid_gid: Some((1000, 1001)),
        perms: Some(0o100_644),
        times: Some((1_700_000_000, 1_700_000_001)),
        extended: vec![(b"k@x".to_vec(), b"v".to_vec())],
    };
    let h = b"\x00\x01h".to_vec();
    let p = b"/a/b\n\xff".to_vec();
    vec![
        Packet::Init {
            version: 3,
            extensions: vec![],
        },
        Packet::Version {
            version: 3,
            extensions: vec![(ext::LIMITS.0.to_vec(), ext::LIMITS.1.to_vec())],
        },
        Packet::Open {
            id: 1,
            path: p.clone(),
            pflags: open::READ | open::WRITE | open::CREAT | open::EXCL,
            attrs: attrs.clone(),
        },
        Packet::Close {
            id: 2,
            handle: h.clone(),
        },
        Packet::Read {
            id: 3,
            handle: h.clone(),
            offset: 1 << 33,
            len: 32768,
        },
        Packet::Write {
            id: 4,
            handle: h.clone(),
            offset: 7,
            data: b"data".to_vec().into(),
        },
        Packet::Lstat {
            id: 5,
            path: p.clone(),
        },
        Packet::Fstat {
            id: 6,
            handle: h.clone(),
        },
        Packet::Setstat {
            id: 7,
            path: p.clone(),
            attrs: Attrs {
                perms: Some(0o600),
                ..Attrs::default()
            },
        },
        Packet::Fsetstat {
            id: 8,
            handle: h.clone(),
            attrs: Attrs {
                times: Some((1, 2)),
                ..Attrs::default()
            },
        },
        Packet::Opendir {
            id: 9,
            path: p.clone(),
        },
        Packet::Readdir {
            id: 10,
            handle: h.clone(),
        },
        Packet::Remove {
            id: 11,
            path: p.clone(),
        },
        Packet::Mkdir {
            id: 12,
            path: p.clone(),
            attrs: Attrs {
                perms: Some(0o700),
                ..Attrs::default()
            },
        },
        Packet::Rmdir {
            id: 13,
            path: p.clone(),
        },
        Packet::Realpath {
            id: 14,
            path: b".".to_vec(),
        },
        Packet::Stat {
            id: 15,
            path: p.clone(),
        },
        Packet::Rename {
            id: 16,
            from: b"a".to_vec(),
            to: b"b".to_vec(),
        },
        Packet::Readlink {
            id: 17,
            path: p.clone(),
        },
        Packet::Symlink {
            id: 18,
            link: b"link".to_vec(),
            target: b"target".to_vec(),
        },
        Packet::Status {
            id: 19,
            code: status::NO_SUCH_FILE,
            message: b"No such file".to_vec(),
            lang: b"en".to_vec(),
        },
        Packet::Handle {
            id: 20,
            handle: h.clone(),
        },
        Packet::Data {
            id: 21,
            data: b"\x00bytes\xff".to_vec().into(),
        },
        Packet::Name {
            id: 22,
            names: vec![
                Name {
                    filename: b"x\ny".to_vec(),
                    longname: b"-rw-r--r-- x".to_vec(),
                    attrs,
                },
                Name {
                    filename: b"z".to_vec(),
                    ..Name::default()
                },
            ],
        },
        Packet::Attrs {
            id: 23,
            attrs: Attrs::default(),
        },
        Packet::Extended {
            id: 24,
            name: ext::HOME_DIRECTORY.0.to_vec(),
            data: proto::ext_args(&[b""]),
        },
        Packet::ExtendedReply {
            id: 25,
            data: Limits {
                packet: 262144,
                read: 261120,
                write: 261120,
                handles: 0,
            }
            .encode(),
        },
    ]
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn a_sf_1_every_packet_type_round_trips() {
    let all = one_of_each();
    // Every type of the protocol is covered.
    let mut kinds: Vec<u8> = all.iter().map(Packet::kind).collect();
    kinds.sort_unstable();
    kinds.dedup();
    assert_eq!(kinds.len(), 27);
    let mut text = String::new();
    for p in &all {
        let frame = p.encode();
        assert_eq!(
            u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize,
            frame.len() - 4
        );
        let back = Packet::decode(frame[4..].to_vec()).unwrap();
        assert_eq!(&back, p);
        text.push_str(&format!("{:>3} {}\n", p.kind(), hex(&frame)));
    }
    insta::assert_snapshot!(text);
}

#[test]
fn a_sf_1_init_and_version_carry_no_request_id() {
    let (s, _lost, h) = scripted(|mut srv| {
        let init = srv.hello(&[]);
        // length 5: the type and the version, no id.
        assert_eq!(init, [0, 0, 0, 5, fxp::INIT, 0, 0, 0, 3]);
        while srv.request().is_some() {}
    });
    assert_eq!(s.version(), 3);
    for p in one_of_each() {
        assert_eq!(
            p.id().is_none(),
            matches!(p, Packet::Init { .. } | Packet::Version { .. })
        );
    }
    close(s);
    h.join().unwrap();
}

/// The server answers the first request with `reply`; the call fails with "connection
/// lost", the session is lost with `why` in its reason, and the reader never held a frame
/// larger than `bound`.
fn session_ends_on(reply: Vec<u8>, why: &str, bound: usize) {
    let (s, lost, h) = scripted(move |mut srv| {
        srv.hello(&[]);
        let Some(Packet::Realpath { .. }) = srv.request() else {
            panic!("expected REALPATH")
        };
        srv.raw(&reply);
        // A truncated frame needs the pipe closed to end.
        srv.hang_up();
        while srv.request().is_some() {}
    });
    let r = s.realpath(b".", &never());
    assert_eq!(r, Err(SftpError::Lost));
    let l = lost.recv_timeout(T).expect("loss reported");
    assert!(l.reason.contains(why), "{l:?}");
    assert!(s.lost().is_some());
    let st = s.stats();
    assert!(
        st.largest_frame <= bound,
        "retained {} > {bound}",
        st.largest_frame
    );
    // Nothing more goes out on a lost session.
    assert_eq!(s.lstat(b"/", &never()), Err(SftpError::Lost));
    close(s);
    h.join().unwrap();
}

/// A frame of `body`.
fn frame(body: &[u8]) -> Vec<u8> {
    let mut f = (body.len() as u32).to_be_bytes().to_vec();
    f.extend_from_slice(body);
    f
}

/// The bound for a session whose only frames were `VERSION` (no extensions) and `body`.
fn bound(body: &[u8]) -> usize {
    body.len().max(5)
}

#[test]
fn a_sf_1_a_length_above_the_maximum_ends_the_session() {
    // 1 MiB declared, a few bytes sent: nothing is allocated for it.
    let mut f = (1u32 << 20).to_be_bytes().to_vec();
    f.extend_from_slice(&[fxp::DATA, 0, 0, 0, 1]);
    session_ends_on(f, "too long", 5);
    let f = ((proto::MAX_PACKET + 1) as u32).to_be_bytes().to_vec();
    session_ends_on(f, "too long", 5);
}

#[test]
fn a_sf_1_a_truncated_string_ends_the_session() {
    let mut b = vec![fxp::NAME, 0, 0, 0, 1, 0, 0, 0, 1];
    b.extend_from_slice(&1000u32.to_be_bytes());
    // Enough bytes for the count of one name, far too few for its string.
    b.extend_from_slice(b"a string much shorter than 1000 bytes");
    session_ends_on(frame(&b), "truncated", bound(&b));
}

#[test]
fn a_sf_1_a_count_larger_than_the_packet_ends_the_session() {
    let mut b = vec![fxp::NAME, 0, 0, 0, 1];
    b.extend_from_slice(&u32::MAX.to_be_bytes());
    b.extend_from_slice(&[0; 24]);
    session_ends_on(frame(&b), "count", bound(&b));
    // Attribute extension pairs are counted the same way.
    let mut b = vec![fxp::ATTRS, 0, 0, 0, 1];
    b.extend_from_slice(&0x8000_0000u32.to_be_bytes());
    b.extend_from_slice(&0x4000_0000u32.to_be_bytes());
    session_ends_on(frame(&b), "count", bound(&b));
}

#[test]
fn a_sf_1_a_reply_to_an_unknown_id_ends_the_session() {
    let p = Packet::Status {
        id: 0xdead,
        code: status::OK,
        message: vec![],
        lang: vec![],
    };
    let f = p.encode();
    session_ends_on(f.clone(), "unknown request", f.len() - 4);
}

#[test]
fn a_sf_1_a_truncated_frame_ends_the_session() {
    let mut f = 100u32.to_be_bytes().to_vec();
    f.extend_from_slice(&[fxp::DATA, 0, 0, 0, 1, 0, 0]);
    session_ends_on(f, "inside a packet", 100);
}

#[test]
fn a_sf_1_a_request_or_unknown_type_as_a_reply_ends_the_session() {
    let f = Packet::Open {
        id: 1,
        path: b"x".to_vec(),
        pflags: 0,
        attrs: Attrs::default(),
    }
    .encode();
    session_ends_on(f.clone(), "not a reply", f.len() - 4);
    let f = frame(&[150, 0, 0, 0, 1]);
    session_ends_on(f, "unknown packet type", 5);
    let f = Packet::Version {
        version: 3,
        extensions: vec![],
    }
    .encode();
    session_ends_on(f, "not a reply", 5);
}

#[test]
fn a_sf_1_decode_errors_name_the_bound() {
    assert_eq!(
        Packet::decode(vec![fxp::HANDLE, 0, 0, 0, 1, 0, 0, 1, 0]),
        Err(DecodeError::Truncated)
    );
}

// ---- A-SF-2, the protocol half: sftp-server on pipes ---------------------------------------

#[test]
fn a_sf_2_version_and_extensions() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("sftp-version");
    let (s, _lost) = sftp_server(&d.path);
    assert_eq!(s.version(), 3);
    for e in [
        ext::POSIX_RENAME,
        ext::HARDLINK,
        ext::FSYNC,
        ext::STATVFS,
        ext::LIMITS,
        ext::HOME_DIRECTORY,
    ] {
        assert!(
            s.has(e),
            "{} {} not announced: {:?}",
            String::from_utf8_lossy(e.0),
            String::from_utf8_lossy(e.1),
            s.extensions()
        );
    }
    let c = s.caps();
    assert!(c.hard_link && c.posix_rename && c.fsync && c.statvfs && !c.random_access);
    // limits@openssh.com raises the request sizes, at most 256 KiB.
    let z = s.sizes(&never());
    assert!(
        z.read > 32 * 1024 && z.read as usize <= proto::MAX_DATA,
        "{z:?}"
    );
    assert!(
        z.write > 32 * 1024 && z.write as usize <= proto::MAX_DATA,
        "{z:?}"
    );
    let v: StatVfs = s.statvfs(&bytes(&d.path), &never()).unwrap();
    assert!(v.frsize > 0 && v.blocks > 0, "{v:?}");
    close(s);
}

/// The login directory from the passwords database, as `sftp-server` reads it (not `$HOME`).
fn passwd_home() -> Option<Vec<u8>> {
    let uid = rustix::process::getuid().as_raw().to_string();
    let out = std::process::Command::new("getent")
        .args(["passwd", &uid])
        .output()
        .ok()?;
    let line = out.stdout.split(|&b| b == b'\n').next()?.to_vec();
    Some(line.split(|&b| b == b':').nth(5)?.to_vec())
}

#[test]
fn a_sf_2_home_directory_and_the_realpath_fallback() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("sftp-home");
    let (s, _lost) = sftp_server(&d.path);
    match passwd_home() {
        Some(h) => assert_eq!(s.home(&never()).unwrap(), h),
        None => skip("getent passwd is not available"),
    }
    let canon = std::fs::canonicalize(&d.path).unwrap();
    assert_eq!(s.realpath(b".", &never()).unwrap(), bytes(&canon));
    close(s);

    // The extension takes exactly one string (empty: the login user) and is answered with
    // SSH_FXP_NAME.
    let (s, _lost, h) = scripted(|mut srv| {
        srv.hello(&[ext::HOME_DIRECTORY]);
        let Some(Packet::Extended { id, name, data }) = srv.request() else {
            panic!("expected the extension")
        };
        assert_eq!(name, b"home-directory");
        assert_eq!(data, [0, 0, 0, 0]);
        srv.reply(&Packet::Name {
            id,
            names: vec![Name {
                filename: b"/home/x".to_vec(),
                ..Name::default()
            }],
        });
        while srv.request().is_some() {}
    });
    assert_eq!(s.home(&never()).unwrap(), b"/home/x");
    close(s);
    h.join().unwrap();

    // Without it: REALPATH(".").
    let (s, _lost, h) = scripted(|mut srv| {
        srv.hello(&[]);
        let Some(Packet::Realpath { id, path }) = srv.request() else {
            panic!("expected REALPATH")
        };
        assert_eq!(path, b".");
        srv.reply(&Packet::Name {
            id,
            names: vec![Name {
                filename: b"/srv/start".to_vec(),
                ..Name::default()
            }],
        });
        while srv.request().is_some() {}
    });
    assert_eq!(s.home(&never()).unwrap(), b"/srv/start");
    close(s);
    h.join().unwrap();
}

#[test]
fn a_sf_2_readdir_batches_of_100_with_dot_and_dotdot() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("sftp-readdir");
    let big = d.join("big");
    std::fs::create_dir(&big).unwrap();
    for i in 0..10_000 {
        std::fs::File::create(big.join(format!("f{i:05}"))).unwrap();
    }
    let (s, _lost) = sftp_server(&d.path);
    let before = s.stats();
    let h = s.opendir(&bytes(&big), &never()).unwrap();
    let mut replies = 0;
    let mut names = Vec::new();
    while let Some(batch) = s.readdir(&h, &never()).unwrap() {
        replies += 1;
        assert!(batch.len() <= 100, "{}", batch.len());
        names.extend(batch.into_iter().map(|n| n.filename));
    }
    s.close_handle(&h, &never()).unwrap();
    // 10,002 names with . and .., in 101 NAME replies, then one EOF status.
    assert_eq!(names.len(), 10_002);
    assert_eq!(replies, 101);
    assert!(names.iter().any(|n| n == b"."));
    assert!(names.iter().any(|n| n == b".."));
    let after = s.stats();
    // OPENDIR, 102 READDIRs, CLOSE: no request per entry.
    assert_eq!(after.requests - before.requests, 104);
    assert_eq!(after.replies - before.replies, 104);
    close(s);
}

#[test]
fn a_sf_2_names_are_byte_exact() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("sftp-names");
    let dir = d.join("n");
    std::fs::create_dir(&dir).unwrap();
    let long = vec![b'l'; 255];
    let want: Vec<Vec<u8>> = vec![
        b"new\nline".to_vec(),
        b"bad\xff\xfeutf8".to_vec(),
        long,
        b" lead and trail ".to_vec(),
        "\u{e6}\u{f8}\u{e5}".as_bytes().to_vec(),
    ];
    for n in &want {
        std::fs::write(dir.join(OsStr::from_bytes(n)), n).unwrap();
    }
    let (s, _lost) = sftp_server(&d.path);
    let h = s.opendir(&bytes(&dir), &never()).unwrap();
    let mut got = Vec::new();
    while let Some(batch) = s.readdir(&h, &never()).unwrap() {
        for n in batch {
            if n.filename != b"." && n.filename != b".." {
                assert_eq!(n.attrs.size, Some(n.filename.len() as u64));
                assert_eq!(n.attrs.kind(), Some(0o100_000));
                got.push(n.filename);
            }
        }
    }
    s.close_handle(&h, &never()).unwrap();
    got.sort();
    let mut w = want.clone();
    w.sort();
    assert_eq!(got, w);
    // Each name also round-trips through LSTAT and a download.
    for n in &want {
        let mut p = bytes(&dir);
        p.push(b'/');
        p.extend_from_slice(n);
        assert_eq!(s.lstat(&p, &never()).unwrap().size, Some(n.len() as u64));
        let fh = s.open(&p, open::READ, Attrs::default(), &never()).unwrap();
        let mut r = s.reader(fh, None, Arc::new(never()));
        let mut data = Vec::new();
        r.read_to_end(&mut data).unwrap();
        assert_eq!(&data, n);
    }
    close(s);
}

#[test]
fn a_sf_2_lstat_stat_and_readlink_of_a_symlink() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("sftp-link");
    std::fs::write(d.join("target.txt"), b"12345").unwrap();
    std::os::unix::fs::symlink("target.txt", d.join("link")).unwrap();
    std::os::unix::fs::symlink("missing", d.join("dangling")).unwrap();
    let (s, _lost) = sftp_server(&d.path);
    let link = bytes(&d.join("link"));
    assert_eq!(s.lstat(&link, &never()).unwrap().kind(), Some(0o120_000));
    let st = s.stat(&link, &never()).unwrap();
    assert_eq!(st.kind(), Some(0o100_000));
    assert_eq!(st.size, Some(5));
    assert_eq!(s.readlink(&link, &never()).unwrap(), b"target.txt");
    match s.stat(&bytes(&d.join("dangling")), &never()) {
        Err(SftpError::Status { code, .. }) => assert_eq!(code, status::NO_SUCH_FILE),
        other => panic!("{other:?}"),
    }
    // SYMLINK in OpenSSH's order: the link is made where asked, pointing at the target.
    let made = bytes(&d.join("made"));
    s.symlink(&made, b"target.txt", &never()).unwrap();
    assert_eq!(
        std::fs::read_link(d.join("made")).unwrap(),
        std::path::Path::new("target.txt")
    );
    assert!(!d.join("target.txt").is_symlink());
    close(s);
}

fn download(s: &Session, path: &[u8], hint: Option<u64>) -> Vec<u8> {
    let h = s
        .open(path, open::READ, Attrs::default(), &never())
        .unwrap();
    let mut r = s.reader(h, hint, Arc::new(never()));
    let mut out = Vec::new();
    r.read_to_end(&mut out).unwrap();
    out
}

#[test]
fn pipelined_download_is_byte_exact() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("sftp-download");
    let sizes = [0usize, 1, (1 << 20) + 1, 3 * (1 << 20) + 17];
    for (i, n) in sizes.iter().enumerate() {
        std::fs::write(d.join(format!("f{i}")), noise(*n, i as u64)).unwrap();
    }
    // With the limits the server announces, and with sftp(1)'s defaults.
    for defaults in [false, true] {
        let (s, _lost) = sftp_server(&d.path);
        if defaults {
            assert!(s.set_sizes(Sizes::default()));
        }
        for (i, n) in sizes.iter().enumerate() {
            let p = bytes(&d.join(format!("f{i}")));
            let want = noise(*n, i as u64);
            assert!(download(&s, &p, Some(*n as u64)) == want, "size {n}");
            // A wrong hint only costs requests.
            assert!(download(&s, &p, None) == want, "size {n}, no hint");
            assert!(download(&s, &p, Some(1)) == want, "size {n}, small hint");
        }
        close(s);
    }
}

#[test]
fn pipelined_upload_is_byte_exact() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("sftp-upload");
    let (s, _lost) = sftp_server(&d.path);
    for (i, n) in [0usize, 1, 5 * (1 << 20) + 3].iter().enumerate() {
        let data = noise(*n, 7 + i as u64);
        let dest = d.join(format!("up{i}"));
        let h = s
            .open(
                &bytes(&dest),
                open::WRITE | open::CREAT | open::EXCL,
                Attrs {
                    perms: Some(0o600),
                    ..Attrs::default()
                },
                &never(),
            )
            .unwrap();
        let mut acked = 0;
        let got = s
            .write_from(&h, 0, &mut &data[..], &never(), &mut |a| acked = a)
            .unwrap();
        s.close_handle(&h, &never()).unwrap();
        assert_eq!(got, *n as u64);
        assert_eq!(acked, *n as u64);
        assert!(std::fs::read(&dest).unwrap() == data, "size {n}");
    }
    // An existing name: OPEN with EXCL fails with the server's status.
    match s.open(
        &bytes(&d.join("up0")),
        open::WRITE | open::CREAT | open::EXCL,
        Attrs::default(),
        &never(),
    ) {
        Err(SftpError::Status { .. }) => {}
        other => panic!("{other:?}"),
    }
    close(s);
}

/// A scripted file server: answers `READ`s in reverse order of arrival once `batch` of them
/// are queued or no request came for 20 ms, with every third reply cut to half.
fn serve_file(mut srv: Server, content: Vec<u8>, batch: usize) {
    srv.hello(&[]);
    let mut queued: Vec<(u32, u64, u32)> = Vec::new();
    let mut n = 0u64;
    loop {
        let p = match srv.request_within(Duration::from_millis(20)) {
            Some(Some(p)) => Some(p),
            Some(None) => return,
            None => None,
        };
        let flush = match p {
            None => !queued.is_empty(),
            Some(Packet::Read {
                id, offset, len, ..
            }) => {
                queued.push((id, offset, len));
                queued.len() >= batch
            }
            Some(p) => {
                answer_other(&mut srv, p);
                false
            }
        };
        if flush {
            for (id, offset, len) in queued.drain(..).rev() {
                n += 1;
                let reply = if offset as usize >= content.len() {
                    Packet::Status {
                        id,
                        code: status::EOF,
                        message: vec![],
                        lang: vec![],
                    }
                } else {
                    let end = (offset as usize + len as usize).min(content.len());
                    let mut cut = end;
                    if n.is_multiple_of(3) && end - offset as usize > 1 {
                        cut = offset as usize + (end - offset as usize) / 2;
                    }
                    Packet::Data {
                        id,
                        data: content[offset as usize..cut].to_vec().into(),
                    }
                };
                srv.reply(&reply);
            }
        }
    }
}

fn answer_other(srv: &mut Server, p: Packet) {
    match p {
        Packet::Open { id, .. } => {
            srv.reply(&Packet::Handle {
                id,
                handle: b"h".to_vec(),
            });
        }
        Packet::Close { id, .. } => {
            srv.reply(&Packet::Status {
                id,
                code: status::OK,
                message: vec![],
                lang: vec![],
            });
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn out_of_order_replies_and_short_reads() {
    for (size, window, batch) in [(1_000_000usize, 8, 8), (70_001, 4, 1), (0, 4, 1)] {
        let content = noise(size, 3);
        let c = content.clone();
        let (s, _lost, h) = scripted(move |srv| serve_file(srv, c, batch));
        assert!(s.set_sizes(Sizes {
            read: 4096,
            write: 4096,
            window,
        }));
        let fh = s
            .open(b"/f", open::READ, Attrs::default(), &never())
            .unwrap();
        let mut r = s.reader(fh, Some(size as u64), Arc::new(never()));
        // A single-byte read at a time too: blocks are split across reads.
        let mut out = vec![0u8; 1];
        let n = r.read(&mut out).unwrap();
        let mut rest = Vec::new();
        r.read_to_end(&mut rest).unwrap();
        out.truncate(n);
        out.extend(rest);
        assert!(out == content, "size {size}");
        drop(r);
        assert!(s.lost().is_none());
        close(s);
        h.join().unwrap();
    }
}

#[test]
fn a_window_is_kept_in_flight() {
    if !have_sftp_server() {
        return;
    }
    // 25 ms each way: a round trip of 50 ms. 4 MiB in 32 KiB requests is 128 of them;
    // one at a time that is 6.4 s, with a window of 64 about three round trips.
    let d = test_dir("sftp-latency");
    let data = noise(4 << 20, 11);
    std::fs::write(d.join("f"), &data).unwrap();
    let (s, _lost) = sftp_server_with_latency(&d.path, Duration::from_millis(25));
    assert!(s.set_sizes(Sizes::default()));
    let p = bytes(&d.join("f"));
    let t = Instant::now();
    let got = download(&s, &p, Some(data.len() as u64));
    let took = t.elapsed();
    assert!(got == data);
    eprintln!("4 MiB over a 50 ms round trip: {took:?}");
    assert!(
        took < Duration::from_millis(1500),
        "{took:?}: the reads were not pipelined"
    );
    // Uploads keep a window too.
    let h = s
        .open(
            &bytes(&d.join("up")),
            open::WRITE | open::CREAT,
            Attrs::default(),
            &never(),
        )
        .unwrap();
    let t = Instant::now();
    s.write_from(&h, 0, &mut &data[..], &never(), &mut |_| {})
        .unwrap();
    let took = t.elapsed();
    s.close_handle(&h, &never()).unwrap();
    eprintln!("4 MiB upload over a 50 ms round trip: {took:?}");
    assert!(took < Duration::from_millis(1500), "{took:?}");
    assert!(std::fs::read(d.join("up")).unwrap() == data);
    close(s);
}

#[test]
fn cancel_drains_the_window_and_the_session_stays_usable() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("sftp-cancel");
    let data = noise(8 << 20, 5);
    std::fs::write(d.join("f"), &data).unwrap();
    let (s, lost) = sftp_server_with_latency(&d.path, Duration::from_millis(20));
    assert!(s.set_sizes(Sizes::default()));
    let h = s
        .open(&bytes(&d.join("f")), open::READ, Attrs::default(), &never())
        .unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let mut r = s.reader(h, Some(data.len() as u64), cancel.clone());
    let mut buf = vec![0u8; 100_000];
    r.read_exact(&mut buf).unwrap();
    assert_eq!(buf[..], data[..100_000]);
    let sent = s.stats().requests;
    cancel.store(true, Ordering::SeqCst);
    // The block in hand is still delivered; then the cancel stops the reader.
    let e = loop {
        match r.read(&mut buf) {
            Ok(n) => assert!(n > 0, "the file did not end"),
            Err(e) => break e,
        }
    };
    assert_eq!(sftp_error(&e), Some(&SftpError::Cancelled));
    // Nothing more was sent, and every reply to what was in flight arrived and was dropped.
    assert_eq!(s.stats().requests, sent);
    drop(r);
    assert!(s.lost().is_none());
    assert_eq!(
        s.lstat(&bytes(&d.join("f")), &never()).unwrap().size,
        Some(data.len() as u64)
    );
    assert!(lost.try_recv().is_err());
    close(s);
}

#[test]
fn a_stuck_server_ends_the_session_after_the_drain_window() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("sftp-stuck");
    let fifo = d.join("fifo");
    let st = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(st.success());
    std::fs::write(d.join("f"), b"x").unwrap();
    let (s, lost) = sftp_server(&d.path);
    let pid = s.pid().unwrap();
    // The OPEN of a FIFO blocks sftp-server, and every later request waits behind it.
    let _open = s
        .send(&never(), |id| Packet::Open {
            id,
            path: bytes(&fifo),
            pflags: open::READ,
            attrs: Attrs::default(),
        })
        .unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    let s2 = s.clone();
    let f = bytes(&d.join("f"));
    let t = std::thread::spawn(move || {
        let started = Instant::now();
        let r = s2.lstat(&f, &c);
        (r, started.elapsed())
    });
    std::thread::sleep(Duration::from_millis(300));
    // The server answers nothing while its child lives.
    assert!(!t.is_finished());
    assert!(common::sftp::exists(pid));
    let cancelled = Instant::now();
    cancel.store(true, Ordering::SeqCst);
    let (r, _) = t.join().unwrap();
    let waited = cancelled.elapsed();
    assert_eq!(r, Err(SftpError::Lost));
    assert!(waited >= DRAIN, "{waited:?}");
    assert!(waited < DRAIN + Duration::from_secs(3), "{waited:?}");
    let l = lost.recv_timeout(T).unwrap();
    assert!(l.reason.contains("stopped answering"), "{l:?}");
    assert!(s.lost().is_some());
    // The child was killed and reaped.
    assert!(common::sftp::gone_within(pid, T));
    close(s);
}

/// A cancelled `OPENDIR` and a cancelled `OPEN` whose `HANDLE` arrives during the drain:
/// the session closes each handle instead of dropping it, and stays usable (P3 2.5, 5.5;
/// review finding A1).
#[test]
fn a_cancelled_open_closes_the_handle_that_arrives_in_the_drain() {
    let cancel = Arc::new(AtomicBool::new(false));
    let log: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let (c, l) = (cancel.clone(), log.clone());
    let (s, lost, h) = scripted(move |mut srv| {
        srv.hello(&[]);
        let mut handles = 0u8;
        while let Some(p) = srv.request() {
            let r = match p {
                Packet::Opendir { id, .. } | Packet::Open { id, .. } => {
                    l.lock().unwrap().push(format!("{:?}", p.kind()));
                    // The caller cancels and drains; the handle arrives inside the window.
                    c.store(true, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(200));
                    handles += 1;
                    Packet::Handle {
                        id,
                        handle: vec![b'h', b'0' + handles],
                    }
                }
                Packet::Close { id, handle } => {
                    let h = String::from_utf8_lossy(&handle).into_owned();
                    l.lock().unwrap().push(format!("close {h}"));
                    Packet::Status {
                        id,
                        code: status::OK,
                        message: Vec::new(),
                        lang: Vec::new(),
                    }
                }
                Packet::Lstat { id, .. } => {
                    l.lock().unwrap().push("lstat".into());
                    Packet::Attrs {
                        id,
                        attrs: Attrs::default(),
                    }
                }
                other => panic!("unexpected {other:?}"),
            };
            srv.reply(&r);
        }
    });
    let (opendir, open) = (fxp::OPENDIR.to_string(), fxp::OPEN.to_string());
    assert_eq!(s.opendir(b"/d", &cancel), Err(SftpError::Cancelled));
    cancel.store(false, Ordering::SeqCst);
    assert_eq!(
        s.open(b"/f", open::READ, Attrs::default(), &cancel),
        Err(SftpError::Cancelled)
    );
    assert!(s.lost().is_none());
    // The server answers in order: every CLOSE was handled before this reply.
    assert!(s.lstat(b"/f", &never()).is_ok());
    assert_eq!(
        *log.lock().unwrap(),
        [
            opendir,
            "close h1".into(),
            open,
            "close h2".into(),
            "lstat".to_string()
        ]
    );
    close(s);
    h.join().unwrap();
    assert!(lost.try_recv().is_err());
}

// ---- batches (P3 5.3, 5.5, 5.6) ----------------------------------------------------------

fn lstat_reply(id: u32, size: u64) -> Packet {
    Packet::Attrs {
        id,
        attrs: Attrs {
            size: Some(size),
            ..Attrs::default()
        },
    }
}

/// A batch sends its requests without waiting for a reply (the server reads all three
/// before it answers any) and collects the replies in any order, each at its request's
/// index. When the session ends in the middle of a batch, the replies that arrived before
/// the end are kept, and the others are missing.
#[test]
fn a_batch_costs_one_round_trip_and_keeps_what_arrived_before_a_loss() {
    let (s, lost, h) = scripted(|mut srv| {
        srv.hello(&[]);
        let mut ids = Vec::new();
        for _ in 0..3 {
            let Some(Some(Packet::Lstat { id, .. })) = srv.request_within(T) else {
                panic!("the batch's requests did not all arrive before a reply")
            };
            ids.push(id);
        }
        for (k, id) in ids.iter().enumerate().rev() {
            srv.reply(&lstat_reply(*id, k as u64));
        }
        // The second batch: two replies, then the connection ends.
        let mut ids = Vec::new();
        for _ in 0..3 {
            let Some(Some(Packet::Lstat { id, .. })) = srv.request_within(T) else {
                panic!("expected LSTAT")
            };
            ids.push(id);
        }
        srv.reply(&lstat_reply(ids[1], 11));
        srv.reply(&lstat_reply(ids[0], 10));
        srv.hang_up();
        while srv.request().is_some() {}
    });
    let never = never();
    let mut b = s.batch(&never);
    for _ in 0..3 {
        b.send(|id| Packet::Lstat {
            id,
            path: b"/x".to_vec(),
        })
        .unwrap();
    }
    let mut got = b.collect();
    assert!(!got.lost && !got.cancelled);
    for k in 0..3 {
        match got.take(k) {
            Some(Packet::Attrs { attrs, .. }) => assert_eq!(attrs.size, Some(k as u64)),
            other => panic!("{k}: {other:?}"),
        }
    }
    let mut b = s.batch(&never);
    for _ in 0..3 {
        b.send(|id| Packet::Lstat {
            id,
            path: b"/y".to_vec(),
        })
        .unwrap();
    }
    let mut got = b.collect();
    assert!(got.lost);
    for (k, size) in [(0, Some(10)), (1, Some(11)), (2, None)] {
        let a = got.take(k).map(|p| match p {
            Packet::Attrs { attrs, .. } => attrs.size.unwrap(),
            other => panic!("{other:?}"),
        });
        assert_eq!(a, size, "reply {k}");
    }
    assert!(lost.recv_timeout(T).is_ok());
    close(s);
    h.join().unwrap();
}

/// A cancel drops none of a batch's replies: the batch waits for them and says it saw the
/// cancel, and the session stays usable (P3 5.6). A server that then answers nothing for
/// the drain window after a cancel is stuck: the session ends (P3 2.5).
#[test]
fn a_cancelled_batch_waits_for_its_replies_and_a_silent_server_ends_the_session() {
    let cancel = Arc::new(AtomicBool::new(false));
    let c = cancel.clone();
    let (s, lost, h) = scripted(move |mut srv| {
        srv.hello(&[]);
        let mut ids = Vec::new();
        for _ in 0..2 {
            let Some(Packet::Lstat { id, .. }) = srv.request() else {
                panic!("expected LSTAT")
            };
            ids.push(id);
        }
        c.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(300));
        for id in ids {
            srv.reply(&lstat_reply(id, 7));
        }
        // The next batch gets no reply at all.
        while srv.request().is_some() {}
    });
    let mut b = s.batch(&cancel);
    for _ in 0..2 {
        b.send(|id| Packet::Lstat {
            id,
            path: b"/x".to_vec(),
        })
        .unwrap();
    }
    let mut got = b.collect();
    assert!(got.cancelled && !got.lost);
    assert!(got.take(0).is_some() && got.take(1).is_some());
    assert!(s.lost().is_none());
    let mut b = s.batch(&cancel);
    b.send(|id| Packet::Lstat {
        id,
        path: b"/x".to_vec(),
    })
    .unwrap();
    let t = Instant::now();
    let got = b.collect();
    let waited = t.elapsed();
    assert!(got.lost, "{got:?}");
    assert!(
        waited >= DRAIN && waited < DRAIN + Duration::from_secs(3),
        "{waited:?}"
    );
    let l = lost.recv_timeout(T).unwrap();
    assert!(l.reason.contains("stopped answering"), "{l:?}");
    close(s);
    h.join().unwrap();
}

/// Batch mode (P3 5.5): once its `READ`s reach the size, the reader sends a one-byte `READ`
/// at the size, the final `FSTAT` and the `CLOSE` behind them, all before any reply. A
/// short read that arrives after the `CLOSE` went out ends the reader with `Stop::Gap` at
/// the first byte it could not deliver; `finish` then drops what is in flight and returns
/// the `FSTAT`'s attributes and the `CLOSE`'s outcome.
#[test]
fn batch_mode_sends_the_close_behind_the_last_read_and_stops_at_a_gap() {
    use manycommander::remote::session::Stop;
    let content = noise(10_000, 4);
    let c = content.clone();
    let (s, _lost, h) = scripted(move |mut srv| {
        srv.hello(&[]);
        let Some(Packet::Open { id, .. }) = srv.request() else {
            panic!("expected OPEN")
        };
        srv.reply(&Packet::Handle {
            id,
            handle: b"h".to_vec(),
        });
        let mut got = Vec::new();
        for _ in 0..6 {
            let Some(Some(p)) = srv.request_within(T) else {
                panic!("the batch did not arrive before a reply: {got:?}")
            };
            got.push(p);
        }
        let reads: Vec<(u32, u64, u32)> = got
            .iter()
            .filter_map(|p| match p {
                Packet::Read {
                    id, offset, len, ..
                } => Some((*id, *offset, *len)),
                _ => None,
            })
            .collect();
        assert_eq!(
            reads.iter().map(|r| (r.1, r.2)).collect::<Vec<_>>(),
            [(0, 4096), (4096, 4096), (8192, 1808), (10_000, 1)]
        );
        assert!(matches!(got[4], Packet::Fstat { .. }), "{got:?}");
        assert!(matches!(got[5], Packet::Close { .. }), "{got:?}");
        for (k, (id, off, len)) in reads.into_iter().enumerate() {
            let reply = match k {
                // The third READ is cut to half: a short read after the CLOSE went out.
                2 => Packet::Data {
                    id,
                    data: c[off as usize..off as usize + 904].to_vec().into(),
                },
                3 => Packet::Status {
                    id,
                    code: status::EOF,
                    message: vec![],
                    lang: vec![],
                },
                _ => Packet::Data {
                    id,
                    data: c[off as usize..(off + u64::from(len)) as usize]
                        .to_vec()
                        .into(),
                },
            };
            srv.reply(&reply);
        }
        srv.reply(&lstat_reply(got[4].id().unwrap(), 10_000));
        srv.reply(&Packet::Status {
            id: got[5].id().unwrap(),
            code: status::OK,
            message: vec![],
            lang: vec![],
        });
        while srv.request().is_some() {}
    });
    assert!(s.set_sizes(Sizes {
        read: 4096,
        write: 4096,
        window: 8,
    }));
    let fh = s
        .open(b"/f", open::READ, Attrs::default(), &never())
        .unwrap();
    let mut r = s.reader_exact(fh, 10_000, Arc::new(never()));
    r.prime().unwrap();
    let mut out = Vec::new();
    r.read_to_end(&mut out).unwrap();
    assert_eq!(r.stop(), Some(Stop::Gap));
    assert_eq!(r.position(), 9096);
    assert!(out[..] == content[..9096]);
    let (a, closed) = r.finish().unwrap();
    assert_eq!(a.size, Some(10_000));
    assert_eq!(closed, Ok(()));
    drop(r);
    assert!(s.lost().is_none());
    close(s);
    h.join().unwrap();
}

#[test]
fn session_loss_fails_every_outstanding_request() {
    if !have_sftp_server() {
        return;
    }
    no_core_dumps();
    let d = test_dir("sftp-loss");
    let fifo = d.join("fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let (s, lost) = sftp_server(&d.path);
    let pid = s.pid().unwrap();
    s.send(&never(), |id| Packet::Open {
        id,
        path: bytes(&fifo),
        pflags: open::READ,
        attrs: Attrs::default(),
    })
    .unwrap();
    let waiters: Vec<_> = (0..4)
        .map(|_| {
            let s = s.clone();
            let p = bytes(&d.path);
            std::thread::spawn(move || s.lstat(&p, &AtomicBool::new(false)))
        })
        .collect();
    std::thread::sleep(Duration::from_millis(200));
    // The server dies, as a dropped connection would end ssh.
    let spid = rustix::process::Pid::from_raw(pid as i32).unwrap();
    rustix::process::kill_process(spid, rustix::process::Signal::KILL).unwrap();
    for w in waiters {
        assert_eq!(w.join().unwrap(), Err(SftpError::Lost));
    }
    let l = lost.recv_timeout(T).unwrap();
    assert!(l.reason.contains("closed"), "{l:?}");
    assert!(lost.try_recv().is_err(), "the loss is reported once");
    assert!(common::sftp::gone_within(pid, T), "the child is reaped");
    assert_eq!(s.realpath(b".", &never()), Err(SftpError::Lost));
    close(s);
}

#[test]
fn a_closed_session_reports_no_loss_and_its_child_exits() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("sftp-close");
    let (s, lost) = sftp_server(&d.path);
    let pid = s.pid().unwrap();
    assert!(s.realpath(b".", &never()).is_ok());
    // Dropping the last handle closes without waiting; a helper thread reaps.
    drop(s);
    assert!(common::sftp::gone_within(pid, T));
    assert!(lost.recv_timeout(Duration::from_millis(300)).is_err());
}

#[test]
fn server_errors_arrive_as_statuses() {
    if !have_sftp_server() {
        return;
    }
    let d = test_dir("sftp-errors");
    std::fs::create_dir(d.join("ro")).unwrap();
    std::fs::set_permissions(d.join("ro"), std::fs::Permissions::from_mode(0o500)).unwrap();
    let (s, _lost) = sftp_server(&d.path);
    match s.lstat(&bytes(&d.join("missing")), &never()) {
        Err(SftpError::Status { code, .. }) => assert_eq!(code, status::NO_SUCH_FILE),
        other => panic!("{other:?}"),
    }
    match s.mkdir(&bytes(&d.join("ro/x")), Attrs::default(), &never()) {
        Err(SftpError::Status { code, .. }) => assert_eq!(code, status::PERMISSION_DENIED),
        other => panic!("{other:?}"),
    }
    s.mkdir(&bytes(&d.join("new")), Attrs::default(), &never())
        .unwrap();
    s.rename(&bytes(&d.join("new")), &bytes(&d.join("renamed")), &never())
        .unwrap();
    s.rmdir(&bytes(&d.join("renamed")), &never()).unwrap();
    assert!(!d.join("renamed").exists());
    std::fs::set_permissions(d.join("ro"), std::fs::Permissions::from_mode(0o700)).unwrap();
    close(s);
}

/// P-26's shape, by hand: a 256 MiB download through `sftp-server` on pipes with the
/// pipelined window, against `sftp -D sftp-server` getting the same file. Run with
/// `cargo test --release --test sftp_proto -- --ignored --nocapture throughput`.
#[test]
#[ignore]
fn throughput_against_sftp_on_the_same_pipe() {
    if !have_sftp_server() {
        return;
    }
    no_core_dumps();
    let d = test_dir("sftp-throughput");
    let n = 256usize << 20;
    let src = d.join("big");
    std::fs::write(&src, noise(n, 1)).unwrap();
    let (s, _lost) = sftp_server(&d.path);
    let p = bytes(&src);
    // As the copy engine reads a `Stream` (P3 2.3): into its 1 MiB job buffer, each read
    // written to the temporary file; and with the file writes left out.
    let mut best = Duration::MAX;
    let mut best_discard = Duration::MAX;
    let mut buf = vec![0u8; 1 << 20];
    for round in 0..6 {
        let discard = round % 2 == 1;
        let h = s.open(&p, open::READ, Attrs::default(), &never()).unwrap();
        let t = Instant::now();
        let mut r = s.reader(h, Some(n as u64), Arc::new(never()));
        let mut out = std::fs::File::create(d.join("ours")).unwrap();
        let mut copied = 0u64;
        loop {
            let k = r.read(&mut buf).unwrap();
            if k == 0 {
                break;
            }
            if !discard {
                std::io::Write::write_all(&mut out, &buf[..k]).unwrap();
            }
            copied += k as u64;
        }
        drop(out);
        let took = t.elapsed();
        if discard {
            best_discard = best_discard.min(took);
        } else {
            best = best.min(took);
        }
        assert_eq!(copied, n as u64);
    }
    eprintln!(
        "256 MiB, protocol only (no file writes): {:.0} ms",
        best_discard.as_secs_f64() * 1000.0
    );
    close(s);
    let mut sftp_best = Duration::MAX;
    for _ in 0..3 {
        let _ = std::fs::remove_file(d.join("theirs"));
        let t = Instant::now();
        let st = std::process::Command::new("sftp")
            .arg("-q")
            .arg("-D")
            .arg(common::sftp::SFTP_SERVER)
            .arg("-b")
            .arg("-")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .and_then(|mut c| {
                use std::io::Write;
                let mut i = c.stdin.take().unwrap();
                writeln!(i, "get {} {}", src.display(), d.join("theirs").display())?;
                drop(i);
                c.wait()
            });
        match st {
            Ok(s) if s.success() => sftp_best = sftp_best.min(t.elapsed()),
            other => {
                eprintln!("sftp did not run: {other:?}");
                return;
            }
        }
    }
    let mib = n as f64 / (1 << 20) as f64;
    eprintln!(
        "256 MiB: manycommander {:.0} ms ({:.0} MiB/s), sftp {:.0} ms ({:.0} MiB/s), ratio {:.2}",
        best.as_secs_f64() * 1000.0,
        mib / best.as_secs_f64(),
        sftp_best.as_secs_f64() * 1000.0,
        mib / sftp_best.as_secs_f64(),
        best.as_secs_f64() / sftp_best.as_secs_f64()
    );
}
