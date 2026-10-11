# ADR 0011: A keymap that the terminals let through

## Status

Accepted -- 2026-09-27. Amended the same day: tabs are selected with `Ctrl+1` to `Ctrl+9`.
Amended 2026-10-04: the keyboard protocol push also carries `REPORT_ALTERNATE_KEYS`.

## Context

manycommander follows the Total Commander keymap: F-keys for the verbs, `Ctrl+PgUp` for the
parent directory, `Shift+Down` to mark, `Ctrl+PgDn` to open an archive. It runs inside a
terminal emulator, inside a Wayland compositor, and sometimes inside tmux. Each of those
layers can claim a chord before manycommander sees it:

- Ghostty binds `Shift+Down` (selection), `Ctrl+PgUp`/`Ctrl+PgDn` (its own tabs),
  `Ctrl+Enter` (fullscreen) and `Alt+1` to `Alt+9` (its own tabs).
- kitty and Ghostty bind `Ctrl+Tab`, `Ctrl+Shift+Tab` and `Ctrl+Shift+Enter`; Kitty binds
  `Ctrl+Shift+F5` (reload its config), which is Total Commander's link key.
- Hyprland on Omarchy binds `SUPER` chords, `F9`, `Alt+Tab` variants and `Alt+Print`.
- Omarchy's tmux configuration binds `M-Left`, `M-Right`, `M-Up`, `M-Down`, `M-Enter` and
  `M-1` to `M-9` in its root table.

Legacy terminal encoding also hides some chords: `Ctrl+M` is `Enter`, `Ctrl+3` is `Esc`,
`Ctrl+2` is NUL, `Alt+[` is the CSI introducer, and `Ctrl+F3` is `CSI 1;5 R`, the same bytes
as a cursor position report.

A chord that a layer above claims either does nothing or does something else, silently.

## Decision

1. **No default binding uses a chord** that Ghostty, foot, Alacritty, Kitty, the Omarchy
   Hyprland bindings or tmux's default root table bind by default. The audit that checks
   this is part of the design and is repeated whenever a key is added
   ([TDR-1 appendix B](../tdr/0001-core.md#appendix-b-terminal-key-binding-audit)).
2. **Replacements** where the Total Commander chord is taken: `Insert` marks and moves down
   (`Shift+Down` is dropped), `Alt+Up` goes to the parent, `Alt+Enter` inserts the name into
   the command line, `Ctrl+1` to `Ctrl+9` select a tab (`Alt+digit` still works where it
   arrives), `Alt+O` opens a file as an archive. One chord per action: a replaced chord is
   not kept as a hidden alias.
3. **The kitty keyboard protocol** is pushed when the terminal supports it, with
   `DISAMBIGUATE_ESCAPE_CODES` and `REPORT_ALTERNATE_KEYS`. The first tells `Ctrl+M` from
   `Enter`, `Ctrl+F3` from a cursor report, and `Ctrl+digit` from control bytes. The second
   makes the terminal report the shifted character, so `Alt+*` and `Alt+=` act on every
   keyboard layout.
4. **Limits inside tmux are documented, not worked around.** tmux runs manycommander
   without the keyboard protocol unless its `extended-keys` reach the terminal; the keys page
   names `Backspace`, `Alt+PgUp`/`Alt+PgDn` and the chords that need `extended-keys`.

## Rationale

- A Total Commander user's muscle memory matters, but a chord that the terminal eats is
  worse than a different chord: the user presses it and nothing happens, or their terminal
  opens a tab.
- The four terminals Omarchy ships all support the kitty protocol, so relying on it for
  `Ctrl+M`, `Ctrl+F3` and `Ctrl+1` to `Ctrl+9` costs nothing there. `Shift+F*`, `Ctrl+F*`
  (except `Ctrl+F3`) and `Alt+Up` use the xterm modifier encoding and work without it.
- `REPORT_ALTERNATE_KEYS` came from a manual check on a Spanish layout: with
  `DISAMBIGUATE_ESCAPE_CODES` alone, Ghostty reported `Alt+*` as `Alt+Shift++`, and the
  binding did nothing. Earlier passes of the same check had gone through tmux (no protocol)
  or `wtype` (no real `Shift`), which hid the defect.

## Consequences

- Some Total Commander chords differ, and the F1 help and the keys page list the
  replacements.
- `tests/ui_keys.rs` sends every chord as the bytes a terminal emits, in legacy encoding and
  with the protocol, and replays bytes recorded from Ghostty and foot on US and Spanish
  layouts (`tests/fixtures/keys/`, recorded by `scripts/fixtures/record-keys.py` in a nested
  Hyprland).
- Inside Omarchy's tmux, `Alt+Left`/`Alt+Right` (history), `Alt+Up`, `Alt+Enter` and
  `Alt+digit` stay with tmux, and `Ctrl+1` to `Ctrl+9` and `Ctrl+M` arrive as legacy bytes.
  Users who need them there turn on tmux's `extended-keys`.
- Every new binding needs an audit row before it ships.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Keep Total Commander's chords and tell users to unbind them in their terminal | Changes the user's terminal configuration for one program; breaks the terminal's own tabs |
| A configurable keymap | Not designed; the default keymap must work out of the box either way |
| `Ctrl+Alt+1` to `Ctrl+Alt+9` for tabs | Used first; `Ctrl+digit` is shorter, and Ghostty and foot claim only `Ctrl+0` among `Ctrl+digit` |
| Keep old chords as hidden aliases | Two chords per action make the help and the audit inexact |

## References

- [TDR-1 8 Keymap](../tdr/0001-core.md#8-keymap)
- [TDR-1 appendix B: terminal key-binding audit](../tdr/0001-core.md#appendix-b-terminal-key-binding-audit)
- [TDR-2 10 Keymap](../tdr/0002-navigation-and-bulk-tools.md#10-keymap),
  [TDR-3 6 Keymap](../tdr/0003-archives-quick-view-and-sftp.md#6-keymap)
- [ADR 0012: Typing filters the panel](0012-typing-filters-the-panel.md)
