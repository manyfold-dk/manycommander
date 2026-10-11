# ADR 0013: Hand files to the desktop with gio open; F3 opens what a pager cannot show

## Status

Accepted -- 2026-10-05.

The F3 rule dates from 2026-10-04; the opener changed from `xdg-open` to `gio open` on
2026-10-05.

## Context

`Enter` on a file that is not a directory or an archive hands it to the desktop's default
application. `F3` runs `$PAGER` on it (TDR-1 6). Two problems showed up in daily use.

**F3 on a picture showed bytes.** A pager can show text, not a JPEG, a PDF or a web page.
F3 is the "look at it" key, so on such a file it should open the program that can show it.

**`xdg-open` hid terminal programs.** Outside a desktop environment that `xdg-open` knows,
such as Hyprland, it types a file by its content and runs the handler's `Exec` line itself,
whether the desktop entry says `Terminal=true` or not. A file whose handler is a terminal
program (a text file whose default application is a terminal editor) started that program
without a terminal. It showed nothing and never ended: in a manual check, three F3 presses
on a `.html` file that `file` calls plain text left three hidden editors running.

## Decision

1. `Enter` and F3 hand a file to the desktop with `setsid -f gio open <path>` when an
   executable `gio` is in an absolute directory of `PATH`, and with `setsid -f xdg-open
   <path>` otherwise. The lookup runs once, at the first open. stdio goes to `/dev/null`, and
   a helper thread reaps the short-lived `setsid`.
2. F3 on a regular file, or a symlink to one, whose **name** says picture, document, audio,
   video or web page opens it that way instead of in `$PAGER`. The extension decides,
   ignoring case, from a fixed list (TDR-1 6). Every other file, Markdown and all other text
   included, still goes to the pager. F4 does not change.
3. An archive member or a remote file is first copied into the private view directory, as
   for the pager (TDR-3 3.4, 5.5), and the copy is opened. The application reads the copy
   after F3 returns, so the copy stays until manycommander exits. manycommander then removes
   an unchanged copy, and keeps and names an edited one: "archives are read-only; your
   edited copy is at ...", or for a remote file "not uploaded to the server; ...".

## Rationale

- `gio` (GLib) types a file by its name first, reads the same `mimeapps.list` associations,
  and runs a `Terminal=true` handler in a terminal (through `xdg-terminal-exec` where it is
  installed). Checked in a nested Hyprland: a text file opened as `foot -e nvim`, a JPEG in
  the image viewer.
- Deciding by name means the UI thread reads nothing to decide (P-1); a file named `x.png`
  that holds text still goes to an image viewer, which then reports it.
- Spawning by argv keeps NFR-SEC: no shell sees the path. `setsid` puts the application in
  its own session, so it outlives manycommander and never receives the terminal's signals.
- `xdg-open` stays the fallback for systems without GLib's `gio`.

## Consequences

- A view copy of an archive member or a remote file can outlive the F3 hand-off, until
  manycommander exits. A copy the application changed is kept and reported, never written
  back.
- Every pty test and benchmark puts a no-op `gio` and `xdg-open` first on `PATH`, so no test
  run reaches the desktop.
- The extension list is part of the design; a new kind needs a TDR change.
- `Enter` on an executable file opens it the same way and never runs it; running needs the
  command line.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Keep `xdg-open` | Hides terminal handlers outside a desktop environment it knows (the defect above) |
| Decide by content (`file`, magic bytes) | Reads the file on the UI thread, or needs a round trip to a worker before every F3 |
| Run the handler's `Exec` line from manycommander | Re-implements desktop-entry parsing and terminal selection that `gio` already does |
| `xdg-open` on `Enter` for an archive member or a remote file without a copy | The application would read a file that does not exist locally |

## References

- [TDR-1 6 Command line and hand-off](../tdr/0001-core.md#6-command-line-and-hand-off)
- [TDR-3 3.4 View and edit a member](../tdr/0003-archives-quick-view-and-sftp.md#34-view-and-edit-a-member),
  and TDR-3 5.5 for remote files
- [ADR 0014: Terminal signals during a hand-off belong to the child](0014-terminal-signals-during-a-hand-off-belong-to-the-child.md)
