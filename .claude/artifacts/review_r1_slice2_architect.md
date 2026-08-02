# Review R1 — Slice 2 Architecture

**Verdict:** PASS-WITH-ACTIONABLE
**Date:** 2026-04-19

## Summary

The Slice 2 ADR and plan are substantially sound. The Slice 1/Slice 2 contract boundary is explicit and cross-checked at the module-path level. The fallback-tag asymmetry (read but never write) has a clear, defensible rationale grounded in the parent ADR and upstream registry limitations. The cache split between `capability.rs` and `cache.rs` is clean. The error-envelope policy (no schema bump, additive enum values) is coherent. However, six actionable findings remain that must be resolved before implementation begins: the manifest-walk fallback has no stated max-referrers bound; the CI TTL detection has a container-on-a-laptop edge case that makes the distinction unreliable; the `spdx-rs` unmaintained status is understated relative to the risk it poses; the SBOM trust gap (unverified SBOM referrers emitted without warning) is not disclosed in the user-facing surfaces; the PR-FAQ has an internal-consistency error in its exit-code description; and the S2-E "bundle wins" precedence rule is unspecified despite both formats possibly being present simultaneously.

---

## Actionable Findings

### F1 — Manifest-walk fallback is unbounded (Q3)

**Location:** ADR §Decision S2-A, manifest-walk algorithm steps 1–4; plan §Step 3.1 `branch_3` test.

**Problem:** The algorithm says "for each descriptor in `manifests[]`" without any cap. The referenced research (`research_oci_referrers_2026.md §3`) states fallback lists are "routinely ≤10 entries." That is an empirical observation, not a protocol limit. A malicious registry can return an OCI ImageIndex with an arbitrary number of descriptors in `manifests[]`; OCX would fetch every one. The ADR notes "Referrer list pagination — v3+ if load-bearing" but the pagination guard is different from a per-response descriptor count cap. There is no `max_referrers` constant, no circuit-breaker, and no test asserting that the manifest-walk terminates when a threshold is exceeded.

**Required fix:** Add a `MAX_REFERRER_WALK: usize = 50` constant (or a registry-policy-derived value; 50 is consistent with OCI spec guidance for index cardinality). The manifest-walk algorithm must truncate at this limit and emit a `warn`-level trace event. The ADR must document this cap normatively. Plan §Step 3.1 branch tests must include an eighth test: `manifest_walk_truncates_at_max_referrers`.

**Rationale:** Without a cap the feature is a network-amplification vector against OCX users on adversarially-operated registries.

---

### F2 — CI TTL detection is unreliable for containerised dev machines (Q8)

**Location:** ADR §Decision S2-B rationale ("same bypass (`--no-cache`), same TTL detection heuristic (stdin-is-a-TTY OR `CI=true` from `ci-id`)"); Slice 1 ADR §S1-C same heuristic.

**Problem:** The 1h/24h TTL split relies on the `ci-id` crate detecting `CI=true`. Docker Desktop, devcontainers, GitHub Codespaces, and many corporate laptop images set `CI=true` in the base image environment, which means a developer's laptop running OCX in a container gets the 24h CI TTL. The developer pushes a new signature, runs `ocx verify` five minutes later, and gets a stale cache hit with no indication why. The ADR acknowledges the cache-versus-new-signature staleness problem but only in the risk table ("if a registry just enabled Referrers API, run with `--no-cache` once") — it does not address the identity-mismatch scenario where a freshly signed artifact is invisible for up to 24h.

**Required fix:** The ADR must explicitly document this known false-positive for `CI=true` in the risk table (not just the "registry adds Referrers API" case). The user-guide entry for the cache must include the phrase "If you are developing inside a container that sets `CI=true`, use `--no-cache` when verifying freshly-pushed signatures." No code change is required, but the missing disclosure is a documentation contract gap that will generate support questions.

**Rationale:** "Same TTL detection heuristic as Slice 1" is not sufficient justification when the heuristic has a known false-positive class that the Slice 2 feature (short-lived referrer freshness) exacerbates.

---

### F3 — SBOM trust gap undisclosed (Q6)

**Location:** ADR §Decision S2-D; ADR §SBOM parsing algorithm step 6; PR-FAQ "What is `ocx sbom`?"; PRD §User Stories Persona 1.

