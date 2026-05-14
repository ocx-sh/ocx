# Review R2 — Slice 1 Spec Compliance

**Verdict:** PASS-WITH-ACTIONABLE
**Date:** 2026-04-19
**Focus:** spec-compliance
**Phase:** post-stub (plan artifact re-review after R1 fix pass)
**R1 source:** `.claude/artifacts/review_r1_slice1_spec_compliance.md`

---

## Finding Status

### A1 — `--format json` success shape for `ocx verify`
**Status: PARTIAL**

The `VerificationReport` stub in Step 1.9 (plan line 238) now lists fields: `identifier, platform, identity, issuer, signed_at, cert_expired_but_tlog_valid`. The acceptance table row for the verify happy path (plan line 618) enumerates the `--format json` success fields (`data.signer_identity`, `data.signer_issuer`, `data.rekor_log_index`, `data.signed_at`). The ADR (lines 610-624) shows a pinned JSON sample. However, Step 3.10 (plan line 383) does not include a named `test_verify_json_success_shape` test case asserting the success fields — the only `--format json` mention in that step is "schema_version == 1, exit_code present, error_kind present" (error envelope checks). The success-path acceptance assertion must be extractable by a builder from the acceptance table, but the test step itself is missing it. Partial credit: shape is specified, test step does not name the assertion.

**Remaining gap:** Add a sub-bullet to Step 3.10 asserting the verify `--format json` success fields (`data.signer_identity`, `data.signer_issuer`, `data.rekor_log_index`, `data.signed_at`) in the acceptance test list.

---

### A2 — UX scenario tests (Fulcio 401→80, Fulcio 403→78, registry 401→80, `--no-tty --no-ambient`→77)
**Status: PARTIAL**

Step 3.11 (plan lines 390-395) covers registry 403→77, 429→75, 5xx→69, Rekor unavailable→82, Referrers unsupported→83. The `no_tty=true` path is in Step 3.2 as a unit test. Missing from any test step: (a) Fulcio 401→80 (`OidcTokenRejected`), (b) Fulcio 403→78 (`FulcioBadRequest`), (c) registry 401→80 (`Unauthorized`) on the sign push side. These three have defined exit codes and fake_fulcio / fake_registry infrastructure exists in the plan, but no test bullet references them. The `--no-tty --no-ambient` CLI-level acceptance test (distinct from the unit test in Step 3.2) is also absent.

**Remaining gap:** Add to Step 3.11 or the acceptance table: Fulcio 401 → exit 80, Fulcio 403 → exit 78, registry 401 on sign push → exit 80, `--no-tty` with no-ambient CLI-level → exit 77.

---

### A3 — `RekorUnavailable = 82` in canonical `ExitCode` enum
**Status: ADDRESSED**

`quality-rust-exit_codes.md` lines 80-88 include both `RekorUnavailable = 82` (with doc comment distinguishing from `Unavailable`) and `ReferrersUnsupported = 83`. The scripts case-branch example (lines 198-202) also includes codes 82 and 83.

---

### A4 — `error_kind`/`error_kind_detail` mapping table
**Status: ADDRESSED**

ADR lines 552-574 contain a complete two-column table enumerating all `error_kind` category strings and their corresponding `error_kind_detail` values for both sign and verify, covering all variants from the inventory. The stability contract (ADR line 549) declares this set as frozen v1.

---

### A5 — `fake_registry` committed to Rust binary in Files to Modify + Phase 3 stub step
**Status: PARTIAL**

`test/helpers/fake_registry/`, `test/helpers/fake_fulcio/`, `test/helpers/fake_rekor/` are all in the Files to Modify table (plan lines 536-538) and Step 3.11 (lines 391-395) commits to a Rust axum binary with a tape-driven design. The design is now fully specified.

However, the R1 remediation asked for a dedicated stub step (Step 3.0 or pre-3.11) for the fake_registry infrastructure. No such step was added — the infrastructure is mentioned only in the narrative of Step 3.11. The builder must infer that the helpers must be stubbed before any Step 3.11 tests can compile.

**Remaining gap:** Add a Step 3.0 (or re-number as Step 3.0a) that explicitly tasks the builder with creating the `fake_registry`, `fake_fulcio`, and `fake_rekor` binary stubs (empty `main`, `Cargo.toml`, `axum` skeleton) before the other specification test steps.

---

### A6 — `SignErrorKind` + `VerifyErrorKind` variant lists enumerated
**Status: ADDRESSED**

ADR lines 398-488 enumerate all `SignErrorKind` and `VerifyErrorKind` variants with Rust doc comments, exit-code justifications, and explicit merger rationale. Step 1.4 and 1.5 reference the ADR section by heading. Step 3.9 (plan lines 351-373) tests every variant individually.

