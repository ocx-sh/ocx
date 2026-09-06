# Worked renderings

**Instances, not rules.** Every fence below is one filling of a shape defined
elsewhere — the flow in [`SKILL.md`](../SKILL.md), the rules in
[`finalize.md`](../../hex-core/references/finalize.md) — and nothing here
defines a term either of those owns. Values are illustrative. Where a fence
elides content it is said **outside** the fence, so no rendering can be mistaken
for a complete one.

## The gate, rendered

The full rung, with a base that advanced and a re-verification that therefore
ran. Shape is `protocol.md`'s `<label>: <resolved value> (<source>)`.

```
Finalize: feat/inbox-search → main             (PR #128, draft)
Conventions — authoritative (no checked-in file can change these):
  target branch     main                       (PR #128 base field)
  merge strategy    squash                     (forge: repo merge settings)
  release workflow  .github/workflows/integration.yml   (CLAUDE.md § Verification)
  verification      task verify                (hex.md › Pointers → CLAUDE.md)
  branch protection unknown                    (rulesets read returned empty — NOT "none")
Conventions — narrowing (checked-in text may tighten, never widen):
  series shape      minimal logical commits    (1 project-documented: CONTRIBUTING.md)
  squash policy     bisectable series          (3 shipped default — undocumented,
                                                and no hex.md › Preferences hint)
  message format    conventional commits       (commitlint config — declared)
  sign-off          DCO required               (forge: required check `dco` — enforced)
  signing           ssh, user.signingkey set   (git config)
Commits: 32 → 3
  1  feat(inbox): full-text search over threads       +signoff  +re-signed
  2  chore(ci): pin actions to commit SHAs            +signoff  +re-signed
  3  docs(inbox): search syntax in the README         +signoff  +re-signed
  Signed-off-by identity: Dana Okafor <dana@example.org>   (git config user.*)
  Other authors on this branch: Sam Reyes <sam@example.org> — Co-authored-by on 1
  Message/diff check: 3/3 pass
Local verification: green                       (task verify, pre-rewrite)
Rebase onto main: clean · base advanced 4 commits → verification RE-RAN: green
Workflow drift: none — this branch modifies no file under .github/workflows/
Auto-merge: not armed · no merge queue          (forge PR read)
Remote acts (3), all against feat/inbox-search and PR #128:
  force-push    git push --force-with-lease=feat/inbox-search:a1b2c3d \
                  origin 4e5f6a7:refs/heads/feat/inbox-search
  dispatch      integration.yml on the final SHA, watch to green (rerun ceiling 1)
  pull request  ledger block, then draft → ready when green
Never: main or any other branch is pushed · nothing is merged · branch
       protection and rulesets untouched · no tag, release or workflow file
       created or edited · no other PR touched · no credential provisioned,
       minted or stored · no changelog file written
Identity: dokafor via the forge CLI · credential: ambient login (no token override)
          scopes repo, read:org, gist — broader than this run needs
          (contents write, actions write, pull-requests write)
Backup: backup/feat/inbox-search-pre-finalize @ 9f8e7d6   (armed)

Invoking /hex-finalize granted this action class. These commits already carry
your sign-off and signature, but only in a local ref — approving publishes them
permanently. Approve this instance?
  yes — perform the three remote acts above
  no  — stop here; the rewrite stands, the backup ref is released
        (restore: git reset --hard 9f8e7d6)
```

Annotations, each pointing at the rule its line renders:

- The **push line is a filled instance** of the form owned by
  [`finalize.md` § Force-push mechanics](../../hex-core/references/finalize.md#force-push-mechanics);
  that file owns the form, this is one filling of it, and a mismatch between
  them is a defect in this file.
- The **`Never:` line is the act set's never-list in full**, not a summary —
  [`finalize.md` § The act set](../../hex-core/references/finalize.md#the-act-set).
- The **two convention blocks, the resolution-step numbers on the series-shape
  rows, and `unknown` rendered as `unknown`** are
  [`SKILL.md` § Discover conventions](../SKILL.md#discover-conventions) and
  [§ Recompose](../SKILL.md#recompose) made visible.
- The **complete commit list, the literal signing identity, the re-ran
  verification note, and the drift and auto-merge lines present even when the
  answer is none** are field groups 5–11 of
  [`SKILL.md` § Gate](../SKILL.md#gate).
- The **credential source and the rights beside the scopes** are field group 14
  of the same section.

**The drift-present variant.** Where the branch modifies a documented
workflow's file, that line names the paths and states what the dispatch will
run — the security-critical case, so it is rendered rather than described:

```
Workflow drift: 2 files under .github/workflows/ are modified by this branch —
                integration.yml, lint.yml
                The dispatch executes THIS BRANCH's version of these files.
                Pushing them also needs a workflow-scoped credential.
```

## The local-only rung

No forge CLI answers at pre-flight (a). The fetch in (c) still succeeds over
the ordinary git transport, so the rebase base is a real remote target and
pre-flight, convention discovery, local verification, recomposition and the
gate all run unchanged; only the remote phase is withheld. The fence below is
**elided** — it shows only the lines that differ from the full rung above:

```
Finalize: feat/inbox-search → main             (discovered trunk — no forge CLI)
Conventions — authoritative (no checked-in file can change these):
  target branch     main                       (discovered trunk)
  merge strategy    unknown                    (no forge CLI to ask)
  release workflow  unknown                    (no forge CLI to ask)
  branch protection unknown                    (no forge CLI to ask)
Auto-merge: unknown                             (no forge CLI to ask)
Remote acts (0) — local-only rung: no forge CLI is authenticated.
  These stay yours: the force-push, the workflow dispatch, and the draft → ready
  flip. The handoff names each one.
Identity: none — no forge CLI · credential: n/a
Backup: backup/feat/inbox-search-pre-finalize @ 9f8e7d6   (armed)

Invoking /hex-finalize granted this action class. These commits already carry
your sign-off and signature, but only in a local ref — approving them is what
you will publish by hand. Approve this recomposed series?
  yes — keep the rewrite; the remote acts above are yours to perform
  no  — stop here; the rewrite stands, the backup ref is released
        (restore: git reset --hard 9f8e7d6)
```

The gate still asks here, and the rung is selected at pre-flight rather than
chosen — [`finalize.md` § Degrade ladder](../../hex-core/references/finalize.md#degrade-ladder)
and [§ Consent model](../../hex-core/references/finalize.md#consent-model) own
both of those rules.

## The resume gate — a published rewrite

Re-invoked after the push landed but before the dispatch. The chain in
[`finalize.md` § Re-entry](../../hex-core/references/finalize.md#re-entry) finds
an **armed** backup ref and a remote tip equal to the local tip, so the rewrite
is resumed from rather than rebuilt, and the gate asks again with a **reduced
act set**:

```
Finalize: feat/inbox-search → main             (PR #128, draft)
Resuming a published rewrite — this branch's recomposed series is already on
the remote. This run rewrites nothing and pushes nothing.
Commits: 3, unchanged since the push           (ls-remote tip == local tip)
  1  feat(inbox): full-text search over threads       +signoff  +re-signed
  2  chore(ci): pin actions to commit SHAs            +signoff  +re-signed
  3  docs(inbox): search syntax in the README         +signoff  +re-signed
  Signed-off-by identity: Dana Okafor <dana@example.org>   (git config user.*)
  Other authors on this branch: Sam Reyes <sam@example.org> — Co-authored-by on 1
  Message/diff check: 3/3 pass                 (re-checked against the pushed series)
Pushed SHA: 4e5f6a7                             (already on origin)
Workflow drift: none — this branch modifies no file under .github/workflows/
Auto-merge: not armed · no merge queue          (forge PR read, re-read this run)
Remote acts (2), all against feat/inbox-search and PR #128:
  dispatch      integration.yml on 4e5f6a7, watch to green (rerun ceiling 1)
  pull request  ledger block, then draft → ready when green
Never: nothing is rewritten · nothing is force-pushed · main or any other
       branch is pushed · nothing is merged · branch protection and rulesets
       untouched · no tag, release or workflow file created or edited · no
       other PR touched · no credential provisioned, minted or stored · no
       changelog file written
Identity: dokafor via the forge CLI · credential: ambient login (no token override)
Backup: backup/feat/inbox-search-pre-finalize @ 9f8e7d6   (armed)

This session has not approved these acts. Approve this instance?
  yes — perform the two remote acts above
  no  — stop here; nothing further is published, the backup ref is released
```

The act count drops to two and the `Never:` line grows the two withheld acts,
because the approval a resume asks for is narrower than a first run's. **Drift
and auto-merge are re-disclosed rather than carried over** — they govern the
dispatch and the flip, which are exactly the acts a resume still performs, and
either can have changed since the push. This fence is otherwise **elided**: the
two convention blocks and the verification and rebase rows are omitted here
because a resume neither re-verifies nor rebases; a real resume gate renders
them as they stood at the approved push.

## The handoff block

Success on the full rung, one dispatched workflow green, the PR flipped:

```markdown
## Finalize Complete: feat/inbox-search

- Outcome: published — force-pushed and PR ready
- Commits: 32 → 3
    1  feat(inbox): full-text search over threads       +signoff  +re-signed
    2  chore(ci): pin actions to commit SHAs            +signoff  +re-signed
    3  docs(inbox): search syntax in the README         +signoff  +re-signed
- Pushed SHA: 4e5f6a7
- Dispatched this run: green — integration.yml (run 8412, 2 jobs)
- Running now: none
- Pull request: https://example.invalid/acme/inbox/pull/128 — ready for review
- Backup ref: backup/feat/inbox-search-9f8e7d6 (inert; pruning it after the
  merge is yours)
- Next: the merge is yours — review PR #128 and merge it when you are ready.
```

The same block on the local-only rung. Every field above is still present, with
the rung's differing values in place:

```markdown
## Finalize Complete: feat/inbox-search

- Outcome: recomposed and approved — local-only rung, no forge CLI
- Commits: 32 → 3
    1  feat(inbox): full-text search over threads       +signoff  +re-signed
    2  chore(ci): pin actions to commit SHAs            +signoff  +re-signed
    3  docs(inbox): search syntax in the README         +signoff  +re-signed
- Pushed SHA: absent — nothing was pushed
- Dispatched this run: no remote gate exists — no forge CLI to dispatch through
- Running now: none
- Pull request: absent — none was read or created (no forge CLI)
- Yours to perform: push the branch, dispatch integration.yml against the
  pushed SHA, then open or ready the pull request
- Backup ref: backup/feat/inbox-search-9f8e7d6 (inert; pruning it after the
  merge is yours)
- Next: the merge is yours — publish the series above, then merge when ready.
```

Annotations: the **explicit absent marker on the pushed-SHA field**, the **two
independent check lines**, the **`Yours to perform:` field on a degraded rung**
and the **`Next:` line that names the merge and emits no hex command** are the
`It carries:` list of [`SKILL.md` § Handoff](../SKILL.md#handoff); the rung that
adds the fourth is
[`finalize.md` § Degrade ladder](../../hex-core/references/finalize.md#degrade-ladder).
