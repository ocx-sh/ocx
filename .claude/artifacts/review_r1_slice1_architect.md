# Review R1 — Slice 1 Architecture

**Verdict:** PASS-WITH-ACTIONABLE
**Date:** 2026-04-19

---

## Summary

The ADR and plan are structurally sound and show serious research depth. Nine out of the ten review areas pass with minor or no issues. Three areas need concrete fixes before implementation begins: (1) the fallback-tag read asymmetry is not defended in the Slice 1 ADR — it is stated but not argued, which is a problem for any reviewer not holding the Slice 2 plan in their head simultaneously; (2) the `TokenProvider` trait surface is insufficient to swap the signing backend without breaking the v0.3 format contract — it conflates OIDC dispatch with the Fulcio/Rekor signing pipeline in a way that makes the trait definition misleading as a seam for backend substitution; (3) the acceptance test strategy relies on Sigstore staging as the sole live integration path, which makes the happy-path sign+verify acceptance test CI-hostile in practice. All other checked areas — exit-code resolution, subsystem boundary cleanliness, error taxonomy, testing pyramid for unit tests, re-sign idempotency, and Slice 2 contract stability — are either defensible or explicitly flagged for human review. No FAIL-tier violations found.

---

## Actionable Findings

### Finding 1 — Fallback-tag asymmetry not defended in Slice 1 ADR (Q2)

**Location:** ADR §Decision S1-F, ADR §"Not Doing" list, plan §Out of Scope

**Problem:** S1-F states "never write fallback tag on push" and then adds one sentence: "Slice 2 will still *read* legacy tag-based signatures other tools have written." This is the most asymmetric decision in the entire feature — OCX writes a pure referrer format that GHCR and Docker Hub cannot discover, but in Slice 2 it will also read `.sig` tags it never writes. The ADR does not defend why a write-never / read-sometimes stance is correct *from OCX's perspective*. The only argument given is "parent ADR ruling." But the parent ADR was written before issue #24 forced signing into v1 scope. The parent ADR's "no fallback tag" stance was about simplicity in *discovery infrastructure*, not about signing ergonomics where the asymmetry actively harms users on GHCR/Docker Hub — the two largest registries on the compat matrix.

**Required fix:** Add a named sub-decision to S1-F that explicitly addresses the asymmetry:
- What does a user on GHCR see when they run `ocx package sign`? (Hard error, `ReferrersUnsupported`, exit 69 — confirmed by the exit-code table, but not called out in S1-F.)
- What is the decision rationale for accepting that GHCR users cannot sign at all in v1, rather than offering write-side fallback? The "forces registries to adopt Referrers API" argument in S1-F is advocacy, not a trade-off analysis. Steelman the fallback position: write-side fallback would cost a second push-path and a GC problem but would immediately serve the largest user population. Explicitly reject it with that cost acknowledged.
- State that Slice 2's read-side fallback does not imply a future Slice 3 write-side fallback (or state that it might — but decide now so Slice 2 does not silently drift toward it).

---

### Finding 2 — `TokenProvider` trait conflates OIDC dispatch with signing-backend seam (Q5, Q7)

**Location:** ADR §Architecture Decisions, plan §Step 1.4, plan §Step 3.2, ADR §Forward-Compat for v2

**Problem:** The ADR defines `TokenProvider` as the seam for swapping the signing backend ("KMS signers plug in at the Fulcio step as a sibling abstraction"), and the plan places `TokenProvider` in `oci/sign/oidc.rs`. But `TokenProvider` only returns an `OidcToken` — it is a token acquisition interface, not a signing pipeline interface. A KMS signer does not acquire an OIDC token and exchange it for a Fulcio cert; it holds its own private key and signs directly, bypassing Fulcio entirely. The forward-compat note in the ADR is therefore wrong: `TokenProvider` is not the seam for KMS. The actual signing pipeline (`fulcio.rs → rekor.rs → bundle.rs`) cannot be swapped by replacing a `TokenProvider`.

The sigstore-rs 0.13 upgrade concern (Q7) compounds this: `SigningBackend` is mentioned in the review brief but does not exist in the ADR or plan — there is no `SigningBackend` trait. The `TokenProvider` trait is too narrow to absorb an API change in `sigstore-rs::fulcio::FulcioClient` because callers of Fulcio are in `fulcio.rs`, not behind the `TokenProvider` abstraction.

