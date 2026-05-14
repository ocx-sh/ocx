# Phase 3 Architect Review — OCI Referrers Slice 1 Stubs

- **Commit:** `f9e0601`
- **Phase:** 3 (Verify Architecture) — post-stub, pre-Specify
- **Tier:** max (architect findings are first-class — RE-STUB is a real verdict)
- **Reviewer:** worker-architect (Opus)
- **Scope:** stub-only diff; **no** implementation bodies reviewed
- **ADR under review:** `.claude/artifacts/adr_oci_referrers_signing_v1.md`
- **Plan under review:** `.claude/state/plans/plan_slice1_sign_and_verify.md`

---

## Verdict

**ACCEPT-WITH-CONDITIONS**

The stubs faithfully realize the ADR's module boundaries, three-layer error pattern, `ClassifyErrorKind` split, injection seams (C-S1-3), `Signer` trait shape, exit-code taxonomy (C-S1-2), and three-tier directory layout (`oci/referrer/`, `oci/sign/`, `oci/verify/`). All four stub-worker-flagged decisions are architecturally sound.

The conditions are three fixes the Specify phase must apply before implementation begins:

1. **Block — ErrorEnvelope JSON shape diverges from ADR C-S1-1 frozen contract.** The stub's `{ schema_version, success, error }` and `{ schema_version, success, data }` shapes contradict the ADR's frozen `{ schema_version, command, exit_code, error|data }` shapes. This is a public contract violation, not a refactor; the `success: bool` field is not in the frozen envelope at all, and `command` + `exit_code` are required at the envelope root.
2. **Warn — `EnvelopeError.context` typed as `BTreeMap<&str, String>` silently drops semantics the ADR's context catalog defines as mixed-type (e.g., `bundle_digest: null`, numeric fields in Slice 2 extensions).** The ADR explicitly shows `"bundle_digest": null`, and consumers of the frozen v1 envelope will see the null/absent distinction matter.
3. **Warn — `VerifyContextHolder` with `Option<TrustRoot>` at `build()` is over-engineered** relative to OCX's existing patterns; a simpler Cow-based or explicit-constructor shape is cleaner and does not leak "None means embedded" into the type signature.

None of the three is load-bearing against the rest of the architecture — each can be corrected in a small, scoped change in the Specify phase or as a Phase 3 amendment before tests are written. RE-STUB is not required.

---

## Module Boundary Assessment

**Pass.**

New peer modules follow the OCX convention from `arch-principles.md` (no `mod.rs`, one-concept-per-file, named-file submodules):

- `oci/referrer.rs` + `oci/referrer/{capability,manifest,media_types}.rs`
- `oci/sign.rs` + `oci/sign/{bundle,error,fulcio,oidc,oidc_ambient,oidc_ambient_inline,oidc_browser,pipeline,rekor,signer}.rs`
- `oci/verify.rs` + `oci/verify/{error,identity,pipeline,trust_root}.rs`

Placement is correct per the "Where Features Land" table: OCI-layer concerns live in `ocx_lib/src/oci/`, package-manager task wrappers in `package_manager/tasks/sign.rs` and `tasks/verify.rs`, CLI commands in `ocx_cli/src/command/`. No subsystem boundary is violated; the referrer/sign/verify tree does not reach into `file_structure` or `package` directly — it composes through `OciTransport`, `Index`, and the three-layer error chain.

`oci/referrer/` as a peer of `oci/client/` and `oci/index/` is the right call — referrers and capability probing are cross-cutting across sign (push referrer) and verify (list referrers), so co-locating them under neither `sign` nor `verify` prevents a future coupling.

---

## Extension Seam Assessment

**Pass.**

Every ADR-mandated injection seam is present on the context struct, exposed as a field (not hidden behind a constructor-time default):

