# Runbook writing

## Runbook Format

Operational procedures follow this structure:

```markdown
# Procedure Name

## Prerequisites
- Required tools and access
- Configuration needed

## Pre-Action Checklist
- [ ] Verification steps before starting

## Procedure

### Step 1: Action Name
Explanation and commands.

### Step 2: Action Name

## Verification
How to confirm success.

## Rollback
Recovery steps if something fails.
```

## Simplified Technical English (Runbooks Only)

Runbooks are read under time pressure, often by somebody who did not write them. Runbooks
in `docs/runbooks/` therefore follow **ASD-STE100 Simplified Technical English**, limited to
the rules below.

**No other document type uses STE.** ADRs, specs, plans and READMEs carry argument,
causality and trade-off. STE flattens those, so applying it there loses content.

### Rules

| Rule | Requirement |
|------|-------------|
| One instruction per sentence | A step that does two things is two steps. |
| Sentence length | Maximum 20 words in a procedure, 25 in a description. |
| Paragraph length | Maximum 6 sentences in a procedure. |
| Voice | Active only. Name the actor: "ArgoCD syncs the application". |
| Mood | Imperative for steps: "Run", "Check", "Confirm". |
| Tense | Present tense. Do not use future or conditional forms. |
| Articles | Always include them. Write "Check the pod status", not "Check pod status". |
| Pronouns | Do not carry `it`, `this` or `that` across sentences. Repeat the noun. |
| One word, one meaning | Choose one term per concept and keep it for the whole document. Do not alternate `sync`, `reconcile` and `apply` for the same action. |
| Warnings first | A warning goes before the step that it applies to, never after. |
| No ellipsis | Write the full clause. |

### Technical terms

The STE approved word list does not cover this estate's vocabulary. Treat product names,
resource kinds, commands and flags as **Technical Names** (`PersistentVolumeClaim`,
`kubectl`, `--kubeconfig`). Treat the verbs that the tools themselves use as **Technical
Verbs** (`sync`, `reconcile`, `drain`, `cordon`, `vendor`). Use both freely.

The rules above are what STE buys us. The dictionary is not, so do not enforce it.

### Example

Before:

```markdown
Once the ArgoCD app has been refreshed it should sync automatically, though if it doesn't
you may need to check whether selfHeal is still enabled and possibly restart the operator.
```

After:

```markdown
1. Refresh the ArgoCD application.
2. Confirm that the application status changes to `Synced`.
3. If the status does not change, check that `selfHeal` is `true`.
4. If `selfHeal` is `true`, restart the `cilium-operator` pod.
```
