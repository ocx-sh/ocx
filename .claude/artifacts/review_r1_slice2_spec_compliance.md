# Review R1 — Slice 2 Spec Compliance

**Verdict:** PASS-WITH-ACTIONABLE
**Date:** 2026-04-19
**Focus:** spec-compliance
**Phase:** post-stub

## Summary

The Slice 2 plan (`plan_slice2_external_discovery.md`) is substantially stronger than the original plan it supersedes (`codex_review_plan_oci_referrers.md` reviewed). All 9 Codex findings from the original review are explicitly re-affirmed in the plan (correct repo paths, Context injection via `sbom_context()`, cache algorithm with 6-branch unit tests, expanded `ClientError`, typed JSON envelope, committed fixtures, no `cosign sign` in CI, no `trybuild` unit tests, no new `ExitCode` variants). The blocked-by-Slice-1 table is explicit, every import is cross-checked to a Slice 1 plan line number, and the `cache.rs` ownership discrepancy (meta-plan vs. actual Slice 1 delivery) is cleanly resolved. Three actionable gaps remain: a stale Goal metric in the PRD that references the removed `--trust-policy` surface, two stale Risks rows in the PRD that describe the superseded `level = "skip"` and trust-policy TOML which cannot fire in Slice 2, and the `spdx-rs` API choice deferred to implementation with no contract specified. All other check items pass.

---

## Actionable Findings

### Finding 1 — PRD Goals table: stale `--trust-policy wired in v1` metric
**Source:** `/home/mherwig/dev/ocx-evelynn/.claude/artifacts/prd_oci_referrers_discovery.md` line 72
**Text:** `"Set up the ecosystem for auto-verify in v2 without surface breakage" | "--trust-policy wired in v1; schema v1 accepted"`

The `--trust-policy` flag was explicitly removed from both slices. The metric column still describes it as shipped in v1, which is false per the amendment summary on line 24 of the same file. A builder reading the Goals table would be confused about whether `--trust-policy` is required.

**Remediation:** Replace the metric cell text with `"Flag-based enforcement in v1 (no trust-policy TOML); schema v1 frozen at root; exit codes 78/79 reserved for v3+"`.

---

### Finding 2 — PRD Risks table: two rows describe superseded behavior
**Source:** `/home/mherwig/dev/ocx-evelynn/.claude/artifacts/prd_oci_referrers_discovery.md` lines 233–234
**Text (row 1):** `"User sets level = 'strict' in trust-policy and hits v1 rejection"` — references `level = "strict"` which does not exist in either slice.
**Text (row 2):** `"Trust-policy v1 shape diverges from v2 shape"` — describes `level = "skip"` TOML schema which was rejected and removed.

Both risks originate in the superseded ADR and were never amended out. They describe behavior (TOML trust-policy parsing, `level` enum, `version = "1"` gate) that the amendment summary on line 24 explicitly removed. A security reviewer reading this table would incorrectly infer that `level`-based policy parsing exists in Slice 2.

**Remediation:** Delete both rows. Add a replacement row: `"Trust-policy TOML not shipped; exit codes 78/79 reserved for v3+" | L | L | "No code to mitigate; documented as out-of-scope"`.

---

### Finding 3 — `spdx-rs` API surface unspecified at stub contract level
**Source:** `/home/mherwig/dev/ocx-evelynn/.claude/artifacts/adr_oci_referrers_discovery_v2.md` §SBOM parsing algorithm; plan Step 4.6
**Text in plan Step 4.6:** `"spdx_rs::parsers::spdx_from_tag_value or serde_json::from_slice::<spdx_rs::SPDX>(...) (crate API TBD at implementation; document choice in code comment)"`

The stub contract in Step 1.6 declares `pub fn parse(bytes: &[u8]) -> Result<SbomSummary, SbomErrorKind>` but leaves the `spdx-rs` API selection explicitly deferred to implementation. This is not a post-stub issue per se, but the test in Step 3.5 (`parse_spdx_2_2_backward_compat`) depends on the crate actually accepting SPDX 2.2 documents — a property that varies between `spdx_from_tag_value` and the `serde_json` JSON path. If the wrong API is chosen at implementation time, the 2.2 backward-compat test behavior changes unexpectedly.

The Codex review finding #3 (cache contract) and the plan's own Phase 2 spec-compliance check list ask whether the contract is fully specified, and this is one hole: the `spdx-rs` function-call API is the only new crate API in Slice 2 left as TBD.

**Remediation:** Resolve the API choice in the stub phase document (not deferred to impl). Add to Step 1.6's `sbom/spdx.rs` stub note: `"Uses serde_json::from_slice::<spdx::SPDX>(...) for JSON path"` (or `spdx_from_tag_value` if the tag-value format is the authoritative one). This prevents the implementation from silently changing the contract the tests were written against.