| Seam | ADR reference | Stub location |
|---|---|---|
| `fulcio_url` | C-S1-3 | `SignContext::fulcio_url: &'a str` |
| `rekor_url` (sign) | C-S1-3 | `SignContext::rekor_url: &'a str` |
| `rekor_url` (verify) | C-S1-3 | `VerifyContext::rekor_url: &'a str` |
| `trust_root` (verify) | C-S1-3 | `VerifyContext::trust_root: &'a TrustRoot` |
| `Signer` trait object | F2 | `SignContext::signer: &'a dyn Signer` |
| `TokenProvider` trait object | C-S1-4 | `SignContext::token_provider: &'a dyn TokenProvider` |
| `OciTransport` trait object | existing | both contexts: `transport: &'a dyn OciTransport` |
| `Index` | existing | both contexts: `index: &'a Index` |

Testability check: every external dependency is injectable at the pipeline entry point, so the fake_fulcio / fake_rekor helpers named in the plan can substitute without monkey-patching a global. The `TrustRoot::load_from_pem` constructor (verify/trust_root.rs:14) provides the test injection path the ADR requires.

F5 `Context::online_context()` accessor (`crates/ocx_cli/src/app/context.rs:169`) is present with the correct signature returning `(&Index, &Client)` and routing to `ExitCode::OfflineBlocked = 81` via `Error::OfflineMode`. Matches the F5 ADR resolution (single accessor, not separate sign/verify variants).

---

## Error Architecture Assessment

**Pass with one concern.**

Three-layer pattern is applied consistently:

- `Error::Sign(Box<SignError>)` / `Error::Verify(Box<VerifyError>)` at the top level (error.rs) — boxed per `clippy::result_large_err`
- `SignError { identifier, kind } / VerifyError { identifier, kind }` as context-bearing middle layer (both use `#[error("{identifier}: {kind}")]` and `#[source] kind`, matching the canonical pattern in `quality-rust-errors.md` §"Structured Error Chain Pattern")
- `SignErrorKind` / `VerifyErrorKind` as pure discriminant enums with `#[non_exhaustive]`, `#[derive(thiserror::Error)]`, and lowercase no-period `#[error("...")]` strings per C-GOOD-ERR

`ClassifyErrorKind` (leaf, non-`Option<ExitCode>` return) vs `ClassifyExitCode` (outer, `Option<ExitCode>` return) split at `crates/ocx_lib/src/cli/classify.rs:44,64` is an excellent design choice. The non-optional `exit_code()` on the leaf forces the match to be exhaustive, so adding a `SignErrorKind` variant without updating the classification becomes a compile error, eliminating the "silent drift" risk the ADR explicitly identifies.

Concern: The outer `Error::Sign` / `Error::Verify` classify dispatches to `self.as_ref().classify()` which resolves to `SignError::classify()` which returns `Some(self.kind.exit_code())`. That's correct — but `try_classify` in `crates/ocx_lib/src/cli/classify.rs:110` does not currently enumerate `SignError` or `VerifyError` in the `try_downcast!` ladder. The path today relies on `crate::Error` being downcast and then internally dispatching. Verify the integration test locked-in (see `fn lib_package_manager_find_failed_maps_to_not_found`) also covers the Sign/Verify path, or add a test in the Specify phase. Not a blocker for stubs; flagged for Phase 4.

Variant inventory matches the ADR with justified extensions:

- `VerifyErrorKind` additions beyond the original ADR inventory: `RekorLookupFailed`, `CertificateExpired`, `CertificateRevoked`, `TrustRootUnavailable`, `BundleNotFound`. Each maps to a distinct exit code and remediation — these are not over-engineered; they are the concrete set that falls out of implementing TUF + Rekor checks. Accept.
- `SignErrorKind` matches the ADR 1:1.

---

## OCX Convention Compliance

