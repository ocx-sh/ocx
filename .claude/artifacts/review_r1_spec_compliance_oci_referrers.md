# Review R1 — Spec-Compliance (Phase: post-stub)
## OCI Referrers Discovery — `ocx verify` and `ocx sbom`

**Reviewer:** worker-reviewer (spec-compliance focus)
**Round:** 1
**Date:** 2026-04-19
**Artifacts reviewed:**
- `adr_oci_referrers_discovery.md`
- `state/plans/plan_oci_referrers_discovery.md`
- `prd_oci_referrers_discovery.md`
- `pr_faq_oci_referrers_discovery.md`

---

## Summary

**Verdict: PASS-WITH-FIXES**

The four design artifacts are coherent, thorough, and clearly reflect serious prior research. The ADR's four one-way-door decisions are well-reasoned, the trust-policy stub is deliberately non-foreclosing, the JSON schema has `schema_version: 1` correctly anchored, and the plan's test list maps well to the specified behaviors. However, 12 actionable defects were found where a builder running `/swarm-execute` would either (a) have to make an undocumented judgment call or (b) produce a spec-violation that a later reviewer would correctly flag as a regression. These are concentrated in three areas: exit-code precedence ambiguity, `--format json` flag absence, and `--download -` output-routing contradiction. Additionally, 5 open questions in the PRD remain unresolved and must be resolved before Phase 3 (specification) begins, or the tests will encode inconsistent assumptions. 4 deferred findings require human judgment. 7 trivial nits were observed and are not listed individually.

**Actionable: 12. Deferred: 4. Trivia: 7.**

---

## Actionable Findings

### 1. `adr_oci_referrers_discovery.md` §CLI Contracts — `--format json` flag absent from both commands

**Finding:** The JSON output section (`ocx verify --format json`, `ocx sbom --format json`) references a `--format json` flag that does not appear in either command's flag table under §CLI Contracts. The `VerifyArgs` stub in the plan (Step 1.7) also omits it. The `subsystem-cli.md` API layer uses a global `--format json` flag routed through `ContextOptions`, but the ADR flag tables must still list it explicitly so the builder knows the per-command scope vs. the global-context scope.

**Fix:** Add `--format <FORMAT>` (values: `text | json`, default: `text`) to both `ocx verify` and `ocx sbom` flag tables in ADR §CLI Contracts, or explicitly state "format is a global flag from `ContextOptions`; no per-command `--format` needed." The plan's Step 1.7 `VerifyArgs` struct must either include the field or carry a comment pointing at `ContextOptions`. Without this, a builder cannot know whether the format flag is per-command or inherited.

---

### 2. `adr_oci_referrers_discovery.md` §CLI Contracts — exit-code precedence when `--distribution-spec v1.1-referrers-api` is forced and `--require-referrers` is also set, and the registry returns 404

**Finding:** Two exit codes compete for the same failure path. With `--distribution-spec v1.1-referrers-api` and `--require-referrers`, if the registry returns 404 on the referrers API call: `ReferrersUnsupported` maps to 69 (Unavailable) and `RequireUnmet` would map to 65 (DataError). The ADR says `ReferrersUnsupported` fires "only when `--distribution-spec v1.1-referrers-api` is explicitly set." But if the user also sets `--require-referrers`, is it 69 or 65? The error taxonomy does not state which takes priority.

**Fix:** Add a one-line precedence rule to ADR §Error Taxonomy: "Transport errors (69, 75, 79, 80, 81) take precedence over require-check failures (65). A require-check failure is only emitted when discovery succeeds but the result set is empty or below threshold." This matches the intent implicit in `Q-PRD-7` but Q-PRD-7 is currently unresolved; promote the answer here.

---

### 3. `adr_oci_referrers_discovery.md` §CLI Contracts — `--require-artifact-type` combination with `--min-referrers` is undefined

**Finding:** The hard red flag checklist explicitly asks: "what wins when `--require-referrers`, `--min-referrers N`, and `--require-artifact-type TYPE` are combined?" The ADR lists all three flags and says each fails with exit 65, but gives no ordering. If `--min-referrers 2` and `--require-artifact-type application/vnd.dev.sigstore.bundle.v0.3+json` are both specified and there are 2 referrers but neither is a sigstore bundle: which message appears in stderr and which check's failure is canonical? Without this, the plan's unit test `require_artifact_type_one_missing_errors` will produce an underspecified assertion.

