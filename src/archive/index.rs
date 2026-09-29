#![forbid(unsafe_code)]
//! The index of one archive (P3 3.2): one node per entry, with its parent, name (in an
//! arena), kind, mode, size, mtime, link target and locator.
//!
//! Building the index applies A-1 and the rules of P3 3.2. A member name is data, never a
//! path: it is split on `/`, empty and `.` components are dropped, a leading `/` is dropped
//! and counted, and a member with a `..` component, a NUL, or a component that fails
//! `valid_component` is skipped as "unsafe path", as is a member whose parent path names a
//! non-directory member. Implicit directories are synthesized (mode `0o755`, no time). The
//! last of duplicate members wins, as in tar, except that a member of another kind never
//! replaces a directory with children ("conflicting member"). A hard link's target is
//! resolved only as an index path with A-1's rules: an absolute target, or one that fails
//! them, names no node.
//!
//! While a scan runs the tree is the scan thread's alone; the index publishes it once
//! complete ([`Tree::finish`]), and listings, lookups and sizes then read it from memory
//! (P3 3.3). The node of a parent is always created before its children, so node ids grow
//! from the root outwards.

use crate::fsops::plan::valid_component;
use crate::fsops::sys::{Kind, Meta, Ts};
use crate::panel::entry::{Entry, LinkKind, NOTIME};
use crate::provider::VPath;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::hash::{BuildHasher, BuildHasherDefault, Hasher, RandomState};
use std::os::unix::ffi::OsStrExt;

/// A node of the tree; [`ROOT`] is the archive root.
pub type NodeId = u32;
pub const ROOT: NodeId = 0;
const NONE: u32 = u32::MAX;

/// A listing stops at this many entries (P3 2.6, A-4).
pub const MAX_ENTRIES: usize = 1_000_000;
/// An index never grows beyond this many bytes (P3 2.6): the cache bound, for one index.
pub const MAX_INDEX_BYTES: usize = 128 << 20;

/// Why a member is not in the index (A-1, P3 3.2).
pub const UNSAFE_PATH: &str = "unsafe path";
pub const CONFLICTING: &str = "conflicting member";
pub const SPARSE: &str = "sparse member";
pub const UNSUPPORTED: &str = "unsupported member type";
pub const DAMAGED_MEMBER: &str = "damaged member";
pub const LINK_TOO_LONG: &str = "link target too long";

/// A symlink is followed at most this many times while it is resolved, as the kernel does.
const MAX_HOPS: u32 = 40;

/// The kind of a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeKind {
    Dir,
    File,
    Symlink,
    /// A tar hard link: its target is an earlier node, or none (A-3).
    HardLink,
    /// A device, FIFO or socket member: listed, never extracted (A-3).
    Special,
}

/// A synthesized directory: no member of its own, no time.
pub const IMPLICIT: u8 = 1;
/// An encrypted zip entry: listed, never extracted (P3 3.2).
pub const ENCRYPTED: u8 = 2;
/// The member carries no time.
pub const NO_TIME: u8 = 4;

#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub parent: NodeId,
    /// While building: the first child. Finished: the first index in `kids`.
    first: u32,
    /// While building: the next sibling. Finished: the number of children.
    next: u32,
    name_off: u32,
    name_len: u32,
    pub kind: NodeKind,
    pub flags: u8,
    /// Permission bits as the member stores them (`0o7777`); extraction masks them (A-3).
    pub mode: u16,
    pub mtime_ns: u32,
    pub mtime: i64,
    /// A file's size. A directory's total of the regular files below it, once finished.
    pub size: u64,
    /// A zip entry index or a tar data offset in the decompressed stream (P3 3.2).
    pub locator: u64,
    /// A symlink's index into the link targets; a hard link's target node or `NONE`.
    link: u32,
}

/// One member as a reader sees it, before A-1.
#[derive(Clone, Copy, Debug)]
pub struct Member<'a> {
    pub name: &'a [u8],
    pub kind: MemberKind<'a>,
    pub mode: u32,
    pub size: u64,
    pub mtime: Option<Ts>,
    pub locator: u64,
    pub encrypted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberKind<'a> {
    Dir,
    File,
    /// The link's target text.
    Symlink(&'a [u8]),
    /// The target member's name.
    HardLink(&'a [u8]),
    Special,
}

/// What [`Tree::add`] did with a member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Added {
    New(NodeId),
    /// A later duplicate replaced the node (the last wins).
    Replaced(NodeId),
    Skipped,
}