| Convention | ADR/Rule | Stub status |
|---|---|---|
| One concept per file | `arch-principles.md` | Pass |
| Named-file modules (no `mod.rs`) | `arch-principles.md`, `subsystem-package-manager.md` | Pass — aggregators at `oci/referrer.rs`, `oci/sign.rs`, `oci/verify.rs` |
| `#[non_exhaustive]` on error enums only (not on internal enums) | `arch-principles.md` | Pass — `ReferrersSupport` has no `#[non_exhaustive]`, error kinds do |
| Full type names (not `Os`/`Arch`) | `arch-principles.md` | Pass |
| Lowercase `#[error("...")]` | `quality-rust-errors.md` | Pass (acronyms preserved: "Fulcio", "Rekor", "OIDC", "SET", "TSA", "CSR") |
| Three-layer error model | `subsystem-package-manager.md` | Pass |
| Facade discipline | `subsystem-package-manager.md` | Pass — `PackageManager::sign_one` / `verify_one` only expose public methods; error wrapping via `_map_sign_error` / `_map_verify_error` free functions |
| `#[source]` on wrapping variants | `quality-rust-errors.md` | Pass |
| `clippy::result_large_err` remediation | `quality-rust.md` | Box-wrap at the outer enum boundary — correct |
| `OidcToken` redacts Debug | `quality-security.md` | Pass |
| ClientError expansion (Unauthorized / Forbidden / RateLimited / ServiceUnavailable / ReferrersUnsupported) | Codex finding #3 | Pass |

One observation: `error_envelope.rs` uses `BTreeMap<&'a str, String>` with lifetime-parameterized keys. The `&'a str` keys are workable only because the caller controls all key strings (they are `&'static str` or stack-borrowed). This bleeds a lifetime into the envelope struct — see the ErrorEnvelope concern below; switching to `serde_json::Value` would also erase the lifetime parameter, which is a secondary benefit.

---

## F-Decision Audit (F1–F9)

| # | Decision | Stub status |
|---|---|---|
| F1 | Fallback-tag referrers discovery test-locked behind a `#[cfg(test)]` assertion | Not applicable to stubs (belongs in Specify/Implement). Capability probe structure is in place (`ReferrersApiCapability::probe`). |
| F2 | `Signer` trait narrowed to `sign(target_digest) -> SignedBundle`, token provider as separate concern | Pass — `Signer::sign(&self, target_digest: &Digest) -> Result<SignedBundle, SignErrorKind>` at `sign/signer.rs`; `TokenProvider` at `sign/oidc.rs` |
| F3 | Service-URL injection seams on both contexts | Pass — `fulcio_url` / `rekor_url` on `SignContext`; `rekor_url` on `VerifyContext` |
| F4 | Exit-code taxonomy: `OfflineBlocked=81`, `RekorUnavailable=82`, `ReferrersUnsupported=83` | Pass — present in canonical `ExitCode` enum (confirmed via classify.rs test file: `RekorUnavailable`, `ReferrersUnsupported` used in `ClientError::classify`) |
| F5 | Single `online_context()` accessor on CLI `Context` | Pass — `crates/ocx_cli/src/app/context.rs:169` |
| F6 | Variant inventory in Sign/VerifyErrorKind | Pass — verified against ADR §"Variant inventory" |
| F7 | `ClassifyErrorKind` as separate trait from `ClassifyExitCode` | Pass — at `crates/ocx_lib/src/cli/classify.rs:44,64` |
| F8 | `--identity-token` precedence (file > stdin > env; raw flag rejected) | CLI parser stub present at `command/package_sign.rs` with mutually-exclusive `--identity-token-file` and `--identity-token-stdin` — correct |
| F9 (if any) | n/a | — |

All architect F-decisions are realized structurally. No RE-STUB required on this axis.

---

## ADR Fidelity

**Pass on all load-bearing items except the JSON envelope shape.**

- C-S1-2 (exit-code taxonomy) — realized in both `SignErrorKind::exit_code()` and `VerifyErrorKind::exit_code()`.
- C-S1-3 (injection seams) — realized on both contexts (see F3).
- C-S1-4 (token provider precedence) — `DispatchingTokenProvider { override_token, no_tty }` in `oidc.rs` plus the CLI mutex flags.
- Sigstore bundle v0.3 — `SIGSTORE_BUNDLE_V03 = "application/vnd.dev.sigstore.bundle.v0.3+json"` in `media_types.rs` matches.
- Empty config digest `sha256:44136fa3...` and size 2 — matches canonical OCI empty config constants.
- Referrer manifest shape (`schemaVersion`, `mediaType`, `artifactType`, `config`, `layers`, `subject`) — `ReferrerManifest` struct matches the ADR example.

