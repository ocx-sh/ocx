# Architecture Review — PR #87 (OCI Referrers Sign + Verify Slice 1)

**Status:** Phase 5 stub scaffold — `SignPipeline::run` / `VerifyPipeline::run` / Fulcio / Rekor / Bundle / Ambient still `unimplemented!()`; CLI entry points fall through to typed `Internal` / `TrustRootUnavailable` errors.

**Scope:** Diff against merge-base `1fa82446`. Read-only adversarial.

## Verdict

**Conditionally pass** for stub phase. Architecture is sound, boundaries are clean, the trait splits (`Signer` / `TokenProvider` / `AmbientProvider` / `ClassifyErrorKind`) faithfully implement ADR S1-C and Architect F2/F6/F7. The C-S1-3 injection seams (`fulcio_url` / `rekor_url` / `trust_root`) are wired through both `Context` types as `url::Url` / borrowed `TrustRoot`. The frozen v1 JSON envelope is byte-locked by golden tests. No proprietary media types; only `application/vnd.dev.sigstore.bundle.v0.3+json`. No fallback `.sig` tag writes (S1-F upheld).

What blocks "pass" outright is one **actionable** boundary leak (CLI duplicates SSRF / token-file logic that belongs in `ocx_lib`) and three **suggest** items that are cheap to defer to Phase 5c.

## Count by tier

- **Block:** 0
- **Actionable:** 2
- **Deferred (need ADR amendment):** 2
- **Suggest:** 4

## Top 3 boundary / SOLID violations

### 1. (Actionable) SSRF validation lives in CLI; should be in `ocx_lib`

`crates/ocx_cli/src/command/sigstore_url.rs::validate_sigstore_url` is pure domain logic (HTTPS-only with loopback carve-out, userinfo rejection, scheme allow-list) consumed by both `package_sign.rs` and `verify.rs`. It returns `anyhow::Result<Url>` — `anyhow` in a layer that is fed to `oci/sign/pipeline.rs` via context. This violates the lib/CLI error boundary rule in `quality-rust-errors.md` ("`anyhow::Error` in library APIs" = Block-tier). Today it is only re-borrowed in the CLI, so it sneaks through, but Phase 5c plans to forward `fulcio_url: &Url` into `SignContext`. At that point the validator's *output type* will cross the boundary and the `anyhow::Result` return becomes a real coupling problem.

**Fix:** move `validate_sigstore_url` into `ocx_lib::oci::sign::endpoint` (or `oci::endpoint`), return `Result<Url, SignErrorKind::ConfigError>` (new variant) — the existing `FulcioBadRequest` is the wrong fit because the failure is local, not a Fulcio response. Then both CLI commands call the lib validator and surface the typed kind through the existing chain.

### 2. (Actionable) Token-file resolution duplicates the trait split

`PackageSign::resolve_override_token` (CLI) does:
- mode-bits permission check (`mode & 0o077`)
- async file read with TOCTOU-safe handle reuse
- stdin async read + trim
- `OCX_IDENTITY_TOKEN` env fallback
- Windows refusal branch
- emits `SignErrorKind::IdentityTokenFilePermissive` + `SignErrorKind::OidcPreCheckFailed`

This is **OIDC token acquisition logic** — exactly the responsibility the `TokenProvider` trait in `oci::sign::oidc` was carved out to own. The CLI is reaching past the abstraction and producing `SignError` directly. SRP violation at the module level: `package_sign.rs` now coordinates *and* executes part of the OIDC dispatch state machine.

**Fix:** add a `FileTokenProvider` / `StdinTokenProvider` / `EnvTokenProvider` impl in `oci/sign/`, each returning `OidcToken` with the existing `Zeroizing<String>` storage. CLI shrinks to "construct dispatcher from the three flag values and pass to context". The `IdentityTokenFilePermissive` check moves to `FileTokenProvider::new(path)`. The `DispatchingTokenProvider::new()` stub (which is currently nilary!) gains the parameters the plan's amendment log already documents — they are missing today.

### 3. (Suggest) `SignContext` exposes `&dyn OciTransport` but no client accessor exists

`crates/ocx_cli/src/command/package_sign.rs` lines 117–132 document the Phase 5c blocker: `SignPipeline::run` requires `&dyn OciTransport`, but `oci::Client` keeps transport private. The current "fix" is to error with `SignErrorKind::Internal` so exit code maps to `Failure (1)`. This is a **one-way door risk**: if Phase 5c adds a `Client::transport()` accessor, that accessor becomes part of the lib's stable surface (everything in `crates/ocx_lib/src/oci/sign/**` is `pub`). Alternatives — adding `SignContext::new_from_client(&Client)`, or making the pipeline take `&Client` directly and reach `transport` through a `pub(crate)` accessor — preserve more flexibility.

The ADR does not pick between these. Recommend a brief ADR amendment locking the choice **before** Phase 5c rather than letting it land implicitly in the first sign-pipeline PR.

## ADR-deviation list

