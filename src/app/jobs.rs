#![forbid(unsafe_code)]
//! Job wiring (design 3.1, 4.4): the channel-backed [`Interaction`] and the worker thread.
//! One job runs at a time. The worker runs under `catch_unwind` (`run_guarded`), so a
//! panic ends as a failed report.

use super::event::{Event, JobEvent};
use crate::fsops::job::{JobSpec, run_guarded};
use crate::fsops::question::{Answer, Interaction, Progress, Question};
use crate::fsops::sys::Sys;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Sender, channel};

/// The worker's side of the UI: questions block until the dialog answers.
pub struct ChannelUi {
    pub tx: Sender<Event>,
}

impl Interaction for ChannelUi {
    fn ask(&mut self, q: Question) -> Answer {
        let (rtx, rrx) = channel();
        if self.tx.send(Event::Job(JobEvent::Ask(q, rtx))).is_err() {
            return Answer::Cancel;
        }
        rrx.recv().unwrap_or(Answer::Cancel)
    }

    fn progress(&mut self, p: Progress) {
        let _ = self.tx.send(Event::Job(JobEvent::Progress(p)));
    }
}

/// Starts the worker for `spec`; `cancel` is the job's flag.
pub fn spawn(spec: JobSpec, tx: Sender<Event>, cancel: Arc<AtomicBool>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("job".into())
        // Traversal recurses once per directory level.
        .stack_size(64 << 20)
        .spawn(move || {
            let sys = Sys::new(cancel);
            let mut ui = ChannelUi { tx: tx.clone() };
            let report = run_guarded(spec, &sys, &mut ui);
            let _ = tx.send(Event::Job(JobEvent::Done(report)));
        })?;
    Ok(())
}
