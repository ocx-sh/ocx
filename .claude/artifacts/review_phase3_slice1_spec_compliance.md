# Phase 3 Spec-Compliance Review — Slice 1 Stubs

**Commit:** f9e0601 (`refactor: stub OCI referrers signing + verify scaffolding`)  
**Base:** 2933eae  
**Focus:** spec-compliance, phase: post-stub  
**Tier:** max

---

## Verdict

**FAIL — 4 Block-tier findings, 2 Warn-tier findings**

The stubs are largely well-formed and the core error taxonomy, exit code assignments, and module layout are correct. However, there are four contract violations against the frozen C-S1-1 JSON envelope shape that must be fixed before Phase 3 tests can be written, plus one C-S1-3 seam omission.

---

## Actionable Findings

### BLOCK-1 — Envelope missing `command` and `exit_code` at top level (C-S1-1)

**File:** `crates/ocx_cli/src/error_envelope.rs:52-57`

The plan (Step 1.10) and ADR §JSON error envelope freeze the error envelope as:
```json
{ "schema_version": 1, "command": "package sign", "exit_code": 80, "error": { ... } }
```

The stub `ErrorEnvelope` struct is:
```rust
pub struct ErrorEnvelope<'a> {
    pub schema_version: u32,
    pub success: bool,      // ← WRONG: not in frozen ADR contract
    pub error: EnvelopeError<'a>,
    // MISSING: command: &'a str
    // MISSING: exit_code: u8
}
```

Two deviations:
1. `command: &'a str` is absent — the ADR and plan spec both have it at top level.
2. `exit_code: u8` is absent at the envelope root — the ADR has it at `$.exit_code`, not inside `$.error.exit_code`. The stub places it inside `EnvelopeError` (`pub exit_code: u8`), which is wrong: exit_code is a top-level field for programmatic dispatch without parsing the nested error object.
3. `success: bool` is a field introduced by the stub but absent from both the ADR and plan contract. The ADR uses `error` vs `data` presence as the success discriminator, not a boolean.

**Fix:** Align `ErrorEnvelope` with the frozen shape:
```rust
pub struct ErrorEnvelope<'a> {
    pub schema_version: u32,
    pub command: &'a str,
    pub exit_code: u8,
    pub error: EnvelopeError<'a>,
}
```
Remove `success: bool` from both `ErrorEnvelope` and `SuccessEnvelope`. Move `exit_code` out of `EnvelopeError` and into `ErrorEnvelope`. Add `command: &'a str` to both `ErrorEnvelope` and `SuccessEnvelope`.

---

### BLOCK-2 — Envelope `EnvelopeError` missing `detail` and `remediation` fields; `kind` is a raw `&str` instead of a typed enum (C-S1-1)

**File:** `crates/ocx_cli/src/error_envelope.rs:60-78`

The plan spec (Step 1.10 §EnvelopeError) defines:
```rust
pub struct EnvelopeError {
    kind: ErrorKind,                          // coarse category (serde enum)
    detail: Option<ErrorKindDetail>,          // narrow enum, omitted if None
    message: String,
    remediation: Option<String>,              // omitted if None
    context: BTreeMap<...>,
}
```

The stub has:
```rust
pub struct EnvelopeError<'a> {
    pub kind: &'a str,           // ← raw string; not the typed ErrorKind enum
    pub exit_code: u8,           // ← wrong nesting (see BLOCK-1)
    pub message: String,
    // MISSING: detail: Option<ErrorKindDetail>
    // MISSING: remediation: Option<String>
    pub context: BTreeMap<&'a str, String>,
}
```

`detail` and `remediation` are optional but must be present in the struct as `Option<_>` so Phase 3 tests can assert their values and consumers can rely on their presence as defined by the v1 stability contract.

`kind` as `&'a str` is not wrong if it will serialize identically, but it defeats the contract that tests can assert on typed variants. The plan explicitly says `ErrorKind` (coarse category enum) and `ErrorKindDetail` (narrow enum). At stub phase these can be placeholder enums with `unimplemented!()` variants, but they must be typed.

**Fix:** Add `detail: Option<&'a str>` (or a placeholder `ErrorKindDetail` enum) and `remediation: Option<String>` to `EnvelopeError`. Remove `exit_code` from `EnvelopeError` (it goes in `ErrorEnvelope` per BLOCK-1).

The `context: BTreeMap<&'a str, String>` vs `BTreeMap<String, serde_json::Value>` question is addressed in the Stub-Worker-Flagged Decisions section below — this is Warn-tier (see WARN-1).

---

### BLOCK-3 — `VerifyContext` missing `fulcio_url` injection seam (C-S1-3)

**File:** `crates/ocx_lib/src/oci/verify/pipeline.rs:30-51`

