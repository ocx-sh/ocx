# Review R3 — Spec-Compliance Validation (Codex Cross-Model Pass)
## OCI Referrers Discovery — `ocx verify` and `ocx sbom`

**Reviewer:** worker-reviewer (spec-compliance, validation-only)
**Round:** 3 (Codex finding closure validation — NOT a fresh adversarial review)
**Date:** 2026-04-19
**Sources:**
- Codex report: `.claude/artifacts/codex_review_plan_oci_referrers.md` (9 Actionable, 1 Deferred)
- Edited plan: `.claude/state/plans/plan_oci_referrers_discovery.md`
- Edited ADR: `.claude/artifacts/adr_oci_referrers_discovery.md`
- Own R1 report: `.claude/artifacts/review_r1_spec_compliance_oci_referrers.md` (context only — not re-flagged)

---

## Summary

VALIDATION-PASS. All 9 Codex actionable findings are **Closed** — each has a concrete, builder-safe edit in the plan and/or ADR that directly addresses the stated problem. DF-CODEX-1 is present in the plan's Deferred Findings section with the correct classification. The "Round 3 Decisions (Codex Cross-Model Pass)" section exists in the ADR. No new drift was introduced by the edits. The plan and ADR are internally consistent with each other after the Round 3 edits.

Disposition counts: **9 Closed / 0 Partially closed / 0 Not closed / 0 New drift**.

---

## Per-Finding Verdict

| Codex Finding # | Disposition | Evidence (plan/ADR section + location) | Concern |
|---|---|---|---|
| 1 (Repo-shape drift) | **Closed** | Plan: Architecture Changes section (lines 85–140) adds inline "Repo-shape note" block explicitly naming flat aggregators `command.rs`, `api/data.rs`, OCI client paths `oci/client/{error,transport,native_transport}.rs`, `Args::execute`, `App::run`, `main.rs` **Untouched** note; Files to Modify table (lines 850–892) repeats the same paths and marks `main.rs` explicitly **Untouched**; Step 1.8 (lines 350–364) specifies the flat aggregator convention explicitly. ADR §Round 3 Decisions row Codex #1 records the adjudication. | None. |
| 2 (`ReferrerDiscovery::new` Index injection) | **Closed** | Plan: Context Wiring section (lines 143–182) specifies `ReferrerDiscovery::new(client, index, store, policy, mode)` with an explicit split-of-concerns table; algorithm Step 4.8 order-of-operations list (line 773) mandates tag resolution via `Index::select`/`Index::fetch_manifest` and forbids `Client::resolve_reference`; reviewer checklist in Phase 2 adds `no ClientBuilder`, `no RemoteIndex::new`, `no LocalIndex::new` in the referrer tree. ADR §API Contract (lines 778–809) is updated with `index: oci::Index` in the struct and a split-of-concerns comment. ADR §Round 3 Decisions row Codex #2 records the adjudication. | None. |
| 3 (Cache/offline algorithm undefined) | **Closed** | Plan: Step 4.8 (lines 762–788) provides an explicit 12-step algorithm with TTL values (1h interactive / 24h CI / 7-day capability probe), `--no-cache` semantics (bypasses reads, preserves writes), offline cache-hit / cache-miss / cache-expired behavior with exact exit codes (0 / 81 / 81), and a `--offline` + `--no-cache` mutual-exclusion rule (exit 64). Phase 3 unit test list (lines 527–548) enumerates 12 cache/offline tests covering all these cases. ADR §Cache Layout already had the TTL numbers; Round 3 promoted them to tested contracts. ADR §Round 3 Decisions row Codex #3 records the adjudication. | None. |
| 4 (ClientError taxonomy) | **Closed** | Plan: Step 1.1 (lines 206–236) adds `Unauthorized(String)`, `Forbidden(String)`, `RateLimited { retry_after: Option<u64> }`, `ServiceUnavailable { status: u16, reason: String }` with `ClassifyExitCode` mappings (80/77/75/69). Step 4.2 (lines 709–714) maps each HTTP status to the corresponding variant. Unit tests `list_referrers_maps_401_to_unauthorized`, `maps_403_to_forbidden`, `maps_429_to_rate_limited_with_retry_after`, `maps_5xx_to_service_unavailable` are listed in Phase 3.1 (lines 564–570). ADR §Error Taxonomy (lines 596–629) is updated with the full expanded `ClientError`. ADR §Round 3 Decisions row Codex #4 records the adjudication. | None. |
| 5 (Trust-policy error split) | **Closed** | Plan: Step 1.5 (lines 290–313) defines `TrustPolicyParse { path, source: toml::de::Error }` → exit 78, `TrustPolicyNotFound { path }` → exit 79, `TrustPolicyIo { path, source: io::Error }` → exit 74, `UnsupportedTrustPolicyVersion { version }` → exit 78 as separate variants with separate `ClassifyExitCode` mappings. Phase 3.1 trust-policy tests (lines 517–526) include `load_explicit_path_missing_errors_not_found_79`, `load_invalid_toml_errors`, `load_version_2_errors_with_forward_compat_hint`, each locking distinct variants. ADR §Error Taxonomy (lines 631–688) lists the expanded `ReferrerDiscoveryError` enum with all four trust-policy variants. ADR §Round 3 Decisions row Codex #5 records the adjudication. | None. |
| 6 (Compile-fail → trybuild) | **Closed** | Plan: Phase 3.1 (lines 572–589) replaces the inline `#[cfg(test)]` specification with an explicit `trybuild` harness: `crates/ocx_lib/tests/ui.rs` entry point, `tests/ui/list_referrers_rejects_identifier.rs` compile-fail body, `tests/ui/list_referrers_rejects_identifier.stderr` reference file; `trybuild = "1"` added as dev-dependency; regeneration command documented; downgrade fallback to architecture-review-only if rustc message churn proves prohibitive. Files to Modify table (lines 878–881) lists the three new files. ADR §Round 3 Decisions row Codex #6 records the adjudication. | None. |
| 7 (Fixture designs undefined) | **Closed** | Plan: fixture prep section (lines 636–650) adds concrete designs for every scenario: `malformed_referrer_index` uses raw `requests.put` with two seeded schema-violation scenarios (missing `manifests`, `digest: "banana"`); `registry_401_fixture` and `registry_5xx_fixture` use NGINX reverse proxy under docker-compose `profiles:` returning the required status only on `/v2/*/referrers/*`; `spoofed_oci_filters_applied_fixture` uses NGINX response-header modifier with an explicit unit-test fallback if NGINX approach proves infeasible; `registry_without_referrers_api` specifies two acceptable implementations (pinned `distribution:v3.0.0-beta.1` or NGINX proxy). Files to Modify (line 888) lists `test/fixtures/nginx/`. ADR §Round 3 Decisions row Codex #7 records the adjudication. | None. |
| 8 (`cosign sign` live-Fulcio dependency) | **Closed** | Plan: `signed_package` fixture (lines 626–631) is rewritten to use `oras attach` with static cosign sigstore-bundle-v0.3 manifests from `test/fixtures/cosign/<name>/`; no `cosign sign` invocation at CI time; `sigstore-rs` test-mode flag (`verify_with_no_certificate_check`) gated to `#[cfg(test)]` to avoid cert-validity-window expiry; regeneration is a one-time human-run step documented in `test/fixtures/cosign/scripts/regenerate.sh` (not CI-invoked); expired-cert error-path test vector included. Files to Modify (lines 886–887) lists `test/fixtures/cosign/` and `test/fixtures/cosign/scripts/regenerate.sh`. ADR §Round 3 Decisions row Codex #8 records the adjudication. | None. |
| 9 (JSON error path / schema_version on errors) | **Closed** | Plan: Step 1.9 (lines 366–455) defines `VerifyErrorReport { schema_version: SchemaVersionV1, reference, error: VerifyErrorEnvelope { kind, message, exit_code } }` and a mirror `SbomErrorReport`; `command/verify.rs` `execute` catches `ReferrerDiscoveryError`, routes through `context.api().report(&VerifyErrorReport {...})` when `is_json()` is true, returns `ExitCode` directly; `--format plain` bubbles to `main` unchanged; `--download -` suppresses both success and error envelopes. `test_verify_schema_version_present_in_every_branch` acceptance test (line 676) validates all JSON branches. ADR §Round 3 Decisions row Codex #9 records the adjudication. | None. |

