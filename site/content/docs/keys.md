+++
title = "Keys"
description = "The complete keymap: panels, marks, the F-key verbs, tabs and the command line."
weight = 30
+++

`F1` inside manycommander shows this keymap. Keys that are also command-line editing keys,
such as `Enter`, `Home` and `Ctrl+A`, act on the panel while the command line is empty and
on the text once you have typed something.

## Panels

| Key | Action |
|---|---|
| `Tab` | Switch to the other panel |
| `Up`, `Down`, `PgUp`, `PgDn`, `Home`, `End` | Move the cursor |
| `Enter` | Enter the directory, or open the file with `xdg-open` |
| `Backspace`, `Alt+Up` | Parent directory |
| `Alt+Left`, `Alt+Right` | Back and forward in the panel's history |
| `Ctrl+S` | Quick search: type to jump to a name |
| `Alt+.` | Show or hide hidden files |
| `Ctrl+F3`, `Ctrl+F4`, `Ctrl+F5`, `Ctrl+F6` | Sort by name, extension, size or time; again to reverse |
| `Ctrl+R` | Re-read both panels |
| `Ctrl+U` | Swap the panels |
| `Esc` | Stop a directory that is still loading |

Panels refresh on their own when a directory changes on disk.

## Marks

| Key | Action |
|---|---|
| `Insert` | Mark or unmark, and move down |
| `Space` | Mark or unmark; on a directory, also compute its size |
| `Ctrl+A` | Mark all |
| `Alt+=`, `Alt+-` | Mark or unmark by glob |
| `Alt+*` | Invert the marks |

The file verbs act on the marked entries, or on the entry under the cursor when nothing is
marked.

## Verbs

| Key | Action |
|---|---|
| `F3` | View the file in `$PAGER` |
| `F4`, `Shift+F4` | Edit the file, or a new file, in `$EDITOR` |
| `F5` | Copy to the other panel |
| `F6`, `Shift+F6` | Move to the other panel, rename in place |
| `F7` | Make a directory; `a/b/c` creates the parents |
| `F8` | Move to the trash |
| `Shift+F8` | Delete permanently, after you type `delete` |
| `F10`, `Alt+X` | Quit |

What each verb guarantees is on the [file operations](@/docs/file-operations.md) page.

## Tabs

| Key | Action |
|---|---|
| `Ctrl+T`, `Ctrl+W` | New tab, close tab |
| `Alt+PgUp`, `Alt+PgDn` | Previous and next tab |
| `Ctrl+1` .. `Ctrl+9` | Go to a tab |

Each panel has its own tabs. Tabs keep their marks while hidden.

## Command line

Typing goes to the command line under the panels. `Enter` runs the line with `$SHELL -c`
in the active panel's directory; `cd DIR` changes the panel instead.

| Key | Action |
|---|---|
| `Alt+Enter`, `Alt+P` | Insert the quoted name, or path, under the cursor |
| `Ctrl+O` | Show the last command's output |
| `Ctrl+A`, `Ctrl+E` | Start, end of the line |
| `Ctrl+U`, `Ctrl+K` | Delete to the start, to the end |
| `Ctrl+W` | Delete the previous word |
| `Ctrl+P`, `Ctrl+N` | Previous, next history entry |
| `Esc` | Clear the line |

manycommander avoids `Ctrl+Tab`, `Ctrl+Shift+Tab` and `Ctrl+Shift+Enter`, because Kitty and
Ghostty use them for their own tabs and windows.
