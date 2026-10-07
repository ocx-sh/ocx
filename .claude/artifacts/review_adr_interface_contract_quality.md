# Review: ADR + system design — OCX machine-interface contract (quality, adversarial)

**Reviewer:** quality focus, adversarial seat, hex-architect xhigh panel (Opus)
**Date:** 2026-10-03 · **HEAD:** `34337a6a2`
**Under review:** `adr_ocx_interface_contract.md`, `system_design_ocx_interface_contract.md`
**Inputs read:** both artifacts in full; dossier `.agents/discussions/ocx-interface-contract.md`; `discover_ocx_interface_contract.md`; `research_zero_dep_sdk_codegen.md`, `research_interface_contract_tooling.md`, `research_machine_output_compat.md` (direct answer); `scripts/crate_map.toml`; `adr_crate_split_workspace.md` § crate map; ocx-mirror sources (grep, read-only).

**Verdict: Needs Work.** The recommendation (Option D, phases 0 → 1 → 2 with C as off-ramp) survives. Two Block findings must be fixed in the text before `/hex-plan`. Neither one overturns D. The largest Warn is that the most expensive component, the owned 6-language generator, is never weighed: the trade-off matrix holds it constant across A, B and D.

Counts: **Block 2 · Warn 11 · Suggest 8**. All are actionable unless marked Deferred.

---

## Block

### B-1 · "Fail-closed" differ is a denylist, so unmodelled keywords pass silently
**Anchor:** system design § 3.8 Compat gate ("Any construct the lint forbids, met in either document, yields `unmodelled`"); ADR § Risks row 1, § The six tensions #4, § Don't-own assessment ("fail-closed").
**Argument.** The ADR rests its case for owning the differ on one safety claim: the differ fails closed outside a known subset. As specified, though, the subset is the complement of the lint's *bans* (L01–L04: `anyOf`, type arrays, `null`, untagged `oneOf`). It is not the set of keywords the differ *models*. Keywords that are neither banned nor modelled get no B-rule and no `unmodelled` finding: `additionalProperties` (the ADR keeps 4 map roots and moves them under a named field, so map schemas stay), `items`/`prefixItems`, `minimum`/`maximum`, `minItems`, `allOf`, `not`, `if/then`, `patternProperties`, `dependentRequired`. A change to any of them passes green. That is the "green indistinguishable from never ran" shape quality-core § Unchecked Green rates Block when shipped. The mutation corpus does not catch it: V6 has "one case per rule", so it only exercises the rules that already exist.
**Fix.** Make the differ an allowlist. Declare the closed set of JSON Schema keywords it models (`type`, `properties`, `required`, `$ref`, `$defs`, `enum`, `const`, `oneOf`, `items`, `additionalProperties`, `pattern`, `format`, `description`, `title`, `minimum`, …), each with its B-rule or an explicit "ignored, not contract" entry. Any other keyword in either document yields `unmodelled` (breaking). Add a V6 red case: introduce `allOf` (or change `minItems`) and the gate must red with `unmodelled`. Floor the differ's reader: visited keywords must be ≥ an independent count of keywords in the raw JSON.

### B-2 · Phase 0 breaks ocx-mirror's ecosystem API, but the rollout has no phase-0 lockstep row
**Anchor:** system design § 3.4 Migration ("`ocx_config::env::keys` is deleted … the public `var`, `flag`, `string` taking `&str` are removed, so a literal read stops compiling"); ADR § Migration › Consumers (rows start at Phase 1); § Phases row 0 exit criterion.
**Argument.** ocx-mirror uses exactly the API that phase 0 deletes (verified by grep in `/home/mherwig/dev/ocx-mirror/crates`):
- `ocx_config::env::keys::{OCX_OFFLINE, OCX_FROZEN, OCX_ANNOUNCE_TOKEN, CREDENTIAL_KEYS}`: `ocx_cli/create.rs:57`, `announce.rs:20`, `ocx_mirror_spec/src/validate.rs:605`, `sign_config/tests.rs:555`.
- `ocx_util::env::var(&str)` with literals and computed names: `ocx_mirror_spec/src/annotations.rs:82,85` (`GITHUB_ACTIONS`, `GITLAB_CI`), `source.rs:123`, `ocx_mirror_http/src/auth.rs:142-145` (per-registry names built at runtime).

