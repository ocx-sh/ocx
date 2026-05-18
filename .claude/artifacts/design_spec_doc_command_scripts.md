# Design Spec: Tested Doc-Command Scripts

> Component design for the mechanism decided in
> `adr_tested_doc_command_mechanism.md`. No implementation here.
> Contracts are written so a `/swarm-execute` tester can author failing
> tests **without reading implementation code**.
>
> **2026-05-17 addendum (ADR Decision H).** Adds the publish-time
> display-render layer and region-grammar unification. New contract families:
> **RN1–RN7** (render rules), **DE1–DE5** (`display_env` seam schema + static
> SP0-safe accessor + TypedDict parity gate), **RG1–RG3** (unified region
> grammar), **NC4** (VitePress missing-region silent-fallback verify guard),
> **EQ1–EQ3** (one-tree invariant — Codex Finding 1 resolved by ADR H-4a). §7
> migration is **revised** below: it is now a single converged-tree migration,
> not the two independent header rewrites of the original §7.1/§7.2 (those
> subsections are superseded; their taxonomy-rewrite content is retained).

## 0. Glossary

| Term | Meaning |
|---|---|
| **Doc script** | A `.sh` file under `test/` carrying the unified header; runs as an acceptance test always, optionally produces a cast. |
| **State provider** | Named, registered object that publishes the pre-publish registry state a script needs and exposes both the recordings display-name map and the Scenario `$PKG_*` env projection. Unifies legacy `SETUPS` + `SCENARIOS`. |
| **Publish task** | The single declared `test/` → `website/` bridge task. Copies opted-in scripts (+ optional casts) into `website/src/_scripts/`. |
| **Drift gate** | The acceptance case (verify path) that fails when any doc script fails — i.e. when a documented command is stale. |
| **Slug** | The metadata-derived flat publish identifier from `# doc:`. |

## 1. Script Metadata Schema

### 1.1 Grammar

Header = contiguous comment block at the top of the file. Shebang (`#!…`)
ignored. Parsing stops at the first non-blank, non-comment line (same rule as
`parse_script_metadata`). One metadata pair per line:

```
# <key>: <value>
```

Keys are case-insensitive, lowercased on parse; values are stripped. Unknown
keys are a **hard parse error** (fail fast — prevents silent typos like
`# scenrio:`). Recognised keys:

| Key | Required | Type | Meaning |
|---|---|---|---|
| `state` | no (default `setup:basic`) | **family-qualified** name | `setup:<name>` (recordings family) or `scenario:<Name>` (scenario family). The bare unqualified form is rejected (hard error) — family prefix is mandatory so resolution is unambiguous. Replaces legacy `# setup:` / `# scenario:`. |
| `doc` | required iff published | slug | Page-binding slug. Flat namespace under `_scripts/`. Absent ⇒ script is tested-only, never published. |
| `cast` | no (default `false`) | `true` \| `false` | Opt in to cast generation in `website:build`. **Constraint:** a `# cast: true` script MUST contain exactly one `# region cast` / `# endregion cast` block (§1.3); only that block is replayed in the cast. |
| `title` | no | string | Cast title + drift-failure label. Defaults to file stem. |
| `description` | no | string | Human note; surfaced in failure output. |
| `expect` | no | path | Optional golden-output file (relative to script dir). When present, the executor diffs ANSI-stripped combined stdout+stderr against it; mismatch fails the case with a unified diff. Absent ⇒ assertion-only (default). |

**State resolution is explicit-family (ADR B1′; corrects Codex finding 1).**
`# state:` is `setup:<name>` or `scenario:<Name>`. The recordings family
(`SetupAdapter` over legacy `SETUPS`) names: `basic`, `multi-version`,
`full-catalog`, `variants`, `dependencies`, `deps-visibility`, `publisher`. The
scenario family (`ScenarioAdapter` over legacy `Scenario` subclasses) names are
the **actual class registry keys**: `BasicPackage`, `DiamondDeps`,
`MultiEntrypoints`, `MultiLayer`, `ThreeLevelDeps`, `TwoLevelDeps` (there is **no
scenario key `basic`** — verified against `test/src/scenarios/*.py`). The
explicit `setup:` / `scenario:` prefix means no collision can exist between
families by construction (SP1) — there is no implicit "union view" and no
"union `basic`". Default when `# state:` absent = `setup:basic`.

### 1.3 Cast region (corrects Codex finding 3)

The drift-gate executor runs the **entire** script body (assertions, `set -euo
pipefail`, multiline control flow — all of it). The cast recorder replays
**line-by-line through a PTY** (`run_command()` per non-comment line, prompt
wait between) and CANNOT handle multiline constructs, heredocs, or assertion
scaffolding without hanging or leaking test-only commands into the demo.

Resolution: a `# cast: true` script demarcates the demo command lines:

```
# region cast
ocx package install --select uv:0.10
ocx package which uv
# endregion cast
```

The cast layer replays **only** the lines inside `# region cast` …
`# endregion cast` (PTY-safe one-command-per-line). Everything outside —
`set -euo pipefail`, `$(…)` capture, `[[ … ]] || exit 1` assertions — runs in
the drift gate but is **never** in the cast. A `# cast: true` script without
exactly one cast region is a hard parse error.

`# doc:` slug grammar: `^[a-z0-9]+(?:[-/][a-z0-9]+)*$` (lowercase, `-` and `/`
as separators; `/` is the **published directory separator** — the slug maps
1:1 to a nested path `website/src/_scripts/<slug>.sh`, no escaping; LDR
2026-05-17, see §3.2 / ADR Decision D). Validation is a hard error on violation.

### 1.2 Concrete example script

`test/doc_scripts/install_select.sh`:

```sh
#!/usr/bin/env bash
# state: setup:basic
# doc: getting-started/install-select
# title: Install and select a tool
# description: Install uv, mark it current, run it via the clean env.
# cast: true
set -euo pipefail

# region cast
ocx package install --select "$PKG_UV"
ocx package which uv
# endregion cast

out="$(ocx package exec uv -- uv --version)"
[[ "$out" == *"uv 0.10"* ]] || { echo "unexpected: $out" >&2; exit 1; }
```

Notes the tester can assert against:

- `state: setup:basic` ⇒ the recordings-family `basic` provider publishes a `uv`
  package and exposes `$PKG_UV` (display-name `uv`).
- Cast region replays only the two `ocx` lines; the `set -euo pipefail`,
  capture, and `[[ … ]]` assertion run in the drift gate but never in the cast.
- Body uses Scenario env (`$PKG_UV`) and bash assertions — canonical executor
  per ADR Decision B1′.
- `cast: true` ⇒ this script *also* produces `_scripts/…`-adjacent cast in
  `website:build`; a script without `cast:` runs in verify but never records.
- `doc:` slug present ⇒ publishable; published flat as
  `getting-started/install-select.sh` (see §3.2).

## 2. Component Contract — Unified Executor

The drift gate executes every doc script as a Scenario-harness acceptance case.

**Inputs:** a discovered `.sh` path; an `OcxRunner`; a `tmp_path`.

**Behaviour contract (testable black-box):**

| ID | Given | When | Then |
|---|---|---|---|
| EX1 | a script with `# state: setup:basic` | executed | the recordings-family `basic` provider is instantiated and its packages published before the body runs |
| EX2 | a script body that exits 0 | executed | the case passes |
| EX3 | a script body that exits non-zero | executed | the case fails; failure text includes `title` (or stem), the script path, `description` if present, the `# doc:` slug if present, and **ANSI-stripped** captured stdout+stderr (CI logs stay readable) |
| EX4 | a script with `# state: <unknown>` or unqualified (no `setup:`/`scenario:` prefix) | collected | the case fails with `invalid state '<v>'; expected setup:<name> or scenario:<Name>; available: <sorted families>` |
| EX5 | a script with an unknown header key | collected | the case fails with `unknown metadata key '<key>' in <file>` |
| EX6 | a script with no `# state:` | executed | the default (`setup:basic`) provider is used |
| EX9 | a `# cast: true` script with zero or >1 `# region cast` blocks | collected | hard parse error `cast script must have exactly one cast region` |
| EX7 | (structural) the doc-script test module | imported | carries `pytestmark = pytest.mark.skipif(sys.platform == "win32", …)` — assertable on any platform without Windows execution (parity with `test_scenarios_smoke.py`) |
| EX8 | a script with `# cast: true` | run on the **verify path** | it still runs as a normal acceptance case; no cast is produced on the verify path |
| GO1 | a script with `# expect: out.txt` whose ANSI-stripped stdout+stderr equals `out.txt` | executed | the case passes |
| GO2 | the same with output ≠ golden | executed | the case fails with a unified diff (golden vs actual, both ANSI-stripped) in the failure text |
| GO3 | a script with `# expect:` pointing at a missing file | collected | the case fails with `golden file not found: <path>` |
| EX10 | any script | executed | **(LDR 2026-05-18)** the drift gate runs the **raw** body under `provider.script_env()` (SP7-prefixed `$PKG_*`) — it does **not** renderable-substitute before `bash -c`. **Why not (SP7 blocker, discovered Phase-6 verify):** `provision()` pushes to an SP7 parallel-isolation repo `t_<8hex>_<repo>`; only the SP7-prefixed `$PKG_*`/`$REPO_*` in `script_env` resolves against the registry. Substituting to the clean display short (`webapp:2.0.0`) makes the command fail `package not found` (verified: `deps`/`deps-flat`/`deps-why` scripts). SP7 cannot be dropped (drift gate runs `-n auto`; serialising it was explicitly rejected, SP7 LDR). **Honest guarantee (replaces the deleted RN6 disclaimer):** *DE6-canonical equivalence*, not byte-identity. The displayed artifact is **this exact source** rendered (RN1/RN2 + RN8) with the *declared* values; DE6 gates `declared == canonical(provisioned)` (SP7 prefix `^[ts]_[0-9a-f]{8}_` stripped). Therefore the displayed command **is** the drift-gated command with only the parallel-isolation prefix canonicalised away — a precise, gated relationship (drift gate proves the source runs green; DE6 proves the only displayed-vs-executed delta is the SP7 prefix; the publish substring+parity tests prove the display is a faithful RN8 slice of that same source). Golden output (GO1–GO3) unaffected. |

