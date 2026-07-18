# PR-FAQ: OCX Ships Built-in Signing and Verification

## Press Release

**OCX v0.X.Y introduces `ocx package sign` and `ocx verify` — cosign-compatible supply-chain guarantees with zero external tooling.**

OCX is the first general-purpose binary package manager built on the OCI distribution spec. Starting with v0.X.Y, OCX ships end-to-end Sigstore-backed signing and verification as core commands, eliminating the need to install `cosign` or wire up separate OIDC plumbing in CI pipelines.

The release completes a supply-chain loop that previously required two tools: publishers signed with `cosign sign`, consumers verified with `cosign verify`, and OCX users maintained a parallel dependency on cosign's CLI semantics. With v0.X.Y, both operations are native to OCX and interoperable with the wider Sigstore ecosystem.

**What ships:**

- `ocx package sign <REFERENCE>` — cosign keyless signing producing a Sigstore bundle v0.3 pushed via the OCI Referrers API. Automatic OIDC token acquisition on GitHub Actions, GitLab CI, CircleCI, Buildkite, and GCP Cloud Build. Browser PKCE for laptop workflows.
- `ocx verify <REFERENCE> --certificate-identity <I> --certificate-oidc-issuer <O>` — strict keyless verification: Fulcio cert chain, Rekor transparency log SET, and OIDC identity match. No escape hatches. No skip levels. Fails closed.
- Full interoperability: bundles signed by `ocx package sign` verify with `cosign verify`, and vice versa (external signature discovery lands in the next release).
- Typed exit codes: every failure maps to a distinct sysexits-aligned code — CI scripts branch on `$?` without parsing stderr.
- `--format json` produces typed error envelopes with `schema_version: 1`. Bazel rules, GitHub Actions steps, and custom pipelines consume signing and verification results programmatically.

**What stays out of scope** (by design, not omission): SBOM discovery, external signature formats, DSSE attestations, TOML trust-policy files, HSM-backed signing, and Notation support. Each has a documented path forward in future releases; none is a silent blocker for v0.X.Y.

**Why now.** Supply-chain integrity is a 2026 baseline for any package manager touching CI. Nix, Homebrew, and language-specific PMs ship piecemeal stories. OCX is the first to deliver a *single-binary* keyless signing flow grounded in the OCI distribution spec — meaning organizations that already run an OCI registry get supply-chain guarantees at zero additional infrastructure cost.

---

## FAQ

### What does "half-product" mean, and why does this release matter?

A pre-release version of this feature shipped verification-only with no real enforcement — the only trust level was `skip`. Users rejected it: verify-only without a way to sign from OCX itself meant teams still needed a parallel cosign install in CI, defeating the "single tool" property that differentiates OCX. v0.X.Y ships both sides of the loop as real, enforcing operations.

### Can I still use `cosign` alongside OCX?

