#![forbid(unsafe_code)]
//! The open SFTP sessions (P3 5.7, NFR-RES): at most [`MAX_SESSIONS`], keyed by the server
//! as typed (`(user, host, port)`, P3 5.1).
//!
//! A session is in use while anything besides the pool holds its [`RemoteProvider`]: a
//! visible tab that shows a place on it (a hidden tab releases it, P3 2.2), a load, a job, a
//! view copy or a preview. A session that is not in use stays open for a quick return, so
//! going back in the history or showing a hidden tab rarely reconnects. Opening a fifth
//! closes the least recently used session that is not in use; when all four are in use,
//! the connect is refused with [`FULL`]. `Ctrl+T` on a remote tab shares its session. A lost
//! session leaves the pool (its child was reaped when it was lost) and stays lost until the
//! user reconnects; manycommander never reconnects on its own.
//!
//! The pool lives on the UI thread and makes no I/O: it reads lost flags and reference
//! counts. Dropping the last reference to a session closes it on a helper thread.

use super::provider::RemoteProvider;
use crate::provider::Target;
use std::sync::Arc;
use std::time::Instant;

/// At most this many SFTP sessions are open (P3 2.6, 5.7).
pub const MAX_SESSIONS: usize = 4;

/// What a fifth connect says while four sessions are in use (P3 5.7).
pub const FULL: &str = "4 connections are open; close a remote tab";

struct Open {
    remote: Arc<RemoteProvider>,
    used: Instant,
}

/// The open sessions (P3 5.7).
#[derive(Default)]
pub struct Pool {
    open: Vec<Open>,
}

impl Pool {
    /// Lets go of lost sessions.
    fn prune(&mut self) {
        self.open.retain(|o| o.remote.lost().is_none());
    }

    /// The usable session for `t`, marked as used now.
    pub fn get(&mut self, t: &Target) -> Option<Arc<RemoteProvider>> {
        self.prune();
        let o = self.open.iter_mut().find(|o| o.remote.target() == t)?;
        o.used = Instant::now();
        Some(o.remote.clone())
    }

    /// Before a connect (P3 5.7): at [`MAX_SESSIONS`] the least recently used session that
    /// is not in use closes; `Err(FULL)` when all of them are in use. Returns the number of
    /// the session it closed.
    pub fn make_room(&mut self) -> Result<Option<u64>, &'static str> {
        self.prune();
        if self.open.len() < MAX_SESSIONS {
            return Ok(None);
        }
        let idle = (0..self.open.len())
            .filter(|&i| !self.in_use(i))
            .min_by_key(|&i| self.open[i].used)
            .ok_or(FULL)?;
        let o = self.open.remove(idle);
        let n = o.remote.id();
        tracing::info!(session = n, "sftp: the least recently used session closes");
        // Its last reference: the session closes on a helper thread.
        drop(o);
        Ok(Some(n))
    }

    /// Whether anything besides the pool holds session `i`.
    fn in_use(&self, i: usize) -> bool {
        Arc::strong_count(&self.open[i].remote) > 1
    }

    /// A new session. The caller made room first.
    pub fn insert(&mut self, remote: Arc<RemoteProvider>) {
        self.open.push(Open {
            remote,
            used: Instant::now(),
        });
    }

    /// The open sessions' numbers, least recently used first.
    pub fn numbers(&self) -> Vec<u64> {
        let mut v: Vec<(Instant, u64)> =
            self.open.iter().map(|o| (o.used, o.remote.id())).collect();
        v.sort();
        v.into_iter().map(|(_, n)| n).collect()
    }

    /// How many sessions the pool holds, lost ones included until the next lookup.
    pub fn len(&self) -> usize {
        self.open.len()
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    /// manycommander's exit: each session closes ssh's stdin and waits for it (P3 5.2).
    pub fn close_all(&mut self) {
        for o in self.open.drain(..) {
            o.remote.session().close_wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::Session;

    fn remote(host: &str) -> Arc<RemoteProvider> {
        Arc::new(RemoteProvider::new(
            Session::detached(),
            Target {
                user: None,
                host: host.into(),
                port: None,
            },
        ))
    }

    fn target(host: &str) -> Target {
        Target {
            user: None,
            host: host.into(),
            port: None,
        }
    }

    #[test]
    fn lost_sessions_leave_the_pool() {
        let mut p = Pool::default();
        // A detached session counts as lost.
        p.insert(remote("a"));
        assert!(p.get(&target("a")).is_none());
        assert!(p.is_empty());
    }
}
