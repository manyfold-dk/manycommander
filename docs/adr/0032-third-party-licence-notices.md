# ADR 0032: Releases carry the licence texts of the crates they are built from

## Status

Accepted -- 2026-10-11.

## Context

The release binary is statically linked: it contains the code of over 150 Rust crates,
the C library libzstd (through `zstd-sys`, [ADR 0021](0021-archive-decoders.md)) and the
Rust standard library. Their licences (MIT, Apache-2.0, BSD, ISC, Zlib, Unicode and the bzip2
licence; `deny.toml` admits only permissive ones) ask that a binary distribution carry the
licence text and copyright notice. The tarball and the AUR package carried only
manycommander's own `LICENSE`.

## Decision

1. Every release tarball carries `THIRD-PARTY-LICENSES`: for each licence, the crates under it
   and the licence text with the crates' own copyright lines. `scripts/notices.sh` writes it
   with cargo-about from `Cargo.lock`, the target's dependency graph and the template in
   `contrib/notices/`. Build and dev dependencies are left out, because they are not in the
   binary. A closing section covers the Rust standard library.
2. The release workflow's `build` job writes the file
   ([ADR 0030](0030-release-in-two-jobs.md)); the `ci` workflow writes it on every push and
   pull request, so a dependency whose licence text cannot be found fails CI, not a release.
3. The AUR package installs the file to `/usr/share/licenses/manycommander-bin/`.
4. cargo-about is a pinned release binary, checked against its SHA-256
   (`contrib/notices/tools.env`), which Renovate updates like the cargo-deny pin
   ([ADR 0029](0029-repository-protections-and-dependency-updates.md)).

## Rationale

- Generating the file from the lock file at release time keeps it exact for that release; a
  committed copy would go stale with every Renovate update.
- cargo-about reads the licence files the crates ship, so the copyright lines are the
  authors' own, not a generic text for the licence's identifier. It finds libzstd's BSD
  licence in `zstd-sys`.
- Its accepted list repeats the allow list of `deny.toml`: `cargo deny` decides what may enter
  the build, and cargo-about refuses to write notices for anything outside it.

## Consequences

- Releases before this decision lack the file; the AUR package needs a release that has it.
- A new licence admitted in `deny.toml` must also be added to `contrib/notices/about.toml`,
  or CI fails.
- A cargo-about update fails CI until its checksum is updated, as with cargo-deny.

## References

- `scripts/notices.sh`, `contrib/notices/`, `.github/workflows/release.yml`,
  `.github/workflows/ci.yml`, `contrib/aur/manycommander-bin/PKGBUILD.in`
- [ADR 0028: Release artefacts, packages and the website](0028-release-artefacts-and-website.md)
- [Release](../runbooks/release.md)