Yes. Bundles produced by `ocx package sign` are verifiable by `cosign verify` against the same identity/issuer flags, and the inverse lands in the next release (external signature discovery reads cosign's legacy tag-based signatures). Mixed pipelines — one tool signing, the other verifying — work today for OCX-signed artifacts.

### Which CI platforms work without extra configuration?

Ambient OIDC detection via the `ci-id` crate covers GitHub Actions, GitLab CI, CircleCI, Buildkite, and Google Cloud Build at launch. Each platform's ambient token is detected automatically. For any other CI, pass `--identity-token <TOKEN>` with a pre-fetched OIDC token.

The most common failure on GitHub Actions — forgetting `permissions: id-token: write` — produces a typed error at exit code 77 with a remediation pointing at the exact GitHub docs URL.

### Why no `--insecure-ignore-tlog` or `skip` mode?

Escape hatches degrade the security property users install this feature for. If a consumer needs to accept an unsigned or wrong-identity package, they can choose not to run `ocx verify` — but once they do run it, the answer is binary. Cosign has `--insecure-ignore-tlog`; OCX does not, by design. Users who need that mode can continue using cosign directly.

### What happens on a registry that doesn't support the Referrers API?

`ocx package sign` exits 69 (`Unavailable`) with a specific error indicating the registry lacks spec-v1.1 support. OCX deliberately does not write fallback `.sig` tags — the parent ADR (`adr_oci_artifact_enrichment.md`) rules out fallback tags to preserve a single source of truth and avoid GC races.

GHCR and Docker Hub, as of April 2026, do not yet support the Referrers API. Alternatives: use a compliant registry (OCX's default `ocx.sh`, Harbor, Azure Container Registry, Zot), wait for GHCR support, or sign externally with cosign. The next release adds external-signature discovery so OCX verify will accept cosign-written legacy signatures.

### How are exit codes structured?

OCX aligns with BSD `sysexits.h`:

- `0` success
- `64` bad CLI invocation
- `65` corrupted data (malformed bundle, bad cert chain)
- `69` service unavailable (registry 5xx, Fulcio down, Referrers API missing)
- `74` local I/O error
- `75` rate-limited (honor Retry-After)
- `77` permission denied (registry 403, offline rejected for sign, OIDC pre-check failure)
- `79` no signatures found
- `80` auth error (registry 401, Fulcio 401, identity mismatch)
- `82` Rekor unavailable (distinct from generic 69 so CI scripts can branch on "retry later" vs "registry broken")

Exit code `78` is reserved for the forthcoming TOML trust-policy parse path; exit code `79` doubles as "trust-policy file not found" when the TOML path lands.

### What about DSSE attestations and `ocx package attest`?

Shipping signing without attestations is intentional. The pinned sigstore-rs crate (v0.13) does not expose a DSSE signer; the functionality lands when sigstore-rs 0.14 does. Shipping attestations on a forked sigstore-rs was considered and rejected — the maintenance cost of a fork exceeds the user-facing value for one feature.

### Can I verify a package signed three years ago?

Yes, if the signature was made when the cert was valid and the Rekor SET is intact. OCX follows cosign's policy: expired cert + valid Rekor SET = valid signature. The transparency log is the temporal anchor; the cert's expiry reflects when it was issued, not when it stopped being legitimate.

### How does this interact with `--offline`?

`ocx package sign --offline` is rejected with exit 77. Signing requires Fulcio and Rekor, both of which are online services; there is no coherent offline semantics. `ocx verify --offline` is also rejected for the same reason — verification requires fetching the referrer manifest from the registry and the TUF root update check.

Offline air-gap workflows are a real user need but out of scope for v1. The next signing release may add a staged-signing protocol.

### Will `ocx install` auto-verify in the future?

Not by default. Auto-verify on install would require every OCX user to produce signatures (or every install to fail closed), which is a breaking change for the primary automation use case. Auto-verify remains opt-in via explicit `ocx verify && ocx install` sequencing. A future release may add a config-file flag to enable auto-verify per-repo, but v1 does not ship it.

### What if Rekor is down?

`ocx package sign` exits 82 (`RekorUnavailable`), distinct from 69 (generic unavailable). The Fulcio cert has been obtained but cannot be persisted to the transparency log; OCX discards the partial state rather than writing a half-complete bundle. On the verify side, Rekor unreachable produces the same 82 — the SET cannot be validated, so verification fails closed.

This split of exit codes (82 for Rekor specifically) lets CI scripts distinguish "the whole registry is broken, stop trying" (69) from "the transparency log is having a bad day, retry in 15 minutes" (82).

### How is this tested without depending on live Sigstore?

Three tiers:

1. **Pre-generated deterministic fixtures** under `test/fixtures/signing/` cover the unit layer. Fixtures are regenerated manually by maintainers against Sigstore staging and committed; CI never regenerates them.
2. **Opt-in integration tests** against Sigstore staging (`fulcio.sigstage.dev`, `rekor.sigstage.dev`) run behind an env flag. They skip gracefully if staging is unavailable.
3. **Cross-tool interop** asserts OCX-signed bundles verify with `cosign verify` (cosign is installed via `ocx install cosign` in CI setup). The reverse direction — `cosign sign` producing a fixture OCX verifies — is explicitly avoided to keep the fixture surface independent of cosign's versioning.

### Does this introduce new runtime dependencies?

Two:

- `sigstore = "=0.13"` — pinned; the wire format is stable and upgrading is a deliberate event.
- `ci-id = "0.3"` — ambient OIDC detection.

Both are existing dependencies in the Rust OCI and Sigstore ecosystems and have no transitive surprises. No native deps, no C bindings.

### What's in the next release?

The Slice 2 plan adds:

- `ocx sbom <REFERENCE>` — SBOM discovery and display (SPDX 2.3 + CycloneDX parsing).
- External signature discovery — `ocx verify` picks up cosign legacy `.sig` tags on registries without Referrers API (GHCR, Docker Hub), so signatures produced outside OCX verify natively.
- Referrer-index caching — a 1h/24h TTL cache for referrer lookups.

Both the ADR (`adr_oci_referrers_signing_v1.md`) and PRD include forward-compat hooks that Slice 2 plugs into without breaking the v1 surface.

### Where do I read the full design?

- Architectural decisions: `.claude/artifacts/adr_oci_referrers_signing_v1.md`
- User-facing requirements and scenarios: `.claude/artifacts/prd_oci_referrers_signing_v1.md`
- Implementation plan: `.claude/state/plans/plan_slice1_sign_and_verify.md`
- Parent ADR (media types, referrer subject rules): `.claude/artifacts/adr_oci_artifact_enrichment.md` §Amendment 2026-04-19
- Issue: [#24 OCI referrers API for signature and SBOM discovery](https://github.com/ocx-sh/ocx/issues/24)
