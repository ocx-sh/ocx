# PRD: OCI Referrers Signing v1 (Slice 1 — Sign + Verify)

## Metadata

- **Status:** Approved
- **Date:** 2026-04-19
- **Author:** worker-architect (Slice 1 Design phase)
- **Related ADR:** [`adr_oci_referrers_signing_v1.md`](./adr_oci_referrers_signing_v1.md)
- **Related PR-FAQ:** [`pr_faq_oci_referrers_signing_v1.md`](./pr_faq_oci_referrers_signing_v1.md)
- **Related Plan:** [`plan_slice1_sign_and_verify.md`](../state/plans/plan_slice1_sign_and_verify.md)
- **Issue:** [#24](https://github.com/ocx-sh/ocx/issues/24)

## Problem Statement

OCX users shipping binaries through OCI registries cannot prove to their downstream consumers that the binary they pulled is byte-for-byte the same binary the publisher pushed, signed by the publisher's verifiable identity. Today, supply-chain guarantees in OCX pipelines require a parallel install of `cosign` plus manual wire-up — violating the "single tool" property that is a core OCX differentiator (see [`product-context.md`](../rules/product-context.md)).

The user rejected a verify-only MVI on 2026-04-18 as a "half-product": shipping verification without a way to sign from OCX itself meant users still needed cosign in their CI. Slice 1 closes the loop.

## Goals

- **G1.** A CI publisher can sign an OCX package with a single command that requires no prior cosign install, no keypair management, and no manual OIDC token handling on supported CI platforms.
- **G2.** A CI consumer can verify a package with a single command. Verification fails closed — there is no way to silently accept an unsigned or wrong-identity package.
- **G3.** Artifacts produced by `ocx package sign` are verifiable by `cosign verify`. Artifacts signed by `cosign sign` are verifiable by `ocx verify` (Slice 2 layers in external-sig discovery; Slice 1 provides the primitives).
- **G4.** Every failure produces an exit code a shell script or CI step can branch on without parsing stderr.
- **G5.** Every failure in `--format json` mode produces a typed envelope with a stable `schema_version`.

## Non-Goals (v1)

- `ocx sbom` (Slice 2)
- External signature discovery from cosign legacy `.sig` tags (Slice 2)
- DSSE / `ocx package attest` (waiting on sigstore-rs 0.14)
- TOML trust-policy file (v2 — forward-compat hooks reserved in v1 exit codes)
- HSM / KMS signing (v2+)
- Notation support (no Rust library exists)
- Offline signing (rejected in ADR S1-E)
- `--insecure-ignore-tlog` (rejected in ADR D4)

## Personas

### P1 — Release Engineer in CI (primary)

- **Role:** Platform engineer operating a release pipeline in GitHub Actions for a medium-size open-source project.
- **Context:** Has `id-token: write` permission configured on the workflow. Runs `ocx package push` then `ocx package sign` as the final step of a release job.
- **Primary pain:** Today must install cosign as a separate step, configure a separate `id-token: write` permission check, and wire up the OCI referrer plumbing manually.
- **Success criterion:** Signing is a single `ocx package sign ocx.sh/cmake:3.28` call, OIDC acquisition is automatic, and the output is verifiable with `cosign` by consumers who haven't adopted OCX yet.

### P2 — CI Consumer (primary)

- **Role:** Platform engineer operating a deployment pipeline consuming binaries from an internal OCI registry.
- **Context:** Runs `ocx verify` in CI before `ocx install` to gate deployment on a signature match.
- **Primary pain:** Today depends on cosign's CLI, which has a notoriously unfriendly NDJSON output for `verify-blob` and different flag names depending on version.
- **Success criterion:** Single `ocx verify` call with an expected identity and issuer. Exit code 0 = proceed, non-zero = stop pipeline. `--format json` produces a typed envelope a Bazel rule or GitHub Action step can consume without stderr parsing.

### P3 — Laptop Publisher (secondary)

- **Role:** Maintainer manually pushing a bugfix release from their laptop while CI is being debugged.
- **Context:** Runs `ocx package sign` interactively. Expects a browser PKCE flow.
- **Primary pain:** cosign's laptop flow sometimes gets confused about which browser to open; `ocx` should be deterministic.
- **Success criterion:** `ocx package sign` opens the browser, the user confirms, signing completes, and future `ocx verify` commands from any machine accept the signature.

### Non-targets

- **End users typing `ocx install` on their workstations** — they don't need to sign and are not targeted for verify either (that's a CI concern).
- **Security auditors drafting trust policies** — policy-as-code is v2 (TOML file).
- **Private-CA operators** — v1 uses the public Sigstore trust root only.