**Required fix:** The ADR's forward-compat note for HSM/KMS must be corrected. Two options:
- **Option A (preferred):** Introduce a thin `Signer` trait in `oci/sign/mod.rs` with a single method `async fn sign(&self, ctx: SignContext<'_>) -> Result<SignResult, SignError>`. `KeylessSigner` (the v1 Fulcio/Rekor path) implements it. v2 KMS signers implement it independently. `TokenProvider` in `oidc.rs` remains a narrow helper used only by `KeylessSigner`. This is the actual extensibility seam. The ADR should name it.
- **Option B:** Drop the forward-compat note for KMS and simply document that KMS support requires a new implementation file — no trait needed. Simpler and YAGNI-compliant.

Either way, remove the misleading statement "KMS signers plug in at the Fulcio step as a sibling abstraction" from the ADR.

---

### Finding 3 — Acceptance test happy-path sign+verify is CI-hostile (Q6)

**Location:** Plan §Step 3.10, plan §Testing Strategy §Integration Tests, plan §Gate 4

**Problem:** The "happy path sign + verify" acceptance test (test_sign.py + test_verify.py) requires Sigstore staging to be reachable for a real sign → push → verify cycle against a live Fulcio/Rekor. The plan acknowledges this with `OCX_TEST_SIGSTORE_STAGING=1` and "skip gracefully when unavailable." But the PRD scenario S1 (sign happy path) is the first scenario listed; skipping it in CI means the PR gate does not cover the primary feature scenario. This is the "coverage by runnable fixture, not test-name" concern from the Codex R3 review.

The plan's own mitigation — "pre-generate deterministic bundle fixtures committed under `test/fixtures/signing/`" — is correct for unit tests but does not cover the acceptance-test pipeline path (push to registry:2 → read back). There is no plan for a mock Fulcio + mock Rekor that could run in docker-compose.

**Required fix:** Add one of the following to the plan before Phase 3 begins:
- **Option A (recommended):** Add a `fake_fulcio` + `fake_rekor` service to the docker-compose test stack. These need only implement the subset of the Fulcio/Rekor API used by the signing pipeline (CSR → cert, hashedrekord → SET). A small Rust binary under `test/helpers/fake_sigstore/` achieves this. The happy-path acceptance tests then run without network access, making Gate 4 a strict CI gate.
- **Option B:** Explicitly declare that `test_sign.py::test_sign_happy_path` is a *manual* or *staging* test, move it to a separate `test/tests/staging/` directory, and add a replacement acceptance test that exercises the sign *output shape* (referrer manifest pushed to registry:2 with the correct `artifactType`, correct `subject` digest, correct bundle layer media type) using a pre-signed fixture injected directly. This separates "does the pipeline invoke Fulcio correctly" (unit test) from "does the OCI push produce the right referrer shape" (acceptance test).

The current plan conflates the two, making Gate 4 dependent on a live external service.

---

### Finding 4 — Exit code 69 overloaded: ReferrersUnsupported semantically differs from Unavailable (Q3)

**Location:** ADR §Capability cache contract, ADR §Exit Code Taxonomy, plan §Step 3.11

