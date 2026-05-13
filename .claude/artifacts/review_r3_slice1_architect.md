# Architecture Review R3 — slice-1 referrers sign + verify (`feat/oci-referrers-sign-verify`)

**Status:** Phase 5a/5b stub scaffold; `SignPipeline::run`, `VerifyPipeline::run`, Fulcio, Rekor, Bundle, Ambient, browser providers, `Signer::sign`, `sign_one`, `verify_one`, `PackageManager::sign_one/verify_one`, and `KeylessSigner::sign` are still `unimplemented!()`. CLI entry points reach the lib but short-circuit (`SignContext` is constructed at panic in tests; the live `package_sign::execute` panics at `unimplemented!("ocx package sign: Phase 5c …")` after the override-token resolution; `verify::execute` short-circuits with `VerifyErrorKind::TrustRootUnavailable`).

**Scope:** branch tip vs `main` (9 commits, 8146 insertions); read-only adversarial pass focused on structural / dependency-direction / future-door concerns only. Style + line-level nits and ADR-spec faithfulness deferred to other reviewers. Findings already addressed in `review_pr-87_architecture.md` are not re-raised (SSRF moved into `ocx_lib::oci::sign::endpoint`, `TokenProvider` impls now exist in `oidc.rs`, `IdentityTokenFilePermissive` redacts path in `Display`).

## Verdict

**Conditionally pass with Block-tier deferral.** The Block finding (B1) is a structural deferral that the prior architect round flagged as `Suggest #3` — it is now hardening into a one-way door at the `oci::Client` boundary and must be resolved by an ADR amendment **before** any Phase 5c PR lands. Slice-1 itself stays sound: traits are minimal, kind enums are exhaustively classified, the JSON envelope is golden-tested, `referrer/` lives correctly under `oci/`, and the verify pipeline borrows `&TrustRoot` instead of owning a global. Three High and three Warn findings call out structural drift that should be fixed before flipping the stubs.

## Count by tier