## User Scenarios

### Scenario S1 — Happy path: CI sign (GHA)

**Context:** Release workflow in GitHub Actions with `permissions: id-token: write`.

**Steps:**

1. Prior step: `ocx package push ocx.sh/cmake:3.28 ./cmake-3.28.tar.xz -p linux/amd64`
2. Current step: `ocx package sign ocx.sh/cmake:3.28 -p linux/amd64`

**Expected output (stdout plain):**

```
Signed ocx.sh/cmake:3.28 (linux/amd64)
  identity:  https://github.com/example/cmake-release/.github/workflows/release.yml@refs/heads/main
  issuer:    https://token.actions.githubusercontent.com
  referrer:  sha256:a1b2...
```

**Exit code:** 0.

**JSON output** (C-S1-1 frozen v1 contract — nested `data` object; never a top-level `signed: [...]` or `verified: [...]` array):

```json
{
  "schema_version": 1,
  "command": "package sign",
  "exit_code": 0,
  "data": {
    "identifier": "ocx.sh/cmake:3.28",
    "platform": "linux/amd64",
    "subject_digest": "sha256:...",
    "referrer_digest": "sha256:a1b2...",
    "bundle_digest": "sha256:c2d0...",
    "signer": "keyless-fulcio",
    "certificate_identity": "https://github.com/example/cmake-release/.github/workflows/release.yml@refs/heads/main",
    "certificate_oidc_issuer": "https://token.actions.githubusercontent.com"
  }
}
```

### Scenario S2 — Happy path: CI verify

**Context:** Deployment workflow gating on signature.

**Steps:**

```
ocx verify ocx.sh/cmake:3.28 \
  --certificate-identity https://github.com/example/cmake-release/.github/workflows/release.yml@refs/heads/main \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  -p linux/amd64
```

**Expected output (stdout plain):**

```
Verified ocx.sh/cmake:3.28 (linux/amd64)
  identity:  https://github.com/example/cmake-release/.github/workflows/release.yml@refs/heads/main
  issuer:    https://token.actions.githubusercontent.com
  signed_at: 2026-04-19T12:00:00Z (Rekor SET)
```

**Exit code:** 0. Deployment proceeds.

### Scenario S3 — Happy path: laptop sign (browser PKCE)

**Context:** Maintainer at a TTY, no ambient OIDC (`ci-id` returns None).

**Steps:**

1. `ocx package sign ocx.sh/cmake:3.28 -p linux/amd64`
2. Terminal prints: `Opening browser for Sigstore OIDC authentication: https://oauth2.sigstore.dev/...`
3. Browser opens; user logs in with GitHub / Google / Microsoft identity.
4. Browser redirects to a local callback; OCX captures the code, exchanges for token, proceeds to Fulcio.
5. Signing completes; referrer pushed.

**Exit code:** 0.

### Scenario S4 — Ambient detection failure in CI (actionable error)

**Context:** Release workflow where the maintainer forgot `id-token: write` permission.

> **Design decision (C-S1-4):** A raw `--identity-token <TOKEN>` flag was rejected on security grounds. Tokens passed as argv leak through process listings (`/proc/<pid>/cmdline`, `ps auxwww`), shell history, and CI debug surfaces (`set -x`, rendered command traces in GitHub Actions). The defensible overrides are (in precedence order): (1) `--identity-token-file <PATH>`, (2) `--identity-token-stdin`, (3) `OCX_IDENTITY_TOKEN` env var. Ambient OIDC remains the primary path; these three are escape hatches.

**Steps:** `ocx package sign ocx.sh/cmake:3.28 -p linux/amd64`

