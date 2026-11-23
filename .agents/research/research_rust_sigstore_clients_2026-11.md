# Research: Rust Sigstore clients, 2026-11

## Metadata

**Date:** 2026-11-23
**Domain:** security
**Triggered by:** discussion `.agents/discussions/rekor-v2-sigstore-rust.md` — whether to pair the Rekor v2 migration (ocx-sh/ocx#107) with a switch from `sigstore-rs` to `sigstore-rust`
**Expires:** 2027-02-23 (`sigstore-rust` shipped three breaking 0.x releases in the 30 days before this date; re-verify before any decision)

## Direct Answer

A = `sigstore-rs` (crate `sigstore`). B = `sigstore-rust` (12 `sigstore-*`
crates). B has a Rekor v2 client and v2 verification; no evidence of v2 in A.
B has no found support for cosign legacy formats (simplesigning, `.sig` tags,
legacy bundle, referrers). Neither repository's CHANGELOG nor B's source was
read directly; gaps are marked unconfirmed.

## Findings

### 1. Rekor v2

- B: `RekorV2Client`, `hashedrekord/0.0.2` only, no lookup or search; reads
  checkpoints and tiles, consumers compute proofs; signing needs an RFC 3161
  timestamp
  ([rekor README](https://raw.githubusercontent.com/sigstore/sigstore-rust/main/crates/sigstore-rekor/README.md)).
  PR [#176](https://github.com/sigstore/sigstore-rust/pull/176) "complete and
  harden Rekor v2 support" merged 2026-08-27. Open:
  [#374](https://github.com/sigstore/sigstore-rust/issues/374) /
  [#375](https://github.com/sigstore/sigstore-rust/pull/375) — v2 verification
  rejects P-384 keys (0.16.0 and main, 2026-11-20).
- A: nothing found. The [rekor-evolution blog](https://blog.sigstore.dev/rekor-evolution/)
  (2026-06-28) lists it as lacking v2; release notes through
  [0.15.1](https://github.com/sigstore/sigstore-rs/releases) do not mention v2.

### 2. Rekor v1

- B: v1 client (`hashedrekord/0.0.1`, `dsse/0.0.1`); verifies checkpoints,
  inclusion proofs and SETs ([README](https://github.com/sigstore/sigstore-rust)).
- A: verifies Rekor bundles ([README](https://github.com/sigstore/sigstore-rs)).
- No drop plan found for either.

### 3. Trust material

- B: TUF via `sigstore-tuf`; `SigningContext::production()` loads the signing
  config through TUF; no built-in log URLs since PR
  [#240](https://github.com/sigstore/sigstore-rust/pull/240) (2026-09-24);
  signing without a log landed in v0.16.0.
- A: TUF via `tough` 0.24 behind `sigstore-trust-root`. `SigningConfig` use
  unconfirmed. Open [#626](https://github.com/sigstore/sigstore-rs/issues/626)
  (no custom HTTP client for TUF), PR
  [#663](https://github.com/sigstore/sigstore-rs/pull/663) open for it;
  [#539](https://github.com/sigstore/sigstore-rs/issues/539) (TSA-only
  instances, no maintainer reply).

### 4. Bundle versions

- B: reads and writes v0.1, v0.2, v0.3 (README).
- A: unconfirmed. [#432](https://github.com/sigstore/sigstore-rs/issues/432)
  (bundle 0.3 support, opened 2025-02-06);
  [#608](https://github.com/sigstore/sigstore-rs/issues/608) (DSSE v0.3 bundles
  from `cosign attest-blob` wrongly rejected).

### 5. Cosign legacy

- A: `cosign` module with simple-signing payloads; checks `.sig` tag and
  OCI-referrer bundle signatures
  ([docs.rs](https://docs.rs/crate/sigstore/latest/source/src/cosign/mod.rs));
  v0.14.0 added cosign v3 verification. Verification-focused.
- B: no support found for simplesigning, `.sig` tags, the legacy cosign bundle
  or referrers. No maintainer statement found on "old cosign features" beyond
  the ocx-sh/ocx#107 comment.

### 6. Attestations

- B: DSSE and in-toto subjects with any digest (v0.17.0); Rekor `intoto`
  handling in PR [#175](https://github.com/sigstore/sigstore-rust/pull/175);
  Rekor v2 logs DSSE as `hashedrekord` of the PAE.
- A: README says attestation verification is unsupported;
  [#588](https://github.com/sigstore/sigstore-rs/issues/588).

### 7. OIDC

- B: ambient CI detection (GitHub Actions in README examples); interactive and
  pass-in flows unconfirmed.
- A: OpenID Connect API and an `openidflow` example; ambient detection unconfirmed.

### 8. Engineering

- B: v0.17.0 on 2026-11-22; prior 2026-11-09 and 2026-10-23, many breaking
  changes; targets 1.0. Edition 2021, MSRV 1.86, Apache-2.0, tokio, aws-lc-rs
  default with rustls, optional boring or native-tls
  ([Cargo.toml](https://raw.githubusercontent.com/sigstore/sigstore-rust/main/Cargo.toml)).
  9 open issues. 100% conformance reported in
  [ossf/tac#574](https://github.com/ossf/tac/issues/574); 120 audit hours funded
  (contract signed 2026-05-07).
- A: v0.15.1 on 2026-11-22; 0.15.0 2026-11-05, 0.14.0 2026-05-22, 0.13.0
  2025-10-16. Edition 2024. Defaults `native-tls` plus `full`; `rustls-tls`
  uses aws-lc-rs; `ring` removed in 0.14.0. Apache-2.0. 53 open issues,
  including RUSTSEC unmaintained-dependency advisories. Conformance pass unconfirmed.

### 9. Adoption

- B: mise (`mise-sigstore` on `sigstore-verify` 0.16 —
  [Cargo.toml](https://raw.githubusercontent.com/jdx/mise/main/crates/mise-sigstore/Cargo.toml));
  pnpm's Rust CLI (`sigstore-sign = 0.16.0`; PR
  [#17752](https://github.com/pnpm/pnpm/pull/17752) pins
  `SigningContext::production()` to Rekor v1 because it picks the highest
  listed version); `rattler_sigstore`, `nono`, `rattler_build_source_cache`.
- A: `cargo-bundle`, `bpfman`, `blue-build-process-management`, jdx's
  `sigstore-verification` 0.2.8; about 1.28M total downloads.

### 10. Relationship

- B was donated by prefix.dev (formerly `prefix-dev/sigstore-rust`,
  [ossf/tac#574](https://github.com/ossf/tac/issues/574)). No statement on
  successor, deprecation or merge between A and B.

### Rekor v1 on the public-good instance

- [Rekor v2 GA](https://blog.sigstore.dev/rekor-v2-ga/) (2025-10-10): v1
  frozen "eventually", with one year's notice.
- [Rekor evolution](https://blog.sigstore.dev/rekor-evolution/) (2026-06-28):
  v1 stays the default "for the foreseeable future".
- Hardening post (2026-11-16, blog.sigstore.dev): covers disabling some entry
  types; nothing on a freeze. Which types and when: not established.
- No freeze or sunset date announced. Status page and mailing list not checked.

## negative

- No A-side v2 evidence (absence of evidence, not proof).
- B's 0.x cadence and breaking changes are heavy (three releases in 30 days).
- A's licence, MSRV and openssl default are stated differently across mirrors.

## leads

- [sigstore-conformance](https://github.com/sigstore/sigstore-conformance) CI
  results for either library.
- B's source or docs.rs for simplesigning and referrers.
- PR [#375](https://github.com/sigstore/sigstore-rust/pull/375) once merged.
- The entry types disabled by the 2026-11-16 hardening post — OCX writes
  `dsse:0.0.1` on v1.
