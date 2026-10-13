+++
title = "Compared"
description = "What manycommander does differently from mc, ranger, yazi, nnn and lf, read from each project's own documentation: panels and keys, archives and servers as panels, the copy and trash guarantees, and the live theme."
weight = 90

[extra]
group = "reference"
short = "Compared"
wide = true
+++

Each cell reads the project's own main documentation, on the date in the sources below.
The values:

| Value | Meaning |
|---|---|
| built in | the documentation describes it as part of the program |
| external | the documentation offers it through a plugin, a script or another program |
| partial | the documentation describes part of it, or a related feature |
| not documented | the documentation read does not describe it; the feature may exist in a plugin, a later release or a page not read |

"Not documented" is a statement about the documentation, never about the program. A
correction with a source is welcome as an [issue](https://github.com/manyfold-dk/manycommander/issues).

## The table

| | manycommander | mc | ranger | yazi | nnn | lf |
|---|---|---|---|---|---|---|
| Two panels side by side | built in | built in | partial: a multi-pane view mode | not documented | not documented: contexts show one directory at a time | not documented |
| F-key verbs (`F3` view, `F5` copy, `F6` move, `F8` delete) | built in | built in | not documented | not documented | not documented | not documented: vi and readline keys |
| Tabs in a panel | built in | not documented | built in | built in | built in: contexts | left to the window manager or multiplexer, by design |
| Archives browsed as directories | built in: zip, tar, compressed tar, 7z | built in: tar and zip through the VFS | external: previewed with `atool` or `bsdtar` | partial: archive preview and extraction | partial: mount with FUSE, extract, list | not documented |
| SFTP through your own `ssh` configuration | built in: config, keys, agent, `ProxyJump` | built in: sftp file system; `~/.ssh/config` not documented | not documented | partial: a virtual file system for remote files | external: `sshfs` or `rclone` mounts | not documented |
| Pictures in the terminal | built in: kitty graphics, Sixel, halfblocks | not documented | external: w3m, ueberzug, kitty, iTerm2 | built in: kitty, iTerm2, Sixel, Überzug++ | external: a thumbnail plugin | external: a previewer script |
| Find by name and content | built in | built in | not documented | partial: ripgrep and fd integration | partial: a search plugin | not documented: search matches names |
| Multi-rename with a preview and undo | built in | not documented | built in: bulk rename | built in: bulk rename | built in: batch rename | not documented |
| Delete to the freedesktop.org trash | built in | not documented | not documented | built in: trash bin | partial: integration with a trash utility | not documented |
| A copy written under a temporary name and renamed into place | built in | not documented | not documented | not documented | not documented | not documented |
| Jump to frequent directories | built in: bookmarks and frequency, zoxide merged in | built in: the directory hotlist | partial: bookmarks | partial: zoxide integration | partial: bookmarks | not documented |
| Follows the Omarchy theme switch live | built in | not documented | not documented | partial: flavours switch with the terminal's light or dark scheme | not documented | not documented |
| Language | Rust | C | Python | Rust | C | Go |
| Licence | MIT or Apache-2.0 | GPL-3.0-or-later | GPL-3.0 | MIT | BSD-2-Clause | MIT |

The row on copies states what the documentation says, not what the programs do: a program
can copy safely without documenting it. manycommander documents its copy in
[file operations](@/docs/file-operations.md) and [durability](@/docs/durability.md),
because it is a promise the program is tested against.

## What this table leaves out

The other five are good programs with strengths this table does not measure: mc's age
and ubiquity, ranger's and yazi's plugin ecosystems, nnn's size and speed on small
machines, lf's minimal surface and its server for many instances. If you want Miller
columns, vi keys or a plugin system, manycommander is not the one; it keeps the two
panels and the F-keys of Total Commander.

## Sources

Read on 2026-10-13:

- mc: [the manual page](https://github.com/MidnightCommander/mc/blob/master/doc/man/mc.1.in) and [midnight-commander.org](https://midnight-commander.org/)
- ranger: [the README](https://github.com/ranger/ranger)
- yazi: [the README](https://github.com/sxyazi/yazi)
- nnn: [the README](https://github.com/jarun/nnn)
- lf: [the README](https://github.com/gokcehan/lf) and [its documentation](https://github.com/gokcehan/lf/blob/master/doc.md)
- manycommander: [these docs](@/docs/_index.md)
