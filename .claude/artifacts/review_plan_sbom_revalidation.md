# Re-validation pass — SPEC focus, closure verification only

Target: `.claude/state/plans/plan_sbom_attestations.md` (257 lines, post-fix).
Inputs: `review_plan_sbom_{spec,security,architect,codex}.md` + SOTA findings (a)-(d).
Method: per-finding closure check (outcome-equivalence, not literal-text compliance),
then a coherence sweep of the rewritten sections only.
Reviewer: opus. Not a fresh review — no new-finding hunt outside the sweep scope.

Verdict: **Needs Work** — 5 actionable, 0 deferred. All five are text edits inside
existing cells; no WP re-cut, no wave change, no DAG change. R-1 is the must-fix:
it can silently revert a Block-tier fix from this same round.

## Closure table

| Finding | Fix located | Closed |
|---|---|---|
| spec F-1 WP0 under-declares identifier rename | WP0 cell now lists `verify/pipeline.rs`, `sign/{bundle,rekor}.rs`, `auto_verify.rs`, `adversarial.py` + "run the greps fresh"; register rows 2/3/5; line 173 inheritance clause | yes |
| spec F-2 flag rename has no file set | WP0 "Flag census" enumerates def `:107`, 5 lib doc sites, 4 test files, README, both website pages; cosign-interop exemption in bold | yes |
| spec F-3 builder pin unowned + BuilderMismatch uncounted | C-017 = 17 variants / 23→40 incl. `BuilderMismatch`; WP6 file cell names the enforcement; ADR amended at 9f057b4b | yes (but see R-1) |
| spec F-4 WP8 Depends vs mermaid; WP3→WP8 wrong | WP8b `WP6 WP1`; WP3→WP9b in both table and graph; all 19 rows cross-checked edge-by-edge | yes |
| spec F-5 WP6 omits tlog.rs; "single exception" false | superseded by restructure: WP5 owns tlog.rs FULLY implemented (no stub); C-012 split `(tlog half)`/`(binding half)`; claim replaced by 9-row register | yes |
| spec F-6 WP2 `light` on a wire-format change | WP2 `panel`; WP3 `panel` | yes |
| spec F-7 module-declaration files missing | `lib.rs`→WP3, `oci.rs`→WP-A, `oci/attest.rs`→WP8a, `oci/verify.rs`→WP6 | yes |
| spec F-8 WP0/WP11 both own the 3 slug rows | WP11 carries "already done in WP0 — do not re-edit" | yes |
| spec F-9 Status block schemas contradict | `State: plan-approved` + `Step: awaiting /hex-execute` now agree | yes (plan half; rule half is out of plan scope, see note) |
| sec F-1 row 13 unowned, stub on shipped path | WP5 implements + wires + unit-tests it; no stub exists | yes |
| sec F-2 WP5 cannot compile against declared deps | WP-A created as the wave-1 leaf (C-001 constants + C-005 `PredicateType`); WP5 Depends `WP0 WP2 WP-A` | yes — this is the reviewer's own preferred variant |
| sec F-3 stub semantics unpinned | "Stub semantics (binding for every WP)" block: refuse, never `Ok`, never `todo!()`; plus the re-cut that eliminated the two stubs | yes |
| sec F-4 four red-before-green mutations unowned | WP10b scope, with "prove the mutation landed AND the restore landed" | yes |
| sec F-5 fixture pointer + 4 uncovered fixtures | WP10-fix repointed to "ADR Testing Strategy fixture data"; S-016..S-019 added and each routed to a WP | yes |
| sec F-6 cross-wave sharing under-declared | 9-row register added | partial — see R-2 |
| sec F-7 WP8 Depends vs mermaid | same as spec F-4 | yes |
| arch F1 WP0 under-declares | same as spec F-1/F-2 + line 173 | yes |
| arch F2 WP5/WP6 seam | region split adopted: WP5 = mode-agnostic region + tlog; WP6 = candidate loop; WP5 L→M | yes — see R-5 for the Scope-column residue |
| arch F3 split WP8 | WP8a/WP8b, wave 4 width 2, register row 6 with the 23-line justification | yes |
| arch F4 extract sign-common early; split WP9 | WP9-pre wave 2 (S) incl. the `iso8601` move from `package_verify.rs:305`; WP9a/WP9b wave 5 | yes |
| arch F5 split WP10 | WP10a/WP10b wave 6 + WP10-fix wave 5 | yes |
| arch F6 do not split WP11 | WP11 kept whole | yes (no action was owed) |
| arch F7 WP5 needs `raw_value`, no WP2 edge | edge in table and graph; WP2 cell carries "added to, never replace `preserve_order`" | yes |
| arch F8 three module-decl edits | all three present | yes |
| arch confirmations: `task schema` bootstrap | line 187 | yes |
| codex A WP2 omits `oci/sign/pipeline.rs` | WP2 cell, with the `:232` call site and both annotations named | yes |
| codex B WP2 omits `api/data/push.rs` | file added + register row 9 | partial — see R-3 |
| codex C `command.rs` module rows | WP9-pre, WP9a, WP9b all list it; register row 8 declares the same-wave WP9a/WP9b collision as expected-and-trivial | yes |
| codex D critical path omits wave-0 chain | line 144 restated as `WP-S → WP2/WP-A → WP4 → …` with WP0 named in the parenthetical — all three of WP4's predecessors present | yes |
| codex E WP11 undersized (Suggest, soft) | not adopted, WP11 stays M | judgment call, defensible; the corroborating pass itself rated it "worth a second look", not confirmed |
| SOTA (a) spike needs a pre-wave-1 home | WP-S, wave 0, cosign v3.1.1 container → zot | yes |
| SOTA (b) WP10a extends cosign interop | WP10a cell names `test_cosign_interop.py` + `fixtures/cosign.py` | yes |
| SOTA (c) WP11 names `setups.py:687` | WP11 cell | yes |
| SOTA (d) Deep Verify covers sigstore | Verification section already names `verify-deep.yml`; no change owed | yes |
| spec D-1 rename scope (deferred) | not recorded anywhere | no — see R-4 |

