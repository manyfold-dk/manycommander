# ADR 0029: Repository rulesets, deploy environments and Renovate

## Status

Accepted -- 2026-10-11.

## Context

manycommander is public and has one maintainer, who pushes to `main` directly; coding agents
work the same way. Before this decision the repository on GitHub had:

- no protection on `main`: a force-push or a deletion would have succeeded;
- no protection on release tags: a `v*` tag could be moved after users installed from it;
- the website's Cloudflare token and account ID as repository secrets, readable by a
  workflow on any branch;
- no automated dependency updates: crates, SHA-pinned actions and the zola, wrangler and
  cargo-deny pins aged until someone looked.

Secret scanning and push protection were on, and every action was already pinned to a full
commit SHA.

## Decision

1. **Ruleset "main"** refuses a deletion of `main` and any push that is not a fast-forward,
   for everyone, with no bypass. Direct pushes stay allowed; required status checks are not
   configured, because they would force every change through a pull request.
2. **Ruleset "release tags"** refuses deleting or moving any `v*` tag, for everyone. A failed
   or broken release uses up its version; the next patch version fixes it
   ([release runbook](../runbooks/release.md#rollback)).
3. **Deploy environments.** The website deploy job runs in the environment `site`, which only
   `main` may use; the AUR job runs in the environment `aur`, which only `main` and `v*` tags
   may use. Their secrets live in those environments, not in the repository.
4. **Renovate** updates the dependencies through `.github/renovate.json`, which extends the
   estate's public preset: Rust crates grouped weekly with a monthly `Cargo.lock` refresh,
   action digests, and custom managers for zola, wrangler, cargo-deny and the
   estate-baseline commit of the CI gate. Where a pin carries a checksum, the pull request
   says which checksum to update, and CI refuses the download until it is.

## Rationale

- The rules that matter for a single maintainer are the irreversible ones: a lost `main`
  or a moved release tag cannot be put right by the next commit. Rules that only add
  ceremony to the maintainer's own pushes are left out.
- An environment limited to `main` keeps the deploy token away from any other branch, even
  one that changes the workflow, at no cost to the normal deploy.
- Dependency updates that arrive as reviewable pull requests, with CI and `cargo deny` run on
  them, beat the alternative: updates made in bulk when an advisory forces them.

## Consequences

- The release runbook no longer deletes tags; a version is never reused.
- The secrets must be entered into the environments by the maintainer: GitHub never reveals a
  secret's value, so existing repository secrets cannot be moved by a tool. Until they are,
  the jobs read the repository secrets of the same names.
- Renovate runs only for repositories its runner is configured to scan, with a token that can
  read and write this one. The configuration file is inert until then.
- Renovate pull requests for zola and cargo-deny fail CI until their checksum is updated by
  hand; that is the intended behaviour.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Classic branch protection with required checks and pull requests | Forces a pull request for every change of a single-maintainer project; the irreversible risks are covered by the ruleset alone |
| A bypass for administrators | The maintainer is the only administrator; a bypass would leave nothing protected |
| Dependabot | The estate runs Renovate with a shared preset; one tool covers crates, actions and the custom pins |

## References

- [ADR 0010: Local-first verification](0010-local-first-verification-gate.md)
- [ADR 0028: Release artefacts, packages and the website](0028-release-artefacts-and-website.md)
- [Set up the website deploy](../runbooks/site-deploy-setup.md),
  [Set up the AUR package](../runbooks/aur-package-setup.md)
