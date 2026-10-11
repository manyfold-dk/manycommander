#![forbid(unsafe_code)]
//! The theme watcher (TDR-1 7.2, source 1).
//!
//! `omarchy-theme-set` replaces `current/theme` as a whole (`rm -rf`, then `mv
//! next-theme theme`), so a watch on `current/theme` would die with the old directory. The
//! watch sits on `current/` and filters: only `IN_MOVED_TO` or `IN_CREATE` of `theme`,
//! `IN_CLOSE_WRITE` of `theme.name`, and `IN_Q_OVERFLOW` trigger a reload, after a 50 ms
//! debounce. Every `next-theme` event and `IN_DELETE` are ignored. When the watched
//! directory does not exist, the nearest existing ancestor is watched, and the watch
//! re-arms when the directory appears. A `--theme-file` that is a symlink also has the
//! directory of the file it resolves to watched (`Link`). The thread blocks in `read`
//! when idle (P-5).

use inotify::{EventMask, Events, Inotify, WatchDescriptor, WatchMask};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const DEBOUNCE: Duration = Duration::from_millis(50);

/// What the watcher follows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Omarchy's `~/.local/state/omarchy/current/`; the palette is `theme/colors.toml`.
    Omarchy { current: PathBuf },
    /// `--theme-file <path>`: its parent is watched, and in-place edits count too. When the
    /// path is a symlink, so is the directory of the file it resolves to.
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

impl Armed {
    /// Whether `wd` is this watch.
    fn holds(&self, wd: &WatchDescriptor) -> bool {
        matches!(self, Armed::Direct(d) | Armed::Ancestor(d, _) if d == wd)
    }
}

/// What a watch on a directory that holds the palette reports.
fn direct_mask() -> WatchMask {
    WatchMask::CREATE
        | WatchMask::MOVED_TO
        | WatchMask::CLOSE_WRITE
        | WatchMask::DELETE_SELF
        | WatchMask::MOVE_SELF
}

fn arm(ino: &mut Inotify, dir: &Path) -> Armed {
    let direct = direct_mask();
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

/// A `--theme-file` that is a symlink (into a dotfiles checkout, say). The file it resolves
/// to is edited in its own directory, where the watch on the link's directory sees nothing,
/// so that directory is watched as well. The link is resolved again after every batch of
/// events: a link pointed at another file moves this watch along, and reloads.
#[derive(Default)]
struct Link {
    /// The file the link resolves to.
    real: Option<PathBuf>,
    /// The watch on that file's directory. It is the main watch itself when both are on the
    /// same directory (inotify gives one descriptor per directory).
    wd: Option<WatchDescriptor>,
}

impl Link {
    /// Resolves `target` again and moves the watch with it. `true` when it resolves to
    /// another file than before.
    fn follow(&mut self, ino: &mut Inotify, target: &Target, armed: &Armed) -> bool {
        let real = match target {
            Target::File { path }
                if path
                    .symlink_metadata()
                    .is_ok_and(|m| m.file_type().is_symlink()) =>
            {
                std::fs::canonicalize(path).ok()
            }
            _ => None,
        };
        if real == self.real && (real.is_none() || self.wd.is_some()) {
            return false;
        }
        if let Some(wd) = self.wd.take()
            && !armed.holds(&wd)
        {
            let _ = ino.watches().remove(wd);
        }
        self.wd = real
            .as_deref()
            .and_then(Path::parent)
            .and_then(|d| ino.watches().add(d, direct_mask()).ok());
        let moved = real != self.real;
        self.real = real;
        moved
    }

    /// Whether an event on [`Link::wd`] is a write of the file the link resolves to.
    fn triggers(&self, mask: EventMask, name: Option<&OsStr>) -> bool {
        name.is_some()
            && name == self.real.as_deref().and_then(Path::file_name)
            && mask.intersects(EventMask::CLOSE_WRITE | EventMask::MOVED_TO)
    }
}

/// The watcher's state.
struct Watch {
    target: Target,
    /// The directory the watch belongs on ([`Target::dir`]).
    dir: PathBuf,
    armed: Armed,
    link: Link,
}

impl Watch {
    /// Takes one batch of events: re-arms and follows the link as they ask. `true` when one
    /// of them triggers a reload.
    fn absorb(&mut self, ino: &mut Inotify, events: Events<'_>) -> bool {
        let mut fire = false;
        let mut rearm = false;
        for ev in events {
            if self.link.wd.as_ref() == Some(&ev.wd) {
                if ev.mask.contains(EventMask::IGNORED) {
                    // Its directory is gone; the next `follow` watches it again if it can.
                    self.link.wd = None;
                } else if self.link.triggers(ev.mask, ev.name) {
                    fire = true;
                }
            }
            scan_event(
                &self.target,
                &self.armed,
                ev.wd,
                ev.mask,
                ev.name,
                &mut fire,
                &mut rearm,
            );
        }
        if rearm {
            if self.link.wd.as_ref().is_some_and(|wd| self.armed.holds(wd)) {
                // Re-arming removes the shared descriptor.
                self.link.wd = None;
            }
            rearm_now(ino, &mut self.armed, &self.dir, &mut fire);
        }
        let moved = self.link.follow(ino, &self.target, &self.armed);
        fire || moved
    }
}

/// Runs the watcher on the calling thread; `notify` is called once per debounced reload.
/// Returns only when inotify itself fails.
pub fn run(target: Target, notify: impl Fn()) -> std::io::Result<()> {
    let mut ino = Inotify::init()?;
    let dir = target.dir();
    let mut w = Watch {
        armed: arm(&mut ino, &dir),
        dir,
        target,
        link: Link::default(),
    };
    w.link.follow(&mut ino, &w.target, &w.armed);
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let events = ino.read_events_blocking(&mut buf)?;
        if !w.absorb(&mut ino, events) {
            continue;
        }
        // Debounce: the window starts at the first triggering event, and later events do
        // not extend it. They are taken (a re-arm still happens) and absorbed into the one
        // reload that follows.
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
                Ok(_) => match ino.read_events(&mut buf) {
                    Ok(events) => {
                        let _absorbed = w.absorb(&mut ino, events);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e),
                },
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
            // If inotify fails, the watcher stops; SIGUSR1 still reloads (TDR-1 7.2).
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
