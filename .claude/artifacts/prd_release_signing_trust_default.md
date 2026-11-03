State: handed-off → architect · Updated: 2026-10-25
Participants: owner + hex-discuss; one session, 2026-10-25
Ratified: 2026-10-25 → architect
Confidence: owner directed the hand-off to `/hex-architect` for a state-of-the-art analysis; research vintages 2026-10-25 (5 lanes, listed below)

# Discussion: Signing OCX releases and trusting publishers without a client release

## Intent

Owner, in their words: support signing of our releases, also sign older releases, for ocx and ocx-mirror (and the other own products, e.g. grimoire), and decide on a default trust policy for the ocx.sh index — which signatures are accepted and how it is reconfigured, e.g. someone mirroring upstream locally who only accepts their own signatures.

The decisive requirement surfaced late: **like npm trusted publishing** — when someone pushes a *new* repository or product into the ocx.sh index, they declare who signs it (their workflow), and that is then honoured by every ocx instance. **This must never require a new ocx release.** Signer changes (add, rotate, revoke) must not break older releases, verification must work offline, and where that trust data lives and how its mutation is handled must be answered.

Why now: the signing and verification machinery is built (`crates/ocx_sign`, `crates/ocx_trust`) but none of the project's own releases are signed, and no default policy exists, so verification is opt-in everywhere.

## Requirements

Phase 1 — release signing (independent of phase 2; plan-sized):

- Sign every OCI release of the own products keylessly (Sigstore, GitHub OIDC/Fulcio) with `ocx package sign`: ocx `cli` (`.github/workflows/release.yml`, `oci-publish.yml`, `post-release-oci-publish.yml`), ocx-mirror (`ocx-mirror/.github/workflows/release.yml`, `oci-publish.yml`), grimoire. Template: `.github/workflows/sdkgen-publish.yml:209-256` (separate `sign` job, only job with `id-token: write`, signs `<repo>@<index digest>`).
- A manually triggered action that signs all older releases lacking a valid signature. Existing code: `ocx-mirror package pipeline sign` (`ocx-mirror/crates/ocx_mirror_pipeline/src/sign_backfill.rs`) signs in place, skips by subject digest, `--identity`/`--issuer` narrow "already signed" by exact match. Unverified: whether it can target a package with no mirror spec (`ocx.sh/ocx/cli`, grimoire).
- Every signing job ends with a self-verify: `ocx package verify --certificate-identity <X> --certificate-oidc-issuer <Y> <ref>@<digest>`. `ocx package sign` reads the leaf SAN and read-backs its sidecar write (`crates/ocx_sign/src/sign/pipeline.rs:138-144`, `:1135-1200`) but never verifies against an expected identity.
- Ship phase 1 **without** any verification policy.
- Owner wants **one stable signer identity per project family** (one for everything OCX, one for grimoire) — no per-release regexes. The default keyless SAN `…/<wf>.yml@refs/tags/<tag>` changes per release; the phase-1 identity scheme must be compatible with whatever phase 2 decides, so the ADR settles it.

Phase 2 — the ADR: how an ocx client learns which signer identities are trusted for a package, satisfying all of:

1. **Self-service onboarding without an ocx release.** A publisher adding a new package/namespace to the ocx.sh index declares its signer; every client honours it. Future grimoire maintainers or third parties add their own.
2. **Change safety.** Adding, rotating or revoking a signer must not break verification of older releases, and must handle a compromised signer.
3. **Offline.** Air-gapped, `--offline`, and frozen `ocx.lock` installs keep verifying.
4. **Operator override.** An operator can replace the default for a scope, e.g. a private mirror that accepts only its own signatures.
5. **Storage and mutation.** Where trust data lives, who may change it, how a change propagates, and how clients detect stale or rolled-back trust data.

Constraint (owner): **derive the design from established prior art** — this problem is solved elsewhere. Candidates to evaluate (none pre-chosen): TUF with delegated targets (PEP 458/480, Notary v1/Docker Content Trust, Uptane, Datadog in-toto+TUF, tuf-on-ci, RSTUF), signed repository metadata (APT/RPM repo signing, Nix binary-cache keys, Go checksum DB), registry-side trusted publishing + provenance (npm, PyPI incl. pending publishers, crates.io, RubyGems, NuGet, JSR), Sigstore `trusted_root.json`/`signing_config`, Notation trust policy + trust stores, Homebrew attestations, OpenSSF Securing Software Repositories WG guidance.

## Decisions

- Staged: phase 1 (signing + backfill + self-verify, no policy) ships before phase 2 (trust distribution).
- Keyless Sigstore via GitHub OIDC for all own repos; no long-lived keys.
- Phase-2 scope may cover all of `ocx.sh/*` — the owner states the index currently holds only project-controlled content (ocx, grimoire, and mirror-published packages).
- **Rejected as starting points:** four designs improvised during this discussion — built-in client anchors for the own families; a mutable per-package `signers` list in the index root; append-only signer entries with validity windows; recording the verifying signer in `ocx.lock`. The owner rejected them as bespoke ("no one else uses it like that"). Re-propose none unless prior art independently supports it.

## Out of scope

