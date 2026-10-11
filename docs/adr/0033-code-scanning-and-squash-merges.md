# ADR 0033: CodeQL code scanning, and pull requests merge by squash only

## Status

Accepted -- 2026-10-11.

## Context

[ADR 0029](0029-repository-protections-and-dependency-updates.md) set the repository's
rulesets, deploy environments and Renovate. Two settings were left at GitHub's defaults:

- Code scanning was off. The workflows run shell steps with values that come from events
  (tags, refs, inputs), the class of input behind workflow script injection, and nothing
  checked them except review.
- Pull requests could merge by merge commit, rebase or squash. The maintainer pushes to `main`
  directly; pull requests come from Renovate and from contributors, and `main` has a linear
  history.

## Decision

1. **CodeQL default setup** analyses the repository on every push to `main`, on every pull
   request and once a week, with the default query suite, for each language GitHub detects:
   the workflows (`actions`), Rust, and the site's and scripts' JavaScript and Python.
   Alerts appear in the repository's Security tab. Nothing waits for the analysis: required
   status checks stay off (ADR 0029).
2. **Squash only.** A pull request merges as one commit whose title is the pull request's
   title and whose message is its commits' messages. Merge commits and rebase merges are off.

## Rationale

- CodeQL's `actions` queries check exactly the workflow risks a reviewer misses most easily,
  and the Rust queries add a second look at the code that handles untrusted archives, file
  names and server replies. Default setup needs no workflow file to maintain.
- One commit per pull request keeps `main` linear, and its title is a Conventional Commit
  title: Renovate writes them, and [CONTRIBUTING.md](../../CONTRIBUTING.md) asks for them.

## Consequences

- An alert does not block a push or a merge; the maintainer reads the Security tab.
- A contributor's pull request with several commits lands as one; its commit messages stay in
  the squashed message. A pull request therefore holds one logical change.

## References

- [ADR 0029: Repository rulesets, deploy environments and Renovate](0029-repository-protections-and-dependency-updates.md)
- [CONTRIBUTING.md](../../CONTRIBUTING.md)
