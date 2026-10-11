# ADR 0009: Live theme through an inotify watcher, with a signal hook as fallback

## Status

Accepted -- 2026-09-27.

## Context

manycommander takes its colours from the active Omarchy theme and must show a new theme
within 200 ms of a switch (P-9), with no installation step and no idle wakeups (P-5).

`omarchy-theme-set` switches a theme in a fixed sequence: it fills
`current/next-theme`, runs `rm -rf current/theme`, runs `mv current/next-theme
current/theme`, rewrites `current/theme.name`, retints the running applications in
parallel, and only then runs the user's hooks in `~/.config/omarchy/hooks/theme-set.d/`.
The theme directory's inode changes on every switch, so a watch placed on `current/theme`
dies with the old directory. Omarchy reloads other terminal programs by signal: `SIGUSR2`
to btop, `SIGUSR1` to helix.

## Decision

- **Primary: a watcher.** A non-recursive inotify watch on
  `~/.local/state/omarchy/current/` (the `inotify` crate). Only `IN_MOVED_TO` or
  `IN_CREATE` for `theme`, `IN_CLOSE_WRITE` for `theme.name`, and `IN_Q_OVERFLOW` trigger a
  reload, after a 50 ms debounce that starts at the first trigger. Every `next-theme` event
  and the `IN_DELETE` of `theme` are ignored.
- **Fallback: a signal.** `SIGUSR1` triggers the same reload. The repository ships
  `contrib/omarchy/theme-set-hook.sh` (`pkill -USR1 -x manycommander`), which the user may
  install as a theme-set hook. manycommander never writes the user's configuration.
- Both feed one idempotent `ReloadTheme` event. A listing thread parses `colors.toml`; the
  UI redraws fully only when the palette changed. A missing or unparsable file keeps the
  current palette.
- `--theme-file` selects another file (and then also reacts to in-place edits);
  `--no-theme-watch` turns the watcher off.

## Rationale

- The watcher needs no installation and sees the swap itself, before the application
  retints run. Measured with a live `omarchy-theme-set`: 62 ms and 64 ms from the `mv` of
  `current/theme` to the new accent on screen; 72 ms and 78 ms on a later release.
- The hook fires only after every application retint, so it is slower from the user's point
  of view, but it works where the watch cannot be placed. Measured: 24-44 ms from the hook's
  start.
- Filtering to the two events that end the sequence yields exactly one effective change per
  switch and never an empty palette (A-TH-2 replays the full sequence).

## Consequences

- manycommander depends on Omarchy's state-directory layout and switch sequence. A-TH-2
  encodes the sequence, so a change in Omarchy shows up as a test failure, and the signal
  hook keeps working meanwhile.
- One inotify watch is spent on the theme, in addition to one per visible panel (NFR-RES).
- A full redraw follows every reload, which also repairs a frame that foot's OSC recolour
  interleaved with.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Reload by hook only | Needs an installation step and fires after all retints |
| A watch on `current/theme` | Dies with the directory inode on the first switch |
| Polling the theme file | Periodic wakeups break P-5 |
| The `notify` crate | The `inotify` crate gives the exact masks and names the filter needs |

## References

- [TDR-1 2](../tdr/0001-core.md#2-environment-assumptions), [TDR-1 7.2](../tdr/0001-core.md#72-live-reload)
- A-TH-1, A-TH-2, A-TH-3; TDR-1 E-17, E-18