**Problem:** `ocx sbom` emits parsed SBOM content without verifying that the SBOM referrer itself is signed. Any party with push access to the registry (or a malicious registry operator) can attach a fabricated SBOM referrer to a legitimate subject digest. `ocx sbom` will parse and display it with exit 0 and no warning. The ADR §D3 says "SBOM is discovery, not scanning" and the `--help` text leads with "Discover and parse SBOMs already attached." But discovery-without-signature-check is different from discovery-without-disclosure. `oras discover` is explicit that it does not verify; the Slice 2 surfaces are not.

**Required fix:** Three places need a single line:
1. ADR §Risks table: add row "Unsigned SBOM referrer emitted without warning — Low severity (attacker needs push access), mitigated by: `ocx verify && ocx sbom` idiom recommended in user guide."
2. PR-FAQ "What is `ocx sbom`?" answer: add the sentence "SBOMs are not signature-verified by `ocx sbom`; to verify that the SBOM itself was signed by the expected identity, run `ocx verify --certificate-identity ... <reference>` before `ocx sbom`."
3. ADR §`ocx sbom` CLI surface: add a `--help` note alongside the existing "Does not generate SBOMs" note.

No architectural change is needed; this is a disclosure gap.

**Rationale:** The PR-FAQ's customer quote says "our EU CRA audit report now points at a single JSON schema." CRA compliance requires provenance on the SBOM itself. Shipping `ocx sbom` without disclosing the lack of SBOM-signature verification will mislead compliance users.

---

### F4 — S2-E missing precedence rule when both formats present (Q7)

**Location:** ADR §Decision S2-E rationale; ADR §`ocx verify` extensions item 4 ("OCX verifies both and reports both in JSON"); plan §Step 1.10.

**Problem:** ADR item 4 says "When BOTH the referrer fallback-tag AND a legacy `.sig` tag exist, OCX verifies both and reports both in JSON. Plain text shows the first matching one." This defines behavior for the dual-format presence case, but the dispatch rule in §`ocx verify` extensions item 2 says "A pass succeeds if any signature format matches." The ADR does not specify which format is attempted first when both are present, nor does it state whether a v0.3 match short-circuits the legacy path or whether both are always fully verified. This matters for: (a) performance (two full Rekor round-trips when one would suffice), (b) audit output completeness, and (c) the "bundle wins" expectation a caller may have if v0.3 is "more authoritative." The plan §Step 3.3 test `mixed_referrers_any_match_wins` tests that one can succeed when the other fails, but does not specify ordering.

**Required fix:** The ADR must add a normative statement: "When multiple valid signatures are found (both v0.3 and legacy), both are verified in full (no short-circuit). The verification result is SUCCESS if at least one passes. The `VerificationReport` JSON includes all verified entries in referrer-list order; there is no 'canonical' format — both are authoritative." If the intent is that v0.3 short-circuits on match, that must be stated explicitly with the performance rationale. Either answer is acceptable; the gap is the absence of a stated rule.

**Rationale:** Callers who parse `signature_format` in the JSON output need a deterministic contract for what is emitted and in what order.

---

### F5 — `spdx-rs` risk understated (Q5 / Q9)

**Location:** ADR §Decision S2-C rationale ("effectively unmaintained as of April 2026 but its 2.3 deserialization code works and is pinned exactly"); ADR §Risks table row "spdx-rs 0.5.5 unmaintained."

**Problem:** The ADR acknowledges `spdx-rs` is unmaintained and proposes fuzz-testing as mitigation. The risk table rates it Medium. The reversibility cost (Q9) of this choice is not analysed: if `spdx-rs` 0.5.5 is discovered to have a parse bug on a real SBOM corpus (not just crafted inputs), the only remediation is vendoring the crate or replacing it. There is no maintained fork. The ADR does not discuss whether the crate is writable by the OCX project (MIT/Apache-2.0 — yes), whether the ocx-sh org would accept the maintenance burden, or whether patching the submodule is a viable path. The "pin exactly" strategy is correct defensively but does not address the scenario where the pin itself is the problem.

