#![forbid(unsafe_code)]
//! The SFTP version 3 codec (P3 5.3): draft-ietf-secsh-filexfer-02 plus the OpenSSH
//! extensions of its `PROTOCOL` file.
//!
//! A packet is a 32-bit length, a type byte and, for every type except `SSH_FXP_INIT` and
//! `SSH_FXP_VERSION`, a 32-bit request id; those two carry a version number instead. A
//! frame longer than [`MAX_PACKET`] ends the session before anything is allocated for it.
//! Inside a packet every string length and every count is checked against the bytes left
//! before any allocation (compare RUSTSEC-2026-0154), so the largest buffer a hostile reply
//! can make manycommander hold is the frame itself. Both directions are encoded and
//! decoded: the client sends requests and reads replies, and the scripted test server does
//! the reverse with the same code.
//!
//! Names and paths stay bytes (M1 3.2).

use std::fmt;
use std::io::Read;

/// The protocol version manycommander speaks.
pub const VERSION: u32 = 3;

/// The largest data a `READ` asks for or a `WRITE` carries: OpenSSH's own maximum message
/// length, 256 KiB (P3 5.3).
pub const MAX_DATA: usize = 256 * 1024;

/// The header of the largest reply, an `SSH_FXP_DATA` of [`MAX_DATA`] bytes: the type, the
/// request id and the data length.
pub const DATA_HEADER: usize = 1 + 4 + 4;

/// The longest frame accepted: 256 KiB plus its header (P3 5.3). A longer one ends the
/// session.
pub const MAX_PACKET: usize = MAX_DATA + DATA_HEADER;

/// Packet types.
pub mod fxp {
    pub const INIT: u8 = 1;
    pub const VERSION: u8 = 2;
    pub const OPEN: u8 = 3;
    pub const CLOSE: u8 = 4;
    pub const READ: u8 = 5;
    pub const WRITE: u8 = 6;
    pub const LSTAT: u8 = 7;
    pub const FSTAT: u8 = 8;
    pub const SETSTAT: u8 = 9;
    pub const FSETSTAT: u8 = 10;
    pub const OPENDIR: u8 = 11;
    pub const READDIR: u8 = 12;
    pub const REMOVE: u8 = 13;
    pub const MKDIR: u8 = 14;
    pub const RMDIR: u8 = 15;
    pub const REALPATH: u8 = 16;
    pub const STAT: u8 = 17;
    pub const RENAME: u8 = 18;
    pub const READLINK: u8 = 19;
    pub const SYMLINK: u8 = 20;
    pub const STATUS: u8 = 101;
    pub const HANDLE: u8 = 102;
    pub const DATA: u8 = 103;
    pub const NAME: u8 = 104;
    pub const ATTRS: u8 = 105;
    pub const EXTENDED: u8 = 200;
    pub const EXTENDED_REPLY: u8 = 201;
}

/// `SSH_FX_*` status codes.
pub mod status {
    pub const OK: u32 = 0;
    pub const EOF: u32 = 1;
    pub const NO_SUCH_FILE: u32 = 2;
    pub const PERMISSION_DENIED: u32 = 3;
    pub const FAILURE: u32 = 4;
    pub const BAD_MESSAGE: u32 = 5;
    pub const NO_CONNECTION: u32 = 6;
    pub const CONNECTION_LOST: u32 = 7;
    pub const OP_UNSUPPORTED: u32 = 8;
}

/// `SSH_FXF_*` flags of `SSH_FXP_OPEN`.
pub mod open {
    pub const READ: u32 = 0x01;
    pub const WRITE: u32 = 0x02;
    pub const APPEND: u32 = 0x04;
    pub const CREAT: u32 = 0x08;
    pub const TRUNC: u32 = 0x10;
    pub const EXCL: u32 = 0x20;
}

/// The extensions manycommander uses, as `SSH_FXP_VERSION` announces them: name and
/// version (P3 5.3). An extension is used only when announced with that version.
pub mod ext {
    pub const POSIX_RENAME: (&[u8], &[u8]) = (b"posix-rename@openssh.com", b"1");
    pub const HARDLINK: (&[u8], &[u8]) = (b"hardlink@openssh.com", b"1");
    pub const FSYNC: (&[u8], &[u8]) = (b"fsync@openssh.com", b"1");
    pub const STATVFS: (&[u8], &[u8]) = (b"statvfs@openssh.com", b"2");
    pub const LIMITS: (&[u8], &[u8]) = (b"limits@openssh.com", b"1");
    /// Announced without the `@openssh.com` suffix. It takes one string argument, the
    /// user name (empty: the login user), and is answered with `SSH_FXP_NAME`.
    pub const HOME_DIRECTORY: (&[u8], &[u8]) = (b"home-directory", b"1");
}

