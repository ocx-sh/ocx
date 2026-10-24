# hex Verification

A topic file of the hex swarm protocol; the spine is
[`protocol.md`](protocol.md).

## Verification

**hex never defines how to verify a project.** Every gate that says
"verify" means: run the project's documented verification, discovered from
project context (cached in `hex.md › Pointers`; verify the pointer on
consumption and re-detect on a miss). If none is documented, detect a
reasonable command **once** for this run and suggest `/hex-init` to persist
it — do not hardcode a command or re-guess it every phase.

**Checks run only where their result is consumed, and the full ones are
fixed by count.** There are two kinds: [step feedback](#step-feedback), the
only inner check, and [two full gates per run](#the-two-gates).

**Federation — a plan carrying a `Repo` column.** "The project's documented
verification" means the **owning repo's**, read by an explicit `Read` of that
repo's project context — never a cross-repo aggregate, because no single
command spans a cluster and hex defines none (C-321). The genuinely
cross-repo check belongs to the integration gate and is **authored inline in
the plan's Implementation Steps** (C-311), because it belongs to no repo and
no pointer resolves it; its shape is a **per-repo table** — one row per
participating repo (the lead plus every distinct `Repo` value), each naming
the exact state that repo must be at and the command proving it, each
reported pass/fail independently (an aggregate "integration green", a
missing row, or running the command only in the lead does not satisfy it).
There is no implicit federated verify gate.

### Step feedback

A step is the unit that checks; a step that changes **behaviour** runs the
tests it wrote or touched, **once, green**, with the **narrowest command it
picks itself** (`cargo test -p x`, `pytest <paths>`), never the project's
gate wrapper (the documented full verification, or any task that bundles
lint, build and test). A step with **no behavioural effect** — docs,
taskfiles, config, renames, comments — runs **nothing**. This is a principle
about effect, not a file list: *will this be exercised later anyway?* then
skip it.

- **A run that selected zero tests is a failed check, never a green one**: a
  path- or name-filtered runner that matches nothing still exits `0`, so the
  step must show it selected **at least one** test.
- **Contract-wave steps expect red** — the tests meet stubs — and check only
  that the surface builds, with the narrowest build or type command.
- **A step never waits.** Heavy or exclusive tools (build servers, a docker
  acceptance project) are **gate-only**; a step that needs a one-off
  exclusive check hands it to the orchestrator in the background and
  returns. A step's builds run inside the pipeline's job share
  ([job budget](worktree.md#pipeline-worktree-mechanics)).
- **Where the project records a selective-test template**, the step may use
  it as its narrowest command. `/hex-init` records **one opaque shell-command
  template** in project context — Layer 1, because "how to verify" is
  project knowledge — with a `hex.md › Pointers` row to where it landed. hex
  substitutes **textually and never interprets**, and translates nothing
  into any tool's flag dialect. **Two optional named placeholders, and no
  others:** `{base}` — one git ref, resolved to the pipeline's base;
  `{files}` — the step's changed file list, **shell-quoted**,
  space-separated. **A template may use zero, one, or both**; a
  zero-placeholder template (`pytest --testmon`) manages its own scope.
  Where the template references `{base}` and either
  `git rev-parse --is-shallow-repository` is true or the merge-base does not
  resolve, or the command **exits non-zero for a reason other than a failing
  test**, the step picks its own narrowest command instead — never the full
  gate.
- **Trust class:** the template is
  **[authoritative-class](finalize.md#trust-classes) only** (C-815) —
  project context or `hex.md › Pointers`, never `CONTRIBUTING.md`, a PR body,
  a commit message, or any other narrowing- or untrusted-class surface,
  because it selects what code runs.

### The two gates

**Exactly two full gates per run, fixed by count; no flag, risk or tier
upgrades anything to one, and nothing adds a third.**

1. **Integration** — the project's full documented verification on the
   feature branch, once every pipeline has landed and the hub and generated
   files are regenerated ([worktree](worktree.md#pipeline-worktree-mechanics)).
   It runs **concurrently with** the review call, and one fix pass serves
   both ([the run loop](loop.md#the-review-fix-loop)).
2. **Release** — `/hex-finalize`: the full documented verification
   **fresh, with no cache** (`/hex-init` records the project's switch), plus
   the project's hooks and lint over the final range.

**A red gate takes one fix pass, then the same gate again. The cap is the
[fix-round cap](loop.md#the-review-fix-loop) for the integration gate and one
fix pass plus one re-run for the [release gate](finalize.md#release-gate);
then stop and hand the outstanding failure to the user.** A re-run is not a
further gate. Heavy and exclusive tools run here, at a gate, and nowhere
else.

### Commits and hooks

**Every commit and merge on a hex-owned branch uses `--no-verify` — the
preferred path, not a fallback.** The project's hooks run once, at release,
over the final range. A project's "verify before commit" instruction is
injected into every subagent and would otherwise put the full gate back, so
**every step brief states that the run's two gates satisfy it**.