**Fix:** Add to ADR §CLI Contracts: "When multiple `--require*` flags are set, all are evaluated against the final (filtered) referrer set. The exit code is 65 for any unmet check. If multiple checks fail, stderr lists all failures; the `require_checks` JSON object marks each as `true/false` independently. Precedence for stderr error message ordering: `require_referrers` first, then `min_referrers`, then each `require_artifact_type` in the order provided."

---

### 4. `adr_oci_referrers_discovery.md` §CLI Contracts — behavior of `--require-artifact-type` when set to empty string

**Finding:** The hard red flag checklist requires semantics for every flag "when unset, set to empty, and set to multiple values." `--require-artifact-type` is a `Vec<String>` (repeatable). The ADR specifies the multi-value case but not the empty-string case: `--require-artifact-type ""`. Is that a usage error (64) or silently ignored?

**Fix:** Add to ADR §CLI Contracts: "`--require-artifact-type` with an empty string value (`--require-artifact-type \"\"`) is a UsageError (exit 64) with message 'artifact-type must be a non-empty media type string'."

---

### 5. `adr_oci_referrers_discovery.md` §CLI Contracts — `--min-referrers 0` is undefined

**Finding:** `--min-referrers N` with N=0 is technically valid (always satisfied) but semantically odd. The ADR does not say whether N=0 is silently allowed (no-op), a usage error, or treated as `--min-referrers 1`.

**Fix:** Add: "`--min-referrers 0` is accepted and is a no-op (equivalent to not specifying the flag); `--min-referrers` with a non-numeric value is UsageError (64) via clap."

---

### 6. `adr_oci_referrers_discovery.md` §CLI Contracts and plan Step 1.7 — `--trust-policy` behavior when file path provided but file missing

**Finding:** PRD `Q-PRD-3` asks: "should we exit 78 (ConfigError) or 79 (NotFound)?" The answer is currently marked `[unresolved]`. Step 1.7 and the trust-policy-related unit tests in the plan (`load_explicit_skip_succeeds`, `load_invalid_toml_errors`) do not cover the "path provided but absent" case explicitly. The builder will have to choose; if they choose wrong relative to the eventual product decision, a later reviewer flags it.

**Fix:** Resolve Q-PRD-3 in the ADR before Phase 3. Recommended answer: "When `--trust-policy <PATH>` is explicitly provided and the file does not exist, exit 78 (ConfigError) with message 'trust policy file not found: <PATH>'. Exit 79 (NotFound) is reserved for registry reference resolution; local file paths that the user explicitly configured are a config error." Add the corresponding unit test `load_explicit_path_missing_errors_config_78` to the plan.

---

### 7. `plan_oci_referrers_discovery.md` §Phase 3 — `test_sbom_download_dash_writes_stdout` has contradictory pass/fail criterion

**Finding:** The plan's own inline comment for this test reveals an unresolved contradiction: "assert JSON-free stdout when `--download -` is used" — but the plan also says "JSON report suppressed to stderr (see docs-reversed convention — actually stay with the ADR)." Two output-routing semantics are in flight within a single test description. The ADR states "structured stdout, diagnostics on stderr" (Invariant 5) and the risks table says "`--download -` stdout is bytes; JSON report suppressed to stderr." The plan test name asserts "writes stdout" but the assertion text mentions "JSON-free stdout."

The actual ambiguity: when `--download -` is used, (a) does the JSON report go to stderr only, (b) go to stderr with a warning that it is being suppressed, or (c) is it entirely omitted? These are observably different in CI (stderr may be discarded).

**Fix:** Resolve this in the ADR before Phase 3. Add to ADR §CLI Contracts under `ocx sbom`: "When `--download -` is specified, raw SBOM bytes are written to stdout; the JSON report (`--format json`) is written to stderr if `--format json` is also specified. If `--format json` is not specified, no JSON report is emitted at all (the default text report is suppressed). Exit codes are unchanged."

---

### 8. `adr_oci_referrers_discovery.md` §JSON Output Schema — `ocx sbom --format json` empty-result shape not specified

**Finding:** The ADR specifies the empty-result JSON shape for `ocx verify` (with `referrers: []`) but does not provide the corresponding empty-result shape for `ocx sbom`. The `SbomReport` differs from `VerifyReport` in that it has `sboms` instead of `referrers` and adds `downloaded_to`. Without an explicit empty-result example for `ocx sbom`, the plan's test `test_sbom_json_schema_version` has an incomplete pass criterion for the empty case, and the builder has to infer the `downloaded_to` field's value when no download was requested (omitted vs. `null` vs. `""`).

