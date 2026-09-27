# manycommander

A dual-pane, keyboard-driven terminal file manager for [Omarchy](https://omarchy.org/), in
the Total Commander tradition: two panels, the F3-F8 verbs, a command line and tabs. It
takes its colours from the active Omarchy theme and re-themes itself live when the theme
changes.

The design is [docs/specs/2026-09-27-manycommander-design.md](docs/specs/2026-09-27-manycommander-design.md);
the implementation plan and its execution record are in [docs/plans/](docs/plans/).

## Requirements

- Linux with a current kernel (6.8 or later: `statx` mount IDs, `renameat2`,
  `copy_file_range`, `syncfs`). Other systems are not supported.
- Stable Rust to build.
- A terminal with truecolor and the kitty keyboard protocol for the full keymap: Ghostty,
  foot, Alacritty and Kitty all qualify.

## Install

```bash
cargo install --path . --root ~/.local
```

This puts `manycommander` in `~/.local/bin`, which is on the Hyprland session's `PATH` in a
default Omarchy install. Check with
`tr '\0' '\n' < /proc/$(pgrep -x Hyprland)/environ | grep ^PATH=`. If it is not, use the
absolute path in the binding below.

## Launch from Hyprland

Add the binding to `~/.config/hypr/bindings.lua`:

```lua
o.bind("SUPER + E", "File manager (dual pane)", { tui = "manycommander", focus = true })
```

`tui` starts it through `omarchy-launch-tui`, so the window class is
`org.omarchy.manycommander`, and `focus = true` focuses a running instance on the second
press. If `SUPER + E` already starts another file manager, unbind or comment out that line
first; keeping it as a comment makes the switch reversible.

Start directories: the working directory on the left and `$HOME` on the right;
`manycommander LEFT [RIGHT]` overrides both.

## Theme

manycommander reads `~/.local/state/omarchy/current/theme/colors.toml` and watches
`~/.local/state/omarchy/current/`, so `omarchy-theme-set` recolours it within a fraction of
a second, with no setup. The terminal keeps the background (and its opacity) unless
`paint_background = true` is set in the config.

The optional hook is a fallback for systems where the watch cannot be placed:

```bash
cp contrib/omarchy/theme-set-hook.sh ~/.config/omarchy/hooks/theme-set.d/manycommander
```

It sends `SIGUSR1`, which reloads the theme. Options: `--theme-file PATH` (or
`MANYCOMMANDER_THEME`) reads another palette file and follows edits to it;
`--no-theme-watch` turns the watcher off (the signal still works). Without truecolor
(`COLORTERM` not `truecolor`/`24bit`) the terminal's ANSI colours are used; `NO_COLOR`
turns colour off.

## Configuration

`~/.config/manycommander/config.toml`, every key optional:

```toml
paint_background = false  # true: panels use the theme's background colour
pager = "less -R"         # overrides $PAGER for F3
editor = "nvim"           # overrides $EDITOR for F4
```

## Keys

F1 shows the full keymap. The essentials:

| Key | Action |
|---|---|
| `Tab` | Other panel |
| `Enter`, `Backspace`, `Alt+Up` | Enter directory or open file, parent directory |
| `Insert`, `Space` | Mark (Space on a directory also computes its size) |
| `F3`, `F4`, `Shift+F4` | View, edit, edit a new file (`$PAGER`, `$EDITOR`) |
| `F5`, `F6`, `Shift+F6` | Copy, move, rename |
| `F7` | Make directory (`a/b/c` creates parents) |
| `F8`, `Shift+F8` | Move to trash, delete permanently (you type `delete`) |
| `Ctrl+T`, `Ctrl+W`, `Alt+PgUp`/`Alt+PgDn` | New tab, close tab, previous/next tab |
| `F10`, `Alt+X` | Quit |

Typing goes to the command line; `Enter` runs it with `$SHELL -c` in the panel's directory,
and `cd DIR` changes the panel. `Alt+Enter` and `Alt+P` insert the name or the path under
the cursor, quoted.

## File operations

- **Copy** never shows a partial file: data is written to `.<name>.mc-partial-<random>` in
  the destination and renamed into place. An existing file is replaced only after you
  answer Overwrite (or Overwrite all / Overwrite all older), and then atomically; the old
  file is never truncated. Symbolic links are copied as links. Ownership, ACLs, extended
  attributes, hard-link structure and sparseness are not preserved.
- **Move** renames when source and destination are on the same filesystem. Across
  filesystems it copies, checks that the source did not change, and deletes the source only
  after the copy is durable (`syncfs`, in batches of 256 files or 256 MiB). A changed source
  is kept. Mount points are skipped, never copied and deleted. On vfat and exfat the change
  check is best-effort, because their timestamps are coarse.
- **Trash** follows the freedesktop.org Trash specification and is compatible with GIO
  (`gio trash --restore`, Nautilus). It never copies across filesystems and never deletes;
  when no trash can be used, it offers Skip or the typed permanent delete for that entry.
- **Delete** (Shift+F8) counts files, directories and bytes first and deletes only after you
  type `delete`. Symbolic links are removed, never followed; mount points are skipped.

Every job ends with a report: each entry is done, skipped with a reason, or failed with the
OS error, and a cancelled job says what it left, for example "move cancelled: 812 moved, 40
still at source".

## Durability

Copy does not fsync, like `cp`. Move keeps every file in at least one complete, committed
place at every moment, and after a crash or power loss too, on filesystems that honour
`syncfs` (btrfs, ext4, xfs): the last unflushed batch can be in both places, never in
neither, and running F6 again merges the rest. After a crash, `.mc-partial-*` files can
remain in a destination directory; manycommander never deletes them automatically.

## Development

```bash
scripts/install-hooks.sh     # the pre-push hook runs scripts/check.sh full
scripts/check.sh quick       # format, lint, unit tests
scripts/check.sh full        # every test (skips fail), cargo-deny, publication gate
scripts/bench/run.sh         # A-P-1 to A-P-7; results in docs/perf/history.md
```

`full` expects `MC_XDEV_DIR` on a second filesystem (default `/dev/shm/mc-xdev`), btrfs under
`target/`, and user namespaces (`unshare -rm`) for the bind-mount tests.

## License

Apache-2.0. See [LICENSE](LICENSE).
