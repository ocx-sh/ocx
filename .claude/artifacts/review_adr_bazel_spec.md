# Review: `adr_bazel_build_adoption.md` — spec focus (testability, internal consistency, traceability)

Reviewer: `reviewer` (focus **spec**), Round 1 of `/hex-architect high`. Model: opus.
Subject: `/home/mherwig/dev/ocx-sion/.claude/artifacts/adr_bazel_build_adoption.md` (1279 lines, Status Proposed)
plus the amendment at `/home/mherwig/dev/ocx-sion/.claude/artifacts/adr_crate_split_workspace.md:19`.
Method: every cited file opened at the cited line with `sed -n`/Read; `git` reads through `/usr/sbin/git`.
Findings anchored by § heading + quoted phrase, never by line number alone.

---

## BLOCK

### B1 | Block | § Stage 3 — casts | "Granularity ruling: 72 targets, one per script"

**What is wrong.** Only **39** of the 72 `test/doc_scripts/*.sh` are cast-enabled. The recorder
runs a script only when its header carries `# cast: true`; the other 33 are doc snippets published
to the website by `scripts:publish` and never recorded. A 72-target ruling mints 33 targets that
record nothing, and makes A3's green ("every cast is a `local`-tagged target that records")
unreachable — a permanently-red acceptance criterion.

**Evidence.**
- `/home/mherwig/dev/ocx-sion/test/recordings/test_recordings.py:1-4`: `"""Generic test that runs each cast-enabled doc script as a recording.` … `For each .sh file in doc_scripts/ with cast: true:`
- `grep -l "cast: true" test/doc_scripts/*.sh | wc -l` → **39** (and `grep -l "cast:"` → 39, so no `cast: false` rows).
- `find website/src/public/casts -name '*.cast' | wc -l` → **39** — the live output set matches the header count exactly.
- `/home/mherwig/dev/ocx-sion/test/doc_scripts/getting-started__install.sh:2-4`: `# state: setup:basic` / `# cast: true` / `# doc: getting-started/install`.
- The claim diff never asserted 72 *casts*; it asserted 72 *files* and hedged — `discover_bazel_full_adoption.md:30`: "implies 72 individual Bazel actions/targets **if done literally per-script**".

**Concrete fix.** Rule **39 cast targets**, enumerated from the `cast: true` header through the
existing discovery export (`test/scripts/doc_scripts_list.py`, the PT6 seam that already emits the
JSON the website publish task consumes). State in the same bullet that the remaining 33 scripts are
inputs to the site rule via `scripts:publish`, not cast targets. Update A3's scope line from
"all 72 `test/doc_scripts/*.sh` as cast targets" to the 39, and re-word its green to name the
artifact (`website/src/public/casts/<doc>/<name>.cast`).

---

### B2 | Block | § Stage 1 — Targets and pins | "One `rust_library` + one `rust_test` per member"

**What is wrong.** The specified target shape cannot reach the 8213 floor the ADR keeps. The floor
was produced by `cargo nextest list --workspace`, which sums test cases over **every** rust-suite —
including the 14 integration-test binaries under `crates/*/tests/`. One `rust_test` per member
compiles only the lib's `#[cfg(test)]` tests; `tests/*.rs` each need their own `rust_test`. So the
BEP-derived count is short by the integration set on a perfectly healthy tree, and
`task rust:test:floor` reds forever. The reader floor written into the same contract
("fewer than 20 test targets reported") encodes the same wrong shape, so it passes on the
under-counting graph instead of catching it.

**Evidence.**
- `/home/mherwig/dev/ocx-sion/taskfiles/rust.taskfile.yml:607-610`: `count=$(cargo nextest list --workspace --release --locked --message-format json | python3 -c 'import json, sys; suites = json.load(sys.stdin)["rust-suites"]; print(sum(len(s["testcases"]) for s in suites.values()))');`
- `crates/NEXTEST_FLOOR` → `8213` (VERIFIED, matches the ADR).
- Integration-test binaries present, counted per crate: `crates/ocx_cli/tests/` 3 files, `crates/ocx_index/tests/` 3, `crates/ocx_package/tests/` 2, `crates/ocx_schema/tests/` 4, `crates/ocx_test_support/tests/` 2 — **14 suites**, carrying ≥135 `#[test]`/`#[tokio::test]` attributes (`crates/ocx_test_support/tests/boundary.rs` alone has 56, `workspace_structure.rs` 35).
- ADR § Stage 2 floor contract: "**Exit 1** | Zero `TestResult` events, or the BEP file absent/empty, or fewer than **20** test targets reported."
- ADR § Acceptance A1 scope: "the 20 workspace members' `rust_test` targets" — the same 20.

**Concrete fix.** Change the contract to "one `rust_library` per member; one `rust_test` for the
lib's unit tests **plus one `rust_test` per `crates/*/tests/*.rs`**" (20 + 14 = 34 test targets
today). Re-derive the reader floor from an enumerated per-crate/per-suite table committed beside
`crates/NEXTEST_FLOOR` — a bare `>= 20` cannot distinguish "integration suites missing" from
"clean". Add "the BEP target count equals the `rust-suites` count from `cargo nextest list --workspace`
on the same commit" as an explicit WP-1b verification item beside the two already named.

---

### B3 | Block | § Decision Outcome | "Both are `local`-tagged … they get **no shared-cache reach at all**"

