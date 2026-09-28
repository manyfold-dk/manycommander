+++
title = "Keys"
description = "The complete keymap: panels, marks, the F-key verbs, results tabs, dialogs, tabs and the command line."
weight = 30
+++

`F1` inside manycommander shows this keymap. Keys that are also command-line editing keys,
such as `Enter`, `Home` and `Ctrl+A`, act on the panel while the command line is empty and
on the text once you have typed something. `Ctrl+D`, `Ctrl+F` and `Ctrl+M` do nothing
while the command line holds text, so they never edit or run it.

## Panels

| Key | Action |
|---|---|
| `Tab` | Switch to the other panel |
| `Up`, `Down`, `PgUp`, `PgDn`, `Home`, `End` | Move the cursor |
| `Enter` | Enter the directory, or open the file with `xdg-open` |
| `Backspace`, `Alt+Up` | Parent directory |
| `Alt+Left`, `Alt+Right` | Back and forward in the panel's history |
| `Ctrl+D` | [Go to a directory](@/docs/find-and-rename.md#go-to-a-directory-ctrl-d): bookmarks and frequent directories |
| `Ctrl+F` | [Quick filter](@/docs/find-and-rename.md#quick-filter-ctrl-f): show only the entries that match |
| `Ctrl+S` | Quick search: type to jump to a name |
| `Alt+.` | Show or hide hidden files |
| `Ctrl+F3`, `Ctrl+F4`, `Ctrl+F5`, `Ctrl+F6` | Sort by name, extension, size or time; again to reverse |
| `Ctrl+R` | Re-read both panels |
| `Ctrl+U` | Swap the panels |
| `Esc` | Stop a directory that is still loading, the search of a results tab, or a running compare; otherwise ask whether to cancel the running job |

Panels refresh on their own when a directory changes on disk.

## Marks

| Key | Action |
|---|---|
| `Insert` | Mark or unmark, and move down |
| `Space` | Mark or unmark; on a directory, also compute its size |
| `Ctrl+A` | Mark all |
| `Alt+=`, `Alt+-` | Mark or unmark by glob |
| `Alt+*` | Invert the marks |

The file verbs act on what you see: the marked entries that are visible, or the entry under
the cursor when no visible entry is marked, and nothing when the cursor is on `..`. A mark
on an entry that the quick filter or the hidden-file toggle hides is kept, but it does not
count and nothing acts on it until the entry shows again.

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
| `Ctrl+M` | [Multi-rename](@/docs/find-and-rename.md#multi-rename-ctrl-m) the marked entries with masks and a preview |
| `Alt+F7` | [Find files](@/docs/find-and-rename.md#find-files-alt-f7) by name and content |
| `Shift+F2` | [Compare](@/docs/find-and-rename.md#compare-directories-shift-f2) the two panels and mark what differs |
| `Alt+L` | [Create links](@/docs/file-operations.md#links-alt-l) in the other panel |
| `Alt+A` | [Change attributes](@/docs/file-operations.md#attributes-alt-a): mode and modification time |
| `F10`, `Alt+X` | Quit |

What each verb guarantees is on the [file operations](@/docs/file-operations.md) page.

## Results tab

`Alt+F7` shows what it finds in a new tab. In that tab:

| Key | Action |
|---|---|
| `Enter` on a file | Go to the file: the tab opens its directory with the cursor on it. `Alt+Left` returns to the results |
| `Enter` on a directory | Enter it; `Alt+Left` returns to the results |
| `F3`, `F4` | View or edit the file. Here `Enter` goes to the file instead of opening it with `xdg-open` |
| `Alt+Enter`, `Alt+P` | Insert the quoted path relative to the search root, or the full path |
| `Backspace`, `Alt+Up` | Go to the search root |
| `F5`, `F6`, `F8`, `Shift+F8` | Copy, move, trash or delete the results, from all their directories |
| `Shift+F6`, `Ctrl+M`, `Alt+L`, `Alt+A` | Rename, multi-rename, link or change attributes, each result in its own directory |
| `Ctrl+R` | Re-read the results; the ones that are gone drop out |
| `Esc` | Stop the running search; the tab keeps what it found |

`F7`, `Shift+F4` and `Shift+F2` answer "not in search results".

## Forms and dialogs

In the forms of find, multi-rename, links, attributes and compare:

| Key | Action |
|---|---|
| `Tab`, `Down` | Next field |
| `Shift+Tab`, `Up` | Previous field |
| `Space` | Toggle a checkbox |
| `Left`, `Right` | Change a choice, such as the link type |
| `Enter` | Run, from any field |
| `Esc` | Close |

In the go-to-directory dialog (`Ctrl+D`):

| Key | Action |
|---|---|
| Typing, `Backspace` | Filter the list |
| `Up`, `Down`, `PgUp`, `PgDn` | Move in the list |
| `Enter` | Go to the selected directory |
| `Insert` | Bookmark the active panel's directory |
| `Delete` | Remove the selected bookmark, or forget the selected frequent directory |
| `Esc` | Close |

In the multi-rename dialog (`Ctrl+M`):

| Key | Action |
|---|---|
| `PgUp`, `PgDn` | Scroll the preview |
| `Enter` | Rename, when no row of the preview shows an error |
| `Ctrl+Z` | Undo the last multi-rename of this session |

`Ctrl+Z` does nothing anywhere else.

## Tabs

| Key | Action |
|---|---|
| `Ctrl+T`, `Ctrl+W` | New tab, close tab |
| `Alt+PgUp`, `Alt+PgDn` | Previous and next tab |
| `Ctrl+1` .. `Ctrl+9` | Go to a tab |

Each panel has its own tabs. Tabs keep their marks while hidden.

## Command line

Typing goes to the command line under the panels. `Enter` runs the line with `$SHELL -c`
in the active panel's directory; `cd DIR` changes the panel instead, and
[`z KEYWORDS`](@/docs/find-and-rename.md#z-on-the-command-line) goes to the best matching
frequent directory.

| Key | Action |
|---|---|
| `Alt+Enter`, `Alt+P` | Insert the quoted name, or path, under the cursor |
| `Ctrl+O` | Show the last command's output |
| `Ctrl+A`, `Ctrl+E` | Start, end of the line |
| `Ctrl+U`, `Ctrl+K` | Delete to the start, to the end |
| `Ctrl+W` | Delete the previous word |
| `Ctrl+P`, `Ctrl+N` | Previous, next history entry |
| `Esc` | Clear the line |

## Terminals

`Ctrl+M` needs the kitty keyboard protocol: without it, a terminal sends `Ctrl+M` as
`Enter`. `Ctrl+1` .. `Ctrl+9` need the protocol too. Ghostty, foot, Alacritty and Kitty,
the terminals Omarchy ships, all support it.

manycommander avoids `Ctrl+Tab`, `Ctrl+Shift+Tab` and `Ctrl+Shift+Enter`, because Kitty and
Ghostty use them for their own tabs and windows, and `Ctrl+Shift+F5`, because Kitty reloads
its config with it.
