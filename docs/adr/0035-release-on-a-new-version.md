# ADR 0035: A new version on main publishes itself

## Status

Accepted -- 2026-10-11. Supersedes the release trigger of [ADR 0028](0028-release-artefacts-and-website.md) and [ADR 0030](0030-release-in-two-jobs.md).

Both started a release with a tag pushed by hand, ADR 0030 after a dry run.

## Context

A release took two decisions that were one. The release commit (the version in `Cargo.toml`
and its section in `CHANGELOG.md`) said what to publish; a tag pushed by hand afterwards said
to publish it. Between the two, the maintainer waited for the `ci` run, ran the release
workflow as a dry run, checked by hand that the tag would land on the commit that `ci` had
tested, and pushed the tag, which ran `scripts/check.sh full` once more. The waiting took
about twenty minutes per release, and only the hand-made check tied the published binary to
the tested commit.

Release tags cannot be moved or deleted ([ADR 0029](0029-repository-protections-and-dependency-updates.md)),
so whatever starts a release must happen once, on purpose.

## Decision

1. The release workflow runs after every `ci` run that passed on a push to `main`. Its job
   `plan` reads the version in `Cargo.toml` at that commit. When the version has a release,
   the run ends there; that is every commit but a release commit.
2. For a version without a release, `build` builds the commit `ci` tested, and `publish`
   creates the GitHub release with `--target` that commit, which creates the tag
   `v<version>` there. The workflow's own token creates the tag; no personal token is
   involved.
3. `plan` refuses, and the run fails, when the version is below the newest release tag, when
   `CHANGELOG.md` has no section for it, or when its tag exists at another commit. A tag at
   the same commit is the trace of a run that failed in `publish`, and a rerun uses it.
4. `build` keeps no right to publish, and `publish` runs nothing from the repository
   (ADR 0030). A run by hand on `main` stays a dry run that builds and publishes nothing.
5. The `aur` workflow builds from the PKGBUILD template of the release's tag, unless a
   packaging fix names another commit with the input `ref`.

## Rationale

- Pushing the release commit is the decision to release; a second step only repeats it.
- The tag is made from the commit `ci` tested, by the run that built it, so the published
  binary, its attestation, its tag and its tested commit agree without a check by hand.
- A tag made with the workflow's token starts no other workflow, so the release cannot be a
  second workflow triggered by the tag: it has to be published in the same run.
- A failure before `publish` leaves no tag. The next push to `main` that passes `ci` starts the
  release again, with the same version.

## Consequences

- A release commit pushed to `main` publishes once its `ci` run passes. A version bumped by
  mistake is used up, as a tag pushed by mistake was before.
- A tag pushed by hand publishes nothing; the [release runbook](../runbooks/release.md) no
  longer tags.
- Every `ci` run on `main` is followed by a short `release` run that ends in `plan`.
- After a release is deleted, every push to `main` fails in `plan` until the version is
  raised, because the tag stays at the old commit.

## References

- [`.github/workflows/release.yml`](../../.github/workflows/release.yml),
  [`.github/workflows/aur.yml`](../../.github/workflows/aur.yml)
- [Release runbook](../runbooks/release.md)
