# Discussion: Rekor v2 migration together with a switch to sigstore-rust

State: handed-off → dropped · Updated: 2026-11-23
Ratified: 2026-11-23 → dropped
Confidence: ratified by the owner after the restate ("waiting sounds good", then approval to close and land). The six research files under `## Research` are dated 2026-11-23, rest on source reading and web sources only — nothing was built or run — and expire 2027-02-23 (the Rekor v1 hardening file 2027-05-23).

## Intent

Decide whether to pair the Rekor v2 migration (ocx-sh/ocx#107) with a switch
from the `sigstore` crate (sigstore-rs 0.14 fork, `external/sigstore-rs`) to
`sigstore-rust`, and whether that switch would shrink the hand-written signing
code in `crates/ocx_sign`.

Out of scope: any code, dependency or config change now; any change to how
already-published, Rekor v1-signed packages verify.

## Decisions

- **Wait. No Rekor v2 work and no library switch now** (owner, 2026-11-23).
  Rekor v1 has no sunset date, stays the public default, still accepts both
  entry kinds OCX writes (`hashedrekord`, `dsse:0.0.1`), and a freeze carries
  one year's notice.
- **Reopen on any one trigger:** a Rekor v1 freeze notice; the public-good
  `SigningConfig` switching its default log to Rekor v2; a real package OCX
  must verify that carries a Rekor v2 entry (today ranked unparseable,
  `crates/ocx_sign/src/verify/pipeline.rs:2637`).
- **When reopened, `sigstore-rust` is the route to v2**, in two steps and never
  one change: swap the verifier at Rekor v1 parity, then add v2. It is the only
  Rust library found with v2 write and verify; it also verifies v1. No blocking
  dependency conflict was found.
- **Code reduction is not a reason to switch.** Of about 9,000 non-test lines
  in `crates/ocx_sign`, about 1,080 have a `sigstore-rust` counterpart, about
  950 would be rewritten against its API, about 6,980 stay OCX-own (cosign
  compatibility, OCI referrers, identity policy, SBOM and predicate types,
  transport hardening). The gain is code not written: v2 client, RFC 3161
  timestamp client, `SigningConfig` discovery, and dropping the fork.
- **Correction to ocx-sh/ocx#107's gate:** signing against Rekor v2 was never
  blocked on a library, because OCX owns its Rekor client
  (`crates/ocx_sign/src/sign/rekor.rs`). Only v2 verification was.
- **Preferred adoption depth when reopened (recommendation, not ratified):**
  replace the verify core and take the v2, timestamp and discovery pieces; keep
  OCX's Fulcio, Rekor v1 and OIDC clients, which carry retry, 409 recovery,
  response-size caps and the SSRF-guarded transport.

## Research

- `.agents/research/research_sigstore_dependency_surface.md` (2026-11-23) —
  `sigstore` is used for bundle verification, key types, Rekor models and the
  trust-root trait only; Fulcio, Rekor v1 write, OIDC, keyless signing,
  simplesigning and DSSE are hand-rolled in `ocx_sign`. Fork delta is one
  function. No `SigningConfig` code exists.
- `.agents/research/research_rust_sigstore_clients_2026-11.md` (2026-11-23) —
  `sigstore-rust` has a Rekor v2 client and v2 verify (`hashedrekord/0.0.2`
  only, RFC 3161 timestamp required to sign); `sigstore-rs` shows none. No
  cosign-legacy support found in `sigstore-rust`. It is 0.x with three breaking
  releases in 30 days. Rekor v1 has no sunset date and stays the default.
- `.agents/research/research_rekor_v1_entry_type_hardening_2026-11.md`
  (2026-11-23) — the 2026-11-16 hardening keeps `hashedrekord` and `dsse`
  enabled on Rekor v1; no deadline for OCX. Rekor v2 requires an RFC 3161
  timestamp in the bundle; whether v2 has a `dsse` kind is contradicted between
  two Sigstore sources.
- `.agents/research/research_sigstore_rust_dependency_fit.md` (2026-11-23) —
  no blocking dependency conflict: `aws-lc-rs`, rustls, `reqwest` 0.13.5 on
  both sides, no build scripts, own TUF client with a caller-supplied HTTP
  client (`TufConfig::with_http_client`, which replaces the fork's one patch).
  Drops `openidconnect` → `rsa` and its `RUSTSEC-2023-0071` exception.
  Duplicated majors: `jiff`, `curve25519-dalek`, `fiat-crypto`. Unknown:
  licences of the new crates, crates.io publication of 8 of 12 crates at
  0.17.0, the `ocx-mirror` ripple.
- `.agents/research/research_sigstore_rust_sign_mapping.md` (2026-11-23) —
  sign and attest side, 4837 non-test lines in scope: 195 call `sigstore` today,
  924 are OCX-own with a `sigstore-rust` counterpart (798 of them need a
  non-trivial adapter), 3718 stay OCX-own. `sigstore-rust` adds what OCX lacks:
  Rekor v2 write, a TSA client, `SigningConfig` discovery. Gaps: encrypted-PEM
  keys, KMS, Rekor v1 public-key (key-mode) upload, simplesigning / referrers /
  sidecar, predicate and SBOM types, response-size caps, retry and 409 recovery,
  ambient OIDC detection building its own HTTP client.
- `.agents/research/research_sigstore_rust_verify_mapping.md` (2026-11-23) —
  verify side, 4163 non-test code lines in scope: 751 call `sigstore` and would
  be rewritten, 152 are OCX-own code `sigstore-rust` replaces outright, 3260
  stay OCX-own. Rekor v1 and v2 verify (`hashedrekord/0.0.2` + RFC 3161) are
  covered; `CosignVerificationKey` maps to `DerPublicKey`. Gaps: unlogged or
  caller-timed signatures are rejected, `dsse/0.0.2` fails at bundle parse, no
  TOFU Rekor key, SET and checkpoint primitives are private, no cosign legacy
  (reachable only through a synthetic v0.1 bundle, never run). Semantic
  difference: a v0.1 bundle with a SET and no inclusion proof passes in
  `sigstore-rust`; OCX refuses it today.

## Related

- https://github.com/ocx-sh/ocx/issues/107 — Rekor v2 migration delta; wolfv's
  `sigstore-rust` offer and "old cosign features" caveat in the comments.
- https://github.com/sigstore/sigstore-rust

## Open questions

- Does the cosign-legacy verify path work through a synthetic v0.1 bundle on
  `sigstore-rust`? Reasoned from source only, never built or run.
- Does Rekor v2 have a `dsse` entry kind? `rekor-tiles` `CLIENTS.md` says no;
  the Rekor v2 GA post says yes. `sigstore-rust` rejects `dsse/0.0.2` at parse.
- Are all 12 `sigstore-*` crates published on crates.io at one version, and do
  the new transitive crates pass `deny.toml` licences and advisories?
- Which public types ripple into the `ocx-mirror` satellite (checkout absent
  during research)?
- Do the PEM key forms the fork's loader accepts (for example PKCS#1 RSA) load
  in `sigstore-rust`?
- Does `sigstore-rust`'s Fulcio request (`algorithm: ""`) work against the local
  test stack, and is its bundle JSON byte-compatible with what OCX emits today?

## Verification

The decision is to wait, so nothing is built now. When a trigger reopens it:

- Re-verify the research first; every file under `## Research` expires
  2027-02-23 or 2027-05-23 and `sigstore-rust` was 0.x with breaking releases.
- Step one (verifier swap at v1 parity) is checked by the existing suites
  unchanged: the `ocx_sign` unit tests (about 540), the sign / verify / attest /
  SBOM / trust acceptance modules under `test/tests/` (about 290 tests), and the
  cosign interop matrix (`test_cosign_matrix_*`, `test_cosign_interop`), against
  the local stack in `test/tests/fixtures/sigstore_stack.py`.
- A regression test must show a v0.1 bundle with a signed entry timestamp and
  no inclusion proof is still refused.
- Step two (v2) is checked by ocx-sh/ocx#107's acceptance criteria: sign
  against a Rekor v2 log and verify through the TUF-distributed trust root;
  v1 bundles still verify; no hardcoded Rekor endpoint
  (`DEFAULT_REKOR_URL`, `crates/ocx_oci/src/endpoint.rs:19`); the "v2 not
  supported" note in `crates/ocx_sign/src/sign/rekor.rs:4-6` removed.