/// The index reached a bound (P3 2.6); the text is the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Full(pub String);

/// The bounds of one index (P3 2.6); tests lower them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub entries: usize,
    pub bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            entries: MAX_ENTRIES,
            bytes: MAX_INDEX_BYTES,
        }
    }
}

impl Limits {
    pub fn entries_message(&self) -> String {
        format!(
            "listing stopped at {} entries",
            thousands(self.entries as u64)
        )
    }

    pub fn bytes_message(&self) -> String {
        format!("listing stopped: the index reached {} MB", self.bytes >> 20)
    }
}

/// `1000000` as `1,000,000`.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What the footer says about the whole archive (P3 3.3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Nodes that stand for a member (not synthesized, not the root).
    pub members: u64,
    /// The unpacked total: the sizes of the regular files.
    pub bytes: u64,
    /// Members whose leading `/` was dropped (A-1).
    pub leading_slash: u64,
    /// Members not in the index, by reason.
    pub skipped: Vec<(&'static str, u64)>,
}

impl Stats {
    pub fn skipped_total(&self) -> u64 {
        self.skipped.iter().map(|(_, n)| n).sum()
    }

    pub fn skipped(&self, why: &str) -> u64 {
        self.skipped
            .iter()
            .find(|(w, _)| *w == why)
            .map_or(0, |(_, n)| *n)
    }

    /// "3 members not shown: unsafe path", or with several reasons
    /// "5 members not shown: 3 unsafe path, 2 sparse member".
    pub fn skipped_text(&self) -> Option<String> {
        let total = self.skipped_total();
        if total == 0 {
            return None;
        }
        let noun = if total == 1 { "member" } else { "members" };
        let why = match &self.skipped[..] {
            [(w, _)] => w.to_string(),
            many => many
                .iter()
                .map(|(w, n)| format!("{n} {w}"))
                .collect::<Vec<_>>()
                .join(", "),
        };
        Some(format!("{total} {noun} not shown: {why}"))
    }
}

/// A hasher for keys that already are hashes.
#[derive(Default)]
struct Pass(u64);

impl Hasher for Pass {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = self.0.rotate_left(8) ^ b as u64;
        }
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
}

/// The tree of an archive (P3 3.2).
pub struct Tree {
    nodes: Vec<Node>,
    names: Vec<u8>,
    links: Vec<Box<[u8]>>,
    links_bytes: usize,
    /// Finished: the children of each directory, grouped by parent and sorted by name.
    kids: Vec<NodeId>,
    /// While building: `hash(parent, name)` to a node with that name, for the lookups of
    /// every member; a collision falls back to the parent's sibling chain.
    map: HashMap<u64, NodeId, BuildHasherDefault<Pass>>,
    hasher: RandomState,
    /// The directories the last [`Tree::add`] synthesized, for the scan's watch.
    implicit: Vec<NodeId>,
    finished: bool,
    limits: Limits,
    pub stats: Stats,
}

impl std::fmt::Debug for Tree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tree")
            .field("nodes", &self.nodes.len())
            .field("finished", &self.finished)
            .finish()
    }
}

impl Default for Tree {
    fn default() -> Self {
        Tree::new(Limits::default())
    }
}

/// The components of a member name under A-1, or `None` when it is unsafe. The same rules
/// as [`VPath::parse`], without its allocations: this runs once per member.
fn components(name: &[u8], out: &mut Vec<(usize, usize)>) -> Option<()> {
    out.clear();
    let mut next = 0;
    for c in name.split(|&b| b == b'/') {
        let at = next;
        next += c.len() + 1;
        if c.is_empty() || c == b"." {
            continue;
        }
        if !valid_component(OsStr::from_bytes(c)) {
            return None;
        }
        out.push((at, c.len()));
    }
    Some(())
}

impl Tree {
    pub fn new(limits: Limits) -> Tree {
        let mut t = Tree {
            nodes: Vec::new(),
            names: Vec::new(),
            links: Vec::new(),
            links_bytes: 0,
            kids: Vec::new(),
            map: HashMap::default(),
            hasher: RandomState::new(),
            implicit: Vec::new(),
            finished: false,
            limits,
            stats: Stats::default(),
        };
        t.nodes.push(Node {
            parent: ROOT,
            first: NONE,
            next: NONE,
            name_off: 0,
            name_len: 0,
            kind: NodeKind::Dir,
            flags: IMPLICIT | NO_TIME,
            mode: 0o755,
            mtime_ns: 0,
            mtime: 0,
            size: 0,
            locator: 0,
            link: NONE,
        });
        t
    }

