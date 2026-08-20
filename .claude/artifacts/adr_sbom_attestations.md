The runtime "more-than-one" check is deliberately NOT written in v1: a single `Option<KeylessMatcher>` field cannot express two backends, so the check's red state would be unreachable (an Unchecked Green). When `[trust.policy.key]` lands, the `ok_or_else` on `keyless` must become a real exactly-one refusal — never an `.or_else` chain that silently first-wins.# ADR: SBOM and DSSE Attestations over OCI Referrers

- **Status:** Proposed
- **Date:** 2026-08-20
- **Deciders:** mherwig
- **Supersedes:** [`adr_oci_referrers_discovery_v2.md`](./adr_oci_referrers_discovery_v2.md) (already marked SUPERSEDED); [`adr_sbom_strategy.md`](./adr_sbom_strategy.md) Phase 3
- **Amends:** [`adr_oci_referrers_signing_v1.md`](./adr_oci_referrers_signing_v1.md) §S1-D (DSSE deferral dissolved), §"Forward-Compat Hooks for v2"; [`adr_trust_policy.md`](./adr_trust_policy.md) D1 (TOML schema — keyless nesting); [`adr_oci_artifact_enrichment.md`](./adr_oci_artifact_enrichment.md) Amendment 2026-04-19 ("DSSE/in-toto deferred" line)
- **Source of the fixed decisions:** [`plan_milestone_split_supply_chain.md`](./plan_milestone_split_supply_chain.md) "Amendment (2026-08-20)"

## Context

Milestone 5 shipped keyless Sigstore *signing*: real X.509 chain walk, SCT
verification, standard Rekor SET + Merkle inclusion proof, TUF-distributed trust
root, cosign v3 bundle interop, and a self-hostable Fulcio/Rekor/TesseraCT/dex
stack the acceptance suite runs against. What it signs is a **message
signature** over one per-platform manifest digest — `MessageSignature` content in
a Sigstore bundle v0.3, `hashedrekord:0.0.1` in the log.

An SBOM is not a signature over a digest. It is a *document about* an artifact,
which means the signed payload is a **DSSE envelope** carrying an in-toto
Statement whose `subject[].digest.sha256` names the artifact. Every other layer —
Fulcio, the certificate, the transparency log, the trust root, identity
matching, the OCI referrer that attaches it — is the same machinery already
shipped. `adr_oci_referrers_signing_v1.md` §S1-D deferred DSSE on the grounds
that sigstore-rs 0.13 had no DSSE path; sigstore-rs 0.14 has a complete one, and
that rationale is now dissolved.

The genuine delta is therefore narrow and precisely nameable:

1. DSSE PAE encoding, and its verification.
2. The `dsse` Rekor entry type (write and read), alongside `hashedrekord`.
3. An in-toto Statement payload path through `Signer` and bundle assembly.
4. Referrer candidates discriminated by parsed bundle content, with cosign's
   annotations written on the push side and read as ordering hints.
5. A CycloneDX reader for `ocx package sbom`.

Everything else in this ADR is composition of shipped parts. Two facts from the
code make that concrete, and both remove work that a naive plan would schedule:

- The verify pipeline's DSSE rejection is **one `matches!` expression**
  (`crates/ocx_lib/src/oci/verify/pipeline.rs:498`), test-pinned by
  `from_bundle_rejects_dsse_envelope` at `:985`. Every step around it —
  capability probe, referrer listing, trust-root resolution, chain walk, SCT,
  SET, Merkle, identity match — is content-agnostic and reusable unchanged.
- The read transport already carries `artifact_type` **and** `annotations`
  through `list_referrers` into `oci::Descriptor`, so consuming an annotation
  needs no transport change and no second fetch. The **write** side does not:
  `ReferrerManifest` (`crates/ocx_lib/src/oci/referrer/manifest.rs:25-46`) has
  six fields and no `annotations`, so OCX cannot emit one today. That field is
  the one genuinely new piece of the referrer shape (D1, Part IV).

The threat model is unchanged and is the reason for the length of Part III: the
registry is untrusted, the referrer's `subject` linkage is *unsigned registry
metadata*, and a validly signed attestation for artifact A can be served as a
referrer of artifact B (CVE-2026-31830). Authenticity and binding are separate
properties and both are mandatory.

## Decision Drivers

| # | Driver | Consequence in this ADR |
|---|---|---|
| 1 | **cosign interop is the compatibility criterion.** `cosign verify-attestation` must read what `ocx package attest` writes, and `ocx package verify` must read what `cosign attest` writes. | Wire shape follows cosign v3.1.3 exactly (D1); Statement `_type` acceptance is deliberately wider than the spec recommends (D-b). |
| 2 | **Reuse over invention.** Signing, trust, identity, referrers, capability caching, error taxonomy and exit codes all exist. | No new error family, no new exit code, no second verify pipeline, no `SbomFormat` trait (D-d, D-h, D-i). |
| 3 | **Fail closed, and never claim a control that does not exist** (SEC-32). | Verification is on by default everywhere, including `ocx package sbom`; the docs state plainly what attestation does *not* prove (freshness, rollback). |
| 4 | **Untrusted bytes are bounded** (PKG-04…07). | Attestations get their own named caps, not a silent reuse of the 512 KiB signature-bundle cap (D-h, Part IV). |
| 5 | **Nothing has shipped.** v0.5.8 predates the signing surface. | Pre-release renames land as plain edits — no aliases, no dual parsing, no deprecation window (D3). |
| 6 | **The spike must not be able to move the architecture.** | Part V names the exact constants and serialization details the day-1 cosign/Rekor spike may adjust, and states what it may not. |

---

# Part I — Owner-Fixed Decisions

Recorded with rationale. **Not open for re-litigation.** Numbering follows the
2026-08-20 amendment in `plan_milestone_split_supply_chain.md`.

## D1 — Storage shape: cosign v3 bundle over OCI referrers

A DSSE-enveloped in-toto Statement inside a Sigstore bundle v0.3, pushed as an
OCI 1.1 referrer of the per-platform manifest, with `artifactType:
application/vnd.dev.sigstore.bundle.v0.3+json` and cosign's **three**
annotations.

**Rationale.** It is the only shape with an existing verifier ecosystem, and it
reuses OCX's shipped referrer push/list path verbatim.

**The annotation set is cosign's, in full.** `WriteAttestationNewBundleFormat`
writes three keys, and OCX writes all three
(`research_cosign_v3_attestation_wire.md` §1):

| Key | Value |
|---|---|
| `org.opencontainers.image.created` | RFC 3339 with an explicit `Z`; `SOURCE_DATE_EPOCH` when set, else now |
| `dev.sigstore.bundle.content` | `dsse-envelope` |
| `dev.sigstore.bundle.predicateType` | the resolved predicateType URI |

Writing two rather than three would fork the wire shape from cosign's for the
same artifact, and **the annotation set is a one-way door**: the manifest's
SHA-256 *is* the referrer's registry address, so bytes already pushed can never
be migrated. It is therefore decided here rather than left to the spike, and it
joins Part V's MAY-NOT list.

`created` reads the clock, which normally forfeits a reproducible referrer
digest. Here it forfeits nothing that was available: each attest run mints a
fresh ephemeral certificate and a fresh Rekor entry, so the bundle blob — and
therefore the referrer manifest that names its digest — is unique per run
regardless (S1-I append-only, D-f). `created` adds no *new* nondeterminism, and
the golden-shape fixture asserts every field except that one.

**Signature referrers gain the same treatment.** They start carrying `created`
plus `dev.sigstore.bundle.content: message-signature`, which is cosign parity
and is what makes a signature referrer distinguishable from an attestation
referrer in a listing. This changes the bytes — and therefore the digest — of a
signature referrer manifest. **Saying it out loud:** the signing surface is
unreleased (v0.5.8 predates it, D5/Driver 5), so this is a plain edit with no
migration, and the signature golden fixtures are regenerated in the same change.

Considered and rejected, recorded so they are not re-proposed:

| Alternative | Why rejected |
|---|---|
| SBOM as a package layer | Breaks every deployed client — `pull` extracts all layers untyped. Pays the bytes on every install, for a document almost no install reads. |
| BuildKit-style in-index attestation manifest | cosign#2688 closed not-planned. Unsigned, digest-mutating (the index digest changes when an attestation is added, invalidating every existing pin), no ecosystem convergence. |
| Config-blob embedding | Taxes every metadata-first resolve with bytes it does not need. |

## D2 — CLI: one general verb

`ocx package attest --predicate FILE --type TYPE`, with cosign's type
vocabulary. `ocx package push --sbom FILE` stays as sugar over the cyclonedx
path. Attach parity across `cyclonedx` / `spdx` / `spdxjson`; **parse and
summarize CycloneDX 1.5–1.7 only** in v1 (`ocx package sbom`), stated in the
docs. No `SbomFormat` abstraction for a second parser that does not exist.

**Rationale.** Attaching is format-blind — the bytes go in a predicate field
either way. Parsing is not. One general verb keeps the CLI surface flat and
matches cosign's own shape, so a user's cosign muscle memory transfers.

## D3 — Pre-release renames land first

Three renames, all plain edits with no compatibility shim (nothing shipped):

