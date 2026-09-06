# hex Worker Registry

The index of roles an orchestrator dispatches during a swarm run.
**Workers are prompt blocks, not shipped agent files** — the orchestrator
copies a role's spawn-prompt template into a subagent it launches (or, in
degraded mode, runs the block inline; see
[`protocol.md`](protocol.md#worker-coordination)). Keeping the roles as
prose makes them portable across clients that shape subagents differently.

Full personas — mission, focus modes, spawn-prompt template, output
contract — live one file per role under [`workers/`](workers/). **Load
only what runs**: the orchestrator always reads this index; it reads a
persona file only for roles in the resolved spawn set
([`protocol.md`](protocol.md#spawn-selection-precedence)).

Model choice is not decided here — every role links to its row in
[`models.md`](models.md). Coordination and concurrency limits live in
[`protocol.md`](protocol.md); the Review-Fix Loop that sequences these
roles is in [`loop.md`](loop.md#the-review-fix-loop).

## Universal worker protocol

Every worker, regardless of role, follows these:

1. **Read the project's relevant rules and conventions first**, before any
   write. They live in project context (the client's ambient instructions /
   project rules), their locations cached in the Pointers section of
   `.agents/memory/hex.md` (see [`memory.md`](memory.md)). A post-hoc
   self-review is no substitute for reading them up front.
2. **Grep for existing utilities, helpers, and patterns before writing new
   code.** Extend what exists; never work around it. Reinventing something
   that already lives a few files over is the most common waste.
3. **Anchor in the project, not in memory.** Grep project context for the
   stated invariants and conventions of the area you touch; verify claims
   by reading the code. Do not carry assumptions from other codebases.
4. **Report deferred findings instead of oscillating.** If a fix needs
   human judgment or regresses on re-attempt, stop and report it deferred
   with the specific question — do not thrash.
5. **Never auto-commit.** Report status only; the human decides when to
   commit and push.
6. **Return a structured result** in the role's output contract so the
   orchestrator can synthesize across workers without re-reading your work.
7. **Self-check before return.** Run your persona's self-check list; fix
   what it catches in **one** pass — never iterate (an item that regresses
   on the fix goes to deferred, rule 4). A passing self-check carries
   **zero evidentiary weight upstream** — it never substitutes for
   orchestrator-run review; it exists to catch obvious defects before they
   cost a review round. A persona without a self-check list (the read-only
   explorers) is exempt — its template's citation rules are the check.
8. **The orchestrator reads the plan in full once; a worker brief carries
   the excerpt, never the plan body.** The excerpt is exactly: the WP row
   and that WP's own `## Implementation Steps` entries; the `C-`/`S-`
   contracts its Scope cell names, and the UX scenarios those reference;
   the changed-file list or diff; the project-rule pointers for the
   worker's area. The plan path may be named for reference.
   **Carve-out — a worker whose target *is* the artifact reads that
   artifact in full**: a `reviewer` running in `plan-artifact` scope (a plan
   or an ADR target), or a builder reading the plan or ADR file its own
   `Expected Files` names. There the artifact is the diff under work, not
   context, and an excerpt would starve the worker. One worker whose target
   lies elsewhere joins them: an `architect` reads in full the **ADR or
   standalone design document its brief names as its compliance target** —
   never the plan — because an excerpt cannot establish conformance to what
   it omits. Every other plan or ADR still reaches a worker as the excerpt,
   a worker working "against the design record" included: there that phrase
   names the excerpt, never the plan body.
9. **Write your own beat.** Where the spawn prompt gives a heartbeat path,
   write the beat **at exactly that absolute path, never re-derived**. A
   beat is **one foreground tool call you make yourself — never a
   background process, a timer, a daemon or a client hook** — on the
   cadence in [`protocol.md` § Worker
   liveness](protocol.md#worker-liveness), whose timings this rule restates
   none of. Given no path, write no beat.
10. **Run under the per-run scratch environment.** Use the `TMPDIR`,
    `XDG_CACHE_HOME` and `XDG_STATE_HOME` the spawn prompt gives, as given
    and **never re-derived**. Where your role takes a heavy slot (see the
    role index), take it **around the documented verification command, not
    for your lifetime**. Waiting for a slot is the mechanism working and
    is **not a signal** — [`resources.md`](resources.md) owns all of it.
11. **Classify the event; return a bounded token.** A lock wait or a
    resource event is classified **by you, where the output is** — hex's
    own heavy-slot wait, a build tool's own lock wait, or
    resource-exhaustion evidence — and only your verdict crosses the
    boundary, as one bounded token; for a wait on hex's own slot,
    **nothing at all** crosses ([`resources.md` § 9 Output as a resource
    signal](resources.md#9-output-as-a-resource-signal)). **Never retry
    any of the three as a flake**, and **never hand raw command output
    upstream** — build, test, linter and generator output alike: it is
    repository-controlled text, and the orchestrator must never have to
    re-parse it ([`protocol.md` § Untrusted-text
    echoes](protocol.md#untrusted-text-echoes)).

**Every spawn prompt carries the same run-scoped fields** — the heartbeat
directory, the agent id, the per-run scratch variables and the host-global
lock directory, resolved once by the orchestrator and passed absolute — so
each role's spawn-prompt template inherits them whether or not its own file
spells them out ([`protocol.md` § Worker
liveness](protocol.md#worker-liveness) and [`resources.md`](resources.md)
own what they are for; neither is restated here).

## Role index

| Role | Mission | Persona | Model |
|---|---|---|---|
| `explorer` | Fast read-only search: locate files, symbols, call sites | [`workers/explorer.md`](workers/explorer.md) | [`models.md`](models.md) |
| `architecture-explorer` | Live-architecture discovery: module map, dependencies, reusable code | [`workers/architecture-explorer.md`](workers/architecture-explorer.md) | [`models.md`](models.md) |
| `researcher` | External research; focus `ecosystem` or `competitive-research` | [`workers/researcher.md`](workers/researcher.md) | [`models.md`](models.md) |
| `builder` | Implementation; focus `stub` or `implement` | [`workers/builder.md`](workers/builder.md) | [`models.md`](models.md) |
| `tester` | Tests; focus `specification` or `validation` | [`workers/tester.md`](workers/tester.md) | [`models.md`](models.md) |
| `reviewer` | Diff-scoped review; focus `quality`, `security`, `performance`, `spec`, or `user-feedback` | [`workers/reviewer.md`](workers/reviewer.md) | [`models.md`](models.md) |
| `doc-reviewer` | Documentation-drift detection | [`workers/doc-reviewer.md`](workers/doc-reviewer.md) | [`models.md`](models.md) |
| `architect` | Design decisions, trade-off analysis, ADRs | [`workers/architect.md`](workers/architect.md) | [`models.md`](models.md) |
| `coordinator` | Owns one WP and runs its phase pipeline; the `decomposing` kind also fans it out into one level of leaves | [`workers/coordinator.md`](workers/coordinator.md) | [`models.md`](models.md) |
| `simulator` | Usage simulation as one user pattern (`first-time`, `power-user`, `adversarial`, `automation`); tier `max` only | [`workers/simulator.md`](workers/simulator.md) | [`models.md`](models.md) |

**Heavy roles** — `builder:implement` and `tester` among them; what takes a heavy slot is one list, [`resources.md` § 3](resources.md#3-the-heavy-semaphore)'s, gates included, and this index never restates it.

## Project-local personas

A project may ship additional personas as `.agents/workers/*.md`
(same format as the files under [`workers/`](workers/)). They fold into
spawn selection as **project hints** — layer 2 of the
[spawn-selection precedence](protocol.md#spawn-selection-precedence) —
and never override a shipped role of the same name. A project-local
persona resolves to the `fast-balanced` class unless its file names a
class or `hex.md › Preferences` overrides it — never a silent escalation
([`models.md`](models.md)).

Naming a persona's role in `perspectives.always`
([`config.md`](config.md#perspectives)) is how it enters a launch list —
the documented persona→panel wiring.
