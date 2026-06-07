# Research: Verify / Discover CLI Patterns (Patterns Axis)

**Date:** 2026-04-19
**Scope:** Phase 2 research for OCX `ocx verify` / `ocx sbom`.
**Axis:** CLI UX patterns.

## TL;DR

- **Exit 0 for no referrers found** (oras model). `ocx verify` and `ocx sbom` are *discovery* commands, not enforcement commands. Add `--require` flags to let callers opt into hard-fail.
- **NDJSON is a wart** — cosign prints verification payloads as newline-delimited JSON objects (not a JSON array). OCX must emit a proper JSON array under `--format json`, with a top-level `schema_version: 1` field from day 1.
- **Cosign's output goes to stderr** (verification status messages and `PrintVerification`), making it unparseable for scripting. OCX must put all structured output on stdout and diagnostics on stderr.
- **`--distribution-spec` is infrastructure complexity, not UX**. OCX should default to auto (probe referrers API, fall back to tag) and expose `--distribution-spec` only as an escape hatch — matching oras's `v1.1-referrers-api | v1.1-referrers-tag | auto` pattern.
- **Notation's trust policy model** (named policies, registry scope globs, verification levels, trust stores by type) is the right shape. Ship v1 with a permissive placeholder; the flag surface and config schema must not foreclose adding real policies later.
- **Key-material flags** should follow notation's positional approach (trust stores in config, not flags) rather than cosign's flag soup (`--certificate-identity`, `--certificate-oidc-issuer`, `--certificate-identity-regexp` — 15+ flags). For OCX v1, accept `--key <path>` only; keyless/OIDC is a future extension.
- **`schema_version: 1` at the root of every JSON output object** is non-negotiable. Downstream consumers (GH Actions, OPA) need a migration path when fields change.

---

## 1. Peer Tools Survey

### 1.1 cosign verify