| From | To | Why |
|---|---|---|
| `--trusted-root` | `--sigstore-trusted-root` | Matches `OCX_SIGSTORE_TRUSTED_ROOT` and the `[trust.sigstore]` config sub-table. One noun for one concept. |
| `[[trust.policy]]` flat keyless matchers | `[trust.policy.keyless]` sub-table (identity / identity_regexp / oidc_issuer), exactly-one-backend validation, future `[trust.policy.key]` slot | A key-based backend is a real future; retrofitting the nesting after publication would be a config break. `builder` (#103) stays a **top-level sibling of `scope`** — it is backend-independent. |
| JSON envelope slug `rekor_unavailable`, **and the Rust identifiers beside it** — `ExitCode::TransparencyLogUnavailable`, `VerifyErrorKind::TransparencyLogUnavailable`, `SignErrorKind::TransparencyLogUnavailable`, `ErrorCategory::TransparencyLogUnavailable` (the enum that *produces* the `error.kind` slug via `#[serde(rename_all = "snake_case")]`) | `transparency_log_unavailable` / `TransparencyLogUnavailable` | Exit code 83 unchanged. The failure is "the transparency log is unreachable", which will outlive the name "Rekor". Slug and identifier move together, or the code says one thing and the contract another. |

## D4 — Keyless-only stays

Browser PKCE remains deferred. The publisher flow is CI ambient detection or
token via file/stdin/env. The corporate-laptop path is a self-hosted Fulcio via
`[trust.sigstore]` (shipped). KMS and key signers stay a v2 `Signer` seam.

## D5 — #104 OSV scan takes exit code 85

83 and 84 are taken (`TransparencyLogUnavailable`, `ReferrersUnsupported`). Acceptance
tests use a local OSV querybatch stub, never osv.dev.

**Consequence for this ADR: attestations introduce no new exit code.** 85 is
spoken for; every new failure here maps onto the existing table (D-h).

## D6 — #200 dogfood is blocked, not dropped

OCX's own physical registry is GHCR, which implements no Referrers API and has
no roadmap item for one. Revisit when the self-publish target moves to a
referrers-capable registry.

## D7 — #198 shrinks; a spike is required

Verify-side machinery shipped with milestone 5. The genuine delta is the
five-item list in Context. **Spike required:** Rekor v1.4.2's acceptance of the
`dsse` / `intoto` entry kinds against the compose stack.

## D8 — Docs placement

Publisher CI guides and the threat model land under the existing
`website/src/docs/` structure (`in-depth/` precedent — `guides/` does not
exist). SBOM signing and verification acceptance tests run against the real
docker-compose Sigstore stack, including negative paths.

---

# Part II — Settled Design Decisions

Ten questions the amendment left open. Each is decided here with its
alternatives, so implementation never re-derives them.

## D-a — The signer's payload path: a second trait method

`Signer` today signs a digest:

```rust
async fn sign(&self, target_digest: &Digest, token: &OidcToken,
              fulcio_url: &Url, rekor_url: &Url) -> Result<SignedBundle, SignErrorKind>;
```

DSSE signs `PAE(payloadType, payload)` and logs a different Rekor entry kind, so
the middle of the flow genuinely diverges. The two ends — ephemeral key, PoP,
Fulcio certificate, bundle assembly — do not.

| Option | Trade-off |
|---|---|
| **(chosen) Add `sign_dsse` to `Signer`; extract the shared Fulcio half into a private helper** | Zero churn at the existing call site and in `SignPipeline`. The two methods differ exactly where the protocols differ. Cost: the trait grows to two methods, and an implementor that only wants one must still supply both. |
| Generalize `sign` to take a `SigningPayload` enum | Every existing implementor, call site and test changes for no behavioural gain; the enum is matched inside the impl immediately, so the branch merely moves. |
| A separate `DsseSigner` trait | Two traits with one implementor each (`KeylessSigner` would implement both) — ARCH-07 says a trait is earned by a second implementation or an exercised double. It would also duplicate the Fulcio half or force a third shared type. |

**Decision.** Extend the existing trait. The payload type is **not** a
parameter: v1 writes exactly one (`application/vnd.in-toto+json`), so it is a
constant, not a stringly-typed argument (ARCH-05). When a second payload type
appears it becomes an enum then, not before.

## D-b — Statement construction and the `_type` tension

cosign v3.1.3 still *writes* `https://in-toto.io/Statement/v0.1`. The security
checklist (row 18) recommends rejecting anything that is not
`https://in-toto.io/Statement/v1`.

| Option | Trade-off |
|---|---|
| Write v0.1, accept v0.1 only | Maximum cosign symmetry, but freezes OCX to a legacy `_type` forever and fails row 18 outright. |
| Write v1, accept v1 only | Spec-clean, and makes `ocx package verify` reject **every cosign-produced attestation in existence** — the interop criterion D1 fixes. |
| **(chosen) Write v1; accept exactly `{v1, v0.1}`** | Strict producer, tolerant consumer, at the correct layer. The two differ only in the `_type` string — v1's ResourceDescriptor adds optional fields and keeps `name` + `digest` — so acceptance is a two-element allowlist, not a second parser. |

**Decision.** `STATEMENT_TYPE_WRITTEN = "https://in-toto.io/Statement/v1"`;
`ACCEPTED_STATEMENT_TYPES = &[v1, "https://in-toto.io/Statement/v0.1"]`.

This is a **documented deviation from checklist row 18**, and the deviation is
narrow: the security value of that row is "do not accept an arbitrary `_type`",
which a closed two-element allowlist preserves in full. Recorded in
`signing.md` so no later reviewer reads it as an oversight.

Subject construction follows cosign: `name` = the bare physical repository path
(informational — row 4 makes `digest` the only authoritative binding),
`digest` = `{"sha256": "<hex, no algorithm prefix>"}` of the per-platform
manifest OCX resolved itself.

**Determinism, stated against what this workspace actually does.**
`crates/ocx_lib/Cargo.toml:50` enables `serde_json`'s `preserve_order`, whose own
comment records that the backing map becomes `indexmap` and that the blast radius
is workspace-wide via feature unification. So `serde_json::Map` here is
**insertion-ordered, not sorted**, and any claim resting on a `BTreeMap`-backed
map would be false (DATA-DET-03 names this feature as the thing that flips it).

The contract is therefore not canonicalization but **byte fidelity per input**:

- The predicate is embedded **verbatim** as a `serde_json::value::RawValue`
  spliced into the Statement, never round-tripped through a `Value`. Whatever
  bytes the `--predicate` file held — pretty-printed, whatever key order,
  whatever number spelling — are the bytes inside the payload.
- The Statement's own wrapper fields are built from typed values in a fixed
  declaration order, so the same inputs produce the same Statement bytes.
- Verification hashes the bytes **received**, never a re-serialization
  (DATA-DIG-04), so no canonical form has to be agreed with any other producer.

Consequently `serde_json_canonicalizer` (already a workspace dependency) is
deliberately **not** used on this path: there is nothing to canonicalize when
nothing is ever re-serialized. Floats in a predicate survive byte-exactly for the
same reason, and no reproducibility claim depends on how they are spelled.

## D-c — Predicate-type vocabulary

D2 fixed "cosign's type vocabulary". The open trap the research surfaced: bare
`slsaprovenance` resolves to `https://slsa.dev/provenance/v0.2`, while
[#102](https://github.com/ocx-sh/ocx/issues/102) requires `>= v1.0` — and #102 is
an **attach-side** requirement ("Validate SLSA spec version from the predicate
type URI; reject < v1.0", scoped by its own revision block to
`ocx package attest --type slsaprovenance` on this engine). It is not a
verification rule, and reading it as one would leave `ocx package attest --type
slsaprovenance` publishing exactly the artifact #102 requires the attach path to
refuse.

| Option | Trade-off |
|---|---|
| Diverge: OCX maps `slsaprovenance` → v1 | Silently produces a *different* predicateType than cosign for the same flag value. Two tools, one word, two meanings — the worst outcome for interop. Also contradicts D2. |
| Adopt cosign's table and take #102 as a *verify*-side policy rule | Misreads the issue, and leaves the attach path publishing v0.2 provenance under the default alias with no error and no warning. |
| **(chosen) cosign's table verbatim in the lookup; enforce the `>= v1.0` floor in `AttestPipeline`** | The alias table stays a pure lookup with no policy in it — the property the second option was reaching for — while #102 is satisfied where it asked to be satisfied. The two hold simultaneously precisely because the check is not in the table. |
| Full URIs only, no aliases | Rejects D2 and makes the common case unusable. |

**Decision.** Adopt cosign's table verbatim, plus a full URI passed through
unchanged. Model it as a `PredicateType` enum with `FromStr` — not a bare
`String` (IDIOM-03) — because the alias set is written down as match arms.

**The floor lives in the pipeline, not the table.** `AttestPipeline` rejects a
resolved provenance predicateType below v1.0 with
`SignErrorKind::ProvenanceVersionUnsupported`, slug
`provenance_version_unsupported`, exit **64** `UsageError` — a bad invocation,
not bad data — and the message names `--type slsaprovenance1` as the fix. One
Part III row, one negative fixture.

The floor is attach-only. **Verify still accepts v0.2 provenance** from external
producers, because cosign writes it and D1's interop criterion outranks a
publishing rule OCX imposes on itself (Not Doing).

To keep the alias resolution visible rather than surprising: **the resolved
predicateType URI is echoed in the `ocx package attest` report**, plain and
JSON, with no new machinery and no WARN on an ordinary state.

The `CosignPredicate {Data, Timestamp}` wrapper applies whenever the RESOLVED
predicate-type URI equals the custom URI — the `custom` alias, the default, and
a full-URI `--type` spelling that same URI — matching cosign since PR #2718
(v1.14.x), which compares resolved types. SBOM predicates are the raw document.

## D-d — Verify composition: one pipeline, branch at the gate

| Option | Trade-off |
|---|---|
| **(chosen) One `VerifyPipeline`; the DSSE gate becomes a mode check; `verifier.verify` runs for both modes; OCX-owned DSSE checks layer on top in `oci/verify/dsse.rs`** | Reuses every content-agnostic step, keeps the chain walk / SCT / validity-window checks the signature path already gets, and still enforces the checklist rows sigstore-rs does not. |
| A second `AttestVerifyPipeline` | Duplicates discovery, capability caching, trust-root resolution, chain walk, SCT, SET, Merkle and identity matching — the entire shipped surface — to reach one different step. Two pipelines drift on the next security fix. |
| **Delegation only** — hand the DSSE branch to sigstore-rs and add nothing | Not available, and not sufficient. Not available: `verify_bundle_content` and `InTotoStatementV1` are both `pub(crate)` in sigstore 0.14.0, so no OCX call site can reach either. Not sufficient, on five counts read out of that version's own DSSE path: (1) nothing on the bundle path checks `_type` — `validate_cosign_v1()` lives in `bundle/intoto.rs` and is called only from `cosign/signature_layers.rs:447`; (2) nothing checks `payloadType` before parsing the payload as a Statement; (3) `InTotoStatementV1::subject_sha256_digest()` reads `.first()` only (`bundle/intoto.rs:115`), and `validate_cosign_v1`'s doc (`bundle/intoto.rs:64-68`) concedes Go cosign and sigstore-go iterate all subjects; (4) the tlog comparison derives `envelope_json` via `serde_json::to_vec(&dsse)` — a re-serialization, not the received bytes (DATA-DIG-04). (Not on this list: signature count. sigstore *does* hard-reject `signatures.len() != 1` — `models.rs:246-252` — but only inside `verify()`'s content construction, where it surfaces as a generic bundle error; OCX's structural-first check re-asserts it so `MultipleSignatures` is the diagnosis the user sees, redundancy rather than a gap-filler.) **Note what this row does *not* say:** there is no "two trust roots" hazard. `crates/ocx_lib/src/oci/verify/trust_root.rs:301` already implements `sigstore::trust::TrustRoot` for OCX's own root, and `pipeline.rs:248` constructs the one `Verifier` from it — a second trust root is unreachable by construction, not merely avoided. |

**Decision.** One pipeline, and delegation **plus** defence in depth rather than
either alone.

`verifier.verify(subject_bytes, bundle, &PolicyDeferredToOcx, true)`
(`pipeline.rs:382`) runs **unchanged for both content modes**. That single call
is the only site in the crate performing chain building against the pinned trust
root at the certificate's own issuance time, SCT verification, and the
`NotBefore <= integratedTime <= NotAfter` window check — so a DSSE branch that
skipped it would verify attestations *more weakly* than `ocx package verify`
already verifies a message signature, under the same command. For a DSSE bundle
that call also performs PAE verification, binds `subject[0]`'s digest to the
input digest, and enforces `dsse:0.0.1` tlog consistency.

`verify_envelope` layers OCX's own checks over the **received** bytes, split
around that call by what each half needs. The **structural half runs first** —
the caps, then `payloadType == application/vnd.in-toto+json` **before** the
payload is parsed (row 3), `_type` ∈ `ACCEPTED_STATEMENT_TYPES` (row 18),
subject binding (below), zero-subject refusal and `sha256`-only within the
DigestSet, exactly one signature (row 8), and predicateType read from the
payload, never an annotation (row 7 / CVE-2022-35929). Structural-first is what
makes the specific `StatementSubject*` and payload-shape kinds *reachable*: the
delegated call refuses most malformed statements too, but with one generic
error, and it runs no OCX check twice — ordering OCX's diagnosis first means
the user sees the precise kind while the delegated refusal becomes redundancy
rather than the only report. The **tlog half runs after** the delegated call —
`verify_tlog_binding` (D-g) and the row-13 validity re-assertion consume entry
material the delegated call has already SET/Merkle-checked.

**Subject binding, precisely.** OCX's `binds_subject` iterates **every**
subject looking for a `sha256` match against the target digest OCX computed
itself — refusing zero subjects, a DigestSet with no `sha256`, and a list in
which **no** subject names the target (rows 4–6, the CVE-2026-31830 shape).
The delegated call *additionally* requires `subject[0]` itself to match: its
DSSE arm fails closed on anything else (`verifier.rs:76-80`). The net contract,
stated honestly: OCX accepts a multi-subject Statement only when `subject[0]`
names the target — **stricter than cosign and sigstore-go**, which iterate all
subjects. cosign-produced attestations are single-subject, so interop is
unaffected; a foreign multi-subject attestation carrying the target elsewhere
in the list is refused, fail-closed. That is a deliberate, recorded limitation
(fixture: a statement whose only match is `subject[1]` → refusal), not a
surprise to be discovered in production.

Correcting the primitive named in earlier drafts of this section: sigstore's DSSE
step is **not** a raw ECDSA `verify_prehash`. It is
`CosignVerificationKey::verify_signature(Signature::Raw(sig), pae)` over the PAE
bytes.

**Row 13 gets OCX's own re-assertion.** `NotBefore <= integratedTime <= NotAfter`
is asserted a second time by OCX, over `parts.integrated_time` and the leaf
certificate's validity window, in one helper in `verify/tlog.rs` beside the SET
and inclusion-proof checks — the path both modes already share, which is what
makes "run for both modes" a structural fact rather than a discipline. That file
does not perform the check today, and its module doc widens to say it does. This
is not redundancy for its own sake: CVE-2024-55655 is exactly the case where a
library silently dropped that step, and row 13's own wording ("not a library
default") exists because of it. A negative fixture with an `integratedTime`
outside the window asserts `CertificateValidityWindow`, so removal of either
check reds.

**Caps are selected by the requested mode, before the first fetch.** The
per-candidate size cap, the candidate-count cap and the cross-candidate byte
budget all come from `VerifyContentMode`, never from the candidate — a
candidate's own mode is unknowable until its bundle is parsed, so deriving the
caps from it would be circular. `Signature` mode keeps 512 KiB / 8 / 4 MiB
unchanged; attestation mode uses the `MAX_ATTESTATION_*` constants. A
red-before-green pair pins this: a 1 MiB bundle is **rejected** in `Signature`
mode and **accepted** in `Attestation` mode, so hoisting the larger constants
into the shared path reds immediately.

**Mode-mismatched candidates do not consume the requested mode's budget.** A
candidate discriminated as the other content kind after fetch-and-parse is
skipped without decrementing the candidate count; the cross-candidate byte
budget bounds the total fetch work, and a hard cap on listing iteration backstops
it. Without this, attaching five attestations and re-running attest a few times
pushes the signature past `MAX_SIGNATURE_CANDIDATES` in the registry's listing
order and `ocx package verify` reports `NoSignaturesFound` for a correctly signed
artifact — an availability regression this ADR would otherwise introduce.
Fixture: one signature plus nine attestation referrers on one subject, and
`ocx package verify` must still succeed.

**Attestation mode collects every verified candidate.** The two modes share
`verify_one_referrer` and differ only at the entry point: the signature path
keeps today's ANY-of `run` (first fully-passing candidate wins, which is correct
for "is this artifact signed"), while the attestation path is
`run_attestations -> Vec<AttestationMatch>`, bounded by the caps. First-match is
wrong for "which SBOMs does this artifact have": under an `identity_regexp`
policy, or across a signing-identity rotation where old and new coexist as two
ANY-of entries by design, first-match lets the **registry's listing order** pick
which document the consumer reads. `VerifyResult` therefore carries no
attestation field at all — one arity, in one place.

The mode is a field on `VerifyContext`, defaulting to today's behaviour:

```rust
pub enum VerifyContentMode {
    Signature,
    Attestation { predicate_type: Option<PredicateType> },
}
```

The load-bearing gate at `pipeline.rs:498` changes from "reject DSSE" to
"reject content that does not match the requested mode". That is a **signature**
change, and it is the one changed signature in the whole design, so it is stated
rather than implied: `BundleParts::from_bundle` (`pipeline.rs:479-481`) takes
only the bundle today and cannot see `VerifyContext`, so it becomes

```rust
fn from_bundle(bundle: &Bundle, mode: &VerifyContentMode) -> Result<Self, VerifyErrorKind>;
```

with `verify_one_referrer` (`pipeline.rs:329-338`) threading `ctx.content` in.

Its pinning test `from_bundle_rejects_dsse_envelope` becomes
`bundle_content_must_match_requested_mode` with both directions asserted — a
`MessageSignature` candidate in `Attestation` mode is skipped, and a
`DsseEnvelope` candidate in `Signature` mode is skipped, in each case without
aborting the scan over the remaining candidates (the shipped loop already merges
a per-candidate failure and continues, `pipeline.rs:310-311`).

## D-e — Discovery, and the `ocx package sbom` contract

**Discovery.** Keep today's call — `list_referrers(..., Some(SIGSTORE_BUNDLE_V03))`
(`pipeline.rs:457`) — unchanged, and layer annotation handling **on top of** it,
never in place of it.

An earlier reading argued for an empty filter on the grounds that OCI 1.1 lets a
registry ignore the `artifactType` query filter. That spec fact is real and it
argues the opposite way here. `filter_and_convert_referrers`
(`crates/ocx_lib/src/oci/client/native_transport.rs:215-243`) says so in its own
doc comment — the server filter may be ignored or applied without the advisory
`OCI-Filters-Applied` header, "so this client-side pass is the only filtering
callers can rely on" — and implements `None => true`. Passing `None` therefore
does not sidestep an unreliable *server* filter; it switches off the only
reliable *client* one, admitting referrers of arbitrary `artifactType` into a
candidate set bounded only by the caps. D-e's other premise stands and points the
same way: signatures and attestations share the `artifactType`, so keeping the
filter costs nothing and excludes nothing legitimate.

**Annotations are ordering and pre-filter hints only — never an exclusion.**
The authoritative discrimination between a signature and an attestation is the
**parsed bundle content oneof**, and the authoritative predicateType is the one
inside the verified payload (row 7 / CVE-2022-35929). Concretely:

- A candidate carrying no annotations, or annotations that disagree with its
  content, is still fetched and parsed. A registry that strips or rewrites
  annotations cannot hide an attestation.
- Annotations may order the candidate set so the likely match is fetched first,
  and may skip a fetch only where the caps would otherwise be spent — never as
  the reason a candidate is absent from the answer.
- A candidate whose annotation and signed predicateType disagree is rejected as
  `PredicateTypeMismatch`, and it reaches that rejection because it was fetched.

The failure this forecloses is an omission attack, and it is not a
verification bypass: a hostile or merely mirror-rewriting registry that relabels
the real CycloneDX referrer's `dev.sigstore.bundle.predicateType` would, under
annotation-as-filter, make `ocx package sbom --type cyclonedx` exit 79
"attestation not found". The operator concludes the artifact carries no SBOM;
a validly signed one exists and was suppressed by one unsigned string. Fail-closed
is the wrong comfort here, because "no SBOM" is precisely the answer this command
exists to give and a consumer acts on it.

Golden fixture: an **OCX-written** attestation referrer is selected. The
cosign-written direction is already covered by the interop test, and it is the
one that would pass even if OCX's own write side emitted nothing.

**`ocx package sbom <id> -p <platform>` contract.**

| Mode | Behaviour |
|---|---|
| default | List **every** verified SBOM attestation: predicate type, subject digest, referrer digest, certificate identity, issuer, signed-at. `--json` reports every match. |
| `--output PATH` (`-` for stdout) | Write the verified predicate document verbatim — the exact sub-slice from inside the envelope, never a re-serialization (row 2). |
| `--summary` | Probe `specVersion` first, dispatch, then parse (DATA-FMT-02 shape) and report component count plus document metadata. A non-CycloneDX or out-of-range document is an explicit refusal of **that entry** (`reason_kind` `sbom_summary_failed`), not a silent empty summary and not an abort of the listing — the scan is over N independent candidates (PKG-22). |

**`--output` refuses ambiguity rather than resolving it.** With more than one
verified attestation of the requested type, writing "the" document means letting
the registry's listing order choose which one the consumer reads. Instead:
`MultipleAttestations { predicate_type, referrer_digests }`, exit 65, naming the
digests so the operator can disambiguate. A per-referrer selection flag is
deferred (Not Doing) rather than guessed at now.

**`--output -` refuses a TTY.** The predicate is authored by whoever holds an
identity the policy admits — under an `identity_regexp` policy that is a large
set — so "verified" does not mean "safe to print". Written verbatim to a
terminal, a component description carrying an OSC 52 sequence sets the operator's
clipboard, and a U+202E reverses a component name (CWE-150). Row 2 wants the raw
bytes and SEC-34 wants them neutralised, and the two cannot both hold on one
stream; the resolution is to keep the bytes exact and decline the terminal. A
typed error names the reason — raw predicate bytes are unsanitized, redirect to a
file or a pipe. A file or a pipe is unaffected and byte-exact. The TTY-detection
precedent is CLI-09/CLI-07, already in the tree.

**Verification is unconditional.** `ocx package sbom` runs the attestation
verify pipeline before emitting anything, resolving identity exactly as `ocx
package verify` does (flags, else `[[trust.policy]]`, else exit 64
`NoIdentityProvided`). There is **no `--no-verify`**: an unverified SBOM listing
is registry-controlled text presented as fact, which is the precise shape SEC-32
exists to prevent. Enumerating attestations without trust is deliberately not a
capability in v1 (Not Doing).

## D-f — `push --sbom` mechanics

D2 fixes the flag. Three mechanics were open.

**Which digest is the subject.** `PushOutcome.canonical_tags` carries per-platform
`sha256.<hex>` tags, from which a digest is textually derivable — but only when
canonical tags are enabled, and `--no-canonical-tag` disables them. Deriving a
subject digest from a tag that may not exist is a latent break.

**Decision:** add `platform_digests: BTreeMap<String, Digest>` to `PushOutcome`
(`crates/ocx_lib/src/publisher.rs:42`), **keyed by `Platform`'s canonical
`Display` form**. It is the only field that holds the subject under
`--no-canonical-tag`.

> **AMENDED 2026-08-20 — superseded by WP9a's implementation ruling:
> `platform_digests` was never built.** The underlying property — the attest
> pipeline must not derive its subject digest from a canonical tag that
> `--no-canonical-tag` may have disabled — holds **by construction** instead:
> `AttestPipeline::run` resolves its per-platform target via
> `index.select(ctx.identifier, ctx.platform, IndexOperation::Resolve)`
> (`crates/ocx_lib/src/oci/attest/pipeline.rs:174`), the same index-indirection
> every other verb already uses, and never touches `canonical_tags` at all.
> `PushOutcome` carries no `platform_digests` field today. The TOCTOU residual
> between a push and a later attest is accepted as identical to the
> pre-existing sign-after-push posture. The rest of this subsection (the key-type
> and wire-type reasoning below) is preserved as the rationale that stood while
> the decision was live; it no longer describes shipped code.

The key type is a decision, not a detail. `Platform`
(`crates/ocx_lib/src/oci/platform.rs:65`) derives `Debug, Clone, PartialEq, Eq,
Hash` and **no `Ord`**, so `BTreeMap<Platform, _>` does not compile — and
`Platform` **must not** gain `Ord` to make it compile. The same type is the
canonical `ocx.lock` and dependency-pin map key, so a derived ordering would make
enum declaration order an observable, load-bearing property of the lockfile
(API-07), with `Any` sorting first by accident of position. Its serde form is a
JSON object (`{"os":…,"architecture":…}`), which cannot be a JSON map key either.
The `Display` form is documented in that same file as the single canonical,
lossless, injective string form and is already the lockfile's map-key spelling —
so it is the key here too.

`PushOutcome` itself is **not** a wire type: it derives `Debug` and nothing else,
so nothing parses it. The parsed cross-tool contract is `PushReport`
(`crates/ocx_cli/src/api/data/push.rs:25`), whose own doc comment names
`ocx-mirror pipeline push` as the consumer — and `PushReport`'s existing keys are
untouched by this change. `platform_digests` is needed in-process only. Because
ocx-mirror takes `ocx_lib` as a path dependency, the residual risk is a
compile-time break at a struct literal rather than a wire break; `PushOutcome`
is marked `#[non_exhaustive]` in the same change to close it.

**Failure atomicity.** Push succeeds, attest fails. The push is **not** rolled
back: a pushed manifest is immutable and OCI offers no un-push.

Re-running `ocx package attest` against the same digest is **idempotent in
outcome, additive in state** — not convergent. S1-I
(`adr_oci_referrers_signing_v1.md:504`) chose "each invocation writes a new
signature as an additional referrer" and explicitly rejected both replace and
append-only-if-absent; attestation inherits it, and each run mints a fresh
certificate, Rekor entry, bundle blob and referrer digest. So every re-run yields
a valid attestation for the digest **and leaves the previous one in place**.

The consequence worth writing down: a flaky CI job retrying attest accumulates
one referrer per attempt, all valid, none wrong, nothing surfacing the growth —
until the subject reaches `MAX_ATTESTATION_CANDIDATES` and verify begins refusing
with `TooManyAttestations`. At that point the user prunes stale referrers with
registry tooling; the constant is deliberately not configurable, because a
configurable cap converts an accumulation bug into a larger accumulation bug.

`PushReport` gains `attestation: Option<AttestationOutcome>` — succeeded, or
failed carrying the error slug in the existing envelope shape (CLI-04) — and the
push command handler combines the two outcomes, exiting with the attest error's
classified code (PKG-24: worst classified failure). Documented in `signing.md`
with the recovery command spelled out.

**Multi-platform.** One attestation per per-platform manifest pushed in this
invocation — the same subject granularity as `ocx package sign -p`, so verify
and `sbom` need no second rule. Under single-platform authoring
(`adr_platform_model_unification.md`) that is exactly one.

## D-g — Accepted Rekor entry kinds on verify

| Option | Trade-off |
|---|---|
| `{dsse:0.0.1, intoto:0.0.1, …}` | `intoto:0.0.1` had its PayloadHash requirement *relaxed* in rekor 1.2.0, so a 0.0.1 entry cannot be relied on to bind the envelope — accepting it weakens row 12 for every entry. |
| `{dsse:0.0.1, intoto:0.0.2}` | Rejected for two independent reasons. What `intoto:0.0.2`'s `hash` is computed over is **unsourced** — the research quotes `dsse:0.0.1`'s `Canonicalize()` verbatim from rekor's source but gives `intoto:0.0.2` only schema field names, so the binding check would be written against an assumed field meaning on the one row that closes a CVE class. And it is **unreachable through the delegated path** (D-d): sigstore 0.14's `tlog_entry_for_dsse` returns `None` for any `kind != "dsse"` or `apiVersion != "0.0.1"`, so an `intoto:0.0.2` entry is refused before OCX's own binding check ever runs. An accepted kind with no reachable green is the mirror image of the unchecked green that disqualifies Rekor v2 below. |
| `{dsse:0.0.1, hashedrekord:0.0.2}` | `hashedrekord:0.0.2` is Rekor v2, which the compose stack cannot produce. Accepting a kind with no reachable red state is an unchecked green. |
| **(chosen) Write and accept `dsse:0.0.1`, and nothing else** | It is what cosign writes, what rekor 1.4.2 produces, what the delegated path admits, and the only kind whose canonicalization is sourced. Rekor v2 arrives with #107, as a constant-table addition. |

**Row 12 binding, rebound to what a bundle can actually prove.** The naive
mechanism — recompute `sha256(envelope_json_bytes)` and compare against
`envelopeHash` — is not something OCX performs itself, because the delegated
verifier already does: sigstore-rs 0.14 reconstructs the envelope from the
bundle's proto3 JSON (`bundle/verify/models.rs` — key order fixed by prost,
empty fields omitted) and fails closed on an `envelopeHash` mismatch. That
makes the *sign side's* serialization a wire contract: the uploaded envelope
must match the proto3-JSON spelling, in particular an empty `keyid` is
omitted, never emitted as `""` (`WireSignature`'s `skip_serializing_if`,
pinned by the `ENVELOPE_JSON` golden). An emitted empty keyid made every
ocx-signed attestation fail **ocx's own verify** — caught by WP10a with
measured hashes. cosign was never affected: it verifies the envelope against
the certificate and does not recompute the log's `envelopeHash`, which also
means cosign interop is no evidence this invariant holds — only a verifier
that checks the log binding (ocx's delegated path) exercises it.
A second reconstruction on OCX's side would duplicate the delegated
comparison, so OCX adds only the checks the delegate does not make.

So the check is split by the side that can honestly perform it:

- **Verify side.** `verify_tlog_binding` compares the canonicalized body's
  `payloadHash` — `sha256` over the **received** envelope's decoded payload
  bytes, which is what rekor's `dsse:0.0.1` actually hashes (its API spec, and
  sigstore-0.14.0 `models.rs:488` recomputes exactly that; the hash-of-PAE
  regime is Rekor v2 `hashedrekord:0.0.2`, which is Not Doing) — and the
  body's `signatures[]` content against the received bundle's DSSE envelope.
  Signature plus payload together bind the presented signature to the logged
  entry, which is the property GHSA-8gw7-4j42-w388 demands. `envelopeHash` is
  **not** recomputed here.
