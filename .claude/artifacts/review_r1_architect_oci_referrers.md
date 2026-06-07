# Round 1 Architect Review: OCI Referrers Discovery (Issue #24)

**Reviewer:** Second-architect adversarial panel  
**Date:** 2026-04-19  
**Artifacts reviewed:** `adr_oci_referrers_discovery.md`, `plan_oci_referrers_discovery.md`, `prd_oci_referrers_discovery.md`, `pr_faq_oci_referrers_discovery.md`  
**Supporting context read:** parent ADR `adr_oci_artifact_enrichment.md`, all three research artifacts, both discover artifacts, arch-principles, subsystem-oci, subsystem-cli

---

## Summary

**Verdict: PASS-WITH-FIXES**

The design is substantially sound. The primary architect has done thorough research, clearly identified the one-way-door decisions, and made defensible calls on each. However, seven findings are actionable before `/swarm-execute` begins: three are correctness risks (the `--download -` stdout-collision ambiguity, the unbounded round-trip count on the fallback path when pagination kicks in, and the trust-policy coupling shape that borrows Notation semantics without justification), and four are documentation/scope gaps that will cause friction if left to implementers. Six additional concerns require human judgment and are deferred. Total: **7 actionable, 6 deferred**.

---

## Actionable Findings

**1. adr_oci_referrers_discovery.md §CLI Contracts / prd §FR-18 — `--download -` stdout collision is underspecified and the chosen resolution contradicts the invariant**

The ADR states Invariant 5: "Structured stdout, diagnostics on stderr. Never interleave." Then the plan's risk table says "`--download -`: stdout is bytes; JSON report suppressed to stderr" — but "suppressed to stderr" is wrong phrasing; it means the JSON report is sent to stderr when `--download -` is active. That violates Invariant 5 in the other direction: stderr carries structured data (the JSON report). The acceptance test is noted but the _spec_ itself contradicts the invariant.

Fix: In the ADR §CLI Contracts and §Invariants, add a formal carve-out: when `--download -` is specified, stdout is raw bytes only; the JSON report is not emitted at all (not redirected to stderr). The test scenario in the plan (`test_sbom_download_dash_writes_stdout`) should assert the exit code, the byte content on stdout, and that stderr is empty. Update the invariant text to state that `--download -` is the sole exception to structured-stdout where raw bytes replace the report entirely.

---

**2. adr_oci_referrers_discovery.md §Decision C / plan §Step 4.5 — Worst-case round-trip count on fallback path is not bounded when there are many referrers**

The ADR says "Typical fallback index has 1–5 descriptors — the N+1 cost is bounded." But the OCI spec defines no upper bound on the number of manifests in an ImageIndex. The `pull_referrers` upstream code does not implement referrer pagination (the plan explicitly defers pagination to v2). If a popular package accumulates 50 referrers (e.g., multi-environment signing + SBOM per release via a CI loop that keeps re-signing without cleanup), the fallback path makes 50 sequential manifest fetches. This is not bounded by the ADR's stated typical range; it is bounded only by the actual size of the tag index.

Fix: Add an explicit defensive cap to the fallback classify loop: `MAX_FALLBACK_MANIFESTS_TO_CLASSIFY = 20` (or another product-justified integer). Above the cap, classify remaining descriptors as `Unknown { reason: "fallback index exceeds classification limit; use --distribution-spec v1.1-referrers-api on a supported registry" }` and emit a warning on stderr. Document the cap in the ADR §Invariants and expose it as a future config knob.

---

**3. adr_oci_referrers_discovery.md §Trust Policy Shape — Notation-shaped trust-policy TOML is not the right shape for cosign keyless and creates a semantic lie**

The trust-policy v1 borrows `registry_scopes`, `trust_stores`, and `trusted_identities` from Notation's policy model. Notation's trust model is certificate-based (X.509 subject, trust store = CA bundle). Cosign keyless trust model is OIDC-based (Fulcio issuer URL, email/SAN identity, Rekor inclusion proof). These are structurally incompatible: a `trust_stores = []` field has Notation-style semantics that mean nothing for cosign keyless, and `trusted_identities = ["*"]` is a wildcard that will be unacceptably dangerous for cosign keyless in v2 (it would pass any OIDC issuer). The ADR acknowledges this only as a risk ("schema mismatch with v2") without addressing why borrowing Notation's shape is principled.

