#![forbid(unsafe_code)]
//! Deterministic fault injection for the file-operation engine (plan: "Failpoints").
//!
//! A registry is an `Arc<Failpoints>` carried by the job's [`Sys`](super::sys::Sys), so an
//! injection reaches the worker thread. Each named step (`copy.chunk`, `commit.rename`,
//! `move.syncfs`, ...) keeps a hit counter that tests assert, so an injection is provably
//! reached. The registry type always exists; only builds with the `failpoints` feature
//! consult it, so release builds pay nothing.

use rustix::io::Errno;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// What an armed step does when its trigger fires. Every action runs before the real call.
#[derive(Clone)]
pub enum Action {
    /// Fail the step with this errno instead of making the call.
    Errno(Errno),
    /// Set the job's cancel flag, then make the call.
    Cancel,
    /// Run the closure, then make the call.
    Call(Arc<dyn Fn() + Send + Sync>),
    /// Run the closure, then fail with the errno.
    CallThenErrno(Arc<dyn Fn() + Send + Sync>, Errno),
}

/// When an armed step fires, counted in hits of that step (1-based).
#[derive(Clone, Copy, Debug)]
pub enum Trigger {
    /// Exactly on the nth hit.
    Nth(u64),
    /// On the nth hit and every later one.
    From(u64),
    /// On every hit.
    Always,
}

impl Trigger {
    fn fires(self, hit: u64) -> bool {
        match self {
            Trigger::Nth(n) => hit == n,
            Trigger::From(n) => hit >= n,
            Trigger::Always => true,
        }
    }
}

struct Rule {
    step: String,
    trigger: Trigger,
    action: Action,
}

/// The per-job failpoint registry.
#[derive(Default)]
pub struct Failpoints {
    rules: Mutex<Vec<Rule>>,
    hits: Mutex<HashMap<String, u64>>,
}

impl Failpoints {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Arms `step`: when `trigger` fires, `action` runs.
    pub fn arm(&self, step: &str, trigger: Trigger, action: Action) {
        self.rules.lock().unwrap().push(Rule {
            step: step.to_owned(),
            trigger,
            action,
        });
    }

    /// How often `step` was reached, armed or not.
    pub fn hits(&self, step: &str) -> u64 {
        self.hits.lock().unwrap().get(step).copied().unwrap_or(0)
    }

    /// Every step reached so far with its count, for sweeps that enumerate step boundaries.
    pub fn all_hits(&self) -> HashMap<String, u64> {
        self.hits.lock().unwrap().clone()
    }

    /// Counts a hit of `step` and applies the first armed rule that fires.
    pub fn check(&self, step: &str, cancel: &AtomicBool) -> Result<(), Errno> {
        let hit = {
            let mut hits = self.hits.lock().unwrap();
            let n = hits.entry(step.to_owned()).or_insert(0);
            *n += 1;
            *n
        };
        let action = {
            let rules = self.rules.lock().unwrap();
            rules
                .iter()
                .find(|r| r.step == step && r.trigger.fires(hit))
                .map(|r| r.action.clone())
        };
        match action {
            None => Ok(()),
            Some(Action::Errno(e)) => Err(e),
            Some(Action::Cancel) => {
                cancel.store(true, Ordering::SeqCst);
                Ok(())
            }
            Some(Action::Call(f)) => {
                f();
                Ok(())
            }
            Some(Action::CallThenErrno(f, e)) => {
                f();
                Err(e)
            }
        }
    }
}
