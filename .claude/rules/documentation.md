---
paths:
  - "**/*.md"
---

# Documentation

Apply this rule to document work. Markdown is the source of truth. Use relative internal
links, `--` rather than an em dash, and `EUR` for currency. Use numbered MADR ADRs in
`docs/adr/`. Update a maintained documentation index when adding or moving documents.
Use a table of contents for documents with enough sections to need navigation; keep its
anchors current. Short skills and adapters do not need a contents table.

| Work | Read when applicable |
|---|---|
| Plan or spec creation/status change | [Lifecycle](../references/documentation/lifecycle.md) -- required fields, vocabulary and transition ownership |
| ADR or README needing a template | [Templates](../references/documentation/templates.md), unless the repo provides its own |
| Runbook under `docs/runbooks/` | [Runbook rules](../references/documentation/runbooks.md) -- STE applies only here |

Use language-tagged code blocks and locally rendered diagrams or inline Mermaid. Verify
relative links after moves, including frontmatter `source` links. A read-only review does
not change lifecycle status. Do not create unrelated documentation just to fill a template.
