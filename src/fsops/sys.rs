//! The syscall layer (design section 4). This is the only module allowed to contain
//! `unsafe` (NFR-SEC). It has one `unsafe` call: glibc's `mallopt` at startup
//! ([`tune_allocator`]), which `rustix` does not wrap. Every syscall goes through `rustix`'s
//! safe wrappers.
//!
//! Every wrapper that a job uses takes the name of its step (`copy.chunk`,
//! `commit.rename`, ...). With the `failpoints` feature, the job's registry is consulted
//! under that name before the real call; without it the check compiles to nothing.
//! No syscall outside the design is added.

use super::failpoints::Failpoints;
use rustix::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use rustix::fs::{
    AtFlags, CWD, Mode, OFlags, RenameFlags, StatFs, Statx, StatxAttributes, StatxFlags, Timestamps,
};
use rustix::io::Errno;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub type Result<T> = std::result::Result<T, Errno>;

/// The mmap threshold [`tune_allocator`] fixes: 128 KiB, glibc's initial value.
#[cfg(target_env = "gnu")]
const MMAP_THRESHOLD: libc::c_int = 128 << 10;

/// Fixes glibc's mmap threshold at 128 KiB (P-6). glibc raises the threshold each time it
/// frees a block it had mmapped, up to 32 MiB. A freed listing of 100k entries (several MiB)
/// then lifts it, the next listings come from the heap, and they stay resident after they
/// are freed: repeated refreshes grew RSS past the P-6 limits. Setting the threshold turns
/// that adjustment off, so large blocks keep going to mmap and back to the kernel when
/// freed. Called once, first thing in `main`, before any thread is spawned.
pub fn tune_allocator() {
    #[cfg(target_env = "gnu")]
    // SAFETY: `mallopt` takes two integers and only changes glibc's allocator parameters;
    // it touches no memory of ours. It runs before any other thread exists, so no
    // allocation runs concurrently with the change. Its result (1 on success) is ignored: on
    // failure the default, dynamic threshold stays.
    unsafe {
        libc::mallopt(libc::M_MMAP_THRESHOLD, MMAP_THRESHOLD);
    }
}

/// `STATX_MNT_ID_UNIQUE` (Linux 6.8). rustix has no named constant for it yet.
const STATX_MNT_ID_UNIQUE: u32 = 0x4000;
/// `STATX_MNT_ID` (Linux 5.8), the fallback when the unique ID is not reported.
const STATX_MNT_ID: u32 = 0x1000;

/// `(st_dev, st_ino, mnt_id)`: the identity used for every decision in design section 4.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct FsIdentity {
    pub dev: u64,
    pub ino: u64,
    /// The unique mount ID (`STATX_MNT_ID_UNIQUE`), or the reusable one when the kernel does
    /// not report the unique ID.
    pub mnt_id: u64,
}

impl FsIdentity {
    /// The rename domain: `(st_dev, mnt_id)`.
    pub fn domain(&self) -> (u64, u64) {
        (self.dev, self.mnt_id)
    }

    /// `(st_dev, st_ino)`: the same inode, whichever mount it is seen through.
    pub fn inode(&self) -> (u64, u64) {
        (self.dev, self.ino)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Fifo,
    Socket,
    BlockDevice,
    CharDevice,
    #[default]
    Unknown,
}

impl Kind {
    pub fn from_mode(mode: u32) -> Kind {
        match mode & 0o170000 {
            0o100000 => Kind::File,
            0o040000 => Kind::Dir,
            0o120000 => Kind::Symlink,
            0o010000 => Kind::Fifo,
            0o140000 => Kind::Socket,
            0o060000 => Kind::BlockDevice,
            0o020000 => Kind::CharDevice,
            _ => Kind::Unknown,
        }
    }

