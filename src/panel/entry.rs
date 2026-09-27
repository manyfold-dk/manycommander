#![forbid(unsafe_code)]
//! Compact entry storage (design 13.1): each panel stores its entries once, as name bytes
//! in one arena plus a small fixed metadata struct.

use crate::fsops::sys::{Kind, Meta};

/// What a symlink points at, from the second listing pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum LinkKind {
    /// Not classified yet (or not a symlink).
    #[default]
    Unknown,
    File,
    Dir,
    Broken,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum EKind {
    File,
    Dir,
    Symlink,
    Special,
}

pub const MARKED: u8 = 1;
pub const HIDDEN: u8 = 2;
pub const EXEC: u8 = 4;
/// `size` of a directory holds its computed size.
pub const SIZED: u8 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name_off: u32,
    pub name_len: u16,
    pub kind: EKind,
    pub link: LinkKind,
    pub flags: u8,
    pub perm: u16,
    pub size: u64,
    pub mtime: i64,
    pub mtime_ns: u32,
}

impl Entry {
    /// An entry for `name` appended to `names`.
    pub fn new(names: &mut Vec<u8>, name: &[u8], m: &Meta) -> Entry {
        let off = names.len() as u32;
        let name = &name[..name.len().min(u16::MAX as usize)];
        names.extend_from_slice(name);
        let kind = match m.kind {
            Kind::File => EKind::File,
            Kind::Dir => EKind::Dir,
            Kind::Symlink => EKind::Symlink,
            _ => EKind::Special,
        };
        let mut flags = 0;
        if name.first() == Some(&b'.') {
            flags |= HIDDEN;
        }
        if kind == EKind::File && m.perm & 0o111 != 0 {
            flags |= EXEC;
        }
        Entry {
            name_off: off,
            name_len: name.len() as u16,
            kind,
            link: LinkKind::Unknown,
            flags,
            perm: (m.perm & 0o7777) as u16,
            size: if kind == EKind::Dir { 0 } else { m.size },
            mtime: m.mtime.sec,
            mtime_ns: m.mtime.nsec,
        }
    }

    pub fn name<'a>(&self, names: &'a [u8]) -> &'a [u8] {
        &names[self.name_off as usize..self.name_off as usize + self.name_len as usize]
    }

    /// A directory, or a symlink to one: sorted with directories, entered by Enter.
    pub fn is_dir_like(&self) -> bool {
        self.kind == EKind::Dir || (self.kind == EKind::Symlink && self.link == LinkKind::Dir)
    }

    pub fn marked(&self) -> bool {
        self.flags & MARKED != 0
    }

    pub fn hidden(&self) -> bool {
        self.flags & HIDDEN != 0
    }

    pub fn exec(&self) -> bool {
        self.flags & EXEC != 0
    }

    /// The extension: the bytes after the last `.`, if that dot is not the first byte.
    pub fn ext<'a>(&self, names: &'a [u8]) -> &'a [u8] {
        let n = self.name(names);
        if self.kind == EKind::Dir {
            return &[];
        }
        match n.iter().rposition(|&c| c == b'.') {
            Some(i) if i > 0 => &n[i + 1..],
            _ => &[],
        }
    }
}

/// `drwxr-xr-x`-style mode text.
pub fn mode_string(e: &Entry) -> String {
    let t = match e.kind {
        EKind::Dir => 'd',
        EKind::Symlink => 'l',
        EKind::Special => 's',
        EKind::File => '-',
    };
    let p = e.perm as u32;
    let mut s = String::with_capacity(10);
    s.push(t);
    for (i, c) in ['r', 'w', 'x', 'r', 'w', 'x', 'r', 'w', 'x']
        .iter()
        .enumerate()
    {
        let bit = 1 << (8 - i);
        s.push(if p & bit != 0 { *c } else { '-' });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_is_small() {
        assert!(
            std::mem::size_of::<Entry>() <= 40,
            "{}",
            std::mem::size_of::<Entry>()
        );
    }
}