CLAUDE.md § Stability tiers makes the ecosystem consumer's upgrade "in the same change series" an obligation, enforced by `task satellite:verify`. The ADR schedules mirror work only in phases 1 and 2, and the phase-0 exit criterion does not mention `satellite:verify`. There is also a design gap. Mirror reads *its own* variables, which do not belong in OCX's registry. The design does not say whether `EnvVar`/`env_vars!` are usable by a downstream crate or whether mirror must route everything through `dynamic()`. Only OCX's workspace ratchet counts `dynamic()` calls.
**Fix.** Add a phase-0 row to § Consumers: mirror moves to `ocx_util::env::vars::*` statics, and `CREDENTIAL_KEYS` derives from the registry. State that `EnvVar` and `env_vars!` are public so a satellite can declare its own variables, with `dynamic()` reserved for data-derived names. Add "`task satellite:verify` green at the phase-0 submodule pointer" to the phase-0 exit criterion.

---

## Warn

### W-1 · The owned 6-language generator is never weighed, and its criterion-1 justification is undercut by the ADR's own decisions
**Anchor:** ADR § Trade-off matrix (A, B, D all assume an owned generator); § Don't-own assessment row "Generator | Criterion 1 … | Own, ratified"; dossier § Decisions "Codegen" and "Pilot SDK: Rust (typify is the strictest generator on unions …)".
**Steelman (owning it is wrong for phases 0–2):**
1. The dossier's criterion-1 evidence is "no generator whose zero-dep output decodes tagged unions **or preserves nullable-vs-absent**". Tension #5 (zero nullable forms in outputs) removes the second half. The ADR concedes this: "removes the generator's hardest requirement".
2. For the only backend built in phases 0–2 (Rust), "stdlib-only" is not the constraint, because `serde` + `serde_json` is an accepted exception. typify emits serde-derived internally tagged enums from `oneOf`+`const`. That is the dossier's own reason for choosing Rust as pilot, and the generator decision drops typify without addressing that reason. The one real unknown is open enums (`research_zero_dep_sdk_codegen.md` § leads: "whether it emits `#[serde(other)]` … unverified"). That is a one-afternoon measurement and should not be settled by assumption.
3. With a tagged, never-null subset, Python `TypedDict` (datamodel-code-generator) and TypeScript (json-schema-to-typescript) already need no runtime decoder, because the tag narrows the type natively. Criterion 1 genuinely holds only for Swift, Java and maybe Go, and all three are in later plans.
4. What OCX must own in every option is the argv/command layer generated from `cli.json`. That is domain code, and no tool reads OCX's grammar. It is a small fraction of an IR plus 6 backends.

**Alternative option E** ("D, but phase 2 = typify for Rust types + owned command/exit/env layer from `cli.json`; generator ownership decided per language in each SDK plan"). Scored on the ADR's own weights with honest C1 (see W-2): C1 4, C2 3, C3 3, C4 4, C5 5, C6 3 → **81** vs D **77**. **The winner changes** if typify's open-enum output is adequate or can be post-processed.
**Fix.** Before phase 2, add a spike to the phase-2 go/no-go: run typify on `tests/golden/reports.json` (post-phase-1 subset) and check tagged dispatch, open enums and `Option`+`default`. Scope phase 2's `ocx-sdkgen` to the Rust backend plus the command layer. Re-decide "own the IR for 6 languages" in the first non-Rust SDK plan, against measured generator output. If the owner holds the ratified decision anyway, the ADR should say it is kept *despite* criterion 1 being mooted for Rust/Python/TS, rather than citing criterion 1.