    pub fn is_special(self) -> bool {
        matches!(
            self,
            Kind::Fifo | Kind::Socket | Kind::BlockDevice | Kind::CharDevice | Kind::Unknown
        )
    }
}

/// A timestamp with nanosecond resolution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Ts {
    pub sec: i64,
    pub nsec: u32,
}

impl Ts {
    pub fn as_nanos(self) -> i128 {
        self.sec as i128 * 1_000_000_000 + self.nsec as i128
    }

    fn timespec(self) -> rustix::fs::Timespec {
        rustix::fs::Timespec {
            tv_sec: self.sec,
            tv_nsec: self.nsec as _,
        }
    }
}

/// The metadata of one entry, from `statx`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Meta {
    pub kind: Kind,
    /// Permission bits including setuid, setgid and sticky (`mode & 0o7777`).
    pub perm: u32,
    pub uid: u32,
    pub nlink: u32,
    pub size: u64,
    /// Allocated 512-byte blocks (`stx_blocks`): fewer than the size needs marks a file
    /// with holes (P2 9.1).
    pub blocks: u64,
    pub id: FsIdentity,
    pub atime: Ts,
    pub mtime: Ts,
    pub ctime: Ts,
    /// The kernel reports the entry as the root of a mount.
    pub mount_root: bool,
    /// The kernel reports the entry as an automount trigger (`STATX_ATTR_AUTOMOUNT`):
    /// opening it would mount a filesystem there. Seen only by a `statx` with
    /// `AT_NO_AUTOMOUNT` of a trigger that is not mounted yet.
    pub automount: bool,
}

/// `S0` of design section 4.7: what must still hold when a move unlinks its source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime: Ts,
    pub ctime: Ts,
}

impl Meta {
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            dev: self.id.dev,
            ino: self.id.ino,
            size: self.size,
            mtime: self.mtime,
            ctime: self.ctime,
        }
    }

    fn from_statx(s: &Statx) -> Meta {
        let ts = |t: rustix::fs::StatxTimestamp| Ts {
            sec: t.tv_sec,
            nsec: t.tv_nsec,
        };
        let mask = s.stx_mask;
        let mnt_id = if mask & (STATX_MNT_ID_UNIQUE | STATX_MNT_ID) != 0 {
            s.stx_mnt_id
        } else {
            0
        };
        Meta {
            kind: Kind::from_mode(s.stx_mode as u32),
            perm: s.stx_mode as u32 & 0o7777,
            uid: s.stx_uid,
            nlink: s.stx_nlink,
            size: s.stx_size,
            blocks: s.stx_blocks,
            id: FsIdentity {
                dev: rustix::fs::makedev(s.stx_dev_major, s.stx_dev_minor),
                ino: s.stx_ino,
                mnt_id,
            },
            atime: ts(s.stx_atime),
            mtime: ts(s.stx_mtime),
            ctime: ts(s.stx_ctime),
            mount_root: s.stx_attributes.contains(StatxAttributes::MOUNT_ROOT),
            automount: s.stx_attributes.contains(StatxAttributes::AUTOMOUNT),
        }
    }
}

fn statx_mask() -> StatxFlags {
    StatxFlags::BASIC_STATS | StatxFlags::from_bits_retain(STATX_MNT_ID_UNIQUE)
}

/// The syscall context of one job (or one listing thread): its cancel flag and, with the
/// `failpoints` feature, its failpoint registry.
#[derive(Clone)]
pub struct Sys {
    cancel: Arc<AtomicBool>,
    #[cfg_attr(not(feature = "failpoints"), allow(dead_code))]
    fp: Option<Arc<Failpoints>>,
}

impl Default for Sys {
    fn default() -> Self {
        Sys::new(Arc::new(AtomicBool::new(false)))
    }
}

/// Times that set the modification time and keep the access time (`UTIME_OMIT`).
fn mtime_only(mtime: Ts) -> Timestamps {
    Timestamps {
        last_access: rustix::fs::Timespec {
            tv_sec: 0,
            tv_nsec: rustix::fs::UTIME_OMIT,
        },
        last_modification: mtime.timespec(),
    }
}