**Expected output (stderr):**

```
error: OIDC token acquisition failed
  reason: ambient detection found GitHub Actions environment but ACTIONS_ID_TOKEN_REQUEST_TOKEN is not set
  remediation: add `permissions: id-token: write` to your workflow (https://docs.github.com/en/actions/deployment/security-hardening-your-deployments/about-security-hardening-with-openid-connect)
  alternative: provide a pre-fetched OIDC token via --identity-token-file <PATH>, --identity-token-stdin, or the OCX_IDENTITY_TOKEN env var
```

**Exit code:** 77 (`PermissionDenied` — OIDC pre-check failure).

**JSON error envelope** (C-S1-1 frozen v1 contract — `error` is nested; `context` is inside `error`):

```json
{
  "schema_version": 1,
  "command": "package sign",
  "exit_code": 77,
  "error": {
    "kind": "permission_denied",
    "detail": "oidc_missing_gha_permission",
    "message": "ambient detection found GitHub Actions environment but ACTIONS_ID_TOKEN_REQUEST_TOKEN is not set",
    "remediation": "add `permissions: id-token: write` to your workflow",
    "context": {
      "identifier": "ocx.sh/cmake:3.28",
      "ci_platform": "github_actions"
    }
  }
}
```

### Scenario S5 — Fulcio outage

**Context:** Sigstore Fulcio is returning 503.

**Steps:** `ocx package sign ocx.sh/cmake:3.28 -p linux/amd64`

**Expected output (stderr):**

```
error: Fulcio unavailable (HTTP 503 Service Unavailable)
  reason: could not obtain signing certificate
  remediation: check https://status.sigstore.dev and retry later
```

**Exit code:** 69 (`Unavailable`).

### Scenario S6 — Rekor outage (distinct from Fulcio)

**Context:** Fulcio succeeded; Rekor returns 502.

**Steps:** `ocx package sign ocx.sh/cmake:3.28 -p linux/amd64`

**Expected output (stderr):**

```
error: Rekor unavailable (HTTP 502 Bad Gateway)
  reason: signing certificate obtained but transparency log entry could not be written
  remediation: Rekor outage — retry later; partial signing state has been discarded
```

**Exit code:** 82 (`RekorUnavailable` — new variant, distinct from 69 so CI scripts can branch on "retry later" vs "network broken").

**Note:** On verify-side, the same 82 fires if Rekor cannot be reached during SET validation.

### Scenario S7 — Registry without Referrers API

**Context:** Target is GHCR (as of 2026-04 still lacks Referrers API).

**Steps:** `ocx package sign ghcr.io/example/cmake:3.28 -p linux/amd64`

**Expected output (stderr):**

```
error: registry does not support Referrers API
  registry: ghcr.io
  remediation: GHCR does not yet implement OCI Distribution Spec v1.1 referrers; OCX does not write
               fallback `.sig` tags (see adr_oci_referrers_signing_v1.md decision S1-F).
               Alternatives: use a compliant registry (ocx.sh, Harbor, ACR, Zot), wait for GHCR support,
               or sign externally with `cosign sign` (OCX cannot verify those signatures until Slice 2).
```

**Exit code:** 83 (`ReferrersUnsupported` — registry lacks OCI Distribution Spec v1.1 Referrers API; dedicated exit code per ADR decision S1-F).

### Scenario S8 — Re-sign (new referrer written)

**Context:** Publisher wants to add a second identity's signature to an existing release (e.g., audit co-sign).

**Steps:**

1. First sign (CI identity): `ocx package sign ocx.sh/cmake:3.28 -p linux/amd64` (from CI)
2. Second sign (release-manager identity, from laptop): `ocx package sign ocx.sh/cmake:3.28 -p linux/amd64` (from laptop)

**Expected output (stdout plain):**

```
Signed ocx.sh/cmake:3.28 (linux/amd64)
  identity:  alice@example.com
  issuer:    https://accounts.google.com
  referrer:  sha256:b3c4...
  note:      existing signatures preserved (referrer list has 2 signatures)
```

**Exit code:** 0.