This is a one-way-door decision: once users write `trust-policy.toml` files with `trust_stores = []` and `trusted_identities = ["*"]`, v2 cannot change the field semantics without a breaking schema migration. The risk table says `version = "1"` gates this, but the _field names_ are already load-bearing in v1 even if their values are ignored. A v2 strict mode for cosign keyless needs `oidc_issuers` and `subject_alt_name_regexp` fields, not `trust_stores`.

Fix: Remove the Notation-borrowed field names from the v1 schema entirely. v1 `trust-policy.toml` should contain only `version = "1"` and the `level = "skip"` setting. The shape for enforcement fields should be explicitly left as TBD in a comment. This is actually simpler (the ADR already treats those fields as ignored), is honest about what v1 enforces, and leaves v2 free to define the correct cosign-native field shape. Update the TOML example in the ADR §Trust Policy Shape accordingly.

---

**4. plan_oci_referrers_discovery.md §Phase 1 Step 1.10 / prd §Dependencies — `cyclonedx-bom` dep is an unspent innovation token**

`cyclonedx-bom = "=0.8.1"` is added to the workspace for "CycloneDX JSON validation." The plan's own scope table says "CycloneDX parsing beyond pass-through download (v2 or later)" and "v1 treats SBOM content as opaque bytes for download." If v1 treats SBOM bytes as opaque, the `cyclonedx-bom` crate does zero validation work at runtime. It is dead code that costs binary size, transitive dependency audit surface, and an innovation token. The `quality-core.md` "Choose Boring Technology" principle says each novel dependency spends a token. `sigstore-rs` is already spending two (pre-1.0 ecosystem lock-in). Adding a third for zero v1 functionality is unjustified.

Fix: Remove `cyclonedx-bom` from the v1 dependency set. Mention it in the plan as a v2 addition when SBOM validation is actually implemented. If a stub is needed for the type system (e.g., a `SbomFormat::CycloneDxJson` enum variant), that variant can exist without importing the crate.

---

**5. adr_oci_referrers_discovery.md §Cache Layout — Cache coherence interaction with the existing pending refactor is not acknowledged**

The MEMORY.md "Cache coherence" entry flags: "Some commands call `context.remote_client()` directly instead of `default_index`. Audit remaining call sites." The new `ReferrerDiscovery` facade introduces a third access pattern: `context.referrer_discovery()` (a new accessor that must be designed). The plan does not specify whether `ReferrerDiscovery` constructs its own `Client` or receives the shared `Client` from `Context`. If it constructs its own, it bypasses the existing token cache and creates a second auth session. If it receives the shared `Client`, it is subject to the same coherence issue as the pending refactor. Either way, the plan silently adds a new call site to the class of bugs already flagged.

Fix: In the plan §Technical Approach, explicitly state that `ReferrerDiscovery::new()` takes a `Client` reference from `Context` (not a new builder call) and that this is the same `Client` used by `PackageManager`. Add a note that `context.referrer_discovery()` is a new `Context` method that initializes `ReferrerDiscovery` with the existing shared client, not a fresh one. Tie this to the existing cache-coherence audit as a related change.

---

**6. plan_oci_referrers_discovery.md §Phase 3 Acceptance Tests — `cosign_keyless_identity` fixture strategy for offline verification is underspecified and creates a probable skip-loop**

The plan proposes: "use pre-generated static test vectors checked into `test/fixtures/cosign/`." But cosign keyless bundle v0.3 includes a Rekor inclusion proof with a checkpoint signature and a signed certificate chain. These test vectors are time-sensitive: the embedded certificate has a short-lived validity window (typically 10 minutes for Fulcio-issued certs). Checked-in test vectors will expire and the tests will start failing with "certificate expired" errors within hours of being committed unless `sigstore-rs` is configured to skip validity window checks in test mode.

Fix: The plan must specify the exact `sigstore-rs` configuration for test-mode verification: either (a) use `verify_with_no_certificate_check = true` or equivalent flag in the test harness, or (b) use a private Fulcio instance in the docker-compose fixture. Document which approach is chosen, confirm `sigstore-rs` v0.13 supports it, and add an acceptance test that explicitly exercises the "certificate expired" error path to prove OCX handles it gracefully (exit code and message).

---

**7. prd_oci_referrers_discovery.md §Open Questions Q-PRD-3 and Q-PRD-7 — Two exit-code ambiguities will cause production bugs if left unresolved before stubbing**