/// Retries a call on `EINTR`.
fn retry<T>(mut f: impl FnMut() -> Result<T>) -> Result<T> {
    loop {
        match f() {
            Err(Errno::INTR) => continue,
            r => return r,
        }
    }
}

impl Sys {
    pub fn new(cancel: Arc<AtomicBool>) -> Self {
        Sys { cancel, fp: None }
    }

    /// A context whose steps consult `fp`. Without the `failpoints` feature the registry is
    /// kept but never consulted.
    pub fn with_failpoints(cancel: Arc<AtomicBool>, fp: Arc<Failpoints>) -> Self {
        Sys {
            cancel,
            fp: Some(fp),
        }
    }

    pub fn cancel_flag(&self) -> &Arc<AtomicBool> {
        &self.cancel
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Consults the failpoint registry for `step`.
    #[inline(always)]
    pub fn hit(&self, step: &'static str) -> Result<()> {
        #[cfg(feature = "failpoints")]
        if let Some(fp) = &self.fp {
            return fp.check(step, &self.cancel);
        }
        let _ = step;
        Ok(())
    }

    // ---- opening ------------------------------------------------------------------------

    /// Opens a job root: the user-visible panel path, resolved once (design 4.3).
    pub fn open_root(&self, path: &Path) -> Result<OwnedFd> {
        retry(|| {
            rustix::fs::open(
                path,
                OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC,
                Mode::empty(),
            )
        })
    }

    /// Descends into a child directory: `O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC`.
    pub fn open_dir(&self, step: &'static str, dir: BorrowedFd, name: &OsStr) -> Result<OwnedFd> {
        self.hit(step)?;
        retry(|| {
            rustix::fs::openat(
                dir,
                name,
                OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::RDONLY | OFlags::CLOEXEC,
                Mode::empty(),
            )
        })
    }

    /// `openat(dir, name, O_PATH | O_NOFOLLOW | O_CLOEXEC)` (design 4.3 step 1).
    pub fn open_path(&self, step: &'static str, dir: BorrowedFd, name: &OsStr) -> Result<OwnedFd> {
        self.hit(step)?;
        retry(|| {
            rustix::fs::openat(
                dir,
                name,
                OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
        })
    }

    /// Reopens an `O_PATH` fd of a regular file for reading through `/proc/self/fd/<n>`
    /// (design 4.3 step 3). The caller confirms the identity afterwards.
    pub fn reopen_read(&self, opath: BorrowedFd) -> Result<OwnedFd> {
        let proc = format!("/proc/self/fd/{}", opath.as_raw_fd());
        retry(|| {
            rustix::fs::openat(
                CWD,
                proc.as_str(),
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOCTTY,
                Mode::empty(),
            )
        })
    }

    /// Creates a new file exclusively: `O_CREAT | O_EXCL | O_WRONLY | O_NOFOLLOW | O_CLOEXEC`.
    pub fn create_excl(
        &self,
        step: &'static str,
        dir: BorrowedFd,
        name: &OsStr,
        mode: u32,
    ) -> Result<OwnedFd> {
        self.hit(step)?;
        retry(|| {
            rustix::fs::openat(
                dir,
                name,
                OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::WRONLY
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC
                    | OFlags::NOCTTY,
                Mode::from_raw_mode(mode),
            )
        })
    }

    // ---- metadata -----------------------------------------------------------------------

    /// `statx(dir, name, AT_SYMLINK_NOFOLLOW)`.
    pub fn stat_at(&self, step: &'static str, dir: BorrowedFd, name: &OsStr) -> Result<Meta> {
        self.hit(step)?;
        retry(|| rustix::fs::statx(dir, name, AtFlags::SYMLINK_NOFOLLOW, statx_mask()))
            .map(|s| Meta::from_statx(&s))
    }

    /// `statx(dir, name, AT_SYMLINK_NOFOLLOW | AT_NO_AUTOMOUNT)`: a search's walk never
    /// follows a symlink and never triggers an automount (P2 5.2). [`Meta::automount`]
    /// tells an automount trigger apart; Linux has no request bit for it, `stx_attributes`
    /// is always filled.
    pub fn stat_at_noauto(&self, dir: BorrowedFd, name: &OsStr) -> Result<Meta> {
        retry(|| {
            rustix::fs::statx(
                dir,
                name,
                AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT,
                statx_mask(),
            )
        })
        .map(|s| Meta::from_statx(&s))
    }

    /// Only the file type (`STATX_TYPE`) of `name`, with the flags of
    /// [`Sys::stat_at_noauto`]: what a `DT_UNKNOWN` directory entry costs a search (P2 5.3).
    pub fn kind_at_noauto(&self, dir: BorrowedFd, name: &OsStr) -> Result<Kind> {
        retry(|| {
            rustix::fs::statx(
                dir,
                name,
                AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT,
                StatxFlags::TYPE,
            )
        })
        .map(|s| Kind::from_mode(s.stx_mode as u32))
    }

    /// `statx` of an open fd (`AT_EMPTY_PATH`).
    pub fn stat_fd(&self, fd: BorrowedFd) -> Result<Meta> {
        retry(|| rustix::fs::statx(fd, c"", AtFlags::EMPTY_PATH, statx_mask()))
            .map(|s| Meta::from_statx(&s))
    }

    /// `statx` of a path, following symlinks (panel paths, the trash home).
    pub fn stat_path(&self, path: &Path) -> Result<Meta> {
        retry(|| rustix::fs::statx(CWD, path, AtFlags::empty(), statx_mask()))
            .map(|s| Meta::from_statx(&s))
    }

    /// `statx` of a path without following a final symlink.
    pub fn lstat_path(&self, path: &Path) -> Result<Meta> {
        retry(|| rustix::fs::statx(CWD, path, AtFlags::SYMLINK_NOFOLLOW, statx_mask()))
            .map(|s| Meta::from_statx(&s))
    }

    pub fn fstatfs(&self, fd: BorrowedFd) -> Result<StatFs> {
        retry(|| rustix::fs::fstatfs(fd))
    }

    /// Free and total bytes of the filesystem holding `fd`, for the panel footer.
    pub fn free_space(&self, fd: BorrowedFd) -> Result<(u64, u64)> {
        let v = retry(|| rustix::fs::fstatvfs(fd))?;
        Ok((v.f_bavail * v.f_frsize, v.f_blocks * v.f_frsize))
    }

    // ---- data ---------------------------------------------------------------------------

    /// One `copy_file_range` call; offsets advance in the kernel (design 4.7 step 3).
    pub fn copy_range(
        &self,
        step: &'static str,
        from: BorrowedFd,
        to: BorrowedFd,
        len: usize,
    ) -> Result<usize> {
        self.hit(step)?;
        retry(|| rustix::fs::copy_file_range(from, None, to, None, len))
    }

    /// One `copy_file_range` call at explicit offsets, which the kernel advances; the file
    /// offsets of both fds stay unchanged (P2 9.1 step 3).
    pub fn copy_range_at(
        &self,
        step: &'static str,
        from: BorrowedFd,
        off_in: &mut u64,
        to: BorrowedFd,
        off_out: &mut u64,
        len: usize,
    ) -> Result<usize> {
        self.hit(step)?;
        retry(|| {
            rustix::fs::copy_file_range(from, Some(&mut *off_in), to, Some(&mut *off_out), len)
        })
    }

    /// `lseek(fd, off, SEEK_DATA)`: the start of the first data at or after `off`. `ENXIO`
    /// means there is none (P2 9.1 step 1).
    pub fn seek_data(&self, step: &'static str, fd: BorrowedFd, off: u64) -> Result<u64> {
        self.hit(step)?;
        retry(|| rustix::fs::seek(fd, rustix::fs::SeekFrom::Data(off)))
    }

    /// `lseek(fd, off, SEEK_HOLE)`: the start of the first hole at or after `off`, or the
    /// end of the file (P2 9.1 step 2).
    pub fn seek_hole(&self, step: &'static str, fd: BorrowedFd, off: u64) -> Result<u64> {
        self.hit(step)?;
        retry(|| rustix::fs::seek(fd, rustix::fs::SeekFrom::Hole(off)))
    }

    /// `pread` at `off`; the file offset stays unchanged.
    pub fn pread(
        &self,
        step: &'static str,
        fd: BorrowedFd,
        buf: &mut [u8],
        off: u64,
    ) -> Result<usize> {
        self.hit(step)?;
        retry(|| rustix::io::pread(fd, &mut *buf, off))
    }

    /// `pwrite` of all of `buf` at `off`, continuing after short writes.
    pub fn pwrite_all(
        &self,
        step: &'static str,
        fd: BorrowedFd,
        mut buf: &[u8],
        mut off: u64,
    ) -> Result<()> {
        self.hit(step)?;
        while !buf.is_empty() {
            let n = retry(|| rustix::io::pwrite(fd, buf, off))?;
            if n == 0 {
                return Err(Errno::IO);
            }
            buf = &buf[n..];
            off += n as u64;
        }
        Ok(())
    }

    /// `ftruncate(fd, len)`: sets the size; an extension is a hole (P2 9.1 step 4).
    pub fn ftruncate(&self, step: &'static str, fd: BorrowedFd, len: u64) -> Result<()> {
        self.hit(step)?;
        retry(|| rustix::fs::ftruncate(fd, len))
    }

    pub fn read(&self, step: &'static str, fd: BorrowedFd, buf: &mut [u8]) -> Result<usize> {
        self.hit(step)?;
        retry(|| rustix::io::read(fd, &mut *buf))
    }

    pub fn write_all(&self, step: &'static str, fd: BorrowedFd, mut buf: &[u8]) -> Result<()> {
        self.hit(step)?;
        while !buf.is_empty() {
            let n = retry(|| rustix::io::write(fd, buf))?;
            if n == 0 {
                return Err(Errno::IO);
            }
            buf = &buf[n..];
        }
        Ok(())
    }

    pub fn fchmod(&self, step: &'static str, fd: BorrowedFd, perm: u32) -> Result<()> {
        self.hit(step)?;
        retry(|| rustix::fs::fchmod(fd, Mode::from_raw_mode(perm)))
    }

    pub fn futimens(&self, step: &'static str, fd: BorrowedFd, atime: Ts, mtime: Ts) -> Result<()> {
        self.hit(step)?;
        let times = Timestamps {
            last_access: atime.timespec(),
            last_modification: mtime.timespec(),
        };
        retry(|| rustix::fs::futimens(fd, &times))
    }

    /// `chmod("/proc/self/fd/<n>")`: changes the mode of the inode an `O_PATH` fd refers to
    /// (P2 8.2). `fchmod` fails with `EBADF` on an `O_PATH` fd; the magic link reaches the
    /// same inode without a name lookup, so a swapped name cannot redirect it (I-5).
    pub fn chmod_fd(&self, step: &'static str, fd: BorrowedFd, perm: u32) -> Result<()> {
        self.hit(step)?;
        let proc = format!("/proc/self/fd/{}", fd.as_raw_fd());
        retry(|| {
            rustix::fs::chmodat(
                CWD,
                proc.as_str(),
                Mode::from_raw_mode(perm),
                AtFlags::empty(),
            )
        })
    }

    /// `utimensat(AT_FDCWD, "/proc/self/fd/<n>", {UTIME_OMIT, mtime}, 0)`: sets the
    /// modification time of the inode an `O_PATH` fd refers to and keeps its access time
    /// (P2 8.2).
    pub fn set_mtime_fd(&self, step: &'static str, fd: BorrowedFd, mtime: Ts) -> Result<()> {
        self.hit(step)?;
        let proc = format!("/proc/self/fd/{}", fd.as_raw_fd());
        let times = mtime_only(mtime);
        retry(|| rustix::fs::utimensat(CWD, proc.as_str(), &times, AtFlags::empty()))
    }

    /// `utimensat(dir, name, {UTIME_OMIT, mtime}, AT_SYMLINK_NOFOLLOW)`: a symlink's own
    /// modification time; its target is untouched (P2 8.2, I-5).
    pub fn set_mtime_nofollow(
        &self,
        step: &'static str,
        dir: BorrowedFd,
        name: &OsStr,
        mtime: Ts,
    ) -> Result<()> {
        self.hit(step)?;
        let times = mtime_only(mtime);
        retry(|| rustix::fs::utimensat(dir, name, &times, AtFlags::SYMLINK_NOFOLLOW))
    }

    pub fn fsync(&self, step: &'static str, fd: BorrowedFd) -> Result<()> {
        self.hit(step)?;
        retry(|| rustix::fs::fsync(fd))
    }

    pub fn syncfs(&self, step: &'static str, fd: BorrowedFd) -> Result<()> {
        self.hit(step)?;
        retry(|| rustix::fs::syncfs(fd))
    }

    // ---- namespace ----------------------------------------------------------------------

    /// `renameat2(..., RENAME_NOREPLACE)` when `noreplace`, else plain `renameat`.
    pub fn rename(
        &self,
        step: &'static str,
        from_dir: BorrowedFd,
        from: &OsStr,
        to_dir: BorrowedFd,
        to: &OsStr,
        noreplace: bool,
    ) -> Result<()> {
        self.hit(step)?;
        let flags = if noreplace {
            RenameFlags::NOREPLACE
        } else {
            RenameFlags::empty()
        };
        retry(|| rustix::fs::renameat_with(from_dir, from, to_dir, to, flags))
    }

    /// `linkat(from_dir, from, to_dir, to, 0)`: links the entry itself, never a symlink target.
    pub fn link(
        &self,
        step: &'static str,
        from_dir: BorrowedFd,
        from: &OsStr,
        to_dir: BorrowedFd,
        to: &OsStr,
    ) -> Result<()> {
        self.hit(step)?;
        retry(|| rustix::fs::linkat(from_dir, from, to_dir, to, AtFlags::empty()))
    }

    /// Links the inode that `fd` refers to under `to_dir/to`:
    /// `linkat(AT_FDCWD, "/proc/self/fd/<n>", to_dir, to, AT_SYMLINK_FOLLOW)`. Unlike a
    /// link by name, it cannot pick up an entry that replaced the name after `fd` was
    /// opened; an inode without names left fails with `ENOENT` (P2 9.2).
    pub fn link_fd(
        &self,
        step: &'static str,
        fd: BorrowedFd,
        to_dir: BorrowedFd,
        to: &OsStr,
    ) -> Result<()> {
        self.hit(step)?;
        let proc = format!("/proc/self/fd/{}", fd.as_raw_fd());
        retry(|| rustix::fs::linkat(CWD, proc.as_str(), to_dir, to, AtFlags::SYMLINK_FOLLOW))
    }

    pub fn unlink(&self, step: &'static str, dir: BorrowedFd, name: &OsStr) -> Result<()> {
        self.hit(step)?;
        retry(|| rustix::fs::unlinkat(dir, name, AtFlags::empty()))
    }

    pub fn rmdir(&self, step: &'static str, dir: BorrowedFd, name: &OsStr) -> Result<()> {
        self.hit(step)?;
        retry(|| rustix::fs::unlinkat(dir, name, AtFlags::REMOVEDIR))
    }

    pub fn mkdir(
        &self,
        step: &'static str,
        dir: BorrowedFd,
        name: &OsStr,
        mode: u32,
    ) -> Result<()> {
        self.hit(step)?;
        retry(|| rustix::fs::mkdirat(dir, name, Mode::from_raw_mode(mode)))
    }

    pub fn symlink(
        &self,
        step: &'static str,
        target: &OsStr,
        dir: BorrowedFd,
        name: &OsStr,
    ) -> Result<()> {
        self.hit(step)?;
        retry(|| rustix::fs::symlinkat(target, dir, name))
    }

    pub fn readlink(&self, step: &'static str, dir: BorrowedFd, name: &OsStr) -> Result<OsString> {
        self.hit(step)?;
        let target = retry(|| rustix::fs::readlinkat(dir, name, Vec::new()))?;
        Ok(OsStr::from_bytes(target.as_bytes()).to_owned())
    }

    /// The entries of a directory fd, without `.` and `..`, as `(name, d_type kind)`.
    /// The fd is duplicated, so the caller keeps its own.
    pub fn read_dir(&self, step: &'static str, dir: BorrowedFd) -> Result<Vec<(OsString, Kind)>> {
        self.hit(step)?;
        let mut d = rustix::fs::Dir::read_from(dir)?;
        let mut out = Vec::new();
        while let Some(e) = d.read() {
            let e = e?;
            let name = e.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            let kind = match e.file_type() {
                rustix::fs::FileType::RegularFile => Kind::File,
                rustix::fs::FileType::Directory => Kind::Dir,
                rustix::fs::FileType::Symlink => Kind::Symlink,
                rustix::fs::FileType::Fifo => Kind::Fifo,
                rustix::fs::FileType::Socket => Kind::Socket,
                rustix::fs::FileType::BlockDevice => Kind::BlockDevice,
                rustix::fs::FileType::CharacterDevice => Kind::CharDevice,
                _ => Kind::Unknown,
            };
            out.push((OsStr::from_bytes(name).to_owned(), kind));
        }
        Ok(out)
    }
}

/// The kernel's path of an open fd (`readlink /proc/self/fd/<n>`): the canonical path of
/// a directory the job opened, used where a path must be written down (trash `Path`).
pub fn fd_path(fd: BorrowedFd) -> Result<std::path::PathBuf> {
    let proc = format!("/proc/self/fd/{}", fd.as_raw_fd());
    let target = retry(|| rustix::fs::readlinkat(CWD, proc.as_str(), Vec::new()))?;
    Ok(std::path::PathBuf::from(OsStr::from_bytes(
        target.as_bytes(),
    )))
}

/// Borrows any fd-like value.
pub fn fd<F: AsFd>(f: &F) -> BorrowedFd<'_> {
    f.as_fd()
}

/// The effective user ID.
pub fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Random bytes for temporary names.
pub fn random_u64() -> u64 {
    let mut b = [0u8; 8];
    match rustix::rand::getrandom(&mut b, rustix::rand::GetRandomFlags::empty()) {
        Ok(_) => u64::from_ne_bytes(b),
        // getrandom cannot fail after boot; fall back to a clock-derived value anyway.
        Err(_) => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0),
    }
}

/// Raises the `RLIMIT_NOFILE` soft limit to the hard limit (design 4.3). Returns the new
/// soft limit.
pub fn raise_nofile_limit() -> Option<u64> {
    use rustix::process::{Resource, getrlimit, setrlimit};
    let mut lim = getrlimit(Resource::Nofile);
    if lim.current != lim.maximum {
        lim.current = lim.maximum;
        setrlimit(Resource::Nofile, lim).ok()?;
    }
    getrlimit(Resource::Nofile).current
}

/// Filesystem magic numbers used by the engine (`fstatfs` `f_type`).
pub mod magic {
    pub const VFAT: i64 = 0x4d44;
    pub const EXFAT: i64 = 0x2011_bab0;
    pub const BTRFS: i64 = 0x9123_683e;
    pub const TMPFS: i64 = 0x0102_1994;
}
