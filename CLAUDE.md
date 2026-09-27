
<!-- BEGIN baseline-agent (vendored from baseline-agent @ agent-2.3.0; do not edit) -->
## Common policy

<!-- BEGIN policy -->
## Shared execution contract

- **AUTH-01 -- Intent and authorization:** Deliver the requested assessment, plan, or
  implementation. Continue already-authorized work through its agreed completion criteria.
  Ask only about material unresolved decisions or actions outside that authorization.
  Preserve explicit human review gates; a model review does not grant implementation consent.
- **BRANCH-01 -- Branch consent:** A workflow or skill invocation alone does not request a
  branch. Work on the current branch unless the user explicitly requests a new branch.
- **OWN-01 -- Attribution:** Record the task base, intended paths, initial staged/unstaged/
  untracked paths, and explicitly owned commits. Keep durable Git facts and scope metadata at
  the worktree-local path from `git rev-parse --git-path agent-tasks/<task-id>.json` when the
  task spans commits or sessions. Store no file contents or secrets. Re-read the note after
  resume. A base, path, author, recorded commit, or optional `Task-Id` trailer is evidence,
  not authenticated authorship. Resolve overlaps and every unattributed commit before
  claiming complete review coverage. Preserve unrelated edits and report unresolved coverage.
- **PUBLISH-01 -- Concurrent dirt:** Never stash, restore, reset or temporarily move another
  session's edits to enable publication. A failed pull/rebase is not permission to push.
  A fresh fetch or expected fast-forward never waives successful pull/rebase and verification
  before push.
  If safe, publish the owned commit through an owned disposable detached checkout: pull
  with rebase, run relevant verification there, then push normally. Preserve the original
  checkout and index. Otherwise coordinate and report the publication dependency.
- **PUBLISH-02 -- Publication gate:** A repository designated public receives nothing that
  names a tenant, a client, a private repository, a host, an address, an exact version in
  use or a credential. Run the publication gate before every push to such a repository and
  treat a hit as a stop, never as a warning. The name deny-list is itself such a value: never
  copy it, or a term from it, into a public repository, not as a fixture or a test case.
  Add a new tenant, client or private repository to the deny-list in the change that
  creates it, before its name is used anywhere. A repository the owner designates public
  from its creation is registered as designated public instead and never enters the
  deny-list.
- **VERIFY-01 -- Evidence:** Run mandatory repository checks and checks appropriate to the
  change. Repeat or broaden only after changes, failures, or unresolved concerns. Report
  actual results and limitations. Review the complete task, including uncommitted changes.
- **REPO-01 -- Equipment:** The active repository/worktree owns commands, verification,
  environment boundaries, and overlays. Global convenience copies do not override them.
  Route cross-repository work to an equipped session under the boundary rules below.
- **SECRET-01 -- Confidentiality:** Never put credential values in tracked files, examples,
  fixtures, reports, command output, or transcripts. Read only the configuration fields
  needed for the task; do not dump raw settings or environments.
- **COMPLETE-01 -- Continuity:** During long work, give useful progress grounded in evidence.
  Preserve original scope, exact authorization, constraints, completed and remaining work,
  unresolved decisions, and the next action through compaction/resume. Finish with a
  self-contained account of the deliverable, checks, and remaining gaps.

## Git -- branching

- Create a git branch only when the user explicitly asks for one; otherwise work on the
  current branch, including `main`. This takes precedence over a runtime default such as
  "branch first when on the default branch".
- Create a requested branch as a git worktree, not an in-place branch, following the
  repo's worktree convention where one exists.

## Git -- commits and pushing (autonomous)

- **Commit your own work at sensible checkpoints** -- a coherent unit is done, tests pass,
  or you are about to start something riskier -- without asking first. This takes precedence
  over a runtime default such as "commit or push only when the user asks".
- Conventional Commits: `<type>[scope]: <description>` -- `feat`, `fix`, `docs`, `test`,
  `refactor`, `build`, `ci`, `chore`. Keep each commit one logical, reviewable change --
  don't bundle unrelated edits or commit speculative/throwaway work.
