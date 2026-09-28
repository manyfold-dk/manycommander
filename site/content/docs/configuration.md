+++
title = "Configuration"
description = "The optional config file, the environment variables manycommander reads, and the files it keeps."
weight = 40
+++

## Config file

`~/.config/manycommander/config.toml`. The file and every key in it are optional.

```toml
paint_background = false  # true: panels use the theme's background colour
pager = "less -R"         # overrides $PAGER for F3
editor = "nvim"           # overrides $EDITOR for F4

[jump]
zoxide = "auto"           # "off": do not read zoxide's ranking
```

| Key | Default | Effect |
|---|---|---|
| `paint_background` | `false` | `true` paints the panels with the theme's `background`; see [theme](@/docs/theme.md#background) |
| `pager` | `$PAGER` | The viewer for `F3` |
| `editor` | `$EDITOR` | The editor for `F4` |
| `jump.zoxide` | `"auto"` | `"auto"` merges zoxide's ranking into [go to directory](@/docs/find-and-rename.md#zoxide) and `z` when `zoxide` is on `PATH`. `"off"` never runs zoxide |

Unknown keys are ignored, so a config written for a newer version still loads. A file that
does not parse is reported once, and manycommander starts with the defaults.

## Environment

| Variable | Effect |
|---|---|
| `PAGER`, `EDITOR` | The viewer for `F3` and the editor for `F4`, unless the config sets them |
| `SHELL` | Runs command-line input with `$SHELL -c` |
| `COLORTERM` | `truecolor` or `24bit` enables theme colours; otherwise the terminal's ANSI colours are used |
| `NO_COLOR` | Turns colour off; bold and the mark glyph keep the cursor and marks visible |
| `MANYCOMMANDER_THEME` | Same as `--theme-file` |
| `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_DATA_HOME` | Move the config and the bookmarks, the saved session and the frequent directories, and the home trash |

## Saved state

manycommander saves the panel paths, the tabs and the command history to
`~/.local/state/manycommander/state.toml` on exit, and restores them on the next start
without arguments. Delete the file to start fresh.

| File | Holds |
|---|---|
| `~/.config/manycommander/hotlist.toml` | The bookmarks of [go to directory](@/docs/find-and-rename.md#go-to-a-directory-ctrl-d), rewritten after every change |
| `~/.local/state/manycommander/dirs.tsv` | The frequent directories, merged on exit |

Delete `dirs.tsv` while manycommander is not running to forget every visit. A results tab
is saved as a tab on its search root.
