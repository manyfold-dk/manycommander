# ADR 0001: Technical design records for the design, ADRs for the decisions

## Status

Accepted -- 2026-10-11.

## Context

manycommander was built in three stages. Each stage had a design specification, an
adversarial review of it, an implementation plan with a task list and an execution log, and
amendments as the implementation met reality. That served a single maintainer well, but it
reads badly from outside:

- The current behaviour of one mechanism was spread over a specification section, its dated
  amendments, a review appendix and a decision log in a plan.
- The reason for a choice sat in an alternatives table, a review resolution or a plan row,
  next to process detail (task numbers, who decided, when the reviewer ran) that a reader of
  a public repository does not need.
- Code comments cite the specifications by section about 1,300 times (`M1 4.7` meant
  section 4.7 of the first specification). Any restructuring that renumbers sections breaks
  those citations silently.

## Decision

Two kinds of record, each with one job:

1. **Technical design records** (`docs/tdr/`) describe how the program works now: invariants,
   mechanisms, limits and acceptance checks. A TDR is living and changes in the same pull
   request as the code. The three specifications became TDR 0001 to TDR 0003, with their
   section numbers kept, their amendments folded into the text, their review appendices
   removed, and the plans' implementation decisions moved into an appendix.
2. **Architecture decision records** (`docs/adr/`) record one decision each, with context,
   rationale, consequences and the alternatives that lost. Accepted ADRs are immutable; a
   reversal is a new ADR.

Code cites a TDR section as `TDR-<n> <section>` and an implementation decision as
`TDR-<n> E-<m>`. Invariant, acceptance-check and performance-target identifiers (`I-1`,
`A-FS-5`, `P-23`) keep their meaning everywhere.

Plans, review transcripts and execution logs are working material. They are not kept in this
repository after their outcome is recorded in a TDR, an ADR, a test or the changelog.

## Rationale

- A reader who wants to know what the code does reads one TDR section. A reader who wants to
  know why reads one ADR. Neither has to replay a history of amendments.
- Keeping the section numbers makes every existing citation in the code resolve with a
  mechanical rename of the citation key, with no hand-edited mapping.
- The identifiers of invariants and checks are the strongest link between the design and the
  test suite (`a_fs_5_...` tests `A-FS-5`). They outlive any document layout.

## Consequences

- Every pull request that changes described behaviour must update a TDR. Review checks it.
- The TDRs are long. They are reference documents with a contents line at the top, not
  reading material.
- The history of how a design evolved, including the review findings that shaped it, is in
  the repository history rather than in the tree. The TDR index says where.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Keep the specifications and plans as they were | Current behaviour stays spread over amendments and logs, and the process detail reads as personal notes |
| One ADR per section, no TDRs | ADRs are immutable; the description of a mechanism must change with the code |
| Rewrite the design per subsystem with new section numbers | Breaks about 1,300 code citations, for a reorganisation that the TDR split already mostly gives |