---

## New Drift

None. No newly-introduced contradictions detected between the plan and ADR after the Round 3 edits, and no feature-scope creep was introduced under the fixes. The one judgment call on Finding 6 (trybuild downgrade fallback) is explicitly documented in both the plan and ADR as a recorded escape hatch, not an undeclared weakening. The DF-16 reference inside Finding 8's narrative (live-Fulcio deferred) was already a prior-round deferred finding, not a new addition.

---

## Deferred Finding + ADR Round 3 Decisions Check

**DF-CODEX-1 present:** YES. Plan, section "Deferred Findings for Handoff", entry "DF-CODEX-1 Trust-policy v1 scope contradiction — sigstore verify code present but unreachable" (plan lines 1118–1125). Correctly classified as "parent-ADR amendment / product scope" with recommended resolver "parent-ADR author / product owner before v1 CLI freeze". Context, both resolution options, and the plan's current middle-path rationale are all stated.

**ADR "Round 3 Decisions (Codex Cross-Model Pass)" section present:** YES. ADR, lines 891–924 (section heading "Round 3 Decisions (Codex Cross-Model Pass)"). Contains a 9-row adjudication table covering all Codex findings, an audit sub-section ("Items the Codex review got right but the plan's prior rationale was non-trivial"), a judgment-call sub-section, and an opportunistic-tightening sub-section. The ADR Changelog (line 932) records the Round 3 edit with a comprehensive summary of all changes applied.