---

### Finding 4 — Legacy `.sig` tag vs. `.sig` suffix ambiguity in plan vs. ADR
**Source:** Plan Step 1.3 public API comment vs. ADR v2 §Decision S2-A (manifest-walk algorithm) step 2a

The plan's Step 1.3 documents `discover_legacy_sig_tag` as trying `/v2/<repo>/manifests/sha256-<subject>.sig`. The ADR §S2-A step 2a says this path is only attempted when "the fallback tag returns 404 AND the subject tag is the tag, not a digest." This conditional is not reflected in the function's stub signature — `discover_legacy_sig_tag` accepts `subject_digest: &Digest`, implying it always tries the `.sig` tag path.

The two-condition guard (404 + subject-is-a-tag) is a correctness requirement (prevents the `.sig` tag lookup from firing on digest-addressed subjects where it can never match), and it needs to be visible in the stub signature or a caller-contract comment so the Phase 2 architecture review can validate it before tests are written.

**Remediation:** Add a caller contract note to Step 1.3: `"Callers MUST only invoke this function when the subject was addressed by a mutable tag (not a digest). Digest-addressed subjects skip this step."` Reflect this as a parameter or precondition in the stub.

---

### Finding 5 — Branch 5 (offline + cache miss) duplicated in ADR algorithm; plan only tests it once
**Source:** ADR v2 §Cache algorithm, Branches 4 and 5; plan Step 3.1

The ADR's pseudocode has the `if offline` check at two points: at the start of Branch 4 (before the network call, when capability is unknown or `--no-cache` is set) and again as Branch 5 (offline + cache miss, at the end). The plan lists only one test: `offline_plus_cache_miss_returns_exit_81`. Branch 4 offline case (offline + capability unknown, no prior cache entry) is structurally distinct from Branch 5 offline case — Branch 4 fires before any network attempt, Branch 5 is a defensive sentinel that should be unreachable given Branch 4, but their combined presence in the pseudocode means the implementation will need to guard both.

A single test covering only "offline + empty cache at the sentinel" does not exercise the Branch 4 path, which is the one that actually fires in practice when `capability` is `Unknown`.

**Remediation:** Add a second unit test to Step 3.1: `branch_4_offline_with_unknown_capability_returns_exit_81` — `offline=true`, capability cache empty, referrer cache empty; assert `Err(OfflineBlocked)` fires from Branch 4 (before any network call is attempted). This covers the actual runtime path for first-time offline use.

---

### Finding 6 — `ocx sbom` offline success path requires warm blob cache, not just warm referrer-index cache
**Source:** PRD FR-S2-15 (line 150); plan Step 3.11 `test_sbom_offline_with_cache_succeeds`; ADR v2 §Decision S2-B

FR-S2-15 states: "`ocx sbom --offline` with a warm referrer-index cache + warm SBOM blob in content-addressed store exits 0." The plan Step 3.11 acceptance test only states "prime cache with an online run; re-run with `OCX_OFFLINE=1`; exit 0 from cache." The test does not specify that the SBOM blob itself (the OCI layer bytes) must also be cached in the content-addressed store, nor does the test assert that no network call is made.

The plan Step 4.9 (`sbom_one`) description says the task pulls the selected manifest then pulls the first layer blob — both via `Client`. If the manifest and blob are not in the blob cache from the prior online run, the offline path would fail even with a warm referrer-index cache. The spec compliance question is: does the implementation plan specify that the manifest + blob pull path also goes through the local CAS before touching `Client`?

**Remediation:** Add to Step 3.11's `test_sbom_offline_with_cache_succeeds` scenario: `"assert transport call counter == 0 (no network calls)"` and add a note to Step 4.9 specifying that blob pulls check `~/.ocx/blobs/` first before calling `Client::pull_blob`, consistent with the existing offline-first architecture. Without this, the offline acceptance test passes even if the blob is re-fetched, giving false confidence.

---

### Finding 7 — `--no-cache` effect on `ocx verify` not in scope table or plan step
**Source:** ADR v2 §Decision S2-B; plan §Scope "In Scope" list; Step 1.10

The plan states `--no-cache` bypasses both caches in the scope for `ocx sbom`. The same bypass should apply to `ocx verify` (per ADR v2 §S2-B: "`--no-cache` bypasses the referrer-index cache AND the Slice-1 capability cache" — for all commands). Step 1.10 modifies `verify.rs` but does not specify that `--no-cache` is threaded through to `resolve_referrers`. There is no unit test in Phase 3 for `verify --no-cache` specifically.

