#![forbid(unsafe_code)]
//! The seam to non-local places (P3 2.1): a provider, not a VFS.
//!
//! The local engine keeps its directory fds, `O_PATH` opens, `renameat2` and `statx`
//! identities: abstracting local I/O behind a trait would weaken I-1 to I-7 and slow P-3
//! and P-7. Only non-local places, an archive index or an SFTP session, go through the
//! narrow [`Provider`]. A path in such a place is a [`VPath`] of byte components, each a
//! single path component (M1 3.2: bytes end to end, no `String` round trip). What a place
//! allows is its [`Caps`], and what fails there is a [`PlaceError`].
//!
//! The seam lands before its implementations: the archive index (T2) and the SFTP session
//! (T5, T6) implement [`Provider`]. Until then the panel's archive and remote sources, a
//! job's non-local roots and the remote destination hold a provider as `Arc<dyn Provider>`.

use crate::fsops::plan::valid_component;
use crate::fsops::sys::{FsIdentity, Meta, Ts};
use crate::panel::listing::ListingMsg;
use rustix::io::Errno;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Read;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::sync::atomic::AtomicBool;

/// A non-local place: an archive index or an SFTP session (P3 2.1).
pub trait Provider: Send + Sync {
    /// What the place allows: read-only for archives; per server extension for SFTP.
    fn caps(&self) -> Caps;
    /// Lists one directory on a listing thread, sending `ListingMsg` batches.
    fn list(
        &self,
        dir: &VPath,
        out: &mut dyn FnMut(ListingMsg),
        cancel: &AtomicBool,
    ) -> Result<(), PlaceError>;
    /// The entry itself, never a symlink's target. Its identity is synthetic
    /// ([`synthetic_id`]): it never equals a local one.
    fn lstat(&self, path: &VPath) -> Result<Meta, PlaceError>;
    /// A regular file's content, for F3, F4 and the quick view; never a symlink.
    fn open_read(
        &self,
        path: &VPath,
        cancel: &AtomicBool,
    ) -> Result<Box<dyn Read + Send>, PlaceError>;
}

/// What a place allows (P3 2.1). An archive allows nothing that writes (A-2); a server
/// allows what its extensions support (P3 5.3, 5.6).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    /// The write verbs of phase 3b: upload, mkdir, rename, delete (P3 5.6).
    pub write: bool,
    /// Uploads commit through `hardlink@openssh.com` (R-1); without it, direct write.
    pub hard_link: bool,
    /// An overwrite replaces atomically through `posix-rename@openssh.com` (R-2); without
    /// it, the overwrite is refused.
    pub posix_rename: bool,
    /// `fsync@openssh.com` syncs an uploaded file's data (R-4).
    pub fsync: bool,
    /// `statvfs@openssh.com` gives the free space for the footer (P3 5.4).
    pub statvfs: bool,
    /// Members are read by locator, so the quick view may preview on cursor rest: zip, 7z
    /// and plain tar (V-5). Compressed tars and servers preview only on `Alt+Q`.
    pub random_access: bool,
}

/// A path in a non-local place (P3 2.1): a list of byte components, each passing
/// [`valid_component`] (not empty, `.` or `..`, without `/` or NUL). The empty list is the
/// place's root. Nothing ever joins a `VPath` onto a filesystem path (A-1).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VPath {
    parts: Vec<OsString>,
}

impl VPath {
    /// The root of the place.
    pub fn root() -> VPath {
        VPath::default()
    }

    /// A path from its components; [`PlaceError::UnsafePath`] when one of them is not a
    /// single component.
    pub fn new(parts: Vec<OsString>) -> Result<VPath, PlaceError> {
        if parts.iter().all(|c| valid_component(c)) {
            Ok(VPath { parts })
        } else {
            Err(PlaceError::UnsafePath)
        }
    }

    /// Splits a member or server name on `/` (A-1): empty and `.` components and a
    /// leading `/` are dropped; a `..` component, a NUL, or any other component that is
    /// not a single component makes the name unsafe ([`PlaceError::UnsafePath`]). The
    /// bytes are kept as they are.
    pub fn parse(name: &[u8]) -> Result<VPath, PlaceError> {
        let mut parts = Vec::new();
        for c in name.split(|&b| b == b'/') {
            if c.is_empty() || c == b"." {
                continue;
            }
            let c = OsStr::from_bytes(c);
            if !valid_component(c) {
                return Err(PlaceError::UnsafePath);
            }
            parts.push(c.to_owned());
        }
        Ok(VPath { parts })
    }