**Required fix:** Add a vendor-or-fork contingency to the ADR risk row: "If `spdx-rs` 0.5.5 emits parse failures on valid SPDX 2.3 SBOMs from standard tools (confirmed by fuzz corpus), the fallback is: (1) vendor the crate at `external/spdx-rs/` under the workspace patch mechanism, and (2) apply targeted fixes. This is acceptable because the crate is Apache-2.0 and the relevant surface area (SPDX 2.3 JSON deserialization) is small." This converts a vague "it works and is pinned" stance into a concrete contingency.

**Rationale:** An unmaintained crate with no named fork is a supply-chain risk in a feature designed for supply-chain compliance. The ADR's own framing makes this a credibility problem if not addressed.

---

### F6 — PR-FAQ exit-code description has an internal error (Q10)

**Location:** PR-FAQ §"What if a verification fails?" answer, last sentence block.

**Problem:** The PR-FAQ states: "exit 77 when the Fulcio cert chain or Rekor SET fails." Exit 77 is `PermissionDenied` — it maps to "Registry 403; `--offline` rejected on sign; OIDC pre-check failure." Fulcio cert chain failure is exit 65 (`DataError`); Rekor SET unavailability is exit 82 (`RekorUnavailable`). A cert-chain or Rekor failure exiting 77 is incorrect per both the Slice 1 and Slice 2 ADR exit-code tables. This is a consumer-facing error in the document positioned as the authoritative customer guide.

**Required fix:** Replace the sentence with: "exit 65 when the cert chain or referrer manifest is malformed; exit 80 when `--certificate-identity` or OIDC issuer does not match the signing cert; exit 82 when the Rekor transparency log is unavailable; exit 79 when no signatures are found. For the full exit-code table see the user guide."

**Rationale:** CI script authors read the PR-FAQ's "What if a verification fails?" section before writing their error-handling branches. A wrong exit-code claim will produce broken pipelines.

---

### F7 — `SbomSummaryReport.signature_format` field is semantically wrong (minor, but public API)

**Location:** Plan §Step 1.11 `SbomSummaryReport` struct definition: `pub signature_format: String, // N/A for sbom`.

**Problem:** `signature_format` has no meaning in an SBOM report. The comment "N/A for sbom, keeps parity on JSON row shape" indicates this field was copied from the verify report to simplify the Printable implementation. Shipping a public field that is always "N/A" in a `schema_version: 1` frozen DTO is a permanent contract liability — it will appear in every CI script that pattern-matches on the JSON keys, confuse callers who assume it carries meaning, and cannot be removed without a schema bump.

**Required fix:** Remove `signature_format` from `SbomSummaryReport`. If the Printable implementation needs a common "row shape" helper, extract a private rendering helper rather than leaking the field into the serialized output. The ADR's `SbomSummary` struct (§Decision S2-C) does not include this field; the plan introduced it independently.

**Rationale:** The ADR explicitly chose schema stability as a first-class constraint (D6). A permanently-"N/A" field is a schema smell that undermines that commitment.

---

### F8 — Cache algorithm Branch 3 has a control-flow error in the pseudocode (code correctness)

**Location:** ADR §Cache algorithm, Branch 3 / Branch 4 structure.

**Problem:** In the pseudocode, Branch 3 ("capability cache says Unsupported") and Branch 4 ("capability unknown OR --no-cache") are written as `if cap == Supported: ... if cap == Unsupported: ... if offline: ...`. Branch 5 ("offline + cache miss") appears after Branch 4's status-code match, but Branch 4 already has an early-return `if offline: return Err(OfflineBlocked)`. This means Branch 5's offline guard is unreachable as written — Branch 4 handles the offline + capability-unknown case before the status-code dispatch can run. The six-branch decomposition in the ADR does not map cleanly to the pseudocode's if-chain, creating ambiguity about what the plan's seventh unit test ("offline + cache miss returns exit 81") actually tests: is it Branch 4's early-exit or Branch 5? The plan §Step 3.1 lists it as "Branch 5" but it is actually reached via Branch 4's offline guard.

**Required fix:** Reconcile the pseudocode with the intended six-branch decomposition. Specifically: the offline guard in Branch 4 and Branch 5 are the same check under different preconditions; collapse them into a single `if offline and cache_miss: return Err(OfflineBlocked)` placed before the status-code dispatch. The six unit tests in the plan should map to six distinct code paths; if Branch 5 is unreachable as described, either remove it from the normative list or re-draw the pseudocode to make it reachable.