The plan's Step 3.3 (`pipeline.rs` tests) covers dispatch behavior but not cache bypass. The acceptance test `test_verify_no_cache_bypasses_cache` is also absent — only `test_sbom_no_cache_bypasses_cache` appears at Step 3.11.

**Remediation:** Add to Step 1.10: "Thread `no_cache: bool` from `Verify` struct through to `resolve_referrers` call." Add `test_verify_no_cache_bypasses_cache` to Step 3.12 acceptance tests with the same transport-counter assertion as the sbom variant.

---

### Finding 8 — `SbomSummaryReport` field `signature_format` is semantically wrong for SBOM context
**Source:** Plan Step 1.11, `SbomSummaryReport` struct definition

The `SbomSummaryReport` struct includes:
```rust
pub signature_format: String,  // N/A for sbom, keeps parity on JSON row shape
```

This field is documented as always `"N/A"` for the SBOM command. Emitting a `signature_format` field in SBOM JSON output is misleading — consumers of `ocx sbom --format json` who inspect `signature_format` will be confused about what it means. The comment "keeps parity on JSON row shape" is not a contract argument; `ocx sbom` and `ocx verify` are different commands with different schemas, and both already carry `schema_version: 1` for schema identity.

The ADR v2 §SbomSummary struct does not include `signature_format`; it appears only in the `VerifyResult` struct (Step 1.4). The plan's DTO introduces this field without ADR backing.

**Remediation:** Remove `signature_format` from `SbomSummaryReport`. If field-alignment between verify and sbom JSON consumers is the goal, that's an API convention issue for Phase 5 (Review-Fix Loop), not a stub contract. The ADR is the authoritative contract and it does not include this field.

---

### Finding 9 — `referrer_cache_corrupt` error kind is "internal" but plan tests it via `error_envelope.rs`
**Source:** ADR v2 §Error-Kind Additions table ("emitted only when `--log-level=debug`"; `n/a` exit code); plan Step 3.10 (does not include a test for this variant)

The ADR marks `referrer_cache_corrupt` as an internal debug-only emission with no exit code, but Step 3.10 enumerates tests for the other 7 error kinds added to `error_envelope.rs` without mentioning `referrer_cache_corrupt`. The plan Step 4.1 says corrupt JSON → `Ok(None)` (treat as miss) + debug log — which means the `referrer_cache_corrupt` error kind is not propagated to the envelope at all; it's a log event only.

The discrepancy: the ADR §Error-Kind Additions table lists `referrer_cache_corrupt` as an `error_kind` string (implying it appears in the envelope), but the cache implementation plan says it's a debug log (implying it does not). This contradiction must be resolved before implementation to avoid either omitting the test or writing dead code in the envelope mapper.

**Remediation:** Remove `referrer_cache_corrupt` from the ADR §Error-Kind Additions table (or move it to a "Log events (not envelope)" section). The plan's cache behavior (treat as miss + debug log) is the correct design per D6 (stable schema). No `error_envelope` test needed for this case; add a `log_emitted_for_corrupt_cache_entry` unit test in `cache.rs` instead that asserts the `tracing::debug!` call fires.

---

## Deferred Findings

### Deferred 1 — `spdx-rs` unmaintained: no maintainer plan if 2.3 edge-SBOM parse breaks
**Source:** ADR v2 §Decision S2-C; PRD Dependency table line 219
**Text:** `"spdx-rs 0.5.5 — unmaintained upstream but pinned exactly; 2.3 deserialization path works"`

The ADR documents the risk and lists fuzz-testing as the mitigation (fuzz SBOMs from `cargo-cyclonedx` + `syft` + hand-crafted edge cases; wrap parse failure in `SbomParseError::Spdx2UnsupportedEdge`). The plan Phase 3 Step 3.5 includes golden fixture tests but does not include a fuzz test step — fuzz-testing is called out in the ADR risk table but not scheduled in any plan phase.

This is deferred rather than actionable because (a) the ADR acknowledges the risk with a named mitigation, (b) the fuzz scope (which edge SBOMs?) requires human judgment on priority before scheduling, and (c) the golden-fixture unit tests provide a functional floor that the implementation can ship against.

**Reason for deferral:** Human judgment needed on whether to add a fuzz step to Phase 3 or Phase 5, and which SBOM edge cases to include. Recommend tracking in the Phase 5 review commit body.

---

## Passed Checks

1. **Q1 — Blocked-by-Slice-1 declared.** Plan lines 16–34 provide an explicit "Blocked by" section listing every symbol imported from Slice 1 with exact paths and consumption descriptions.

