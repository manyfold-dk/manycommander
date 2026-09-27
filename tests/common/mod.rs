//! Shared helpers for the integration tests (plan: "Test directories" and
//! "Environment-dependent tests").
#![allow(dead_code)]

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Skips a test that needs something this environment lacks. With `MC_REQUIRE_ALL=1`
/// (set by `scripts/check.sh full`) a skip fails the test instead.
pub fn skip(reason: &str) {
    if std::env::var("MC_REQUIRE_ALL").as_deref() == Ok("1") {
        panic!("SKIP is not allowed (MC_REQUIRE_ALL=1): {reason}");
    }
    eprintln!("SKIP {reason}");
}

/// A per-test directory that is removed on drop, even when it holds read-only
/// directories or btrfs subvolumes.
pub struct TestDir {
    pub path: PathBuf,
}

impl TestDir {
    pub fn join(&self, p: impl AsRef<Path>) -> PathBuf {
        self.path.join(p)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        remove_tree(&self.path);
    }
}

/// Makes every directory under `p` writable, then removes the tree. Empty btrfs subvolumes
/// go with `rmdir`.
pub fn remove_tree(p: &Path) {
    fn open_up(p: &Path) {
        if let Ok(m) = std::fs::symlink_metadata(p)
            && m.is_dir()
        {
            let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700));
            if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    open_up(&e.path());
                }
            }
        }
    }
    open_up(p);
    if std::fs::remove_dir_all(p).is_err() {
        // A subvolume root cannot be removed by remove_dir_all's unlinkat on some kernels;
        // retry children bottom-up with rmdir.
        fn bottom_up(p: &Path) {
            if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    let ep = e.path();
                    match std::fs::symlink_metadata(&ep) {
                        Ok(m) if m.is_dir() => {
                            bottom_up(&ep);
                            let _ = std::fs::remove_dir(&ep);
                        }
                        _ => {
                            let _ = std::fs::remove_file(&ep);
                        }
                    }
                }
            }
        }
        bottom_up(p);
        let _ = std::fs::remove_dir(p);
    }
}

fn fresh(base: &Path, name: &str) -> TestDir {
    let path = base.join(format!("{name}-{}", std::process::id()));
    remove_tree(&path);
    std::fs::create_dir_all(&path).unwrap();
    TestDir { path }
}

/// The same-filesystem test root: `target/test-tmp/`.
pub fn tmp_root() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp");
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// `target/test-tmp/<name>-<pid>/`.
pub fn test_dir(name: &str) -> TestDir {
    fresh(&tmp_root(), name)
}

/// `$MC_XDEV_DIR/<name>-<pid>/`, on a different filesystem than `target/test-tmp`.
/// Returns `None` (after `skip`) when `MC_XDEV_DIR` is unset or on the same device.
pub fn xdev_dir(name: &str) -> Option<TestDir> {
    let Some(base) = std::env::var_os("MC_XDEV_DIR") else {
        skip("MC_XDEV_DIR is not set; cross-filesystem test not run");
        return None;
    };
    let base = PathBuf::from(base);
    std::fs::create_dir_all(&base).unwrap();
    let local = std::fs::metadata(tmp_root()).unwrap().dev();
    if std::fs::metadata(&base).unwrap().dev() == local {
        skip("MC_XDEV_DIR is on the same device as target/test-tmp");
        return None;
    }
    Some(fresh(&base, name))
}

/// `f_type` of the filesystem holding `p`.
pub fn fs_type(p: &Path) -> i64 {
    rustix::fs::statfs(p).unwrap().f_type as i64
}

pub const BTRFS_MAGIC: i64 = 0x9123_683e;

/// Whether `target/test-tmp` is on btrfs; skips otherwise.
pub fn require_btrfs() -> bool {
    if fs_type(&tmp_root()) != BTRFS_MAGIC {
        skip("target/test-tmp is not on btrfs");
        return false;
    }
    if Command::new("btrfs").arg("--version").output().is_err() {
        skip("btrfs-progs is not installed");
        return false;
    }
    true
}

/// Creates a btrfs subvolume at `p` (unprivileged).
pub fn subvolume_create(p: &Path) {
    let out = Command::new("btrfs")
        .args(["subvolume", "create"])
        .arg(p)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "btrfs subvolume create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Runs the calling test's body inside `unshare -rm` (a user and mount namespace).
///
/// In the parent this re-executes the test binary for exactly `test_name` in the namespace,
/// asserts that the child ran that one test and passed, and returns `false`: the caller
/// returns. In the child it returns `true`: the caller runs the body.
pub fn in_userns(test_name: &str) -> bool {
    if std::env::var_os("MC_IN_USERNS").is_some() {
        return true;
    }
    let ok = Command::new("unshare")
        .args(["-rm", "true"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        skip("unshare -rm is not available (user namespaces)");
        return false;
    }
    let exe = std::env::current_exe().unwrap();
    let out = Command::new("unshare")
        .arg("-rm")
        .arg(&exe)
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env("MC_IN_USERNS", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "namespaced run of {test_name} failed:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains("test result: ok. 1 passed"),
        "namespaced run of {test_name} did not run exactly one test:\n{stdout}\n{stderr}"
    );
    false
}

/// `mount --bind from to`; only inside `in_userns`.
pub fn bind_mount(from: &Path, to: &Path) {
    let out = Command::new("mount")
        .arg("--bind")
        .arg(from)
        .arg(to)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "mount --bind failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn umount(p: &Path) {
    let _ = Command::new("umount").arg(p).status();
}

/// Writes `content` to `p`.
pub fn write(p: &Path, content: &[u8]) {
    std::fs::write(p, content).unwrap();
}

/// BLAKE3 hash of a file's content.
pub fn hash(p: &Path) -> blake3::Hash {
    blake3::hash(&std::fs::read(p).unwrap())
}

/// A deterministic pseudo-random buffer.
pub fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

pub fn os(b: &[u8]) -> &OsStr {
    OsStr::from_bytes(b)
}
