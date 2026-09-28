#![forbid(unsafe_code)]
//! The theme watcher (design 7.2, source 1).
//!
//! `omarchy-theme-set` replaces `current/theme` as a whole (`rm -rf`, then `mv
//! next-theme theme`), so a watch on `current/theme` would die with the old directory. The
//! watch sits on `current/` and filters: only `IN_MOVED_TO` or `IN_CREATE` of `theme`,
//! `IN_CLOSE_WRITE` of `theme.name`, and `IN_Q_OVERFLOW` trigger a reload, after a 50 ms
//! debounce. Every `next-theme` event and `IN_DELETE` are ignored. When the watched
//! directory does not exist, the nearest existing ancestor is watched, and the watch
//! re-arms when the directory appears. The thread blocks in `read` when idle (P-5).

use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const DEBOUNCE: Duration = Duration::from_millis(50);

/// What the watcher follows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Omarchy's `~/.local/state/omarchy/current/`; the palette is `theme/colors.toml`.
    Omarchy { current: PathBuf },
    /// `--theme-file <path>`: its parent is watched, and in-place edits count too.
    File { path: PathBuf },
}

impl Target {
    /// The Omarchy state directory under `$HOME`.
    pub fn omarchy_default(home: Option<&OsStr>) -> Option<Target> {
        home.filter(|h| !h.is_empty()).map(|h| Target::Omarchy {
            current: Path::new(h).join(".local/state/omarchy/current"),
        })
    }

    pub fn palette_path(&self) -> PathBuf {
        match self {
            Target::Omarchy { current } => current.join("theme/colors.toml"),
            Target::File { path } => path.clone(),
        }
    }

    /// The directory the watch belongs on.
    pub fn dir(&self) -> PathBuf {
        match self {
            Target::Omarchy { current } => current.clone(),
            Target::File { path } => path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."))
                .to_path_buf(),
        }
    }
}

/// Whether one event in the watched directory triggers a reload.
pub fn triggers(target: &Target, mask: EventMask, name: Option<&OsStr>) -> bool {
    if mask.contains(EventMask::Q_OVERFLOW) {
        return true;
    }
    let Some(name) = name else { return false };
    match target {
        Target::Omarchy { .. } => {
            (name == "theme" && mask.intersects(EventMask::MOVED_TO | EventMask::CREATE))
                || (name == "theme.name" && mask.contains(EventMask::CLOSE_WRITE))
        }
        Target::File { path } => {
            Some(name) == path.file_name()
                && mask.intersects(EventMask::CLOSE_WRITE | EventMask::MOVED_TO)
        }
    }
}

enum Armed {
    /// The target directory itself is watched.
    Direct(WatchDescriptor),
    /// An ancestor is watched until the next component on the way appears.
    Ancestor(WatchDescriptor, std::ffi::OsString),
    /// Not even `/` could be watched.
    None,
}

fn arm(ino: &mut Inotify, dir: &Path) -> Armed {
    let direct = WatchMask::CREATE
        | WatchMask::MOVED_TO
        | WatchMask::CLOSE_WRITE
        | WatchMask::DELETE_SELF
        | WatchMask::MOVE_SELF;
    let ancestor =
        WatchMask::CREATE | WatchMask::MOVED_TO | WatchMask::DELETE_SELF | WatchMask::MOVE_SELF;
    // A create can land in the gap after the previous watch is gone and before the new one
    // is installed. Stat after installing. If the next component is already there, that
    // create will never be delivered, so arm one level further down.
    for _ in 0..32 {
        if let Ok(wd) = ino.watches().add(dir, direct) {
            return Armed::Direct(wd);
        }
        let Some((parent, next)) = existing_ancestor(dir) else {
            return Armed::None;
        };
        let Ok(wd) = ino.watches().add(&parent, ancestor) else {
            return Armed::None;
        };
        if parent.join(&next).exists() {
            let _ = ino.watches().remove(wd);
            continue;
        }
        return Armed::Ancestor(wd, next);
    }
    Armed::None
}

/// The nearest existing ancestor of `dir`, and the component directly under it.
fn existing_ancestor(dir: &Path) -> Option<(PathBuf, std::ffi::OsString)> {
    let mut child = dir;
    while let Some(parent) = child.parent() {
        if parent.exists() {
            return Some((parent.to_path_buf(), child.file_name()?.to_owned()));
        }
        child = parent;
    }
    None
}