    /// `self/name`; [`PlaceError::UnsafePath`] when `name` is not a single component.
    pub fn join(&self, name: &OsStr) -> Result<VPath, PlaceError> {
        if !valid_component(name) {
            return Err(PlaceError::UnsafePath);
        }
        let mut parts = Vec::with_capacity(self.parts.len() + 1);
        parts.extend_from_slice(&self.parts);
        parts.push(name.to_owned());
        Ok(VPath { parts })
    }

    /// The directory above; `None` at the root.
    pub fn parent(&self) -> Option<VPath> {
        let (_, up) = self.parts.split_last()?;
        Some(VPath { parts: up.to_vec() })
    }

    /// The last component; `None` at the root.
    pub fn name(&self) -> Option<&OsStr> {
        self.parts.last().map(OsString::as_os_str)
    }

    pub fn components(&self) -> &[OsString] {
        &self.parts
    }

    pub fn is_root(&self) -> bool {
        self.parts.is_empty()
    }

    /// Whether `self` is `base` or lies below it.
    pub fn starts_with(&self, base: &VPath) -> bool {
        self.parts.starts_with(&base.parts)
    }

    /// The absolute form, `/a/b`, or `/` for the root: what a title or a report shows
    /// (escaped by the caller, as any name).
    pub fn to_bytes(&self) -> Vec<u8> {
        if self.parts.is_empty() {
            return b"/".to_vec();
        }
        let mut out = Vec::with_capacity(self.parts.iter().map(|c| c.len() + 1).sum());
        for c in &self.parts {
            out.push(b'/');
            out.extend_from_slice(c.as_bytes());
        }
        out
    }

    /// [`VPath::to_bytes`] as an `OsString`.
    pub fn to_os_string(&self) -> OsString {
        OsString::from_vec(self.to_bytes())
    }
}

/// Why a provider call failed (P3 2.1). The messages are the ones the design gives the
/// user; a job reports them per entry (I-7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaceError {
    /// No entry at the path.
    NotFound,
    /// A directory was needed and the entry is something else.
    NotADirectory,
    /// A regular file was needed and the entry is something else: a directory, a symlink,
    /// a FIFO or a device is never opened for data (V-2, R-3).
    NotAFile,
    /// A name that is not a single component, or a member path that fails A-1.
    UnsafePath,
    /// The place refuses the call, with the reason ("archives are read-only",
    /// "encrypted").
    Refused(String),
    /// The data is damaged or no longer matches the index ("archive damaged", "archive
    /// changed", "size mismatch").
    Damaged(String),
    /// The caller cancelled.
    Cancelled,
    /// The session ended (P3 2.5).
    Lost,
    /// An OS error on the local side, such as a read of the archive file.
    Os(Errno),
}

impl fmt::Display for PlaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlaceError::NotFound => f.write_str("disappeared"),
            PlaceError::NotADirectory => f.write_str("not a directory"),
            PlaceError::NotAFile => f.write_str("not a regular file"),
            PlaceError::UnsafePath => f.write_str("unsafe path"),
            PlaceError::Refused(why) | PlaceError::Damaged(why) => f.write_str(why),
            PlaceError::Cancelled => f.write_str("cancelled"),
            PlaceError::Lost => f.write_str("connection lost"),
            PlaceError::Os(e) => f.write_str(&crate::fsops::walk::errno_text(*e)),
        }
    }
}

impl std::error::Error for PlaceError {}

/// A server as the user typed it (P3 5.1): `sftp://[user@]host[:port]`. It keys a
/// connection, and a history place keeps it to reconnect (P3 2.2, 5.7). The address
/// grammar that validates the parts is T5's.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Target {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
}

