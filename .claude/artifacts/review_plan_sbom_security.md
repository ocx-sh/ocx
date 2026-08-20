# Plan review — security focus: does the plan preserve the ADR's posture through decomposition?

- **Target:** `.claude/state/plans/plan_sbom_attestations.md`
- **Design record (not re-litigated):** `.claude/artifacts/adr_sbom_attestations.md` @ e7f30cd2
- **Question:** not "is the ADR right" (3 rounds closed that) but "does the WP split lose anything the ADR pinned".
- **Verdict:** Needs Work — 4 Block, 3 Actionable, 0 Deferred.

## 1. Checklist custody — Part III rows 1..21 mapped to owning WP

Rows 9, 11, 14 are `Not Doing` / shipped-unchanged and need no owner. Rows 19 (docs)
and 20 (meta) are covered by WP11 and by the per-row negative fixtures respectively.

| Row | Enforcement site (ADR:794-816) | WP owning it | Custody |
|---|---|---|---|
| 1 | `attest/dsse.rs::pae` + `verify/dsse.rs` | WP4 + WP6 | OK — WP6 depends on WP4 |
| 2 | `verify/dsse.rs` `VerifiedAttestation` | WP6 (C-011) | OK |
| 3 | `verify/dsse.rs` payloadType-before-parse | WP6 (C-011) | OK |
| 4,5,6 | `attest/statement.rs::binds_subject` | WP4 (C-004) | OK |
| 7 | `verify/dsse.rs` + candidate filter in `verify/pipeline.rs` | WP6 + WP5 | OK |
| 8 | `verify/dsse.rs` one-signature | WP6 (C-011) | OK |
| 10 | `verify/dsse.rs` keyid read-and-discard | WP6 (C-011) | OK |
| 12 | `verify/dsse.rs::verify_tlog_binding` + `sign/bundle.rs` envelopeHash | WP6 + WP7 (same wave 3) | OK |
| **13** | **`verify/tlog.rs::verify_integrated_time_within_certificate`** | **file -> WP5, contract C-012 -> WP6; neither holds both** | **GAP -> F-1** |
| 15 | `attest.rs` `MAX_ATTESTATION_ENVELOPE_BYTES` + fetch path | WP4 (constants) + WP5 (selection) | **no depends edge WP4->WP5 -> F-2** |
| 16 | `verify/dsse.rs` decoded-payload cap | WP6 (C-011) | OK |
| 17 | `attest.rs` candidate/budget caps + discovery | WP4 + WP5 | **same missing edge -> F-2** |
| 18 | `attest/statement.rs` `_type` allowlist | WP4 (C-004) | OK |
| 21 | `attest/pipeline.rs` provenance floor | WP8 (C-009) | OK |

One row (13) has no unambiguous owner. Two rows (15, 17) are split correctly but
across a wave-2 pair with a missing dependency edge.

## 2. Negative-path custody — ADR Testing Strategy tables vs plan S-IDs / WPs

Covered: cross-subject, zero-subject, md5-vs-sha256, no-sha256, annotation-vs-signed
predicateType, wrong payloadType, two signatures, hostile keyid (positive), `_type: v9`,
CVE-2026-39395 malformed payload, tlog binding mismatch, integratedTime window,
`intoto:0.0.1`, 33 MiB, >16 MiB payload, 33 referrers, budget exhausted, `--predicate`
not-JSON / cap boundary / symlink, `--type slsaprovenance`, `attest --offline`, identity
mismatch, Rekor unreachable, `registry:2`, no-identity, MultipleAttestations, TTY refusal.

Falling between WPs — no S-ID and no WP row claims them (detail in F-4, F-5):

1. The four **red-before-green** mutations (ADR:1760-1781).
2. The **mode-selected cap pair** (ADR:1757) — 1 MiB bundle rejected in Signature mode,
   accepted in Attestation mode.
3. **Zero-match -> `AttestationNotFound` (79)** via `aggregate_failure` (ADR:1737); the
   ADR states outright "without this fixture the empty-set path is untested".
4. **`push --sbom --offline`** (ADR:1742) — S-002 covers `attest --offline` only.
5. **Non-CycloneDX predicate under `--summary`** (ADR:1749) — `--summary` appears in no S-ID.

## 3. Wave-boundary hazards

| Wave | Hazard | Contained? |
|---|---|---|
| 1 | WP1 lands the `[trust.policy.keyless]` config break 5 waves before its docs (WP11) | Yes — branch-internal, nothing releases off a feature branch |
| 2 | WP5 stubs `verify/dsse.rs` (attestation-only). No CLI surface reaches Attestation mode until WP9 (wave 5) | Yes, **if** the stub refuses — unpinned today (F-3) |
| 2 | WP5 stubs the row-13 helper in `verify/tlog.rs`, which sits on the **both-modes shared path** reached by `ocx package verify` and `ocx install` auto-verify | **No** — F-1 / F-3 |
| 4 | WP8 puts `content: VerifyContentMode` on `VerifyOptions`, so a library caller can request Attestation mode one full wave before the CLI flag exists — this is the window where a passing stub is reachable from WP8's own tests | Only by F-3 |
| 5 | WP9 ships the complete user-reachable surface with zero end-to-end negative coverage until WP10 (wave 6) | Declared by "Shippable after wave: 6" — accepted, recorded |

