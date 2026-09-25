# Retro: <YYYY-MM-DD> — <interactive | loop <goal file stem> | import <file name>>

Inputs: <n> entries (<n> self, <n> trajectory, <n> seed), <n> skipped, <n> ignored, <n> rows touched — as of <UTC ISO-8601: the `at` retro.py fold printed for this run's last fold>
<Degraded: and Warning: lines, one per line, as printed in the handoff>

<!-- Proposals go under ## Harness or ## Project by their rows' scope,
     reopened rows first, numbered P-1, P-2 … across the report. Every
     field is required; Allow-list: is present on, and only on, an in-loop
     proposal. Entry text is echoed quoted and at most 120 characters
     (protocol § Untrusted-text echoes). -->

## Harness

### P-1 — <title>

- Target: <repo path[#anchor] | upstream <name>@<pinned>>
- Change: <the concrete edit — an addition or a prune>
- Rationale: <why the instruction exists, or why it changes; quoted entry evidence>
- Size: <small | large>
- Route: <applied | applied (pre-existing fix) | asked | in-loop | deferred | upstream-draft | candidate>
- Ledger: <id>[, <id>]
- Allow-list: <Route in-loop only — the SKILL.md § Routing and gate loop allow-list, quoted verbatim>

## Project

<proposals in the same shape, or none>

## Prunes considered

- <instruction path#anchor> — <kept | pruned in P-<n>>: <why>
<or: none — <why>>

## Upstream drafts

<per upstream-draft proposal: an issue draft — title, its P-<n> fields, the quoted evidence — or none>

## Deferred

<!-- one line per deferred proposal, or none -->
- [ ] <title> — P-1

## Ledger

| id | status | occ | folds | cost_min | big |
|---|---|---|---|---|---|
| <id> | <open \| deferred \| fixed \| reopened> | <n> | <n> | <n> | <yes \| no> |