- **Sign side.** OCX holds the exact bytes it uploaded, so `envelopeHash`
  consistency is asserted there, as a self-consistency check at the one place it
  is meaningful.

Recorded deviation: sigstore's own internal comparison runs over a
re-serialization of the parsed envelope. OCX's binding check over the received
bytes is the defence in depth, and CVE-2026-22703 — a confirmed regression of
GHSA-8gw7-4j42-w388 in January 2026 — is why it stays red-before-green locked
rather than trusted to the library.

An entry of any other kind is `VerifyErrorKind::UnsupportedTlogEntryKind {
kind, version }` → exit 65 (`DataError`). It is a data-shape refusal, not a
service outage; 83 stays reserved for an unreachable log.

## D-h — Error taxonomy and the JSON envelope

**No new exit code.** 85 belongs to OSV (D5), and every attestation failure maps
cleanly onto the existing table: a binding or shape failure is `DataError` (65),
an absent attestation is `NotFound` (79), a bad `--type` — including a provenance
alias below the v1.0 attach floor — is `UsageError` (64), an `--offline` refusal
is `PermissionDenied` (77) exactly as the shipped sign refusal already is, and an
unreachable log is the already-existing 83.

**No new error family.** Attest-push failures are `SignErrorKind` variants — it
already owns Fulcio, Rekor and referrer-push failures, and Amendment 7 fixed the
kind-only `Result<_, SignErrorKind>` leaf shape. Attest-verify failures are
`VerifyErrorKind` variants. Minting `AttestErrorKind` would duplicate a dozen
identical variants and a second exit-code classification path.

