+++
title = "Durability"
description = "What survives a crash or power loss during a copy or a move."
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

## Leftovers

After a crash, `.mc-partial-*` files can remain in a destination directory. They are
incomplete copies. manycommander never deletes them automatically; remove them by hand once
you have checked the source.
