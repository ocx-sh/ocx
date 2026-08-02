# Research — OIDC Token Acquisition UX for CLI Signing

**Date:** 2026-04-19
**Scope:** OCX issue #24 expanded scope — how `ocx package sign` should acquire the OIDC identity token consumed by Fulcio during cosign-keyless signing.
**Target user:** primarily CI/automation (GitHub Actions, GitLab, CircleCI, Buildkite). Interactive developer laptop is a secondary context.

## Executive Summary (3 sentences)

OCX's primary CI users (GitHub Actions, GitLab, CircleCI, Buildkite) all provide ambient OIDC tokens via environment variables, making the browser and device flows secondary concerns — implement **ambient detection first** (via `ambient-id` — the active successor to the now-archived `jku/ci-id` crate — with a ~80-line inline env-inspection fallback), with **browser PKCE** as the interactive fallback. The most impactful engineering investment is **client-side pre-validation** (audience check, expiry check) with platform-specific error hints, because the most common failures (missing `id-token: write` on GHA, wrong audience on CircleCI, missing `id_tokens:` block on GitLab) are misconfiguration errors that produce confusing downstream 4xx errors from Fulcio. **Device flow should be deferred to v2** — the intersection of "non-interactive, non-CI, needs to sign" is a rare OCX use case, and browser PKCE already covers the developer-laptop scenario.

## Flow-selection Decision Table (v1 dispatch order)

| Context | Detection Signal | Flow | v1 |
|---|---|---|---|
| `--identity-token <tok>` flag or `SIGSTORE_ID_TOKEN` env | explicit value present | **Passthrough** (expiry + audience pre-check) | ✅ |
| GitHub Actions | `ACTIONS_ID_TOKEN_REQUEST_URL` + `ACTIONS_ID_TOKEN_REQUEST_TOKEN` present | **Ambient: GHA API fetch** (requires `permissions: id-token: write`) | ✅ |
| GitLab CI | `SIGSTORE_ID_TOKEN` injected via job `id_tokens:` block | **Ambient: env var** | ✅ |
| CircleCI | `CIRCLE_OIDC_TOKEN` or `CIRCLE_OIDC_TOKEN_V2` | **Ambient: env var** (audience pre-check required) | ✅ |
| Buildkite | `BUILDKITE_AGENT_ACCESS_TOKEN` + `BUILDKITE_JOB_ID` | **Ambient: Buildkite agent API** | ✅ |
| Google Cloud (GCE/GKE/Cloud Build) | metadata server reachable at `169.254.169.254` | **Ambient: metadata server** | ✅ |
| Interactive laptop | TTY detected (via `is-terminal`) + no ambient token | **Browser PKCE** via sigstore-rs `OauthTokenProvider` | ✅ |
| SSH / headless / no CI / no browser | no TTY + no ambient token | **Hard error** with diagnostic listing supported providers | ✅ (v1) |
| Air-gapped | network unreachable | **Hard error** — keyless signing unsupported without Fulcio+Rekor | ✅ (v1) |
| Device flow (RFC 8628) | — | Deferred to v2 | ⛔ |

## Key Decisions (v1)