**Envelope shape unchanged.** New slugs are additive
within the existing `error.kind` / `error.detail` contract (frozen contract
C-S1-1) and bump nothing. `ENVELOPE_SCHEMA_VERSION` stays `1` (owner ruling,
2026-08-20, superseding a brief `2` reading): a category rename bumps only
when a *released* binary emitted the old spelling — `rekor_unavailable`
never shipped, so no consumer can observe the rename, while a version flip
would itself break scripts pinning the number. The rename in question is D3's
`rekor_unavailable` →
`transparency_log_unavailable`, which is pre-release and lands as a plain
rename — identifier and slug together — across every site the rename census below the table enumerates (the table's
sixteen contract-bearing rows plus the mechanical remainder). Be precise about *which* half of the frozen contract moves:
`rekor_unavailable` is a member of the `error.kind` **category set** (it is
`ErrorCategory::TransparencyLogUnavailable`'s serde name), not merely a `detail` slug —
the rename changes an enumerated value scripts branch on, which is exactly why
the three `command-line.md` rows are in the rename table.

D-j's new `TrustPolicyError::NoBackend` needs no row of its own: it reaches exit
78 `ConfigError` with slug `trust_policy_invalid` through the existing
`VerifyErrorKind::TrustPolicyInvalid(#[from] crate::trust::TrustPolicyError)`
chain, already pinned by `trust_policy_invalid_maps_to_config_error`. Stated here
so a planner does not mint a second config-error slug for it.

Full variant/slug/exit table: see [Error Variants and Exit Codes](#error-variants-and-exit-codes).

## D-i — Module layout

```
crates/ocx_lib/src/oci/attest.rs             aggregator; the MAX_* constants
crates/ocx_lib/src/oci/attest/dsse.rs        PAE, DsseEnvelope, envelope hashes
crates/ocx_lib/src/oci/attest/statement.rs   in-toto Statement build + parse + subject binding
crates/ocx_lib/src/oci/attest/predicate.rs   PredicateType, alias table, CosignPredicate wrapper
crates/ocx_lib/src/oci/attest/pipeline.rs    AttestPipeline::run (push side)
crates/ocx_lib/src/oci/verify/dsse.rs        the verify-side DSSE step
crates/ocx_lib/src/sbom.rs                   SBOM reading (top-level, not under oci)
crates/ocx_lib/src/sbom/cyclonedx.rs         CycloneDX 1.5-1.7 parse + summary
```

Three rules make this layout the one it is:

1. **`oci::attest` is a leaf that owns the format; `oci::verify` consumes it.**
   Dependency runs `verify → attest`, one direction, no cycle (ARCH-16). The
   verify-side step lives under `verify/` because it is a pipeline step in that
   pipeline's vocabulary, not a format concern.

   > **AMENDED 2026-08-20 — the module graph is not acyclic.** As shipped,
   > `attest/dsse.rs` and `attest/statement.rs` return `VerifyErrorKind`
   > values, and `attest/pipeline.rs` and `attest/statement.rs` return
   > `SignErrorKind` values, while `verify/{dsse,pipeline}.rs` and
   > `sign/{bundle,rekor,signer}.rs` import `attest`'s DSSE/Statement types and
   > constants back. `attest ↔ verify` and `attest ↔ sign` are therefore both
   > real cycles — accepted as a direct consequence of D-h (no new error
   > family: `attest`'s own parsers report failures using whichever
   > consumer's error-kind vocabulary applies, rather than minting a fourth
   > enum), not an accident. The "one direction, no cycle" sentence above is
   > left standing for contrast with what shipped; `crates/ocx_lib/src/oci.rs`'s
   > module comment above `pub mod attest` records the corrected shape.
2. **`sbom` is top-level, not under `oci`.** An SBOM document is not an OCI
   concept and its parser must not reach the registry layer. `oci::attest`
   produces bytes; `sbom` interprets them.
3. **No `SbomFormat` trait** (D2). One concrete `cyclonedx` module with inherent
   functions. A trait with one implementation and no exercised double is ARCH-07.

`limits.rs` is deliberately absent — five constants do not earn a file; they
live in `attest.rs` beside the module doc that explains them (PKG-11).

## D-j — Trust-policy nesting mechanics

D3 fixed the shape. The mechanics settled here:

**Serde shape** (schema types; tolerant, per `adr_trust_policy.md` As-Shipped
Notes — **no `deny_unknown_fields` anywhere in this tree**):

```rust
/// Tolerant: a fleet may read a `config.toml` written by a newer ocx.
pub struct TrustPolicy {
    pub scope: Option<String>,
    /// #103 SLSA builder pin. Backend-independent, so a top-level sibling of `scope`.
    pub builder: Option<String>,
    pub keyless: Option<KeylessMatcher>,
    // future: pub key: Option<KeyMatcher>,

    /// Carried VERBATIM from the flat shape, both attributes included. A managed
    /// or project payload writing `system_locked = true` is parsed as an unknown
    /// key and dropped, so it cannot promote itself.
    #[serde(skip)]
    #[schemars(skip)]
    pub system_locked: bool,
}

pub struct KeylessMatcher {
    pub identity: Option<String>,
    pub identity_regexp: Option<String>,
    pub oidc_issuer: Option<String>,
}
```

**Exactly-one-backend validation is enforced by the type system, not by a
runtime count.** Compilation (`TrustPolicy::compile()`, the pre-existing entry
point) resolves the schema type into:

```rust
pub struct CompiledPolicy {
    builder: Option<String>,
    backend: PolicyBackend,
}
// scope stays on TrustPolicy: resolve() filters on the raw policy, so a
// CompiledScope type would have no consumer (as-built correction).

pub enum PolicyBackend { Keyless(CompiledKeyless) }
```

Zero backends → `TrustPolicyError::NoBackend { scope }`. Two backends is
unreachable in v1 because only one variant exists; adding `Key(..)` later forces
every `match` to be updated, which is the point. The runtime "more than one"
check is written now anyway, against the schema type, so the failure is a clear
message rather than a silent first-wins.

**Tolerance extends into the nested table.** Unknown fields inside
`[trust.policy.keyless]` are ignored on the same fleet-forward-compat argument;
the existing regression test
`trust_config_tolerates_unknown_fields_from_newer_ocx` gains a nested case.

**No alias for the flat form.** Nothing shipped — the flat keys are deleted as
if they never existed (repo stability tier).

**`system_locked` survives the nesting unchanged**, and so does `resolve()`'s
preemption: a matching locked policy governs alone, and every unlocked match is
dropped whatever its scope. Deleting it would reopen exactly the escalation it
exists to close — an operator pins `scope = "ghcr.io/acme"` at the system tier,
a local user writes a longer literal prefix `scope = "ghcr.io/acme/tool"` with
their own identity, and longest-prefix election hands the user's policy the
decision on every `ocx package verify` *and* every `auto_verify` install
surface. The existing anti-escalation test is **extended** to the nested form,
not replaced.

**`builder` semantics.** An opaque string matched against the SLSA provenance
predicate's builder identity during attestation verify. It is inert in signature
mode, and a `builder` on a policy that never verifies provenance is **not** an
error — it is forward configuration.

The field path is version-dependent and a single path would be wrong for one of
the two shapes OCX accepts. SLSA v0.2 puts it at `predicate.builder.id`; v1.0
moved it to `predicate.runDetails.builder.id` (and `buildType` to
`predicate.buildDefinition.buildType`). The two schemas share no path for either
value, so the accessor dispatches on the resolved predicateType:

```rust
fn builder_id(predicate_type: &PredicateType, predicate: &Value) -> Option<&str>;
```

reading `runDetails.builder.id` for `slsa.dev/provenance/v1` and `builder.id` for
`v0.2`. Verify accepts both shapes; attach only produces v1 (D-c's floor).

**Absent or unparseable is a refusal, never a skip.** A policy carrying a
`builder` pin against a predicate whose builder field cannot be read fails with
`BuilderMismatch { expected, found: Option<String> }`. The fail-open reading —
"field absent, constraint not applicable, pass" — would leave the pin silently
inert on exactly the version #102 wants, which is a policy bypass with no signal.
One fixture per SLSA shape; a single-shape fixture is precisely the test that
cannot tell fail-open from correct.

**`builder` is ANDed within a policy and ORed across the ANY-of set.**
`resolve()` returns every policy at the winning specificity, so an equal-scope
policy *without* `builder` weakens the set — array-append across the pooled
operator tiers permits exactly that. `system_locked` is the operator's
containment for it, which is the second reason the field above is not optional.

**Both JSON schemas regenerate:** `config/v1.json` and `project/v1.json`, via
`task schema:generate`. Blast radius is 3 Rust files plus the two schemas.

---

# Part III — Normative Verifier Requirements

The compliant-verifier checklist from
[`research_dsse_verification_pitfalls.md`](./research_dsse_verification_pitfalls.md)
is adopted **as normative requirements**, not as advice. Every row names where
it is enforced and how it is proven. A row with no enforcement site is a defect,
not a note.

| # | Requirement | Enforced in | Proven by |
|---|---|---|---|
| 1 | Recompute PAE over the **decoded payload bytes** — never the base64 text, never a re-serialized struct; reject on mismatch | `attest/dsse.rs::pae` + `verify/dsse.rs` | Golden PAE vector test; a mutation swapping decoded for base64 must red |
| 2 | Never re-parse or re-serialize the verified payload before downstream use — pass the original bytes forward | `verify/dsse.rs` returns `VerifiedAttestation { payload: Vec<u8>, predicate: Box<RawValue>, .. }` — the predicate is the verbatim sub-slice, never a re-serialization | Round-trip: `sbom --output` byte-compares against the **pretty-printed** predicate file `attest` was given, demonstrated red first |
| 3 | Check `payloadType == "application/vnd.in-toto+json"` **before** parsing as a Statement | `attest/dsse.rs::DsseEnvelope::parse` (called from `verify/dsse.rs`) | Negative fixture with a plausible-but-wrong payloadType → `PayloadTypeUnsupported` |
| 4 | Compare `statement.subject[].digest.sha256` against the target digest **OCX computed itself**; never trust the referrer `subject` linkage or an annotation as binding | `attest/statement.rs::binds_subject` | Cross-subject fixture (CVE-2026-31830 shape): a valid attestation for A served as a referrer of B must fail `StatementSubjectMismatch` |
| 5 | Reject a zero-subject Statement, and one with no subject matching the target | `attest/statement.rs::binds_subject` | Two negative fixtures → `StatementSubjectAbsent` and `StatementSubjectMismatch` |
| 6 | Match on hardcoded `sha256` inside the DigestSet; a weaker co-present algorithm never satisfies the check | `attest/statement.rs::binds_subject` | Fixture with a matching `md5` and a non-matching `sha256` must fail; a DigestSet with no `sha256` at all → `StatementSubjectWeakAlgorithm` |
| 7 | Read the policy-relevant predicateType from the **verified payload**, never from an unsigned annotation | `verify/dsse.rs` + `AttestPipeline` discovery | Fixture whose annotation and signed predicateType disagree → `PredicateTypeMismatch` |
| 8 | Bundle path: hard-reject `signatures.len() != 1` | `attest/dsse.rs::DsseEnvelope::parse` (called from `verify/dsse.rs`) | Two-signature fixture → `MultipleSignatures` |
| 9 | Non-bundle multi-signer `(t,n)` — **out of v1 scope**, stated in the docs | — | Not Doing |
| 10 | `keyid` is a lookup hint only; never a security decision | `verify/dsse.rs` (field is read and discarded) | Reading pass; a fixture with a hostile `keyid` still verifies |
| 11 | The verification algorithm comes from the pinned trust root's key-type record, never from an envelope field | shipped (`TrustRoot`), unchanged | Existing coverage |
| 12 | The logged entry must commit to the **presented signature**, never to `payloadHash` alone | Verify side: `verify/dsse.rs::verify_tlog_binding` compares the canonicalized body's `payloadHash` **and** `signatures[]` against the received envelope. Sign side: `oci/sign/bundle.rs` asserts `envelopeHash` over the bytes actually uploaded (D-g) | Fixture: a canonicalized body whose `signatures[]` does not match the bundle's → `TlogBindingMismatch`. Kept red-before-green locked — CVE-2026-22703 is a confirmed regression of this exact class |
| 13 | Assert `NotBefore <= integratedTime <= NotAfter` as OCX's own check, not a library default | Two sites, deliberately: `sigstore::bundle::verify::Verifier` performs it inside the `pipeline.rs:382` call for both modes, **and** OCX re-asserts it over `parts.integrated_time` and the parsed leaf in `verify/tlog.rs`, beside the SET and inclusion-proof checks — which is where "runs for both modes" comes from. That file does **not** perform it today; its module doc scopes it to the SET and the proof, and this change widens the doc (D-d) | New negative fixture: an `integratedTime` outside the certificate window → `CertificateValidityWindow`, asserted on the kind, never on "non-zero". There is **no** pre-existing coverage; `test_verify.py` has no case for it today. Named test: `test_verify_attestation_integrated_time_outside_window` (acceptance), plus a unit case beside the helper |
| 14 | Verify the checkpoint signature against the pinned Rekor key **before** trusting any inclusion proof | shipped, unchanged | Existing coverage |
| 15 | `MAX_ATTESTATION_ENVELOPE_BYTES` bounded read on the raw bytes, separate from the 512 KiB signature-bundle cap | `attest.rs` constants + fetch path | Oversize fixture → `AttestationTooLarge` |
| 16 | A separate decoded-payload cap, checked from the base64 length **before** allocating the decode buffer | `attest/dsse.rs::DsseEnvelope::parse` (called from `verify/dsse.rs`) | Oversize fixture → `AttestationPayloadTooLarge` |
| 17 | Cap the attestation candidate count fetched per subject, and the cumulative bytes | `attest.rs` constants + discovery | Fixtures → `TooManyAttestations`, `AttestationBudgetExhausted` |
| 18 | Reject any `_type` outside the allowlist | `attest/statement.rs` | **Deviation, per D-b:** the allowlist is `{v1, v0.1}`, not `{v1}`. Fixture with a third `_type` → `StatementTypeUnsupported` |
| 19 | Per SEC-32: document that verification proves authenticity and integrity **at signing time**, not freshness; imply no rollback protection | `website/src/docs/in-depth/signing.md` | Docs review; the sentence is explicit, not implied by omission |
| 20 | Fail closed on every verification-path exception; never fall through to success on incomplete input | all of the above | Every negative fixture asserts a specific error kind, never merely non-zero |
| 21 | **OCX addition, not from the research checklist** (#102): attach refuses a resolved provenance predicateType below v1.0 | `attest/pipeline.rs` — the alias table stays a pure lookup, the floor is in the pipeline (D-c) | Negative fixture: `--type slsaprovenance` → `ProvenanceVersionUnsupported` (64), message naming `--type slsaprovenance1` |

Rows 1–20 are the research checklist, transcribed with their citations intact.
Row 21 is this ADR's own, added so #102's attach-side requirement has an
enforcement site rather than a paragraph.

Two properties in this table have **no behavioural seam** and therefore need
structural care rather than a test that reads well: row 2 (the *absence* of a
re-serialization) and row 19 (the *presence* of an honest sentence). Row 2 is
covered by the byte-comparison test named above — a real seam, not a source scan,
and it is only a real seam because the predicate travels as a verbatim slice on
both sides (D-b). Row 19 is a docs review item and is listed as such; no test is
claimed for it.

---

# Part IV — Component Contracts

Precise enough that a planner can decompose without re-deriving design. Types
are named at their final module paths.

## Constants (`oci/attest.rs`)

Each carries a rationale comment, a stated configurability decision (all five
are **not** configurable in v1), and a dedicated error variant naming it
(PKG-11).

```rust
/// Raw bytes of one attestation bundle fetched from a registry.
/// 32 MiB: two orders above the largest realistic CycloneDX SBOM for a binary
/// package, two orders below a memory hazard. Deliberately NOT the 512 KiB
/// signature-bundle cap — a different artifact class gets its own bound.
/// Not configurable in v1.
pub(crate) const MAX_ATTESTATION_ENVELOPE_BYTES: usize = 32 * 1024 * 1024;

/// Decoded in-toto Statement payload. Checked from the base64 length BEFORE
/// allocating the decode buffer (base64 expands at a fixed 4/3, so the decoded
/// size is known in advance). Tighter than the envelope cap on purpose: this is
/// the number a document author can reason about. Not configurable in v1.
pub(crate) const MAX_STATEMENT_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;

/// Attestation referrers considered for one subject. Larger than
/// MAX_SIGNATURE_CANDIDATES (8) because attestations legitimately fan out —
/// one per predicate type per producer. Not configurable in v1.
pub(crate) const MAX_ATTESTATION_CANDIDATES: usize = 32;

/// Cumulative attestation bytes fetched in one verify run. Closes the
/// candidates x per-envelope product, which neither cap closes alone.
/// Not configurable in v1.
pub(crate) const MAX_TOTAL_ATTESTATION_BYTES: usize = 64 * 1024 * 1024;

/// Local `--predicate` file. Deliberately 1 MiB BELOW MAX_STATEMENT_PAYLOAD_BYTES:
/// the Statement wraps the predicate in `_type`, `predicateType` and a `subject`
/// array, so an at-the-limit predicate would produce an over-limit payload and
/// verify would refuse what attest accepted. The 1 MiB is the wrapper reserve.
/// Enforced by a bounded read, never a `metadata().len()` check followed by an
/// unbounded one (PKG-04/07). Not configurable in v1.
pub(crate) const MAX_PREDICATE_FILE_BYTES: usize = 15 * 1024 * 1024;

pub(crate) const DSSE_PAYLOAD_TYPE: &str = "application/vnd.in-toto+json";
pub(crate) const STATEMENT_TYPE_WRITTEN: &str = "https://in-toto.io/Statement/v1";
pub(crate) const ACCEPTED_STATEMENT_TYPES: &[&str] = &[
    "https://in-toto.io/Statement/v1",
    "https://in-toto.io/Statement/v0.1", // D-b: cosign v3 still writes this
];
/// (kind, version) pairs accepted from a bundle's `tlogEntries[].kindVersion`.
/// D-g: one entry, deliberately. `intoto:0.0.1` has a relaxed PayloadHash;
/// `intoto:0.0.2`'s canonicalization is unsourced AND unreachable through
/// sigstore's `tlog_entry_for_dsse`; `hashedrekord:0.0.2` is Rekor v2 (#107).
pub(crate) const ACCEPTED_TLOG_KINDS: &[(&str, &str)] = &[("dsse", "0.0.1")];
pub(crate) const TLOG_KIND_WRITTEN: (&str, &str) = ("dsse", "0.0.1");
```

Referrer manifest annotation keys extend the **existing** constants home
(`oci/referrer/media_types.rs` — "values are data-only, do not add logic"). Five
constants, matching cosign's three-key set (D1):

```rust
pub(crate) const ANNOTATION_CREATED: &str = "org.opencontainers.image.created";
pub(crate) const ANNOTATION_BUNDLE_CONTENT: &str = "dev.sigstore.bundle.content";
pub(crate) const ANNOTATION_BUNDLE_PREDICATE_TYPE: &str = "dev.sigstore.bundle.predicateType";
pub(crate) const BUNDLE_CONTENT_DSSE: &str = "dsse-envelope";
pub(crate) const BUNDLE_CONTENT_MESSAGE_SIGNATURE: &str = "message-signature";
```

`BUNDLE_CONTENT_MESSAGE_SIGNATURE` has a writer as of this change — the signature
referrer path (D1) — not just a reader.

## Referrer manifest (`oci/referrer/manifest.rs`, extended)

`ReferrerManifest` has six fields and no annotations today, so nothing above can
reach the wire without this:

```rust
pub struct ReferrerManifest {
    // ... schema_version, media_type, artifact_type, config, layers, subject ...

    /// `skip_serializing_if` is LOAD-BEARING, not tidiness: `to_canonical_json`
    /// is a plain `serde_json::to_vec(self)` and the registry addresses the
    /// referrer by the SHA-256 of exactly those bytes. Without it every manifest
    /// built by a caller that passes `None` gains `"annotations": null` and
    /// changes digest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<BTreeMap<String, String>>,
}
```

with the constructor taking them. `BTreeMap` for byte-stable key order
(DATA-DET-01).

## DSSE (`oci/attest/dsse.rs`)

```rust
/// PAE per the DSSE spec: "DSSEv1" SP LEN(type) SP type SP LEN(body) SP body.
/// `body` is the raw decoded payload, never base64 (checklist row 1).
pub(crate) fn pae(payload_type: &str, payload: &[u8]) -> Vec<u8>;

/// Structural parse of received envelope bytes: payloadType (row 3),
/// exactly-one signature (row 8), decoded-payload cap from the base64
/// length (row 16). Landed in WP4; `verify/dsse.rs` calls in.
pub(crate) fn parse(...) -> Result<DsseEnvelope, VerifyErrorKind>;
// DsseEnvelope also impls Serialize (sign side writes it).

/// Wire shape of the bundle's `dsseEnvelope`. Field order fixed for determinism.
pub(crate) struct DsseEnvelope {
    pub payload: Vec<u8>,       // decoded; base64 only at the serde boundary
    pub payload_type: String,
    pub signatures: Vec<DsseSignature>,
}

pub(crate) struct DsseSignature {
    pub sig: Vec<u8>,
    pub keyid: String,          // hint only, never a security decision (row 10)
}

/// The two hashes the `dsse:0.0.1` canonicalized body commits to.
pub(crate) struct EnvelopeHashes { pub envelope: Digest, pub payload: Digest }

/// Hashes the exact serialized envelope bytes handed to Rekor, not a
/// re-serialization of the struct.
pub(crate) fn envelope_hashes(envelope_json: &[u8], payload: &[u8]) -> EnvelopeHashes;
```

## Statement (`oci/attest/statement.rs`)

```rust
pub(crate) struct Statement {
    pub statement_type: String,      // serde rename: "_type"
    pub subject: Vec<Subject>,
    pub predicate_type: String,
    /// RawValue on BOTH sides (D-b): spliced verbatim when building, borrowed as
    /// the exact sub-slice when parsing. Never a `Value` — that would normalize
    /// the document and silently defeat checklist row 2.
    pub predicate: Box<serde_json::value::RawValue>,
}

pub(crate) struct Subject {
    pub name: String,                            // informational (row 4)
    pub digest: BTreeMap<String, String>,        // BTreeMap: deterministic order
}

/// Build the Statement OCX writes. `subject_name` is the bare physical
/// repository path (cosign parity); `subject_digest` is the per-platform
/// manifest digest OCX resolved itself. `predicate` is the raw file bytes,
/// already validated as JSON by a parse whose `Value` was discarded.
pub(crate) fn build(
    subject_name: &str,
    subject_digest: &Digest,
    predicate_type: &PredicateType,
    predicate: &serde_json::value::RawValue,
) -> Result<Statement, SignErrorKind>;

/// Parse a verified payload. Rejects an `_type` outside ACCEPTED_STATEMENT_TYPES.
pub(crate) fn parse(payload: &[u8]) -> Result<Statement, VerifyErrorKind>;

/// Checklist rows 4/5/6: iterate EVERY subject looking for a `sha256` match —
/// sigstore-rs reads `subject[0]` only, and D-d records the resulting net
/// contract (the delegated call pins `subject[0]`; this check owns the precise
/// diagnoses and the no-match refusal — CVE-2026-31830). Distinct outcomes:
/// no subject at all -> `StatementSubjectAbsent`; no `sha256` key in a DigestSet
/// (a co-present weaker algorithm never satisfies the check) ->
/// `StatementSubjectWeakAlgorithm`; a `sha256` present but naming another
/// artifact -> `StatementSubjectMismatch { expected, actual }`. Three
/// requirements, three slugs, so a consumer can tell them apart.
pub(crate) fn binds_subject(statement: &Statement, target: &Digest) -> Result<(), VerifyErrorKind>;
```

## Predicate type (`oci/attest/predicate.rs`)

```rust
/// cosign's `--type` vocabulary verbatim (D-c), plus a passthrough URI.
pub enum PredicateType {
    CycloneDx,
    Spdx,
    SpdxJson,
    SlsaProvenance,     // -> https://slsa.dev/provenance/v0.2 (cosign parity)
    SlsaProvenance02,
    SlsaProvenance1,    // -> https://slsa.dev/provenance/v1
    Link,
    Vuln,
    OpenVex,
    Custom,
    Uri(String),        // full URI passed through unchanged
}

impl std::str::FromStr for PredicateType { type Err = PredicateTypeParseError; /* .. */ }

/// The URI written into the Statement and the referrer annotation. Echoed in
/// the attest report so the `slsaprovenance` -> v0.2 resolution is visible.
pub fn uri(&self) -> &str;

/// Wraps in `{Data, Timestamp}` whenever the RESOLVED URI equals the custom
/// URI — so a full-URI `--type` spelling the custom URI wraps exactly like the
/// alias (cosign compares resolved types; variant-matching would diverge).
/// Everything else is the raw predicate document (cosign PR #2718, v1.14.x).
/// The wrapper is built around the verbatim slice, so even the wrapped form
/// embeds the original bytes. `Data` embeds the JSON OBJECT, not a string —
/// confirmed by the interop test (`cosign verify-attestation`), not locally.
/// Fallible because `to_raw_value` is: the amendment replaces the earlier
/// infallible signature (WP-A review ruling — no `expect` in library code).
pub fn wrap(
    &self,
    predicate: &serde_json::value::RawValue,
    now: DateTime<Utc>,
) -> Result<Box<serde_json::value::RawValue>, serde_json::Error>;

/// SLSA builder identity, dispatched on version (D-j): `runDetails.builder.id`
/// for provenance v1, `builder.id` for v0.2. `None` means the field is absent or
/// unparseable, which a `builder`-carrying policy treats as a REFUSAL, never a
/// skip — WITHIN provenance predicates only: the pin's subject is provenance
/// builders, so a non-provenance predicate (SBOM, VEX) passes the pin
/// untouched (WP6 panel ruling; matches the forward-configuration prose). The `Value` is parsed from the already-verified predicate for policy
/// evaluation only and is never serialized back.
pub(crate) fn builder_id<'a>(
    predicate_type: &PredicateType,
    predicate: &'a serde_json::Value,
) -> Option<&'a str>;
```

## Signer (`oci/sign/signer.rs`, extended)

```rust
#[async_trait]
pub trait Signer: Send + Sync {
    async fn sign(&self, target_digest: &Digest, token: &OidcToken,
                  fulcio_url: &Url, rekor_url: &Url) -> Result<SignedBundle, SignErrorKind>;

    /// Sign a DSSE payload. The payload type is fixed (`DSSE_PAYLOAD_TYPE`) —
    /// v1 writes exactly one, so it is a constant, not a parameter (D-a).
    /// Signs `sha256(PAE(payload_type, statement_bytes))`; uploads a
    /// `dsse:0.0.1` Rekor entry; returns a bundle whose content oneof is
    /// `dsseEnvelope`.
    async fn sign_dsse(&self, statement_bytes: &[u8], token: &OidcToken,
                       fulcio_url: &Url, rekor_url: &Url) -> Result<SignedBundle, SignErrorKind>;

    fn signer_kind(&self) -> &'static str;
}
```

`SignedBundle` is **unchanged** — `bytes` / `digest` / `certificate_identity` /
`certificate_oidc_issuer` already describe a DSSE bundle exactly as well as a
message-signature one.

Shared half, extracted from the existing `KeylessSigner::sign` body so the two
methods differ only where the protocols differ:

```rust
// oci/sign/signer.rs, private — `KeylessSigner` is a unit struct there
// (`oci/sign/signer.rs:50`), so the shared half is a free function with no
// receiver. There is no `oci/sign/keyless.rs`.
async fn issue_ephemeral_certificate(
    token: &OidcToken,
    fulcio_url: &Url,
) -> Result<EphemeralIdentity, SignErrorKind>;

struct EphemeralIdentity {
    signing_key: SigningKey,
    certificate: FulcioCertificate,
    identity: String,
    issuer: String,
}
```

## Rekor (`oci/sign/rekor.rs`, extended)

```rust
/// `dsse:0.0.1` proposedContent: `{ envelope: <stringified envelope JSON>,
/// verifiers: [base64(cert PEM)] }`. Returns the server's canonicalized body
/// verbatim — never a locally reconstructed one.
pub(crate) async fn upload_dsse_entry(
    &self,
    envelope_json: &[u8],
    leaf_pem: &str,
) -> Result<RekorEntry, SignErrorKind>;
```

## Bundle assembly (`oci/sign/bundle.rs`, extended)

```rust
/// Sibling of `build_bundle`. Same verification material (single leaf
/// certificate, PGI form 3 — cosign parity), `KindVersion { kind: "dsse",
/// version: "0.0.1" }`, inclusion proof mandatory (`ok_or(TransparencyLogUnavailable)`).
pub(super) fn build_dsse_bundle(
    cert: &FulcioCertificate,
    envelope: &DsseEnvelope,
    envelope_json: &[u8],
    rekor: &RekorEntry,
) -> Result<SignedBundle, SignErrorKind>;
```

## Attest pipeline (`oci/attest/pipeline.rs`)

Mirrors `SignPipeline` field-for-field where the concerns are identical, so the
two read as siblings.

```rust
pub struct AttestContext<'a> {
    pub identifier: &'a Identifier,
    pub platform: &'a Platform,
    pub signer: &'a dyn Signer,
    pub token_provider: &'a dyn TokenProvider,
    pub predicate_type: &'a PredicateType,
    /// The file's bytes, validated as JSON and otherwise untouched (D-b).
    pub predicate: &'a serde_json::value::RawValue,
    pub no_cache: bool,
    /// Present so the S1-E policy refusal can run here rather than being
    /// reinvented per call site (see the step order below).
    pub offline: bool,
    pub index: &'a Index,
    pub fulcio_url: &'a Url,
    pub rekor_url: &'a Url,
    pub state: &'a StateStore,
}

pub struct AttestResult {
    pub subject_digest: Digest,
    pub predicate_type: String,      // the RESOLVED URI (D-c)
    pub bundle_digest: Digest,
    pub referrer_digest: Digest,
    pub referrer_descriptor: Descriptor,
    pub certificate_identity: String,
    pub certificate_oidc_issuer: String,
}

pub struct AttestPipeline;

impl AttestPipeline {
    pub async fn run(client: &Client, ctx: AttestContext<'_>)
        -> Result<AttestResult, SignError>;
}
```

Step order follows `SignPipeline` with one deliberate divergence up front:
**the S1-E offline refusal first**, then the predicateType resolve and
provenance floor (row 21) — a pure function of `--type`, refused as a usage
error *before* any network or credential is touched, so a doomed run never
costs an OAuth flow — then SSRF floor on both trust URLs, resolve the
per-platform target, index indirection to the physical reference, referrers
capability probe, token acquisition → build Statement → `sign_dsse` → push
bundle blob → push referrer manifest with the three annotations (D1). No
fallback tag is ever written (ADR S1-F, unchanged).

**The offline gate is not optional and not inherited for free.** `ocx package
sign` refuses offline deliberately — `SignErrorKind::OfflineSignRefused`, exit 77
`PermissionDenied`, a policy rejection of the *action* rather than a passive
network failure — and it does so in `execute()` **before** the token resolver is
called, not inside it. So moving the token resolver into
`package_sign_common.rs` carries everything across *except* this. Attest gets its
own: `SignErrorKind::OfflineAttestRefused`, slug `offline_attest_refused`, exit
77, raised before token resolution, with a fixture mirroring
`test_sign_offline_refused`. The refusal helper moves into
`package_sign_common.rs` **together with** the resolver — forking it is precisely
what that extraction exists to prevent. Without this, two sibling commands doing
the same keyless work answer the same user error two different ways (77 vs a
transport 69/75), and a script branching on 77 to tell policy from outage
misdiagnoses.

## Verify additions (`oci/verify/`)

```rust
// oci/verify/pipeline.rs
pub enum VerifyContentMode {
    Signature,
    Attestation { predicate_type: Option<PredicateType> },
}

pub struct VerifyContext<'a> {
    // ... existing fields unchanged ...
    /// Explicit at every construction site (`Signature` at shipped callers);
/// `ocx package verify` behaviour is untouched.
    pub content: VerifyContentMode,
}

/// `VerifyResult` is UNCHANGED — no `attestation: Option<..>` field. One arity,
/// carried by `AttestationMatch` below (D-d): an `Option` here plus a `Vec` in
/// the report would be two contracts disagreeing about how many attestations a
/// subject can have.
pub struct VerifyResult { /* ... existing fields, untouched ... */ }

pub struct VerifiedAttestation {
    pub predicate_type: String,     // from the SIGNED payload (row 7)
    pub payload: Vec<u8>,           // original decoded Statement bytes
    /// The predicate as the verbatim sub-slice of `payload`, parsed at
    /// construction. This is what `sbom --output` writes, so the bytes reaching
    /// the user are the bytes the publisher signed (row 2).
    pub predicate: Box<serde_json::value::RawValue>,
    pub subject_digest: Digest,
}

/// One verified attestation plus the verification facts about the candidate it
/// came from. `VerifiedAttestation` alone cannot populate the report DTO:
/// `referrer_digest`, `certificate_identity`, `certificate_oidc_issuer` and
/// `signed_at` all live on `VerifyResult` (`pipeline.rs:114-125`), and D-e's
/// default listing promises all four.
pub struct AttestationMatch {
    pub verify: VerifyResult,
    pub attestation: VerifiedAttestation,
}
```

`signed_at` is `u64` epoch seconds on `VerifyResult` and RFC-3339-with-`Z` in the
DTO (PLAT-31); that conversion exists today as a private free function at
`command/package_verify.rs:305` (`iso8601`); it moves into the shared
`package_sign_common.rs` so both report paths reuse one helper instead of
re-deriving it.

```rust
// oci/verify/dsse.rs — OCX's defence-in-depth layer around
// `verifier.verify(subject_bytes, bundle, &PolicyDeferredToOcx, true)`, split
// per D-d: the structural half (caps, payloadType, `_type`, subject binding,
// signature count, predicateType) runs BEFORE that call so its precise error
// kinds are the ones a user sees; the tlog half (`verify_tlog_binding`, the
// row-13 validity re-assertion) runs AFTER it, over entry material the call
// has already SET/Merkle-checked.
//
// It takes NO verifying key: the crypto is delegated. An earlier draft passed a
// `CosignVerificationKey`, which implied this function replaced that call rather
// than layering over it — the reading under which attestations would silently
// lose three checks the signature path performs.
pub(super) fn verify_envelope(
    bundle: &Bundle,
    target_digest: &Digest,
    expected_predicate_type: Option<&PredicateType>,
) -> Result<VerifiedAttestation, VerifyErrorKind>;

/// D-g row 12, over the RECEIVED bytes: the canonicalized body's `payloadHash`
/// (sha256 over the received envelope's decoded payload bytes — rekor
/// `dsse:0.0.1` hashes the payload, not the PAE; hash-of-PAE is the Rekor v2
/// regime) and its `signatures[]` must
/// match the bundle's envelope. `envelopeHash` is NOT recomputed here — it
/// commits to the stringified envelope the signer uploaded, which cannot be
/// reconstructed byte-identically from a protobuf-JSON bundle. Rejects an
/// unaccepted (kind, version).
pub(super) fn verify_tlog_binding(
    entry: &BundleParts,
    envelope: &DsseEnvelope,
    payload: &[u8],
) -> Result<(), VerifyErrorKind>;

```

Row 13's re-assertion is **not** in `dsse.rs`, because it must run for both
content modes and the DSSE module runs for only one:

```rust
// oci/verify/tlog.rs — beside the SET and inclusion-proof checks, which already
// run for both modes. That file does NOT perform this check today: its module
// doc scopes it to the SET and the proof, and this change widens that doc.
//
/// Part III row 13 (CVE-2024-55655). OCX's own re-assertion of a window the
/// delegated verifier also checks — deliberate duplication, because the CVE is
/// precisely a library dropping it. Takes the already-parsed leaf:
/// `parse_certificate` (`verify/identity.rs:31`) runs once at `pipeline.rs:404`
/// and its `Certificate` is threaded here rather than re-parsed, so the window
/// checked is the window the identity check read.
pub(super) fn verify_integrated_time_within_certificate(
    integrated_time: i64,
    leaf: &Certificate,
) -> Result<(), VerifyErrorKind>;
```

## SBOM reading (`sbom.rs`, `sbom/cyclonedx.rs`)

```rust
/// What `--summary` reports. No trait, no format dispatch (D2/D-i).
pub struct SbomSummary {
    pub spec_version: String,
    pub serial_number: Option<String>,
    pub component_count: usize,
    pub top_level_component: Option<String>,
}

/// Parses CycloneDX 1.5, 1.6 and 1.7. Any other document — including a
/// CycloneDX outside that range — is an explicit refusal, never an empty
/// summary.
pub fn summarize_cyclonedx(document: &[u8]) -> Result<SbomSummary, SbomError>;
```

**Probe `specVersion` first, dispatch, then parse** — the DATA-FMT-02 shape, not
a direct `from_slice::<CycloneDx16>()`. A direct typed parse turns "this is
CycloneDX 1.4" into an opaque field-level serde error somewhere in the middle of
the document; the probe turns it into a version refusal naming the version. The
probe struct reads `specVersion` and nothing else, so a 1.7 document with fields
this reader does not know still reaches the right arm.

`serde_json`'s `unbounded_depth` stays **off** (it is off by default; the point is
that nothing here turns it on). The predicate is attacker-supplied JSON of
arbitrary nesting, and the default recursion limit is the only thing between a
hostile document and a stack overflow — which is a crash, not a caught error.

## Manager facade (`package_manager/tasks/attest.rs`, `.../sbom.rs`)

Mirrors `SignOptions` / `SignReport` / `sign_one` exactly.

```rust
pub struct AttestOptions {
    pub fulcio_url: Url,
    pub rekor_url: Url,
    pub identity_token: Option<Zeroizing<String>>,
    pub predicate_type: PredicateType,
    /// RAW FILE BYTES, not a parsed `Value`. Validated by a parse whose result is
    /// discarded, then spliced verbatim (D-b). A `Value` here would normalize
    /// whitespace and number spelling before anything downstream could preserve
    /// them, making checklist row 2's round-trip green for the wrong reason.
    pub predicate: Vec<u8>,
    pub no_cache: bool,
    pub no_tty: bool,
    /// Mirrors `SignOptions`: the S1-E refusal runs before token resolution.
    pub offline: bool,
}

pub struct AttestReport { pub result: AttestResult }

impl PackageManager {
    pub async fn attest_one(&self, package: &oci::Identifier, platform: &oci::Platform,
                            opts: AttestOptions) -> Result<AttestReport, PackageError>;
}
```

```rust
pub struct SbomOptions<'a> {
    pub policies: &'a [CompiledPolicy],
    pub client: &'a oci::Client,
    pub trust_root: &'a TrustRoot,
    pub rekor_url: &'a Url,
    pub offline: bool,
    pub state: &'a StateStore,
    pub no_cache: bool,
    /// `--type` narrowing. Without this field `PackageSbom.predicate_type` has
    /// no route into the pipeline and the flag is unreachable.
    pub predicate_type: Option<PredicateType>,
}

pub struct SbomReport { pub attestations: Vec<AttestationMatch> }

impl PackageManager {
    /// Read-only: routes through `read_only_view()` like `verify_one`, so
    /// reading an SBOM never grows the permanent local index.
    pub async fn sbom_one(&self, package: &oci::Identifier, platform: &oci::Platform,
                          opts: SbomOptions<'_>) -> Result<SbomReport, PackageError>;
}
```

`VerifyOptions` (`crates/ocx_lib/src/package_manager/tasks/verify.rs:39-55`) gains
the same kind of field. The CLI never builds a `VerifyContext` directly — the
chain is `CLI → VerifyOptions → verify_one() → VerifyContext` — so without this
row `verify --attestation` and `verify --type` are declared flags that nothing
reads:

```rust
pub struct VerifyOptions<'a> {
    // ... seven existing fields unchanged ...
    /// Not defaulted — every construction site states what it verifies
    /// (`Signature` at both shipped callers); today's behaviour is untouched.
    pub content: VerifyContentMode,
}
```

Both wrap failures with a `map_*_error` helper into
`PackageErrorKind::Internal(crate::Error::Sign(..) | ::Verify(..))`, matching
the shipped sign and verify facades so the exit code survives the batch
classifier.

## CLI (`crates/ocx_cli/src/command/`)

```rust
// package_attest.rs
pub struct PackageAttest {
    #[clap(short = 'p', long = "platform", required = true, value_name = "PLATFORM")]
    platform: oci::Platform,
    /// Predicate document to attach.
    #[clap(long = "predicate", required = true, value_name = "PATH")]
    predicate: PathBuf,
    /// Predicate type: an alias (cyclonedx, spdx, spdxjson, slsaprovenance, ...) or a full URI.
    #[clap(long = "type", required = true, value_name = "TYPE")]
    predicate_type: PredicateType,
    #[clap(long = "fulcio-url", value_name = "URL", default_value = DEFAULT_FULCIO_URL)]
    fulcio_url: String,
    #[clap(long = "rekor-url", value_name = "URL", default_value = DEFAULT_REKOR_URL)]
    rekor_url: String,
    #[clap(long = "identity-token-file", value_name = "PATH", conflicts_with = "identity_token_stdin")]
    identity_token_file: Option<PathBuf>,
    #[clap(long = "identity-token-stdin", conflicts_with = "identity_token_file")]
    identity_token_stdin: bool,
    #[clap(long = "no-tty")]
    no_tty: bool,
    #[clap(long = "no-cache")]
    no_cache: bool,
    identifier: options::Identifier,
}
```

The token-resolution precedence (`--identity-token-file` > `--identity-token-stdin`
> `OCX_IDENTITY_TOKEN`), the Unix `O_NOFOLLOW` + owner + `0o077` permission
gate, and the deliberate absence of a `--identity-token <VALUE>` flag are
**reused verbatim** from `package_sign.rs`. That resolver moves to
`command/package_sign_common.rs` and is called by both — it is security-critical
and must not fork (the flat, no-`mod.rs` `<command>_common.rs` convention). The
offline refusal moves with it (see the attest pipeline step order).

**`--predicate` opens with `O_NOFOLLOW`, and deliberately checks nothing else.**
`ELOOP` is a refusal naming CWE-367, and the bytes are read from the same handle
that was opened, closing the stat/read race. The asymmetry with
`--identity-token-file` — which additionally rejects a file not owned by the
effective uid and any file with `mode & 0o077` set — is a decision, not an
oversight: a predicate is public data destined for publication, and a 0644 SBOM
written by an earlier CI step is the normal case, so an ownership or mode gate
would reject the ordinary invocation while protecting nothing. The symlink refusal
is kept because the consequence of following one is not confidentiality-shaped but
irreversible: whatever the link points at gets embedded, signed with the caller's
identity, pushed, and hashed into an append-only public log.

```rust
// package_sbom.rs
pub struct PackageSbom {
    #[clap(short = 'p', long = "platform", required = true, value_name = "PLATFORM")]
    platform: oci::Platform,
    /// Write the verified predicate document to PATH ("-" for stdout).
    #[clap(long = "output", short = 'o', value_name = "PATH", conflicts_with = "summary")]
    output: Option<PathBuf>,
    /// Parse the SBOM and report component counts (CycloneDX 1.5-1.7 only).
    #[clap(long = "summary", conflicts_with = "output")]
    summary: bool,
    /// Restrict to one predicate type.
    #[clap(long = "type", value_name = "TYPE")]
    predicate_type: Option<PredicateType>,
    #[clap(long = "certificate-identity", value_name = "IDENTITY",
           requires = "certificate_oidc_issuer")]
    certificate_identity: Option<String>,
    #[clap(long = "certificate-oidc-issuer", value_name = "URL",
           requires = "certificate_identity")]
    certificate_oidc_issuer: Option<String>,
    #[clap(long = "sigstore-trusted-root", value_name = "PATH")]
    sigstore_trusted_root: Option<PathBuf>,
    #[clap(long = "rekor-url", value_name = "URL", default_value = DEFAULT_REKOR_URL)]
    rekor_url: String,
    #[clap(long = "no-cache")]
    no_cache: bool,
    identifier: options::Identifier,
}
```

`ocx package verify` gains `--attestation` (switches `VerifyContentMode`) and
`--type` (narrows the predicate type). `ocx package push` gains `--sbom PATH`,
sugar for the cyclonedx path (D-f).

## Report DTOs (`crates/ocx_cli/src/api/data/`)

Sibling types per verb — never a mutation of `VerificationReport` or
`SignatureReport`.

```rust
// api/data/attestation.rs
pub struct AttestationReport {
    pub identifier: String,
    pub platform: String,
    pub subject_digest: String,
    pub predicate_type: String,   // the RESOLVED URI (D-c makes this load-bearing)
    pub bundle_digest: String,
    pub referrer_digest: String,
    pub certificate_identity: String,
    pub certificate_oidc_issuer: String,
}