Q-PRD-3: missing `--trust-policy` file → exit 78 (ConfigError) or 79 (NotFound)? Q-PRD-7: registry 500 + `--require-referrers` → exit 69 (Unavailable) or 65 (DataError)?

These are not just product questions — they are contractual behaviors that will be encoded in `ClassifyExitCode` impls during Phase 1 stubbing. If the stubs hard-code the wrong code, Phase 3 tests will pass against the wrong exit code, the wrong default will survive all reviews, and CI integrations will be silently wrong.

Fix: Resolve both questions before Phase 1 begins. Recommended resolutions (the review panel should confirm): Q-PRD-3 should be exit 79 (NotFound) when the explicit `--trust-policy <PATH>` argument points at a missing file (user specified a path; the path is not there — that is a data-not-found error, not a config-format error). Q-PRD-7 should be 69 (Unavailable) — transport failure preempts the require check; a user cannot distinguish "no signatures" from "registry is down" in the 500 case. Both resolutions should be written back into the ADR §Error Taxonomy and §CLI Contracts before the stub PR is opened.

---

## Deferred Findings

**D1. adr_oci_referrers_discovery.md §Decision A3 — Parent-ADR amendment authority: is a "read-side only" split principled or post-hoc rationalization?**

The new ADR claims the parent ADR's "no fallback" stance was written in push-path context and that the read path is just ~50 lines. This is correct as stated. However, the parent ADR's "no tag fallback" section ends with: "When GHCR adds support, OCX users get supply-chain features automatically with no code changes." That clause explicitly anticipated waiting. The new ADR directly counters this choice. Whether the parent ADR's authors would accept this amendment requires human judgment about the original intent. The risk of user confusion ("why does `ocx package sign` tell me my registry is unsupported, but `ocx verify` works fine?") is real and requires a user-guide section explicitly explaining the asymmetry. The asymmetry is defensible but only if documented clearly.

Deferral reason: Requires the original ADR author to confirm intent, not just a second architect.

---

**D2. adr_oci_referrers_discovery.md §Decision B1 — sigstore-rs pre-1.0 lock-in: what is the exit path if sigstore-rs stagnates?**

The ADR pins `=0.13` and notes "10 breaking changes in 19 releases." It does not address: what happens if sigstore-rs reaches 0.14 and OCX cannot safely upgrade because `sigstore-rs` introduced a required TUF root format change that breaks the embedded root? The ADR says "pin and plan deliberate upgrades" but there is no trip wire. The `sigstore-rs` maintainers (Sigstore org) have every incentive to keep it moving; the risk is not stagnation but forced upgrades that land on OCX's critical path at inconvenient moments.

Deferral reason: Requires a policy decision on how long OCX will hold a pinned pre-1.0 dep before making an upgrade PR a blocker milestone. Human judgment on org priorities.

---

**D3. adr_oci_referrers_discovery.md §Decision D3 — "install does not auto-verify" vs competitor default behaviors**

The ADR acknowledges that pip with `--require-hashes`, npm with `--integrity`, and docker with Content Trust verify by default. It decides not to. The stated rationale — "GHCR/Docker Hub lack the Referrers API; fail-closed would break the majority of installs on day one" — is correct for v1. But it forecloses a clean migration path: once `ocx verify && ocx install` becomes the documented canonical pattern, the ergonomic pressure to flip the default increases. When GHCR eventually adds the Referrers API, OCX will face a moment where "opt-in verify" looks like a security wart compared to peers.

Deferral reason: This is a product roadmap decision about when OCX is willing to be opinionated about supply-chain verification. Requires human judgment about market positioning and customer readiness. Not a blocker for v1.

---

**D4. adr_oci_referrers_discovery.md §No DSSE → No in-toto predicates — false sense of security risk**

The ADR defers DSSE attestation verification. Modern supply-chain attestations (SLSA provenance, VEX statements, deployment annotations) are predominantly DSSE-wrapped in-toto predicates as of 2026. `ocx verify` will report "verified" (or rather, "referrers found, verification skipped per trust policy") while silently ignoring the attestation that the package's provenance chain was tampered. The ADR's mitigation — text output and JSON output say "not a signature, not verified" — partially addresses this but only if users read the output carefully. The PR-FAQ's "What signature formats does v1 verify?" section is correct but buries the DSSE gap. The risk: a compliance auditor asks "is this package's provenance verified?" and an engineer reads `ocx verify` output as "yes" when the meaningful attestation is the DSSE predicate, not the cosign bundle.

