# ADR 0034: manycommander is licensed under MIT or Apache-2.0

## Status

Accepted -- 2026-10-11.

## Context

manycommander was licensed under Apache-2.0 alone. Omarchy, the desktop it is made for, and
Omarchy's plugins are MIT-licensed, and so are most of the tools it sits beside. Apache-2.0
code cannot be combined into a GPL-2.0-only work, and a project under MIT cannot take
Apache-2.0-only code without adding the Apache terms. Most Rust crates, including almost all
of manycommander's dependencies, are offered under "MIT OR Apache-2.0".

Every commit so far comes from the maintainer, with no outside contributions and no forks,
so the project's licence can still be changed by the maintainer alone. After the first
outside contribution it would need every contributor's consent.

## Decision

1. manycommander is licensed under MIT or Apache-2.0, at the user's option. `Cargo.toml`
   says `license = "MIT OR Apache-2.0"`.
2. `LICENSE-APACHE` holds the Apache License 2.0 text, unchanged from the former `LICENSE`.
   `LICENSE-MIT` holds the MIT text with the line "Copyright (c) 2026 Manyfold".
3. Contributions come in under the same terms: unless a contributor states otherwise, a
   contribution submitted for inclusion is dual licensed as above (the clause in `README.md`
   and `CONTRIBUTING.md`, the Rust ecosystem's usual wording).
4. Release tarballs carry both licence files. The AUR package declares the SPDX expression
   `MIT OR Apache-2.0` and installs `LICENSE-MIT`, because the MIT text carries the copyright
   line; Arch ships the Apache text itself.

## Rationale

- MIT keeps manycommander combinable with Omarchy and its plugins, and with GPL-2.0 code;
  Apache-2.0 keeps its explicit patent grant for those who want it.
- The dual licence is what Rust users expect, so the crate needs no explanation.
- Relicensing costs nothing now and much more later.

## Consequences

- Releases published before this decision were under Apache-2.0 alone and stay so; later
  releases are dual licensed.
- `deny.toml` is unchanged: every dependency licence it admits is permissive and compatible
  with both licences.
- A future change of licence needs the consent of every contributor whose work remains.

## References

- `LICENSE-APACHE`, `LICENSE-MIT`, `Cargo.toml`, [README.md](../../README.md#license),
  [CONTRIBUTING.md](../../CONTRIBUTING.md)
- [ADR 0028: Release artefacts, packages and the website](0028-release-artefacts-and-website.md)
- [ADR 0032: Releases carry the licence texts of the crates they are built from](0032-third-party-licence-notices.md)