- **`url::Url` vs ADR `&str`.** ADR §"Context injection" specifies `fulcio_url: &'a str` and `rekor_url: &'a str`. Implementation uses `&'a url::Url`. Sound choice (parsing once at boundary, not in pipeline), but unrecorded. Minor — fold into next R-pass.
- **`ReferrersApiCapability` TTL = 6h.** ADR specifies 24h CI / 1h interactive. `capability.rs:26` hardcodes `TTL_SECS = 6 * 3600` with no env / interactive branch. Deviation could matter on busy CI matrices (more probes than ADR estimates). Not a correctness issue.
- **`InternalPathInvalid` classifies to `Failure`.** `error.rs:208` maps `Self::InternalPathInvalid(_)` to `ExitCode::Failure` instead of `DataError` — orthogonal to this PR but touched by the same Slice-1 fix wave (`Sign`/`Verify` added to the same `classify` impl).
- **`VerifyErrorKind::RekorSetInvalid → DataError(65)`.** Implementation correctly applies the 2026-04-21 amendment that overrides the ADR's original `82` mapping. Logged here so the override is auditable; not a deviation.

## Deferred items (need ADR amendment)

1. **Client transport accessor pattern** (see violation #3) — fork point between exposed `transport()` accessor vs. context-builder helper. Recommend two-paragraph amendment to `adr_oci_referrers_signing_v1.md` §"Context injection".
2. **ADR-noted `url::Url` parse type for injection seams** — surface the deviation in the amendment log so future maintainers don't "fix" it back to `&str`.

## Boundary check — passes

- **Crate dep direction:** `ocx_lib` does not depend on `ocx_cli`. Verified — all sign/verify types live in `ocx_lib::oci::{sign,verify}` and CLI imports via `ocx_lib::oci::sign::{SignError, SignErrorKind}`.
- **No `anyhow` in `ocx_lib`:** confirmed — `SignError` / `VerifyError` are `thiserror`-derived three-layer (Error → Kind), `Box<dyn Error + Send + Sync>` for the `Internal` variant only.
- **`#[non_exhaustive]`:** applied to `SignErrorKind`, `VerifyErrorKind`, `ClientError`, `ReferrersSupport` (via serde, no enum-grow break). `TrustRoot` not non-exhaustive but is a struct.
- **Three-layer error pattern:** `Error → SignError(identifier, kind) → SignErrorKind` honors the ADR D8 contract. `ClassifyErrorKind` trait keeps exit-code mapping in lockstep with kind enum via exhaustive match (compile error on variant add).
- **Sigstore-rs types not leaked through public API:** `FulcioCertificate` is `pub(super)`; `RekorEntry` is `pub(super)`; the `SignedBundle` re-export carries only `bytes: Vec<u8>` + `digest: String` — sigstore-rs types live behind `mod fulcio` / `mod rekor` (private). Healthy.
- **Bundle / referrer manifest formats:** spec-compliant (`application/vnd.dev.sigstore.bundle.v0.3+json`, OCI 1.1 manifest with `subject`, OCI empty-config digest baked in). No proprietary annotations beyond `dev.ocx.sign.tool-version`.
- **Trust root storage:** `~/.ocx/state/referrers/<registry>.json` for capability cache. No new `OCX_HOME` top-level dir introduced; reuses `state/`. Good.
- **Cross-subsystem coupling:** no sign/verify references in `crates/ocx_lib/src/project/`. CLI's `cli/classify.rs` ladder adds two `try_downcast!` entries (`SignError`, `VerifyError`) — minimal, additive.

## Suggest items

- **`ReferrersApiCapability::probe`** uses a synthetic `latest` tag (`capability.rs:88`) to satisfy the reference type. Documented in the doc comment, but couples capability to a tag namespace. Worth a `Reference::digest_only(...)` helper.
- **`AmbientProvider::detect` static trait method** prevents heterogeneous chains (must call each impl's static function). Consider an instance method returning `Option<Self>` so dispatching can hold `Vec<Box<dyn AmbientProvider>>`.
- **`PackageError::Internal(crate::Error::Sign(Box<SignError>))`** double-wraps: `Error::Sign` boxes already, then `PackageErrorKind::Internal(Error::Sign(...))` adds one indirection. The `_map_sign_error` free function is dead — consider deleting the stub or wiring it.
- **Plan-status drift:** plan Status block (line 8) says "Active phase: 5 — Implementation"; many phase-5 modules are still `unimplemented!()`. The phase label should distinguish "5a stubs landed" from "5c wired". Minor process hygiene.

## Files referenced

- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/sign/pipeline.rs`
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/sign/signer.rs`
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/sign/oidc.rs`
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/sign/error.rs`
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/verify/pipeline.rs`
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/verify/error.rs`
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/verify/trust_root.rs`
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/referrer/capability.rs`
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/oci/client/error.rs`
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/cli/classify.rs`
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/error.rs`
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_cli/src/command/package_sign.rs`
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_cli/src/command/verify.rs`
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_cli/src/command/sigstore_url.rs`
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_cli/src/app/context.rs`
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_cli/src/error_envelope.rs`
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/package_manager/tasks/sign.rs`
- `/home/mherwig/dev/ocx-evelynn/crates/ocx_lib/src/package_manager/tasks/verify.rs`