**Source:** sigstore/cosign, [doc/cosign_verify.md](https://github.com/sigstore/cosign/blob/main/doc/cosign_verify.md) — accessed 2026-04-19; [issue #210](https://github.com/sigstore/cosign/issues/210) — accessed 2026-04-19; [issue #2510](https://github.com/sigstore/cosign/issues/2510) — accessed 2026-04-19

**Flag shape (abridged):**
```
cosign verify [flags] <image>

Key material (choose one family):
  --key cosign.pub | gcpkms://... | k8s://...
  --ca-roots ca-roots.pem
  --ca-intermediates ca-intermediates
  --certificate-chain chain.crt

Keyless / OIDC identity:
  --certificate-identity name@example.com
  --certificate-identity-regexp <regex>
  --certificate-oidc-issuer https://accounts.example.com
  --certificate-oidc-issuer-regexp <regex>
  --certificate-github-workflow-name
  --certificate-github-workflow-ref
  --certificate-github-workflow-repository
  --certificate-github-workflow-sha
  --certificate-github-workflow-trigger

Verification controls:
  --check-claims=true          (default true)
  -a foo=bar                   (annotation assertion, repeatable)
  --signature-digest-algorithm sha256|sha384|sha512
  --insecure-ignore-sct
  --insecure-ignore-tlog
  --rekor-url https://rekor.sigstore.dev
  --trusted-root trusted-root.json
  --use-signed-timestamps
  --private-infrastructure

Output:
  -o, --output json|text       (default json)
  --max-workers 10

OCI v1.1:
  --experimental-oci11         (incomplete — see issue #4335)
```

**Exit codes:**
- Exit 0: at least one signature found matching the key/identity.
- Exit 1: no signatures found, or verification fails (signature invalid, identity mismatch).
- No sysexits.h alignment — bare 0/1.

**Output format warts (critical for OCX to avoid):**

1. **NDJSON wart**: When verifying a multiply-signed image, `cosign verify --output json` emits multiple JSON objects separated by newlines — one per valid signature — not a JSON array. Issue #210 (opened 2021, still unresolved as of 2026-04-19) proposes wrapping in a proper array. Downstream scripts using `jq` must use `jq -s .` to slurp NDJSON into an array before processing.

2. **Stderr mixing**: Cosign's `PrintVerification` function writes verification payloads to the same stream as status messages. Issue #2510 (2022) documents that informational messages, warnings, and machine-readable payloads are not cleanly separated. The SBOM attachment deprecation warning is explicitly printed via `fmt.Fprintln(os.Stderr, ...)` — but other output is inconsistent.

3. **Attestation output**: `cosign verify-attestation` has the same NDJSON wart. Issue #2404 labels the output "malformed JSON" from the attestation path.

4. **OCI v1.1 incompleteness**: `--experimental-oci11` flag exists on `sign`/`verify`/`tree` but is absent from `verify-attestation`, `download attestation`, and `attach attestation`. Issue #4335 tracks completion.

**Recommendation for OCX:** Do not imitate cosign's output model at all. Use a proper JSON array in `--format json` mode, emit only structured data to stdout, diagnostics to stderr.

---

### 1.2 notation verify

**Source:** [notaryproject.dev (verify guide)](https://notaryproject.dev/docs/user-guides/how-to/verify-image/) — accessed 2026-04-19; [notaryproject/specifications trust-store-trust-policy.md](https://github.com/notaryproject/specifications/blob/main/specs/trust-store-trust-policy.md) — accessed 2026-04-19; [notation verify.go source](https://github.com/notaryproject/notation/blob/main/cmd/notation/verify.go) — accessed 2026-04-19

**Flag shape:**
```
notation verify [flags] <reference>

Verification:
  --plugin-config "{key}={value}"   (passed to verification plugin)
  --user-metadata "{key}={value}"   (custom metadata assertions)
  --max-signatures int              (default 100)
  --oci-layout                      ([Experimental] verify OCI image layout)
  --scope string                    ([Experimental] override trust policy scope)

Logging / security (inherited):
  --verbose, --debug
  (TLS flags inherited from parent)
```

Notation has **no `--key` flag on the CLI**. Key material lives entirely in trust stores on disk (`~/.config/notation/trust-store/`). Trust policy is a JSON file at `~/.config/notation/trust-policy.json` or `~/.config/notation/trust-policy.oci.json`.

**Trust policy JSON schema (v1.0):**
```json
{
  "version": "1.0",
  "trustPolicies": [
    {
      "name": "my-policy",
      "registryScopes": ["registry.example.com/myapp/*"],
      "signatureVerification": {
        "level": "strict",
        "override": {
          "revocation": "skip"
        },
        "verifyTimestamp": "afterCertExpiry"
      },
      "trustStores": ["ca:my-ca", "tsa:my-tsa"],
      "trustedIdentities": [
        "x509.subject: C=US, ST=California, O=Acme Inc., CN=signer"
      ]
    }
  ]
}
```

Verification levels:
- `strict`: all checks enforced (integrity, authenticity, expiry, revocation, timestamp)
- `permissive`: authenticity + integrity enforced; timestamp/expiry/revocation logged
- `audit`: integrity enforced; all others logged
- `skip`: nothing checked

Trust store types: `ca` (X.509 root), `signingAuthority`, `tsa` (timestamp authority).

**Exit codes:** Notation returns non-zero on verification failure but does not publish a stable sysexits-aligned taxonomy. Failure exits with code 1 in most cases.

**Output format:** Plain text by default; no `--format json` flag in the stable surface. Experimental JSON output exists in some builds via the `Printer` interface but is not documented as stable.

**Recommendation for OCX:** Adopt notation's *trust policy model* (named policies, registry scope globs, verification level enum), not the flag soup approach. The policy-as-file pattern is future-proof and keeps the flag surface clean.

---

### 1.3 oras discover

**Source:** [oras.land/docs/commands/oras_discover](https://oras.land/docs/commands/oras_discover/) — accessed 2026-04-19; [oras v1.3.0-beta.3 blog](https://oras.land/blog/oras-v1.3.0-beta.3/) — accessed 2026-04-19; [oras discover.go source](https://github.com/oras-project/oras/blob/main/cmd/oras/root/discover.go) — accessed 2026-04-19

**Flag shape:**
```
oras discover [flags] <reference>

Discovery:
  --artifact-type string        filter referrers by artifact type
  --depth int                   recursion depth (0 = unlimited, default 0)
  --distribution-spec string    v1.1-referrers-api | v1.1-referrers-tag
                                (absent = auto-probe)

Output:
  --format string               tree (default) | table (deprecated) | json | go-template
  --template string             Go template expression for go-template mode

Authentication (inherited):
  --username, --password, --password-stdin
  --identity-token, --identity-token-stdin
  --ca-file, --cert-file, --key-file
  --insecure, --plain-http

OCI layout:
  --oci-layout
  --oci-layout-path string
```

**JSON output schema (v1.3.0+):**
```json
{
  "reference": "registry.example.com/myapp:v1.0",
  "mediaType": "application/vnd.oci.image.manifest.v1+json",
  "digest": "sha256:deadbeef...",
  "size": 1234,
  "referrers": [
    {
      "reference": "registry.example.com/myapp@sha256:abc123...",
      "mediaType": "application/vnd.oci.image.manifest.v1+json",
      "artifactType": "application/vnd.dev.cosign.signature.v1+json",
      "digest": "sha256:abc123...",
      "size": 567,
      "annotations": {
        "dev.cosignproject.cosign/signature": "..."
      },
      "referrers": []
    }
  ]
}
```

Note: the field was renamed from `manifests` to `referrers` in v1.3.0 — a non-backward-compatible change that reinforces the need for `schema_version` in OCX's own output.

**Exit behavior for empty referrers:** The `fetchAllReferrers` function completes normally when the result list is empty. The command exits 0 — an empty result is a valid, successful query. This is the correct model for a discovery command used in pipelines.

**`--distribution-spec` behavior:** When absent, oras auto-probes: it first calls the OCI referrers API endpoint (`GET /v2/<n>/referrers/<digest>`); if the registry returns 404, it falls back to the referrers tag schema (tag derived from the digest with `:` replaced by `-`). When specified, it forces the named method, useful for debugging or for registries with known behavior.

**Recommendation for OCX:** This is the cleanest peer reference for `ocx verify --list` / `ocx sbom`. Mirror the `--distribution-spec` escape hatch and the exit-0-on-empty contract.

---

### 1.4 crane

**Source:** [google/go-containerregistry crane docs](https://github.com/google/go-containerregistry/blob/main/cmd/crane/doc/crane.md) — accessed 2026-04-19; [go-containerregistry issue #2205](https://github.com/google/go-containerregistry/issues/2205) — accessed 2026-04-19

`crane` does **not** have a `referrers` subcommand. Its supply-chain verification surface is limited to `crane manifest <ref>` (raw manifest fetch, useful for inspecting the `subject` field) and `crane ls <repo>` (listing tags, including the referrers tag fallback tags like `sha256-deadbeef...`).

`crane` exposes the OCI v1.1 referrers tag fallback as a visible artifact of its `ls` output — querying a repo that uses the tag fallback scheme shows `sha256-deadbeef...` tags alongside semantic tags. This is a known footgun (issue #2205 documents a race condition in go-containerregistry's tag-fallback update logic under concurrent writers).

**Conclusion:** crane is not a peer for OCX's verify/sbom commands. Relevant only as a low-level inspection tool or for internal library use (oci-client, which OCX already uses, is based on go-containerregistry patterns).

---

### 1.5 syft / trivy sbom / grype

**Source:** [anchore/syft wiki — attestation](https://github.com/anchore/syft/wiki/attestation) — accessed 2026-04-19; [trivy.dev sbom docs](https://trivy.dev/latest/docs/supply-chain/sbom/) — accessed 2026-04-19; [anchore/grype GitHub](https://github.com/anchore/grype) — accessed 2026-04-19

**syft:**
- Generates SBOMs from container images and filesystems.
- Outputs: CycloneDX (JSON + XML), SPDX (JSON + tag-value), syft-native JSON, GitHub dependency snapshot format.
- Attestation: `syft attest` wraps the SBOM in an in-toto statement and attaches it via `cosign` as a cosign-style referrer (uses the cosign tag-fallback scheme, not the OCI v1.1 referrers API natively). Future migration to OCI v1.1 referrers API is planned.
- SBOM *discovery* is not syft's job — syft *generates* SBOMs. To discover a syft-generated SBOM from a registry, you would use `cosign download attestation` (for cosign-style) or `oras discover` (for OCI v1.1 referrers).
- Relevant artifact types syft attaches: `application/vnd.in-toto+json` (attestation envelope), predicates of type `https://spdx.dev/Document` or `https://cyclonedx.org/bom`.

**trivy sbom:**
- `trivy sbom <file>` scans an existing SBOM file for vulnerabilities.
- `trivy image --format cyclonedx <ref>` generates an SBOM as part of an image scan.
- Trivy *detects* embedded SBOMs within container image layers by file extension: `.spdx`, `.spdx.json`, `.cdx`, `.cdx.json`.
- Trivy does not use the OCI v1.1 referrers API to discover externally-attached SBOMs at scan time (as of 2026-04-19). This is an ecosystem gap.
- For attestation discovery, trivy can consume SBOMs attached by cosign via `trivy image --scanners vuln,secret <ref>` with `COSIGN_*` environment variables — but this goes through cosign's attestation download, not native OCI referrers.

**grype:**
- `grype <ref>` vulnerability scanner; does not do OCI referrer discovery natively.
- Can accept a syft SBOM as input (`grype sbom:<path>`).
- The ecosystem expectation (noted in [artifacts-spec/scenarios.md](https://github.com/oras-project/artifacts-spec/blob/main/scenarios.md)) is that future tooling will use the OCI v1.1 referrers API with a well-known `artifactType` for SBOMs (`application/vnd.cyclonedx+json` or `application/spdx+json`) to enable discovery without cosign intermediation.

**Summary of SBOM artifact types in active use:**

| Tool | artifactType / mediaType | Discovery mechanism |
|------|--------------------------|---------------------|
| cosign attest (syft) | `application/vnd.in-toto+json` | cosign tag fallback or OCI v1.1 referrers |
| cosign attach sbom | `application/vnd.syft+json` (deprecated) | cosign tag fallback |
| notation (future) | TBD | OCI v1.1 referrers API |
| OCI v1.1 native SBOM | `application/vnd.cyclonedx+json` / `application/spdx+json` | OCI v1.1 referrers API |

---

## 2. Exit Code Decision

**Decision: Exit 0 when no referrers found.**

**Rationale:**

`ocx verify` and `ocx sbom` are *discovery* commands. Their job is to query the registry and report what is attached to a given OCI artifact. An empty result is a valid answer to a valid query — not a failure.

The oras model is correct here: `oras discover` exits 0 when no referrers exist. The command succeeded (it reached the registry, ran the query, received an answer). The answer happens to be empty.

Contrast with `cosign verify`, which exits 1 on no signatures. That is appropriate for cosign because it is an *enforcement* command — the caller is asserting "this image must have a valid signature matching this key." Absence of a matching signature *is* a failure of the enforcement goal.

OCX's issue #24 is framed as discovery, not enforcement. The distinction maps directly to exit codes:

| Command intent | Zero referrers result | Exit code |
|---|---|---|
| Discovery (ocx verify, ocx sbom default) | Empty list, command succeeded | 0 |
| Enforcement (opt-in via `--require`) | Requirement not met | 65 (`DataError`) |

**`--require` flag design for hard-fail:**

```
--require-referrers          fail (exit 65) if no referrers found at all
--min-referrers N            fail (exit 65) if fewer than N referrers found
--require-artifact-type T    fail (exit 65) if no referrer with this artifactType found
```

These flags convert discovery into enforcement, composably. A CI script that must enforce signature presence writes:

```sh
ocx verify --require-artifact-type application/vnd.dev.cosign.signature.v1+json \
           registry.example.com/myapp:v1.0
```

Without `--require*`, the same command exits 0 with an empty list — safe for use in non-enforcing pipelines.

**Exit code mapping for error conditions in `ocx verify` / `ocx sbom`:**

| Condition | Exit code |
|---|---|
| Success (referrers found or empty) | `Success` (0) |
| `--require*` unmet | `DataError` (65) |
| Registry unreachable (network error) | `Unavailable` (69) |
| Registry returned 5xx | `Unavailable` (69) or `TempFail` (75) |
| Registry returned 401 | `AuthError` (80) |
| Registry returned 403 | `PermissionDenied` (77) |
| Malformed OCI index response | `DataError` (65) |
| Invalid reference syntax | `UsageError` (64) |
| Offline mode blocked | `OfflineBlocked` (81) |

---

## 3. Proposed Flag Surfaces

### 3.1 ocx verify

```rust
/// ocx verify <ref>
///
/// Discover referrer artifacts attached to an OCI artifact via the OCI v1.1
/// Referrers API (or tag fallback for older registries). Exits 0 whether or
/// not referrers are found unless a --require flag is set.
#[derive(Debug, clap::Args)]
pub struct VerifyArgs {
    // ── Filtering ────────────────────────────────────────────────────────────
    /// Filter referrers by OCI artifact type.
    /// Example: --artifact-type application/vnd.dev.cosign.signature.v1+json
    #[arg(long)]
    pub artifact_type: Option<String>,

    // ── Hard-fail (enforcement) ───────────────────────────────────────────────
    /// Fail with exit code 65 if no referrers are found.
    #[arg(long)]
    pub require_referrers: bool,

    /// Fail with exit code 65 if fewer than N referrers are found.
    #[arg(long, value_name = "N")]
    pub min_referrers: Option<usize>,

    /// Fail with exit code 65 if no referrer with this artifact type is found.
    /// May be specified multiple times (all types must be present).
    #[arg(long, value_name = "TYPE")]
    pub require_artifact_type: Vec<String>,

    // ── Registry capability ───────────────────────────────────────────────────
    /// Force a specific OCI distribution spec referrers strategy.
    ///
    /// By default, OCX auto-probes: tries the v1.1 referrers API first;
    /// falls back to the tag scheme if the registry returns 404.
    ///
    /// Use this flag only to override auto-detection for registries with
    /// known behavior or for debugging.
    #[arg(long, value_name = "SPEC", value_enum)]
    pub distribution_spec: Option<DistributionSpec>,

    // ── Positional ────────────────────────────────────────────────────────────
    /// OCI reference to discover referrers for.
    /// Examples: registry.example.com/myapp:v1.0
    ///           registry.example.com/myapp@sha256:deadbeef...
    pub reference: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DistributionSpec {
    /// OCI v1.1 referrers API (GET /v2/<n>/referrers/<digest>).
    #[value(name = "v1.1-referrers-api")]
    V1_1ReferrersApi,
    /// OCI v1.1 tag fallback scheme (sha256-<truncated-digest> tag).
    #[value(name = "v1.1-referrers-tag")]
    V1_1ReferrersTag,
}
```

### 3.2 ocx sbom

```rust
/// ocx sbom <ref>
///
/// Discover SBOM artifacts attached to an OCI artifact. Filters the OCI v1.1
/// referrers list to known SBOM artifact types (CycloneDX, SPDX, in-toto).
/// Exits 0 whether or not SBOMs are found unless --require is set.
#[derive(Debug, clap::Args)]
pub struct SbomArgs {
    /// Download the SBOM content and write it to this path.
    /// When multiple SBOMs are found and --download is specified, the first
    /// match (by artifact type preference: cyclonedx > spdx > in-toto) is used
    /// unless --artifact-type selects a specific one.
    #[arg(long, value_name = "PATH")]
    pub download: Option<std::path::PathBuf>,

    /// Filter to a specific SBOM artifact type.
    ///
    /// Defaults: all known SBOM types are shown.
    ///   application/vnd.cyclonedx+json
    ///   application/spdx+json
    ///   application/vnd.in-toto+json
    #[arg(long)]
    pub artifact_type: Option<String>,

    /// Fail with exit code 65 if no SBOM referrers are found.
    #[arg(long)]
    pub require: bool,

    /// Force a specific OCI distribution spec referrers strategy.
    /// See `ocx verify --help` for details.
    #[arg(long, value_name = "SPEC", value_enum)]
    pub distribution_spec: Option<DistributionSpec>,

    /// OCI reference to discover SBOMs for.
    pub reference: String,
}
```

Flag order note: all flags precede positional `<reference>` per OCX convention (user memory preference for flags-before-positional).

---

## 4. JSON Schemas (v1, stable)

### 4.1 ocx verify --format json

```json
{
  "schema_version": 1,
  "reference": "registry.example.com/myapp:v1.0",
  "subject_digest": "sha256:deadbeef1234567890abcdef1234567890abcdef1234567890abcdef12345678",
  "referrers": [
    {
      "digest": "sha256:abc1230000000000000000000000000000000000000000000000000000000001",
      "artifact_type": "application/vnd.dev.cosign.signature.v1+json",
      "media_type": "application/vnd.oci.image.manifest.v1+json",
      "size": 1402,
      "annotations": {
        "dev.cosignproject.cosign/signature": "MEYCIQDd..."
      }
    },
    {
      "digest": "sha256:def4560000000000000000000000000000000000000000000000000000000002",
      "artifact_type": "application/vnd.in-toto+json",
      "media_type": "application/vnd.oci.image.manifest.v1+json",
      "size": 8192,
      "annotations": {
        "in-toto.io/predicate-type": "https://spdx.dev/Document"
      }
    }
  ],
  "referrer_count": 2,
  "registry_method": "referrers-api"
}
```

Fields:

| Field | Type | Notes |
|---|---|---|
| `schema_version` | integer | Always `1` for this version. Increment on breaking field changes. |
| `reference` | string | Input reference as resolved (may include digest if tag was resolved). |
| `subject_digest` | string | The digest of the subject manifest being queried. |
| `referrers` | array | OCI referrer descriptors. Empty array when none found. |
| `referrers[].digest` | string | `algorithm:hex` format. |
| `referrers[].artifact_type` | string | OCI `artifactType` field value. |
| `referrers[].media_type` | string | Manifest media type. |
| `referrers[].size` | integer | Manifest size in bytes. |
| `referrers[].annotations` | object | Key-value annotations from the manifest. Absent if empty. |
| `referrer_count` | integer | Convenience; equals `len(referrers)`. |
| `registry_method` | string | `"referrers-api"` or `"referrers-tag"` — which mechanism was used. |

Empty result (still exit 0):
```json
{
  "schema_version": 1,
  "reference": "registry.example.com/myapp:v1.0",
  "subject_digest": "sha256:deadbeef...",
  "referrers": [],
  "referrer_count": 0,
  "registry_method": "referrers-api"
}
```

### 4.2 ocx sbom --format json

```json
{
  "schema_version": 1,
  "reference": "registry.example.com/myapp:v1.0",
  "subject_digest": "sha256:deadbeef1234567890abcdef1234567890abcdef1234567890abcdef12345678",
  "sboms": [
    {
      "digest": "sha256:bom0000000000000000000000000000000000000000000000000000000000001",
      "artifact_type": "application/vnd.cyclonedx+json",
      "media_type": "application/vnd.oci.image.manifest.v1+json",
      "size": 14820,
      "annotations": {
        "org.opencontainers.image.created": "2026-01-15T12:00:00Z",
        "org.opencontainers.image.description": "CycloneDX SBOM for myapp v1.0"
      }
    },
    {
      "digest": "sha256:spdx0000000000000000000000000000000000000000000000000000000000002",
      "artifact_type": "application/vnd.in-toto+json",
      "media_type": "application/vnd.oci.image.manifest.v1+json",
      "size": 10240,
      "annotations": {
        "in-toto.io/predicate-type": "https://spdx.dev/Document"
      }
    }
  ],
  "sbom_count": 2,
  "registry_method": "referrers-api"
}
```

Well-known SBOM `artifact_type` values OCX should recognize and filter for:

| `artifact_type` | Format | Notes |
|---|---|---|
| `application/vnd.cyclonedx+json` | CycloneDX JSON | OCI v1.1 native |
| `application/vnd.cyclonedx+xml` | CycloneDX XML | OCI v1.1 native |
| `application/spdx+json` | SPDX JSON | OCI v1.1 native |
| `application/vnd.in-toto+json` | in-toto envelope | Cosign attest; check `annotations["in-toto.io/predicate-type"]` for sub-type |
| `application/vnd.syft+json` | Syft native (deprecated) | Cosign attach sbom (legacy) |

---

## 5. Trust Policy Shape (v1 placeholder, future-compatible)

OCX v1 ships with an implicit "accept any referrer" policy — no trust enforcement. The architecture must not foreclose adding real trust policies in v2.

**Design constraints:**
1. The `ocx verify` and `ocx sbom` commands must accept a `--trust-policy <path>` flag even in v1, even if the flag is ignored or activates a stub.
2. The config file format for trust policies must follow notation's structure (named policies, registry scopes as globs, verification levels) so OCX users who know notation can transfer knowledge.
3. Trust stores should be on disk at `$OCX_HOME/trust-store/<type>/<name>/` mirroring notation's layout.

**v1 stub trust policy shape (future-compatible):**

```toml
# $OCX_HOME/trust-policy.toml  (v1 stub — verify reads this but all verification is "skip")
version = "1"

[[policies]]
name = "default"
registry_scopes = ["*"]
[policies.signature_verification]
level = "skip"   # v1 default: no enforcement
trust_stores = []
trusted_identities = ["*"]
```

**v2 target shape (do not implement now, but design must support):**

```toml
version = "1"

[[policies]]
name = "production"
registry_scopes = [
  "ghcr.io/myorg/*",
  "registry.example.com/prod/*",
]
[policies.signature_verification]
level = "strict"   # strict | permissive | audit | skip
trust_stores = ["ca:my-org-ca", "tsa:sigstore-tsa"]
trusted_identities = [
  "x509.subject: O=Acme Inc., CN=ci-signer",
]
[policies.signature_verification.override]
revocation = "skip"   # enforce | log | skip
```

**CLI flag surface (v1 — wired but stub):**

```rust
/// Path to a trust policy TOML file.
/// Defaults to $OCX_HOME/trust-policy.toml if it exists.
/// In v1, trust policies are read but signature verification level
/// defaults to "skip" (discovery only).
#[arg(long, value_name = "PATH")]
pub trust_policy: Option<std::path::PathBuf>,
```

This flag must appear on both `VerifyArgs` and `SbomArgs` from day one. Shipping without it and adding it in v2 would be a breaking change if callers have scripted the command surface.

---

## 6. Registry Capability Negotiation

**OCI v1.1 auto-probe algorithm:**

1. Issue `GET /v2/<name>/referrers/<digest>` (the OCI v1.1 referrers API endpoint).
2. If registry returns HTTP 200 with `Content-Type: application/vnd.oci.image.index.v1+json`: use referrers API result. Set `registry_method: "referrers-api"` in output.
3. If registry returns HTTP 404: fall back to the tag scheme.
   - Construct fallback tag: `sha256-<first 64 chars of hex digest>` (`:` → `-`).
   - Issue `GET /v2/<name>/manifests/<fallback-tag>`.
   - If 200: parse as OCI image index. Set `registry_method: "referrers-tag"` in output.
   - If 404: no referrers exist. Return empty list, exit 0.
4. If `--distribution-spec v1.1-referrers-api` forced and registry returns 404: return error with `Unavailable` (69) exit code and message "registry does not support the OCI v1.1 referrers API".
5. If `--distribution-spec v1.1-referrers-tag` forced: skip API probe, go directly to step 3.

**Registry support matrix (as of 2026-04-19):**

| Registry | OCI v1.1 Referrers API | Source |
|---|---|---|
| Amazon ECR | Supported (since April 2024) | [AWS Blog](https://aws.amazon.com/blogs/opensource/diving-into-oci-image-and-distribution-1-1-support-in-amazon-ecr/) |
| GHCR (GitHub) | Supported | [OCI v1.1 announcement](https://opencontainers.org/posts/blog/2024-03-13-image-and-distribution-1-1/) |
| Google Artifact Registry | Supported | |
| JFrog Artifactory | Full conformance declared | [JFrog Blog](https://jfrog.com/blog/full-conformance-to-oci-v1-1/) |
| Quay.io | Supported | [Red Hat Blog](https://www.redhat.com/en/blog/announcing-open-container-initiativereferrers-api-quayio-step-towards-enhanced-security-and-compliance) |
| Docker Hub | Supported | |
| `registry:2` (test) | No referrers API (tag fallback only) | Local test registry |

The last row is important: OCX's acceptance test suite uses `registry:2` (Docker's reference implementation), which does **not** implement the OCI v1.1 referrers API. Tests must exercise both the API path (mocked or against a v1.1-capable registry) and the tag fallback path (`registry:2`).

**`OCI-Subject` header optimization:** When pushing a referrer (not relevant to `ocx verify` / `ocx sbom` directly, but relevant to OCX's future `ocx push` or mirror pipeline), check for the `OCI-Subject` response header. Its presence means the registry updated the referrers index automatically — no need to maintain the fallback tag. Its absence means the client must manage the fallback tag manually.

---

## 7. Anti-Patterns to Avoid

**1. cosign NDJSON output** — `cosign verify --output json` emits one JSON object per line, not a JSON array. This makes `jq .field` fail; users must know to use `jq -s .[0].field`. [Issue #210](https://github.com/sigstore/cosign/issues/210) has been open since 2021 with no resolution. **OCX must emit a JSON array from day one.**

**2. cosign stdout/stderr mixing** — Status messages, warnings, and machine-readable payloads go to the same stream. [Issue #2510](https://github.com/sigstore/cosign/issues/2510) lists specific cases. A downstream script piping `cosign verify ... | jq` will silently fail when a warning message is interleaved. **OCX must put all structured output on stdout, all diagnostics on stderr, with no interleaving.**

**3. cosign flag soup for identity** — `--certificate-identity`, `--certificate-identity-regexp`, `--certificate-oidc-issuer`, `--certificate-oidc-issuer-regexp`, `--certificate-github-workflow-name`, `--certificate-github-workflow-ref`, `--certificate-github-workflow-repository`, `--certificate-github-workflow-sha`, `--certificate-github-workflow-trigger`. Fifteen-plus flags for identity assertion at the CLI. **OCX should keep identity assertion in the trust policy file (notation model), not in CLI flags.**

**4. notation's lack of `--format json`** — The stable `notation verify` surface has no machine-readable output mode. Policy violations are reported as human-readable text only. **OCX must ship `--format json` from v1 with a stable schema, not as a later addition.**

**5. oras renaming `manifests` → `referrers` in v1.3.0** — A silent breaking change in the JSON output field name that broke downstream parsers. **OCX must include `schema_version: 1` at the root of all JSON output from the first release.** Parsers can gate on `schema_version` to handle migrations.

**6. cosign `--experimental-oci11` flag fragmentation** — The flag exists on some commands but not others ([issue #4335](https://github.com/sigstore/cosign/issues/4335)). **OCX must implement OCI v1.1 referrers support uniformly across `ocx verify` and `ocx sbom` from the start, with consistent auto-probe behavior in both.**

**7. `registry:2` false negative on referrers** — The Docker reference registry returns 404 for the referrers API endpoint. A naive implementation that treats 404 as "error" rather than "fall back to tag scheme" will incorrectly report all OCX acceptance tests as failing. The fallback must be implemented and tested. Source: oras discover fallback behavior observation; [OCI distribution spec](https://github.com/opencontainers/distribution-spec/blob/main/spec.md).

---

## Citations

- [OCI Distribution Spec v1.1 — Referrers API](https://github.com/opencontainers/distribution-spec/blob/main/spec.md) — canonical spec for referrers endpoint, tag fallback, OCI-Subject header — accessed 2026-04-19
- [OCI Image and Distribution Specs v1.1 Releases (OCI blog)](https://opencontainers.org/posts/blog/2024-03-13-image-and-distribution-1-1/) — announcement post, ecosystem adoption summary — accessed 2026-04-19
- [sigstore/cosign — cosign_verify.md](https://github.com/sigstore/cosign/blob/main/doc/cosign_verify.md) — complete flag reference for cosign verify — accessed 2026-04-19
- [sigstore/cosign — issue #210 (NDJSON wart)](https://github.com/sigstore/cosign/issues/210) — NDJSON vs JSON array discussion — accessed 2026-04-19
- [sigstore/cosign — issue #2510 (stdout/stderr mixing)](https://github.com/sigstore/cosign/issues/2510) — inappropriate stdout printing — accessed 2026-04-19
- [sigstore/cosign — issue #1370 (improve JSON output)](https://github.com/sigstore/cosign/issues/1370) — JSON output structure discussion — accessed 2026-04-19
- [sigstore/cosign — issue #4335 (OCI 1.1 completeness)](https://github.com/sigstore/cosign/issues/4335) — incomplete OCI v1.1 support across commands — accessed 2026-04-19
- [notaryproject/specifications — trust-store-trust-policy.md](https://github.com/notaryproject/specifications/blob/main/specs/trust-store-trust-policy.md) — trust policy JSON schema, verification levels — accessed 2026-04-19
- [notaryproject/notation — verify.go](https://github.com/notaryproject/notation/blob/main/cmd/notation/verify.go) — notation verify flag surface — accessed 2026-04-19
- [oras.land — oras discover command reference](https://oras.land/docs/commands/oras_discover/) — full flag list, distribution-spec options — accessed 2026-04-19
- [oras v1.3.0-beta.3 blog — enriched discover output](https://oras.land/blog/oras-v1.3.0-beta.3/) — JSON schema with `referrers` rename — accessed 2026-04-19
- [oras-project/oras — discover.go](https://github.com/oras-project/oras/blob/main/cmd/oras/root/discover.go) — distribution-spec flag implementation, exit behavior — accessed 2026-04-19
- [anchore/syft wiki — attestation](https://github.com/anchore/syft/wiki/attestation) — syft attestation artifact types — accessed 2026-04-19
- [trivy.dev — supply chain SBOM docs](https://trivy.dev/latest/docs/supply-chain/sbom/) — trivy SBOM detection methods — accessed 2026-04-19
- [anchore/grype](https://github.com/anchore/grype) — grype SBOM input support — accessed 2026-04-19
- [chainguard.dev — building towards OCI v1.1 in cosign](https://www.chainguard.dev/unchained/building-towards-oci-v1-1-support-in-cosign) — OCI v1.1 referrers API registry support matrix — accessed 2026-04-19
- [AWS Blog — ECR OCI v1.1 support](https://aws.amazon.com/blogs/opensource/diving-into-oci-image-and-distribution-1-1-support-in-amazon-ecr/) — ECR referrers API GA — accessed 2026-04-19
- [JFrog Blog — OCI v1.1 conformance](https://jfrog.com/blog/full-conformance-to-oci-v1-1/) — JFrog Artifactory conformance declaration — accessed 2026-04-19
- [Red Hat Blog — Quay.io OCI v1.1](https://www.redhat.com/en/blog/announcing-open-container-initiativereferrers-api-quayio-step-towards-enhanced-security-and-compliance) — Quay.io referrers API support — accessed 2026-04-19
- [google/go-containerregistry — issue #2205 (tag fallback race)](https://github.com/google/go-containerregistry/issues/2205) — concurrent writers race in tag fallback scheme — accessed 2026-04-19
- [BSD sysexits.h manpage](https://man.freebsd.org/cgi/man.cgi?sysexits) — canonical exit code numeric values — accessed 2026-04-19