**Verify:** A subsequent `ocx verify` matching *either* identity accepts the package. The verify command does not require all signatures to match — it requires *at least one* that matches the provided `--certificate-identity` / `--certificate-oidc-issuer`.

### Scenario S9 — Expired cert chain on verify

**Context:** Signed three years ago; Fulcio-issued cert has expired. Rekor SET still valid (temporal proof intact).

**Steps:** `ocx verify ocx.sh/cmake:3.28-ancient --certificate-identity ... --certificate-oidc-issuer ...`

**Expected output (stderr):**

```
Verified ocx.sh/cmake:3.28-ancient (linux/amd64)
  identity:   alice@example.com
  issuer:     https://accounts.google.com
  signed_at:  2023-04-19T12:00:00Z (Rekor SET)
  cert_expired_but_tlog_valid: true
```

**Exit code:** 0. Per cosign policy, expired cert + valid Rekor SET from before cert expiry = still valid.

### Scenario S10 — Identity mismatch (verify)

**Context:** Signature exists but was made by a different identity than expected.

**Steps:**

```
ocx verify ocx.sh/cmake:3.28 \
  --certificate-identity https://github.com/expected/release/.github/workflows/release.yml@refs/heads/main \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

**Expected output (stderr):**

```
error: no signature matches the provided identity
  found: 1 signature(s) on ocx.sh/cmake:3.28
    signature 1:
      identity: https://github.com/OTHER-org/release/.github/workflows/release.yml@refs/heads/main
      issuer:   https://token.actions.githubusercontent.com
  remediation: either trust the actual signer by updating --certificate-identity,
               or reject the package (current behavior)
```

**Exit code:** 77 (`PermissionDenied` — authenticated signer is not the expected identity; matches ADR `IdentityMismatch` variant).

### Scenario S11 — No signatures found on verify

**Context:** Package hasn't been signed.

**Steps:** `ocx verify ocx.sh/cmake:3.28 --certificate-identity ... --certificate-oidc-issuer ...`

**Expected output (stderr):**

```
error: no signatures found for ocx.sh/cmake:3.28 (linux/amd64)
  remediation: publisher has not signed this release; contact the publisher or accept
               that this release is unsigned (at your own risk — OCX does not provide
               an escape hatch to accept unsigned releases)
