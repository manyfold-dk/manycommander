+++
title = "Install"
description = "Download a release for Linux x86-64, or build manycommander from source with cargo, and put it on the Hyprland session's PATH."
weight = 10

[extra]
group = "start"
+++

## Requirements

- Linux with kernel 6.8 or later. manycommander relies on `statx` mount IDs, `renameat2`,
  `copy_file_range` and `syncfs` error reporting. Other systems are not supported.
- A terminal with truecolor and the kitty keyboard protocol for the full keymap. Ghostty,
  foot, Alacritty and Kitty, the terminals Omarchy ships, all qualify.
- For a build from source only: a stable Rust toolchain (`rustup` or the distribution's
  `rust` package). The crate uses the 2024 edition.

## Install a release

Every [release](https://github.com/manyfold-dk/manycommander/releases) carries a binary
for Linux x86-64, `manycommander-<version>-x86_64-linux.tar.gz`, and its SHA-256 checksum.
These commands find the latest release, download it, check the checksum, and install
`manycommander` into `~/.local/bin`:

```bash
{{ config.extra.install_release }}
```

`sha256sum -c` prints `OK` for a good download; on a mismatch it prints `FAILED` and
nothing is installed. For another release, set `tag` to its tag from the releases page
instead of the first three lines. `manycommander --version` shows which release runs.

A default Omarchy install has `~/.local/bin` on the Hyprland session's `PATH`. To check,
read the environment of the running compositor:

```bash
tr '\0' '\n' < /proc/$(pgrep -x Hyprland)/environ | grep ^PATH=
```

If `~/.local/bin` is missing there, use the absolute path in the
[Hyprland binding](@/docs/launch.md).

## Build from source

A build from source needs the Rust toolchain from the requirements.

```bash
cargo install --git https://github.com/manyfold-dk/manycommander --root ~/.local
```

This builds the latest `main`, the development version, as a release binary and puts
`manycommander` in `~/.local/bin`. To build a published release instead, add `--tag` with
its tag from the [releases page](https://github.com/manyfold-dk/manycommander/releases).

From a clone:

```bash
git clone https://github.com/manyfold-dk/manycommander
cd manycommander
cargo install --path . --root ~/.local
```

## Update and remove

To update a release, run the release commands again; `install` replaces the binary. To
update a build from source, run its install command again; cargo rebuilds when the
repository has new commits. Add `--force` to rebuild the same commit.

Remove a release with `rm ~/.local/bin/manycommander`, and a build from source with:

```bash
cargo uninstall --root ~/.local manycommander
```

Removing the binary leaves two small files behind: the config in
`~/.config/manycommander/` and the saved panels, tabs and history in
`~/.local/state/manycommander/`.