**Discovery contract:** one pytest case per `.sh` file under the doc-script
root; case id = path relative to that root (parity with
`test_scenarios_smoke.py`). Empty/missing root ⇒ zero cases, not an error.

**Non-goals:** the executor makes no claim about output content beyond what the
script's own `[[ … ]] || exit 1` asserts (same contract as
`test_scenario_script`).

## 3. Component Contract — State-Provider Registry Unification

Replaces the two legacy registries (`recordings.setups.SETUPS` dict-of-funcs;
`src.scenarios.SCENARIOS` dict-of-subclasses) with **one** named registry.

**Adapter model (ADR Decision B1′ — no registry merge):** a `StateProvider`
Protocol with two adapters. `SetupAdapter` wraps a legacy `SETUPS` function;
`ScenarioAdapter` wraps a legacy `Scenario` subclass. Both consumers depend on
the Protocol only. Legacy registries are **untouched** (behaviour-preserving).

**Provider contract (testable):**

| ID | Property |
|---|---|
| SP0 | Provider registration and module import perform **zero registry I/O**. Provisioning occurs only when the provider is invoked with live fixtures. Tester: import the provider registry with no registry fixture, assert no network call / no OCI push. |
| SP1 | Resolution is **explicit-family** (`setup:<name>` / `scenario:<Name>`); the two families are separate namespaces so no collision is possible by construction. The legacy `basic` name exists only in the recordings family (`setup:basic`); the scenario family has **no** `basic` key (its keys are PascalCase class names — SP6). Tester: `setup:basic` resolves to the recordings adapter; `scenario:basic` fails with `invalid state` (no such scenario key); an unqualified `basic` is rejected (EX4). |
| SP2 | `provider.packages` is a `dict[str, PackageInfo]` keyed by **display name** (recordings' contract — drives display→actual sanitisation) |
| SP3 | `provider.script_env()` returns the Scenario env projection (`$PKG_*`, `$FQ_*`, `$REPO_*`, `$TAG_*`, `$MARKER_*`, `$HOME_KEY_*`, `$OCX`, `$OCX_HOME`, `$REGISTRY`, `$SCENARIO_TMP`) for the same packages |
| SP4 | `provider.display_map` returns `{actual_repo: display_name}` (recordings' `sanitize_map` input) and `{display_name: actual_repo}` (recordings' `repo_map` input) |
| SP5 | `SetupAdapter` over each legacy `SETUPS` name (`basic`, `multi-version`, `full-catalog`, `variants`, `dependencies`, `deps-visibility`, `publisher`) is **behaviour-equivalent** to calling that `SETUPS` function directly (existing recordings suite passes unchanged) |
| SP6 | `ScenarioAdapter` over each legacy `Scenario` subclass — the **actual registry keys** `BasicPackage`, `DiamondDeps`, `MultiEntrypoints`, `MultiLayer`, `ThreeLevelDeps`, `TwoLevelDeps` (verified `test/src/scenarios/*.py`; **no `basic` key**) — exposes the **same `$PKG_*` projection** as the subclass today (existing scenario suite passes unchanged) |
| SP7 | **Parallel-isolation invariant (Living Design Record, 2026-05-17).** The legacy `SETUPS` functions push to *fixed* repo names when called with the default `prefix=""` (correct for the serial recordings suite, which wants clean cast display names). The drift gate runs in `test:parallel` (`-n auto`) where many cases provision a recordings family concurrently → registry:2 collides on identical repo+tag. `SetupAdapter.provision` MUST therefore pass a **unique repo prefix per provision** (e.g. `t_<8hex>_`, mirroring the `unique_repo` convention in `subsystem-tests.md`) using the `prefix` parameter the `SETUPS` functions already accept — no SETUPS edit, no test-side `xdist_group` serialization (which would serialize the whole drift gate and defeat the parallelism the mechanism requires). `ScenarioAdapter` already isolates (Scenario fixtures are UUID-scoped) and is unaffected. The prefix changes only `PackageInfo.repo`/`.fq`; display-name dict keys (SP2/SP5) and the `$PKG_*` projection (SP3) are prefix-agnostic, and `display_map` (SP4) still maps the (now-prefixed) actual repo ↔ display name — its inverse property is unchanged. |
| SP8 | **Publisher working-directory contract (Living Design Record, 2026-05-17).** The `publisher` SETUPS function writes its source tree (`build/`, `build-v2/`, `base/`, `metadata.json`, `README.md`) relative to the `tmp_path` it receives. `SetupAdapter.provision` calls the function with `state_path = tmp_path / "_state"` (not the pytest-fixture `tmp_path`) to avoid `make_package` subdirectory collisions. The recordings runner must therefore `cd` to `provider.work_dir` (the exact directory the function wrote into) rather than the pytest fixture root. `StateProvider` exposes `work_dir: Path | None`: `SetupAdapter` sets it to `state_path` after `provision()`; `ScenarioAdapter` always returns `None` (scenarios do not write a publisher-style tree). This property is **recordings-only** — the drift-gate executor (`run_doc_script`) does not consult `work_dir`. |

> **No collision exists by construction.** Explicit `setup:` / `scenario:`
> prefixes are separate namespaces. The legacy `basic` lives only in the
> recordings family. The scenario family is keyed by PascalCase class names —
> there is no scenario `basic`. No "union `basic`", no behaviour merge, no
> Two-Hats violation. (Codex finding 1: earlier lowercase scenario names
> `diamond_deps` etc. were wrong; corrected to the real class keys above.)

**Both consumers read the same adapter by name:** the drift-gate executor
(§2, via `script_env`) and the cast recorder (§4, via `packages`/`display_map`).

## 4. Component Contract — Cast Layer (opt-in, `website:build` only)

**Inputs:** scripts with `# cast: true`; the same state provider as the drift
gate; the existing PTY `CastRecorder`.

**Behaviour contract:**

| ID | Given | When | Then |
|---|---|---|---|
| CA1 | script with `# cast: false` (or absent) | `website:build` recordings step | no `.cast` written for it |
| CA2 | script with `# cast: true` and `# doc: <slug>` | `website:build` recordings step | a `.cast` written as `<slug>.cast` (nested slug path; same `/`→`__` flatten as PT2) under the casts dir; the page binds `<Terminal src="/casts/<flat-slug>.cast">`. **Decision (one-way, touches Phase 5):** cast filename derives from the **doc slug**, not the script stem — existing `<Terminal src>` references are updated during the Phase 5 migration. A `# cast: true` script with no `# doc:` (demo-only cast) keeps stem-named `.cast`. |
| CA3 | the same script | run on the verify path | no `.cast` written (EX8) |
| CA4 | cast generation | always | uses the *same* adapter state as the drift gate (no second state definition) |
| CA5 | a `# cast: true` script with a `# region cast`/`# endregion cast` block | recorded | the PTY replay sees **only** the lines inside the region (one command per line); lines outside (`set -euo pipefail`, `$(…)` capture, `[[ … ]]` assertions) are **never** sent to the recorder — no hang, no test-scaffold leakage into the cast (Codex finding 3) |

Cast generation reuses the existing `CastRecorder` PTY replay; the only changes
are: (a) replayed lines come **only** from the script's `# region cast` block
(§1.3), not the whole body; (b) repo rewriting uses `provider.display_map`
(SP4). The full body still runs in the drift gate (§2) — the region is a
display selector, not an execution selector.

## 5. Component Contract — Publish Task

The single declared seam (`website/recordings.taskfile.yml` already plays this
architectural role; the publish task is either added there or in a sibling
`website/` taskfile that `website:build` calls — owner is the **website**
subsystem, never `test/`).

**Inputs:** doc scripts under the test-side doc-script root that declare `# doc:`.
**Outputs:** for each, a rendered (RN1–RN7) copy at the **nested** path
`website/src/_scripts/<slug>.sh` (slug `/` = dir separator); for
`# cast: true` scripts, the cast at `website/src/public/casts/<slug>.cast`
(same nested slug scheme, LDR 2026-05-17).

**Behaviour contract (testable):**

| ID | Property |
|---|---|
| PT1 | A script with no `# doc:` is **not** copied (tested-only) |
| PT2 | `# doc: a/b-c` ⇒ exactly one file `website/src/_scripts/a/b-c.sh` (**nested**; slug `/` = directory separator; parent dirs created `mkdir -p`; injective since distinct slugs ⇒ distinct paths — no escaping). LDR 2026-05-17 (was flat `a__b-c.sh`). |
| PT3 | Re-running the task with unchanged inputs makes **no writes** (idempotent; `status:`/`sources:` per taskfile caching contract) |
| PT4 | Two scripts with the same `# doc:` slug ⇒ task fails with `duplicate doc slug '<slug>' (<fileA>, <fileB>)` and writes nothing |
| PT5 | The task maintains a manifest `website/src/_scripts/.published.json` listing task-owned files (nested slug paths). The orphan sweep removes **only** task-owned `*.sh` no longer backed by a current slug, then prunes slug directories it owns that became empty. It MUST NOT delete files absent from the manifest, non-`.sh` files, or any directory still containing foreign content. Tester: place a foreign `keep.txt` and a foreign `other.sh` not in the manifest (incl. one inside an owned slug dir), run task, assert both survive while a real orphan `.sh` is removed and a fully-emptied owned dir is pruned. |
| PT6 | The task discovers scripts **only** via the test subsystem's declared export — `task test:doc-scripts:list` emitting JSON `[{path, slug, cast, expect}]`. No `website/` file contains a `test/` path literal. Tester: grep every `website/` taskfile/source for `test/` path literals tied to discovery, assert none; assert the task consumes the JSON export. |
| PT7 | Ordering: the publish task completes **before** `bunx vitepress build` (transclusion is build-time and reads `_scripts/`); it is **independent of** `recordings:parallel` (different output set — `_scripts/*.sh` vs `casts/*.cast`) and may run before or in parallel with it |
| PT8 | The pre-existing reverse leak — `test/recordings/conftest.py` hardcoding `website/src/public/casts/` — is fixed: the casts **output** dir is a parameter/env, defaulting via the website seam, so no `test/` file hardcodes a `website/` **output/build** path. **Scope clarification (Living Design Record, 2026-05-17):** PT8 governs *output-path coupling and build-dependency coupling* only. The NC2 verify-path gate (`test/src/doc_binding.py` `WALKTHROUGH_PAGES`) **must** name the five `website/src/docs/*.md` pages — reading the artifact under test is the gate's defined function (§6b), not a leak; it is explicitly exempt from the PT8 grep. Tester: grep `test/` for hardcoded `website/` *output/casts* path literals, assert none; the NC2 page-input references are out of scope. |

**Idempotency / caching:** `sources:` = the doc-script set + their headers;
`status:` checks the published-copy set matches (presence + content hash). The
`.task/` fingerprint dir must be gitignored (taskfile caching contract).

**Ordering in `website:build`:** insert publish into the `build` cmd list such
that PT7 holds — i.e. before `bunx vitepress build`, with no required edge to
`recordings:parallel`.

## 6. Component Contract — Drift Gate

**Which verify task runs it:** the doc-script acceptance case is collected by
the `test:parallel` path (root `taskfile.yml` `.verify:build-test` →
`test:parallel`), exactly like `test_scenarios_smoke.py`. It therefore runs in
`task verify` and in `task test:parallel`, and is **absent** from
`website:build`'s critical path (ADR Decision A).

**Failure ergonomics (testable):**

| ID | Given | Then the failure message contains |
|---|---|---|
| DG1 | a doc script whose documented command no longer exists (nonzero exit) | the script path, the `# title`, and — when `# doc:` present — the slug, so a human maps script→doc page in one read |
| DG2 | a doc script with `# doc: getting-started/install-select` failing | the literal slug `getting-started/install-select` (so the failing doc page is named, not just the test file) |
| DG3 | the suite green | every documented command in every published walkthrough page executed successfully (drift gated) |

> DG1/DG2 are the script→doc-page mapping requirement: a release engineer
> reading CI output must learn *which website page* is stale without opening
> the script.

## 7. Migration

Pre-1.0, **no compat shims, no dual-header support** (ADR §8). All conversions
land in the migration phase's own commits.

> **Revised 2026-05-17 (ADR Decision H-4a — one-tree convergence).** The
> original §7.1 ("22 recordings scripts") and §7.2 ("31 stale prose refs")
> framed migration as **two independent script families with separate
> headers**. ADR H-4a supersedes that: the recordings tree
> (`test/recordings/scripts/`) **converges into** `test/doc_scripts/` and
> adopts the **single** unified header + region grammar. The migration is now
> **one converged-tree migration** in three folded steps (7.0a–7.0c below).
> The taxonomy-rewrite mapping and tombstone table in §7.2 remain
> authoritative and are reused verbatim by step 7.0b — only the *framing*
> ("two trees") is superseded, not the rewrite content.

### 7.0a Converge recordings scripts into the one tree (ADR H-4a / EQ1)

**Premise correction (Round-1 review F1 — verified live):** all 21
`test/recordings/scripts/*.sh` **already carry the unified
`# state:`/`# cast:`/`# doc:`/`# region cast` header** and are parsed by
`src.doc_scripts.parse_doc_header`. There is **no legacy `# setup:` header
rewrite** (the stale `test/recordings/conftest.py:12` comment and the earlier
draft §7.1 mapping are wrong). Convergence is a **move + dedup**, not a header
migration.

**Sequencing constraint (Round-1 review F2 — Block):** ~8 slugs collide
*outright today* (verified: `getting-started/install-select`,
`.../install`, `.../exec`, `.../exec-multi`, `.../env`, `.../env-multi`,
`.../find-candidate`, `.../uninstall` exist in **both** trees). PT4
(duplicate `# doc:` slug ⇒ publish hard-fails) means the moment both trees
feed the seam, `task verify`/publish is **red**. Therefore the dedup
(§7.0a) is a **precondition for the page-transclusion phase**, not a
parallel track — see ADR H-4a and the plan's corrected critical path.

Steps (one converged commit-set):

1. **Move** each `test/recordings/scripts/*.sh` into `test/doc_scripts/`
   (nested slug-derived path). Header is already unified — *verify only*, do
   not rewrite.
2. **Dedup the ~8 colliding slugs:** one converged script per slug. The
   asserted `test/doc_scripts/` body is **canonical** (it has the
   `set -euo pipefail` + `$PKG_*` + `[[…]]` assertions the drift gate needs).
   Fold the recordings demo into its `# region cast`; **rewrite any literal
   repo ref in the region to the matching `$PKG_<KEY>` projected var only**
   (a literal `uv:0.10` in an *executed* region line cannot resolve under
   SP7 UUID-prefixing — H1-infeasibility applies to the region body too;
   and per RN5/DE1 `$PKG_<KEY>` is the *only* renderable displayed-region
   var — never `$REPO_*`/`$FQ_*`/`$REGISTRY`/`$SCENARIO_TMP`, Codex F2).
3. **Repoint** every `<Terminal src="/casts/…">` to the converged
   slug-named cast (CA2).
4. **Switch recordings discovery to the seam** (EQ3): update
   `test/recordings/conftest.py` / recordings taskfile to consume
   `task test:doc-scripts:list` filtered to `cast == true`; remove any
   `test/recordings/scripts` glob. Fix the stale conftest comment.
5. **Pre-removal consumer check:** grep all of `test/`, taskfiles,
   conftests for any non-doc consumer of `test/recordings/scripts/`;
   migrate/retire before deleting the directory.
6. **Delete** `test/recordings/scripts/` (EQ1).
7. **Transitional fold-equivalence gate (EQ-T, one-shot Hat-1 — F3/F5):**
   before/within the dedup commit, render each converged script's region
   and assert its command set byte-equals the pre-migration recordings
   script's command set (catches a lossy fold EQ1–EQ3 structurally cannot).
   Deleted at the end of the convergence phase — a safety net, not a
   standing gate.
8. Add the provider's static **declared-names surface** (ADR H-1 / DE2) for
   any `$PKG_*` a rendered region references, so `display_env` is SP0-safe.

The legacy `SETUPS` functions are **untouched** (SP5); the recordings-family
`basic` and scenario-family `basic` remain distinct adapters (SP1) — no
union, no behaviour merge (ADR B1′ / Two-Hats).

### 7.0b Rewrite 31 stale prose refs (taxonomy mapping reused from §7.2)

Apply the §7.2 taxonomy-rewrite + tombstone tables verbatim, then replace each
inline snippet with `<<< @/_scripts/<slug>.sh{sh}` (**no `#region`** —
the published file is the pre-rendered region body, ADR H-2 / NC4).

### 7.0c Add the render-layer + seam fields

- Extend `DocScriptExportEntry` and the mirrored `_DocScriptExportEntry` with
  `display_env: dict[str,str]` (DE1) under the DE5 parity gate.
- Add `StateProvider.declared_display_env()` (DE2) + each provider's static
  declared-names surface (ADR H-1 note); enforce DE4 (SP0) by an
  import/no-network test.
- Add the render pass (RN1–RN7) to the publish task; it consumes `display_env`
  from the seam.
- Add NC4 + RG3 to the verify-path static gate module alongside NC1–NC3.

### 7.1 (VOID — premise falsified by Round-1 review F1)

> This table described a `# setup:` → `# state:` header rewrite for the 21
> recordings scripts. **It is wrong:** all 21 already carry the unified
> header (verified live). No header migration exists. The real convergence
> work is the move + ~8-slug dedup in §7.0a. Kept only as a pointer so the
> earlier (incorrect) framing is not silently reintroduced; do **not**
> execute this table.

Recordings-family names (`basic`, `multi-version`, `full-catalog`, `variants`,
`dependencies`, `deps-visibility`, `publisher`) resolve through `SetupAdapter`
to the **untouched** legacy `SETUPS` functions (SP5). The `basic` collision with
the scenario-family `basic` is **dissolved by family namespacing** (SP1, ADR
B1′): the recordings-family `basic` and the scenario-family `basic` remain two
distinct adapters — **no union is created, no behaviour merged**. The tester
asserts two distinct objects (SP1) plus behaviour-equivalence in each family
independently (SP5 for `SetupAdapter`, SP6 for `ScenarioAdapter`).

### 7.2 (taxonomy mapping — authoritative; framing superseded by 7.0b) 31 stale prose references → tested scripts

Pages in scope (prose walkthroughs — tenet #5):
`website/src/docs/getting-started.md`, `user-guide.md`, `faq.md`,
`in-depth/environments.md`, `in-depth/entry-points.md`.

Taxonomy rewrite mapping (apply to every occurrence in prose; then replace the
inline code with a `<<< @/_scripts/<slug>.sh{sh}` to a tested script):

| Stale form | Current taxonomy |
|---|---|
| `ocx shell env` / `ocx shell hook` / `ocx shell init` | removed — replace with the current activation flow (`ocx direnv …` / root-tier `ocx env` per `command-line.md` live anchors); **no inline shell-eval snippet** |
| `ocx ci export` | removed — tombstone; rewrite the surrounding prose to the supported export path |
| `ocx install …` | `ocx package install …` |
| `ocx exec …` | `ocx package exec …` |
| `ocx which …` | `ocx package which …` |
| `ocx find …` | `ocx package which …` (`find` → `which`) |
| `ocx info …` | `ocx about …` (`info` → `about`) |
| `ocx update …` | `ocx upgrade …` (`update` → `upgrade`) |
| `ocx <cmd> --global …` (per-command) | root-only `--global` (flag before subcommand, per memory `feedback_flags_before_args` + recent `collapse --global` commit) |

Tombstone/rename record (authoritative for this migration; `command-line.md`'s
own tombstones remain governed by `test_doc_command_reference.py`):

| Old command | Disposition | Doc action |
|---|---|---|
| `shell env/hook/init` | REMOVED | delete snippet; rewrite prose to current activation; no script |
| `ci export` | REMOVED | delete snippet; rewrite prose; no script |
| `install` / `exec` / `which` | MOVED → `package *` | rewrite + back with tested script |
| `find` | RENAMED → `package which` | rewrite + tested script |
| `info` | RENAMED → `about` | rewrite + tested script |
| `update` | RENAMED → `upgrade` | rewrite + tested script |
| per-cmd `--global` | COLLAPSED → root `--global` | rewrite flag position; tested script |

Each rewritten walkthrough snippet becomes a doc script with a `# doc:` slug;
the prose embeds `<<< @/_scripts/<slug>.sh{sh}`. `cast:` defaults off —
opt in only where the page already shows a `<Terminal>`/GIF.

### 7.3 Out of scope

`command-line.md` — untouched; remains under `test_doc_command_reference.py`
only (tenet #5). No execution, no `<<<` binding.

## 8. UX Scenarios

### 8.1 Happy path — author adds a snippet

1. Author writes `test/doc_scripts/env_compose.sh` with
   `# state: dependencies`, `# doc: user-guide/env-compose`, body using
   `$PKG_WEBAPP` + a `[[ … ]] || exit 1` assertion.
2. `task test:parallel` → the new case runs, passes (drift gate green).
3. Author adds `<<< @/_scripts/user-guide/env-compose.sh{sh}` to
   `user-guide.md`.
4. `task website:build` → publish task copies the script flat; VitePress
   transcludes it at build time. No cast (no `# cast:`).

### 8.2 Failure — a command goes stale

1. CLI renames `ocx package exec` again.
2. `test/doc_scripts/install_select.sh` exits non-zero (`exec` gone).
3. **Which test fails:** the doc-script drift-gate case for that file, on
   `task verify` / `task test:parallel` (not `website:build`).
4. **Message (DG1/DG2):** includes path
   `test/doc_scripts/install_select.sh`, title `Install and select a tool`,
   slug `getting-started/install-select`, and captured stderr — engineer maps
   straight to `getting-started.md`.

### 8.3 Cast opt-in

1. Author flips `# cast: false` → `# cast: true` on an existing passing script.
2. `task verify` unchanged (EX8/CA3 — still no cast on verify path).
3. `task website:build` recordings step now also emits `<slug>.cast` (CA2);
   author wires `<Terminal src="/casts/<slug>.cast" …>` in the page.

### 8.4 Page rename

1. Page `getting-started.md` section anchor changes; author updates
   `# doc:` slug `getting-started/install-select` →
   `getting-started/first-install`.
2. Publish task: old `_scripts/getting-started/install-select.sh` is an orphan
   ⇒ removed (PT5); new nested copy written (PT2). Author updates the `<<<` line
   to the new nested slug path. No test-tree change required (binding is metadata, not
   path — tenet #2).
3. **If the author forgets to update the `<<<` line:** NC2 (verify-path binding
   check, §6b) fails in `task verify` — the stale `<<<` reference no longer
   resolves to a published slug. Caught at verify, not deferred to
   `website:build` (Codex finding 2).

## 6b. Component Contract — Doc-Binding Verify Gate (NC2)

A static check on the **verify path** (no website build, no Docker): for each
of the five walkthrough pages, every `<<< @/_scripts/<file>.sh` reference must
resolve to a slug present in the publish export (`task test:doc-scripts:list`
JSON / the `.published.json` manifest contract).

| ID | Given | Then |
|---|---|---|
| NC1 | a walkthrough page | zero inline ` ```bash `/` ```sh ` blocks containing an `ocx ` **invocation** that are not `<<<` transclusions (no ungated inline examples). **Exemption (Living Design Record, 2026-05-17; extended after Codex cross-model review):** NC1 must detect an `ocx` invocation *structurally*, not by the literal substring `ocx ` — it matches `ocx`, a path-qualified form (`…/ocx`, e.g. `"$HOME/.ocx/ocx"`), and the pinned form (`${OCX_BINARY_PIN:-ocx}`), and treats a language-labelled fence (` ```sh [label] `) as a shell fence. A fenced block is exempt when it is a **generated/installer-managed artifact listing**, not a runnable example: (i) its first non-blank line is a shebang (`#!`) — the install-time launcher script in `entry-points.md`/`environments.md` (`#!/bin/sh … exec ocx launcher exec '<digest-path>' …`); or (ii) it is the OCX-installer-written shell-profile fragment delimited by the installer block markers `# BEGIN ocx` … `# END ocx` (shown in `user-guide.md` shell-activation section). Both are documentation of an on-disk artifact OCX itself writes (placeholder paths / installer-managed markers), not an invocation a reader types. NC1's intent is "every demonstrated `ocx` *invocation* is backed by a tested script"; a generated-artifact listing is not an invocation and is exempt. A genuine path-qualified inline invocation outside these exemptions IS flagged (closes the Codex false-negative). |
| NC2 | a `<<< @/_scripts/<file>.sh` reference whose `<file>` has no backing `# doc:` slug in the export | the verify-path check fails naming the page + the unresolved reference (a slug typo / rename / stale page reference fails `task verify`, **not** only `website:build`) |
| NC3 | every `<<<` reference resolves | check passes (binding is drift-gated together with command execution) |

NC1–NC3 run in the same `test:parallel` collection as the drift gate, so a
green `task verify` proves *both* that documented commands execute *and* that
every page actually binds to a real published tested script.

## 6e. Component Contract — Display-Render Layer (RN1–RN9)

> ADR Decision H-1 (Option H2) / H-2. A pure, deterministic transform applied
> by the publish task (§5) to each script that has a `# doc:` slug, producing
> the `website/src/_scripts/<slug>.sh` *display artifact*. The tested
> source under `test/` is **never** mutated. No registry I/O, no provisioning
> (the input is the seam JSON + the raw script text only).

**Inputs:** the raw script text; the script's parsed `DocScriptMeta`; the
`display_env: dict[str,str]` for that script's resolved state (from the seam,
DE1–DE3).

**Render rule precedence (exact, testable):**

| ID | Given | When | Then |
|---|---|---|---|
| RN1 | a script **with** a `# region cast` / `# endregion cast` block | rendered | output = the lines **strictly between** the two markers (markers excluded), in file order, with leading/trailing fully-blank lines trimmed. Lines outside the region (header, `set -euo pipefail`, `$(…)` capture, `[[ … ]]` assertions) are **absent**. **Coordinate convention (LDR — Phase-2 stub ambiguity #1):** `cast_region` is the **1-based inclusive** `(start, end)` marker-line span exactly as set by `parse_doc_header` and already consumed by `recordings/cast_layer.py::_extract_region_lines` (which slices `all_lines[start:end-1]` to exclude the marker lines). The render layer MUST reuse that identical slicing semantics — do not reinvent a 0-based/exclusive variant. Over the JSON seam the tuple is a 2-element array; index positionally (`region[0]`, `region[1]`). |
| RN2 | a script **without** a region block | rendered | output = full body **minus**: (a) a leading shebang line (`#!…`), (b) the contiguous `# <key>: <value>` metadata header block (same span `parse_doc_header` consumes; plain non-metadata comments are **kept**), (c) a single leading `set -euo pipefail` line (and a `set -e`/`set -eu`/`set -euo pipefail` variant) if it is the first non-blank line after the header. Nothing else is removed. **Blank-trim (LDR 2026-05-18):** like RN1, leading/trailing fully-blank lines of the result are trimmed (the header-terminating blank line previously leaked as a spurious first line — fixed). |
| RN3 | output from RN1 or RN2 containing `$NAME`, `${NAME}`, `"$NAME"`, `"${NAME}"` where `NAME` ∈ `display_env` keys | rendered | every occurrence is replaced by `display_env[NAME]` (NAME ∈ {PKG_<KEY>, REPO_<KEY>} — the renderable matrix, LDR 2026-05-17). Replacement is literal text substitution on the post-strip text; the surrounding quotes (if any) are preserved verbatim (`"$PKG_UV"` → `"uv:0.10"`; `$PKG_UV` → `uv:0.10`). |
| RN4 | `$(…)`, `` `…` ``, `$$`, `$@`, `$#`, `$?`, `$!`, `$0`, positional `$1`..`$9` / `${1}`.., **or** a `$NAME`/`${NAME}` that is an **ambient shell var** the author legitimately displays (`$HOME`, `$PATH`, `$PWD`, `$USER`, `$SHELL`, `$XDG_*`, … — anything not matching the RN5 fixture namespace) | rendered | **left verbatim** — no expansion, no error. Not fixture leakage; a walkthrough may legitimately show `cd "$HOME"`. Only `$PKG_<KEY>` (RN3) is substituted; only the RN5 fixture namespace errors. |
| RN5 | a `$NAME` / `${NAME}` whose `NAME` matches the **fixture namespace** `^(FQ\|TAG\|MARKER\|HOME_KEY)_` (PKG_/REPO_ are renderable, not banned) **or** is a runner-harness var (`REGISTRY`, `SCENARIO_TMP`, `OCX`, `OCX_HOME`) — i.e. anything `_build_script_env_from_packages` injects **other than** a `display_env`-keyed `$PKG_<KEY>` | publish task | **hard publish error**, no file written, message: `non-renderable fixture/harness variable '$NAME' in <script path> (slug <slug>): only $PKG_<KEY> is renderable in a displayed region (DE1/DE2); declare it or remove it`. **Supported-variable matrix (Codex F2 — closes the hole):** the SP0-safe renderable projected vars are **`$PKG_<KEY>`** (→ short ref) and **`$REPO_<KEY>`** (→ bare repo name, short minus `:tag`; LDR 2026-05-17 — needed for `ocx index list <repo>` / version-qualified `"$REPO_X:25.0.0"`) (→ `display_env[PKG_<KEY>]`, the canonical short ref). `FQ_*`/`TAG_*`/`MARKER_*`/`HOME_KEY_*` (registry-/run-dependent — no clean static reader form) and runner vars (`$REGISTRY`, `$SCENARIO_TMP`, `$OCX*`) are **banned from displayed regions**: a reader must never see a fixture repo, a UUID marker, or a harness path. They remain usable **outside** the displayed region (assertions). Verified: **0 of 54 live scripts** reference any of these in a body line (Codex's "live scripts already use `$REPO_*`/`$REGISTRY`/`$SCENARIO_TMP`" is false against the tree — no mass migration needed; the ban is a forward guard). A miss must fail loudly like EX5/PT4. Ambient shell vars are **not** errors (RN4). **Live-state correction (LDR — Phase-2 specify):** the earlier claim "0 of 54 live scripts reference these" was **wrong** (a flawed grep that filtered `#`-lines and under-matched). The proper RN5b static check finds **~27 displayed scripts** today referencing `$REPO_*`/`$SCENARIO_TMP`/`$REGISTRY` in their would-be-displayed body — Codex F2's premise was in fact **correct**. These are remediated in **Phase 3/4** (each gets a `# region cast` scoping the clean displayed commands so the `$REPO_*`/assertion lines fall *outside* the displayed region, or is rewritten to `$PKG_<KEY>`). RN5b is therefore a **Phase-4-completion gate**, expected RED until that migration lands — not a Phase-2 blocker. |
| RN6 | the rendered `.sh` | inspected | **No generated-marker / disclaimer header is prepended (LDR 2026-05-18).** The old `# Rendered for display from <slug>; not the tested source.` line is **removed** — it understated the guarantee. The display artifact is, by construction, the RN1/RN2-stripped + blank-trimmed + RN8-substituted projection of **the exact source the drift gate executes**; that source is proven green by the drift gate, and DE6 proves the only displayed-vs-executed token delta is the SP7 isolation prefix (canonicalised away) — so the displayed command **is** a tested command (DE6-canonical equivalence, EX10), not "not tested". The publish substring + RN8-parity tests prove the display is a faithful slice of that same source. The artifact is still **NOT contractually required to be standalone-valid bash**: a region slice may omit its enclosing `if`/`fi` (RN1); the runnable, asserted form is the full body the drift gate runs. |
| RN7 | the same script rendered twice with the same inputs | rendered | byte-identical output (pure function; feeds PT3 idempotency — the render runs **before** the PT3 content-hash compare so an unchanged source + unchanged `display_env` produces no write). |
| RN8 | the renderable substitution (RN3 algorithm) | factored | a single pure function `substitute_renderable(text, display_env)` is the **one** substitution implementation, used by `render_display` (after RN1/RN2 strip) and by the publish/test contract tests. **It is NOT applied by the drift-gate executor** (EX10 — SP7 blocker; the drift gate runs the raw body so the SP7-prefixed `script_env` ref resolves). It performs only the RN3 replacement (`$PKG_<KEY>`/`$REPO_<KEY>` keys present in `display_env`); leaves non-renderable `$VAR` verbatim; raises **nothing** (RN5/RN5b stay the *display-region* gate). The displayed↔executed link is the *DE6-canonical-equivalence* relation (EX10), not a shared execution path. Canonical impl in the test subsystem (`substitute_renderable`); the website publish script carries a behaviour-mirrored `_substitute_renderable` (PT6: website never imports the test subsystem) guarded by a parity test (parity-gate pattern, cf. DE5). |
| RN9 | a displayed (`# doc:` present) script whose **rendered display output** contains verification scaffolding — a capture assignment `name="$(…)"`, a `[[ … ]]` test, an assertion `\|\| exit`/bare `exit <n>`, or a `>&2` error echo | **verify-path static check** (`test:parallel`, no publish/Docker — renders each displayed script via `render_display` and scans the *result*) | **fails `task verify`** listing `<slug> (rendered line N): <line>`. **(LDR 2026-05-18, user-reported.)** The reader-facing snippet and the cast must show only documented commands; captures + assertions belong **outside** the `# region cast` block (the drift gate still runs the full body — the assertion stays tested, just not shown). Fix = region-scoping (the established convention: clean `ocx` lines in-region, `out="$(…)"`/`[[…]]` after `# endregion cast`). Parity with RN5b/RG3. Render is the single source of truth for "what is shown", so the gate checks the rendered text, not the raw file. |
| RN5b | a displayed (`# doc:` present) script whose body/region references a fixture/harness var **outside the renderable matrix** — `$FQ_*`/`$TAG_*`/`$MARKER_*`/`$HOME_KEY_*`/`$REGISTRY`/`$SCENARIO_TMP`/`$OCX*` (i.e. RN5's error class; the renderable matrix `$PKG_<KEY>` + `$REPO_<KEY>` is excluded — LDR 2026-05-17) | **verify-path static check** (in `test:parallel`, no publish, no Docker) | **fails `task verify`** with `non-renderable var '$NAME' in displayed region of <script> (slug <slug>): renderable matrix is $PKG_<KEY>/$REPO_<KEY> (DE1/DE2); move it outside the displayed region or rephrase`. RN5b is the **static pre-publish surface** of the RN5 rule (RN5 itself is the publish-task *runtime* hard error — a different code path/phase). RN5b catches it on the verify path before `website:build`, parity with NC4/RG3. **Live state (LDR — Phase-2 specify):** Codex F2's premise was **correct** — ~27 displayed scripts referenced `$REPO_*`/`$SCENARIO_TMP`/`$REGISTRY`; remediated in Phase 3 (region-scoped, or `$REPO_*` now renderable). RN5b keeps the tree at zero violations. |

**Edge cases / error modes:**

- Empty region (markers adjacent) ⇒ RN1 yields empty output ⇒ publish task
  hard error `empty cast region in <path> (slug <slug>)` (a slug that renders
  to nothing is an authoring bug, treated like RN5).
- A `# region cast` present on a script with `# cast: false` and `# doc:` set
  ⇒ RN1 still applies (the region is the *display* selector independent of
  cast opt-in — ADR H-3; the marker name is a documented misnomer for this
  case, RG2). EX9's "exactly one region" arity is enforced by the parser only
  when `# cast: true`; for display-only the publish task enforces the same
  ≤1-region arity and errors `display script has >1 cast region` on >1.
- `$NAME` inside a single-quoted string (`'$PKG_UV'`) ⇒ still substituted
  (RN3 is text substitution, not a shell-semantics emulation; documented so a
  tester asserts the literal-text behaviour rather than shell behaviour). A
  script that needs a literal `$PKG_UV` shown unexpanded is out of scope —
  the layer's purpose is expansion; no escape hatch pre-1.0 (YAGNI).

## 6f. Component Contract — `display_env` Seam Schema + Static Accessor (DE1–DE5)

> ADR Decision H-1 / H-2. The render layer's variable values reach the
> website **only** through the existing `task test:doc-scripts:list` JSON seam
> (PT6). The website never imports `test/` (PT6 unchanged, strengthened).

| ID | Property |
|---|---|
| DE1 | The seam export entry (`DocScriptExportEntry`, canonical in `test/src/doc_scripts.py`) gains one field `display_env: dict[str, str]` mapping **bare env-var name → canonical display value** for the script's resolved `# state:` (e.g. `{"PKG_UV": "uv:0.10", "PKG_CMAKE": "cmake:3.28"}`). Keys are the renderable matrix `PKG_<KEY>` (→ short ref) and `REPO_<KEY>` (→ bare repo, short minus `:tag`) (no `$`) — both are SP0-safe static reader forms (LDR 2026-05-17). `$FQ_*`/`$TAG_*`/`$MARKER_*`/`$HOME_KEY_*`/runner vars have no clean static reader form and are non-renderable (RN5/RN5b). Values are the canonical short reference a reader would type. JSON shape: `[{path, slug, cast, expect, display_env}, …]`. `display_env` is **always present** (possibly `{}` for a tested-only/no-package script), never `null`. **Wire-format note (Living Design Record — Phase-1 stub ambiguity #3):** the seam is JSON; `cast_region: tuple[int,int] | None` serialises as a 2-element array or `null`. The DE5 parity gate compares the *Python annotation strings* (both TypedDicts declare `tuple[int, int] | None`) so parity holds; but a consumer (`publish_doc_scripts.py`) reading the JSON receives `list[int] | None`. Consumers MUST treat `cast_region` positionally (`region[0]`, `region[1]`) and not assume a `tuple` instance. `title` is populated from `DocScriptMeta.title` (always a non-empty `str` for an exported/parseable script — `str | None` annotation kept for parity/forward-safety; `None` is reserved, not produced by the current export which skips parse-error files). |
| DE2 | `display_env` is produced by a NEW **static, zero-I/O** accessor `StateProvider.declared_display_env() -> dict[str,str]`. It derives `{PKG_<KEY>: <short_ref>}` from each provider's **declared** package names — static metadata, **not** by calling `provision()` / `setup()` / any `SETUPS` function. **Shape (Living Design Record — Phase-1 stub ambiguity #2 resolved):** the declared surface is a **module-level static table keyed by the family-qualified state name** — `DECLARED_PACKAGES: dict[str, dict[str, str]]` mapping `"setup:<name>"` / `"scenario:<Name>"` → `{display_key: short_ref}` (e.g. `{"setup:basic": {"uv": "uv:0.10"}, "setup:multi-version": {"corretto": "corretto:21.0.0"}}`). It is **not** a single shared class attribute on `SetupAdapter`/`ScenarioAdapter` (those wrap different providers per name — a shared class dict cannot hold per-name values). Each adapter instance knows its bound state name; `declared_display_env()` looks itself up in the table and projects keys to `PKG_<KEY>` (`display_name.upper().replace("-","_")`). The table is a hand-authored literal (SP0-safe; DE6 cross-checks it against the provisioned truth; DE0 is the oracle). For `multi-version`/`variants` the declared value MUST match the runtime "first version wins" projection (`versions[0].short`) — that ordering fact is exactly what DE6 guards. `<KEY>` derivation matches `_build_script_env_from_packages` exactly (`display_name.upper().replace("-", "_")`) so the rendered token namespace is identical to the runtime `$PKG_*` namespace. The value is the canonical short ref (`uv:0.10`), **not** the SP7 UUID-prefixed actual repo (which does not exist statically and must never appear in a rendered page). |
| DE3 | The export resolves each script's `# state:` (default `setup:basic`, EX6) to its provider and calls `declared_display_env()` for that provider only. A script whose state has no declared packages ⇒ `display_env == {}` (valid; RN3 then expands nothing, RN5 fires only if the script references a var). Resolution reuses `resolve_state` (no new resolver). |
| DE4 | **SP0 invariant extended to the accessor.** `declared_display_env()` and the seam export performing it do **zero registry I/O / zero network / no Docker socket touch / no `provision()` call**. Tester: import the provider registry and run `task test:doc-scripts:list` with **no registry fixture / no docker compose up**, assert it exits 0 and emits valid JSON with populated `display_env`, and assert (monkeypatch / network-deny) that no OCI push or HTTP call occurred — parity with the existing SP0 import-time zero-network test. This is the contract that lets publish-time render stay SP0-safe (ADR H-2). |
| DE5 | **TypedDict parity gate.** `DocScriptExportEntry` (canonical, `test/src/doc_scripts.py`) and the hand-mirrored `_DocScriptExportEntry` (`website/scripts/publish_doc_scripts.py`) MUST have identical key sets and per-key type annotations. A verify-path static test asserts the two `__annotations__` are equal (key set + stringified types). **Mechanism (spec-compliance review #10):** extract annotations **without importing the website module** (it is stdlib-only today but the test must not depend on that) — parse both TypedDicts' `__annotations__` via `ast` from source text. The test subsystem is allowed to *read* website source for a static structural check (parity with NC2/PT8 scope clarification); it does not runtime-import `website/`. Tester: add a field to one only, assert the gate fails with both names + the differing keys. Closes the pre-existing unguarded manual-sync coupling. |
| DE0 | **Hat-1 characterization oracle (committed first, workflow-refactor Phase 1).** For every legacy `SETUPS` name and every `Scenario` subclass, a test pins the post-`provision()` `provider.packages` short refs to a literal expected map. This is the oracle DE2/DE3 must match and DE6 cross-checks. Before Phase-1 stubs exist the oracle's import of `StateProvider.declared_display_env` fails as `ImportError`/`AttributeError` (the expected pre-stub red — a *structural* failure, not behavioural). It runs in the provisioned suite (it calls `provision()`); it is **not** the SP0 static path. |
| DE6 | **Declared-vs-provisioned cross-check (value-correctness; Round-1 F4, Codex F1).** A **provisioned acceptance test in the `test:parallel` collection** — the *same* collection as the drift gate `test_doc_scripts.py`, which already provisions `registry:2` (ADR Decision A). `task verify` phase-2 (`.verify:build-test` → `test:parallel`) runs that provisioned collection, so **DE6 runs under `task verify` — it is the mandatory final gate, not an optional side-suite.** For each provider: after `provision()`, assert `declared_display_env()` key set + values == the **canonical** projection `{f"PKG_{k.upper().replace('-','_')}": canonical(p.short)}` where **`canonical(s)` strips the SP7 isolation prefix** `^[ts]_[0-9a-f]{8}_` (family-specific: `t_<8hex>_` for `SetupAdapter`, `s_<8hex>_` for `ScenarioAdapter`). **Living Design Record (2026-05-17, Phase-1 implement):** `PackageInfo.short` is `<actual-repo>:<tag>` and the actual repo is SP7-prefixed, so raw `p.short` carries the prefix; `declared_display_env()` returns the *prefix-free* short a reader actually types (the whole render purpose). DE3 (declared == DE0-oracle canonical) and DE6 therefore compare against `canonical(p.short)`, **not** raw `p.short` — an oracle that used raw `p.short` is wrong (it would demand the table contain UUID-prefixed repos, contradicting the feature). Catches `DECLARED_PACKAGES` *value staleness* (e.g. `multi_version` keys `$PKG_CORRETTO` to `versions[0].short` — a "first wins" ordering fact in the setup body, not in any declarable name; add a version at index 0 and runtime ≠ declared). RN5/DE4 cannot catch this (known var, stale value). **Honest guarantee scope:** the *pure-static subset alone* proves only seam shape + SP0; but `task verify` is **not** static-only — it runs the provisioned `test:parallel` suite, so a green `task verify` **does** gate value-correctness via DE6. DE6 must be authored as a normal collected `test:parallel` case (no `skip`/opt-in marker) so it cannot be silently excluded. |

**Consumer behaviour:** `publish_doc_scripts.py` reads `display_env` per entry
from the seam JSON and passes it to the render pass (§6e). It does **not**
recompute it and contains **no** `test/` path literal (PT6 invariant holds —
the new field rides the existing subprocess→JSON boundary).

## 6g. Component Contract — Unified Region Grammar (RG1–RG3)

> ADR Decision H-3 / G. One marker grammar serves drift gate (full body),
> cast replay (region only), and display render (region only).

| ID | Property |
|---|---|
| RG0 | **`cast_region` is populated whenever a `# region cast` / `# endregion cast` block is present, independent of `# cast:` value** (LDR — Phase-2 specify; ADR H-3: the region is the *display* selector, not a cast-only concern). `parse_doc_header` records the 1-based inclusive span for any script carrying the markers; the EX9 *arity* hard-error (`≠1` region) still fires **only** when `# cast: true` (a display-only script with no region simply renders full-body-minus-header per RN2; with >1 region the publish task errors per §6e edge-cases / RN1). This lets a display-only (`# doc:`, `# cast: false`) script use a region to scope its clean displayed commands (excluding `$REPO_*`/assertion lines) — the mechanism RN5b remediation relies on. Parser change lands in Phase 3. |
| RG1 | The **only** region marker is `# region cast` / `# endregion cast` (exact stripped-line match, as `parse_doc_header` already enforces — §1.3, EX9). It is simultaneously: (a) ignored by the drift gate (whole body runs — §2), (b) the cast replay selector (CA5), (c) the display render selector (RN1), (d) a **native VitePress region** (`#\s*#?region` / `#\s*#?endregion` per `vuejs/vitepress` `snippet.ts` — verified Axis 1) so the marker lines are auto-excluded by VitePress even though, per ADR H-2, the prose transcludes the **pre-rendered** file (no `#region` in the `<<<`) so the native selector is a safety belt, not the primary mechanism. |
| RG2 | No second grammar (`# region doc` etc.) exists or is accepted (ADR H-3). A display-only, no-cast multi-step script either carries a `# region cast` block (display-narrowed; the name is a documented misnomer for this case — RN6/§6e note) **or** carries no region and is rendered full-body-minus-header (RN2). An unknown `# region <x>` / `# endregion <x>` where `<x> != cast` is **not** a recognised marker: it is left as a literal comment line by the parser (no error) but, because RN2 keeps non-metadata comments, it would appear verbatim in the rendered output — flagged by RG3. |
| RG3 | A verify-path static check: any displayed (`# doc:` present) script containing a line matching `^#\s*#?(end)?region\b` whose region name is **not** `cast` fails the gate `unrecognised region marker '<line>' in <script> (slug <slug>): only 'cast' regions are supported (ADR H-3)`. This prevents a typo'd `# region cats` silently shipping a marker comment into a rendered page and prevents accidental reintroduction of a second region grammar. Runs in the same `test:parallel` collection as NC1–NC4. |

## 6h. Component Contract — Missing-Region Silent-Fallback Guard (NC4)

> Extends §6b (NC1–NC3). Research Axis 1 / GH vuejs/vitepress #4625 (unfixed,
> PR #5014 open): a VitePress `<<<` referencing a **missing** region returns
> the **entire file** silently — a region-name typo would dump headers,
> assertions, and unrendered `$PKG_*` into a published page with no error.

| ID | Given | Then |
|---|---|---|
| NC4 | a walkthrough page transclusion `<<< @/_scripts/<file>.sh#<region>{…}` (i.e. any `<<<` that names a `#region`) | the verify-path check **fails** with `region '#<region>' referenced by <page> for <file> — doc-author <<< references must not name a region (ADR H-2: transclude the pre-rendered file with no #region); the publish render already selected the region`. Rationale: under ADR H-2 the published file **is** the rendered region body, so a correct prose `<<<` never carries `#region`. Any `#region` in a doc-author `<<<` is therefore by-definition wrong and is the exact shape the silent-fallback trap exploits — banning it on the verify path makes the trap unreachable from the doc-author surface. |
| NC4b | the publish-task render encountering a **malformed region** in a `# doc:` script: (a) `# region cast` with **no matching `# endregion cast`** (unclosed — VitePress `findRegion` returns null ⇒ same GH #4625 whole-file dump), (b) **>1** `# region cast` block (VitePress silently takes the *first*, dropping the rest — researcher Q2-B), or (c) a publish-internal `#region` selector against a file lacking that region | the publish task itself **hard-errors before writing**: `malformed cast region in <file> (slug <slug>): <unclosed|duplicate(<n>)|missing '#<region>'>` — never relying on VitePress's silent fallback. (a)/(b) overlap EX9 for `# cast: true` but NC4b also covers **display-only** `# doc:` scripts where EX9's `# cast: true` precondition does not fire — closing the silent class the spec-compliance + research panels both flagged. Defence in depth: H-2 means the render selects in Python and emits no `#region` `<<<`, but any future path that does must fail loud. |
| NC4c | every walkthrough `<<<` reference (NC2-resolved) carries **no** `#region` fragment | check passes — binding is to a pre-rendered flat file (ADR H-2), drift-gated together with command execution and slug resolution (NC2/NC3). |

NC4 runs in the same `test:parallel` collection as NC1–NC3 and RG3. A green
`task verify` now proves: documented commands execute (drift gate), every
`<<<` resolves to a published slug (NC2/NC3), no inline ungated `ocx`
invocation exists (NC1), no unrecognised region grammar leaked (RG3), and **no
`<<<` can trigger the VitePress missing-region whole-file dump** (NC4).

## 6i. Component Contract — One-Tree Invariant (EQ1–EQ3, Codex Finding 1 RESOLVED)

> ADR Decision H-4a (one-tree convergence). Codex Finding 1 /
> `project_doc_cast_two_tree_drift`: `test/doc_scripts/` and
> `test/recordings/scripts/` were two trees; ~10 overlapping slugs could
> silently diverge. **Resolution: convergence, not an equivalence gate** —
> after migration there is exactly one tree, so EQ1–EQ3 are a *structural
> guard that the second tree does not silently reappear*, not a byte-diff.

| ID | Property |
|---|---|
| EQ1 | There is exactly **one** doc/recording script tree: `test/doc_scripts/`. After the §7 migration, `test/recordings/scripts/` contains **no** `*.sh` (directory removed or empty). Tester: assert `test/recordings/scripts/**/*.sh` is empty/absent; assert every recording-backed demo resolves to a file under `test/doc_scripts/`. |
| EQ2 | No slug is backed by two files (already PT4 for `# doc:` collisions); EQ2 extends this to **cast-bearing** scripts: no two `# cast: true` scripts share a `# doc:` slug (PT4 covers it) and no `# cast: true` script outside `test/doc_scripts/` exists (EQ1). One slug ⇒ one source ⇒ one render ⇒ one cast — drift class structurally impossible. |
| EQ3 | The recordings pipeline (`website:build`) consumes its scripts from `test/doc_scripts/` via the **same** seam (`task test:doc-scripts:list`) as the publish task and drift gate — there is no second discovery path. Tester: grep the recordings taskfile/runner for a `test/recordings/scripts` glob, assert none; assert the cast step iterates the seam export filtered to `cast == true`. |
| EQ3b | **Cast-orphan sweep (LDR — Phase-3 cast audit).** Root cause of "broken casts": no sweep existed, so `website/src/public/casts/*.cast` accumulated dead files whose source script was deleted/renamed (audit found 5: `catalog`, `env-offline`, `info`, `profile`, `shell-env` — 3 showing *removed* CLI commands, none page-referenced). The recordings step MUST, after generating casts, remove any `*.cast` not backed by a current `cast == true` seam entry (slug-derived nested name), scoped like PT5 (manifest of task-owned casts; never delete foreign/non-`.cast`). Tester: stale `foo.cast` with no backing script ⇒ removed on next build; a foreign file survives. Casts are gitignored generated content, so this sweep is what keeps deploys orphan-free. |
| EQ-T | **Transitional fold-equivalence gate (one-shot Hat-1 — Round-1 F3/F5, tightened per Codex F3).** EQ1–EQ3 are *structural* and **do not** catch a *lossy fold* when the ~8 colliding slugs are merged. For the convergence phase only: for each migrated recordings script, assert the converged script's rendered cast region (RN1, post `$PKG_*`-rewrite normalised back to display via `display_env`) is the **ordered command sequence, byte-for-byte equal** to the pre-migration recordings script's command sequence — **not** set-equality (a reordered/duplicated `install/select/which` flow preserves the set but breaks the demo; Codex F3). An intentional delta requires an explicit per-script waiver recorded in the migration commit. This is the migration-correctness oracle H-4b would have provided; **deleted at the end of the convergence phase** (one-time safety net, not a standing tax — ADR H-4a bullet). |

EQ1–EQ3 run in the `test:parallel` collection. They make the
two-tree-divergence failure mode **unrepresentable** (no second tree, no
second discovery path) rather than detectable-after-the-fact — the stronger
fix per ADR H-4a. **Codex Finding 1 is RESOLVED (by convergence, ADR H-4a);
EQ1–EQ3 guard against silent reintroduction.**

> **Status 2026-05-18 (/swarm-review max review-fix):** EQ1–EQ3 + EQ3b
> implemented as standing tests (`test/tests/test_doc_scripts_one_tree.py`,
> 10 cases, no skip marker, sanity-checked: re-adding a `.sh` under
> `test/recordings/scripts/` fails EQ1+EQ2). **EQ-T** ran retroactively
> (reconstructed from `git show 19f0a2dc:test/recordings/scripts/*.sh` —
> the migration had landed before the oracle): **20/21 lossless**; one
> explicit waiver `getting-started/env` (additive `ocx package install
> "$PKG_CORRETTO"` prerequisite, not a lossy fold) pinned by standing
> `test_eqt_residual_getting_started_env_region_shape`. EQ-T not retained
> as a standing gate (ADR H-4a — no perpetual equivalence tax).

## 9. Error Taxonomy & Edge Cases

| Case | Detected by | Behaviour |
|---|---|---|
| Script with no `# doc:` | publish task | not published (PT1); still runs as drift-gate case |
| Orphan published copy (task-owned) | publish task | removed via manifest scope (PT5) |
| Foreign file/subdir in `_scripts/` | publish task | **not** deleted — outside `.published.json` manifest (PT5) |
| `# state:` family prefix | explicit `setup:`/`scenario:` only; unqualified rejected (EX4); no implicit union, no collision possible |
| `scenario:basic` | invalid — scenario family has no `basic` key (keys are class names; SP6) |
| Provisioning on import | provider registry | forbidden — zero registry I/O at import (SP0) |
| `# cast: true` with 0 or >1 cast regions | header parser | hard parse error (EX9) |
| `# cast: true`, multiline/heredoc inside cast region | cast layer | author error — region must be one PTY-safe command per line (§1.3); enforce in parsing/tests |
| `# expect:` golden mismatch | drift-gate executor | unified diff in failure text (GO2); missing golden ⇒ hard fail (GO3) |
| `# doc:` slug collision (two scripts) | publish task | hard fail, no writes (PT4) |
| Unknown / unqualified `# state:` value | drift-gate executor | case fails with expected-form + available-families message (EX4) |
| Unknown header key (typo) | header parser | case fails (EX5) — typos cannot silently disable a check |
| Invalid slug grammar | header parser / publish task | hard fail with the offending value |
| Windows | drift-gate collection | all doc-script cases skipped (EX7), parity with scenarios; Windows command coverage stays in the pytest acceptance suite |
| `# cast: true` but provider has no usable display map | cast layer (website:build) | recordings step fails loudly; verify path unaffected (cast is additive) |
| `<<<` points at a non-published slug | **verify-path NC2 check** | fails `task verify` naming the page + unresolved ref (no longer deferred to `website:build` — Codex finding 2) |
| Projected-var `$PKG_*`/`$REPO_*`/`$FQ_*`/`$TAG_*`/`$MARKER_*`/`$HOME_KEY_*` in displayed region with no `display_env` entry (typo'd/undeclared fixture var) | publish-task render (RN5) | **hard publish error**, no file written, names script + slug + missing var (never shipped literal/UUID) |
| Ambient shell var (`$HOME`, `$PATH`, `$PWD`, …) or `$(…)`/positional `$1`/`$$`/`$@` in displayed region | publish-task render (RN4) | **left verbatim** — not fixture leakage; only `display_env`-keyed names expand (RN3), only projected-var misses error (RN5) |
| Empty `# region cast` (markers adjacent) on a `# doc:` script | publish-task render | hard error `empty cast region` (slug renders to nothing — authoring bug) |
| `# region <x>` where `<x> != cast` on a displayed script | verify-path RG3 | fails `task verify`: only `cast` regions supported (ADR H-3); prevents typo'd marker shipping into a page + second-grammar reintroduction |
| Doc-author `<<<` carrying a `#region` fragment | verify-path NC4 | fails `task verify` — published file is pre-rendered (ADR H-2); a `#region` `<<<` is the exact shape that triggers the VitePress GH #4625 silent whole-file dump |
| `declared_display_env()` performs registry/network I/O on import or export | DE4 (SP0-extended) test | forbidden — zero I/O; parity with the existing SP0 import-time zero-network test |
| `DocScriptExportEntry` ↔ `_DocScriptExportEntry` key/type drift | verify-path DE5 parity test | fails naming both modules + differing keys (closes the pre-existing unguarded manual-sync coupling) |
| A `*.sh` reappears under `test/recordings/scripts/` (or a 2nd discovery path) | verify-path EQ1/EQ3 | fails — one-tree invariant; Codex Finding 1 stays resolved (cannot silently re-fork) |

## 10. Testable Contract Index (for `/swarm-execute` tester)

Write failing tests for: EX1–EX9 (executor; EX4 family-qualified state, EX9
cast-region arity) + GO1–GO3 (golden output); SP0 (zero-I/O on import), SP1
(explicit-family resolution, no collision possible), SP2–SP6 (adapter
behaviour-equivalence to legacy; SP6 uses real class keys `BasicPackage`…);
CA1–CA5 (cast opt-in; CA2 slug-named cast; CA5 cast-region replay only); PT1–PT8
(publish task — PT3 idempotency, PT4 slug collision, PT5 manifest-scoped sweep +
foreign-file survival, PT6 JSON-export discovery, PT8 reverse-leak removed);
DG1–DG3 (failure ergonomics + green-suite invariant); **NC1–NC4** (verify-path:
no inline `ocx ` blocks; every `<<<` resolves to a published slug; binding
drift-gated with execution; NC4 no `<<<` may name a `#region`).

ADR Decision H families (added 2026-05-17):

- **RN1–RN7** (render layer): RN1 region-only output, RN2
  full-body-minus-header/shebang/`set -euo pipefail`, RN3 `$VAR`/`${VAR}`/
  quoted-var expansion via `display_env`, RN4 ambient shell vars (`$HOME`…)
  + shell-special/`$(…)` left verbatim, RN5 **projected-var-prefix miss**
  (`PKG_/REPO_/FQ_/TAG_/MARKER_/HOME_KEY_` not in `display_env`) = **hard
  publish error** (not left literal), RN5b **verify-path static** scrub of
  the same class (static surface of RN5; fails `task verify` before publish),
  RN6 rendered `.sh` is a display artifact (not contractually valid bash),
  RN7 pure/idempotent (feeds PT3).
- **DE0** (Hat-1 oracle, committed first): for every `SETUPS` name + every
  `Scenario` subclass, pin the post-`provision()` `provider.packages` short
  refs — the oracle `declared_display_env()` (DE2) must match (DE3) and DE6
  cross-checks against. Fails as `ImportError`/`AttributeError` before
  Phase-1 stubs exist.
- **DE1–DE5** (seam schema + accessor): DE1 `display_env` JSON field (always
  present, never null), DE2 static `declared_display_env()` over **declared**
  names (no `provision()`), DE3 per-state resolution via `resolve_state` (==
  DE0 oracle), DE4 **SP0 extended** (zero-I/O accessor — import/no-network
  test), DE5 `DocScriptExportEntry`↔`_DocScriptExportEntry` parity gate.
- **DE6** (declared-vs-provisioned cross-check — value-correctness, F4 /
  Codex F1): a **provisioned `test:parallel` case** (same collection as the
  drift gate, which already provisions `registry:2`) ⇒ **runs under
  `task verify`** (phase-2 `.verify:build-test`) as the mandatory final
  gate, not an optional side-suite. After `provision()`, assert
  `declared_display_env()` keys+values == the `PKG_<KEY>`→`pkg.short`
  projection of `provider.packages`. Catches `DECLARED_PACKAGES` value-
  staleness (e.g. `versions[0]` ordering) RN5/DE4 cannot. Authored as a
  normal collected case (no `skip`/opt-in marker) so it cannot be silently
  excluded.
- **RG1–RG3** (unified region grammar): RG1 single `# region cast` marker
  serves drift/cast/display/native-VitePress, RG2 no `# region doc` second
  grammar, RG3 verify-path reject of any non-`cast` region marker on a
  displayed script.
- **EQ1–EQ3 + EQ-T** (one-tree invariant — Codex Finding 1 RESOLVED by ADR
  H-4a): EQ1 no `*.sh` outside `test/doc_scripts/`, EQ2 one slug ⇒ one source
  ⇒ one cast, EQ3 single seam discovery path (no `test/recordings/scripts`
  glob), **EQ-T** one-shot Hat-1 fold-equivalence (Phase-3-only; deleted
  post-convergence; depends on RN1+`display_env` so runs after P1/P2).
- **Also indexed** (referenced as gates by the plan; full contracts in §3/§6h/§6i):
  **SP7** (unique-prefix parallel isolation — `t_<8hex>_` repo prefix),
  **SP8** (`StateProvider.work_dir` for publisher recordings), **NC4b**
  (publish-task hard-error on unclosed / >1 `# region cast`, incl.
  display-only `# doc:` scripts), **NC4c** (every walkthrough `<<<` carries
  no `#region` — clean-binding positive check).

Migration assertions: every page in §7.2 contains zero stale forms from the
mapping table; every rewritten walkthrough snippet is backed by a transcluded
`_scripts/<slug>.sh` (no `#region` fragment — NC4) whose drift-gate case
passes; the published file's rendered text contains no `# <key>:` header line,
no `set -euo pipefail`, and no literal `$PKG_*` (RN2/RN3); NC1 closes the
silent re-drift door (a new inline example added later would otherwise be
ungated); EQ1 proves the recordings tree converged (Codex Finding 1 cannot
silently re-fork).