Authenticode / Windows shim signing (`build-windows-shims.yml` placeholder); signing GitHub release tarballs and the install script (unless the ADR finds it necessary); the SBOM dogfood ([ocx-sh/ocx#200](https://github.com/ocx-sh/ocx/issues/200)); the Sigstore public-good trust root itself (settled by `.claude/artifacts/adr_offline_verify_trust_cache.md`).

## Current state (facts, 2026-10-25)

- No built-in or default policy. `Config::default().trust_policies()` is empty (`crates/ocx_config/src/lib.rs:1480`). With no matching policy, install skips verification with an INFO line (`crates/ocx_package_manager/src/tasks/auto_verify.rs:129-131`); with no operator policy, auto-verify is not attached (`crates/ocx_cli/src/app/context.rs:401-402`). A matching policy that fails aborts the install.
- `[[trust.policy]]` grammar: scope prefix or include/exclude; `signers` ANY-of keyless `{identity | identity_regexp, oidc_issuer}` or key. Tiers System/User/Home/Managed/Explicit; the operator tier decides when it matches; the project `ocx.toml` is ignored then; System is locked (`crates/ocx_trust/src/lib.rs:160-466`, `:839-901`). It matches SAN + issuer only. Fulcio's immutable owner/repo-ID extensions are unused, and so are they in sigstore-rs.
- Managed-config distribution exists: `ocx config push` publishes an operator config as an OCI artifact; a digest-pinned `[managed] source` consumes it (`.claude/artifacts/adr_offline_verify_trust_cache.md` amendment §5).
- Index: root `p/<ns>/<pkg>.json` carries `repository`, `owners[]` (login + numeric GitHub id), `upstream`, `tags` → digest (`ocx-sh/index` `schema/root.schema.json`; client struct `crates/ocx_index/src/wire.rs:54`, no `deny_unknown_fields`). The index bot auto-merges owner-authored tag refreshes; edits to `repository`/`owners` need human review (`ocx-sh/index` `CONTRIBUTING.md:25-57`). Clients cache roots locally (`~/.ocx/index/<host>/p/…`).
- `ocx.lock` (lock_version 3) records repository + per-platform digests only.
- Offline Sigstore trust material: six-rung trusted-root ladder plus a 24 h cache (`.claude/artifacts/adr_offline_verify_trust_cache.md`).
- `ocx package copy` and `ocx-mirror registry sync` carry upstream signature referrers verbatim. Verify caps at 8 candidates ([ocx-sh/ocx#403](https://github.com/ocx-sh/ocx/issues/403)).
- No ADR decides a default policy or self-signing of releases. Related issues: [ocx-sh/ocx#199](https://github.com/ocx-sh/ocx/issues/199), [#102](https://github.com/ocx-sh/ocx/issues/102), [#108](https://github.com/ocx-sh/ocx/issues/108), [#109](https://github.com/ocx-sh/ocx/issues/109), [ocx-sh/ocx-mirror#7](https://github.com/ocx-sh/ocx-mirror/issues/7).

## Open questions

- [NEEDS CLARIFICATION: Which signer-identity scheme do the own repos use, so that phase-1 signatures stay valid under the phase-2 model?]
  Recommended: settle in the ADR first, then let phase 1 sign under it — re-signing later appends referrers against the 8-candidate cap.
- [NEEDS CLARIFICATION: Does phase 1 wait for the ADR?]
  Recommended: only the identity-scheme decision blocks it.
- Who signs what: the publisher's artifacts, the index's metadata, or both — and what an ocx client embeds as its single anchor.

## Verification

The ADR is acceptable when it:

1. maps each of phase-2 requirements 1–5 to a named mechanism with cited adopters;
2. states the threat model — index host or repo compromise, registry compromise, publisher CI compromise, rollback/freeze of trust data — and which mechanism covers which threat;
3. specifies every wire/persisted change (index root schema, `ocx.lock`, config, verify semantics) with read-path backward compatibility for published packages, per `CLAUDE.md` § Stability tiers;
4. sketches acceptance tests: a signer rotation keeps an old release verifying; a package onboarded by index PR verifies on an unchanged ocx binary; an offline frozen install verifies; an operator override replaces the default for its scope.

Phase 1 is verified by a signing job's self-verify step going red on a deliberately wrong expected identity and green on the right one.

## Research

Working notes from five research lanes. Only `research_stable_signer_identity.md` is published, in `.claude/artifacts/`; the other lane files were not kept in the repository.

- `.agents/research/research_release_signing_prior_art.md` — retro-signing, reusable-workflow identity, default-trust override models, mirror re-signing
- `.agents/research/research_signer_binding_ecosystems.md` — signer binding across npm/PyPI/crates/RubyGems/NuGet/Maven/Homebrew/gh/Notation/DCT/Go sumdb
- `.agents/research/research_trust_metadata_distribution.md` — TUF / tuf-on-ci / Sigstore TUF / alternatives × threats × cost
- `.agents/research/research_stable_signer_identity.md` — constant-SAN workflow (uv/ruff/Chainguard), central service, extension matching, KMS, gh org attestations, regex pitfalls
- `.agents/research/research_release_signing_codebase.md` — codebase recon (workflows, sign CLI, trust tiers, copy/mirror, ADRs, open issues)

Gap the owner named: these lanes covered adjacent facets. None was a state-of-the-art comparison of how established systems solve requirements 1–5 *together*. That analysis is the ADR's research phase.
