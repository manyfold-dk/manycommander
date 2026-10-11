# manycommander documentation for contributors

User documentation lives on [manycommander.app](https://manycommander.app/docs/) and in
[site/content/docs/](../site/content/docs/). This directory holds what a contributor needs:
how the program is built, why it is built that way, and how it is released.

| Directory | Holds | Changes when |
|---|---|---|
| [tdr/](tdr/README.md) | **Technical design records.** How each part of manycommander works and the invariants it upholds. Living documents: they describe the code on `main` | A change alters behaviour that a TDR describes. The TDR changes in the same pull request as the code |
| [adr/](adr/README.md) | **Architecture decision records.** One decision each: the context, the decision, the alternatives and the consequences. Immutable once accepted | A decision of note is made or reversed. A reversal is a new ADR that supersedes the old one |
| [runbooks/](runbooks/) | Maintainer procedures: verifying a release by hand, setting up the AUR package and the website | A procedure changes |
| [perf/](perf/history.md) | Benchmark history and performance investigations | `scripts/bench/run.sh` runs, or an investigation is written up |

## Identifiers

The design uses stable identifiers. Code comments and test names cite them, so they never
change meaning and are never reused.

| Identifier | Meaning | Defined in |
|---|---|---|
| `TDR-1 4.7` | Section 4.7 of TDR 0001. Section numbers in a TDR are stable | [tdr/](tdr/README.md) |
| `I-1` .. `I-10` | Invariants of the local file operations | [TDR-1 4.1](tdr/0001-core.md#41-invariants), [TDR-2 1.2](tdr/0002-navigation-and-bulk-tools.md#12-invariants) |
| `A-1` .. `A-5`, `R-1` .. `R-6`, `V-1` .. `V-6` | Invariants of archives, SFTP and previews | [TDR-3 1.3](tdr/0003-archives-quick-view-and-sftp.md#13-invariants) |
| `A-FS-5`, `A-QV-8`, ... | Acceptance checks. Most are automated tests that carry the identifier in their name | The acceptance section of each TDR |
| `P-1` .. `P-27`, `NFR-*` | Performance targets and other non-functional requirements | The requirements section of each TDR |
| `TDR-2 E-28` | Implementation decision 28 of TDR 0002, in its appendix A | The appendix of each TDR |
| `ADR 0007` | An architecture decision record | [adr/](adr/README.md) |

## How the design changes

1. A change that alters what a TDR describes updates that TDR in the same pull request.
   Section numbers stay; a new section takes the next free number.
2. A change that makes a decision of note, such as a new dependency, a new invariant, a
   trade-off against a performance target or a rejected alternative, adds an ADR with the
   next number. The TDR links to it.
3. Acceptance checks are tests. A new check gets the next free identifier in its group, and
   the test name carries it (`a_fs_14_...`).
4. `scripts/check.sh full` must pass. See [CONTRIBUTING.md](../CONTRIBUTING.md).