/// `SSH_FILEXFER_ATTR_*` flags.
mod attr {
    pub const SIZE: u32 = 0x0000_0001;
    pub const UIDGID: u32 = 0x0000_0002;
    pub const PERMISSIONS: u32 = 0x0000_0004;
    pub const ACMODTIME: u32 = 0x0000_0008;
    pub const EXTENDED: u32 = 0x8000_0000;
    pub const KNOWN: u32 = SIZE | UIDGID | PERMISSIONS | ACMODTIME | EXTENDED;
}

/// File attributes (`ATTRS`). Every field is optional on the wire; times are whole seconds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attrs {
    pub size: Option<u64>,
    pub uid_gid: Option<(u32, u32)>,
    /// The mode, including the file type bits (`S_IFMT`) when the server sends them.
    pub perms: Option<u32>,
    /// `(atime, mtime)`.
    pub times: Option<(u32, u32)>,
    pub extended: Vec<(Vec<u8>, Vec<u8>)>,
}

impl Attrs {
    /// The file type bits of the mode, when the server sent a mode.
    pub fn kind(&self) -> Option<u32> {
        self.perms.map(|p| p & 0o170_000)
    }
}

/// One entry of an `SSH_FXP_NAME` reply.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Name {
    pub filename: Vec<u8>,
    /// The `ls -l`-style line; manycommander ignores it (P3 5.4).
    pub longname: Vec<u8>,
    pub attrs: Attrs,
}

/// A packet's bulk data (`SSH_FXP_DATA`, `SSH_FXP_WRITE`), kept inside the frame it
/// arrived in, so a 256 KiB reply is never copied on its way to the caller.
#[derive(Clone, Default)]
pub struct Payload {
    buf: Vec<u8>,
    start: usize,
}

impl Payload {
    pub fn new(buf: Vec<u8>) -> Payload {
        Payload { buf, start: 0 }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf[self.start..]
    }
}

impl std::ops::Deref for Payload {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl PartialEq for Payload {
    fn eq(&self, other: &Payload) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for Payload {}

impl fmt::Debug for Payload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Payload({} bytes)", self.len())
    }
}

impl From<Vec<u8>> for Payload {
    fn from(v: Vec<u8>) -> Payload {
        Payload::new(v)
    }
}

/// Extension pairs of `SSH_FXP_INIT` and `SSH_FXP_VERSION`.
pub type Extensions = Vec<(Vec<u8>, Vec<u8>)>;

/// Every packet type of the protocol, in both directions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Packet {
    /// No request id: the version number instead.
    Init {
        version: u32,
        extensions: Extensions,
    },
    /// No request id: the version number instead.
    Version {
        version: u32,
        extensions: Extensions,
    },
    Open {
        id: u32,
        path: Vec<u8>,
        pflags: u32,
        attrs: Attrs,
    },
    Close {
        id: u32,
        handle: Vec<u8>,
    },
    Read {
        id: u32,
        handle: Vec<u8>,
        offset: u64,
        len: u32,
    },
    Write {
        id: u32,
        handle: Vec<u8>,
        offset: u64,
        data: Payload,
    },
    Lstat {
        id: u32,
        path: Vec<u8>,
    },
    Fstat {
        id: u32,
        handle: Vec<u8>,
    },
    Setstat {
        id: u32,
        path: Vec<u8>,
        attrs: Attrs,
    },
    Fsetstat {
        id: u32,
        handle: Vec<u8>,
        attrs: Attrs,
    },
    Opendir {
        id: u32,
        path: Vec<u8>,
    },
    Readdir {
        id: u32,
        handle: Vec<u8>,
    },
    Remove {
        id: u32,
        path: Vec<u8>,
    },
    Mkdir {
        id: u32,
        path: Vec<u8>,
        attrs: Attrs,
    },
    Rmdir {
        id: u32,
        path: Vec<u8>,
    },
    Realpath {
        id: u32,
        path: Vec<u8>,
    },
    Stat {
        id: u32,
        path: Vec<u8>,
    },
    Rename {
        id: u32,
        from: Vec<u8>,
        to: Vec<u8>,
    },
    Readlink {
        id: u32,
        path: Vec<u8>,
    },
    /// Creates `link` pointing at `target`. OpenSSH takes the two paths in the reverse of
    /// the draft's order, target first, and the wire carries them that way (P3 5.3).
    Symlink {
        id: u32,
        link: Vec<u8>,
        target: Vec<u8>,
    },
    Status {
        id: u32,
        code: u32,
        message: Vec<u8>,
        lang: Vec<u8>,
    },
    Handle {
        id: u32,
        handle: Vec<u8>,
    },
    Data {
        id: u32,
        data: Payload,
    },
    Name {
        id: u32,
        names: Vec<Name>,
    },
    Attrs {
        id: u32,
        attrs: Attrs,
    },
    /// An extension request: its name, then its own arguments.
    Extended {
        id: u32,
        name: Vec<u8>,
        data: Vec<u8>,
    },
    /// An extension's reply; its layout depends on the request ([`Limits`], [`StatVfs`]).
    ExtendedReply {
        id: u32,
        data: Vec<u8>,
    },
}

