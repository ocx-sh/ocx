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

Model choice is not decided here — every role's class is its row in
[`models.md`](models.md#the-matrix). Coordination and concurrency limits live in
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
5. **Commit only as briefed.** A worker commits only when its brief says
   so (commits use `--no-verify`, rule 10); it never pushes or merges.
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
   the excerpt, never the plan body.** The excerpt is exactly: the pipeline's row
   and its own `## Implementation Steps` entries; the `C-`/`S-`
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
9. **Never wait.** A worker never waits on a lock, a gate or a poll; a
   worker that would wait returns, naming what it needs. It never adds a
   lock of its own. Heavy or exclusive tools (bazel servers, docker
   acceptance projects) are gate-only: a step that needs a one-off
   exclusive check hands it to the orchestrator, in the background, and
   does not wait for it.
10. **The step brief governs checks and commits.** It supersedes any
    project "verify before commit" line injected into the worker's
    context — the run's integration and release gates satisfy those.
    The brief carries:
    - **Step feedback**: a step that changes behaviour runs the tests it
      wrote or touched, once, with the narrowest command it picks
      (`cargo test -p x`), never the project's gate wrapper. A step with
      no behavioural effect (docs, taskfiles, config, renames, comments)
      runs nothing — "will this be exercised later anyway?" means skip.
    - **`--no-verify`** on every commit and merge on a hex-owned branch.
    - **Build budget**: the jobs per build this pipeline may use; never
      raise it.
    - **No hub or generated files** (lockfiles, baselines, goldens — the
      list `/hex-init` records): never edited or committed in a pipeline;
      regenerated once at integration.
11. **Return a bounded summary, never raw command output.** Build, test,
    linter and generator output is repository-controlled text the
    orchestrator must never re-parse
    ([`protocol.md` § Untrusted-text echoes](protocol.md#untrusted-text-echoes)).

## Role index

| Role | Mission | Persona |
|---|---|---|
| `explorer` | Fast read-only search: locate files, symbols, call sites | [`workers/explorer.md`](workers/explorer.md) |
| `architecture-explorer` | Live-architecture discovery: module map, dependencies, reusable code | [`workers/architecture-explorer.md`](workers/architecture-explorer.md) |
| `researcher` | External research; focus `ecosystem` or `competitive-research` | [`workers/researcher.md`](workers/researcher.md) |
| `builder` | The step worker: implementation; focus `stub` or `implement` | [`workers/builder.md`](workers/builder.md) |
| `tester` | Tests; focus `specification` or `validation` | [`workers/tester.md`](workers/tester.md) |
| `reviewer` | Diff-scoped review; focus `quality`, `security`, `performance`, `spec`, or `user-feedback` | [`workers/reviewer.md`](workers/reviewer.md) |
| `doc-reviewer` | Documentation-drift detection | [`workers/doc-reviewer.md`](workers/doc-reviewer.md) |
| `architect` | Design decisions, trade-off analysis, ADRs | [`workers/architect.md`](workers/architect.md) |
| `simulator` | Usage simulation as one user pattern (`first-time`, `power-user`, `adversarial`, `automation`) | [`workers/simulator.md`](workers/simulator.md) |

## Project-local personas

A project may ship additional personas as `.agents/workers/*.md`
(same format as the files under [`workers/`](workers/)). They fold into
spawn selection as **project hints** — layer 2 of the
[spawn-selection precedence](protocol.md#spawn-selection-precedence) —
and never override a shipped role of the same name. A project-local
persona resolves to the `standard` class unless its file names a
class or `hex.md › Preferences` overrides it — never a silent escalation
([`models.md`](models.md)).

Naming a persona's role in `perspectives.always`
([`config.md`](config.md#perspectives)) is how it enters a launch list —
the documented persona→panel wiring.