```

**Exit code:** 79 (`NotFound`).

## Functional Requirements

| # | Requirement | Acceptance Test |
|---|---|---|
| FR-1 | `ocx package sign <REFERENCE> -p <PLATFORM>` writes a Sigstore bundle v0.3 referrer on a Referrers-API-supporting registry | `test_sign_writes_v0_3_referrer` |
| FR-2 | On GHCR or similar (no Referrers API), `ocx package sign` exits 83 with `ReferrersUnsupported` — no fallback tag written | `test_sign_fails_83_on_no_referrers_api` |
| FR-3 | Ambient OIDC (GHA, GitLab, CircleCI, Buildkite, GCP) is detected via `ambient-id` + inline fallback without requiring any identity-token override | `test_sign_ambient_detection_gha` (per-platform matrix) |
| FR-4 | Missing GHA `id-token: write` permission produces exit 77 with `oidc_missing_gha_permission` detail | `test_sign_gha_missing_id_token_permission_exits_77` |
| FR-5 | Laptop TTY flow opens browser PKCE when no ambient and no identity-token override (`--identity-token-file`, `--identity-token-stdin`, `OCX_IDENTITY_TOKEN`) | `test_sign_browser_pkce_opens_browser` (manual; gated by a `--no-tty` in auto tests) |
| FR-6 | `--no-tty` + no ambient + no identity-token override exits 77 with `oidc_no_tty_no_ambient` | `test_sign_no_tty_no_ambient_exits_77` |
| FR-7 | `--offline` on `package sign` is rejected with exit 77 | `test_sign_offline_exits_77` |
| FR-8 | `ocx verify <REFERENCE> --certificate-identity <I> --certificate-oidc-issuer <O>` succeeds when signature matches | `test_verify_happy_path` |
| FR-9 | Missing `--certificate-identity` or `--certificate-oidc-issuer` exits 64 (`UsageError`) | `test_verify_missing_flags_exits_64` |
| FR-10 | Identity mismatch exits 77 with `identity_mismatch` (ADR-canonical variant) | `test_verify_identity_mismatch_exits_77` |
| FR-11 | No signatures found exits 79 with `no_signatures_found` | `test_verify_no_signatures_exits_79` |
| FR-12 | Rekor unreachable on verify exits 82 with `rekor_unavailable` | `test_verify_rekor_down_exits_82` |
| FR-13 | Registry 5xx during verify exits 69 with `service_unavailable` (per ADR Resolution A on exit code 81) | `test_verify_registry_5xx_exits_69` |
| FR-14 | Re-sign writes a new referrer; existing signatures remain discoverable | `test_sign_idempotent_appends_new_referrer` |
| FR-15 | Cosign can verify a bundle produced by `ocx package sign` (cross-tool interoperability) | `test_cosign_verify_ocx_signed` (uses `cosign` binary, installed via `ocx install cosign` in test setup) |
| FR-16 | `--format json` produces envelope with `schema_version: 1` for all errors | `test_sign_json_error_envelope`, `test_verify_json_error_envelope` |
| FR-17 | Capability cache respects 24h TTL in CI mode, 1h interactive | `test_capability_cache_ttl_ci`, `test_capability_cache_ttl_interactive` |
| FR-18 | `--no-cache` global flag bypasses capability cache | `test_no_cache_bypasses_capability_cache` |
| FR-19 | Expired cert + valid Rekor SET verifies successfully | `test_verify_expired_cert_valid_tlog` (fixture-based) |

## Non-Functional Requirements

| # | Requirement | Measurement |
|---|---|---|
| NFR-1 | `ocx package sign` completes in <10s over a warm network (local registry) | `test_sign_perf_local_registry` (best-effort gate; warn if >15s, fail if >30s) |
| NFR-2 | `ocx verify` completes in <5s over a warm network | Same gating model |
| NFR-3 | Zero net-new runtime dependencies outside the sigstore-rs + ci-id pair | `cargo tree` diff in review |
| NFR-4 | Every new PackageErrorKind variant has a documented exit-code classification | `test_every_error_kind_has_exit_code_test` (AI config test) |
| NFR-5 | `--log-level=debug` surfaces OIDC token acquisition path (ambient vs flag vs browser) without leaking the token | `test_debug_log_no_token_leak` (negative grep) |

## Acceptance Test Strategy

### Test environments

| Layer | Tool | Sigstore endpoint | Purpose |
|---|---|---|---|
| Unit (Rust) | `cargo nextest` | none — pre-generated fixtures | Fast feedback; state-machine coverage |
| Integration (Rust) | `cargo nextest` with `#[ignore]` gate, opt-in via `OCX_TEST_SIGSTORE_STAGING=1` | `fulcio.sigstage.dev` + `rekor.sigstage.dev` | Real HTTP round-trip against staging |
| Acceptance (pytest) | `uv run pytest` | staging (opt-in) OR pre-generated deterministic bundles | End-to-end CLI; cross-tool cosign compat |
| Manual | — | production (`fulcio.sigstore.dev` + `rekor.sigstore.dev`) | Pre-release smoke test before tagging a release |

### Fixture contract

Committed at `test/fixtures/signing/`:

- `bundle_v03_gha.json` — pre-generated bundle produced against staging; targets a specific well-known manifest digest
- `bundle_v03_expired_cert.json` — cert has expired; SET still valid (for FR-19)
- `fulcio_root.pem` — staging Fulcio root (rotated only on Fulcio rotation)
- `rekor_pubkey.pem` — staging Rekor public key
- `target_manifest.json` + `target_manifest.sha256` — the per-platform image manifest the bundles target
- `README.md` — documents how to regenerate fixtures (runbook uses `cosign sign-blob` + `ocx package sign` against staging)

Fixture generation is **manual**, gated on a maintainer command. CI never regenerates fixtures.

### CI matrix

- Linux/amd64 (primary) — full matrix
- macOS/arm64 — smoke (`test_sign_writes_v0_3_referrer`, `test_verify_happy_path`, `test_verify_no_signatures_exits_79`)
- Windows/amd64 — smoke (same as macOS)