/// Why a frame or a packet does not decode. Any of them ends the session (P3 5.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The frame's length field is above [`MAX_PACKET`].
    TooLong(u32),
    /// A frame of length zero: no type byte.
    Empty,
    /// The stream ended inside a frame.
    TruncatedFrame,
    /// A field runs past the end of the packet (a string length that is too large).
    Truncated,
    /// A count claims more items than the bytes left could hold.
    Count(u32),
    /// A type this protocol does not have.
    UnknownType(u8),
    /// Attribute flags this protocol version does not define.
    AttrFlags(u32),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::TooLong(n) => write!(f, "packet of {n} bytes is too long"),
            DecodeError::Empty => f.write_str("empty packet"),
            DecodeError::TruncatedFrame => f.write_str("the stream ended inside a packet"),
            DecodeError::Truncated => f.write_str("truncated packet"),
            DecodeError::Count(n) => write!(f, "count {n} is larger than the packet"),
            DecodeError::UnknownType(t) => write!(f, "unknown packet type {t}"),
            DecodeError::AttrFlags(x) => write!(f, "unknown attribute flags {x:#x}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// A bounds-checked reader over one packet.
struct Cur<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Cur<'a> {
    fn left(&self) -> usize {
        self.b.len() - self.at
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if n > self.left() {
            return Err(DecodeError::Truncated);
        }
        let s = &self.b[self.at..self.at + n];
        self.at += n;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        let s = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Ok(u64::from_be_bytes(a))
    }

    /// A string's bytes, borrowed: its length is checked before anything is copied.
    fn str(&mut self) -> Result<&'a [u8], DecodeError> {
        let n = self.u32()? as usize;
        self.take(n)
    }

    fn string(&mut self) -> Result<Vec<u8>, DecodeError> {
        Ok(self.str()?.to_vec())
    }

    /// A count of items that take at least `min` bytes each, checked against the bytes
    /// left before any allocation.
    fn count(&mut self, min: usize) -> Result<usize, DecodeError> {
        let n = self.u32()?;
        if n as usize > self.left() / min {
            return Err(DecodeError::Count(n));
        }
        Ok(n as usize)
    }

    fn rest(&mut self) -> &'a [u8] {
        let s = &self.b[self.at..];
        self.at = self.b.len();
        s
    }

    fn attrs(&mut self) -> Result<Attrs, DecodeError> {
        let flags = self.u32()?;
        if flags & !attr::KNOWN != 0 {
            return Err(DecodeError::AttrFlags(flags));
        }
        let mut a = Attrs::default();
        if flags & attr::SIZE != 0 {
            a.size = Some(self.u64()?);
        }
        if flags & attr::UIDGID != 0 {
            a.uid_gid = Some((self.u32()?, self.u32()?));
        }
        if flags & attr::PERMISSIONS != 0 {
            a.perms = Some(self.u32()?);
        }
        if flags & attr::ACMODTIME != 0 {
            a.times = Some((self.u32()?, self.u32()?));
        }
        if flags & attr::EXTENDED != 0 {
            // Each pair is at least two string lengths.
            let n = self.count(8)?;
            for _ in 0..n {
                let k = self.string()?;
                let v = self.string()?;
                a.extended.push((k, v));
            }
        }
        Ok(a)
    }

    fn extensions(&mut self) -> Result<Extensions, DecodeError> {
        let mut out = Vec::new();
        while self.left() > 0 {
            let k = self.string()?;
            let v = self.string()?;
            out.push((k, v));
        }
        Ok(out)
    }
}

/// Appends big-endian fields to a packet under construction.
struct Enc(Vec<u8>);

