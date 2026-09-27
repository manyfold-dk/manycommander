#![forbid(unsafe_code)]
//! The keymap (design section 8).
//!
//! Ownership rule: when the command line is empty, panel bindings apply. When it holds
//! text, the line-editing keys go to the line, and the panel keeps only cursor movement
//! (`Up`, `Down`, `PgUp`, `PgDn`) and the F-keys. `Esc` clears the line. No binding uses a
//! chord that the Omarchy terminals (Ghostty, foot, Alacritty, Kitty) or Hyprland bind by
//! default; the T11 audit in the plan lists what they bind.

use crate::panel::sort::SortKey;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    // Cursor and panels (always active).
    Up,
    Down,
    PageUp,
    PageDown,
    SwitchPanel,
    MarkAndDown,
    MarkGlob,
    UnmarkGlob,
    InvertMarks,
    QuickSearch,
    ToggleHidden,
    Reread,
    HistoryBack,
    HistoryForward,
    Parent,
    InsertName,
    InsertPath,
    ShowOutput,
    Sort(SortKey),
    Help,
    View,
    Edit,
    EditNew,
    Copy,
    Move,
    Rename,
    Mkdir,
    Trash,
    Delete,
    Quit,
    // Line empty.
    Enter,
    First,
    Last,
    MarkSpace,
    MarkAll,
    SwapPanels,
    CloseTab,
    Escape,
    // Line has text (or a printable key).
    LineChar(char),
    LineRun,
    LineBackspace,
    LineDelete,
    LineLeft,
    LineRight,
    LineHome,
    LineEnd,
    LineKillStart,
    LineKillEnd,
    LineKillWord,
    LineClear,
    HistoryPrev,
    HistoryNext,
    // M2 tabs.
    NewTab,
    PrevTab,
    NextTab,
    GotoTab(u8),
    None,
}

