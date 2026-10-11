# ADR 0031: The site's deploy tool installs from a lockfile

## Status

Accepted -- 2026-10-11.

## Context

The site workflow deployed with `npx --yes "wrangler@<version>"`, the version pinned in
`site/tools.env` ([ADR 0028](0028-release-artefacts-and-website.md)). The pin fixed wrangler
itself, but npm resolved wrangler's dependencies afresh on every run, so each deploy could run
a dependency release that nobody had seen. That run held the Cloudflare deploy token in its
environment, and `npx` ran the packages' install scripts with it.

## Decision

1. `site/package.json` pins wrangler as a development dependency, and `site/package-lock.json`
   locks wrangler and every package it depends on, with their integrity hashes.
2. Both the check (`scripts/site.sh worker`) and the deploy install with
   `npm ci --ignore-scripts`: exactly the locked packages, and no install scripts.
3. The deploy installs in a step of its own, without the Cloudflare secrets, and only the
   deploy step that runs wrangler gets them.
4. Renovate updates wrangler through its npm manager and refreshes the lockfile monthly.
   Wrangler updates are merged by hand: the rule in `.github/renovate.json` turns off the
   estate preset's automatic merge of npm development dependencies, which is meant for jobs
   that hold no secret.

## Rationale

- A lockfile makes the deploy run the code that the pull request's checks ran, not whatever
  npm resolves on the day.
- Wrangler and its runtime ship their platform binaries as optional dependencies, so they work
  without install scripts; `scripts/site.sh worker`, which runs `wrangler dev`, shows that they
  do.

## Consequences

- The zola pin and its checksum stay in `site/tools.env`; only wrangler moved.
- A wrangler update arrives as a Renovate pull request that changes `package.json` and the
  lockfile; the site workflow checks it before it is merged.
- `site/node_modules/` is ignored; `scripts/site.sh worker` creates it.

## References

- `site/package.json`, `site/package-lock.json`, `scripts/site.sh`, `.github/workflows/site.yml`
- [ADR 0029: Repository rulesets, deploy environments and Renovate](0029-repository-protections-and-dependency-updates.md)
- [Set up the website deploy](../runbooks/site-deploy-setup.md)