**What is wrong.** Self-contradiction on the one control the ADR calls load-bearing. § Decision
Outcome says stages 3 and 4 are *both* `local`-tagged and therefore get no shared-cache reach.
§ Stage 3 says the opposite for half of stage 3: the website rule is the **only** sandboxed,
shared-cache-eligible target in the whole design, and is the sole reason the hermeticity gate
exists. A planner reading § Decision Outcome — the section that sets what gets built and in what
order — concludes the site rule needs no hermeticity precondition and no `no-remote-cache`
holding pattern, dropping the ADR's only defence against a cross-machine-wrong cache entry.

**Evidence (same file, three places).**
- § Decision Outcome: "**Stages 3 and 4 have materially weaker measured justification.** Both are `local`-tagged, which (per `bazel.build/reference/be/common-definitions`) already implies `no-remote-cache` — they get **no shared-cache reach at all**."
- § Stage 3 › Website: "**Tags: `requires-network` only** — this is the one stage of four that stays sandboxed and therefore the one stage eligible for the shared cache."
- § Cache staging ruling 2: "**The website rule is the only sandboxed cacheable stage**, and therefore the only place an undeclared-input bug produces a cross-machine-wrong entry."
- § Considered Options A and § Consequences both state it correctly ("**Casts and acceptance** are `local`-tagged"), so § Decision Outcome is the single outlier.
- Corroborated by the research the ADR cites: `research_bazel_cache_trust_boundary.md:30` — "The stage that needs an *explicit* audit is the **website build** (tagged only `requires-network` in the plan, not `local`/`no-sandbox`)".

**Concrete fix.** Replace "Stages 3 and 4 … Both are `local`-tagged" with "**The casts half of stage 3
and all of stage 4 are `local`-tagged** … the website rule is the one sandboxed cacheable target and
carries the hermeticity check as a landing precondition (§ Cache staging ruling 2)."

---

## HIGH

### H1 | High | § Stage 4 — acceptance | "Tagged **`local`**, per the `SCOPED_ROWS` glob for the cache unit"

**What is wrong.** `SCOPED_ROWS` is not a partition, so it cannot define a cache unit, and A4's red
state cannot be satisfied as written. Three defects in the live table: (i) globs **overlap** — the
same test module is named by two crate rows, so a per-row target duplicates execution and "exactly
that module re-runs" is false by construction; (ii) two rows are the literal word `escalate`, which
names no files at all and has no target shape; (iii) the row an ordinary commit hits most,
`ocx` (the CLI crate), is one of the two `escalate` rows. Separately, the ADR never states the
concurrency contract: today the suite is one pytest-xdist process against one docker-compose stack;
under Bazel each `sh_test` spawns its own `uv run pytest`, and nothing says how N of them share the
registry/zot/sigstore ports.

**Evidence.** `/home/mherwig/dev/ocx-sion/test/taskfile.yml:371-414` (`SCOPED_ROWS: map:`), 20 rows:
- `ocx_oci: … tests/test_logging.py` and `ocx_trust: tests/test_trust_*.py tests/test_logging.py` — overlap.
- `ocx_console: … tests/test_completion_ascii.py` and `ocx_shell: … tests/test_completion_ascii.py` — overlap.
- `ocx_store: … tests/test_windows_shim.py` and `ocx_shim: tests/test_windows_shim.py` — overlap.
- `ocx_test_support: escalate` and `ocx: escalate` — no glob.
- `/home/mherwig/dev/ocx-sion/test/taskfile.yml:427`: `[ -n "$row" ] || { echo "test:scoped: no row for crate '{{.ITEM}}' — add one to SCOPED_ROWS in test/taskfile.yml" >&2; exit 1; };` — the table is a *selection* mechanism with a hand-maintained fallback, not a disjoint ownership map.

**Concrete fix.** Make the cache unit **one `sh_test` per `test/tests/test_*.py`** (the natural
disjoint unit) and keep `SCOPED_ROWS` purely as the crate→target *selection* query. State
explicitly what an `escalate` row maps to (`//test:all`). Add the concurrency rule
(`--local_test_jobs=1` for the compose-backed targets, or a documented port-per-target scheme) and
re-word A4's red to name one non-overlapping module.

---

### H2 | High | § Observability | "A bespoke post-build script, `scripts/bep_to_otlp.py`"

**What is wrong.** This is the only new component with **no gate contract**. The ADR gives every
other new script a five-row table — Reads / Compares / Exit 0 / Exit 1 (each with its stderr text) /
Red state (§ pin authority, § Stage 1 drift gate, § Stage 2 floor, § Stage 2 ceiling). The BEP→OTLP
script gets prose only: no exit-code semantics, no failure messages, no reader floor, and no span
schema (span name, parent, attribute keys). A tester cannot write a failing test from it. A5's
"Tempo span count == BEP target count" is an assertion about a live Grafana/Tempo round trip, not a
contract the script can be tested against offline.

**Evidence.** § Observability names inputs ("`TestResult.test_attempt_duration`", "a
`SpawnMetrics`-based aggregation … keyed by `target_label`", "`ExecLogEntry.Spawn.runner` … +
`ExecLogEntry.Spawn.cache_hit`") and one behaviour it copies ("a silent no-op when unconfigured
(`[ -n "${OTEL_EXPORTER_OTLP_ENDPOINT:-}" ] || exit 0` at L50-51)") — and stops. The ADR's own
standard in the same file: § Stage 2 floor contract's "(a reader floor; without it, a query that
returns nothing is indistinguishable from a clean tree)". A silent-no-op script with no floor is
exactly that failure mode, and `.claude/rules/subsystem-ci.md` records that this pipeline has
already shipped it twice.

