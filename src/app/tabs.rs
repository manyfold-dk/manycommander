#![forbid(unsafe_code)]
//! Tabs (TDR-1 5): `Ctrl+T` opens a tab on the current place, `Ctrl+W` closes one, and the
//! switch keys show another. A hidden tab releases its listing and its watch and reloads
//! when shown (E-31, NFR-RES); a hidden archive or server tab reopens through the index
//! cache or the session pool (TDR-3 2.2, 5.7).

use super::App;
use super::event::Effect;
use super::keys::Action;
use crate::panel::{Panel, Place, Record};

impl App {
    pub(super) fn tab_action(&mut self, a: Action) -> Vec<Effect> {
        let side = self.active;
        let mut fx = Vec::new();
        let old_slot = self.sides[side].panel().slot;
        let n = self.sides[side].tabs.len();
        match a {
            Action::NewTab => {
                let slot = self.new_slot();
                let cur = self.sides[side].panel();
                let dir = cur.dir.clone();
                // A new tab of a remote tab shares its session (TDR-3 5.7).
                let remote = cur.remote().filter(|v| !v.lost()).cloned();
                let mut p = Panel::new(slot, dir.clone());
                p.sort = cur.sort;
                p.show_hidden = cur.show_hidden;
                self.hide_tab(side, &mut fx);
                let s = &mut self.sides[side];
                s.tabs.insert(s.active + 1, p);
                s.active += 1;
                match remote {
                    Some(v) => {
                        let at = v.remote_dir();
                        fx.extend(self.show_remote(side, v.session, at, None, Record::No));
                    }
                    None => fx.extend(self.load(side, dir, None, false)),
                }
                // A new tab of the same directory is not a visit.
                self.mark_visit(slot, false);
                return fx;
            }
            Action::CloseTab => {
                if n <= 1 {
                    return fx;
                }
                self.leave_results(side);
                self.abandon_load(side);
                let s = &mut self.sides[side];
                s.tabs.remove(s.active);
                if s.active >= s.tabs.len() {
                    s.active = s.tabs.len() - 1;
                }
                fx.push(Effect::Watch {
                    slot: old_slot,
                    dir: None,
                });
            }
            Action::PrevTab | Action::NextTab | Action::GotoTab(_) => {
                let cur = self.sides[side].active;
                let target = match a {
                    Action::PrevTab => (cur + n - 1) % n,
                    Action::NextTab => (cur + 1) % n,
                    Action::GotoTab(k) => (k as usize - 1).min(n - 1),
                    _ => cur,
                };
                if target == cur {
                    return fx;
                }
                self.hide_tab(side, &mut fx);
                self.sides[side].active = target;
            }
            _ => return fx,
        }
        fx.extend(self.show_tab(side));
        fx
    }

    /// The active tab of `side` goes into the background: no watch, no listing.
    pub(super) fn hide_tab(&mut self, side: usize, fx: &mut Vec<Effect>) {
        let slot = self.sides[side].panel().slot;
        self.abandon_load(side);
        self.sides[side].panel_mut().release();
        fx.push(Effect::Watch { slot, dir: None });
    }

    /// The active tab of `side` comes to the front: reload it; the listing's completion
    /// adds the watch again ([`Panel::on_done`] reports a released tab's directory).
    fn show_tab(&mut self, side: usize) -> Vec<Effect> {
        // A released archive tab reopens through the index cache, a remote tab through the
        // pool (TDR-3 2.2, 5.7).
        if let Some(place) = self.sides[side].panel_mut().reopen.take() {
            let cursor = self.sides[side].panel().cursor_name().map(<[u8]>::to_vec);
            let remote = matches!(place, Place::Remote { .. });
            let fx = match place {
                place @ Place::Remote { .. } => {
                    self.open_remote_place(side, place, cursor, Record::No)
                }
                place => self.open_place(side, place, cursor, Record::No),
            };
            let p = self.sides[side].panel_mut();
            p.released = false;
            if fx.is_empty() && remote {
                // A refused reconnect: the tab shows its local directory.
                let dir = p.dir.clone();
                return self.load_ex(side, dir, None, true, Record::No);
            }
            return fx;
        }
        let p = self.sides[side].panel();
        if p.loading.is_some() {
            return Vec::new();
        }
        if !p.loaded_once {
            let dir = p.dir.clone();
            return self.load_ex(side, dir, None, true, Record::No);
        }
        self.refresh_slot(side)
    }
}
