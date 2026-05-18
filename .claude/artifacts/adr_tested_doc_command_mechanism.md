# ADR: Tested Website Command-Example Mechanism + CLI-Doc Drift Fix

> Status: Accepted (design record — no implementation in this ADR)
> Date: 2026-05-17 (Decision H appended 2026-05-17 — display-render + region unification)
> Tier: high · Reversibility: Medium (pre-1.0; CLAUDE.md permits rewrite of unreleased surfaces)
> Supersedes: nothing · Related: `design_spec_doc_command_scripts.md`,
> `research_vitepress_transclusion_cast_cost.md`,
> `.claude/state/plans/meta-plan_tested-doc-commands.md`

## 1. Context

OCX website prose pages (`getting-started.md`, `user-guide.md`, `faq.md`,
`in-depth/environments.md`, `in-depth/entry-points.md`) embed `ocx …` command
examples as inline code strings. None are executed. The CLI taxonomy changed
substantially (`shell env/hook/init` removed, `ci export` removed,
`install/exec/which` moved under `ocx package …`, `find→which`, `info→about`,
`update→upgrade`, per-command `--global` collapsed to a single root-only flag —
see recent commits on `hardening`). Roughly 31 prose command references are now
stale. Nothing fails when a documented command no longer exists, so drift ships.

A tested example mechanism already exists for *visual demos*: the recordings
pipeline (`test/recordings/`) executes `.sh` scripts through a PTY against the
real `registry:2`, sanitises output, and writes asciicast `.cast` files consumed
by the `<Terminal>` Vue component. It runs only in `task website:build`, never
in `task verify`. A second harness, the Scenario harness
(`test/src/scenarios/`), runs `.sh` scripts as subprocess acceptance tests with
rich `$PKG_*`/`$MARKER_*` env and bash assertions; it *is* in `task verify` via
`test_scenarios_smoke.py` → `test:parallel`.

The release-hardening goal: make every documented command an executed,
drift-gated example, without coupling website page layout to the test directory
tree, and without building a parallel doctest system.

## 2. Locked Tenets (accepted constraints — not re-litigated here)

1. **Single source of truth** = tested `.sh` scripts under `test/`. Extend
   existing infra; no parallel doctest engine.
2. **Decouple website ↔ test source layout.** Doc page structure must NOT mirror
   the test directory tree. Page↔script binding is carried by *script metadata*,
   not directory parallelism. Exactly one explicit, declared publish step bridges
   `test/` → `website/`.
3. **Cast generation is OPTIONAL per script**, default off.
4. **Audit + fix all stale prose commands** to the current taxonomy; a green
   suite gates drift for release.
5. **Coverage** target = prose walkthrough pages. `command-line.md` keeps its
   existing structural gate (`test_doc_command_reference.py`) and is not in
   scope for execution.

These are inputs, not decisions. The ADR records the remaining open decisions:
(A) verify-gate placement, (B) the canonical executor + unified header schema,
(D) the publish/transclusion boundary. (C) is a rationale correction.

## 3. Decision A — Verify-Gate Placement (resolved, no options needed)

Drift gating must run wherever `task verify` runs. Per
`research_vitepress_transclusion_cast_cost.md` and the verify chain
(`taskfile.yml` `.verify:build-test` → `test:parallel`), recordings run only in
`website:build`; the acceptance path runs in verify.

**Decision:** Every doc script is **first an acceptance-tested script** collected
by the `test:parallel` path (same path as `test_scenarios_smoke.py`). This is
the drift gate. **Cast generation is an additive, opt-in layer** that the
existing recordings pipeline (`website:recordings:parallel`, inside
`website:build`) applies only to scripts that opt in via `# cast: true`.

Consequence: a stale documented command fails `task verify` (red acceptance
case), independent of whether the website is ever built. Cast/GIF cost stays out
of the per-commit loop.

## 4. Decision B — Canonical Executor + Unified Header Schema

### Problem

Two incompatible executors exist today:

| Axis | Recordings PTY runner (`test/recordings/`) | Scenario harness (`test/src/scenarios/`) |
|---|---|---|
| Header | `# setup: <name>` → `SETUPS` func | `# scenario: <Name>` → `Scenario` subclass |
| Execution | persistent bash in a PTY, per-command | one `bash -c <body>` subprocess |
| Env to script | none (commands rewritten, no `$VAR`) | rich `$PKG_*/$FQ_*/$MARKER_*/$OCX/$REGISTRY` |
| Assertion model | exit-code only (rc≠0 ⇒ AssertionError) | exit-code only, but bash `[[ … ]] || exit 1` idiomatic |
| Output handling | sanitised, digest-truncated, written to `.cast` | captured, surfaced on failure |
| In `task verify`? | **No** (only `website:build`) | **Yes** (`test_scenarios_smoke.py`) |
| Produces cast? | Yes | No |

A doc script must run as (a) an acceptance test always, (b) an optional cast.
One script body + header must satisfy both.

### Option B1 — Canonical = Scenario harness; recordings consumes the same scripts

The Scenario harness is the *primary* executor (it is already in verify, has
rich env, bash assertions, registered pre-publish state). Unify the header
schema so a single `.sh` declares its pre-publish state once. The recordings PTY
runner is refactored to *consume the same scripts and the same state registry*
when `# cast: true`, replaying the command lines through its PTY for visual
fidelity.