impl Enc {
    /// A packet of type `t`, with room for its length.
    fn new(t: u8) -> Enc {
        let mut v = Vec::with_capacity(64);
        v.extend_from_slice(&[0, 0, 0, 0, t]);
        Enc(v)
    }

    fn u32(&mut self, x: u32) -> &mut Enc {
        self.0.extend_from_slice(&x.to_be_bytes());
        self
    }

    fn u64(&mut self, x: u64) -> &mut Enc {
        self.0.extend_from_slice(&x.to_be_bytes());
        self
    }

    fn str(&mut self, s: &[u8]) -> &mut Enc {
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s);
        self
    }

    fn raw(&mut self, s: &[u8]) -> &mut Enc {
        self.0.extend_from_slice(s);
        self
    }

    fn attrs(&mut self, a: &Attrs) -> &mut Enc {
        let mut flags = 0;
        if a.size.is_some() {
            flags |= attr::SIZE;
        }
        if a.uid_gid.is_some() {
            flags |= attr::UIDGID;
        }
        if a.perms.is_some() {
            flags |= attr::PERMISSIONS;
        }
        if a.times.is_some() {
            flags |= attr::ACMODTIME;
        }
        if !a.extended.is_empty() {
            flags |= attr::EXTENDED;
        }
        self.u32(flags);
        if let Some(s) = a.size {
            self.u64(s);
        }
        if let Some((u, g)) = a.uid_gid {
            self.u32(u).u32(g);
        }
        if let Some(p) = a.perms {
            self.u32(p);
        }
        if let Some((at, mt)) = a.times {
            self.u32(at).u32(mt);
        }
        if !a.extended.is_empty() {
            self.u32(a.extended.len() as u32);
            for (k, v) in &a.extended {
                self.str(k).str(v);
            }
        }
        self
    }

    fn extensions(&mut self, e: &Extensions) -> &mut Enc {
        for (k, v) in e {
            self.str(k).str(v);
        }
        self
    }

    /// The finished frame, its length filled in.
    fn done(self) -> Vec<u8> {
        let mut v = self.0;
        let n = (v.len() - 4) as u32;
        v[..4].copy_from_slice(&n.to_be_bytes());
        v
    }
}

/// The frame of a `WRITE` without its data: the session sends the data slice after it, so
/// a 256 KiB block is never copied into a packet buffer.
pub fn write_header(id: u32, handle: &[u8], offset: u64, len: usize) -> Vec<u8> {
    let mut e = Enc::new(fxp::WRITE);
    e.u32(id).str(handle).u64(offset).u32(len as u32);
    let mut v = e.0;
    let n = (v.len() - 4 + len) as u32;
    v[..4].copy_from_slice(&n.to_be_bytes());
    v
}

impl Packet {
    /// The type byte.
    pub fn kind(&self) -> u8 {
        match self {
            Packet::Init { .. } => fxp::INIT,
            Packet::Version { .. } => fxp::VERSION,
            Packet::Open { .. } => fxp::OPEN,
            Packet::Close { .. } => fxp::CLOSE,
            Packet::Read { .. } => fxp::READ,
            Packet::Write { .. } => fxp::WRITE,
            Packet::Lstat { .. } => fxp::LSTAT,
            Packet::Fstat { .. } => fxp::FSTAT,
            Packet::Setstat { .. } => fxp::SETSTAT,
            Packet::Fsetstat { .. } => fxp::FSETSTAT,
            Packet::Opendir { .. } => fxp::OPENDIR,
            Packet::Readdir { .. } => fxp::READDIR,
            Packet::Remove { .. } => fxp::REMOVE,
            Packet::Mkdir { .. } => fxp::MKDIR,
            Packet::Rmdir { .. } => fxp::RMDIR,
            Packet::Realpath { .. } => fxp::REALPATH,
            Packet::Stat { .. } => fxp::STAT,
            Packet::Rename { .. } => fxp::RENAME,
            Packet::Readlink { .. } => fxp::READLINK,
            Packet::Symlink { .. } => fxp::SYMLINK,
            Packet::Status { .. } => fxp::STATUS,
            Packet::Handle { .. } => fxp::HANDLE,
            Packet::Data { .. } => fxp::DATA,
            Packet::Name { .. } => fxp::NAME,
            Packet::Attrs { .. } => fxp::ATTRS,
            Packet::Extended { .. } => fxp::EXTENDED,
            Packet::ExtendedReply { .. } => fxp::EXTENDED_REPLY,
        }
    }

