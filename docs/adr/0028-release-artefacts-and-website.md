# ADR 0028: Release artefacts, packages and the website

## Status

Accepted -- 2026-09-28; amended 2026-09-30.

The amendment: the website states each guarantee only for the filesystems and servers that
provide it.

## Context

manycommander is one Linux x86-64 binary. Its users run Omarchy, which ships `mise`, or
another Arch-based system, where the AUR is the usual source of software that is not in the
official repositories. A user must be able to check that a download is the one the project
built. User documentation needs a home that is easier to read than a repository, and the
project's own claims (no half-written file, safe moves) must be stated where users read
them, precisely.

## Decision

**Releases.** A tag `v<version>` that matches the version in `Cargo.toml` starts the release
workflow (`.github/workflows/release.yml`). On GitHub's runner it builds with
`cargo build --release --locked`, strips the binary, packages
`manycommander-<version>-x86_64-linux.tar.gz` with `LICENSE`, `README.md` and
`CHANGELOG.md`, writes its SHA-256 file, attaches a build provenance attestation, and
publishes a GitHub release whose notes are the version's section of `CHANGELOG.md`. A tag
that does not match `Cargo.toml`, or a changelog without the section, fails the release.
The crate is not published to crates.io (`publish = false`).

**Installation.** The documented ways, in order:

1. `mise use -g github:manyfold-dk/manycommander`, which downloads the latest release and
   checks it; `omarchy update` keeps it current.
2. A release tarball by hand, checked with `sha256sum -c`.
3. `cargo install --git`, labelled the development version.

**AUR.** After each release, the aur workflow renders the package `manycommander-bin` from
`contrib/aur/` with `scripts/aur.sh`, builds it and checks it with `namcap` in an Arch Linux
container, installs it and runs it. The push to the AUR runs only when the repository
variable `AUR_PUBLISH` is `true` and the secret `AUR_SSH_PRIVATE_KEY` is set; the
[AUR runbook](../runbooks/aur-package-setup.md) describes the one-time setup.

**Website.** The site at manycommander.app is a Zola project in `site/`, served by a
Cloudflare Worker that redirects `www` to the apex and applies the headers of
`site/static/_headers` to every response: a Content Security Policy of `default-src 'none'`
with scripts, styles, images and fonts from the site itself only, HSTS, `nosniff`, a strict
referrer policy, a restrictive permissions policy and `Cross-Origin-Opener-Policy`. Fonts are
self-hosted; the site loads nothing from a third party. The site workflow builds the site,
checks its links and dry-runs the Worker on every change, and deploys from `main` when the
repository variable `SITE_DEPLOY` is `true`. The tool versions are pinned in
`site/tools.env`, and the Zola download is checked against its SHA-256.

**Claims.** User documentation lives in `site/content/docs/`; contributor documentation in
`docs/`. The website scopes every guarantee to where it holds: the copy guarantee to ext4,
btrfs and xfs, the local direct-write exception beside it, and what a server without hard
links gets beside the SFTP feature.

## Rationale

- A single static binary needs no build on the user's machine; mise and the checksum make
  the download verifiable, and the attestation ties it to the workflow that built it.
- The AUR package is built and tested in CI on every release, so enabling the push changes
  nothing about the package itself.
- A static site behind a Worker has a small attack surface, and a strict CSP is possible
  because the templates carry no inline scripts or styles.
- A review of the website found its headline promise ("never leaves a half-written file")
  stronger than the SFTP and direct-write documentation; stating each guarantee with its
  scope keeps the claims true.

## Consequences

- Only Linux x86-64 has a binary; other targets build from source.
- The release workflow trusts the tag: whoever tags must tag a commit on `main` whose CI
  passed. The [release runbook](../runbooks/release.md) lists the checks.
- The site's deploy token and the AUR key are repository secrets; the
  [site runbook](../runbooks/site-deploy-setup.md) scopes the token to Workers scripts and
  routes of the one zone.
- A change to a guarantee in a TDR also needs the website's wording checked.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Building from source as the main install route | Needs a Rust toolchain and minutes of build time, while a checked binary exists; it stays documented as the development version |
| Loading fonts or scripts from a CDN | Weakens the CSP and adds a third party to every page view |

## References

- `.github/workflows/release.yml`, `.github/workflows/aur.yml`, `.github/workflows/site.yml`
- [Release](../runbooks/release.md), [AUR package setup](../runbooks/aur-package-setup.md), [site deploy setup](../runbooks/site-deploy-setup.md)
- [TDR-3 11](../tdr/0003-archives-quick-view-and-sftp.md#11-release)
