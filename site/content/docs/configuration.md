+++
title = "Configuration"
description = "The optional config file, including the quick view and SFTP settings, the environment variables manycommander reads, and the files it keeps."
weight = 60

[extra]
group = "reference"
+++

## Config file

`~/.config/manycommander/config.toml`. The file and every key in it are optional.

```toml
paint_background = false  # true: panels use the theme's background colour
pager = "less -R"         # overrides $PAGER for F3
editor = "nvim"           # overrides $EDITOR for F4

[jump]
zoxide = "auto"           # "off": do not read zoxide's ranking

[preview]
protocol = "auto"         # "kitty", "sixel", "halfblocks" or "off"

[sftp]
ssh = ["ssh", "-F", "/home/you/.ssh/sftp_config"]   # the program and its arguments
```

| Key | Default | Effect |
|---|---|---|
| `paint_background` | `false` | `true` paints the panels with the theme's `background`; see [theme](@/docs/theme.md#background) |
| `pager` | `$PAGER` | The viewer for `F3` |
| `editor` | `$EDITOR` | The editor for `F4` |
| `jump.zoxide` | `"auto"` | `"auto"` merges zoxide's ranking into [go to directory](@/docs/find-and-rename.md#zoxide) and `z` when `zoxide` is on `PATH`. `"off"` never runs zoxide |
| `preview.protocol` | `"auto"` | How the [quick view](@/docs/quick-view.md#terminals) draws pictures. `"auto"` follows what the terminal answers at startup. `"kitty"`, `"sixel"` and `"halfblocks"` force one; kitty and sixel fall back to halfblocks when the terminal reports no cell size. `"off"` shows only the info card |
| `sftp.ssh` | `["ssh"]` | The program and its own arguments for [SFTP](@/docs/sftp.md#the-ssh-command) connections, as a list: it never goes through a shell |

Unknown keys are ignored, so a config written for a newer version still loads. A file that
does not parse is reported once, and manycommander starts with the defaults.

### sftp.ssh

manycommander puts its fixed ssh options directly after the program, then the other
arguments of `sftp.ssh`, then the user, port and host. The fixed options switch off
forwarding, local and remote commands, a terminal and the escape character, and they win:
ssh keeps the first value it sees for an option. So that nothing can override them,
manycommander rejects, with the argument named:

- an argument that is or starts with `-o`;
- the flags `-A`, `-X`, `-Y`, `-t` and `-e`, also inside a group such as `-vA`, because ssh
  lets them override an `-o` wherever they stand;
- a word that is not an option, and `--`, because ssh would read it as the host.

The check runs when you connect, before anything starts. `-F`, `-i`, `-J`, `-v` and the other
options are allowed; put anything else in the file that `-F` names.

## Environment

| Variable | Effect |
|---|---|
| `PAGER`, `EDITOR` | The viewer for `F3` and the editor for `F4`, unless the config sets them |
| `SHELL` | Runs command-line input with `$SHELL -c` |
| `COLORTERM` | `truecolor` or `24bit` enables theme colours and pictures in the quick view; otherwise the terminal's ANSI colours are used, and the quick view shows only the card |
| `NO_COLOR` | Turns colour off; bold and the mark glyph keep the cursor and marks visible. The quick view shows only the card |
| `TMUX`, `TERM`, `TERM_PROGRAM` | Inside tmux (`TMUX` set, `TERM` starting with `tmux`, or `TERM_PROGRAM=tmux`), the quick view sends its graphics through tmux's passthrough |
| `XDG_RUNTIME_DIR` | Holds `manycommander/view/`, where `F3` and `F4` put the copy of an archive member or a server file. Without it, manycommander makes a private directory in the system's temporary directory |
| `MANYCOMMANDER_THEME` | Same as `--theme-file` |
| `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_DATA_HOME` | Move the config and the bookmarks, the saved session and the frequent directories, and the home trash |

## Saved state

manycommander saves the panel paths, the tabs and the command history to
`~/.local/state/manycommander/state.toml` on exit, and restores them on the next start
without arguments. Delete the file to start fresh.

| File | Holds |
|---|---|
| `~/.config/manycommander/hotlist.toml` | The bookmarks of [go to directory](@/docs/find-and-rename.md#go-to-a-directory-ctrl-d), rewritten after every change. A server bookmark is an entry `url = "sftp://..."` |
| `~/.local/state/manycommander/dirs.tsv` | The frequent directories, merged on exit |

Delete `dirs.tsv` while manycommander is not running to forget every visit. A results tab
is saved as a tab on its search root, an archive tab as the directory that holds the
archive, and a server tab as its local directory: manycommander never connects at startup.
Frequent directories are local directories only.