    /// The number of nodes, the root included.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// The bounds this tree stops at.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// The memory the index holds, approximately (P3 2.6).
    pub fn bytes(&self) -> usize {
        self.nodes.capacity() * std::mem::size_of::<Node>()
            + self.names.capacity()
            + self.links.capacity() * std::mem::size_of::<Box<[u8]>>()
            + self.links_bytes
            + self.kids.capacity() * 4
            + self.map.capacity() * 13
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    pub fn name(&self, id: NodeId) -> &[u8] {
        let n = &self.nodes[id as usize];
        &self.names[n.name_off as usize..(n.name_off + n.name_len) as usize]
    }

    /// A symlink's target text.
    pub fn link_target(&self, id: NodeId) -> Option<&[u8]> {
        let n = &self.nodes[id as usize];
        (n.kind == NodeKind::Symlink && n.link != NONE).then(|| &*self.links[n.link as usize])
    }

    /// A hard link's target node (A-3): `None` when its target names no node.
    pub fn hard_target(&self, id: NodeId) -> Option<NodeId> {
        let n = &self.nodes[id as usize];
        (n.kind == NodeKind::HardLink && n.link != NONE).then_some(n.link)
    }

    fn key(&self, parent: NodeId, name: &[u8]) -> u64 {
        self.hasher.hash_one((parent, name))
    }

    /// The child `name` of directory `dir`.
    pub fn child(&self, dir: NodeId, name: &[u8]) -> Option<NodeId> {
        if self.finished {
            let d = &self.nodes[dir as usize];
            if d.kind != NodeKind::Dir {
                return None;
            }
            let range = &self.kids[d.first as usize..(d.first + d.next) as usize];
            return range
                .binary_search_by(|&k| self.name(k).cmp(name))
                .ok()
                .map(|i| range[i]);
        }
        match self.map.get(&self.key(dir, name)) {
            Some(&id) if self.nodes[id as usize].parent == dir && self.name(id) == name => Some(id),
            // A hash collision: the sibling chain decides.
            Some(_) => self.children(dir).find(|&k| self.name(k) == name),
            None => None,
        }
    }

    /// The children of `dir`, in no particular order.
    pub fn children(&self, dir: NodeId) -> Children<'_> {
        let d = &self.nodes[dir as usize];
        if self.finished {
            let range = if d.kind == NodeKind::Dir {
                &self.kids[d.first as usize..(d.first + d.next) as usize]
            } else {
                &[][..]
            };
            Children::Slice(range.iter())
        } else {
            Children::Chain {
                tree: self,
                at: d.first,
            }
        }
    }

    fn has_children(&self, dir: NodeId) -> bool {
        self.children(dir).next().is_some()
    }

    /// The node at `path`, without following symlinks.
    pub fn lookup(&self, path: &VPath) -> Option<NodeId> {
        let mut at = ROOT;
        for c in path.components() {
            at = self.child(at, c.as_bytes())?;
        }
        Some(at)
    }

    /// The path of a node.
    pub fn path_of(&self, mut id: NodeId) -> VPath {
        let mut parts = Vec::new();
        while id != ROOT {
            parts.push(OsStr::from_bytes(self.name(id)).to_owned());
            id = self.nodes[id as usize].parent;
        }
        parts.reverse();
        // Every stored name passed `valid_component`.
        VPath::new(parts).unwrap_or_default()
    }

    /// The directories the last [`Tree::add`] synthesized on the way to its member, parents
    /// first: a scan shows them as they appear (P3 3.3).
    pub fn synthesized(&self) -> &[NodeId] {
        &self.implicit
    }

    /// Counts a member that is not in the index.
    pub fn skip(&mut self, why: &'static str) {
        match self.stats.skipped.iter_mut().find(|(w, _)| *w == why) {
            Some((_, n)) => *n += 1,
            None => self.stats.skipped.push((why, 1)),
        }
    }