**The one miss: C-S1-1 JSON envelope frozen shape.** The ADR (§"JSON error envelope") is unambiguous:

> Top-level keys are strictly `schema_version`, `command`, `exit_code`, `error` (error path) or `schema_version`, `command`, `exit_code`, `data` (success path). No flattening...

The stub's `ErrorEnvelope` has `{ schema_version, success, error }` and moves `exit_code` *inside* `error`. The stub's `SuccessEnvelope` has `{ schema_version, success, data }` and omits both `command` and `exit_code`. This is not a minor naming drift — `success: bool` is a field that does not exist in the frozen v1 contract at all, and `command` is promised as always-present. Any consumer that already read the ADR and wrote to the frozen shape will fail to parse what the stub emits. Fix in Specify phase.

---

## Stub-Worker-Flagged Decisions — Verdicts

### 1. `Error::Sign(Box<SignError>)` / `Error::Verify(Box<VerifyError>)` boxing

**ACCEPT.**

`SignError` carries a `SignErrorKind` which includes variants like `OidcPreCheckFailed { reason: String }` and `SigningPipelineInternal(String)`, plus an `Identifier`. Unboxed, each `Result<T, Error>` would grow to carry the largest `Error` variant. Box-wrapping the infrequent Sign / Verify variants keeps the common `Result<T, Error>` cheap and satisfies `clippy::result_large_err` without poisoning the hot path. This is the canonical pattern for large-variant error enums.

### 2. `ClassifyErrorKind` separate from `ClassifyExitCode`

**ACCEPT — architecturally strong.**

The split enforces two distinct contracts:

- `ClassifyErrorKind::exit_code() -> ExitCode` (infallible, always present): every leaf kind **must** map to an exit code.
- `ClassifyExitCode::classify() -> Option<ExitCode>` (fallible, may delegate): outer wrapping errors can return `None` to let the chain walker continue via `source()`.

This keeps the `classify_error` chain walker simple and eliminates the "silent drift" failure mode the ADR explicitly identifies in §"Why a trait, not a free function per kind." Adding a variant to `SignErrorKind` is a compile error in the `match` inside `ClassifyErrorKind::exit_code()` — the match must list every variant. That is the most powerful compile-time lock the design can buy.

### 3. `ErrorEnvelope.context: BTreeMap<&str, String>` vs `serde_json::Value`

**CONDITIONAL — recommend `BTreeMap<String, serde_json::Value>` or equivalent.**

The current `BTreeMap<&'a str, String>` type has three problems vs the ADR's context catalog:

1. **Null vs absent.** The ADR example (§"JSON error envelope") shows `"bundle_digest": null` as a valid state (present, but not yet computed — distinct from absent). `BTreeMap<_, String>` cannot represent null values; `#[serde(skip_serializing_if = "BTreeMap::is_empty")]` on the whole map does not help per-key. Using `serde_json::Value` lets `Value::Null` serialize correctly.
2. **Mixed value types.** Slice 2's additive fields (e.g., `retry_after: u64`, numeric `log_index` in success envelopes per §"Frozen v1 success contract") cannot fit `String` without stringifying, which the ADR comment claims is acceptable ("a port is rendered as its decimal string") but the success envelope counterexample breaks this — `log_index: 98765432` is clearly a JSON number in the ADR example, not a string.
3. **Lifetime leakage.** `&'a str` keys force a lifetime parameter on `ErrorEnvelope` that propagates to every call site.

`ocx_cli` already transitively depends on `serde_json` through `serde` + the OCI stack, so pulling the name in is effectively free. The rationale in the doc comment ("avoid pulling `serde_json` into `ocx_cli`") is not load-bearing.

Minimum acceptable fix: `BTreeMap<&'static str, serde_json::Value>` (or `&'static str` keys since they are always literal constants). Preferable: `BTreeMap<String, serde_json::Value>` to erase the lifetime param entirely.

### 4. `VerifyContextHolder` borrow-owning pattern with `Option<TrustRoot>`

