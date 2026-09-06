# research-lanes — Researcher Spawn Contract and Lane Menu

The shared contract every `/hex-discuss` research spawn draws from: what a
lane's prompt must carry, how findings come back, and which lanes exist —
fixed, opt-in, and the one judgment-question exception.

## Preamble

Every lane spawn extends the researcher's base spawn template
([`researcher.md`](../../hex-core/references/workers/researcher.md)) with a
lane-specific topic and the decision it informs; this file adds only what
differs per lane, never restates the base fields.

Lane spawns override one base-template field: the `### Recommendation`
self-check that template mandates does not apply here — a lane returns
findings, `negative:`, and `leads:` only, no recommendation of its own. The
one exception is the council lane below, where the user's own selection is
already the judgment call; what comes back there is the orchestrator's
synthesis across seats, never a single seat's opinion.

Neutrality itself is owned by
[`../SKILL.md` § Grill ruleset rule (d)](../SKILL.md#grill-ruleset), not restated
here — what this file adds: the exception is lexically scoped to opt-in lanes (the
council lane below may target a judgment question outright, because the user's own
selection is the position-taking, not the researcher's), and council seats carry
one further blindness rule (d) doesn't state — blind to each other's output, not
just to the user's leaning.

Findings that outgrow a paragraph persist as an artifact using the research
template's header contract
([`research.md`](../../hex-init/assets/templates/research.md)) — a
`# Research: [Topic]` title line and a `## Metadata` block — so a later
`/hex-plan` or `/hex-architect` run can pick the file up cold; anything
shorter stays inline in the return.

## Lane menu

Two lanes are the entry wave's fixed pair: **codebase recon** and
**prior-art web scan**, seeded from intake slot 1 and never gated behind a
menu — dispatch timing (slot-1 present vs deferred) is owned by
[`../SKILL.md` § Entry wave](../SKILL.md#entry-wave). Every lane, these two
included, spawns at the researcher row's pinned class in
[`models.md`](../../hex-core/references/models.md) — a capability class,
never a literal model name.

Beyond the fixed pair, the menu is open-ended: a default opt-in set the
user can multi-select, plus whatever `leads:` a returning lane surfaces
(Return schema, below) and whatever a later release registers here.

- **Community threads/blogs** — forums, issue trackers, blog posts: how
  practitioners actually talk about the problem.
- **Adjacent fields** — how a neighboring discipline solved the same shape
  of problem.
- **SOTA** — papers, benchmarks, the current frontier.
- **Competitive/vendor** — how comparable products solve it, and what they
  deliberately do differently.
- **Repo archaeology** — this repo's own history: prior attempts, reverted
  approaches, why they didn't stick.
- **Council** — the one lane that targets a judgment question instead of a
  fact; see below.

A lane's offer chip carries its running spend, e.g. `community threads — 3
researchers; 5 spent this discussion`.

## Return schema

Every lane returns the same shape: findings with sources, in the
researcher persona's own `## Research: <topic>` structure. Two fields this
bundle adds on top:

- **`negative:`** — dead ends and contradicting evidence. Reported, never
  silently dropped.
- **`leads:`** — adjacent lanes worth a follow-up, one line each: the lane
  name and the one-sentence reason. These feed the offerable lane set at
  the next multi-select.

## Council

Council is the opt-in lane that targets a judgment question outright — the
Preamble's exception, exercised. Picking it sends one design question to N
researcher seats, default 3, bounded above by the effective concurrency cap
([`protocol.md#worker-coordination`](../../hex-core/references/protocol.md#worker-coordination));
N above the cap batches per that section, announced. Council seats count
toward the expansion's hard cap of 12 researchers; a request above it
truncates to the cap, announced once — the same rule as every other lane.
Every seat runs at the researcher row's pinned `fast-balanced` class — no
escalation, this stays a breadth lane, not a depth one.

Each seat gets a distinct perspective from this file's own list — a
worker-prompt framing, not a shared technique: premortem seat,
user-advocate seat, operability seat, simplicity seat, and any others this
file later adds. Seats are blind to the user's leaning on the question and
blind to each other's output. No mutual ranking, ever: same-family seats
scoring each other measures noise, not signal.

The orchestrator synthesizes the N returns into one aside: agreements,
where seats diverge, and a single recommendation — the same judgment call
the skill already exercises on any design question, no new role added.
Cost is N spawns, stated in the offering chip text.

Edge cases: a seat that dies returns a transport note, once, same as any
other lane; cross-model seats are out of scope for this lane.
