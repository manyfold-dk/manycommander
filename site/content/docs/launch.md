+++
title = "Launch"
description = "Bind manycommander to SUPER + E in Hyprland, and the command-line arguments."
weight = 20

[extra]
group = "start"
+++

## From Hyprland

Add one line to `~/.config/hypr/bindings.lua`:

```lua
o.bind("SUPER + E", "File manager (dual pane)", { tui = "manycommander", focus = true })
```

`tui` starts manycommander in the default terminal through `omarchy-launch-tui`, so the
window class is `org.omarchy.manycommander` and window rules can target it. With
`focus = true`, a second press focuses the running window instead of opening another one.

If `SUPER + E` already starts a different file manager, comment out that line first.
Keeping the old line as a comment makes the switch a one-line change in either direction.

## From a terminal

```bash
manycommander                     # the last session's panels and tabs
manycommander ~/Downloads         # the left panel starts in ~/Downloads
manycommander /mnt/usb ~/Pictures # both panels
```

On exit, manycommander saves each panel's tabs, their sort order and hidden-file setting,
and the command history to `~/.local/state/manycommander/state.toml`. The next start
restores them, with the panel that was active. A directory named on the command line wins
over the saved tabs of its side, and the left panel starts active; the other side is still
restored. A saved path that no longer exists falls back to its nearest existing parent.

On the very first start, the left panel shows the working directory and the right panel
shows `$HOME`.

## Options

| Option | Effect |
|---|---|
| `LEFT [RIGHT]` | Start directories for the two panels |
| `--theme-file PATH` | Read the palette from `PATH` instead of the Omarchy theme, and follow edits to it. `MANYCOMMANDER_THEME` does the same |
| `--no-theme-watch` | Do not watch for theme changes; `SIGUSR1` still reloads |
| `--log FILE` | Write a diagnostic log to `FILE` |
| `-h`, `--help` | Print the usage line |