// api/data/sbom.rs — named apart from the lib-side `SbomReport`, following the
// existing pair convention (lib `SignReport` / CLI `SignatureReport`,
// lib `AttestReport` / CLI `AttestationReport`).
pub struct SbomListingReport { entries: Vec<SbomEntry> }

pub struct SbomEntry {
    pub predicate_type: String,
    pub subject_digest: String,
    pub referrer_digest: String,
    pub certificate_identity: String,
    pub certificate_oidc_issuer: String,
    pub signed_at: String,                  // RFC 3339, explicit Z (PLAT-31)
    pub summary: Option<SbomSummaryOut>,    // populated only under --summary
}
```

One `SbomEntry` per `AttestationMatch`: the first two fields come from
`VerifiedAttestation`, the next four from the match's `VerifyResult`. That is why
`SbomReport` carries matches rather than bare attestations (Part IV, above) — a
`Vec<VerifiedAttestation>` cannot populate four of these seven fields.

```rust
// api/data/push.rs — existing type, one additive field (D-f)
pub struct PushReport {
    // ... the five existing machine-readable keys, untouched: ocx-mirror
    //     pipeline push keys its go/no-go off `status` ...
    /// `None` unless `--sbom` was passed. Failure carries the error slug in the
    /// existing envelope shape (CLI-04) rather than a bespoke string.
    pub attestation: Option<AttestationOutcome>,
}