    fn push(&mut self, parent: NodeId, name: &[u8], mut node: Node) -> Result<NodeId, Full> {
        if self.nodes.len() >= self.limits.entries {
            return Err(Full(self.limits.entries_message()));
        }
        if self.bytes() + std::mem::size_of::<Node>() + name.len() + 16 > self.limits.bytes {
            return Err(Full(self.limits.bytes_message()));
        }
        let id = self.nodes.len() as NodeId;
        node.parent = parent;
        node.name_off = self.names.len() as u32;
        node.name_len = name.len() as u32;
        node.first = NONE;
        let p = &mut self.nodes[parent as usize];
        node.next = p.first;
        p.first = id;
        self.names.extend_from_slice(name);
        self.nodes.push(node);
        let key = self.key(parent, name);
        self.map.entry(key).or_insert(id);
        Ok(id)
    }

    /// The node a hard link names (A-3, P3 3.2): an absolute target, a `..` or any other
    /// component that fails A-1 names none; a directory is never a link target. A link to
    /// another link names that link's target.
    fn hard_link_target(&self, target: &[u8], scratch: &mut Vec<(usize, usize)>) -> u32 {
        if target.first() == Some(&b'/') || components(target, scratch).is_none() {
            return NONE;
        }
        let mut at = ROOT;
        for &(o, l) in scratch.iter() {
            match self.child(at, &target[o..o + l]) {
                Some(id) => at = id,
                None => return NONE,
            }
        }
        let n = &self.nodes[at as usize];
        match n.kind {
            NodeKind::Dir => NONE,
            NodeKind::HardLink => n.link,
            _ => at,
        }
    }

    fn store_link(&mut self, target: &[u8]) -> u32 {
        self.links_bytes += target.len();
        self.links.push(target.into());
        (self.links.len() - 1) as u32
    }

    /// Adds one member under A-1 (P3 3.2). `Err` when the index is full (P3 2.6): the
    /// listing stops there.
    pub fn add(&mut self, m: Member<'_>) -> Result<Added, Full> {
        self.implicit.clear();
        let mut scratch = Vec::new();
        if components(m.name, &mut scratch).is_none() {
            self.skip(UNSAFE_PATH);
            return Ok(Added::Skipped);
        }
        let leading = m.name.first() == Some(&b'/');
        let Some((&last, dirs)) = scratch.split_last() else {
            // The archive root itself ("./", "/"): a directory member gives the root its
            // metadata; anything else cannot stand there.
            if m.kind == MemberKind::Dir {
                let r = &mut self.nodes[ROOT as usize];
                r.mode = (m.mode & 0o7777) as u16;
                if let Some(t) = m.mtime {
                    r.mtime = t.sec;
                    r.mtime_ns = t.nsec;
                    r.flags &= !NO_TIME;
                }
                return Ok(Added::Skipped);
            }
            self.skip(UNSAFE_PATH);
            return Ok(Added::Skipped);
        };
        let mut dir = ROOT;
        for &(o, l) in dirs {
            let c = &m.name[o..o + l];
            match self.child(dir, c) {
                Some(id) if self.nodes[id as usize].kind == NodeKind::Dir => dir = id,
                // The parent path names a file, a symlink or a special member (A-1): the
                // shape of CVE-2025-29787.
                Some(_) => {
                    self.skip(UNSAFE_PATH);
                    return Ok(Added::Skipped);
                }
                None => {
                    let implicit = Node {
                        kind: NodeKind::Dir,
                        flags: IMPLICIT | NO_TIME,
                        mode: 0o755,
                        ..self.blank()
                    };
                    dir = self.push(dir, c, implicit)?;
                    self.implicit.push(dir);
                }
            }
        }
        let (o, l) = last;
        let name = &m.name[o..o + l];
        let kind = match m.kind {
            MemberKind::Dir => NodeKind::Dir,
            MemberKind::File => NodeKind::File,
            MemberKind::Symlink(_) => NodeKind::Symlink,
            MemberKind::HardLink(_) => NodeKind::HardLink,
            MemberKind::Special => NodeKind::Special,
        };
        let existing = self.child(dir, name);
        if let Some(id) = existing
            && self.nodes[id as usize].kind == NodeKind::Dir
            && kind != NodeKind::Dir
            && self.has_children(id)
        {
            self.skip(CONFLICTING);
            return Ok(Added::Skipped);
        }
        let link = match m.kind {
            MemberKind::Symlink(t) => self.store_link(t),
            MemberKind::HardLink(t) => self.hard_link_target(t, &mut scratch),
            _ => NONE,
        };
        let mut flags = 0;
        if m.encrypted {
            flags |= ENCRYPTED;
        }
        let (mtime, mtime_ns) = match m.mtime {
            Some(t) => (t.sec, t.nsec),
            None => {
                flags |= NO_TIME;
                (0, 0)
            }
        };
        let fields = Node {
            kind,
            flags,
            mode: (m.mode & 0o7777) as u16,
            mtime,
            mtime_ns,
            size: if kind == NodeKind::File { m.size } else { 0 },
            locator: m.locator,
            link,
            ..self.blank()
        };
        if leading {
            self.stats.leading_slash += 1;
        }
        match existing {
            Some(id) => {
                // The last of duplicate members wins (P3 3.2); the node keeps its place
                // and its children.
                let n = &mut self.nodes[id as usize];
                let was_member = n.flags & IMPLICIT == 0;
                n.kind = fields.kind;
                n.flags = fields.flags;
                n.mode = fields.mode;
                n.mtime = fields.mtime;
                n.mtime_ns = fields.mtime_ns;
                n.size = fields.size;
                n.locator = fields.locator;
                n.link = fields.link;
                if !was_member {
                    self.stats.members += 1;
                }
                Ok(Added::Replaced(id))
            }
            None => {
                let id = self.push(dir, name, fields)?;
                self.stats.members += 1;
                Ok(Added::New(id))
            }
        }
    }