- Registry unification: merge `SETUPS` (function registry) and `SCENARIOS`
  (subclass registry) into a single named **state-provider registry**. A
  provider yields published `PackageInfo` keyed by display name (recordings'
  contract) *and* exposes the `$PKG_*` env projection (Scenario's contract).
  Both consumers read the same provider by name.
- Drift gate: scripts collected by a new acceptance case (sibling of
  `test_scenarios_smoke.py`) on the verify path.
- Cast: recordings runner, given the same script + provider, replays lines in
  its PTY, sanitises, writes `.cast`. Only for `# cast: true`.

| Criterion | Weight | Score (1–5) | Note |
|---|---|---|---|
| Already in verify | 0.30 | 5 | Scenario harness is the verify-path executor today |
| Rich, debuggable failures | 0.20 | 5 | bash assertions, stdout/stderr surfaced |
| Cast fidelity preserved | 0.20 | 4 | PTY replay path retained for opt-in scripts |
| Registry unification cost | 0.15 | 3 | two registries → one provider abstraction; non-trivial but bounded |
| Migration effort (22 + 31) | 0.15 | 3 | recordings scripts must adopt provider names; prose refs rewritten |
| **Weighted** | | **4.15** | |

Risk: the recordings runner currently rewrites display names → UUID repos via
`repo_map`; a unified provider must still expose that mapping. Mitigation: the
provider contract includes the display→actual map (recordings already derive it
from `setup_env`).

Reversibility: Medium. The unified registry is internal to `test/`; doc-facing
surface is only the header keys.

### Option B2 — Canonical = Recordings PTY runner; promote it onto the verify path

Keep the PTY runner as the single executor; move its collection onto the verify
path (run `test/recordings/` without cast write in `test:parallel`, with cast
write in `website:build`). Retire the Scenario harness for doc scripts.

| Criterion | Weight | Score (1–5) | Note |
|---|---|---|---|
| Already in verify | 0.30 | 2 | recordings path is NOT in verify; must be added |
| Rich, debuggable failures | 0.20 | 2 | exit-code only, no `$VAR`, no bash assertion idiom |
| Cast fidelity preserved | 0.20 | 5 | native to this runner |
| Registry unification cost | 0.15 | 4 | single registry kept (`SETUPS`) |
| Migration effort | 0.15 | 2 | Scenario-based scripts/tests lose rich env; rewrite assertions |
| **Weighted** | | **2.75** | |

Risk: doc scripts could only assert exit codes, not behaviour
(`[[ "$out" == … ]]`). Stale-command detection still works (command absent ⇒
nonzero), but content drift (changed output, wrong flag still parsing) is far
weaker. Pulling the full PTY+Docker recordings cost into `task verify` directly
contradicts the cost lever identified in research (Axis 2 §"Real cost lever").

### Option B1′ — Canonical = Scenario harness; **adapter, no registry merge**

As B1 (Scenario harness canonical, recordings additive opt-in cast), but the two
legacy registries are **not merged**. Introduce one thin `StateProvider`
Protocol with two adapters: `SetupAdapter` wraps a legacy `SETUPS` function;
`ScenarioAdapter` wraps a legacy `Scenario` subclass. Both consumers depend on
the Protocol only. State is selected by an **explicit family prefix**:
`setup:<name>` (recordings family) or `scenario:<Name>` (scenario family). No
implicit union, no collision possible by construction — `setup:basic` is the
recordings `SETUPS['basic']`; the scenario family is keyed by PascalCase class
names (`BasicPackage`…) and has **no** `basic` key. (Codex finding 1: an earlier
draft assumed lowercase scenario names + a `basic` collision — both wrong
against the live `SCENARIOS` registry; explicit-family resolution removes the
problem entirely.)

| Criterion | Weight | Score (1–5) | Note |
|---|---|---|---|
| Already in verify | 0.30 | 5 | Scenario harness is the verify-path executor today |
| Rich, debuggable failures | 0.20 | 5 | bash assertions, stdout/stderr surfaced |
| Cast fidelity preserved | 0.20 | 4 | PTY replay path retained for opt-in scripts |
| Registry-change cost | 0.15 | 5 | **no merge** — Protocol + 2 adapters; legacy untouched, behaviour-preserving |
| Migration effort (22 + 31) | 0.15 | 4 | headers only; no `basic` union, no behaviour merge |
| **Weighted** | | **4.70** | |

### Decision B (chosen): **Option B1′**

Canonical executor for tested-only doc scripts = the **Scenario harness**
(verify-path, rich env, bash assertions). Cast is an additive PTY replay layer
keyed off `# cast: true`, served by the existing recordings pipeline inside
`website:build`. The two legacy state registries are **adapted, not merged**,
behind a single `StateProvider` Protocol so one script declares its pre-publish
state once and both consumers read it by name — without rewriting or merging the
live registries during hardening.

Rationale: Decision A requires the drift gate on the verify path; the Scenario
harness is already there with behavioural assertions. B2 re-imports the
Docker+PTY+cast cost research says must stay out of the per-commit loop. Merging
the registries (B1) is a behaviour change to shared test infra mid-hardening
with an uncharacterized `basic` union — a Two-Hats violation per
`workflow-refactor.md`. B1′ adapts instead of merging: same doc-facing contract
(one script body + header, two consumers), **zero behaviour change** to the
existing acceptance suite, collision dissolved. Cost: two adapter classes vs one
merged registry — the right call mid-hardening.

**SP0 (lazy-provisioning invariant):** provider registration + module import
perform **no registry I/O**; provisioning (OCI pushes) occurs only when a
provider is invoked with live fixtures. Enforced by an import-time zero-network
test. Without it, a future provider could move `make_package` into `__init__`
and silently re-import Docker cost into `task verify` — the one-way door
Decision A closes.

## 5. Decision C — Corrected Cast-Cost Rationale (rationale, not a choice)

The meta-plan's tenet #3 rationale ("recordings cost CPU even parallelized";
"cast generation optional because tested-only is cheaper") is **factually
wrong at the per-test level** and is corrected here:

- Skipping the `.cast` write saves ≈ 1 ms. The dominant cost — real OCI pushes
  to `registry:2` plus real `ocx` command execution — is incurred *whether or
  not* a cast is produced, because the script must run to gate drift.
- Therefore "tested-only is cheaper because it skips the cast" is false.
- **The true justification for cast-optionality:** (1) not every documented
  snippet is a visual demo — most are correctness checks; (2) the cast→GIF
  conversion (`agg`/gifski) and the human visual-review burden are real,
  recurring costs; (3) GIF artifact churn pollutes diffs. Cast generation is
  opt-in because *producing a reviewable visual asset* is expensive and
  selective, not because *running the script* is.
- **The real cost lever is pipeline placement** (Decision A), not per-script
  cast skipping: keep the Docker+PTY+cast chain in `website:build`, keep the
  cheap acceptance run in `task verify`.

## 6. Decision D — Publish / Transclusion Boundary

### Constraints (from research Axis 1)

- VitePress `<<<` is **build-time** and can only read files **under
  `website/src/`** (`@` = `website/src/`). Scripts authored in `test/` are
  outside `srcDir` → **not transcludable**.
- A build step must copy scripts into `website/src/`. `website/src/_scripts/`
  (underscore prefix ⇒ VitePress does not route it as a page) is the
  transclusion-only home; `<<< @/_scripts/<…>` binds the doc to the *published
  copy*, never the test path.
