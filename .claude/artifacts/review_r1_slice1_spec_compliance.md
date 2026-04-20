# Review R1 — Slice 1 Spec Compliance

**Verdict:** PASS-WITH-ACTIONABLE
**Date:** 2026-04-19
**Focus:** spec-compliance
**Phase:** post-stub (plan artifact review before code exists)

---

## Summary

The plan is well-structured and has internalized all 9 Codex findings from the prior review cycle: file paths are now correct for the actual repo layout, `Context` injects both `default_index` and `Client`, `ClientError` gets all five HTTP-status variants, fixtures are deterministic and committed, `cosign sign` is never called from CI, and the JSON error envelope has an explicit dispatch mechanism in `main.rs`. The signing algorithm (ECDSA P-256 + SHA-256), bundle format (v0.3 only), idempotency behaviour (append-new-referrer), and capability-cache contract are all pinned with byte-level or semantic precision. Five material gaps remain: the `--format json` success-path output shape is under-specified for `ocx verify` (the PRD specifies it for sign but the plan's `VerificationReport` struct comment is a placeholder); four UX error scenarios from the review checklist have no matching test steps; `RekorUnavailable = 82` is not in the canonical `ExitCode` enum rule; the `error_kind` / `error_kind_detail` stable-value inventory is never enumerated; and `fake_registry` acceptance-test infrastructure is named but never designed. These are fixable without architectural rework.

---

## Actionable Findings

### Finding A1 — `--format json` success shape for `ocx verify` is unspecified
**Question:** #3 (CLI determinism — verify success JSON shape)
**Location:** `plan_slice1_sign_and_verify.md:232-233` (`VerificationReport` stub), `plan_slice1_sign_and_verify.md:354` (acceptance test assertion)
**Excerpt:** `pub struct VerificationReport { /* identifier, platform, identity, issuer, signed_at, cert_expired_but_tlog_valid */ }`
**Gap:** The struct is a placeholder comment. The PRD (`prd_oci_referrers_signing_v1.md:90-105`) specifies the JSON success shape for `package sign` in full, but there is no equivalent pinned JSON for `ocx verify --format json` success. The acceptance test at Step 3.10 only checks `schema_version == 1, exit_code present, error_kind present` for the error envelope — it does not assert on the success-path shape of `VerificationReport`.
**Remediation:** Add a pinned JSON sample for `ocx verify --format json` success to the plan (matching the ADR's verify output table), expand `VerificationReport`'s stub field list to match it exactly, and add a Step 3.10 test case asserting the success JSON shape (fields: `identifier`, `platform`, `certificate_identity`, `certificate_oidc_issuer`, `signed_at`, `cert_expired_but_tlog_valid`).

---

### Finding A2 — UX scenarios for Fulcio 401, Fulcio 403, registry 401, and `no-TTY + no-ambient` are missing test steps
**Question:** #2 (UX scenarios enumerated)
**Location:** `plan_slice1_sign_and_verify.md:285-295` (Step 3.2), `plan_slice1_sign_and_verify.md:346-361` (Steps 3.10–3.11)
**Gap:** The review checklist requires explicit test coverage for: Fulcio 401 (OIDC token rejected → exit 80), Fulcio 403 (exit 77), registry 401 (→ exit 80), `--no-tty --no-ambient` sign → exit 77. Registry 401 and 403 on the push-side (not verify-side) are absent from Step 3.11. Fulcio 401 and 403 are handled by ADR exit-code table (80 and 78 respectively) but neither appears in any test step in the plan. The `no-TTY + no-ambient` case is listed in Step 3.2 as an OIDC unit test but is never covered at acceptance level.
**Remediation:** Add to Step 3.11: (a) fulcio 401 → exit 80 `OidcTokenRejectedByFulcio`, (b) fulcio 403 → exit 78 `ConfigError`, (c) registry 401 on sign push → exit 80 `Unauthorized`, (d) `no-TTY + no-ambient` at CLI level → exit 77 (complement the unit test with a CLI-level acceptance test using `--no-tty` and a fake registry that never triggers ambient). These all have defined exit codes in the ADR; the gap is tests, not design.

---

### Finding A3 — `RekorUnavailable = 82` not in the canonical `ExitCode` enum in `quality-rust-exit_codes.md`
**Question:** #4 (exit code discipline)
**Location:** `adr_oci_referrers_signing_v1.md:230–241`, `.claude/rules/quality-rust-exit_codes.md` (canonical enum stops at `OfflineBlocked = 81`)
**Gap:** The ADR introduces `RekorUnavailable = 82` as a new `ExitCode` variant (marked **NEW**). The canonical `ExitCode` enum in `quality-rust-exit_codes.md` — the single source of truth per the rule — does not include `= 82`. The plan's exit-code classification tests (Step 3.9) will test a variant that doesn't exist in the referenced enum definition, causing the rule and the plan to diverge.
**Remediation:** Add `RekorUnavailable = 82` to the `ExitCode` enum in `.claude/rules/quality-rust-exit_codes.md` (with doc comment: "Rekor transparency log unavailable — distinct from registry 5xx at 69; temporal proof cannot be validated.") and update the sysexits case-branch example in that rule to include code 82. This is a rule-file edit, not a code edit, but it must precede the stub phase so the `ClassifyExitCode` implementation has a canonical target.

---

### Finding A4 — `error_kind` and `error_kind_detail` stable-value inventory never enumerated
**Question:** #5 (error-envelope JSON for every error branch) and #3 (CLI determinism)
**Location:** `plan_slice1_sign_and_verify.md:237-255` (Step 1.10 `ErrorEnvelope` struct), `adr_oci_referrers_signing_v1.md:366-380` (JSON envelope sample)
**Gap:** The ADR says "Fields `error_kind` and `error_kind_detail` are stable enum values … Adding a new error kind is a minor-version bump." But neither the ADR nor the plan enumerates the full set of allowed `error_kind` / `error_kind_detail` values. The plan's `ErrorEnvelope` struct has `error_kind: ErrorKind` and `error_kind_detail: Option<ErrorKindDetail>` (opaque types). Step 3.7 only tests that the serde rename is snake_case and that null is omitted — it does not assert that a specific `SignErrorKind` variant serializes to a specific `error_kind` string. Without an enumerated table, the builder has no contract to test against and the stability promise is vacuous.
**Remediation:** Add an `ErrorKind` ↔ `error_kind` string mapping table to the plan (or reference the ADR with a pointer saying "ErrorKind enum has values: `auth_error`, `permission_denied`, `unavailable`, `data_error`, `config_error`, `not_found`, `io_error`, `temp_fail`") and a parallel `error_kind_detail` mapping covering at minimum: `oidc_missing_gha_permission`, `oidc_missing_gitlab_id_tokens`, `oidc_circle_ci_audience_misconfig`, `oidc_no_tty_no_ambient`, `oidc_token_rejected`, `certificate_identity_mismatch`, `certificate_oidc_issuer_mismatch`, `no_signatures_found`, `rekor_unavailable`, `referrers_unsupported`. Then add a Step 3.7 test case that asserts each `SignErrorKind` variant serializes to its expected `error_kind` + `error_kind_detail` pair.

---

### Finding A5 — `fake_registry` acceptance-test infrastructure named but never specified
**Question:** #16 (deterministic fixtures, Codex finding #7/#8)
**Location:** `plan_slice1_sign_and_verify.md:361-362` (Step 3.11)
**Excerpt:** "use `registry:2` response stubs (custom nginx config or bespoke Rust test-registry binary at `test/helpers/fake_registry/`) — **not** recorded HTTP traces"
**Gap:** Step 3.11 requires a fake registry that can respond with 403, 429, 5xx, and missing-referrers-API behaviour. The plan offers two options ("nginx config OR Rust binary") and names a path (`test/helpers/fake_registry/`) but never commits to either design. This is the Codex finding #7 failure mode: named infrastructure without a defined harness. The builder cannot implement the acceptance tests without knowing which approach is chosen. A Rust binary at that path is a non-trivial deliverable that needs to be in scope.
**Remediation:** Commit to one approach (recommended: a minimal Rust axum/hyper binary at `test/helpers/fake_registry/` that serves static responses per configurable JSON fixture — simpler and portable vs. nginx). Add the `test/helpers/fake_registry/` source tree to the "Files to Modify" table and to Phase 3's deliverable list with a stub step (Step 3.0 or pre-3.11). If the nginx approach is preferred, add an nginx config file to the fixture table.

---

### Finding A6 — `SignErrorKind` and `VerifyErrorKind` variant lists never enumerated in the plan
**Question:** #1 (contract completeness) and #14 (error taxonomy)
**Location:** `plan_slice1_sign_and_verify.md:168` (Step 1.4), `plan_slice1_sign_and_verify.md:172` (Step 1.5)
**Gap:** Step 1.4 says "See Exit-Code Mapping below" for the `SignErrorKind` enum, but no "Exit-Code Mapping" section exists in the plan — the ADR's exit-code table is the closest analog. Without an explicit list of `SignErrorKind` and `VerifyErrorKind` variants in the plan, the stub builder must reverse-engineer them from the ADR, risking drift. Step 3.9 requires "a structural test that asserts every `SignErrorKind` / `VerifyErrorKind` variant is covered" — this test cannot be written without the variant list.
**Remediation:** Add a subsection to Phase 1 (or a table in the "Architecture Changes" section) listing every `SignErrorKind` and `VerifyErrorKind` variant by name, its mapped exit code, and the `error_kind_detail` string it serializes to. Minimum variants to enumerate: `SignErrorKind::{OidcNoTtyNoAmbient, OidcMissingGhaPermission, OidcMissingGitlabIdTokens, OidcCircleCiAudienceMisconfig, OidcTokenRejected, FulcioUnavailable, FulcioConfigError, RekorUnavailable, BlobPushFailed, ReferrerPushFailed, OfflineRejected, ReferrersUnsupported}` and `VerifyErrorKind::{NoSignaturesFound, CertificateIdentityMismatch, CertificateOidcIssuerMismatch, MalformedBundle, RekorSetMissing, RekorUnavailable, TrustRootError}`.

---

### Finding A7 — Referrer manifest `config.digest` constant value and `config.size` must be byte-precise in the plan
**Question:** #8 (referrer manifest shape — byte-level precision)
**Location:** `adr_oci_referrers_signing_v1.md:395-421` (referrer manifest shape)
**Gap:** The ADR referrer manifest sample shows `"digest": "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a", "size": 2` for the empty config. This is the SHA-256 of `{}` (2 bytes). The plan (Step 1.3 stub and Step 3.4 tests) does not mention these constants explicitly — `media_types.rs` has `SIGSTORE_BUNDLE_V03` and `EMPTY_CONFIG` but the test step (3.4) only checks "config uses empty-config media type" without asserting the digest or size. If the builder uses `sha256({})` (the correct `{}`) vs. `sha256("")` (empty string) vs. any other value, no test catches it.
**Remediation:** Add `EMPTY_CONFIG_DIGEST: &str = "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"` and `EMPTY_CONFIG_SIZE: u64 = 2` to the planned `media_types.rs` constants table, and add a Step 3.4 test case asserting `config.digest == EMPTY_CONFIG_DIGEST` and `config.size == 2`.

---

### Finding A8 — `SignPipeline` / `VerifyPipeline` context types (`SignContext`, `VerifyContext`) are undefined
**Question:** #1 (contract completeness — inputs and invariants)
**Location:** `plan_slice1_sign_and_verify.md:168`, `plan_slice1_sign_and_verify.md:172`
**Gap:** `SignPipeline::run(ctx: SignContext<'_>)` and `VerifyPipeline::run(ctx: VerifyContext<'_>)` are declared as the pipeline entrypoints, but `SignContext` and `VerifyContext` types are never defined — their fields, lifetimes, and invariants are absent from the plan. The builder has no contract for what these types carry (e.g., does `SignContext` contain the `Identifier`, `Platform`, `OidcToken`, `DispatchingTokenProvider`, flags, or the resolved manifest digest?). This is a stub-phase contract gap: the Phase 2 architecture review cannot validate shape because the shape is undefined.
**Remediation:** Add struct field sketches for `SignContext<'_>` and `VerifyContext<'_>` to the Step 1.4 / Step 1.5 stubs (as a comment table, not full Rust). Minimum fields for `SignContext`: `identifier: &Identifier`, `platform: &Platform`, `token_provider: &dyn TokenProvider`, `no_cache: bool`, `transport: &dyn OciTransport`, `index: &Index`. Minimum fields for `VerifyContext`: `identifier: &Identifier`, `platform: &Platform`, `certificate_identity: &str`, `certificate_oidc_issuer: &str`, `no_cache: bool`, `transport: &dyn OciTransport`, `index: &Index`.

---

### Finding A9 — No test for `--format json` on verify success path (only error path tested)
**Question:** #3 (CLI determinism for all branches, including success)
**Location:** `plan_slice1_sign_and_verify.md:354` (Step 3.10 assertion list)
**Gap:** Step 3.10's `--format json` check is listed only under "error envelope structural checks" (`schema_version == 1, exit_code present, error_kind present`). The PRD specifies a distinct success shape for `ocx package sign --format json` (S1, lines 89–105: a top-level `"signed": [...]` array). There is no corresponding test for `ocx verify --format json` success shape. The builder has no coverage that `VerificationReport::print_json` produces a parseable, schema-correct output.
**Remediation:** Add a Step 3.10 test case: `test_verify_json_success_shape` — run `ocx verify --format json` against a staged happy-path fixture, assert: (a) valid JSON, (b) `schema_version == 1`, (c) `"verified"` key present as array, (d) each element has `identifier`, `platform`, `certificate_identity`, `certificate_oidc_issuer`, `signed_at`, `cert_expired_but_tlog_valid` fields.

---

### Finding A10 — `tasks.rs` convention mismatch: plan says `tasks/mod.rs` but repo uses `tasks.rs`
**Question:** #12 (repo-shape accuracy)
**Location:** `plan_slice1_sign_and_verify.md:175` (Step 1.6), `plan_slice1_sign_and_verify.md:498` (Files to Modify table)
**Excerpt:** "re-exported from `tasks/mod.rs` (i.e. `tasks.rs` per subsystem convention)"
**Gap:** The plan correctly notes the repo uses `tasks.rs` (not `tasks/mod.rs`) as a convention, but the parenthetical is easy to miss and the "Files to Modify" table at line 498 lists the file as `crates/ocx_lib/src/package_manager/tasks.rs` — correct. However, Step 1.6's narrative says "re-exported from `tasks/mod.rs`" first, before the parenthetical correction. A builder reading quickly could create `tasks/mod.rs` instead of modifying `tasks.rs`. This is confirmed by the actual repo: `tasks.rs` exists (319B) and `tasks/` is a directory of individual task files.
**Remediation:** Edit Step 1.6 to say "modify `crates/ocx_lib/src/package_manager/tasks.rs` to re-export the new `sign` and `verify` modules" — remove the confusing `mod.rs` mention. The "Files to Modify" table is already correct.

---

## Deferred Findings

### Deferred D1 — Exit code for `VerifyErrorKind::RekorUnavailable` ambiguity on the verify side
**Location:** `adr_oci_referrers_signing_v1.md:208-209`, `plan_slice1_sign_and_verify.md:172`
**Gap:** The ADR assigns exit 82 to `RekorUnavailable` for both sign (Rekor unreachable after Fulcio) and verify (SET cannot be validated). `VerifyErrorKind` would therefore contain `RekorUnavailable` mapping to exit 82 — but `VerifyErrorKind` is its own enum defined in `oci/verify/error.rs`. It's unclear whether `SignErrorKind::RekorUnavailable` and `VerifyErrorKind::RekorUnavailable` are separate variants that both map to 82, or whether a shared `RekorError` lives in a common module. The plan does not specify. If they're separate, `ClassifyExitCode` must downcast both correctly. If shared, the module hierarchy changes.
**Reason for deferral:** Human judgment needed on whether a shared `RekorError` type belongs in `oci/rekor.rs` vs. independent variants in each pipeline error enum.

### Deferred D2 — `ocx verify` against a cosign-signed bundle (FR-15 second direction) is scoped as "Slice 2"
**Location:** `prd_oci_referrers_signing_v1.md:24` (G3), `prd_oci_referrers_signing_v1.md:335` (FR-15)
**Gap:** G3 states "Artifacts signed by `cosign sign` are verifiable by `ocx verify` (Slice 2 layers in external-sig discovery; Slice 1 provides the primitives)." FR-15 in the Slice-1 acceptance matrix is `test_cosign_verify_ocx_signed` — only OCX-sign / cosign-verify. The reverse (cosign-sign / OCX-verify) is deferred. This is consistent with scope, but the plan should confirm this is a known gap for reviewers assessing G3 completeness.
**Reason for deferral:** Scope decision already made in ADR (Slice 2). No code action required in Slice 1. Flagged for human reviewer who needs to communicate this to users asking about G3.

---

## Passed Checks

1. **Signing algorithm** (Q6): ECDSA P-256 + SHA-256 locked in via ADR decision S1-A; `sigstore = "=0.13"` pinned.
2. **Bundle-write format** (Q7): v0.3 only on push; no legacy tag-write path; S1-F explicitly rejects fallback tags.
3. **Referrer manifest shape** (Q8): `artifactType`, `subject`, `config` (empty sentinel with SHA-256), `layers[0].mediaType` all present in the ADR manifest sample — partially addressed (see A7 for the gap on byte-precision in the plan's tests).
4. **Sigstore staging CI strategy** (Q9): `OCX_TEST_SIGSTORE_STAGING=1` env-gate is specified; integration tests skip gracefully when staging unavailable; fixture-based unit tests are the primary guarantee; staging is not live Fulcio/Rekor.
5. **Re-sign idempotency test** (Q10): Step 3.10 includes "Re-sign produces a second referrer without removing the first" as an explicit acceptance test scenario. Decision S1-I confirmed in ADR.
6. **OIDC pre-check unit tests** (Q11): Step 3.2 covers all six `SignErrorKind::Oidc*` variants with mock injection via `AmbientDetector` trait.
7. **Repo-shape accuracy** (Q12): `command.rs` (flat file, not `command/mod.rs`), `api/data.rs` (flat file), `crates/ocx_lib/src/oci/client/error.rs` and `transport.rs` — all match actual disk layout verified against the working tree. No references to removed modules.
8. **Cache contract split** (Q13): Slice 1 capability cache (24h/1h TTL) is cleanly separated from Slice 2 referrer-index cache; `--no-cache` bypasses capability cache in Slice 1; referrer-index cache is explicitly "reserved" for Slice 2.
9. **ClientError taxonomy** (Q14): Five new variants specified (`Unauthorized`, `Forbidden`, `RateLimited`, `ServiceUnavailable`, `ReferrersUnsupported`) with structured fields including `retry_after`; each gets a `ClassifyExitCode` arm; `test_transport.rs` gets builder methods for all five.
10. **No `trybuild` as unit test** (Q15): Phase 2 is explicitly an architecture review, not a `trybuild` phase; Codex finding #6 acknowledged and documented in the plan notes.
11. **Deterministic fixtures** (Q16): `bundle_v03_gha.json`, `bundle_v03_expired_cert.json`, `fulcio_root.pem`, `rekor_pubkey.pem`, `target_manifest.json/.sha256` all committed; `cosign sign` never called from CI (Codex finding #8 confirmed).
12. **JSON error envelope dispatch mechanism** (Q17): Step 4.16 and Step 1.10 specify `render_error_envelope` called from `main.rs` when `--format json` and an error propagates. The mechanism (dispatch in `main.rs`, serialize via `ErrorEnvelope`, emit to stderr, exit with classified code) is explicit.