**Concrete fix.** Add the same five-row table: Reads (`--build_event_json_file`,
`--execution_log_compact_file`); Emits (one span per target, name = `target_label`, attributes
`ocx.bazel.runner`, `ocx.bazel.cache_hit`, `ocx.build.id`); Exit 0 (spans exported == targets read);
Exit 1 with a reader floor (`bep_to_otlp read <n> targets and <m> spawns — the reader stopped early`);
Exit 0 + no-op when `OTEL_EXPORTER_OTLP_ENDPOINT` is unset. Then A5's red is reproducible without a
Grafana session.

---

### H3 | High | § Stage 2 — "The second call site the dossier misses" | "a per-crate `cargo nextest -p <crate>` step (L152-155)"

**What is wrong.** The citation points at prose, and the section misses two of the three call
sites it exists to name. `taskfile.yml:152-155` is the body of `verify:scoped`'s `summary:` field —
documentation, not a step. The actual per-crate run is at `:223`; there is a **second** scoped
nextest invocation at `:206`; and a per-crate doctest at `:225`. The section also delivers no
contract — it ends "The dossier's 'Surfaces touched' list does not name it; the plan must" — so the
fast local loop, which is what a developer actually feels, is left entirely to the plan.

**Evidence.**
- `/home/mherwig/dev/ocx-sion/taskfile.yml:153`: `crate clippy / nextest / doc tests, the workspace` — inside `summary: |`.
- `/home/mherwig/dev/ocx-sion/taskfile.yml:223`: `cmd: cargo nextest run -p {{.ITEM}} --locked --no-tests=warn` (under `- for: { var: CRATES }`).
- `/home/mherwig/dev/ocx-sion/taskfile.yml:206`: `cmd: cargo nextest run -p ocx_test_support --test workspace_structure --locked` (the `ROUTE_MANIFESTS` route).
- `/home/mherwig/dev/ocx-sion/taskfile.yml:225`: `cmd: cargo test --doc -p {{.ITEM}} --locked`.
- The claim diff was correctly hedged — `discover_bazel_full_adoption.md:28`: "(not shown above but **referenced** at L152-155)". The ADR dropped the hedge.

**Concrete fix.** Cite `:206` and `:223`. Give the Bazel-native contract in the same shape as the
other gates: `bazel test //crates/<name>:all` for each `CRATES` entry, `--no-tests=warn` equivalent
(`--build_tests_only` + an empty-target-set tolerance), and say what the `ROUTE_MANIFESTS` route
(`workspace_structure`, a `ocx_test_support` integration target) maps to — which is also the target
class B2 says is missing from the graph.

---

### H4 | High | § The file set this ADR creates | "| Path | Contract | Tracked |"

**What is wrong.** The ADR enumerates only what it **creates**. Nothing anywhere — no table, no
bullet list — enumerates what it **edits**, although the ADR itself rules on several of those edits.
The dossier listed them all; the ADR drops the list, so `/hex-plan` decomposing from this document
has no work-package surface for them.

**Missing edited surfaces (all named by the dossier at `.agents/discussions/bazel-full-adoption.md:143-154`):**
`ocx.toml` / `ocx.lock` (the `bazel = "…:9.2.0"` and `agg` entries this ADR rules on in § rules_ocx
as the sole tool path); `taskfile.yml` + `taskfiles/rust.taskfile.yml`;
`.github/workflows/verify-basic.yml` and `verify-deep.yml`; `.github/actions/` (bazel bootstrap via
`setup-ocx`); `website/taskfile.yml` + `website/recordings.taskfile.yml`; `.claude/rules.md`;
`CLAUDE.md` § Build & Development; `.claude/rules/subsystem-ci.md`; `.claude/rules/subsystem-taskfiles.md`;
and a **new** dev-setup docs page.

Two of these carry findings the claim diff already made and the ADR silently dropped:
- `discover_bazel_full_adoption.md:37` (claim 14): `.claude/rules.md`'s auto-load row lists a **narrower** set than `bazel-quality.md`'s own `paths:` frontmatter, "and should be widened to match (a `meta-ai-config.md`-flagged drift)". Verified: `.claude/rules/bazel-quality.md:18` carries `- "**/*.scl"` among globs the catalog row omits.
- `discover_bazel_full_adoption.md:40` (claim 17): "**ABSENT** — confirmed, none exists … no `contributing.md`, no `contributing/` directory" — so the docs surface is a new page, not an edit.

**Concrete fix.** Add a second table, "The file set this ADR edits", with one row per surface and the
contract of the edit, mirroring the create table. Carry the two claim-diff consequences into it.

---

### H5 | High | § Open Questions | "[NEEDS CLARIFICATION #2]"

**What is wrong.** Two defects in the three-marker budget.