2. **Q2 — Slice 1 import cross-check.** The "Slice 1 cross-check" table at plan lines 170–186 maps every imported path to a Slice 1 plan line number. All 12 paths verified present in Slice 1 plan (confirmed by reading `plan_slice1_sign_and_verify.md` lines 471–510).

3. **Q3 — `cache.rs` ownership.** Plan line 186 explicitly resolves the meta-plan discrepancy: "Slice 1 plan does not list `oci/referrer/cache.rs`. That file is net-new in Slice 2." Clean override documented.

4. **Q4 — Cache algorithm specified.** ADR v2 §Cache algorithm (normative pseudocode) defines all 6 branches. Plan Step 3.1 maps 7 unit tests (6 branches + `--no-cache` bypass) to those branches by name. Finding 5 above flags Branch 4/5 offline overlap — addressable but does not invalidate the overall coverage.

5. **Q5 — SBOM discovery contract.** `ocx sbom` inputs/outputs fully specified: CLI surface (Step 1.9), `ParsedSbom` return type (Step 1.6), `SbomSummaryReport` DTO (Step 1.11), error variants (Step 1.6 `SbomErrorKind`), `--format json` schema determined by `SbomSummaryReport` with `schema_version: 1`.

6. **Q6 — SBOM parsing contract (partially).** `cyclonedx-bom` 0.8.1 API is specified by crate name + version; `SbomSummary` extraction steps are detailed in Step 4.5. `spdx-rs` API is partially deferred (Finding 3). CycloneDX contract is complete.

7. **Q7 — Legacy cosign signature detection.** Detection logic specified: `sha256-<digest>.sig` tag path in Step 1.3; `config.mediaType == COSIGN_LEGACY_SIG_V1` dispatch in Step 1.4; parsing into `LegacyCosignBundle` with Rekor SET + cert chain; unit tests in Step 3.2 (7 tests). Finding 4 flags a minor stub-contract ambiguity.

8. **Q8 — Manifest-walk fallback.** Algorithm is normatively specified in ADR v2 §S2-A steps 1–4. Plan Step 4.2/4.4 implements it. Cosign #4641 defensive classification is a named requirement in FR-S2-4 and acceptance test `test_verify_cosign_4641_defensive_classify`. No explicit pagination or rate-limit-retry contract — consistent with the "out of scope" decision on pagination and the `Retry-After` honor via `ClientError::RateLimited → exit 75`.

9. **Q9 — Exit codes.** ADR v2 §Exit Code Taxonomy table covers all Slice-2 paths; every error kind in §Error-Kind Additions maps to an exit code. No new `ExitCode` variants. PRD Persona 1 acceptance criteria list the full exit-code set (0/64/65/69/74/75/77/79/80/81/82) with a 100% coverage NFR (NFR-S2-7).

10. **Q10 — JSON error envelope.** ADR v2 §Decision S2-D: reuse verbatim, same `schema_version: 1`, new kinds are enum additions. Plan Step 3.10 includes 7 unit tests covering every new error kind's envelope serialization. `referrer_cache_corrupt` gap is captured in Finding 9.

11. **Q11 — PR-FAQ scrubbed of old skip-level / trust-policy-TOML.** PR-FAQ lines 23–24 explicitly state `level = "skip"` never existed in either shipped slice. The amendment summary is clean. Finding 2 flags two un-amended rows in the PRD Risks table (not the PR-FAQ). The PR-FAQ itself passes.

12. **Q12 — Acceptance tests concrete.** `test_sbom.py` (13 scenarios, Step 3.11) and `test_verify_legacy.py` (10 scenarios, Step 3.12) both specify fixture-registry setup, expected JSON shape, and error scenarios. No live `cosign sign` dependency — fixtures are committed bytes per Codex finding #8.

13. **Q13 — PRD deltas.** PRD is amended (not rewritten) per the amendment summary on line 24. FR/NFR lists are Slice 2 scoped (FR-S2-1 through FR-S2-21, NFR-S2-1 through NFR-S2-14). Slice 1 FRs are referenced by pointer, not duplicated.

14. **Q14 — Superseded ADR handled.** `adr_oci_referrers_discovery.md` line 6 reads `"Status: Superseded by adr_oci_referrers_discovery_v2.md (2026-04-19)"`. Content retained for historical record. Passes.

15. **Q15 — All 9 Codex findings carried forward.** Plan line 88 lists all 9 by name. Each is verified: repo paths (correct crate paths throughout), context injection (`sbom_context()` returns `(&Index, &Client)`), cache contract (6-branch algorithm), error taxonomy (no new ExitCode variants), JSON envelope (schema_version 1 frozen), fixtures (committed bytes at `test/fixtures/`), no `cosign sign` in CI, no `trybuild`, no new ExitCode variants.
