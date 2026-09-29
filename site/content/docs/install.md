+++
title = "Install"
description = "Build manycommander with cargo and put it on the Hyprland session's PATH."
weight = 10
+++

## Requirements

- Linux with kernel 6.8 or later. manycommander relies on `statx` mount IDs, `renameat2`,
  `copy_file_range` and `syncfs` error reporting. Other systems are not supported.
- A stable Rust toolchain (`rustup` or the distribution's `rust` package). The crate uses
  the 2024 edition.
- A terminal with truecolor and the kitty keyboard protocol for the full keymap. Ghostty,
  foot, Alacritty and Kitty, the terminals Omarchy ships, all qualify.

## Install from Git

```bash
cargo install --git https://github.com/manyfold-dk/manycommander --root ~/.local
```

This builds the latest `main` as a release binary and puts `manycommander` in
`~/.local/bin`. To install a published release instead, add `--tag` with its tag from the
[releases page](https://github.com/manyfold-dk/manycommander/releases), or unpack the
release's tarball and copy its `manycommander` to `~/.local/bin`; `manycommander --version`
shows which one runs. A default Omarchy
install has that directory on the Hyprland session's `PATH`. To check, read the environment
of the running compositor:

```bash
tr '\0' '\n' < /proc/$(pgrep -x Hyprland)/environ | grep ^PATH=
```

If `~/.local/bin` is missing there, use the absolute path in the
[Hyprland binding](@/docs/launch.md).

## Install from a clone

```bash
git clone https://github.com/manyfold-dk/manycommander
cd manycommander
cargo install --path . --root ~/.local
```

## Update and remove

Run the install command again to update; cargo rebuilds when the repository has new
commits. Add `--force` to rebuild the same commit.

```bash
cargo uninstall --root ~/.local manycommander
```

Removing the binary leaves two small files behind: the config in
`~/.config/manycommander/` and the saved panels, tabs and history in
`~/.local/state/manycommander/`.
