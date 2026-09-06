# hex Review Checklist

A topic file of the hex swarm protocol; the spine is
[`protocol.md`](protocol.md).

## The shipped checklist

**The single home for review criteria** (`adr_0016` C-989). Every review
brief that carries a checklist inlines sections from this file; a reviewer
never opens it — the load map gives a `reviewer` spawn `severity.md` and
nothing else ([`protocol.md`](protocol.md)'s load map),
so the **orchestrator composes the sections into the brief's `Checklist:`
slot** ([`workers/reviewer.md`](workers/reviewer.md)). Every item is
answerable from the diff and the contract excerpt alone; an item that needs
the whole tree, the plan body or a conversation is not a checklist item and
does not belong here. A seat answers **every** item it was handed —
verified with the evidence, or marked not applicable with the reason — and
its self-check refuses a return that skipped one.

## Composition

Which sections a brief carries is decided once, here, by join level
([`loop.md` § Review by join level](loop.md#review-by-join-level)):

| Level | Sections |
|---|---|
| `L0` | none — the builder's own self-check ([`workers/builder.md`](workers/builder.md)) and the mechanical evidence-table grep; a mechanical gate takes no judgement list |
| inline (a tier-`low` orchestrator reviewing its own edit) | `spec` + `quality`, answered by the orchestrator itself before it commits |
| `L1` | `spec` + `quality` |
| `L2` | per the run's `review` axis ([`hex-execute/overlays.md`](../../hex-execute/overlays.md#review-axis)): `minimal` → `spec` + `quality`; `full` → + `security` when the diff touches a security-sensitive path, + `performance` when it touches a hot path or async code, + `docs` when doc-drift triggers match; `adversarial` → + `architecture` + `pitfalls`. A `sec`, `hot` or `door` flag on any joined WP forces `security` + `performance` on at every value |
| `L3` | each `/hex-review` panel seat carries the section of its own focus; `user-feedback` when that seat fires |

Three rules sit on top of the table:

- **The project's own rules are always a section.** Every composed brief
  also names the project's quality rules and the invariants of the areas
  the diff touches, located via `hex.md › Pointers` — universal rule 1 and
  the reviewer brief's "anchor in the project's rules" line already bind
  this; the checklist makes it explicit. That, plus a project persona wired
  through `perspectives.always` ([`config.md`](config.md#perspectives)),
  is how a project extends the checklist; there is no separate project
  checklist file.
- **`review.<level>.checklist: [<focus>…]`** in `hex.md › Preferences`
  replaces the derived section set for that level's brief
  ([`config.md`](config.md#key-vocabulary), C-990). It is the only breadth
  knob `L1` has; at `L2` an explicit `--review` flag still wins under the
  ordinary later-wins precedence.
- **A `perspectives.always` rule adds its persona's checklist as one more
  section at `L2`** and a seat at `L3`, unchanged from `adr_0015` C-985.

## spec

- Every requirement ID in the excerpt is satisfied by a line in the diff —
  verify by locating each ID's evidence (a test assertion, a symbol, a doc
  line) and reading it against the ID's contract text.
- Every specification test that names an ID fails without the change and
  passes with it — verify by reading the assertion, not the test name.
- Nothing in the diff is unrequested — a behaviour, flag, endpoint or file
  no ID asked for is a finding (the convergence contract's reverse gap).
- No contradiction with the excerpt's stated invariants, error variants or
  defaults — verify each invariant the excerpt states against the code path
  that could break it.
- Public surface matches the design record: names, signatures, error
  types — verify by diffing the excerpt's surface list against the stubs.
- A partial delivery says so: a TODO, a stub body, a skipped test, or a
  deferred branch is a finding, never a note.

## quality

- Names follow the project's own conventions for this area — verify against
  a neighbouring file, not from memory.
- No duplication of a utility that already exists in the project — verify
  by grepping for the function's core operation before accepting a new
  helper.
- Tests exist for every branch the diff adds and assert behaviour, not
  implementation — verify by reading what each new test would fail on.
- No dead code, commented-out code, debug output or unused import
  introduced — verify by grep on the diff.
- Error handling is explicit where the diff can fail — no swallowed
  exception, no bare catch, no ignored return; verify each new call that
  can fail.
- The diff stays inside the WP's declared file set — a file outside it is
  a finding even when the change is right.

## security

- No secret, token, credential or key in the diff or in a test fixture —
  verify by grep for the usual markers and for high-entropy literals.
- Every input that crosses a trust boundary is validated before use —
  verify each new parser, deserializer, path join, shell or query
  construction for injection and traversal.
- Auth and authz checks are on the path the diff adds, not assumed from a
  caller — verify by tracing the new entry point to the check.
- A new dependency or lockfile change is named and justified — verify the
  manifest diff against the excerpt.
- Cryptography uses the project's existing primitives and no home-rolled
  construction — verify by grep for new crypto imports.
- Cite a standard weakness ID (CWE) where one applies.

## performance

- No allocation, I/O or lock inside a loop the diff adds on a hot path —
  verify each loop body in the diff.
- No blocking call in an async path — verify each await site and each
  synchronous call reached from one.
- No N+1 access pattern: a query or fetch inside an iteration is a finding
  unless batched — verify by reading the iteration.
- Unbounded growth is bounded: a collection, cache or queue the diff adds
  has a limit or an eviction — verify where it is written to.
- A changed complexity class is stated: an O(n²) scan replacing a lookup
  is a finding unless the excerpt accepts it.

## docs

- Every user-facing surface the diff changes — flag, command, config key,
  error message, schema — has its documentation updated in the same diff;
  verify by grep on the doc paths the project's doc-drift triggers name.
- A documented example still runs against the changed surface — verify by
  reading the example against the new signature.
- A changelog or release-note entry exists where the project keeps one —
  verify the path from project context.
- No documentation claims behaviour the diff does not implement — verify
  each doc line the diff adds against the code it describes.

## architecture

- Module boundaries and dependency direction are respected — a new import
  across a boundary the project's ADRs or rules forbid is a finding; verify
  against the named ADR, read in full.
- The diff honours every ADR covering its area — verify each decision the
  ADR states against the code path that could violate it.
- No new abstraction with a single implementation, no configuration for a
  value that never changes — verify each new interface, factory or option.
- A one-way-door change — schema, wire format, public API, storage
  layout — is named in the excerpt and has a migration or compatibility
  story; verify by reading the excerpt's reversibility note.
- A shared symbol the diff changes has every caller in the diff or
  unaffected — verify by grep for the symbol across the tree.

## pitfalls

- No known-pitfall pattern of the language or framework in the diff —
  verify against the researcher's or the project's documented pitfalls
  list, located via `hex.md › Pointers` when present.
- The algorithm or library choice is current for the problem — a superseded
  primitive or a deprecated API the project has moved off is a finding.
- Concurrency: shared state the diff touches is protected the way the
  project already protects it — verify each new access to shared state.
- Time, locale, encoding and path separators are handled the way the
  project's existing code handles them — verify each new formatting or
  parsing site.
- Resource lifetime: every handle, file, socket, or temp dir the diff opens
  is closed on every path — verify each open against its close.

## user-feedback

- Naming and wording of user-facing surfaces (CLI flags, messages, API
  names) are consistent with the project's existing ones — verify against
  a neighbouring surface.
- Every new error message says what happened and what to do — verify each
  message the diff adds for an actionable remedy.
- The change matches the project's documented product intent, located via
  `hex.md › Pointers` when present — a surface that contradicts it is a
  finding.
- Defaults are safe and unsurprising for a first-time user — verify each
  new default against the excerpt's user-experience section.