    /// The request id; `None` for `INIT` and `VERSION`, which carry none.
    pub fn id(&self) -> Option<u32> {
        match self {
            Packet::Init { .. } | Packet::Version { .. } => None,
            Packet::Open { id, .. }
            | Packet::Close { id, .. }
            | Packet::Read { id, .. }
            | Packet::Write { id, .. }
            | Packet::Lstat { id, .. }
            | Packet::Fstat { id, .. }
            | Packet::Setstat { id, .. }
            | Packet::Fsetstat { id, .. }
            | Packet::Opendir { id, .. }
            | Packet::Readdir { id, .. }
            | Packet::Remove { id, .. }
            | Packet::Mkdir { id, .. }
            | Packet::Rmdir { id, .. }
            | Packet::Realpath { id, .. }
            | Packet::Stat { id, .. }
            | Packet::Rename { id, .. }
            | Packet::Readlink { id, .. }
            | Packet::Symlink { id, .. }
            | Packet::Status { id, .. }
            | Packet::Handle { id, .. }
            | Packet::Data { id, .. }
            | Packet::Name { id, .. }
            | Packet::Attrs { id, .. }
            | Packet::Extended { id, .. }
            | Packet::ExtendedReply { id, .. } => Some(*id),
        }
    }

    /// Whether a server sends this type after `VERSION`.
    pub fn is_reply(&self) -> bool {
        matches!(
            self,
            Packet::Status { .. }
                | Packet::Handle { .. }
                | Packet::Data { .. }
                | Packet::Name { .. }
                | Packet::Attrs { .. }
                | Packet::ExtendedReply { .. }
        )
    }