/// Maps a key press to an action, given whether the command line is empty.
pub fn map(k: KeyEvent, line_empty: bool) -> Action {
    use Action::*;
    use KeyCode as K;
    let m = k.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL);
    let alt = m.contains(KeyModifiers::ALT);
    let shift = m.contains(KeyModifiers::SHIFT);

    // Always active.
    match k.code {
        K::Up if alt => return Parent,
        K::Up => return Up,
        K::Down => return Down,
        K::PageUp if alt => return PrevTab,
        K::PageDown if alt => return NextTab,
        K::PageUp => return PageUp,
        K::PageDown => return PageDown,
        K::Tab if !ctrl && !alt => return SwitchPanel,
        K::Insert => return MarkAndDown,
        K::F(1) => return Help,
        K::F(3) if ctrl => return Sort(SortKey::Name),
        K::F(4) if ctrl => return Sort(SortKey::Ext),
        K::F(5) if ctrl => return Sort(SortKey::Size),
        K::F(6) if ctrl => return Sort(SortKey::Mtime),
        K::F(3) => return View,
        K::F(4) if shift => return EditNew,
        K::F(4) => return Edit,
        K::F(5) => return Copy,
        K::F(6) if shift => return Rename,
        K::F(6) => return Move,
        K::F(7) => return Mkdir,
        K::F(8) if shift => return Delete,
        K::F(8) => return Trash,
        K::F(10) => return Quit,
        K::Char('x') if alt && !ctrl => return Quit,
        K::Char('=') if alt => return MarkGlob,
        K::Char('-') if alt => return UnmarkGlob,
        K::Char('*') if alt => return InvertMarks,
        K::Char('.') if alt => return ToggleHidden,
        K::Char('p') if alt => return InsertPath,
        // Ctrl+Alt+digit; Alt+digit also works where the terminal leaves it alone.
        K::Char(c @ '1'..='9') if alt => return GotoTab(c as u8 - b'0'),
        K::Left if alt => return HistoryBack,
        K::Right if alt => return HistoryForward,
        K::Enter if alt => return InsertName,
        K::Char('s') if ctrl => return QuickSearch,
        K::Char('r') if ctrl => return Reread,
        K::Char('o') if ctrl => return ShowOutput,
        K::Char('t') if ctrl => return NewTab,
        K::Char('p') if ctrl => return HistoryPrev,
        K::Char('n') if ctrl => return HistoryNext,
        _ => {}
    }

    if line_empty {
        return match k.code {
            K::Enter => Enter,
            K::Backspace => Parent,
            K::Char('h') if ctrl => Parent,
            K::Home => First,
            K::End => Last,
            K::Char(' ') if !ctrl && !alt => MarkSpace,
            K::Char('a') if ctrl => MarkAll,
            K::Char('u') if ctrl => SwapPanels,
            K::Char('w') if ctrl => CloseTab,
            K::Esc => Escape,
            K::Char(c) if !ctrl && !alt => LineChar(c),
            _ => None,
        };
    }
    match k.code {
        K::Enter => LineRun,
        K::Backspace => LineBackspace,
        K::Char('h') if ctrl => LineBackspace,
        K::Delete => LineDelete,
        K::Left => LineLeft,
        K::Right => LineRight,
        K::Home => LineHome,
        K::End => LineEnd,
        K::Char('a') if ctrl => LineHome,
        K::Char('e') if ctrl => LineEnd,
        K::Char('u') if ctrl => LineKillStart,
        K::Char('k') if ctrl => LineKillEnd,
        K::Char('w') if ctrl => LineKillWord,
        K::Esc => LineClear,
        K::Char(c) if !ctrl && !alt => LineChar(c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn ownership_rule() {
        let none = KeyModifiers::NONE;
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(map(k(KeyCode::Backspace, none), true), Action::Parent);
        assert_eq!(
            map(k(KeyCode::Backspace, none), false),
            Action::LineBackspace
        );
        assert_eq!(map(k(KeyCode::Char('u'), ctrl), true), Action::SwapPanels);
        assert_eq!(
            map(k(KeyCode::Char('u'), ctrl), false),
            Action::LineKillStart
        );
        assert_eq!(map(k(KeyCode::Char('a'), ctrl), true), Action::MarkAll);
        assert_eq!(map(k(KeyCode::Char('a'), ctrl), false), Action::LineHome);
        assert_eq!(map(k(KeyCode::Char('w'), ctrl), true), Action::CloseTab);
        assert_eq!(
            map(k(KeyCode::Char('w'), ctrl), false),
            Action::LineKillWord
        );
        assert_eq!(map(k(KeyCode::Char(' '), none), true), Action::MarkSpace);
        assert_eq!(
            map(k(KeyCode::Char(' '), none), false),
            Action::LineChar(' ')
        );
        assert_eq!(map(k(KeyCode::Up, none), false), Action::Up);
        assert_eq!(map(k(KeyCode::F(5), none), false), Action::Copy);
        assert_eq!(map(k(KeyCode::Enter, none), true), Action::Enter);
        assert_eq!(map(k(KeyCode::Enter, none), false), Action::LineRun);
        assert_eq!(map(k(KeyCode::Char('h'), ctrl), true), Action::Parent);
        assert_eq!(
            map(k(KeyCode::Char('x'), KeyModifiers::ALT), true),
            Action::Quit
        );
        assert_eq!(
            map(k(KeyCode::F(8), KeyModifiers::SHIFT), true),
            Action::Delete
        );
        assert_eq!(
            map(k(KeyCode::F(3), ctrl), true),
            Action::Sort(SortKey::Name)
        );
        assert_eq!(
            map(k(KeyCode::Enter, KeyModifiers::ALT), true),
            Action::InsertName
        );
        assert_eq!(
            map(k(KeyCode::Enter, ctrl), false),
            Action::LineRun,
            "Ctrl+Enter is the terminal's"
        );
        assert_eq!(
            map(k(KeyCode::Up, KeyModifiers::ALT), false),
            Action::Parent
        );
        assert_eq!(
            map(k(KeyCode::PageUp, ctrl), true),
            Action::PageUp,
            "Ctrl+PgUp is the terminal's"
        );
        assert_eq!(
            map(k(KeyCode::Down, KeyModifiers::SHIFT), true),
            Action::Down
        );
        assert_eq!(
            map(k(KeyCode::Char('3'), ctrl | KeyModifiers::ALT), true),
            Action::GotoTab(3)
        );
    }
}