    fn blank(&self) -> Node {
        Node {
            parent: ROOT,
            first: NONE,
            next: NONE,
            name_off: 0,
            name_len: 0,
            kind: NodeKind::File,
            flags: 0,
            mode: 0,
            mtime_ns: 0,
            mtime: 0,
            size: 0,
            locator: 0,
            link: NONE,
        }
    }

    /// Completes the index: directory totals, the unpacked total, and the sorted child
    /// ranges that lookups search; the build-time map goes.
    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        let n = self.nodes.len();
        for node in &mut self.nodes {
            if node.kind == NodeKind::Dir {
                node.size = 0;
            }
        }
        let mut total = 0u64;
        for id in (1..n).rev() {
            let (kind, size, parent) = {
                let node = &self.nodes[id];
                (node.kind, node.size, node.parent as usize)
            };
            match kind {
                NodeKind::File => {
                    total = total.saturating_add(size);
                    let p = &mut self.nodes[parent];
                    p.size = p.size.saturating_add(size);
                }
                NodeKind::Dir => {
                    let p = &mut self.nodes[parent];
                    p.size = p.size.saturating_add(size);
                }
                _ => {}
            }
        }
        self.stats.bytes = total;
        let mut count = vec![0u32; n];
        for node in &self.nodes[1..] {
            count[node.parent as usize] += 1;
        }
        let mut start = vec![0u32; n];
        let mut sum = 0u32;
        for (s, c) in start.iter_mut().zip(&count) {
            *s = sum;
            sum += c;
        }
        let mut kids = vec![0u32; n.saturating_sub(1)];
        let mut fill = start.clone();
        for (id, node) in self.nodes.iter().enumerate().skip(1) {
            let p = node.parent as usize;
            kids[fill[p] as usize] = id as u32;
            fill[p] += 1;
        }
        for d in 0..n {
            let (s, c) = (start[d] as usize, count[d] as usize);
            if c > 1 {
                let (nodes, names) = (&self.nodes, &self.names);
                let name = |k: u32| {
                    let x = &nodes[k as usize];
                    &names[x.name_off as usize..(x.name_off + x.name_len) as usize]
                };
                kids[s..s + c].sort_unstable_by(|&a, &b| name(a).cmp(name(b)));
            }
            let node = &mut self.nodes[d];
            node.first = start[d];
            node.next = count[d];
        }
        self.kids = kids;
        self.map = HashMap::default();
        self.nodes.shrink_to_fit();
        self.names.shrink_to_fit();
        self.finished = true;
    }

    /// Where a symlink leads inside the archive (P3 3.2), following symlinks on the way at
    /// most [`MAX_HOPS`] times: `None` when the target is absolute, leaves the archive
    /// through `..`, or names nothing. `..` is resolved after the symlink before it, as
    /// the kernel does.
    pub fn follow(&self, id: NodeId) -> Option<NodeId> {
        let mut hops = 0;
        self.follow_from(id, &mut hops)
    }

    fn follow_from(&self, mut id: NodeId, hops: &mut u32) -> Option<NodeId> {
        while self.nodes[id as usize].kind == NodeKind::Symlink {
            *hops += 1;
            if *hops > MAX_HOPS {
                return None;
            }
            let target = self.link_target(id)?;
            if target.first() == Some(&b'/') {
                return None;
            }
            let mut cur = self.nodes[id as usize].parent;
            for c in target.split(|&b| b == b'/') {
                match c {
                    b"" | b"." => {}
                    b".." => {
                        let d = self.follow_from(cur, hops)?;
                        if d == ROOT {
                            return None;
                        }
                        cur = self.nodes[d as usize].parent;
                    }
                    name => {
                        let d = self.follow_from(cur, hops)?;
                        if self.nodes[d as usize].kind != NodeKind::Dir {
                            return None;
                        }
                        cur = self.child(d, name)?;
                    }
                }
            }
            id = cur;
        }
        Some(id)
    }

    /// What a symlink points at, for the panel's second pass (M1 3.1).
    pub fn link_kind(&self, id: NodeId) -> LinkKind {
        match self.follow(id) {
            Some(t) if self.nodes[t as usize].kind == NodeKind::Dir => LinkKind::Dir,
            Some(_) => LinkKind::File,
            None => LinkKind::Broken,
        }
    }

    /// The node's metadata as the panel and the provider see it. Its identity is left to
    /// the caller (a synthetic one, P3 2.1).
    pub fn meta(&self, id: NodeId) -> Meta {
        let n = &self.nodes[id as usize];
        let (kind, size) = match n.kind {
            NodeKind::Dir => (Kind::Dir, n.size),
            NodeKind::File => (Kind::File, n.size),
            NodeKind::Symlink => (
                Kind::Symlink,
                self.link_target(id).map_or(0, |t| t.len() as u64),
            ),
            NodeKind::HardLink => (
                Kind::File,
                self.hard_target(id)
                    .map_or(0, |t| self.nodes[t as usize].size),
            ),
            NodeKind::Special => (Kind::Unknown, 0),
        };
        let mtime = Ts {
            sec: n.mtime,
            nsec: n.mtime_ns,
        };
        Meta {
            kind,
            perm: n.mode as u32,
            nlink: 1,
            size,
            mtime,
            ctime: mtime,
            atime: mtime,
            ..Meta::default()
        }
    }

    /// The panel entry of a node, its name appended to `names`.
    pub fn entry(&self, id: NodeId, names: &mut Vec<u8>) -> Entry {
        let n = &self.nodes[id as usize];
        let mut e = Entry::new(names, self.name(id), &self.meta(id));
        if n.kind == NodeKind::Dir {
            e.size = 0;
        }
        if n.flags & NO_TIME != 0 {
            e.flags |= NOTIME;
        }
        e
    }

    /// The name of every child as an owned string, for tests and messages.
    pub fn child_names(&self, dir: NodeId) -> Vec<OsString> {
        self.children(dir)
            .map(|k| OsStr::from_bytes(self.name(k)).to_owned())
            .collect()
    }
}