C-S1-3 (plan Step 1.5 context field table, quoted verbatim): `rekor_url: &str` is present. However `fulcio_url` is NOT listed for `VerifyContext` in the plan's field table — only `rekor_url` is required for verify per the plan.

Wait — re-checking the ADR. The ADR `context` field catalog (Spec A7) lists `fulcio_url` as `"sign only, after step 6 attempt"`. The plan Step 1.5 `VerifyContext` field table also lists only `rekor_url`, not `fulcio_url`.

**Corrected assessment:** `VerifyContext` correctly omits `fulcio_url` — it is a sign-path field only. The verify pipeline only contacts Rekor, not Fulcio. **This is NOT a finding.** Retract.

*(Self-correction: BLOCK-3 retracted — `VerifyContext` is correct as implemented.)*

---

### BLOCK-3 (renumbered) — `VerifyContextHolder` does not enforce the invariant that `context()` cannot be called before `build()` succeeds

**File:** `crates/ocx_lib/src/oci/verify/pipeline.rs:72-86`

The stub `VerifyContextHolder` stores `trust_root: TrustRoot` and `phantom: PhantomData<&'a ()>`, but the `context()` method is `unimplemented!()`. This is expected at stub phase. However the flagged decision (#4) asks whether the holder enforces the `None` trust-root invariant.

The signature `build(..., trust_root: Option<TrustRoot>) -> Result<VerifyContextHolder<'a>, VerifyErrorKind>` is correct: `None` means "use embedded production root" and a `Err(VerifyErrorKind::TrustRootUnavailable)` exits if the embedded root cannot be loaded. The holder then lends a `VerifyContext` that borrows `trust_root`. The stub structure is sound — the invariant that a `None` trust-root can never slip past `build()` is enforced by type: `VerifyContext.trust_root: &'a TrustRoot` is a non-optional reference, so `context()` cannot produce a context without a valid trust-root.

**Finding:** The stub `context()` body is `unimplemented!()` as required. The ownership pattern is structurally correct. **Not a block-tier finding.** The stub-worker's concern is resolved — classified as Deferred (Phase 5 implementation must not insert a `unwrap()` or `expect()` in the `context()` body).

---

### BLOCK-4 — `SuccessEnvelope` missing `command` and `exit_code` fields (C-S1-1)

**File:** `crates/ocx_cli/src/error_envelope.rs:84-99`

The plan Step 1.10 specifies:
```rust
pub struct SuccessEnvelope<'a, T: Serialize> {
    schema_version: u32,
    command: &'a str,
    exit_code: u8,  // always 0 on this path
    data: T,
}
```

The stub has:
```rust
pub struct SuccessEnvelope<'a, T: Serialize> {
    pub schema_version: u32,
    pub success: bool,    // ← wrong (not in contract; see BLOCK-1)
    pub data: &'a T,
    // MISSING: command: &'a str
    // MISSING: exit_code: u8
}
```

Same class of deviation as BLOCK-1. The success envelope must carry `command` and `exit_code` so machine consumers can unambiguously identify the command and parse the response without re-parsing the raw process exit code.

**Fix:** Mirror the error envelope top-level structure: add `command: &'a str` and `exit_code: u8`, remove `success: bool`.

---

### WARN-1 — `EnvelopeError.context` uses `BTreeMap<&'a str, String>` instead of `BTreeMap<String, serde_json::Value>` (C-S1-1 deferred variant concern)

**File:** `crates/ocx_cli/src/error_envelope.rs:77`

The plan (Step 1.10) specifies `context: BTreeMap<String, serde_json::Value>`. The stub uses `BTreeMap<&'a str, String>`. The stub worker's rationale was "avoid pulling serde_json into ocx_cli."

Checking `Cargo.toml` for `ocx_cli`: `serde_json` is already a dependency of `ocx_cli` (it's used in `api.rs` for JSON output). The avoidance rationale does not hold.

More importantly, the `context` field catalog (Spec A7) lists `bundle_digest: null` before step 13 — a `null` JSON value cannot be represented in `BTreeMap<&'a str, String>`. If the context must express `null` vs absent, `String` values cannot carry that distinction.

This is Warn-tier at stub phase because the stub body is `unimplemented!()` and the map will be populated in Phase 5. However, if Phase 3 tests assert on the serialized JSON context shape (they will — Spec A7 and A9 demand it), the wrong field type will require a Phase 5 reshaping that breaks tests written against the wrong type.

**Fix:** Change `context: BTreeMap<&'a str, String>` to `context: BTreeMap<String, serde_json::Value>` now. `serde_json` is already an `ocx_cli` dep.

---

### WARN-2 — `SignErrorKind::SigningPipelineInternal(String)` carries a `String` field, breaking source-chain discipline

**File:** `crates/ocx_lib/src/oci/sign/error.rs:104-105`

The ADR specifies: "Forced to be a leaf variant — no nested anyhow." The stub implements this as `SigningPipelineInternal(String)` with `#[error("signing pipeline internal error: {0}")]`. Carrying a `String` means `.to_string()` on an inner error would be called at the call site, erasing the source chain. The field should be `Box<dyn std::error::Error + Send + Sync>` with `#[source]`, or if it must be a pure leaf, the variant should carry no field at all and the outer `SignError.identifier` provides the context. As currently written, any implementation that does `SignErrorKind::SigningPipelineInternal(inner_err.to_string())` at a call site violates `quality-rust-errors.md` block-tier rule against `.to_string()` in `map_err()`.

This is Warn-tier at stub phase (no implementation body yet), but the type shape will guide implementers toward the wrong pattern.

**Fix:** Change to `SigningPipelineInternal(#[source] Box<dyn std::error::Error + Send + Sync>)` with `#[error("signing pipeline internal error: {0}")]`, or make it a unit variant with no payload if the intent is truly a catch-all with no upstream source. Check the ADR: "Catch-all for Fulcio/Rekor HTTP errors outside the codes above. Exit 1. Forced to be a leaf variant — no nested anyhow." — the ADR says "no nested anyhow," not "no source." A `Box<dyn Error + Send + Sync + 'static>` with `#[source]` preserves the chain without using anyhow. Recommend this.

---

## Stub-Worker-Flagged Decisions

### Decision 1 — `Error::Sign(Box<SignError>)` / `Error::Verify(Box<VerifyError>)` boxing

**Verdict: Correct.**

`crates/ocx_lib/src/error.rs` adds:
```rust
Sign(#[from] Box<crate::oci::sign::SignError>),
Verify(#[from] Box<crate::oci::verify::VerifyError>),
```

The `#[error(transparent)]` + `#[from]` on a `Box<T>` preserves the full error chain through the box. `source()` on the outer `Error::Sign` variant delegates to `Box<SignError>::source()` which in turn is `SignError::source()` → `SignErrorKind`. Chain walking in `classify_error` works correctly: `err.chain()` will surface `SignError`, which the classify function downcasts via `downcast_ref::<SignError>()` and delegates to `e.as_ref().classify()`. This is verified by reading `crates/ocx_lib/src/error.rs` lines 168-169 where `Self::Sign(e) => e.as_ref().classify()` is correctly implemented. The boxing approach for `clippy::result_large_err` is standard practice. Ergonomics: `From<Box<SignError>>` means callers must write `Box::new(sign_err).into()` or use `SignError::new(...).into()` with a `From<SignError> for Box<SignError>` blanket impl (already provided by the standard library). **No remediation needed.**

---

### Decision 2 — `ClassifyErrorKind` as a separate trait

**Verdict: Correct.**

`ClassifyErrorKind` is defined in `crates/ocx_lib/src/cli/classify.rs` as an infallible `fn exit_code(&self) -> ExitCode`. It is implemented on `SignErrorKind` and `VerifyErrorKind` with exhaustive `match` arms. The exhaustive match forces a compile error when new variants are added without updating the classification — exactly the invariant Architect F7 requires. Placement in `ocx_lib::cli::classify` is correct (not in `ocx_cli`, keeping the dependency direction lib → no circular dep). **No remediation needed.**

---

### Decision 3 — `ErrorEnvelope.context: BTreeMap<&str, String>` vs `serde_json::Value`

**Verdict: Fix required (see WARN-1 above, elevated to recommend-fix).**

`serde_json` is already a dep in `ocx_cli`. The `null` representation gap (e.g., `bundle_digest: null` before step 13 per Spec A7) cannot be expressed with `String` values. The worker's avoidance rationale does not apply. This is addressed in WARN-1.

---

### Decision 4 — `VerifyContextHolder` ownership pattern

**Verdict: Structurally sound.**

`VerifyContext::build(..., trust_root: Option<TrustRoot>) -> Result<VerifyContextHolder<'a>, VerifyErrorKind>` correctly encodes the invariant. `None` → embedded production root; `Some(root)` → injected test root. Either way, a successful `build()` produces a `VerifyContextHolder` that owns the resolved `TrustRoot`. The `context()` method then lends `VerifyContext` with `trust_root: &'a TrustRoot` — a non-optional reference that cannot be null. A `None` trust-root cannot slip through to the pipeline. The stub body is `unimplemented!()` as required. **No remediation needed at stub phase.** Phase 5 implementer must not add an `.unwrap()` or `.expect()` inside `context()` — flag this for the Phase 5 implementation reviewer.

---

## Deferred / Observations

1. **`VerifyErrorKind` has more variants than the ADR's canonical list** (`RekorLookupFailed`, `CertificateExpired`, `CertificateRevoked`, `TrustRootUnavailable`, `BundleNotFound`). The plan's Step 1.5 calls out the full list including these variants (`CertificateExpired`, `CertificateRevoked`, `TrustRootUnavailable`, `SignatureInvalid`, `BundleNotFound`, etc.). These are in the plan's canonical list (Step 1.5, quoted list). Cross-checking: the ADR variant inventory does NOT list `RekorLookupFailed`, `CertificateExpired`, `CertificateRevoked`, or `BundleNotFound` in the ADR §VerifyErrorKind block. The plan's Step 1.5 adds them as C-S1-2 canonical names. Since the plan supersedes the ADR for the concrete variant list (the ADR is the strategic document, the plan is the implementation spec), these are acceptable. **Deferred to human confirmation**: do `CertificateExpired`, `CertificateRevoked`, `RekorLookupFailed`, `BundleNotFound` need an exit-code test each? They are in the `VerifyErrorKind::exit_code()` exhaustive match and classified correctly (`DataError` and `RekorUnavailable` respectively), so the compile-gate applies. **No block finding.**

2. **`DEFAULT_FULCIO_URL` in `package_sign.rs` points to `https://fulcio.sigstore.dev` (without the `/api/v2/signingCert` path suffix)**. The ADR specifies the Fulcio URL as `https://fulcio.sigstore.dev/api/v2/signingCert`. The stub stores only the base URL — the path suffix is likely appended inside `fulcio.rs`. This is an observation for Phase 5: ensure the full path is used when making CSR POST requests, not just the base URL. Not a block finding at stub phase.

3. **`RateLimited.retry_after` type changed from `Option<Duration>` to `Option<u64>` in `ClientError`**. The ADR shows `retry_after: Option<Duration>`. The stub uses `Option<u64>` (seconds). `Duration` carries the same semantic with better type safety. This is a Warn-tier design smell but acceptable if the serialization contract is clear. Not blocking.

4. **`VerifyContext` has no `fulcio_url` field**. As noted above, the ADR and plan both specify this as sign-only. Correctly absent. Confirmed not a finding.

5. **`tasks/sign.rs` and `tasks/verify.rs` added to the `tasks.rs` aggregator** (not `tasks/mod.rs`). This matches the OCX convention enforced by the plan (Spec A10). Correctly done.

---

## Quality Gates Observed

- `cargo check --workspace`: **GREEN** at f9e0601 (confirmed — output shows `Finished dev profile [unoptimized + debuginfo] target(s) in 0.14s; cargo build (0 crates compiled)`).
- `cargo clippy`: Not run independently (heavy). `cargo check` is clean; the `#[allow(dead_code)]` suppressions on stub fields are appropriate and follow the stub worker's instructions.

---

## Contract Freeze Checklist Result

| Check | Status |
|---|---|
| C-S1-1: `ErrorEnvelope` nested `error` object | Partial — nested correctly, but missing `command`, `exit_code` at top level; has extra `success` field |
| C-S1-1: `error.context` nested under `error`, not flattened | Pass |
| C-S1-1: `SuccessEnvelope` has `command` and `exit_code` | Fail — both missing |
| C-S1-2: `SignErrorKind` variant names match ADR | Pass — all 8 variants present with exact names |
| C-S1-2: `VerifyErrorKind` variant names match plan canonical list | Pass — all variants present including extended set |
| C-S1-2: `IdentityMismatch` → ExitCode 77 | Pass |
| C-S1-2: `RekorUnavailable` → ExitCode 82 | Pass (both sign and verify paths) |
| C-S1-2: `ReferrersUnsupported` → ExitCode 83 | Pass |
| C-S1-3: `SignContext.fulcio_url` + `rekor_url` injection seams | Pass |
| C-S1-3: `VerifyContext.rekor_url` injection seam | Pass |
| C-S1-3: `VerifyContext.trust_root` from `VerifyContextHolder` | Pass |
| C-S1-4: No raw `--identity-token <VALUE>` flag | Pass — only `--identity-token-file`, `--identity-token-stdin`, and env `OCX_IDENTITY_TOKEN` documented |
| Three-layer error pattern for Sign and Verify | Pass |
| `#[non_exhaustive]` on error enums | Pass |
| `#[source]` on wrapping variants | Pass |
| `ExitCode::RekorUnavailable = 82` exists | Pass |
| `ExitCode::ReferrersUnsupported = 83` exists | Pass |
| `online_context()` accessor on `app::Context` | Pass |
| `tasks.rs` aggregator (not `tasks/mod.rs`) | Pass |
| All stub bodies are `unimplemented!()` | Pass |
| No new sigstore/ambient-id deps added | Pass |
| `cargo check --workspace` green | Pass |