---

### A7 — `EMPTY_CONFIG_DIGEST` + `EMPTY_CONFIG_SIZE` constants + byte-precision test case
**Status: PARTIAL**

The ADR referrer manifest sample (lines 643-648) shows the exact values (`sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a`, size 2). Step 1.3 (plan line 170) lists `EMPTY_CONFIG` as a constant name but does not enumerate `EMPTY_CONFIG_DIGEST` and `EMPTY_CONFIG_SIZE` as separate named constants. Step 3.4 (plan lines 309-312) checks `artifactType` and `subject` but still says "config uses empty-config media type" without an explicit assertion that `config.digest == EMPTY_CONFIG_DIGEST` and `config.size == 2`.

**Remaining gap:** Update Step 1.3 to name the constants `EMPTY_CONFIG_DIGEST` and `EMPTY_CONFIG_SIZE` explicitly; update Step 3.4 to add a test case asserting `config.digest == EMPTY_CONFIG_DIGEST` and `config.size == 2`.

---

### A8 — `SignContext<'_>` / `VerifyContext<'_>` field sketches
**Status: UNRESOLVED**

Step 1.4 (plan line 174) declares `SignPipeline::run(ctx: SignContext<'_>)` and Step 1.5 (line 178) declares `VerifyPipeline::run(ctx: VerifyContext<'_>)`. Neither step includes a field sketch (comment table or struct outline) for what these context types carry. The builder still must infer the fields from the surrounding code descriptions.

**Remaining gap:** Add field sketches for `SignContext` and `VerifyContext` to Steps 1.4 and 1.5 as stated in the R1 remediation.

---

### A9 — `test_verify_json_success_shape`
**Status: PARTIAL** (subsumed by A1)

Same status as A1 — the verify `--format json` success fields are present in the acceptance table but not as a named bullet in Step 3.10.

---

### A10 — `tasks/mod.rs` confusion removed from Step 1.6
**Status: ADDRESSED**

Step 1.6 (plan lines 181-182) explicitly states: "the aggregator module file is `package_manager/tasks.rs` (NOT `tasks/mod.rs`)." The Files to Modify table (line 533) lists `package_manager/tasks.rs` — correct. The confusing `mod.rs` mention is absent.

---

## Summary of Findings

| Finding | Status |
|---------|--------|
| A1 | PARTIAL |
| A2 | PARTIAL |
| A3 | ADDRESSED |
| A4 | ADDRESSED |
| A5 | PARTIAL |
| A6 | ADDRESSED |
| A7 | PARTIAL |
| A8 | UNRESOLVED |
| A9 | PARTIAL (same as A1) |
| A10 | ADDRESSED |

**Addressed:** A3, A4, A6, A10 (4 of 10)
**Partial:** A1, A2, A5, A7, A9 (5 of 10; all have a clear remaining gap)
**Unresolved:** A8 (1 of 10)

## Actionable Findings Requiring Another Fix Pass

1. **A1/A9** — Step 3.10: add a sub-bullet asserting verify `--format json` success shape (`data.signer_identity`, `data.signer_issuer`, `data.rekor_log_index`, `data.signed_at`). Evidence: plan line 383 lists only error-envelope checks; acceptance table line 618 has the fields but they are not wired into a test step.

2. **A2** — Step 3.11: add four test bullets: (a) `fake_fulcio` returns 401 → exit 80, (b) `fake_fulcio` returns 403 → exit 78, (c) `fake_registry` returns 401 on blob/manifest PUT → exit 80, (d) CLI `--no-tty` with no-ambient environment → exit 77. Evidence: plan lines 390-395 do not mention these.

3. **A5** — Add Step 3.0 (or Step 3.0a) tasking the builder to stub `test/helpers/fake_registry/`, `fake_fulcio/`, `fake_rekor/` (empty binary skeletons) before the specification test steps. Evidence: plan line 391 describes these in Step 3.11 narrative only.

4. **A7** — Step 1.3: name the constants `EMPTY_CONFIG_DIGEST` and `EMPTY_CONFIG_SIZE`. Step 3.4: add assertion `config.digest == EMPTY_CONFIG_DIGEST && config.size == 2`. Evidence: plan line 170 says `EMPTY_CONFIG` (opaque); plan lines 309-312 omit digest/size assertions.

5. **A8** — Steps 1.4 and 1.5: add comment-table field sketches for `SignContext<'_>` and `VerifyContext<'_>`. Evidence: plan lines 174, 178 declare the type names but no fields.

## Verdict: PASS-WITH-ACTIONABLE

5 actionable findings remain (A1/A9 counted once, A2, A5, A7, A8). All are plan-text edits — no architectural rework required. The four fully-addressed findings (A3, A4, A6, A10) are correctly resolved.