## Actionable findings

### F-1 (Block) — Part III row 13 has no owning WP, and its wave-2 stub sits on the shipped verify path

`plan:87` gives `oci/verify/tlog.rs` to WP5, whose only contract is C-010 (`plan:45`) —
which contains no tlog work at all. `plan:88` gives C-012 to WP6, and C-012 (`plan:47`)
explicitly reads "`verify/tlog.rs` row-13 validity re-assertion, both modes" — but WP6's
expected-files cell names only `verify/dsse.rs` and `verify/pipeline.rs`. WP5 has the
file without the contract; WP6 has the contract without the file. Row 13 is the one row
the ADR flags as having *no* pre-existing coverage (ADR:808).

Why this one is worse than an ordinary custody gap: ADR:1257-1274 places
`verify_integrated_time_within_certificate` on the path **both content modes share**, and
that path is live today —
`crates/ocx_lib/src/oci/verify/pipeline.rs:402` (`parse_certificate`) and `:546-567`
(`verify_rekor_set`) are traversed by `ocx package verify` and by `ocx install`
auto-verify (`crates/ocx_lib/src/package_manager/tasks/auto_verify.rs`). A wave-2 stub
returning `Ok(())` is a security control that exists, is called, and does nothing.
Verified that nothing can red it: `grep -rn "integrated_time|integratedTime|validity_window|NotAfter" test/tests/*.py`
returns zero hits, so every wave-merge `task verify --force` passes on the no-op. The one
test that can red it — `test_verify_attestation_integrated_time_outside_window` (ADR:808) —
lands in wave 6 with WP10.

**Fix.** Add `oci/verify/tlog.rs` (implement) to WP6's expected files; mark WP5's entry
`(stub signature only, call site NOT wired)`; add the pair to the merge-plan's named
stub-handoff exception at `plan:131-133` beside `verify/dsse.rs`.

### F-2 (Block) — WP5 cannot compile against its declared dependencies; the shortest repair forks the caps and the alias table

`plan:87` declares `WP5 ... Depends: WP0`. Three things WP5 needs come from WPs it does
not depend on, all in the same wave (so absent from its worktree):

- `PredicateType` — ADR:1180-1183 defines `VerifyContentMode::Attestation { predicate_type: Option<PredicateType> }`,
  and ADR:1235-1239's `verify_envelope` signature (which WP5 stubs) takes
  `Option<&PredicateType>`. `PredicateType` is C-005 -> `oci/attest/predicate.rs` -> **WP4**.
- `MAX_ATTESTATION_ENVELOPE_BYTES` / `MAX_ATTESTATION_CANDIDATES` / `MAX_TOTAL_ATTESTATION_BYTES`
  — C-010 requires "caps selected by requested mode"; the constants are C-001 ->
  `oci/attest.rs` -> **WP4**.
- `serde_json`'s `raw_value` feature — C-010's `AttestationMatch` embeds `VerifiedAttestation`,
  whose `predicate` is `Box<serde_json::value::RawValue>` (ADR:1197-1205). Confirmed not
  enabled today: `crates/ocx_lib/Cargo.toml:50` reads `features = ["preserve_order"]`.
  `raw_value` is added by C-002 -> **WP2**.

Security consequence, not just build order: a blocked WP5 worker's shortest path to green
is a local copy. Two copies of a `MAX_*` breaks PKG-11's one-constant-one-error-variant
contract and lets the cap-pair fixture pass against a constant the fetch path does not
use. Two copies of the alias table re-opens the `slsaprovenance` -> v0.2 trap the ADR pins
explicitly at ADR:1786-1788 ("so a later 'fix' to v1 breaks a test rather than interop").

**Fix.** Add `WP2 WP4` to WP5's Depends and move WP5 to wave 3; or split C-001's constants
plus C-005's `PredicateType` into a wave-1 leaf WP that WP4 and WP5 both depend on. Either
way correct the mermaid (`plan:115`) and the "wave 2 is width-2" claim (`plan:135-138`).

### F-3 (Block) — stub semantics on verification paths are unpinned

The plan mandates stubs (`plan:87`, `plan:139-142`) but never states what one returns.
Both defaults are wrong here, in opposite directions:

- `unimplemented!()` on the row-13 helper panics every `ocx package verify` and
  `ocx install` from wave 2 — exit 101, not a classified refusal (EXIT-04 / ERR-15).
- `Ok(())` / `Ok(Default::default())` is F-1's silent no-op control, and for
  `verify_envelope` it is a verify path that accepts DSSE with no structural checks.

**Fix.** One line in the WP execution-style block, two-pronged because the two stubs sit
on different paths:

- `verify_envelope` (attestation-only, unreachable from the CLI until wave 5): the stub
  returns a refusing `Err`, never `Ok`, never `unimplemented!()`; WP5 ships one test
  asserting Attestation mode refuses, and WP6 deletes that test in the commit that
  implements the check.
- `verify_integrated_time_within_certificate` (both-modes, shipped path): WP5 writes the
  signature only and **does not wire the call site**. The call lands with the
  implementation in WP6.

### F-4 (Block) — the four red-before-green mutations have no owner

ADR:1760-1781 names four demonstrated-red steps as mandatory, each because it "is the
shape that passes for the wrong reason": PAE input, the mode gate, subject binding
(the CVE-2026-31830 shape, called "the single highest-value assertion in the suite"),
and predicate byte fidelity. No WP row, no S-ID, and the plan's Verification block
(`plan:189-194`) mentions none of them. Under `quality-core.md` "Unchecked Green",
shipping a check whose red state was never reachable is Block-tier.

They also cannot sit with the code they mutate: the targets are in WP4/WP5/WP6/WP7
(waves 2-3), but the tests that must red are WP10's acceptance tests (wave 6).

**Fix.** Add to WP10's scope: "ADR Testing Strategy -> Red-before-green, all four
mutations". Each run must prove the mutated text was present before the run **and**
prove the revert landed after it — a harness that reports success unconditionally makes
a no-op edit indistinguishable from a real one.

### F-5 (Actionable) — WP10's fixture pointer names a section that does not contain the fixtures

`plan:92` scopes WP10 to "Part III fixture tables". Part III (ADR:794-816) is the
21-row checklist with a "Proven by" column; the fixture tables live under **Testing
Strategy** (ADR:1682-1793): Interop, Golden shapes, Negative paths, Cap and arity,
Red-before-green, Unit-level. A worker following the pointer reads the wrong section.

Four fixtures inside those tables are claimed by no S-ID and no WP row:

| ADR line | Fixture | Why it cannot be dropped |
|---|---|---|
| 1757 | 1 MiB bundle: rejected in Signature mode, accepted in Attestation mode | The pair is what makes "caps are selected by the requested mode" checked rather than stated (D-d) |
| 1737 | Zero verified matches -> `AttestationNotFound` (79) via `aggregate_failure` | ADR's own words: "without this fixture the empty-set path is untested". S-006 covers no-identity, S-007 covers >1 match; nothing covers 0 |
| 1742 | `push --sbom --offline` -> `OfflineAttestRefused` (77) | S-002 covers `attest --offline`; `push --sbom` reaches the attest pipeline by a different route (D-f), so 77 needs its own assertion on that route |
| 1749 | Non-CycloneDX predicate under `--summary` -> explicit refusal | `--summary` appears in no S-ID, and C-015's `package sbom` scope names only `--output` TTY refusal and `MultipleAttestations` |

**Fix.** Repoint WP10 at "ADR Testing Strategy tables"; add the four as S-016..S-019.

### F-6 (Actionable, low) — cross-wave file sharing is under-declared

`plan:128-133` names exactly one exception ("the single deliberate exception: WP5 stubs
`verify/dsse.rs` which WP6 implements"). Four more files are claimed by two WPs:
`oci/verify/pipeline.rs` (WP5, WP6), `oci/verify/tlog.rs` (WP5, WP6 after F-1),
`crates/ocx_cli/src/command/package_verify.rs` (WP0 rename sites, WP9 new flags), and
`test/tests/test_verify.py` + `test_sign.py` (WP0 assertion renames, WP10 new tests). All
are cross-wave, so the disjointness claim holds — but the parenthetical reads as an
exhaustive audit and is not one, which costs a reader confidence in the audit itself.

**Fix.** List all five, or reword to "conflicts are impossible *within* a wave; the
cross-wave shared files are: ...".

### F-7 (Actionable, low) — WP8's Depends cell contradicts the mermaid

`plan:90` gives WP8 `Depends: WP6 WP7`; the graph adds `WP1 --> WP8` (`plan:120`) and
`WP3 --> WP8` (`plan:121`). Both graph edges are real — C-014's `sbom_one --summary`
calls C-013's `summarize_cyclonedx` (WP3), and S-013's `builder` pin needs C-020's trust
reshape (WP1). Table and graph must agree, or merge order is read off whichever the
executor happens to open.

## Not a finding — recorded so it is not re-raised

`OCX_IDENTITY_TOKEN` stays in the token-resolution precedence chain that
`package_sign_common.rs` carries to `package attest` (ADR:1407-1413), which CLI-11
discourages for secrets. It is shipped behaviour extended to a second command by a
**verbatim** move — the correct call, since forking a security-critical resolver is the
worse outcome — and the plan introduces no new argv or env token path. WP10's fixtures use
the file-only `identity_token` helper (`plan:145-147`). The `--predicate` `O_NOFOLLOW`
hardening (C-015) lands in WP9, the same WP that introduces `--predicate`, so there is no
wave in which the flag exists unhardened.