    /// The whole frame, length included.
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::new(self.kind());
        match self {
            Packet::Init {
                version,
                extensions,
            }
            | Packet::Version {
                version,
                extensions,
            } => {
                e.u32(*version).extensions(extensions);
            }
            Packet::Open {
                id,
                path,
                pflags,
                attrs,
            } => {
                e.u32(*id).str(path).u32(*pflags).attrs(attrs);
            }
            Packet::Close { id, handle }
            | Packet::Fstat { id, handle }
            | Packet::Readdir { id, handle }
            | Packet::Handle { id, handle } => {
                e.u32(*id).str(handle);
            }
            Packet::Read {
                id,
                handle,
                offset,
                len,
            } => {
                e.u32(*id).str(handle).u64(*offset).u32(*len);
            }
            Packet::Write {
                id,
                handle,
                offset,
                data,
            } => {
                e.u32(*id).str(handle).u64(*offset).str(data);
            }
            Packet::Lstat { id, path }
            | Packet::Opendir { id, path }
            | Packet::Remove { id, path }
            | Packet::Rmdir { id, path }
            | Packet::Realpath { id, path }
            | Packet::Stat { id, path }
            | Packet::Readlink { id, path } => {
                e.u32(*id).str(path);
            }
            Packet::Setstat { id, path, attrs } | Packet::Mkdir { id, path, attrs } => {
                e.u32(*id).str(path).attrs(attrs);
            }
            Packet::Fsetstat { id, handle, attrs } => {
                e.u32(*id).str(handle).attrs(attrs);
            }
            Packet::Rename { id, from, to } => {
                e.u32(*id).str(from).str(to);
            }
            Packet::Symlink { id, link, target } => {
                e.u32(*id).str(target).str(link);
            }
            Packet::Status {
                id,
                code,
                message,
                lang,
            } => {
                e.u32(*id).u32(*code).str(message).str(lang);
            }
            Packet::Data { id, data } => {
                e.u32(*id).str(data);
            }
            Packet::Name { id, names } => {
                e.u32(*id).u32(names.len() as u32);
                for n in names {
                    e.str(&n.filename).str(&n.longname).attrs(&n.attrs);
                }
            }
            Packet::Attrs { id, attrs } => {
                e.u32(*id).attrs(attrs);
            }
            Packet::Extended { id, name, data } => {
                e.u32(*id).str(name).raw(data);
            }
            Packet::ExtendedReply { id, data } => {
                e.u32(*id).raw(data);
            }
        }
        e.done()
    }

    /// Decodes one packet from its frame (the type byte onward, without the length).
    /// Takes the frame so that `DATA` and `WRITE` keep their bytes in place.
    pub fn decode(frame: Vec<u8>) -> Result<Packet, DecodeError> {
        let mut c = Cur { b: &frame, at: 0 };
        let t = c.u8().map_err(|_| DecodeError::Empty)?;
        let p = match t {
            fxp::INIT | fxp::VERSION => {
                let version = c.u32()?;
                let extensions = c.extensions()?;
                if t == fxp::INIT {
                    Packet::Init {
                        version,
                        extensions,
                    }
                } else {
                    Packet::Version {
                        version,
                        extensions,
                    }
                }
            }
            fxp::OPEN => Packet::Open {
                id: c.u32()?,
                path: c.string()?,
                pflags: c.u32()?,
                attrs: c.attrs()?,
            },
            fxp::CLOSE => Packet::Close {
                id: c.u32()?,
                handle: c.string()?,
            },
            fxp::READ => Packet::Read {
                id: c.u32()?,
                handle: c.string()?,
                offset: c.u64()?,
                len: c.u32()?,
            },
            fxp::WRITE => {
                let id = c.u32()?;
                let handle = c.string()?;
                let offset = c.u64()?;
                let n = c.u32()? as usize;
                if n > c.left() {
                    return Err(DecodeError::Truncated);
                }
                let start = c.at;
                return Ok(Packet::Write {
                    id,
                    handle,
                    offset,
                    data: Payload::slice(frame, start, n),
                });
            }
            fxp::LSTAT => Packet::Lstat {
                id: c.u32()?,
                path: c.string()?,
            },
            fxp::FSTAT => Packet::Fstat {
                id: c.u32()?,
                handle: c.string()?,
            },
            fxp::SETSTAT => Packet::Setstat {
                id: c.u32()?,
                path: c.string()?,
                attrs: c.attrs()?,
            },
            fxp::FSETSTAT => Packet::Fsetstat {
                id: c.u32()?,
                handle: c.string()?,
                attrs: c.attrs()?,
            },
            fxp::OPENDIR => Packet::Opendir {
                id: c.u32()?,
                path: c.string()?,
            },
            fxp::READDIR => Packet::Readdir {
                id: c.u32()?,
                handle: c.string()?,
            },
            fxp::REMOVE => Packet::Remove {
                id: c.u32()?,
                path: c.string()?,
            },
            fxp::MKDIR => Packet::Mkdir {
                id: c.u32()?,
                path: c.string()?,
                attrs: c.attrs()?,
            },
            fxp::RMDIR => Packet::Rmdir {
                id: c.u32()?,
                path: c.string()?,
            },
            fxp::REALPATH => Packet::Realpath {
                id: c.u32()?,
                path: c.string()?,
            },
            fxp::STAT => Packet::Stat {
                id: c.u32()?,
                path: c.string()?,
            },
            fxp::RENAME => Packet::Rename {
                id: c.u32()?,
                from: c.string()?,
                to: c.string()?,
            },
            fxp::READLINK => Packet::Readlink {
                id: c.u32()?,
                path: c.string()?,
            },
            fxp::SYMLINK => {
                let id = c.u32()?;
                let target = c.string()?;
                let link = c.string()?;
                Packet::Symlink { id, link, target }
            }
            fxp::STATUS => {
                let id = c.u32()?;
                let code = c.u32()?;
                // Servers older than the draft end the packet after the code.
                let (message, lang) = if c.left() == 0 {
                    (Vec::new(), Vec::new())
                } else {
                    let m = c.string()?;
                    let l = if c.left() == 0 {
                        Vec::new()
                    } else {
                        c.string()?
                    };
                    (m, l)
                };
                Packet::Status {
                    id,
                    code,
                    message,
                    lang,
                }
            }
            fxp::HANDLE => Packet::Handle {
                id: c.u32()?,
                handle: c.string()?,
            },
            fxp::DATA => {
                let id = c.u32()?;
                let n = c.u32()? as usize;
                if n > c.left() {
                    return Err(DecodeError::Truncated);
                }
                let start = c.at;
                return Ok(Packet::Data {
                    id,
                    data: Payload::slice(frame, start, n),
                });
            }
            fxp::NAME => {
                let id = c.u32()?;
                // Each name is at least two string lengths and the attribute flags.
                let n = c.count(12)?;
                let mut names = Vec::new();
                for _ in 0..n {
                    names.push(Name {
                        filename: c.string()?,
                        longname: c.string()?,
                        attrs: c.attrs()?,
                    });
                }
                Packet::Name { id, names }
            }
            fxp::ATTRS => Packet::Attrs {
                id: c.u32()?,
                attrs: c.attrs()?,
            },
            fxp::EXTENDED => Packet::Extended {
                id: c.u32()?,
                name: c.string()?,
                data: c.rest().to_vec(),
            },
            fxp::EXTENDED_REPLY => Packet::ExtendedReply {
                id: c.u32()?,
                data: c.rest().to_vec(),
            },
            t => return Err(DecodeError::UnknownType(t)),
        };
        Ok(p)
    }
}