## Coherence sweep — rewritten sections

Checked and clean:

- **Table↔mermaid**: all 19 WPs present in both; every Depends cell reproduced
  edge-for-edge in the graph, and no graph edge lacks a table entry. Subgraph
  membership matches the Wave column for all 19.
- **Merge plan** (147-150) reproduces the Wave column exactly.
- **Wave widths** 1/5/3/2/2/3/3; the width-justification parenthetical (5/3/3/3)
  covers waves 1/2/5/6 and correctly omits the width-2 hub waves it names in prose.
- **Depends vs C-ID content**: spot-checked the four riskiest — WP4 (`raw_value`
  →WP2, `PredicateType`+`oci/attest.rs`→WP-A), WP5 (all four inputs traced),
  WP8b (`AttestationMatch`/`VerifyContentMode` transitively via WP6; correctly
  does *not* depend on WP3, per spec F-4's `--summary` correction), WP9b
  (`WP8b WP3 WP9-pre`). All satisfied.
- **File-disjointness within every wave**: holds. The two same-wave collisions
  (register rows 6, 7) carry insertion-point distances; row 8's `command.rs`
  collision is declared as expected-and-trivially-resolved rather than denied.
- **S-016..S-019**: each has an owning parse/enforce WP *and* an acceptance WP —
  S-016 WP5+WP10b, S-017 WP6+WP10b, S-018 WP9a+WP10a, S-019 WP3+WP9b+WP10a.
- **Old-layout sweep**: no stale scheduling reference to the pre-split WP8/WP9/WP10
  anywhere in the table, graph, merge plan or register. Three prose family-refs
  remain (`WP10/WP11 handovers` line 190, `in WP10` line 216, `WP4–WP8` line 217);
  all are readable as family references and none carries scheduling data — noted,
  not filed.
- **Verification and Infrastructure sections**: accurate post-restructure; no
  dangling WP reference. **Open questions is the exception — see R-4.**

## Remaining edits

### R-1 (must fix) — the plan pins the ADR at a commit that predates the fixes the plan depends on

`plan:22-23` reads "**The ADR is the contract source**: `adr_sbom_attestations.md`
(commit **e7f30cd2**, through 3 review rounds)", and `plan:25-26` reads "Where this
plan and the ADR disagree, the ADR wins and the plan is the defect."

The ADR was amended after that commit, at `9f057b4b` ("plan-panel corrections"),
which is where three of this round's fixes actually landed. Verified by diffing the
two revisions:

| Claim | ADR @ e7f30cd2 | ADR @ HEAD (9f057b4b) | Plan says |
|---|---|---|---|
| pinned verify slug rows | `23 → 39` (`:1615`) | `23 → 40` (`:1636`) | `23→40` (C-017) |
| new `VerifyErrorKind` variants | `16 variants` (`:1637`) | `17 variants` (`:1658`) | `17 … incl. BuilderMismatch` (C-017) |
| `BuilderMismatch` in the variant table | absent | present (`:1548`) | required by C-017, WP0, WP6 |

Also amended at `9f057b4b`: the rename census note (the whole basis of WP0's file
set) and the `iso8601` location correction (the basis of WP9-pre).

Failure scenario: an executing agent follows `plan:22-26` literally, reads the ADR
at e7f30cd2, finds 16 variants and 23→39 with no `BuilderMismatch`, applies the
stated tie-break, concludes the plan is the defect, and implements WP0 without
`BuilderMismatch` — silently reverting spec F-3, a Block from this same round, and
leaving WP6's builder-pin enforcement with no error kind to raise.

**Fix:** change the commit reference to `9f057b4b` (and, since the ADR gained a
fourth round, "through 3 review rounds" → 4). Same edit region: `plan:8`
`**Last update:** 2026-08-20 (after e7f30cd2: ADR re-validation round closed)` is
three commits stale — the plan was rewritten after `d1429904`; per the Plan Status
Protocol this field names the commit the plan was last touched by.

### R-2 — the shared-file register omits three multi-WP files while claiming to be exhaustive

`plan:151-152` heads the register "**Shared-file register (exhaustive — every
multi-WP file** …)". Three are missing, each verified against the tree:

| File | First writer | Second writer | Waves |
|---|---|---|---|
| `crates/ocx_lib/src/oci/verify.rs` | WP0 — `--trusted-root` doc ref at `:13`, listed in WP0's flag census | WP6 — `mod dsse;` beside `mod tlog;` at `:32` | 1 / 3 |
| `website/src/docs/reference/command-line.md` | WP0 — slug rows `:3770/:3778/:3934` + flag rows (9 `--trusted-root` hits) | WP11 — Documentation Surfaces | 1 / 6 |
| `website/src/docs/reference/environment.md` | WP0 — flag rows (2 hits) | WP11 — Documentation Surfaces | 1 / 6 |

All three are cross-wave, so there is no conflict and no scheduling consequence.
The defect is the false exhaustiveness claim, which is precisely what security F-6
asked to remove: an audit that reads as complete and is not costs the next reader
their confidence in the audit. WP11's cell carries a do-not-re-edit note for the
three slug rows but says nothing about the flag rows or `environment.md`.

**Fix:** three rows appended to the register (or drop the word "exhaustive").

### R-3 — C-021 mandates `#[non_exhaustive]` on `PushOutcome` but names no replacement construction seam

`plan:56` (C-021) and `plan:89` (WP2), which reads: "`crates/ocx_cli/src/api/data/push.rs`
(test-module bare `PushOutcome {` literal at `:112` updated for `#[non_exhaustive]`
— cross-crate struct literals stop compiling)".

Verified against the tree: `crates/ocx_lib/src/publisher.rs:42` defines `PushOutcome`
with no `#[non_exhaustive]` today, and `grep "impl PushOutcome" crates/ocx_lib/src/publisher.rs`
returns **zero matches** — there is no constructor to fall back to. `#[non_exhaustive]`
blocks struct-literal *and* functional-update construction from outside the defining
crate unconditionally, test code included, so "updated" cannot mean "add the field".
The WP must add a public constructor or test-support seam on `ocx_lib::publisher` —
a new public API surface that C-021, the contract, does not name. This is the
unclosed half of codex Finding B, whose stated fix was "include `api/data/push.rs`
in WP2 **and replace those test fixtures with a public constructor/helper**".