**Rationale:** Ambiguous pseudocode in the normative cache algorithm will result in an implementation that does not match the intended state machine. The tests will pass against the wrong implementation.

---

## Deferred Findings

- **CycloneDX 1.6 silent pass-through:** The ADR states "1.6 bytes pass through unchanged but do not parse" and emit `sbom_unsupported_format`. The phrase "pass through unchanged" is potentially misleading — it could imply raw bytes are available via `--download` even when the parse fails. If that is the intent, the `--download` behavior on parse failure should be explicitly specified (raw bytes still written, summary omitted, exit 65). Deferred because this is a product decision requiring human judgment on whether to expose raw bytes on a parse-fail path.

- **In-toto predicate DSSE envelope unwrapping:** The ADR says Slice 2 "unwraps DSSE envelope; recurse on the predicate payload" but DSSE verification is explicitly listed as a gap in `sigstore-rs` 0.13 and as "Not Doing" in the scope guardrails. There is a minor inconsistency: the SBOM parsing algorithm step 5 dispatches `application/vnd.in-toto+json → unwrap DSSE envelope; recurse`. If DSSE unwrap does not involve signature verification, what does "unwrap" mean concretely? This should be clarified in v3 scope documentation. Deferred because it does not affect the Slice 2 implementation path (in-toto referrers would produce `SbomFormat::InTotoWrapper` today and recurse on the inner payload without any signature check — which is consistent with the "discovery, not scanning" principle but should be confirmed).

---

## Passed Checks

1. **Slice 1/Slice 2 contract boundary (Q1).** The plan includes an explicit cross-check table (§Slice 1 cross-check) with grep-confirmed line numbers for every imported path. The meta-plan discrepancy (`cache.rs` ownership) is explicitly called out and resolved in the plan. The Slice 1 ADR interface (transport, capability, verify pipeline, error envelope, exit codes) is consumed without modification — no Slice 1 re-architecture required.

2. **Fallback-tag asymmetry rationale (Q2).** S2-A's read-only asymmetry is explicitly grounded in the parent ADR's write-side ban and the absence of concurrent-write race conditions on the read path. The "no write fallback" property remains a single source of truth. The asymmetry's framing in ADR §Decision Drivers D1 + D2 is defensible and correct.

3. **`cache.rs` split from `capability.rs` (Q4).** Responsibilities are clearly separated: `capability.rs` owns the registry-level probe ("does this registry support the Referrers API?"); `cache.rs` owns the subject-digest-level referrer-index cache ("what referrers does this subject have?"). The plan §Step 2.1 reviewer perspective explicitly checks that `oci/referrer/cache.rs` has no knowledge of SBOM or legacy-cosign semantics. No overlap.

4. **Dual-format verify precedence for the non-ambiguous case (Q7 partial).** The "any match succeeds" rule is clearly stated for the common case. The gap is only in the dual-present ordering (F4 above), not in the success/failure semantics.

5. **PR-FAQ amendment integrity (Q10) — structural.** The PR-FAQ has removed all references to trust-policy TOML, `level = "skip"`, `--require-referrers`, and `--distribution-spec`. The Slice 2 scope (external discovery + SBOM) is consistently framed as layered on top of Slice 1. The amendment summary at the top is accurate. One internal error remains (F6).

6. **Exit-code taxonomy coherence (Q8 partial).** Slice 2 adds zero new `ExitCode` variants. All new `PackageErrorKind` variants map to existing exit codes with appropriate semantics. The 81/69/82 resolution (Resolution A) is correctly inherited from Slice 1 and consistently applied throughout both the ADR and the plan.

7. **SBOM parser crate choice rationale (Q5 partial).** The choice of `cyclonedx-bom` is well-justified — maintained by the CycloneDX org, correct version range, Apache-2.0. The `spdx-rs` choice is documented as unmaintained and pinned exactly; the contingency gap is captured in F5 but the core choice is the right call given no maintained alternative exists. S2-C option table correctly evaluates the trade-offs.

8. **Schema stability (S2-D).** The "additive enum values, same `schema_version`" policy is correctly stated and coherently applied. The plan implements it correctly via `#[non_exhaustive]` enums and the `Other(String)` tail. No schema bump is warranted.
