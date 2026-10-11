#![forbid(unsafe_code)]
//! The compare's events and its status row (TDR-2 7). The marks apply only when both panels
//! still show the listings the request was made from; a cancelled or replaced compare's
//! events are dropped. The form that starts a compare is in [`super::forms`].

use super::App;
use super::event::Effect;
use crate::compare::{CompareMsg, Marks, Mode};

impl App {
    pub(super) fn on_compare(&mut self, m: CompareMsg) -> Vec<Effect> {
        let current = self.compare.as_ref().map(|c| c.id);
        match m {
            CompareMsg::Progress { id, progress } if Some(id) == current => {
                if let Some(c) = self.compare.as_mut() {
                    c.progress = Some(progress);
                }
            }
            CompareMsg::Marks {
                id,
                listings,
                marks,
            } if Some(id) == current => self.apply_compare(listings, marks),
            CompareMsg::Done { id, error } if Some(id) == current => {
                self.compare = None;
                if let Some(e) = error {
                    self.warn(format!("compare: {e}"));
                }
            }
            // A cancelled or replaced compare.
            _ => {}
        }
        Vec::new()
    }

    /// Applies compare marks (TDR-2 7) when both panels still show the listings `(slot,
    /// generation)` the request was made from; otherwise nothing is marked. Existing marks
    /// in both panels are cleared first.
    fn apply_compare(&mut self, listings: [(usize, u64); 2], marks: Marks) {
        let same = (0..2).all(|s| {
            let p = self.sides[s].panel();
            p.slot == listings[s].0 && p.listing_generation() == Some(listings[s].1)
        });
        if !same {
            self.warn("the directories changed; compare again");
            return;
        }
        self.sides[0].panel_mut().replace_marks(&marks.left);
        self.sides[1].panel_mut().replace_marks(&marks.right);
        self.say(marks.summary.to_string());
    }

    /// Progress text of a running compare for the status row.
    pub fn compare_line(&self) -> Option<String> {
        let c = self.compare.as_ref()?;
        let mut s = match c.mode {
            Mode::DateSize => "compare by date and size".to_string(),
            Mode::Content => "compare by content".to_string(),
        };
        match &c.progress {
            Some(p) => {
                s += &format!(": {}/{} pairs", p.pairs_done, p.pairs_total);
                if p.bytes_total > 0 {
                    let pct = p.bytes_done as f64 * 100.0 / p.bytes_total as f64;
                    s += &format!(", {pct:.0}%");
                }
            }
            None => s += ": running",
        }
        s += "   Esc: cancel";
        Some(s)
    }
}