### D-OIDC-1: Ambient detection via `ambient-id` crate with inline fallback
- **Pick:** `ambient-id = "0.1"` (or latest) as primary; inline ~80-line env-var detector as fallback trait implementation for providers `ambient-id` does not cover.
- **Why:** The previously-chosen `jku/ci-id` was **archived on 2026-01-27** (permanently read-only; 3 open issues + 1 open PR will never be merged; CVE response path gone). `ambient-id` is actively maintained, Fedora packaging review underway (RHBZ#2396331), and is the successor the Sigstore ecosystem is converging on as of 2026-Q2.
- **Fallback design:** OCX wraps detection behind a local `AmbientProvider` trait (see D-OIDC-4 v2 hook). Primary impl delegates to `ambient-id`. A secondary inline impl inspects `ACTIONS_ID_TOKEN_REQUEST_URL`, `SIGSTORE_ID_TOKEN` (GitLab), `CIRCLE_OIDC_TOKEN_V2`, `BUILDKITE_AGENT_ACCESS_TOKEN`, and the GCP metadata server — ~80 lines of stable Rust. If `ambient-id` introduces a regression or API drift, the inline fallback keeps OCX signing operational.
- **Alternative rejected:** Pinning the archived `ci-id` crate — unacceptable security posture (security-sensitive OIDC path with no maintainer channel for CVE fixes).
- **Alternative rejected:** Inline-only detection — forgoes community alignment with the Sigstore ecosystem's evolving convention and places full maintenance burden on OCX.

### D-OIDC-2: `--identity-token` passthrough as a first-class option
- **Pick:** `--identity-token <jwt>` flag + `OCX_IDENTITY_TOKEN` env var. Both are forwarded to `sigstore-rs` unmodified after pre-check.
- **Why:** Matches `cosign sign --identity-token`; lets advanced CI systems inject tokens from external flows (vault, OIDC federation, service account federation).
- **Pre-check:** decode JWT payload (base64url + serde_json), verify `exp > now + 60s`, verify `aud == "sigstore"`. Fail fast with actionable error before bothering Fulcio.

### D-OIDC-3: Browser PKCE via sigstore-rs for interactive laptop
- **Pick:** delegate entirely to `sigstore::oauth::OauthTokenProvider` in `sigstore = "=0.13"`. No custom HTTP server.
- **Listener port:** ephemeral (localhost:0) — sigstore-rs binds to an OS-assigned free port. Docs note this avoids port-conflict failures reported in cosign issue #1258-family.
- **Redirect URL:** `http://localhost:<ephemeral-port>/auth/callback`.
- **Fallback when browser cannot open:** print URL to stderr with `please open in browser:` prefix; wait for callback. No QR code in v1.

### D-OIDC-4: Device flow deferred to v2
- **Why defer:** the target OCX user is CI; the secondary target is "developer with a browser." The intersection "headless SSH session that needs to sign" is v2-scope.
- **v2 trigger:** if GitHub issue requests accumulate or an enterprise integration needs air-gapped SSH signing, revisit.
- **Library-ready:** `openidconnect = "4.0.1"` already ships RFC 8628 device-grant support. Plug-in point is a new `DeviceTokenProvider` that implements the same `TokenProvider` trait we define for sigstore-rs passthrough.

### D-OIDC-5: Audience is fixed to `"sigstore"` (not configurable in v1)
- **Why:** Fulcio **requires** `aud == "sigstore"` (non-negotiable; baked into Fulcio policy). Making it configurable invites misconfiguration.
- **CircleCI caveat:** CircleCI's default OIDC audience is `<org-id>`, not `sigstore`. Workaround: use `CIRCLE_OIDC_TOKEN_V2` with an explicit audience configured in the project, or have the user pre-configure. Document the caveat in error messages.

### D-OIDC-6: Client-side error pre-validation with platform-specific hints
- **Pick:** on pre-check failure, map the error to a typed `OidcError` variant with a platform-specific fix:
  - `TokenExpired { exp, now }` → "token expired at {exp}; fetch a fresh one"
  - `WrongAudience { expected: "sigstore", actual }` → CircleCI: "configure `oidc_token.audience = sigstore` in your project"
  - `MissingGhaPermission` (detected by `ACTIONS_ID_TOKEN_REQUEST_URL` absent but `GITHUB_ACTIONS=true`) → "add `permissions: id-token: write` to your workflow"
  - `MissingGitlabIdTokens` (detected by `CI_JOB_ID` present but `SIGSTORE_ID_TOKEN` absent) → "add `id_tokens: SIGSTORE_ID_TOKEN: { aud: sigstore }` to your job"
  - `NoTtyNoAmbient` → "run interactively, pass `--identity-token`, or use a supported CI"
- **Why:** Fulcio returns generic 401s; users cannot diagnose from the HTTP error alone. These rules capture field-report patterns from cosign issue tracker (#1258, #2849, #2863).

## Risks & Mitigations

| Risk | Mitigation |
|---|---|
| sigstore-rs v0.13 OIDC surface changes between now and v1 cut | Pin to `=0.13` (already a hard dep); wrap `OauthTokenProvider` behind an OCX-local `TokenProvider` trait so v2 can swap impls without CLI breakage |
| `ambient-id` diverges from Sigstore ecosystem conventions or introduces a regression | Inline fallback `AmbientProvider` impl (~80 lines) ships alongside the `ambient-id`-backed impl; toggleable via hidden config if ecosystem support consolidates on a different crate |
| `ambient-id` introduces a CVE or API break before v1 cut | Pin to a known-good minor range; the inline fallback lets OCX de-integrate quickly via feature flag |
| `ACTIONS_ID_TOKEN_REQUEST_URL` fetch can fail (transient network) | Retry 3x with backoff at Transport layer (already standard for all OCX HTTP) |
| User passes a JWT that isn't actually a JWT | `serde_json::from_slice` on decoded payload fails cleanly → map to `InvalidToken` variant |
| CircleCI user forgets audience config → repeated Fulcio 403s | Pre-check decodes `aud` before calling Fulcio; actionable error |

## Crate Choices (v1)

| Purpose | Crate | Version | Notes |
|---|---|---|---|
| Ambient CI detection (primary) | `ambient-id` | latest (0.1.x) | Active successor to archived `jku/ci-id`; Fedora packaging review in progress (RHBZ#2396331) as of 2026-Q2 |
| Ambient CI detection (fallback) | — | — | Inline ~80-line env-inspection impl of `AmbientProvider`; covers GHA, GitLab, CircleCI, Buildkite, GCP metadata server |
| TTY detection | `is-terminal` | `0.4` | Cross-platform; already transitively in tree via `indicatif` |
| JWT payload decode (pre-check only) | — | — | Manual `base64` + `serde_json::from_slice`. **Do NOT add `jsonwebtoken`** — we don't verify signatures (Fulcio does), just peek at `exp`/`aud`. |
| Browser OAuth flow | `sigstore` (existing) | `=0.13` | Reuse built-in `OauthTokenProvider` |
| HTTP client for GHA token fetch | `reqwest` (existing) | — | Already in tree |

## Implementation Sketch (types only; full ADR in `adr_oci_referrers_signing_v1.md`)

```rust
// crates/ocx_lib/src/oci/sign/oidc.rs
pub trait TokenProvider: Send + Sync {
    async fn token(&self, audience: &str) -> Result<IdentityToken, OidcError>;
    fn provider_name(&self) -> &'static str;
}

pub struct IdentityToken(String);  // raw JWT, validated to parse

pub enum OidcError {
    TokenExpired { exp: i64, now: i64 },
    WrongAudience { expected: &'static str, actual: String },
    MissingGhaPermission,
    MissingGitlabIdTokens,
    CircleCiAudienceMisconfig,
    NoTtyNoAmbient,
    ProviderFailure { provider: &'static str, source: BoxError },
    InvalidToken(String),
}

pub enum TokenSource {
    ExplicitFlag(IdentityToken),       // --identity-token
    Ambient(Box<dyn TokenProvider>),   // ci-id detected
    Browser(Box<dyn TokenProvider>),   // sigstore-rs OauthTokenProvider
}

pub fn resolve_provider(cfg: &SigningConfig) -> Result<TokenSource, OidcError>;
```

**Dispatch order (pseudocode):**
```
if let Some(tok) = cfg.identity_token_flag_or_env { pre_check(tok)?; return ExplicitFlag(tok); }
// Primary: ambient-id crate (active, Sigstore-ecosystem-aligned).
if let Some(provider) = ambient_id::detect() { return Ambient(provider); }
// Fallback: inline env-inspection (~80 lines) — GHA, GitLab, CircleCI, Buildkite, GCP.
if let Some(provider) = inline_ambient::detect() { return Ambient(provider); }
if is_terminal::stderr() { return Browser(sigstore::OauthTokenProvider::new()); }
return Err(NoTtyNoAmbient);
```

## Sources

- [Sigstore OIDC overview](https://docs.sigstore.dev/cosign/signing/overview/)
- [cosign GitHub Actions provider source](https://github.com/sigstore/cosign/blob/main/pkg/providers/github/github.go)
- [cosign providers directory](https://github.com/sigstore/cosign/tree/main/pkg/providers)
- [cosign device flow source](https://github.com/sigstore/sigstore/blob/main/pkg/oauthflow/device.go)
- [ci-id Rust crate (archived 2026-01-27)](https://github.com/jku/ci-id) — read-only; superseded
- [ambient-id crate (active successor)](https://crates.io/crates/ambient-id)
- [Fedora packaging review RHBZ#2396331](https://bugzilla.redhat.com/show_bug.cgi?id=2396331) — ambient-id packaging status
- [GitLab keyless signing examples](https://docs.gitlab.com/ci/yaml/signing_examples/)
- [CircleCI OIDC tokens](https://circleci.com/docs/openid-connect-tokens/)
- [Fulcio OIDC requirements](https://docs.sigstore.dev/certificate_authority/oidc-in-fulcio/)
- [Sigstore security model](https://docs.sigstore.dev/about/security/)
- [cosign issue #2849 — ambient credential detection docs](https://github.com/sigstore/cosign/issues/2849)
- [cosign issue #2863 — SIGSTORE_ID_TOKEN env var](https://github.com/sigstore/cosign/issues/2863)
- [cosign issue #1258 — GHA `id-token: write` misconfiguration](https://github.com/sigstore/cosign/issues/1258)
- [openidconnect crate (RFC 8628)](https://docs.rs/openidconnect/latest/openidconnect/)
- [jsonwebtoken crate stats](https://lib.rs/crates/jsonwebtoken)
- [GitHub Actions OIDC docs](https://docs.github.com/en/actions/concepts/security/openid-connect)
- [sigstore-rs crate stats](https://lib.rs/crates/sigstore) — 49k monthly downloads, 16 reverse deps (April 2026)
