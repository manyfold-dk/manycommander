+++
title = "Theme"
description = "How manycommander follows the active Omarchy theme, live, and the fallbacks."
weight = 50
+++

manycommander reads the active Omarchy palette from
`~/.local/state/omarchy/current/theme/colors.toml`. It watches
`~/.local/state/omarchy/current/`, so `omarchy-theme-set`, or picking a theme from the
Omarchy menu, recolours a running manycommander within a fraction of a second. There is
nothing to install.

## Where the colours go

| Element | Palette key |
|---|---|
| Files, directories, executables | `foreground`, `bright_foreground` in bold, `green` |
| Symlinks, broken symlinks, hidden entries | `cyan`, `red`, `dark_foreground` |
| Marked entries | `yellow` in bold, with a `▸` marker |
| Cursor row, active panel | `background` on `accent` |
| Cursor row, inactive panel | `selection` |
| Active and inactive panel border | `accent`, `muted` |
| Size and date columns | `light_foreground` |
| Dialogs | `dark_background` with an `accent` border |
| Function-key bar | `foreground` on `lighter_background`, key numbers in `accent` |

Light themes need nothing special: every role refers to a semantic key, not to a fixed
colour.

## Background

By default the terminal keeps the background, including its opacity, and manycommander
paints only text and highlights. Omarchy retints the terminal in the same theme switch, so
both change together. Set `paint_background = true` in the
[config](@/docs/configuration.md) to paint the panels with the theme's `background`
instead.

## The optional hook

The watcher needs no setup. For a system where the watch cannot be placed, the repository
ships a theme-set hook that sends `SIGUSR1`:

```bash
cp contrib/omarchy/theme-set-hook.sh ~/.config/omarchy/hooks/theme-set.d/manycommander
```

Omarchy runs the hook after it has retinted every other app, so the hook is slower than the
watcher.

## Other palettes and fallbacks

- `--theme-file PATH`, or `MANYCOMMANDER_THEME`, reads another `colors.toml` and follows
  edits to it. That is handy while writing a theme.
- Without truecolor (`COLORTERM` is not `truecolor` or `24bit`), every role falls back to
  a named ANSI colour, which the terminal draws from its own Omarchy-retinted palette.
- When `colors.toml` is missing or does not parse at startup, manycommander uses the ANSI
  fallback. When it happens during a live reload, manycommander keeps the palette it has.