impl Target {
    /// `sftp://[user@]host[:port]`, without a path.
    pub fn address(&self) -> String {
        let mut s = String::from("sftp://");
        if let Some(u) = &self.user {
            s.push_str(u);
            s.push('@');
        }
        s.push_str(&self.host);
        if let Some(p) = self.port {
            s.push_str(&format!(":{p}"));
        }
        s
    }
}

/// An archive file's identity in the index cache (P3 3.2): `(st_dev, st_ino, size, mtime,
/// ctime)`. Another key for the same name means the archive changed on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct StatKey {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime: Ts,
    pub ctime: Ts,
}

impl StatKey {
    pub fn of(m: &Meta) -> StatKey {
        StatKey {
            dev: m.id.dev,
            ino: m.id.ino,
            size: m.size,
            mtime: m.mtime,
            ctime: m.ctime,
        }
    }
}

/// The `st_dev` bit of a synthetic identity (P3 2.1). The kernel's `dev_t` has 12 major and
/// 20 minor bits, so no local `st_dev` has it, and a synthetic identity never equals a
/// local one: the same-file and destination-inside-source checks (M1 4.6) never match a
/// non-local entry.
pub const SYNTHETIC_DEV: u64 = 1 << 63;

/// The synthetic identity of entry `n` in place `place`.
pub fn synthetic_id(place: u64, n: u64) -> FsIdentity {
    FsIdentity {
        dev: SYNTHETIC_DEV | place,
        ino: n,
        mnt_id: 0,
    }
}

/// Whether an identity is synthetic (from a non-local place).
pub fn is_synthetic(id: &FsIdentity) -> bool {
    id.dev & SYNTHETIC_DEV != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &[u8]) -> OsString {
        OsString::from_vec(s.to_vec())
    }

    #[test]
    fn member_names_split_into_single_components() {
        let p = VPath::parse(b"/./a//b/./c\xff/").unwrap();
        assert_eq!(p.components(), [os(b"a"), os(b"b"), os(b"c\xff")]);
        assert_eq!(p.to_bytes(), b"/a/b/c\xff");
        assert_eq!(VPath::parse(b"").unwrap(), VPath::root());
        assert_eq!(VPath::parse(b"./").unwrap(), VPath::root());
        for bad in [&b"a/../b"[..], b"..", b"a/b\0c", b"../x"] {
            assert_eq!(VPath::parse(bad), Err(PlaceError::UnsafePath), "{bad:?}");
        }
        assert_eq!(PlaceError::UnsafePath.to_string(), "unsafe path");
    }

    #[test]
    fn paths_join_and_split_by_component() {
        let root = VPath::root();
        assert!(root.is_root() && root.parent().is_none() && root.name().is_none());
        assert_eq!(root.to_bytes(), b"/");
        let a = root.join(OsStr::new("a")).unwrap();
        let ab = a.join(OsStr::new("b")).unwrap();
        assert_eq!(ab.parent(), Some(a.clone()));
        assert_eq!(ab.name(), Some(OsStr::new("b")));
        assert!(ab.starts_with(&a) && ab.starts_with(&root) && !a.starts_with(&ab));
        for bad in ["", ".", "..", "x/y"] {
            assert_eq!(a.join(OsStr::new(bad)), Err(PlaceError::UnsafePath));
        }
        assert!(VPath::new(vec![os(b"ok"), os(b"a\0")]).is_err());
        assert_eq!(VPath::new(vec![os(b"x")]).unwrap().to_os_string(), "/x");
    }

    #[test]
    fn a_synthetic_identity_never_equals_a_local_one() {
        let id = synthetic_id(3, 7);
        assert!(is_synthetic(&id));
        // The largest local device number: 12 major and 20 minor bits.
        let local = rustix::fs::makedev(0xfff, 0xfffff);
        assert_eq!(local & SYNTHETIC_DEV, 0);
        assert!(!is_synthetic(&FsIdentity {
            dev: local,
            ..FsIdentity::default()
        }));
    }

    #[test]
    fn a_target_prints_as_typed() {
        let t = Target {
            user: Some("u".into()),
            host: "h".into(),
            port: Some(2222),
        };
        assert_eq!(t.address(), "sftp://u@h:2222");
        let t = Target {
            user: None,
            host: "h".into(),
            port: None,
        };
        assert_eq!(t.address(), "sftp://h");
    }
}