Deferral reason: Whether to treat DSSE discovery-without-verification as acceptable v1 scope, or to add an explicit `--warn-unverified-attestations` flag that exits non-zero when DSSE attestations are found but unverified, is a product decision. Needs human input on the compliance audience's risk tolerance.

---

**D5. prd_oci_referrers_discovery.md §NFR-1 and NFR-2 — NFR targets have no measurement plan**

NFR-1 targets `<200ms cold-cache p50` and NFR-2 targets `≤+3.5 MB binary size`. Both are reasonable targets, but the plan has no phase that measures them. If binary-size blows past 3.5 MB due to `sigstore-rs` transitive deps (it embeds a TUF root, crypto primitives, and a full HTTP client), this will be discovered in the review-fix loop rather than as a gated check. Similarly, latency testing against a local registry:2 is not the same as p50 against a real registry.

Deferral reason: Whether to add a binary-size gate to `task verify` and a benchmark for referrer discovery latency is an engineering process decision. Not blocking for v1 correctness, but if the team cares about the NFR values they should add measurement before shipping.

---

**D6. plan_oci_referrers_discovery.md §Phase 4 Step 4.8 Discovery Algorithm — Per-platform descent for Platform::any() packages is underspecified**

The algorithm says: "If resolved manifest is ImageIndex, pick ImageManifest digest for platform (or Platform::current() if None)." But OCX supports `Platform::any()` — platform-agnostic packages (Java tools, text utilities). For a `Platform::any()` package, there is no per-platform ImageManifest in the ImageIndex; the package manifest is directly under the top-level tag. The algorithm as written would fail to find it.

Deferral reason: Whether `Platform::any()` packages are a supported input for `ocx verify` in v1, and how the per-platform descent should degrade for them, is a product decision about the completeness of the feature. The fix is a single branch in `discovery.rs` — small, but the product team should decide whether `Platform::any()` packages get full or partial verify support in v1 before Phase 4 begins.

---

## Overall Trade-off Critique

The strongest architectural call in this design is the read/write split on fallback-tag behavior. The original parent ADR's reasoning against fallback was sound — concurrent write races, permanent second code path, maintenance cost — and all of that reasoning correctly remains in place for the push path. The new ADR cleanly separates the costs: read-side fallback is a pure GET sequence with no race conditions. Importantly, the parent ADR's own risk section listed "GHCR delays indefinitely" as a recognized risk with "tag fallback can be added later if needed" as a mitigation. That mitigation is being exercised here, not overturning the decision. The split is principled, not rationalization, and the defensive-parse for cosign bug #4641 shows the architect went one level deeper than required.

The weakest architectural call is the trust-policy TOML shape. Borrowing Notation's `trust_stores` and `trusted_identities` field vocabulary for a schema that will be used for cosign keyless verification is a category error. Notation trust stores are X.509 CA bundles; cosign keyless trust has no concept of "trust store" — it has OIDC issuers, subject patterns, and Rekor log transparency thresholds. The ADR correctly marks these fields as "v1 ignored," but the field names are already a de-facto API contract once users commit them to version control. If v2 must rename `trust_stores` to `oidc_issuers` or add `rekor_url`, it is a breaking schema change. The clean fix is to ship v1 with only `level = "skip"` in the enforcement block and leave all enforcement-specific fields as v2 TBD. This is not a large change but it is a one-way door that should be corrected before the stub compiles.

The innovation token budget is under pressure but defensible for two of the three new dependencies. `sigstore-rs` + `sigstore-trust-root` is one token, not two (they are co-versioned by the same org, effectively a single dependency decision). `cyclonedx-bom` at v1 with zero functionality is a token wasted. Remove it. The total for this feature should be one token (sigstore), not two.

The coupling risk that the first architect most clearly underweighted is the `Context` wiring for `ReferrerDiscovery`. The `Context` struct already has a known cache-coherence problem (flagged in MEMORY.md), and adding a new `referrer_discovery()` accessor without explicitly anchoring it to the existing shared `Client` creates a path to a duplicate auth session. This is not hypothetical — it is the exact failure mode that created the pending cache-coherence refactor. The plan should explicitly address how `ReferrerDiscovery` is initialized from `Context` before the stub lands, not as a builder note.
