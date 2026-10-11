#![forbid(unsafe_code)]
//! Filesystem identity (design section 4.2).
//!
//! `rename(2)` works only within one mount of one filesystem, and on btrfs only within one
//! subvolume. `st_dev` alone cannot tell these apart: bind mounts share `st_dev`, and btrfs
//! subvolumes that are not mount points have their own. Every decision therefore uses the
//! pair `(st_dev, mnt_id)`.

pub use super::sys::FsIdentity;
use super::sys::MNT_ID_UNKNOWN;

/// How a child directory relates to its parent (design 4.2 table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relation {
    /// Same `mnt_id`, same `st_dev`: same filesystem and subvolume.
    Same,
    /// Same `mnt_id`, different `st_dev`: a btrfs subvolume inside the mount.
    Subvolume,
    /// Different `mnt_id`: a mount point or a bind mount.
    Mount,
}

/// How `child` relates to `parent`. A mount ID the kernel did not report
/// ([`MNT_ID_UNKNOWN`], a kernel before 5.8) counts as a different mount: move, delete and
/// a recursive attribute change then skip the directory instead of crossing a mount point
/// they cannot see.
pub fn relation(parent: &FsIdentity, child: &FsIdentity) -> Relation {
    if parent.mnt_id != child.mnt_id
        || parent.mnt_id == MNT_ID_UNKNOWN
        || child.mnt_id == MNT_ID_UNKNOWN
    {
        Relation::Mount
    } else if parent.dev != child.dev {
        Relation::Subvolume
    } else {
        Relation::Same
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(dev: u64, ino: u64, mnt_id: u64) -> FsIdentity {
        FsIdentity { dev, ino, mnt_id }
    }

    #[test]
    fn relation_table() {
        assert_eq!(relation(&id(1, 2, 7), &id(1, 3, 7)), Relation::Same);
        assert_eq!(relation(&id(1, 2, 7), &id(9, 3, 7)), Relation::Subvolume);
        assert_eq!(relation(&id(1, 2, 7), &id(1, 3, 8)), Relation::Mount);
        assert_eq!(relation(&id(1, 2, 7), &id(9, 3, 8)), Relation::Mount);
    }

    #[test]
    fn an_unknown_mount_id_is_never_the_same_mount() {
        let u = MNT_ID_UNKNOWN;
        assert_eq!(relation(&id(1, 2, u), &id(1, 3, u)), Relation::Mount);
        assert_eq!(relation(&id(1, 2, 7), &id(1, 3, u)), Relation::Mount);
        assert_eq!(relation(&id(1, 2, u), &id(1, 3, 7)), Relation::Mount);
    }
}
