# ADR: The OCX machine-interface contract — one vocabulary, gated documents, generated SDKs

## Metadata

**Status:** Accepted
**Date:** 2026-10-03 (round-1 review and validation fixes applied the same day)
**Deciders:** Michael Herwig (owner); Architect (Opus)
**Tier:** xhigh
**Reversibility:** one-way door, high. Phase 0 is two-way: lints, registry, tests, all internal. Phase 1 is **the door** for users: every user-visible break, the `reports/v2` `$id` and the root convention are one-way once scripts branch on them; reverting a break is a second break. In phase 2, the gate, ledger and differ stay two-way (internal), while publishing the `ocx-sdk` crate and the `ocx.sh/ocx/sdkgen` package is one-way, and every published `schema_version` integer can never decrease. The open-enum policy is a loosening promise and stays reversible toward stricter. The `cli` JSON Schema stays unpublished until an external consumer exists, so the `cli.json` format remains two-way.
**Blast radius:** external contract + cross-area. Every `--format json` root (56), the error document, the CLI grammar (73 visible command nodes, 284 `#[arg]` fields), every `OCX_*` variable (53 documented), two consumers (`ocx-mirror` lockstep, `ocx-sdk-python` sibling), the Bazel clippy lane, the release ceremony, the acceptance suite (105 modules parse JSON) and the AI-config rule catalog.
**Domain Tags:** api | integration | devops | security
**Related:** `.agents/discussions/ocx-interface-contract.md` (ratified dossier, data), `discover_ocx_interface_contract.md` (claim diff at `34337a6a2`), `system_design_ocx_interface_contract.md` (component contracts), round-1 reviews `review_adr_interface_contract_{spec,quality,security,sota,codex}.md`, validation review `review_adr_interface_contract_validation.md`
**Supersedes (in part):** `adr_oci_referrers_signing_v1.md` § "Stability contract (frozen v1)" (bump rule for new `kind` values and top-level fields) and the second bullet of its Amendment 9 (text in § Enum policy; that file is not edited here).
**Amends (stated, not made):** `CLAUDE.md` § "Stability tiers" (interface list; the ecosystem-crate enumeration gains `ocx_env`) and § Architecture crate table; `.claude/rules/arch-principles.md` crate table; `.claude/rules/subsystem-cli-api.md` § "JSON Serialization"; `adr_crate_split_workspace.md` crate map (`ocx_env`, `ocx_sdkgen`, `ocx_schema` edges), § Stability tiers ecosystem list, the `ocx_config` charter row, and § "`env` splits" (superseded: vocabulary and accessor go to `ocx_env`, not `ocx_config`); `.claude/rules.md` catalog.
**Superseded By:** —

**Required artifacts:**

