---
name: plan-design
description: Create a design for an explicit design request or a material unresolved design decision.
---

# Plan design

Read the relevant repository context and any approved design first. Honor its decisions
and existing implementation authorization. Routine edits with a clear outcome do not need
this workflow. A design-only request ends with the design; it does not authorize execution.

Resolve only consequential unknowns. Batch related questions using the available question
tool, or ask in conversation if none exists. Ask sequentially when an answer determines
the next question. Compare alternatives only where a real choice remains.

Write the design at `docs/specs/YYYY-MM-DD-<topic>-design.md` unless directed otherwise.
Describe the outcome, boundaries, relevant architecture, tradeoffs and acceptance checks.
Read the [lifecycle contract](../../references/documentation/lifecycle.md); new specs start
as `draft`. Keep explicit human review gates. Request any remaining design decision against
the concrete written artifact; do not ask again for decisions already approved.

If an independent review is requested or warranted, use the active runtime adapter and
[review brief](references/review.md). Otherwise check scope and consistency locally.
Use available visual tools when they help explain a design. For an authorized implementation plan, use the available `plan-writing`
skill or write the equivalent plan directly when that skill is unavailable.