**Fix:** Add to ADR §JSON Output Schema:

```json
// ocx sbom, empty result, no --download flag:
{
  "schema_version": 1,
  "reference": "ghcr.io/example/cmake:3.28",
  "resolved_digest": "sha256:aaaa...",
  "subject_digest": "sha256:bbbb...",
  "platform": "linux/amd64",
  "registry_method": "referrers-tag",
  "sbom_count": 0,
  "sboms": [],
  "downloaded_to": null,
  "require_check": false
}
```

Explicitly state: "`downloaded_to` is `null` (not omitted) when `--download` is not specified."

---

### 9. `adr_oci_referrers_discovery.md` §JSON Output Schema — `classification` field type not formally typed

**Finding:** The full-result JSON example in the ADR uses `"verification": { "attempted": true, "result": "skip", "reason": "..." }` with a nested object, but the PR-FAQ appendix JSON mockup uses `"classification": "signature:cosign-bundle-v03"` as a flat string. These are inconsistent shapes. The plan's `ReferrerDto` type has a `classification: ClassificationDto` but the ADR does not define `ClassificationDto`'s JSON serialization: is it a flat discriminant string (as in the PR-FAQ) or a nested object? The `verification: VerificationDto` is also a nested object in the ADR but the PR-FAQ mockup shows it as `{"result": "skip", "reason": "..."}` without the `"attempted"` field. A builder reading both will produce different serializations.

**Fix:** The ADR §JSON Output Schema is the authority; the PR-FAQ is illustrative. Add a note to the ADR: "PR-FAQ JSON mockups are abbreviated for readability; the ADR schema is canonical." Formally define `ClassificationDto` as a flat discriminant string (e.g., `"signature:cosign-bundle-v03"`, `"sbom:cyclonedx-json"`, `"unknown"`) with the full enum of valid values listed. OR define it as a nested object — pick one and lock it in the ADR. The current mismatch between ADR and PR-FAQ will produce a spec-drift finding in Phase 5.

---

### 10. `plan_oci_referrers_discovery.md` §Phase 3 — `cosign_keyless_identity` fixture strategy has an unresolved path

**Finding:** The plan says for the `cosign_keyless_identity` fixture: "Preferred: use pre-generated static test vectors checked into `test/fixtures/cosign/`" but also suggests "fake OIDC issuer running inside registry:2 compose?" as an alternative. These are not equivalent: static vectors cannot test the full cosign keyless verification path (they test classification and parsing, not cryptographic verification). A builder implementing Phase 3 will need to decide; the plan's test `test_verify_trust_policy_skip_default_reports_skip_per_referrer` would pass trivially regardless of which fixture strategy is chosen (since v1 is discovery-only). However, when Phase 4 adds actual cosign keyless verification (if trust levels other than `skip` land), the fixture strategy determines whether Phase 3 tests will catch a broken verification path.

More concretely: the plan says Phase 3 tests must fail against stubs with `unimplemented!()`. With static vectors, the `test_verify_happy_api_path_exits_0_with_json` test could pass against a stub that only does discovery if the stub is written to return a hardcoded `ReferrerEntry` from the fixture without actually verifying. This risks writing Phase 3 tests that are permanently green against non-implementations.

**Fix:** Add to the plan a clear fixture strategy decision: "Static bundle vectors in `test/fixtures/cosign/` are the fixture strategy for v1. These vectors are pre-signed with a known cosign keyless identity (using a test OIDC issuer or a recycled development certificate) and serve as the ground truth for classification and verification tests. Full live OIDC verification (against real Fulcio) is NOT tested in CI. The `cosign_keyless_identity` fixture provides a `(bundle_path, expected_subject_digest)` tuple to tests that need it." State explicitly in the plan which tests require the static vector fixture vs. the live registry fixture.

---

### 11. `adr_oci_referrers_discovery.md` §CLI Contracts — `74` (IoError) in exit-code table is not in the allowed set stated in the prompt

