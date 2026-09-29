+++
title = "Durability"
description = "What survives a crash or power loss during a copy, a move, an extraction or a transfer to or from a server."
weight = 70
+++

## Copy

A copy does not call `fsync`, like `cp`: the kernel writes the data back in its own time.
A copy only reads its sources, so a crash during a copy never touches them. Copy still never
shows a partial file under its real name while it runs; see
[file operations](@/docs/file-operations.md).

## Move

A move keeps every file in at least one complete, committed place at every moment. That
also holds after a crash or power loss, on filesystems that honour `syncfs`: btrfs, ext4
and xfs.

A cross-filesystem move works in batches of 256 files or 256 MiB. After each batch,
manycommander flushes the destination filesystem with `syncfs` and only then deletes that
batch's sources. After a crash, the last unflushed batch can be in both places, never in
neither. Run `F6` again to merge the rest.

A file with several hard links in the selection keeps all its source names until every one
of them has been copied, and the flush after that deletes them. A crash in between leaves
the file in both places.

## Extraction and downloads

Extracting from an archive and downloading from a server are copies: they do not call
`fsync` either.

## Moves across hosts

A move between the local disk and an [SFTP server](@/docs/sftp.md) is best-effort, on every
server, and its confirm dialog says so before it starts. SFTP can neither make a new
directory entry durable on the server nor tell whether a file is still the one that was
read, so the guarantee above does not hold.

- **Upload move** (`F6` into a server panel). Each local source is deleted only after its
  upload is complete under its final name, and only when the source did not change
  meanwhile, in batches like a local move. When the server offers `fsync@openssh.com`, the
  uploaded data is synced there first; the new name itself is not. Without it, the report
  says "not synced on the server": a crash of the server can then lose uploads whose local
  sources are gone. A local file with several hard links in the selection keeps every name
  after the first one, and the report says "source changed; kept both" for them.
- **Download move** (`F6` out of a server panel). The downloaded files are flushed here with
  `syncfs`, as in a local move, and the remote sources are kept: their size and a one-second
  modification time cannot tell the file that was read from one written in the same second.
  The confirm dialog and the report say "remote sources kept: the server cannot identify
  them". Delete them with `Shift+F8` once you are sure.

## Leftovers

After a crash, `.mc-partial-*` files can remain in a destination directory, on a local disk
or on a server. They are incomplete copies. manycommander never deletes them automatically;
remove them by hand once you have checked the source.