/// Runs the watcher on the calling thread; `notify` is called once per debounced reload.
/// Returns only when inotify itself fails.
pub fn run(target: Target, notify: impl Fn()) -> std::io::Result<()> {
    let mut ino = Inotify::init()?;
    let dir = target.dir();
    let mut armed = arm(&mut ino, &dir);
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let mut fire = false;
        let mut rearm = false;
        {
            let events = ino.read_events_blocking(&mut buf)?;
            for ev in events {
                scan_event(
                    &target, &armed, ev.wd, ev.mask, ev.name, &mut fire, &mut rearm,
                );
            }
        }
        if rearm {
            rearm_now(&mut ino, &mut armed, &dir, &mut fire);
        }
        if !fire {
            continue;
        }
        // Debounce: absorb what follows within 50 ms, then reload once.
        let deadline = Instant::now() + DEBOUNCE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let ts = rustix::fs::Timespec {
                tv_sec: left.as_secs() as i64,
                tv_nsec: left.subsec_nanos() as _,
            };
            let mut fds = [rustix::event::PollFd::new(
                &ino,
                rustix::event::PollFlags::IN,
            )];
            match rustix::event::poll(&mut fds, Some(&ts)) {
                Ok(0) => break,
                Ok(_) => {
                    let mut again = false;
                    let mut re = false;
                    match ino.read_events(&mut buf) {
                        Ok(events) => {
                            for ev in events {
                                scan_event(
                                    &target, &armed, ev.wd, ev.mask, ev.name, &mut again, &mut re,
                                );
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(e) => return Err(e),
                    }
                    if re {
                        rearm_now(&mut ino, &mut armed, &dir, &mut again);
                    }
                }
                Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(e.into()),
            }
        }
        notify();
    }
}

fn scan_event(
    target: &Target,
    armed: &Armed,
    wd: WatchDescriptor,
    mask: EventMask,
    name: Option<&OsStr>,
    fire: &mut bool,
    rearm: &mut bool,
) {
    if mask.contains(EventMask::Q_OVERFLOW) {
        *fire = true;
        return;
    }
    match armed {
        Armed::Direct(d) if *d == wd => {
            if mask.intersects(EventMask::DELETE_SELF | EventMask::MOVE_SELF | EventMask::IGNORED) {
                *rearm = true;
            } else if triggers(target, mask, name) {
                *fire = true;
            }
        }
        Armed::Ancestor(d, next)
            if *d == wd
                && (mask.intersects(
                    EventMask::DELETE_SELF | EventMask::MOVE_SELF | EventMask::IGNORED,
                ) || name == Some(next.as_os_str())) =>
        {
            *rearm = true;
        }
        _ => {}
    }
}

fn rearm_now(ino: &mut Inotify, armed: &mut Armed, dir: &Path, fire: &mut bool) {
    match std::mem::replace(armed, Armed::None) {
        Armed::Direct(wd) | Armed::Ancestor(wd, _) => {
            let _ = ino.watches().remove(wd);
        }
        Armed::None => {}
    }
    *armed = arm(ino, dir);
    if matches!(armed, Armed::Direct(_)) {
        // The directory (re)appeared: its palette may be new.
        *fire = true;
    }
}

/// Starts the watcher on its own thread.
pub fn spawn(
    target: Target,
    notify: impl Fn() + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("theme-watch".into())
        .spawn(move || {
            // If inotify fails, the watcher stops; SIGUSR1 still reloads (design 7.2).
            if let Err(e) = run(target, notify) {
                tracing::warn!("theme watcher stopped: {e}");
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_follows_the_design() {
        let t = Target::Omarchy {
            current: PathBuf::from("/s/current"),
        };
        let n = |s: &'static str| Some(OsStr::new(s));
        assert!(triggers(
            &t,
            EventMask::MOVED_TO | EventMask::ISDIR,
            n("theme")
        ));
        assert!(triggers(
            &t,
            EventMask::CREATE | EventMask::ISDIR,
            n("theme")
        ));
        assert!(triggers(&t, EventMask::CLOSE_WRITE, n("theme.name")));
        assert!(triggers(&t, EventMask::Q_OVERFLOW, None));
        assert!(!triggers(
            &t,
            EventMask::DELETE | EventMask::ISDIR,
            n("theme")
        ));
        assert!(!triggers(&t, EventMask::MODIFY, n("theme.name")));
        assert!(!triggers(
            &t,
            EventMask::CREATE | EventMask::ISDIR,
            n("next-theme")
        ));
        assert!(!triggers(
            &t,
            EventMask::MOVED_FROM | EventMask::ISDIR,
            n("next-theme")
        ));
        assert!(!triggers(&t, EventMask::CLOSE_WRITE, n("background")));
        let f = Target::File {
            path: PathBuf::from("/x/my.toml"),
        };
        assert!(triggers(&f, EventMask::CLOSE_WRITE, n("my.toml")));
        assert!(triggers(&f, EventMask::MOVED_TO, n("my.toml")));
        assert!(!triggers(&f, EventMask::CLOSE_WRITE, n("other.toml")));
        assert_eq!(f.dir(), Path::new("/x"));
    }
}