### Sigstore staging availability

If `fulcio.sigstage.dev` or `rekor.sigstage.dev` returns 5xx for the duration of a CI run, the integration tests skip with a `SIGSTORE_STAGING_UNAVAILABLE` marker. This is a deliberate escape hatch to avoid external-dependency flake; the fixture-based unit tests are the primary guarantee.

### Anti-pattern: do NOT depend on `cosign sign` in CI

Per Codex finding #8. We verify cosign/OCX interop by having OCX sign and `cosign verify` (FR-15). We do **not** have `cosign sign` produce fixtures, because that path pins us to an external binary's version semantics.

## Out of Scope / Deferred to v2

| Capability | Target | Forward-compat hook |
|---|---|---|
| SBOM discovery (`ocx sbom`) | Slice 2 | Media-type table in `oci/referrer/media_types.rs` extensible |
| External signature discovery (legacy cosign tags) | Slice 2 | `verify/pipeline.rs` extensible; current code never writes tags |
| `ocx package attest` (DSSE) | v2 (sigstore-rs 0.14+) | `TokenProvider` trait works for attest too |
| TOML trust-policy file | v2 | Exit codes 78 + 79 reserved |
| HSM / KMS signing | v2+ | `TokenProvider` trait is narrow; KMS plugs in at Fulcio step |
| Notation (CNCF) support | Indefinite | No Rust library |
| Private CA / BYO trust root | v2 | `trust_root.rs` wraps `sigstore-trust-root` cleanly |
| Signature GC on registry | Never (registry concern) | Referrer append-only per S1-I |

## Risks & Open Questions

| ID | Risk / Question | Owner | Resolution path |
|---|---|---|---|
| RQ-1 | sigstore-rs 0.13 has DSSE gap blocking `ocx package attest` | Eng | Pin; watch 0.14 release for attest unlock |
| RQ-2 | Exit-code 81 conflict with existing `OfflineBlocked` enum variant | ADR author | Flagged to human reviewer; Resolution A adopted in ADR (81 stays `OfflineBlocked`, 69 handles network 5xx) |
| RQ-3 | `ambient-id` crate may lack GCP Cloud Build support | Eng | Acceptable gap; users pass token via `--identity-token-file`, `--identity-token-stdin`, or `OCX_IDENTITY_TOKEN` env var |
| RQ-4 | Fulcio root TUF rotation mid-release | Sec | Pinned via `sigstore-trust-root = "0.6.4"`; refresh cadence matches sigstore-rs minor releases |
| RQ-5 | Rekor non-determinism makes fixtures non-bytewise-stable | Eng | Fixture contract: fields present & well-formed, not byte equality |

## Success Metrics

- **Adoption** — >20% of OCX-published packages signed within 6 months of v1 GA.
- **Reliability** — <0.5% of `ocx package sign` invocations fail due to OCX-fixable issues (excludes Fulcio / Rekor outages).
- **Interop** — `cosign verify` accepts 100% of successfully-signed OCX bundles against matching identity/issuer flags (FR-15 in CI).
- **Zero escape hatches** — no `--insecure` flag is added post-launch without a formal v2 scope re-open.

## References

- [`adr_oci_referrers_signing_v1.md`](./adr_oci_referrers_signing_v1.md) — design decisions S1-A through S1-I
- [`pr_faq_oci_referrers_signing_v1.md`](./pr_faq_oci_referrers_signing_v1.md) — working-backwards press release
- [`plan_slice1_sign_and_verify.md`](../state/plans/plan_slice1_sign_and_verify.md) — implementation plan
- [`research_cosign_signing_flow.md`](./research_cosign_signing_flow.md) — push-side state machine
- [`research_oidc_cli_flows.md`](./research_oidc_cli_flows.md) — ambient OIDC dispatch
- [Sigstore Bundle v0.3 spec](https://github.com/sigstore/protobuf-specs/blob/main/protos/sigstore_bundle.proto)
- [OCI Distribution Spec v1.1 — Referrers API](https://github.com/opencontainers/distribution-spec/blob/main/spec.md#listing-referrers)
