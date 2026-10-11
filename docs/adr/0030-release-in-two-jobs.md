# ADR 0030: The release builds without the right to publish

## Status

Accepted -- 2026-10-11. Trigger superseded by [ADR 0035](0035-release-on-a-new-version.md): a new version on main, not a pushed tag.

## Context

The release workflow ran as one job that held `contents: write`, `id-token: write` and
`attestations: write` while it ran `cargo build` over the whole dependency graph, including
build scripts and the C compile of libzstd. Code that runs in that job could create or change
a release, request an OIDC token, or sign a build provenance attestation for something else.
A compromised build script would therefore not only change the binary, which the provenance
of a correct build would not reveal, but could also publish under the project's name.

Release tags cannot be moved or deleted ([ADR 0029](0029-repository-protections-and-dependency-updates.md)),
so a release workflow that fails after its tag exists uses up that version.

## Decision

The workflow has two jobs:

1. **`build`**, with the repository's read-only token: it checks that the commit is on `main`
   and, for a tag, that the tag matches `Cargo.toml`; builds with `--locked`; packages the
   tarball and its SHA-256; takes the release notes from `CHANGELOG.md`; and uploads the three
   files as an artifact.
2. **`publish`**, with the write, OIDC and attestation permissions: it downloads the artifact
   (the download fails on a digest mismatch), checks the SHA-256 again, attests the tarball,
   and creates the release. It checks out nothing and runs no code from the repository.

A run started by hand on `main` runs `build` only: a dry run of everything up to publishing,
which the [release runbook](../runbooks/release.md) requires before a tag is pushed.

## Rationale

- The permission that matters is held only by a job whose every step is either a pinned
  action or a short shell command written in the workflow itself.
- The dry run moves every failure the build can have (a stale `Cargo.lock`, a missing
  changelog section, a compile error on the runner) ahead of the tag, where it costs nothing.

## Consequences

- A compromised build can still produce a wrong binary, which the attestation then vouches
  for as built by this workflow; the split removes the publishing and signing reach, not that.
- The artifact lives for seven days; a release is published within minutes of its build.
- The `aur` job now follows `publish`. A dry run skips both.

## References

- [`.github/workflows/release.yml`](../../.github/workflows/release.yml)
- [ADR 0028: Release artefacts, packages and the website](0028-release-artefacts-and-website.md)