**CONDITIONAL — recommend Cow or explicit constructors.**

The current shape:

```rust
VerifyContext::build(..., trust_root: Option<TrustRoot>, ...) -> Result<VerifyContextHolder<'a>, VerifyErrorKind>
```

Two complaints:

1. **`Option<TrustRoot>` leaks "None means embedded" into the type signature.** The caller must read a doc comment to know that `None` triggers `TrustRoot::load_embedded()`. Explicit named constructors (`with_embedded`, `with_injected_root`) or two call sites with different type signatures make intent obvious at the call site.
2. **`VerifyContextHolder` with `PhantomData<&'a ()>` is over-engineered.** The holder exists only to own the `TrustRoot` while the `VerifyContext` borrows it. `Cow<'a, TrustRoot>` achieves the same goal without a second struct, and without leaking `PhantomData` into the API surface.

Concrete alternatives (pick one in Specify):

- **Explicit constructors**: `VerifyContext::with_embedded_trust_root(...)` and `VerifyContext::with_trust_root(root, ...)` — simplest, zero lifetime games, no holder.
- **Cow-based**: `trust_root: Cow<'a, TrustRoot>` on `VerifyContext` directly, no holder at all.
- **Caller-owned**: CLI resolves the root upstream (`let root = TrustRoot::load_embedded()?;`) and always passes `&TrustRoot` — pushes the "which root" decision into the CLI layer where it naturally lives.

The third option matches OCX's pattern elsewhere: upstream code owns its resources, pipeline code borrows. Given the ADR explicitly notes test-vs-production is the only branch, the third option also aligns with "make the CLI layer resolve the policy question so the pipeline stays pure." But any of the three is strictly better than the current holder + `PhantomData` + `Option<TrustRoot>` approach. Non-blocker — this is an API-quality improvement, not a correctness issue.

---

## Actionable Findings

### Block (must fix before Specify writes tests)

| Finding | File:line | Fix |
|---|---|---|
| `ErrorEnvelope` / `SuccessEnvelope` JSON shape diverges from ADR C-S1-1 frozen contract: replace `success: bool` with `command: &'a str` + `exit_code: u8` at the envelope root; move `exit_code` out of the inner `error` object (or keep a mirror, but the root is mandatory); add optional `detail: Option<&'a str>` and `remediation: Option<&'a str>` fields on `EnvelopeError` per the ADR's "frozen v1" clause | `crates/ocx_cli/src/error_envelope.rs:53-89` | Rewrite the struct shapes to match the frozen shape; Specify-phase JSON golden tests will catch regressions |

### Warn (should fix before Implement)

| Finding | File:line | Fix |
|---|---|---|
| `EnvelopeError.context: BTreeMap<&str, String>` cannot represent null values or numeric fields as shown in the ADR examples (`"bundle_digest": null`, `"log_index": 98765432`) | `crates/ocx_cli/src/error_envelope.rs:77` | Switch to `BTreeMap<&'static str, serde_json::Value>` or `BTreeMap<String, serde_json::Value>`; `ocx_cli` already depends on `serde_json` transitively |
| `VerifyContextHolder` + `Option<TrustRoot>` + `PhantomData<&'a ()>` is an over-engineered borrow-owning pattern | `crates/ocx_lib/src/oci/verify/pipeline.rs:62-94` | Prefer one of: explicit `VerifyContext::with_embedded_trust_root` / `with_trust_root`, or `Cow<'a, TrustRoot>` on `VerifyContext` directly, or CLI-layer root resolution with always-`&TrustRoot` |
| `classify_error` downcast ladder does not include `SignError` / `VerifyError` — dispatch relies on outer `crate::Error` downcast + internal delegation only; add explicit downcast entries so an anyhow-wrapped `SignError` (not going through `crate::Error`) still classifies | `crates/ocx_lib/src/cli/classify.rs:110-137` | Add `try_downcast!(crate::oci::sign::SignError);` and `try_downcast!(crate::oci::verify::VerifyError);` to the dispatch ladder |

### Suggest (may fix)