impl Payload {
    /// `len` bytes of `frame` from `start`; the rest of the frame is dropped from view.
    fn slice(mut frame: Vec<u8>, start: usize, len: usize) -> Payload {
        frame.truncate(start + len);
        Payload { buf: frame, start }
    }
}

/// Reads one frame and returns its body (the type byte onward). `Ok(None)`: the stream
/// ended between frames. The length is checked against [`MAX_PACKET`] before the buffer is
/// allocated, and the buffer holds exactly that frame.
pub fn read_frame(r: &mut dyn Read) -> Result<Option<Vec<u8>>, FrameError> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut len[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(FrameError::Decode(DecodeError::TruncatedFrame)),
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(FrameError::Io(e)),
        }
    }
    let n = u32::from_be_bytes(len);
    if n as usize > MAX_PACKET {
        return Err(FrameError::Decode(DecodeError::TooLong(n)));
    }
    if n == 0 {
        return Err(FrameError::Decode(DecodeError::Empty));
    }
    let mut body = Vec::with_capacity(n as usize);
    match r.take(u64::from(n)).read_to_end(&mut body) {
        Ok(_) if body.len() == n as usize => Ok(Some(body)),
        Ok(_) => Err(FrameError::Decode(DecodeError::TruncatedFrame)),
        Err(e) => Err(FrameError::Io(e)),
    }
}

/// A frame that could not be read.
#[derive(Debug)]
pub enum FrameError {
    Io(std::io::Error),
    Decode(DecodeError),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::Io(e) => write!(f, "{e}"),
            FrameError::Decode(e) => write!(f, "protocol error: {e}"),
        }
    }
}

/// The reply of `limits@openssh.com`: what the server accepts (P3 5.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    pub packet: u64,
    pub read: u64,
    pub write: u64,
    pub handles: u64,
}

