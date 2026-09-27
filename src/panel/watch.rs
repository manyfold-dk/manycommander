#![forbid(unsafe_code)]
//! Panel watchers (design section 5, NFR-RES): one inotify instance on its own thread,
//! one watch per visible panel, debounced 200 ms. `inotify_add_watch` resolves a path, so
//! it runs here and never on the UI thread (P-1). The thread blocks in `poll` when idle.

use inotify::{Inotify, WatchDescriptor, WatchMask};
use rustix::fd::{AsFd, OwnedFd};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

pub const DEBOUNCE: Duration = Duration::from_millis(200);

pub struct PanelWatcher {
    cmd: Sender<(usize, Option<PathBuf>)>,
    wake: OwnedFd,
}

impl PanelWatcher {
    /// Starts the watcher; `notify(slot)` runs once per debounced change.
    pub fn spawn(notify: impl Fn(usize) + Send + 'static) -> std::io::Result<PanelWatcher> {
        let (tx, rx) = channel();
        let (wake_r, wake_w) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)?;
        let ino = Inotify::init()?;
        std::thread::Builder::new()
            .name("panel-watch".into())
            .spawn(move || run(ino, rx, wake_r, notify))?;
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

fn run(
    mut ino: Inotify,
    rx: Receiver<(usize, Option<PathBuf>)>,
    wake: OwnedFd,
    notify: impl Fn(usize),
) {
    let mut slots: HashMap<usize, WatchDescriptor> = HashMap::new();
    let mut pending: HashMap<usize, Instant> = HashMap::new();
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
                if let Some(wd) = slots.remove(&slot)
                    && !slots.values().any(|w| *w == wd)
                {
                    let _ = ino.watches().remove(wd);
                }
                pending.remove(&slot);
                if let Some(d) = dir
                    && let Ok(wd) = ino.watches().add(&d, mask())
                {
                    slots.insert(slot, wd);
                }
            }
        }
        if ino_ready {
            match ino.read_events(&mut buf) {
                Ok(events) => {
                    let deadline = Instant::now() + DEBOUNCE;
                    for ev in events {
                        for (slot, wd) in &slots {
                            if *wd == ev.wd || ev.mask.contains(inotify::EventMask::Q_OVERFLOW) {
                                pending.entry(*slot).or_insert(deadline);
                            }
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
            notify(s);
        }
    }
}