### W-2 · The matrix's 82-vs-70 margin comes from scoring C inconsistently; D wins through N1, not on merit
**Anchor:** ADR § Trade-off matrix; § Option C ("Phases 0 and 1 of the chosen option *are* C").
**Argument.** The ADR defines C as D's phases 0+1 (the off-ramp). It then scores C as if phase 1 did not exist, and in one row as if a gate existed:
- C3 break volume 4 vs D 3: C's breaks *are* D's phase-1 breaks, and phase 2 adds no CLI breaks. They should be equal (3).
- C4 codegen fitness 1: after phase 1 the schemas are in the codegen subset, so this scores the absence of a generator, not fitness. Should be 4. (The ADR's own reading, 1, is retained below as a bound.)
- C6 time-to-gate 5: C never has a gate. Should be 1, not best.
- D's C1 is 5 while its own top risk is "the differ misses a rule", which is a C1 failure. B (oasdiff, 755 rule cases) gets 4. D should be at most 4.

**Recomputation** (weights 5/4/4/4/3/2):

| Scoring | A | B | C | D |
|---|---|---|---|---|
| As written | 65 | 64 | 70 | 82 |
| Consistent C (C1 2, C3 3, C4 4, C6 1); D C1 4 | 65 | 64 | 75 | 77 |
| Same, alternative weighting C2 owned surface 5, C1 safety 4 | 61 | 60 | **78** | 75 |

Under a defensible "don't own" weighting the off-ramp C wins on merit. D survives only because N1 is a ratified filter. That matters because the phase-2 go/no-go is a merit decision.
**Fix.** Rescore C consistently with its definition. State plainly in § Decision Outcome that D wins through N1 and that the merit margin over C is small. Make the go/no-go criteria weigh cost (differ + generator LOC, maintenance) against measured consumer benefit, not only "spike agrees".

### W-3 · The don't-own bar is applied inconsistently: a Block-tier label dismisses B while D owns a larger emitter
**Anchor:** ADR § Option B cons ("an emitter of an external wire format, the Block-tier case in quality-core"); § The six tensions #4.
**Argument.** quality-core's Block escalation targets *hand-rolled* serializers, codecs and escaping, where the byte-level rules are owned. The oasdiff wrapper builds a stub OpenAPI 3.1 document as a `serde_json::Value` (research: "~20-line wrapper that rewrites `$defs` refs"), with serialization delegated. Meanwhile D's generator emits Rust/Swift/Java/Go/TS/Python *source text*, with identifier escaping, keyword collisions and string-literal escaping in doc comments. Under the ADR's own reading, that is much deeper in the escalated class.
**Fix.** Drop the Block-tier label from B's cons and argue B on what does hold: oasdiff cannot diff `cli.json`/the registry, and it is a Go binary outside the Bazel graph. Add to § Don't-own assessment that the generator's backends are external-syntax emitters, with the mitigation: generated code is compiled and integration-tested in each SDK's CI, and golden output is kept per backend.

### W-4 · Handshake granularity: report mismatches surface only after side effects, and `cli` mismatches only warn
**Anchor:** ADR § Contract scope ("Report versions are not listed there: they travel in every payload"); system design § 3.12 Handshake ("`contract.cli` equal or warn").
**Argument.**
1. Per-root versions in the payload mean the SDK detects `ContractMismatch` *after* `ocx package push`/`install` has run. The dossier wanted handshake detection so a program can refuse up front, and mutating commands are exactly where that matters.
2. `cli.json` is versioned per document, and a mismatch only warns. So a recorded G09 (default changed) or G04 (arg became required) break lets the SDK run the command with the new default silently. The ADR rejected document-wide versions for reports for exactly this reason ("a break refuses only the commands it touches"), then applied the opposite to the grammar.
**Fix.** (a) `ocx version --format json` `contract` gains `reports: {<Root>: n, …}` (56 integers, additive), and the SDK refuses a mismatched root *before* spawning its command. (b) Version `cli.json` per command (`CommandSpec.version`, same bump rule as report roots). The handshake then refuses only the affected commands, and "equal or warn" goes away.

### W-5 · Description changes are classified additive, but the one observed semantic break was description-only
**Anchor:** system design § 3.8 ("Additive (reported, never red): … description changed"); ADR § Context (`737a66052` "changed only a `description`"); tension #4 (semantic breaks prompted by the sync rule, which is guidance only).
**Argument.** The only evidence of a semantic break in this repo's history would pass the gate silently. The safeguard is a path-scoped rule that the ADR itself calls "guidance only; the tests enforce", and here no test does.
**Fix.** A changed `description` on an *existing* property or enum value reds unless the ledger carries `rule = "semantic"` or `rule = "doc"` (no bump) for that pointer. That makes the reviewer answer the semantic question once per edit at the cost of one TOML line. If that is too much friction, emit description diffs as a non-red gate report that the release task prints.

### W-6 · During phase 1, `schema_version` is emitted but no rule governs it
**Anchor:** ADR § Root convention item 4; § Versioning ("Versions count from the phase-2 baseline"); § Phases row 1 exit ("all phase-1 flag renames in one release", with no equivalent for report reshapes).
**Steelman for gate-first.** Phase 1 is the largest break wave. If its reshapes span several releases while every root says `schema_version: 1`, then a consumer pinned on `== 1` misreads 0.7.0 vs 0.7.1. That is the silent drift the contract exists to stop, and it happens exactly when consumers need detection most. The ADR already cares about this failure (item 4 starts 6 roots at 2 "so a 0.6-era consumer … fails loudly") but does not apply the same logic inside phase 1.
**Why coherence-first still survives.** Gating the incoherent tree would force the differ to model `anyOf` and two nullable forms, which is the code the subset exists to avoid.
**Fix.** Pick one rule: (a) every phase-1 report reshape lands in one release (mirroring the flag rule), or (b) `schema_version` emission and the `reports/v2` `$id` land in the last phase-1 release, or (c) during phase 1, any golden change to a root bumps that root (coarse, needs no differ).

### W-7 · Reversibility is misplaced: phase 1 is called reversible, and phase 2's gate is called the door
**Anchor:** ADR § Metadata Reversibility; § Trade-off matrix row "Reversibility" ("D: two-way until baseline").
**Argument.**
- Truly one-way: phase-1 user-visible breaks (reverting one is a second break); the published `reports/v2.json` `$id` (phase 1); the root convention (`schema_version` first, `items`) once scripts branch on it; the published crate name and API of `ocx-sdk` on crates.io and the `ocx.sh/ocx/sdkgen` package (phase 2); every published `schema_version` integer, which can never decrease.
- Elective one-way: publishing `https://ocx.sh/schemas/cli/v1.json` (ADR § Contract scope). The only consumer of `cli.json` is `ocx-sdkgen`, which embeds it. A public URL turns an internal format into a contract with no consumer (YAGNI).
- Cheap to undo: the differ, ledger, waivers, lint walker, clippy ban, registry shape, the `cli.json` format while unpublished, and the sdkgen IR.
- Neutral: errors on stdout (status quo, already frozen by the signing ADR); open-enum policy (a loosening promise that OCX can later tighten without harming consumers).

**Fix.** Rewrite the Reversibility line: phase 0 is two-way. Phase 1 is one-way for users, so its breaks are the door. In phase 2, crates.io and package publication are one-way; the gate is two-way. Keep `cli.json`'s schema unpublished until an external consumer exists.

### W-8 · The env registry in `ocx_util` breaks its charter, misreads the uv precedent, and fans rebuilds out to every crate
**Anchor:** ADR § Dossier reconciliation row 2 ("One registry in `ocx_util` (bottom crate; uv's `uv-static` precedent)"); system design § 3.4 (`EnvVar.flag: Option<&'static str>` names CLI flags); NFR § Build.
**Argument.**
- `adr_crate_split_workspace.md` § crate map defines `ocx_util` as "Domain-free primitives … boundary: nothing generic may name an OCX type", and gives "env-var vocabulary and validation" to `ocx_config`. A table of every `OCX_*` name, its CLI flag mirror and its secret class is domain vocabulary, and `flag = "--offline"` makes the bottom crate name application-layer flags.
- `uv-static` is a *dedicated* crate, not uv's util crate, so the precedent argues for a new leaf crate.
- `ocx_util` sits under every crate. Every edit to an env doc comment changes its metadata and invalidates every downstream Bazel action, which drives `rust:build`, clippy, unit tests and accept targets through the CLI binary. The NFR "Build" row mentions only the one-time `clippy.toml` invalidation.

**Fix.** Either (a) keep the `EnvVar` *type* and seam in `ocx_util` and move the *declarations* into a new leaf crate `ocx_env` (ecosystem tier, `= ["ocx_util"]`; the crate-map edges then need `ocx_console`/`ocx_oci`/`ocx_sign` → `ocx_env`), or (b) keep everything in `ocx_util`, amend its charter in the ADR explicitly, and record the rebuild fan-out under NFR § Build. Either way, keep the `flag` link out of the bottom crate: join it in `ocx_schema`, which already sees both sides.

### W-9 · Env aliases sit outside `deprecated.rs`/`RENAMED`, against the batched-window convention
**Anchor:** ADR § Flag renames — the batched window ("Env renames … join the same window through an `aliases` field in the registry, swept by `test_deprecated_spellings.py`"); system design § 3.4 `aliases`.
**Argument.** CLAUDE.md: "every deprecated spelling in flight lives in one `deprecated.rs` deleted whole, and is listed in that file's `RENAMED` — the authority `test_deprecated_spellings.py` reads". A registry field in `ocx_util` is a second home that is not deleted with the file, and the sweep does not read it today.
**Fix.** List env renames as `RENAMED` rows in `command/deprecated.rs`. Either the CLI translates old → new at startup through the seam (one warn on stderr), or a test asserts registry `aliases` == the env rows in `RENAMED`, so deleting the file forces the field empty.

### W-10 · Phase 2 includes the Python SDK migration, which the dossier puts out of scope
**Anchor:** ADR § Phases row 2 ("then `ocx-sdk-python` onto generated types"); dossier § Intent ("Out of scope: implementing SDKs beyond the Rust pilot (Python/…) follow in later plans").
**Argument.** It also commits a second generator backend (Python) inside this ADR's phases without any exit criterion for it. This is scope creep that strengthens the case in W-1.
**Fix.** Move the Python migration to a follow-up plan, named in § Consumers as "after phase 2", or add a Python-backend exit criterion and say why the dossier's scope line is overridden.

### W-11 · The mirror lockstep becomes a triangle with no specified pin rule
**Anchor:** ADR § Consumers row ocx-mirror Phase 2; system design § 3.11 Distribution ("SDK repos pin `ocx-sdkgen`"); CLAUDE.md `task satellite:verify`.
**Argument.** Today mirror's report structs live in mirror and move with its `external/ocx` submodule, so there is one pin. In phase 2 mirror depends on `ocx-sdk-rust` (its own release cadence), which pins `ocx-sdkgen` (one contract release), while mirror separately pins the `ocx` submodule. A report break in the ocx tree does not red `satellite:verify` until the SDK is regenerated and released. That moves detection out of the same change series the ecosystem contract requires.
**Fix.** Pick and state one mechanism. Either mirror generates its SDK from the submodule's goldens (sdkgen run as a tool from the submodule, with no internal crate in mirror's cargo graph), or `satellite:verify` regenerates `ocx-sdk-rust` from the in-tree goldens and builds mirror against it. Also state who owns version skew between the submodule pointer and the SDK pin.