pub enum AttestationOutcome {
    Succeeded { referrer_digest: String, predicate_type: String },
    Failed { kind: String, message: String },
}
```

**Every field carrying registry-sourced text** — `identifier`,
`certificate_identity`, `certificate_oidc_issuer`, `predicate_type` when it came
off the wire, and every `summary` string — passes the SEC-31/34 terminal
sanitizer at the render boundary. `predicate_type` is the sharpest of these: it
is attacker-controlled inside a signed payload, so being *authentic* says
nothing about being *printable*. Plain-mode column budget applies (≤5 columns);
the full 71-column digest appears at most once per view.

---

# Error Variants and Exit Codes

**No new exit code** (D-h). Every row maps onto the pinned table.

## New `VerifyErrorKind` variants

Seventeen new variants. None reuses an existing one: the shipped
`SubjectDigestMismatch` (`verify/error.rs:120-128`) means *the registry served
subject-manifest bytes that do not hash to the resolved digest* — a transport
integrity failure — and is left untouched. A Statement whose `subject[]` does not
bind the target is a different fact about a different document, and collapsing
the two would make one slug mean two things in a script's `case`.

| Variant | Slug (`kind_detail`) | Exit | When |
|---|---|---|---|
| `AttestationNotFound` | `attestation_not_found` | 79 `NotFound` | The scan ends with zero matches for the request: no attestation-mode referrer at all, or none whose **signed** predicateType matches the requested `--type`. Narrowing is by the verified payload after fetch-and-parse — annotations never exclude (D-e) |
| `PredicateTypeMismatch { expected, actual }` | `predicate_type_mismatch` | 65 `DataError` | Signed predicateType ≠ requested, or ≠ the referrer annotation |
| `StatementSubjectMismatch { expected, actual }` | `statement_subject_mismatch` | 65 | No subject in the signed Statement binds the target digest (row 4) |
| `StatementSubjectAbsent` | `statement_subject_absent` | 65 | Zero-subject Statement (row 5) |
| `StatementSubjectWeakAlgorithm { algorithms }` | `statement_subject_weak_algorithm` | 65 | A subject's DigestSet carries no `sha256` entry (row 6) |
| `BuilderMismatch { expected, found: Option<String> }` | `builder_mismatch` | 65 | A policy `builder` pin against a provenance predicate whose builder identity is absent, unparseable, or different (D-j — refusal, never a skip) |
| `StatementTypeUnsupported { statement_type }` | `statement_type_unsupported` | 65 | `_type` outside `ACCEPTED_STATEMENT_TYPES` |
| `PayloadTypeUnsupported { payload_type }` | `payload_type_unsupported` | 65 | `payloadType` ≠ `application/vnd.in-toto+json` (row 3) |
| `MultipleSignatures { count }` | `multiple_signatures` | 65 | Bundle DSSE envelope carries ≠ 1 signature (row 8) |
| `MultipleAttestations { predicate_types, referrer_digests }` | `multiple_attestations` | 65 | `sbom --output` found >1 verified match (D-e). `predicate_types` is every distinct type in the set, sorted — one type named out of a mixed set is wrong about the rest and hides the `--type` value that resolves it |
| `UnsupportedTlogEntryKind { kind, version }` | `unsupported_tlog_entry_kind` | 65 | `kindVersion` outside `ACCEPTED_TLOG_KINDS` (D-g) |
| `TlogBindingMismatch` | `tlog_binding_mismatch` | 65 | The canonicalized body's `payloadHash` or `signatures[]` does not match the received envelope (row 12) |
| `CertificateValidityWindow { integrated_time, not_before, not_after }` | `certificate_validity_window` | 65 | `integratedTime` outside the leaf certificate's window (row 13, CVE-2024-55655) |
| `AttestationTooLarge { limit, actual }` | `attestation_too_large` | 65 | `MAX_ATTESTATION_ENVELOPE_BYTES` |
| `AttestationPayloadTooLarge { limit, actual }` | `attestation_payload_too_large` | 65 | `MAX_STATEMENT_PAYLOAD_BYTES` |
| `TooManyAttestations { limit }` | `too_many_attestations` | 65 | `MAX_ATTESTATION_CANDIDATES` |
| `AttestationBudgetExhausted { limit }` | `attestation_budget_exhausted` | 65 | `MAX_TOTAL_ATTESTATION_BYTES` |

`TlogBindingMismatch` is deliberately **not** named `EnvelopeHashMismatch`: OCX's
own binding check never recomputes an envelope hash (D-g — the delegated
verifier reconstructs it from proto3 JSON and fails closed), so a name promising
that comparison would describe a check this code does not make. The sign-side
`envelopeHash` assertion is an invariant of the upload path, not a verify-time
variant.

## New `SignErrorKind` variants

| Variant | Slug | Exit | When |
|---|---|---|---|
| `PredicateNotJson` | `predicate_not_json` | 65 `DataError` | `--predicate` file is not parseable JSON |
| `PredicateTooLarge { limit, actual }` | `predicate_too_large` | 65 | `MAX_PREDICATE_FILE_BYTES` |
| `ProvenanceVersionUnsupported { resolved }` | `provenance_version_unsupported` | 64 `UsageError` | Attach resolved a provenance predicateType below v1.0 (#102, Part III row 21); the message names `--type slsaprovenance1` as the fix |
| `OfflineAttestRefused` | `offline_attest_refused` | 77 `PermissionDenied` | `--offline` with `attest`/`push --sbom` — refused before token resolution (Part IV, attest pipeline) |

`ProvenanceVersionUnsupported` is 64 rather than 65 because the offending value
came from the invocation, not from data: the user typed a `--type` alias that
resolves below the floor, and the remedy is a different flag value.
`OfflineAttestRefused` reuses 77 verbatim from the shipped
`test_sign_offline_refused` contract — attesting is signing, and a policy refusal
must not classify differently depending on which verb reached it.

An unparseable `--type` never reaches the library: it is a clap value-parse
failure at the CLI boundary → exit 64 `UsageError`.

## Renames (D3, pre-release, no alias)

The slug and the Rust identifier move together. Renaming only the slug would
leave `TransparencyLogUnavailable` naming a condition the user-visible contract no longer
calls that, and the next reader would have to hold two vocabularies at once.
Exit **83 is unchanged** at every site.

| Site | From | To |
|---|---|---|
| `crates/ocx_lib/src/oci/verify/error.rs:188` | `VerifyErrorKind::TransparencyLogUnavailable` | `TransparencyLogUnavailable` |
| `crates/ocx_lib/src/oci/verify/error.rs:395` | slug `rekor_unavailable` | `transparency_log_unavailable` |
| `crates/ocx_lib/src/oci/verify/error.rs:666` | pinned-slug test row | same |
| `crates/ocx_lib/src/oci/sign/error.rs` (variant) | `SignErrorKind::TransparencyLogUnavailable` | `TransparencyLogUnavailable` |
| `crates/ocx_lib/src/oci/sign/error.rs:210` | slug `rekor_unavailable` | `transparency_log_unavailable` |
| `crates/ocx_lib/src/oci/sign/error.rs:387` | pinned-slug test row | same |
| `crates/ocx_cli/src/error_envelope.rs:307` | serde-name pinning test | same |
| `crates/ocx_cli/src/error_envelope.rs:59` | `ErrorCategory::TransparencyLogUnavailable` variant declaration — `error.kind` is this enum's serde snake_case name, so this row is the *wire* half of the rename, not internal vocabulary | `TransparencyLogUnavailable` |
| `crates/ocx_cli/src/error_envelope.rs:99` | `ExitCode::TransparencyLogUnavailable => Self::TransparencyLogUnavailable` arm | both sides renamed |
| `crates/ocx_cli/src/error_envelope.rs:548` | `(ExitCode::TransparencyLogUnavailable, ErrorCategory::TransparencyLogUnavailable)` total-map test row | same |
| `crates/ocx_lib/src/cli/exit_code.rs:77` | `ExitCode::TransparencyLogUnavailable = 83` | `TransparencyLogUnavailable = 83` |
| `crates/ocx_lib/src/cli/classify.rs` | classification arms naming the variant | same |
| `website/src/docs/reference/command-line.md:3770` | `rekor_unavailable` row | `transparency_log_unavailable` |
| `website/src/docs/reference/command-line.md:3778` | `rekor_unavailable` row | `transparency_log_unavailable` |
| `website/src/docs/reference/command-line.md:3934` | `rekor_unavailable` row | `transparency_log_unavailable` |
| `test/tests/test_verify.py:446`, `test/tests/test_sign.py:737` | acceptance assertions on the slug | same |

The table above names the contract-bearing sites; it is **not** the full
mechanical census. `TransparencyLogUnavailable|rekor_unavailable` occurs in 12 files —
additionally `oci/sign/rekor.rs` (8 hits), `oci/sign/bundle.rs` (5),
`oci/verify/pipeline.rs` (7 — incl. the live construction at `:549` and
`failure_rank` at `:1048`), `package_manager/tasks/auto_verify.rs:301` (doc)
and `test/tests/fixtures/adversarial.py:207` — and the `--trusted-root` flag
is defined at `crates/ocx_cli/src/command/package_verify.rs:107` with doc/test
references across `oci/verify.rs`, `trust_resolve.rs`, `trust_root.rs`,
`verify/error.rs`, `tasks/auto_verify.rs`, `app/context.rs:984`, the
acceptance arg builder `test/tests/fixtures/sigstore_stack.py:89`,
`test_verify.py:59`, `test_trust_policy.py:64`, `test_offline_verify.py:200`
(asserts the flag in stderr), assorted docstrings, `test/sigstore/README.md`,
`environment.md:222,230` and `command-line.md:3803-3844`. The implementing WP
runs the grep census fresh rather than trusting any line list. One site is
**exempt by meaning**: `test/tests/test_cosign_interop.py:94,99,145` spell
`--trusted-root` as *cosign's own flag* — a mechanical sweep that renames them
corrupts the interop test.

Nothing is released, so the rename lands in place with no alias, no
`#[serde(rename)]` shim and no deprecation window (D3). **The commit subject
carries the break** — subjects are the changelog (`cliff.toml` renders one bullet
per commit from the subject alone), so the subject reads as the release-note
sentence and the body carries the reasoning.