**Problem:** The capability cache returns "unsupported" when the registry explicitly returns 404 on `/v2/<name>/referrers/`. The ADR maps this to `ReferrersUnsupported → exit 69 (Unavailable)`. But `Unavailable = 69` is documented as "Registry unreachable (DNS / connection refused)" — a transient fault. `ReferrersUnsupported` is a *permanent* registry capability gap, not a transient failure. Users reading exit 69 from shell scripts will retry on 69 (it's in the sysexits semantic family of "come back later"), but `ReferrersUnsupported` is not retry-worthy. This is a user-contract violation: the exit code advises retry, but the condition is permanent.

The Codex-scripted example at the bottom of `quality-rust-exit_codes.md` shows `69 → "registry unreachable; retry with backoff"`. A user following that script will loop forever on GHCR.

**Required fix:** Add a new exit code `83 = ReferrersUnsupported` in the exit-code table (one slot above `RekorUnavailable = 82`). Map `ReferrersUnsupported` → 83. Update `ClientError::ReferrersUnsupported`'s `ClassifyExitCode` impl accordingly. The remediation message must say "this registry does not support the OCI Referrers API; use a supported registry" — not "check network."

---

### Finding 5 — `signing_context()` and `verify_context()` return identical signatures; the split is premature (Q4)

**Location:** ADR §Context injection, plan §Step 1.7

**Problem:** Both `signing_context()` and `verify_context()` return `Result<(&oci::index::Index, &oci::Client)>` and both error via `Error::OfflineMode` when offline. Their bodies in step 4.13 are identical: `return (&self.default_index, self.remote_client()?)`. If both accessors have the same signature and the same body, they are the same function under two names. The only difference that could justify two named accessors would be: different offline behavior, different index selection, or different authentication scope — none of which are specified in the ADR. This violates YAGNI.

**Required fix:** Collapse to a single `online_context()` accessor. If v2 signing requires different auth (e.g., a signing-specific credential), introduce the split then as an additive change. Keeping two identical accessors pre-emptively creates the illusion of a distinction that does not exist, which will confuse implementers.

---

### Finding 6 — `VerifyErrorKind` in `oci/verify/error.rs` duplicates `SignErrorKind` taxonomy without justification (Q4)

**Location:** ADR §New crate/module shape, plan §Step 1.5, plan §Step 3.9

**Problem:** The plan creates `oci/sign/error.rs` (SignErrorKind) and `oci/verify/error.rs` (VerifyErrorKind) as separate enums. The exit-code table contains variants shared across both contexts: `DataError (65)` maps to "corrupted referrer manifest; malformed bundle" — both sign and verify can hit this. `Unavailable (69)` applies to both. `AuthError (80)` applies to both (registry 401 during push or fetch). If both enums map overlapping variants to the same exit codes, callers of `classify_error()` must pattern-match both enums in parallel.

The ADR does not explain why these are separate enums rather than a unified `SignVerifyErrorKind` or why `ClientError` variants (which already carry 401/403/5xx) are not sufficient for the overlapping cases.

**Required fix:** Either (a) add a justification to the ADR for why the enums are separate (e.g., "verbs differ — sign emits `SigningCertificateExpired`; verify emits `CertificateIdentityMismatch`; the overlap is small and the verb-specificity is worth the duplication") or (b) extract shared variants into a `common` module re-exported by both. The plan's Step 3.9 says "A structural test asserts that every SignErrorKind / VerifyErrorKind variant is covered" — this test will be fragile if variants overlap without a canonical mapping.

---

### Finding 7 — `error_envelope.rs` belongs in `ocx_lib`, not `ocx_cli` (Q4, Q9)

**Location:** ADR §JSON error envelope, plan §Step 1.10, plan §Step 4.16

**Problem:** `ErrorEnvelope` is placed in `crates/ocx_cli/src/error_envelope.rs`. The ADR notes that `schema_version: 1` is the stable contract and that adding an error kind is a minor-version bump. But the error *kinds* are defined in `ocx_lib` (`SignErrorKind`, `VerifyErrorKind`, `ClientError`). Serializing them into the envelope therefore requires either (a) the envelope knowing about lib-internal types, or (b) the lib types implementing a serialization interface used by the envelope.

If the envelope lives in `ocx_cli`, it must import and pattern-match against lib error types that are in a lower crate — creating a dependency inversion: the CLI serialization layer has knowledge of every lib error variant by name. When a new `SignErrorKind` variant is added, the developer must remember to update `error_envelope.rs` in the CLI crate. There is no compile-time enforcement of completeness.

**Required fix:** Add a `ClassifyErrorKind` trait (or similar) to `ocx_lib` that each error enum implements, returning a stable `(ErrorKindToken, Option<ErrorKindDetailToken>)` pair. The envelope in `ocx_cli` calls this trait rather than pattern-matching variants directly. This keeps `ocx_lib` as the single source of truth for error taxonomy, and makes the "adding a variant = minor semver bump" rule enforceable by compile-time trait bounds, not documentation.

---

### Finding 8 — No v0.14 migration sketch for sigstore-rs (Q7)

**Location:** ADR §Risks, plan §Dependencies

**Problem:** The ADR pins `sigstore = "=0.13"` and wraps all calls in `fulcio.rs`/`rekor.rs`. This is the right isolation strategy. But there is no documented check for what would constitute a bundle-format regression on upgrade to 0.14. The Risks table says "upgrade path is localized" — but localized to what level? If sigstore-rs 0.14 changes the Fulcio CSR format, the Rekor entry type name, or the bundle v0.3 field names, the wrapper isolation only helps if the internal types are not leaked across the wrapper boundary.

The specific risk is: `oci/sign/bundle.rs` constructs the Sigstore bundle v0.3 JSON. If `sigstore-rs` 0.14 changes the protobuf-derived JSON field names (e.g., `messageSignature` → `message_signature` in some serialization version), the bundle pushed by OCX will be rejected by `cosign verify`. The plan has no test that cross-checks the bundle JSON field names against the Sigstore protobuf spec golden fixture.

**Required fix:** Add one sentence to the Risks table: "sigstore-rs 0.14 bundle format regression: mitigated by Step 3.3 golden fixture test — the fixture is validated against `cosign verify` in the pre-release smoke test (§Manual Testing). On any sigstore-rs minor bump, the golden fixture test must re-pass before merging." Additionally, add to the fixture README runbook that the golden fixture must also be verified by `cosign verify --bundle` after regeneration.

---

## Deferred Findings

**D1 — Exit-code 81 resolution (Q3):** The ADR correctly flags this for human review and adopts Resolution A (keep `OfflineBlocked = 81`, route 5xx to `Unavailable = 69`). Resolution A is semantically defensible. However, the cosign "parity" story is weakened: cosign has no concept of `OfflineBlocked` and exits 1 on most errors. "Cosign-like error-reporting" was never a strong parity claim for exit codes — cosign does not define a stable exit-code taxonomy. Resolution A is acceptable; the "cosign parity" framing in the review brief is a red herring. Human sign-off is still warranted on whether `69` for network 5xx is acceptable to existing users who script against it.

**D2 — Re-sign storage bloat (Q10):** The ADR correctly states "cleanup is GC's job, not sign's." The risk is real but low-severity for v1 given that OCX's primary signing use case is publisher CI (not user-side re-signs). Deferred. If adoption is higher than expected, a `ocx package sign --replace` flag can be added in v2 as a best-effort replace (subject to the race-condition caveat the ADR already documents).

**D3 — `oci/sign/` reaching into `oci/verify/` (Q4):** The module tree as specified does not have any cross-calls between `sign/` and `verify/`. The `TrustRoot` in `verify/trust_root.rs` is the only type that conceptually both paths might want (verify needs it; sign implicitly trusts Fulcio's TUF root via `sigstore-trust-root`). Whether the trust root should be shared across `sign/` and `verify/` via a `keyless/` parent module is a v2 concern (when DSSE/attest lands and needs the same TUF root). For v1, the split is clean. If a `shared` parent becomes necessary in Slice 2 (for the legacy-tag read path), a `refactor:` commit can extract it without breaking the Slice 1 interface.

**D4 — `ci-id` crate being pre-1.0 (Q5):** The plan pins `ci-id = "^0.3"` with a `^` semver bound. If `ci-id` publishes a breaking 0.4, the pin absorbs it but the dispatch state machine may break silently if new environment detection logic conflicts with OCX's `DispatchingTokenProvider`. No action required for v1; the `--identity-token` fallback covers gaps. Flag for the deps review pass.

---

## Passed Checks

**Q1 — Option depth on S1-A through S1-I:** Each decision presents 3+ options with honest cons. S1-A, S1-B, S1-C, S1-D, S1-E, S1-G, S1-H, S1-I all steelman the rejected alternatives. S1-F's rejection of the fallback option is the weakest (Finding 1), but the option itself is listed. No strawmanning found.

**Q3 — Exit-code 81 cosign parity:** Resolution A does not claim cosign parity on exit codes, and cosign does not define a stable exit-code taxonomy, so no parity break exists. The `69` vs `81` conflict is documented and flagged; no silent regression.

**Q4 — `ocx_lib/oci/sign/*` does not reach into `oci/verify/*`:** The module tree as specified has no cross-calls. `identity.rs` is in `verify/`; `oidc.rs` is in `sign/`. The pipeline files do not import from each other.

**Q8 — Reversibility of one-way doors:** S1-B (bundle v0.3 only) and S1-F (no fallback tag) are the two hardest one-way doors. Both are documented as "additive later" — Slice 2 can add legacy-tag read without changing v1 push behavior. S1-H (no skip level) is the most politically loaded decision; the ADR correctly defers break-glass to "users who need it can continue using cosign directly" rather than building an escape hatch that would need deprecation later.

**Q9 — Slice 2 boundary stability:** `oci/referrer/capability.rs`, `oci/client/error.rs` additions, and `error_envelope.rs` are all additive. Slice 2 will import `capability.rs` (referrer-index cache builds on it) and extend `error_envelope.rs` (SBOM errors). Neither requires breaking the Slice 1 public interface as defined.