- **Block:** 1 (one-way door at the OCI client boundary)
- **High:** 3 (boundary-leak of `endpoint` from `sign` into `verify`; `AmbientProvider::detect` static-trait dead-end; `DispatchingTokenProvider::new` signature too narrow for the dispatch state machine it claims to own)
- **Warn:** 3 (`ReferrerManifest::to_canonical_json` returns `SignErrorKind` — purpose-cross; `online_context` accessor leaks `&Client` shape past the facade; `pub(crate)` overuse on `oci::sign`/`oci::verify` modules contradicts `quality-rust.md` "warn-tier")
- **Suggest:** 3 (`Signer` + `TokenProvider` split honest assessment; capability cache layout; verify's `--rekor-url` flag without `--fulcio-url` asymmetry)
- **Deferred (ADR amendment required):** 2 (B1 plus the `url::Url` vs `&str` injection-seam type from R1)

## SOLID / boundary findings

### B1 (Block, deferred) — `oci::Client::transport()` accessor is a one-way door not locked by ADR

`crates/ocx_cli/src/command/package_sign.rs:132-140` documents the Phase 5c blocker: `SignPipeline::run` needs `&dyn OciTransport`, but `oci::Client` keeps transport private. R2 deferred this. As of branch tip the same comment is still there, the same `unimplemented!()` is emitted, **and `SignContext::transport: &'a dyn OciTransport`** is now a public field of a `pub struct` in `oci::sign::pipeline` (`crates/ocx_lib/src/oci/sign/pipeline.rs:34-57`). That public field locks the shape: whatever Phase 5c does to wire it must produce a `&dyn OciTransport` — meaning either:

1. `oci::Client` grows a `pub fn transport(&self) -> &dyn OciTransport` accessor (the entire transport abstraction is now part of `ocx_lib`'s public surface — Block for OCP/ISP: every internal transport tweak becomes semver-relevant), **or**
2. `SignContext::new_from_client(&Client)` lives on `oci::sign` (good, but `SignContext`'s public fields must then go `pub(super)` and the type becomes opaque — that's a fork from today's shape that breaks the tests in `pipeline.rs:142-194`), **or**
3. `SignPipeline::run` is rewritten to take `&Client` directly (best: keeps transport private, but turns `SignContext` into a builder/options struct rather than a context — the C-S1-3 "transport seam" injected by tests then has to flow through a different surface).

This decision **must** be locked in `adr_oci_referrers_signing_v1.md` (§"Context injection") before the first Phase 5c PR. Today's silent path is option 1 by accretion, which is the worst architectural outcome — once a `Client::transport()` accessor exists the only way to remove it is a breaking change to `ocx_lib`.

**Fix:** ADR amendment selecting option 2 or 3, plus an `#[allow(dead_code)]` removed when wiring lands. The product-context (Differentiator #9) asserts keyless Sigstore via OCI Referrers is a **product-level differentiator** — burying the seam under a transport accessor that future signers (KMS, HSM) also inherit pollutes the boundary right at the differentiator's edge.

**Why Block, not High:** every Phase 5c PR will exercise this; the choice is one-way; deferring past the first wiring PR locks in whichever lands first.

### F1 (High) — `oci::verify::error` reaches into `oci::sign::endpoint` for `UrlRejection`

`crates/ocx_lib/src/oci/verify/error.rs:11` and `:141` import `UrlRejection` from `oci::sign::endpoint`. `verify` then exposes `VerifyErrorKind::InvalidEndpointUrl { reason: UrlRejection, … }` — a verify-side error variant carrying a sign-side type. This is the exact subsystem-cohesion violation R2 introduced when it moved the SSRF validator into `ocx_lib`: the validator landed under `sign`, but `verify` also calls it (`crates/ocx_cli/src/command/verify.rs:29,76`) and now imports `UrlRejection` cross-subsystem.

Concretely: `verify`'s public error API has a structural dependency on the `sign` subsystem. Any future signer that wants to move endpoint validation out of `sign` (KMS signers do not need Fulcio URL validation) breaks `verify`. The two subsystems are supposed to be **symmetric peers** under `oci/`, not parent–child.

**Fix:** lift `endpoint` from `oci::sign::endpoint` to `oci::endpoint` (peer of `sign/` and `verify/`). It is already neutral — the helper validates *any* Sigstore endpoint URL and the doc comment at `endpoint.rs:13-17` already pitches it as serving "any future library consumer." Moving it costs one rename and lets both subsystems import a peer module instead of one reaching into the other's private surface. `SignErrorKind::InvalidEndpointUrl` and `VerifyErrorKind::InvalidEndpointUrl` each keep their own variant (good — already done) but import the rejection from a neutral location.

This also unblocks Slice 2 cleanly: when SBOM verify, attestation verify, or third-party trust-root verifiers land, they all need this validator and none of them belong under `sign`.

### F2 (High) — `AmbientProvider::detect` is a static-trait method, blocking heterogeneous dispatch

`crates/ocx_lib/src/oci/sign/oidc.rs:80-86`:

```rust
pub trait AmbientProvider: Send + Sync {
    fn detect() -> Option<Box<dyn TokenProvider>>
    where
        Self: Sized;
}
```

R2 mentioned this as Suggest #2. It is now hardening: `oidc_ambient.rs` and `oidc_ambient_inline.rs` each implement `detect()` as an associated function with no instance state, the dispatch chain documented in `oidc.rs:6-23` claims to walk providers in order, and `DispatchingTokenProvider::new` (line 109-114) takes only `(override_token, no_tty)` — there is no place to register additional ambient providers without editing the dispatcher's body. That is OCP-violating: a v2 GitHub Enterprise OIDC provider, a third-party trust-root SDK, or a Bazel-rule ambient provider must edit `oidc.rs` itself instead of registering an impl.

The static method also forces every call site to know the concrete type, so the dispatcher cannot iterate `Vec<Box<dyn AmbientProvider>>` — heterogeneous detection is impossible. It is genuinely a `fn() → Option<Box<dyn TokenProvider>>` constructor, not a trait method; calling it a trait creates the illusion of polymorphism that does not exist.

**Fix (pick one before Phase 5 fills the body):**

- **Instance method**: `fn detect(&self) -> Option<Box<dyn TokenProvider>>`. Dispatcher holds `Vec<Box<dyn AmbientProvider>>`; new providers register without touching the dispatcher. Cost: a tiny `pub struct AmbientIdProvider;` instantiation at call sites.
- **Plain free function**: drop the trait; have `oidc_ambient::detect()` and `oidc_ambient_inline::detect()` as module functions; the dispatcher hard-codes the order. Honest about the current state of the design (the chain is fixed at compile time) and removes the trait illusion.

Either is fine. Keeping the static-trait shape closes a future door (third-party providers) for zero current gain.

### F3 (High) — `DispatchingTokenProvider::new(override_token, no_tty)` ignores the dispatch state-machine inputs

`crates/ocx_lib/src/oci/sign/oidc.rs:109-114`:

```rust
pub fn new(override_token: Option<Zeroizing<String>>, no_tty: bool) -> Self {
    Self { override_token, no_tty }
}
```

The doc comment one screen up (`oidc.rs:88-100`) says the dispatcher implements the ADR S1-C state machine: override → ambient chain → browser. But the constructor takes only override + no_tty, not the ambient chain, not the browser provider. R1 flagged the same problem (`adr_oci_referrers_signing_v1.md` Architect F5 + Codex finding #2). R2 fix was to "construct from CLI flags and pass to context" — but the dispatcher still cannot be configured with the providers it claims to own.

Either:
- The dispatcher is a thin shim and the ambient/browser providers are hard-coded inside `acquire()` (then the comment is wrong — there is no state machine, only a fixed chain), **or**
- The dispatcher actually owns the chain and `new()` must take `(override_token, ambient_providers: Vec<Box<dyn AmbientProvider>>, browser: Option<Box<dyn TokenProvider>>, no_tty: bool)`.

The current shape advertises the second contract while delivering the first. Phase 5c will solve this one of two ways and either choice forecloses the other.

**Fix:** rewrite the constructor signature now (Phase 5b — before bodies fill in), even as a stub. Type-system-encoded contract beats prose at `oidc.rs:88-100`.

## Dependency-direction findings

### F4 (Warn) — `ReferrerManifest::to_canonical_json -> Result<_, SignErrorKind>` couples a neutral OCI primitive to sign-side errors

`crates/ocx_lib/src/oci/referrer/manifest.rs:62`:

```rust
pub fn to_canonical_json(&self) -> Result<Vec<u8>, SignErrorKind> {
```

`ReferrerManifest` is a primitive — it carries an `application/vnd.oci.image.manifest.v1+json` with `subject`, `artifactType`, `layers`, `config`. It is *not* sign-specific: Slice 2 SBOMs, attestations, and third-party signature formats all use it. Returning `SignErrorKind` ties this neutral type to the sign subsystem's failure taxonomy — verify-side discovery (`oci::verify::pipeline`) and Slice 2 SBOM discovery cannot consume this without round-tripping through a sign-side variant they have no business mentioning.

**Fix:** introduce `crate::oci::referrer::error::ReferrerError` (a single variant `SerializationFailed(serde_json::Error)` is enough today) and have both `SignErrorKind::Internal` and `VerifyErrorKind::Internal` carry it via `#[source]`. Or, more honestly, return `Result<Vec<u8>, serde_json::Error>` and let each consumer wrap as appropriate — there is no value-add in OCX's own typed error here since `to_vec` is the operation.

### F5 (Warn) — `Context::online_context()` returns `&Client` shape; better to be `&dyn OciTransport`

`crates/ocx_cli/src/app/context.rs:230-233` returns `Result<(&Index, &Client)>`. This conflates two concerns: index resolution (no auth) and registry transport (auth). Sign and verify pipelines do not need `&Client`; they need `&dyn OciTransport`. Exposing `&Client` at the CLI seam means **every** future caller of `online_context` (verify, sign, future trust-root TUF refresh, future attestation push) is now coupled to the concrete `Client` type — exactly what `OciTransport` exists to prevent.

The accessor exists for B1's resolution path: a future `online_context` may need to return `(&Index, &dyn OciTransport)` once `Client::transport()` lands. Locking that today keeps the public surface narrower.

**Fix:** when B1 is resolved, change `online_context` to return what the sign/verify contexts actually consume. If `SignContext` ends up taking `&Client` (option 3 in B1), the accessor is fine as-is. The two decisions are coupled — resolve them together in the same ADR amendment.

### F6 (Warn) — `pub(crate)` modules on `oci::sign::{oidc, oidc_ambient, oidc_ambient_inline, oidc_browser, pipeline}` and `oci::verify::{pipeline, trust_root}`

`crates/ocx_lib/src/oci/sign.rs:41-50` and `oci/verify.rs:23-27` mark these `pub(crate)`. `quality-rust.md` warn-tier explicitly calls this out: "control visibility through module nesting, not path qualifiers." Either:

- They are genuinely internal (only `tasks/sign.rs` and `tasks/verify.rs` consume them) → make them private (drop `pub(crate)`) and move the `pub use` from sign.rs/verify.rs.
- They are part of the lib's public API (the ADR §"New crate / module shape" lists them under `oci/sign/` so the test fixtures and integration tests can hit them) → make them `pub` and accept the API surface, then write the migration story.

The current shape is "we'd like them to be private but the CLI / tests need them" — that's the design smell `quality-rust.md` is warning against. Fix is small: either pick `pub` (and lock to public-API stability) or `pub(super)` and force tests into module-nested locations.

## ADR realization gaps (architectural, not implementation)

### G1 — Capability cache at `~/.ocx/state/referrers/<registry>.json` (ADR §"Capability cache contract")

ADR says `~/.ocx/blobs/{registry}/.capabilities.json`. Implementation (`capability.rs:195-200` and the prior reviewer's note in `review_pr-87_architecture.md`) uses `~/.ocx/state/referrers/<registry>.json`. The implementation choice is better — `blobs/` is the content-addressed store and adding a `.capabilities.json` file mixes concerns — but the ADR has not been amended. Three-store architecture (blobs / layers / packages / symlinks) does not include a `state/` tier; this is **a new fourth tier** introduced silently. That is a product-architecture decision worth recording in `arch-principles.md` and the user-guide.

**Fix:** amend `adr_oci_referrers_signing_v1.md` §"Capability cache contract" to record the actual path. Add `state/` to the three-store table in `arch-principles.md` ("Key Concepts") and `subsystem-file-structure.md`. Not blocking the slice but the storage layout shouldn't drift silently.

### G2 — `referrer/` lives under `oci/` (correct, **but not peer to `sign/verify`** as ADR implies)

ADR module map shows `oci/referrer/` peer to `oci/sign/` and `oci/verify/`. Implementation matches. **Adversarial probe:** `sign/pipeline.rs` and `verify/pipeline.rs` both need to construct `ReferrerManifest` and probe `ReferrersApiCapability`. Today, that means `sign` → `referrer` and `verify` → `referrer`. The `referrer` module is genuinely a peer (no circular dep: it doesn't import from `sign` or `verify` — verified via `referrer/manifest.rs` only importing `SignErrorKind` for F4 above, which the F4 fix removes).

Once F4 is fixed, `referrer/` is correctly peer-positioned. **No action needed** beyond F4.

### G3 — `Signer` trait is `&dyn Signer` injected into `SignContext` (ADR §"`Signer` trait abstraction")

ADR `Signer::sign(&self, target_digest: &Digest) -> Result<SignedBundle, SignError>`. Impl: `Result<SignedBundle, SignErrorKind>` (`signer.rs:27`). The kind-only return is a deliberate downgrade — the pipeline knows the identifier and re-wraps. R2 review didn't flag this; it's defensible. Worth recording in the ADR amendment log nonetheless.

## Future-door analysis

| Slice 2 requirement | Current shape blocks? | Notes |
|---|---|---|
| Rekor v2 client + TSA verification | No, but `VerifyErrorKind::RekorSetAbsentTsaPresent` is the only forward-compat hook. When sigstore-rs 0.14 lands a v2 client, the variant remains correct; the pipeline branches inside `verify/pipeline.rs`. | Good. |
| TSA-only mode (no Rekor) | Partially. Verify pipeline assumes Rekor SET as primary anchor (`VerifyErrorKind::RekorSetMalformed` is a DataError → terminal); a "TSA-only" config path would need to bypass that. | Block when v2 is wired — re-evaluate the SET-required assumption. |
| Trust-root rotation | Yes (good). `TrustRoot::load_embedded` and `load_from_pem` are split; rotation lives in `load_embedded`. | Good. |
| Third-party trust roots (private CA, BYO TUF) | **Yes today**, because `VerifyContext::trust_root: &'a TrustRoot` is concrete, not `&'a dyn TrustRoot`. A private-CA `TrustRoot` needs to be the same type. | Acceptable: `TrustRoot::der_certs: Vec<Vec<u8>>` is data-shaped, not behavior-shaped — extending is additive. Reconfirm before Slice 2. |
| HSM/KMS signing | Yes (the `Signer` trait is the seam). New signer impl alongside `KeylessSigner`; the `TokenProvider` split (per ADR rationale at lines 354-356) means KMS signers do **not** carry an `Option<TokenProvider>`. Good. | Validated. |
| Multi-platform sign/verify | Partial. `SignContext::platform: &Platform` is single. Multi-platform = iterate. Matches the ADR ("multi-platform sign is iteration over this flow"). | Reconfirm when `SignContext` becomes a builder per B1. |
| SBOM discovery + attest read | **F4 blocks this today.** Once F4 is fixed, the `referrer/` primitive is reusable. | Tied to F4. |
| Signer-side telemetry (`signer_kind()`) | Wired via `Signer::signer_kind()` returning `&'static str` (`signer.rs:33,65`). | Good. |

## Trade-off honesty: `Signer` + `TokenProvider` split

**Claim from ADR §"`Signer` trait abstraction":** two responsibilities, different lifetimes (token = 10 min; signer identity = long-lived).

**Honest assessment given Slice 1 only ships `KeylessSigner`:** the lifetime argument is real but the *current* code has exactly one signer impl using exactly one token-acquisition flow. The split today is one-impl-on-each-side — that's the "speculative second-caller" smell `quality-core.md` YAGNI warns against. Counter-argument: introducing a second signer (KMS) later is exactly the v2 case the ADR cites, and adding a trait there is more disruptive than carving it out now (the pipeline signature changes either way). Since both `Signer` and `TokenProvider` already have multiple impls scaffolded (`FileTokenProvider`, `StdinTokenProvider`, `EnvTokenProvider` + ambient + browser + dispatching), the duplication is in token-provider land, not signer land — *and* the token-provider split is well-justified by the C-S1-4 precedence chain.

**Verdict:** keep both. The split survives the YAGNI test because (a) `TokenProvider` already has 5 stub impls so duplication is genuine, and (b) the test scaffolding in `pipeline.rs:142-194` already exercises the seam.

## Deferred items (need ADR amendment)

1. **B1: Client transport accessor pattern.** Choose between (a) `Client::transport()` accessor, (b) `SignContext::new_from_client(&Client)`, or (c) `SignPipeline::run(&Client, options)`. Lock in `adr_oci_referrers_signing_v1.md` §"Context injection" before any Phase 5c PR. This already cropped up in R2; this round it has hardened into a Block.
2. **G1: capability-cache path.** Amend ADR to `~/.ocx/state/referrers/<registry>.json` and add a `state/` tier note to `arch-principles.md` + `subsystem-file-structure.md`.
3. **R2 carry-over:** `url::Url` vs ADR `&str` injection seam (still unrecorded in the amendment log).
4. **R2 carry-over:** `ReferrersApiCapability::TTL_SECS = 6 * 3600` deviates from ADR's "24h CI / 1h interactive." Implementation choice may be correct but the deviation remains silent.

## Files referenced

- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/sign/endpoint.rs` (lines 1-219)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/sign/signer.rs` (lines 1-69)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/sign/oidc.rs` (lines 80-114, 130-203)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/sign/pipeline.rs` (lines 34-90)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/sign/error.rs` (lines 130-148)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/sign.rs` (lines 28-56)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/verify/error.rs` (lines 11-145)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/verify/pipeline.rs` (lines 34-80)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/verify/trust_root.rs` (lines 95-148)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/verify.rs` (lines 14-29)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/referrer/capability.rs` (lines 25-26, 105-200)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/referrer/manifest.rs` (lines 12-65)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/cli/classify.rs` (lines 93-160, 431-520)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/error.rs` (lines 102-115, 203-235)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/package_manager/tasks/sign.rs` (lines 78-93)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/package_manager/tasks/verify.rs` (lines 70-86)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_cli/src/command/package_sign.rs` (lines 88-225)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_cli/src/command/verify.rs` (lines 67-105)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_cli/src/api/data/signature.rs` (lines 35-109)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_cli/src/api/data/verification.rs` (lines 60-180)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_cli/src/app/context.rs` (lines 219-234)
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_cli/src/error_envelope.rs` (lines 38-234)
- `/home/mherwig/dev/ocx-evelynn/.claude/artifacts/adr_oci_referrers_signing_v1.md`
- `/home/mherwig/dev/ocx-evelynn/.claude/artifacts/review_pr-87_architecture.md`
