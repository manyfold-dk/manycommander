# ADR 0020: Archives are indexed by manycommander and extracted only through the local engine

## Status

Accepted -- 2026-09-28.

## Context

Users browse archives they did not make: downloads, pacman packages, attachments. The Rust
archive crates offer convenient `extract` and `unpack` functions, and so does libarchive.
Those functions have a history of the same defect, a member name joined onto a filesystem
path:

| Advisory | Crate | Defect |
|---|---|---|
| RUSTSEC-2025-0168 | `zip` | Incorrect path canonicalization during extraction leads to an arbitrary file write |
| RUSTSEC-2026-0067 | `tar` | `unpack_in` can `chmod` arbitrary directories by following symlinks |
| RUSTSEC-2026-0068 | `tar` | PAX size headers are ignored when the header size is non-zero, so two parsers disagree on where a member ends |
| RUSTSEC-2026-0245 | `sevenz-rust` | Path traversal in `decompress_impl` enables an arbitrary file write |

libarchive had parser CVEs in the same period (CVE-2025-5914, CVE-2025-5915), and it runs
many parsers in-process.

## Decision

manycommander builds its own index of every archive and never calls a crate's `extract` or
`unpack` function. Five invariants hold
([TDR-3 1.3](../tdr/0003-archives-quick-view-and-sftp.md#13-invariants)):

- **A-1, member paths are data.** The index splits a name on `/` and validates every
  component; a `..`, a NUL, or a parent that is not a directory member makes the member
  "unsafe path", listed in the footer and never extracted.
- **A-2, extraction writes only through the local engine.** Extraction is a copy job with an
  archive origin ([ADR 0019](0019-non-local-places-through-a-narrow-provider.md)): directory
  fds, a per-component `O_NOFOLLOW` walk from the destination root, and the temporary-file
  commit. A symlink member becomes a symlink and is never traversed.
- **A-3, no special files and no privilege bits.** Devices, FIFOs and sockets are skipped;
  modes are masked to `0o777`; a hard link is resolved only inside the index and made from
  an `O_PATH` fd of the inode this job extracted.
- **A-4, bounded amplification.** Extraction counts each member's output and stops at its
  declared size; zstd and xz decoders run with a 128 MiB window cap.
- **A-5, parser consistency.** Extraction re-checks the header at each selected member's
  locator against the index and fails the member with "archive changed" on a difference.

`bsdtar` is the differential oracle: for every generated fixture, the listing must match
`bsdtar`'s in names, types, sizes, link targets, modes and times (A-AR-1). It is also the
later path for formats the Rust readers do not cover (rar, iso, cpio), as a subprocess.

## Rationale

- The advisories share one cause. Owning the join between a member name and a path removes
  the class instead of tracking each instance.
- The local engine already upholds no partial file (I-2), no overwrite without a decision
  (I-3) and symlinks as objects (I-5), and it is tested by failpoint sweeps. Extraction
  inherits all of it.
- The index also serves browsing, F3 and the quick view, so the archive is parsed once per
  scan.

## Consequences

- The index code and the per-crate memory caps are manycommander's to maintain: a 4 MiB
  guard on tar long names and pax headers, an xz dictionary limit, a zip entry-count check
  before the crate reads the central directory, a 7z header walk
  ([TDR-3 E-6, E-28](../tdr/0003-archives-quick-view-and-sftp.md#appendix-a-implementation-decisions)).
- 21 committed hostile fixtures, with the script that made them, exercise the invariants
  (A-AR-2). A review of the guard found one case where it and the `tar` crate disagreed on a
  pax size; the guard now takes sizes from the crate's own parse.
- Extraction stays fast: the 10k-entry package extracts at 1.46x `bsdtar -xf` as
  `.tar.zst` with the scan included, and at 0.80x as zip (P-22).
- Nested archives, encrypted zip members and GNU sparse members are listed but not
  extracted.

## Alternatives considered

| Alternative | Why not |
|---|---|
| The crates' `extract` / `unpack` | The source of the advisories above |
| libarchive in-process (`compress-tools`) | A memory bug in any of its parsers ends the process; recent CVEs; returns names only; links a system library |
| `bsdtar` as a subprocess for every format | A process and a format conversion per operation for formats the Rust readers cover |

## References

- [TDR-3 3](../tdr/0003-archives-quick-view-and-sftp.md#3-archives)
- [ADR 0021](0021-archive-decoders.md): the decoders