| Finding | File:line | Fix |
|---|---|---|
| `ReferrerManifest::to_canonical_json()` is a module-level stub — if the Sigstore bundle signing path needs canonical JSON, the canonicalization library choice (e.g., `serde_jcs` vs hand-rolled) is an architectural decision worth recording in the plan before Implement starts | `crates/ocx_lib/src/oci/referrer/manifest.rs` | Plan an entry in Specify / Implement for canonical-JSON library choice |
| `BrowserOauthProvider` is a scaffolding stub — confirm in the plan whether Slice 1 actually ships this path or defers to CI-only (ambient detection). Current stub suggests shipping; ADR suggests CI-only + `--no-tty` kill switch | `crates/ocx_lib/src/oci/sign/oidc_browser.rs` | Clarify Slice 1 scope in plan |
| `SigningPipelineInternal(String)` holds a stringified error, which destroys the source chain per `quality-rust-errors.md` — acceptable as a catch-all but consider `SigningPipelineInternal(#[source] Box<dyn std::error::Error + Send + Sync>)` | `crates/ocx_lib/src/oci/sign/error.rs:105` | Consider in Implement phase; not a stub-level concern |

---

## Accepted Risks / Deferred Observations

- **Rekor v2 + TSA path.** `RekorSetAbsentTsaPresent` is encoded as a distinct variant with exit 82. The ADR defers Rekor v2 until sigstore-rs ships it. Stubs correctly carry the marker; no action until sigstore-rs 0.14+.
- **`#[allow(dead_code)]` markers on stub fields.** Acceptable in Phase 1 stubs; will be consumed in Phase 5 implementation. `cargo check` clean.
- **`--fulcio-url` / `--rekor-url` exposed as CLI flags** (`command/package_sign.rs`) — beyond what the plan strictly required, but consistent with `oras --distribution-spec` precedent noted in `research_verify_cli_patterns.md`. Accept; gives sysadmins an escape hatch.
- **Pipeline state-machine steps not enumerated in code comments.** Sign pipeline doc comment says "15-step" but the stub body is `unimplemented!()` with no step enumeration. The ADR has the list; Implement phase will materialize the steps. Not a stub issue.
- **`BundleBuilder` has no `build_from_signer` convenience** — `SignPipeline` will need to assemble fields by hand. Accept as Implement-phase detail.

---

## Summary for Orchestrator

**Verdict:** ACCEPT-WITH-CONDITIONS.

**Top 3 findings:**

1. **[BLOCK] `error_envelope.rs` shape contradicts ADR C-S1-1 frozen envelope.** Root-level keys must be `{ schema_version, command, exit_code, error }` (error) or `{ schema_version, command, exit_code, data }` (success). Stub currently has `{ schema_version, success, error|data }` with `exit_code` nested inside `error`. Fix required before Specify writes golden JSON tests. `crates/ocx_cli/src/error_envelope.rs:53-89`.
2. **[WARN] `EnvelopeError.context` typed as `BTreeMap<&str, String>`** cannot represent the ADR's explicit null value (`"bundle_digest": null`) or Slice 2's numeric fields (`"log_index": 98765432` in the success envelope). Recommend `BTreeMap<&'static str, serde_json::Value>`. `crates/ocx_cli/src/error_envelope.rs:77`.
3. **[WARN] `VerifyContextHolder` + `Option<TrustRoot>` + `PhantomData<&'a ()>` is over-engineered.** A plain `Cow<'a, TrustRoot>` on `VerifyContext`, or two explicit named constructors, are cleaner. `crates/ocx_lib/src/oci/verify/pipeline.rs:62-94`.

All four stub-worker-flagged decisions have verdicts:

- Boxed `Error::Sign` / `Error::Verify` — **Accept**.
- `ClassifyErrorKind` / `ClassifyExitCode` split — **Accept** (exemplary design).
- `ErrorEnvelope.context` type — **Conditional** (see finding #2).
- `VerifyContextHolder` — **Conditional** (see finding #3).

No RE-STUB needed; all conditions are fixable in Specify as targeted amendments without invalidating the module structure, trait signatures, or error taxonomy.
