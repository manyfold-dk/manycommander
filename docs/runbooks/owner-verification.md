# Verify manycommander by hand on the latest release

This runbook is the owner's hands-on verification of the latest release. The latest release
contains phase 3 and the type-to-filter change. The runbook covers the open manual items of
the three implementation plans and the checks of the type-to-filter change:

| Plan | Open manual items |
|---|---|
| [M1 and M2](../plans/implemented/2026-09-27-manycommander-m1-m2.md#open-items) | T11 chord confirmation, feel test, T16 `SUPER + E` switch, A-P-7 decision |
| [Phase 2](../plans/implemented/2026-09-28-manycommander-phase2.md#open-items) | Phase 2 chords in the terminals, A-P-7 decision, A-FD-7 again (optional) |
| [Phase 3](../plans/2026-09-28-manycommander-phase3.md#a-qv-8-manual-checklist-owner) | A-QV-8, A-SF-12, the phase 3 chords, A-P-7 decision |
| Type to filter (no plan) | Part 4: typing filters the panel, and `Ctrl+E` gives the command line the focus |
| F3 in applications and Markdown (no plan) | Part 5: `F3` opens pictures and web pages in their application, and the quick view renders Markdown |

The runbook also checks the M1 manual acceptance checks again on the release: A-LN-1, A-TH-1,
A-UI-1, A-UI-2, A-UI-3, A-TR-1 and A-TR-3.

The designs define the checks:

| Design | Acceptance checks |
|---|---|
| [M1 and M2 design](../specs/implemented/2026-09-27-manycommander-design.md#11-acceptance-checks) | Section 11 |
| [Phase 2 design](../specs/implemented/2026-09-28-manycommander-phase2-design.md#12-acceptance-checks) | Section 12 |
| [Phase 3 design](../specs/implemented/2026-09-28-manycommander-phase3-design.md#8-acceptance-checks) | Section 8 |

The type-to-filter change has no acceptance checks of its own. The amendments of 2026-09-30
define the change: [M1 and M2 design section 8](../specs/implemented/2026-09-27-manycommander-design.md#8-keymap),
[phase 2 design section 4](../specs/implemented/2026-09-28-manycommander-phase2-design.md#4-quick-filter)
and [phase 2 design section 10](../specs/implemented/2026-09-28-manycommander-phase2-design.md#10-keymap).

The user documentation is in [site/content/docs/](../../site/content/docs/).

Each check has an ID. A design ID, such as A-LN-1 or A-QV-8.3, names a check that a design
defines. An `OV-` ID names a check that only this runbook defines. An `OD-` ID names a
decision for the owner.

Contents: [Prerequisites](#prerequisites) · [Pre-action checklist](#pre-action-checklist) ·
[Procedure](#procedure) · [Part 1: M1 and M2](#part-1-m1-and-m2) ·
[Part 2: Phase 2](#part-2-phase-2) · [Part 3: Phase 3](#part-3-phase-3) ·
[Part 4: Type to filter](#part-4-type-to-filter) ·
[Part 5: F3 in applications and Markdown](#part-5-f3-in-applications-and-markdown) ·
[Decisions for the owner](#decisions-for-the-owner) · [Verification](#verification) ·
[Report back](#report-back) · [Rollback](#rollback)

## Prerequisites

### Equipment

- The Omarchy laptop, logged in to Hyprland.
- Ghostty (the default terminal) and foot.
- tmux. Part 3 starts its own tmux servers with separate sockets. Your own tmux servers
  stay unchanged. The Omarchy tmux configuration is at
  `/usr/share/omarchy/config/tmux/tmux.conf`.
- The commands `bsdtar`, `zip`, `zstd`, `xz`, `magick` (ImageMagick), `python3`, `rclone`,
  `fusermount3`, `gio`, `udisksctl`, `mkfs.vfat`, `jq`, `wl-copy`, `gh` (logged in), `mise`
  and `cargo`. `zoxide` is optional. Run this command to find a missing one:

  ```bash
  for c in bsdtar zip zstd xz magick python3 rclone fusermount3 gio udisksctl mkfs.vfat jq wl-copy gh mise cargo; do
    command -v "$c" >/dev/null || echo "missing: $c"
  done
  ```

- A checkout of the manycommander repository, on `main` at or after the release tag. The
  runbook uses its `scripts/fixtures/stall-fuse.sh`, `contrib/omarchy/theme-set-hook.sh`
  and `tests/manual.rs`.
- For part 3: an SSH server that you control. The runbook calls the server `HOST`. `HOST`
  is a `Host` entry in `~/.ssh/config`. Replace `HOST` in each command with the name of
  that entry. When you reach the server through a jump host, the entry has a `ProxyJump`
  line. `HOST2` is a second server entry, for one optional check.
- On `HOST`: a login directory where you can write. The runbook uses `~/mc-verify` there.

> **Warning:** The repository is public. Do not write a real host name, user name, address
> or path of your own into the repository. The results file and the logs hold such values.
> Keep them outside the repository.

### Install the release under test

The release under test is the latest release. Use option A, option B or option C.

> **Warning:** The mise shims come before `~/.local/bin` on the `PATH` of the Hyprland
> session. When mise has manycommander, options B and C do not change the binary that runs.
> Use option A when `mise ls github:manyfold-dk/manycommander` shows a version.

1. Read the tag of the latest release:

   ```bash
   tag="$(gh release view --repo manyfold-dk/manycommander --json tagName --jq .tagName)"
   echo "$tag"
   ```

2. Option A: install or update the release with mise. mise holds back a new release for
   24 hours. `MISE_MINIMUM_RELEASE_AGE=0` removes the wait.

   ```bash
   mise use -g github:manyfold-dk/manycommander
   MISE_MINIMUM_RELEASE_AGE=0 mise up github:manyfold-dk/manycommander
   ```

3. Option B: build the tag with cargo.

   ```bash
   cargo install --git https://github.com/manyfold-dk/manycommander --tag "$tag" --root ~/.local --locked
   ```

   If cargo says that the package is already installed, run the command again with `--force`.

4. Option C: install the binary from the release tarball.

   ```bash
   tmp="$(mktemp -d)"
   gh release download "$tag" --repo manyfold-dk/manycommander --pattern '*-x86_64-linux.tar.gz*' --dir "$tmp"
   (cd "$tmp" && sha256sum -c ./*.sha256)
   tar -xzf "$tmp"/*.tar.gz -C "$tmp"
   install -m 755 "$tmp"/manycommander-*-x86_64-linux/manycommander ~/.local/bin/manycommander
   ```

   `sha256sum -c` prints `OK` for the tarball.

5. Confirm the version:

   ```bash
   command -v manycommander
   manycommander --version
   echo "manycommander ${tag#v}"
   ```

   With option A, the first line is a path under `.local/share/mise` of your home. With
   options B and C, the first line is the `.local/bin/manycommander` path of your home. The
   last two lines are identical.

### The log

`manycommander --log FILE` appends a diagnostic log to `FILE`. The log never holds file
contents. The log holds paths. Each line starts with a timestamp, a level, the thread and
the module name.

| Line | Example | Meaning |
|---|---|---|
| `key` | `key code=F(5) modifiers=KeyModifiers(0x0) action=Copy` | One line for each key press that reaches manycommander. `code` and `modifiers` show what the terminal sent. `action` is the key map's action while the command line does not have the focus |
| `terminal probe` | `terminal probe probe_us=4100 kitty=true sixel=false keyboard=true da1=true cell=Some((10, 20)) tmux=false discarded=0 protocol="kitty"` | One line at the start. `keyboard` is the kitty keyboard protocol. `protocol` is how the quick view draws pictures. `tmux` is `true` inside tmux |
| `first full frame` | `first full frame first_full_frame_us=13100` | The time from the start to the first frame with both panels listed |
| `frame` | `frame key_to_flush_us=1200` | The time from a key to the frame on the screen |
| `theme reload applied` | `theme reload applied ms=3.2` | A new palette is on the screen. `theme reload: palette unchanged` is a reload without a change |
| `preview request`, `preview stages`, `preview transmit` | `preview stages generation=4 read_ms=2.1 decode_ms=101.3 prepare_ms=17.5 total_ms=121.0 bytes=2250000 protocol="kitty"` | The quick view's work on one picture |
| `archive scan done` | `archive scan done archive=... format=... nodes=... ms=...` | One complete read of an archive |
| `find done` | `find done id=1 dirs=... results=... errors=... ms=...` | One search |
| `job start`, `job done` | `job done summary=...` | One file operation |
| `sftp connect`, `sftp session lost`, `sftp session closed` | `sftp connect session=1 ok=true ms=840` | SFTP connections |
| `sftp: the least recently used session closes` | `sftp: the least recently used session closes session=1` | The pool of four connections closes one |

The numbers in the examples are samples, not targets.

The key map computes the `action` of a `key` line as if the command line does not have the
focus. A typed character therefore shows `action=FilterChar(...)`, also when the character
goes to the command line, a dialog or a form. For a key that edits the command line, the log
shows the panel action or `None`. For such a key, the screen decides the result.

To follow a log while you work, run this command in a second terminal:

```bash
tail -f "$PG/logs/NAME.log" | grep --line-buffered -E ' key |terminal probe|first full frame|theme reload|preview stages|sftp'
```

### The playground

The playground is a new directory on the same filesystem as your home directory. The
runbook uses `~/mc-verify`. The directory `/dev/shm/mc-verify-$USER` is a second
filesystem (tmpfs) for the cross-filesystem checks.

The `mcv` function starts manycommander with a log. `mcv NAME [LEFT [RIGHT]]` writes the log
to `$PG/logs/NAME.log`, and removes an old log of the same name first. `mcv` sets
`XDG_STATE_HOME` to `$PG/state`. The checks therefore do not change your saved panels,
tabs, command history and frequent directories. The bookmarks and the config stay in
`~/.config/manycommander/`.

The `mcpid` function prints the process ID of the manycommander that `mcv NAME` started.
`mcpid NAME` finds the process by its log file, not by its start time. A signal therefore
never goes to another manycommander, for example the manycommander of `SUPER + E`.

1. Run this block. To use another directory, change `~/mc-verify` in the three places.

```bash
mkdir -p ~/mc-verify
cat > ~/mc-verify/env.sh <<'EOF'
export PG="$HOME/mc-verify"
export XDEV="/dev/shm/mc-verify-$USER"
export MC_REPO="$HOME/path/to/manycommander"
mcv() {
  local name="$1"
  shift
  mkdir -p "$PG/logs"
  rm -f "$PG/logs/$name.log"
  XDG_STATE_HOME="$PG/state" manycommander --log "$PG/logs/$name.log" "$@"
}
mcpid() {
  pgrep -x manycommander | while read -r pid; do
    tr '\0' '\n' < "/proc/$pid/cmdline" | grep -qxF "$PG/logs/$1.log" && echo "$pid"
  done
}
EOF
```

2. Edit the `MC_REPO` line of `~/mc-verify/env.sh`. Set the path of your checkout.
3. Run `source ~/mc-verify/env.sh`.
4. Run the block below in bash. The block takes about half a minute. The last line is
   `playground ready: ` and the path of the playground.

```bash
(
set -eu
mkdir -p "$PG"/logs "$PG"/state "$PG"/keys/dir-a "$PG"/keys/dir-b "$PG"/images "$PG"/archives \
  "$PG"/trash "$PG"/filter "$PG"/filter-out "$PG"/find/deep/er "$PG"/find/.hidden "$PG"/find/mnt \
  "$PG"/find-mnt-src "$PG"/rename "$PG"/compare/left "$PG"/compare/right "$PG"/links/src/folder \
  "$PG"/links/dst "$PG"/attr/tree/sub "$PG"/fidelity/hl "$PG"/extract/mc "$PG"/extract/bsdtar \
  "$PG"/sftp-local/up-dir "$PG"/ssh "$PG"/tree/docs "$PG"/tree/bin "$PG"/typefilter/Docs \
  "$XDEV"/copy "$XDEV"/move

# Chords: harmless files and directories.
printf 'one\n' > "$PG/keys/file-1.txt"
printf 'two\n' > "$PG/keys/file-2.txt"
printf 'hidden\n' > "$PG/keys/.hidden-file"

# Pictures: an upright picture, the same picture rotated through its EXIF orientation,
# a 12 MP camera-like JPEG, an animated GIF (red first frame), a picture that is too
# wide, 30 small pictures for fast scrolling, and a text file.
cd "$PG/images"
magick -size 1200x800 gradient:navy-gold -gravity north -pointsize 160 -fill white -annotate +0+40 TOP upright.jpg
magick upright.jpg -rotate -90 rotated-raw.jpg
python3 - rotated-raw.jpg rotated.jpg <<'PY'
import sys
data = open(sys.argv[1], "rb").read()
exif = b"Exif\0\0MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x06\0\0\0\0\0\0"
seg = b"\xff\xe1" + (len(exif) + 2).to_bytes(2, "big") + exif
open(sys.argv[2], "wb").write(data[:2] + seg + data[2:])
PY
rm rotated-raw.jpg
magick -size 4000x3000 plasma:fractal -attenuate 0.4 +noise Gaussian -quality 92 photo-12mp.jpg
magick -delay 50 -size 400x300 xc:red xc:green xc:blue -loop 0 anim.gif
magick -size 20000x100 xc:gray wide.png
for i in $(seq 10 39); do
  magick -size 800x600 "xc:hsl($((i * 9)),70%,45%)" -gravity center -pointsize 200 -fill white -annotate +0+0 "$i" "burst-$i.jpg"
done
printf 'first line\nsecond line\n\ta tab\n' > notes.txt

# Archives: one tree in zip, tar.xz, tar.gz and 7z; a renamed zip; a file that is no zip;
# a real pacman package; and a large tar.xz for a long read.
cd "$PG"
printf 'readme\n' > tree/README.txt
printf 'notes in the archive\n' > tree/docs/notes.txt
cp images/upright.jpg tree/docs/picture.jpg
printf '#!/bin/sh\necho hi\n' > tree/bin/hello.sh
chmod 755 tree/bin/hello.sh
ln -s ../README.txt tree/docs/readme-link
zip -qry archives/tree.zip tree
bsdtar -cJf archives/tree.tar.xz tree
bsdtar -czf archives/tree.tar.gz tree
bsdtar --format 7zip -cf archives/tree.7z tree
cp archives/tree.zip archives/book.epub
printf 'not a zip\n' > archives/fake.zip
pkg="$(find /var/cache/pacman/pkg -name '*.pkg.tar.zst' -size +2M -size -40M -print -quit)"
[ -n "$pkg" ] || pkg="$(ls -S /var/cache/pacman/pkg/*.pkg.tar.zst | tail -n 1)"
cp "$pkg" archives/real.pkg.tar.zst
big="$(ls -S /var/cache/pacman/pkg/*.pkg.tar.zst | head -n 1)"
zstd -dcq "$big" | xz -T0 -1 > archives/big.tar.xz

# Trash: a name with a newline and a byte that is not UTF-8, and a plain name.
printf 'odd\n' > "$PG/trash/$(printf 'new\nline \377 name')"
printf 'plain\n' > "$PG/trash/plain.txt"

# Quick filter.
for n in alpha beta gamma delta photo-1.jpg photo-2.jpg notes.md readme.txt; do
  printf '%s\n' "$n" > "$PG/filter/$n"
done

# Type to filter: names for case folding, the fuzzy tier and a lone `[`; a zip for Alt+O.
for n in README.md reader.rs bread.txt Cargo.toml config.toml 'Æbler.txt' 'notes[old].txt'; do
  printf '%s\n' "$n" > "$PG/typefilter/$n"
done
cp "$PG/archives/tree.zip" "$PG/typefilter/tree.zip"

# Find.
printf 'the needle-verify text\n' > "$PG/find/deep/er/has-needle.txt"
printf 'NEEDLE-VERIFY in capitals\n' > "$PG/find/upper.txt"
printf 'hidden needle-verify\n' > "$PG/find/.hidden/secret.txt"
printf 'no match\n' > "$PG/find/plain.txt"
printf 'trash me\n' > "$PG/find/deep/trash-me-needle.txt"
printf 'remove me\n' > "$PG/find/vanish-needle.txt"
printf 'on the other filesystem\n' > "$PG/find-mnt-src/on-mount-needle.txt"

# Multi-rename.
cd "$PG/rename"
for i in 1 2 3 4 5 6; do printf 'img %s\n' "$i" > "IMG_000$i.JPG"; done
printf 'x\n' > 'my photo.JPG'
printf 'one\n' > swap-1
printf 'two\n' > swap-2

# Compare.
cd "$PG/compare"
printf 'same\n' > left/same.txt
cp -p left/same.txt right/same.txt
printf 'new\n' > left/newer.txt
printf 'old\n' > right/newer.txt
touch -d '2026-01-02 10:00:00' left/newer.txt
touch -d '2026-01-01 10:00:00' right/newer.txt
printf 'only left\n' > left/only-left.txt
printf 'only right\n' > right/only-right.txt
mkdir -p left/only-dir
printf 'short\n' > left/size.txt
printf 'longer text\n' > right/size.txt
touch -d '2026-01-03 10:00:00' left/size.txt right/size.txt
printf 'abc\n' > left/content.txt
printf 'xyz\n' > right/content.txt
touch -d '2026-01-04 10:00:00' left/content.txt right/content.txt

# Links and attributes.
printf 'target\n' > "$PG/links/src/target.txt"
cd "$PG/attr"
printf 'outside\n' > outside.txt
printf 'solo\n' > solo.txt
printf 'file\n' > tree/file.txt
printf 'deep\n' > tree/sub/deep.txt
ln -s ../outside.txt tree/link-out
chmod 644 outside.txt solo.txt tree/file.txt tree/sub/deep.txt
chmod 755 tree tree/sub

# Copy fidelity: a sparse file, a hard-link pair, a triple, and a link from outside.
cd "$PG/fidelity"
truncate -s 1G sparse.img
dd if=/dev/urandom of=sparse.img bs=1M count=4 seek=512 conv=notrunc status=none
printf 'pair\n' > hl/a
ln hl/a hl/b
printf 'triple\n' > hl/t1
ln hl/t1 hl/t2
ln hl/t1 hl/t3
printf 'outside\n' > out-link
ln out-link hl/c

# SFTP: local files to upload.
printf 'upload one\n' > "$PG/sftp-local/up-1.txt"
printf 'inner\n' > "$PG/sftp-local/up-dir/inner.txt"
printf 'move me\n' > "$PG/sftp-local/move-me.txt"
cp "$PG/images/upright.jpg" "$PG/sftp-local/picture.jpg"

# The results file: a copy of this runbook.
cp "$MC_REPO/docs/runbooks/owner-verification.md" "$PG/results.md"
echo "playground ready: $PG"
)
```

5. Run `identify -format '%[orientation]\n' "$PG/images/rotated.jpg"`. The output is
   `RightTop`.

### Record the results

`$PG/results.md` is a copy of this runbook. Fill the Result and Notes columns of the copy.
A Result is `pass`, `fail`, `skip` or `n/a`:

| Result | Use |
|---|---|
| `pass` | The Expected column is true |
| `fail` | A part of the Expected column is not true. Write what you saw and the log name in Notes |
| `skip` | You did not do an optional check. Write the reason in Notes |
| `n/a` | The keyboard or the server cannot do the check. Write the reason in Notes |

## Pre-action checklist

- [ ] `manycommander --version` shows the release under test.
- [ ] `source ~/mc-verify/env.sh` runs without an error. Run it again in each new terminal.
- [ ] `$PG/results.md` exists.
- [ ] The current theme is recorded:
      `cat ~/.local/state/omarchy/current/theme.name > "$PG/theme-before"`.
- [ ] The hook directory is recorded:
      `ls ~/.config/omarchy/hooks/theme-set.d/ > "$PG/hooks-before"`.
- [ ] The bindings are backed up: `cp ~/.config/hypr/bindings.lua "$PG/bindings.lua.before"`.
- [ ] `~/.config/manycommander/config.toml` does not set `paint_background = true` (A-TH-1).
- [ ] `type mcpid` shows a function. Steps 1.8 and 3.4 send signals to the process that
      `mcpid` prints.
- [ ] For part 3: `ssh HOST true` works.
- [ ] For part 3: no important work uses the network during Step 3.4.

## Procedure

Conventions:

- "Run" means: run the command in a shell. A "second terminal" is another terminal window
  where you ran `source ~/mc-verify/env.sh`.
- "Press" means a key or a chord in manycommander. "Type" means characters in manycommander.
  In a panel, the first typed character opens the quick filter. In a dialog or a form, the
  characters go to the field.
- "The command line" means manycommander's command line under the panels. `Ctrl+E` gives
  the command line the focus. The terminal cursor shows on the command line only while the
  command line has the focus. `Enter` runs the line and gives the focus back to the panel.
  `Esc` empties the line and gives the focus back to the panel.
- "On the command line, run `CMD`" means three actions: press `Ctrl+E`, type `CMD`, and
  press `Enter`.
- A laptop keyboard can lack `Insert`, `Home`, `End`, `PgUp` or `PgDn`. Use the `Fn`
  combination of the keyboard, or an external keyboard. Record a key that you cannot press as
  `n/a`.
- Each `mcv` start restores the active panel of the previous start from `$PG/state`. Before
  the first row of a step, check that the left panel is active: its border has the accent
  colour. Press `Tab` if the right panel is active.
- Each step ends with its results table.

> **Warning:** `Enter` on a file opens the file in a desktop application through `xdg-open`.
> `Enter` on the filter line also opens the entry under the cursor. Keep the cursor on a
> directory when you press `Enter`, unless a step says otherwise. To close the filter line
> without an action, press `Ctrl+F`.

### Part 1: M1 and M2

Source: the [M1 and M2 plan](../plans/implemented/2026-09-27-manycommander-m1-m2.md), its
[keymap audit](../plans/implemented/2026-09-27-manycommander-m1-m2.md#keymap-audit-t11) and its
[M1 acceptance record](../plans/implemented/2026-09-27-manycommander-m1-m2.md#m1-acceptance-t14).

#### Step 1.1: Confirm the M1 and M2 chords in Ghostty and foot

This step closes the M1 plan's open item "T11 chord confirmation". The chords are those of
design section 8 and the [keys page](../../site/content/docs/keys.md).

1. In Ghostty, run `source ~/mc-verify/env.sh`.
2. Run `mcv keys-ghostty "$PG/keys" "$PG/keys/dir-b"`.
3. In a second terminal, run the command below:

   ```bash
   tail -f "$PG/logs/keys-ghostty.log" | grep --line-buffered -E ' key |terminal probe'
   ```

4. Do the rows of the table in order.
5. For each row, compare the new `key` lines with the Expected column.
6. For each row, compare the screen with the Expected column.
7. Record the Ghostty result in the Result column.
8. Do items 1 to 7 again in foot, with the log name `keys-foot`.
9. Record the foot result after the Ghostty result, for example `pass / fail`.

| ID | Check | Expected | Result (Ghostty / foot) | Notes |
|---|---|---|---|---|
| OV-M1-K00 | Read the `terminal probe` line | `keyboard=true`. `probe_us` is below 50000. Ghostty: `protocol="kitty"`. foot: `protocol="sixel"` | | |
| OV-M1-K01 | Press `Down`, `Up`, `PgDn`, `PgUp` | `action=Down`, `Up`, `PageDown`, `PageUp`. The cursor moves | | |
| OV-M1-K02 | Press `End`, then `Home` | `action=Last`, then `First`. The cursor goes to the last entry, then to the first entry | | |
| OV-M1-K03 | Press `Tab` two times | `action=SwitchPanel`. The other panel becomes active, then the first panel again | | |
| OV-M1-K04 | Move the cursor to `file-1.txt`. Press `Insert` | `action=MarkAndDown`. `file-1.txt` is marked. The cursor is on `file-2.txt` | | |
| OV-M1-K05 | Move the cursor to `file-1.txt`. Press `Space` | `action=MarkSpace`. The mark of `file-1.txt` goes off | | |
| OV-M1-K06 | Press `Ctrl+A` | `action=MarkAll`. Every entry is marked | | |
| OV-M1-K07 | Press `Alt+-`, then `Enter` | `action=UnmarkGlob`. A prompt shows the glob `*`. Every mark goes off | | |
| OV-M1-K08 | Press `Alt+=`, type `*.txt`, press `Enter` | `action=MarkGlob`. `file-1.txt` and `file-2.txt` are marked | | |
| OV-M1-K09 | Press `Alt+*`. Use `Shift` for the `*` when your keyboard layout needs it. Then press `Alt+-` and `Enter` | `action=InvertMarks`. The marks invert. Then every mark goes off | | |
| OV-M1-K10 | Press `Ctrl+S`, type `fi`, press `Esc` | `action=QuickSearch`. The status row shows `Quick search: fi`. No filter line opens. The cursor jumps to `file-1.txt` | | |
| OV-M1-K11 | Press `Alt+.` two times | `action=ToggleHidden`. `.hidden-file` appears or disappears each time | | |
| OV-M1-K12 | Press `Ctrl+R` | `action=Reread`. The panels stay on their entries | | |
| OV-M1-K13 | Move the cursor to `dir-a`. Press `Enter` | `action=Enter`. The panel shows `dir-a` | | |
| OV-M1-K14 | Press `Backspace` | `action=Parent`. The panel shows `keys` | | |
| OV-M1-K15 | Press `Enter` on `dir-a`. Press `Ctrl+H` | `action=Parent`. The `code` is `Char('h')` with `CONTROL`, or `Backspace`. The panel shows `keys` | | |
| OV-M1-K16 | Press `Enter` on `dir-a`. Press `Alt+Up` | `action=Parent`. The panel shows `keys` | | |
| OV-M1-K17 | Press `Alt+Left`, then `Alt+Right` | `action=HistoryBack`, then `HistoryForward`. The panel goes back to `dir-a`, then forward to `keys` | | |
| OV-M1-K18 | Move the cursor to `file-1.txt`. Press `Alt+Enter` | `action=InsertName`. The command line shows the quoted name `'file-1.txt'`. The terminal cursor shows on the command line | | |
| OV-M1-K19 | Press `Alt+P` | `action=InsertPath`. The command line also shows the quoted full path | | |
| OV-M1-K20 | Press `Esc` | The command line is empty. The terminal cursor goes from the command line. The log shows `action=Escape` | | |
| OV-M1-K21 | Press `Ctrl+E`. Type `abc def`. Press `Home`, `End`, `Left`, `Right`, `Ctrl+A`, `Ctrl+E` | The first `Ctrl+E` shows `action=FocusLine`, and the terminal cursor shows on the command line. The line shows `abc def`. No filter line opens. The line cursor moves each time. The panel cursor and the marks do not change | | |
| OV-M1-K22 | With `abc def` on the line, press `Ctrl+W` | `def` goes. No tab closes | | |
| OV-M1-K23 | Press `Ctrl+U` | The line is empty. The panels do not swap. The terminal cursor stays on the command line | | |
| OV-M1-K24 | Type `abc`. Press `Home`, then `Ctrl+K` | `abc` goes to the command line. Then the line is empty | | |
| OV-M1-K25 | Type `ab`. Press `Backspace`, then `Esc` | `Backspace` removes `b`. `Esc` empties the line. The terminal cursor goes from the command line | | |
| OV-M1-K26 | Press `Ctrl+E`. Type `true`. Press `Enter` | The screen shows `[exit 0] press Enter to return`. `Enter` returns to the panels | | |
| OV-M1-K27 | Press `Ctrl+P`, then `Ctrl+N` | `action=HistoryPrev`, then `HistoryNext`. `true` appears on the line, then goes. The terminal cursor stays on the command line | | |
| OV-M1-K28 | Press `Ctrl+O`. Then press a key | `action=ShowOutput`. The terminal's normal screen shows the output of `true`. The key returns to the panels | | |
| OV-M1-K29 | Press `Esc`. Then press `Ctrl+U`. After the check, press `Ctrl+U` again, so that the active panel shows `keys` for the next rows | `Esc` gives the focus back to the panel. `action=SwapPanels`. The two panels change places. While the command line has the focus, `Ctrl+U` does not swap the panels, although the log shows `action=SwapPanels` | | |
| OV-M1-K30 | Press `Ctrl+F4`, `Ctrl+F5`, `Ctrl+F6`, `Ctrl+F3` | `action=Sort(Ext)`, `Sort(Size)`, `Sort(Mtime)`, `Sort(Name)`. The sort order changes each time | | |
| OV-M1-K31 | Press `F1`, then `Esc` | `action=Help`. The help opens and closes | | |
| OV-M1-K32 | On `file-1.txt`, press `F3` and quit the pager. Press `F4` and quit the editor. Press `Shift+F4`, then `Esc` | `action=View`, `Edit`, `EditNew`. Each one opens and returns to the panels | | |
| OV-M1-K33 | On `file-2.txt`, press `F5`, `F6`, `Shift+F6`, `F7`, `F8` and `Shift+F8`. Press `Esc` after each one | `action=Copy`, `Move`, `Rename`, `Mkdir`, `Trash`, `Delete`. Each dialog opens. `Esc` closes each dialog. No file changes | | |
| OV-M1-K34 | Press `Ctrl+T` two times | `action=NewTab`. The panel shows a tab bar with three tabs | | |
| OV-M1-K35 | Press `Alt+PgUp`, then `Alt+PgDn` | `action=PrevTab`, then `NextTab`. The active tab changes | | |
| OV-M1-K36 | Press `Ctrl+1`, `Ctrl+2`, `Ctrl+3` | `action=GotoTab(1)`, `GotoTab(2)`, `GotoTab(3)`. The active tab changes | | |
| OV-M1-K37 | Press `Alt+1`, then `Alt+2` | foot: `action=GotoTab(1)`, then `GotoTab(2)`. Ghostty: Ghostty binds the chords to its own tabs. Record the `key` lines that the log shows | | |
| OV-M1-K38 | Press `Ctrl+0` | The terminal resets its font size. No `key` line | | |
| OV-M1-K39 | Press `Ctrl+W` two times | `action=CloseTab`. One tab remains | | |
| OV-M1-K40 | Press `Alt+X`. Start manycommander again with the same `mcv` command. Press `F10` | `action=Quit` both times. manycommander quits both times | | |

#### Step 1.2: Check the tabs and the restore (M2)

Design section 11.5 defines the M2 checks. The automated tests cover the M2 checks. This
step is the check on the release.

1. Run `mcv m2-restore "$PG/keys" "$PG/keys/dir-b"`.
2. Press `Ctrl+T` two times. The left panel has three tabs.
3. In the third tab, on the command line, run `cd dir-a`.
4. Press `Ctrl+2`.
5. Press `F7`, type `gone-soon`, press `Enter`.
6. Move the cursor to `gone-soon` and press `Enter`.
7. On the command line, run `echo mc-verify-history`. Press `Enter` again to return to the
   panels.
8. Press `F10`.
9. Run `rmdir "$PG/keys/gone-soon"`.
10. Run `mcv m2-restore-2`. Do not give a directory.
11. Record the results.
12. Press `F10`.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| OV-M1-R1 | The left panel after the start of item 10. Press `Ctrl+3` | Three tabs. The third tab shows `dir-a` | | |
| OV-M1-R2 | Press `Ctrl+2` | The second tab shows `keys`, the nearest existing parent of `gone-soon` | | |
| OV-M1-R3 | Press `Ctrl+P` | The command line shows `echo mc-verify-history`. Press `Esc` after the check | | |

#### Step 1.3: Switch SUPER + E to manycommander (T16) and check A-LN-1

This step closes the M1 plan's open item "T16". Design section 9 gives the binding line.

> **Warning:** An error in `bindings.lua` can disable key bindings. Keep a terminal open
> before you start. `$PG/bindings.lua.before` holds the old file.

> **Note:** The session did items 1 to 7 at your request. It switched `SUPER + E` on
> 2026-09-29 and removed the trial binding on 2026-09-30. The backup is
> `~/.config/hypr/bindings.lua.bak.1790710807`. Start at item 8. Do items 1 to 7 only after
> a rollback.

1. Open `~/.config/hypr/bindings.lua` in an editor.
2. Find the line that binds `SUPER + E` to Double Commander:

   ```lua
   o.bind("SUPER + E", "Double Commander", { launch = "doublecmd" })
   ```

3. Put `-- ` at the start of that line. The comment stays for the rollback.
4. Add the line of design section 9 below the comment:

   ```lua
   o.bind("SUPER + E", "File manager (dual pane)", { tui = "manycommander", focus = true })
   ```

5. Find the trial binding on `SUPER + ALT + E` from the M1 check. The trial binding starts a
   build in the repository's `target/release` directory, not the release under test.
6. Put `-- ` at the start of the trial binding line.
7. Save the file.
8. Run `hyprctl reload`.
9. Run `hyprctl configerrors`.
10. Run `hyprctl binds -j | jq -c '.[] | select(.key == "E" and .modmask == 64) | {description, dispatcher}'`.
11. Run `tr '\0' '\n' < /proc/$(pgrep -x Hyprland)/environ | grep '^PATH='`.
12. Press `SUPER + E`.
13. Give the focus to another window.
14. Press `SUPER + E` again.
15. In a terminal, run `sleep 5; hyprctl activewindow | grep 'class:'`.
16. Before the 5 seconds end, press `SUPER + E`.
17. Run `hyprctl clients -j | jq '[.[] | select(.class == "org.omarchy.manycommander")] | length'`.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| OV-M1-T16.1 | The output of item 9 | Empty | | |
| OV-M1-T16.2 | The output of item 10 | One line with the description `File manager (dual pane)`. With the Lua configuration, the dispatcher is `__lua` | | |
| OV-M1-T16.3 | The output of item 11 | The value contains the directory of your install: `.local/share/mise/shims` for mise, `.local/bin` for cargo or a tarball | | |
| A-LN-1.1 | Item 12 | A terminal window opens with manycommander | | |
| A-LN-1.2 | Item 14 | The manycommander window gets the focus. No second manycommander window opens | | |
| A-LN-1.3 | Items 15 and 16 | The output is `class: org.omarchy.manycommander` | | |
| A-LN-1.4 | The output of item 17 | `1` | | |

#### Step 1.4: Do the feel test

This step closes the M1 plan's open item "Feel test". The item is "OK so far" since
2026-09-27.

1. Use manycommander through `SUPER + E` for normal file work for at least one working day.
2. Write down each moment that feels slow, surprising or missing, compared with Double
   Commander.
3. Record `pass` when nothing in the notes blocks daily use.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| OV-M1-FEEL | One working day of normal use | manycommander feels instant, and nothing blocks daily use | | |

#### Step 1.5: Switch the theme live (A-TH-1)

A-TH-1 asks for a new border colour within 200 ms. With the watcher, the time starts at the
`mv` of `current/theme`. With the hook, the time starts when the hook starts. The eye cannot
measure 200 ms. Items 1 to 15 check the behaviour. Items 16 and 17 measure the time.

> **Warning:** `omarchy-theme-set` changes the theme of the whole desktop. Item 12 changes the
> theme back.

1. Run `ls ~/.config/omarchy/hooks/theme-set.d/`.
2. If the list contains `manycommander`, run the command below. Omarchy skips files that end
   in `.sample`.

   ```bash
   mv ~/.config/omarchy/hooks/theme-set.d/manycommander ~/.config/omarchy/hooks/theme-set.d/manycommander.sample
   ```

3. In Ghostty, run `mcv a-th-1-watch "$PG"`.
4. Note the colour of the active panel's border.
5. Choose a theme that differs from the name in `$PG/theme-before`, for example `catppuccin`
   or `nord`. `ls /usr/share/omarchy/themes` lists the themes.
6. In a second terminal, run `omarchy-theme-set catppuccin`. Use your theme name.
7. Watch the border while `omarchy-theme-set` runs.
8. Press `F10`.
9. Run `cp "$MC_REPO/contrib/omarchy/theme-set-hook.sh" ~/.config/omarchy/hooks/theme-set.d/manycommander`.
10. Run `rm -f ~/.config/omarchy/hooks/theme-set.d/manycommander.sample`.
11. Run `mcv a-th-1-hook --no-theme-watch "$PG"`.
12. In the second terminal, run `omarchy-theme-set "$(cat "$PG/theme-before")"`.
13. Watch the border while `omarchy-theme-set` runs.
14. Press `F10`.
15. Run `grep -c 'theme reload applied' "$PG/logs/a-th-1-watch.log" "$PG/logs/a-th-1-hook.log"`.
16. Optional: run the timed check of the M1 session. The test switches to another theme and
    back. The test leaves the hook installed.

    ```bash
    cd "$MC_REPO" && MC_MANUAL=1 cargo test --test manual a_th_1_live_theme_switch -- --ignored --nocapture
    ```

17. Read the four `EVIDENCE A-TH-1` lines.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| A-TH-1.1 | Item 7: the watcher, no hook | The border changes to the new accent while `omarchy-theme-set` still runs, before the other applications finish | | |
| A-TH-1.2 | Item 13: the hook, with `--no-theme-watch` | The border changes back at the end of `omarchy-theme-set`, when the hook runs | | |
| A-TH-1.3 | The output of item 15 | `1` for each log | | |
| A-TH-1.4 | Optional: the four values of item 17 | Each value is 200 ms or less. The test passes | | |

#### Step 1.6: Check a stalled FUSE mount (A-UI-1)

The fixture mounts an empty directory with rclone and stops rclone with `SIGSTOP`. Every
access to the mount point then blocks.

> **Warning:** Do not open the stuck mount point in another program. That program blocks
> too. Leave the fixture shell with `exit`, so that the fixture resumes rclone and unmounts.

1. In Ghostty, run the command below. The fixture prints
   `stall-fuse: ... is stuck (rclone ... stopped)` and starts manycommander at once:

   ```bash
   "$MC_REPO/scripts/fixtures/stall-fuse.sh" bash -c 'source ~/mc-verify/env.sh && mcv a-ui-1 "$MC_STALL_DIR"; exec bash'
   ```

2. rclone answers for the stuck mount point from its cache for 1 second only. A later start
   also waits for the stuck mount point, and the panel stays `(loading)`. Item 1 starts in
   time.
3. Check that the panel shows `fine`, `src` and `stuck`.
4. Move the cursor to `stuck` and press `Enter`.
5. Press `Esc`.
6. Read the first `frame` line after the `Esc` key line: `grep -A 3 'code=Esc' "$PG/logs/a-ui-1.log"`.
7. Press `Enter` on `stuck` again.
8. Move the cursor to `fine` and press `Enter`.
9. Press `F10`.
10. Run `exit`.
11. Run `findmnt -t fuse.rclone`.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| A-UI-1.1 | Item 4 | The panel shows `(loading)` | | |
| A-UI-1.2 | Items 5 and 6 | The panel returns to the previous directory at once. `key_to_flush_us` is below 100000 | | |
| A-UI-1.3 | Item 7 | The screen shows `previous load of this directory is still blocked` | | |
| A-UI-1.4 | Item 8 | The panel shows `ok` | | |
| A-UI-1.5 | Item 9 | manycommander quits with the load still blocked. The shell works normally | | |
| A-UI-1.6 | Item 11 | No mount point with `mc-stall` in its path | | |

#### Step 1.7: Show external changes (A-UI-2)

1. Run `mcv a-ui-2 "$PG/keys"`.
2. Move the cursor to `file-2.txt`.
3. In a second terminal, run `touch "$PG/keys/file-0.txt"`.
4. Run `rm "$PG/keys/file-0.txt"`.
5. Press `F10`.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| A-UI-2.1 | Item 3 | `file-0.txt` appears within 1 second. The cursor stays on `file-2.txt` | | |
| A-UI-2.2 | Item 4 | `file-0.txt` disappears within 1 second. The cursor stays on `file-2.txt` | | |

#### Step 1.8: Hand the terminal off and back (A-UI-3)

Start manycommander from a shell in Ghostty, so that the shell's job control works. The
commands in the second terminal use `less` as the pager and `nvim` as the editor. Use the
process names of your pager and editor when they differ.

1. Run `mcv a-ui-3 "$PG/keys"`.
2. Move the cursor to `file-1.txt` and press `F3`.
3. Press `q`.
4. Press `F3` again.
5. In the second terminal, run `pkill -KILL -P "$(mcpid a-ui-3)" -x less`.
6. Press `F4`.
7. In the second terminal, run `pkill -KILL -P "$(mcpid a-ui-3)" -x nvim`.
8. On the command line, run `sleep 301`.
9. In the second terminal, run `pkill -KILL -f 'sleep 301'`.
10. Press `Enter`.
11. In the second terminal, run `kill -TSTP "$(mcpid a-ui-3)"`.
12. In the Ghostty window, run `fg`.
13. In the second terminal, run `kill -TERM "$(mcpid a-ui-3)"`.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| A-UI-3.1 | Items 2 and 3 | The pager shows `one`. After `q`, manycommander shows a full screen | | |
| A-UI-3.2 | Items 4 and 5 | manycommander returns with a full screen | | |
| A-UI-3.3 | Items 6 and 7 | manycommander returns. The screen shows `[killed by signal 9]` | | |
| A-UI-3.4 | Items 8 to 10 | The screen shows `[killed by signal 9] press Enter to return`. `Enter` returns to the panels | | |
| A-UI-3.5 | Items 11 and 12 | The shell prompt shows, with the job stopped. `fg` returns to a full manycommander screen | | |
| A-UI-3.6 | Item 13 | manycommander exits. The shell echoes what you type, and `Enter` starts a new line | | |

#### Step 1.9: Restore from the home trash (A-TR-1)

1. Run `mcv a-tr-1 "$PG/trash"`.
2. Look at the name with the newline.
3. Press `Ctrl+A`, then `F8`, then `Enter`.
4. Press `F10`.
5. Run `gio trash --list | grep -aF "$PG/trash/"`.
6. Run the command below. The command restores the name with the newline:

   ```bash
   gio trash --restore 'trash:///new%0Aline%20%FF%20name'
   ```

7. Run `ls -b "$PG/trash"`.
8. Open Nautilus. Open the Trash. Restore `plain.txt`.
9. Run `ls -b "$PG/trash"`.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| A-TR-1.1 | Item 2 | The panel shows the name escaped, as `new\nline \xff name` | | |
| A-TR-1.2 | Item 3 | The dialog asks to move 2 items to the trash. After `Enter`, the panel is empty | | |
| A-TR-1.3 | Item 5 | `gio trash --list` shows two entries with a `trash:///` address. GIO shows the original path of `plain.txt`. For the name with the byte that is not UTF-8, GIO can show `(null)`, also for an entry that GIO trashed itself | | |
| A-TR-1.4 | Items 6 and 7 | The output shows `new\nline\ \377\ name` | | |
| A-TR-1.5 | Items 8 and 9 | The output also shows `plain.txt` | | |

#### Step 1.10: Restore from a top-directory trash on vfat (A-TR-3)

The M1 session checked the tmpfs layout and the sticky `.Trash` on ext4. GIO refuses to
restore on tmpfs ([E-33](../plans/implemented/2026-09-27-manycommander-m1-m2.md#decisions-made-during-execution)).
This step checks the vfat part again.

1. Run `truncate -s 64M "$PG/vfat.img"`.
2. Run `mkfs.vfat -n MCVFAT "$PG/vfat.img"`.
3. Attach the image:

   ```bash
   dev="$(udisksctl loop-setup -f "$PG/vfat.img" --no-user-interaction | sed -n 's/.* as \(.*\)\.$/\1/p')"
   echo "$dev"
   ```

4. Run `udisksctl mount -b "$dev" --no-user-interaction`. If udisks says that the device is
   already mounted, go to the next item.
5. Run `mnt="$(findmnt -n -o TARGET -S "$dev")"; echo "$mnt"`.
6. Run `printf 'vfat\n' > "$mnt/item.txt"`.
7. Run `mcv a-tr-3 "$mnt"`.
8. Move the cursor to `item.txt`. Press `F8`, then `Enter`.
9. Press `F10`.
10. Run `cat "$mnt/.Trash-$(id -u)/info/item.txt.trashinfo"`.
11. Run `gio trash --restore "$(gio trash --list | grep -aF "$mnt/item.txt" | cut -f 1)"`.
12. Run `cat "$mnt/item.txt"`.
13. Run `udisksctl unmount -b "$dev" --no-user-interaction`.
14. Run `udisksctl loop-delete -b "$dev" --no-user-interaction`.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| A-TR-3.1 | Item 10 | The file has the line `Path=item.txt`, a path relative to the top directory | | |
| A-TR-3.2 | Items 11 and 12 | The output is `vfat` | | |

#### Step 1.11: Decide on A-P-7

A-P-7 misses two of its four parts in every run since M1. Read
[OD-1](#od-1-a-p-7-small-file-copy-and-move). Record your decision in the
[decision record](#decision-record).

### Part 2: Phase 2

Source: the [phase 2 plan](../plans/implemented/2026-09-28-manycommander-phase2.md), the
[phase 2 design](../specs/implemented/2026-09-28-manycommander-phase2-design.md) sections 3 to 10, and
the [find and rename page](../../site/content/docs/find-and-rename.md).

#### Step 2.1: Confirm the phase 2 chords in Ghostty and foot

This step closes the phase 2 plan's open item "New chords in the terminals".

1. In Ghostty, run `mcv keys2-ghostty "$PG/keys" "$PG/keys/dir-b"`.
2. In a second terminal, run `tail -f "$PG/logs/keys2-ghostty.log" | grep --line-buffered ' key '`.
3. Do the rows of the table in order.
4. Press `Esc` to empty the command line. Press `F10`.
5. Do items 1 to 4 again in foot, with the log name `keys2-foot`.
6. Record the results as `Ghostty / foot`.

The log computes `action` as if the command line does not have the focus. With text on the
line, the log still shows `action=Directories`, `Filter` or `MultiRename`. The screen decides
rows OV-P2-K09 to OV-P2-K12.

| ID | Check | Expected | Result (Ghostty / foot) | Notes |
|---|---|---|---|---|
| OV-P2-K01 | Press `Ctrl+D`, then `Esc` | `action=Directories`. The "Go to directory" dialog opens and closes | | |
| OV-P2-K02 | Press `Ctrl+F`, then `Esc` | `action=Filter`. The filter line opens and closes | | |
| OV-P2-K03 | Move the cursor to `dir-a`. Press `Ctrl+M`, then `Esc` | `action=MultiRename` with `code=Char('m')`, not `Enter`. The multi-rename tool opens for `dir-a` and closes | | |
| OV-P2-K04 | Press `Alt+F7`, then `Esc` | `action=Find`. The find form opens and closes | | |
| OV-P2-K05 | Press `Shift+F2`, then `Esc` | `action=Compare`. The compare form opens and closes | | |
| OV-P2-K06 | Move the cursor to `file-1.txt`. Press `Alt+L`, then `Esc` | `action=Link`. The link form opens and closes | | |
| OV-P2-K07 | Press `Alt+A`, then `Esc` | `action=Attributes`. The form's mode label shows `Mode (now 0644)` | | |
| OV-P2-K08 | Press `Ctrl+Z` | `action=None`. Nothing happens. manycommander does not stop | | |
| OV-P2-K09 | Press `Ctrl+E`. Type `echo mc-verify-ran`. Press `Ctrl+D` | No dialog opens. The line keeps its text | | |
| OV-P2-K10 | With the same text, press `Ctrl+F` | No filter line opens. The line keeps its text | | |
| OV-P2-K11 | With the same text, press `Ctrl+M` | Nothing runs: no `[exit 0]` screen. The line keeps its text | | |
| OV-P2-K12 | With the same text, press `Alt+F7`, `Shift+F2`, `Alt+L` and `Alt+A`. Press `Esc` after each one | Each form opens and closes. The line keeps its text | | |

#### Step 2.2: Go to a directory: bookmarks, frecency, zoxide and z

manycommander expands `$PG` in a `cd` on the command line.

1. Run `mcv p2-dirs "$PG" "$PG/keys"`.
2. On the command line, run `cd $PG/compare/left`, then `cd $PG`. Do this pair three times.
3. On the command line, run `cd $PG/links`, then `cd $PG`.
4. Press `Ctrl+D`.
5. Type `comp le`.
6. Press `Backspace` until the filter is empty.
7. If zoxide is installed, run `zoxide query --list --score | head` in a second terminal.
8. Press `Esc`.
9. On the command line, run `cd $PG/links`.
10. Press `Ctrl+D`, then `Insert`.
11. In the second terminal, run `cat ~/.config/manycommander/hotlist.toml`.
12. Move the dialog's cursor to the bookmark. Press `Delete`.
13. Run `cat ~/.config/manycommander/hotlist.toml` again. Press `Esc` in manycommander.
14. On the command line, run `z left`.
15. On the command line, run `z mc-verify-nomatch`.
16. On the command line, run `z`. Press `Esc`.
17. Press `F10`.
18. Run `cat "$PG/state/manycommander/dirs.tsv"`.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| OV-P2-DJ1 | Item 4 | The frequent directories show `~/mc-verify/compare/left` above `~/mc-verify/links`. `~/mc-verify` itself is not in the list | | |
| OV-P2-DJ2 | Item 5 | `~/mc-verify/compare/left` shows. `~/mc-verify/links` does not show | | |
| OV-P2-DJ3 | Item 7 | The dialog also shows directories from zoxide's list. `n/a` without zoxide | | |
| OV-P2-DJ4 | Items 10 and 11 | A row marked `*` shows `~/mc-verify/links`. `hotlist.toml` holds the path | | |
| OV-P2-DJ5 | Items 12 and 13 | The bookmark row goes. `hotlist.toml` no longer holds the path | | |
| OV-P2-DJ6 | Item 14 | The panel shows `compare/left` | | |
| OV-P2-DJ7 | Items 15 and 16 | `z mc-verify-nomatch` says `z: no match`. `z` alone opens the dialog | | |
| OV-P2-DJ8 | Item 18 | Lines with a rank, a time and the visited paths, after the line `# manycommander dirs v1` | | |

#### Step 2.3: Filter a panel, and act only on what you see (I-8)

On the filter line, `Ctrl+F` closes the line and keeps the filter. `Enter` also keeps the
filter, but `Enter` then opens the entry under the cursor.

1. Run `mcv p2-filter "$PG/filter" "$PG/filter-out"`.
2. Mark `alpha` and `photo-1.jpg` with `Insert`.
3. Press `Ctrl+F`. Type `photo`.
4. Press `Ctrl+F`.
5. Press `F5`, then `Enter`.
6. Run `ls "$PG/filter-out"`.
7. Type `beta`. A typed character starts a new filter. Press `Ctrl+F`.
8. Move the cursor to `beta`. Press `F8`. Read the dialog. Press `Esc`.
9. Press `Ctrl+F`, then `Esc`.
10. Press `Ctrl+F`. Type `*.jpg`. Press `Ctrl+F`.
11. Move the cursor to `..` and press `Enter`. Then press `Alt+Left`.
12. Press `F10`.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| OV-P2-QF1 | Item 3 | The panel shows only `..`, `photo-1.jpg` and `photo-2.jpg` | | |
| OV-P2-QF2 | Item 4 | The footer says `2 of 8 entries (filter: photo)` and counts 1 marked entry | | |
| OV-P2-QF3 | Items 5 and 6 | The dialog names 1 entry. The output is `photo-1.jpg` only. `alpha` is not copied | | |
| OV-P2-QF4 | Items 7 and 8 | The footer counts no marked entry. The trash dialog names 1 entry, `beta`. Nothing goes to the trash | | |
| OV-P2-QF5 | Item 9 | The filter goes. The footer counts 2 marked entries again: `alpha` and `photo-1.jpg` | | |
| OV-P2-QF6 | Item 10 | The panel shows only `..`, `photo-1.jpg` and `photo-2.jpg`. `notes.md` does not show | | |
| OV-P2-QF7 | Item 11 | The panel shows the playground without a filter. After `Alt+Left`, the panel shows every entry of `filter` | | |

#### Step 2.4: Find files and use the results tab

This step mounts `$PG/find-mnt-src` on `$PG/find/mnt` with rclone. The mount is a second
filesystem inside the search tree.

1. Run `rclone mount "$PG/find-mnt-src" "$PG/find/mnt" --daemon --daemon-wait 10s`.
2. Run `ls "$PG/find/mnt"`. The output is `on-mount-needle.txt`.
3. Run `mcv p2-find "$PG/find" "$PG/filter-out"`.
4. Press `Alt+F7`. Type `needle` in the Name field. Keep "Stay on this filesystem" on. Turn
   "Hidden entries" off if it is on: the form copies the panel's setting. Press `Enter`.
5. Press `Alt+F7`. Type `needle`. Move to "Stay on this filesystem" with `Tab`, and press
   `Space` to turn it off. Press `Enter`.
6. Press `Alt+F7`. Type `secret`. Turn "Hidden entries" on. Press `Enter`.
7. Press `Alt+F7`. Keep the Name field empty. Type `needle-verify` in "Containing text".
   Turn "Hidden entries" off if it is on. Keep "Match case" off. Press `Enter`.
8. Do item 7 again with "Match case" on.
9. Press `Ctrl+2` to show the tab of item 4. Move the cursor to `deep/er/has-needle.txt`.
   Press `Enter`.
10. Press `Alt+Left`.
11. Mark `deep/er/has-needle.txt` and `vanish-needle.txt`. Press `F5`, then `Enter`.
12. Run `ls "$PG/filter-out"`.
13. Press `Alt+-`, then `Enter`. The marks of item 11 stay after the copy. Move the cursor to
    `deep/trash-me-needle.txt`. Press `F8`, then `Enter`.
14. Run `rm "$PG/find/vanish-needle.txt"`. Press `Ctrl+R`.
15. Press `F7`.
16. Press `Alt+F7`. Delete the "Search in" text and type `/usr/lib`. Keep the Name field
    empty. Type `mc-verify-nothing` in "Containing text". Press `Enter`. Press `Esc` after
    1 second.
17. Press `F10`.
18. Run `fusermount3 -u "$PG/find/mnt"`.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| OV-P2-FD1 | Item 4 | A new tab `find: needle`. Results: `deep/er/has-needle.txt`, `deep/trash-me-needle.txt`, `vanish-needle.txt`. `mnt/on-mount-needle.txt` is not a result. The footer does not say `(searching)` | | |
| OV-P2-FD2 | Item 5 | The results also hold `mnt/on-mount-needle.txt` | | |
| OV-P2-FD3 | Item 6 | The result is `.hidden/secret.txt` | | |
| OV-P2-FD4 | Item 7 | The results are `deep/er/has-needle.txt` and `upper.txt` | | |
| OV-P2-FD5 | Item 8 | The result is `deep/er/has-needle.txt` only | | |
| OV-P2-FD6 | Items 9 and 10 | `Enter` shows `deep/er` with the cursor on `has-needle.txt`. `Alt+Left` shows the results again | | |
| OV-P2-FD7 | Items 11 and 12 | Both results from two directories are in `filter-out` | | |
| OV-P2-FD8 | Item 13 | The result goes from the tab after the job | | |
| OV-P2-FD9 | Item 14 | `vanish-needle.txt` goes from the tab | | |
| OV-P2-FD10 | Item 15 | The screen shows `not in search results` | | |
| OV-P2-FD11 | Item 16 | The search stops at once. The tab says `(cancelled)` | | |

#### Step 2.5: Rename many files

Each `Ctrl+M` opens the tool with the default fields: Name mask `[N]`, Extension mask `[E]`,
Search empty, Regex off, Case unchanged, and the counter 1, 1, 1. Replace the text of a field
before you type the value of an item.

1. Run `mcv p2-rename "$PG/rename" "$PG"`.
2. Press `Alt+=`, type `IMG_*`, press `Enter`.
3. Press `Ctrl+M`. Type `holiday-[C]` in the Name mask field. Set the counter digits to `3`.
   Set Case to lower with `Left` or `Right`. Read the preview. Press `Enter`.
4. Press `Alt+=`, type `holiday-*`, press `Enter`. Press `Ctrl+M`. Type `^holiday-(\d+)` in
   Search. Type `trip-$1` in Replace. Turn Regex on. Press `Enter`.
5. Press `Alt+-`, then `Enter`. Move the cursor to `my photo.JPG`. Press `Ctrl+M`. Set Case
   to title. Press `Enter`.
6. Press `Alt+=`, type `trip-*`, press `Enter`. Press `Ctrl+M`. Type `same` in the Name
   mask. Read the preview. Replace the Name mask with `[X]`. Read the preview. Press `Enter`.
7. Replace the Name mask with `[N]`. Turn Regex on. Type `(` in Search. Read the preview.
   Press `Esc`.
8. Press `Alt+-`, then `Enter`. Move the cursor to `swap-1`. Press `Ctrl+M`. Type `swap-2` in
   the Name mask. Read the preview. Press `Esc`.
9. Press `Ctrl+F3` so that the name sort is reversed and `swap-2` is above `swap-1`. Mark
   `swap-1` and `swap-2`. Press `Ctrl+M`. Type `swap-[C]` in the Name mask. Read the preview.
   Press `Enter`.
10. Run `cat "$PG/rename/swap-1" "$PG/rename/swap-2"; ls -A "$PG/rename" | grep mc-rename`.
11. Press `Ctrl+M`, then `Ctrl+Z`.
12. Run `cat "$PG/rename/swap-1" "$PG/rename/swap-2"`.
13. Press `Ctrl+F3` to restore the sort. Press `F10`.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| OV-P2-MR1 | Item 3 | The preview shows `IMG_0001.JPG -> holiday-001.jpg` to `IMG_0006.JPG -> holiday-006.jpg`. After `Enter`, the panel shows the new names | | |
| OV-P2-MR2 | Item 4 | The names are `trip-001.jpg` to `trip-006.jpg` | | |
| OV-P2-MR3 | Item 5 | The name is `My Photo.jpg` | | |
| OV-P2-MR4 | Item 6 | `same`: each row shows an error for the same new name. `[X]`: one line above the rows shows `Name mask: [X]: unknown placeholder`. `Enter` renames nothing and shows the first error | | |
| OV-P2-MR5 | Item 7 | One line above the rows shows a regular expression error, for example `Search: unclosed group` | | |
| OV-P2-MR6 | Item 8 | The row shows that the name exists | | |
| OV-P2-MR7 | Item 9 | The preview shows `swap-2 -> swap-1` and `swap-1 -> swap-2`, both ok | | |
| OV-P2-MR8 | Item 10 | The output is `two`, then `one`. `grep` finds no `mc-rename` name | | |
| OV-P2-MR9 | Items 11 and 12 | The output is `one`, then `two` | | |

#### Step 2.6: Compare two directories

1. Run `mcv p2-compare "$PG/compare/left" "$PG/compare/right"`.
2. Press `Shift+F2`. Keep "by date and size" and "include directories". Press `Enter`.
3. Press `Shift+F2`. Change the choice to "by content" with `Right`. Press `Enter`.
4. Press `F10`.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| OV-P2-CD1 | Item 2, left panel | Marked: `newer.txt`, `only-left.txt`, `only-dir`, `size.txt`. Not marked: `same.txt`, `content.txt` | | |
| OV-P2-CD2 | Item 2, right panel | Marked: `only-right.txt`, `size.txt`. Not marked: `same.txt`, `newer.txt`, `content.txt`. The status row sums up the marks | | |
| OV-P2-CD3 | Item 3 | Also marked on both sides: `newer.txt` and `content.txt`. `same.txt` is not marked. The summary counts `differ in content` and `differ in size` | | |

#### Step 2.7: Create links

1. Run `mcv p2-links "$PG/links/src" "$PG/links/dst"`.
2. Move the cursor to `target.txt`. Press `Alt+L`. Keep "symbolic, relative". Press `Enter`.
3. Run `readlink "$PG/links/dst/target.txt"`.
4. Press `Alt+L` again. Press `Enter`. Read the question. Press `Esc` to cancel the job.
5. Press `Alt+L`. Change the destination name to `target-abs.txt`. Choose "symbolic,
   absolute" with `Right`. Press `Enter`.
6. Run `readlink "$PG/links/dst/target-abs.txt"`.
7. Press `Alt+L`. Change the destination name to `target-hard.txt`. Choose "hard". Press
   `Enter`.
8. Run `stat -c '%i %h %n' "$PG/links/src/target.txt" "$PG/links/dst/target-hard.txt"`.
9. Move the cursor to `folder`. Press `Alt+L`. Choose "hard". Press `Enter`.
10. Move the cursor to `target.txt`. Press `Alt+L`. Change the destination to
    `$XDEV/target-hard.txt` with the value of `$XDEV`. Choose "hard". Press `Enter`.
11. Press `F10`.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| OV-P2-LK1 | Item 3 | `../src/target.txt` | | |
| OV-P2-LK2 | Item 4 | A "link exists" question with Skip, Skip all, Rename and Cancel job. No Overwrite | | |
| OV-P2-LK3 | Item 6 | The absolute path of `target.txt` in the playground | | |
| OV-P2-LK4 | Item 8 | Both lines show the same inode number and the link count 2 | | |
| OV-P2-LK5 | Item 9 | The report says `directories cannot be hard-linked` | | |
| OV-P2-LK6 | Item 10 | The report says `hard links cannot cross filesystems` | | |

#### Step 2.8: Change attributes

> **Warning:** Item 5 removes the read and execute permission from `$PG/attr/tree`. Item 6
> gives them back. If item 6 fails, run `chmod -R u+rwx "$PG/attr/tree"`.

1. Run `mcv p2-attr "$PG/attr"`.
2. Move the cursor to `solo.txt`. Press `Alt+A`. Read the Mode label. Type `600`. Read the
   preview. Press `Enter`.
3. Press `Alt+A`. Type `u+x,g+r,o=r`. Press `Enter`.
4. Run `stat -c '%a %n' "$PG/attr/solo.txt"`.
5. Move the cursor to `tree`. Press `Alt+A`. Type `a-rx`. Turn Recursive on. Press `Enter`.
6. Press `Alt+A`. Type `u+rx`. Turn Recursive on. Press `Enter`.
7. Run `stat -c '%a %n' "$PG/attr/tree" "$PG/attr/tree/sub" "$PG/attr/tree/file.txt" "$PG/attr/tree/sub/deep.txt" "$PG/attr/outside.txt"`.
8. Press `Alt+A`. Keep Mode empty. Type `2026-01-01 12:00` in "Modification time". Keep
   Recursive off. Press `Enter`.
9. Press `Enter` on `tree`. Move the cursor to `link-out`. Press `Alt+A`. Keep Mode empty.
   Type `2026-01-02 12:00`. Press `Enter`.
10. Run `stat -c '%y %n' "$PG/attr/tree" "$PG/attr/tree/link-out" "$PG/attr/outside.txt"`.
11. Press `F10`.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| OV-P2-AT1 | Item 2 | The label shows `Mode (now 0644)`. The preview shows `rw-r--r-- -> rw-------` | | |
| OV-P2-AT2 | Items 3 and 4 | `744` and the path of `solo.txt` | | |
| OV-P2-AT3 | Items 5 and 6 | Both jobs end without an error for a file. The report of item 5 says that `link-out` has no mode of its own | | |
| OV-P2-AT4 | Item 7 | `tree`, `sub`, `file.txt` and `deep.txt` show `700`. `outside.txt` shows `644` | | |
| OV-P2-AT5 | Items 8 to 10 | `tree` shows `2026-01-01 12:00:00`. `link-out` shows `2026-01-02 12:00:00`. `outside.txt` keeps its time | | |

#### Step 2.9: Keep sparse files and hard links on copy and move

`$XDEV` is on tmpfs, a second filesystem.

1. Run `du -k "$PG/fidelity/sparse.img"; ls -l "$PG/fidelity/sparse.img"`.
2. Run `mcv p2-fidelity "$PG/fidelity" "$XDEV/copy"`.
3. Move the cursor to `sparse.img`. Press `F5`, then `Enter`.
4. Run `du -k "$XDEV/copy/sparse.img"; ls -l "$XDEV/copy/sparse.img"; cmp "$PG/fidelity/sparse.img" "$XDEV/copy/sparse.img"`.
5. Move the cursor to `hl`. Press `F5`, then `Enter`.
6. Run `stat -c '%h %i %n' "$XDEV/copy/hl/"*`.
7. Press `Tab`. On the command line, run `cd $XDEV/move`. Press `Tab`.
8. Move the cursor to `hl`. Press `F6`, then `Enter`. Read the report.
9. Run `stat -c '%h %i %n' "$XDEV/move/hl/"*; ls "$PG/fidelity"`.
10. Press `F10`.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| OV-P2-SP1 | Item 1 | `du` shows about 4096 KiB. `ls -l` shows 1073741824 bytes | | |
| OV-P2-SP2 | Items 3 and 4 | `du` shows at most 4160 KiB. `ls -l` shows 1073741824 bytes. `cmp` prints nothing | | |
| OV-P2-HL1 | Items 5 and 6 | `a` and `b` share one inode with the link count 2. `t1`, `t2` and `t3` share one inode with the link count 3. `c` has the link count 1 | | |
| OV-P2-HL2 | Item 8 | The report does not say "source changed" | | |
| OV-P2-HL3 | Item 9 | The same link structure as OV-P2-HL1 in `move/hl`. `ls` does not show `hl` in `fidelity` | | |

#### Step 2.10: Search over a stalled FUSE mount again (A-FD-7, optional)

The phase 2 session ran A-FD-7 and recorded the result in the
[open items](../plans/implemented/2026-09-28-manycommander-phase2.md#open-items). Do items 1 to 11 by
hand, or item 12 instead.

> **Warning:** Do not open the stuck mount point in another program. Leave the fixture shell
> with `exit`.

1. In Ghostty, run `"$MC_REPO/scripts/fixtures/stall-fuse.sh" bash`.
2. In the new shell, run `source ~/mc-verify/env.sh`.
3. Run `echo "$MC_STALL_DIR"`. Note the path.
4. Wait 2 seconds.
5. Run `mcv a-fd-7 "$MC_STALL_DIR/src" "$MC_STALL_DIR/fine"`.
6. Press `Alt+F7`. Replace the "Search in" text with the path of item 3. Type `ok` in the
   Name field. Keep "Stay on this filesystem" on. Press `Enter`.
7. Press `Alt+F7`. Do item 6 again with "Stay on this filesystem" off.
8. Press `F1`, then `Esc`. Press `Tab` two times.
9. Press `Esc` in the results tab.
10. Press `F10`.
11. Run `exit`.
12. Alternative: run the manual test of the phase 2 session:

    ```bash
    cd "$MC_REPO" && MC_MANUAL=1 cargo test --test manual a_fd_7 -- --ignored --nocapture
    ```

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| A-FD-7.1 | Item 6 | The search ends. The tab shows `fine/ok`. The footer does not say `(searching)` | | |
| A-FD-7.2 | Items 7 and 8 | The footer says `(searching)`. The help opens and closes at once. `Tab` works at once | | |
| A-FD-7.3 | Item 9 | The tab says `(cancelled)` | | |
| A-FD-7.4 | Items 10 and 11 | manycommander quits. The fixture unmounts. `findmnt -t fuse.rclone` shows no `mc-stall` mount point | | |
| A-FD-7.5 | Item 12, as an alternative | The test passes. The `EVIDENCE A-FD-7` lines show the cleanup | | |

### Part 3: Phase 3

Source: the [phase 3 plan](../plans/2026-09-28-manycommander-phase3.md), the
[phase 3 design](../specs/implemented/2026-09-28-manycommander-phase3-design.md) sections 3 to 6, and the
pages [archives](../../site/content/docs/archives.md),
[quick view](../../site/content/docs/quick-view.md) and [SFTP](../../site/content/docs/sftp.md).

> **Warning:** A crash of Ghostty closes every window and tab of the Ghostty process. For the
> Ghostty items of Steps 3.1, 3.2 and 3.6, start a separate Ghostty process:
> `ghostty --gtk-single-instance=false`. Record a crash as `fail`, with the time.

#### Step 3.1: Confirm the phase 3 chords in Ghostty and foot

The phase 3 plan's Verification section asks for this check of `Ctrl+Q`, `Alt+Q` and `Alt+O`.

1. In Ghostty, run `mcv keys3-ghostty "$PG/images" "$PG/archives"`.
2. In a second terminal, run `tail -f "$PG/logs/keys3-ghostty.log" | grep --line-buffered ' key '`.
3. Do the rows of the table in order.
4. Press `F10`.
5. Do items 1 to 4 again in foot, with the log name `keys3-foot`.
6. Record the results as `Ghostty / foot`.

| ID | Check | Expected | Result (Ghostty / foot) | Notes |
|---|---|---|---|---|
| OV-P3-K01 | Press `Ctrl+Q`, then `Ctrl+Q` | `action=QuickView`. The right side becomes the quick view, then the panel again | | |
| OV-P3-K02 | Press `Ctrl+Q`. Move the cursor to `upright.jpg`. Press `Alt+Q` | `action=QuickLoad`. The picture shows. The quick view loads a local picture when the cursor stops, so `Alt+Q` makes no visible change here. The `key` line is the check | | |
| OV-P3-K03 | Press `Ctrl+E`. Type `abc`. Press `Ctrl+Q` two times | The view turns off and on. The line keeps `abc` | | |
| OV-P3-K04 | With `abc` on the line, move the cursor to `burst-10.jpg` and press `Alt+Q` | The picture shows. The line keeps `abc` | | |
| OV-P3-K05 | Press `Ctrl+Q` and `Tab`. Move the cursor to `book.epub`. With `abc` on the line, press `Alt+O` | Nothing opens. The line keeps `abc`. The log shows `action=OpenArchive` | | |
| OV-P3-K06 | Press `Esc`. Press `Alt+O` on `book.epub` | `action=OpenArchive`. The panel shows `book.epub` as an archive with `tree` | | |

#### Step 3.2: Check the quick view in Ghostty, foot and tmux (A-QV-8)

This step expands the 13 checks of the plan's
[A-QV-8 checklist](../plans/2026-09-28-manycommander-phase3.md#a-qv-8-manual-checklist-owner).

> **Warning:** Item 13 changes the theme of the whole desktop. Item 14 changes it back.

Ghostty without tmux (A-QV-8.1 to A-QV-8.7):

1. In Ghostty, run `mcv qv-ghostty "$PG/images" "$PG/keys"`.
2. Press `Ctrl+Q`.
3. Run `grep 'terminal probe' "$PG/logs/qv-ghostty.log"`.
4. Move the cursor to `upright.jpg`. Let the cursor rest.
5. Move the cursor to `rotated.jpg`. Let the cursor rest.
6. Move the cursor to `anim.gif`. Let the cursor rest.
7. Hold `Down` over the `burst-*.jpg` files. Then let the cursor rest.
8. Move the cursor to `wide.png`, then to `notes.txt`.
9. Move the cursor to `upright.jpg`. Press `F7`. Press `Esc`. Press `F1`. Press `Esc`.
10. Press `Ctrl+E`. Type `true`. Press `Enter`. Look at the `[exit 0]` screen. Press `Enter`.
11. Press the terminal's font size keys: `Ctrl+=` two times, then `Ctrl+-` two times.
12. Change the size of the Ghostty window, for example with full screen on and off.
13. In a second terminal, run `omarchy-theme-set catppuccin`. Use a theme other than yours.
14. Run `omarchy-theme-set "$(cat "$PG/theme-before")"`.
15. Press `Tab`. Press `Tab` again. Press `Ctrl+U`. Press `Ctrl+U` again.
16. Press `Ctrl+Q`.
17. Press `Ctrl+Q`. Let the cursor rest on `upright.jpg`. Press `F10`.

foot without tmux (A-QV-8.8):

18. In foot, run `mcv qv-foot "$PG/images" "$PG/keys"`.
19. Do items 2 to 17 in foot. Read the probe line of `qv-foot.log`.

Ghostty in tmux with `allow-passthrough on` (A-QV-8.9):

20. In a new Ghostty window, run the command below. The command starts a tmux server with
    the Omarchy configuration on its own socket:

    ```bash
    tmux -L mcv -f /usr/share/omarchy/config/tmux/tmux.conf new-session -s mcv
    ```

21. Run `tmux show -gv allow-passthrough; tmux show -p allow-passthrough`. Note the output.
22. Run `source ~/mc-verify/env.sh`. Run `mcv qv-tmux-on "$PG/images" "$PG/keys"`.
23. Press `Ctrl+Q`. Let the cursor rest on `upright.jpg`. Press `F10`.
24. Run `grep 'terminal probe' "$PG/logs/qv-tmux-on.log"`.
25. Run `tmux show -gv allow-passthrough; tmux show -p allow-passthrough` again.

Ghostty in tmux with `allow-passthrough` off (A-QV-8.10):

26. In a new Ghostty window, run `tmux -L mcv-plain -f /dev/null new-session -s plain`.
27. Run `tmux show -gv allow-passthrough; tmux show -p allow-passthrough`. Note the output.
28. Run `source ~/mc-verify/env.sh`. Run `mcv qv-tmux-off "$PG/images" "$PG/keys"`.
29. Press `Ctrl+Q`. Let the cursor rest on `upright.jpg`. Press `F10`.
30. Run `grep 'terminal probe' "$PG/logs/qv-tmux-off.log"`.
31. Run `tmux show -gv allow-passthrough; tmux show -p allow-passthrough` again.

foot in tmux (A-QV-8.11):

32. In foot, run `tmux -L mcv-foot -f /usr/share/omarchy/config/tmux/tmux.conf new-session -s foot`.
33. Run `source ~/mc-verify/env.sh`. Run `mcv qv-foot-tmux "$PG/images" "$PG/keys"`.
34. Press `Ctrl+Q`. Let the cursor rest on `upright.jpg`, then on `rotated.jpg`. Press `F10`.
35. Run `grep 'terminal probe' "$PG/logs/qv-foot-tmux.log"`.

The configuration and `NO_COLOR` (A-QV-8.12), in Ghostty without tmux:

36. Run the commands below:

    ```bash
    mkdir -p "$PG/cfg-halfblocks/manycommander"
    printf '[preview]\nprotocol = "halfblocks"\n' > "$PG/cfg-halfblocks/manycommander/config.toml"
    ```

37. Run `XDG_CONFIG_HOME="$PG/cfg-halfblocks" mcv qv-halfblocks "$PG/images" "$PG/keys"`.
38. Press `Ctrl+Q`. Let the cursor rest on `upright.jpg`. Press `F10`.
39. Run `NO_COLOR=1 mcv qv-nocolor "$PG/images" "$PG/keys"`.
40. Press `Ctrl+Q`. Let the cursor rest on `upright.jpg`. Press `F10`.
41. Run `grep -h 'terminal probe' "$PG/logs/qv-halfblocks.log" "$PG/logs/qv-nocolor.log"`.

Text on the command line (A-QV-8.13):

42. Run `mcv qv-line "$PG/images" "$PG/keys"`.
43. Press `Ctrl+E`. Type `abc`. Press `Ctrl+Q`. Move the cursor to `upright.jpg` with `Down`.
44. Press `Alt+Q`. Press `Ctrl+Q`. Press `Esc`. Press `F10`.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| A-QV-8.1 | Items 3 to 8 | `protocol="kitty"`, `tmux=false`. The picture shows about 100 ms after the cursor rests. `TOP` is at the top in `rotated.jpg`. `anim.gif` shows a still red picture. During fast scrolling the view shows only cards. `wide.png` shows the card `image larger than 16384 x 16384 px`. `notes.txt` shows a card with its first lines | | |
| A-QV-8.2 | Item 9 | The dialog and the help hide the picture. Nothing draws over them. The picture returns after `Esc` | | |
| A-QV-8.3 | Item 10 | The `[exit 0]` screen shows no picture. The picture returns after the second `Enter`. `F3` is not the hand-off here: `F3` on a picture opens the picture in its application | | |
| A-QV-8.4 | Items 13 and 14 | The picture returns after each redraw of the theme change | | |
| A-QV-8.5 | Items 11 and 12 | The view shows the card, then the picture fits the pane again. The picture never extends past the pane | | |
| A-QV-8.6 | Items 15 and 16 | No old pixels stay on either side | | |
| A-QV-8.7 | Item 17 | The shell screen shows no picture | | |
| A-QV-8.8 | Items 18 and 19 | `protocol="sixel"`. Items 2 to 17 give the same results as in Ghostty. No sixel pixels stay behind. The screen never scrolls | | |
| A-QV-8.9 | Items 20 to 25 | `tmux=true`, `protocol="kitty (tmux, unicode placeholders)"`. Note in Notes whether the picture shows. The output of item 25 equals the output of item 21 | | |
| A-QV-8.10 | Items 26 to 31 | `probe_us` is about 100000. `protocol="halfblocks"` and a halfblocks picture (see the note below). The output of item 31 equals the output of item 27 | | |
| A-QV-8.11 | Items 32 to 35 | `protocol="halfblocks"`, also with `sixel=true` in the probe line: inside tmux the automatic choice never takes sixel. The picture shows, and no pixels stay behind | | |
| A-QV-8.12 | Items 36 to 41 | `qv-halfblocks`: `protocol="halfblocks"` and a picture in full colour. `qv-nocolor`: `protocol="off"` and the card only | | |
| A-QV-8.13 | Items 42 to 44 | `Ctrl+Q` and `Alt+Q` act. The line keeps `abc` until `Esc` | | |

Note on A-QV-8.10 and A-QV-8.11: inside tmux the terminal's answer comes from tmux. A tmux
that has sixel reports sixel even when Ghostty around it cannot draw it. Since commit
`e0e85d2` the automatic choice inside tmux is kitty graphics or halfblocks, never sixel.
`preview.protocol = "sixel"` in the configuration still selects sixel, for foot inside tmux.

#### Step 3.3: Browse and extract archives

"Leave the archive" means: press `Backspace` until the panel shows the directory that holds
the archive.

1. Run `mcv p3-archives "$PG/archives" "$PG/extract/mc"`.
2. Move the cursor to `tree.zip` and press `Enter`. Read the title. Leave the archive.
3. Move the cursor to `book.epub`. Press `Alt+O`. Leave the archive.
4. Move the cursor to `fake.zip` and press `Enter`.
5. Press `Enter` on `tree.tar.xz`. Look at `tree`. Leave the archive. Do the same for
   `tree.tar.gz` and `tree.7z`.
6. Press `Enter` on `tree.tar.gz`. Leave the archive. At once, press `Enter` on
   `tree.tar.gz` again. Leave the archive.
7. Press `Enter` on `real.pkg.tar.zst`. Press `Alt+.` until `.PKGINFO` shows. Read the
   footer. Leave the archive.
8. Press `Enter` on `tree.zip`, then on `tree`. Move the cursor to `README.txt`. Press `F3`.
   Quit the pager.
9. Run `ls -A "$XDG_RUNTIME_DIR/manycommander/view/"`.
10. Press `Enter` on `docs`. Move the cursor to `notes.txt`. Press `F4`. Add a line. Save the
    file and quit the editor.
11. Run `cat` with the path that the screen shows.
12. Press `Ctrl+Q`. Move the cursor to `picture.jpg`. Let the cursor rest. Press `Ctrl+Q`.
13. Press `F8`. Read the screen. Press `F7`. Read the screen.
14. Leave the archive. Press `Enter` on `tree.tar.xz`. Go to `tree/docs`. Press `Ctrl+Q`. Let
    the cursor rest on `picture.jpg`. Press `Alt+Q`. Press `Ctrl+Q`. Leave the archive.
15. Press `Tab`. On the command line, run `cd $PG/archives`. Press `Enter` on `tree.zip`.
    Press `Tab`. Move the cursor to `fake.zip`. Press `F5`.
16. Press `Tab`. Leave the archive. On the command line, run `cd $PG/extract/mc`. Press `Tab`.
17. Press `Enter` on `real.pkg.tar.zst`. Press `Alt+.` until the entries that start with `.`
    show. Press `Ctrl+A`. Press `F5`. Check that the destination is the `extract/mc`
    directory of the playground. Press `Enter`.
18. Run the commands below:

    ```bash
    bsdtar -xf "$PG/archives/real.pkg.tar.zst" -C "$PG/extract/bsdtar"
    diff -r --no-dereference "$PG/extract/mc" "$PG/extract/bsdtar"
    ```

19. Leave the archive. Press `Enter` on `big.tar.xz`. Read the footer. Press `Esc` within
    3 seconds.
20. Run `grep -A 3 'code=Esc' "$PG/logs/p3-archives.log" | tail -n 4`.
21. Press `F10`.
22. Run `grep 'archive scan done' "$PG/logs/p3-archives.log" | grep -c 'tree.tar.gz'`.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| OV-P3-AR1 | Item 2 | The panel lists `tree`. The title ends in `tree.zip:/`. `..` at the archive root returns to `archives` with the cursor on `tree.zip` | | |
| OV-P3-AR2 | Item 3 | `book.epub` opens as an archive and lists `tree` | | |
| OV-P3-AR3 | Item 4 | The panel footer ends in `fake.zip: not a zip archive`. The panel stays in `archives` | | |
| OV-P3-AR4 | Item 5 | Each archive lists `README.txt`, `bin/hello.sh`, `docs/notes.txt`, `docs/picture.jpg`, and `docs/readme-link` as a symbolic link | | |
| OV-P3-AR5 | Items 6 and 22 | Each entry of item 6 is instant. The count of item 22 is `1` | | |
| OV-P3-AR6 | Item 7 | The panel lists the package. `.PKGINFO`, `.BUILDINFO` and `.MTREE` show. The footer shows the entry count and the unpacked size | | |
| OV-P3-AR7 | Items 8 and 9 | The pager shows `readme`. The view directory holds no copy after the pager | | |
| OV-P3-AR8 | Items 10 and 11 | The screen shows `archives are read-only; your edited copy is at` and a path. The file at that path holds your line | | |
| OV-P3-AR9 | Item 12 | A zip member: the picture shows when the cursor rests | | |
| OV-P3-AR10 | Item 13 | Each key shows `archives are read-only` | | |
| OV-P3-AR11 | Item 14 | A compressed-tar member: the card says `compressed archive member: Alt+Q previews it`. `Alt+Q` shows the picture | | |
| OV-P3-AR12 | Item 15 | `F5` into an archive panel shows `archives are read-only` | | |
| OV-P3-AR13 | Items 17 and 18 | The job reports every entry done. `diff` prints nothing | | |
| OV-P3-AR14 | Items 19 and 20 | The footer shows `reading archive: ... of ... MB`. After `Esc` the panel returns at once. The `frame` line after the `Esc` key shows `key_to_flush_us` below 100000 | | |

#### Step 3.4: Connect to a real server over SFTP (A-SF-12)

This step is A-SF-12 of the phase 3 design and the SFTP tour. Replace `HOST` with your
`Host` entry. `RUSER` and `PORT` are the values that `ssh -G HOST` shows for `user` and
`port`.

> **Warning:** Do not stop the server's sshd for these checks. You can lose access to the
> server. A-SF-12.22 ends the connection from the laptop side.

Preparation:

1. Run `ssh -G HOST | awk '$1 == "user" || $1 == "port" || $1 == "proxyjump" || $1 ~ /^serveralive/'`.
2. Prepare the server directory:

   ```bash
   ssh HOST 'mkdir -p mc-verify/down mc-verify/up mc-verify/deltree/sub \
     && printf "line one\n" > mc-verify/down/remote.txt \
     && printf "fetch me\n" > mc-verify/down/fetch-move.txt \
     && head -c 3000000 /dev/urandom > mc-verify/down/blob.bin \
     && ln -sfn remote.txt mc-verify/down/link.txt \
     && ln -sfn ../down mc-verify/deltree/link-to-down \
     && printf "x\n" > mc-verify/deltree/sub/file.txt'
   ```

3. Prepare an ssh configuration with an empty `known_hosts` file, for the first-connection
   prompt. Your own `known_hosts` stays unchanged:

```bash
: > "$PG/ssh/known_hosts_first"
cat > "$PG/ssh/first.conf" <<EOF
UserKnownHostsFile $PG/ssh/known_hosts_first
GlobalKnownHostsFile /dev/null
StrictHostKeyChecking ask
Include $HOME/.ssh/config
EOF
mkdir -p "$PG/cfg-first/manycommander"
printf '[sftp]\nssh = ["ssh", "-F", "%s"]\n' "$PG/ssh/first.conf" > "$PG/cfg-first/manycommander/config.toml"
```

Prompts and the connect hand-off:

4. Run `XDG_CONFIG_HOME="$PG/cfg-first" mcv sftp-first "$PG/sftp-local"`.
5. On the command line, run `cd sftp://HOST/~/mc-verify`.
6. At the host-key question, press `Ctrl+C`. Press `Enter`.
7. Do items 5 and 6 again, but press `Ctrl+Z` at the host-key question. If the prompt does
   not end, press `Ctrl+C`, or `Ctrl+Z` again. Press `Enter`.
8. Do item 5 again. Answer `yes` to each host-key question.
9. Press `F10`. Run `wc -l "$PG/ssh/known_hosts_first"`.
10. Run `(unset SSH_AUTH_SOCK; mcv sftp-passphrase "$PG/sftp-local")`.
11. Do item 5 again. Type the passphrase of your key at ssh's prompt. Press `F10`.
12. Optional, for a changed host key. Run the block below. The block copies your
    `known_hosts`, and puts a wrong key for `HOST` into the copy:

```bash
hn="$(ssh -G HOST | awk '$1 == "hostname" {print $2}')"
port="$(ssh -G HOST | awk '$1 == "port" {print $2}')"
hka="$(ssh -G HOST | awk '$1 == "hostkeyalias" {print $2}')"
key="$hn"; [ "$port" = 22 ] || key="[$hn]:$port"; [ -z "$hka" ] || key="$hka"
ssh-keygen -q -t ed25519 -N '' -f "$PG/ssh/fake"
cp ~/.ssh/known_hosts "$PG/ssh/known_hosts_changed" 2>/dev/null || : > "$PG/ssh/known_hosts_changed"
ssh-keygen -R "$key" -f "$PG/ssh/known_hosts_changed"
printf '%s %s\n' "$key" "$(cut -d ' ' -f 1,2 "$PG/ssh/fake.pub")" >> "$PG/ssh/known_hosts_changed"
cat > "$PG/ssh/changed.conf" <<EOF
UserKnownHostsFile $PG/ssh/known_hosts_changed
GlobalKnownHostsFile /dev/null
StrictHostKeyChecking yes
Include $HOME/.ssh/config
EOF
mkdir -p "$PG/cfg-changed/manycommander"
printf '[sftp]\nssh = ["ssh", "-F", "%s"]\n' "$PG/ssh/changed.conf" > "$PG/cfg-changed/manycommander/config.toml"
```

13. Optional: run `XDG_CONFIG_HOME="$PG/cfg-changed" mcv sftp-changed "$PG/sftp-local"`. Do
    item 5 again. Press `Enter`, then `F10`.

Browse, view and edit:

14. Run `mcv sftp "$PG/sftp-local" "$PG/sftp-local"`.
15. On the command line, run `cd sftp://HOST/~/mc-verify`.
16. Press `Enter` on `down`. Press `Backspace`. Press `Space` on `down`.
17. Press `Enter` on `down`. Move the cursor to `remote.txt`. Press `F3`. Quit the pager.
    Run `ls -A "$XDG_RUNTIME_DIR/manycommander/view/"`.
18. Press `F4`. Quit the editor without a change.
19. Press `F4`. Add a line. Save and quit. Answer `Upload`. Run `ssh HOST cat mc-verify/down/remote.txt`.
20. Press `F4`. Add a line. Before you save, run
    `ssh HOST 'printf "server change\n" >> mc-verify/down/remote.txt'` in the second terminal.
    Save and quit. Answer `Save as "remote (1).txt"`.
21. Run `ssh HOST ls mc-verify/down`.

Transfers:

22. Mark `blob.bin` and `link.txt`. Press `F5`, then `Enter`.
23. Run the commands below:

    ```bash
    sha256sum "$PG/sftp-local/blob.bin"; ssh HOST sha256sum mc-verify/down/blob.bin
    readlink "$PG/sftp-local/link.txt"
    ```

24. Press `Backspace`. Press `Enter` on `up`. Press `Tab`. Mark `up-1.txt`, `up-dir` and
    `picture.jpg`. Press `F5`, then `Enter`.
25. Run `ssh HOST ls -AR mc-verify/up`.
26. Press `Alt+-`, then `Enter`. The marks of item 24 stay after the upload. Move the cursor to
    `move-me.txt`. Press `F6`. Read the dialog. Press `Enter`.
27. Run `ls "$PG/sftp-local"; ssh HOST ls mc-verify/up`.
28. Press `Tab`. Press `Backspace`. Press `Enter` on `down`. Move the cursor to
    `fetch-move.txt`. Press `F6`. Read the dialog. Press `Enter`. Read the report.
29. Run `ls "$PG/sftp-local"; ssh HOST ls mc-verify/down`.

Changes on the server:

30. Press `Tab`. On the command line, run `cd sftp://HOST/~/mc-verify/up`.
    Press `Tab`. The left panel is in `down`, the right panel in `up`.
31. Move the cursor to `blob.bin`. Press `F6`, then `Enter`. Run `ssh HOST ls mc-verify/down mc-verify/up`.
32. Press `Tab`. Move the cursor to `up-1.txt`. Press `Shift+F6`. Replace the name with
    `renamed.txt`. Press `Enter`.
33. Press `Shift+F6` on `renamed.txt`. Replace the name with `move-me.txt`. Press `Enter`.
    Read the question. Answer Skip.
34. Press `F7`. Type `newdir`. Press `Enter`. Press `F7` again. Type `newdir`. Press `Enter`.
35. Press `Backspace`. Move the cursor to `deltree`. Press `Shift+F8`. Answer the first
    question with `Enter`. Type `delete` at the second question. Press `Enter`. Run
    `ssh HOST ls mc-verify mc-verify/down`.
36. Press `F8` on `down`.
37. Press `Enter` on `up`. Press `Ctrl+Q`. Let the cursor rest on `picture.jpg`. Press
    `Alt+Q`. Press `Ctrl+Q`.

Connection loss, the pool and bookmarks:

38. In the second terminal, run `pkill -TERM -P "$(mcpid sftp)" -x ssh`.
39. Press `F7`. Press `Esc` if a dialog opens.
40. Press `Ctrl+R`. Answer ssh's prompts, if any.

> **Warning:** Item 41 turns off all networks of the laptop. Other programs also lose the
> network. On a tailnet, the DNS service of `tailscaled` sometimes stays silent after the
> networks come back. Then ssh cannot resolve `HOST`, and `Ctrl+R` shows
> `Could not resolve hostname`. To repair the DNS service, run
> `sudo systemctl restart tailscaled`. Then press `Enter` and `Ctrl+R` again.

41. Optional: in the second terminal, run `nmcli networking off; sleep 75; nmcli networking on`.
    The `sleep` time must be longer than `serveraliveinterval` x `serveralivecountmax` from
    item 1. Wait for the shell prompt. Press `Ctrl+R`. Skip this item when
    `serveraliveinterval` is `0`.
42. Press `Ctrl+T`. On the command line, run `cd sftp://HOST:PORT/~/mc-verify`.
43. Press `Ctrl+T`. On the command line, run `cd sftp://RUSER@HOST/~/mc-verify`.
44. Press `Ctrl+T`. On the command line, run `cd sftp://RUSER@HOST:PORT/~/mc-verify`.
45. Run `pgrep -P "$(mcpid sftp)" -x ssh | wc -l`.
46. Optional: press `Ctrl+T`. On the command line, run `cd sftp://HOST2/`. Run the command of
    item 45 again. Run `grep 'least recently used' "$PG/logs/sftp.log"`.
47. Press `Ctrl+1`. Press `Ctrl+D`, then `Insert`. Press `Esc`.
48. Run `grep url ~/.config/manycommander/hotlist.toml`.
49. Press `F10`. Run `mcv sftp-restart`. Do not give a directory.
50. In the second terminal, run `grep -c 'sftp connect' "$PG/logs/sftp-restart.log"`.
51. Press `Ctrl+D`. Move the cursor to the `sftp://` bookmark. Press `Enter`.
52. Press `Ctrl+D`. Move the cursor to the `sftp://` bookmark. Press `Delete`. Press `Esc`.
    Press `F10`.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| A-SF-12.1 | Item 5 | The screen shows `connecting to sftp://HOST ... (Ctrl+C cancels)`, then ssh's host-key question | | |
| A-SF-12.2 | Item 6: `Ctrl+C` at the prompt | ssh's messages stay above `[connection failed] press Enter to return`. After `Enter`, the panel is still `sftp-local` | | |
| A-SF-12.3 | Item 7: `Ctrl+Z` at the prompt (E-20) | manycommander does not hang. `Ctrl+C` or a second `Ctrl+Z` gives `[connection failed] press Enter to return`. With a ProxyJump host, the first `Ctrl+Z` can stop only the proxy | | |
| A-SF-12.4 | Items 8 and 9 | The panel shows `down`, `up` and `deltree`. The screen shows `connected to sftp://HOST`. `known_hosts_first` holds the new key lines | | |
| A-SF-12.5 | Item 1 and item 8, ProxyJump | When item 1 shows `proxyjump`, the connect of item 8 works through the jump host. `n/a` without a jump host | | |
| A-SF-12.6 | Items 10 and 11 | ssh asks for the passphrase in the hand-off. After the passphrase, the panel shows the server directory. `n/a` for a key without a passphrase; note a security-key touch prompt instead | | |
| A-SF-12.7 | Items 12 and 13, optional | ssh prints `REMOTE HOST IDENTIFICATION HAS CHANGED` and refuses. The screen shows `[connection failed] press Enter to return`. The panel stays | | |
| A-SF-12.8 | Item 16 | `Enter` and `Backspace` move on the server. `Space` shows the size of `down` | | |
| A-SF-12.9 | Item 17 | The pager shows `line one`. The view directory holds no copy after the pager | | |
| A-SF-12.10 | Item 18 | No question. Nothing is uploaded | | |
| A-SF-12.11 | Item 19 | The question asks to upload the copy, with Upload and `Keep the local copy`. The server file holds your line | | |
| A-SF-12.12 | Items 20 and 21 | The question also offers `Save as "remote (1).txt"`. The server holds `remote (1).txt` | | |
| A-SF-12.13 | Items 22 and 23 | The two checksums are equal. `readlink` prints `remote.txt` | | |
| A-SF-12.14 | Items 24 and 25 | The server holds `up-1.txt`, `up-dir/inner.txt` and `picture.jpg`. No name contains `mc-partial` | | |
| A-SF-12.15 | Items 26 and 27 | The dialog says `This move is best-effort: a server cannot make it durable.` `move-me.txt` is on the server, not in `sftp-local` | | |
| A-SF-12.16 | Items 28 and 29 | The dialog says that the move is best-effort. The report says `remote sources kept: the server cannot identify them`. `fetch-move.txt` is in `sftp-local` and still on the server | | |
| A-SF-12.17 | Item 31: `F6` within one server | `blob.bin` moves from `down` to `up` on the server | | |
| A-SF-12.18 | Items 32 and 33: `Shift+F6` | `up-1.txt` becomes `renamed.txt`. The second rename raises the "file exists" question, and Skip keeps both files | | |
| A-SF-12.19 | Item 34: `F7` | `newdir` appears. The second `F7` reports that `newdir` exists and puts the cursor on it | | |
| A-SF-12.20 | Item 35: `Shift+F8` | The typed confirmation asks for `delete`. `deltree` goes. `down` stays complete | | |
| A-SF-12.21 | Item 36: `F8` | The screen shows `no trash on the server; Shift+F8 deletes permanently` | | |
| A-SF-12.22 | Item 37 | The card says `remote file: Alt+Q previews it`. `Alt+Q` shows the picture | | |
| A-SF-12.23 | Items 38 and 39 | The footer says `connection lost -- Ctrl+R reconnects`. `F7` is refused | | |
| A-SF-12.24 | Item 40 | The panel reconnects and lists `up` again | | |
| A-SF-12.25 | Item 41, optional | The panel says `connection lost` after the keepalive time. `Ctrl+R` reconnects | | |
| A-SF-12.26 | Items 42 to 46 | Item 45 prints `4`. Item 46: the count stays `4`, and the log has one `least recently used` line | | |
| A-SF-12.27 | Items 47 and 48 | A bookmark row shows the `sftp://` address. `hotlist.toml` holds a `url = "sftp://` line | | |
| A-SF-12.28 | Items 49 and 50 | The start does not connect. The former server tabs show their local directory. The count of item 50 is `0` | | |
| A-SF-12.29 | Items 51 and 52 | `Enter` on the bookmark connects through the hand-off. `Delete` removes the bookmark | | |

#### Step 3.5: Check the Alt chords inside Omarchy's tmux

The phase 3 [keymap audit](../plans/2026-09-28-manycommander-phase3.md#keymap-audit-t9)
found that Omarchy's tmux configuration keeps `Alt+Left`, `Alt+Right`, `Alt+Up`, `Alt+Enter`
and `Alt+1` to `Alt+9`. The [keys page](../../site/content/docs/keys.md#terminals) names
`Backspace` and `Ctrl+1` to `Ctrl+9` as the replacements inside tmux.

> **Warning:** Inside Omarchy's tmux, `Alt+Enter` splits the pane, and `Alt+Esc` closes the
> pane. Do not press `Alt+Esc`. Inside tmux, `Ctrl+M` can arrive as `Enter`. Keep the
> command line empty and the cursor on a directory when you press `Ctrl+M`.

1. In Ghostty, run `tmux -L mcv -f /usr/share/omarchy/config/tmux/tmux.conf new-session -s tmuxkeys`.
2. Run `source ~/mc-verify/env.sh`. Run `mcv tmux-keys "$PG/keys" "$PG/keys/dir-b"`.
3. In a second terminal, run `tail -f "$PG/logs/tmux-keys.log" | grep --line-buffered -E ' key |terminal probe'`.
4. Do the rows of the table in order.
5. Press `F10`. Run `exit` to end the tmux session.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| OV-P3-TM1 | The `terminal probe` line | `tmux=true`. Note the `keyboard` value | | |
| OV-P3-TM2 | Press `Alt+Left`, then `Alt+Right` | No `key` line. tmux takes the chords | | |
| OV-P3-TM3 | Press `Alt+Up` | No `key` line | | |
| OV-P3-TM4 | Press `Alt+1` | No `key` line | | |
| OV-P3-TM5 | Press `Alt+Enter`. Run `exit` in the new pane | No `key` line. tmux splits the pane | | |
| OV-P3-TM6 | Press `Enter` on `dir-a`. Press `Backspace` | `action=Parent`. The panel shows `keys` | | |
| OV-P3-TM7 | Press `Ctrl+T` two times. Press `Ctrl+1`, then `Ctrl+2` | `action=GotoTab(1)`, then `GotoTab(2)`. Note the `key` lines. A filter line with a digit (`action=FilterChar('1')`) is a fail. Press `Esc` to close such a filter line. Inside Omarchy's tmux, `Ctrl+2` can arrive as the tmux prefix (`Ctrl+Space`): the status bar shows `PREFIX`, and tmux takes the next key | | |
| OV-P3-TM8 | Press `Alt+PgUp`, then `Alt+PgDn` | `action=PrevTab`, then `NextTab` | | |
| OV-P3-TM9 | Move the cursor to `dir-a`. Press `Ctrl+M` | `action=MultiRename`. Note the `key` line. `action=Enter` is a fail | | |
| OV-P3-TM10 | Press `Ctrl+F3` | `action=Sort(Name)`. Note the `key` line | | |
| OV-P3-TM11 | Press `Ctrl+E`. Type `abc`. Press `Esc` | `action=FocusLine`. The terminal cursor shows on the command line, and `abc` goes to the command line. `Esc` empties the line | | |

A test on 2026-09-29 found that tmux does not answer the probe's keyboard-protocol query
(`keyboard=false` in a tmux server without a client). In the same test, the tmux key `C-m`
reached manycommander as `code=Enter`. Without the protocol, `Ctrl+1` to `Ctrl+9`, `Ctrl+M`
and `Ctrl+F3` can fail. Record what you see. [OD-4](#od-4-tmux-findings) covers a fail.

#### Step 3.6: Time a first preview of a camera JPEG (P-23, optional)

P-23 passed at the edge for kitty graphics: 148.7 ms for a 150 ms target. This step gives a
number from your own photos for [OD-2](#od-2-p-23-jpeg-decoder).

1. Optional: copy a 12 MP photo from your camera to `$PG/images/camera.jpg`.
2. In Ghostty without tmux, run `mcv p23 "$PG/images" "$PG/keys"`.
3. Press `Ctrl+Q`. Move the cursor to `camera.jpg`, or to `photo-12mp.jpg`. Let the cursor
   rest for 1 second.
4. Press `F10`.
5. Run `grep -E 'preview (request|stages|transmit)' "$PG/logs/p23.log"`.
6. Subtract the timestamp of the last `preview request` line from the timestamp of the next
   `preview transmit` line.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| OV-P3-P23 | Items 5 and 6 | The difference is 150 ms or less. Write `decode_ms`, `prepare_ms`, `total_ms` and the difference in Notes | | |

### Part 4: Type to filter

Source: the amendments of 2026-09-30 to the
[M1 and M2 design section 8](../specs/implemented/2026-09-27-manycommander-design.md#8-keymap)
and the [phase 2 design sections 4](../specs/implemented/2026-09-28-manycommander-phase2-design.md#4-quick-filter)
and [10](../specs/implemented/2026-09-28-manycommander-phase2-design.md#10-keymap), the
[changelog](../../CHANGELOG.md) and the [keys page](../../site/content/docs/keys.md). The
change has no plan. The `typefilter` directory of the playground has 9 entries.

#### Step 4.1: Filter a panel by typing

> **Warning:** Rows OV-TF-F9 and OV-TF-F10 press `Enter` on the filter line. Before you
> press `Enter`, check that the panel shows no entry or only `Docs`.

1. In Ghostty, run `mcv tf-filter "$PG/typefilter" "$PG/keys"`.
2. In a second terminal, run `tail -f "$PG/logs/tf-filter.log" | grep --line-buffered ' key '`.
3. Do the rows of the table in order.
4. Press `F10`.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| OV-TF-F1 | Type `rea` | `action=FilterChar('r')`, `FilterChar('e')`, `FilterChar('a')`. The status row shows `Filter: rea` and `Ctrl+E: command line`. The panel shows only `..`, `bread.txt`, `reader.rs` and `README.md`. The footer says `3 of 9 entries (filter: rea)`. The cursor is on a name that starts with `rea`. The command line stays empty | | |
| OV-TF-F2 | Type `DME` | The panel shows only `..` and `README.md`. The footer says `1 of 9 entries (filter: reaDME)`. The filter ignores case | | |
| OV-TF-F3 | Press `Backspace` six times. Then press `Backspace` again | After six presses, the filter line is empty and stays open. The panel shows every entry. The seventh press closes the filter line. The panel stays in `typefilter`, although each `key` line shows `action=Parent` | | |
| OV-TF-F4 | Type `confg` | The panel shows only `..` and `config.toml`. The footer says `1 of 9 entries (fuzzy filter: confg)`. The status row shows `fuzzy match` | | |
| OV-TF-F5 | Press `Backspace`. Type `ig` | The footer says `1 of 9 entries (filter: config)`. The status row does not show `fuzzy match` | | |
| OV-TF-F6 | Press `Esc`. Type `reamde` | `Esc` closes the filter line and removes the filter. Then the panel shows only `..`, `reader.rs` and `README.md`. The footer says `2 of 9 entries (fuzzy filter: reamde)`. From six letters, one wrong, missing, extra or swapped letter counts | | |
| OV-TF-F7 | Press `Esc`. Type `æb` | The panel shows only `..` and `Æbler.txt`. The footer says `(filter: æb)`, without `fuzzy`. Record `n/a` when the keyboard has no `æ` | | |
| OV-TF-F8 | Press `Esc`. Type `s[o` | The panel shows only `..` and `notes[old].txt`. A `[` without a later `]` is a letter | | |
| OV-TF-F9 | Press `Esc`. Type `qqqq`. Press `Enter` | The footer says `0 of 9 entries (filter: qqqq)`. `Enter` closes the filter line and keeps the filter. Nothing opens. The panel stays in `typefilter` | | |
| OV-TF-F10 | Press `Ctrl+F`, then `Esc`. Type `doc`. Press `Enter`. Then press `Backspace` | `Ctrl+F` and `Esc` remove the filter. `doc` shows only `..` and `Docs`, with the cursor on `Docs`. `Enter` opens `Docs`. `Backspace` returns to `typefilter` without a filter | | |
| OV-TF-F11 | Move the cursor to `bread.txt`. Press `Space` | `action=MarkSpace`. `bread.txt` is marked. No filter line opens | | |

#### Step 4.2: Give the command line the focus in Ghostty and foot

1. In Ghostty, run `mcv tf-line-ghostty "$PG/typefilter" "$PG/keys"`.
2. In a second terminal, run `tail -f "$PG/logs/tf-line-ghostty.log" | grep --line-buffered ' key '`.
3. In a third terminal, run `printf 'echo pasted' | wl-copy`. Row OV-TF-L14 pastes the text.
4. Do the rows of the table in order.
5. Press `F10`.
6. Do items 1 to 5 again in foot, with the log name `tf-line-foot`.
7. Record the results as `Ghostty / foot`.

| ID | Check | Expected | Result (Ghostty / foot) | Notes |
|---|---|---|---|---|
| OV-TF-L1 | Look at the screen after the start | The terminal cursor does not show. The function-key bar names each key with its `F`: `F1Help`, `F3View`, `F10Quit` | | |
| OV-TF-L2 | Press `Ctrl+E` | `action=FocusLine`. The terminal cursor shows on the command line | | |
| OV-TF-L3 | Type `ls` | The command line shows `ls`. No filter line opens | | |
| OV-TF-L4 | Press `Backspace` two times. Type `x` | The line is empty after two presses. The line keeps the focus: `x` goes to the command line | | |
| OV-TF-L5 | Press `Backspace` two times | The first press removes `x`. The second press gives the focus back to the panel, and the terminal cursor goes from the command line. The panel stays in `typefilter` | | |
| OV-TF-L6 | Press `Ctrl+E`, then `Ctrl+F`. Press `Esc` | `Ctrl+F` leaves the empty command line and opens the filter line. `Esc` closes the filter line | | |
| OV-TF-L7 | Press `Ctrl+E`, then `Ctrl+D`. Press `Esc` | The "Go to directory" dialog opens. `Esc` closes the dialog | | |
| OV-TF-L8 | Move the cursor to `Docs`. Press `Ctrl+E`, then `Ctrl+M`. Press `Esc` | The multi-rename tool opens for `Docs`. `Esc` closes the tool. Nothing runs | | |
| OV-TF-L9 | Move the cursor to `tree.zip`. Press `Ctrl+E`, then `Alt+O`. Press `Backspace` | `Alt+O` leaves the empty command line. The panel shows `tree.zip` as an archive with `tree`. `Backspace` leaves the archive | | |
| OV-TF-L10 | Type `d`. Press `Ctrl+E` | The filter line closes. The footer keeps `(filter: d)`. The terminal cursor shows on the command line | | |
| OV-TF-L11 | Type `true`. Press `Enter`. Press `Enter` again | The screen shows `[exit 0] press Enter to return`. After the second `Enter`, the panels show, and the terminal cursor does not show. The footer keeps `(filter: d)` | | |
| OV-TF-L12 | Press `Ctrl+E`, then `Enter` | `Enter` on the empty command line runs nothing. The terminal cursor goes from the command line | | |
| OV-TF-L13 | Move the cursor to `Docs`. Press `Alt+Enter`. Press `Esc` | The command line shows `'Docs'`, with the terminal cursor. `Esc` empties the line. The terminal cursor goes from the command line | | |
| OV-TF-L14 | Press the paste chord of the terminal, for example `Ctrl+Shift+V`. Press `Esc` | The command line shows `echo pasted`, with the terminal cursor. `Esc` empties the line | | |
| OV-TF-L15 | Press `F1`. Read the top border of the help. Press `Esc` | The top border shows `manycommander`, the version that `manycommander --version` shows, and `Help` | | |

### Part 5: F3 in applications and Markdown

Source: the amendments of 2026-10-05 to the
[M1 and M2 design section 6](../specs/implemented/2026-09-27-manycommander-design.md#6-command-line-and-hand-off)
and the [phase 3 design section 4.6](../specs/implemented/2026-09-28-manycommander-phase3-design.md#46-the-info-card),
the [changelog](../../CHANGELOG.md) and the page
[file operations](../../site/content/docs/file-operations.md). The change has no plan.

#### Step 5.1: Open files with F3 and read Markdown in the quick view

> **Warning:** Rows OV-F3-1, OV-F3-2 and OV-F3-6 open applications on your desktop: the image
> viewer and the browser. Close each application after its row.

1. Run the commands below. The commands make the `f3` directory of the playground:

   ```bash
   mkdir -p "$PG/f3"
   cp "$PG/images/upright.jpg" "$PG/f3/"
   printf '<!DOCTYPE html>\n<html><head><title>manycommander</title></head><body><h1>manycommander</h1></body></html>\n' > "$PG/f3/page.html"
   printf '# Title\n\n- one\n- two\n\nSome `code` here.\n' > "$PG/f3/notes.md"
   printf 'plain text\n' > "$PG/f3/plain.txt"
   ```

   `xdg-open` reads the content of a file to find its type. A file without `<html>` is
   plain text to `xdg-open`, and `xdg-open` does not open plain text in the browser.

2. Run the command below. The command records the copies that earlier runs kept:

   ```bash
   ls -A "$XDG_RUNTIME_DIR/manycommander/view" > "$PG/view-before" 2>/dev/null
   ```

3. In Ghostty, run `mcv f3 "$PG/f3" "$PG/keys"`.
4. Do rows OV-F3-1 to OV-F3-5 in order.
5. Run `mcv f3-ar "$PG/archives" "$PG/keys"`.
6. Do rows OV-F3-6 and OV-F3-7.
7. Run `mcv f3-term "$PG/f3" "$PG/keys"`.
8. Do row OV-F3-8.

| ID | Check | Expected | Result | Notes |
|---|---|---|---|---|
| OV-F3-1 | Move the cursor to `upright.jpg`. Press `F3` | The picture opens in the desktop's image viewer. The panels stay on the screen | | |
| OV-F3-2 | Close the image viewer. Move the cursor to `page.html`. Press `F3` | The page opens in the default browser | | |
| OV-F3-3 | Close the browser tab. Move the cursor to `notes.md`. Press `F3`. Quit the pager | The pager shows the Markdown text as it is in the file | | |
| OV-F3-4 | Press `Ctrl+Q`. Keep the cursor on `notes.md` | The quick view shows `Title` in the accent colour, bold and underlined, then two lines with a bullet, then `Some code here.` with `code` in the metadata colour | | |
| OV-F3-5 | Press `Ctrl+Q`. Press `F10` | The quick view closes. manycommander ends | | |
| OV-F3-6 | Press `Enter` on `tree.zip`, then on `tree`, then on `docs`. Move the cursor to `picture.jpg`. Press `F3` | The status row shows the copy. Then the picture opens in the image viewer | | |
| OV-F3-7 | Close the image viewer. Press `F10`. Run `ls -A "$XDG_RUNTIME_DIR/manycommander/view" 2>/dev/null \| diff "$PG/view-before" -` | The output is empty: manycommander removed the copy when it ended | | |
| OV-F3-8 | Move the cursor to `plain.txt`. Press `Enter`. Close the new window. Press `F10`. Run `pgrep -af 'f3/plain.txt'` | `Enter` opens `plain.txt` in the default application for text. A terminal editor opens in a new terminal window. `pgrep` shows no process | | |

## Decisions for the owner

### OD-1: A-P-7 small-file copy and move

A-P-7 of the M1 design has four parts. Two parts pass in every run. A 4 GiB file copies to
ext4 at 0.45x to 0.65x of `cp`. A 4 GiB btrfs reflink copy takes 0.003 s to 0.004 s. Two
parts miss:

| Run | 50k files of 4 KiB, copy, against `cp -r` (target at most 1.5x) | 50k files of 4 KiB, move to ext4, against `mv` (target at most 2x) |
|---|---|---|
| M1 final run ([M1 plan, benchmarks](../plans/implemented/2026-09-27-manycommander-m1-m2.md#benchmarks-t12-t15)) | 1.71x | 2.44x (28.8 s against 11.8 s) |
| Phase 2, T9 ([phase 2 plan](../plans/implemented/2026-09-28-manycommander-phase2.md#benchmarks-t9)) | 1.60x to 1.72x | 2.61x to 2.83x |
| Phase 3, T10 ([history](../perf/history.md)) | 1.70x (2.19 s against 1.29 s) | 2.50x (28.54 s against 11.40 s) |

The `mv` baseline changed from 35.0 s to 11.8 s between runs of the M1 session.

The M1 session measured two options and did not adopt them:

| Option | Part | Measured | Trade-off |
|---|---|---|---|
| A: commit a copied file with `O_TMPFILE` and `linkat`. Keep the named temporary file where `O_TMPFILE` is not available: vfat, exfat, most FUSE filesystems | copy | 1.36x `cp -r`. Direct writes, which break I-2, measured 1.16x | I-2 and I-3 stay: `linkat` fails atomically with `EEXIST`, and no partial name is ever visible. A crash leaves no `.mc-partial-*` file. Design 4.7 steps 2 and 5, and the documented crash residue, change |
| B: batches of 1024 files instead of 256 | move | 15.5 s against 27.7 s with 256 files, in the same run. No ratio against a current `mv` run (unverified) | I-1 stays. After a crash, up to 1024 files can exist in two places, instead of up to 256 |
| C: accept the misses | copy, move | -- | The P-7 and A-P-7 targets of the M1 design change to the measured values |

Options A and B are independent. Choose A or C for the copy part. Choose B or C for the move
part.

### OD-2: P-23 JPEG decoder

| Measurement ([phase 3 plan](../plans/2026-09-28-manycommander-phase3.md#benchmarks-t10), [history](../perf/history.md)) | Value | Target |
|---|---|---|
| Kitty graphics, first preview of a camera-like 12 MP JPEG, worst of 40 | 148.7 ms. 149.1 ms and 151.1 ms in two more runs | 150 ms |
| Sixel, the same | 182.0 ms | 200 ms |
| Halfblocks, the same | 131.3 ms | 150 ms |
| The JPEG decode alone | 100 ms to 105 ms | -- |
| A cache hit | below 10 ms | 16 ms |

The JPEG decoder of the `image` crate cannot decode at a reduced scale. The decode is most
of the time.

| Option | Trade-off |
|---|---|
| A: keep the decoder | No change. The kitty tail stays at the target |
| B: change to a JPEG decoder that decodes at a reduced scale | The decode time drops (unverified: the plans measured no candidate). A new parser of untrusted files: the V-2 limits must hold, and NFR-SUP applies. A decoder in C repeats the in-process risk that D-1 accepts for zstd |
| C: relax P-23 for camera JPEGs | The P-23 target of the phase 3 design changes |

The plans name no candidate decoder. When you choose B, the next session evaluates
candidates first.

### OD-3: The feel test and the switch

Decide after Step 1.4. Close the M1 plan's feel test item, or keep it open with your notes.
Keep `SUPER + E` on manycommander, or roll back to Double Commander ([Rollback](#rollback)).

### OD-4: tmux findings

Decide only when a check below fails.

| Check | Answers |
|---|---|
| A-QV-8.9: no picture from unicode placeholders in Ghostty inside tmux | Document the limit. Or change the default inside tmux to halfblocks (a code change). `preview.protocol = "halfblocks"` works today as a personal setting |
| OV-P3-TM7, OV-P3-TM9, OV-P3-TM10: a chord that needs the keyboard protocol fails inside tmux | The keys page already names `Alt+PgUp`, `Alt+PgDn` and `Alt+P` for tmux and says that `Ctrl+1` to `Ctrl+9` and `Ctrl+M` need tmux's `extended-keys`. Decide whether to turn on `extended-keys` in your tmux configuration, or ask for a code change |

### OD-5: SFTP tree round trips (done)

The owner asked for this change after the phase 3 release. A small file now takes 3 round
trips in each direction, not 6. 1000 files of 4 KiB through ssh take 0.87x (download) and
0.97x (upload) of `sftp -rp`. At a round trip of 30 ms, 200 files take 0.75x. No decision is
open. Step 3.4 checks transfers on your server.

### Decision record

| ID | Decision | Your choice | Date |
|---|---|---|---|
| OD-1a | A-P-7, the copy part: option A or C | | |
| OD-1b | A-P-7, the move part: option B or C | | |
| OD-2 | P-23: option A, B or C | | |
| OD-3a | The feel test item: close or keep open | | |
| OD-3b | `SUPER + E`: keep manycommander or roll back | | |
| OD-4 | tmux findings: the answer for each failed check | | |
| OD-5 | SFTP tree round trips: done (no decision) | done | |

## Verification

1. Run the command below. The command lists the result rows with an empty Result cell. The
   output is empty when every row has a result:

   ```bash
   grep -nE '^\| (A-|OV-)[^|]+\|[^|]*\|[^|]*\|[[:space:]]*\|' "$PG/results.md"
   ```

2. Run `grep -nE '^\| OD-[^|]+\|[^|]*\|[[:space:]]*\|' "$PG/results.md"`. The output is
   empty when every decision has a choice.
3. Run `grep -nE '^\| (A-|OV-)[^|]+\|[^|]*\|[^|]*\|[^|]*fail' "$PG/results.md"`. The
   command lists the rows with a `fail`. Confirm that each row has a note and a log name.
4. Run `cat ~/.local/state/omarchy/current/theme.name "$PG/theme-before"`. The two lines are
   identical.
5. Press `SUPER + E`. manycommander opens, or Double Commander after a rollback.

## Report back

> **Warning:** The results file and the logs hold paths, host names and user names. Do not
> copy them into the repository. The publication gate refuses them, and the repository is
> public.

1. Fill every Result cell and every decision in `$PG/results.md`.
2. For each `fail`, write in Notes what you saw, the log name (`$PG/logs/NAME.log`) and the
   time.
3. Start a session in the repository root.
4. Give the session the path of `$PG/results.md` and of `$PG/logs/`.
5. Ask the session to record the results in the execution records of the plans, and to
   close the open items. The session writes no host name, user name or path of yours into
   the repository.

| Results | Closes or updates |
|---|---|
| OV-M1-K00 to OV-M1-K40 | M1 plan, open item "T11 chord confirmation" |
| OV-M1-R1 to OV-M1-R3 | M1 plan, T15 and design section 11.5: evidence on the release |
| OV-M1-T16.1 to OV-M1-T16.3, A-LN-1.1 to A-LN-1.4 | M1 plan, open item "T16", and the status row T16 |
| OV-M1-FEEL, OD-3 | M1 plan, open item "Feel test" |
| A-TH-1, A-UI-1, A-UI-2, A-UI-3, A-TR-1, A-TR-3 | M1 plan, "M1 acceptance (T14)": evidence on the release |
| OD-1 | M1 plan, open item "A-P-7". Phase 2 plan, open item "A-P-7". Phase 3 plan, next action |
| OV-P2-K01 to OV-P2-K12 | Phase 2 plan, open item "New chords in the terminals" |
| OV-P2-DJ, OV-P2-QF, OV-P2-FD, OV-P2-MR, OV-P2-CD, OV-P2-LK, OV-P2-AT, OV-P2-SP, OV-P2-HL | Phase 2 plan: evidence on the release for A-DJ, A-QF, A-FD, A-MR, A-CD, A-LK, A-AT, A-SP and A-HL |
| A-FD-7.1 to A-FD-7.5 | Phase 2 plan, open item "A-FD-7": a second run |
| OV-P3-K01 to OV-P3-K06 | Phase 3 plan, Verification: the chords `Ctrl+Q`, `Alt+Q` and `Alt+O` |
| A-QV-8.1 to A-QV-8.13 | Phase 3 plan, "A-QV-8 manual checklist (owner)" and Verification |
| OV-P3-AR1 to OV-P3-AR14 | Phase 3 plan: evidence on the release for A-AR-1 to A-AR-7 and P-20 |
| A-SF-12.1 to A-SF-12.29 | Phase 3 plan, Verification: A-SF-12 |
| OV-P3-TM1 to OV-P3-TM11, OD-4 | Phase 3 plan, keymap audit (T9): the last row |
| OV-P3-P23, OD-2 | Phase 3 plan, benchmarks: the P-23 row |
| OV-TF-F1 to OV-TF-F11, OV-TF-L1 to OV-TF-L15 | Phase 2 plan, execution record: evidence on the release for the amendments of 2026-09-30 to M1 section 8 and P2 sections 4 and 10 |
| OD-5 | The benchmark history: the SFTP trees proposal |
| OV-F3-1 to OV-F3-8 | M1 plan, "M1 acceptance (T14)": evidence for the amendments of 2026-10-05 to M1 section 6 and P3 section 4.6 |

## Rollback

- Restore the key bindings. The session removed the Double Commander package on
  2026-09-30. Install the package again before you go back to Double Commander:

  ```bash
  omarchy pkg add doublecmd-qt6
  cp ~/.config/hypr/bindings.lua.bak.1790710807 ~/.config/hypr/bindings.lua
  hyprctl reload
  hyprctl configerrors
  ```

- Restore the theme: `omarchy-theme-set "$(cat "$PG/theme-before")"`.
- Restore the hook directory. Compare `ls ~/.config/omarchy/hooks/theme-set.d/` with
  `$PG/hooks-before`. Remove `manycommander` when `$PG/hooks-before` does not list it.
- Unmount a remaining FUSE mount: `fusermount3 -u "$PG/find/mnt"`. For a stalled fixture
  mount, first run `pkill -CONT -x rclone`, then `fusermount3 -u` on the `stuck` mount point
  that `findmnt -t fuse.rclone` shows.
- Remove a remaining loop device: `udisksctl unmount -b "$dev" --no-user-interaction`, then
  `udisksctl loop-delete -b "$dev" --no-user-interaction`.
- Stop the tmux servers of the runbook:

  ```bash
  for s in mcv mcv-plain mcv-foot; do tmux -L "$s" kill-server 2>/dev/null; done
  ```

- Remove the test bookmarks: `Delete` in the `Ctrl+D` dialog.
- Remove the trash entries of the playground: `gio trash --list | grep -aF "$PG/"` lists them.
  Delete them in the Nautilus trash.
- Remove the server directory: `ssh HOST 'rm -rf mc-verify'`.
- Remove the playground. `a-rx` can leave directories that you cannot read:

  ```bash
  chmod -R u+rwx "$PG" "$XDEV"
  rm -rf "$PG" "$XDEV"
  ```

- Go back to an older release. With option A, run
  `mise use -g github:manyfold-dk/manycommander@VERSION`. `VERSION` is the older tag without
  the `v`. With option B or C, do the install with the older tag and `--force`.
