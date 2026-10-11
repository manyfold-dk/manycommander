# ADR 0012: Typing filters the panel; the command line takes the focus with Ctrl+E

## Status

Accepted -- 2026-09-30.

## Context

In Total Commander, and in manycommander until this decision, a printable key that has no
binding goes to the command line under the panels. The command line then owns the
line-editing keys until it is empty again (TDR-1 8, the ownership rule).

In practice the most frequent typed action in a large directory is finding an entry, not
starting a shell command. manycommander already had two ways to find one, both behind a
chord: `Ctrl+S` jumps to the next name with a typed prefix, and `Ctrl+F` opens the quick
filter (TDR-2 4). Typing into the command line by accident had two costs. A stray letter
took over `Ctrl+A`, `Ctrl+U`, `Home` and `End` for line editing, and a stray `Enter`
afterwards ran the text as a shell command.

## Decision

1. The command line has a **focus**. Without it, a printable key other than `Space` opens
   the quick filter with that character as a new filter.
2. `Ctrl+E` gives the command line the focus. So does anything that puts text on it:
   `Alt+Enter`, `Alt+P`, `Ctrl+P`, a paste. A line that holds text always has the focus.
   Running the line (`Enter`), clearing it (`Esc`) and `Backspace` on an empty line give the
   focus back to the panel. The terminal cursor shows on the line only while it has the
   focus.
3. The quick filter becomes good enough to be the default action of a typed key:
   - it ignores case, with Unicode simple lowercase folding for a non-ASCII text;
   - it forgives typos: when no name contains the text, the names with the fewest edits
     show (one edit from 4 characters, two from 9), and the footer says "fuzzy";
   - the cursor goes to the first name that starts with the text;
   - `Enter` on the filter line keeps the filter and opens the entry under the cursor.
4. In the TDR-1 8 keymap tables, "line empty" reads "line without the focus" and "line has
   text" reads "line with the focus". On a focused but empty line, `Ctrl+D`, `Ctrl+F`,
   `Ctrl+M` and `Alt+O` leave the line and act.

## Rationale

- Narrowing a list by typing is what a keyboard user does most in a large directory, and
  the filter shows every match at once where a prefix jump shows one.
- A shell command is rarer and deliberate, so one extra chord in front of it is cheap.
  `Ctrl+E` is free in Ghostty, foot, Alacritty, Kitty and Hyprland, and legacy terminals send
  it as ENQ, which nothing else uses.
- A focus makes the ownership rule visible: the terminal cursor on the line says where the
  next key goes.
- The filter stays fast enough for a re-filter on every keystroke. Measured on the reference
  machine over 100k entries: substring 1.40 ms, first character 0.43 ms, glob 2.79 ms, the
  fuzzy tier over every entry 6.79 ms, against the 16 ms target of P-12; key-to-flush p99 on a
  pty 2.56 ms.

## Consequences

- This departs from the Total Commander tradition that the rest of the keymap follows. The
  F1 help, the keys page and the status row ("Ctrl+E: command line" beside the filter) say
  so.
- A printable key never reaches the shell by accident.
- `Space` keeps its meaning (mark), so a filter cannot start with a space.
- Tests that type a command press `Ctrl+E` first.
- A short text needs care in the fuzzy tier: a text of at most 5 characters never drops one
  of its own characters, because the rest would match any short piece of a name; and the
  tier waits for entries not yet sorted in, so a streaming listing never shows fuzzy results
  over a partial order.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Keep printable keys on the command line (Total Commander) | The common action needs a chord, and stray keys reach the shell |
| Type to jump (prefix search) instead of type to filter | Shows one match at a time; `Ctrl+S` already offers it |
| A modal "command mode" key such as `:` | A printable key would then be both a filter character and a mode switch |

## References

- [TDR-1 8 Keymap](../tdr/0001-core.md#8-keymap)
- [TDR-2 4 Quick filter](../tdr/0002-navigation-and-bulk-tools.md#4-quick-filter),
  [TDR-2 10 Keymap](../tdr/0002-navigation-and-bulk-tools.md#10-keymap)
- [ADR 0011: A keymap that the terminals let through](0011-keymap-the-terminals-let-through.md)