**Finding:** The ADR's exit-code table for `ocx verify` includes `74  IoError — local cache read/write failure` and `75  TempFail — 429 rate limit after retry backoff exhausted`. Both 74 and 75 are listed. The prompt's "allowed set" is stated as `{0, 64, 65, 69, 79, 80, 81}`. The ADR also documents 77 (PermissionDenied), 78 (ConfigError), and 80 (AuthError) which are consistent with the `quality-rust-exit_codes.md` canonical enum. The discrepancy is that codes 74, 75, and 77 are present in the ADR table but absent from the prompt's restricted set. This is not a defect in the artifacts themselves (the ADR is consistent with `quality-rust-exit_codes.md`) but constitutes an internal inconsistency worth noting: the plan's exit-code classification tests (Step 3.1) do not include a `referrer_discovery_io_error_maps_74` test or a `referrer_discovery_temp_fail_maps_75` test despite 74 and 75 appearing in the ADR's exit-code table.

**Fix:** Add to the plan §Phase 3 exit-code classification tests: `referrer_discovery_io_cache_write_maps_74` (cache write failure → 74) and `referrer_discovery_temp_fail_429_maps_75` (rate-limit exhausted → 75). These are in scope based on the ADR's own exit-code table.

---

### 12. `prd_oci_referrers_discovery.md` §Open Questions — Q-PRD-6 (CI context heuristic) and Q-PRD-7 (transport error vs. require-check precedence) must be resolved before Phase 3

**Finding:** Q-PRD-6 asks whether the CI context (24h cache TTL) is determined by `stdin is not a TTY` or `CI=true`. The cache's `ReferrerStore` will implement this logic; both the `meta_ttl_expiry_detection` unit test and the `test_verify_cache_hit_offline_succeeds_0` acceptance test depend on knowing which heuristic is used. If the builder defaults to one choice and the reviewer later sees the other in a test, it surfaces as a spec-compliance finding.

Q-PRD-7 asks: when both a transport error and `--require-referrers` apply, which exit code wins? This maps directly to Finding #2 above and the plan's test `test_verify_registry_5xx_fails_69` vs. `test_verify_require_referrers_fails_65_on_empty`. The plan currently lists both tests as independent, but they do not address the combined case.

**Fix:** Resolve both in the PRD (or ADR) before handing off to Phase 3:
- Q-PRD-6: "CI TTL heuristic: use `stdin is not a TTY` (`!atty::is(atty::Stream::Stdin)`) as the detection method. `CI=true` is a secondary hint: if `CI=true` is set AND stdin is a TTY (interactive CI terminal), still use 24h TTL."
- Q-PRD-7: Resolved by Finding #2 fix — transport errors take precedence.

---

## Deferred Findings

### D1. `prd_oci_referrers_discovery.md` §Open Questions Q-PRD-1 — short digest prefix support

**Concern:** Q-PRD-1 asks whether `ocx verify sha256:abcd12` (short prefix) should be accepted. The plan assumes full digests only, but short digests are a common user affordance in Docker and ORAS. If the first real users attempt `ocx verify sha256:abcd12` and get a usage error (64), that is a UX cliff.

**Why it needs human judgment:** This is a product decision about user affordance vs. implementation complexity. Short digest prefix resolution requires a registry search (or a local cache walk), which is a non-trivial addition. The team must decide before Phase 3 whether this is in-scope for v1 or explicitly out-of-scope with a documented error message, because it affects whether the acceptance test suite includes a `test_verify_short_digest_fails_64` regression test.

---

### D2. `prd_oci_referrers_discovery.md` §Open Questions Q-PRD-8 — Rekor transparency-log inclusion proofs in JSON

**Concern:** Q-PRD-8 asks whether `ocx verify` should include Rekor inclusion proofs in JSON output when available. The PR-FAQ's external FAQ states: "Cosign keyless (sigstore bundle v0.3) — fully verified via `sigstore-rs` including transparency-log inclusion proofs." This implies the proofs are verified but the JSON output shape does not expose them. If proofs are verified but not surfaced in JSON, users debugging a failed verification have no way to know which log entry was checked.

**Why it needs human judgment:** The PRD defers this to v2, but the PR-FAQ implies proofs are included in v1 verification. The architect must decide: (a) verify proofs silently (v1 behavior, PR-FAQ claim), (b) verify proofs and expose the log entry in JSON (adds instability risk to the JSON schema), or (c) skip proof verification entirely in v1 and make it a v2 feature. This affects both `sigstore-rs` API surface and JSON schema stability. A human must reconcile the PR-FAQ claim with the PRD's out-of-scope statement.