---

## Suggest

- **S-1 · Zero runtime deps (decision survives for phases 0–2).** The Rust exception covers the pilot. The cost lands only in Swift/Java plans. Note that zero-deps is the sole reason the generator must be owned (W-1), so revisit the two together per language.
- **S-2 · `ocx_sdkgen` tier.** It is published as `ocx.sh/ocx/sdkgen` and pinned by external repos, so its CLI and output are observable: interface-tier under CLAUDE.md's practical test, like `ocx_shim`. Classify it explicitly. Its IR loader "re-checks L01–L04" with no edge to `ocx_schema` (`ocx_sdkgen = []`), which duplicates the lint. Share one module or accept the duplication by name. Also add the crate to CLAUDE.md's architecture table (21 → 22 members), which is missing from § Required artifacts.
- **S-3 · Gate edge cases.** B05 ("enum/`const` value removed") fires on every legitimate `schema_version` const bump; exclude `/properties/schema_version` explicitly. `SweepReport<R>` is published twice from one generic impl, so `SCHEMA_VERSION` is shared, and gate step (4) "every other root's = baseline" makes a break in one instantiation unpassable unless both are listed. State that both subjects must be listed, or carry the version on `R`.
- **S-4 · Clippy residue inventory.** System design § 3.5 lists production residue only. discover § 4 lists ≥10 test-code raw reads (`ocx_cli/src/app/seam.rs:410-426`, `ocx_shell/src/shell/hook.rs:1104-1918`, `ocx_package_manager/src/tasks/resolve.rs:4244,4286`, …) inside crates whose `rust_test` targets `//crates/...` covers. Decide route-through-`EnvLock` or `#[expect]` for them, and size the phase-0 "`disallowed_methods` baseline 0" exit accordingly. (Clippy ban vs extending `ocx_util::env` only: **survives**. The ban resolves by `DefId` (measured), unlike a textual ratchet, and `rust-cargo.md` LINT-09 already mandates the root file.)
- **S-5 · NFR § Verify time is overstated.** "re-run only when a golden changes" is false: `ocx_schema = ["ocx", …]` in `scripts/crate_map.toml`, so lint/gate/golden tests re-run on almost any Rust edit. Restate the scope.
- **S-6 · Pretty-printing the error document changes the framing.** The JSON is the same, but line-oriented readers (`tail -n1 | jq`) break. Give it its own `!` commit subject, even though `ENVELOPE_SCHEMA_VERSION` stays 1.
- **S-7 · L10 name heuristics.** `*_path`/`*_dir` → `AbsolutePath` conflicts with the existing `RelativePath` vocabulary. Exempt fields typed `RelativePath`, or name the rule's exceptions in the style guide rather than in waivers.
- **S-8 · `clippy.toml` vs LINT-09.** The new file omits LINT-09's mandated `process::exit`/`thread::sleep` entries "to join later". Name the follow-up issue so the MUST rule is not silently half-met.

---

## Dossier decisions that survive the steelman (one line each)

- **Shell-out transport:** survives. Any other transport is a second interface, and terraform-exec is direct precedent. Pulumi's move away concerns an engine embedding, not a backend CLI.
- **Coherence pass before the gate:** survives on merit (it keeps the differ over a small subset), conditional on W-6.
- **Owned compat differ:** survives. The grammar and registry differs are domain, and over the tagged never-null subset the schema differ is small. Conditional on B-1 (an allowlist makes "fail-closed" true).
- **Owned grammar format (`cli.json`):** survives. `clap_usage` was measured dropping env, `requires` and types (criterion 2).
- **Owned lint walker instead of meta-schema:** survives. L10–L13 vocabulary rules need a walker either way.
- **Errors on stdout, never-null outputs, per-root versions, open enums everywhere:** survive. Each is the status quo or the research-backed lowest-ambiguity form.
- **Clippy ban + registry:** survives (see S-4).
- **Zero runtime deps:** survives for this ADR's phases (see S-1).

## Deferred

None. Every finding has a fix the architect can write. W-1's final call ("own the IR for 6 languages") is the owner's, but the spike that informs it is actionable now.