Both pinned slug tables (`kind_detail_values_are_stable`) grow: verify 23 → 40
rows, sign 12 → 16. **Those tables are the contract**: a new variant that does
not appear in them is a defect, not an omission.

---

# Affected Code Surfaces

| Path | Change |
|---|---|
| `crates/ocx_lib/src/oci/attest.rs` | **new** — aggregator, the five `MAX_*` constants, the type/kind constant tables |
| `crates/ocx_lib/src/oci/attest/dsse.rs` | **new** — PAE, `DsseEnvelope`, envelope hashes |
| `crates/ocx_lib/src/oci/attest/statement.rs` | **new** — Statement build/parse/`binds_subject` |
| `crates/ocx_lib/src/oci/attest/predicate.rs` | **new** — `PredicateType`, alias table, `CosignPredicate` wrapper |
| `crates/ocx_lib/src/oci/attest/pipeline.rs` | **new** — `AttestContext` / `AttestResult` / `AttestPipeline` |
| `crates/ocx_lib/src/oci/verify/dsse.rs` | **new** — `verify_envelope` (the defence-in-depth layer over received bytes) + `verify_tlog_binding` |
| `crates/ocx_lib/src/oci/verify/tlog.rs` | row-13 validity-window re-assertion, beside the existing SET and inclusion-proof checks; module doc widened to say so |
| `crates/ocx_lib/src/sbom.rs`, `sbom/cyclonedx.rs` | **new** — CycloneDX 1.5–1.7 reader (top-level, no `oci` dependency) |
| `crates/ocx_lib/src/oci/sign/signer.rs` | `sign_dsse` added to the trait; `KeylessSigner` implements it; `issue_ephemeral_certificate` extracted so both sign paths share one issuance |
| `crates/ocx_lib/src/oci/sign/rekor.rs` | `upload_dsse_entry` |
| `crates/ocx_lib/src/oci/sign/bundle.rs` | `build_dsse_bundle` |
| `crates/ocx_lib/src/oci/sign/error.rs` | 4 variants + 4 slug rows + the 83 rename |
| `crates/ocx_lib/src/oci/verify/error.rs` | 17 variants + 17 slug rows + the 83 rename |
| `crates/ocx_lib/src/oci/verify/pipeline.rs` | `VerifyContentMode` on `VerifyContext`; the `:498` gate becomes a mode check; `from_bundle` takes the mode; mode- and predicate-type-narrowed candidate filter (annotations order only, D-e) |
| `crates/ocx_lib/src/oci/referrer/manifest.rs` | `ReferrerManifest` gains `annotations: Option<BTreeMap<String, String>>` with `skip_serializing_if = "Option::is_none"` — load-bearing, so existing signature-manifest bytes are unchanged when `None` |
| `crates/ocx_lib/src/oci/referrer/media_types.rs` | 5 annotation constants (data-only, extended not duplicated) |
| `crates/ocx_lib/src/trust.rs` | `KeylessMatcher` nesting; `PolicyBackend`; `builder` field; `NoBackend` error |
| `crates/ocx_lib/src/package_manager/tasks/attest.rs` | **new** — `AttestOptions` / `AttestReport` / `attest_one` |
| `crates/ocx_lib/src/package_manager/tasks/sbom.rs` | **new** — `SbomOptions` / `SbomReport` / `sbom_one` |
| `crates/ocx_lib/src/package_manager/tasks/verify.rs` | `VerifyOptions` gains `content: VerifyContentMode`, threaded to `VerifyContext` |
| `crates/ocx_lib/src/package_manager/tasks.rs` | aggregator rows |
| `crates/ocx_lib/src/publisher.rs` (`PushOutcome`) | **not added** — superseded by the D-f amendment (2026-08-20): `platform_digests` was skipped, the property holds by construction via `AttestPipeline`'s index-indirection resolve. `PushOutcome` is `#[non_exhaustive]` regardless, for the unrelated `ocx-mirror` path-dependency reason already stated in D-f |
| `crates/ocx_lib/Cargo.toml` | `serde_json` gains the `raw_value` feature. The workspace declaration (root `Cargo.toml:66`) is a bare version string; this member manifest (`:50`) is where the feature list already lives, so `raw_value` joins `preserve_order` there and reaches the whole graph by unification |
| `crates/ocx_cli/src/command/package_attest.rs` | **new** |
| `crates/ocx_cli/src/command/package_sbom.rs` | **new** |
| `crates/ocx_cli/src/command/package_sign_common.rs` | **new** — the OIDC token resolver moved out of `package_sign.rs`, shared verbatim |
| `crates/ocx_cli/src/command/package_sign.rs` | calls the extracted resolver |
| `crates/ocx_cli/src/command/package_verify.rs` | `--attestation`, `--type`, `--trusted-root` → `--sigstore-trusted-root` |
| `crates/ocx_cli/src/command/package_push.rs` | `--sbom PATH`; combines the push and attest exit codes in the handler |
| `crates/ocx_cli/src/command/package.rs` | two dispatcher variants |
| `crates/ocx_cli/src/api/data/attestation.rs`, `sbom.rs` | **new** DTOs |
| `crates/ocx_cli/src/api/data/push.rs` | `PushReport` gains `attestation: Option<AttestationOutcome>` (D-f) |
| `crates/ocx_cli/src/api/data.rs` | two `pub mod` rows |
| `crates/ocx_lib/src/cli/exit_code.rs`, `crates/ocx_lib/src/cli/classify.rs`, `crates/ocx_cli/src/error_envelope.rs` | the 83 rename (see the rename table above) |
| `crates/ocx_schema/…` → `config/v1.json`, `project/v1.json` | regenerate (`task schema:generate`) — **both**, per `adr_trust_policy.md` |

# Documentation Surfaces

Enumerated because a plan that omits them ships an undocumented contract.

| Surface | Change |
|---|---|
| `website/src/docs/in-depth/signing.md` | Attestation section: what `attest` writes, the exact referrer shape, the `--type` table **with the resolved URI per alias** (the `slsaprovenance` → v0.2 trap made explicit), the D-b `_type` deviation and why, push-then-attest failure recovery. One sentence on the `payloadType` deviation: in-toto v1.2 broadened the accepted set, and OCX still requires exactly `application/vnd.in-toto+json` — a deliberate narrowing, stated so a reader meeting a v1.2 producer knows it is a decision rather than a gap. **Slice Boundary / Current Limitations / Deferred** blocks updated per SEC-32: DSSE moves out of Deferred; freshness and rollback are stated as *not* provided; Rekor v2 and `(t,n)` multi-signer stay Deferred. |
| `website/src/docs/in-depth/threat-model.md` | **new or extended** — the unsigned-referrer-linkage attack (CVE-2026-31830) and how subject binding closes it; what a verified attestation does and does not prove. |
| `website/src/docs/reference/command-line.md` | `ocx package attest`, `ocx package sbom`, `verify --attestation/--type`, `push --sbom`, `--trusted-root` → `--sigstore-trusted-root`. **Plus three existing slug rows** — `:3770`, `:3778`, `:3934` — where `rekor_unavailable` becomes `transparency_log_unavailable`. These are the user-facing half of the rename table; missing one leaves the published slug reference contradicting the binary. |
| `website/src/docs/reference/configuration.md` | `[trust.policy.keyless]` nesting, `builder`, exactly-one-backend, the future `[trust.policy.key]` slot. |
| `website/src/docs/reference/environment.md` | `OCX_SIGSTORE_TRUSTED_ROOT` cross-reference to the renamed flag. |
| `website/src/docs/reference/environment.md` | `SOURCE_DATE_EPOCH` — `ocx package sign`/`attest` honor it for the referrer `created` annotation (reproducible builds); previously undocumented as something ocx READS | WP11 |
| `website/src/docs/reference/configuration.md`, `in-depth/self-hosted-sigstore.md`, `user-guide.md`, `in-depth/signing.md`, `reference/command-line.md` | every `[[trust.policy]]` example (19 across the five files) rewritten to the nested `[trust.policy.keyless]` form — the flat form now exits 78 | WP11 |
| `website/src/docs/in-depth/self-hosted-sigstore.md` | Rekor `dsse` entry-kind support note; what the compose stack does and does not cover (no Rekor v2). |
| `website/src/docs/user-guide.md` | One paragraph placing attestation next to signing; link out. |
| `website/src/docs/user-guide/attach-an-sbom.md` | **new** — publisher path, with an asciinema cast via `Terminal.vue`, recorded through `recordings.taskfile.yml`. Under the existing `user-guide/` directory (D8: existing structure only — `use-cases/` does not exist and is not created). |
| `website/src/docs/user-guide/verify-an-sbom.md` | **new** — consumer path, same cast mechanism. |
| `website/.vitepress/config.mts` | Sidebar rows for `threat-model.md` and both new user-guide pages — every docs page is registered by a literal sidebar entry, so a page without a row here ships unreachable. |
| `.claude/rules/subsystem-cli-commands.md` | Two new command rows + the changed verify flags. |
| `.claude/rules/arch-principles.md` | ADR index row; `oci/attest` + `sbom` in "Where Features Land". |
| `.claude/rules/quality-rust-exit_codes.md` | 83's doc comment reworded for the slug rename (number unchanged). |
| `.claude/rules.md` | Catalog rows for any rule file touched, same commit. |

# Testing Strategy

Against the **real** docker-compose Sigstore stack (dex, Fulcio 1.8.8, Rekor
1.4.2 + Trillian, TesseraCT) plus zot 2.1.18. `registry:2` remains the exit-84
fixture. `fake_sigstore.py` is gone and stays gone.

**Exit codes and stream contents are asserted separately** (TEST-10) — a
combined-output assertion cannot tell a code change from a wording change.

## Interop, both directions

| Test | Asserts |
|---|---|
| `ocx package attest` → `cosign verify-attestation` | The bytes OCX writes are readable by the compatibility target (D1). This is the test the whole wire-shape decision exists to pass. |
| `cosign attest` → `ocx package verify --attestation` | Includes the `_type: v0.1` case, which is exactly why D-b accepts it. |
| `ocx package attest` → `ocx package sbom --output` | Round-trip: the extracted bytes are **byte-identical** to the input predicate (checklist row 2 — this is the seam, not a source scan). The input fixture is a **pretty-printed** CycloneDX file, chosen because a compact one round-trips even through a re-serializing implementation and would make the test green for the wrong reason. |

## Golden shapes

Committed fixtures, regenerated by a named command:

- The referrer manifest OCX pushes (top-level `artifactType`, empty config
  descriptor actively pushed, one bundle layer, the three annotations).
- The DSSE envelope and the assembled bundle JSON, as two golden-shape tests
  built on the existing `test_verify.py:245`/`:304` pair:
  `test_attest_dsse_envelope_golden_shape` and
  `test_sbom_dsse_envelope_golden_shape`. Two, not one: `attest` and
  `push --sbom` reach the same builder by different routes, and a single golden
  cannot show that both arrive at the same bytes.