1. **A dangling numbered reference.** `§ Open questions … disposition` ("Its **threshold** is
   [NEEDS CLARIFICATION #2] below") and `§ Acceptance A2` ("not below the nextest baseline by the
   threshold set in [NEEDS CLARIFICATION #2]") both cite a numbered marker. The three markers in
   § Open Questions carry **no numbers**, so "#2" resolves to nothing and a reader must guess by
   reading order. A token grep returns **5** hits, not 3, so the cap is also unverifiable mechanically.
2. **One marker is already settled by the ADR itself.** Marker 1 asks "Is this ADR's 'go' conditional
   on WP-0's decision file — with that file retaining the authority to return no-go …?". § Considered
   Options C answers it, bolded, in the ADR's own voice: "**This ADR's `go` is conditional on WP-0's
   decision file, which cannot be written until the median is measured and which retains the
   authority to return no-go.**" That slot is spent ratifying a decision the ADR has taken, while a
   genuinely unresolved question sits in prose: § Targets and pins — "**The plan owes a check for this
   pair** [`rust-toolchain.toml` ↔ `MODULE.bazel`] … and is listed as a plan-docket item" — which
   appears in **no** docket; the disposition table's only `PLAN DOCKET` row is WP-0b (rules_ocx).

**Concrete fix.** Number the three markers `#1/#2/#3` in the marker text itself. Retire marker 1 to
a one-line statement of the position already taken (and let WP-0's decision file carry the
ratification), and spend the freed slot on the floor-reachability question B2 raises, which a human
must rule on before the target graph is drawn. Add the toolchain-pin drift check as a `PLAN DOCKET`
row in the disposition table.

---

## WARN

### W1 | Warn | § Open questions from the dossier — disposition | "All nine entries of the dossier's `## Open questions` section (its lines 167-201)"

The dossier's `## Open questions` heading is at line 167 and holds **one** bullet (169-171,
`build.rs` under strict action env). `## Verification` starts at line **173**; the other eight
dispositioned items live at the tail of *that* section (187-201). The cited range 167-201 therefore
also sweeps in six `## Verification` bullets (175-185) the table does not disposition at all —
though the ADR does address them in § Acceptance A1-A7. The nine-row count is right and **nothing
is dropped**; the citation just points readers at the wrong section.

Evidence: `.agents/discussions/bazel-full-adoption.md:167` `'## Open questions'`; `:173`
`'## Verification'`; `:187-201` the eight `[NEEDS CLARIFICATION]`/bullet items.

Fix: "the dossier's single `## Open questions` bullet (169-171) plus the eight unresolved items at
the tail of `## Verification` (187-201)". Add a line noting that verification bullets 175-185 map to
§ Acceptance A1-A7, so a reader can see they were not skipped.

### W2 | Warn | § Relationship to `adr_crate_split_workspace.md` | "reads, verbatim and verified:"

Present tense, and false against the live file — this ADR has already rewritten that line. The
quoted block is the **pre-amendment** text.

Evidence: `/usr/sbin/git -C /home/mherwig/dev/ocx-sion diff .claude/artifacts/adr_crate_split_workspace.md`
shows `-- [x] No new build system (Bazel parked by the dossier), no new dependency introduced by this ADR`
replaced by the amended line; the current `adr_crate_split_workspace.md:19` is the new text.

Fix: "read, before this ADR's amendment, verbatim and verified:".

### W3 | Warn | § Stage 2 — The hook point | "is a 13-step sequential `cmds:` block … The other nine"

Both numbers are wrong. `.verify:build-test` has **14** steps; four are replaced, so **ten** remain —
and the ADR's own parenthetical enumerates ten.

Evidence: `/home/mherwig/dev/ocx-sion/taskfile.yml:288-331`, in order: `rust:license:check`,
`rust:license:deps`, `rust:license:notice:check`, `scripts:suite-census`, `scripts:dead-path-sweep`,
`rust:lint:ratchet`, `rust:doc:ratchet`, `rust:build`, `rust:test:floor`, `rust:test:unit`,
`rust:test:ceiling`, `rust:test:ceiling:self-test`, `rust:test:doc`, `test:parallel`.
(The line range `288-331` itself is VERIFIED.)

Fix: "a 14-step sequential `cmds:` block … The other ten".

### W4 | Warn | § Acceptance A6 | "`grep -rn 'http_file\|http_archive' MODULE.bazel *.bzl **/*.bzl`"

The acceptance gate is itself an unchecked green. `**/*.bzl` expands recursively only with
`globstar` enabled — off by default in non-interactive bash, absent in POSIX `sh` and in Taskfile's
default shell; `*.bzl` at the repo root matches nothing and, unquoted with no match, is passed to
`grep` literally. The command can therefore read **only `MODULE.bazel`** and still return exactly
one hit, because the one expected hit (the Nerd Font `http_archive`) lives in `MODULE.bazel`. The
reader floor the ADR wrote ("the grep returning zero hits means it did not read the files") does not
fire, so a check that read one file is indistinguishable from one that read the tree — the precise
failure § Acceptance's own preamble forbids.

Fix: `git ls-files -z 'MODULE.bazel' '*.bzl' | xargs -0 grep -n 'http_file\|http_archive'`, and floor
on the **file count** read (`… read <n> files, expected >= <k>`), not only on the hit count.

### W5 | Warn | § Acceptance A2 | "a PR lane with no secret still reads the cache anonymously; a `main` run increments bazel-remote's `http_cache` write metric"

Two green criteria with no red half, in a section whose own preamble says "A criterion whose red
half has not been shown is not met." Both are single-outcome observations against a live remote
service; nothing says how the negative is produced, so a misconfigured endpoint and a working one
are indistinguishable.

Fix: name both negatives — for the read, point the PR lane at an unreachable endpoint and show the
build still exits 0 with `WARNING: Remote Cache: Connection refused` and **no** cache hits (which is
also the NFR Availability claim, currently asserted with no demonstration); for the write, run the
same job with `--remote_upload_local_results=false` and show the metric does **not** move.

### W6 | Warn | § Stage 1 — Gate contract `task bazel:build:drift` | "The set of first-party `ocx_*` deps per target"

Two gaps. (i) Scope: only first-party edges are compared, so a generated `BUILD.bazel` that drops a
third-party edge (`@crates//:tokio`) or a `[patch.crates-io]` submodule edge passes the drift gate
and only reds at compile time, after the whole pilot build — and the ADR's own A7 gate
`bazel build --nobuild //...` is loading/analysis only, so it will not catch it either. The three
`external/` rows are trivially empty-vs-empty under this rule, making the 23-package reader floor
weaker than it looks. (ii) Placement: the check reads via `bazel query --output=build`, which needs
a loading-clean graph, yet it is scheduled in `.verify:lint`, a parallel `deps:` block that runs
before anything has established the graph loads.

Fix: compare the **full** dependency set with a declared name-mapping table for the `@crates//`
transform (the `scripts/crate_map.toml` precedent the section cites is a full edge table, not a
subset), and move the gate after the loading check.

### W7 | Warn | § Stage 3 — casts | "72 cast actions (granularity ruled below)"

Beyond the count (B1), there is no target contract: no declared inputs, no declared output, no exit
semantics, and no statement of how a Bazel action relates to the live recorder — which is a pytest
run over a shared fixture set (`uv run pytest recordings/ -n auto`), driven by each script's
`# state:` / `# cast:` / `# doc:` header, not a per-script command. A3's green, "every cast is a
`local`-tagged target that records", names no artifact and admits no failing test.

Evidence: `/home/mherwig/dev/ocx-sion/website/recordings.taskfile.yml:52-69` — `parallel:` with
`sources: [recordings/**/*.py, doc_scripts/**/*.sh, conftest.py, src/**/*.py, pyproject.toml, '{{.OCX_BINARY}}']`
and `cmds: [ensure-binary, uv run pytest recordings/ -n auto …]`, `env: OCX_DOC_CASTS_DIR`.

Fix: declare per-target inputs (the script, `test/recordings/**`, `test/src/**`, the `ocx` binary,
the compose stack), the output (`website/src/public/casts/<doc>/<name>.cast`), and the `state:`
provisioning each script's header names — then A3's green is a file-existence assertion, not prose.

### W8 | Warn | § Evidence and attestation status | "Every claim in this ADR traces to a file:line in one of the four input artifacts, or is marked below"

The rule is stated and then broken twice, and the citation scheme the ADR uses for the research is
never defined.

- **Untraced claims.** § Context — "replaced one **278,777-LOC** crate"; § Relationship — "with **63
  reciprocated module cycles**". Neither number appears in any of the four input artifacts (checked
  all four); both trace to a **fifth** source, `adr_crate_split_workspace.md:25` and `:663`. Neither
  is marked owner-attested or UNVERIFIED. (The figure is also a stale baseline — the repo's own
  `discover_crate_split_file_map.md:940` records `| LOC | 278,777 | **348,648** | +69,871 (+25.1%) |`.)
- **Undefined numbering.** "research artifact 3" and "research artifact 4" are used throughout
  (§ pin authority, § Decision Drivers, § Cache staging, the corrections table) with no key anywhere
  saying which file is which — and the two orderings in the ADR disagree: `**Related:**` lists
  dossier, claim diff, cache-trust, toolchain; § Industry Context's table lists claim diff,
  toolchain, cache-trust. A reader resolving "research 4 § 2" has to infer the scheme.

Fix: cite the fifth source by name (it is a legitimate one), or mark the two numbers as inherited.
Number the artifacts once in § Industry Context's table and cite by that number consistently — or
drop the numbering and cite by filename.

### W9 | Warn | § Cache staging ruling 5 vs § disposition table | which lane holds the write credential

The dossier question is marked **DECISION — closed**, but two statements give different answers for
one lane. § Stage 2 ruling 1 puts a Bazel lane in `verify-deep.yml`'s **Linux matrix leg**;
`verify-deep.yml` triggers on `push: [main]`. Ruling 5 states the write condition as a *predicate* —
"Write happens only when `github.event_name == 'push' && github.ref == 'refs/heads/main'`" — which
that leg satisfies, and states the compensating flag applies to "every **non-main** lane", which that
leg is not. The disposition table states it as a *lane*: "`verify-basic.yml`, push-to-`main` only".
Under the predicate reading, verify-deep's main-push Linux leg attempts a write with no credential;
under the lane reading it carries neither the credential nor the `--remote_upload_local_results=false`
that BZL-CACHE-01's verification treats an absence of as the finding.

Evidence: `discover_bazel_full_adoption.md:21` (claim 4c) — `verify-deep.yml` triggers include
`push:[main]`; ADR § Stage 2 ruling 1; § Cache staging ruling 5; § disposition row "Which CI lane
holds the bazel-cache write credential".

Fix: state it once as a lane **and** a predicate together, and say explicitly that
`verify-deep.yml`'s Linux leg is a **reader on every trigger including `push` to `main`**, carrying
`--remote_upload_local_results=false` unconditionally.

---

## NOTE

### N1 | Note | § The pin authority — Gate contract | "The `X.Y.Z` parsed out of `bazel version 9.2.0`"

Garbled: the command named one row above is `bazel --version`, and its output string is
`bazel 9.2.0`, not `bazel version 9.2.0`. Fix the literal so the parser contract is unambiguous.

### N2 | Note | § Stage 4 and § Acceptance A4 | "`SCOPED_ROWS`-globbed"

`SCOPED_ROWS` is cited four times with no location. It is `/home/mherwig/dev/ocx-sion/test/taskfile.yml:371`.
Cite it once, as the ADR does for every other live artifact.

### N3 | Note | § Non-functional requirements — Scalability | "~15 acceptance"

There are **20** `SCOPED_ROWS` rows (two of them `escalate`), and under H1's fix the acceptance unit
is per test module, not per crate. Restate the estimate once the unit is settled; the ~135-150 total
and its comparison to BZL-CI-01's ~300 tripwire both move with it.

### N4 | Note | § Considered Options / § Decision Outcome | the matrix reconciliation is stated

Not a finding — recorded so it is not re-litigated. The matrix arithmetic is correct
(weights 6+4+5+4+3+2+2 = 26; A 94, B 108, C 98, D 60, max 130 — all four recompute exactly), and the
ADR does **not** recommend against its own matrix silently: § Decision Outcome states it plainly —
"B scores highest as a *terminal state* and the matrix says so plainly; A is chosen because the
owner's ratified goal is the four-stage end state and because stages 3 and 4 are separable,
individually abortable increments". The reconciliation is explicit and survives.

---

## Question 6 — the amendment to `adr_crate_split_workspace.md:19`

| Test | Verdict |
|---|---|
| (a) exactly one line | **Pass.** `/usr/sbin/git diff` shows a single `-`/`+` pair on line 19; no other hunk in the file. |
| (b) grammatically coherent | **Pass with a referent ambiguity** — see below. |
| (c) correctly *not* setting `Superseded By:` | **Pass.** `adr_crate_split_workspace.md:15` still reads `**Superseded By:** —` and `:5` still reads `**Status:** Accepted`. |

**(b), the one defect.** The original checkbox carried two claims joined by a comma ("No new build
system (Bazel parked by the dossier), no new dependency introduced by this ADR"). The rewrite moves
the second claim **after** the amendment note, so it now trails a sentence that names a *different*
ADR:

> … **Amended 2026-09-21 by `adr_bazel_build_adoption.md`**: the park is reopened — … and test
> wall-clock is now the dominant pain. No new dependency introduced by this ADR

"this ADR" in that trailing clause originally meant the crate-split ADR; after the split it sits
immediately downstream of `adr_bazel_build_adoption.md` and reads as a claim about the Bazel ADR —
which would be false (the Bazel ADR introduces rules_rust, gazelle_rust, rules_shell,
buildifier_prebuilt and a `git_override` on rules_ocx). **Fix:** move the second claim back in front
of the amendment note — "No new build system at the time of this ADR (Bazel parked by the dossier),
no new dependency introduced by this ADR. **Amended 2026-09-21 by `adr_bazel_build_adoption.md`**:
the park is reopened — …". Still one line.

**(c) rationale checks out.** The ADR's justification for leaving `Superseded By:` empty cites a
live convention, and the citation is real: `.claude/rules/arch-principles.md:133` carries
"**Amended 2026-09-10 by `adr_self_update_handoff.md`**", and `:139` a second instance
("**Amended 2026-09-04 by `adr_toolchain_activation.md`**"). The claim diff's recommendation to set
the field (`discover_bazel_full_adoption.md:17`, claim 3c) is overridden **explicitly and with
reasons** in § Relationship, which is the correct handling of a contradicted input.

---

## Question 1 — testability, contract by contract

| Contract | Testable from the ADR alone? | Verdict |
|---|---|---|
| Per-crate `rust_test` targets (§ Stage 1) | No | **B2** — the specified shape cannot reach the floor it is measured against; the target set is under-specified (no integration-test targets, no naming scheme). |
| Website coarse rule (§ Stage 3) | Mostly | Inputs, output and tags are named; the hermeticity gate has a genuine two-halves contract. Missing: the rule's attribute surface and what "the site directory" resolves to. Acceptable at ADR altitude. |
| Cast actions (§ Stage 3) | No | **B1** (wrong count) + **W7** (no inputs/outputs/exit contract). |
| Acceptance `sh_test`s (§ Stage 4) | No | **H1** — the cache unit is defined by a non-disjoint table with two `escalate` rows; A4's red is unsatisfiable as written. |
| `.bazelversion` ↔ pin drift check (§ pin authority) | **Yes** | The strongest contract in the ADR: reads, comparison, exit 0, three distinct exit-1 messages, a reader floor, placement (`cmds:` not `preconditions:`), and a three-case red state. A tester can write this blind. Only N1's garbled literal to fix. |
| BEP-native floor / ceiling reader (§ Stage 2) | Partly | Exit codes, messages and a reader floor are all specified — but the floor value is unreachable under the target shape the same ADR specifies (**B2**), and the reader floor is calibrated to that wrong shape. Fix B2 and this becomes testable. |
| BEP→OTLP script (§ Observability) | No | **H2** — the only new component with no gate contract at all. |
| BUILD drift gate (§ Stage 1) | Yes, narrowly | Contract is complete and red-stated; scope is narrower than the failure it must catch (**W6**). |

---

## Question 3 — claim spot-checks (19 opened at the cited line)

All ten the brief named, plus nine more. Each opened with `sed -n`/Read, not grep.

| # | ADR claim | Verdict |
|---|---|---|
| 1 | `crates/NEXTEST_FLOOR` = 8213 | **VERIFIED** — file content is `8213`. |
| 2 | `cast_recorder.py:377` is `pexpect.spawn(` | **VERIFIED** — line 377 is `self._shell = pexpect.spawn(`; `import pexpect` at :9. |
| 3 | `verify-deep.yml` has no floor/ceiling; `verify-basic.yml`'s `smoke` does | **VERIFIED** — floor `verify-basic.yml:211`, Test `:219`, ceiling `:221`, self-test `:228`; `verify-deep.yml:135-136` is `- name: Test` / `run: cargo nextest run --workspace --target=… --profile ci --locked`. A repo-wide `git grep` for `test:floor|test:ceiling` under `.github/workflows/` returns only the three `verify-basic.yml` hits. |
| 4 | `website/taskfile.yml:45-63` is a 5-stage DAG including `scripts:publish` | **VERIFIED** — `build:` at :45; `schema:default` :59, `scripts:publish` :60, `recordings:parallel` :61, `sbom:generate:page` :62, `bunx vitepress build` :63. |
| 5 | `crates/ocx_cli/build.rs` reads `CI` at L40 and six `GITHUB_*` at L87-96 | **VERIFIED** — `:40` `let in_ci = std::env::var_os("CI").is_some();`; `:87` `for var in [` through `:96` `}`, listing exactly `GITHUB_SERVER_URL, GITHUB_REPOSITORY, GITHUB_RUN_ID, GITHUB_WORKFLOW, GITHUB_REF, GITHUB_SHA`. |
| 6 | `bazel-quality.md:50-51` "through the pin — `bazelisk` reading `.bazelversion`, never a `bazel` on `$PATH`" | **VERIFIED** — verbatim, under `## The Gate`. |
| 7 | `bazel-quality/typescript.md:93-94` are BZL-JS-01 and BZL-JS-03 at severity MUST | **VERIFIED** — :93 BZL-JS-01, :94 BZL-JS-03, both rows end `| MUST |`. |
| 8 | `bazel-adopt/SKILL.md:158-174` is step 4, "**Not the subtree with the worst pain**" | **VERIFIED** — `### 4. Choose the pilot` at :158; the quoted sentence and the "Empty output = … any subtree is eligible, and you pick on size" text are verbatim; anti-pattern item 5 at :384. |
| 9 | 72 `test/doc_scripts/*.sh` | **VERIFIED as a file count** — 72 `.sh` files, flat. **But the ADR's use of it is wrong** — see **B1**. |
| 10 | 20 workspace members + 3 patched submodules | **VERIFIED** — 20 `crates/*/Cargo.toml`; `Cargo.toml` `exclude` and `[patch.crates-io]` both name the three `external/` crates. |
| 11 | `ocx.lock` "carries no resolved semver anywhere (verified: `ocx.lock:8-19`)" | **VERIFIED** — `:8-19` is the `actionlint` entry: `[[tool]]`, `name`, `group`, `repository`, `[tool.platforms]`, six digests. The only `version` tokens in the file are `lock_version = 3` (:2) and `declaration_hash_version = 1` (:3), neither a resolved tool semver. The drift-check mechanism rests on a true premise. |
| 12 | BZL-CACHE-03 quote, incl. "**pinned** — a new setup takes the helper from its first line" | **VERIFIED** — `.claude/rules/bazel-quality/caching.md:71`, verbatim, severity `MUST (new) / SHOULD (existing)`. The ADR's "MUST for a new setup" reading is exact. |
| 13 | `branches-python-ts-cpp.md:108-111` escape clause | **VERIFIED** — `:108` "**Adopting for a repository not already on pnpm is a "no" as a starting move.**" through `:111` "…already justified by other languages." |
| 14 | `bazel-quality.md` frontmatter carries `license`/`repository` at lines 21-22 | **VERIFIED** — `:21` `license: Apache-2.0`, `:22` `repository: https://github.com/ocx-sh/grimoire-lore`. The ADR's correction of the brief's description is itself correct. |
| 15 | `grimoire.toml:6` pins `bazel-essentials = "ghcr.io/ocx-sh/lore/bazel-essentials:latest"` | **VERIFIED** — exact. |
| 16 | `taskfile.yml:288-331` is `.verify:build-test` | **VERIFIED** (range). **Step count wrong** — see **W3**. |
| 17 | `taskfiles/rust.taskfile.yml:257-278` runs `cargo test --doc --workspace --locked` and states nextest cannot run doctests | **VERIFIED** — `test:doc:` at :257, `desc: Run the workspace doctests`, summary states it. |
| 18 | `verify:scoped` is `taskfile.yml:129-251` with a per-crate nextest step at L152-155 | **Range VERIFIED** (`verify:scoped:` :129, `verify:mark:` :252). **Step citation WRONG** — see **H3**. |
| 19 | "`local` … precludes … remotely cached, remotely executed, or run inside the sandbox" | **VERIFIED against the cited research** — `research_bazel_cache_trust_boundary.md:27`, quoting Bazel's Common Definitions. |

Additional cross-checks against the two research artifacts, all consistent with the ADR:
rules_rust default `1.98.0` (`research_bazel_toolchain_verification.md:26`); `#3732` scoped to
`crates_vendor` (`:14`); 9.2.0 is newest GA, 9.3.0 at rc (`:61`); gazelle_rust on BCR 0.1.0, issue
#5 closed (`:42-45`); `agg` absent from the live index (`:85`); `--execution_log_compact_file` current
spelling (`:90`); `TargetComplete` has no timing field (`:92`); no Bun ruleset on BCR (`:101`).

---

## Question 2 — internal-consistency sweep, the four named risk areas

| Area | Verdict |
|---|---|
| (a) `--credential_helper` conditional or unconditional | **Consistent.** Nine mentions, all agree it is unconditional from day one. § Cache staging ruling 4 explicitly names and overrides the research's weaker framing — "This is stronger than research artifact 3's conditional framing, which made the preference contingent on BEP export. The rule does not condition on that." — and `research_bazel_cache_trust_boundary.md:69` does indeed phrase it conditionally, so the override is honest. The corrections table, the NFR Security row and the disposition row all match. `.bazelrc.user`'s "personal `--credential_helper`" is consistent with BZL-CACHE-04 (the file is gitignored, so not a *tracked* rc). **No finding.** |
| (b) `ocx.toml` pins `:9.2.0` or `:9` | **Consistent.** Three mentions, all `:9.2.0` (§ pin authority, § rules_ocx tool table, the corrections table), each naming the divergence from the dossier as deliberate. `.bazelversion` "Exactly `9.2.0`" agrees, and the WP-0 escape ("If `9.2.0` does not resolve … WP-0 picks the nearest resolvable 9.x GA and `.bazelversion` follows it") is stated, not implied. **No finding** — except that `ocx.toml`/`ocx.lock` appear in no file-set table at all (**H4**). |
| (c) what the drift check compares | **Consistent, and the premise is true.** All four mentions (§ file set, § pin authority contract, C4 component table, disposition row) say `.bazelversion` vs the running binary. The load-bearing claim that `ocx.lock` carries no semver is **independently verified** (spot-check 11). The dossier's ".bazelversion ↔ ocx.lock" spelling survives only as the disposition table's question title, which is correct. **No finding** beyond N1's garbled literal. |
| (d) matrix vs recommendation | **Reconciled explicitly** — see N4. **No finding.** |

Contradictions the sweep *did* find are B3, W9, W2, W3 and H3 above.

---

## Question 4 — disposition completeness

- Dossier items counted by hand: **9** (one under `## Open questions`, eight at the tail of `## Verification`).
- Disposition table rows: **9**. Mapping is 1:1, in dossier order. **Nothing is missing.**
- Dispositions contradicted elsewhere: **none found.** The `build.rs` row, the credential row and the
  drift-check row each point at a § that says the same thing. (W9 is a tension *inside* the
  credential ruling, not between it and its disposition.)
- Section attribution is wrong — **W1**.
- Sweep of the dossier's `## Decisions` list (lines 29-97) against the ADR: all 14 decision bullets
  and all seven G3 sub-goals are carried (G3.1→A1 … G3.7→A7). One is silently *narrowed*: the dossier's
  "nextest stays local-only" becomes "the darwin and windows legs keep `cargo nextest run`" in
  § Stage 2 ruling 1 — but that ruling names the dossier's own out-of-scope list as its warrant, so
  the narrowing is argued, not hidden. **What is dropped is the dossier's `## Surfaces touched`
  edit list — H4.**

---

## Question 5 — open-questions cap

- `[NEEDS CLARIFICATION:` markers in § Open Questions: **3**. Cap respected.
- Token occurrences file-wide: **5** (two are cross-references) — **H5**.
- Are they the right three? #2 (WP-1c's no-go threshold) and #3 (how rules_ocx pins the `ocx`
  binary, and whether that pin enters the action key) are both genuine, both need a human, and #3 is
  a sharp cache-correctness question the inputs could not answer. **#1 is not** — the ADR answers it
  itself in § Considered Options C. The question that should have taken that slot is the one B2
  raises. **H5.**

---

## Question 7 — acceptance criteria, red states and scope

| Criterion | Scope named? | Red half specified? | Verdict |
|---|---|---|---|
| **A1** per-crate skip (the pilot's proof) | **Yes** — "`//crates/...` on Linux only … Not the `external/` libraries, not doctests, not darwin/windows." | **Yes, and this one is the model.** The green's own cold run is the red for "every target cached"; then two further halves: touch a leaf and read the expected re-run set from `bazel query 'rdeps(…)'` "not guessed"; touch a hub and require a *different*, larger set — with the discriminator stated outright: "If touching a leaf and touching a hub produce the same re-run set, the graph is not per-crate and A1 is **not met**, whatever run 2 reported — a universal cache hit and a universally-invalidating graph are indistinguishable from the summary line alone." Evidence is pinned to the BEP, not the terminal summary. | **Strong.** The one acceptance criterion that fully meets `quality-core.md` § Unchecked Green. Its only defect is inherited: the scope says "the 20 workspace members' `rust_test` targets", which **B2** shows is the wrong target set. |
| **A2** lane swap and gates | Yes — smoke job + verify-deep Linux leg, explicitly not darwin/windows. | Partly — four gates have named reds and the write-isolation red is excellent ("A green here that was never run with the credential present proves nothing about isolation"). Two greens have none. | **W5.** |
| **A3** casts and website | Yes for the site rule; wrong for the casts (72). | Yes for both halves of the site rule; the doc-script-edit red is exactly right. Nothing for the cast targets. | **B1**, **W7**. |
| **A4** acceptance | Names the `SCOPED_ROWS` glob as the scope — which is not a partition. | Red stated, but unsatisfiable for overlapping/`escalate` rows. | **H1.** |
| **A5** telemetry | Scope is "one build". | Yes — "set the OTLP export queue below the span count → the two counts diverge. Restore → equality", with the reason it is trusted (`subsystem-ci.md` records it as the only thing that caught the queue-drop defect). | **Good** — but no component contract behind it (**H2**). |
| **A6** rules_ocx sole tool path | Yes. | Reader floor present in intent, defeated by the command as written. | **W4.** |
| **A7** bazel-quality gate set | Yes — the four commands, exit codes propagated, "never `\|\| true`, never a stdout scrape". | Yes — each of the four shown red against a named planted violation. | **Good.** |

---

## Summary

| Severity | Count |
|---|---|
| Block | 3 |
| High | 5 |
| Warn | 9 |
| Note | 4 |

**Verdict: not ready for `/hex-plan` as written.** Three Blocks would each make a planner build the
wrong thing — a 72-target cast graph where 39 targets exist (B1), a Rust target shape that cannot
reach the floor the same ADR keeps (B2), and a Decision Outcome paragraph that tells the planner the
website rule needs no hermeticity gate (B3). Everything else is a rework round, not a rebuild. The
ADR's reasoning quality is high — the pin-authority contract, A1's leaf-vs-hub discriminator, the
write-isolation red and the named overrides of the vendored rules are all better than the bar — and
the four named risk areas (credential helper, `:9.2.0`, the drift mechanism, the matrix
reconciliation) are each internally consistent on a full sweep.
