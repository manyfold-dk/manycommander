# ADR 0021: Archive decoders -- libzstd in C, pure Rust for the rest

## Status

Accepted -- 2026-09-28.

## Context

The archive formats need a decoder each: deflate (zip, gzip), zstd, xz, bzip2 and 7z's
coders. Every decoder runs in-process on untrusted input. A Rust panic in a decoder is
caught on its worker thread and ends only that scan (NFR-REL); a memory fault in C code is
not a panic and ends the whole process.

The main use case is pacman's `.pkg.tar.zst`. A compressed tar lists only by decompressing
all of it, so the zstd decoder sets the listing time of every package.

## Decision

| Format | Decoder | Notes |
|---|---|---|
| deflate, gzip | `flate2` with the pure-Rust `zlib-rs` backend | Measured faster than the `gzip` tool |
| zstd | `zstd`: C libzstd compiled by `cc` from the crate's bundled sources; legacy formats off; `window_log_max` 27 | Nothing links a system library |
| xz | `lzma-rust2`, pure Rust; its `unsafe` `optimization` feature is on, because `zip` and `sevenz-rust2` enable it on their lines | 128 MiB memory limit |
| bzip2 | `bzip2` with the pure-Rust `libbz2-rs-sys` backend | `deny.toml` allows the bzip2 licence |
| 7z | `sevenz-rust2`, default features off (no AES, no compression, no PPMd) | A second `lzma-rust2` line comes with it |

Beyond the crates' own limits, manycommander caps what the crates would read whole into
memory: tar long names and pax headers at 4 MiB, an xz LZMA2 dictionary and a 7z dictionary
at 128 MiB, a 7z header at 64 MiB with its file, folder and stream counts held to the entry
bound, and a zip's declared entry count before the crate reads the central directory.

## Rationale

- **zstd in C.** Listing the largest cached package took 510 ms with libzstd and 1500 ms
  with the pure-Rust `ruzstd`. libzstd is one narrow, heavily fuzzed decoder; the in-process
  C risk is accepted for it alone.
- **xz in Rust.** The C `liblzma` is 1.6x faster, but its supply-chain history
  (CVE-2024-3094) weighs against it, and `zip` and `sevenz-rust2` already use `lzma-rust2`.
- **bzip2.** The `bzip2` crate is `MIT OR Apache-2.0`; its default backend carries the SPDX
  bzip2 licence, which is permissive and BSD-style, and the C backend carries it too. The
  only alternative was to drop the format.
- **7z.** `sevenz-rust2` is pure Rust and fits the index model. The original `sevenz-rust`
  has RUSTSEC-2026-0245 (path traversal, no fix) and RUSTSEC-2026-0246 (unmaintained). The
  releases of `sevenz-rust2` that share `zip`'s `lzma-rust2` line keep coder properties
  private and predate a block-header overflow fix, so the build carries the latest release
  and two more crates.
- **The caps.** Without the 7z header walk, a 2.5 KB header made the crate build 16 Mi
  entries (about 1.8 GB). The tar guard took a crafted archive's peak from about 123 MiB of
  extra memory to about 5 MiB.

## Consequences

- A memory fault or an abort in libzstd ends manycommander. A-AR-3 feeds it truncated
  input; the risk is stated in NFR-REL.
- `lzma-rust2`'s `unsafe` fast path is in the build. The `unsafe` rule of NFR-SEC covers
  manycommander's own code only.
- The archive readers added about 30 crates. `cargo deny` warns about the duplicate
  `lzma-rust2` line and stays clean with the licence row.
- The SPDX identifier of the bzip2 licence contains a dotted release number. It appears
  only in `deny.toml`; documents and the changelog name the licence in words.
- A first-rows target (P-19) holds for every format, but xz and bzip2 full scans take
  seconds on large packages, with progress and `Esc`.

## Alternatives considered

| Alternative | Why not |
|---|---|
| `ruzstd` | 3x slower on the main use case |
| C `liblzma` | Supply-chain history; one more C decoder in-process |
| Dropping `.tar.bz2` | Still common for source releases; the licence is permissive |
| `sevenz-rust` | Unfixed path traversal; unmaintained |
| libarchive for everything | Many C parsers in-process ([ADR 0020](0020-archives-indexed-and-extracted-by-the-local-engine.md)) |

## References

- [TDR-3 1.2 (D-1, D-2, D-6) and 2.7](../tdr/0003-archives-quick-view-and-sftp.md#27-dependencies)
- [TDR-3 appendix A, E-5, E-6, E-28](../tdr/0003-archives-quick-view-and-sftp.md#appendix-a-implementation-decisions)