- A PAE vector: known `payload_type` + payload → known bytes.
- The `dsse:0.0.1` canonicalized body OCX recomputes against.

## Negative paths — one per checklist row that can fail

Each asserts a **specific** error kind, never merely non-zero:

| Fixture | Expected |
|---|---|
| Valid attestation for artifact A, served as a referrer of artifact B | `StatementSubjectMismatch` (65) |
| Zero-subject Statement | `StatementSubjectAbsent` (65) |
| Matching `md5`, non-matching `sha256` in the DigestSet | `StatementSubjectMismatch` (65) |
| DigestSet carrying no `sha256` entry at all | `StatementSubjectWeakAlgorithm` (65) |
| Annotation says `cyclonedx`, signed payload says `vuln` | `PredicateTypeMismatch` (65) |
| `payloadType: application/json` | `PayloadTypeUnsupported` (65) |
| Two signatures in the envelope | `MultipleSignatures` (65) |
| A hostile `keyid` on the one signature, everything else valid | **verifies** — row 10: `keyid` is a lookup hint, never a security decision. A positive fixture in a negative table, deliberately: the failure mode here is code that starts trusting the field |
| `_type: https://in-toto.io/Statement/v9` | `StatementTypeUnsupported` (65) |
| Valid signature over a payload that is not a parseable Statement (CVE-2026-39395 shape) | A specific parse-refusal kind — **never** success. The CVE is exactly "signature verified, therefore accepted" |
| Canonicalized body whose `signatures[]` does not match the bundle envelope | `TlogBindingMismatch` (65) |
| `integratedTime` outside the leaf certificate's validity window | `CertificateValidityWindow` (65) |
| `intoto:0.0.1` tlog entry | `UnsupportedTlogEntryKind` (65) |
| 33 MiB envelope | `AttestationTooLarge` (65) |
| Envelope whose base64 payload decodes past 16 MiB | `AttestationPayloadTooLarge` (65) |
| 33 attestation referrers | `TooManyAttestations` (65) |
| Referrer set under the count cap whose cumulative bytes cross `MAX_TOTAL_ATTESTATION_BYTES` | `AttestationBudgetExhausted` (65) — the count cap and the byte budget are two different attacks, so a fixture tripping only the count proves nothing about the budget |
| A subject with referrers, none of whose **signed** predicateType matches the request (annotations never exclude — D-e) | `AttestationNotFound` (79): a `--type` narrowing miss records nothing per candidate (`TypeNarrowed` consumes a slot silently — S-017), so a scan ending with zero matches reports not-found; a payload-vs-annotation disagreement is the separate `PredicateTypeMismatch` (65) case. Without this fixture the empty-set path is untested |
| `--predicate` pointing at a file that is not parseable JSON | `PredicateNotJson` (65) |
| `--predicate` file one byte over `MAX_PREDICATE_FILE_BYTES`, plus one exactly at the cap | `PredicateTooLarge` (65) and success — the boundary is tested from both sides |
| `--predicate` pointing at a symlink | refusal naming CWE-367 (Part IV, CLI — the `--predicate` open hardening) |
| `--type slsaprovenance` at attach | `ProvenanceVersionUnsupported` (64), message naming `--type slsaprovenance1` |
| `builder` pin + v1 provenance whose `runDetails.builder.id` differs; pin + predicate with the field absent | `BuilderMismatch` (65) in both — found `Some(other)` vs found `None`; a v0.2 fixture asserts the `builder.id` dispatch path (D-j) |
| `ocx package attest --offline`, and `push --sbom --offline` | `OfflineAttestRefused` (77), mirroring `test_sign_offline_refused` |
| Attestation signed by an identity no policy admits | existing identity-mismatch kind (unchanged) |
| Rekor unreachable mid-attest | 83, slug `transparency_log_unavailable` |
| `registry:2` target | 84 `ReferrersUnsupported` |
| `ocx package sbom` with neither flags nor a matching policy | 64 `NoIdentityProvided` |
| `ocx package sbom --output` with two verified attestations of the requested type | `MultipleAttestations` (65), naming both referrer digests |
| `ocx package sbom --output -` with stdout attached to a TTY | typed refusal — raw predicate bytes are unsanitized (D-e) |
| A non-CycloneDX predicate under `--summary` | that entry moves to `refused` (`sbom_summary_failed`), exit 0 — not an empty summary, and not a refusal of the readable documents beside it |

## Cap and arity fixtures

Two pairs that no single-sided fixture can prove:

| Fixture | Asserts |
|---|---|
| One 1 MiB bundle, verified twice: once in `Signature` mode, once in `Attestation` mode | **Rejected** the first time, **accepted** the second. A cap that is never exercised from both sides is indistinguishable from one cap applied everywhere — this pair is what makes "caps are selected by the requested mode" a checked claim rather than a stated one (D-d). |
| One subject carrying one signature referrer **and nine attestation referrers** | `ocx package verify` (signature mode) still succeeds. Mode-mismatched candidates are discriminated after fetch and parse, so if they consumed the signature mode's candidate budget (8) the nine attestations would starve the one signature out and verification of a correctly signed artifact would fail for a reason the user cannot see. |

## Red-before-green

Four properties get an explicit demonstrated-red step, because each is the shape
that passes for the wrong reason:

1. **PAE input.** Mutate `pae()` to consume the base64 *text* instead of the
   decoded payload bytes; the golden PAE vector and the interop tests must red.
   Both spellings produce a stable, plausible-looking digest, so nothing but a
   demonstrated red distinguishes them (row 1).
2. **The mode gate.** Mutate `VerifyContentMode` matching to accept either
   content in either mode; the two direction tests must red. A gate that only
   ever sees one content type is indistinguishable from no gate.
3. **Subject binding.** Delete the `binds_subject` call; the cross-subject
   fixture must red **on its asserted kind**: the delegated call still refuses
   the splice, but with its generic mapped error rather than
   `StatementSubjectMismatch`, so an assertion on the specific kind is what
   makes this mutation detectable. This is the CVE-2026-31830 shape and the
   single highest-value assertion in the suite.
4. **Predicate byte fidelity.** Replace the `RawValue` splice with a
   parse-and-re-serialize on either the write or the read side; the
   pretty-printed round-trip must red. Byte identity is the whole property, and
   a compact fixture would survive the mutation.

## Unit-level

- `pae()` against the spec's own example.
- `PredicateType` `FromStr`/`uri()` round-trip across the whole alias table,
  with `slsaprovenance` → v0.2 asserted **explicitly** so a later "fix" to v1
  breaks a test rather than interop.
- `binds_subject` table-driven across the row 4/5/6 cases.
- `trust_config_tolerates_unknown_fields_from_newer_ocx` extended with an
  unknown key inside `[trust.policy.keyless]`.
- `CompiledPolicy::try_from` with zero backends → `NoBackend`.

---

# Part V — Day-1 Spike Scope

The spike runs `cosign attest` / `cosign verify-attestation` and a real
`dsse:0.0.1` upload against the compose stack, to pin bytes the research could
only reach at medium confidence. **It may adjust constants and serialization
details. It may not adjust the architecture.**

## The spike MAY adjust

| # | Finding | What moves |
|---|---|---|
| 1 | Whether `artifactType` sits at the manifest top level or on the config descriptor (one summariser pass disagreed with BUNDLE_SPEC) | The `ReferrerManifest` serialization for attestation referrers. One field position — **but not only that**: the OCI 1.1 Referrers API filters on the top-level `artifactType`, so if the field moves to the config descriptor, the registry-side filter matches nothing and discovery falls back to fetch-and-parse content discrimination alone (annotations are hints, never authoritative — D-e). The spike therefore re-confirms D-e's filter decision with whatever it finds, rather than treating the position as cosmetic. |
| 2 | Whether `cosign verify-attestation` accepts a `_type: .../v1` Statement | `STATEMENT_TYPE_WRITTEN` flips to v0.1, **and** D-b plus the `signing.md` deviation paragraph are amended in the same change — the ADR's recorded reason for writing v1 would otherwise contradict the constant, and the published deviation note would describe a deviation that no longer exists. `ACCEPTED_STATEMENT_TYPES` is unchanged either way — it already holds both. |
| 3 | Whether rekor 1.4.2 accepts the `dsse:0.0.1` `proposedContent` as researched, and the exact field spelling | The `upload_dsse_entry` request body. |
| 4 | The exact `canonicalizedBody` rekor returns for a `dsse` entry | The recomputation in `verify_tlog_binding`. |
| 5 | The literal annotation values cosign writes | `BUNDLE_CONTENT_DSSE` and siblings in `media_types.rs`. |

Each of the five is a constant, a field position, or one table row. None
requires a new type, a new module, a new error family, a different pipeline, or
a different storage shape.

## The spike MAY NOT adjust

- **The storage shape** (D1) — that is owner-fixed and the alternatives are
  recorded as rejected.
- **The verify composition** (D-d) — `verifier.verify` runs unchanged for both
  modes and `verify_envelope` layers OCX's own checks over the received bytes
  afterwards. A spike finding that some check is redundant with sigstore's is
  not a licence to drop the layer: the delegation gaps it closes are recorded,
  and one of them is a CVE class.
- **The accepted tlog entry kinds** (D-g) — `{dsse:0.0.1}` and nothing else.
  `intoto:0.0.2` is settled as Not Doing, on the ground that sigstore's
  `tlog_entry_for_dsse` hard-rejects any other kind, so no green path through
  the delegated verifier exists for it. A spike observing that rekor *accepts*
  the kind does not reopen this — acceptance by the log was never the blocker.
- **The annotation set** (D1) — all three written, read as hints only.
- **The error taxonomy or exit codes** (D-h) — no new code, no new family.
- **The module layout** (D-i) or the dependency direction `verify → attest`.
- **Any Part III requirement.** If the spike shows a requirement is expensive,
  that is a cost, not a licence.
- **The `ocx package sbom` verify-by-default contract** (D-e).

Five findings the security review raised against Part V — envelope-hash
recomputation, the `intoto:0.0.2` provenance, the annotation filter, arity, and
the chain/SCT/validity-window source — are **not** spike questions. They are
resolved in Parts II–IV above; the spike inherits them settled and may not
re-open them by observation.

If a spike finding cannot be absorbed by the five rows above, it is an ADR
amendment with the owner in the loop — not an implementation decision.

---

# Consequences

**Positive.**

- The delta really is the five items in Context. Everything else composes, so
  the review surface is small and concentrated on the parts that are genuinely
  new.
- One pipeline means the next security fix to chain walking, SET verification or
  identity matching lands once and covers both content kinds.
- No new exit code, no new error family, no second trust root, no format trait:
  the taxonomy a script already branches on grows by slugs only, plus one
  renamed slug that a script matching `rekor_unavailable` must follow.
- `[trust.policy.keyless]` lands before anything ships, so the key-backend
  future costs nothing later.

**Negative, accepted.**

- `Signer` grows to two methods; an implementor wanting only message signatures
  still supplies both. Accepted over the churn of generalizing `sign`.
- Accepting Statement `_type: v0.1` is a deliberate, documented deviation from
  the security checklist's strictest reading. Mitigated by the closed
  two-element allowlist and stated in `signing.md` rather than left implicit.
- `ocx package sbom` cannot enumerate attestations without a trust decision. A
  real UX cost, taken deliberately: an unverified listing is a false-assurance
  surface.
- `PushOutcome` grows a field. It is a `Debug`-only in-process type, not a wire
  format — the parsed cross-tool contract is `PushReport`, which grows one
  optional field of its own. Both additions are additive, and `PushOutcome`
  gains `#[non_exhaustive]` in the same change so a path-dependency
  reconstructing it cannot silently break on the next field.
- Re-running `attest` is idempotent in *outcome* and additive in *state*: the
  same predicate verifies the same way, and each run appends another referrer
  (S1-I is append-only — a bundle is per-run unique by construction, since the
  certificate is ephemeral and the Rekor entry is fresh). A CI loop that retries
  a flaky attest therefore accumulates referrers toward
  `MAX_ATTESTATION_CANDIDATES`. At the cap the user prunes stale referrers with
  registry tooling; the constant is deliberately not configurable, because a
  knob here would turn a bounded discovery cost into an unbounded one on the
  verify path.
- Five new size constants are five new numbers to be wrong about. Each carries
  a rationale, a configurability decision, an error variant naming it, and a
  fixture that trips it.
- The 83 rename touches 12 slug-bearing files and the flag's definition/reference set across three crates, the docs and the
  acceptance suite, and it breaks any script matching the old slug. Taken now
  because nothing is released; taken *fully* — identifier and slug together —
  because a half-rename leaves two vocabularies for one condition.

**Risks.**

- The compose stack pins Rekor v1. Rekor v2's `hashedrekord:0.0.2`-over-PAE
  regime is untestable here and is deferred to #107 — accepting it now would
  ship a check whose red state is unreachable.
- cosign v3.1.2's notes call it "potentially the last v3.1 release ahead of v4".
  The interop surface is pinned to documented BUNDLE_SPEC behaviour, not to
  incidental v3 internals, precisely so a v4 does not invalidate the design.

---

# Not Doing

Stated so it is not read as an oversight, and so the docs can say it plainly
(SEC-32).

| Not doing | Why |
|---|---|
| Non-bundle multi-signer DSSE, `(t,n)` thresholds | Bundle v0.3 restricts to exactly one signature; a threshold model needs configured `(t,n)` semantics with no consumer yet. |
| Rekor v2 (`hashedrekord:0.0.2` over the PAE hash) | #107. Untestable against the pinned compose stack; adding it now is an unchecked green. |
| Accepting `intoto:0.0.2` tlog entries | Its canonicalization is unsourced, and sigstore-rs's `tlog_entry_for_dsse` hard-rejects every kind but `dsse:0.0.1` — so under D-d's delegated verification the pair has **no reachable green path**. Accepting it in `ACCEPTED_TLOG_KINDS` would be a row that can never match: a check whose passing state is indistinguishable from never running. |
| Refusing v0.2 provenance on **verify** | Part III row 21's floor is attach-side only (#102). Verify keeps accepting `slsaprovenance` v0.2 from external producers, because refusing it would break cosign interop for artifacts OCX did not create and cannot re-sign. |
| A `--referrer <digest>` selection flag for `sbom --output` | Two verified attestations of one predicate type is rare and ambiguous; the typed `MultipleAttestations` refusal naming both digests is the v1 answer. A selector is additive later if the case turns out to be real. |
| Parsing or summarizing SPDX | Attach parity yes, parse no (D2). One reader, no format trait, until a second is genuinely needed. |
| Unverified attestation enumeration (`--no-verify`) | An unverified listing is registry-controlled text presented as fact. |
| Attestation freshness or rollback protection | DSSE + Rekor prove "validly signed and logged at T", forever. They say nothing about a newer superseding SBOM. The digest-pinned lockfile closes artifact rollback independently; attestation staleness is a policy-layer concern nobody has asked for. **Docs must say this, not omit it.** |
| Attaching an attestation to an image index rather than a per-platform manifest | Keeps subject granularity identical to `ocx package sign -p`, so verify and `sbom` need no second rule. |
| KMS / key-based signers, browser PKCE | D4 — v2 `Signer` seam. |
| `[trust.policy.key]` | The slot is reserved by D3's nesting; the backend is not built. |
| OCX self-dogfooding SBOM attestation | D6 — GHCR has no Referrers API. |
| A `--no-canonical-tag`-derived subject digest | D-f (amended 2026-08-20) — the derivation problem is closed by construction: `AttestPipeline` resolves its per-platform target via index indirection, never from `canonical_tags`. `platform_digests` was never built. |