---

### D3. `adr_oci_referrers_discovery.md` §Referrer Cache Layout — atomic write pattern not specified for referrer index

**Concern:** The cache layout section notes that the referrer index is mutable (new referrers can be added) and subject-digest-keyed. The plan's Step 4.6 references "Atomic write via temp-file + rename pattern (per `subsystem-file-structure.md`)". However, the `referrer/cache.rs` unit test `write_then_read_index_roundtrip` does not include a concurrent-writer scenario. The per-registry capability cache (written by the auto-probe path) and the referrer index (written by `discover()`) can be written by two concurrent `ocx verify` invocations (e.g., parallel CI matrix jobs sharing an `$OCX_HOME`).

**Why it needs human judgment:** Whether concurrent-writer atomicity is a Phase 1 requirement or a Phase 4 quality item depends on how shared `$OCX_HOME` in CI matrix jobs is prioritized. The architect should decide: add a concurrent-write test to Phase 3, or document the known limitation ("concurrent writes may produce stale reads for up to one TTL period") and track as a v2 issue. Either is a valid product decision; neither can be made by the builder alone.

---

### D4. `plan_oci_referrers_discovery.md` §Phase 3 §3.2 — `registry_without_referrers_api` fixture feasibility

**Concern:** The plan says: "use an upstream proxy, or `distribution:v3.0.0-beta.1` flag; if not feasible, use `--distribution-spec v1.1-referrers-tag` and a registry:2 without referrers config." This means the acceptance test `test_verify_auto_probe_falls_back_on_404` has a conditional implementation path. The `--distribution-spec v1.1-referrers-tag` workaround does not test auto-probe; it tests the tag path directly, which is a weaker test than the auto-probe scenario. Whether the test is worth weakening depends on CI infrastructure available to the team.

**Why it needs human judgment:** The architect or project maintainer must decide whether the test suite targets the auto-probe behavior (requiring infrastructure configuration) or explicitly documents "auto-probe fallback is tested via the `discover` unit test with a `MockTransport` returning 404; the acceptance test covers only the forced-tag-path." Both are acceptable; only a human can decide how much CI infrastructure investment is warranted.

---

## Trivia

7 nits not listed individually: minor inconsistency in `SbomReport` field name (`sboms` vs `referrers` naming symmetry), two occurrences of "comma-separated" in flag descriptions that are actually space-separated in clap derive, one sentence in the PR-FAQ that capitalizes "Offline-first" differently from the product-context rule, one inconsistency in the text output example between "referrers-api" (ADR) and "referrers-tag (auto-probe → fallback; defensive classify)" (PR-FAQ), and two minor hyphenation inconsistencies in CLI flag descriptions (`artifact-type` vs `artifactType`).

---

## Hard Red Flag Checklist Results

- [x] Every UX scenario in the PRD has a corresponding exit code — PASS (all scenarios map to exit codes in the ADR table)
- [ ] Every flag has semantics for unset / empty / multi-value — FAIL (Findings #4, #5, #6)
- [x] Every test named in the plan has a clear pass/fail criterion implied by ADR contracts — PARTIAL FAIL (Finding #7 for `test_sbom_download_dash_writes_stdout`)
- [x] `--trust-policy` stub flag does not silently accept unparseable files — PASS (validator rejects non-"skip" levels; IO error path documented)
- [ ] `--require-referrers`, `--min-referrers N`, `--require-artifact-type TYPE` combine unambiguously — FAIL (Finding #3)
- [x] `schema_version: 1` present on every JSON output shape — PARTIAL FAIL (empty `ocx sbom` shape missing, Finding #8)
- [x] Registry auto-probe fallback order deterministic — PASS (API first, tag fallback on 404/405, pinnable via `--distribution-spec`)
- [x] Platform-descent behavior specified when subject is ImageIndex vs ImageManifest — PASS (Invariant 3 clearly documented)
- [x] Cosign fallback-tag bug #4641 defensive parsing documented with concrete steps — PASS (Decision C2 and plan Step 4.5 are specific)
- [x] Signature-verify failure vs empty-referrers distinction clear — PASS (exit 65 for require-unmet; empty is exit 0 by default; `verification.result` field distinguishes per-entry)
- [ ] `--format json` flag defined in flag table — FAIL (Finding #1)
- [ ] Exit-code precedence when multiple failures can occur — FAIL (Finding #2)