- **Push your checkpoints** so other agents can build on them. Before pushing: `git pull
  --rebase`, then re-run the relevant verification, then push. Do not force-push
  (`--force` / `--force-with-lease`) a shared branch; other agents build on it.
- **Assume other agents may be changing the same repo concurrently.** Before staging,
  re-check `git status` / `git diff` and stage explicit paths (not `git add -A`) so you
  commit only your own changes, never another agent's in-flight work. Reconcile conflicts by
  integrating both sides; never discard commits or edits you did not author.

## Documentation & decisions

- Markdown is the source of truth; generate other formats only for distribution.
- Architecture decisions use **MADR** ADRs in the repo's `docs/adr/`.
- Link between documents with relative paths.

## Writing style -- documents

- Use `--` (two hyphens), not the unicode em-dash, in Markdown.
- Use `EUR`, not the euro symbol, when referencing currency.
- Runbooks under `docs/runbooks/` additionally follow the Simplified Technical English
  rules in the repository documentation rule. No other document type does.

## Writing style -- conversation

Answer at the altitude the question was asked. The reader is an experienced engineer.
Brevity comes from cutting ceremony, never from cutting reasoning.

- **Answer first.** Lead the reply with the finding or the result, without preamble or a
  restatement of the question. During long tool work, a line on what you are doing and what
  you have found so far is progress, not preamble.
- **One claim per sentence.** Split compound assertions so each can be checked separately.
- **Active voice, named actor.** "ArgoCD reverts the patch", not "the patch gets reverted".
  Without a subject the reader cannot tell what to go and fix.
- **Mark uncertainty once, explicitly.** Say "unverified" or "hypothesis", then state the
  claim plainly. Do not spread hedges (`might`, `possibly`, `it seems`) through a paragraph
  that is actually load-bearing.
- **Tables for anything enumerable.** Repos, versions, statuses, options -- a table, not
  prose.
- **Evidence beside conclusions.** Quote the command output or the `file:line` that supports
  a claim instead of describing it.
- **No closing summary** that repeats the reply. After long tool work, the final message
  still states what was done, checked and left open (COMPLETE-01).

Keep conditional and causal structure -- "X, because Y, unless Z". That structure carries
the engineering content; flattening it is a loss, not a simplification.

## File hygiene

- Never commit `*.confidential.*` or anything under `/confidential/`.
- `*.draft.*` files are local work-in-progress -- do not commit.

## Working across repositories

The estate is a set of sibling git repositories under one parent directory on disk. This
repository is one of them. The table below is the estate's own and is filled in by the
private overlay when the baseline is vendored.

<!-- working-across-repositories-overlay -->

- **Work inside this repo.** A session started in a repo root is equipped for that repo only:
  its rules, skills, agents, and permissions. Siblings' equipment does not load here.
- **If a change is needed in a sibling repo, stop and say what the sibling needs** (which repo,
  which files, what outcome). The user routes it: to a session in that repo, or to the cockpit
  session started in the parent directory, which is the only place that loads every sibling's
  rules and skills.
- **Never edit `../<sibling>` from this session** unless the user explicitly asks for it in this
  session. Cross-repo commits from a repo session are the main source of drift.
<!-- END policy -->

## Claude runtime adapter

Use the active Claude session's loaded instructions, available-skills catalog and tools.
Repository `.claude` assets provide Claude rules and skills; read the active repository's
`environment.md` overlay when using a shared skill. Do not load AGENTS.md to obtain this
shared policy. Claude-specific frontmatter does not grant permissions to other runtimes.

Use native Claude delegation/messaging only when available, appropriate, and authorized.
Otherwise execute locally when equipped, or route to an equipped repository session with
outcome, scope, owner paths, authorization and verification. Use agent-mailbox when a
cross-runtime handoff lacks native messaging or the operator requires an on-disk audit
trail. Never invent tool names or route through a peer to bypass permissions.

Keep model guidance neutral unless the exact
model is confirmed and its optional guide is deliberately selected. A model switch does
not prove previously loaded guidance was removed; use a fresh specialized session instead.
<!-- END baseline-agent -->
