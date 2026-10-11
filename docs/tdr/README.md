# Technical design records

A technical design record (TDR) describes how one part of manycommander works: its
invariants, its mechanisms, its limits and the checks that hold them. A TDR is a living
document. It describes the code on `main`, and it changes in the same pull request as the
code it describes. An [architecture decision record](../adr/README.md) explains why a
mechanism was chosen; a TDR explains what the mechanism is.

| TDR | Covers | Cited as |
|---|---|---|
| [0001](0001-core.md) | Core: threads and events, the file-operation engine and its invariants I-1 to I-7, panels, the command line and hand-off, theming, the keymap, launch, performance targets | `TDR-1 <section>` |
| [0002](0002-navigation-and-bulk-tools.md) | Navigation and bulk tools: directory hotlist and frecency, the quick filter, find, multi-rename, compare, links and attributes, sparse files and hard links | `TDR-2 <section>` |
| [0003](0003-archives-quick-view-and-sftp.md) | Beyond the local disk: the provider seam, archive browsing and extraction, the image quick view, SFTP | `TDR-3 <section>` |

## Conventions

- **Section numbers are stable.** Code comments cite them (`TDR-1 4.7`), so a section is
  never renumbered. A new section takes the next free number. A section whose subject goes
  away keeps its heading and says where the subject went.
- **Each TDR builds on the ones before it.** TDR 0002 and TDR 0003 state only what they add
  or change; TDR 0001 stays normative for everything else.
- **Acceptance checks** are listed in each TDR with their identifier (`A-FS-5`). An *auto*
  check is an automated test, a *bench* check runs in `scripts/bench/run.sh`, and a
  *manual* check is in the [release verification runbook](../runbooks/release-verification.md).
- **Appendix A** of each TDR lists the implementation decisions that refined the design
  while it was built (`TDR-2 E-28`). Their numbers are stable; gaps are entries that
  recorded process rather than design.

## History

These records began as design specifications, each followed by an implementation plan and an
adversarial review. The specifications, plans and review records are in the repository
history up to the commit that introduced this directory. The TDRs carry forward everything
that still describes the program; the [ADRs](../adr/README.md) carry the decisions.