Secondary, minor: there are three literals, not one — `push.rs:112`, `:180`, `:203`.
Compile-caught, so self-correcting; the seam is not.

**Fix:** one clause in C-021 naming the seam (e.g. `PushOutcome::new(..)`, or a
`#[cfg(any(test, feature = "__testing"))]` builder), so the public-API shape is a
contract decision rather than a wave-1 improvisation.

### R-4 — "Open questions" still describes the pre-restructure layout, and spec D-1 is unrecorded

`plan:246-250` reads: "None. [NEEDS CLARIFICATION] count: 0 — the ADR settled all 3
candidate ambiguities (spike bounds are in ADR Part V; **the spike itself is WP4/WP7's
Implement phase against the compose stack, not a separate WP**)."

That parenthetical is now false: the restructure created **WP-S as a separate wave-0
WP** for exactly that spike (SOTA finding (a)), and `plan:6` names wave 0 "cosign wire
spike". The plan contradicts itself on whether the spike is a WP, and `/hex-execute`
reads the plan.

Same section is where spec D-1 belongs. D-1 asked whether the `rekor_unavailable` →
`transparency_log_unavailable` rename belongs in this branch at all. The plan honours
ADR D3 ("Pre-release renames land first", `adr:137`) by making it WP0, but never
records the question as considered-and-closed — the strings `D3` and the scope
rationale appear nowhere in the plan.

**Fix:** rewrite the parenthetical to point at WP-S, and add one line recording D-1
as closed by ADR D3 (rename lands as WP0, wave 1, ahead of every consumer).

### R-5 (low) — C-010 is split across WP5/WP6 in the file cells but not in the Scope column

`plan:94-95`. WP5's Scope is `C-010, C-012(tlog half), S-016` and its file cell scopes
`verify/pipeline.rs` to the "mode-agnostic region ONLY". But C-010 (`plan:45`) also
contains "mode-mismatch skips don't consume budget" and "`run_attestations ->
Vec<AttestationMatch>` collect-all" — and WP6's file cell claims exactly those
("attestation arm of `verify_one_referrer`, `run_attestations`, mode-mismatch skip
accounting"), while WP6's Scope column does not mention C-010 at all.

The file cells are unambiguous, so an agent reading the whole row lands correctly.
The Scope column is the traceability index, and a C-ID-driven pass hands WP5 work its
own file cell forbids. The plan already annotates every other split contract —
`C-012(tlog half)`/`(binding half)`, `C-015(extraction)`/`(attest/push)`/`(sbom/verify)`,
`C-016(partial)` ×2 — so C-010 is the lone exception to the plan's own convention.

**Fix:** WP5 → `C-010(mode-agnostic half)`; WP6 → add `C-010(candidate-loop half)`.

## Notes (no edit owed)

- **spec F-9, rule half.** `.claude/rules/meta-ai-config.md:137-141` still enumerates
  only `/swarm-*` Step values, so `awaiting /hex-execute` is unlisted. It passes the
  gate: `TestPlanStatusBlock._MANDATORY_FIELDS` checks the four field *labels*, not
  Step membership. This is an AI-config edit, not owned by this plan (WP11 carries
  `subsystem-cli-commands`, `arch-principles`, `quality-rust-exit_codes`, `rules.md`
  — not `meta-ai-config.md`). Route separately.
- **WP0's flag census lists `app/context.rs`, which carries no `--trusted-root`.**
  Verified: `:983-987` names only `OCX_SIGSTORE_TRUSTED_ROOT` and `[trust.sigstore]`,
  neither of which is renamed. Inherited from the ADR census. Harmless — WP0's own
  instruction is "run the greps fresh", and over-inclusion is the safe direction.
- **codex E (WP11 sized M against 16 surfaces).** Left as M. The corroborating pass
  rated it soft and not apples-to-apples against WP0's L; declining a Suggest-tier
  sizing note is a defensible call, though the decision is not recorded anywhere.