- The published path must encode page binding **without mirroring the test
  directory layout** (tenet #2).

### Option D1 — Flat namespace derived from a `# doc:` metadata key

Each script declares `# doc: <slug>` (e.g. `# doc: getting-started/install`).
The publish task writes a single flat copy to
`website/src/_scripts/<slug>.sh`, where `<slug>` is derived only from the
metadata, never from the script's path under `test/`. Docs transclude
`<<< @/_scripts/getting-started/install.sh`. The test tree may reorganise
freely; only the `# doc:` value is contractual.

| Criterion | Weight | Score | Note |
|---|---|---|---|
| Test↔website layout independence | 0.35 | 5 | published path is metadata-derived, not path-derived |
| Single declared seam | 0.20 | 5 | one publish task owns the bridge (precedent: `recordings.taskfile.yml`) |
| Author ergonomics | 0.20 | 4 | one `# doc:` line; transclusion path is predictable from the slug |
| Collision safety | 0.15 | 4 | duplicate `# doc:` slug = hard error in publish task |
| Reversibility | 0.10 | 3 | docs cite `_scripts/<slug>`; renaming a slug is a cheap pre-1.0 sweep |
| **Weighted** | | **4.45** | |

### Option D2 — Mirror the test path under `_scripts/`

Publish copies `test/<path>/foo.sh` → `website/src/_scripts/<path>/foo.sh`,
1:1. Docs transclude the mirrored path.

| Criterion | Weight | Score | Note |
|---|---|---|---|
| Test↔website layout independence | 0.35 | 1 | **violates tenet #2** — website paths now encode test tree |
| Single declared seam | 0.20 | 4 | still one task, but the seam leaks layout |
| Author ergonomics | 0.20 | 3 | must know test tree to write a `<<<` |
| Collision safety | 0.15 | 5 | path uniqueness inherited from filesystem |
| Reversibility | 0.10 | 1 | any test-tree refactor breaks every citing doc |
| **Weighted** | | **2.15** | |

### Decision D (chosen): **Option D1**

Binding is via a `# doc: <slug>` header key. The publish task writes a
**nested, metadata-derived path** at `website/src/_scripts/<slug>.sh`, where
the slug's `/` is the **directory separator** (e.g. `# doc:
getting-started/install` → `website/src/_scripts/getting-started/install.sh`,
transcluded `<<< @/_scripts/getting-started/install.sh{sh}`).

**Living Design Record (2026-05-17, user request):** the original D1 wrote a
*flat* name with `/`→`__` escaping (`getting-started__install.sh`). Replaced
with the nested path: it is markedly more readable in prose/markdown `<<<`
references and in the `_scripts/` tree, and removes the `__`-escaping
machinery and its "slug grammar must exclude `_`" injectivity caveat
entirely. **Tenet #2 still holds:** the directory structure derives from the
`# doc:` *slug* (page-author metadata), **not** from the script's path under
`test/` — the test tree may still reorganise freely; only the slug is
contractual. slug→path is trivially injective (distinct slugs ⇒ distinct
paths; the slug grammar `^[a-z0-9]+(?:[-/][a-z0-9]+)*$` maps 1:1 to a
relative path with no escaping). The orphan sweep stays manifest-scoped and
additionally prunes now-empty slug directories it owns.

**Seam contract (PT6 — was aspirational, now specified):** the cited precedent
`recordings.taskfile.yml` actually *does* hardcode `test/` paths and
`test/recordings/conftest.py` hardcodes the `website/src/public/casts/` path —
the existing bridge violates tenet #2 in **both** directions. The new design
does not inherit that. The **test subsystem exposes a discovery entrypoint**
(e.g. `task test:doc-scripts:list` emitting JSON `[{path, slug, cast}]`); the
website-owned publish task consumes that JSON only — no `test/` glob in any
`website/` file. Symmetrically, the website-owned publish task is the **sole
writer** of `website/src/_scripts/`. The pre-existing reverse leak (`test/`
hardcoding the `casts/` path) is **in scope**: parameterize the cast output
directory so `test/` no longer hardcodes `website/`. If any part of that leak
fix is deferred, the plan must say so explicitly rather than imply a clean seam.

**Orphan-sweep scoping (PT5):** the publish task maintains a manifest
(`website/src/_scripts/.published.json`) of task-owned files. The orphan sweep
removes **only** task-owned `*.sh` files no longer backed by a current slug. It
MUST NOT delete foreign files, subdirectories, or anything absent from the
manifest. `_scripts/` is shared filesystem space under `website/src/`; an
unscoped "delete everything not mine" sweep on a shared dir is destructive and
undefended — the manifest bounds the blast radius.

## 6b. Decision E — Optional Golden-Output Assertion

Exit-code + bash `[[ ]]` assertions miss a real drift class: a renamed
subcommand that still exits 0 with different output, a dropped `--format json`
field, changed error wording. The whole point is *drift* detection. SOTA tested-
doc tools (tesh, mdtest, Rust doctests, VHS `.txt` goldens) pin output and diff.

**Decision:** add an **optional** `# expect:` capability (a golden-output file
or inline expectation the script diffs against). Optional, not mandatory —
opt-in for high-value snippets (`ocx about`, `ocx package which`, `--format
json` shapes); most scripts stay assertion-only to avoid brittle full-output
pinning. Captured stdout/stderr is ANSI-stripped before diffing and before
inclusion in failure messages (CI logs stay readable; goldens stay stable).
Detailed contract in `design_spec_doc_command_scripts.md`.

## 6c. Decision F — Doc-binding gated on the verify path (Codex finding 2)

The stated goal is "green `task verify` ⇒ documented commands drift-gated". A
slug typo or page-rename that leaves a `<<<` reference pointing at no published
script must therefore also fail `task verify` — **not** be deferred to
`website:build`. **Decision:** a static verify-path check (NC2) cross-checks
every `<<< @/_scripts/…` reference in the walkthrough pages against the publish
export/manifest, in the same `test:parallel` collection as the drift gate. No
website build, no Docker. Binding correctness is gated together with command
execution. (Earlier plan deferred this; promoting it is required for the stated
guarantee to hold.)

## 6d. Decision G — Cast region marker (Codex finding 3)

The recordings PTY recorder replays **one command per line**, prompt-wait
between — it cannot run a full bash body (`set -euo pipefail`, multiline
control flow, `[[ … ]]` assertions) without hanging or leaking test-only
commands into the demo. **Decision:** a `# cast: true` script demarcates its
demo lines with a single `# region cast` / `# endregion cast` block. The drift
gate runs the **whole** script (assertions included); the cast layer replays
**only** the region. The region is a *display* selector, not an execution
selector — full behavioural assertion power is retained while casts stay clean.

## 6e. Decision H — Display-Render Layer + Region Unification (2026-05-17)

> Added 2026-05-17 after `research_doc_script_display_render.md`. One-Way Door
> Medium (pre-1.0; header-schema + region-grammar cutover, no compat shim per
> §8). Extends Decisions D, E, F, G. Supersedes the §7.1/§7.2 framing that
> treated doc_scripts and recordings as two script families with independent
> headers — the region grammar is now **shared** across both (H-3) and the
> two trees converge (H-4).

### Context

Decision D publishes `test/doc_scripts/*.sh` **verbatim** to
`website/src/_scripts/<slug>.sh`; the prose transcludes `<<< @/_scripts/<slug>.sh{sh}`
(whole file). The reader therefore sees the shebang, the `# state:`/`# doc:`/
`# title:`/`# description:`/`# cast:` metadata header, `set -euo pipefail`, the
`# region cast` markers, the post-region assertion scaffolding, and **literal
`$PKG_UV` / `${PKG_CMAKE}`** tokens that are never expanded (they only exist at
runtime under a provisioned `StateProvider`, and SP7 makes the actual repo a
UUID-prefixed name no reader should ever see). The published example is not the
example a reader would type.

Decision G already established the `# region cast` block as a *display*
selector, not an execution selector (the drift gate runs the whole body; the
cast replays only the region). Research Axis 1 establishes — verified against
`vuejs/vitepress` `snippet.ts` `findRegion()` — that the VitePress shell region
regex is `#\s*#?region` / `#\s*#?endregion`, so **`# region cast` is already a
native VitePress region selector**: `<<< @/_scripts/<slug>.sh#cast{sh}` renders
only the region body, marker lines auto-stripped, with **zero new grammar**.
One marker now serves three projections of one source:

1. **Tested** — drift gate runs the *full body* under the real provisioned env
   (Decision A/B1′; SP0–SP8 untouched).
2. **Published display** — VitePress renders the *region body only* (or
   full-body-minus-header when no region), with `$VAR` expanded to canonical
   display values.
3. **Cast replay** — recordings PTY replays the *region lines only* (Decision G,
   CA5; unchanged).

> **LDR 2026-05-18 — Sub-decision H-5: RN6 disclaimer removed; honest
> guarantee = DE6-canonical equivalence (NOT execute-the-rendered-text).**
> User-reported smell: the published snippet carried a `# Rendered for display
> … not the tested source.` disclaimer because render ran *only at publish*,
> separate from the drift gate (which ran the raw body). The first attempt —
> have the drift gate execute `substitute_renderable(body, declared_display_env())`
> so displayed == executed byte-for-byte — was implemented then **reverted at
> the Phase-6 verify gate**: it is **incompatible with SP7 parallel
> isolation**. `provision()` pushes to an SP7-prefixed repo
> (`t_<8hex>_<repo>`); only the SP7-prefixed `$PKG_*`/`$REPO_*` in `script_env`
> resolves against the registry. Substituting to the clean display short
> (`webapp:2.0.0`) makes the executed command fail `package not found`
> (verified: `deps`/`deps-flat`/`deps-why`). SP7 cannot be dropped (drift gate
> runs `-n auto`; serialising it was explicitly rejected — SP7 LDR). **Final
> resolution:** the drift gate keeps running the **raw** body under
> `script_env` (H-1 unchanged). What changes: (a) the RN6 disclaimer is
> **deleted**; (b) RN2 also blank-trims (the header-terminating blank no longer
> leaks). The honest, gated guarantee replacing the disclaimer is
> **DE6-canonical equivalence**: the display artifact is *this exact source*
> rendered (RN1/RN2 + RN8 with the declared values); the drift gate proves the
> source runs green; DE6 gates `declared == canonical(provisioned)` (SP7 prefix
> stripped); the publish substring + RN8-parity tests prove the display is a
> faithful slice. So the displayed command **is** a tested command, differing
> from the executed one only by the SP7 isolation prefix DE6 canonicalises —
> a precise relationship, not a weaker byte-identity SP7 forbids. RN8 (single
> substitution source) and the renderable-only constraint stand; only the
> "drift gate executes the rendered text" sub-claim is withdrawn.

### Sub-decision H-1 — Render mechanism

The unsolved half (research Axis 2: no prior-art tool both splits run/display
*and* substitutes a fixture variable for a clean display token) is the variable
problem. `$PKG_UV` must appear in the rendered page as `uv`, not as the literal
`$PKG_UV` and never as the UUID-prefixed actual repo.

#### Option H1 — VitePress-native region + R1 literal canonical values

Authoring constraint (research R1): cast-region lines use *literal* canonical
values (`ocx package install --select uv:0.10`); `$PKG_*` is allowed only
*outside* the region (assertions). Display = region body verbatim; no render
pass, no seam change. Header/shebang/`set -euo pipefail` strip is still needed
because the region excludes them natively — actually H1 needs **no strip engine
at all** for region-bearing scripts (the region already excludes the header).

| Criterion | Weight | Score (1–5) | Note |
|---|---|---|---|
| SP7 compatibility (parallel-isolated drift gate) | 0.30 | 1 | **infeasible**: the drift gate runs the cast-region body too, under `-n auto` with UUID-prefixed repos (SP7); a literal `uv:0.10` in an *executed* region line does not resolve to the provisioned `t_<8hex>_uv` repo — every `$PKG_*` walkthrough breaks |
| Implementation cost | 0.20 | 5 | none — pure authoring rule |
| Display fidelity | 0.20 | 4 | clean, but only for the rare no-`$PKG_*` script |
| SP0 preserved | 0.15 | 5 | no seam, no accessor |
| Author ergonomics | 0.15 | 2 | author must hand-maintain literal/var split and keep literals matching the fixture forever (silent drift class) |
| **Weighted** | | **2.85** | |

R1 was the researcher's recommendation but is dissolved by SP7: doc scripts
**deliberately** use `$PKG_*`/`$REPO_*` projected vars precisely because the
drift gate runs in `test:parallel` against UUID-prefixed repos (memory:
"Doc scripts must use `$PKG_*`/`$REPO_*` projected vars, never hardcoded repo
names"; ADR SP7). A literal repo name in an executed region line cannot resolve.
H1 is viable only for the empty-intersection set of displayed scripts that touch
no provisioned package — not a general mechanism.

#### Option H2 — VitePress-native region + R2 static-seam expansion + header strip (chosen)

Publish-time render pass over each published `.sh`:

- **Region present** → emit region body only (the VitePress `#cast` selector
  does this natively; the render pass additionally produces a strip-and-expand
  copy so the rendered text is the *displayable* example, see H-2 for why a
  render pass and not raw `#cast` alone).
- **Region absent** → emit full body minus shebang, minus the `# key:` metadata
  header block, minus a leading `set -euo pipefail`.
- In both cases, expand `$VAR` / `${VAR}` / `"$VAR"` / `"${VAR}"` using a
  `display_env` map carried through the **existing JSON seam**
  (`task test:doc-scripts:list`), derived from a NEW **static, zero-I/O**
  `StateProvider` accessor over **declared** package short-names (H-2).

| Criterion | Weight | Score (1–5) | Note |
|---|---|---|---|
| SP7 compatibility (parallel-isolated drift gate) | 0.30 | 5 | tested body untouched — keeps `$PKG_*`; expansion is publish-time only, never executed; SP7 invariant intact |
| Implementation cost | 0.20 | 3 | one render pass + one static accessor + seam-schema extension + parity gate |
| Display fidelity | 0.20 | 5 | reader sees `ocx package install --select uv:0.10`, no header, no scaffolding |
| SP0 preserved | 0.15 | 5 | accessor is static over *declared* names (no `provision()`, no registry I/O); enforced by an import/no-network test like SP0 |
| Author ergonomics | 0.15 | 5 | author writes one script with `$PKG_*` everywhere; render is automatic |
| **Weighted** | | **4.45** | |

### Decision H-1 (chosen): **Option H2**

The render layer is **publish-time**. Tested execution semantics are
**unchanged** (Decisions A/B1′; SP0–SP8 preserved verbatim). The researcher's
two R2 objections are dissolved, not ignored:

- *"Couples StateProvider → website (PT6)"* → the website never imports `test/`;
  `display_env` rides the existing `task test:doc-scripts:list` JSON seam
  (DE1–DE3). PT6 strengthened, not weakened.
- *"Publish must not provision (SP0)"* → the canonical short name (`uv:0.10`,
  `cmake`) is **declared static metadata**; only the UUID prefix is runtime
  (SP7). A new **static** `StateProvider.declared_display_env()` (DE2) derives
  `{PKG_UV: "uv:0.10", …}` from declared names with **no `provision()`, no
  registry I/O**. SP0 preserved and re-enforced by a dedicated import/no-network
  test (DE4). **Note (load-bearing, surfaced for Phase-1 builder):** the legacy
  `SETUPS` functions and `Scenario.setup()` bodies encode display names as
  *return-dict keys / `self.publish()` call args* — not extractable without
  executing the function. H2 therefore requires each provider to expose an
  **explicit declared-names surface** (a static class/function attribute, e.g.
  `DECLARED_PACKAGES: dict[str, str]` = `{display_key: short_ref}`), authored
  alongside the setup. This is new static metadata the migration must add per
  provider; it is the SP0-safe source for `declared_display_env()`. The design
  spec specifies the contract (DE2); the exact attribute mechanism is a Phase-1
  builder decision constrained by DE2/DE4. **Guarantee scope (F4, honest):**
  `DECLARED_PACKAGES` can hold a *stale value* (e.g. `multi_version` keys
  `$PKG_CORRETTO` to `versions[0].short` — a "first version wins" ordering fact
  encoded in the setup *body*, not in any declarable name; add a version at
  index 0 and the runtime `$PKG_*` value changes while the declared value does
  not). RN5 does **not** catch this (it fires on an *unknown* var, not a *known
  var with a stale value*). DE4 (static, zero-I/O) proves SP0 but **cannot**
  prove declared==actual. The **provisioned cross-check (DE6)** does — and DE6
  is a normal `test:parallel` case in the *same* collection as the drift gate,
  which already provisions `registry:2` (Decision A). `task verify` phase-2
  runs that provisioned collection, so **DE6 runs under `task verify`** as the
  mandatory gate (Codex F1) — it is **not** an optional side-suite. The honest
  scope: the *pure-static subset alone* would prove only seam-shape + SP0, but
  `task verify` is not static-only — it runs the provisioned suite, so a green
  `task verify` **does** gate value-correctness. DE6 must be a collected case
  with no `skip`/opt-in marker so it cannot be silently excluded.

### Sub-decision H-2 — Why a render pass, not raw VitePress `#cast` alone

Raw `<<< @/_scripts/<slug>.sh#cast{sh}` natively strips the header and selects
the region for **free** (research Axis 1, verified). It does **not** expand
`$PKG_*`. A reader of a region that contains `ocx package install --select
"$PKG_UV"` would see the literal `$PKG_UV`. The variable problem (research Axis 2)
is unsolved by the native selector. Therefore the publish task emits a
**rendered copy** (strip + region-select + `display_env` expansion) and the
prose transcludes the *whole rendered file* (`<<< @/_scripts/<slug>.sh{sh}` — no
`#region`), because the rendered file already **is** the region body. This keeps
the silent-fallback trap (research Axis 1; GH #4625: a `<<<` to a *missing*
region returns the whole file with no error) **off the doc-author surface**: the
prose never names a region, so a region-name typo cannot dump the full script
into a page. The trap is instead handled inside the publish task and guarded by
NC4 (region-existence verify-path gate).

**Alternative considered and rejected (F6):** a VitePress-render-time
substitution — `bash-vue`/`sh-vue` fences with `{{ PKG_UV }}` interpolation, or
a custom markdown-it transformer. Rejected on two grounds: (1) verified
inapplicable — `lang-vue` interpolation does not work for `<<<` snippet imports
(`snippet.ts` applies `v-pre` to imported blocks; GH vuejs/vitepress #471,
#3875), so it cannot substitute into transcluded scripts at all; (2) even if it
worked it would require the website layer to know `display_env` at build time
(`VITE_*` env), recreating the PT6 coupling H2 avoids, and spends a
Vue/markdown-plugin innovation token versus a pure-Python text pass (KISS).
Recorded so this is not silently reconsidered.

### Sub-decision H-3 — Region-grammar unification: reuse `# region cast`

**Decision:** the single display+cast region marker is **`# region cast` /
`# endregion cast`** — no second grammar. It is already mandatory for
`# cast: true` (§1.3, EX9) and is a native VitePress region (Axis 1). For a
**display-only, no-cast** multi-step script the marker is **optional**: absent ⇒
the render pass falls back to full-body-minus-header (H2 region-absent rule), so
a generic `# region doc` alias buys nothing and adds a second grammar plus a
second silent-fallback surface. Rejected: introduce `# region doc`. Rationale:
KISS / one marker name; the full-body-minus-header fallback covers the
display-only case without new grammar. If a real display-only-needs-narrowing
case appears post-hardening (a script whose displayed slice ≠ full body ≠ a cast
region), revisit — YAGNI until then. The marker name `cast` is a known minor
misnomer for the no-cast display case; documented in RG2, not worth a rename
churn pre-1.0.

### Sub-decision H-4 — Codex Finding 1 / two-tree drift: one-tree convergence

`test/doc_scripts/` (33 scripts, drift-gated) and `test/recordings/scripts/`
(21 scripts) are two trees; **~8 slugs collide outright today** (verified live:
e.g. `getting-started/install-select` exists in *both* trees, both already
declaring `# state: setup:multi-version`) and can silently diverge (Codex
Finding 1; memory `project_doc_cast_two_tree_drift`).

**Premise correction (Round-1 review, F1):** an earlier draft (and the stale
`test/recordings/conftest.py:12` comment) asserted the 21 recordings scripts
still carry the *legacy* `# setup:` header and are Phase-5-pending. Live
evidence (all 21 verified) shows they **already carry the unified
`# state:`/`# cast:`/`# doc:`/`# region cast` header** and are parsed by the
new `src.doc_scripts.parse_doc_header`. The convergence work is therefore
**not** a header rewrite — it is a physical move + **slug deduplication** +
`<Terminal src>` repoint + recordings-discovery-to-seam switch. PT4
(duplicate-slug ⇒ publish hard-fails) means the collision is a **live blocker
the moment both trees feed the seam**, not a future divergence risk.

Two resolutions:

- **H-4a One-tree convergence (chosen):** recordings scripts **move into**
  `test/doc_scripts/`; the ~8 colliding slugs are deduplicated to one converged
  script per slug (the asserted doc-script body is canonical; the recordings
  demo lines become its `# region cast`, with any literal repo refs rewritten
  to `$PKG_*` so the executed region stays SP7-correct). The recordings
  pipeline consumes the one tree via the same seam. After migration there is
  exactly **one** script tree; a slug has one source.
  - **Transitional fold-equivalence gate (F3/F5, adopted):** EQ1–EQ3 assert
    *structural* one-tree invariants only — they do **not** catch a *lossy
    fold* (a dropped/reordered demo line) for any of the 21 casts. H-4b's
    rejected byte-diff would have served as a one-time migration-correctness
    oracle. To recover that value without H-4b's standing cost, a **one-shot
    Hat-1 characterization gate** is added in the migration phase: render each
    converged script's region and assert its command set byte-equals the
    pre-migration recordings script's command set. It is deleted after the
    convergence phase (a safety net, not a perpetual tax).
    - **LDR 2026-05-18 (EQ-T executed retroactively — /swarm-review max
      finding):** the convergence migration landed (a742b407) *before* the
      EQ-T oracle ran — the second tree was deleted without proving the fold
      lossless (a skipped Hat-1 safety-net step the review flagged Block).
      EQ-T was therefore reconstructed one-shot from
      `git show 19f0a2dc:test/recordings/scripts/*.sh` and run as an
      **ordered** command-sequence equality (per Codex F3, not set-equality)
      across all 21 historical scripts. **Outcome: 20/21 lossless.** One
      explicit waiver: `getting-started/env` — converged region prepends
      `ocx package install "$PKG_CORRETTO"` before `ocx package env`
      (additive prerequisite; the legacy `multi-version` SETUPS fixture
      pre-installed corretto, the converged `setup:multi-version` state does
      not). This is an intentional additive delta, not a dropped/reordered
      line; pinned by a standing targeted regression
      (`test_eqt_residual_getting_started_env_region_shape`). EQ-T itself is
      **not** retained as a standing gate (ADR rejects a perpetual
      equivalence tax — EQ1–EQ3 is the permanent guard). EQ1–EQ3 + EQ3b are
      now implemented as standing `test:parallel`-collected verify-path
      tests (`test/tests/test_doc_scripts_one_tree.py`), so the
      "EQ1–EQ3 as the structural guard" claim below is now backed by code,
      not convention.
- **H-4b Two trees + content-equivalence gate (rejected):** keep both trees;
  add a verify-path gate asserting that for a shared slug the doc-script render
  and the cast region are byte-equivalent.

**Decision: H-4a.** One-tree convergence is the structurally stronger fix: it
*eliminates* the drift class rather than *policing* it (a perpetual equivalence
gate is a standing tax and a new failure mode; the brief and research both call
one-tree "the stronger fix"). H-4b's advantages were under-scored in the first
draft: besides "no script moves" it also provides a *migration-correctness
oracle* — recovered here as the one-shot transitional gate above (H-4a bullet),
which delivers the oracle value without the standing cost. The script move +
dedup is a one-time cost the pre-1.0 no-compat-shim posture (§8) absorbs (note:
**not** a header rewrite — the unified header is already in place, F1). An EQ-family gate is still specified (EQ1–EQ3) but
in its **post-convergence form**: it asserts the *one-tree invariant* (no
`.sh` doc/recording script exists outside the single tree; no slug is backed by
two files) rather than cross-tree byte-equivalence. Codex Finding 1 is marked
**resolved by H-4a** (convergence), with EQ1–EQ3 as the structural guard that
the second tree does not silently reappear. The §7.1 "22 recordings scripts"
heading and §7.2 framing are superseded: see design-spec §7 (revised) — it is
now a single migration of one converged tree, not two independent header
rewrites.

### Reversibility

Medium (one-way door, bounded). The render layer is publish-time and
**build-time-internal**: `_scripts/<slug>.sh` is never a routed VitePress URL
(Decision D / §7 reversibility), so dropping or changing the render rules is an
in-repo sweep with zero external-link/SEO/CDN blast radius. The
`declared_display_env()` accessor + seam-schema extension are additive
(`display_env` is a new optional JSON key; the mirrored TypedDict gains a field
under the DE5 parity gate). The genuinely one-way elements, all pre-1.0 and
`test/`-bounded per §8: (1) the **declared-names static surface** every provider
must gain (H-1 note); (2) the **one-tree convergence** (H-4a) — once recordings
scripts move and the ~8 colliding slugs are deduplicated, the second tree is
deleted (the unified header is already present, so no header rewrite — F1);
(3) cast filenames already slug-derived (CA2) so no new one-way there. No compat
shim, no dual-header (§8) — consistent with the existing posture.

### Consequences

Positive:

- The published example is the example a reader types: no shebang, no
  metadata header, no `set -euo pipefail`, no assertion scaffolding, `$PKG_UV`
  rendered as `uv:0.10`.
- Tested execution is byte-for-byte unchanged (SP0–SP8, Decisions A/B1′ intact);
  three projections from **one** source — no parallel doctest engine (tenet #1).
- Region grammar unified (`# region cast` only) across what were two trees;
  one-tree convergence eliminates the Codex-Finding-1 drift class outright.
- The VitePress silent-fallback trap (GH #4625) is structurally kept off the
  doc-author surface (H-2) and additionally gated (NC4).
- The hand-mirrored `_DocScriptExportEntry` ↔ `DocScriptExportEntry` drift risk
  is closed by an explicit parity gate (DE5).

Negative / costs:

- Every state provider must gain an explicit static declared-names surface
  (H-1 note) — new metadata authored once per provider; the SP0-safe source for
  `display_env`.
- One publish-time render pass (strip + region-select + expand) added to the
  publish task; one new seam JSON key (`display_env`) + the mirror TypedDict
  field + parity gate.
- The 21 recordings scripts **move tree and have ~8 colliding slugs
  deduplicated** (H-4a). They **already carry the unified header** (F1) — this
  is a move + dedup, **not** a header rewrite (the earlier "22→header
  migration" framing is corrected; see §6e H-4 F1 note and design-spec §7.0a).
- Unknown-variable policy in the render pass is a hard publish error (RN5) — a
  `$FOO` with no `display_env` entry fails the publish task loudly rather than
  shipping a literal `$FOO` to a reader.

Neutral:

- `command-line.md` still untouched (tenet #5); its structural gate is
  unaffected.
- Published `.sh` files are no longer guaranteed valid bash (RN6 decides this
  explicitly): they are *display artifacts*, not runnable scripts — the runnable
  source is the tested file under `test/`.

## 7. Consequences

Positive:

- Stale documented commands fail `task verify` (Decision A) — drift is
  release-gated, not visual-build-gated.
- Doc authors get behavioural assertions (Scenario harness), not exit-code-only.
- Website and test trees evolve independently; one declared publish seam.
- Cast/GIF cost stays in `website:build`; per-commit loop stays cheap.
- One script body + header serves acceptance test and optional cast — no
  duplication, no parallel doctest engine (tenet #1).

Negative / costs:

- One-time `StateProvider` Protocol + two adapters (no registry merge — B1′).
  Behaviour-preserving; legacy registries untouched.
- 21 existing recordings scripts move into the one tree with ~8 colliding
  slugs deduplicated (header already unified — F1, no rewrite); 31 stale prose
  refs rewritten and converted to tested scripts.
- The publish task adds one cache-managed step to `website:build` ordering
  (must precede `vitepress build`; independent of `recordings:parallel` — cast
  files are a separate output set).
- Pre-existing `test/`→`website/` cast-path leak comes into scope (parameterize
  the casts dir) so the new seam is genuinely clean, not relegitimizing the
  tenet-#2 violation.

Reversibility (corrected — was overstated as a phantom risk):

- `_scripts/<slug>.sh` is **build-time-internal** (underscore dir, never a
  routed VitePress URL). A slug rename has **zero external-link/SEO/CDN blast
  radius** — only an in-repo grep+edit of `# doc:` + `<<<` citations, with
  automatic orphan cleanup (PT5).
- The only coordinated one-way element is the **header-schema cutover** (no
  compat shim, §8), bounded entirely to `test/`, pre-1.0. That is the real
  (small) one-way door — not the slug.

Neutral:

- `command-line.md` is untouched; its structural gate
  (`test_doc_command_reference.py`) remains the authority for the reference
  page (tenet #5).

## 8. Migration Note (pre-1.0, no compat shims)

Per `project_breaking_compat_next_version` and CLAUDE.md ("for refactors we
often expect the user to just delete all and start over" for unreleased
surfaces): **no migration code, no compatibility shims, no dual-header support.**
The unified header schema replaces the two legacy headers outright. Legacy
`# setup:` / `# scenario:` keys are rewritten in-place during the migration
phase, not aliased. Stale prose commands are rewritten to the current taxonomy
and replaced by transcluded tested scripts in the same change; removed commands
get tombstone/rename mapping in the design spec, not runtime redirects.

Detailed schema, contracts, UX scenarios, error taxonomy, and the
22-script + 31-ref migration mapping are specified in
`design_spec_doc_command_scripts.md`.
