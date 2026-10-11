#![forbid(unsafe_code)]
//! Panel watchers (design section 5, NFR-RES): one inotify instance on its own thread,
//! one watch per visible panel, debounced 200 ms. `inotify_add_watch` resolves a path, so
//! it runs here and never on the UI thread (P-1). The thread blocks in `poll` when idle.
//!
//! A watch belongs to an inode, a panel to a path. When the watched directory goes (it is
//! deleted, or moved away), the watch is armed again on the panel's path when the change
//! is reported: a directory created in its place (`rm -rf build && mkdir build`) is
//! watched from then on. Without one, the panel's refresh moves it to the nearest existing
//! ancestor, which the app watches then. A watch that cannot be added (the
//! `max_user_watches` limit) is reported once.

use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};
use rustix::fd::{AsFd, OwnedFd};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

pub const DEBOUNCE: Duration = Duration::from_millis(200);

pub struct PanelWatcher {
    cmd: Sender<(usize, Option<PathBuf>)>,
    wake: OwnedFd,
}

impl PanelWatcher {
    /// Starts the watcher; `notify(slot)` runs once per debounced change, and
    /// `failed(text)` once, for the first watch that cannot be added.
    pub fn spawn(
        notify: impl Fn(usize) + Send + 'static,
        failed: impl Fn(String) + Send + 'static,
    ) -> std::io::Result<PanelWatcher> {
        let (tx, rx) = channel();
        let (wake_r, wake_w) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)?;
        let ino = Inotify::init()?;
        std::thread::Builder::new()
            .name("panel-watch".into())
            .spawn(move || run(ino, rx, wake_r, notify, failed))?;
        Ok(PanelWatcher {
            cmd: tx,
            wake: wake_w,
        })
    }

    /// Watches `dir` for `slot` (`None`: stop watching for it).
    pub fn set(&self, slot: usize, dir: Option<PathBuf>) {
        if self.cmd.send((slot, dir)).is_ok() {
            let _ = rustix::io::write(&self.wake, b"x");
        }
    }
}

fn mask() -> WatchMask {
    WatchMask::CREATE
        | WatchMask::DELETE
        | WatchMask::MOVED_FROM
        | WatchMask::MOVED_TO
        | WatchMask::CLOSE_WRITE
        | WatchMask::ATTRIB
        | WatchMask::DELETE_SELF
        | WatchMask::MOVE_SELF
}

/// Adds the watch of `dir`; the first failure is reported through `failed`.
fn add(
    ino: &mut Inotify,
    dir: &Path,
    reported: &mut bool,
    failed: &impl Fn(String),
) -> Option<WatchDescriptor> {
    match ino.watches().add(dir, mask()) {
        Ok(wd) => Some(wd),
        // A directory that is gone needs no watch: the panel's refresh leaves it.
        Err(e) if e.raw_os_error() == Some(rustix::io::Errno::NOENT.raw_os_error()) => None,
        Err(e) => {
            if !std::mem::replace(reported, true) {
                failed(format!(
                    "{}: changes are not watched ({e}); Ctrl+R re-reads",
                    dir.display()
                ));
            }
            None
        }
    }
}

fn run(
    mut ino: Inotify,
    rx: Receiver<(usize, Option<PathBuf>)>,
    wake: OwnedFd,
    notify: impl Fn(usize),
    failed: impl Fn(String),
) {
    let mut slots: HashMap<usize, (WatchDescriptor, PathBuf)> = HashMap::new();
    // Slots whose directory went: watched again on their path when the change is reported.
    let mut gone: HashMap<usize, PathBuf> = HashMap::new();
    let mut pending: HashMap<usize, Instant> = HashMap::new();
    let mut reported = false;
    let mut buf = vec![0u8; 16 * 1024];
    let mut drain = [0u8; 64];
    loop {
        let now = Instant::now();
        let timeout = pending
            .values()
            .min()
            .map(|d| d.saturating_duration_since(now));
        let ts = timeout.map(|t| rustix::fs::Timespec {
            tv_sec: t.as_secs() as i64,
            tv_nsec: t.subsec_nanos() as _,
        });
        let (ino_ready, wake_ready) = {
            let mut fds = [
                rustix::event::PollFd::new(&ino, rustix::event::PollFlags::IN),
                rustix::event::PollFd::new(&wake, rustix::event::PollFlags::IN),
            ];
            match rustix::event::poll(&mut fds, ts.as_ref()) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(_) => return,
            }
            (!fds[0].revents().is_empty(), !fds[1].revents().is_empty())
        };
        if wake_ready {
            let _ = rustix::io::read(wake.as_fd(), &mut drain);
            while let Ok((slot, dir)) = rx.try_recv() {
                if let Some((wd, _)) = slots.remove(&slot)
                    && !slots.values().any(|(w, _)| *w == wd)
                {
                    let _ = ino.watches().remove(wd);
                }
                gone.remove(&slot);
                pending.remove(&slot);
                if let Some(d) = dir
                    && let Some(wd) = add(&mut ino, &d, &mut reported, &failed)
                {
                    slots.insert(slot, (wd, d));
                }
            }
        }
        if ino_ready {
            match ino.read_events(&mut buf) {
                Ok(events) => {
                    let deadline = Instant::now() + DEBOUNCE;
                    let mut lost = Vec::new();
                    for ev in events {
                        let overflow = ev.mask.contains(EventMask::Q_OVERFLOW);
                        // The watched directory itself went: deleted (the kernel drops the
                        // watch) or moved away (the watch follows it elsewhere).
                        let went = ev.mask.intersects(
                            EventMask::IGNORED | EventMask::DELETE_SELF | EventMask::MOVE_SELF,
                        );
                        for (slot, (wd, _)) in &slots {
                            if *wd == ev.wd || overflow {
                                pending.entry(*slot).or_insert(deadline);
                            }
                            if *wd == ev.wd && went {
                                lost.push(*slot);
                            }
                        }
                    }
                    for slot in lost {
                        if let Some((wd, path)) = slots.remove(&slot) {
                            if !slots.values().any(|(w, _)| *w == wd) {
                                let _ = ino.watches().remove(wd);
                            }
                            gone.insert(slot, path);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return,
            }
        }
        let now = Instant::now();
        let due: Vec<usize> = pending
            .iter()
            .filter(|(_, d)| **d <= now)
            .map(|(s, _)| *s)
            .collect();
        for s in due {
            pending.remove(&s);
            if let Some(path) = gone.remove(&s)
                && let Some(wd) = add(&mut ino, &path, &mut reported, &failed)
            {
                slots.insert(s, (wd, path));
            }
            notify(s);
        }
    }
}
