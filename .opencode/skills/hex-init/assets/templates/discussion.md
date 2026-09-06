# Discussion: [Topic]

<!--
Pre-plan discussion artifact — an interactive elaboration phase before
/hex-plan or /hex-architect. Filename and location: this project's
documented discussions convention; `.agents/discussions/<slug>.md` if
undocumented. One file per discussion; slug derives from the topic and
stays stable for the discussion's life.
Owner: /hex-discuss. Handoff: /hex-plan; /hex-architect (fast-path input
only at `handed-off → architect`); a spec is never written here directly
(reached via /hex-review's Fold-Back on the converged plan); project
context via the next /hex-init re-audit.

Every path this artifact names is repo-root-relative (`.agents/adrs/…`,
`hex/hex-core/…`) — the fast path's claim diff resolves paths
mechanically. Self-contained: a consumer needs no access to the source
conversation, and every file or interface it touches is named.
-->

<!--
Header contract: no schema-version marker (house rule).
State ∈ active | parked | handed-off → plan | handed-off → architect |
handed-off → context | handed-off → dropped. The vocabulary stays in
this comment: a literal option list on the State line reads as a
malformed state to every consumer that scans the discussions home.
The shipped value is `parked`, not `active`: /hex-init seeds this file
into the live discussions home, and the always-on hex-state rule fires
on `State: active` there — an `active` template would arm a repo-wide
no-edit freeze. Entry writes its own `State: active` stub; this value is
never copied into a real artifact.
-->
State: parked · Updated: [YYYY-MM-DD]
<!-- Optional: Participants: [who; turn count; date range] -->
<!-- Written by the drain itself, never by hand: Ratified: [YYYY-MM-DD] → [plan | architect | context | dropped] -->
<!-- Optional: Confidence: [who ratified; research vintages backing the decisions] -->

<!--
Lazy materialization: each section below appears on its first content —
a research result landing, a captured requirement, or an explicit user
request to capture. Entry writes the header and nothing else. Section
order is fixed; presence is not — never scaffold an empty section.
-->

## Intent

<!-- The problem in the user's words; why now; what is out of scope. -->

## Requirements

<!-- Provisional prose only — no C-/S- IDs; those originate downstream. -->

## Decisions

<!-- Working positions taken with the user. -->

## Threads

<!-- Open/closed discussion threads. -->

## Research

<!-- One entry per landed research artifact, by path. -->

## Related

<!-- Links to other ADRs, plans, discussions, and resources. -->

## Open questions

<!--
Carried, not blocking — the receiving orchestrator's docket. Entries may
carry the house marker:
`- [NEEDS CLARIFICATION: <question>] Recommended: <answer> — <reason>`
The example stays in this comment — the section is materialized on its
first real entry, never scaffolded with a placeholder one.
-->

## Verification

<!-- How the eventual work is checked, once drained. -->