impl Limits {
    pub fn parse(b: &[u8]) -> Result<Limits, DecodeError> {
        let mut c = Cur { b, at: 0 };
        Ok(Limits {
            packet: c.u64()?,
            read: c.u64()?,
            write: c.u64()?,
            handles: c.u64()?,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(32);
        for x in [self.packet, self.read, self.write, self.handles] {
            v.extend_from_slice(&x.to_be_bytes());
        }
        v
    }
}

/// The reply of `statvfs@openssh.com` (P3 5.4): the fields of `statvfs(3)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatVfs {
    pub bsize: u64,
    pub frsize: u64,
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub ffree: u64,
    pub favail: u64,
    pub fsid: u64,
    pub flag: u64,
    pub namemax: u64,
}

impl StatVfs {
    pub fn parse(b: &[u8]) -> Result<StatVfs, DecodeError> {
        let mut c = Cur { b, at: 0 };
        Ok(StatVfs {
            bsize: c.u64()?,
            frsize: c.u64()?,
            blocks: c.u64()?,
            bfree: c.u64()?,
            bavail: c.u64()?,
            files: c.u64()?,
            ffree: c.u64()?,
            favail: c.u64()?,
            fsid: c.u64()?,
            flag: c.u64()?,
            namemax: c.u64()?,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(88);
        for x in [
            self.bsize,
            self.frsize,
            self.blocks,
            self.bfree,
            self.bavail,
            self.files,
            self.ffree,
            self.favail,
            self.fsid,
            self.flag,
            self.namemax,
        ] {
            v.extend_from_slice(&x.to_be_bytes());
        }
        v
    }

    /// Bytes available to an unprivileged user.
    pub fn available(&self) -> u64 {
        self.bavail.saturating_mul(self.frsize.max(1))
    }
}

/// The arguments of an extension request that takes strings: each one length-prefixed.
pub fn ext_args(args: &[&[u8]]) -> Vec<u8> {
    let mut v = Vec::new();
    for a in args {
        v.extend_from_slice(&(a.len() as u32).to_be_bytes());
        v.extend_from_slice(a);
    }
    v
}

/// The text of a status code, for messages.
pub fn status_text(code: u32) -> &'static str {
    match code {
        status::OK => "ok",
        status::EOF => "end of file",
        status::NO_SUCH_FILE => "no such file",
        status::PERMISSION_DENIED => "permission denied",
        status::FAILURE => "failure",
        status::BAD_MESSAGE => "bad message",
        status::NO_CONNECTION => "no connection",
        status::CONNECTION_LOST => "connection lost",
        status::OP_UNSUPPORTED => "not supported by the server",
        _ => "unknown error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(frame: &[u8]) -> Vec<u8> {
        frame[4..].to_vec()
    }

    #[test]
    fn init_and_version_carry_no_request_id() {
        let init = Packet::Init {
            version: VERSION,
            extensions: Vec::new(),
        };
        assert_eq!(init.encode(), [0, 0, 0, 5, fxp::INIT, 0, 0, 0, 3]);
        assert_eq!(init.id(), None);
        let v = Packet::Version {
            version: 3,
            extensions: vec![(b"a".to_vec(), b"1".to_vec())],
        };
        let f = v.encode();
        assert_eq!(&f[4..10], &[fxp::VERSION, 0, 0, 0, 3, 0]);
        assert_eq!(Packet::decode(body(&f)).unwrap(), v);
    }

    #[test]
    fn strings_and_counts_are_checked_before_allocation() {
        // A NAME that claims u32::MAX entries in a 20-byte packet.
        let mut f = vec![fxp::NAME, 0, 0, 0, 7];
        f.extend_from_slice(&u32::MAX.to_be_bytes());
        f.extend_from_slice(&[0; 11]);
        assert_eq!(Packet::decode(f), Err(DecodeError::Count(u32::MAX)));
        // A HANDLE whose string runs past the end.
        let mut f = vec![fxp::HANDLE, 0, 0, 0, 1];
        f.extend_from_slice(&1000u32.to_be_bytes());
        f.extend_from_slice(b"abc");
        assert_eq!(Packet::decode(f), Err(DecodeError::Truncated));
        // DATA shorter than its length.
        let mut f = vec![fxp::DATA, 0, 0, 0, 1];
        f.extend_from_slice(&10u32.to_be_bytes());
        assert_eq!(Packet::decode(f), Err(DecodeError::Truncated));
        assert_eq!(Packet::decode(vec![]), Err(DecodeError::Empty));
        assert_eq!(Packet::decode(vec![99]), Err(DecodeError::UnknownType(99)));
        let mut f = vec![fxp::ATTRS, 0, 0, 0, 1];
        f.extend_from_slice(&0x40u32.to_be_bytes());
        assert_eq!(Packet::decode(f), Err(DecodeError::AttrFlags(0x40)));
    }

    #[test]
    fn frames_are_bounded_before_the_buffer_exists() {
        let mut r: &[u8] = &[0x00, 0x10, 0x00, 0x00, 1, 2, 3];
        match read_frame(&mut r) {
            Err(FrameError::Decode(DecodeError::TooLong(0x0010_0000))) => {}
            other => panic!("{other:?}"),
        }
        let mut r: &[u8] = &[0, 0, 0, 9, fxp::DATA, 0, 0];
        assert!(matches!(
            read_frame(&mut r),
            Err(FrameError::Decode(DecodeError::TruncatedFrame))
        ));
        let mut r: &[u8] = &[];
        assert!(matches!(read_frame(&mut r), Ok(None)));
        let mut r: &[u8] = &[0, 0];
        assert!(matches!(
            read_frame(&mut r),
            Err(FrameError::Decode(DecodeError::TruncatedFrame))
        ));
    }

    #[test]
    fn symlink_takes_the_target_first_on_the_wire() {
        let p = Packet::Symlink {
            id: 1,
            link: b"L".to_vec(),
            target: b"T".to_vec(),
        };
        let f = p.encode();
        assert_eq!(&f[9..], &[0, 0, 0, 1, b'T', 0, 0, 0, 1, b'L']);
        assert_eq!(Packet::decode(body(&f)).unwrap(), p);
    }

    #[test]
    fn a_write_header_frames_the_data_that_follows() {
        let h = write_header(9, b"hh", 5, 3);
        let mut f = h.clone();
        f.extend_from_slice(b"xyz");
        let p = Packet::Write {
            id: 9,
            handle: b"hh".to_vec(),
            offset: 5,
            data: b"xyz".to_vec().into(),
        };
        assert_eq!(f, p.encode());
    }

    #[test]
    fn status_without_message_decodes() {
        let f = vec![fxp::STATUS, 0, 0, 0, 4, 0, 0, 0, 1];
        assert_eq!(
            Packet::decode(f).unwrap(),
            Packet::Status {
                id: 4,
                code: status::EOF,
                message: Vec::new(),
                lang: Vec::new()
            }
        );
    }
}