/// The children of a directory (P3 3.3).
pub enum Children<'a> {
    Slice(std::slice::Iter<'a, NodeId>),
    Chain { tree: &'a Tree, at: u32 },
}

impl Iterator for Children<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        match self {
            Children::Slice(it) => it.next().copied(),
            Children::Chain { tree, at } => {
                if *at == NONE {
                    return None;
                }
                let id = *at;
                *at = tree.nodes[id as usize].next;
                Some(id)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(name: &[u8], kind: MemberKind<'static>) -> Member<'static> {
        let name: &'static [u8] = Box::leak(name.to_vec().into_boxed_slice());
        Member {
            name,
            kind,
            mode: 0o644,
            size: 3,
            mtime: Some(Ts { sec: 100, nsec: 0 }),
            locator: 0,
            encrypted: false,
        }
    }

    fn names(t: &Tree, dir: NodeId) -> Vec<String> {
        let mut v: Vec<String> = t
            .children(dir)
            .map(|k| String::from_utf8_lossy(t.name(k)).into_owned())
            .collect();
        v.sort();
        v
    }

    fn at(t: &Tree, p: &[u8]) -> Option<NodeId> {
        t.lookup(&VPath::parse(p).unwrap())
    }

    /// A-1: names are split, cleaned and checked; implicit directories appear.
    #[test]
    fn member_paths_are_data() {
        let mut t = Tree::default();
        for name in [
            &b"./a/b/c"[..],
            b"/abs/x",
            b"a//d/./e",
            b"../up",
            b"a/../b",
            b"nul\0byte",
            b"a/b",
        ] {
            let kind = if name == b"a/b" {
                MemberKind::Dir
            } else {
                MemberKind::File
            };
            t.add(m(name, kind)).unwrap();
        }
        assert_eq!(t.stats.skipped(UNSAFE_PATH), 3);
        assert_eq!(t.stats.leading_slash, 1);
        assert_eq!(names(&t, ROOT), ["a", "abs"]);
        let a = at(&t, b"a").unwrap();
        assert_eq!(t.node(a).flags & IMPLICIT, IMPLICIT, "synthesized");
        assert_eq!(names(&t, a), ["b", "d"]);
        let b = at(&t, b"a/b").unwrap();
        assert_eq!(
            t.node(b).flags & IMPLICIT,
            0,
            "a later dir member makes it explicit"
        );
        assert_eq!(names(&t, b), ["c"]);
        assert_eq!(t.stats.members, 4, "a/b/c, abs/x, a/d/e and a/b");
        t.finish();
        assert_eq!(names(&t, a), ["b", "d"]);
        assert_eq!(t.path_of(at(&t, b"a/d/e").unwrap()).to_bytes(), b"/a/d/e");
    }

    /// A-1: a member below a file or a symlink member is unsafe (CVE-2025-29787); a member
    /// of another kind never replaces a directory with children.
    #[test]
    fn parents_must_be_directories_and_conflicts_are_skipped() {
        let mut t = Tree::default();
        t.add(m(b"link", MemberKind::Symlink(b"/etc"))).unwrap();
        t.add(m(b"link/passwd", MemberKind::File)).unwrap();
        t.add(m(b"f", MemberKind::File)).unwrap();
        t.add(m(b"f/x", MemberKind::File)).unwrap();
        t.add(m(b"d/x", MemberKind::File)).unwrap();
        assert_eq!(t.add(m(b"d", MemberKind::File)).unwrap(), Added::Skipped);
        assert_eq!(t.stats.skipped(UNSAFE_PATH), 2);
        assert_eq!(t.stats.skipped(CONFLICTING), 1);
        // A childless directory may be replaced, and a symlink by a directory (the
        // RUSTSEC-2026-0067 shape lists as a directory).
        t.add(m(b"e", MemberKind::Dir)).unwrap();
        assert!(matches!(
            t.add(m(b"e", MemberKind::File)),
            Ok(Added::Replaced(_))
        ));
        assert!(matches!(
            t.add(m(b"link", MemberKind::Dir)),
            Ok(Added::Replaced(_))
        ));
        assert_eq!(t.node(at(&t, b"link").unwrap()).kind, NodeKind::Dir);
        assert_eq!(t.node(at(&t, b"e").unwrap()).kind, NodeKind::File);
    }

    /// P3 3.2: the last duplicate wins; hard links name earlier nodes only as index paths.
    #[test]
    fn duplicates_and_hard_links() {
        let mut t = Tree::default();
        let mut first = m(b"x", MemberKind::File);
        first.size = 1;
        first.locator = 10;
        t.add(first).unwrap();
        let mut second = m(b"x", MemberKind::File);
        second.size = 7;
        second.locator = 20;
        assert!(matches!(t.add(second), Ok(Added::Replaced(_))));
        let x = at(&t, b"x").unwrap();
        assert_eq!((t.node(x).size, t.node(x).locator), (7, 20));
        for (name, target) in [
            (&b"h1"[..], &b"./x"[..]),
            (b"h2", b"/x"),
            (b"h3", b"../x"),
            (b"h4", b"missing"),
            (b"h5", b"h1"),
        ] {
            let target: &'static [u8] = Box::leak(target.to_vec().into_boxed_slice());
            t.add(m(name, MemberKind::HardLink(target))).unwrap();
        }
        let h = |n: &[u8]| t.hard_target(at(&t, n).unwrap());
        assert_eq!(h(b"h1"), Some(x));
        assert_eq!(h(b"h2"), None, "an absolute target names no node");
        assert_eq!(h(b"h3"), None, "a `..` names no node");
        assert_eq!(h(b"h4"), None);
        assert_eq!(h(b"h5"), Some(x), "a link to a link names its target");
        assert_eq!(t.meta(at(&t, b"h1").unwrap()).size, 7);
        assert_eq!(t.stats.members, 6);
    }

    #[test]
    fn totals_sorted_children_and_symlinks() {
        let mut t = Tree::default();
        for (n, s) in [(&b"d/b"[..], 2), (b"d/a", 3), (b"d/e/f", 5), (b"top", 7)] {
            let mut x = m(n, MemberKind::File);
            x.size = s;
            t.add(x).unwrap();
        }
        t.add(m(b"d/l1", MemberKind::Symlink(b"e"))).unwrap();
        t.add(m(b"d/l2", MemberKind::Symlink(b"../top"))).unwrap();
        t.add(m(b"d/l3", MemberKind::Symlink(b"../../out")))
            .unwrap();
        t.add(m(b"d/l4", MemberKind::Symlink(b"/abs"))).unwrap();
        t.add(m(b"d/l5", MemberKind::Symlink(b"l6"))).unwrap();
        t.add(m(b"d/l6", MemberKind::Symlink(b"l5"))).unwrap();
        t.add(m(b"d/l7", MemberKind::Symlink(b"l1/f"))).unwrap();
        t.add(m(b"d/l8", MemberKind::Symlink(b"l1/../b"))).unwrap();
        t.finish();
        let d = at(&t, b"d").unwrap();
        assert_eq!(t.node(d).size, 10);
        assert_eq!(t.node(ROOT).size, 17);
        assert_eq!(t.stats.bytes, 17);
        let kids: Vec<Vec<u8>> = t.children(d).map(|k| t.name(k).to_vec()).collect();
        let mut sorted = kids.clone();
        sorted.sort();
        assert_eq!(kids, sorted, "finished children are sorted by name");
        assert_eq!(t.child(d, b"e"), at(&t, b"d/e"));
        assert_eq!(t.child(d, b"zz"), None);
        let k = |n: &[u8]| t.link_kind(at(&t, n).unwrap());
        assert_eq!(k(b"d/l1"), LinkKind::Dir);
        assert_eq!(k(b"d/l2"), LinkKind::File);
        assert_eq!(k(b"d/l3"), LinkKind::Broken, "leaves the archive");
        assert_eq!(k(b"d/l4"), LinkKind::Broken, "absolute");
        assert_eq!(k(b"d/l5"), LinkKind::Broken, "a loop");
        assert_eq!(k(b"d/l7"), LinkKind::File);
        assert_eq!(
            k(b"d/l8"),
            LinkKind::File,
            "`..` after a symlink is physical"
        );
    }

    #[test]
    fn limits_stop_the_listing() {
        let mut t = Tree::new(Limits {
            entries: 3,
            bytes: MAX_INDEX_BYTES,
        });
        t.add(m(b"a", MemberKind::File)).unwrap();
        t.add(m(b"b", MemberKind::File)).unwrap();
        let e = t.add(m(b"c", MemberKind::File)).unwrap_err();
        assert_eq!(e.0, "listing stopped at 3 entries");
        assert_eq!(
            Limits::default().entries_message(),
            "listing stopped at 1,000,000 entries"
        );
        let mut t = Tree::new(Limits {
            entries: MAX_ENTRIES,
            bytes: 4096,
        });
        let mut last = Ok(Added::Skipped);
        for i in 0..1000 {
            last = t.add(m(format!("f{i}").as_bytes(), MemberKind::File));
            if last.is_err() {
                break;
            }
        }
        assert!(last.is_err_and(|e| e.0.contains("the index reached")));
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(thousands(12), "12");
    }

    #[test]
    fn skipped_members_are_described() {
        let mut s = Stats::default();
        assert_eq!(s.skipped_text(), None);
        s.skipped.push((UNSAFE_PATH, 3));
        assert_eq!(
            s.skipped_text().unwrap(),
            "3 members not shown: unsafe path"
        );
        s.skipped.push((SPARSE, 1));
        assert_eq!(
            s.skipped_text().unwrap(),
            "4 members not shown: 3 unsafe path, 1 sparse member"
        );
    }
}