| Artifact | Phase | Owner |
|---|---|---|
| This ADR and `system_design_ocx_interface_contract.md` | — | architect |
| `plan_ocx_interface_contract.md` | before phase 0 | `/hex-plan` |
| `.claude/rules/subsystem-interface-contract.md` + `.claude/rules.md` rows | 0 | builder |
| `CLAUDE.md` amendments (Stability tiers text; crate table rows `ocx_env`, `ocx_sdkgen`; 21 → 23 members) | 0 / 2 | builder |
| `scripts/crate_map.toml` rows and the Rust mirror table | 0 / 2 | builder |
| Supersession note on `adr_oci_referrers_signing_v1.md` | 0 | builder |
| `ocx-sdk-python` pre-phase-1 release enforcing an upper bound | 0 | builder (sibling repo) |
| `ocx-mirror` phase-0 env migration and `task satellite:contract` consumer gate | 0 | builder (lockstep) |
| `website/src/docs/reference/machine-interface.md` | 2 | builder |
| `ocx-sdk-rust` repository | 2 | builder |
| Issues [ocx-sh/ocx#571](https://github.com/ocx-sh/ocx/issues/571) (`OCX_TEST_FAULT*`), [ocx-sh/ocx#572](https://github.com/ocx-sh/ocx/issues/572) (`OCX_CEILING_PATH`, Public) | filed | owner |

**Tech Strategy Alignment:**
- [x] Rust 2024 + Tokio, Bazel graph, `task` entry points. No new tool in `ocx.toml`; no new runtime dependency in `ocx` (`chrono` moves into `ocx_util` from the workspace set).
- [x] Deviations: Rust SDK runtime `serde` + `serde_json`, optional `tokio` (§ Zero-dependency exceptions). No typify: Option D won the go/no-go (§ Go/no-go outcome).

---

## Context

OCX is a backend tool for other tools. Its machine surface exists but is not a contract a program can lean on. Numbers are from the discover pass at `34337a6a2` (= `v0.6.4`) and the round-1 reviews; they override the dossier.

| Surface | State today |
|---|---|
| Reports | 56 published roots (55 `Printable` impls; `SweepReport<R>` published twice). Over the 55 impls: 42 objects, 7 bare arrays, 4 maps, 2 untagged unions. 6 roots print `{schema_version, command, exit_code, data}`. 4 root `$def`s are also nested inside other roots (`SignatureReport`, `AttestationReport`, `PackageInspect`, `PackageDescription`). |
| Report schema | One document, `reports/v1.json`, byte-compared against a golden; no additive/breaking classifier. 6 `!` commits changed it, `$id` never moved; `737a66052` changed only a `description` (a semantic break). Goldens exist only from `v0.6.3`. |
| Schema subset | 44 `anyOf` (41 nullable, 3 untagged unions), 51 `type: [T, "null"]`, 15 non-const `oneOf` unions, 21 string-const `oneOf` enums, 21 schema-valued `additionalProperties`, 1 `additionalProperties: false`, 9 payload properties named `type`, 0 named `items` (5 named `entries`), 260 of 684 properties without a description. |
| Opaque payloads | `IntegrationAttribution.payload` copies arbitrary publisher JSON (nulls, any keys) from already-published packages. |
| Vocabulary | 7 field names / 4 types for an identifier; 13 digest names, 19 bare strings; 5 platform shapes; 3 timestamp shapes; enum values snake/kebab 12/5; 116 omitted vs 91 nullable properties. |
| Errors | Error envelope v1 on stdout, compact. Not schema-described. `error.detail` from 4 of 74 classifiers. `remediation` never emitted. Context-init and clap usage errors print no document under `--format json`. |
| Exit codes | `ExitCode` 19 variants, `ErrorCategory` 15 at the survey; [`adr_exit_code_taxonomy.md`](./adr_exit_code_taxonomy.md) collapses them to 13 and 11 before the baseline. No machine registry; `ocx-sdk-python` lacks 87. |
| Grammar | No export. `-c`, `-l`, `-g` overloaded. Type drift on `--group`/`--tags`/`--description`; 7 output-path spellings. Root defaults are read from the environment at `Command` build time (`default_value_t = env::flag(..)`). |
| Env | Typed seam `ocx_util::env` (106 call sites); ~46 raw `std::env::var*` reads; 37 constants vs 53 documented; `OCX_TEST_FAULT*` ungated ([#571](https://github.com/ocx-sh/ocx/issues/571)); `OCX_CEILING_PATH` undocumented ([#572](https://github.com/ocx-sh/ocx/issues/572)). `ocx-mirror` calls `ocx_util::env::{var, flag}` and `ocx_config::env::keys` at 20+ sites. |
| Lint lane | No root `clippy.toml`; rules_rust 0.74.0 ignores one until `.bazelrc` sets the `clippy.toml` label flag. |
| Consumers | `ocx-mirror` spawns `ocx --format json` at 6 sites with tolerant structs and runs a released, pinned binary. `satellite:verify` builds mirror only (no runtime parse) and is `continue-on-error`. `ocx-sdk-python` probes plain `ocx version` and accepts any newer binary. |

## Decision Drivers

| # | Driver | Weight | Test |
|---|---|---|---|
| C1 | Consumer safety | 5 | Is every shape break caught before release, and loud at runtime in a consumer? |
| C2 | Owned surface | 4 | How much non-domain code does OCX own (quality-core "Don't Own Non-Domain Code")? |
| C3 | Break volume | 4 | How many roots, fields and call sites in consumers break? |
| C4 | Codegen fitness | 4 | Can SDK types be generated from the documents without generics or `anyOf` heuristics? |
| C5 | Toolchain and hermeticity | 3 | Does the gate run inside the Bazel graph with no new tool in `ocx.toml`? |
| C6 | Time to first gated release | 2 | How soon is the gate on? (Never = 1.) |

**Non-negotiables** (owner, ratified; a filter):
- N1. A compat gate where breaks pass only with a recorded acknowledgement, and a generated Rust pilot SDK.
- N2. Zero runtime third-party dependencies per SDK, except documented per-language exceptions.
- N3. People learn about breaks from the changelog only, generated from commit subjects.
- N4. Coherence before width: the gate starts after phase 1.
- N5. An ecosystem consumer is upgraded in the same change series (`CLAUDE.md` Stability tiers).

## Industry Context & Research

| Artifact | Load-bearing finding |
|---|---|
| `research_cli_interface_contracts_prior_art.md` | cargo/terraform/pip/PEP 691 version the document in-band with prose rules; automated diffing lives in protobuf, OpenAPI, Kafka; terraform-exec is the shell-out SDK precedent. |
| `research_machine_output_compat.md` | No surveyed CLI wraps every command; bare arrays cannot carry a version; omit-never-null is the lowest-ambiguity codegen rule; default decoders fail on unknown enum values; internally tagged `type` unions. |
| `research_interface_vocabulary_enforcement.md` | Consistency by construction (shared types), then a floored lint, then prose; lintdiff-style rollout. |
| `research_interface_contract_tooling.md` | `clap_usage` drops env, `requires`, types; clippy `disallowed-methods` resolves by `DefId`; schemars `for_serialize()`; oasdiff diffs JSON Schema wrapped in stub OpenAPI 3.1. |
| `research_zero_dep_sdk_codegen.md` | No zero-dep generator dispatches tagged unions; Rust and Java lack std JSON; Rust lacks a std executor. typify's open-enum behaviour is unmeasured. |
| `research_interface_coherence_inventory.md`, `research_ocx_interface_recon.md` | The counts above. |
| Round-1 SOTA review | Zalando rule 112 and MCP `outputSchema` validation: openness must be in the published schema, not only in generators; Confluent: `additionalProperties: false` makes additions incompatible; Smithy/protobuf conformance suites: one language-neutral corpus for many generators; terraform-exec: the SDK owns spawn env, stdin and cancellation; I-JSON 2^53 limit. |

**Key insight.** Shared types make vocabulary unbypassable, a positive representation subset makes the schema small, and a differ over an explicit keyword allowlist can fail closed. The gate is only as honest as its link to the wire, so the runtime output is validated too.

## Dossier reconciliation

| Dossier item | Finding | Decision |
|---|---|---|
| "~111 raw env sites" | ~46 raw; the rest use `ocx_util::env` | Grow the existing seam |
| Registry in `ocx_config` | Lower crates read env and cannot depend on `ocx_config`; `ocx_util` is chartered domain-free | **New ecosystem leaf crate `ocx_env`** (std only outside `__testing`; uv's `uv-static` precedent): `EnvVar`, `env_vars!`, the seam and every declaration. Every crate that reads env takes a direct edge; `ocx_util` takes none and keeps only `PATH_SEPARATOR`, so its charter stays domain-free. Flag links live in `ocx_cli`, joined in `ocx_schema` |
| Baseline via `git show <tag>` | Bazel sandbox has no git; release sequence tags after verify | Committed baseline rotated after the tag (§ Baseline rotation) |
| Grammar export in phase 2 | Phase-0 lints need it | Export in phase 0; gate in phase 2 |
| `reports/v1 → v2` per break | A document-wide bump refuses every command for one root's break | Per-root and per-command versions; document `$id` only on layout change |
| ocx-mirror 3 sites, phases 1–2 | 6 spawn sites; phase 0 deletes APIs it calls | Mirror lockstep from phase 0; runtime consumer gate |
| Python SDK migration | Dossier puts non-Rust SDKs out of scope | Small pre-phase-1 bound release now; generated-types migration is a follow-up plan, not phase 2 |
| Enum addition additive | Signing ADR says a new `kind` bumps | Superseded (§ Enum policy) |
| `remediation` | Never emitted | Removed |
| Gate scope | 7 kinds exist | `reports`, `errors`, `cli`; others keep their ADR rules |

## Considered Options

### Option A — The dossier as stated
Wrap all 56 roots in `{schema_version, command, exit_code, data}`; error envelope v1 on stdout; OCX JSON grammar; in-house differ against `git show <tag>`; owned 6-language generator.

| Pros | Cons |
|---|---|
| One top level for success and error | Rewraps 50 roots; every `jq` filter and both consumers break |
| All Rust, all Bazel | `command`, `exit_code` duplicate argv and process status; `git show` cannot run in the sandbox |

**Steelman.** A wrapper gives warnings and streams a home, and OCX's own generator removes the generics objection. **Why it loses:** the largest break bill for information the process already carries.

### Option B — Research-adjusted
`usage` KDL + `usage diff`; bare roots with `format_version` and `items`; RFC 9457 errors on stderr; oasdiff over wrapped schemas; omit-never-null; owned generator.

| Pros | Cons |
|---|---|
| Two mature differs; oasdiff has 755 rule cases | OCX still owns the clap walk, a KDL patch layer and an OpenAPI adapter; neither differ reads the exit-code registry or OCX's grammar semantics |
| Best codegen shapes | Two tools outside the Bazel graph; KDL is a second dialect; stderr errors break v1 and share the channel with logs |

**Steelman.** Owning less differ code is what the don't-own bar asks; oasdiff encodes edge cases a fresh differ misses. **Why it loses on its own:** the in-house differ exists anyway for grammar and registry. The schema portion is not settled by assumption: the oasdiff-adapter comparison is a phase-2 entry criterion.

### Option C — Minimal (the off-ramp)
Phases 0 and 1 of D: vocabulary, lints, registry, coherence pass. No gate, no generator; SDKs hand-written.

| Pros | Cons |
|---|---|
| Most of the coherence value; no owned differ or generator | Fails N1: breaks stay invisible to programs |

### Option D — Coherent hybrid with an owned Rust backend
OCX JSON grammar with full clap semantics; object roots led by `schema_version`; errors on stdout in the v1 shape; in-house differ over an allowlisted subset; omit-never-null for OCX-owned structure; open enums in the published schema; owned generator IR with a Rust backend.

### Option E — D with typify for Rust types
Identical to D through phase 1 and for the gate. In phase 2, Rust SDK types come from typify over `reports.json`/`errors.json` (post-processed for open enums if needed); OCX owns only the command layer from `cli.json`, the exit/env constants and the outcome types. Ownership of an IR for other languages is decided per later SDK plan.

| Pros | Cons |
|---|---|
| Owns the domain part (argv layer) and delegates type emission | typify's open-enum and `x-ocx-*` handling unmeasured; a post-processor may be needed |
| Same gate and phases as D | Each later language re-opens the generator question |

### Trade-off matrix (re-scored after review)

Scores 1–5. C is scored as defined (D's phases 0–1): same break volume as D, codegen-ready schemas, no gate (C6 = 1). D's C1 is 4, not 5: its top risk is a differ miss.

| Criterion (weight) | A | B | C | D | E |
|---|---|---|---|---|---|
| C1 Consumer safety (5) | 4 | 4 | 2 | 4 | 4 |
| C2 Owned surface (4) | 2 | 3 | 5 | 2 | 3 |
| C3 Break volume (4) | 1 | 2 | 3 | 3 | 3 |
| C4 Codegen fitness (4) | 3 | 4 | 4 | 4 | 4 |
| C5 Toolchain (3) | 4 | 2 | 5 | 5 | 5 |
| C6 Time to gate (2) | 3 | 3 | 1 | 3 | 3 |
| **Weighted** | **62** | **68** | **75 (fails N1)** | **77** | **81** |
| Alt. weights (C2 5, C1 4) | 60 | 67 | 78 | 75 | 80 |
| Reversibility | one-way at wrap | one-way + 2 tool pins | phase 1 one-way | phase 1 one-way; gate two-way | as D |
| Top risk | 50-root break wave | translator drift | silent drift | differ or owned backend miss | typify open-enum gap |

**Reading the matrix honestly.** D and E beat C only because of N1; on merit the margin is small, and under the don't-own weighting C beats D. E's lead over D (4 points) rests on one unmeasured property: whether typify's output can carry the open-enum form. That is why phase 2 opens with a go/no-go on measured evidence, not on these scores.

## Decision Outcome

**Chosen: Option D's contract (phases 0 and 1, the gate, the SDK contract), with the phase-2 Rust type layer chosen between D and E at the phase-2 go/no-go** (open question 1).

**Rationale.** Every option that meets N1 shares phases 0 and 1, the root convention and the gate; those are decided now. The only unresolved axis is who emits Rust types, and two spikes settle it cheaply before anything is published.

### The six tensions

| # | Tension | Resolution |
|---|---|---|
| 1 | Grammar format | OCX JSON (`cli.json`), owned. `usage` fails the don't-own bar under criterion 2: the bridge drops env, `requires`, arity and types, exactly the contract-relevant semantics. `cli.json` captures requires, conflicts, arity, `require_equals`, `last`/terminator and `allow_hyphen_values` with directional rules. Future OpenCLI/MCP-tools emitters are IR backends, not second walkers. |
| 2 | Success envelope | One root convention, no wrapper: object root, `schema_version` first, list payloads as `{schema_version, items}` (a new name; `entries` is the existing spelling in 5 places and is migrated). |
| 3 | Error channel | stdout, v1 shape kept; pretty-printed (its own `!` commit, since line readers break); `remediation` removed; `error.detail` for every family; emitted for context-init and usage errors too. RFC 9457 rejected (renames every field; its `detail` is OCX's `message`); its "ignore unknown members" rule adopted. |
| 4 | Schema diff | In-house over an explicit keyword allowlist; every other keyword is `unmodelled` (red). Separate algorithms for surviving, added and removed roots and commands. Description changes need a `doc` or `semantic` ledger line. Ownership of the schema portion is re-checked against an oasdiff adapter at the phase-2 go/no-go. |
| 5 | Nullable form | Zero nullable forms in OCX-owned structure; opaque publisher payloads are exempt and preserved byte-for-byte. Null is stripped from the schema only where omission is established by the serializer; a required-nullable field stays visible and reds the lint. Runtime output is validated against the schema. |
| 6 | Enum additions | Additive everywhere. Openness lives in the published schema (`x-ocx-enum` scalar form, an explicit unknown arm on unions), not only in generators. Success decisions over status enums fail closed. |

### Quantified impact

| Metric | Before | After phase 1 |
|---|---|---|
| Roots with in-band version | 6 / 56 | 54 / 54 (emission lands in the last phase-1 release; `PackageDescription` and `PackageInspect` are nested-only and leave the root list) |
| Root shapes reshaped | — | 19 (6 unwrapped, 7 arrays → `items`, 4 maps, 2 unions) |
| Nullable OCX-owned properties | 91 | 0 |
| `anyOf` / closed enums in output schemas | 44 / 36 | 0 / 0 |
| Payload fields named `type` | 9 | 0 (renamed; `type` is the union tag) |
| Properties without description | 260 / 684 | 0 |
| Raw `std::env::var*` in production | ~46 | 0 + `#[expect]` residue pinned by a ratchet |
| Error families with `error.detail` | 4 / 74 | 74 / 74 |
| Commands printing no document on failure under `--format json` | context-init + usage paths | 0 |
| New runtime deps in `ocx` | — | 0 |

### Consequences

**Positive:** one integer per root, command and error document tells a program whether it understands the output, checked before it spawns; env drift and its two hand lists end; generated codes and slugs remove the Python SDK's class of bug; schemars marker machinery is deleted for `reports`/`errors`.

**Negative:** OCX owns a differ (~25 output rules, ~20 grammar rules) and at least a command-layer generator; phase 1 is a break wave across reports, flags and env; strict SDK parsing replaces mirror's tolerant structs.

**Risks**

| Risk | Mitigation |
|---|---|
| Differ misses a change class | Keyword allowlist with `unmodelled` default; corpus asserting every rule code; reader floor on keywords; runtime conformance hook; oasdiff comparison at phase-2 entry |
| Schema and wire diverge | Root wrappers never nested; acceptance-suite validation of every captured stdout; never-null mutation proof |
| Phase 1 never reaches zero waivers | Waivers are a ratchet; permanent decisions go to `exemptions.toml` with a cited ADR; C is the off-ramp |
| Mirror breaks without a red | Strict decode at mirror's 6 sites; `task satellite:contract` runs mirror's acceptance suite on them against the candidate binary, blocking |
| A retired env name silently stops protecting | Retired names warn in the window and exit 78 after it |
| Generated SDK passes attacker-controlled flags | `--long=value` tokens; leading-`-` positionals refused unless the grammar allows hyphen values |

## Go/no-go outcome

**Go, with Option D: OCX owns the Rust backend over its own IR. No typify.** Decided 2026-10-04 against the four criteria in § Phases row 2. The numbers come from `research_typify_spike.md` and `research_oasdiff_spike.md`, which name the command behind each one.

| Criterion | Result | Evidence |
|---|---|---|
| (a) Phase-1 exit | **Partially met; the rest is deferred, not waived** (plan D-17). On the ocx side it is met. Since `ba3cde130`, the conformance hook fails the test that captured the document. There are no waivers: `contract/waivers/` does not exist, and `contract_lint` asserts that none remain. `satellite:contract` is not green: X1 and X3 (ocx-mirror) are blocked on the other-repo grant, so mirror main carries no `ocx_contract_site` markers yet. Phase 2 starts on that basis. The mirror's move onto the SDK (phase-2 exit) waits for X1/X3. | Plan rows WP-28, X1, X3; D-17 |
| (b) typify spike | **Fails the open-question-1 rule.** Given the published schema, typify 0.8.0 emits **0 of 13** unions as tagged: all 13 are `untagged`, take the first arm that fits, and give no signal for an unknown tag. It gets to 10 of 13 only when fed a *closed copy* with the unknown arms stripped, and to 12 of 13 with an added `$ref`-inlining pre-pass. `Var` stays untagged and **silently mis-dispatches** an unknown tag to a known arm, and drops `type` when it serialises. The owned code around it measures **139 LOC**: a 70-line post-pass plus pre-passes. Even then, a known tag with a bad payload decodes as `Unknown` instead of failing; fixing that needs a tag-peeking `Deserialize` the 139 does not include. Known values also sit one level down, as `X::Known(XKnown::V)`, not the SD § 3.14 shape. To stay inside the SDK dependency budget, `date-time` and `pattern` validation must be dropped (`chrono`, `regress`). `Option` + default holds. | Spike §§ 1–3 |
| (c) oasdiff | **No-adopt; the in-house schema differ stays.** Even with severities tuned in-sample, oasdiff misses **12 of 37** corpus breaks (22 at default levels). It has no description rule (D01), no schema pointer for ledger matching, and reports per use site instead of per `$def`. Backing it would still require owned pointer reconstruction and code assignment, plus an unattested 18 MB Go binary outside the Bazel graph. | oasdiff spike, Verdict and Totals |
| (d) Owned LOC | **D ≈ 1,450 owned LOC (range 1,250–1,700); E ≈ 950 plus typify.** The other ~1,000 lines are the same in both: IR for commands, errors and env, the command layer, the runtime text and the CLI. D's extra cost is the type emitter (~350) and the type half of the IR (~150). Against that, E needs the 139 measured LOC, the uncounted tag-peeking decoder, a `Var` override, and a pinned pre-1.0 tool (typify 0.8.0, schemars 0.8, `syn`/`quote`/`prettyplease`) that reads a closed copy of the published schema. Consumer benefit is the same under both, because both feed the same SDK contract. D also emits the § 3.14 open form directly, so no post-pass is needed to get there. | Method below |

**How (d) was estimated.** By analogy with code already in `crates/ocx_sdkgen`, counting non-blank, non-comment production lines:

- `lint.rs` (996) walks the same three documents over the same subset that the IR loader will read.
- `subset.rs` (93) already resolves `$ref`s.
- `versions.rs` (91) already reads every version.

Applying that to the documents' sizes:

- The loader converts rather than checks, so it is about half the lint walker: ~450.
- The type emitter maps 276 `$defs` onto four shapes (struct, scalar enum, union, alias), including the type-peeking union decoder: ~350.
- The command layer covers 69 versioned commands and 388 arguments, and builds argv plus a typed outcome per output mode: ~300.
- The fixed runtime text (handshake, spawn, bounded output, cancellation) is ~300.
- The CLI is ~50.

This is an estimate, not a measurement. WP-37 reports the real count.

**OpenCLI / MCP tools, re-checked as IR backends.** `ir::Command` already holds what both formats need:

- a path
- typed arguments with long name, position, arity and choices
- help text
- output modes that name a report root

An OpenCLI document is a direct serialisation of that tree. An MCP tool list gives one tool per versioned command, with `inputSchema` built from `args` and the output schema taken as the root's `$ref` into `reports.json`. Each would be a second backend of about 150–200 LOC over the same IR, and needs no new IR concept. Two things make it not phase-2 scope:

- No consumer has asked for either.
- OpenCLI is still a draft.

The re-check favours D slightly: an owned IR spanning all three documents is what makes such a backend cheap. Under E, the types would sit outside the IR. Recorded as a possible follow-up, not scheduled.

## Technical Details

Component contracts and sketches: system design. This section fixes the rules.

### Contract scope

| Document | Kind | Published | Version carrier | Gated |
|---|---|---|---|---|
| Report roots | `reports` JSON Schema | `ocx.sh/schemas/reports/v2.json` | per-root `schema_version` | yes |
| Error document + exit/category/slug registry | `errors` JSON Schema (new) | `ocx.sh/schemas/errors/v1.json` | in-band `schema_version` = `$id` major | yes |
| CLI grammar, command→output mapping, env manifest (Public, Foreign, Plumbing; SDKs use Public) | `cli.json` data + `cli` JSON Schema | **unpublished** (golden; embedded in `ocx-sdkgen`) | per-command `version`; document `schema_version` for layout | yes |
| `metadata`, `config`, `project`, `project-lock`, `patch`, `execution-record` | existing | unchanged | existing ADR rules | no |

`ocx version --format json` gains `contract: { errors: n, commands: {<path>: n}, reports: {<Root>: n} }` (additive), so an SDK refuses a mismatched command before spawning it.

### Root convention

1. Every command declares its output modes in `cli.json`: `report(<Root>)`, `report_then_fail(<Root>)` (a valid report and a non-zero exit, e.g. partial sign, mixed sweep), `empty`, `passthrough` (`exec`: the child owns stdout), `shell_stream` (`--shell`, `--ci` emitters). A lint ties every registered root to at least one mode and every mode to a registered root.
2. Under `--format json`: a `report*` mode prints exactly one pretty JSON document; any failure before a report prints exactly one error document (including context-init and clap usage errors, when `--format json` or `--json` appears in argv before any `--`); `--quiet` suppresses reports, never error documents; `passthrough` prints nothing of its own once the child starts.
3. Every report root is an object whose first field is `schema_version` (starts at 1; the 6 formerly wrapped roots start at 2). The version is a property of the **published root**, not of the Rust type: root schemas are separate wrapper entries, never referenced from another `$def`.
4. No report root has a top-level `error` property, so `error` distinguishes the error document from a `report_then_fail` report.
5. Consumers ignore unknown properties. Producers never use `additionalProperties: false`.

### Versioning and the bump rule

| Change | Report root / command | `errors` document |
|---|---|---|
| Additive (new optional/required output field, new root, new enum value, new union variant, new command, new optional flag, widened input constraint, new public env) | no ledger, no bump | no ledger, no bump |
| Breaking finding, or a `semantic` entry | ledger entry; each affected surviving root/command = baseline + 1 | ledger entry; `schema_version` and `$id` major = baseline + 1 |
| Description change on an existing node | ledger line `doc` (no bump) or `semantic` (bump) | same |
| New root or command | version must be 1 | — |
| Removed root or command | ledger entry `B01`/`G01` naming it; no version check | — |
| Bump without entry / stale entry | red | red |

The `schema_version` property and document `$id` are excluded from the differ; the version step owns them. Versions count from the bootstrap tag (§ Baseline rotation). During phase 1, `schema_version` emission and the `reports/v2` `$id` ship together in the **last** phase-1 release, so no release carries a version number over a still-moving shape. Request-side version pinning (`cargo metadata --format-version`, GitHub's dated API version) was weighed and deferred: lockstep with per-item refusal is coherent pre-1.0; the post-1.0 trigger is a consumer that cannot upgrade with ocx.

### Baseline rotation

- Between releases the gate compares goldens with `crates/ocx_schema/contract/baseline/` (the previous release, immutable); ledger entries accumulate.
- `task release:prepare` verifies against that same baseline with the ledger intact. Commit and tag follow as today.
- After the tag exists, `task contract:rotate` copies `git show <tag>:…/golden/*` into `baseline/`, writes `RELEASE`, truncates the ledger, and lands as its own verified commit (`chore(contract): rotate the baseline to <tag>`).
- **Bootstrap:** the bootstrap tag is the release that first emits `schema_version` (the last phase-1 release). `task contract:rotate` runs right after that tag and creates `baseline/`. The gate test target exists from the moment phase 2 builds the differ, and its corpus and fixture cases always run. Its live-repository case compares against `baseline/` and has nothing to compare before the bootstrap rotation; that absence is not a silent skip, because the baseline lint reds whenever a bootstrap tag exists and `baseline/` is still missing (rotation pending).
- **Owner acts:** cutting the bootstrap release and running each rotation are owner/release acts. No feature branch does either.
- **Interim bump rule** (Q-W6 option c): from the bootstrap tag until the gate test exists, and permanently under a phase-2 no-go, any golden byte change to a report root, the `errors` document or a `cli.json` command node bumps that item's version by one. A T0 byte check compares with the newest `v*` tag's goldens: no differ, no ledger. It over-bumps by design (a description edit bumps).
- The T0 lint validates `RELEASE` (`^v\d+\.\d+\.\d+$`, resolved with `git rev-parse --verify --end-of-options refs/tags/<v>^{commit}`), byte equality with the tag, ancestry, and reds when a newer ancestor **release** tag (same pattern) exists and `HEAD` is not that tag's commit (rotation pending). Non-release tags (`backup/*`, `0.3.1`, …) are ignored. CI checkouts fetch tags. `contract/**` gets a CODEOWNERS entry.

### Enum policy (supersedes in part `adr_oci_referrers_signing_v1.md`)

> Adding a value to any enumerated output field — `error.kind`, `error.detail`, `exit_code`, a status or outcome enum, a union `type` tag — is additive and bumps nothing. The published schema states this: scalar enums are open (`type` plus an `x-ocx-enum` value list), unions carry an explicit unknown-variant arm, so a validating consumer accepts new values. Consumers MUST tolerate unknown values; a consumer deciding success MUST treat an unknown status or outcome as failure, and the exit code is authoritative. Every OCX-generated SDK emits open forms and fail-closed success predicates. Removing or renaming a value is breaking. `command` is an open string gated once, through `cli.json`.

`ENVELOPE_SCHEMA_VERSION` stays 1 through phase 1.

*Amended 2026-10-07 by [`adr_exit_code_taxonomy.md`](./adr_exit_code_taxonomy.md): retiring the `error.kind` values for 83–87 removes values and reusing 82 changes one's meaning, so the error document moves to v2 before the baseline is cut.*

### Representation subset (shared by lint, differ, generator)

| Construct | Form |
|---|---|
| Scalar enum | `{"type": "string"\|"integer", "x-ocx-enum": [{"value", "name", "description", …}]}`; exit codes also carry `category`, slugs carry `exit_code` |
| Tagged union | `oneOf` of object arms with required `type` `const`, plus one arm `x-ocx-unknown-variant` whose `type` is `not` the known values |
| Optional | omitted when unset; not in `required`; no `null` |
| Map | `additionalProperties: <schema>` |
| Opaque JSON | `{"x-ocx-opaque": true, "description"}` — publisher data, exempt from naming, vocabulary and nullability rules, compared as a leaf |
| Anything else | not allowed (lint `L17`), `unmodelled` in the differ |

### Vocabulary

| Concept | Rust type (crate) | Wire |
|---|---|---|
| Package identifier | `PackageRef`, `PinnedPackageRef` (`ocx_oci`) | string |
| Digest | `Digest` (`ocx_oci`) | `^sha(256\|384\|512):[0-9a-f]+$` |
| Platform | `Platform` (`ocx_oci`) | OCI platform object; flags take the string form |
| Version | `Version` (`ocx_package`) | string |
| Timestamp | `Timestamp` (new, `ocx_util`) | RFC 3339 UTC `Z`; field ends `_at` |
| Size | `ByteSize` (new, `ocx_util`) | integer bytes, `maximum` 2^53−1; field ends `size` |
| Path | `AbsolutePath`, `RelativePath` (`ocx_util::fs::path`) | string; typed, not name-guessed |
| Registry | `RegistryHost` (new, `ocx_oci`) | `host[:port]` |
| URL in errors | `RedactedUrl` (new, `ocx_oci`) | userinfo stripped at construction |
| Publisher payload | `OpaqueJson` (new, `ocx_util`) | any JSON, preserved |
| Status / outcome | per-report enum | named open `$def`, snake_case; dry runs are a status value |

Each type has one `JsonSchema` impl with a fixed `schema_name` and `schema_id`.

### Env contract

- **Read path:** `EnvVar` declarations in `ocx_env` are the only read path; `std::env` reads are banned by clippy (Bazel and cargo lanes); `dynamic()` reads data-derived names, ratcheted inside `//crates/...`. Satellites declare their own variables with the same macro.
- **Testing vars** are cfg-gated by the `env_vars!` expansion (`#[cfg(any(test, feature = "__testing"))]`) and absent from `DECLARED` and `all()`, so an ungated read fails a release build.
- **Retired names:** within a rename window the old name warns once (name only) and is honoured; after the removal release it exits 78 naming the replacement; never silently ignored. A polarity change never rides an alias: the old name retires to an error immediately. Invalid values on hardening variables (`OCX_FROZEN`, `OCX_OFFLINE`, `OCX_NO_CONSENT`, `OCX_NO_VERIFY`, …) exit 78 instead of falling back to the default.
- **Secrets:** `secret` entries are never formatted (values wrapped in a type without `Display`; parse warnings name the key only); child propagation (`scrub`/`inherit`/`forward`) is a separate attribute from which `CREDENTIAL_KEYS`, the `ocx_script` deny set and mirror's forward list derive. `error.context` keys must be vocabulary types; free strings and raw URLs are refused by lint.

### SDK contract (all languages)

- Handshake over `ocx --format json version`; minimum `ocx` = the contract release recorded in the generated `contract` table; a command whose own version or any output root version differs is **refused before spawning**.
- Commands with a `report_then_fail` mode return an outcome carrying both the typed report and the exit code.
- Argv: every flag value as one `--long=value` token; a positional starting with `-` is refused unless the grammar marks it `allow_hyphen_values`; trailing operands go after `--` where the grammar declares a terminator or `last`; secret-on-stdin flags take a bytes parameter, never argv.
- Spawn: absolute binary resolved once (explicit path, then `OCX_BINARY_PIN`, then `PATH`; Windows resolves `ocx.exe` only); `OCX_*`/`__OCX_*` removed from the child env unless passed through explicitly; stdin closed by default; bounded output; cancellation interrupts, then kills after a grace period. The handshake is a compatibility check, not a trust boundary.

### Don't-own assessment

| Component | Verdict |
|---|---|
| Grammar model, walk, differ | Own: `usage` leaks env, `requires`, arity, types (criterion 2) |
| Registry differ (codes, slugs) | Own: no tool models it |
| Schema differ | Own over the allowlisted subset. The oasdiff adapter was re-checked at phase-2 entry and rejected (§ Go/no-go outcome (c)) |
| Lint walker | Own: vocabulary rules need a walker |
| Generator | Command layer: own (domain). Rust types: own, Option D (§ Go/no-go outcome (b), (d)). Other languages: decided per SDK plan. Backends emit external syntax, so each keeps golden output and is compiled and integration-tested in its SDK's CI |
| JSON, schema generation, clap parsing | Delegate: `serde_json`, `schemars`, `clap` |

### Zero-dependency exceptions

| SDK | Runtime dependency | Justification |
|---|---|---|
| Rust | `serde`, `serde_json` | std has no JSON |
| Rust | `tokio` (feature, off by default) | std has no executor |
| Java (later) | shaded, relocated JSON library | JDK has no JSON |

### CLAUDE.md amendment

In § "Stability tiers", the **Interfaces** list gains "`--format json` reports and error document, the `OCX_*` environment variables, the `ocx-sdkgen` CLI and its output layout". After "**Even interfaces break pre-1.0.**", insert:

> **Machine-facing break records.** Once the interface-contract baseline exists (`adr_ocx_interface_contract.md`), a break to a gated machine document — a `--format json` report root, the error document, or a command in `cli.json` — also needs an entry in `crates/ocx_schema/contract/ledger.toml` and a bumped version on what broke; the compat gate fails without both. The entry and the number are for programs: SDKs and scripts detect the break from the version, before they run the command. People still learn about it from the changelog, through the commit subject, and nowhere else. The ledger is not release notes and adds no migration prose to user docs.

Open question 3 may add one sentence about env spellings in flight. The § Stability tiers ecosystem-crate enumeration gains `ocx_env`. The § Architecture table gains `ocx_env` [ecosystem] and `ocx_sdkgen` [internal; CLI interface]. The same change edits `arch-principles.md`'s crate table (edges) and `adr_crate_split_workspace.md` (crate map, § Stability tiers list, `ocx_config` charter, § "`env` splits" marked superseded).

## Migration and Rollout

### Phases

| Phase | Scope | Entry | Exit |
|---|---|---|---|
| **0 — Vocabulary, lints, env registry** | `clippy.toml` + Bazel wiring; `ocx_env` declarations, seam API, every read routed, Testing cfg gating, retired-name scan; **ocx-mirror env migration in the same change series**; `ocx-sdk-python` upper-bound release; `task satellite:contract` (blocking consumer gate); vocabulary types; `errors` kind; `cli.json` export with full semantics, output modes and hermetic defaults; lint walker with waivers and exemptions; runtime conformance hook (warn-only until phase 1 ends); docs coverage tests; sync rule; CLAUDE.md text | V0 red/green proof incl. inverse and a `#[cfg(test)]` seed | All lints green with waivers; `disallowed_methods` baseline 0; `task satellite:verify` **and** `task satellite:contract` green at the phase-0 submodule pointer; V13 release-build proof red/green |
| **1 — Coherence pass** | Reports (root convention, vocabulary, never-null, open enums, tagged unions, `type` fields renamed), errors (all families, init/usage documents, pretty print), flags (short collisions, type drift, `--output`), env (naming, polarity, retirements, hardening `on_invalid`), the 105 JSON-parsing acceptance modules and `SUITE_FLOOR`, `command-line.md` JSON sections (no migration prose), `schema.taskfile.yml`/genrule/`main.rs` for 8 schema files and `reports/v2`; mirror fixed in the same series for every root it parses | Phase 0 exit | Waivers empty; conformance hook blocking and green; `satellite:contract` green; all flag renames and `schema_version` emission in their single releases |
| **2 — Baseline, gate, SDK** | Bootstrap rotation; compat gate; `ocx-sdkgen` (command layer + Rust types per open question 1); conformance corpus; `ocx-sdk-rust`; mirror's 6 sites onto the SDK; `machine-interface.md` | **Go/no-go:** (a) phase-1 exit met; (b) typify spike on the post-phase-1 `reports.json`: tagged dispatch, open-enum form, `Option` + default, measured; (c) oasdiff-adapter comparison on the mutation corpus: findings, adapter LOC, toolchain cost (pinned version + checksum, run without secrets); (d) owned differ + generator LOC estimate against consumer benefit. No-go = stop at C; the interim bump rule (§ Baseline rotation) then stays permanently | Gate red/green (V6); corpus green in Rust CI; SDK integration against real `ocx`; mirror on the SDK with `satellite:contract` green |

`ocx-sdk-python`'s move to generated types is a follow-up plan after phase 2 (dossier scope).

### Communicating breaks

Commit subjects only, one `!` commit per user-visible break (the error document's pretty print included). No `CHANGELOG.md` edit, no migration prose. From phase 2, ledger + version (programs).

### Flag renames — the batched window

All phase-1 flag renames ship in one release using the `CLAUDE.md` carve-out (hidden variants in `deprecated.rs`, listed in `RENAMED`, one stderr warning, removal release named). If that release is a 0.6.x, they join the open 0.6 → 0.7 window. A freed short letter is not rebound in the same release. Choice-value renames use a parser that recognises the old spelling to warn. `ArgSpec.deprecated` exports the window so the differ classifies "hidden + deprecated" as additive and its removal in the named release as additive.

### Waivers and exemptions

`contract/waivers/<area>.toml` (`{rule, document, pointer, reason}`; one file per area, read together) is a ratchet: un-waived violations and stale waivers both red; it reaches zero at phase-1 exit. `contract/exemptions.toml` (`{rule, pointer, reason, adr}`) holds permanent, decided exceptions (borrowed OCI vocabulary, open question 2's `-g`); every entry cites a decision record and a stale entry reds.

### Consumers

| Consumer | Phase 0 | Phase 1 | Phase 2 |
|---|---|---|---|
| `ocx-mirror` (lockstep) | Moves to `ocx_env` statics; declares its own variables (`GITHUB_ACTIONS`, `GITLAB_CI`, `NETRC`, …) with `env_vars!` and declares a `__testing` feature; computed auth names via `dynamic()`; `CREDENTIAL_KEYS` from the registry. Wiring: an `ocx_env` row in its root `[workspace.dependencies]`, `ocx_env` in `crates/crate_map.toml` `[ocx] allowed`, Bazel `@crates//:ocx_env` deps and a lock repin. A `std::env` clippy ban is optional for mirror (residue: 45 `set_var`/`remove_var` calls in 5 test files). **Strict decode at the 6 sites:** no `#[serde(default)]` on any field mirror acts on (`SweepEnvelope.data` first); one assertion per site in mirror's acceptance suite on a parsed field. `satellite:verify` and `satellite:contract` green | Each break to a root it parses lands with the mirror fix; `satellite:contract` red without it | 6 sites onto `ocx-sdk-rust`; `satellite:contract` regenerates the SDK from in-tree goldens and builds mirror against it; one mirror commit moves submodule pointer, binary pin and SDK version together (mirror owns skew) |
| `ocx-sdk-python` | Release with `MAX_SUPPORTED` = last pre-phase-1 ocx; refuses above it before executing; tested | — | Follow-up plan |
| Scripts | — | Changelog | Version field + changelog |

**Cross-repo work.** `ocx-mirror`, `ocx-sdk-python` and the new `ocx-sdk-rust` are separate repositories. `plan_ocx_interface_contract.md` plans their changes as their own work packages, which land in their own repos. The ocx-side change series pairs with the mirror branch (N5): the ocx PR does not merge before its paired mirror change.

### Out-of-scope security findings

| Finding | Placement |
|---|---|
| `OCX_TEST_FAULT*` ungated in release | [ocx-sh/ocx#571](https://github.com/ocx-sh/ocx/issues/571): cfg gate, rename `__OCX_TESTING_FAULT*`, bounded pause. Phase 0's cfg-gated `Testing` expansion prevents the class |
| `OCX_CEILING_PATH` undocumented | [ocx-sh/ocx#572](https://github.com/ocx-sh/ocx/issues/572): Public, documented; canonicalise or document the lexical comparison |

## NFR coverage

| NFR | Effect |
|---|---|
| Runtime performance | One integer per document; one flatten allocation per invocation. SDK refuses before spawn: one extra handshake process per SDK client. |
| Verify time | `ocx_schema`'s golden tests depend on `ocx`, so they re-run on most Rust edits; the lint and gate tests live in `ocx_sdkgen`, read goldens as data and re-run only when a document changes; T0 lints read committed files; `satellite:contract` is a pre-ready gate in `verify-deep`, not part of `task verify`. |
| Build | `clippy.toml` invalidates the clippy cache once. Any `ocx_env` edit, doc text included, changes the `ocx` binary (`EnvVar.doc` is compiled in): most crates rebuild and every cached acceptance target re-runs, plus `rust:build`, clippy and unit tests. Accepted: declarations change rarely, and a separate docs table would split each declaration across two files. |
| Compatibility | Phase 1 break wave; from phase 2 every break is recorded and detected before spawn. |
| Security | Declared reads only; Testing vars absent from release builds; secrets never formatted; retired names never fail open; argv option injection blocked; `error.context` limited to redacting vocabulary types. |
| Maintainability | Three hand lists become one registry; vocabulary drift becomes a compile or lint error. |
| Supply chain | No new tool in `ocx.toml`; oasdiff only in a pinned one-off spike; `ocx.sh/ocx/sdkgen` signed under the release identity; `ocx-sdk` via crates.io trusted publishing. |

## Verification

Every gate has a reachable red and green (quality-core "Unchecked Green"); proofs gate on the mutation being present.

| ID | Contract | Red | Green |
|---|---|---|---|
| V0 | Clippy ban, Bazel (**phase-0 entry**) | Seed `std::env::var` in a lib and in a `#[cfg(test)]` module → `task rust:clippy:check` red | Seeds removed. **Inverse:** `.bazelrc` line removed, seed present → green |
| V0b | Clippy ban, cargo | Same seed → `cargo clippy -- -D warnings` red | Seed removed |
| V1 | Seam takes declarations | `compile_fail` doctest: `ocx_env::var("X")` (no `&str` read exists) | registered reads compile; `dynamic()` ratchet count unchanged |
| V2 | Env docs two-way | Remove a heading / add `OCX_BOGUS` / < 50 headings parsed | Restored |
| V3 | Lint | `contract_lint_red.json` fires exactly the full rule set; new violation; stale waiver or exemption; an unlisted keyword (`allOf`) → `L17`; visited counts below independent counts | Current tree |
| V4 | Help required | Blank an `#[arg]` doc → `C01` | Restored |
| V5 | Grammar golden | Rename a flag; mutate each of requires/conflicts/num_args/require_equals/last/allow_hyphen_values → `cli.json` changes and the probe test agrees with parsing | `OCX_OFFLINE=1` in the walker env leaves `cli.json` byte-identical |
| V6 | Compat gate | `compat_corpus/` yields exactly the full set of differ codes; bump without entry; stale entry; `minItems` added → `unmodelled`; description change without `doc`; one case per `x-ocx-enum` member change (`value` removed B05, `name` B08, `description` D01, `category` R02, `exit_code` R03, unlisted member U01) | Entry + bump → exactly one finding; acknowledged root deletion green; new root at version 1 with no entry green; `errors` description change with a `doc` entry and no bump green |
| V7 | Baseline | Byte flip; `RELEASE` = non-tag ref; newer ancestor release tag with rotation pending | Fresh rotation; release commit itself; a non-release tag (`backup/*`) on an ancestor newer than `RELEASE` → green |
| V8 | Root wrappers | Nest a root type in another report → wrapper stays top-only and `L14` reds if referenced; `Printable` without `SCHEMA_VERSION` fails to compile | All 54 wrappers top-level |
| V9 | Never-null honest | Remove `skip_serializing_if` from an `Option` field → `L03` red | Restored |
| V10 | Opaque payloads | `NeverNull` or `L03` descends into an `x-ocx-opaque` leaf (leaf guard removed) → the fixture payload's `null` is stripped or `L03` fires | A published-package payload with nested `null` and non-snake keys passes lint and appears byte-identical on stdout |
| V11 | Runtime conformance | Drop a field from the wire only (`#[serde(skip_serializing)]`) → hook red; observed root not in the command's declared modes → red; `ocx` emits a status value missing from its `x-ocx-enum` → red; floor on distinct roots validated | Suite green |
| V12 | Error documents | Bad `OCX_*` config under `--format json` with no document → red | One document, exit 78; clap usage error → one document, exit 64 |
| V13 | Testing vars | Ungated `.get()` of a Testing var → `cargo build --release -p ocx --locked` red | Gated → green |
| V14 | Retired env | Retired name in window → stderr names it (no value); after removal → exit 78; renamed var silently ignored → red | Replacement set, retired name unset → no warning, exit 0; retired name in window, replacement unset → old value honoured |
| V15 | Secrets | Poisoned `OCX_AUTH_*_TOKEN`, `OCX_IDENTITY_TOKEN`, userinfo proxy/mirror URLs: bytes on stdout/stderr → red; secret var with invalid bool echoes value → red; name heuristic without `secret` → red | Clean |
| V16 | Registries | Slug without producer / one slug two codes; Plumbing named `__OCX_TESTING_*`; non-Foreign name not reserved; flag link to unknown flag; shim literal not declared | Clean |
| V17 | Generator | Two runs differ → red; `anyOf` input → IR refusal | Deterministic |
| V18 | SDK | Unknown enum and union tag decode as `Unknown`; unknown status → `is_success() == false`; partial sign and mixed sweep → outcome with report + exit; only a command's `cli` version changed → zero spawns beyond the handshake; identifier `--env=X=1` → refused, never in argv | Integration suite against pinned `ocx` |
| V19 | Conformance corpus | Any backend failing a corpus case (unknown value, unknown property, omitted optional, 2^53 size, version mismatch) | Rust backend green |
| V20 | Consumer gate | Run once per root mirror parses: a report-only mutation of that root → `satellite:contract` red. Named case: re-nest the sweep rows under `data` (the `SweepEnvelope` unwrap) → red; fewer than 6 sites passed against the real binary → red | Same-series mirror fix → green; `satellite:verify` green at the phase-0 pointer |
| V21 | Command reference | Flag missing from its section; global flag missing from General Options; < (measured count − margin) command nodes | Restored |
| V22 | Python bound | Fake binary reporting the phase-1 version → executes | Refused before executing |
| V23 | Sync rule | Missing catalog row or undeclared identical-pattern overlap → `task claude:tests` red | Rows added |
| V24 | Interim bump rule | Golden byte change to one root, the `errors` document or a command node without a bump → red; bump without a byte change → red | Changed item at tag version + 1; unchanged tree at the tag version |

## Open Questions

1. [NEEDS CLARIFICATION: who emits the Rust SDK's types — an owned Rust backend over an OCX IR (D) or typify plus an owned command layer (E) — and does OCX own an IR for all six languages?]
   **Decided 2026-10-04: D** (§ Go/no-go outcome). Every other language is still decided in its own SDK plan.
   **Recommended (as written):** decide at the phase-2 go/no-go from the typify spike. Adopt E if typify produces internally tagged dispatch and `Option` + default, and its enums can be post-processed into the open form in under a day of owned code; otherwise D. Decide ownership for every other language in its own SDK plan against measured generator output.
2. [NEEDS CLARIFICATION: is `-g` (`--global` at the root, `--group` on subcommands, settled earlier as "position disambiguates") a permanent exemption from "one meaning per short letter"?]
   **Recommended:** yes, as an `exemptions.toml` entry citing that ruling; apply the rule to `-c` and `-l`.
3. [NEEDS CLARIFICATION: may env-name spellings in flight live in the `ocx_env` registry (`RETIRED`) rather than only in `deprecated.rs`, which `CLAUDE.md` names as the single home?]
   **Recommended:** yes. The registry must hold retired names permanently anyway (post-removal exit 78), so `deprecated.rs` gains a `RENAMED_ENV` table that a test keeps equal to the in-window subset of `RETIRED`; deleting `deprecated.rs` then forces the window closed. `test/lint/test_deprecated_spellings.py` reads `RENAMED_ENV` beside `RENAMED`, so the stale-spelling sweep covers env names too. Add that sentence to the CLAUDE.md amendment.

## Deferred findings

| Finding | Why deferred | Recommendation |
|---|---|---|
| Retired schema URLs (`reports/v1.json` 404s once v2 publishes; only the current version of each kind is published today) | A website commitment, the owner's call | Keep publishing frozen copies of every retired `$id` version |
| `ocx_script`'s `ocx.env` deny is cosmetic: `ocx.run` children inherit `OCX_AUTH_*` and `OCX_ANNOUNCE_TOKEN` (pre-existing) | Needs a ruling on whether a `--script` script is a trust boundary | If it is, scrub the child env from the registry's `secret` set; if not, delete the deny |

## Handoff list (for `/hex-plan`)

- File an issue for `rust-cargo.md` LINT-09's remaining `clippy.toml` entries (`process::exit`, `thread::sleep`).
- CODEOWNERS for `crates/ocx_schema/contract/**`; CI checkouts fetch tags for the baseline lint.
- Sign `ocx.sh/ocx/sdkgen` keyless under the release identity; SDK repos pin it by digest with a `[[trust.policy]]`; `ocx-sdk` via crates.io trusted publishing.
- oasdiff spike: pin version + checksum, run without secrets, record both in the spike artifact.
- Re-audit env reads inside dependencies on each bump (`SSL_CERT_*`, proxy variables); assert clap's `env` feature stays off.
- Phase-1 never-null rewrite: mandatory `semantic` review of verification, signature, attestation, sweep and push-attestation roots (absent ≠ empty, e.g. `VerificationReport.signatures`).
- A crate-local `clippy.toml` would shadow the root one; the sync rule says so.
- An OpenCLI / MCP-tools emitter is a future IR backend, re-checked at phase-2 entry.
- `ocx-sdk-python` generated-types migration: its own plan after phase 2.

## Review dispositions

Buckets: **A** actionable (applied), **D** deferred (human call), **S** stated convention, **T** trivia. Ids: `SP` spec, `Q` quality, `SE` security, `SO` SOTA, `CX` Codex (numbered in file order). Where = section of this ADR (ADR) or the system design (SD).

| Finding ids | Bucket | Resolved in |
|---|---|---|
| SP-B4, Q-B2, CX-7 (mirror phase-0 env migration) | A | ADR Phases row 0, Consumers; SD 3.4 satellite API |
| SP-B3, CX-6, Q-W11 (runtime mirror gate, pin triangle) | A | ADR Consumers, V20; SD 3.12 |
| SP-B2, CX-9, SE-W8 (baseline rotation, `RELEASE` validation) | A | ADR Baseline rotation, V7; SD 3.9 |
| SP-B1, Q-S3 (nested roots, `SweepReport` versions) | A | ADR Root convention 3, V8; SD 3.2 |
| Q-B1, SP-W4, SO-G2, SP-W1, SP-S6, SP-S7, CX-10 (allowlist differ, root algorithms, `additionalProperties: false`) | A | ADR tension 4, Versioning, Representation subset; SD 3.7, 3.9 |
| SP-W2, CX-8, SO-G1 (scalar enums vs unions, open enums in schema) | A | ADR Enum policy, Representation subset; SD 3.3, 3.7 |
| CX-1, CX-2, SP-W11 (honest never-null, opaque payloads, runtime conformance) | A | ADR tension 5, V9–V11; SD 3.2, 3.10 |
| CX-3, SP-W5, SP-W6, SE-W4, SE-W7 (grammar semantics, hidden/deprecated transitions, hermetic defaults, stdin secrets) | A | ADR tension 1, V5; SD 3.6, 3.9 |
| CX-4, SO-G4, SP-W9 (command→output mapping, one-document rule, init/usage errors) | A | ADR Root convention 1–2, V12; SD 3.6 |
| CX-5, SE-W9 (report-then-fail outcomes, fail-closed success) | A | ADR SDK contract, Enum policy, V18; SD 3.15 |
| CX-11, Q-W4 (refuse before spawn; per-command and per-root versions) | A | ADR Contract scope, SDK contract; SD 3.15 |
| SE-B2 (argv option injection) | A | ADR SDK contract, V18; SD 3.15 |
| SE-B1, SE-W5, SP-W15 (Testing cfg gating, Plumbing prefix, registry V rows) | A | ADR Env contract, V13, V16; SD 3.4 |
| SE-B3 (retired env names fail open) | A | ADR Env contract, V14; SD 3.4 |
| SP-W7, Q-W9 (env spellings outside `deprecated.rs`) | D | Open question 3 (interim design in SD 3.4) |
| SE-W1, SE-W2, SE-S4 (secret attribute, `error.context` redaction, SDK Debug) | A | ADR Env contract, V15; SD 3.3, 3.4, 3.15 |
| SE-W6 (typed values flatten validation) | A | ADR Env contract; SD 3.4 |
| SE-W3, SO-G6 (binary resolution, SDK spawn contract) | A | ADR SDK contract; SD 3.15 |
| CX-12, SP-S8 (Python upper bound) | A | ADR Consumers, V22 |
| Q-W10 (Python migration out of scope) | A | ADR Phases note, Handoff |
| SO-G3, SO-G8 (conformance corpus, 2^53) | A | ADR Phases row 2, V19, Vocabulary; SD 3.14 |
| Q-W1, Q-W2, Q-W3, CX-13, SP-S1 (generator ownership, matrix scoring, don't-own consistency, oasdiff comparison) | A (residual D) | ADR Options E, matrix, Decision, go/no-go; residual = open question 1 |
| Q-W5 (description-only semantic breaks) | A | ADR Versioning; SD 3.9 |
| Q-W6 (`schema_version` during phase 1) | A | ADR Versioning |
| Q-W7 (reversibility placement; `cli` schema unpublished) | A | ADR Metadata, Contract scope |
| Q-W8 (registry in `ocx_util` breaks its charter) | A | ADR Dossier reconciliation; SD 3.4 |
| SP-W3 (permanent exemptions vs empty waivers) | A | ADR Waivers and exemptions (the `-g` call: open question 2) |
| SP-W8 (crate-map edges, `ocx_exit` schema) | A | SD 3.3, 4 |
| SP-W10 (acceptance tests, docs, schema publishing in plan) | A | ADR Phases row 1 |
| SP-W10(c) (retired `$id` URLs) | D | Deferred findings |
| SP-W12, Q-S4 (test-code clippy residue) | A | ADR V0; SD 3.5 |
| SP-W13, Q-S2 (`ocx_sdkgen` tier, BUILD wiring, shared subset module) | A | ADR CLAUDE.md amendment; SD 2, 3.14, 4 |
| SP-W14, Q-S7 (L13 definition, relative paths) | A | ADR Vocabulary; SD 3.8 |
| SP-W16 (V12 floor unit, global flags) | A | ADR V21; SD 3.11 |
| SP-S5 (compile_fail doctest) | A | ADR V1 |
| SP-S9 (target release of the window) | A | ADR Flag renames |
| SP-S10 (overlap table identical patterns) | A | SD 3.13 |
| SP-S11 (env manifest row) | A | ADR Contract scope |
| Q-S5 (verify-time NFR) | A | ADR NFR |
| Q-S6 (pretty print own `!` commit) | A | ADR tension 3 |
| Q-S8, SE-S1, SE-S2, SE-S3, SE-S5, SO-G5, SO-N2 (follow-ups) | A | ADR Handoff list |
| SO-G7 (request-side pinning, minimum-ocx rule) | A | ADR Versioning, SDK contract |
| SO-N1 (`schema_id` with fixed names) | A | ADR Vocabulary |
| SE-D1 (`ocx_script` trust boundary) | D | Deferred findings |
| Q-S1 (zero runtime deps survives) | S | ADR Zero-dependency exceptions |
| SE-S6 (`OCX_CEILING_PATH` semantics) | S | Tracked in [ocx-sh/ocx#572](https://github.com/ocx-sh/ocx/issues/572), ADR Out-of-scope findings |
| SP-S2, SP-S3, SP-S4 (root-shape sum, `items` spelling claim, `type` count) | T | ADR Context, tension 2, Quantified impact |
| Validation review B1, W1–W9, S1–S10 (strict mirror decode, per-root V20; bootstrap tag + interim bump rule; release-tag filter; `errors` `doc` entry; `x-ocx-enum` members; `ocx_env` edges and amendments; mirror wiring; V10/V14 sides; rebuild fan-out; closed producer hook; small fixes) | A | B1: Consumers, V20; SD 3.12. W1: Versioning, Baseline rotation, Phases row 2, V24; SD 3.9. W2: Baseline rotation, V7; SD 3.9. W3, W4, S5, S9: V6; SD 3.7–3.9. W5: Metadata, Dossier, CLAUDE.md amendment; SD 2, 4. W6, S7: Consumers; SD 3.4. W7: V10, V14. W8: NFR Build. W9: V11; SD 3.10. S1–S3, S10: Root convention 2; SD 2, 3.3–3.6. S4: open question 3. S6: SD 4. S8: Env contract |

Counts: 94 findings, 47 after merging duplicates. A 86 (generator ownership leaves open question 1), D 3 (SP-W7 and Q-W9 → open question 3; SE-D1) plus the SP-W10(c) sub-part, S 2, T 3. Validation pass: 20 more, all A.

## Links

- `system_design_ocx_interface_contract.md`
- `adr_oci_referrers_signing_v1.md` (superseded in part), `adr_crate_split_workspace.md`, `adr_bazel_build_adoption.md`, `adr_test_speed_tiers.md`, `adr_dependency_manifest_pinning.md`, `adr_exec_resolution_record.md`, `adr_declared_binaries_metadata.md`, `adr_cli_plugin_pattern.md`, `adr_package_integrations.md` (opaque integration payloads)
- Research: the seven artifacts in § Industry Context; reviews: `review_adr_interface_contract_*.md`

---

## Changelog

| Date | Author | Change |
|---|---|---|
| 2026-10-03 | Architect (Opus) | Initial draft, Proposed |
| 2026-10-03 | Architect (Opus) | Round-1 review fixes: mirror phase-0 lockstep + runtime gate; baseline rotation after tag; root wrappers; keyword-allowlist differ with root algorithms; open-enum representation; opaque payloads; grammar semantics and output modes; SDK outcomes, refusal before spawn, argv rules; env Testing gating, retired names, secrets; Option E and re-scored matrix; review dispositions |
| 2026-10-03 | Architect (Opus) | Validation fixes: B1 strict mirror decode + per-root V20; W1–W9; S1–S10; Accepted |
| 2026-10-03 | hex-plan (Opus) | Plan-review amendments: roots 56 → 54 (nested-only `PackageInspect`/`PackageDescription`); `waivers/<area>.toml`; gate target always built, live case guarded by the baseline lint; byte lint never deleted, inert once the gate is live |
| 2026-10-04 | hex-execute (Opus) | § Go/no-go outcome: phase 2 goes with Option D; oasdiff not adopted; criterion (a) partially met (mirror deferred per plan D-17); open question 1 and the Don't-own rows updated |
