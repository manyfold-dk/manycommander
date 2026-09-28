+++
title = "Configuration"
description = "The optional config file and the environment variables manycommander reads."
weight = 40
+++

## Config file

`~/.config/manycommander/config.toml`. The file and every key in it are optional.

```toml
paint_background = false  # true: panels use the theme's background colour
pager = "less -R"         # overrides $PAGER for F3
editor = "nvim"           # overrides $EDITOR for F4
```

Unknown keys are ignored, so a config written for a newer version still loads.

## Environment

| Variable | Effect |
|---|---|
| `PAGER`, `EDITOR` | The viewer for `F3` and the editor for `F4`, unless the config sets them |
| `SHELL` | Runs command-line input with `$SHELL -c` |
| `COLORTERM` | `truecolor` or `24bit` enables theme colours; otherwise the terminal's ANSI colours are used |
| `NO_COLOR` | Turns colour off; bold and the mark glyph keep the cursor and marks visible |
| `MANYCOMMANDER_THEME` | Same as `--theme-file` |
| `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_DATA_HOME` | Move the config, the saved session and the home trash |

## Saved state

manycommander saves the panel paths, the tabs and the command history to
`~/.local/state/manycommander/state.toml` on exit, and restores them on the next start
without arguments. Delete the file to start fresh.
